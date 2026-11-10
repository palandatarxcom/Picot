// ABOUTME: Remote terminal attachment lifecycle: capability gate, canonical
// ABOUTME: context capture, attempt epochs, invalidation, and blocked state.
// ABOUTME: Owns no DOM and never sends a terminal frame itself.

/**
 * The Remote terminal attachment state machine (plan 2026-11-03 §4).
 *
 * A Remote page may only run terminal commands after three things line up on
 * the *current* connection: the approved binding revision, a freshly captured
 * canonical bootstrap target for the page's own workspace/session, and the
 * authoritative terminal list for that target. This module owns that sequence;
 * `app.js` stays the orchestrator and owns the DOM, routing, and transport.
 *
 * States:
 * - `unavailable` — no approved capability, no live runtime, or a stopped one.
 * - `disconnected` — no authenticated connection.
 * - `attaching` — resolving context; old terminal interaction is disabled.
 * - `ready` — binding installed and the authoritative list applied.
 * - `blocked` — context resolution failed or the binding was rejected.
 */
export const ATTACHMENT_STATES = Object.freeze([
  "disconnected",
  "unavailable",
  "attaching",
  "ready",
  "blocked",
]);

/** How many stopped/crashed identities one attempt may retain (plan §4). */
const EXITED_TARGET_LIMIT = 32;

/**
 * Full runtime identity key (plan §4, R4). Generation and owner are part of the
 * identity: a late event for the same instance before a rebind must not take
 * down the binding that replaced it.
 */
function targetKey(target) {
  return [
    target?.workspaceId,
    target?.sessionId,
    target?.instanceId,
    target?.workspaceGeneration,
    target?.ownerId,
  ].join("\u0000");
}

/** An event target must name every identity field; a partial one is never guessed. */
function hasFullIdentity(target) {
  return Boolean(
    target?.workspaceId &&
      target?.sessionId &&
      target?.instanceId &&
      target?.ownerId &&
      Number.isSafeInteger(target.workspaceGeneration),
  );
}

/**
 * @param {object} deps
 * @param {object} deps.client TerminalClient (binding, generation, list, tabs).
 * @param {() => object} deps.capabilities Current transport capabilities.
 * @param {() => number} deps.connectionEpoch Current authenticated connection id.
 * @param {() => ({workspaceId: string, sessionId: string}|null)} deps.route
 *   The page's own route, captured at call time.
 * @param {(query: object) => Promise<object|null>} deps.queryContext
 *   Side-effect-free canonical bootstrap query.
 * @param {(state: string, detail: object) => void} [deps.onState]
 *   Called on every state change; the orchestrator decides what to render.
 */
export class RemoteTerminalAttachment {
  constructor({ client, capabilities, connectionEpoch, route, queryContext, onState }) {
    this.client = client;
    this.capabilities = capabilities;
    this.connectionEpoch = connectionEpoch;
    this.route = route;
    this.queryContext = queryContext;
    this.onState = onState;
    this.state = "disconnected";
    /** Locally generated attempt id: only the newest one may install anything. */
    this.attempt = 0;
    /** Captured bootstrap target currently installed, or null. */
    this.binding = null;
    /** The one list reply that may enable input for the current attempt. */
    this.pendingList = null;
    /**
     * Full identities reported stopped/crashed for the captured
     * workspace/session during the current attempt (bounded).
     */
    this.exited = new Map();
    /** True while a locally initiated transition froze user input. */
    this.transitioning = false;
    /**
     * The attempt's captured workspace/session. Exit events are filtered against
     * this, never against the page's mutable route (plan §4, R4).
     */
    this.attemptContext = null;
    /** True once `commitTransition` invalidated, so a failure needs a fresh attach. */
    this.transitionInvalidated = false;
  }

  /** A connection/capability notification arrived (coalesced by the callers). */
  notifyConnection() {
    if (this.state === "attaching") return;
    return this.attach();
  }

  notifyDisconnect() {
    this.pendingList = null;
    this.attempt += 1;
    this.binding = null;
    this.attemptContext = null;
    this.exited.clear();
    this.transitioning = false;
    this.transitionInvalidated = false;
    this.client.invalidateBinding();
    this._setState("disconnected");
  }

  /**
   * Resolve fresh context for the page's own route and install it. Every await
   * re-checks the attempt id and connection epoch, so a reply that belongs to a
   * superseded attempt or an older connection is dropped instead of reviving
   * stale tabs.
   */
  async attach() {
    this.pendingList = null;
    this.exited.clear();
    this.attempt += 1;
    this.attemptContext = null;
    const attempt = this.attempt;
    const epoch = this.connectionEpoch();
    // Input closes before the first await: the previous binding and its local
    // tab projection are dropped now, so a keystroke through a still-mounted
    // xterm cannot reach the host during context resolution (plan §4, R2).
    this.binding = null;
    this.client.beginAttach();
    const caps = this.capabilities();
    if (!caps?.terminalBindingSupported) {
      this._invalidate();
      this._setState("unavailable", { reason: "protocol" });
      return false;
    }
    const route = this.route();
    if (!route?.workspaceId || !route?.sessionId) {
      this._invalidate();
      this._setState("unavailable", { reason: "route" });
      return false;
    }
    this.attemptContext = { workspaceId: route.workspaceId, sessionId: route.sessionId };
    this._setState("attaching");
    let target;
    try {
      target = await this.queryContext({
        workspaceId: route.workspaceId,
        sessionId: route.sessionId,
      });
    } catch (error) {
      if (!this._current(attempt, epoch)) return false;
      // `runtime_not_found` is D2's decided case: an explicit unavailable state
      // with a retry action, not a blocked binding. Anything else means the
      // context could not be established at all.
      if (error?.code === "runtime_not_found") {
        this._invalidate();
        this._setState("unavailable", { reason: "runtime" });
      } else {
        this._blocked("context");
      }
      return false;
    }
    if (!this._current(attempt, epoch)) return false;
    if (!target?.workspaceId || !target?.sessionId || !target?.instanceId) {
      this._invalidate();
      this._setState("unavailable", { reason: "runtime" });
      return false;
    }
    // The daemon answered for another page: install nothing, because a binding
    // for a different workspace is exactly the D1 failure this repair removes.
    if (target.workspaceId !== route.workspaceId || target.sessionId !== route.sessionId) {
      this._blocked("mismatch");
      return false;
    }
    if (this.exited.has(targetKey(target))) {
      this._invalidate();
      this._setState("unavailable", { reason: "runtime" });
      return false;
    }
    if (
      !this.client.setBinding({
        workspaceId: target.workspaceId,
        sessionId: target.sessionId,
        instanceId: target.instanceId,
      }) ||
      !this.client.setWorkspaceGeneration(target.workspaceGeneration)
    ) {
      this._blocked("invalid");
      return false;
    }
    this.binding = target;
    this.pendingList = { attempt, epoch, requestId: this.client.requestList() };
    if (this.pendingList.requestId === null) {
      // The command gate refused the list (frozen or invalidated meanwhile).
      this.pendingList = null;
      this._blocked("unavailable");
      return false;
    }
    return true;
  }

  /** Does this synchronous reply belong to the current attachment epoch (R1)? */
  ownsResponse(msg) {
    return this.client.isCurrentRequest(msg?.requestId);
  }

  /**
   * True when this list reply may be applied. Only the current epoch's list can
   * rebuild tabs; while attaching, only the one list this attempt requested can
   * finish it. Input opens in `listedApplied`, i.e. after the authoritative list
   * is the local projection — never before it (plan §4, R2).
   */
  acceptListed(msg) {
    if (!this.ownsResponse(msg)) return false;
    if (this.state === "ready") return true;
    if (this.state !== "attaching" || !this.pendingList) return false;
    return (msg?.requestId ?? null) === this.pendingList.requestId;
  }

  /** The authoritative list has been applied: this attachment may run commands. */
  listedApplied(msg) {
    if (this.state !== "attaching" || !this.pendingList) return;
    if ((msg?.requestId ?? null) !== this.pendingList.requestId) return;
    this.pendingList = null;
    this.client.markReady();
    this._setState("ready");
  }

  /**
   * The host refused a command as an admission decision (plan §3). Ownership
   * comes first: a rejection of a request from an older epoch must not block a
   * newer attachment. Then the attempt is invalidated, its evidence cleared, and
   * the page moves to the state that code allows — one explicit recovery action,
   * never an automatic list retry.
   */
  bindingRejected(code, requestId) {
    if (!this.ownsResponse({ requestId })) return false;
    this.attempt += 1;
    this.exited.clear();
    this.transitioning = false;
    this.transitionInvalidated = false;
    this.client.invalidateBinding();
    this.binding = null;
    this.pendingList = null;
    if (code === "terminal_protocol_unsupported") {
      this._setState("unavailable", { reason: "protocol" });
    } else if (code === "terminal_runtime_unavailable") {
      this._setState("unavailable", { reason: "runtime" });
    } else {
      this._setState("blocked", { reason: code });
    }
    return true;
  }

  /**
   * A sequenced runtime event. Only the full captured identity may invalidate
   * the binding: a late event for the same instance before a generation/owner
   * rebind must not take down the binding that replaced it. Exits for the
   * attempt's captured workspace/session are retained (bounded) so a late
   * bootstrap answer for a moved identity can be refused.
   */
  handleRuntimeEvent(frame) {
    const type = frame?.event?.type;
    if (type !== "runtime_stopped" && type !== "runtime_crashed") return false;
    const target = frame?.target;
    // A partial identity is never guessed into a match.
    if (!hasFullIdentity(target)) return false;
    if (this.binding && targetKey(this.binding) === targetKey(target)) {
      this._invalidate();
      this._setState("unavailable", { reason: "runtime" });
      return true;
    }
    const context = this.binding || this.attemptContext;
    if (
      context &&
      context.workspaceId === target.workspaceId &&
      context.sessionId === target.sessionId
    ) {
      this._rememberExit(target);
    }
    return false;
  }

  /**
   * Freeze user input for a locally initiated workspace/session transition,
   * checkpointing first (plan §4). Returns false when the checkpoint refused, in
   * which case the caller must not commit.
   */
  async beginTransition(checkpoint) {
    this.transitioning = true;
    this.client.freeze();
    if (typeof checkpoint !== "function") return true;
    try {
      await checkpoint();
      return true;
    } catch (error) {
      this.transitioning = false;
      this.client.unfreeze();
      throw error;
    }
  }

  /**
   * The transition was cancelled, refused, or shortcut. Two different cases
   * (plan §4): while the binding was never invalidated, unfreezing restores it;
   * once `commitTransition` ran, the binding is gone and only a fresh attach can
   * open input again — unfreezing would hand a dead binding back to the page.
   */
  cancelTransition() {
    this.transitioning = false;
    const invalidated = this.transitionInvalidated;
    this.transitionInvalidated = false;
    if (invalidated) return this.attach();
    this.client.unfreeze();
    return false;
  }

  /**
   * The transition is about to commit: invalidate first, so no command can be
   * sent under the old binding while the new workspace/session binds. The caller
   * reattaches (or reloads) afterwards.
   */
  commitTransition() {
    this.transitioning = false;
    this.transitionInvalidated = true;
    this.attempt += 1;
    this.pendingList = null;
    this.binding = null;
    this.exited.clear();
    this.client.invalidateBinding();
    this._setState("disconnected", { reason: "transition" });
  }

  /** Explicit user retry: re-query a supported runtime (never overrides the gate). */
  retry() {
    return this.attach();
  }

  _current(attempt, epoch) {
    return attempt === this.attempt && epoch === this.connectionEpoch();
  }

  _rememberExit(target) {
    if (this.exited.size >= EXITED_TARGET_LIMIT) {
      // Bounded evidence: a full store means the attempt is no longer
      // trustworthy, so drop it and re-query instead of guessing.
      this.exited.clear();
      this.pendingList = null;
      this.attempt += 1;
      this._invalidate();
      this._setState("unavailable", { reason: "runtime" });
      return;
    }
    this.exited.set(targetKey(target), true);
  }

  _invalidate() {
    this.pendingList = null;
    this.binding = null;
    this.client.invalidateBinding();
  }

  _blocked(reason) {
    this._invalidate();
    this._setState("blocked", { reason });
  }

  _setState(state, detail = {}) {
    this.state = state;
    this.onState?.(state, detail);
  }
}

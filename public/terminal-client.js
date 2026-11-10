// ABOUTME: Owner-scoped terminal broker client: request correlation, snapshot
// ABOUTME: replay (checkpoint before strictly sequential journal), and reconnect.
// ABOUTME: Owns no DOM; visual rendering belongs to terminal-tab.js.

/**
 * Command types whose failures the app classifies via consumeCommandType.
 * Success responses never echo requestId, so only these start-class commands
 * are tracked; a track-all map would grow for the workspace lifetime.
 */
const TRACKED_COMMAND_TYPES = new Set(["terminal_create", "terminal_restart"]);

/**
 * TerminalClient owns the broker protocol surface for one owner/workspace:
 * request correlation, the workspace-generation compare token, and the
 * snapshot-before-journal replay state machine. It never touches the DOM and
 * never accepts owner/root/port hints — those are derived by the host.
 */
export class TerminalClient {
  /**
   * @param {object} opts
   * @param {(envelope: object) => (string|null)} opts.send -底层发送；接收完整 terminal_command envelope。
   * @param {(terminalId: string, generation: number) => object} opts.createTab - 为一个 terminal 创建渲染适配器。
   */
  constructor({ send, createTab }) {
    this.send = send;
    this.createTab = createTab;
    /** terminalId -> { tab, generation, lastAppliedSequence, paused } */
    this.tabs = new Map();
    this.workspaceGeneration = null;
    this.requestCounter = 0;
    /** Remote binding from the canonical bootstrap target; null on Desktop. */
    this.binding = null;
    /** Bumped on every invalidation so old replies cannot satisfy new waiters. */
    this.epoch = 0;
    this.frozen = false;
    /**
     * Highest command id issued before the last invalidation. Ids are monotonic
     * (`tm-<n>`), so this one boundary answers "does this reply belong to the
     * current epoch?" for every response — including the high-frequency
     * input/ack families — without an unbounded requestId map (plan §4).
     */
    this.staleThrough = 0;
    /**
     * Remote attachment command phase: `null` (open), `"attaching"` (only the
     * controlled initial list may pass), or `"closed"` (no binding).
     */
    this.attachPhase = "closed";
    this.initialListSent = false;
    /** Pending sendAndAwait matchers awaiting a synchronous response. */
    this._awaiters = [];
    /**
     * requestId -> payload type for tracked start-class commands only,
     * used to classify command failures.
     */
    this._pendingCommands = new Map();
  }

  /** Update the host-owned workspace generation compare token. */
  setWorkspaceGeneration(generation) {
    const value = Number(generation);
    if (!Number.isSafeInteger(value) || value < 0) return false;
    this.workspaceGeneration = value;
    return true;
  }

  /**
   * Install the Remote binding captured from the canonical bootstrap target:
   * `{ workspaceId, sessionId, instanceId }`. Every command then carries it, and
   * the host admits the command only against that page and that exact runtime.
   * Desktop never calls this, so its envelope stays unchanged.
   */
  setBinding({ workspaceId, sessionId, instanceId } = {}) {
    const valid = [workspaceId, sessionId, instanceId].every(
      (value) => typeof value === "string" && value.trim(),
    );
    if (!valid) return false;
    this.binding = { workspaceId, sessionId, instanceId };
    return true;
  }

  /**
   * Invalidate the Remote binding and every in-flight waiter (plan §4). After
   * this the command gate is closed — no frame can be sent until a fresh
   * binding and generation are installed — existing awaiters settle with null
   * immediately, their timers are cleared, and the local tab projection is
   * disposed *without* closing daemon PTYs.
   *
   * Not `setWorkspaceGeneration(null)`: `Number(null)` is `0`, which would leave
   * the gate open on a stale page.
   */
  invalidateBinding() {
    this.binding = null;
    this.workspaceGeneration = null;
    this.epoch += 1;
    this.staleThrough = this.requestCounter;
    this.attachPhase = "closed";
    this.initialListSent = false;
    this.unfreeze();
    for (const awaiter of this._awaiters.splice(0)) {
      clearTimeout(awaiter.timer);
      awaiter.resolve(null);
    }
    this._pendingCommands.clear();
    this.reset();
  }

  /**
   * Start a Remote attachment attempt (plan §4). The previous binding,
   * generation and local tab projection are gone immediately, so no command —
   * least of all a keystroke through a still-mounted xterm — can be sent while
   * context is resolving. Until the authoritative list is applied only the one
   * controlled initial list is allowed through.
   */
  beginAttach() {
    this.invalidateBinding();
    this.attachPhase = "attaching";
  }

  /** The authoritative list for the current attempt is applied: input opens. */
  markReady() {
    this.attachPhase = null;
  }

  /**
   * True when a response carrying this requestId belongs to the current
   * invalidation epoch. Anything at or below the boundary was issued before the
   * last invalidation and must not touch current state (plan §4, R1).
   */
  isCurrentRequest(requestId) {
    const id = typeof requestId === "string" ? requestId : "";
    const serial = Number(id.slice(id.lastIndexOf("-") + 1));
    // Above the boundary it belongs to this epoch; above the counter it is not
    // an id this client ever issued.
    return (
      Number.isSafeInteger(serial) && serial > this.staleThrough && serial <= this.requestCounter
    );
  }

  /**
   * Freeze user command entry points for a locally initiated transition, while
   * still allowing the controlled pre-transition checkpoint (plan §4). xterm
   * keyboard input and panel controls both go through `command`, so this closes
   * them together instead of relying on the panel's interaction lock.
   */
  freeze() {
    this.frozen = true;
  }

  unfreeze() {
    this.frozen = false;
  }

  /** Drop every tab and destroy its adapter (e.g. before a full rebuild). */
  reset() {
    for (const entry of this.tabs.values()) {
      entry.tab.destroy?.();
    }
    this.tabs.clear();
    this._pendingCommands.clear();
  }

  /** Build a terminal_command envelope around an inner payload and send it. */
  command(payload) {
    if (this.workspaceGeneration === null) return null;
    // Freeze allows only the controlled checkpoint: everything else is a user
    // entry point whose binding is about to change.
    if (this.frozen && payload?.type !== "terminal_checkpoint") return null;
    // Attaching is not Ready: only the single controlled initial list may be
    // sent, never a user command (plan §4, R2).
    if (this.attachPhase === "attaching") {
      if (payload?.type !== "terminal_list" || this.initialListSent) return null;
      this.initialListSent = true;
    }
    const requestId = `tm-${++this.requestCounter}`;
    const envelope = {
      type: "terminal_command",
      requestId,
      workspaceGeneration: this.workspaceGeneration,
      payload,
    };
    if (this.binding) Object.assign(envelope, this.binding);
    if (TRACKED_COMMAND_TYPES.has(payload?.type)) {
      this._pendingCommands.set(requestId, payload.type);
    }
    this.send(envelope);
    return requestId;
  }

  /**
   * Send a command and resolve when a matching synchronous response arrives
   * (resolved via resolveResponse from the app's event listener). Resolves
   * `null` on timeout so a stuck host cannot block the caller indefinitely.
   */
  sendAndAwait(payload, match, timeoutMs = 5000) {
    return new Promise((resolve) => {
      // Correlate by the requestId this client generated plus the epoch it was
      // generated in: a late reply from an invalidated attempt can neither
      // satisfy this waiter nor be satisfied by it.
      const requestId = this.command(payload);
      if (requestId === null) {
        resolve(null);
        return;
      }
      const awaiter = { resolve, match, requestId, epoch: this.epoch, timer: null };
      awaiter.timer = setTimeout(() => {
        const idx = this._awaiters.indexOf(awaiter);
        if (idx >= 0) {
          this._awaiters.splice(idx, 1);
          resolve(null);
        }
      }, timeoutMs);
      this._awaiters.push(awaiter);
    });
  }

  /** Return and forget the payload type associated with a failed command. */
  consumeCommandType(requestId) {
    const type = this._pendingCommands.get(requestId) || null;
    this._pendingCommands.delete(requestId);
    return type;
  }

  /** Resolve any sendAndAwait awaiter whose matcher accepts this response. */
  resolveResponse(msg) {
    if (msg?.requestId) this._pendingCommands.delete(msg.requestId);
    for (let i = this._awaiters.length - 1; i >= 0; i -= 1) {
      const awaiter = this._awaiters[i];
      if (awaiter.epoch !== this.epoch) continue;
      if (!msg?.requestId || msg.requestId !== awaiter.requestId) continue;
      if (awaiter.match(msg)) {
        const { resolve, timer } = awaiter;
        clearTimeout(timer);
        this._awaiters.splice(i, 1);
        resolve(msg);
      }
    }
  }

  /** Request a fresh terminal_list (used on attach, reconnect, and gap recovery). */
  requestList() {
    return this.command({ type: "terminal_list" });
  }

  /** Apply a terminal_listed response: rebuild tabs from descriptors. */
  applyListed({ tabs }) {
    const liveTabs = (tabs || []).filter((tab) => tab.status !== "restoredMetadata");
    const live = new Set(liveTabs.map((t) => t.terminalId));
    // Drop tabs the host no longer reports (closed/exited elsewhere).
    for (const id of [...this.tabs.keys()]) {
      if (!live.has(id)) this.removeTab(id);
    }
    for (const desc of liveTabs) {
      this.applySnapshot({
        terminalId: desc.terminalId,
        generation: desc.generation,
        checkpoint: desc.checkpoint,
        checkpointWatermark: desc.checkpointWatermark,
        historyGap: desc.historyGap,
        journal: desc.journal ? [desc.journal] : [],
      });
    }
  }

  /**
   * Restore one tab: write the serialized checkpoint first, then apply journal
   * batches only when the next sequence is exactly lastApplied + 1. A forward
   * gap pauses the tab and requests a fresh list rather than appending blindly.
   */
  applySnapshot({ terminalId, generation, checkpoint, checkpointWatermark, historyGap, journal }) {
    let entry = this.tabs.get(terminalId);
    if (!entry || entry.generation !== generation) {
      const tab = this.createTab(terminalId, generation);
      entry = {
        tab,
        generation,
        lastAppliedSequence: 0,
        paused: false,
        historyGap: Boolean(historyGap),
      };
      this.tabs.set(terminalId, entry);
    }
    // A fresh snapshot/list is authoritative: clear a pause left by a prior
    // forward-gap recovery before replaying checkpoint + journal.
    entry.paused = false;
    entry.historyGap = Boolean(historyGap);
    if (checkpoint) {
      entry.tab.writeSnapshot(checkpoint);
      entry.lastAppliedSequence = Number(checkpointWatermark) || 0;
    }
    for (const batch of journal || []) {
      this.applyBatch(entry, batch);
    }
  }

  /** Apply a live terminal_output event with strict sequence validation. */
  applyOutput({ terminalId, generation, firstSequence, lastSequence, dataBase64 }) {
    const entry = this.tabs.get(terminalId);
    if (!entry || entry.generation !== generation || entry.paused) {
      return;
    }
    this.applyBatch(entry, { firstSequence, lastSequence, dataBase64 });
  }

  /** Remove a tab from local state (closed/exited). */
  removeTab(terminalId, generation = null) {
    const entry = this.tabs.get(terminalId);
    if (!entry || (generation !== null && entry.generation !== generation)) {
      return false;
    }
    entry.tab.destroy?.();
    this.tabs.delete(terminalId);
    return true;
  }

  applyBatch(entry, batch) {
    const first = Number(batch.firstSequence);
    const last = Number(batch.lastSequence);
    // Duplicate or already-applied sequences are ignored without pausing.
    if (first <= entry.lastAppliedSequence) {
      return;
    }
    // A retention gap is an explicit server state: display the retained tail
    // and continue from it instead of requesting the same unrecoverable list
    // forever.
    if (entry.historyGap) {
      entry.tab.writeOutput(batch.dataBase64);
      entry.lastAppliedSequence = last;
      entry.tab.ack(last);
      this.command({
        type: "terminal_ack",
        terminalId: entry.tab.terminalId,
        generation: entry.tab.generation,
        sequence: last,
      });
      return;
    }
    if (first !== entry.lastAppliedSequence + 1) {
      // Forward gap: continuity cannot be proven. Pause and re-snapshot.
      entry.paused = true;
      this.requestList();
      return;
    }
    entry.tab.writeOutput(batch.dataBase64);
    entry.lastAppliedSequence = last;
    entry.tab.ack(last);
    this.command({
      type: "terminal_ack",
      terminalId: entry.tab.terminalId,
      generation: entry.tab.generation,
      sequence: last,
    });
  }
}

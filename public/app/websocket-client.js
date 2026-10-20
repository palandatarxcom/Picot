// ABOUTME: Maintains the broker WebSocket, authentication handshake, and reconnect state.
// ABOUTME: Dispatches correlated control, session, and owner-scoped ephemeral frames.

import { consumeInjectedCapability, readInjectedCapability } from "./host-origin.js";

/**
 * WebSocket Client - Handles connection to backend WebSocket server
 */

// WebSocket readyState constants (spec §4.1). Named literals instead of live
// `WebSocket.OPEN` lookups: the global may be unstubbed in test environments.
const WS_CONNECTING = 0;
const WS_OPEN = 1;
const WS_CLOSING = 2;
const WS_CLOSED = 3;

/**
 * Resolve sole WebSocket endpoint from current HostServer origin.
 *
 * Native Pi runtimes speak stdin/stdout RPC; browser never connects to a Pi
 * port. Keeping URL construction origin-local also prevents an untrusted
 * query parameter from redirecting control traffic to an arbitrary socket.
 */
export function resolveWebSocketUrl(env = globalThis.window || globalThis) {
  const loc = env?.location || globalThis.location;
  const host = loc?.host;
  if (!host) throw new Error("HostServer origin is unavailable");
  const protocol = loc?.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${host}/v2/ws`;
}

export class WebSocketClient extends EventTarget {
  constructor(url, { identity = "desktop", grant = null, capabilitiesFromAck } = {}) {
    super();
    this.url = url;
    // "bridge" is a window behind the app-owned loopback bridge (remote host):
    // its hello carries the window grant and never the desktop capability.
    this.identity = identity;
    this.bridgeGrant = grant;
    this._capabilitiesFromAck = capabilitiesFromAck || (() => ({ native: true, class: "native" }));
    this.ws = null;
    this.reconnectAttempts = 0;
    this.maxReconnectAttempts = Infinity;
    this.reconnectDelay = 1000;
    this.maxReconnectDelay = 10000;
    this.isIntentionallyClosed = false;
    this.reconnectTimer = null;
    this.connectionState = "idle";
    if (!url.endsWith("/v2/ws")) {
      throw new Error("HostServer v2 WebSocket URL required");
    }
    this.protocolVersion = 2;
    this.clientId = null;
    // Cache capability privately for the lifetime of this page. The injected
    // global is one-shot, but every reconnect must repeat the authenticated
    // hello; never rediscover capability from URL or browser storage.
    this.desktopCapability = readInjectedCapability(globalThis);
    consumeInjectedCapability(globalThis);
    this.workspaceId = null;
    this.sessionId = null;
    this.canonicalRoute = this._readCanonicalRoute();
    this.requestCounter = 0;
    // Host capability state from authenticated hello_ack; consumers gate
    // native-only UI on it.
    this.capabilities = { native: false };
    // True after HostServer accepts hello. `connected` fires only after this.
    this.authenticated = false;
    this._pendingConnect = false;
    // Pending host requests keyed by requestId. Each entry resolves/rejects
    // the promise returned by sendControl() when a matching host_response
    // arrives (or on timeout / disconnect). `onProgress` receives streamed
    // control_progress frames (e.g. updater download chunks).
    this.pendingControls = new Map();
    this.controlTimeoutMs = 30000;
    this.sequenceGapRecoveryPending = false;
  }

  _readCanonicalRoute() {
    const match = (globalThis.location?.pathname || "").match(
      /^\/workspaces\/([^/]+)\/sessions\/([^/]+)/,
    );
    if (!match) return false;
    this.workspaceId = decodeURIComponent(match[1]);
    this.sessionId = decodeURIComponent(match[2]);
    return true;
  }

  getRuntimeTarget() {
    if (!this.workspaceId || !this.sessionId) return null;
    return {
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
      instanceId: this._instanceId(),
    };
  }

  setRoutingContext({ workspaceId, sessionId, instanceId }) {
    // The canonical route is the verified initial target. Same-workspace
    // transitions adopt a host-prepared target in this document, so it must
    // not freeze the routing triple after bootstrap.
    if (typeof workspaceId === "string" && workspaceId.trim()) {
      this.workspaceId = workspaceId.trim();
    }
    if (sessionId === null) this.sessionId = null;
    if (typeof sessionId === "string" && sessionId.trim()) {
      this.sessionId = sessionId.trim();
    }
    if (typeof instanceId === "string" && instanceId.trim()) {
      this._instanceIdValue = instanceId.trim();
    }
    console.debug("[WS route] setRoutingContext", {
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
    });
  }

  connect() {
    if (this.connectionState === "connecting") return;
    if (this.ws && this.ws.readyState === WS_OPEN) return;
    if (this.ws && this.ws.readyState === WS_CONNECTING) return;

    this.isIntentionallyClosed = false;
    this.connectionState = "connecting";
    this._pendingConnect = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    // Close only fully stale sockets before reconnecting
    if (this.ws && (this.ws.readyState === WS_CLOSING || this.ws.readyState === WS_CLOSED)) {
      this.ws = null;
    }
    this.ws = new WebSocket(this.url);

    this.ws.onopen = () => {
      console.log("[WS] Open; sending hello");
      this.reconnectAttempts = 0;
      this.connectionState = "open";
      this.authenticated = false;
      this._sendClientHello();
    };

    this.ws.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data);
        this.handleMessage(message);
      } catch (error) {
        console.error("[WS] Failed to parse message:", error);
      }
    };

    this.ws.onerror = (error) => {
      console.error("[WS] Error:", error);
      this.dispatchEvent(new CustomEvent("error", { detail: error }));
    };

    this.ws.onclose = (event) => {
      console.log(`[WS] Disconnected (code=${event.code}, reason=${event.reason || "n/a"})`);
      this.connectionState = "closed";
      this.dispatchEvent(new CustomEvent("disconnected"));

      this.rejectAllControls(new Error("WebSocket disconnected"));

      if (!this.isIntentionallyClosed) {
        this.attemptReconnect();
      }
    };
  }

  disconnect() {
    this.isIntentionallyClosed = true;
    this.connectionState = "closed";
    this.authenticated = false;
    this._pendingConnect = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.close();
    }
  }

  // Force reconnect — resets attempt counter and connects fresh
  forceReconnect() {
    this.reconnectAttempts = 0;
    this.isIntentionallyClosed = false;
    this.connectionState = "closed";
    if (this.ws && this.ws.readyState === WS_OPEN) {
      try {
        this.ws.close(1000, "force reconnect");
      } catch (_e) {}
    }
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.connect();
  }

  attemptReconnect() {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      console.error("[WS] Max reconnection attempts reached");
      this.dispatchEvent(new CustomEvent("reconnectFailed"));
      return;
    }

    this.reconnectAttempts++;
    const delay = Math.min(
      this.maxReconnectDelay,
      this.reconnectDelay * 2 ** (this.reconnectAttempts - 1),
    );

    console.log(
      `[WS] Reconnecting in ${delay}ms (attempt ${this.reconnectAttempts}/${this.maxReconnectAttempts})`,
    );

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  send(data) {
    if (this.ws && this.ws.readyState === WS_OPEN) {
      const requestId = `req-${++this.requestCounter}`;
      const payload = {
        type: "runtime_request",
        protocolVersion: 2,
        requestId,
        target: this._wireTarget(),
        command: data,
        idempotencyKey: `ui-${requestId}`,
      };
      console.debug("[WS route] send", {
        command: payload.command?.type || payload.type,
        requestId: payload.requestId,
        workspaceId: payload.target?.workspaceId,
        sessionId: payload.target?.sessionId,
      });
      this.ws.send(JSON.stringify(payload));
      // Return the requestId so callers can correlate a later
      // `command_undeliverable` reply back to the message they sent.
      return payload.requestId || null;
    } else {
      console.error("[WS] Cannot send, not connected");
      return null;
    }
  }

  _instanceId() {
    return this._instanceIdValue || "primary";
  }

  _readNativeCapability() {
    return this.desktopCapability;
  }

  async loadCanonicalTarget(fetchImpl = globalThis.fetch?.bind(globalThis)) {
    if (!this.canonicalRoute) return null;
    // A remote host has no HTTP data plane (contract 2 / T1 inventory): the
    // bridge client resolves the same triple over the restricted WS op.
    const target =
      this.identity === "bridge"
        ? await this._bootstrapRemoteTarget()
        : await this._bootstrapHttpTarget(fetchImpl);
    if (!target) return null;
    this.setRoutingContext(target);
    return target;
  }

  async _bootstrapHttpTarget(fetchImpl) {
    if (typeof fetchImpl !== "function") return null;
    const query = new URLSearchParams({
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
    });
    const response = await fetchImpl(`/v2/bootstrap?${query}`);
    if (!response.ok) throw new Error(`Host bootstrap failed (${response.status})`);
    return response.json();
  }

  // The op has a fixed schema host-side — any field beyond these five is
  // refused — so the frame is built here instead of sendData(), which adds
  // `protocolVersion`. `workspace.bootstrap` answers `data_response.target`.
  async _bootstrapRemoteTarget() {
    if (!this.ws || this.ws.readyState !== WS_OPEN) this.connect();
    const reply = await this._sendRequest(
      (requestId) => ({
        type: "data_request",
        requestId,
        operation: "workspace.bootstrap",
        workspaceId: this.workspaceId,
        sessionId: this.sessionId,
      }),
      { label: 'Data operation "workspace.bootstrap"' },
    );
    return reply?.target ?? null;
  }

  // Wire frames carry only the routing triple. Owner binding and workspace
  // generation never cross the wire — the host derives them from the live
  // runtime and the owner registry on every admission.
  _wireTarget() {
    return {
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
      instanceId: this._instanceId(),
    };
  }

  requestSnapshot() {
    this._requestCanonicalSnapshot();
  }

  requestRuntimeSnapshot(target) {
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    if (!target?.workspaceId || !target?.sessionId || !target?.instanceId) return;
    this.ws.send(
      JSON.stringify({
        type: "runtime_snapshot_request",
        protocolVersion: 2,
        requestId: `snapshot-${++this.requestCounter}`,
        target: {
          workspaceId: target.workspaceId,
          sessionId: target.sessionId,
          instanceId: target.instanceId,
        },
      }),
    );
  }

  // Send canonical v2 hello. Desktop windows present injected capability.
  _sendClientHello() {
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    const capability = this._readNativeCapability();
    this._readCanonicalRoute();
    const hello = {
      type: "hello",
      protocolVersion: 2,
      clientType: "desktop",
      clientId: this.clientId || `desktop-${globalThis.crypto?.randomUUID?.() || Date.now()}`,
    };
    if (this.identity === "desktop") hello.desktopCapability = capability;
    else if (this.bridgeGrant) hello.bridgeGrant = this.bridgeGrant;
    try {
      this.ws.send(JSON.stringify(hello));
    } catch (err) {
      console.error("[WS] Failed to send hello:", err);
    }
  }

  // Send an owner-scoped ephemeral command and return its requestId. The broker
  // derives the owner from the authenticated connection, never from the payload.
  sendEphemeral(instanceId, generation, payload) {
    if (this.ws && this.ws.readyState === WS_OPEN) {
      const requestId = `ep-${++this.requestCounter}`;
      const envelope = {
        type: "ephemeral_command",
        protocolVersion: 2,
        requestId,
        ephemeralInstanceId: instanceId,
        generation,
        payload,
      };
      try {
        this.ws.send(JSON.stringify(envelope));
      } catch (err) {
        console.error("[WS] Failed to send ephemeral command:", err);
        return null;
      }
      return requestId;
    }
    console.error("[WS] Cannot send ephemeral command, not connected");
    return null;
  }

  // Resolve once the socket is OPEN, or reject after `timeoutMs`. Lets control
  // commands sent during startup wait briefly for the broker connection instead
  // of failing the race between page load and the WS handshake.
  waitForOpen(timeoutMs = 5000) {
    if (this.ws && this.ws.readyState === WS_OPEN) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.removeEventListener("connected", onConnected);
        reject(new Error("WebSocket not connected"));
      }, timeoutMs);
      const onConnected = () => {
        clearTimeout(timer);
        this.removeEventListener("connected", onConnected);
        resolve();
      };
      this.addEventListener("connected", onConnected);
    });
  }

  // Send a control command (process/window lifecycle or native op handled by
  // the broker host, not forwarded to a pi upstream) and resolve with the
  // broker's result. Mirrors the promise semantics of a Tauri `invoke()` so
  // callers can stay transport-agnostic. `onProgress` (optional) receives
  // streamed control_progress frames; `timeoutMs` overrides the default for
  // long/interactive ops (folder picker, updater download).
  //
  // When already connected we register + send synchronously (snappy + makes the
  // requestId correlation deterministic). When not yet connected we wait briefly
  // for the broker handshake to win the page-load race before sending.
  sendControl(command, args = {}, options = {}) {
    if (this.ws && this.ws.readyState === WS_OPEN) {
      return this._sendControlNow(command, args, options);
    }
    return this.waitForOpen().then(() => this._sendControlNow(command, args, options));
  }

  _sendControlNow(command, args = {}, { onProgress = null, timeoutMs } = {}) {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WS_OPEN) {
        reject(new Error("WebSocket not connected; cannot send control command"));
        return;
      }
      const requestId = `ctl-${++this.requestCounter}`;
      const entry = { resolve, reject, onProgress, timer: null };
      const effectiveTimeout = typeof timeoutMs === "number" ? timeoutMs : this.controlTimeoutMs;
      if (effectiveTimeout > 0) {
        entry.timer = setTimeout(() => {
          if (this.pendingControls.has(requestId)) {
            this.pendingControls.delete(requestId);
            reject(new Error(`Control command "${command}" timed out`));
          }
        }, effectiveTimeout);
      }
      this.pendingControls.set(requestId, entry);

      const envelope = this._canonicalControlEnvelope(command, requestId, args);
      try {
        this.ws.send(JSON.stringify(envelope));
      } catch (err) {
        if (entry.timer) clearTimeout(entry.timer);
        this.pendingControls.delete(requestId);
        reject(err);
      }
    });
  }

  // Host data-plane request (`data_request` → `data_response`).
  sendData(operation, args = {}, options = {}) {
    return this._sendRequest(
      (requestId) => ({
        type: "data_request",
        protocolVersion: 2,
        requestId,
        operation,
        workspaceId: this.workspaceId || undefined,
        ...args,
      }),
      { label: `Data operation "${operation}"`, timeoutMs: options.timeoutMs },
    );
  }

  // Runtime command whose correlated reply drives the composer's delivery
  // record: exposes the generated requestId together with the response
  // promise so prompt acceptance/rejection can be tracked per send.
  sendRuntimeWithId(command, targetOrOptions = null, options = {}) {
    const requestId = `req-${++this.requestCounter}`;
    const response = this._sendRequestFixedId(
      requestId,
      () => {
        const hasExplicitTarget =
          targetOrOptions &&
          typeof targetOrOptions === "object" &&
          "workspaceId" in targetOrOptions &&
          "sessionId" in targetOrOptions &&
          "instanceId" in targetOrOptions;
        const target = hasExplicitTarget ? targetOrOptions : this._wireTarget();
        const requestOptions = hasExplicitTarget ? options : (targetOrOptions ?? {});
        // `timeoutMs` is applied by the caller's option bag, not here.
        const { idempotencyKey } = requestOptions;
        return {
          type: "runtime_request",
          protocolVersion: 2,
          requestId,
          target,
          command,
          idempotencyKey: idempotencyKey || `ui-${requestId}`,
        };
      },
      {
        label: `Runtime command "${command?.type || "unknown"}"`,
        timeoutMs: options.timeoutMs,
        // No unwrap: the delivery record needs the raw `{ success, ... }`
        // envelope, not Pi's inner data payload.
        rejectOnFailure: false,
      },
    );
    return { requestId, response };
  }

  // Runtime command with a correlated `runtime_response`. A caller may bind
  // the request to a prepared target so a same-workspace session adoption
  // cannot race with another routing-context change.
  sendRuntime(command, targetOrOptions = null, options = {}) {
    const hasExplicitTarget =
      targetOrOptions &&
      typeof targetOrOptions === "object" &&
      "workspaceId" in targetOrOptions &&
      "sessionId" in targetOrOptions &&
      "instanceId" in targetOrOptions;
    const target = hasExplicitTarget ? targetOrOptions : this._wireTarget();
    const requestOptions = hasExplicitTarget ? options : (targetOrOptions ?? {});
    const { timeoutMs, idempotencyKey, raw = false } = requestOptions;
    return this._sendRequest(
      (requestId) => ({
        type: "runtime_request",
        protocolVersion: 2,
        requestId,
        target,
        command,
        idempotencyKey: idempotencyKey || `ui-${requestId}`,
      }),
      {
        label: `Runtime command "${command?.type || "unknown"}"`,
        timeoutMs,
        // Pi replies `{ success, data }`; the pending map already hands us that
        // object, so unwrap only the inner payload. `raw` keeps the envelope
        // for commands whose verdict lives at the top level (abort: success
        // with no data payload).
        ...(raw ? {} : { unwrap: (reply) => reply?.data ?? null }),
        rejectOnFailure: true,
      },
    );
  }

  _sendRequest(envelopeFor, options) {
    const requestId = `req-${++this.requestCounter}`;
    return this._sendRequestFixedId(requestId, () => envelopeFor(requestId), options);
  }

  _sendRequestFixedId(
    requestId,
    envelopeFor,
    { label, timeoutMs, unwrap, rejectOnFailure = false },
  ) {
    const deliver = () =>
      new Promise((resolve, reject) => {
        if (!this.ws || this.ws.readyState !== WS_OPEN) {
          reject(new Error(`WebSocket not connected; cannot send ${label}`));
          return;
        }
        const effectiveTimeout = typeof timeoutMs === "number" ? timeoutMs : this.controlTimeoutMs;
        const envelope = envelopeFor(requestId);
        const entry = {
          resolve: (payload) => {
            if (rejectOnFailure && payload && payload.success === false) {
              reject(new Error(payload.error || `${label} failed`));
              return;
            }
            resolve(typeof unwrap === "function" ? unwrap(payload) : payload);
          },
          reject,
          onProgress: null,
          timer: null,
          // Remember which runtime triple a runtime_request was bound to;
          // handleMessage re-attaches it to the controlResponse detail.
          target: envelope.type === "runtime_request" ? envelope.target : null,
        };
        if (effectiveTimeout > 0) {
          entry.timer = setTimeout(() => {
            if (this.pendingControls.has(requestId)) {
              this.pendingControls.delete(requestId);
              reject(new Error(`${label} timed out`));
            }
          }, effectiveTimeout);
        }
        this.pendingControls.set(requestId, entry);
        try {
          this.ws.send(JSON.stringify(envelope));
        } catch (err) {
          if (entry.timer) clearTimeout(entry.timer);
          this.pendingControls.delete(requestId);
          reject(err);
        }
      });
    if (this.ws && this.ws.readyState === WS_OPEN) return deliver();
    return this.waitForOpen().then(deliver);
  }

  subscribeRuntimeTarget(target) {
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    if (!target?.workspaceId || !target?.sessionId || !target?.instanceId) return;
    this.ws.send(
      JSON.stringify({
        type: "runtime_subscribe",
        protocolVersion: 2,
        requestId: `sub-${++this.requestCounter}`,
        target: {
          workspaceId: target.workspaceId,
          sessionId: target.sessionId,
          instanceId: target.instanceId,
        },
      }),
    );
  }

  _subscribeCanonicalTarget() {
    this.subscribeRuntimeTarget(this._wireTarget());
  }

  /**
   * Re-subscribe to the current runtime so the host replays its pending
   * (still unanswered) extension dialogs. Idempotent; used by the
   * questionnaire card to recover a walker frame lost in transit.
   */
  requestPendingDialogReplay() {
    this._subscribeCanonicalTarget();
  }

  _requestCanonicalSnapshot() {
    this.requestRuntimeSnapshot(this._wireTarget());
  }

  _canonicalControlEnvelope(command, requestId, args) {
    // Host owns process and workspace lifecycle.
    return {
      type: "host_request",
      protocolVersion: 2,
      requestId,
      operation: command,
      args: args || {},
      idempotencyKey: `ui-${requestId}`,
    };
  }

  _recoverFromSequenceGap() {
    if (this.sequenceGapRecoveryPending) return;
    if (!this.ws || this.ws.readyState !== WS_OPEN) return;
    if (!this.workspaceId || !this.sessionId) return;
    this.sequenceGapRecoveryPending = true;
    this.ws.send(
      JSON.stringify({
        type: "runtime_snapshot_request",
        protocolVersion: 2,
        requestId: `snapshot-gap-${++this.requestCounter}`,
        target: this._wireTarget(),
      }),
    );
  }

  resolveControl(message) {
    const requestId = message?.requestId;
    if (!requestId) return;
    const pending = this.pendingControls.get(requestId);
    if (!pending) return;
    this.pendingControls.delete(requestId);
    if (pending.timer) clearTimeout(pending.timer);
    if (message.ok === false) {
      const error = new Error(message.error || "Control command failed");
      if (message.errorCode) error.code = message.errorCode;
      pending.reject(error);
    } else {
      pending.resolve(message.result);
    }
  }

  handleControlProgress(message) {
    const pending = this.pendingControls.get(message?.requestId);
    if (pending && typeof pending.onProgress === "function") {
      try {
        pending.onProgress(message.data);
      } catch (err) {
        console.error("[WS] control progress handler failed:", err);
      }
    }
  }

  rejectAllControls(error) {
    for (const [, pending] of this.pendingControls) {
      if (pending.timer) clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pendingControls.clear();
  }

  handleMessage(message) {
    if (
      message.type === "runtime_response" ||
      message.type === "host_response" ||
      message.type === "data_response"
    ) {
      // Runtime/host replies use `response`; data replies carry payload fields
      // at top level. Normalize both to resolveControl's result shape.
      let normalized = message;
      if (message.result === undefined && message.response !== undefined) {
        normalized = { ...message, result: message.response };
      } else if (message.type === "data_response" && message.result === undefined) {
        const { type, requestId, ok, ...payload } = message;
        normalized = { ...message, result: payload };
      }
      // Runtime replies carry no target on the wire; the pending entry
      // remembers the triple the request was bound to. Re-attach it so
      // controlResponse consumers can tell a foreground round-trip from a
      // bound-to-another-runtime one.
      const pendingEntry = this.pendingControls.get(message.requestId);
      if (pendingEntry?.target) normalized = { ...normalized, target: pendingEntry.target };
      this.resolveControl(normalized);
      this.dispatchEvent(new CustomEvent("controlResponse", { detail: normalized }));
      return;
    }

    if (message.type === "error") {
      if (message.error?.code === "event_sequence_gap") {
        this._recoverFromSequenceGap();
      }
      const error = {
        requestId: message.requestId,
        code: message.error?.code,
        message: message.error?.message || message.error?.code || "Request failed",
      };
      // Handshake rejections carry no requestId, so they reach no promise;
      // host-connection reads credential state from this frame.
      this.dispatchEvent(new CustomEvent("hostError", { detail: error }));
      this.resolveControl({
        requestId: error.requestId,
        ok: false,
        // Keep the machine code alongside the human message: callers such as the
        // file preview panel must distinguish a conflict from any other failure.
        errorCode: error.code,
        error: error.message,
      });
      if (error.requestId) {
        this.dispatchEvent(new CustomEvent("runtimeError", { detail: error }));
      }
      return;
    }

    if (message.type === "runtime_event") {
      this.dispatchEvent(new CustomEvent("runtimeEvent", { detail: message }));
      return;
    }

    if (message.type === "runtime_snapshot") {
      this.sequenceGapRecoveryPending = false;
      this.dispatchEvent(new CustomEvent("runtimeSnapshot", { detail: message }));
      return;
    }

    // Streamed progress for an in-flight host request (e.g. updater download).
    if (message.type === "control_progress") {
      this.handleControlProgress(message);
      return;
    }

    // HostServer v2 acknowledges authenticated hello directly.
    if (message.type === "hello_ack") {
      this.capabilities = this._capabilitiesFromAck(message);
      this.authenticated = true;
      this.dispatchEvent(new CustomEvent("hostCapabilities", { detail: this.capabilities }));
      this._subscribeCanonicalTarget();
      this._requestCanonicalSnapshot();
      if (this._pendingConnect) {
        this._pendingConnect = false;
        this.dispatchEvent(new CustomEvent("connected"));
      }
      return;
    }

    // Ack for the raw runtime_subscribe frame sent in hello_ack; nothing is
    // pending on it, but it must be recognized so it never logs as unknown.
    if (message.type === "runtime_subscribed") {
      return;
    }

    // Owner-scoped bootstrap (live ephemeral descriptors) for a desktop client.
    if (message.type === "owner_bootstrap") {
      this.dispatchEvent(new CustomEvent("ownerBootstrap", { detail: message }));
      return;
    }

    // App-global registry mutated by ANY native window (or host prune).
    // Every authenticated native client refreshes its sidebar.
    if (message.type === "registry_changed") {
      this.dispatchEvent(new CustomEvent("registryChanged", { detail: message }));
      return;
    }
    if (message.type === "registry_changed") {
      this.dispatchEvent(new CustomEvent("registryChanged", { detail: message }));
      return;
    }

    // A task-completion OS notification was clicked: route the UI to it.
    if (message.type === "notification_activated") {
      this.dispatchEvent(new CustomEvent("notificationActivated", { detail: message }));
      return;
    }

    // Owner-scoped MCP sign-in progress from the host's `pi mcp login` runner.
    // Non-secret by construction: operation id, status, authorization URL.
    if (message.type === "mcpLoginUpdate") {
      this.dispatchEvent(new CustomEvent("mcpLoginUpdate", { detail: message }));
      return;
    }

    if (message.type === "git_status" || message.type === "git_diff") {
      this.dispatchEvent(
        new CustomEvent(message.type === "git_status" ? "gitStatus" : "gitDiff", {
          detail: message,
        }),
      );
      return;
    }
    if (
      message.type === "git_log" ||
      message.type === "git_log_detail" ||
      message.type === "git_commit_diff"
    ) {
      const eventName = {
        git_log: "gitLog",
        git_log_detail: "gitLogDetail",
        git_commit_diff: "gitCommitDiff",
      }[message.type];
      this.dispatchEvent(new CustomEvent(eventName, { detail: message }));
      return;
    }
    if (message.type === "git_ai_commit_message") {
      this.dispatchEvent(new CustomEvent("gitAiCommitMessage", { detail: message }));
      return;
    }
    if (message.type === "git_commit_confirmation_required") {
      this.dispatchEvent(new CustomEvent("gitCommitConfirmationRequired", { detail: message }));
      return;
    }
    if (message.type === "git_commit_result") {
      this.dispatchEvent(new CustomEvent("gitCommitResult", { detail: message }));
      return;
    }
    if (message.type === "git_commit_started") {
      this.dispatchEvent(new CustomEvent("gitCommitStarted", { detail: message }));
      return;
    }
    if (message.type === "git_command_ack") {
      this.dispatchEvent(new CustomEvent("gitCommandAck", { detail: message }));
      return;
    }
    if (message.type === "git_push") {
      this.dispatchEvent(new CustomEvent("gitPush", { detail: message }));
      return;
    }
    if (message.type === "git_push_failed") {
      this.dispatchEvent(new CustomEvent("gitPushFailed", { detail: message }));
      return;
    }
    if (message.type === "runtime_started") {
      this.dispatchEvent(new CustomEvent("runtimeStarted", { detail: message }));
      return;
    }
    if (message.type === "git_command_failed" || message.type === "git_ai_commit_message_failed") {
      this.dispatchEvent(new CustomEvent("gitCommandFailed", { detail: message }));
      return;
    }

    // A sequenced event from one of this owner's ephemeral runtimes.
    if (message.type === "ephemeral_event" || message.type === "ephemeral_snapshot") {
      this.dispatchEvent(new CustomEvent("ephemeralEvent", { detail: message }));
      return;
    }

    // An ephemeral command could not be routed/delivered (correlated by
    // requestId); the error is generic and never reveals instance existence.
    if (message.type === "ephemeral_command_failed") {
      this.dispatchEvent(new CustomEvent("ephemeralCommandFailed", { detail: message }));
      return;
    }

    // Terminal PTY events (owner-scoped): the host terminal manager delivers
    // these only to the current authenticated native client. They never pass
    // through Pi or the embedded server.
    if (message.type === "terminal_event") {
      this.dispatchEvent(new CustomEvent("terminalEvent", { detail: message }));
      return;
    }
    if (message.type === "terminal_command_failed") {
      this.dispatchEvent(new CustomEvent("terminalCommandFailed", { detail: message }));
      return;
    }
    // Manager synchronous responses (terminal_created/listed/closed/restarted/...)
    // are delivered on this socket too; surface them as terminalEvent so the
    // panel can rebuild tab state.
    if (typeof message.type === "string" && message.type.startsWith("terminal_")) {
      this.dispatchEvent(new CustomEvent("terminalEvent", { detail: message }));
      return;
    }

    // Host-targeted window close request: the coordinator runs its serialized
    // risk/settlement flow and replies with window_close_approve.
    if (message.type === "window_close_request") {
      this.dispatchEvent(new CustomEvent("windowCloseRequest", { detail: message }));
      return;
    }

    console.warn("[WS] Unknown HostServer v2 message type:", message.type);
  }
}

// Client for the Picot Configuration data plane.
//
// pi's native RPC command set is fixed and cannot be extended, so Configuration
// operations (model catalog, API keys, agent-config / models.json files) are
// served by the `picot-config` command registered in the picot-bridge
// extension. We invoke it by sending a native RPC `prompt` of the form
// `/picot-config <json>` — extension commands execute immediately without
// hitting the LLM or session history. The handler returns its result through
// `ctx.ui.notify(JSON)`, which arrives here as a `notify` extension-UI event.
// We correlate requests and responses by a per-call id.
//
// `consumeNotify(request)` must be called for every incoming `notify` event; it
// returns true when the notification was a config response (and should NOT be
// rendered as a chat message), false otherwise.

import { randomId } from "../utils/random-id.js";

const DEFAULT_TIMEOUT_MS = 30_000;

export function consumeConfigResponseFrame(gateway, frame) {
  return Boolean(
    frame?.type === "runtime_event" &&
      frame.event?.type === "extension_ui_request" &&
      gateway.consumeNotify(frame.event, frame.target ?? null),
  );
}

function sameRuntimeTarget(left, right) {
  return (
    left.workspaceId === right.workspaceId &&
    left.sessionId === right.sessionId &&
    (left.instanceId ?? "") === (right.instanceId ?? "")
  );
}

export class ConfigGateway {
  #runtime;
  #getTarget;
  #pending = new Map();
  #waitUntilReady;

  constructor({ runtime, getTarget, waitUntilReady = null }) {
    this.#runtime = runtime;
    this.#getTarget = getTarget;
    this.#waitUntilReady = waitUntilReady;
  }

  // Invoke a configuration operation. Resolves with the handler payload
  // `{ ok: boolean, data?, error? }`. Rejects only on transport/timeout errors.
  call(op, params = {}, options = {}) {
    if (this.#waitUntilReady) {
      // The readiness gate runs before #send, so #send's timeout does not
      // cover it: a runtime that never proves live (e.g. a disk-restored
      // session that never binds) must surface as an error, not hang the
      // caller forever.
      let gateTimer;
      const gateTimeout = new Promise((_, reject) => {
        gateTimer = setTimeout(
          () => reject(new Error(`Configuration request "${op}" timed out waiting for runtime`)),
          options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
        );
      });
      return Promise.race([
        this.#waitUntilReady().then(() => {
          // Stop the gate timer the moment the gate opens: it starts before
          // the send, so an un-cleared timer always wins the race and reports
          // a slow response as "waiting for runtime" — the wrong cause.
          clearTimeout(gateTimer);
          return this.#send(op, params, options);
        }),
        gateTimeout,
      ]).finally(() => clearTimeout(gateTimer));
    }
    return this.#send(op, params, options);
  }

  #send(op, params, { timeoutMs = DEFAULT_TIMEOUT_MS, target: targetOverride, beforeSend }) {
    // MCP-only local guard: readiness can resolve long after the user action,
    // so the caller re-verifies its frozen routing target here, synchronously,
    // immediately before the request is dispatched. It never awaits and never
    // re-targets: a throw rejects the call with nothing sent.
    try {
      beforeSend?.();
    } catch (error) {
      return Promise.reject(error);
    }
    const target = targetOverride ?? this.#getTarget();
    if (!target) return Promise.reject(new Error("No active session for configuration request"));
    const id = `cfg-${randomId()}`;
    const message = `/picot-config ${JSON.stringify({ id, op, params })}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`Configuration request "${op}" timed out`));
      }, timeoutMs);
      this.#pending.set(id, { resolve, reject, timer, target });
      this.#runtime
        .request({ type: "prompt", message }, target, { idempotencyKey: id })
        .catch((error) => {
          const pending = this.#pending.get(id);
          if (!pending) return;
          clearTimeout(pending.timer);
          this.#pending.delete(id);
          reject(error);
        });
    });
  }

  // Returns true if the notification was a config response (consumed).
  // `expectedTarget` is the routing triple the frame arrived on: a pending
  // request bound to another runtime target is swallowed without resolving —
  // its own timeout reports the miss instead of handing a foreign runtime's
  // payload to this caller.
  consumeNotify(request, expectedTarget = null) {
    const message = request?.message;
    if (typeof message !== "string" || !message.includes("__picotConfig")) return false;
    let payload;
    try {
      payload = JSON.parse(message);
    } catch {
      return false;
    }
    const id = payload?.__picotConfig;
    if (typeof id !== "string") return false;
    const pending = this.#pending.get(id);
    if (!pending) return true; // ours, but already settled/timed out — still swallow it
    if (pending.target && expectedTarget && !sameRuntimeTarget(pending.target, expectedTarget)) {
      return true; // foreign runtime anomaly: swallow, let the timeout fire
    }
    clearTimeout(pending.timer);
    this.#pending.delete(id);
    const { __picotConfig, ...result } = payload;
    pending.resolve(result);
    return true;
  }
}

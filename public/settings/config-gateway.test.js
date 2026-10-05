import { describe, expect, it, vi } from "vitest";
import { ConfigGateway, consumeConfigResponseFrame } from "./config-gateway.js";

function createHarness() {
  const requests = [];
  const runtime = {
    request: vi.fn((command, target, options) => {
      requests.push({ command, target, options });
      return Promise.resolve({ acceptance: "accepted" });
    }),
  };
  const target = { workspaceId: "w", sessionId: "s", instanceId: "i" };
  const gateway = new ConfigGateway({ runtime, getTarget: () => target });
  return { requests, runtime, gateway };
}

function idFromRequest(request) {
  const message = request.command.message;
  return JSON.parse(message.slice("/picot-config ".length)).id;
}

describe("ConfigGateway", () => {
  it("waits for the active runtime target before sending startup configuration reads", async () => {
    let markReady;
    const ready = new Promise((resolve) => {
      markReady = resolve;
    });
    const waitUntilReady = () => ready;
    const runtime = { request: vi.fn().mockResolvedValue({ acceptance: "accepted" }) };
    const target = { workspaceId: "w", sessionId: "s", instanceId: "i" };
    const gateway = new ConfigGateway({ runtime, getTarget: () => target, waitUntilReady });

    void gateway.call("get_default_thinking_level");
    void gateway.call("get_default_auto_compaction");
    await Promise.resolve();
    expect(runtime.request).not.toHaveBeenCalled();

    markReady();
    await vi.waitFor(() => expect(runtime.request).toHaveBeenCalledTimes(2));
  });

  it("invokes /picot-config with an idempotency key and resolves on the matching notify", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("list_model_catalog", { foo: 1 });
    expect(requests).toHaveLength(1);
    const { command, options } = requests[0];
    expect(command.type).toBe("prompt");
    expect(command.message.startsWith("/picot-config ")).toBe(true);
    const payload = JSON.parse(command.message.slice("/picot-config ".length));
    expect(payload).toMatchObject({ op: "list_model_catalog", params: { foo: 1 } });
    expect(options.idempotencyKey).toBe(payload.id);

    const consumed = gateway.consumeNotify({
      message: JSON.stringify({ __picotConfig: payload.id, ok: true, data: { providers: [] } }),
    });
    expect(consumed).toBe(true);
    await expect(promise).resolves.toEqual({ ok: true, data: { providers: [] } });
  });

  it("sends navigate_tree through the bridge command contract", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("navigate_tree", {
      targetId: "leaf-9",
      summarize: false,
      label: "Resume branch",
    });
    const { command, options } = requests[0];
    const payload = JSON.parse(command.message.slice("/picot-config ".length));
    expect(payload).toMatchObject({
      op: "navigate_tree",
      params: { targetId: "leaf-9", summarize: false, label: "Resume branch" },
    });
    expect(options.idempotencyKey).toBe(payload.id);
    gateway.consumeNotify({
      message: JSON.stringify({
        __picotConfig: payload.id,
        ok: true,
        data: { cancelled: false },
      }),
    });
    await expect(promise).resolves.toEqual({ ok: true, data: { cancelled: false } });
  });

  it("consumes a matching-target response even when it arrives from a background subscription", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("generate_session_title");
    const id = idFromRequest(requests[0]);

    const consumed = consumeConfigResponseFrame(gateway, {
      type: "runtime_event",
      target: { workspaceId: "w", sessionId: "s", instanceId: "i" },
      event: {
        type: "extension_ui_request",
        method: "notify",
        message: JSON.stringify({ __picotConfig: id, ok: true, data: { title: "Done" } }),
      },
    });

    expect(consumed).toBe(true);
    await expect(promise).resolves.toEqual({ ok: true, data: { title: "Done" } });
  });

  it("never resolves a pending request from a different runtime target", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("generate_session_title");
    const id = idFromRequest(requests[0]);
    let settled = false;
    void promise.then(
      () => {
        settled = true;
      },
      () => {
        settled = true;
      },
    );

    // A notify carrying our id but emitted by another runtime is a routing
    // anomaly: it must be swallowed (never rendered as chat) without
    // resolving the pending caller bound to the original target.
    const consumed = consumeConfigResponseFrame(gateway, {
      type: "runtime_event",
      target: { workspaceId: "w", sessionId: "background-s", instanceId: "background-i" },
      event: {
        type: "extension_ui_request",
        method: "notify",
        message: JSON.stringify({ __picotConfig: id, ok: true, data: { title: "Stale" } }),
      },
    });

    expect(consumed).toBe(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(settled).toBe(false);

    // The reply from the target the request was actually sent to resolves.
    const consumedMatching = consumeConfigResponseFrame(gateway, {
      type: "runtime_event",
      target: { workspaceId: "w", sessionId: "s", instanceId: "i" },
      event: {
        type: "extension_ui_request",
        method: "notify",
        message: JSON.stringify({ __picotConfig: id, ok: true, data: { title: "Done" } }),
      },
    });
    expect(consumedMatching).toBe(true);
    await expect(promise).resolves.toEqual({ ok: true, data: { title: "Done" } });
  });

  it("ignores notifications that are not config responses", () => {
    const { gateway } = createHarness();
    expect(gateway.consumeNotify({ message: "hello world" })).toBe(false);
    expect(gateway.consumeNotify({ message: undefined })).toBe(false);
    expect(gateway.consumeNotify({ message: '{"__picotConfig":123}' })).toBe(false);
  });

  it("swallows config responses even after the caller settled", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("read_agent_config");
    const id = idFromRequest(requests[0]);
    gateway.consumeNotify({ message: JSON.stringify({ __picotConfig: id, ok: true }) });
    await promise;
    // A duplicate/late notify for the same id is still recognized as ours.
    expect(
      gateway.consumeNotify({ message: JSON.stringify({ __picotConfig: id, ok: true }) }),
    ).toBe(true);
  });

  it("rejects when the runtime request fails", async () => {
    const target = { workspaceId: "w", sessionId: "s", instanceId: "i" };
    const runtime = { request: vi.fn(() => Promise.reject(new Error("runtime down"))) };
    const gateway = new ConfigGateway({ runtime, getTarget: () => target });
    await expect(gateway.call("list_model_catalog")).rejects.toThrow("runtime down");
  });

  it("times out when no response arrives", async () => {
    vi.useFakeTimers();
    try {
      const { gateway } = createHarness();
      const promise = gateway.call("generate_session_title", {}, { timeoutMs: 1000 });
      const assertion = expect(promise).rejects.toThrow("timed out");
      await vi.advanceTimersByTimeAsync(1000);
      await assertion;
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects when the readiness gate never opens", async () => {
    vi.useFakeTimers();
    try {
      const runtime = { request: vi.fn() };
      const gateway = new ConfigGateway({
        runtime,
        getTarget: () => ({ workspaceId: "w", sessionId: "s", instanceId: "i" }),
        waitUntilReady: () => new Promise(() => {}),
      });
      const assertion = expect(gateway.call("list_model_catalog")).rejects.toThrow(
        "waiting for runtime",
      );
      await vi.advanceTimersByTimeAsync(30_000);
      await assertion;
      expect(runtime.request).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("rejects when there is no active session target", async () => {
    const runtime = { request: vi.fn() };
    const gateway = new ConfigGateway({ runtime, getTarget: () => null });
    await expect(gateway.call("list_model_catalog")).rejects.toThrow("No active session");
  });
});

describe("ConfigGateway timeout attribution", () => {
  it("a gate that opens keeps the send's own timeout as the reported cause", async () => {
    vi.useFakeTimers();
    try {
      // The gate opens immediately; the runtime never answers. The gate timer
      // starts before the send, so without clearing it the caller is told the
      // runtime was never ready — the wrong cause for a stalled response.
      const gateway = new ConfigGateway({
        runtime: { request: vi.fn(() => Promise.resolve({ acceptance: "accepted" })) },
        getTarget: () => ({ workspaceId: "w", sessionId: "s", instanceId: "i" }),
        waitUntilReady: () => Promise.resolve(),
      });
      const promise = gateway.call("list_model_catalog");
      const assertion = expect(promise).rejects.toThrow(
        'Configuration request "list_model_catalog" timed out',
      );
      await vi.advanceTimersByTimeAsync(30_100);
      await assertion;
      await expect(promise).rejects.not.toThrow("waiting for runtime");
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("ConfigGateway beforeSend guard", () => {
  it("runs the guard synchronously after readiness and before the request is dispatched", async () => {
    const order = [];
    let sentId = null;
    const runtime = {
      request: vi.fn((command) => {
        order.push("request");
        sentId = JSON.parse(command.message.slice("/picot-config ".length)).id;
        return Promise.resolve({ acceptance: "accepted" });
      }),
    };
    const target = { workspaceId: "w", sessionId: "s", instanceId: "i" };
    let ready = false;
    const gateway = new ConfigGateway({
      runtime,
      getTarget: () => target,
      waitUntilReady: () => {
        ready = true;
        return Promise.resolve();
      },
    });

    const promise = gateway.call(
      "mcp_import_global_overrides",
      {},
      {
        beforeSend: () => {
          order.push("beforeSend");
          expect(ready).toBe(true);
        },
      },
    );
    expect(order).toEqual([]); // readiness has not resolved yet
    await vi.waitFor(() => expect(runtime.request).toHaveBeenCalledTimes(1));
    expect(order).toEqual(["beforeSend", "request"]);
    gateway.consumeNotify({ message: JSON.stringify({ __picotConfig: sentId, ok: true }) });
    await expect(promise).resolves.toEqual({ ok: true });
  });

  it("rejects without sending when the guard throws, on both gate paths", async () => {
    const { runtime, gateway } = createHarness();
    await expect(
      gateway.call(
        "mcp_save_server",
        {},
        {
          beforeSend: () => {
            throw new Error("Runtime target changed before MCP configuration was sent");
          },
        },
      ),
    ).rejects.toThrow(/Runtime target changed/);
    expect(runtime.request).not.toHaveBeenCalled();

    const gatedRuntime = { request: vi.fn().mockResolvedValue({ acceptance: "accepted" }) };
    const gated = new ConfigGateway({
      runtime: gatedRuntime,
      getTarget: () => ({ workspaceId: "w", sessionId: "s", instanceId: "i" }),
      waitUntilReady: () => Promise.resolve(),
    });
    await expect(
      gated.call(
        "mcp_import_global_overrides",
        {},
        {
          beforeSend: () => {
            throw new Error("MCP page is closed");
          },
        },
      ),
    ).rejects.toThrow(/closed/);
    expect(gatedRuntime.request).not.toHaveBeenCalled();
  });

  it("keeps default callers unchanged when no guard is passed", async () => {
    const { requests, gateway } = createHarness();
    const promise = gateway.call("list_model_catalog");
    expect(requests).toHaveLength(1);
    const consumed = gateway.consumeNotify({
      message: JSON.stringify({ __picotConfig: idFromRequest(requests[0]), ok: true, data: {} }),
    });
    expect(consumed).toBe(true);
    await expect(promise).resolves.toEqual({ ok: true, data: {} });
  });
});

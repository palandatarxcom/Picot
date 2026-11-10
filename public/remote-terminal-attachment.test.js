// ABOUTME: Remote terminal attachment lifecycle: capability gate, captured
// ABOUTME: context, attempt/epoch gating, full-identity exits, and blocked (no-retry) state.
import { describe, expect, test, vi } from "vitest";
import { RemoteTerminalAttachment } from "./remote-terminal-attachment.js";
import { TerminalClient } from "./terminal-client.js";

const CAPS_OK = { native: true, terminal: true, terminalBindingSupported: true };
const ROUTE = { workspaceId: "w-1", sessionId: "s-1" };
// The bootstrap target carries the full runtime identity (plan §4/R4).
const TARGET = {
  workspaceId: "w-1",
  sessionId: "s-1",
  instanceId: "i-1",
  workspaceGeneration: 4,
  ownerId: "owner-1",
};
const STOPPED = { event: { type: "runtime_stopped" }, target: { ...TARGET } };

function tab() {
  return {
    writeSnapshot: vi.fn(),
    writeOutput: vi.fn(),
    ack: vi.fn(),
    destroy: vi.fn(),
  };
}

function harness({ caps = CAPS_OK, route = ROUTE, query } = {}) {
  const sent = [];
  const client = new TerminalClient({
    send: (envelope) => sent.push(envelope),
    createTab: () => tab(),
  });
  const states = [];
  let epoch = 1;
  const capabilities = { current: caps };
  const queryContext = vi.fn(query || (async () => ({ ...TARGET })));
  const attachment = new RemoteTerminalAttachment({
    client,
    capabilities: () => capabilities.current,
    connectionEpoch: () => epoch,
    route: () => route,
    queryContext,
    onState: (state, detail) => states.push({ state, detail }),
  });
  return {
    attachment,
    client,
    sent,
    states,
    capabilities,
    queryContext,
    bumpEpoch: () => {
      epoch += 1;
    },
    setRoute: (next) => {
      route = next;
    },
  };
}

const listed = (requestId) => ({ type: "terminal_listed", requestId, tabs: [] });

/** Attach and consume the controlled initial list, i.e. reach Ready. */
async function ready(h) {
  await h.attachment.attach();
  const requestId = h.sent.at(-1).requestId;
  const msg = listed(requestId);
  expect(h.attachment.acceptListed(msg)).toBe(true);
  h.attachment.listedApplied(msg);
  return requestId;
}

const inputFrame = () => ({ type: "terminal_input", terminalId: "t1", dataBase64: "YQ==" });

describe("RemoteTerminalAttachment capability gate", () => {
  test("an unsupported revision never queries context and never sends a frame", async () => {
    const h = harness({ caps: { native: true, terminal: true, terminalBindingSupported: false } });
    await h.attachment.attach();
    expect(h.attachment.state).toBe("unavailable");
    expect(h.queryContext).not.toHaveBeenCalled();
    expect(h.sent).toHaveLength(0);
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("a supported connection attaches with the approved envelope", async () => {
    const h = harness();
    await h.attachment.attach();
    expect(h.attachment.state).toBe("attaching");
    expect(h.queryContext).toHaveBeenCalledWith(ROUTE);
    const list = h.sent.at(-1);
    expect(list).toMatchObject({
      type: "terminal_command",
      workspaceId: "w-1",
      workspaceGeneration: 4,
      sessionId: "s-1",
      instanceId: "i-1",
      payload: { type: "terminal_list" },
    });
    // Attaching is not Ready: a user command (a keystroke) can never leave, and
    // no frame is produced for it (plan §4, R2).
    expect(h.client.command(inputFrame())).toBeNull();
    expect(h.sent.filter((f) => f.payload?.type === "terminal_input")).toHaveLength(0);
  });

  test("input opens only after the authoritative list is applied", async () => {
    const h = harness();
    await h.attachment.attach();
    const msg = listed(h.sent.at(-1).requestId);
    expect(h.attachment.acceptListed(msg)).toBe(true);
    // The gate is still closed: the list has not been applied yet.
    expect(h.client.command(inputFrame())).toBeNull();
    expect(h.attachment.state).toBe("attaching");
    h.attachment.listedApplied(msg);
    expect(h.attachment.state).toBe("ready");
    expect(h.client.command(inputFrame())).toBe("tm-2");
    expect(h.sent.at(-1).payload).toEqual(inputFrame());
  });

  test("a deferred list keeps every user command closed", async () => {
    const h = harness();
    let release;
    const deferred = new Promise((resolve) => {
      release = resolve;
    });
    h.queryContext.mockImplementation(async () => {
      await deferred;
      return { ...TARGET };
    });
    const pending = h.attachment.attach();
    // Nothing bound yet: not even a list, let alone a keystroke.
    expect(h.client.command(inputFrame())).toBeNull();
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
    expect(h.sent).toHaveLength(0);
    release();
    await pending;
    // The controlled initial list is the only frame so far.
    expect(h.sent.map((f) => f.payload.type)).toEqual(["terminal_list"]);
    expect(h.client.command(inputFrame())).toBeNull();
  });

  test("a context reply that belongs to another page blocks instead of binding", async () => {
    const h = harness({ query: async () => ({ ...TARGET, workspaceId: "w-other" }) });
    await h.attachment.attach();
    expect(h.attachment.state).toBe("blocked");
    expect(h.attachment.binding).toBeNull();
    expect(h.client.binding).toBeNull();
    // Blocked means no list was requested for the wrong page.
    expect(h.sent.some((f) => f.payload?.type === "terminal_list")).toBe(false);
  });

  test("no live runtime is explicit unavailability and sends no terminal command", async () => {
    const h = harness({ query: async () => null });
    await h.attachment.attach();
    expect(h.attachment.state).toBe("unavailable");
    expect(h.attachment.binding).toBeNull();
    expect(h.sent).toHaveLength(0);
  });

  test("runtime_not_found is explicit unavailability, not a blocked binding", async () => {
    const h = harness({
      query: async () => {
        const error = new Error("runtime_not_found");
        error.code = "runtime_not_found";
        throw error;
      },
    });
    await h.attachment.attach();
    expect(h.attachment.state).toBe("unavailable");
    expect(h.states.at(-1).detail).toEqual({ reason: "runtime" });
    expect(h.sent).toHaveLength(0);
  });

  test("a bootstrap failure blocks and does not manufacture a binding", async () => {
    const h = harness({
      query: async () => {
        throw new Error("runtime_not_found");
      },
    });
    await h.attachment.attach();
    expect(h.attachment.state).toBe("blocked");
    expect(h.sent).toHaveLength(0);
  });

  test("only the attempt's own list enables input", async () => {
    const h = harness();
    await h.attachment.attach();
    // A late list from a previous attempt (different requestId) is refused.
    expect(h.attachment.acceptListed(listed("tm-999999"))).toBe(false);
    expect(h.attachment.state).toBe("attaching");
    const own = h.sent.at(-1).requestId;
    const msg = listed(own);
    expect(h.attachment.acceptListed(msg)).toBe(true);
    h.attachment.listedApplied(msg);
    // Once ready, ordinary refreshes still apply — for the current epoch only,
    // and only for an id this client actually issued.
    const issued = h.client.requestList();
    expect(h.attachment.acceptListed(listed(issued))).toBe(true);
    expect(h.attachment.acceptListed(listed("tm-999999"))).toBe(false);
  });
});

describe("RemoteTerminalAttachment invalidation", () => {
  test("disconnect invalidates immediately and blocks every command", async () => {
    const h = harness();
    await ready(h);
    h.sent.length = 0;

    h.attachment.notifyDisconnect();

    expect(h.attachment.state).toBe("disconnected");
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
    expect(
      h.client.command({ type: "terminal_close", terminalId: "t1", generation: 1 }),
    ).toBeNull();
    expect(h.sent).toHaveLength(0);
  });

  test("a context reply from an older connection cannot install a binding", async () => {
    let release;
    const h = harness({
      query: () =>
        new Promise((resolve) => {
          release = () => resolve({ ...TARGET });
        }),
    });
    const pending = h.attachment.attach();
    h.bumpEpoch(); // reconnect while the query is in flight
    release();
    await pending;
    expect(h.attachment.state).toBe("attaching");
    expect(h.attachment.binding).toBeNull();
    expect(h.client.binding).toBeNull();
  });

  test("reconnect resolves fresh context before requesting a list", async () => {
    const h = harness();
    await ready(h);
    h.attachment.notifyDisconnect();
    h.sent.length = 0;
    h.queryContext.mockClear();

    await h.attachment.attach();

    expect(h.queryContext).toHaveBeenCalledTimes(1);
    expect(h.sent[0].payload).toEqual({ type: "terminal_list" });
  });

  test("duplicate readiness notifications coalesce into one initial list", async () => {
    const h = harness();
    await h.attachment.attach();
    await h.attachment.notifyConnection();
    await h.attachment.notifyConnection();
    expect(h.sent.filter((f) => f.payload?.type === "terminal_list")).toHaveLength(1);
  });

  test("a list from an invalidated attempt never rebuilds tabs (R1)", async () => {
    const h = harness();
    await ready(h);
    const staleReadyRefresh = h.client.requestList();
    h.attachment.notifyDisconnect();
    expect(h.attachment.acceptListed(listed(staleReadyRefresh))).toBe(false);
    expect(h.attachment.ownsResponse({ requestId: staleReadyRefresh })).toBe(false);
  });

  test("an old create/close success cannot trigger a list after invalidation (R1)", async () => {
    const h = harness();
    await ready(h);
    const staleCreate = h.client.command({ type: "terminal_create", profileId: "default" });
    h.attachment.notifyDisconnect();
    expect(h.attachment.ownsResponse({ requestId: staleCreate })).toBe(false);
    // The gate is what the app uses before reacting to a success frame.
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("the matching runtime stop invalidates an attached binding", async () => {
    const h = harness();
    await ready(h);

    const matched = h.attachment.handleRuntimeEvent(STOPPED);

    expect(matched).toBe(true);
    expect(h.attachment.state).toBe("unavailable");
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("a stop from an older instance leaves the new binding alive", async () => {
    const h = harness();
    await ready(h);

    const matched = h.attachment.handleRuntimeEvent({
      event: { type: "runtime_crashed" },
      target: { ...TARGET, instanceId: "i-0" },
    });

    expect(matched).toBe(false);
    expect(h.attachment.state).toBe("ready");
    expect(h.client.binding).toEqual({
      workspaceId: "w-1",
      sessionId: "s-1",
      instanceId: "i-1",
    });
  });

  test("a same-instance event for another generation or owner leaves the binding (R4)", async () => {
    for (const target of [
      { ...TARGET, workspaceGeneration: 5 },
      { ...TARGET, ownerId: "owner-2" },
    ]) {
      const h = harness();
      await ready(h);
      const matched = h.attachment.handleRuntimeEvent({
        event: { type: "runtime_stopped" },
        target,
      });
      expect(matched).toBe(false);
      expect(h.attachment.state).toBe("ready");
    }
  });

  test("an event without full identity is never matched (R4)", async () => {
    const h = harness();
    await ready(h);
    const withoutOwner = { ...TARGET };
    delete withoutOwner.ownerId;
    expect(
      h.attachment.handleRuntimeEvent({ event: { type: "runtime_stopped" }, target: withoutOwner }),
    ).toBe(false);
    expect(h.attachment.state).toBe("ready");
  });

  test("a stop that lands while attaching makes the late context unusable", async () => {
    let release;
    const h = harness({
      query: () =>
        new Promise((resolve) => {
          release = () => resolve({ ...TARGET });
        }),
    });
    const pending = h.attachment.attach();
    // The runtime is reported stopped for this attempt before bootstrap lands.
    h.attachment.handleRuntimeEvent(STOPPED);
    release();
    await pending;

    expect(h.attachment.state).toBe("unavailable");
    expect(h.attachment.binding).toBeNull();
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("bootstrap held → stop → late list cannot enable input", async () => {
    let release;
    const h = harness({
      query: () =>
        new Promise((resolve) => {
          release = () => resolve({ ...TARGET });
        }),
    });
    const pending = h.attachment.attach();
    h.attachment.handleRuntimeEvent(STOPPED);
    release();
    await pending;
    // The attempt is dead: neither a late list for it nor a command reopens it.
    expect(h.attachment.acceptListed(listed("tm-1"))).toBe(false);
    expect(h.attachment.state).toBe("unavailable");
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("a late bootstrap for a moved identity is refused, a new instance is not", async () => {
    let release;
    const h = harness({
      query: () =>
        new Promise((resolve) => {
          release = () => resolve({ ...TARGET });
        }),
    });
    const pending = h.attachment.attach();
    // Same instance, new generation: the exit evidence belongs to generation 4
    // only, so the generation-5 identity the daemon now reports is not refused
    // by it — but this attempt's captured target (generation 4) is.
    h.attachment.handleRuntimeEvent({
      event: { type: "runtime_stopped" },
      target: { ...TARGET, workspaceGeneration: 5 },
    });
    release();
    await pending;
    expect(h.attachment.state).toBe("attaching");
  });
});

describe("RemoteTerminalAttachment rejection handling (B3)", () => {
  const cases = [
    ["terminal_protocol_unsupported", "unavailable"],
    ["terminal_runtime_unavailable", "unavailable"],
    ["terminal_binding_invalid", "blocked"],
    ["terminal_binding_mismatch", "blocked"],
  ];

  for (const [code, expected] of cases) {
    test(`${code} invalidates the binding without sending another frame`, async () => {
      const h = harness();
      await ready(h);
      const owned = h.client.requestList();
      h.sent.length = 0;

      expect(h.attachment.bindingRejected(code, owned)).toBe(true);

      expect(h.attachment.state).toBe(expected);
      expect(h.attachment.binding).toBeNull();
      expect(h.client.binding).toBeNull();
      // No projection, no automatic list: one explicit recovery action instead.
      expect(h.client.tabs.size).toBe(0);
      expect(h.sent).toHaveLength(0);
      expect(h.client.command({ type: "terminal_list" })).toBeNull();
    });
  }

  test("a rejection of an older epoch cannot block the current attachment", async () => {
    const h = harness();
    await ready(h);
    const stale = h.client.requestList();
    h.attachment.notifyDisconnect();
    await ready(h);
    h.sent.length = 0;

    expect(h.attachment.bindingRejected("terminal_binding_mismatch", stale)).toBe(false);
    expect(h.attachment.state).toBe("ready");
    expect(h.client.binding).not.toBeNull();
    expect(h.sent).toHaveLength(0);
  });
});

describe("RemoteTerminalAttachment transitions", () => {
  test("freeze → checkpoint → invalidation → commit keeps user input closed", async () => {
    const order = [];
    const h = harness();
    await ready(h);
    h.sent.length = 0;

    const freeze = h.client.freeze.bind(h.client);
    h.client.freeze = () => {
      order.push("freeze");
      freeze();
    };
    const invalidate = h.client.invalidateBinding.bind(h.client);
    h.client.invalidateBinding = () => {
      order.push("invalidate");
      invalidate();
    };

    const began = await h.attachment.beginTransition(async () => {
      order.push("checkpoint");
      // The controlled checkpoint is the only frame the freeze allows.
      h.client.command({
        type: "terminal_checkpoint",
        terminalId: "t1",
        generation: 1,
        watermark: 0,
        snapshotBase64: "",
      });
      // A keyboard keystroke during the transition must not escape.
      expect(h.client.command(inputFrame())).toBe(null);
    });
    expect(began).toBe(true);
    h.attachment.commitTransition();
    order.push("commit");

    expect(order).toEqual(["freeze", "checkpoint", "invalidate", "commit"]);
    expect(h.sent.map((f) => f.payload.type)).toEqual(["terminal_checkpoint"]);
    expect(h.attachment.state).toBe("disconnected");
    expect(h.client.command({ type: "terminal_list" })).toBeNull();
  });

  test("a cancelled transition unfreezes the binding that never changed", async () => {
    const h = harness();
    await ready(h);
    await h.attachment.beginTransition(async () => {});
    expect(h.client.command({ type: "terminal_list" })).toBeNull();

    h.attachment.cancelTransition();

    expect(h.client.command({ type: "terminal_list" })).not.toBeNull();
  });

  test("a refused checkpoint throws and restores the previous binding", async () => {
    const h = harness();
    await ready(h);
    await expect(
      h.attachment.beginTransition(async () => {
        throw new Error("checkpoint timed out");
      }),
    ).rejects.toThrow("checkpoint timed out");
    expect(h.attachment.transitioning).toBe(false);
    expect(h.client.command({ type: "terminal_list" })).not.toBeNull();
  });

  test("a failed commit after invalidation takes a fresh binding, not an unfreeze", async () => {
    const h = harness();
    await ready(h);
    await h.attachment.beginTransition(async () => {});
    h.attachment.commitTransition();
    h.sent.length = 0;
    h.queryContext.mockClear();

    const pending = h.attachment.cancelTransition();

    await pending;
    expect(h.queryContext).toHaveBeenCalledTimes(1);
    expect(h.attachment.state).toBe("attaching");
    expect(h.sent.map((f) => f.payload.type)).toEqual(["terminal_list"]);
  });
});

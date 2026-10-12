// ABOUTME: P2 history fold gate — mount batching, reveal, search interaction.
// ABOUTME: Drives renderSessionHistory through the app harness like the
// ABOUTME: canonical-snapshot tests, then asserts what mounted.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const wsInstances = [];

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    wsInstances.push(this);
    setTimeout(() => {
      this.readyState = FakeWebSocket.OPEN;
      this.onopen?.();
    }, 0);
  }

  reply(frame) {
    setTimeout(() => this.onmessage?.({ data: JSON.stringify(frame) }), 0);
  }

  send(raw) {
    const envelope = JSON.parse(raw);
    if (envelope.type === "hello") {
      this.reply({ type: "hello_ack", protocolVersion: 2 });
      return;
    }
    if (envelope.type === "runtime_subscribe") {
      this.reply({ type: "runtime_subscribed", requestId: envelope.requestId });
      return;
    }
    if (envelope.type === "host_request") {
      // The sidebar's registry load answers an empty registry, which
      // satisfies sessionsLoaded so mirror sync stops deferring.
      let response = null;
      if (envelope.operation === "workspace.list") {
        response = { workspaces: [], removed: [] };
      } else if (envelope.operation === "runtime_instances") {
        response = { instances: [] };
      }
      if (response) {
        this.reply({
          type: "host_response",
          requestId: envelope.requestId,
          ok: true,
          response,
        });
      }
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
  window.history.pushState(null, "", "/workspaces/w1/sessions/s1");
  document.documentElement.innerHTML = readFileSync(
    join(process.cwd(), "public/index.html"),
    "utf8",
  );
  const storage = new Map();
  const storageApi = {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: (key) => storage.delete(key),
  };
  vi.stubGlobal("localStorage", storageApi);
  vi.stubGlobal("sessionStorage", storageApi);
  globalThis.WebSocket = FakeWebSocket;
  globalThis.fetch = vi.fn(async (input) => {
    const url = String(input);
    if (url === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
    }
    if (url.startsWith("/v2/bootstrap")) {
      return new Response(
        JSON.stringify({ workspaceId: "w1", sessionId: "s1", instanceId: "primary" }),
      );
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  await initI18n();
  vi.spyOn(console, "debug").mockImplementation(() => {});
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "info").mockImplementation(() => {});
  globalThis.requestAnimationFrame = (callback) => callback();
  globalThis.ResizeObserver = class {
    observe() {}

    disconnect() {}
  };
  globalThis.ResizeObserver = class {
    observe() {}

    disconnect() {}
  };
  window.matchMedia = vi.fn(() => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
  }));
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.documentElement.innerHTML = "";
  delete globalThis.WebSocket;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
});

const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };

function makeTurnEntries(n) {
  const entries = [];
  for (let i = 0; i < n; i += 1) {
    entries.push({
      id: `u${i}`,
      parentId: i === 0 ? null : `a${i - 1}`,
      type: "message",
      message: { role: "user", content: `prompt ${i}` },
    });
    entries.push({
      id: `a${i}`,
      parentId: `u${i}`,
      type: "message",
      message: { role: "assistant", content: [{ type: "text", text: `answer ${i}` }] },
    });
  }
  return entries;
}

test("gate mounts the newest 2 turns plus one batch control; reveal mounts the next 2", async () => {
  const entries = makeTurnEntries(6);
  await import("./app.js?history-gate");

  const ws = wsInstances.at(-1);
  // Dispatch a foreground snapshot whose message list carries the entries
  // (the runtimeSnapshot handler maps bare messages into session entries).
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/s1.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  const userMessages = [...messages.querySelectorAll(".message.user")];
  const gate = messages.querySelector(".history-gate");

  // Newest 2 turns mounted; the gate control exists with the remaining count.
  expect(userMessages).toHaveLength(2);
  expect(userMessages[0].textContent).toContain("prompt 4");
  expect(userMessages[1].textContent).toContain("prompt 5");
  expect(gate).not.toBeNull();
  expect(gate.textContent).toContain("4");

  // P1: history turns render through the same turn-section contract as the
  // live stream — one <section class="turn" data-turn-id> per mounted turn,
  // minus the status header (logs carry no run duration). The section is the
  // rail registry's mounted anchor.
  const sections = [...messages.querySelectorAll("section.turn")];
  expect(sections).toHaveLength(2);
  for (const section of sections) {
    expect(section.dataset.turnId).toBeTruthy();
    expect(section.querySelector(".turn-status")).toBeNull();
    expect(section.querySelector(".turn-rail")).not.toBeNull();
    expect(section.querySelector(".turn-answer")).not.toBeNull();
  }

  // P2 interaction: with the gate closed the rail registry lists EVERY turn
  // of the session, not the mounted subset — folded turns carry previews with
  // a null anchor until revealed.
  const ticks = document.querySelectorAll("#conv-nav-track .conv-nav-dot");
  expect(ticks.length).toBeGreaterThan(2); // windowed, but sourced from all 6
  const railLabels = [...ticks].map((tick) => tick.getAttribute("aria-label"));
  expect(railLabels.some((label) => label.includes("prompt 0"))).toBe(true);

  // "Load older history" mounts the next 2 in order, anchored above.
  const scrollTopBefore = messages.scrollTop;
  gate.querySelector(".history-gate-btn").click();
  await new Promise((resolve) => setTimeout(resolve, 10));
  const usersAfter = [...messages.querySelectorAll(".message.user")];
  expect(usersAfter).toHaveLength(4);
  expect(usersAfter[0].textContent).toContain("prompt 2");
  expect(messages.scrollTop).toBeGreaterThanOrEqual(scrollTopBefore);
  // Control remains with the updated count.
  expect(messages.querySelector(".history-gate").textContent).toContain("2");

  // "Load all history" mounts every remaining turn.
  messages.querySelector(".history-gate-all").click();
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(6);
  expect(messages.querySelector(".history-gate")).toBeNull();
});

test("a search render mounts all turns before highlighting; the gate re-applies next plain render", async () => {
  const entries = makeTurnEntries(5);
  await import("./app.js?history-gate-search");
  const ws = wsInstances.at(-1);

  const snapshot = (messages, seq, sessionFile = "/pi/sessions/s2.jsonl") =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence: seq,
        target,
        state: { pi: { sessionFile, isStreaming: false }, messages },
      }),
    });

  snapshot(
    entries.map((e) => e.message),
    1,
  );
  await new Promise((resolve) => setTimeout(resolve, 300));
  const messages = document.getElementById("messages");
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);
  expect(messages.querySelector(".history-gate")).not.toBeNull();

  // The real search path: the sidebar search input drives sidebar.searchQuery
  // (via setupSidebarSearchControl), and the next transcript render passes it
  // into renderSessionHistory — the same chain production uses.
  const searchInput = document.getElementById("session-search-input");
  searchInput.value = "1";
  searchInput.dispatchEvent(new Event("input", { bubbles: true }));
  snapshot(
    entries.map((e) => e.message),
    2,
  );
  await new Promise((resolve) => setTimeout(resolve, 300));

  // (a) The search render mounts every folded turn before highlighting, so
  // old matches are findable; highlights are applied to the mounted DOM.
  const users = [...messages.querySelectorAll(".message.user")];
  expect(users).toHaveLength(5);
  expect(users.some((el) => el.textContent.includes("prompt 1"))).toBe(true);
  expect(messages.querySelectorAll("mark[data-search-highlight='true']").length).toBeGreaterThan(0);

  // (b) Clearing the query and re-rendering restores the gate: newest 2
  // mounted plus one batch control (forceReset from the search render).
  searchInput.value = "";
  searchInput.dispatchEvent(new Event("input", { bubbles: true }));
  snapshot(
    entries.map((e) => e.message),
    3,
  );
  await new Promise((resolve) => setTimeout(resolve, 300));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);
  expect(messages.querySelector(".history-gate")).not.toBeNull();

  // A session switch resets the reveal state.
  const otherEntries = makeTurnEntries(4);
  window.history.pushState(null, "", "/workspaces/w1/sessions/s9");
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 4,
      target: { workspaceId: "w1", sessionId: "s9", instanceId: "primary" },
      state: {
        pi: { sessionFile: "/pi/sessions/s9.jsonl", isStreaming: false },
        messages: otherEntries.map((e) => e.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);
});

test("history turns render the user row above the rail and the answer", async () => {
  // Regression: the turn section is built rail → answer, so a user bubble simply
  // appended into it lands BELOW its own answer (observed on real sessions:
  // every turn read assistant-first, user-last). The bubble must be claimed
  // into the user slot, as the live path does.
  const entries = [
    {
      id: "u1",
      parentId: null,
      type: "message",
      message: { role: "user", content: "first prompt" },
    },
    {
      id: "a1",
      parentId: "u1",
      type: "message",
      message: {
        role: "assistant",
        content: [{ type: "toolCall", id: "t1", name: "bash", arguments: { command: "ls" } }],
      },
    },
    {
      id: "r1",
      parentId: "a1",
      type: "message",
      message: {
        role: "toolResult",
        toolCallId: "t1",
        content: [{ type: "text", text: "ok" }],
      },
    },
    {
      id: "a2",
      parentId: "r1",
      type: "message",
      message: { role: "assistant", content: [{ type: "text", text: "first answer" }] },
    },
  ];

  await import("./app.js?history-turn-order");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/order.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const turn = document.querySelector("#messages section.turn");
  expect(turn).not.toBeNull();
  expect([...turn.children].map((child) => child.className.split(" ")[0])).toEqual([
    "message",
    "process-details-group",
    "turn-answer",
  ]);
  expect(turn.querySelector(".message.user")?.textContent).toContain("first prompt");
  expect(turn.querySelector(".turn-answer")?.textContent).toContain("first answer");
});

test("turns revealed by the batch controls keep the user row above the answer", async () => {
  // Regression: the reveal path renders each turn into a DocumentFragment, so a
  // user-slot claim that requires an attached node silently does nothing there —
  // the first paint looked right while every batch-revealed turn read
  // assistant-first, user-last.
  const entries = makeTurnEntries(6);
  await import("./app.js?history-gate-reveal-order");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/reveal.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  const assertTurnOrder = () => {
    for (const turn of messages.querySelectorAll("section.turn")) {
      const first = turn.firstElementChild;
      expect(first.classList.contains("message")).toBe(true);
      expect(first.classList.contains("user")).toBe(true);
      const answer = turn.querySelector(".turn-answer");
      if (answer) {
        // 4 = DOCUMENT_POSITION_FOLLOWING: the answer follows the user row.
        expect(first.compareDocumentPosition(answer) & 4).toBeTruthy();
      }
    }
  };
  assertTurnOrder();

  messages.querySelector(".history-gate-btn")?.click(); // Load older history
  await new Promise((resolve) => setTimeout(resolve, 60));
  // The newly revealed turns must obey the same order as the first paint.
  expect(messages.querySelectorAll("section.turn").length).toBeGreaterThan(2);
  assertTurnOrder();

  messages.querySelector(".history-gate-all")?.click(); // Load all history
  await new Promise((resolve) => setTimeout(resolve, 120));
  assertTurnOrder();
});

test("auto-reveal mounts one batch per intersection and fills until the gate is exhausted", async () => {
  const entries = makeTurnEntries(6);
  let rafQueue = [];
  globalThis.requestAnimationFrame = (callback) => {
    rafQueue.push(callback);
    return rafQueue.length;
  };
  await import("./app.js?history-gate-auto-reveal");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/ar.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  const messages = document.getElementById("messages");
  const gate = messages.querySelector(".history-gate");
  expect(gate).not.toBeNull();
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);

  // One scroll arrival within the threshold reveals exactly one batch …
  messages.scrollTop = 40;
  messages.dispatchEvent(new Event("scroll"));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(4);
  expect(messages.querySelector(".history-gate").textContent).toContain("2");

  // … and while the scroller still cannot overflow (jsdom reports no
  // geometry), one rAF frame continues the chain until the gate is exhausted,
  // removing the control.
  const frame = rafQueue.splice(0);
  rafQueue = [];
  for (const callback of frame) callback();
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(6);
  expect(messages.querySelector(".history-gate")).toBeNull();

  // The exhausted gate drops its listener: further scrolls reveal nothing.
  messages.dispatchEvent(new Event("scroll"));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(6);
});

test("a search render disconnects the auto-reveal observer without re-observing", async () => {
  const entries = makeTurnEntries(5);
  await import("./app.js?history-gate-search-no-observe");
  const ws = wsInstances.at(-1);
  const snapshot = (messages, seq) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence: seq,
        target,
        state: {
          pi: { sessionFile: "/pi/sessions/sn.jsonl", isStreaming: false },
          messages,
        },
      }),
    });
  snapshot(
    entries.map((e) => e.message),
    1,
  );
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  const searchInput = document.getElementById("session-search-input");
  searchInput.value = "1";
  searchInput.dispatchEvent(new Event("input", { bubbles: true }));
  snapshot(
    entries.map((e) => e.message),
    2,
  );
  await new Promise((resolve) => setTimeout(resolve, 300));

  // The search render mounts everything (no gate) and disconnects the
  // auto-reveal listener: a scroll arrival reveals nothing further.
  expect(messages.querySelector(".history-gate")).toBeNull();
  const usersAfterSearch = messages.querySelectorAll(".message.user").length;
  messages.scrollTop = 0;
  messages.dispatchEvent(new Event("scroll"));
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(messages.querySelectorAll(".message.user").length).toBe(usersAfterSearch);
});

test("the jump-to-bottom button appears when scrolled up and hides at the bottom", async () => {
  await import("./app.js?scroll-bottom-btn");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/sb.jsonl", isStreaming: false },
        messages: [{ role: "user", content: "hello" }],
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  const button = document.getElementById("scroll-bottom-btn");
  expect(button.classList.contains("hidden")).toBe(true);

  // Fake scroll geometry: content far taller than the viewport, scrolled up.
  Object.defineProperty(messages, "scrollHeight", { configurable: true, value: 3000 });
  Object.defineProperty(messages, "clientHeight", { configurable: true, value: 800 });
  messages.scrollTop = 0;
  messages.dispatchEvent(new Event("scroll"));
  expect(button.classList.contains("hidden")).toBe(false);

  // Back at the bottom: the button hides again (badge logic untouched).
  messages.scrollTop = 2200;
  messages.dispatchEvent(new Event("scroll"));
  expect(button.classList.contains("hidden")).toBe(true);
});

test("a plain re-render with more turns (snapshot then disk) never re-gates", async () => {
  // Pi's compacted snapshot carries fewer turns than the session file (the
  // compaction drops pre-compaction messages, the disk read keeps them). The
  // snapshot render must not shrink the reveal count below the mount default,
  // or the disk render that follows mounts a gate for turns it just proved.
  const compacted = makeTurnEntries(1);
  const disk = makeTurnEntries(2);
  await import("./app.js?history-gate-regrow");
  const ws = wsInstances.at(-1);
  const snapshot = (entries, seq) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence: seq,
        target,
        state: {
          pi: { sessionFile: "/pi/sessions/s3.jsonl", isStreaming: false },
          messages: entries.map((entry) => entry.message),
        },
      }),
    });
  snapshot(compacted, 1);
  await new Promise((resolve) => setTimeout(resolve, 300));
  // The compacted snapshot itself renders ungated (1 turn).
  expect(document.getElementById("messages").querySelector(".history-gate")).toBeNull();

  snapshot(disk, 2);
  await new Promise((resolve) => setTimeout(resolve, 300));
  const messages = document.getElementById("messages");
  expect(messages.querySelector(".history-gate")).toBeNull();
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);
});

test("a leading session system row is not a foldable turn", async () => {
  // Pi sessions open with a system entry the renderer never draws. Counting
  // it as its own turn makes every session gate one empty section.
  const entries = [
    {
      id: "sys",
      parentId: null,
      type: "message",
      message: { role: "system", content: "session header" },
    },
    ...makeTurnEntries(2),
  ];
  await import("./app.js?history-gate-system-row");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/s4.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  const messages = document.getElementById("messages");
  expect(messages.querySelector(".history-gate")).toBeNull();
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(2);
  expect(messages.querySelectorAll("section.turn")).toHaveLength(2);
});

test("revealed turns mount below the control so it stays the transcript's first element", async () => {
  // The gate is the transcript's top anchor: loading older turns inserts them
  // directly below it, so a reader who scrolled up to the newly mounted
  // history finds the control (and its count) right above, not buried under
  // the turns it just revealed.
  const entries = makeTurnEntries(6);
  await import("./app.js?history-gate-anchor-below");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/s5.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  expect(messages.querySelector(".history-gate")).not.toBeNull();

  messages.querySelector(".history-gate-btn")?.click(); // Load older history
  await new Promise((resolve) => setTimeout(resolve, 60));

  const gate = messages.querySelector(".history-gate");
  expect(messages.firstElementChild).toBe(gate);
  // The batch lands immediately below the control, ahead of the turns that
  // were already mounted (turns 4-5), so the transcript still reads
  // oldest → newest and the control keeps its place at the very top.
  expect(gate.nextElementSibling.classList.contains("turn")).toBe(true);
  expect([...messages.querySelectorAll(".message.user")].map((el) => el.textContent)).toEqual([
    "prompt 2",
    "prompt 3",
    "prompt 4",
    "prompt 5",
  ]);
});

test("the reveal chain stops once the transcript fills the viewport", async () => {
  // The control is anchored at the top, so it never leaves the trigger zone
  // after a batch: intersection can no longer signal "the viewport is still
  // empty". The chain must stop on geometry instead, or one arrival at the
  // top would drain the whole gate.
  const entries = makeTurnEntries(6);
  let rafQueue = [];
  globalThis.requestAnimationFrame = (callback) => {
    rafQueue.push(callback);
    return rafQueue.length;
  };
  await import("./app.js?history-gate-chain-overflow");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/s6.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));

  const messages = document.getElementById("messages");
  // Fake scroll geometry: content taller than the viewport from the start.
  Object.defineProperty(messages, "scrollHeight", { configurable: true, value: 3000 });
  Object.defineProperty(messages, "clientHeight", { configurable: true, value: 800 });

  messages.scrollTop = 40;
  messages.dispatchEvent(new Event("scroll"));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(4);

  const frame = rafQueue.splice(0);
  rafQueue = [];
  for (const callback of frame) callback();

  // The viewport is full: the chain stops and the gate keeps its remaining
  // count for the next scroll-up.
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(4);
  expect(messages.querySelector(".history-gate").textContent).toContain("2");
});

test("a revealed batch compensates the scroll so the reading position holds", async () => {
  // The batch lands between the anchored control and the turns the reader is
  // looking at, so the insert must be compensated: without it the reader's
  // content jumps down by the batch height. Forcing the scroller back to the
  // control instead (the previous settle) is the other failure mode — a
  // reveal may load, but it must never scroll the view.
  const entries = makeTurnEntries(6);
  let rafQueue = [];
  globalThis.requestAnimationFrame = (callback) => {
    rafQueue.push(callback);
    return rafQueue.length;
  };
  await import("./app.js?history-gate-anchor-hold");
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: "/pi/sessions/s8.jsonl", isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  // jsdom never fires rAFs: drain the render-time ones so the flush below
  // only runs the reveal chain's continuations.
  const stale = rafQueue.splice(0);
  rafQueue = [];
  for (const callback of stale) callback();

  const messages = document.getElementById("messages");
  // Geometry tracks the mounted turns, so an insert has a real height.
  Object.defineProperty(messages, "scrollHeight", {
    configurable: true,
    get: () => messages.querySelectorAll("section.turn").length * 500,
  });
  Object.defineProperty(messages, "clientHeight", { configurable: true, value: 800 });
  const turnsBefore = messages.querySelectorAll("section.turn").length;
  expect(turnsBefore).toBe(2);
  // A reader arriving at the gate: inside the 96px trigger zone.
  messages.scrollTop = 40;

  messages.dispatchEvent(new Event("scroll"));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(4);
  const inserted = (messages.querySelectorAll("section.turn").length - turnsBefore) * 500;

  // Compensated by exactly the inserted height: the content in view holds
  // still, and nothing jumps the view back to the control.
  expect(inserted).toBe(1000);
  expect(messages.scrollTop).toBe(40 + inserted);
  expect(messages.scrollTop).not.toBe(96 + 32);

  const frame = rafQueue.splice(0);
  rafQueue = [];
  for (const callback of frame) callback();

  // The chain stopped (viewport full) and the compensation left room to
  // scroll up: the next arrival loads another batch.
  expect(messages.querySelector(".history-gate").textContent).toContain("2");
  messages.scrollTop = 30;
  messages.dispatchEvent(new Event("scroll"));
  expect([...messages.querySelectorAll(".message.user")]).toHaveLength(6);
});

// ── History turn-files card: the written-path list rebuilt from the log ─────
// (Drives the same renderSessionHistory pass as the gate tests above.)

function historyTurn(source, toolCalls, answerText) {
  const entries = [
    {
      id: `${source}-u`,
      parentId: null,
      type: "message",
      message: { role: "user", content: `${source} prompt` },
    },
  ];
  for (const call of toolCalls) {
    entries.push({
      id: `${source}-${call.id}-call`,
      parentId: entries.at(-1).id,
      type: "message",
      message: {
        role: "assistant",
        content: [{ type: "toolCall", id: call.id, name: call.name, arguments: call.arguments }],
      },
    });
    entries.push({
      id: `${source}-${call.id}-result`,
      parentId: entries.at(-1).id,
      type: "message",
      message: {
        role: "toolResult",
        toolCallId: call.id,
        toolName: call.name,
        content: [{ type: "text", text: call.isError ? "failed" : "ok" }],
        details: call.details,
        isError: call.isError === true,
      },
    });
  }
  entries.push({
    id: `${source}-a`,
    parentId: entries.at(-1).id,
    type: "message",
    message: { role: "assistant", content: [{ type: "text", text: answerText }] },
  });
  return entries;
}

async function renderHistoryEntries(entries, sessionFile) {
  const ws = wsInstances.at(-1);
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 1,
      target,
      state: {
        pi: { sessionFile: `/pi/sessions/${sessionFile}.jsonl`, isStreaming: false },
        messages: entries.map((entry) => entry.message),
      },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
}

test("a line-based multi-file edit lists all four files in history", async () => {
  await import("./app.js?history-files-multi");
  const files = [
    "agents/research/clinical-medical-manager.md",
    "agents/research/clinical-medical-writer.md",
    "AGENTS.md",
    "README.md",
  ];
  await renderHistoryEntries(
    historyTurn(
      "multi",
      [
        {
          id: "t1",
          name: "edit",
          arguments: { text: "[AGENTS.md]\n@REPLACE\n-x\n+y" },
          details: { files: files.map((path) => ({ path, kind: "update" })) },
        },
      ],
      "four files updated",
    ),
    "history-files-multi",
  );

  const card = document.getElementById("messages").querySelector(".turn-files-card");
  expect(card).not.toBeNull();
  const rows = [...card.querySelectorAll(".turn-files-row")];
  expect(rows.map((row) => row.dataset.path)).toEqual(files);
  // History cards carry no frozen stats (spec Q3) and no degradation note.
  expect(card.querySelector(".turn-files-note")).toBeNull();
  expect(card.querySelector(".turn-files-badge--modified")).toBeNull();
});

test("history keeps first-write order, dedupes repeats, and drops failed edits", async () => {
  await import("./app.js?history-files-order");
  await renderHistoryEntries(
    historyTurn(
      "order",
      [
        {
          id: "t1",
          name: "edit",
          arguments: { text: "[docs/a.md]" },
          details: { files: [{ path: "docs/a.md" }, { path: "docs/b.md" }] },
        },
        { id: "t2", name: "write", arguments: { path: "docs/b.md" }, details: {} },
        {
          id: "t3",
          name: "edit",
          arguments: { text: "[docs/c.md]" },
          details: { files: [{ path: "docs/c.md" }] },
          isError: true,
        },
      ],
      "done",
    ),
    "history-files-order",
  );

  const rows = [...document.querySelectorAll(".turn-files-row")];
  expect(rows.map((row) => row.dataset.path)).toEqual(["docs/a.md", "docs/b.md"]);
});

test("a successful subagent fan-out lists its changedFiles in history", async () => {
  await import("./app.js?history-files-subagent");
  const files = [
    "tmp/fddw_rwe_proposal/D2-研究提案客户沟通版-v0.2.pptx",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/deck.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_deck.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/check_report.json",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/build_pptx.sh",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/gen_notes.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_corr.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/qa_spill.py",
    "tmp/fddw_rwe_proposal/deck-客户版-v0.2/tools/corr_report.json",
  ];
  await renderHistoryEntries(
    historyTurn(
      "subagent",
      [
        {
          id: "sub1",
          name: "subagent",
          arguments: {
            agent: "PPTX助手",
            async: false,
            cwd: "/Users/linyong/tmp/datarx-picot-ext",
            task: "续做客户沟通版 v0.2",
          },
          details: {
            results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: files } } }],
          },
        },
      ],
      "fan-out done",
    ),
    "history-files-subagent",
  );

  const card = document.getElementById("messages").querySelector(".turn-files-card");
  expect(card).not.toBeNull();
  const rows = [...card.querySelectorAll(".turn-files-row")];
  expect(rows.map((row) => row.dataset.path)).toEqual(files);
  // History cards carry no frozen stats (spec Q3) and no degradation note.
  expect(card.querySelector(".turn-files-note")).toBeNull();
});

test("history ignores a failed subagent fan-out", async () => {
  await import("./app.js?history-files-subagent-failed");
  await renderHistoryEntries(
    historyTurn(
      "subagent-failed",
      [
        {
          id: "sub2",
          name: "subagent",
          arguments: { agent: "PPTX助手", cwd: "/ws" },
          details: {
            results: [{ exitCode: 1, acceptance: { childReport: { changedFiles: ["tmp/x.md"] } } }],
          },
          isError: true,
        },
      ],
      "failed",
    ),
    "history-files-subagent-failed",
  );

  expect(document.getElementById("messages").querySelector(".turn-files-card")).toBeNull();
});

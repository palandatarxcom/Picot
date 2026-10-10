// ABOUTME: Integration coverage for returning to a session whose runtime is
// ABOUTME: blocked on a questionnaire — the card must come back on screen.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

vi.mock("./session/onboarding.js", () => ({
  getOnboardingState: () => ({
    canQuery: true,
    canType: true,
    needsProject: false,
    needsModel: false,
    message: "",
  }),
}));

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const wsInstances = [];
let snapshots = new Map(); // native sessionId -> { sessionFile, streaming }
let preparedTargets = new Map(); // sessionPath -> native sessionId
let instancesData = [];
let sequence = 0;

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
    wsInstances.push(this);
    setTimeout(() => {
      this.readyState = FakeWebSocket.OPEN;
      this.onopen?.();
    }, 0);
  }

  reply(frame) {
    setTimeout(() => this.onmessage?.({ data: JSON.stringify(frame) }), 0);
  }

  snapshotFor(target, requestId = null) {
    const entry = snapshots.get(target?.sessionId) || {};
    this.reply({
      type: "runtime_snapshot",
      protocolVersion: 2,
      requestId,
      sequence: ++sequence,
      target,
      state: {
        lifecycle: entry.lifecycle || (entry.streaming ? "Working" : "Idle"),
        pi: {
          sessionFile: entry.sessionFile || null,
          isStreaming: Boolean(entry.streaming),
          sessionId: target?.sessionId || null,
          model: { provider: "anthropic", id: "claude-test" },
        },
        messages: entry.messages || [],
      },
    });
  }

  send(raw) {
    const envelope = JSON.parse(raw);
    this.sent.push(envelope);
    if (envelope.type === "hello") {
      this.reply({ type: "hello_ack", protocolVersion: 2 });
      return;
    }
    if (envelope.type === "runtime_subscribe") {
      this.reply({ type: "runtime_subscribed", requestId: envelope.requestId });
      return;
    }
    if (envelope.type === "runtime_snapshot_request") {
      this.snapshotFor(envelope.target, envelope.requestId);
      return;
    }
    if (envelope.type === "data_request") {
      this.reply({
        type: "data_response",
        requestId: envelope.requestId,
        ok: true,
        messages: [],
      });
      return;
    }
    if (envelope.type === "runtime_request") {
      const data = envelope.command?.type === "get_commands" ? { commands: [] } : {};
      this.reply({
        type: "runtime_response",
        requestId: envelope.requestId,
        response: { success: true, data },
      });
      return;
    }
    if (envelope.type === "host_request") {
      let response = null;
      if (envelope.operation === "workspace.list") response = { workspaces: [], removed: [] };
      else if (envelope.operation === "runtime_instances") response = { instances: instancesData };
      else if (envelope.operation === "workspace_target_prepare") {
        const sessionId = preparedTargets.get(envelope.args?.sessionPath) || null;
        response = {
          transitionGeneration: 1,
          targetWorkspaceId: "w1",
          targetSessionId: sessionId,
          targetOrigin: globalThis.location.origin,
        };
      } else if (envelope.operation === "workspace_transition_commit") response = {};
      if (response) {
        this.reply({ type: "host_response", requestId: envelope.requestId, ok: true, response });
      }
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

const S1 = "/pi/sessions/s1.jsonl";
const S2 = "/pi/sessions/s2.jsonl";
const TARGET_S1 = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
const TARGET_S2 = { workspaceId: "w1", sessionId: "s2", instanceId: "secondary" };
// Two logged turns: the first finished with an answer, the second is the run
// that is still going (thinking only) when the session comes back to the front.
const HISTORY_MESSAGES = [
  { role: "user", content: "prompt 0" },
  {
    role: "assistant",
    content: [
      { type: "thinking", thinking: "thought 0" },
      { type: "text", text: "answer 0" },
    ],
  },
  { role: "user", content: "prompt 1" },
  { role: "assistant", content: [{ type: "thinking", thinking: "thought 1" }] },
];
// A tail turn whose ask_user_question call has no tool result: the log itself
// says this session is waiting on an answer.
const PENDING_QUESTION_MESSAGES = [
  { role: "user", content: "prompt q" },
  {
    role: "assistant",
    content: [
      { type: "thinking", thinking: "asking" },
      {
        type: "toolCall",
        id: "call-1",
        name: "ask_user_question",
        arguments: {
          questions: [{ question: "Which database?", options: ["Postgres", "SQLite"] }],
        },
      },
    ],
  },
];

beforeEach(async () => {
  wsInstances.length = 0;
  sequence = 0;
  snapshots = new Map([
    ["s1", { sessionFile: S1, streaming: true, messages: HISTORY_MESSAGES }],
    ["s2", { sessionFile: S2, streaming: false, messages: HISTORY_MESSAGES }],
  ]);
  preparedTargets = new Map([
    [S1, "s1"],
    [S2, "s2"],
  ]);
  instancesData = [
    { ...TARGET_S1, sessionFile: S1, streaming: true },
    { ...TARGET_S2, sessionFile: S2, streaming: false },
  ];
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
    if (url === "/locales/en.json") return new Response(JSON.stringify(enMessages));
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
  vi.spyOn(console, "warn").mockImplementation(() => {});
  globalThis.requestAnimationFrame = (callback) => callback();
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

const settle = (ms = 40) => new Promise((resolve) => setTimeout(resolve, ms));

function runtimeEvent(ws, target, event, seq = ++sequence) {
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_event",
      protocolVersion: 2,
      sequence: seq,
      target,
      event,
    }),
  });
}

function switchTo(filePath) {
  return globalThis.__picotSessionView.select({ filePath, cwd: "" }, { path: "" });
}

function visibleCard() {
  const card = document.querySelector(".questionnaire-card");
  if (!card) return null;
  const overlay = card.closest(".questionnaire-inline");
  return overlay && !overlay.closest(".hidden") ? card : null;
}

test("a question asked while the session is in the background is back on screen", async () => {
  await import("./app.js?switch-questionnaire-background");
  const ws = wsInstances.at(-1);
  await settle();

  // Session A is live; switch to B while its model is still working.
  await switchTo(S2);
  await settle();
  expect(visibleCard()).toBeNull();

  // A keeps running in the background and blocks on ask_user_question.
  runtimeEvent(ws, TARGET_S1, { type: "agent_start", turnId: "t1" }, 100);
  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "tool_execution_start",
      toolCallId: "tool-1",
      toolName: "ask_user_question",
      args: { questions: [{ question: "Which database?", options: ["Postgres", "SQLite"] }] },
    },
    101,
  );
  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "extension_ui_request",
      id: "q-1",
      method: "select",
      title: "Which database?",
      options: ["Postgres", "SQLite"],
    },
    102,
  );
  await settle();

  // Back to A: the runtime is still blocked, so the question must be askable.
  await switchTo(S1);
  await settle();
  expect(visibleCard()?.closest(".turn-card-slot")).not.toBeNull();
});

test("a questionnaire left open in a session is still on screen after returning", async () => {
  await import("./app.js?switch-questionnaire-parked");
  const ws = wsInstances.at(-1);
  await settle();

  runtimeEvent(ws, TARGET_S1, { type: "agent_start", turnId: "t2" }, 200);
  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "tool_execution_start",
      toolCallId: "tool-2",
      toolName: "ask_user_question",
      args: { questions: [{ question: "Which queue?", options: ["FIFO", "LIFO"] }] },
    },
    201,
  );
  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "extension_ui_request",
      id: "q-2",
      method: "select",
      title: "Which queue?",
      options: ["FIFO", "LIFO"],
    },
    202,
  );
  await settle();
  expect(visibleCard()).not.toBeNull();

  await switchTo(S2);
  await settle();
  expect(visibleCard()).toBeNull();
  await switchTo(S1);
  await settle();
  const returned = visibleCard();
  // The returning session's live turn hosts the card inline, never the modal
  // container the plain dialogs replace wholesale.
  expect(returned?.closest(".turn-card-slot")).not.toBeNull();
});

test("returning to a still-running session keeps a live turn on screen", async () => {
  await import("./app.js?switch-live-turn");
  const ws = wsInstances.at(-1);
  await settle();
  // Session A keeps working while the user is away; coming back must not look
  // like a finished transcript with a red stop button.
  await switchTo(S2);
  await settle();
  runtimeEvent(ws, TARGET_S1, { type: "agent_start", turnId: "t3" }, 300);
  await settle();

  await switchTo(S1);
  await settle();
  const status = document.querySelector(".turn-status.live");
  expect(status).not.toBeNull();
  expect(status.textContent).toContain("claude-test");
});

test("the adopted turn's clock starts at the first live output, not at adoption", async () => {
  await import("./app.js?switch-live-turn-clock");
  const ws = wsInstances.at(-1);
  await settle();
  await switchTo(S2);
  await settle();
  runtimeEvent(ws, TARGET_S1, { type: "agent_start", turnId: "t4" }, 400);
  await settle();

  await switchTo(S1);
  await settle();
  const status = document.querySelector(".turn-status.live");
  // Nothing of this run has been watched yet, and Pi reports no run start: an
  // elapsed readout here would be a number we cannot stand behind.
  expect(status.textContent).not.toMatch(/\d+s/);

  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "tool_execution_start",
      toolCallId: "tool-3",
      toolName: "read",
      args: { path: "/tmp/a" },
    },
    401,
  );
  await settle();
  expect(document.querySelector(".turn-status.live").textContent).toMatch(/\b0s\b/);
});

test("the newest turn's rail mounts expanded while older turns stay folded", async () => {
  await import("./app.js?switch-tail-rail");
  await settle();
  // Anchor on rail content: the transcript also carries the adopted live turn
  // (always expanded), so "the last toggle" would not be the history turn.
  const disclosureFor = (thought) => {
    const turn = [...document.querySelectorAll("section.turn")].find((section) =>
      section.querySelector(".process-details-body")?.textContent.includes(thought),
    );
    return turn?.querySelector(".process-details-toggle")?.getAttribute("aria-expanded") ?? null;
  };

  await switchTo(S1);
  await settle();
  expect(disclosureFor("thought 0")).toBe("false");
  expect(disclosureFor("thought 1")).toBe("true");

  // Unconditional on purpose: the newest turn is never folded, whether its run
  // is still going or already finished — a cached streaming flag can be wrong
  // exactly when the model is blocked on a question.
  await switchTo(S2);
  await settle();
  expect(disclosureFor("thought 0")).toBe("false");
  expect(disclosureFor("thought 1")).toBe("true");
});

test("a replayed blocking request rebuilds the card from the log alone", async () => {
  await import("./app.js?switch-log-questionnaire");
  const ws = wsInstances.at(-1);
  await settle();

  // The session's log holds the unanswered question; no park was ever filled
  // (this page never saw the original request), which is the reload /
  // cross-workspace case.
  snapshots.set("s1", { sessionFile: S1, streaming: true, messages: PENDING_QUESTION_MESSAGES });
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      protocolVersion: 2,
      sequence: 500,
      target: TARGET_S1,
      state: {
        lifecycle: "Working",
        pi: {
          sessionFile: S1,
          isStreaming: true,
          model: { provider: "anthropic", id: "claude-test" },
        },
        messages: PENDING_QUESTION_MESSAGES,
      },
    }),
  });
  await settle();
  expect(document.querySelector(".questionnaire-card")).toBeNull();

  // The host replays the still-pending dialog on (re)subscribe.
  runtimeEvent(
    ws,
    TARGET_S1,
    {
      type: "extension_ui_request",
      id: "replay-1",
      method: "select",
      title: "Which database?",
      options: ["Postgres", "SQLite"],
    },
    501,
  );
  await settle();

  const card = document.querySelector(".questionnaire-card");
  expect(card).not.toBeNull();
  expect(document.querySelector(".dialog-options")).toBeNull();

  card.querySelectorAll("input[type='radio']")[0].click();
  card.querySelector(".questionnaire-submit").click();
  await settle();
  process.stdout.write(
    `[diag] live-card=${Boolean(document.querySelector(".questionnaire-card"))} oldConnected=${card.isConnected} busy=${document.querySelector(".questionnaire-submit")?.getAttribute("aria-busy")} sent types=${JSON.stringify(ws.sent.map((f) => f.type))} connected=${ws.readyState}\n`,
  );
  // The response rides a runtime_request envelope, like every other send.
  const answers = ws.sent.filter((frame) => frame.command?.type === "extension_ui_response");
  expect(answers).toHaveLength(1);
  expect(answers[0].command.id).toBe("replay-1");
  expect(answers[0].command.value).toContain("Postgres");
});

test("the host state machine keeps the live turn when pi samples idle", async () => {
  await import("./app.js?switch-lifecycle-authority");
  await settle();

  // Pi's `isStreaming` is an instantaneous `!isIdle()` sample: a snapshot taken
  // between messages of a live run reports false. The host state machine says
  // Working, and that is what must decide.
  snapshots.set("s2", { sessionFile: S2, streaming: false, lifecycle: "Working" });
  await switchTo(S2);
  await settle();

  expect(document.querySelector(".turn-status.live")).not.toBeNull();
});

test("switching sessions sends the sidebar's scanned file path with the history read", async () => {
  await import("./app.js?switch-history-path");
  await settle();
  await switchTo(S2);
  // The sidebar knows the session's scanned JSONL path; the history read must
  // carry it so the host can skip its bucket rescan. The host re-validates the
  // hint, so sending it is a pure optimization, never an authority claim.
  const read = await vi.waitFor(() => {
    const frame = wsInstances
      .at(-1)
      .sent.find((f) => f.operation === "read_session_messages" && f.sessionId === "s2");
    expect(frame).toBeTruthy();
    return frame;
  });
  expect(read.sessionFile).toBe(S2);
  // Drain the switch's async sidebar warm before jsdom tears down: its
  // deferred render would otherwise fire after the environment is gone and
  // surface as an unhandled `document is not defined` rejection.
  await settle(120);
});

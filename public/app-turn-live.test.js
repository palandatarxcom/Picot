// ABOUTME: P1 live-turn behavior: a scripted event stream renders one turn
// ABOUTME: section whose projection (rail order, answer, status) matches the
// ABOUTME: structural contract history rendering produces for the same blocks.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const wsInstances = [];
// Paths handed to the host by the turn-files card's one frozen stats query.
const gitTurnStatsRequests = [];

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    wsInstances.push(this);
    // Async open so wsClient's onopen/onmessage assignments land first.
    setTimeout(() => {
      this.readyState = FakeWebSocket.OPEN;
      this.onopen?.();
    }, 0);
  }

  send(raw) {
    const envelope = JSON.parse(raw);
    // The turn-files card awaits git_turn_stats before mounting; answer it so
    // a scripted turn settles without waiting on a transport timeout.
    if (envelope.type === "host_request" && envelope.operation === "git_turn_stats") {
      const paths = envelope.args?.paths ?? [];
      gitTurnStatsRequests.push(paths);
      setTimeout(() => {
        this.onmessage?.({
          data: JSON.stringify({
            type: "host_response",
            requestId: envelope.requestId,
            ok: true,
            response: {
              files: paths.map((path) => ({ path, additions: 1, deletions: 0, status: "M" })),
            },
          }),
        });
      }, 0);
      return;
    }
    if (envelope.type === "runtime_request" && envelope.command?.type === "abort") {
      setTimeout(() => {
        this.onmessage?.({
          data: JSON.stringify({
            type: "runtime_response",
            requestId: envelope.requestId,
            response: { type: "response", command: "abort", success: true },
          }),
        });
      }, 0);
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
  gitTurnStatsRequests.length = 0;
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
    if (String(input) === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
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

test("a scripted live turn renders rail + answer + settled status in one section", async () => {
  await import("./app.js?turn-live-projection");

  const ws = wsInstances.at(-1);
  expect(ws).toBeTruthy();

  let sequence = 0;
  const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
  const send = (event, extra = {}) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_event",
        protocolVersion: 2,
        sequence: ++sequence,
        target,
        event,
        ...extra,
      }),
    });

  const assistantMessage = (text) => ({ role: "assistant", content: [{ type: "text", text }] });

  // ── The scripted sequence from the spec's verification note ──
  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: { role: "assistant", content: [{ type: "thinking", thinking: "pondering" }] },
    assistantMessageEvent: { type: "thinking_delta", delta: "pondering" },
  });
  send({
    type: "message_update",
    message: assistantMessage("Step one"),
    assistantMessageEvent: { type: "text_delta", delta: "Step one" },
  });
  send({ type: "message_end", message: assistantMessage("Step one"), entryId: "e1" });
  send({
    type: "tool_execution_start",
    toolCallId: "t1",
    toolName: "bash",
    args: { command: "ls" },
  });
  send({
    type: "tool_execution_end",
    toolCallId: "t1",
    result: { content: [{ type: "text", text: "done" }] },
    isError: false,
  });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("Final answer"),
    assistantMessageEvent: { type: "text_delta", delta: "Final answer" },
  });
  send({ type: "message_end", message: assistantMessage("Final answer"), entryId: "e2" });
  send({ type: "agent_end" });

  // Let any pending microtasks (async handlers) settle before asserting.
  await new Promise((resolve) => setTimeout(resolve, 20));

  const messages = document.getElementById("messages");
  const sections = messages.querySelectorAll("section.turn");
  expect(sections).toHaveLength(1);
  const section = sections[0];

  // Thinking renders ONCE, in the rail — never inside the answer's content
  // (the finalize path must not duplicate what the rail already owns).
  const thinkingBlocks = section.querySelectorAll(".thinking-block");
  expect(thinkingBlocks).toHaveLength(1);
  expect(section.querySelector(".turn-rail .thinking-block")).not.toBeNull();
  expect(section.querySelectorAll(".turn-answer .thinking-block")).toHaveLength(0);

  // Rail: thinking, then the demoted first text segment, then the tool card
  // (arrival order), styled like history rail rows.
  const railBody = section.querySelector(".turn-rail .process-details-body");
  const railChildren = [...railBody.children];
  expect(railChildren).toHaveLength(3);
  expect(railChildren[0].classList.contains("thinking-block")).toBe(true);
  expect(railChildren[0].textContent).toContain("pondering");
  expect(railChildren[1].classList.contains("assistant")).toBe(true);
  expect(railChildren[1].dataset.turnDemoted).toBe("true");
  expect(railChildren[1].classList.contains("history")).toBe(true);
  expect(railChildren[1].querySelector(".message-actions")).toBeNull();
  expect(railChildren[1].textContent).toContain("Step one");
  expect(railChildren[1].dataset.entryId).toBe("e1");
  expect(railChildren[2].classList.contains("tool-card")).toBe(true);
  expect(railChildren[2].dataset.toolCallId).toBe("t1");

  // Answer: the final assistant text stays in the answer slot with its toolbar.
  const answer = section.querySelector(".turn-answer");
  const answerMessages = answer.querySelectorAll(".message.assistant");
  expect(answerMessages).toHaveLength(1);
  expect(answerMessages[0].textContent).toContain("Final answer");
  expect(answerMessages[0].dataset.entryId).toBe("e2");
  expect(answerMessages[0].querySelector(".message-actions")).not.toBeNull();

  // Settled: the readout merges into the answer's toolbar as its last item and
  // the standalone status row is gone — a finished turn is one meta line.
  expect(section.querySelector(".turn-status")).toBeNull();
  const duration = section.querySelector(".turn-answer .message-actions .turn-duration");
  expect(duration).not.toBeNull();
  expect(duration.textContent).toContain("Worked for");
  expect(duration.parentElement.lastElementChild).toBe(duration);
  expect(section.querySelector(".turn-rail").classList.contains("expanded")).toBe(false);
  expect(section.querySelector(".process-details-label").textContent).toContain("2");
  expect(section.querySelector(".process-details-label").textContent).toContain("1");
});

test("abort mid-turn closes the live turn — settled without duration, rail folded, no live spinner", async () => {
  await import("./app.js?turn-live-abort");

  const ws = wsInstances.at(-1);
  let sequence = 0;
  const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
  const send = (event) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_event",
        protocolVersion: 2,
        sequence: ++sequence,
        target,
        event,
      }),
    });

  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: { role: "assistant", content: [{ type: "text", text: "partial work" }] },
    assistantMessageEvent: { type: "text_delta", delta: "partial work" },
  });
  await new Promise((resolve) => setTimeout(resolve, 20));

  const messages = document.getElementById("messages");
  let section = messages.querySelector("section.turn");
  expect(section).not.toBeNull();
  expect(section.querySelector(".turn-status").classList.contains("live")).toBe(true);

  // User abort: agent_end is delayed/missing — the click alone must close
  // the turn (no leftover live spinner or 1s status timer).
  document.getElementById("abort-btn").click();
  await new Promise((resolve) => setTimeout(resolve, 20));

  section = messages.querySelector("section.turn");
  expect(section).not.toBeNull();
  const status = section.querySelector(".turn-status");
  expect(status.classList.contains("live")).toBe(false);
  expect(status.classList.contains("settled")).toBe(true);
  // Aborted runs never completed: no duration is claimed.
  expect(status.textContent).not.toContain("Worked for");
  expect(section.querySelector(".turn-rail").classList.contains("expanded")).toBe(false);
});

test("a live turn's write tool settles a turn-files card under the answer", async () => {
  await import("./app.js?turn-live-files-card");

  const ws = wsInstances.at(-1);
  let sequence = 0;
  const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
  const send = (event, extra = {}) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_event",
        protocolVersion: 2,
        sequence: ++sequence,
        target,
        event,
        ...extra,
      }),
    });

  const assistantMessage = (text) => ({ role: "assistant", content: [{ type: "text", text }] });

  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("editing"),
    assistantMessageEvent: { type: "text_delta", delta: "editing" },
  });
  send({ type: "message_end", message: assistantMessage("editing"), entryId: "f1" });
  send({
    type: "tool_execution_start",
    toolCallId: "w1",
    toolName: "edit",
    args: { path: "public/style.css", old_string: "a", new_string: "b" },
  });
  send({
    type: "tool_execution_end",
    toolCallId: "w1",
    result: { content: [{ type: "text", text: "ok" }] },
    isError: false,
  });
  // Frames arrive over the wire, never in the same tick as their handler's
  // microtasks: let the write record land before the run continues.
  await new Promise((resolve) => setTimeout(resolve, 0));
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("Done"),
    assistantMessageEvent: { type: "text_delta", delta: "Done" },
  });
  send({ type: "message_end", message: assistantMessage("Done"), entryId: "f2" });
  send({ type: "agent_end" });

  await new Promise((resolve) => setTimeout(resolve, 50));

  const messages = document.getElementById("messages");
  const card = messages.querySelector(".turn-files-card");
  console.warn(
    `[files-card] present=${Boolean(card)} rows=${messages.querySelectorAll(".turn-files-row").length}`,
  );
  expect(card).not.toBeNull();
  expect(card.textContent).toContain("style.css");
});

test("a live line-based multi-file edit settles a 4-row card and one 4-path stats query", async () => {
  // Real shape (session 2026-10-03T10-21-58, edit call_7ae0f14f): args carry
  // only the line-editing script; the four written files exist solely on
  // result.details.files. Both the card and the single frozen git query must
  // see all four.
  await import("./app.js?turn-live-files-card-multi");

  const ws = wsInstances.at(-1);
  let sequence = 0;
  const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
  const send = (event) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_event",
        protocolVersion: 2,
        sequence: ++sequence,
        target,
        event,
      }),
    });

  const assistantMessage = (text) => ({ role: "assistant", content: [{ type: "text", text }] });
  const files = [
    "agents/research/clinical-medical-manager.md",
    "agents/research/clinical-medical-writer.md",
    "AGENTS.md",
    "README.md",
  ];

  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("editing"),
    assistantMessageEvent: { type: "text_delta", delta: "editing" },
  });
  send({ type: "message_end", message: assistantMessage("editing"), entryId: "m1" });
  send({
    type: "tool_execution_start",
    toolCallId: "e1",
    toolName: "edit",
    args: { text: "[AGENTS.md]\n@REPLACE\n-x\n+y" },
  });
  send({
    type: "tool_execution_end",
    toolCallId: "e1",
    result: { isError: false, details: { files: files.map((path) => ({ path, kind: "update" })) } },
    isError: false,
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("Done"),
    assistantMessageEvent: { type: "text_delta", delta: "Done" },
  });
  send({ type: "message_end", message: assistantMessage("Done"), entryId: "m2" });
  send({ type: "agent_end" });

  await new Promise((resolve) => setTimeout(resolve, 50));

  const messages = document.getElementById("messages");
  const rows = [...messages.querySelectorAll(".turn-files-row")];
  expect(gitTurnStatsRequests).toEqual([files]);
  expect(rows.map((row) => row.dataset.path)).toEqual(files);
});

test("a live successful subagent fan-out lists its changedFiles and queries their stats", async () => {
  // Real shape (session 2026-10-12T02-36-47, subagent call_88dc84bdf): the
  // nine written files exist only on details.results[0].acceptance.childReport.
  // changedFiles. The card and the single frozen git query must both see them.
  await import("./app.js?turn-live-subagent-files");

  const ws = wsInstances.at(-1);
  let sequence = 0;
  const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };
  const send = (event) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_event",
        protocolVersion: 2,
        sequence: ++sequence,
        target,
        event,
      }),
    });

  const assistantMessage = (text) => ({ role: "assistant", content: [{ type: "text", text }] });
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

  send({ type: "agent_start" });
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("delegating"),
    assistantMessageEvent: { type: "text_delta", delta: "delegating" },
  });
  send({ type: "message_end", message: assistantMessage("delegating"), entryId: "s1" });
  send({
    type: "tool_execution_start",
    toolCallId: "sub1",
    toolName: "subagent",
    args: { agent: "PPTX助手", async: false, cwd: "/Users/linyong/tmp/datarx-picot-ext" },
  });
  send({
    type: "tool_execution_end",
    toolCallId: "sub1",
    toolName: "subagent",
    result: {
      isError: false,
      details: { results: [{ exitCode: 0, acceptance: { childReport: { changedFiles: files } } }] },
    },
    isError: false,
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  send({ type: "message_start", message: { role: "assistant", content: [] } });
  send({
    type: "message_update",
    message: assistantMessage("Done"),
    assistantMessageEvent: { type: "text_delta", delta: "Done" },
  });
  send({ type: "message_end", message: assistantMessage("Done"), entryId: "s2" });
  send({ type: "agent_end" });

  await new Promise((resolve) => setTimeout(resolve, 50));

  const rows = [...document.getElementById("messages").querySelectorAll(".turn-files-row")];
  expect(gitTurnStatsRequests).toEqual([files]);
  expect(rows.map((row) => row.dataset.path)).toEqual(files);
});

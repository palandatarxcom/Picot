// ABOUTME: Steering/queue UX integration (2026-09-19 spec) — streaming Enter
// ABOUTME: sends a steer, queue pills come from queue_update, clear_queue is
// ABOUTME: reachable, Esc clears before aborting, and steer attachments stay
// ABOUTME: composer-owned until the correlated acceptance releases them.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

// The real processor decodes/resizes via canvas, which jsdom cannot run. The
// logic under test is the composer→C3 attachment ownership, not the codec.
vi.mock("./image-attachments.js", () => ({
  processImageFile: async (file) => ({ data: "FAKE-BASE64", mimeType: file?.type || "image/png" }),
  processImagePayload: async (payload) => ({
    data: payload?.data ?? "FAKE-BASE64",
    mimeType: payload?.mimeType || "image/png",
  }),
  isSupportedImageMime: () => true,
}));

// The composer only enables sending once Pi has reported configured models, and
// that list arrives through the picot-config bridge — outside this test's fake
// WebSocket. Pin the onboarding gate open so the send paths are reachable; the
// gate itself is covered by its own module test.
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
let clearQueueData = { steering: [], followUp: [] };
let clearQueueFails = false;
let commandRegistry = [];
let swallowClearQueue = false;
let promptFails = false;
let promptTooLarge = false;
let abortFails = false;
let deferPromptResponse = false;
let deferredPromptId = null;
let runtimeInstancesData = [];

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
    if (envelope.type === "host_request") {
      let response = null;
      if (envelope.operation === "workspace.list") response = { workspaces: [], removed: [] };
      else if (envelope.operation === "runtime_instances")
        response = { instances: runtimeInstancesData };
      if (response) {
        this.reply({ type: "host_response", requestId: envelope.requestId, ok: true, response });
      }
      return;
    }
    if (envelope.type === "runtime_request") {
      const command = envelope.command || {};
      if (command.type === "prompt" && deferPromptResponse) {
        deferredPromptId = envelope.requestId;
        return;
      }
      if (command.type === "prompt" && promptFails) {
        this.reply({
          type: "runtime_response",
          requestId: envelope.requestId,
          response: { success: false, error: "steer rejected by pi" },
        });
        return;
      }
      if (command.type === "prompt" && promptTooLarge) {
        // Mirror the host's oversized-frame rejection: a `type:"error"` frame
        // whose machine code survives the WS layer into the delivery record.
        this.reply({
          type: "error",
          requestId: envelope.requestId,
          error: {
            code: "command_too_large",
            message: "Runtime command exceeds protocol limit",
          },
        });
        return;
      }
      if (command.type === "clear_queue" && swallowClearQueue) {
        // Deliberately never answer: exercises the abort cap.
        return;
      }
      if (command.type === "clear_queue" && clearQueueFails) {
        this.reply({
          type: "runtime_response",
          requestId: envelope.requestId,
          response: { success: false, error: "clear_queue rejected" },
        });
        return;
      }
      if (command.type === "abort" && abortFails) {
        // A simulated stop failure (host or pi rejected it): pi never
        // aborted anything.
        this.reply({
          type: "runtime_response",
          requestId: envelope.requestId,
          response: { success: false, error: "Invalid runtime command: abort requires turnId" },
        });
        return;
      }
      if (command.type === "abort") {
        // pi replies once the session is idle: success carries the stop.
        this.reply({
          type: "runtime_response",
          requestId: envelope.requestId,
          response: { type: "response", command: "abort", success: true },
        });
        return;
      }
      const data =
        command.type === "get_commands"
          ? { commands: commandRegistry }
          : command.type === "clear_queue"
            ? clearQueueData
            : {};
      this.reply({
        type: "runtime_response",
        requestId: envelope.requestId,
        response: { success: true, data },
      });
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
  clearQueueData = { steering: [], followUp: [] };
  clearQueueFails = false;
  commandRegistry = [];
  swallowClearQueue = false;
  promptFails = false;
  promptTooLarge = false;
  abortFails = false;
  deferPromptResponse = false;
  deferredPromptId = null;
  runtimeInstancesData = [];
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
const settle = (ms = 40) => new Promise((resolve) => setTimeout(resolve, ms));

function runtimeEvent(ws, event, sequence = 1) {
  ws.onmessage({
    data: JSON.stringify({ type: "runtime_event", protocolVersion: 2, sequence, target, event }),
  });
}

const commandFrames = (ws, type) =>
  ws.sent.filter(
    (frame) =>
      frame.type === "runtime_request" &&
      frame.command?.type === type &&
      // The picot-config bridge rides the same `prompt` command type; it is
      // infrastructure traffic (config reads released by the readiness gate),
      // never part of the chat flows these assertions count.
      !String(frame.command.message ?? "").startsWith("/picot-config"),
  );

function typeIntoComposer(text) {
  const input = document.getElementById("message-input");
  input.value = text;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  return input;
}

function pressAltEnter() {
  document
    .getElementById("message-input")
    .dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", altKey: true, bubbles: true, cancelable: true }),
    );
}

function pressEnter() {
  document
    .getElementById("message-input")
    .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
}

function pasteImage() {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", {
    value: {
      // A real paste carries text accessors too; the paste-offload listener
      // reads them, so the stub must not be image-only.
      getData: () => "",
      files: [],
      items: [
        {
          type: "image/png",
          getAsFile: () => new File([new Uint8Array([1, 2, 3])], "shot.png", { type: "image/png" }),
        },
      ],
    },
  });
  document.getElementById("message-input").dispatchEvent(event);
}

function pendingPreviews() {
  return [...document.querySelectorAll("#image-previews .image-preview")];
}

function renderPiQueue(ws, steering, followUp, sequence) {
  runtimeEvent(ws, { type: "queue_update", steering, followUp }, sequence);
}

test("streaming Enter sends a steer prompt with no optimistic bubble", async () => {
  await import("./app.js?steering-enter");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  typeIntoComposer("switch to tabs");
  pressEnter();
  await settle();

  const steers = commandFrames(ws, "prompt").filter(
    (frame) => frame.command.streamingBehavior === "steer",
  );
  expect(steers).toHaveLength(1);
  expect(steers[0].command.message).toBe("switch to tabs");
  // The pill comes from queue_update, never from a hand-rendered bubble.
  expect(document.querySelectorAll("#messages .message.user")).toHaveLength(0);
});

test("idle Enter still sends a plain prompt without streamingBehavior", async () => {
  await import("./app.js?steering-idle");
  const ws = wsInstances.at(-1);
  await settle();

  typeIntoComposer("plain question");
  pressEnter();
  await settle();

  const prompts = commandFrames(ws, "prompt");
  expect(prompts).toHaveLength(1);
  expect(prompts[0].command.streamingBehavior).toBeUndefined();
});

test("queue_update renders read-only pills and clear_queue refills the composer", async () => {
  await import("./app.js?steering-clear");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["switch to tabs"], ["summarize"], 2);
  await settle();

  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);
  expect([...queueEl.querySelectorAll(".queued-msg-label")].map((el) => el.textContent)).toEqual([
    "Steer",
    "Follow-up",
  ]);
  // Per-item removal does not exist in the protocol, so rows carry no control.
  expect(queueEl.querySelectorAll(".queued-msg button")).toHaveLength(0);

  clearQueueData = { steering: ["switch to tabs"], followUp: ["summarize"] };
  queueEl.querySelector(".pi-queue-clear").click();
  await settle();

  // The P0 regression guard: the frame must actually reach the runtime.
  expect(commandFrames(ws, "clear_queue")).toHaveLength(1);
  const input = document.getElementById("message-input");
  expect(input.value).toContain("switch to tabs");
  expect(input.value).toContain("summarize");
  expect(document.getElementById("pi-queue").classList.contains("hidden")).toBe(true);
});

test("a rejected clear keeps the pills and leaves the composer untouched", async () => {
  await import("./app.js?steering-clear-fails");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["keep me queued"], [], 2);
  await settle();

  clearQueueFails = true;
  const input = document.getElementById("message-input");
  input.value = "";
  // rpcCommand logs the rejection; assert the exact expected line instead of
  // letting it pollute the run output.
  const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  document.getElementById("pi-queue").querySelector(".pi-queue-clear").click();
  await settle();

  expect(errorSpy).toHaveBeenCalledWith(
    "rpcCommand failed:",
    "clear_queue",
    "clear_queue rejected",
  );
  expect(commandFrames(ws, "clear_queue")).toHaveLength(1);
  // Nothing was cleared at pi, so nothing may claim it was.
  expect(input.value).toBe("");
  expect(document.getElementById("pi-queue").classList.contains("hidden")).toBe(false);
});

test("Escape aborts bare and leaves pi's queue alone", async () => {
  // Pi-native (2026-09-25, supersedes Q3-A): Esc aborts ONLY. Queued
  // steer/followUp stay at pi — it continues with them once the run
  // terminates — so no clear_queue and no composer restore. Pi's abort
  // command carries no turnId (rpc-commands.md); the host authorizes the
  // active operation and forwards it bare.
  await import("./app.js?steering-esc");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["hold on"], [], 1);
  typeIntoComposer("my draft");

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle(80);

  // Exactly one command: the bare abort. No clear_queue.
  expect(commandFrames(ws, "clear_queue")).toHaveLength(0);
  const aborts = commandFrames(ws, "abort");
  expect(aborts).toHaveLength(1);
  expect(aborts[0].command.turnId).toBeUndefined();
  // The queued text and the draft stay exactly where they were.
  expect(document.getElementById("message-input").value).toBe("my draft");
  // "run 真正终止" from the UI's side: back to idle (send visible, abort gone).
  expect(document.getElementById("abort-btn").classList.contains("hidden")).toBe(true);
  expect(document.getElementById("send-btn").classList.contains("hidden")).toBe(false);
  expect(document.getElementById("send-caret-btn").classList.contains("hidden")).toBe(true);
});

test("an extension command while streaming goes out as a bare prompt", async () => {
  commandRegistry = [{ name: "mycommand", source: "extension", description: "runs code" }];
  await import("./app.js?steering-extension");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  typeIntoComposer("/mycommand now");
  pressEnter();
  await settle();

  // rpc.md: extension commands execute immediately even mid-run, so the send
  // must stay a bare prompt — never a queued steer.
  const prompts = commandFrames(ws, "prompt");
  expect(prompts).toHaveLength(1);
  expect(prompts[0].command.message).toBe("/mycommand now");
  expect(prompts[0].command.streamingBehavior).toBeUndefined();
});

test("clearing appends the restored text below an existing draft", async () => {
  await import("./app.js?steering-refill-append");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["queued follow-up text"], [], 2);
  await settle();

  clearQueueData = { steering: ["queued follow-up text"], followUp: [] };
  const input = typeIntoComposer("draft I typed");
  document.getElementById("pi-queue").querySelector(".pi-queue-clear").click();
  await settle();

  expect(input.value).toBe("draft I typed\nqueued follow-up text");
});

test("Escape aborts bare even before any turn event arrives", async () => {
  // Streaming started but no further event arrived (edge): the bare abort
  // still goes out at once — nothing queues up in front of the stop.
  await import("./app.js?steering-esc-no-turn");
  const ws = wsInstances.at(-1);
  await settle();
  // Streaming, with no turn-scoped event yet.
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle(80);

  const aborts = commandFrames(ws, "abort");
  expect(aborts).toHaveLength(1);
  expect(aborts[0].command.turnId).toBeUndefined();
  expect(commandFrames(ws, "clear_queue")).toHaveLength(0);
});

test("Escape aborts immediately — nothing is sent ahead of the stop", async () => {
  // Pi-native (2026-09-25): the stop path sends only the abort, so no
  // clear_queue round-trip (and no cap) can delay it.
  await import("./app.js?steering-esc-cap");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle(80);

  expect(commandFrames(ws, "clear_queue")).toHaveLength(0);
  const aborts = commandFrames(ws, "abort");
  expect(aborts).toHaveLength(1);
  expect(aborts[0].command.turnId).toBeUndefined();
});

test("Alt+Enter while streaming queues a follow_up, not a steer", async () => {
  await import("./app.js?steering-alt-enter");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  typeIntoComposer("summarize when done");
  pressAltEnter();
  await settle();

  expect(commandFrames(ws, "follow_up")).toHaveLength(0);
  expect(commandFrames(ws, "prompt")).toHaveLength(0);
  // The composer clears and the item renders in the local queue area.
  expect(document.getElementById("message-input").value).toBe("");
  const row = document.querySelector("#queued-messages .followup-msg");
  expect(row).not.toBeNull();
  expect(row.textContent).toContain("summarize when done");
  // Persisted for a same-window reload (the anonymous key: no snapshot yet).
  expect(globalThis.localStorage.getItem("pi-studio:followup-queue:")).toContain(
    "summarize when done",
  );
});

test("a queued item can be edited back into the composer and deleted", async () => {
  await import("./app.js?followup-edit-delete");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  typeIntoComposer("first item");
  pressAltEnter();
  await settle();
  typeIntoComposer("second item");
  pressAltEnter();
  await settle();
  const rows = () => [...document.querySelectorAll("#queued-messages .followup-msg")];
  expect(rows()).toHaveLength(2);

  // Delete the second item: one row left, nothing ever sent.
  rows()[1].querySelector('[aria-label="Delete"]').click();
  await settle();
  expect(rows()).toHaveLength(1);
  expect(rows()[0].textContent).toContain("first item");
  expect(commandFrames(ws, "prompt")).toHaveLength(0);

  // Edit pulls the remaining item back into the composer and out of the queue.
  rows()[0].querySelector('[aria-label="Edit"]').click();
  await settle();
  expect(document.getElementById("message-input").value).toBe("first item");
  expect(rows()).toHaveLength(0);
});

test("send-now during a run goes out as a steer without aborting", async () => {
  await import("./app.js?followup-send-now");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  typeIntoComposer("jump the queue");
  pressAltEnter();
  await settle();

  document
    .querySelector("#queued-messages .followup-msg")
    .querySelector('[aria-label="Send now"]')
    .click();
  await settle();

  const steers = commandFrames(ws, "prompt").filter(
    (frame) => frame.command.streamingBehavior === "steer",
  );
  expect(steers).toHaveLength(1);
  expect(steers[0].command.message).toBe("jump the queue");
  expect(commandFrames(ws, "abort")).toHaveLength(0);
  expect(document.querySelectorAll("#queued-messages .followup-msg")).toHaveLength(0);
});

test("a rejected send-now returns the item to the head of the queue", async () => {
  // D6 (2026-09-26): a rejected drain/send-now re-queues at the head — the
  // item's text belongs to the queue, not the composer.
  await import("./app.js?followup-requeue");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  typeIntoComposer("will bounce");
  pressAltEnter();
  await settle();
  typeIntoComposer("stays queued");
  pressAltEnter();
  await settle();

  promptFails = true;
  document
    .querySelectorAll("#queued-messages .followup-msg")[0]
    .querySelector('[aria-label="Send now"]')
    .click();
  await settle();

  const rows = [...document.querySelectorAll("#queued-messages .followup-msg")];
  expect(rows).toHaveLength(2);
  // The rejected item is back at the head, ahead of the item that never left.
  expect(rows[0].textContent).toContain("will bounce");
  expect(rows[1].textContent).toContain("stays queued");
  // No composer dump: the draft stays exactly as it was.
  expect(document.getElementById("message-input").value).toBe("");
});

test("a rejected abort keeps the streaming UI honest instead of unlocking", async () => {
  // 2026-09-26 fix: the optimistic unlock painted a blue composer over a run
  // pi never stopped (a dropped or failed abort).
  await import("./app.js?abort-rejected");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  abortFails = true;
  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle(120);

  const aborts = commandFrames(ws, "abort");
  expect(aborts).toHaveLength(1);
  expect(aborts[0].command.turnId).toBeUndefined();
  // The stop was NOT confirmed: the run's UI must stay locked.
  expect(document.getElementById("abort-btn").classList.contains("hidden")).toBe(false);
  expect(document.getElementById("send-btn").classList.contains("hidden")).toBe(true);
});

test("agent_settled drains the queue head as a plain prompt, one at a time", async () => {
  await import("./app.js?followup-drain");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  typeIntoComposer("first follow-up");
  pressAltEnter();
  await settle();
  typeIntoComposer("second follow-up");
  pressAltEnter();
  await settle();
  expect(commandFrames(ws, "prompt")).toHaveLength(0);

  // The run ends: end + settled release exactly one item as a new prompt.
  runtimeEvent(ws, { type: "agent_end", messages: [] }, 2);
  runtimeEvent(ws, { type: "agent_settled" }, 3);
  await settle();

  const prompts = commandFrames(ws, "prompt").filter(
    (frame) => frame.command.streamingBehavior === undefined,
  );
  expect(prompts).toHaveLength(1);
  expect(prompts[0].command.message).toBe("first follow-up");
  // The second item waits for its own turn's settled.
  expect(document.querySelectorAll("#queued-messages .followup-msg")).toHaveLength(1);
  expect(document.querySelector("#queued-messages .followup-msg").textContent).toContain(
    "second follow-up",
  );
});

test("abort drains only one local follow-up and keeps next pill until its own settle", async () => {
  await import("./app.js?followup-abort-drain");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  typeIntoComposer("first follow-up");
  pressAltEnter();
  await settle();
  typeIntoComposer("second follow-up");
  pressAltEnter();
  await settle();
  expect(document.querySelectorAll("#queued-messages .followup-msg")).toHaveLength(2);

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle();
  runtimeEvent(ws, { type: "agent_end", messages: [] }, 2);
  runtimeEvent(ws, { type: "agent_settled" }, 3);
  await settle();
  expect(commandFrames(ws, "prompt").map((frame) => frame.command.message)).toEqual([
    "first follow-up",
  ]);
  expect(
    [...document.querySelectorAll("#queued-messages .followup-msg")].map((row) => row.textContent),
  ).toEqual([expect.stringContaining("second follow-up")]);
  expect(
    [...document.querySelectorAll("#messages .message.user")].filter((row) =>
      row.textContent.includes("second follow-up"),
    ),
  ).toHaveLength(0);
  runtimeEvent(
    ws,
    {
      type: "agent_start",
    },
    4,
  );
  runtimeEvent(
    ws,
    {
      type: "message_start",
      message: { role: "user", content: [{ type: "text", text: "first follow-up" }] },
    },
    5,
  );
  await settle();
  expect(
    [...document.querySelectorAll("#messages .message.user")].filter((row) =>
      row.textContent.includes("first follow-up"),
    ),
  ).toHaveLength(1);
});

test("a rejected steer keeps the run's streaming state", async () => {
  await import("./app.js?steering-steer-rejected");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  const abortBtn = document.getElementById("abort-btn");
  const typing = document.getElementById("typing-indicator");
  expect(abortBtn.classList.contains("hidden")).toBe(false);

  promptFails = true;
  typeIntoComposer("steer that pi rejects");
  pressEnter();
  await settle();

  expect(commandFrames(ws, "prompt")).toHaveLength(1);
  // The steer dispatch rides promptDelivery (not rpcCommand), so the failure
  // surfaces as a transcript error row carrying pi's message.
  const errorRow = document.querySelector("#messages .error-message");
  expect(errorRow).not.toBeNull();
  expect(errorRow.textContent).toContain("steer rejected by pi");
  // Q1-A: a rejection of a dispatch made MID-RUN must not end the run's
  // streaming state — the run is still going, only this steer failed.
  expect(abortBtn.classList.contains("hidden")).toBe(false);
  expect(typing.classList.contains("hidden")).toBe(false);
  // And the failed text comes back to the composer.
  expect(document.getElementById("message-input").value).toContain("steer that pi rejects");
});

test("a steer renders no optimistic bubble, and pi's echo still appears", async () => {
  await import("./app.js?steering-echo");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  typeIntoComposer("steer me");
  pressEnter();
  await settle();
  // Mid-run sends draw no bubble of their own (the pill comes from queue_update).
  expect(document.querySelectorAll("#messages .message.user")).toHaveLength(0);

  // Later pi delivers it and echoes the user message: with no `lastSentMessage`
  // accounting for steers, that echo is what puts the prompt in the transcript.
  runtimeEvent(
    ws,
    {
      type: "message_start",
      message: { role: "user", content: [{ type: "text", text: "steer me" }] },
    },
    2,
  );
  await settle();
  const bubbles = [...document.querySelectorAll("#messages .message.user")];
  expect(bubbles).toHaveLength(1);
  expect(bubbles[0].textContent).toContain("steer me");
});

test("an idle direct send still dedupes its own echo", async () => {
  await import("./app.js?steering-echo-idle");
  const ws = wsInstances.at(-1);
  await settle();

  typeIntoComposer("plain question");
  pressEnter();
  await settle();
  // The optimistic bubble is rendered up front for a direct send...
  expect(document.querySelectorAll("#messages .message.user")).toHaveLength(1);

  // ...so pi's echo of the same text must not double it (lastSentMessage).
  runtimeEvent(
    ws,
    {
      type: "message_start",
      message: { role: "user", content: [{ type: "text", text: "plain question" }] },
    },
    1,
  );
  await settle();
  expect(document.querySelectorAll("#messages .message.user")).toHaveLength(1);
});

test("idle Alt+Enter degrades to a direct send", async () => {
  await import("./app.js?steering-alt-enter-idle");
  const ws = wsInstances.at(-1);
  await settle();

  typeIntoComposer("idle alt enter");
  pressAltEnter();
  await settle();

  // Spec 按钮可见性: a bare keypress has no disabled state to show, so idle
  // Alt+Enter degrades to the ordinary send instead of blocking.
  expect(commandFrames(ws, "follow_up")).toHaveLength(0);
  const prompts = commandFrames(ws, "prompt");
  expect(prompts).toHaveLength(1);
  expect(prompts[0].command.message).toBe("idle alt enter");
  expect(prompts[0].command.streamingBehavior).toBeUndefined();
});

test("switching the session identity drops the previous session's queue pills", async () => {
  await import("./app.js?steering-queue-scope");
  const ws = wsInstances.at(-1);
  await settle();
  const snapshot = (sessionFile, sequence) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence,
        target,
        state: { pi: { sessionFile, isStreaming: true }, messages: [] },
      }),
    });

  snapshot("/pi/sessions/s1.jsonl", 1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["belongs to s1"], [], 2);
  await settle();
  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);

  // Same-runtime session switch: the identity changes without a page reload.
  snapshot("/pi/sessions/s2.jsonl", 3);
  await settle();

  expect(queueEl.classList.contains("hidden")).toBe(true);
});

test("a queue parked by the session switch comes back with that session", async () => {
  await import("./app.js?steering-queue-park");
  const ws = wsInstances.at(-1);
  await settle();
  const snapshot = (sessionFile, sequence) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence,
        target,
        state: { pi: { sessionFile, isStreaming: true }, messages: [] },
      }),
    });

  snapshot("/pi/sessions/s1.jsonl", 1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["belongs to s1"], ["and its follow-up"], 2);
  await settle();
  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);

  // Same-runtime session switch: s2 has no queue of its own.
  snapshot("/pi/sessions/s2.jsonl", 3);
  await settle();
  expect(queueEl.classList.contains("hidden")).toBe(true);

  // pi re-emits queue_update only when the queue mutates, and get_state
  // carries no queue text: the pills for s1 can only come from the park.
  snapshot("/pi/sessions/s1.jsonl", 4);
  await settle();
  expect(queueEl.classList.contains("hidden")).toBe(false);
  expect([...queueEl.querySelectorAll(".queued-msg-label")].map((el) => el.textContent)).toEqual([
    "Steer",
    "Follow-up",
  ]);
  expect([...queueEl.querySelectorAll(".queued-msg-text")].map((el) => el.textContent)).toEqual([
    "belongs to s1",
    "and its follow-up",
  ]);
});

test("a freshly spawned runtime starts with an empty parked queue", async () => {
  await import("./app.js?steering-queue-park-spawn");
  const ws = wsInstances.at(-1);
  await settle();
  const snapshot = (sessionFile, sequence) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence,
        target,
        state: { pi: { sessionFile, isStreaming: true }, messages: [] },
      }),
    });

  snapshot("/pi/sessions/s1.jsonl", 1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["orphaned by a restart"], [], 2);
  await settle();
  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);

  // A spawn is a new process: its queue is empty by construction, so a park
  // left over from the previous one (or a missed stop event) must not repaint.
  runtimeInstancesData = [
    {
      workspaceId: "w1",
      sessionId: "s1",
      instanceId: "secondary",
      sessionFile: "/pi/sessions/s1.jsonl",
      streaming: false,
    },
  ];
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_started",
      workspaceId: "w1",
      sessionId: "s1",
      instanceId: "secondary",
    }),
  });
  await settle();
  expect(queueEl.classList.contains("hidden")).toBe(true);
});

test("a stopped runtime's parked queue does not come back", async () => {
  await import("./app.js?steering-queue-park-stopped");
  const ws = wsInstances.at(-1);
  await settle();
  const snapshot = (sessionFile, sequence) =>
    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_snapshot",
        protocolVersion: 2,
        sequence,
        target,
        state: { pi: { sessionFile, isStreaming: true }, messages: [] },
      }),
    });

  snapshot("/pi/sessions/s1.jsonl", 1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["dies with the process"], [], 2);
  await settle();
  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);

  runtimeEvent(ws, { type: "runtime_stopped" }, 3);
  await settle();
  expect(queueEl.classList.contains("hidden")).toBe(true);

  // A later return to that session spawns a fresh runtime with an empty
  // queue, so the dead process's pills must not be replayed.
  snapshot("/pi/sessions/s2.jsonl", 4);
  await settle();
  snapshot("/pi/sessions/s1.jsonl", 5);
  await settle();
  expect(queueEl.classList.contains("hidden")).toBe(true);
});

test("the panel comes back when pi reports a new queue after a clear", async () => {
  await import("./app.js?steering-queue-repaint");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["first"], [], 2);
  await settle();

  clearQueueData = { steering: ["first"], followUp: [] };
  document.getElementById("pi-queue").querySelector(".pi-queue-clear").click();
  await settle();
  expect(document.getElementById("pi-queue").classList.contains("hidden")).toBe(true);

  // The hide was a confirmed clear, not a permanent one.
  renderPiQueue(ws, ["second"], ["after"], 3);
  await settle();
  const queueEl = document.getElementById("pi-queue");
  expect(queueEl.classList.contains("hidden")).toBe(false);
  expect([...queueEl.querySelectorAll(".queued-msg-label")].map((el) => el.textContent)).toEqual([
    "Steer",
    "Follow-up",
  ]);
});

test("Esc during an in-flight steer leaves the queue at pi and the composer empty", async () => {
  // Pi-native (2026-09-25, supersedes Q3-A): Esc aborts only. The in-flight
  // steer stays queued at pi (it runs once the aborted turn ends), and the
  // composer keeps whatever the user typed next — nothing is restored into it.
  await import("./app.js?steering-esc-inflight");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  deferPromptResponse = true;
  typeIntoComposer("hold on please");
  pressEnter();
  await settle();
  expect(commandFrames(ws, "prompt")).toHaveLength(1);

  document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
  await settle(120);
  // Esc touches nothing on the queue path: no clear_queue, no composer
  // restore — the queued text lives at pi until the aborted run ends.
  expect(commandFrames(ws, "clear_queue")).toHaveLength(0);
  const aborts = commandFrames(ws, "abort");
  expect(aborts).toHaveLength(1);
  expect(aborts[0].command.turnId).toBeUndefined();

  // The steer's own acceptance lands after the abort without injecting the
  // queued text anywhere.
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_response",
      requestId: deferredPromptId,
      response: { success: true, data: {} },
    }),
  });
  await settle();
  expect(commandFrames(ws, "clear_queue")).toHaveLength(0);
});

test("clearing restores both buckets, in queue order, when the composer is empty", async () => {
  await import("./app.js?steering-clear-both");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  renderPiQueue(ws, ["steer text"], ["follow-up text"], 2);
  await settle();

  // pi returns both buckets (verified against 0.85.1); the refill joins them.
  clearQueueData = { steering: ["steer text"], followUp: ["follow-up text"] };
  document.getElementById("pi-queue").querySelector(".pi-queue-clear").click();
  await settle();

  expect(document.getElementById("message-input").value).toBe("steer text\nfollow-up text");
  expect(document.getElementById("pi-queue").classList.contains("hidden")).toBe(true);
});

test("a steer carries the pending image and acceptance releases exactly it", async () => {
  await import("./app.js?steering-attachments");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  pasteImage();
  await settle();
  expect(pendingPreviews()).toHaveLength(1);

  deferPromptResponse = true;
  typeIntoComposer("look at this");
  pressEnter();
  await settle();

  const steers = commandFrames(ws, "prompt").filter(
    (frame) => frame.command.streamingBehavior === "steer",
  );
  expect(steers).toHaveLength(1);
  expect(steers[0].command.images).toEqual([
    { type: "image", data: "FAKE-BASE64", mimeType: "image/png" },
  ]);
  // C3 keeps the attachments owned by the composer until pi answers.
  expect(pendingPreviews()).toHaveLength(1);

  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_response",
      requestId: deferredPromptId,
      response: { success: true, data: {} },
    }),
  });
  await settle();
  expect(pendingPreviews()).toHaveLength(0);
  expect(document.getElementById("image-previews").classList.contains("hidden")).toBe(true);
});

test("a rejected steer leaves the pending image attached", async () => {
  await import("./app.js?steering-attachments-rejected");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  pasteImage();
  await settle();
  expect(pendingPreviews()).toHaveLength(1);

  promptFails = true;
  typeIntoComposer("this one gets rejected");
  pressEnter();
  await settle();

  expect(commandFrames(ws, "prompt")).toHaveLength(1);
  // The steer never reached pi, so the attachment must still be there to send.
  expect(pendingPreviews()).toHaveLength(1);
});

test("an oversized prompt rejection shows the size hint and keeps text + images", async () => {
  // Regression lock for the command_too_large → messageTooLarge mapping:
  // composer state must survive exactly like any other rejection, but the
  // transcript shows the actionable size hint instead of the protocol detail.
  await import("./app.js?oversized-rejected");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  promptTooLarge = true;
  pasteImage();
  await settle();
  typeIntoComposer("three big screenshots");
  pressEnter();
  await settle();

  const errorRow = document.querySelector("#messages .error-message");
  expect(errorRow).not.toBeNull();
  expect(errorRow.textContent).toContain("Message too large");
  expect(errorRow.textContent).not.toContain("protocol limit");
  // Nothing was consumed: text stays, the attachment stays pending.
  expect(document.getElementById("message-input").value).toContain("three big screenshots");
  expect(pendingPreviews()).toHaveLength(1);
});

test("pi's echo of a steer renders above that turn's answer, not below it", async () => {
  await import("./app.js?steering-echo-order");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  // A steer draws no optimistic bubble, and pi emits agent_start BEFORE the
  // user echo (probe: 0.4s agent_start, 0.4s message_start(user)). So the echo
  // is the turn's only user row and must land inside the turn, above its
  // status/rail/answer — otherwise the prompt shows up under its own answer.
  runtimeEvent(
    ws,
    {
      type: "message_start",
      message: { role: "user", content: [{ type: "text", text: "steer me" }] },
    },
    2,
  );
  await settle();
  runtimeEvent(ws, { type: "message_start", message: { role: "assistant", content: [] } }, 3);
  runtimeEvent(
    ws,
    {
      type: "message_update",
      message: { role: "assistant", content: [{ type: "text", text: "the answer" }] },
      assistantMessageEvent: { type: "text_delta", delta: "the answer" },
    },
    4,
  );
  await settle();

  const messages = document.getElementById("messages");
  const turn = messages.querySelector("section.turn");
  expect(turn).not.toBeNull();
  const userRow = turn.querySelector(".message.user");
  expect(userRow).not.toBeNull();
  expect(userRow.textContent).toContain("steer me");
  // Order inside the turn: user → status → rail → answer.
  const ordered = [...turn.children].map((child) => child.className.split(" ")[0]);
  expect(ordered[0]).toBe("message");
  expect(turn.firstElementChild).toBe(userRow);
});

test("a follow-up delivered inside the same run opens its own turn", async () => {
  // pi sends NO agent_start for a follow-up: it drains inside the running turn
  // (probe: queue_update followUp=[] → message_start(user) → assistant, one
  // agent_end). Without a turn boundary here the second task's answer keeps
  // appending to the first turn and its prompt lands below both answers.
  await import("./app.js?steering-followup-turn");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();

  const userStart = (text, sequence) =>
    runtimeEvent(
      ws,
      { type: "message_start", message: { role: "user", content: [{ type: "text", text }] } },
      sequence,
    );
  const assistantText = (text, sequence) => {
    runtimeEvent(
      ws,
      { type: "message_start", message: { role: "assistant", content: [] } },
      sequence,
    );
    runtimeEvent(
      ws,
      {
        type: "message_update",
        message: { role: "assistant", content: [{ type: "text", text }] },
        assistantMessageEvent: { type: "text_delta", delta: text },
      },
      sequence + 1,
    );
  };

  userStart("first prompt", 2);
  await settle();
  assistantText("first answer", 3);
  await settle();

  userStart("follow-up prompt", 5); // same run: no agent_start
  await settle();
  assistantText("second answer", 6);
  await settle();

  const messages = document.getElementById("messages");
  const turns = [...messages.querySelectorAll("section.turn")];
  expect(turns).toHaveLength(2);
  // No user row may be left floating outside a turn.
  expect(messages.querySelectorAll(":scope > .message.user")).toHaveLength(0);

  expect(turns[0].firstElementChild?.textContent).toContain("first prompt");
  expect(turns[0].querySelector(".turn-answer")?.textContent).toContain("first answer");
  expect(turns[0].querySelector(".turn-answer")?.textContent).not.toContain("second answer");
  expect(turns[1].firstElementChild?.textContent).toContain("follow-up prompt");
  expect(turns[1].querySelector(".turn-answer")?.textContent).toContain("second answer");
  // The first task really did finish at that boundary, so its status row
  // settles with a duration instead of going blank; the new turn is live.
  expect(turns[0].querySelector(".turn-status")?.textContent).toContain("Worked for");
  expect(turns[1].querySelector(".turn-status")?.classList.contains("live")).toBe(true);
});

test("the delayed-send caret exists only while a run is active", async () => {
  await import("./app.js?steering-caret");
  const ws = wsInstances.at(-1);
  await settle();
  const caret = document.getElementById("send-caret-btn");
  const abortBtn = document.getElementById("abort-btn");
  expect(caret.classList.contains("hidden")).toBe(true);
  // Spec 按钮可见性: while streaming the caret must sit to the LEFT of the red
  // abort button (4 = DOCUMENT_POSITION_FOLLOWING).
  expect(caret.compareDocumentPosition(abortBtn) & 4).toBeTruthy();

  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  expect(caret.classList.contains("hidden")).toBe(false);

  runtimeEvent(ws, { type: "agent_end" }, 2);
  await settle();
  expect(caret.classList.contains("hidden")).toBe(true);
});

test("a pulled-back item is not re-queued when its rejection lands late", async () => {
  // B1 (review): pullBack restored the text to the composer; a late
  // rejection must not also re-queue the item — the text would exist twice.
  vi.useFakeTimers();
  try {
    await import("./app.js?followup-pulledback");
    const ws = wsInstances.at(-1);
    await vi.advanceTimersByTimeAsync(0);
    runtimeEvent(ws, { type: "agent_start" });
    await vi.advanceTimersByTimeAsync(0);
    typeIntoComposer("take me back");
    pressAltEnter();
    await vi.advanceTimersByTimeAsync(0);

    deferPromptResponse = true;
    document
      .querySelector("#queued-messages .followup-msg")
      .querySelector('[aria-label="Send now"]')
      .click();
    await vi.advanceTimersByTimeAsync(0);
    // No reply for 8s: the unconfirmed pill appears.
    await vi.advanceTimersByTimeAsync(8100);
    const pill = document.querySelector("#queued-messages .unconfirmed-msg");
    expect(pill).not.toBeNull();

    pill.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(document.getElementById("message-input").value).toContain("take me back");

    ws.onmessage({
      data: JSON.stringify({
        type: "runtime_response",
        requestId: deferredPromptId,
        response: { success: false, error: "late rejection" },
      }),
    });
    await vi.advanceTimersByTimeAsync(0);

    expect(document.querySelectorAll("#queued-messages .followup-msg")).toHaveLength(0);
    expect(document.getElementById("message-input").value).toContain("take me back");
  } finally {
    vi.useRealTimers();
  }
});

test("editing an item after a rejected send-now does not duplicate its images", async () => {
  // B2 (review): the rejection already restored the item's images to the
  // previews; the edit path must append only what is genuinely missing.
  await import("./app.js?followup-edit-images");
  const ws = wsInstances.at(-1);
  await settle();
  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  pasteImage();
  await settle();
  expect(pendingPreviews()).toHaveLength(1);

  typeIntoComposer("with an image");
  pressAltEnter();
  await settle();
  expect(pendingPreviews()).toHaveLength(0); // consumed by the queue item

  promptFails = true;
  document
    .querySelector("#queued-messages .followup-msg")
    .querySelector('[aria-label="Send now"]')
    .click();
  await settle();
  // Rejection restored the image to the previews exactly once.
  expect(pendingPreviews()).toHaveLength(1);

  document
    .querySelector("#queued-messages .followup-msg")
    .querySelector('[aria-label="Edit"]')
    .click();
  await settle();
  expect(pendingPreviews()).toHaveLength(1);
});

// ABOUTME: B′ in-place stopped-session state — runtime_stopped/crashed on the
// ABOUTME: active session clears streaming UI, paints a banner, and disables
// ABOUTME: the composer (sending to a dead runtime cannot respawn it: the
// ABOUTME: host rejects runtime_request for a non-live target with
// ABOUTME: unauthorized_target — host_server.rs dispatch → runtime_target_is_live).
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

// The real processor decodes/resizes via canvas, which jsdom cannot run; the
// logic under test never touches image codecs.
vi.mock("./image-attachments.js", () => ({
  processImageFile: async (file) => ({ data: "FAKE-BASE64", mimeType: file?.type || "image/png" }),
  processImagePayload: async (payload) => ({
    data: payload?.data ?? "FAKE-BASE64",
    mimeType: payload?.mimeType || "image/png",
  }),
  isSupportedImageMime: () => true,
}));

// Pin the onboarding gate open so the composer assertions exercise only the
// stopped-session gating; the gate itself has its own module test.
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

const SESSION_FILE = "/tmp/w1/20261017_090000_aaa.jsonl";
const target = { workspaceId: "w1", sessionId: "s1", instanceId: "primary" };

const wsInstances = [];
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
    if (envelope.type === "runtime_snapshot_request") {
      this.reply({
        type: "runtime_snapshot",
        requestId: envelope.requestId,
        target: envelope.target,
        state: {
          lifecycle: "Idle",
          messages: [],
          stats: {},
          pi: { sessionFile: SESSION_FILE, isStreaming: false },
        },
      });
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
      this.reply({
        type: "runtime_response",
        requestId: envelope.requestId,
        response: { success: true, data: {} },
      });
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
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

const settle = (ms = 40) => new Promise((resolve) => setTimeout(resolve, ms));

let sequenceCounter = 0;
function runtimeEvent(ws, event) {
  sequenceCounter += 1;
  ws.onmessage({
    data: JSON.stringify({
      type: "runtime_event",
      protocolVersion: 2,
      sequence: sequenceCounter,
      target,
      event,
    }),
  });
}

function commandFrames(ws, type) {
  return ws.sent.filter(
    (frame) =>
      frame.type === "runtime_request" &&
      frame.command?.type === type &&
      // The picot-config bridge rides the same `prompt` command type; it is
      // infrastructure traffic (config reads released by the readiness gate),
      // never part of the chat flows these assertions count.
      !String(frame.command.message ?? "").startsWith("/picot-config"),
  );
}

function typeIntoComposer(text) {
  const input = document.getElementById("message-input");
  input.value = text;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  return input;
}

function pressEnter() {
  document
    .getElementById("message-input")
    .dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
}

test("runtime_stopped mid-stream clears streaming UI, banners the chat, disables composer", async () => {
  await import("./app.js?stopped-banner");
  const ws = wsInstances.at(-1);
  await settle(80);

  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  expect(document.getElementById("status-text").textContent).toBe(enMessages.status.working);

  runtimeEvent(ws, { type: "runtime_stopped" });
  await settle();

  const banner = document.querySelector("#messages .session-stopped-banner");
  expect(banner).toBeTruthy();
  expect(banner.textContent).toBe(enMessages.workspace.sessionStopped);
  // Banner sits at the top of the chat area, above the transcript.
  expect(document.getElementById("messages").firstElementChild).toBe(banner);

  // Streaming UI is gone: status back to connected, abort hidden.
  expect(document.getElementById("status-text").textContent).toBe(enMessages.status.connected);
  expect(document.getElementById("abort-btn").classList.contains("hidden")).toBe(true);

  // Composer is dead: input and send disabled, input-area marked stopped.
  expect(document.getElementById("message-input").disabled).toBe(true);
  expect(document.getElementById("send-btn").disabled).toBe(true);
  expect(document.querySelector(".input-area").classList.contains("session-stopped")).toBe(true);
});

test("runtime_crashed paints the ended-session wording", async () => {
  await import("./app.js?stopped-banner-crashed");
  const ws = wsInstances.at(-1);
  await settle(80);

  runtimeEvent(ws, { type: "runtime_crashed" });
  await settle();

  const banner = document.querySelector("#messages .session-stopped-banner");
  expect(banner).toBeTruthy();
  expect(banner.textContent).toBe(enMessages.workspace.sessionEnded);
  expect(banner.dataset.kind).toBe("crashed");
});

test("no respawn on send: Enter while stopped sends no runtime_request; a fresh runtime revives the composer", async () => {
  await import("./app.js?stopped-no-respawn");
  const ws = wsInstances.at(-1);
  await settle(80);
  runtimeEvent(ws, { type: "runtime_stopped" });
  await settle();

  // The host rejects a runtime_request aimed at a dead target
  // (unauthorized_target), and nothing on the frontend respawns on send —
  // so the composer must not even emit the frame.
  typeIntoComposer("hello dead runtime");
  pressEnter();
  await settle();
  expect(commandFrames(ws, "prompt")).toHaveLength(0);

  // A fresh runtime spawn (reopening the session) is the revival path.
  runtimeInstancesData = [{ ...target, sessionFile: SESSION_FILE }];
  // The host event carries the routing triple at the top level.
  ws.onmessage({ data: JSON.stringify({ type: "runtime_started", ...target }) });
  await settle(120);

  expect(document.querySelector("#messages .session-stopped-banner")).toBeNull();
  expect(document.getElementById("message-input").disabled).toBe(false);
  expect(document.querySelector(".input-area").classList.contains("session-stopped")).toBe(false);
});

test("a normal agent_start/agent_end turn never raises the banner", async () => {
  await import("./app.js?stopped-no-misfire");
  const ws = wsInstances.at(-1);
  await settle(80);

  runtimeEvent(ws, { type: "agent_start" });
  await settle();
  runtimeEvent(ws, { type: "agent_end" });
  await settle();

  expect(document.querySelector("#messages .session-stopped-banner")).toBeNull();
  expect(document.getElementById("message-input").disabled).toBe(false);
  expect(document.getElementById("send-btn").disabled).toBe(false);
});

// ABOUTME: Multi-window §7 listener degradation: inside the app the notification click is
// routed by app-side Rust; only a plain browser tab keeps the self-routing fallback.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const wsInstances = [];
const sentEnvelopes = [];

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

  send(raw) {
    sentEnvelopes.push(JSON.parse(raw));
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
  sentEnvelopes.length = 0;
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
  vi.spyOn(console, "error").mockImplementation(() => {});
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
  delete globalThis.WebSocket;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
  delete window.__TAURI__;
  document.documentElement.innerHTML = "";
});

const ACTIVATION = {
  type: "notification_activated",
  workspaceId: "w1",
  sessionId: "s1",
  sessionFile: "/tmp/sessions/w1/s1.jsonl",
  cwd: "/tmp/proj",
};

async function bootApp(query) {
  // Literal specifiers: Vite cannot analyze a fully dynamic import, and the
  // query keeps each case a fresh module with its own boot side effects.
  const cases = {
    "notif-tauri-passive": () => import("./app.js?notif-tauri-passive"),
    "notif-browser-fallback": () => import("./app.js?notif-browser-fallback"),
    "notif-routed-navigate": () => import("./app.js?notif-routed-navigate"),
    "notif-routed-open": () => import("./app.js?notif-routed-open"),
  };
  await cases[query]();
  await new Promise((resolve) => setTimeout(resolve, 20));
  return wsInstances.at(-1);
}

function sentOperations() {
  return sentEnvelopes
    .filter((envelope) => envelope.type === "host_request")
    .map((envelope) => envelope.operation);
}

test("in the app (__TAURI__), the broadcast stays passive — no window self-navigates", async () => {
  window.__TAURI__ = { window: {} };
  const ws = await bootApp("notif-tauri-passive");

  ws.onmessage({ data: JSON.stringify(ACTIVATION) });
  await new Promise((resolve) => setTimeout(resolve, 20));

  expect(sentOperations()).not.toContain("workspace_target_prepare");
  expect(sentOperations()).not.toContain("open_workspace");
});

test("in a plain browser tab, the self-routing fallback keeps working", async () => {
  const ws = await bootApp("notif-browser-fallback");

  ws.onmessage({ data: JSON.stringify(ACTIVATION) });
  await new Promise((resolve) => setTimeout(resolve, 20));

  expect(sentOperations()).toContain("workspace_target_prepare");
});

test("a routed navigate event runs the in-window session select", async () => {
  await bootApp("notif-routed-navigate");

  document.dispatchEvent(
    new CustomEvent("picot:notification-navigate", {
      detail: { sessionFile: ACTIVATION.sessionFile, cwd: ACTIVATION.cwd },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 20));

  expect(sentOperations()).toContain("workspace_target_prepare");
});

test("a routed open event rides the open_workspace chain", async () => {
  await bootApp("notif-routed-open");

  document.dispatchEvent(
    new CustomEvent("picot:notification-open", {
      detail: { cwd: ACTIVATION.cwd, sessionFile: ACTIVATION.sessionFile },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 20));

  const opened = sentEnvelopes.find((envelope) => envelope.operation === "open_workspace");
  expect(opened).toBeTruthy();
  expect(opened.args).toMatchObject({
    cwd: "/tmp/proj",
    sessionPath: "/tmp/sessions/w1/s1.jsonl",
    openWindow: true,
  });
});

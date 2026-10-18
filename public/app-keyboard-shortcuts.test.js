// ABOUTME: Verifies the ⌘N/⌘T rebind (multi-window spec §5/D6): ⌘T starts a new
// ABOUTME: session in the focused webview; ⌘N no longer does (it is the native
// ABOUTME: New Window menu item, handled Rust-side, and never reaches the page).
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
  }

  send(message) {
    const frame = JSON.parse(message);
    this.sent.push(frame);
    if (frame.type === "data_request") {
      let response = null;
      if (frame.operation === "workspace_info") {
        response = {
          isGit:
            !String(frame.workspaceId || "").includes("non-git") &&
            !String(frame.workspaceId || "").includes("nongit"),
        };
      }
      if (response) {
        queueMicrotask(() =>
          this.onmessage?.({
            data: JSON.stringify({
              type: "data_response",
              requestId: frame.requestId,
              ok: true,
              ...response,
            }),
          }),
        );
      }
    }
    if (frame.type === "host_request") {
      let response = null;
      if (frame.operation === "workspace.list") {
        response = { workspaces: [], removed: [] };
      } else if (frame.operation === "runtime_instances") {
        response = { instances: [] };
      } else if (frame.operation === "list_skill_inventory") {
        response = { skills: [] };
      }
      if (response) {
        queueMicrotask(() =>
          this.onmessage?.({
            data: JSON.stringify({
              type: "host_response",
              requestId: frame.requestId,
              ok: true,
              response,
            }),
          }),
        );
      }
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  const fixture = new DOMParser().parseFromString(
    readFileSync(join(process.cwd(), "public/index.html"), "utf8"),
    "text/html",
  );
  document.documentElement.replaceChildren(...fixture.documentElement.childNodes);
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
  vi.spyOn(console, "debug").mockImplementation(() => {});
  vi.spyOn(console, "log").mockImplementation(() => {});
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
  document.documentElement.replaceChildren();
  delete globalThis.WebSocket;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
});

function dispatchShortcut(key) {
  const event = new KeyboardEvent("keydown", {
    key,
    metaKey: true,
    bubbles: true,
    cancelable: true,
  });
  document.dispatchEvent(event);
  return event;
}

test("Cmd+T starts a new session in the focused webview", async () => {
  await import("./app.js?keyboard-shortcuts");

  const statusText = document.getElementById("status-text");
  expect(statusText).not.toBeNull();
  statusText.textContent = "baseline";

  const event = dispatchShortcut("t");
  expect(event.defaultPrevented).toBe(true);
  // The browser/dev fallback path announces the new session synchronously
  // before awaiting the runtime; that announcement is the observable proof
  // the shortcut reached newSession().
  expect(statusText.textContent).toBe(enMessages.status.startingNewSession);
});

test("Cmd+N no longer starts a session in the webview", async () => {
  await import("./app.js?keyboard-shortcuts-n");

  const statusText = document.getElementById("status-text");
  statusText.textContent = "baseline";

  const event = dispatchShortcut("n");
  expect(event.defaultPrevented).toBe(false);
  expect(statusText.textContent).toBe("baseline");
});

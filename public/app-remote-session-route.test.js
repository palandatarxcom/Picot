// ABOUTME: Remote-host cross-workspace routing — the navigation must stay on the
// ABOUTME: page origin; a remote daemon's loopback targetOrigin is unreachable locally.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

// The local WebView page: the local daemon's loopback origin plus the host
// param that makes this window a remote-host page (phase2.5 contract 4).
const PAGE_ORIGIN = "http://127.0.0.1:55001";
const PAGE_HREF = `${PAGE_ORIGIN}/?host=943be80c-4bb1-448c-bd66-870efec36dee`;
const HOST_ID = "943be80c-4bb1-448c-bd66-870efec36dee";

// What the remote daemon's device_target_prepare reports: its OWN loopback
// origin. From the local machine that port belongs to nobody (or to the wrong
// service), so navigating there is the bug this file pins.
const REMOTE_TARGET_ORIGIN =
  "http://127.0.0.1:62465/workspaces/9a51a0d7-400f-4df3-bacb-356d7d52c182/sessions/01a022fa-abcd-7385-9eac-93c8a2337b1f";
const REMOTE_TARGET_PATH =
  "/workspaces/9a51a0d7-400f-4df3-bacb-356d7d52c182/sessions/01a022fa-abcd-7385-9eac-93c8a2337b1f";

const REMOTE_WS_PATH = "/Users/linyong/tmp/PI/picot-corp-v3";
const REMOTE_SESSION_FILE =
  "/Users/linyong/.pi/agent/sessions/--bucket--/2026-08-21T06-20-48-461Z_01a022fa-abcd-7385-9eac-93c8a2337b1f.jsonl";

const wsInstances = [];
let prepareResponse = null;
const navigation = { assigns: [], replaces: [] };

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
      // The bridge rewrites the hello to clientType remote; the daemon's ack
      // advertises the faces a paired device holds (probe-verified shape).
      this.reply({
        type: "hello_ack",
        protocolVersion: 2,
        capabilities: {
          native: true,
          navigation: true,
          git: true,
          terminal: true,
          management: false,
        },
      });
      return;
    }
    if (envelope.type === "runtime_subscribe") {
      this.reply({ type: "runtime_subscribed", requestId: envelope.requestId });
      return;
    }
    if (envelope.type === "runtime_snapshot_request") {
      this.reply({
        type: "runtime_snapshot",
        protocolVersion: 2,
        requestId: envelope.requestId,
        sequence: 1,
        target: envelope.target,
        state: { lifecycle: "Idle", pi: null, messages: [] },
      });
      return;
    }
    if (envelope.type === "data_request") {
      this.reply({ type: "data_response", requestId: envelope.requestId, ok: true, messages: [] });
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
      else if (envelope.operation === "runtime_instances") response = { instances: [] };
      else if (envelope.operation === "workspace_target_prepare") {
        response = prepareResponse ?? {
          classification: "cross",
          transitionGeneration: 7,
          settleRequired: true,
          targetWorkspaceId: "9a51a0d7-400f-4df3-bacb-356d7d52c182",
          targetSessionId: "01a022fa-abcd-7385-9eac-93c8a2337b1f",
          targetOrigin: REMOTE_TARGET_ORIGIN,
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

/** jsdom's window.location is configurable as a whole (landing.test.js's
 * pattern): swap it for the remote-host page URL and record every navigation. */
const locationDescriptor = Object.getOwnPropertyDescriptor(window, "location");
function installRemoteHostPage() {
  navigation.assigns.length = 0;
  navigation.replaces.length = 0;
  const parsed = new URL(PAGE_HREF);
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      href: PAGE_HREF,
      origin: PAGE_ORIGIN,
      protocol: parsed.protocol,
      host: parsed.host,
      hostname: parsed.hostname,
      port: parsed.port,
      pathname: parsed.pathname,
      search: parsed.search,
      hash: parsed.hash,
      searchParams: parsed.searchParams,
      toString: () => PAGE_HREF,
      assign: (url) => navigation.assigns.push(String(url)),
      replace: (url) => navigation.replaces.push(String(url)),
      reload: () => {},
    },
  });
}

function restoreLocation() {
  if (locationDescriptor) Object.defineProperty(window, "location", locationDescriptor);
}

beforeEach(async () => {
  wsInstances.length = 0;
  prepareResponse = null;
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
  // connectHost resolves a remote host through the app shell's host_status.
  globalThis.__TAURI__ = {
    core: {
      invoke: vi.fn(async (command) => {
        if (command === "host_status") {
          return { wsUrl: "ws://127.0.0.1:55001/v2/ws", bridgeOrigin: PAGE_ORIGIN };
        }
        if (command === "host_list") return [];
        return { ok: true };
      }),
    },
  };
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
  installRemoteHostPage();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  restoreLocation();
  delete globalThis.WebSocket;
  delete globalThis.__TAURI__;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
});

const settle = (ms = 40) => new Promise((resolve) => setTimeout(resolve, ms));

function hostRequests(ws, operation) {
  return ws.sent.filter((f) => f.type === "host_request" && f.operation === operation);
}

test("cross-workspace select on a remote host navigates within the page origin", async () => {
  await import("./app.js?remote-route-cross-origin");
  const ws = wsInstances.at(-1);
  await settle();

  await globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  // The branch ran end to end: prepare answered, commit landed.
  expect(hostRequests(ws, "workspace_target_prepare")).toHaveLength(1);
  expect(hostRequests(ws, "workspace_transition_commit")).toHaveLength(1);

  // The navigation must leave: same page origin (the local daemon serves this
  // page; the remote's 127.0.0.1:62465 is a different machine's port), the
  // remote route path, and the host param that keeps the window remote.
  expect(navigation.assigns).toHaveLength(1);
  const target = new URL(navigation.assigns[0]);
  expect(`${target.protocol}//${target.host}`).toBe(PAGE_ORIGIN);
  expect(target.pathname).toBe(REMOTE_TARGET_PATH);
  expect(target.searchParams.get("host")).toBe(HOST_ID);
  // Explicitly not the remote daemon's loopback origin.
  expect(target.port).not.toBe("62465");
});

test("a target on a foreign hostname is still rejected (guard unchanged)", async () => {
  prepareResponse = {
    classification: "cross",
    transitionGeneration: 3,
    settleRequired: true,
    targetWorkspaceId: "w-foreign",
    targetSessionId: "s-foreign",
    targetOrigin: "http://192.168.1.9:62465/workspaces/w-foreign/sessions/s-foreign",
  };
  await import("./app.js?remote-route-foreign-origin");
  const ws = wsInstances.at(-1);
  await settle();

  await globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  expect(hostRequests(ws, "workspace_target_prepare")).toHaveLength(1);
  // The existing cross-origin guard must keep refusing foreign hostnames; the
  // remote-host fix may only retarget same-host loopback ports, never weaken it.
  expect(navigation.assigns).toHaveLength(0);
});

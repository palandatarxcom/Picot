// ABOUTME: Verifies cross-workspace session selection adopts the target in place.
// ABOUTME: The document stays alive: pushState replaces location.assign navigation.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

const wsInstances = [];
let sequence = 0;

const S2 = "/pi/sessions/w2/s2.jsonl";

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
      this.reply({
        type: "hello_ack",
        protocolVersion: 2,
        capabilities: {
          native: true,
          navigation: true,
          git: true,
          terminal: true,
          management: true,
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
        sequence: ++sequence,
        target: envelope.target,
        state: {
          lifecycle: "Idle",
          pi: {
            sessionFile: S2,
            isStreaming: false,
            sessionId: envelope.target?.sessionId || null,
            model: { provider: "anthropic", id: "claude-test" },
          },
          messages: [],
        },
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
      if (envelope.operation === "workspace.list")
        response = { workspaces: [{ canonicalPath: "/w1" }], removed: [] };
      else if (envelope.operation === "runtime_instances") {
        // Production timing: a freshly spawned runtime stays invisible to
        // this summary until Pi's first get_state lands (a session-<uuid>
        // id never matches a JSONL header), so spawn races see an empty list.
        response = { instances: [] };
      } else if (envelope.operation === "workspace_target_prepare") {
        response = {
          classification: "cross",
          transitionGeneration: 7,
          targetWorkspaceId: "w2",
          targetSessionId: "s2",
          targetInstanceId: "instance-2",
          targetOrigin: `${globalThis.location.origin}/workspaces/w2/sessions/s2`,
        };
      } else if (envelope.operation === "workspace_transition_commit") {
        response = {
          targetOrigin: `${globalThis.location.origin}/workspaces/w2/sessions/s2`,
          workspaceGeneration: 9,
        };
      } else if (envelope.operation === "ephemeral_bootstrap") {
        response = [];
      } else if (envelope.operation === "pick_folder") {
        response = "/w2";
      } else if (envelope.operation === "workspace.add") {
        response = {
          added: true,
          workspace: { workspaceId: "w2-uuid", canonicalPath: "/w2" },
        };
      } else if (envelope.operation === "workspace.remove") {
        response = { removed: true };
      }
      if (response) {
        this.reply({ type: "host_response", requestId: envelope.requestId, ok: true, response });
      }
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  wsInstances.length = 0;
  sequence = 0;
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

const settle = (ms = 120) => new Promise((resolve) => setTimeout(resolve, ms));

test("cross-workspace select adopts the target in place instead of navigating", async () => {
  await import("./app.js?cross-inplace");
  const ws = wsInstances.at(-1);
  await settle();

  // The file browser currently shows workspace w1's tree, so selecting a
  // session whose cwd is w2 takes the cross-workspace branch.
  globalThis.__picotSessionView.setFileBrowserWorkspacePath("/w1");
  const workspaceListCallsBefore = ws.sent.filter(
    (frame) => frame.type === "host_request" && frame.operation === "workspace.list",
  ).length;

  await globalThis.__picotSessionView.select({ filePath: S2, cwd: "/w2" }, { path: "/w2" });
  await settle();

  // 1. No full-page navigation: jsdom has no real assign, so the legacy path
  //    would abort in navigateInWindow and leave the route untouched. The
  //    pathname check below therefore fails red until the switch is in place.
  // 2. The canonical route moved to the adopted runtime (pushState).
  expect(window.location.pathname).toBe("/workspaces/w2/sessions/s2");
  // 3. The runtime channel re-targeted to the prepared w2/s2 instance.
  const subscribed = ws.sent.some(
    (frame) =>
      frame.type === "runtime_subscribe" &&
      frame.target?.workspaceId === "w2" &&
      frame.target?.sessionId === "s2" &&
      frame.target?.instanceId === "instance-2",
  );
  expect(subscribed).toBe(true);
  // 4. The terminal rebinding carries the committed workspace generation, so
  //    the frozen panel can reattach without a page reload.
  const terminalList = ws.sent.find(
    (frame) =>
      frame.type === "terminal_command" &&
      frame.payload?.type === "terminal_list" &&
      frame.workspaceGeneration === 9,
  );
  expect(terminalList).toBeTruthy();
  // 5. The sidebar data refreshed for the new workspace.
  const workspaceListCallsAfter = ws.sent.filter(
    (frame) => frame.type === "host_request" && frame.operation === "workspace.list",
  ).length;
  expect(workspaceListCallsAfter).toBeGreaterThan(workspaceListCallsBefore);
});

test("a spawned new session adopts in place and releases the ephemeral locks", async () => {
  await import("./app.js?cross-inplace-spawn");
  const ws = wsInstances.at(-1);
  await settle();

  // Cmd+T drives the header "+ New Session" path: spawnFreshSession with
  // forceNewSession, then the in-place adoption tail.
  document.dispatchEvent(
    new KeyboardEvent("keydown", { key: "t", metaKey: true, bubbles: true, cancelable: true }),
  );
  await settle(200);

  const prepare = ws.sent.find(
    (frame) => frame.type === "host_request" && frame.operation === "workspace_target_prepare",
  );
  expect(prepare?.args?.forceNewSession).toBe(true);
  // The document survived: the route moved to the spawned runtime.
  expect(window.location.pathname).toBe("/workspaces/w2/sessions/s2");
  const subscribed = ws.sent.some(
    (frame) =>
      frame.type === "runtime_subscribe" &&
      frame.target?.sessionId === "s2" &&
      frame.target?.instanceId === "instance-2",
  );
  expect(subscribed).toBe(true);
  // The adoption released the ephemeral surfaces: the rebind pulled the
  // owner's ephemeral descriptors instead of leaving them locked.
  const ephemeralBootstraps = ws.sent.filter(
    (frame) => frame.type === "host_request" && frame.operation === "ephemeral_bootstrap",
  );
  expect(ephemeralBootstraps.length).toBeGreaterThanOrEqual(1);
});

test("adding a workspace via the picker spawns and adopts in place", async () => {
  await import("./app.js?cross-inplace-readd");
  const ws = wsInstances.at(-1);
  await settle();

  const control = (operation) =>
    ws.sent.filter((frame) => frame.type === "host_request" && frame.operation === operation);

  // 1. Remove workspace w2 from the registry (the context-menu entry). Its
  //    old live runtime, if any, stays bound to the deleted uuid.
  await globalThis.__picotSidebar.removeWorkspace({
    source: "registry",
    registryId: "w2-uuid",
    workspaceId: "ws:w2-uuid",
    path: "/w2",
  });
  await settle();
  expect(control("workspace.remove").length).toBeGreaterThanOrEqual(1);

  // 2. Re-add it via the picker: pick_folder → workspace.add (new uuid) →
  //    prepare(spawn) → commit → in-place adoption.
  await globalThis.__picotSidebar.addProjectViaPicker();
  await settle(200);

  expect(control("pick_folder").length).toBeGreaterThanOrEqual(1);
  expect(control("workspace.add").length).toBeGreaterThanOrEqual(1);
  const prepare = control("workspace_target_prepare").at(-1);
  expect(prepare?.args?.forceNewSession).toBe(true);
  expect(control("workspace_transition_commit").length).toBeGreaterThanOrEqual(1);
  // The document survived: the route moved onto the added workspace.
  expect(window.location.pathname).toBe("/workspaces/w2/sessions/s2");
});

test("a registry_changed arriving mid-launch still refreshes the sidebar", async () => {
  await import("./app.js?cross-inplace-bucket-race");
  const ws = wsInstances.at(-1);
  await settle();

  const control = (operation) =>
    ws.sent.filter((frame) => frame.type === "host_request" && frame.operation === operation);

  // The host broadcasts registry_changed twice around a spawn: the add
  // itself, and the session-bucket record once Pi's first get_state lands.
  // The bucket one can arrive while the launch flow is still in flight
  // (workspaceLaunchInProgress), which is exactly when a re-added
  // workspace's history rows need the refresh to appear.
  const originalSend = ws.send.bind(ws);
  let addRequest = null;
  ws.send = (raw) => {
    const envelope = JSON.parse(raw);
    if (envelope.type === "host_request" && envelope.operation === "workspace.add") {
      // Hold the add reply: the launch stays mid-flight
      // (workspaceLaunchInProgress), isolating the registry event's effect.
      addRequest = envelope;
      // Synchronous delivery, strictly before the add reply resolves — the
      // mid-launch window where the old guard swallowed the refresh.
      ws.onmessage({
        data: JSON.stringify({
          type: "registry_changed",
          reason: "session_bucket_recorded",
        }),
      });
      return;
    }
    return originalSend(raw);
  };

  const listsBefore = control("workspace.list").length;
  // Drive the real toolbar button: it is what raises
  // workspaceLaunchInProgress around the picker flow.
  document.getElementById("add-project-btn").click();
  await new Promise((resolve) => setTimeout(resolve, 200));

  // While the launch is still awaiting the add reply, the registry event
  // must have refreshed the sidebar's registry data — swallowing it here
  // hides a re-added workspace's history when the bucket record lands fast.
  expect(control("workspace.list").length).toBeGreaterThan(listsBefore);

  // Release the launch: answer the held add and let the flow finish.
  if (addRequest) {
    ws.reply({
      type: "host_response",
      requestId: addRequest.requestId,
      ok: true,
      response: {
        added: true,
        workspace: { workspaceId: "w2-uuid", canonicalPath: "/w2" },
      },
    });
  }
  ws.send = originalSend;
  await new Promise((resolve) => setTimeout(resolve, 400));
});

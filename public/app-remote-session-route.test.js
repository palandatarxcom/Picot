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

let currentHref = PAGE_HREF;
// Canonical Remote workspace page (the one a terminal may bind to).
const WORKSPACE_PAGE_HREF = `${PAGE_ORIGIN}/workspaces/w-remote/sessions/s-remote?host=${HOST_ID}`;
const wsInstances = [];
let prepareResponse = null;
let bootstrapError = null;
let ackCapabilities = {
  native: true,
  navigation: true,
  git: true,
  terminal: true,
  management: false,
};
const navigation = { assigns: [], replaces: [] };
// Terminal replies: scripts set the authoritative list tabs and can hold every
// reply to model a deferred list / deferred checkpoint.
let listedTabs = [];
let heldTerminalReplies = null;
// Deferred canonical bootstrap: a re-attachment whose context has not resolved.
let heldBootstraps = null;
// Deferred host commit: a transition whose commit reply has not landed yet.
let heldCommit = null;

/** Keyboard path probe: the terminal tab's own onData callback. */
const keyboard = { fire: null };

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
        capabilities: ackCapabilities,
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
      if (envelope.operation === "workspace.bootstrap") {
        if (heldBootstraps) {
          heldBootstraps.push(envelope);
          return;
        }
        if (bootstrapError) {
          // The server's real shape for a refused data op is an `error` frame
          // (host_server's `Err((code, message))` path), not an ok:false
          // data_response.
          this.reply({
            type: "error",
            requestId: envelope.requestId,
            error: { code: bootstrapError, message: bootstrapError },
          });
          return;
        }
        this.reply({
          type: "data_response",
          requestId: envelope.requestId,
          ok: true,
          target: {
            workspaceId: envelope.workspaceId,
            sessionId: envelope.sessionId,
            instanceId: "instance-remote",
            workspaceGeneration: 4,
            ownerId: "owner-remote",
          },
        });
        return;
      }
      this.reply({ type: "data_response", requestId: envelope.requestId, ok: true, messages: [] });
      return;
    }
    if (envelope.type === "terminal_command") {
      if (heldTerminalReplies) {
        heldTerminalReplies.push(envelope);
        return;
      }
      this.replyTerminal(envelope);
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
      } else if (envelope.operation === "workspace_transition_commit") {
        if (heldCommit) {
          heldCommit.push(envelope);
          return;
        }
        if (this.failCommit) {
          this.reply({
            type: "host_response",
            requestId: envelope.requestId,
            ok: false,
            error: "commit refused",
          });
          return;
        }
        response = {};
      }
      if (response) {
        this.reply({ type: "host_response", requestId: envelope.requestId, ok: true, response });
      }
    }
  }

  /** Answer one terminal command the way the host does (correlated requestId). */
  replyTerminal(envelope) {
    const payload = envelope.payload || {};
    if (payload.type === "terminal_list") {
      this.reply({
        type: "terminal_listed",
        requestId: envelope.requestId,
        tabs: listedTabs,
        activeIndex: null,
        panelHeightPx: null,
      });
      return;
    }
    if (payload.type === "terminal_checkpoint") {
      this.reply({
        type: "terminal_checkpoint_acked",
        requestId: envelope.requestId,
        terminalId: payload.terminalId,
      });
      return;
    }
    if (payload.type === "terminal_create") {
      this.reply({
        type: "terminal_created",
        requestId: envelope.requestId,
        terminalId: "t-created",
        generation: 1,
      });
      return;
    }
    this.reply({ type: "terminal_command_acked", requestId: envelope.requestId });
  }

  /** Answer one deferred canonical bootstrap exactly as the host would. */
  replyBootstrap(envelope) {
    this.reply({
      type: "data_response",
      requestId: envelope.requestId,
      ok: true,
      target: {
        workspaceId: envelope.workspaceId,
        sessionId: envelope.sessionId,
        instanceId: "instance-remote",
        workspaceGeneration: 4,
        ownerId: "owner-remote",
      },
    });
  }

  /** Model a lost connection: the authenticated socket closes on its own. */
  drop() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.({ code: 1006, reason: "test drop" });
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

/** jsdom's window.location is configurable as a whole (landing.test.js's
 * pattern): swap it for the remote-host page URL and record every navigation. */
const locationDescriptor = Object.getOwnPropertyDescriptor(window, "location");
function installRemoteHostPage(href = PAGE_HREF) {
  navigation.assigns.length = 0;
  navigation.replaces.length = 0;
  const parsed = new URL(href);
  currentHref = href;
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      href: currentHref,
      origin: PAGE_ORIGIN,
      protocol: parsed.protocol,
      host: parsed.host,
      hostname: parsed.hostname,
      port: parsed.port,
      pathname: parsed.pathname,
      search: parsed.search,
      hash: parsed.hash,
      searchParams: parsed.searchParams,
      toString: () => currentHref,
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
  listedTabs = [];
  heldTerminalReplies = null;
  heldBootstraps = null;
  heldCommit = null;
  keyboard.fire = null;
  prepareResponse = null;
  bootstrapError = null;
  ackCapabilities = {
    native: true,
    navigation: true,
    git: true,
    terminal: true,
    management: false,
  };
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
  globalThis.PicotXterm = {
    Terminal: class {
      constructor() {
        this.options = {};
        this.rows = 24;
        this.cols = 80;
        this.unicode = { activeVersion: "6" };
        this._data = [];
      }
      onData(cb) {
        this._data.push(cb);
        keyboard.fire = (data) => {
          for (const fn of this._data) fn(data);
        };
        return { dispose: () => {} };
      }
      onResize() {
        return { dispose: () => {} };
      }
      loadAddon() {}
      open() {}
      write() {}
      reset() {}
      focus() {}
      refresh() {}
      dispose() {}
    },
    FitAddon: class {
      fit() {}
      dispose() {}
    },
    SerializeAddon: class {
      serialize() {
        return "checkpoint";
      }
      dispose() {}
    },
    SearchAddon: class {
      dispose() {}
    },
    Unicode11Addon: class {
      dispose() {}
    },
    WebglAddon: class {
      dispose() {}
    },
  };
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
  delete globalThis.__picotRemoteTerminal;
  delete globalThis.PicotXterm;
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

const terminalFrames = (ws) => ws.sent.filter((f) => f.type === "terminal_command");

test("an old-server ack sends no terminal frame and reports unavailable", async () => {
  // `terminal: true` without the approved revision is exactly the case a
  // disclaimer cannot fix: zero frames, explicit upgrade guidance.
  await import("./app.js?remote-terminal-old-server");
  const ws = wsInstances.at(-1);
  await settle();

  expect(terminalFrames(ws)).toHaveLength(0);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("unavailable");
  expect(globalThis.__picotRemoteTerminal.availability()).toBe("unavailable");
  const banner = document.querySelector("[data-terminal-availability='unavailable']");
  expect(banner?.textContent).toContain("Update the host");
  expect(banner?.querySelector("[data-terminal-retry]")).toBeNull();
});

test("a supporting ack attaches with the captured canonical target", async () => {
  ackCapabilities = {
    native: true,
    navigation: true,
    git: true,
    terminal: true,
    management: false,
    terminalBindingVersion: 1,
  };
  installRemoteHostPage(WORKSPACE_PAGE_HREF);
  await import("./app.js?remote-terminal-supported");
  const ws = wsInstances.at(-1);
  await settle();

  const frames = terminalFrames(ws);
  expect(frames).toHaveLength(1);
  expect(frames[0]).toMatchObject({
    workspaceId: "w-remote",
    workspaceGeneration: 4,
    sessionId: "s-remote",
    instanceId: "instance-remote",
    payload: { type: "terminal_list" },
  });
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  expect(globalThis.__picotRemoteTerminal.availability()).toBe("ready");
});

test("runtime_not_found blocks without a list loop and retry re-queries once", async () => {
  ackCapabilities = {
    native: true,
    navigation: true,
    git: true,
    terminal: true,
    management: false,
    terminalBindingVersion: 1,
  };
  installRemoteHostPage(WORKSPACE_PAGE_HREF);
  bootstrapError = "runtime_not_found";
  await import("./app.js?remote-terminal-no-runtime");
  const ws = wsInstances.at(-1);
  await settle();

  expect(terminalFrames(ws)).toHaveLength(0);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("unavailable");
  const banner = document.querySelector("[data-terminal-availability='unavailable']");
  expect(banner?.textContent).toContain("no live runtime");
  const retry = banner?.querySelector("[data-terminal-retry]");
  expect(retry).not.toBeNull();

  // Only an explicit retry re-queries; a rejection never loops on its own. The
  // baseline includes the page's own top-level canonical bootstrap.
  const bootstraps = () =>
    ws.sent.filter((f) => f.type === "data_request" && f.operation === "workspace.bootstrap");
  const baseline = bootstraps().length;
  await settle(80);
  expect(bootstraps().length).toBe(baseline);
  bootstrapError = null;
  retry.click();
  await settle();

  expect(bootstraps().length).toBe(baseline + 1);
  expect(terminalFrames(ws)).toHaveLength(1);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
});

test("an owner bootstrap on a remote page cannot overwrite the terminal binding", async () => {
  ackCapabilities = {
    native: true,
    navigation: true,
    git: true,
    terminal: true,
    management: false,
    terminalBindingVersion: 1,
  };
  installRemoteHostPage(WORKSPACE_PAGE_HREF);
  await import("./app.js?remote-terminal-owner-bootstrap");
  const ws = wsInstances.at(-1);
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");

  // The same pairing owner's window broadcasts its own workspace generation.
  ws.reply({
    type: "owner_bootstrap",
    workspaceGeneration: 99,
    instances: [],
  });
  await settle();

  const frames = terminalFrames(ws);
  // The binding is unchanged: still the captured generation 4, no reattachment.
  expect(frames.at(-1).workspaceGeneration).toBe(4);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
});

// ── Plan 2026-11-03 §4: attachment gating, transitions, and recovery ───────

const SUPPORTED_ACK = {
  native: true,
  navigation: true,
  git: true,
  terminal: true,
  management: false,
  terminalBindingVersion: 1,
};
const RUNNING_TAB = {
  terminalId: "t-live",
  generation: 1,
  label: "Terminal",
  profileId: "default",
  status: "running",
};

const terminalPayloads = (ws, type) =>
  terminalFrames(ws).filter((frame) => frame.payload?.type === type);
const tabButtons = () => document.querySelectorAll(".terminal-tab[data-terminal-id]");

// One module instance per test: app.js is a singleton with module-level state.
const APP_MODULES = {
  "remote-keyboard-gate": () => import("./app.js?remote-keyboard-gate"),
  "remote-rejection-terminal_protocol_unsupported": () =>
    import("./app.js?remote-rejection-protocol"),
  "remote-rejection-terminal_runtime_unavailable": () =>
    import("./app.js?remote-rejection-runtime"),
  "remote-rejection-terminal_binding_invalid": () => import("./app.js?remote-rejection-invalid"),
  "remote-rejection-terminal_binding_mismatch": () => import("./app.js?remote-rejection-mismatch"),
  "remote-stale-epoch": () => import("./app.js?remote-stale-epoch"),
  "remote-cross-transition": () => import("./app.js?remote-cross-transition"),
  "remote-commit-failure": () => import("./app.js?remote-commit-failure"),
  "remote-focused-existing": () => import("./app.js?remote-focused-existing"),
  "remote-same-workspace": () => import("./app.js?remote-same-workspace"),
  "remote-late-cross-ready": () => import("./app.js?remote-late-cross-ready"),
  "remote-version-switch": () => import("./app.js?remote-version-switch"),
};

/** Import the app on the canonical Remote workspace page and reach Ready. */
async function attachReady(tag) {
  ackCapabilities = SUPPORTED_ACK;
  installRemoteHostPage(WORKSPACE_PAGE_HREF);
  listedTabs = [{ ...RUNNING_TAB }];
  // Vite needs a static import expression per specifier; the tag only keeps
  // each test's module instance separate.
  await APP_MODULES[tag]();
  const ws = wsInstances.at(-1);
  await settle();
  return ws;
}

test("a deferred list keeps the keyboard closed, and the applied list opens it (R2)", async () => {
  const first = await attachReady("remote-keyboard-gate");
  expect(tabButtons()).toHaveLength(1);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  keyboard.fire("a");
  expect(terminalPayloads(first, "terminal_input")).toHaveLength(1);

  // A reconnect starts a new attempt whose context has not resolved yet.
  heldBootstraps = [];
  heldTerminalReplies = [];
  first.drop();
  await settle(1200);
  const ws = wsInstances.at(-1);
  expect(ws).not.toBe(first);
  await settle();
  expect(heldBootstraps).toHaveLength(1);
  // The old projection and its adapter are gone; no keystroke and no panel
  // control can reach the host before the new attempt owns a binding.
  keyboard.fire?.("b");
  document.querySelector("[data-terminal-new-tab]").click();
  expect(terminalFrames(ws)).toHaveLength(0);

  // Context resolves: the controlled initial list is the only frame allowed.
  const bootstrap = heldBootstraps.shift();
  heldBootstraps = null;
  ws.replyBootstrap(bootstrap);
  await settle();
  expect(terminalPayloads(ws, "terminal_list")).toHaveLength(1);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("attaching");
  keyboard.fire?.("c");
  document.querySelector("[data-terminal-new-tab]").click();
  expect(terminalPayloads(ws, "terminal_input")).toHaveLength(0);
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(0);

  // The authoritative list lands: input and the panel open together.
  const [list] = heldTerminalReplies;
  heldTerminalReplies = null;
  ws.replyTerminal(list);
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  keyboard.fire("d");
  expect(terminalPayloads(ws, "terminal_input")).toHaveLength(1);
  document.querySelector("[data-terminal-new-tab]").click();
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(1);
});

for (const [code, expected] of [
  ["terminal_protocol_unsupported", "unavailable"],
  ["terminal_runtime_unavailable", "unavailable"],
  ["terminal_binding_invalid", "blocked"],
  ["terminal_binding_mismatch", "blocked"],
]) {
  test(`a ${code} rejection blocks the attachment without a list loop (B3)`, async () => {
    const ws = await attachReady(`remote-rejection-${code}`);
    // The user asks for a new tab; the host refuses it as an admission decision.
    document.querySelector("[data-terminal-new-tab]").click();
    await settle();
    const created = terminalPayloads(ws, "terminal_create");
    expect(created).toHaveLength(1);
    const sentBefore = terminalFrames(ws).length;

    // No exception escapes the rejection path (the pre-fix code threw here).
    ws.reply({
      type: "terminal_command_failed",
      requestId: created[0].requestId,
      code,
      error: "refused",
    });
    await settle();

    expect(globalThis.__picotRemoteTerminal.state()).toBe(expected);
    // Zero automatic retry: no list, no create, no input.
    expect(terminalFrames(ws).map((f) => f.payload?.type)).toEqual(
      terminalFrames(ws)
        .slice(0, sentBefore)
        .map((f) => f.payload?.type),
    );
    // The binding and its local projection are gone, and the surface explains
    // itself with the state the code allows.
    expect(globalThis.__picotRemoteTerminal.availability()).toBe(expected);
    // The projection followed the binding out (B3): the old running tab is no
    // longer rendered, its activity is gone, and the surface offers exactly one
    // reachable recovery action instead of a dead tab.
    expect(tabButtons()).toHaveLength(0);
    expect(document.querySelector("[data-terminal-id='t-live']")).toBeNull();
    const banner = document.querySelector(`[data-terminal-availability='${expected}']`);
    expect(banner).not.toBeNull();
    const recover = banner.querySelector("[data-terminal-retry]");
    if (code === "terminal_protocol_unsupported") {
      // No client-side remedy for a protocol gap: guidance only, by design.
      expect(recover).toBeNull();
    } else {
      expect(recover).not.toBeNull();
      expect(recover.textContent).toBe(expected === "blocked" ? "Reload page" : "Retry");
    }
    // A keystroke has no adapter left and produces no frame.
    keyboard.fire?.("x");
    expect(terminalFrames(ws).map((f) => f.payload?.type)).toEqual(
      terminalFrames(ws)
        .slice(0, sentBefore)
        .map((f) => f.payload?.type),
    );
  });
}

test("a list or failure from an invalidated epoch cannot rebuild the page (R1)", async () => {
  const ws = await attachReady("remote-stale-epoch");
  const firstListId = terminalPayloads(ws, "terminal_list")[0].requestId;
  expect(tabButtons()).toHaveLength(1);

  // Disconnect invalidates the attachment immediately, and its projection with
  // it (B3): the page keeps no tab it cannot address.
  ws.drop();
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("disconnected");
  expect(tabButtons()).toHaveLength(0);

  // A late list for the invalidated attachment must not rebuild the projection:
  // no new tab metadata, no input, still disconnected.
  ws.reply({
    type: "terminal_listed",
    requestId: firstListId,
    tabs: [{ ...RUNNING_TAB, terminalId: "t-stale" }],
    activeIndex: null,
    panelHeightPx: null,
  });
  await settle();
  expect(document.querySelector("[data-terminal-id='t-stale']")).toBeNull();
  expect(tabButtons()).toHaveLength(0);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("disconnected");

  // A late ordinary failure must not start a list either.
  const beforeFrames = terminalFrames(ws).length;
  ws.reply({
    type: "terminal_command_failed",
    requestId: firstListId,
    error: "stale failure",
  });
  await settle();
  expect(terminalFrames(ws).length).toBe(beforeFrames);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("disconnected");
});

test("a cross-workspace select freezes input, checkpoints, and invalidates before commit (R3)", async () => {
  const ws = await attachReady("remote-cross-transition");

  // Hold the checkpoint acknowledgement: the transition must stay frozen.
  heldTerminalReplies = [];
  const selecting = globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  // The controlled checkpoint is the only terminal frame the freeze allows.
  const checkpoints = terminalPayloads(ws, "terminal_checkpoint");
  expect(checkpoints).toHaveLength(1);
  const frozenFrames = terminalFrames(ws).length;
  keyboard.fire("frozen");
  document.querySelector("[data-terminal-new-tab]").click();
  expect(terminalFrames(ws).length).toBe(frozenFrames);
  // Nothing was committed while the checkpoint was outstanding.
  expect(hostRequests(ws, "workspace_transition_commit")).toHaveLength(0);

  // The checkpoint lands: the commit proceeds and the page navigates.
  const held = heldTerminalReplies;
  heldTerminalReplies = null;
  for (const frame of held) ws.replyTerminal(frame);
  await selecting;
  await settle();

  expect(hostRequests(ws, "workspace_transition_commit")).toHaveLength(1);
  expect(navigation.assigns).toHaveLength(1);
  // The old binding was invalidated before the host committed.
  expect(globalThis.__picotRemoteTerminal.state()).toBe("disconnected");
  const afterCommit = terminalFrames(ws).length;
  keyboard.fire("still frozen");
  expect(terminalFrames(ws).length).toBe(afterCommit);
});

test("a failed commit re-attaches instead of leaving the terminal dead (R3)", async () => {
  const ws = await attachReady("remote-commit-failure");
  ws.failCommit = true;
  const bootstrapsBefore = ws.sent.filter(
    (frame) => frame.type === "data_request" && frame.operation === "workspace.bootstrap",
  ).length;

  await globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  // The commit failed after the binding was invalidated, so a fresh context is
  // resolved instead of unfreezing a dead binding.
  const bootstraps = ws.sent.filter(
    (frame) => frame.type === "data_request" && frame.operation === "workspace.bootstrap",
  );
  expect(bootstraps.length).toBe(bootstrapsBefore + 1);
  expect(globalThis.__picotRemoteTerminal.state()).not.toBe("disconnected");
  expect(navigation.assigns).toHaveLength(0);
});

test("a focusedExisting prepare keeps the page and its terminal untouched (R3)", async () => {
  const ws = await attachReady("remote-focused-existing");
  prepareResponse = { focusedExisting: true, transitionGeneration: 4 };
  const beforeFrames = terminalFrames(ws).length;

  await globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  expect(hostRequests(ws, "workspace_transition_commit")).toHaveLength(0);
  expect(navigation.assigns).toHaveLength(0);
  // No freeze, no checkpoint, no invalidation: still the same binding.
  expect(terminalPayloads(ws, "terminal_checkpoint")).toHaveLength(0);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  expect(terminalFrames(ws).length).toBe(beforeFrames);
});

test("a same-workspace select re-attaches and releases the panel lock (R3)", async () => {
  const ws = await attachReady("remote-same-workspace");
  // Production seam: the same-workspace branch compares against the file
  // browser's workspace, which a jsdom file-tree load cannot produce.
  globalThis.__picotSessionView.setFileBrowserWorkspacePath("/work/same");
  const bootstrapsBefore = ws.sent.filter(
    (frame) => frame.type === "data_request" && frame.operation === "workspace.bootstrap",
  ).length;

  await globalThis.__picotSessionView.select(
    { filePath: "/sessions/same.jsonl", cwd: "/work/same" },
    { path: "/work/same", dirName: "same" },
  );
  await settle();

  // The page survives: no navigation, one commit, and a fresh binding.
  expect(navigation.assigns).toHaveLength(0);
  expect(hostRequests(ws, "workspace_transition_commit")).toHaveLength(1);
  const bootstraps = ws.sent.filter(
    (frame) => frame.type === "data_request" && frame.operation === "workspace.bootstrap",
  );
  expect(bootstraps.length).toBe(bootstrapsBefore + 1);
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");

  // The transition lock is gone: a real click creates a tab again.
  const before = terminalPayloads(ws, "terminal_create").length;
  document.querySelector("[data-terminal-new-tab]").click();
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(before + 1);
});

test("late replies from a previous Ready attachment cannot touch the new one (R1)", async () => {
  const first = await attachReady("remote-late-cross-ready");
  // Request ids the previous attachment issued while it was Ready.
  const oldListId = terminalPayloads(first, "terminal_list")[0].requestId;
  document.querySelector("[data-terminal-new-tab]").click();
  await settle();
  const oldCreate = terminalPayloads(first, "terminal_create")[0];
  expect(oldCreate.requestId).toBeTruthy();
  expect(tabButtons()).toHaveLength(1);

  // A reconnect starts a new attempt; its context and list stay deferred so the
  // old attachment's frames can be delivered *after* the new one is Ready.
  heldBootstraps = [];
  heldTerminalReplies = [];
  first.drop();
  await settle(1200);
  const ws = wsInstances.at(-1);
  expect(ws).not.toBe(first);
  const bootstrap = heldBootstraps.shift();
  heldBootstraps = null;
  ws.replyBootstrap(bootstrap);
  await settle();
  const [list] = heldTerminalReplies;
  heldTerminalReplies = null;
  ws.replyTerminal(list);
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  const readyTabs = tabButtons().length;
  expect(readyTabs).toBe(1);
  const beforeFrames = terminalFrames(ws).length;

  // All three late-response classes from the old epoch, on the live connection.
  ws.reply({
    type: "terminal_listed",
    requestId: oldListId,
    tabs: [{ ...RUNNING_TAB, terminalId: "t-old-a" }],
    activeIndex: null,
    panelHeightPx: null,
  });
  ws.reply({
    type: "terminal_created",
    requestId: oldCreate.requestId,
    terminalId: "t-old-a",
    generation: 3,
  });
  ws.reply({
    type: "terminal_command_failed",
    requestId: oldCreate.requestId,
    error: "stale failure",
  });
  await settle();

  // None of them rebuilt tabs, changed state, or started a list.
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  expect(document.querySelector("[data-terminal-id='t-old-a']")).toBeNull();
  expect(tabButtons()).toHaveLength(readyTabs);
  expect(terminalFrames(ws)).toHaveLength(beforeFrames);
});

test("a reconnect to an old server drops the terminal to unavailable with zero frames", async () => {
  const first = await attachReady("remote-version-switch");
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  expect(tabButtons()).toHaveLength(1);

  // The same page reconnects to a host without the approved binding revision.
  ackCapabilities = {
    native: true,
    navigation: true,
    git: true,
    terminal: true,
    management: false,
  };
  first.drop();
  await settle(1200);
  const ws = wsInstances.at(-1);
  expect(ws).not.toBe(first);
  await settle();

  expect(globalThis.__picotRemoteTerminal.state()).toBe("unavailable");
  expect(globalThis.__picotRemoteTerminal.availability()).toBe("unavailable");
  // The projection followed the capability out, and no frame was sent.
  expect(tabButtons()).toHaveLength(0);
  expect(terminalFrames(ws)).toHaveLength(0);
  const banner = document.querySelector("[data-terminal-availability='unavailable']");
  expect(banner?.textContent).toContain("Update the host");
});

test("a desktop cross-workspace transition stays frozen through the host commit (R3)", async () => {
  ackCapabilities = SUPPORTED_ACK;
  // A desktop page: no host param, so this page builds no Remote attachment and
  // has no command gate — the panel lock is the only thing freezing it.
  installRemoteHostPage(`${PAGE_ORIGIN}/workspaces/w-local/sessions/s-local`);
  await import("./app.js?desktop-deferred-commit");
  const ws = wsInstances.at(-1);
  await settle();
  expect(globalThis.__picotRemoteTerminal).toBeNull();
  ws.reply({ type: "owner_bootstrap", workspaceGeneration: 4, instances: [] });
  await settle();

  // Control: while nothing is transitioning, this click reaches the host.
  const newTab = () => document.querySelector("[data-terminal-new-tab]");
  newTab().click();
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(1);

  heldCommit = [];
  const selecting = globalThis.__picotSessionView.select(
    { filePath: REMOTE_SESSION_FILE, cwd: REMOTE_WS_PATH },
    { path: REMOTE_WS_PATH, dirName: "picot-corp-v3" },
  );
  await settle();

  // The checkpoint is done and the commit is out, but the host has not answered
  // it: the panel must still be frozen, so no new tab can be created here.
  expect(heldCommit).toHaveLength(1);
  newTab().click();
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(1);

  // The commit lands; the page navigates and never unfroze before that.
  const [commit] = heldCommit;
  heldCommit = null;
  ws.reply({ type: "host_response", requestId: commit.requestId, ok: true, response: {} });
  await selecting;
  await settle();
  expect(navigation.assigns).toHaveLength(1);
  newTab().click();
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(1);
});

test("an unavailable terminal opens from the toggle so its reason and action are reachable (R6)", async () => {
  ackCapabilities = SUPPORTED_ACK;
  installRemoteHostPage(WORKSPACE_PAGE_HREF);
  bootstrapError = "runtime_not_found";
  await import("./app.js?remote-unavailable-visible");
  const ws = wsInstances.at(-1);
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("unavailable");

  const toggle = document.querySelector("[data-terminal-toggle]");
  const panel = document.querySelector("[data-terminal-panel]");
  expect(panel.classList.contains("hidden")).toBe(true);
  toggle.click();
  await settle();
  expect(panel.classList.contains("hidden")).toBe(false);
  const banner = panel.querySelector("[data-terminal-availability='unavailable']");
  expect(banner).not.toBeNull();
  expect(banner.textContent).toContain("no live runtime");

  // The action is reachable and re-queries exactly once, with no PTY created.
  const retry = banner.querySelector("[data-terminal-retry]");
  expect(retry).not.toBeNull();
  bootstrapError = null;
  retry.click();
  await settle();
  expect(globalThis.__picotRemoteTerminal.state()).toBe("ready");
  expect(terminalPayloads(ws, "terminal_create")).toHaveLength(0);
});

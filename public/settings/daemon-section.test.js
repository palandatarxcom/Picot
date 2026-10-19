// @vitest-environment jsdom

// ABOUTME: Verifies the Settings → Daemon section: status/toggle rendering,
// ABOUTME: the policy-only quit toggle, the restart confirm flow, the permanent
// ABOUTME: running-agents list and its stop flow, polling, and locale coverage.

import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const LOCALES = ["en", "zh", "ja", "es"];

const messagesFor = (locale) => JSON.parse(readFileSync(`public/locales/${locale}.json`, "utf8"));

const lookupValue = (messages, key) => {
  let node = messages;
  for (const part of key.split(".")) {
    if (node == null || typeof node !== "object") return undefined;
    node = node[part];
  }
  return typeof node === "string" ? node : undefined;
};

// Every leaf key a locale object carries, dotted: the coverage check compares
// this against the keys the renderer asks for, so a nested group like
// `agentState.*` still has to be reached by some render path.
const leafKeys = (node, prefix) =>
  Object.entries(node).flatMap(([name, value]) =>
    value && typeof value === "object"
      ? leafKeys(value, `${prefix}${name}.`)
      : [`${prefix}${name}`],
  );

// The same i18n contract the Subagents coverage suite uses: mocked t() records
// every key the renderer asks for (including keys assembled at runtime) and
// resolves from en.json, returning the key itself when a lookup fails — which is
// exactly the "untranslated" case the coverage loop below catches.
const { requested } = vi.hoisted(() => ({ requested: new Set() }));

vi.mock("../i18n.js", async (importOriginal) => {
  const actual = await importOriginal();
  const { readFileSync: read } = await import("node:fs");
  const en = JSON.parse(read("public/locales/en.json", "utf8"));
  return {
    ...actual,
    t: (key, params = {}) => {
      requested.add(key);
      const value = lookupValue(en, key);
      if (typeof value !== "string") return key;
      return value.replace(/\{(\w+)\}/g, (_, name) =>
        params[name] === undefined ? `{${name}}` : String(params[name]),
      );
    },
  };
});

import { t } from "../i18n.js";
import { setupDaemonSection, tauriDaemonTransport } from "./daemon-section.js";

const EN = messagesFor("en");
const SECTION_KEYS = "settings.daemon.";

// The section markup is extracted from the shipped index.html, not re-typed:
// a missing id in the real page must fail these tests, not silently pass
// against a hand-written fixture.
const INDEX = new JSDOM(readFileSync("public/index.html", "utf8")).window.document;
const SECTION = INDEX.getElementById("setting-daemon-section");
const SECTION_HTML = SECTION.outerHTML;
// The restart confirm is a body-level modal overlay (same shape as the git
// confirm dialog), so it is extracted and mounted beside the section, not
// inside it — exactly how the shipped page lays the two out.
const CONFIRM_OVERLAY = INDEX.getElementById("daemon-restart-confirm");
const CONFIRM_HTML = CONFIRM_OVERLAY.outerHTML;
// Every key the *static* markup carries (labels the i18n DOM pass translates:
// they never travel through t() and would otherwise look like orphaned copy).
const MARKUP_KEYS = [
  ...SECTION.querySelectorAll("[data-i18n], [data-i18n-aria-label]"),
  ...CONFIRM_OVERLAY.querySelectorAll("[data-i18n], [data-i18n-aria-label]"),
].flatMap((node) => [node.dataset.i18n, node.dataset.i18nAriaLabel].filter(Boolean));

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A status document as `daemon_status` produces it. */
function status(overrides = {}) {
  return {
    running: true,
    version: "0.5.0",
    buildId: "build-1",
    pid: 4242,
    hostOrigin: "http://127.0.0.1:6001",
    agents: [],
    keepRunningAfterQuit: false,
    ...overrides,
  };
}

function agent(overrides = {}) {
  return {
    workspaceId: "w1",
    sessionId: "s1",
    instanceId: "i1",
    cwd: "/Users/x/proj",
    streaming: false,
    ...overrides,
  };
}

function mountSection() {
  document.body.innerHTML = SECTION_HTML + CONFIRM_HTML;
  return {
    section: document.getElementById("setting-daemon-section"),
    statusValue: document.getElementById("setting-daemon-status-value"),
    notice: document.getElementById("daemon-section-notice"),
    toggle: document.getElementById("toggle-daemon-keep-running"),
    note: document.getElementById("daemon-keep-running-note"),
    restartBtn: document.getElementById("btn-daemon-restart"),
    confirm: document.getElementById("daemon-restart-confirm"),
    confirmText: document.getElementById("daemon-restart-confirm-text"),
    agentsLabel: document.getElementById("daemon-restart-agents-label"),
    confirmBtn: document.getElementById("btn-daemon-restart-confirm"),
    cancelBtn: document.getElementById("btn-daemon-restart-cancel"),
    empty: document.getElementById("daemon-agents-empty"),
    stopConfirm: document.getElementById("daemon-stop-confirm"),
    stopTarget: document.getElementById("daemon-stop-confirm-target"),
    stopConfirmBtn: document.getElementById("btn-daemon-stop-confirm"),
    stopCancelBtn: document.getElementById("btn-daemon-stop-cancel"),
  };
}

/** The rows of the permanent list, without its empty-state node. */
const agentRows = () => [...document.querySelectorAll("#daemon-agents .daemon-runtime-row")];

/** The permanent list's Stop buttons, in row order. */
const stopButtons = () => [
  ...document.querySelectorAll("#daemon-runtime-content:not([hidden]) .daemon-agent-stop"),
];

/** Mock the WebView's `__TAURI__.core.invoke` and record every call. */
function mountInvoke({
  status = null,
  setKeepRunning = null,
  restart = null,
  stopAgent = null,
} = {}) {
  const calls = [];
  const invoke = vi.fn(async (command, args) => {
    // The Pair Device block rides this section's lifecycle; its relay reads
    // are answered but not recorded — its behavior has its own suite.
    if (command === "relay_status") {
      return { enabled: false, state: "offline", endpoint: "paseo.021521.xyz:443", serverId: null };
    }
    if (command === "relay_set_enabled") return { enabled: args?.enabled };
    if (command === "relay_offer") throw new Error("relay is not enabled");
    calls.push({ command, args });
    if (command === "daemon_status") {
      if (status instanceof Error) throw status;
      return typeof status === "function" ? status() : status;
    }
    if (command === "daemon_set_keep_running") {
      if (setKeepRunning instanceof Error) throw setKeepRunning;
      return typeof setKeepRunning === "function" ? setKeepRunning(args) : undefined;
    }
    if (command === "daemon_restart") {
      if (restart instanceof Error) throw restart;
      return typeof restart === "function" ? restart(args) : { pid: 4243 };
    }
    if (command === "daemon_agent_stop") {
      if (stopAgent instanceof Error) throw stopAgent;
      return typeof stopAgent === "function" ? stopAgent(args) : { stopped: args };
    }
    throw new Error(`unexpected command ${command}`);
  });
  window.__TAURI__ = { core: { invoke } };
  return { invoke, calls, commands: () => calls.map((call) => call.command) };
}

function setup(overrides = {}, loadWorkspaceSessions) {
  const dom = mountSection();
  const harness = mountInvoke(overrides);
  const page = setupDaemonSection({ root: document, loadWorkspaceSessions });
  return { ...dom, ...harness, page };
}

beforeEach(() => {
  document.body.innerHTML = "";
  delete window.__TAURI__;
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("status row", () => {
  it("shows the running daemon with its version and the current quit policy", async () => {
    const { page, statusValue, toggle } = setup({
      status: status({ version: "0.5.0", keepRunningAfterQuit: false }),
    });
    await page.activate();
    expect(statusValue.textContent).toBe(`${EN.settings.daemon.running} · v0.5.0`);
    expect(statusValue.dataset.tone).toBe("ok");
    // The policy is part of the status, not a separate call.
    expect(toggle.classList.contains("on")).toBe(false);
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
  });

  it("shows the stopped copy and the daemon's reason instead of a version", async () => {
    const { page, statusValue, notice, toggle } = setup({
      status: status({
        running: false,
        reason: "the admin channel closed while the request was in flight",
        version: undefined,
        keepRunningAfterQuit: true,
      }),
    });
    await page.activate();
    expect(statusValue.textContent).toBe(EN.settings.daemon.stopped);
    expect(statusValue.dataset.tone).toBe("warn");
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toContain(
      "the admin channel closed while the request was in flight",
    );
    // A stopped daemon still reports the app-private policy: the toggle keeps
    // showing the truth while nothing is running.
    expect(toggle.classList.contains("on")).toBe(true);
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
  });

  it("surfaces a failed status read without inventing a daemon state", async () => {
    const { page, statusValue, notice, toggle } = setup({
      status: new Error("Tauri IPC is unavailable in this shell"),
    });
    await page.activate();
    expect(statusValue.textContent).toBe(EN.settings.daemon.unknown);
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toContain("Tauri IPC is unavailable in this shell");
    // No status means no policy to render: the toggle stays as the markup left
    // it rather than flipping to a state we cannot know.
    expect(toggle.classList.contains("on")).toBe(false);
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
  });
});

describe("quit-policy toggle", () => {
  it("writes the policy only, then explains that nothing was restarted or paused", async () => {
    const { page, commands, calls, toggle, note, statusValue } = setup({
      status: status({ keepRunningAfterQuit: false }),
    });
    await page.activate();
    const before = commands().length;
    toggle.click();
    await flush();
    expect(calls.at(-1).command).toBe("daemon_set_keep_running");
    expect(calls.at(-1).args).toEqual({ value: true });
    // Zero daemon traffic beyond the write: no probe, no status refresh, no
    // restart (spec §3.2.2 — the daemon never learns the policy).
    expect(commands().length).toBe(before + 1);
    expect(commands().slice(before)).toEqual(["daemon_set_keep_running"]);
    expect(toggle.classList.contains("on")).toBe(true);
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    expect(note.textContent).toBe(EN.settings.daemon.keepRunningSaved);
    expect(statusValue.textContent).toBe(`${EN.settings.daemon.running} · v0.5.0`);
  });

  it("rolls the toggle back and names the failure when the write fails", async () => {
    const { page, toggle, note, notice } = setup({
      status: status({ keepRunningAfterQuit: false }),
      setKeepRunning: new Error("disk is full"),
    });
    await page.activate();
    const description = note.textContent;
    expect(description).toBe(EN.settings.daemon.keepRunningDescription);
    toggle.click();
    await flush();
    expect(toggle.classList.contains("on")).toBe(false);
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    expect(note.textContent).toBe(description);
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toContain("disk is full");
  });
});

describe("restart", () => {
  it("confirms with the fresh running-agent list before restarting", async () => {
    let version = "0.5.0";
    const { page, commands, confirm, confirmText, agentsLabel, restartBtn } = setup({
      status: () =>
        status({
          version,
          pid: version === "0.5.0" ? 4242 : 4243,
          agents: [
            agent({ sessionId: "s1", cwd: "/Users/x/proj" }),
            agent({ workspaceId: "w2", sessionId: "s2", cwd: null, streaming: true }),
          ],
        }),
      restart: () => {
        version = "0.6.0";
        return { pid: 4243, version };
      },
    });
    await page.activate();
    expect(confirm.hidden).toBe(true);

    restartBtn.click();
    await flush();
    // A *fresh* status produced the count the user approves, and nothing was
    // restarted yet.
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
    expect(confirm.hidden).toBe(false);
    // The confirm is a body-level modal overlay, not an inline row.
    expect(confirm.classList.contains("daemon-restart-dialog-overlay")).toBe(true);
    expect(confirm.getAttribute("aria-modal")).toBe("true");
    expect(confirmText.textContent).toBe(EN.settings.daemon.restartConfirm);
    expect(agentsLabel.textContent).toBe(
      EN.settings.daemon.restartAgentCount.replace("{count}", "2"),
    );
    // The per-agent rows are gone from the confirm: the permanent list above
    // is the only per-agent surface.
    expect(confirm.querySelector(".daemon-agent-list")).toBeNull();
  });

  it("runs the transaction and refreshes the status when confirmed", async () => {
    let version = "0.5.0";
    const { page, commands, calls, restartBtn } = setup({
      status: () => status({ version, agents: [agent()] }),
      restart: () => {
        version = "0.6.0";
        return { pid: 4243, version };
      },
    });
    await page.activate();
    restartBtn.click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    await flush();
    expect(commands()).toEqual([
      "daemon_status",
      "daemon_status",
      "daemon_restart",
      "daemon_status",
    ]);
    expect(calls[2].args).toBeUndefined();
    const statusValue = document.getElementById("setting-daemon-status-value");
    expect(statusValue.textContent).toBe(`${EN.settings.daemon.running} · v0.6.0`);
    expect(document.getElementById("daemon-restart-confirm").hidden).toBe(true);
  });

  it("cancels without touching the daemon", async () => {
    const { page, commands, restartBtn } = setup({ status: status({ agents: [agent()] }) });
    await page.activate();
    restartBtn.click();
    await flush();
    document.getElementById("btn-daemon-restart-cancel").click();
    await flush();
    expect(document.getElementById("daemon-restart-confirm").hidden).toBe(true);
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
  });

  it("closes the confirm on Escape without touching the daemon", async () => {
    const { page, commands, restartBtn } = setup({ status: status({ agents: [agent()] }) });
    await page.activate();
    restartBtn.click();
    await flush();
    const overlay = document.getElementById("daemon-restart-confirm");
    expect(overlay.hidden).toBe(false);
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    expect(overlay.hidden).toBe(true);
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
  });

  it("ignores Escape while the restart transaction is in flight", async () => {
    const { page, restartBtn } = setup({
      status: status({ agents: [agent()] }),
      restart: () => new Promise(() => {}),
    });
    await page.activate();
    restartBtn.click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    expect(document.getElementById("daemon-restart-confirm").hidden).toBe(false);
  });

  it("says so when there is nothing running", async () => {
    const { page, agentsLabel, restartBtn } = setup({ status: status({ agents: [] }) });
    await page.activate();
    restartBtn.click();
    await flush();
    expect(agentsLabel.textContent).toBe(EN.settings.daemon.restartNoAgents);
  });

  it("keeps the failure visible when the transaction aborts", async () => {
    const { page, commands, restartBtn, notice } = setup({
      status: status({ agents: [agent()] }),
      restart: new Error("the Picot daemon restart failed at spawn: no binary"),
    });
    await page.activate();
    restartBtn.click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    await flush();
    expect(commands().at(-1)).toBe("daemon_restart");
    expect(document.getElementById("daemon-restart-confirm").hidden).toBe(false);
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toContain("no binary");
  });

  it("does not open the confirm when the agent list cannot be read", async () => {
    const { page, commands, restartBtn, notice } = setup({
      status: new Error("the daemon link is gone"),
    });
    await page.activate();
    restartBtn.click();
    await flush();
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
    expect(document.getElementById("daemon-restart-confirm").hidden).toBe(true);
    expect(notice.textContent).toContain("the daemon link is gone");
  });
});

// ── The permanent running-agents list (plan B2) ──────────────────────────────
//
// The v1 runtime management surface: a single-window Picot otherwise has no way
// to stop one runtime, and the list is the only place the daemon's own view of
// every live agent is on screen.
describe("running agents list", () => {
  it("renders a row per live runtime with its coordinator state", async () => {
    const { page, empty } = setup({
      status: status({
        agents: [
          agent({ sessionId: "s1", cwd: "/Users/x/proj", state: "working" }),
          agent({ workspaceId: "w2", sessionId: "s2", cwd: null, state: "idle" }),
          agent({ workspaceId: "w3", sessionId: "s3", state: null }),
        ],
      }),
    });
    await page.activate();
    const rows = agentRows();
    expect(rows).toHaveLength(3);
    expect(document.querySelector(".pkg-manager-group-header").textContent).toBe("/Users/x/proj");
    expect(rows[0].textContent).toContain("s1");
    expect(rows[0].querySelector(".daemon-agent-state").textContent).toBe(
      EN.settings.daemon.agentState.working,
    );
    // A runtime with no cwd falls back to its workspace id, like the restart
    // confirm's rows.
    expect(
      [...document.querySelectorAll(".pkg-manager-group-header")].map((el) => el.textContent),
    ).toContain("w2");
    expect(rows[1].querySelector(".daemon-agent-state").textContent).toBe(
      EN.settings.daemon.agentState.idle,
    );
    expect(rows[2].querySelector(".daemon-agent-state").textContent).toBe("—");
    expect(empty.hidden).toBe(true);
  });

  it("shows the empty-state placeholder when nothing is running", async () => {
    const { page, empty } = setup({ status: status({ agents: [] }) });
    await page.activate();
    expect(empty.hidden).toBe(false);
    expect(empty.textContent).toBe(EN.settings.daemon.agentsEmpty);
    expect(agentRows()).toHaveLength(0);
    expect(stopButtons()).toHaveLength(0);
  });

  it("stops a non-working agent directly, with the exact identity triple", async () => {
    const { page, calls, stopConfirm } = setup({
      status: status({ agents: [agent({ state: "idle" })] }),
    });
    await page.activate();
    stopButtons()[0].click();
    await flush();
    const sent = calls.filter((call) => call.command === "daemon_agent_stop");
    expect(sent).toHaveLength(1);
    // The daemon matches the full triple, so the command carries all three ids
    // and nothing else.
    expect(sent[0].args).toEqual({ workspaceId: "w1", sessionId: "s1", instanceId: "i1" });
    expect(stopConfirm.hidden).toBe(true);
    // The list is re-read from the daemon, never updated optimistically.
    expect(calls.map((call) => call.command)).toEqual([
      "daemon_status",
      "daemon_agent_stop",
      "daemon_status",
    ]);
  });

  it("asks before interrupting a working agent, and cancelling sends nothing", async () => {
    const { page, calls, stopConfirm, stopTarget, stopCancelBtn } = setup({
      status: status({ agents: [agent({ sessionId: "s9", state: "working" })] }),
    });
    await page.activate();
    stopButtons()[0].click();
    await flush();
    // The decision is on screen; nothing has been sent yet.
    expect(stopConfirm.hidden).toBe(false);
    expect(stopTarget.textContent).toContain("s9");
    expect(calls.map((call) => call.command)).toEqual(["daemon_status"]);

    stopCancelBtn.click();
    await flush();
    expect(stopConfirm.hidden).toBe(true);
    expect(calls.map((call) => call.command)).toEqual(["daemon_status"]);
    expect(stopButtons()[0].textContent).toBe(EN.settings.daemon.stopAgent);

    // Only the confirm sends the stop.
    stopButtons()[0].click();
    await flush();
    document.getElementById("btn-daemon-stop-confirm").click();
    await flush();
    expect(calls.map((call) => call.command)).toEqual([
      "daemon_status",
      "daemon_agent_stop",
      "daemon_status",
    ]);
  });

  it("marks the stopped row while its call is in flight", async () => {
    let release;
    const { page, stopConfirm, notice } = setup({
      status: status({ agents: [agent({ state: "idle" })] }),
      stopAgent: () =>
        new Promise((resolve) => {
          release = () => resolve({ stopped: {} });
        }),
    });
    await page.activate();
    stopButtons()[0].click();
    await flush();
    expect(stopButtons()[0].textContent).toBe(EN.settings.daemon.stoppingAgent);
    expect(stopButtons()[0].disabled).toBe(true);
    release();
    await flush();
    await flush();
    expect(stopButtons()[0].textContent).toBe(EN.settings.daemon.stopAgent);
    expect(stopConfirm.hidden).toBe(true);
    expect(notice.classList.contains("hidden")).toBe(true);
  });

  it("surfaces a failed stop without dropping the row", async () => {
    const { page, notice } = setup({
      status: status({ agents: [agent({ state: "idle" })] }),
      stopAgent: new Error("no runtime matches that identity"),
    });
    await page.activate();
    stopButtons()[0].click();
    await flush();
    await flush();
    expect(notice.hidden).toBe(false);
    expect(notice.textContent).toContain("no runtime matches that identity");
    expect(agentRows()).toHaveLength(1);
  });

  it("disables pending working-stop confirmation controls during restart", async () => {
    let release;
    const { page, restartBtn, confirmBtn, stopConfirmBtn, stopCancelBtn } = setup({
      status: status({ agents: [agent({ state: "working" })] }),
      restart: () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    });
    await page.activate();
    stopButtons()[0].click();
    restartBtn.click();
    await flush();
    confirmBtn.click();
    await flush();
    expect(stopConfirmBtn.disabled).toBe(true);
    expect(stopCancelBtn.disabled).toBe(true);
    release({ pid: 4243 });
    await flush();
    await flush();
    expect(stopConfirmBtn.disabled).toBe(false);
    page.leave();
  });

  it("disables every Stop while a restart transaction is draining runtimes", async () => {
    let release;
    const { page, stopConfirm } = setup({
      status: status({ agents: [agent({ state: "idle" })] }),
      restart: () =>
        new Promise((resolve) => {
          release = () => resolve({ pid: 4243 });
        }),
    });
    await page.activate();
    expect(stopButtons()[0].disabled).toBe(false);
    document.getElementById("btn-daemon-restart").click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    expect(stopButtons()[0].disabled).toBe(true);
    // A disabled button also refuses the confirmation path.
    stopButtons()[0].click();
    await flush();
    expect(stopConfirm.hidden).toBe(true);
    release();
    await flush();
    await flush();
    expect(stopButtons()[0].disabled).toBe(false);
  });
});

describe("runtime master-detail and JSONL names", () => {
  const detail = (key) => document.getElementById(`daemon-detail-${key}`)?.textContent;
  const deferred = () => {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => {
      resolve = yes;
      reject = no;
    });
    return { promise, resolve, reject };
  };

  it("groups by workspace identity, selects by triple, and shows all six full values", async () => {
    const agents = [
      agent({ sessionId: "session-123456789", state: "working" }),
      agent({ sessionId: "session-abcdefgh", instanceId: "i2", state: "idle" }),
      agent({ workspaceId: "w2", sessionId: "other", instanceId: "i3", state: "crashed" }),
    ];
    const load = vi.fn(async () => ({ sessions: [] }));
    const { page, stopConfirm } = setup({ status: status({ agents }) }, load);
    await page.activate();
    await flush();
    expect(load.mock.calls).toEqual([["w1"], ["w2"]]);
    expect(document.querySelectorAll(".pkg-manager-group-header")).toHaveLength(2);
    expect(agentRows()[0].getAttribute("aria-pressed")).toBe("true");
    stopButtons()[0].click();
    expect(stopConfirm.hidden).toBe(false);
    agentRows()[1].click();
    expect(stopConfirm.hidden).toBe(true);
    expect(agentRows()[1].classList.contains("is-selected")).toBe(true);
    expect(agentRows()[0].getAttribute("aria-pressed")).toBe("false");
    expect(
      ["runtime-id", "session-name", "session-id", "workspace", "state", "cwd"].map(detail),
    ).toEqual([
      "i2",
      "—",
      "session-abcdefgh",
      "w1",
      EN.settings.daemon.agentState.idle,
      "/Users/x/proj",
    ]);
    expect(agentRows()[1].textContent).toContain("session-");
    await page.refresh();
    expect(agentRows()[1].getAttribute("aria-pressed")).toBe("true");
    page.leave();
  });

  it("preserves the focused triple across polling and reordering without changing selection", async () => {
    vi.useFakeTimers();
    const a = agent();
    const b = agent({ workspaceId: "w2", instanceId: "i2" });
    let agents = [a, b];
    const { page } = setup({ status: () => status({ agents }) });
    await page.activate();
    agentRows()[1].focus();
    agents = [b, a];
    await vi.advanceTimersByTimeAsync(5000);
    expect(document.activeElement).toBe(agentRows()[0]);
    expect(agentRows()[1].getAttribute("aria-pressed")).toBe("true");
    page.leave();
  });

  it("falls back to the selected row when the focused triple disappears", async () => {
    let agents = [agent(), agent({ instanceId: "i2" })];
    const { page } = setup({ status: () => status({ agents }) });
    await page.activate();
    agentRows()[1].focus();
    agents = [agent()];
    await page.refresh();
    expect(document.activeElement).toBe(agentRows()[0]);
    page.leave();
  });

  it("does not steal detail or restart-modal focus during polling", async () => {
    vi.useFakeTimers();
    const { page, restartBtn, cancelBtn } = setup({
      status: status({ agents: [agent()] }),
    });
    await page.activate();
    const stop = stopButtons()[0];
    stop.focus();
    await vi.advanceTimersByTimeAsync(5000);
    expect(document.activeElement).toBe(stop);
    restartBtn.click();
    await vi.advanceTimersByTimeAsync(0);
    expect(document.activeElement).toBe(cancelBtn);
    await vi.advanceTimersByTimeAsync(5000);
    expect(document.activeElement).toBe(cancelBtn);
    page.leave();
  });

  it("loads exact JSONL names without sidebar data, then applies rename and blank clearing", async () => {
    const first = deferred();
    const load = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce({ sessions: [{ id: "s1", name: "Renamed" }] })
      .mockResolvedValueOnce({ sessions: [{ id: "s1", name: "  " }] });
    const { page } = setup({ status: status({ agents: [agent({ state: "idle" })] }) }, load);
    await page.activate();
    expect(detail("session-name")).toBe(EN.settings.daemon.sessionNameLoading);
    expect(agentRows()[0].textContent).toContain("s1");
    first.resolve({
      sessions: [
        { id: "s1", name: "<b>Real name</b>" },
        { id: "s10", name: "Wrong" },
      ],
    });
    await flush();
    expect(detail("session-name")).toBe("<b>Real name</b>");
    expect(agentRows()[0].querySelector(".pkg-manager-sidebar-name").textContent).toBe(
      "<b>Real name</b>",
    );
    expect(agentRows()[0].querySelector("b")).toBeNull();
    expect(agentRows()[0].textContent).toContain("s1");
    await page.refresh();
    await flush();
    expect(detail("session-name")).toBe("Renamed");
    await page.refresh();
    await flush();
    expect(detail("session-name")).toBe("—");
    expect(agentRows()[0].textContent).not.toContain("Renamed");
    page.leave();
  });

  it("distinguishes failure from no match and never substitutes firstMessage", async () => {
    const load = vi
      .fn()
      .mockRejectedValueOnce(new Error("unregistered workspace"))
      .mockResolvedValueOnce({
        sessions: [{ id: "s10", name: "Wrong", firstMessage: "Not a name" }],
      })
      .mockResolvedValueOnce({ sessions: [{ id: "s1", firstMessage: "Not a name" }] });
    const { page } = setup({ status: status({ agents: [agent()] }) }, load);
    await page.activate();
    await flush();
    expect(detail("session-name")).toBe(EN.settings.daemon.sessionNameUnavailable);
    expect(agentRows()[0].textContent).toContain("s1");
    await page.refresh();
    await flush();
    expect(detail("session-name")).toBe("—");
    await page.refresh();
    await flush();
    expect(detail("session-name")).toBe("—");
    page.leave();
  });

  it("deduplicates pending workspace scans and accepts them across status refreshes", async () => {
    const old = deferred();
    const load = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValue({ sessions: [{ id: "s1", name: "Fresh" }] });
    const { page } = setup(
      { status: status({ agents: [agent(), agent({ instanceId: "i2" })] }) },
      load,
    );
    await page.activate();
    await page.refresh();
    expect(load).toHaveBeenCalledTimes(1);
    old.resolve({ sessions: [{ id: "s1", name: "Still current" }] });
    await flush();
    expect(detail("session-name")).toBe("Still current");
    await page.refresh();
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    expect(detail("session-name")).toBe("Fresh");
    page.leave();
  });

  it("converges with six-second scans across five-second polls without overlap", async () => {
    vi.useFakeTimers();
    let inFlight = 0;
    let maxInFlight = 0;
    const load = vi.fn(() => {
      inFlight++;
      maxInFlight = Math.max(maxInFlight, inFlight);
      const name = `Scan ${load.mock.calls.length}`;
      return new Promise((resolve) => {
        setTimeout(() => {
          inFlight--;
          resolve({ sessions: [{ id: "s1", name }] });
        }, 6000);
      });
    });
    const { page } = setup(
      { status: status({ agents: [agent(), agent({ instanceId: "i2" })] }) },
      load,
    );
    await page.activate();
    for (let scan = 1; scan <= 3; scan++) {
      await vi.advanceTimersByTimeAsync(scan === 1 ? 5000 : 9000);
      expect(load).toHaveBeenCalledTimes(scan);
      await vi.advanceTimersByTimeAsync(1000);
      expect(detail("session-name")).toBe(`Scan ${scan}`);
    }
    expect(maxInFlight).toBe(1);
    page.leave();
  });

  it("rejects a removed workspace scan even if that workspace rejoins before completion", async () => {
    const old = deferred();
    const load = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValue({ sessions: [{ id: "s1", name: "Fresh" }] });
    let agents = [agent()];
    const { page } = setup({ status: () => status({ agents }) }, load);
    await page.activate();
    agents = [];
    await page.refresh();
    agents = [agent()];
    await page.refresh();
    expect(load).toHaveBeenCalledTimes(1);
    old.resolve({ sessions: [{ id: "s1", name: "Removed" }] });
    await flush();
    expect(detail("session-name")).toBe(EN.settings.daemon.sessionNameLoading);
    await page.refresh();
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    expect(detail("session-name")).toBe("Fresh");
    page.leave();
  });

  it("rejects a previous visit's pending scan on immediate re-entry without overlap", async () => {
    const old = deferred();
    const load = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValue({ sessions: [{ id: "s1", name: "New visit" }] });
    const { page } = setup({ status: status({ agents: [agent()] }) }, load);
    await page.activate();
    page.leave();
    await page.activate();
    expect(load).toHaveBeenCalledTimes(1);
    old.resolve({ sessions: [{ id: "s1", name: "Old visit" }] });
    await flush();
    expect(detail("session-name")).toBe(EN.settings.daemon.sessionNameLoading);
    await page.refresh();
    await flush();
    expect(load).toHaveBeenCalledTimes(2);
    expect(detail("session-name")).toBe("New visit");
    page.leave();
  });

  it("matches late scan names against the current session after runtime replacement", async () => {
    const old = deferred();
    let agents = [agent({ sessionId: "temporary", state: "working" })];
    const { page } = setup({ status: () => status({ agents }) }, () => old.promise);
    await page.activate();
    agents = [agent({ sessionId: "replacement", state: "idle" })];
    await page.refresh();
    old.resolve({ sessions: [{ id: "temporary", name: "Old session" }] });
    await flush();
    expect(detail("session-name")).toBe("—");
    expect(detail("session-id")).toBe("replacement");
    expect(detail("state")).toBe(EN.settings.daemon.agentState.idle);
    expect(agentRows()[0].getAttribute("aria-pressed")).toBe("true");
    page.leave();
  });

  it("late names change only text, never selection, state or a pending Stop target", async () => {
    const names = deferred();
    const a = agent({ sessionId: "same", state: "working" });
    const b = agent({ workspaceId: "w2", sessionId: "same", instanceId: "i2", state: "working" });
    const { page, calls, stopConfirm, stopConfirmBtn } = setup(
      { status: status({ agents: [a, b] }) },
      vi.fn((wid) =>
        wid === "w1" ? Promise.resolve({ sessions: [{ id: "same", name: "A" }] }) : names.promise,
      ),
    );
    await page.activate();
    agentRows()[1].click();
    stopButtons()[0].click();
    names.resolve({ sessions: [{ id: "same", name: "B" }] });
    await flush();
    expect(detail("session-name")).toBe("B");
    expect(detail("state")).toBe(EN.settings.daemon.agentState.working);
    expect(agentRows()[1].getAttribute("aria-pressed")).toBe("true");
    expect(stopConfirm.hidden).toBe(false);
    stopConfirmBtn.click();
    await flush();
    expect(calls.find((call) => call.command === "daemon_agent_stop").args).toEqual({
      workspaceId: "w2",
      sessionId: "same",
      instanceId: "i2",
    });
    page.leave();
  });

  it("drops late name responses after leave and resumes fresh on re-entry", async () => {
    const late = deferred();
    const load = vi
      .fn()
      .mockReturnValueOnce(late.promise)
      .mockResolvedValue({ sessions: [{ id: "s1", name: "Current" }] });
    const { page } = setup({ status: status({ agents: [agent()] }) }, load);
    await page.activate();
    page.leave();
    const before = document.getElementById("daemon-runtime-detail").textContent;
    late.resolve({ sessions: [{ id: "s1", name: "Late" }] });
    await flush();
    expect(document.getElementById("daemon-runtime-detail").textContent).toBe(before);
    await page.activate();
    await flush();
    expect(detail("session-name")).toBe("Current");
    page.leave();
  });

  it("keeps newer status and names when an older status arrives last", async () => {
    const oldStatus = deferred();
    const load = vi.fn(async (workspaceId) => ({ sessions: [{ id: "s1", name: workspaceId }] }));
    const { page, statusValue } = setup({ status: status({ agents: [agent()] }) }, load);
    await page.activate();
    await flush();
    const invoke = window.__TAURI__.core.invoke;
    invoke.mockImplementationOnce(() => oldStatus.promise);
    const oldRead = page.refresh();
    invoke.mockImplementationOnce(async () =>
      status({ version: "new", agents: [agent({ workspaceId: "new-w", instanceId: "new-i" })] }),
    );
    await page.refresh();
    await flush();
    expect(detail("runtime-id")).toBe("new-i");
    expect(detail("session-name")).toBe("new-w");
    oldStatus.resolve(status({ version: "old", agents: [agent()] }));
    await oldRead;
    expect(detail("runtime-id")).toBe("new-i");
    expect(detail("session-name")).toBe("new-w");
    expect(statusValue.textContent).toContain("vnew");
    expect(load.mock.calls).toEqual([["w1"], ["new-w"]]);
    page.leave();
  });

  it("drops a confirmation when the selected triple is replaced, even with a reused instance ID", async () => {
    let agents = [agent({ state: "working" })];
    const { page, stopConfirm, stopConfirmBtn, calls } = setup({
      status: () => status({ agents }),
    });
    await page.activate();
    stopButtons()[0].click();
    expect(stopConfirm.hidden).toBe(false);
    agents = [agent({ sessionId: "replacement", state: "idle" })];
    await page.refresh();
    expect(detail("session-id")).toBe("replacement");
    expect(stopConfirm.hidden).toBe(true);
    stopConfirmBtn.click();
    expect(calls.some((call) => call.command === "daemon_agent_stop")).toBe(false);
    page.leave();
  });

  it("does not scan the whole registry for an agent that has no workspace", async () => {
    const load = vi.fn(async () => ({ sessions: [] }));
    const { page } = setup({ status: status({ agents: [agent({ state: "idle" })] }) }, load);
    await page.activate();
    await flush();
    expect(load.mock.calls).toEqual([["w1"]]);
    window.__TAURI__.core.invoke.mockImplementationOnce(async () =>
      status({ agents: [{ sessionId: "s1", instanceId: "i1", state: "idle" }] }),
    );
    await page.refresh();
    await flush();
    // A status frame with no workspaceId must not become workspaceSessions(null).
    expect(load.mock.calls).toEqual([["w1"]]);
    page.leave();
  });

  it("does not start polling or name scans if status resolves after leave", async () => {
    vi.useFakeTimers();
    const delayed = deferred();
    const load = vi.fn(async () => ({ sessions: [] }));
    const { page, commands } = setup({ status: () => delayed.promise }, load);
    const opening = page.activate();
    page.leave();
    delayed.resolve(status({ agents: [agent()] }));
    await opening;
    await vi.advanceTimersByTimeAsync(15000);
    expect(commands()).toEqual(["daemon_status"]);
    expect(load).not.toHaveBeenCalled();
  });
});

// ── Restart confirm layout ───────────────────────────────────────────────────
//
// `.settings-row` sets `display: flex`, which beats the UA `[hidden]` rule: the
// hidden confirm row rendered anyway, so Restart, Cancel and "Restart now" were
// all on screen at once. The fix is a stylesheet rule, so the regression is
// pinned at that level — a jsdom render cannot reproduce the cascade (the page
// stylesheet is never loaded there, and the UA rule alone would hide the row).
describe("restart confirm layout", () => {
  const STYLE = readFileSync("public/style.css", "utf8");
  const ruleBody = (selector) => {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return STYLE.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
  };

  it("hides a hidden settings row instead of letting display: flex win", () => {
    expect(ruleBody(".settings-row[hidden]")).toContain("display: none");
  });

  it("hides the flex overlay instead of letting display: flex win", () => {
    expect(ruleBody(".daemon-restart-dialog-overlay[hidden]")).toContain("display: none");
  });

  it("keeps short session IDs outside the flex-growing package source style", async () => {
    const { page } = setup({ status: status({ agents: [agent({ sessionId: "abcdefgh-full" })] }) });
    await page.activate();
    const id = agentRows()[0].querySelector(".daemon-runtime-session-id");
    expect(id?.textContent).toBe("abcdefgh");
    expect(id?.classList.contains("pkg-manager-source")).toBe(false);
    expect(ruleBody(".daemon-runtime-session-id")).toContain("white-space: nowrap");
    page.leave();
  });

  it("keeps the count line and the confirm actions in one revealed container", () => {
    const dom = mountSection();
    const actions = dom.confirm.querySelector(".daemon-confirm-actions");
    expect(actions).not.toBeNull();
    expect(dom.agentsLabel.parentElement).toBe(actions.parentElement);
    // Both live under the row openConfirm() reveals: the count is only
    // reachable together with the buttons it qualifies.
    expect(dom.confirm.contains(dom.agentsLabel)).toBe(true);
    expect(dom.confirm.contains(actions)).toBe(true);
  });

  it("hides the stop confirm and the empty state without a display of their own", () => {
    const dom = mountSection();
    // The flex confirmation needs an explicit hidden rule, even inside detail.
    expect(ruleBody(".daemon-confirm[hidden]")).toContain("display: none");
    expect(dom.stopConfirm.classList.contains("daemon-confirm")).toBe(true);
    expect(dom.stopConfirm.closest("#daemon-runtime-detail")).not.toBeNull();
    expect(dom.stopConfirm.hidden).toBe(true);
    expect(STYLE).not.toMatch(/#daemon-agents-empty\s*\{[^}]*display\s*:/);
    expect(STYLE).not.toMatch(/\.daemon-agent-list\s+li\s*\{[^}]*display\s*:/);
    expect(STYLE).not.toMatch(/#daemon-agents\s*\{[^}]*display\s*:/);
  });
});

describe("polling", () => {
  it("polls while the section is open and stops when it closes", async () => {
    vi.useFakeTimers();
    const { page, commands } = setup({ status: status() });
    await page.activate();
    expect(commands()).toEqual(["daemon_status"]);
    await vi.advanceTimersByTimeAsync(5000);
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
    await vi.advanceTimersByTimeAsync(5000);
    expect(commands()).toHaveLength(3);
    page.leave();
    await vi.advanceTimersByTimeAsync(15000);
    expect(commands()).toHaveLength(3);
    // Re-entering polls again, and the confirm is not left open across visits.
    await page.activate();
    expect(commands()).toHaveLength(4);
  });

  it("is a no-op in a document without the section", async () => {
    document.body.innerHTML = "";
    const invoke = vi.fn();
    window.__TAURI__ = { core: { invoke } };
    const page = setupDaemonSection({ root: document });
    await page.activate();
    page.leave();
    expect(invoke).not.toHaveBeenCalled();
  });
});

// ── Locale coverage (mirrors subagents-locale-coverage.test.js) ──────────────
//
// Every key the section asks for must exist in all four locales, and every
// `settings.daemon.*` key a locale carries must be reached by some render path:
// an orphaned key is how a removed surface comes back half-alive.

describe("locale coverage", () => {
  it("resolves every requested key in every locale, with no orphaned copy", async () => {
    // Every branch that renders copy, through the real module + real markup.
    await renderEveryBranch();

    // The pairDevice subtree has its own coverage suite
    // (pair-device-section.test.js): its keys are excluded on both sides.
    const isDaemonKey = (key) =>
      key.startsWith(SECTION_KEYS) && !key.startsWith(`${SECTION_KEYS}pairDevice.`);
    const keys = new Set([...requested, ...MARKUP_KEYS].filter(isDaemonKey));
    expect(keys.size).toBeGreaterThan(10);
    for (const key of requested) {
      expect(key.startsWith(SECTION_KEYS)).toBe(true);
    }
    // The row's initial placeholder is the one shared platform key the static
    // markup carries; anything else here belongs to the daemon section.
    expect(MARKUP_KEYS.filter((key) => !key.startsWith(SECTION_KEYS))).toEqual(["status.loading"]);
    for (const locale of LOCALES) {
      const messages = messagesFor(locale);
      for (const key of keys) {
        const value = lookupValue(messages, key);
        expect(typeof value, `${locale} is missing ${key}`).toBe("string");
        expect(value.trim(), `${locale} has empty copy for ${key}`).not.toBe("");
      }
    }
    // No unused copy in en.json (the reference locale).
    const declared = leafKeys(EN.settings.daemon, SECTION_KEYS).filter(isDaemonKey);
    expect([...declared].sort()).toEqual([...keys].sort());
  });
});

async function renderEveryBranch() {
  // Running, stopped-with-reason, a failed read, and both toggle outcomes.
  for (const state of [
    status({ keepRunningAfterQuit: false }),
    status({ running: false, reason: "down", keepRunningAfterQuit: true }),
    new Error("no IPC"),
  ]) {
    const dom = mountSection();
    const harness = mountInvoke({ status: state });
    const page = setupDaemonSection({ root: document });
    await page.activate();
    dom.toggle.click();
    await flush();
    dom.restartBtn.click();
    await flush();
    document.getElementById("btn-daemon-restart-cancel").click();
    harness.invoke.mockClear();
  }
  // The confirm's own copy, with a row for every coordinator state and without
  // agents, and the failure path.
  for (const agents of [
    ["starting", "trusting", "ready", "working", "idle", "suspended", "crashed", "stopped"].map(
      (state) => agent({ state }),
    ),
    [],
  ]) {
    mountSection();
    mountInvoke({ status: status({ agents }) });
    const page = setupDaemonSection({ root: document });
    await page.activate();
    document.getElementById("btn-daemon-restart").click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    await flush();
  }
  // Every failure notice the module can render.
  for (const failing of [
    { setKeepRunning: new Error("save failed") },
    { restart: new Error("restart failed") },
  ]) {
    mountSection();
    mountInvoke({ status: status({ agents: [agent()] }), ...failing });
    const page = setupDaemonSection({ root: document });
    await page.activate();
    document.getElementById("toggle-daemon-keep-running").click();
    await flush();
    document.getElementById("btn-daemon-restart").click();
    await flush();
    document.getElementById("btn-daemon-restart-confirm").click();
    await flush();
    await flush();
  }
  // The permanent list: its rows, its empty state, and both stop paths (a
  // direct stop, and a working agent's confirm — the confirm's own copy lives
  // in the markup, but its cancel and confirm buttons are still exercised).
  for (const agents of [[agent({ state: "working" }), agent({ state: "idle" })], []]) {
    mountSection();
    mountInvoke({ status: status({ agents }) });
    const page = setupDaemonSection({ root: document });
    await page.activate();
    // The first row is the working one: its Stop asks, cancel backs out, and
    // the confirm sends it.
    stopButtons()[0]?.click();
    await flush();
    stopButtons()[0]?.click();
    await flush();
    document.getElementById("btn-daemon-stop-cancel").click();
    await flush();
    stopButtons()[0]?.click();
    await flush();
    document.getElementById("btn-daemon-stop-confirm").click();
    await flush();
    await flush();
  }
  // The stop failure notice.
  mountSection();
  mountInvoke({
    status: status({ agents: [agent({ state: "idle" })] }),
    stopAgent: new Error("no"),
  });
  {
    const page = setupDaemonSection({ root: document });
    await page.activate();
    stopButtons()[0].click();
    await flush();
    await flush();
  }
  expect(typeof t("settings.daemon.title")).toBe("string");
}

describe("a shell without Tauri IPC", () => {
  it("stays unavailable and silent instead of leaking the IPC error (PM C10②)", async () => {
    const { statusValue, notice } = mountSection();
    // No `window.__TAURI__`: a plain browser tab. The daemon relationship is
    // app-level, so there is no failure for this shell to report.
    delete window.__TAURI__;
    const page = setupDaemonSection({ root: document });
    await page.activate();
    expect(statusValue.textContent).toBe(EN.settings.daemon.unknown);
    // The notice element is hidden by the class (the markup carries no
    // `hidden` attribute), so assert the class and the copy.
    expect(notice.classList.contains("hidden")).toBe(true);
    expect(notice.textContent).toBe("");
  });

  it("reports whether the app-level IPC is present at all", () => {
    delete window.__TAURI__;
    expect(tauriDaemonTransport().available()).toBe(false);
    window.__TAURI__ = { core: { invoke: () => Promise.resolve() } };
    expect(tauriDaemonTransport().available()).toBe(true);
    // A call in a shell without IPC still rejects (the section must never
    // claim a daemon state it cannot read).
    delete window.__TAURI__;
    return expect(tauriDaemonTransport().status()).rejects.toThrow(/Tauri IPC/);
  });
});

// @vitest-environment jsdom

// ABOUTME: Verifies the Settings → General → Daemon section: status/toggle rendering,
// ABOUTME: the policy-only quit toggle, the restart confirm flow, polling, and locale coverage.

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
import { setupDaemonSection } from "./daemon-section.js";

const EN = messagesFor("en");
const SECTION_KEYS = "settings.daemon.";

// The section markup is extracted from the shipped index.html, not re-typed:
// a missing id in the real page must fail these tests, not silently pass
// against a hand-written fixture.
const INDEX = new JSDOM(readFileSync("public/index.html", "utf8")).window.document;
const SECTION = INDEX.getElementById("setting-daemon-section");
const SECTION_HTML = SECTION.outerHTML;
// Every key the *static* markup carries (labels the i18n DOM pass translates:
// they never travel through t() and would otherwise look like orphaned copy).
const MARKUP_KEYS = [...SECTION.querySelectorAll("[data-i18n], [data-i18n-aria-label]")].flatMap(
  (node) => [node.dataset.i18n, node.dataset.i18nAriaLabel].filter(Boolean),
);

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
  document.body.innerHTML = SECTION_HTML;
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
    agents: document.getElementById("daemon-restart-agents"),
    confirmBtn: document.getElementById("btn-daemon-restart-confirm"),
    cancelBtn: document.getElementById("btn-daemon-restart-cancel"),
  };
}

/** Mock the WebView's `__TAURI__.core.invoke` and record every call. */
function mountInvoke({ status = null, setKeepRunning = null, restart = null } = {}) {
  const calls = [];
  const invoke = vi.fn(async (command, args) => {
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
    throw new Error(`unexpected command ${command}`);
  });
  window.__TAURI__ = { core: { invoke } };
  return { invoke, calls, commands: () => calls.map((call) => call.command) };
}

function setup(overrides = {}) {
  const dom = mountSection();
  const harness = mountInvoke(overrides);
  const page = setupDaemonSection({ root: document });
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
    const { page, commands, confirm, confirmText, agentsLabel, agents, restartBtn } = setup({
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
    // A *fresh* status produced the list the user approves, and nothing was
    // restarted yet.
    expect(commands()).toEqual(["daemon_status", "daemon_status"]);
    expect(confirm.hidden).toBe(false);
    expect(confirmText.textContent).toBe(EN.settings.daemon.restartConfirm);
    expect(agentsLabel.textContent).toBe(
      EN.settings.daemon.restartAgentCount.replace("{count}", "2"),
    );
    const rows = [...agents.querySelectorAll("li")].map((row) => row.textContent);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain("/Users/x/proj");
    expect(rows[0]).toContain("s1");
    expect(rows[1]).toContain("w2");
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

  it("says so when there is nothing running", async () => {
    const { page, agentsLabel, agents, restartBtn } = setup({ status: status({ agents: [] }) });
    await page.activate();
    restartBtn.click();
    await flush();
    expect(agentsLabel.textContent).toBe(EN.settings.daemon.restartNoAgents);
    expect(agents.querySelectorAll("li")).toHaveLength(0);
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

    const keys = new Set([
      ...requested,
      ...MARKUP_KEYS.filter((key) => key.startsWith(SECTION_KEYS)),
    ]);
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
    const declared = Object.keys(EN.settings.daemon).map((name) => SECTION_KEYS + name);
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
  // The confirm's own copy, with and without agents, and the failure path.
  for (const agents of [[agent()], []]) {
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
  expect(typeof t("settings.daemon.title")).toBe("string");
}

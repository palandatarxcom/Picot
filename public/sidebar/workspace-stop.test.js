// ABOUTME: Task 7 Step 1(b) frontend matrix: the sidebar workspace "Stop Running"
// ABOUTME: entry (visibility, confirm dialog, transport op, four-locale copy).

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { initI18n } from "../i18n.js";
import { SessionSidebar } from "./index.js";

const dom = new JSDOM("<!doctype html><html><body><div id='sessions'></div></body></html>", {
  url: "http://localhost/",
});
globalThis.window = dom.window;
globalThis.document = dom.window.document;

const REGISTRY_ROWS = [
  {
    workspaceId: "uuid-1",
    canonicalPath: "/work/alpha",
    displayName: "alpha",
    pinned: false,
    lastOpenedAt: 500,
  },
  {
    workspaceId: "uuid-2",
    canonicalPath: "/work/beta",
    displayName: "beta",
    pinned: false,
    lastOpenedAt: 900,
  },
];

const RUNNING_ON_BETA = {
  workspaceId: "uuid-2",
  sessionId: "session-2",
  instanceId: "instance-2",
  cwd: "/work/beta",
  sessionFile: "/pi/sessions/--work-beta--/2026-10-15T00-00-00-000Z_live.jsonl",
  pid: 4242,
  streaming: false,
};

// The English strings this suite asserts on, pinned to `locales/en.json` by the
// copy test below: a reworded locale value fails there instead of silently
// weakening these assertions.
const EN_SIDEBAR_STOP = {
  stopWorkspace: "Stop Running",
  stopWorkspaceConfirm:
    "Stop the agents running in “{workspace}”? {count} running runtime(s) will be stopped. The window stays open and shows the session as stopped.",
  stopWorkspaceStreaming: "A turn is running right now; stopping kills it.",
  stopWorkspaceStopped:
    "Stopped {count} running runtime(s) in “{workspace}”. Reopen the session to start a new one.",
  stopWorkspaceFailed: "Could not stop the agents running in this workspace.",
};

function makeTransport({ rows = REGISTRY_ROWS, instances = [] } = {}) {
  const state = { instances: [...instances] };
  const transport = {
    available: true,
    capabilities: { native: true },
    listWorkspaces: vi.fn(async () => ({ workspaces: rows, removed: [] })),
    runtimeInstances: vi.fn(async () => ({ instances: state.instances })),
    removeWorkspace: vi.fn(async () => ({ removed: true })),
    stopWorkspace: vi.fn(async () => {
      // The host stops the runtime; the next inventory no longer lists it.
      state.instances = [];
      return { stopped: true };
    }),
    setWorkspacePinned: vi.fn(async () => ({})),
  };
  return { transport, state };
}

async function makeSidebar({ rows = REGISTRY_ROWS, instances = [], isCurrentWorkspace } = {}) {
  const { transport, state } = makeTransport({ rows, instances });
  const notices = [];
  const sidebar = new SessionSidebar(document.getElementById("sessions"), vi.fn(), vi.fn(), {
    transport,
    onSessionNotice: (message) => notices.push(message),
    isCurrentWorkspace: isCurrentWorkspace ?? (() => false),
  });
  globalThis.fetch = vi.fn(async (url) => {
    const route = String(url);
    if (route.includes("/locales/")) {
      // The real table: the dialog/notice assertions then ride the shipped
      // copy, and the four-locale test below pins the same keys.
      const file = route.split("/locales/")[1]?.split("?")[0] || "en.json";
      return {
        ok: true,
        json: async () =>
          JSON.parse(readFileSync(join(process.cwd(), "public/locales", file), "utf8")),
      };
    }
    if (route.startsWith("/api/workspace-sessions")) {
      return { ok: true, json: async () => ({ path: "", dirName: null, sessions: [] }) };
    }
    throw new Error(`unexpected fetch ${route}`);
  });
  await initI18n();
  return { sidebar, transport, notices, state };
}

function menuFor(sidebar, workspace) {
  const event = new window.MouseEvent("contextmenu", { bubbles: true });
  Object.defineProperty(event, "preventDefault", { value: vi.fn() });
  sidebar.showWorkspaceContextMenu(event, workspace);
  return [...document.querySelectorAll(".sidebar-context-menu .context-menu-item")];
}

function stopItem(sidebar, workspace) {
  return menuFor(sidebar, workspace).find((button) =>
    button.textContent.includes(EN_SIDEBAR_STOP.stopWorkspace),
  );
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("sidebar workspace stop entry", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
    document.body.innerHTML = "<div id='sessions'></div>";
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
    for (const node of document.querySelectorAll(".sidebar-confirm-overlay")) node.remove();
  });

  test("a live runtime row offers Stop Running; an idle workspace does not", async () => {
    const { sidebar } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(beta.runningInstances).toHaveLength(1);

    expect(stopItem(sidebar, beta)?.disabled).toBe(false);
    sidebar.closeContextMenu();

    // Nothing running in alpha: no dead entry to click.
    expect(stopItem(sidebar, alpha)).toBeUndefined();
    sidebar.closeContextMenu();
  });

  test("the confirm dialog names the workspace and its running state, and cancel stops nothing", async () => {
    const { sidebar, transport } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    stopItem(sidebar, beta).click();
    await flush();

    const dialog = document.querySelector(".sidebar-confirm-overlay .sidebar-confirm-dialog");
    expect(dialog).not.toBeNull();
    const text = dialog.textContent;
    expect(text).toContain("beta");
    expect(text).toContain("1 running runtime");
    // An idle runtime offers no mid-turn warning (the busy case below does).
    expect(text).not.toContain(EN_SIDEBAR_STOP.stopWorkspaceStreaming);

    document.querySelector(".sidebar-confirm-no").click();
    await flush();
    expect(transport.stopWorkspace).not.toHaveBeenCalled();
    expect(document.querySelector(".sidebar-confirm-overlay")).toBeNull();
  });

  test("a mid-turn runtime is called out in the confirm dialog", async () => {
    const { sidebar } = await makeSidebar({
      instances: [{ ...RUNNING_ON_BETA, streaming: true }],
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    stopItem(sidebar, beta).click();
    await flush();

    expect(document.querySelector(".sidebar-confirm-overlay").textContent).toContain(
      EN_SIDEBAR_STOP.stopWorkspaceStreaming,
    );
    document.querySelector(".sidebar-confirm-no").click();
    await flush();
  });

  test("confirming stops exactly that workspace and the row degrades to a stopped session", async () => {
    const { sidebar, transport, notices } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    stopItem(sidebar, beta).click();
    await flush();
    document.querySelector(".sidebar-confirm-yes").click();
    await flush();
    await flush();

    // The raw DB uuid travels (never the display-only `ws:` identity).
    expect(transport.stopWorkspace).toHaveBeenCalledWith("uuid-2");
    const refreshed = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    expect(refreshed.runningInstances).toEqual([]);
    expect(refreshed.sessions.some((session) => session.isRunning)).toBe(false);
    expect(notices).toContain(
      "Stopped 1 running runtime(s) in “beta”. Reopen the session to start a new one.",
    );
  });

  test("stopping the current workspace is allowed (unlike remove), behind the confirm", async () => {
    const { sidebar, transport } = await makeSidebar({
      instances: [RUNNING_ON_BETA],
      isCurrentWorkspace: (project) => project?.path === "/work/beta",
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    const item = stopItem(sidebar, beta);
    expect(item.disabled).toBe(false);
    item.click();
    await flush();
    document.querySelector(".sidebar-confirm-yes").click();
    await flush();
    await flush();

    expect(transport.stopWorkspace).toHaveBeenCalledWith("uuid-2");
  });

  test("a refused stop surfaces a notice instead of a silent failure", async () => {
    const { sidebar, transport, notices } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    transport.stopWorkspace.mockRejectedValueOnce(new Error("workspace_not_found"));

    stopItem(sidebar, beta).click();
    await flush();
    document.querySelector(".sidebar-confirm-yes").click();
    await flush();
    await flush();

    expect(notices).toContain(EN_SIDEBAR_STOP.stopWorkspaceFailed);
  });

  test("a reload failure after the stop still reports what was stopped", async () => {
    const { sidebar, notices } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    sidebar.refresh = vi.fn(async () => {
      throw new Error("reload failed");
    });

    stopItem(sidebar, beta).click();
    await flush();
    document.querySelector(".sidebar-confirm-yes").click();
    await flush();
    await flush();

    expect(notices).toContain(
      "Stopped 1 running runtime(s) in “beta”. Reopen the session to start a new one.",
    );
  });

  test("a workspace with no runtime offers no stop entry even from a direct call", async () => {
    const { sidebar, transport } = await makeSidebar({ instances: [] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    await sidebar.stopWorkspace(beta);
    await flush();
    expect(document.querySelector(".sidebar-confirm-overlay")).toBeNull();
    expect(transport.stopWorkspace).not.toHaveBeenCalled();
  });
});

describe("workspace list removal", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
    document.body.innerHTML = "<div id='sessions'></div>";
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
    for (const node of document.querySelectorAll(".sidebar-confirm-overlay")) node.remove();
  });

  function removeItem(sidebar, workspace) {
    return menuFor(sidebar, workspace).find((button) =>
      button.textContent.includes("Remove from list"),
    );
  }

  test("remove-from-list stops nothing and opens no confirm (PM B9)", async () => {
    const { sidebar, transport, notices } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    const remove = removeItem(sidebar, beta);
    expect(remove).toBeDefined();
    expect(remove.disabled).toBe(false);
    remove.click();
    await flush();
    await flush();

    expect(document.querySelector(".sidebar-confirm-overlay")).toBeNull();
    expect(transport.stopWorkspace).not.toHaveBeenCalled();
    expect(transport.removeWorkspace).toHaveBeenCalledWith("uuid-2");
    expect(notices).toContain("Removed from the list; directory and session files were kept.");
  });

  test("a stop confirm left open is dismissed by another workspace action (PM B9)", async () => {
    const { sidebar, transport } = await makeSidebar({ instances: [RUNNING_ON_BETA] });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    // The user opens Stop Running and does not answer it.
    stopItem(sidebar, beta).click();
    await flush();
    expect(document.querySelector(".sidebar-confirm-overlay")).not.toBeNull();

    // …then picks "Remove from list" instead. The pending stop must be
    // dismissed, not left behind as "the dialog remove-from-list opened".
    removeItem(sidebar, beta).click();
    await flush();
    await flush();

    expect(document.querySelector(".sidebar-confirm-overlay")).toBeNull();
    expect(transport.stopWorkspace).not.toHaveBeenCalled();
    expect(transport.removeWorkspace).toHaveBeenCalledWith("uuid-2");
  });

  test("the current workspace's remove entry is disabled and stops nothing", async () => {
    const { sidebar, transport, notices } = await makeSidebar({
      instances: [RUNNING_ON_BETA],
      isCurrentWorkspace: (project) => project?.path === "/work/beta",
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    const remove = removeItem(sidebar, beta);
    expect(remove.disabled).toBe(true);
    // A dispatched click (a synthetic harness click, or a retargeted one) must
    // still land on the refusal, never on the stop path.
    remove.dispatchEvent(new window.MouseEvent("click", { bubbles: true }));
    await flush();
    await flush();

    expect(document.querySelector(".sidebar-confirm-overlay")).toBeNull();
    expect(transport.stopWorkspace).not.toHaveBeenCalled();
    expect(transport.removeWorkspace).not.toHaveBeenCalled();
    expect(notices).toContain("Cannot remove the current workspace");
  });
});

describe("workspace stop copy", () => {
  test("every stop key resolves in all four locales", () => {
    const keys = Object.keys(EN_SIDEBAR_STOP);
    for (const locale of ["en", "zh", "ja", "es"]) {
      const table = JSON.parse(
        readFileSync(join(process.cwd(), `public/locales/${locale}.json`), "utf8"),
      );
      for (const key of keys) {
        expect(typeof table.sidebar?.[key], `${locale}.sidebar.${key} is missing`).toBe("string");
        expect(table.sidebar[key].length, `${locale}.sidebar.${key} is empty`).toBeGreaterThan(0);
      }
    }
    // The English strings this suite asserts on are the shipped ones.
    const en = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));
    for (const [key, value] of Object.entries(EN_SIDEBAR_STOP)) {
      expect(en.sidebar[key], `en.sidebar.${key} drifted from this suite`).toBe(value);
    }
  });
});

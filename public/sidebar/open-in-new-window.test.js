// ABOUTME: Multi-window T2 — right-click "open in new window" entries on the
// ABOUTME: shared sidebar: session rows (openWorkspace + sessionPath) and
// ABOUTME: landing-only workspace rows (openWorkspace + openWindow).
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, expect, test, vi } from "vitest";

vi.mock("../i18n.js", () => ({
  t: (key) => key,
  onLocaleChange: () => () => {},
}));

import { JSDOM } from "jsdom";
import { SessionSidebar } from "./index.js";

beforeEach(() => {
  const dom = new JSDOM("<!doctype html><div id=root></div>", { url: "http://localhost:3001" });
  globalThis.document = dom.window.document;
  globalThis.window = dom.window;
  globalThis.CSS = dom.window.CSS;
  globalThis.localStorage = dom.window.localStorage;
});

function makeSidebar({ openWorkspaceResult, options = {} } = {}) {
  const transport = {
    available: true,
    capabilities: { native: true },
    listWorkspaces: vi.fn(async () => ({ workspaces: [], removed: [] })),
    runtimeInstances: vi.fn(async () => ({ instances: [] })),
    openWorkspace: vi.fn(async () => openWorkspaceResult ?? {}),
  };
  const notices = [];
  const sidebar = new SessionSidebar(document.getElementById("root"), vi.fn(), vi.fn(), {
    transport,
    onSessionNotice: (message) => notices.push(message),
    ...options,
  });
  return { sidebar, transport, notices };
}

function sessionMenu(sidebar) {
  const item = document.createElement("div");
  item.className = "session-item";
  item.dataset.filePath = "/tmp/s.jsonl";
  const title = document.createElement("div");
  title.className = "session-title";
  item.appendChild(title);
  document.getElementById("root").appendChild(item);
  sidebar.showSessionContextMenu(null, item, { filePath: "/tmp/s.jsonl", name: "a" });
  return [...document.querySelectorAll(".sidebar-context-menu .context-menu-item")];
}

function workspaceMenu(sidebar, workspace) {
  const event = new window.MouseEvent("contextmenu", { bubbles: true });
  Object.defineProperty(event, "preventDefault", { value: vi.fn() });
  sidebar.showWorkspaceContextMenu(event, workspace);
  return [...document.querySelectorAll(".sidebar-context-menu .context-menu-item")];
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("session context menu: open in new window", () => {
  test("renders the entry and calls transport.openWorkspace with sessionPath", async () => {
    const { sidebar, transport } = makeSidebar();
    sidebar.projects = [{ path: "/tmp/ws", sessions: [{ filePath: "/tmp/s.jsonl", name: "a" }] }];
    const buttons = sessionMenu(sidebar);
    const entry = buttons.find((button) => button.textContent === "sidebar.openInNewWindow");
    expect(entry).toBeTruthy();
    entry.click();
    await flush();
    expect(transport.openWorkspace).toHaveBeenCalledWith("/tmp/ws", {
      sessionPath: "/tmp/s.jsonl",
      openWindow: true,
    });
    // The menu closes behind the action.
    expect(document.querySelector(".sidebar-context-menu")).toBeNull();
  });

  test("a focusedExisting reply surfaces as a notice; failure as an error notice", async () => {
    const { sidebar, transport, notices } = makeSidebar({
      openWorkspaceResult: { workspaceId: "wid", sessionId: "sid", focusedExisting: true },
    });
    sidebar.projects = [{ path: "/tmp/ws", sessions: [{ filePath: "/tmp/s.jsonl" }] }];
    const entry = sessionMenu(sidebar).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    entry.click();
    await flush();
    expect(notices).toContain("workspace.focusedExisting");

    transport.openWorkspace.mockRejectedValueOnce(new Error("focus failed"));
    const entry2 = sessionMenu(sidebar).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    entry2.click();
    await flush();
    await flush();
    expect(notices).toContain("sidebar.openInNewWindowFailed");
  });

  test("hides the entry when the transport cannot open workspaces", () => {
    const { sidebar } = makeSidebar();
    sidebar.transport = { ...sidebar.transport, openWorkspace: undefined };
    sidebar.projects = [{ path: "/tmp/ws", sessions: [{ filePath: "/tmp/s.jsonl" }] }];
    const entry = sessionMenu(sidebar).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    expect(entry).toBeUndefined();
  });
});

describe("workspace context menu: landing open-in-new-window seam", () => {
  const workspace = { path: "/tmp/ws", source: "registry", pinned: false };

  test("renders only when the seam is wired (landing), then invokes it", () => {
    const bare = makeSidebar();
    const bareEntry = workspaceMenu(bare.sidebar, workspace).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    expect(bareEntry).toBeUndefined();

    const onOpenWorkspaceNewWindow = vi.fn();
    const { sidebar } = makeSidebar({ options: { onOpenWorkspaceNewWindow } });
    const entry = workspaceMenu(sidebar, workspace).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    expect(entry).toBeTruthy();
    entry.click();
    expect(onOpenWorkspaceNewWindow).toHaveBeenCalledWith(workspace);
  });

  test("live-only rows (not registry-backed) never offer the entry", () => {
    const onOpenWorkspaceNewWindow = vi.fn();
    const { sidebar } = makeSidebar({ options: { onOpenWorkspaceNewWindow } });
    const entry = workspaceMenu(sidebar, { path: "/tmp/live", source: "live" }).find(
      (button) => button.textContent === "sidebar.openInNewWindow",
    );
    expect(entry).toBeUndefined();
  });
});

describe("open-in-new-window locale keys", () => {
  test("all four locales ship the new sidebar keys", () => {
    for (const file of ["en.json", "zh.json", "ja.json", "es.json"]) {
      const locale = JSON.parse(readFileSync(join(process.cwd(), "public/locales", file), "utf8"));
      expect(locale.sidebar.openInNewWindow, `${file} sidebar.openInNewWindow`).toBeTruthy();
      expect(
        locale.sidebar.openInNewWindowFailed,
        `${file} sidebar.openInNewWindowFailed`,
      ).toBeTruthy();
    }
  });
});

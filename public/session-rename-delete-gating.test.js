// ABOUTME: Regression tests for gating rename/delete on active or busy sessions.
// ABOUTME: Rename is gated by active/streaming only; delete also by live instances.
// ABOUTME: Covers in-place toggling, entry guards and the live-instance snapshot.
import { beforeEach, describe, expect, test, vi } from "vitest";

vi.mock("./i18n.js", () => ({
  t: (key) => key,
  onLocaleChange: () => () => {},
}));

import { JSDOM } from "jsdom";
import { buildSessionItem } from "./sidebar/build-session-item.js";
import { SessionSidebar } from "./sidebar/index.js";

const transport = {
  available: true,
  capabilities: { native: true },
  sessionRename: vi.fn(async () => ({ ok: true })),
  sessionDeleteBatch: vi.fn(async () => ({ deleted: 1, running: [], errors: [] })),
  runtimeInstances: vi.fn(async () => ({ instances: [] })),
  listWorkspaces: vi.fn(async () => ({ workspaces: [], removed: [] })),
};

beforeEach(() => {
  const dom = new JSDOM("<!doctype html><div id=root></div>", { url: "http://localhost:3001" });
  globalThis.document = dom.window.document;
  globalThis.window = dom.window;
  globalThis.CSS = dom.window.CSS;
  globalThis.localStorage = dom.window.localStorage;
  transport.sessionRename.mockClear();
  transport.sessionDeleteBatch.mockClear();
  transport.runtimeInstances.mockClear();
});

function makeSidebar() {
  const root = document.getElementById("root");
  return new SessionSidebar(root, vi.fn(), vi.fn(), { transport });
}

function rowFor(sidebar, overrides = {}) {
  return sidebar.buildSessionItem(
    { filePath: "/sessions/a.jsonl", name: "Some name", ...overrides.session },
    { path: "/w" },
    { showDeleteButton: true },
  );
}

function renderedRow(sidebar, filePath) {
  return sidebar.container.querySelector(`.session-item[data-file-path="${filePath}"]`);
}

/** One expanded workspace with two idle sessions, rendered by the real render(). */
function sessionsWorkspace() {
  return {
    workspaceId: "ws-1",
    path: "/w",
    folderName: "w",
    sessions: [
      { filePath: "/sessions/a.jsonl", name: "A", mtime: "2026-01-01T00:00:00.000Z" },
      { filePath: "/sessions/b.jsonl", name: "B", mtime: "2026-01-02T00:00:00.000Z" },
    ],
    sessionCount: 2,
  };
}

function visibleButtons(item) {
  const rename = item.querySelector(".session-rename-btn");
  const del = item.querySelector(".session-delete-btn");
  return {
    renameExists: Boolean(rename),
    renameHidden: rename?.classList.contains("action-hidden") ?? null,
    renameDisabled: rename?.disabled ?? null,
    deleteExists: Boolean(del),
    deleteHidden: del?.classList.contains("action-hidden") ?? null,
    deleteDisabled: del?.disabled ?? null,
  };
}

describe("rename/delete gating on blocked sessions", () => {
  test("active session renders both buttons hidden in place", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/a.jsonl";
    const state = visibleButtons(rowFor(sidebar));
    expect(state).toEqual({
      renameExists: true,
      renameHidden: true,
      renameDisabled: true,
      deleteExists: true,
      deleteHidden: true,
      deleteDisabled: true,
    });
  });

  test("idle non-active session shows enabled buttons", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/other.jsonl";
    const state = visibleButtons(rowFor(sidebar));
    expect(state.renameHidden).toBe(false);
    expect(state.renameDisabled).toBe(false);
    expect(state.deleteHidden).toBe(false);
    expect(state.deleteDisabled).toBe(false);
  });

  // 2026-10-09: rename no longer mirrors delete. A live instance only blocks
  // delete; the rename gate is the strict subset active ∨ streaming.
  test("streaming gates rename while a live instance only gates delete", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/other.jsonl";
    sidebar.streamingFiles.add("/sessions/a.jsonl");
    expect(visibleButtons(rowFor(sidebar)).renameHidden).toBe(true);

    sidebar.streamingFiles.delete("/sessions/a.jsonl");
    sidebar.getLiveInstances = () => [{ sessionFile: "/sessions/a.jsonl" }];
    const live = visibleButtons(rowFor(sidebar));
    expect(live.renameHidden).toBe(false);
    expect(live.deleteHidden).toBe(true);
  });

  test("raw node builder honors explicit blocked reasons (Focus path)", () => {
    const item = buildSessionItem({
      session: { filePath: "/sessions/a.jsonl" },
      showDeleteButton: true,
      deletionBlockedReason: "busy",
      renameBlockedReason: "busy-rename",
      onRename: (_filePath, _session, node) => node,
    });
    expect(item.querySelector(".session-rename-btn").classList.contains("action-hidden")).toBe(
      true,
    );
    expect(item.querySelector(".session-delete-btn").classList.contains("action-hidden")).toBe(
      true,
    );
  });

  test("context menu does not open for blocked sessions", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/a.jsonl";
    const item = rowFor(sidebar);
    sidebar.showSessionContextMenu(null, item, {
      filePath: "/sessions/a.jsonl",
      name: "n",
    });
    expect(document.querySelector(".sidebar-context-menu")).toBeNull();
  });
});

// 2026-10-09 rename-gating tightening: rename is blocked only while the
// session is the active one or has a running turn. An idle session with a
// background runtime stays renameable; delete keeps its live gate.
describe("rename gating: active or streaming only", () => {
  test("R1: switching away releases the previously active session for rename", () => {
    const sidebar = makeSidebar();
    sidebar.projects = [sessionsWorkspace()];
    sidebar.expandedWorkspaces.add("ws-1");
    // A is open and still has a background runtime (live, but idle).
    sidebar.getLiveInstances = () => [{ sessionFile: "/sessions/a.jsonl" }];

    sidebar.setActive("/sessions/a.jsonl");
    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBe("sidebar.renameDisabledActive");
    expect(visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl")).renameHidden).toBe(true);

    sidebar.setActive("/sessions/b.jsonl");

    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBeNull();
    const state = visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl"));
    expect(state.renameHidden).toBe(false);
    expect(state.renameDisabled).toBe(false);
    // The delete gate is untouched: A is still live in the background.
    expect(state.deleteHidden).toBe(true);
  });

  test("R2: an idle live session keeps rename available but stays delete-blocked", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/other.jsonl";
    sidebar.getLiveInstances = () => [{ sessionFile: "/sessions/a.jsonl" }];

    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBeNull();
    const state = visibleButtons(rowFor(sidebar));
    expect(state.renameHidden).toBe(false);
    expect(state.renameDisabled).toBe(false);
    expect(sidebar.deletionBlockedReason("/sessions/a.jsonl")).toBe(
      "sidebar.deleteDisabledRunning",
    );
    expect(state.deleteHidden).toBe(true);
    expect(state.deleteDisabled).toBe(true);
  });

  test("R3: a streaming session still blocks rename", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/other.jsonl";
    sidebar.streamingFiles.add("/sessions/a.jsonl");

    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBe(
      "sidebar.renameDisabledStreaming",
    );
    const state = visibleButtons(rowFor(sidebar));
    expect(state.renameHidden).toBe(true);
    expect(state.renameDisabled).toBe(true);
  });

  test("R4: the active session still blocks rename, and active wins over streaming", () => {
    const sidebar = makeSidebar();
    sidebar.setActive("/sessions/a.jsonl");

    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBe("sidebar.renameDisabledActive");
    expect(visibleButtons(rowFor(sidebar)).renameHidden).toBe(true);

    sidebar.streamingFiles.add("/sessions/a.jsonl");
    expect(sidebar.renameBlockedReason("/sessions/a.jsonl")).toBe("sidebar.renameDisabledActive");
  });

  test("R5: a refresh reporting a live instance leaves the cached row renameable", () => {
    const sidebar = makeSidebar();
    sidebar.projects = [sessionsWorkspace()];
    sidebar.expandedWorkspaces.add("ws-1");
    sidebar.liveInstancesSnapshot = [{ sessionFile: "/sessions/a.jsonl" }];

    sidebar.render();
    expect(visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl")).renameHidden).toBe(false);
    expect(visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl")).deleteHidden).toBe(true);

    // Repeated refreshes reuse the keyed workspace row: its action state must
    // follow the current gate instead of a stale snapshot decision.
    sidebar.render();
    const state = visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl"));
    expect(state.renameHidden).toBe(false);
    expect(state.deleteHidden).toBe(true);
  });

  test("R6: a refresh that drops the live instance re-enables the cached row's delete", () => {
    const sidebar = makeSidebar();
    sidebar.projects = [sessionsWorkspace()];
    sidebar.expandedWorkspaces.add("ws-1");
    sidebar.liveInstancesSnapshot = [{ sessionFile: "/sessions/a.jsonl" }];
    sidebar.render();
    expect(visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl")).deleteHidden).toBe(true);

    // The runtime stopped; the next refresh reports no live instance. The keyed
    // row must not keep the delete action disabled from the earlier snapshot.
    sidebar.liveInstancesSnapshot = [];
    sidebar.render();
    expect(visibleButtons(renderedRow(sidebar, "/sessions/a.jsonl")).deleteHidden).toBe(false);
  });
});

describe("state flips update rendered rows in place", () => {
  test("idle → streaming → idle toggles button visibility without rebuild", () => {
    const sidebar = makeSidebar();
    const item = rowFor(sidebar);
    sidebar.container.appendChild(item);
    sidebar.rebuildStatusIndex();

    sidebar.setStreaming("/sessions/a.jsonl", true);
    expect(visibleButtons(item).renameHidden).toBe(true);
    expect(visibleButtons(item).deleteHidden).toBe(true);

    sidebar.setStreaming("/sessions/a.jsonl", false);
    expect(visibleButtons(item).renameHidden).toBe(false);
    expect(visibleButtons(item).deleteHidden).toBe(false);
  });

  test("live-instance snapshot feeds the delete gate, not rename", async () => {
    const sidebar = makeSidebar();
    transport.runtimeInstances.mockResolvedValueOnce({
      instances: [{ sessionFile: "/sessions/a.jsonl" }],
    });
    await sidebar.fetchLiveInstances();
    expect(sidebar.isLiveSession("/sessions/a.jsonl")).toBe(true);
    const state = visibleButtons(rowFor(sidebar));
    expect(state.renameHidden).toBe(false);
    expect(state.deleteHidden).toBe(true);
  });
});

describe("entry guards re-check the gate", () => {
  test("startRename refuses a blocked session", () => {
    const sidebar = makeSidebar();
    sidebar.activeSessionFile = "/sessions/a.jsonl";
    const item = rowFor(sidebar);
    sidebar.container.appendChild(item);
    sidebar.startRename(item, { filePath: "/sessions/a.jsonl", name: "n" });
    expect(item.querySelector(".session-rename-input")).toBeNull();
  });

  test("deleteSession bails with a notice on a blocked session", async () => {
    const sidebar = makeSidebar();
    const notices = [];
    sidebar.onSessionNotice = (message) => notices.push(message);
    sidebar.activeSessionFile = "/sessions/a.jsonl";
    await sidebar.deleteSession("/sessions/a.jsonl");
    expect(transport.sessionDeleteBatch).not.toHaveBeenCalled();
    expect(notices).toEqual(["sidebar.deleteDisabledActive"]);
  });
});

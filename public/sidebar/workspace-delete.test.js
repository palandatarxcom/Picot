// ABOUTME: Tests for workspace-level batch deletion via the context menu.
// ABOUTME: Covers confirm flow, delete-batch payload, and state cleanup per response class.
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { initI18n } from "../i18n.js";
import { SessionSidebar } from "./index.js";

function setupDom() {
  const dom = new JSDOM('<div id="sessions"></div>', { url: "http://localhost:3001" });
  globalThis.document = dom.window.document;
  globalThis.localStorage = dom.window.localStorage;
  globalThis.CSS = dom.window.CSS || { escape: (v) => String(v).replace(/["\\]/g, "\\$&") };
}

function deleteBatchFetch(response) {
  const mock = vi.fn(async (url, init) => {
    const target = String(url);
    if (target.includes("/api/sessions/delete-batch")) {
      const body = JSON.parse(init?.body || "{}");
      mock.lastPayload = body;
      return { ok: true, status: 200, json: async () => response };
    }
    if (target.includes("/api/sessions")) {
      return { ok: true, status: 200, json: async () => ({ projects: [] }) };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  });
  return mock;
}

beforeEach(async () => {
  setupDom();
  global.fetch = vi.fn(async (url) => {
    if (String(url).includes("/locales/en.json")) {
      return {
        ok: true,
        status: 200,
        json: async () => ({
          sidebar: {
            recent: "RECENT",
            pinned: "PINNED",
            projects: "PROJECTS",
            openProject: "Open project",
            emptySession: "Empty",
            deleteSession: "Delete",
            deleteSessionConfirmOne: "Delete this session permanently?",
            deleteSessionConfirmMany: "Delete {count} sessions permanently?",
            deleteSessionAriaLabel: "Delete sessions",
            deleteSessionRunning: "Cannot delete a running session",
            deleteSessionFailed: "Failed to delete session",
            pinWorkspace: "Pin workspace",
            unpinWorkspace: "Unpin workspace",
            openInFinder: "Open in Finder",
            openInFileManager: "Open in File Manager",
            deleteWorkspaceMainSessions: "Delete all main sessions",
            deleteWorkspaceMainSessionsConfirm:
              "Delete {count} main sessions permanently? Hidden subagent sessions will be kept.",
            noMainSessionsToDelete:
              "No main sessions to delete; hidden subagent sessions were kept.",
            deletedMainSessionsSubagentsKept:
              "Deleted {count} main sessions. {hiddenCount} hidden subagent sessions were kept.",
            sessionCountPending: "Session count will be calculated after opening this workspace.",
            deleteWorkspaceNamePrompt: "Type workspace name to confirm:",
            deleteWorkspaceNameWarning: "Workspace name does not match.",
            deleteWorkspaceNameLabel: "Workspace name",
            deleteWorkspaceRunning: "Some sessions are still running",
          },
          actions: { cancel: "Cancel", delete: "Delete" },
        }),
      };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  });
  await initI18n();
});

afterEach(() => {
  vi.restoreAllMocks();
});

function makeSidebar({ notice } = {}) {
  const transport = {
    sessionDeleteBatch: vi.fn(async (filePaths) => {
      const response = await global.fetch("/api/sessions/delete-batch", {
        method: "POST",
        body: JSON.stringify({ filePaths }),
      });
      return response.json();
    }),
  };
  const sidebar = new SessionSidebar(document.getElementById("sessions"), vi.fn(), vi.fn(), {
    onSessionNotice: notice,
    transport,
  });
  sidebar.projects = [];
  return sidebar;
}

const WORKSPACE = {
  path: "/w",
  sessions: [
    { filePath: "/s/a.jsonl", name: "A" },
    { filePath: "/s/b.jsonl", name: "B" },
  ],
};

describe("SessionSidebar workspace deletion", () => {
  test("individual delete cancellation sends no delete-batch request", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 1, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    sidebar.showFallbackConfirmDialog = vi.fn(async () => false);

    await expect(sidebar.deleteSession("/s/a.jsonl")).resolves.toBe(false);
    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining("/api/sessions/delete-batch"),
      expect.anything(),
    );
  });

  test("host rejection surfaces a failure notice and reports not deleted", async () => {
    const sidebar = makeSidebar();
    sidebar.showFallbackConfirmDialog = vi.fn(async () => true);
    sidebar.transport.sessionDeleteBatch = vi.fn(async () => {
      throw new Error("workspace is not available");
    });
    const notice = vi.fn();
    sidebar.onSessionNotice = notice;

    await expect(sidebar.deleteSession("/s/a.jsonl")).resolves.toBe(false);
    expect(notice).toHaveBeenCalledWith("Failed to delete session");
  });

  test("per-path errors surface a failure notice and report not deleted", async () => {
    const sidebar = makeSidebar();
    sidebar.showFallbackConfirmDialog = vi.fn(async () => true);
    sidebar.transport.sessionDeleteBatch = vi.fn(async () => ({
      deleted: 0,
      errors: ["/s/a.jsonl"],
      running: [],
    }));
    const notice = vi.fn();
    sidebar.onSessionNotice = notice;

    await expect(sidebar.deleteSession("/s/a.jsonl")).resolves.toBe(false);
    expect(notice).toHaveBeenCalledWith("Failed to delete session");
    expect(sidebar.transport.sessionDeleteBatch).toHaveBeenCalledWith(["/s/a.jsonl"]);
  });

  test("confirm cancel sends no delete-batch request", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 0, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const operation = sidebar.deleteWorkspaceSessions(WORKSPACE);
    document.querySelector(".sidebar-confirm-no").click();
    await operation;

    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining("/api/sessions/delete-batch"),
      expect.anything(),
    );
  });

  test("confirmed deletion posts all eligible paths in one batch", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 2, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const operation = sidebar.deleteWorkspaceSessions({
      ...WORKSPACE,
      folderName: "w",
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    input.value = "w";
    dialog.querySelector(".sidebar-confirm-yes").click();
    await operation;

    expect(fetchMock.lastPayload).toEqual({ filePaths: ["/s/a.jsonl", "/s/b.jsonl"] });
  });

  test("running sessions stay listed and surface a notice; deleted ones are cleaned", async () => {
    const fetchMock = deleteBatchFetch({
      deleted: 1,
      errors: [],
      running: ["/s/b.jsonl"],
    });
    global.fetch = fetchMock;
    const notice = vi.fn();
    const sidebar = makeSidebar({ notice });
    const operation = sidebar.deleteWorkspaceSessions({
      ...WORKSPACE,
      folderName: "w",
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    input.value = "w";
    dialog.querySelector(".sidebar-confirm-yes").click();
    await operation;

    expect(fetchMock.lastPayload).toEqual({ filePaths: ["/s/a.jsonl", "/s/b.jsonl"] });
    expect(notice).toHaveBeenCalled();
  });

  test("batch that deletes nothing while reporting errors surfaces a failure notice", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 0, errors: ["/s/a.jsonl"], running: [] });
    global.fetch = fetchMock;
    const notice = vi.fn();
    const sidebar = makeSidebar({ notice });
    const operation = sidebar.deleteWorkspaceSessions({
      ...WORKSPACE,
      folderName: "w",
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    input.value = "w";
    dialog.querySelector(".sidebar-confirm-yes").click();
    await operation;

    expect(fetchMock.lastPayload).toEqual({ filePaths: ["/s/a.jsonl", "/s/b.jsonl"] });
    expect(notice).toHaveBeenCalledWith("Failed to delete session");
  });

  test("workspace name mismatch keeps modal open and sends no request", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 1, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const operation = sidebar.deleteWorkspaceSessions({
      path: "/work/picot-v3",
      folderName: "picot-v3",
      sessions: [{ filePath: "/s/a.jsonl" }],
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    const confirm = dialog.querySelector(".sidebar-confirm-yes");
    input.value = "wrong";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    confirm.click();

    expect(dialog.querySelector(".workspace-delete-warning").hidden).toBe(false);
    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining("/api/sessions/delete-batch"),
      expect.anything(),
    );

    dialog.querySelector(".sidebar-confirm-no").click();
    await operation;
  });

  test("exact workspace name sends one batch request", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 2, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const operation = sidebar.deleteWorkspaceSessions({
      path: "/work/picot-v3",
      folderName: "picot-v3",
      sessions: [{ filePath: "/s/a.jsonl" }, { filePath: "/s/b.jsonl" }],
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    input.value = "picot-v3";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    dialog.querySelector(".sidebar-confirm-yes").click();

    await operation;
    const deleteRequests = fetchMock.mock.calls.filter(([url]) =>
      String(url).includes("/api/sessions/delete-batch"),
    );
    expect(deleteRequests).toHaveLength(1);
    expect(fetchMock.lastPayload).toEqual({ filePaths: ["/s/a.jsonl", "/s/b.jsonl"] });
  });

  test("workspace delete modal keeps input and actions in separate flow sections", () => {
    const sidebar = makeSidebar();
    const operation = sidebar.deleteWorkspaceSessions({
      ...WORKSPACE,
      folderName: "w",
    });
    const dialog = document.querySelector(".workspace-delete-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    const actions = dialog.querySelector(".sidebar-confirm-actions");

    expect(dialog).toBeTruthy();
    expect(input).toBeTruthy();
    expect(actions).toBeTruthy();
    expect(input.closest(".workspace-delete-confirm-label")).toBeTruthy();
    expect(actions.previousElementSibling).toBe(dialog.querySelector(".workspace-delete-warning"));
    expect(dialog.classList.contains("sidebar-confirm-dialog")).toBe(true);

    dialog.querySelector(".sidebar-confirm-no").click();
    return operation;
  });

  test("cold registry workspaces force-load visible main sessions before confirmation", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 2, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const workspace = {
      source: "registry",
      path: "/work/cold",
      folderName: "cold",
      sessions: [],
    };
    const ensure = vi
      .spyOn(sidebar, "ensureWorkspaceSessions")
      .mockImplementation(async (project, options) => {
        expect(options).toEqual({ force: true });
        project.sessions = [
          { filePath: "/s/main-a.jsonl", name: "Main A" },
          { filePath: "/s/main-b.jsonl", name: "Main B" },
        ];
        project.sessionCount = 2;
        project.hiddenSubagentCount = 3;
      });

    const operation = sidebar.deleteWorkspaceSessions(workspace);
    await vi.waitFor(() => {
      expect(document.querySelector(".sidebar-confirm-dialog")).not.toBeNull();
    });
    expect(document.querySelector(".sidebar-confirm-message").textContent).toContain(
      "2 main sessions",
    );
    const input = document.querySelector(".workspace-delete-confirm-input");
    input.value = "cold";
    document.querySelector(".sidebar-confirm-yes").click();
    await operation;

    expect(ensure).toHaveBeenCalledWith(workspace, { force: true });
    expect(fetchMock.lastPayload).toEqual({
      filePaths: ["/s/main-a.jsonl", "/s/main-b.jsonl"],
    });
  });

  test("waits for an in-flight registry load before force-loading delete paths", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 1, errors: [], running: [] });
    global.fetch = fetchMock;
    const sidebar = makeSidebar();
    const workspace = {
      source: "registry",
      registryId: "workspace-id",
      workspaceId: "ws:workspace-id",
      path: "/work/racing",
      folderName: "racing",
      sessions: [{ filePath: "/s/stale.jsonl", name: "Stale" }],
    };
    sidebar.projects = [workspace];
    let releaseInitialLoad;
    sidebar.transport.workspaceSessions = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            releaseInitialLoad = resolve;
          }),
      )
      .mockResolvedValue({
        dirName: "--bucket--",
        sessions: [{ filePath: "/s/fresh.jsonl", name: "Fresh" }],
        sessionCount: 1,
        hiddenSubagentCount: 0,
      });
    sidebar.refresh = vi.fn(async () => {
      sidebar.projects = [workspace];
    });

    const initialLoad = sidebar.ensureWorkspaceSessions(workspace);
    await vi.waitFor(() => expect(sidebar._registryBusyRows.has(workspace.workspaceId)).toBe(true));
    const deletion = sidebar.deleteWorkspaceSessions(workspace);
    expect(document.querySelector(".sidebar-confirm-dialog")).toBeNull();

    releaseInitialLoad({
      dirName: "--bucket--",
      sessions: [{ filePath: "/s/stale.jsonl", name: "Stale" }],
      sessionCount: 1,
      hiddenSubagentCount: 0,
    });
    await initialLoad;
    await vi.waitFor(() => {
      expect(document.querySelector(".sidebar-confirm-dialog")).not.toBeNull();
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    dialog.querySelector(".workspace-delete-confirm-input").value = "racing";
    dialog.querySelector(".sidebar-confirm-yes").click();
    await deletion;

    expect(fetchMock.lastPayload).toEqual({ filePaths: ["/s/fresh.jsonl"] });
  });

  test("reports when only hidden subagent sessions remain", async () => {
    const notice = vi.fn();
    const sidebar = makeSidebar({ notice });
    const workspace = {
      source: "registry",
      path: "/work/hidden-only",
      folderName: "hidden-only",
      sessions: [],
    };
    vi.spyOn(sidebar, "ensureWorkspaceSessions").mockImplementation(async (project) => {
      project.sessions = [];
      project.hiddenSubagentCount = 2;
    });

    await sidebar.deleteWorkspaceSessions(workspace);

    expect(document.querySelector(".sidebar-confirm-dialog")).toBeNull();
    expect(sidebar.transport.sessionDeleteBatch).not.toHaveBeenCalled();
    expect(notice).toHaveBeenCalledWith(
      "No main sessions to delete; hidden subagent sessions were kept.",
    );
  });

  test("reports retained hidden subagent sessions after main-session deletion", async () => {
    const fetchMock = deleteBatchFetch({ deleted: 2, errors: [], running: [] });
    global.fetch = fetchMock;
    const notice = vi.fn();
    const sidebar = makeSidebar({ notice });
    const workspace = {
      source: "registry",
      path: "/work/retained",
      folderName: "retained",
      sessions: [{ filePath: "/s/main-a.jsonl" }, { filePath: "/s/main-b.jsonl" }],
    };
    vi.spyOn(sidebar, "ensureWorkspaceSessions").mockImplementation(async (project) => {
      project.sessions = workspace.sessions;
      project.sessionCount = 2;
      project.hiddenSubagentCount = 2;
    });
    sidebar.refresh = vi.fn(async () => {
      sidebar.projects = [workspace];
    });

    const operation = sidebar.deleteWorkspaceSessions(workspace);
    await vi.waitFor(() => {
      expect(document.querySelector(".sidebar-confirm-dialog")).not.toBeNull();
    });
    const dialog = document.querySelector(".sidebar-confirm-dialog");
    const input = dialog.querySelector(".workspace-delete-confirm-input");
    input.value = "retained";
    dialog.querySelector(".sidebar-confirm-yes").click();
    await operation;

    expect(notice).toHaveBeenCalledWith(
      "Deleted 2 main sessions. 2 hidden subagent sessions were kept.",
    );
  });

  test("workspace context menu renders the delete-all entry", () => {
    const sidebar = makeSidebar();
    // Cookie pin store is retired; empty registry pins keep the menu lean.
    sidebar._registryPins = [];
    const event = new window.MouseEvent("contextmenu", { bubbles: true });
    Object.defineProperty(event, "preventDefault", { value: vi.fn() });

    sidebar.showWorkspaceContextMenu(event, WORKSPACE);

    const items = [...document.querySelectorAll(".context-menu-item")].map((b) => b.textContent);
    expect(items).toContain("Delete all main sessions");
  });
});

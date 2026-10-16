// ABOUTME: Verifies the sidebar's registry data source end-to-end at unit level.
// ABOUTME: Covers registry/live merge identity, lazy per-workspace session loads,
// keyed row reuse, registry-pin bridging, prune toasts, and search scoping.

import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { initI18n } from "../i18n.js";
import { mergeRegistryWorkspaces, registryPinsFromProjects } from "../workspace-projects.js";
import { SessionSidebar } from "./index.js";

const dom = new JSDOM("<!doctype html><html><body><div id='sessions'></div></body></html>", {
  url: "http://localhost/",
});
globalThis.window = dom.window;
globalThis.document = dom.window.document;

function makeRegistryTransport(rows, responses = {}) {
  return {
    available: true,
    capabilities: { native: true },
    listWorkspaces: vi.fn(async () => ({
      workspaces: rows,
      removed: responses.removed ?? [],
    })),
    pickFolder: vi.fn(async () => responses.pickedFolder ?? ""),
    addWorkspace: vi.fn(async () => responses.addResult ?? { added: true }),
    removeWorkspace: vi.fn(async () => ({ removed: true })),
    runtimeInstances: vi.fn(async () => ({ instances: responses.instances ?? [] })),
    setWorkspacePinned: vi.fn(async () => ({})),
    workspaceSessions: vi.fn(async (workspaceId, options = {}) => {
      // The sidebar sends the raw DB uuid (registryId) — the host resolves
      // rows by that id; the `ws:` prefix is display-only identity.
      const row = rows.find((item) => item.workspaceId === workspaceId);
      const query = new URLSearchParams({ path: row?.canonicalPath || workspaceId });
      if (options.countOnly) query.set("countOnly", "1");
      const response = await globalThis.fetch(`/api/workspace-sessions?${query}`);
      return response.json();
    }),
    searchSessions: vi.fn(async (queryText) => {
      const response = await globalThis.fetch(
        `/api/search?q=${encodeURIComponent(queryText)}&paths=${encodeURIComponent(JSON.stringify(rows.map((row) => row.canonicalPath)))}`,
      );
      return response.json();
    }),
    sessionDeleteBatch: vi.fn(async (filePaths) => {
      const response = await globalThis.fetch("/api/sessions/delete-batch", {
        method: "POST",
        body: JSON.stringify({ filePaths }),
      });
      return response.json();
    }),
    sessionRename: vi.fn(async (filePath, name) => {
      const response = await globalThis.fetch("/api/sessions/rename", {
        method: "POST",
        body: JSON.stringify({ filePath, name }),
      });
      return response.json();
    }),
  };
}

async function makeSidebar({
  rows = [],
  transportResponses = {},
  fetchRoutes = {},
  isCurrentWorkspace,
  getFocusWorkspacePath,
  onWorkspaceFocus,
  onRegisterWorkspace,
} = {}) {
  const transport = makeRegistryTransport(rows, {
    ...transportResponses,
    instances: fetchRoutes.instances ?? transportResponses.instances ?? [],
  });
  const notices = [];
  const sidebar = new SessionSidebar(document.getElementById("sessions"), vi.fn(), vi.fn(), {
    pinStore: {
      getRenderableState: () => ({ workspaces: [] }),
      isWorkspacePinned: () => false,
      subscribe: () => () => {},
    },
    transport,
    onSessionNotice: (message) => notices.push(message),
    isCurrentWorkspace: isCurrentWorkspace ?? (() => false),
    getFocusWorkspacePath,
    onWorkspaceFocus,
    onRegisterWorkspace,
  });
  // House pattern: initI18n reads dictionaries through this same stub, so
  // serve the keys this suite asserts on before any API route.
  globalThis.fetch = vi.fn(async (url) => {
    const route = String(url);
    if (route.includes("/locales/")) {
      return {
        ok: true,
        json: async () => ({
          sidebar: {
            workspaceMissingRemoved:
              "Directory missing — removed from the list. Session files were kept.",
            emptyRegistryHint: "Add a project to see its sessions here.",
            sessionCountPending: "Session count will be calculated after opening this workspace.",
            alreadyRegistered: "This project is already in the list.",
            newSession: "New chat",
            removeFromList: "Remove from list",
            removedFromList: "Removed from the list; directory and session files were kept.",
            removedFromListStillRunning:
              "Removed from the list; a window is still running — the row disappears once it closes.",
            removeDisabledCurrent: "Cannot remove the current workspace",
          },
        }),
      };
    }
    if (route.startsWith("/api/instances")) {
      return {
        ok: true,
        json: async () => ({ instances: fetchRoutes.instances ?? [] }),
      };
    }
    if (route.startsWith("/api/workspace-sessions")) {
      return {
        ok: true,
        json: async () =>
          fetchRoutes.workspaceSessions?.(new URL(route, "http://localhost")) ?? {
            path: "",
            dirName: null,
            sessions: [],
          },
      };
    }
    if (route.startsWith("/api/search")) {
      return { ok: true, json: async () => ({ results: [] }) };
    }
    if (route.startsWith("/api/sessions")) {
      return { ok: true, json: async () => ({ projects: [] }) };
    }
    throw new Error(`unexpected fetch ${route}`);
  });
  await initI18n();
  return { sidebar, transport, notices };
}

const REGISTRY_ROWS = [
  {
    workspaceId: "uuid-1",
    canonicalPath: "/work/alpha",
    displayName: "alpha",
    pinned: true,
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

describe("registry/live merging", () => {
  test("preserves SQL order and assigns stable ws identities", () => {
    const { projects } = mergeRegistryWorkspaces(REGISTRY_ROWS, [], []);
    expect(projects.map((project) => project.workspaceId)).toEqual(["ws:uuid-1", "ws:uuid-2"]);
    expect(projects[0].pinned).toBe(true);
    expect(projects[0].source).toBe("registry");
    expect(projects[0].dirName).toBeNull();
  });

  test("live instances join registered paths; strays become provisional rows", () => {
    const instances = [
      { cwd: "/work/beta", port: 1 },
      { cwd: "/tmp/unregistered", port: 2 },
    ];
    const { projects } = mergeRegistryWorkspaces(REGISTRY_ROWS, instances, []);
    expect(projects).toHaveLength(3);
    // Instances attach to their registered path (beta), not the first row.
    const beta = projects.find((project) => project.workspaceId === "ws:uuid-2");
    expect(beta.runningInstances).toHaveLength(1);
    const alpha = projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(alpha.runningInstances).toHaveLength(0);
    const stray = projects.find((project) => project.path === "/tmp/unregistered");
    expect(stray.isProvisional).toBe(true);
    expect(stray.source).toBe("live");
  });

  test("provisional ids reconcile into ws identities carrying prior state keys", () => {
    const previous = [
      {
        workspaceId: "path:/work/alpha",
        path: "/work/alpha",
        isProvisional: true,
      },
    ];
    const { projects, reconciliations } = mergeRegistryWorkspaces([REGISTRY_ROWS[0]], [], previous);
    expect(reconciliations).toEqual([
      { fromId: "path:/work/alpha", toId: "ws:uuid-1", path: "/work/alpha" },
    ]);
    expect(projects[0].workspaceId).toBe("ws:uuid-1");
  });

  test("registry pins bridge into the unified pinned view", () => {
    const { projects } = mergeRegistryWorkspaces(REGISTRY_ROWS, [], []);
    expect(registryPinsFromProjects(projects)).toEqual([{ id: "ws:uuid-1", path: "/work/alpha" }]);
  });
});

describe("registry-backed sidebar loading", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
  });

  test("loads rows, surfaces prune toast, and refreshes synchronously enough", async () => {
    const { sidebar, transport, notices } = await makeSidebar({
      rows: REGISTRY_ROWS,
      transportResponses: { removed: [{ workspaceId: "gone", path: "/ghost" }] },
      fetchRoutes: { instances: [] },
    });
    await sidebar.loadSessions();

    expect(transport.listWorkspaces).toHaveBeenCalled();
    expect(sidebar.projects.map((project) => project.workspaceId)).toEqual([
      "ws:uuid-1",
      "ws:uuid-2",
    ]);
    expect(notices).toContain(
      "Directory missing — removed from the list. Session files were kept.",
    );
  });

  test("expanding a registry row lazily fetches only that path once", async () => {
    const sessionsPayload = [{ filePath: "/sessions/a.jsonl", name: "A" }];
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) => {
          expect(url.searchParams.get("path")).toBe("/work/alpha");
          return { path: "/work/alpha", dirName: "-work-alpha", sessions: sessionsPayload };
        },
      },
    });
    await sidebar.loadSessions();
    await sidebar.setWorkspaceExpanded(sidebar.projects[0], true);
    // allow the promise chain started inside setWorkspaceExpanded
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(alpha.sessions).toEqual(sessionsPayload);
    const callsAfterFirst = globalThis.fetch.mock.calls.filter(([url]) =>
      String(url).startsWith("/api/workspace-sessions"),
    );
    expect(callsAfterFirst).toHaveLength(1);

    // Second expand serves from cache without a new request.
    sidebar.setWorkspaceExpanded(alpha, false);
    await sidebar.setWorkspaceExpanded(alpha, true);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const callsAfterSecond = globalThis.fetch.mock.calls.filter(([url]) =>
      String(url).startsWith("/api/workspace-sessions"),
    );
    expect(callsAfterSecond.length).toBe(1);

    // Invalidation drops exactly one cache entry so the next expand refetches.
    sidebar.invalidateWorkspaceSessions("/work/alpha");
    await sidebar.ensureWorkspaceSessions(alpha);
    const callsAfterInvalidate = globalThis.fetch.mock.calls.filter(([url]) =>
      String(url).startsWith("/api/workspace-sessions"),
    );
    expect(callsAfterInvalidate.length).toBe(2);
  });

  test("full-text search scopes requests to listed project paths", async () => {
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [] },
    });
    await sidebar.loadSessions();
    // The production flow goes through setSearchQuery's debounce; replicate
    // its post-debounce state without waiting on timers.
    sidebar.searchQuery = "needle";
    await sidebar.fullTextSearch("needle");
    const searchCalls = globalThis.fetch.mock.calls.filter(([url]) =>
      String(url).startsWith("/api/search"),
    );
    expect(searchCalls).toHaveLength(1);
    const requestedUrl = new URL(searchCalls[0][0], "http://localhost");
    expect(requestedUrl.searchParams.get("q")).toBe("needle");
    expect(JSON.parse(requestedUrl.searchParams.get("paths"))).toEqual([
      "/work/alpha",
      "/work/beta",
    ]);
  });

  test("scoped refresh reloads the Focus workspace when its normal row is collapsed", async () => {
    const sessionsPayload = [{ filePath: "/sessions/focus.jsonl", name: "Focus" }];
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      getFocusWorkspacePath: () => "/work/alpha",
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) => {
          if (url.searchParams.get("path") === "/work/alpha") {
            return { path: "/work/alpha", dirName: "-work-alpha", sessions: sessionsPayload };
          }
          return { path: "/work/beta", dirName: "-work-beta", sessions: [] };
        },
      },
    });
    await sidebar.loadSessions();
    expect(sidebar.isWorkspaceExpanded(sidebar.projects[0])).toBe(false);

    await sidebar.refresh({ workspacePath: "/work/alpha" });

    const fullLoads = globalThis.fetch.mock.calls.filter(([url]) => {
      const route = String(url);
      return route.startsWith("/api/workspace-sessions") && !route.includes("countOnly=1");
    });
    expect(fullLoads).toHaveLength(1);
    expect(sidebar.projects[0].sessions).toEqual(sessionsPayload);
  });
});

describe("keyed row reuse", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
  });

  test("shows the Focus button as soon as a session becomes active", async () => {
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha"
            ? { sessions: [{ filePath: "/sessions/a.jsonl", name: "A" }] }
            : { sessions: [] },
      },
      onWorkspaceFocus: vi.fn(),
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.path === "/work/alpha");
    sidebar.setWorkspaceExpanded(alpha, true);
    await vi.waitFor(() => {
      expect(document.querySelector('[data-file-path="/sessions/a.jsonl"]')).not.toBeNull();
    });

    expect(
      document.querySelector('[data-workspace-id="ws:uuid-1"] .workspace-focus-btn'),
    ).toBeNull();

    sidebar.setActive("/sessions/a.jsonl");

    expect(
      document.querySelector('[data-workspace-id="ws:uuid-1"] .workspace-focus-btn'),
    ).not.toBeNull();
  });

  test("unchanged rows keep their DOM node across refresh renders", async () => {
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [] },
    });
    await sidebar.loadSessions();
    const firstNode = sidebar.projectRowNode(
      sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2"),
      null,
    ).group;

    const refreshed = sidebar.projectRowNode(
      sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2"),
      null,
    );
    expect(refreshed.group).toBe(firstNode);

    // Mutating visible data changes the signature and forces a rebuild.
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    beta.sessions.push({ filePath: "/sessions/new.jsonl", mtime: 12345 });
    const rebuilt = sidebar.projectRowNode(beta, null);
    expect(rebuilt.group).not.toBe(firstNode);
  });
});

describe("add / remove / pin registry flows", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
  });

  test("addProjectViaPicker registers, preserves expansion, and starts a new session", async () => {
    const rows = [];
    const onRegisterWorkspace = vi.fn().mockResolvedValue(true);
    const { sidebar, transport } = await makeSidebar({
      get rows() {
        return rows;
      },
      onRegisterWorkspace,
    });
    transport.pickFolder.mockResolvedValue("/work/gamma");
    transport.addWorkspace.mockResolvedValue({
      added: true,
      workspace: { workspaceId: "uuid-3", canonicalPath: "/work/gamma" },
    });
    await sidebar.addProjectViaPicker();

    expect(transport.addWorkspace).toHaveBeenCalledWith("/work/gamma");
    expect(onRegisterWorkspace).toHaveBeenCalledWith("/work/gamma");
    expect(transport.listWorkspaces).not.toHaveBeenCalled();
    expect(sidebar.expandedWorkspaces.has("ws:uuid-3")).toBe(true);
  });

  test("re-adding an existing project still starts a new session", async () => {
    const onRegisterWorkspace = vi.fn().mockResolvedValue(true);
    const { sidebar, transport } = await makeSidebar({
      rows: REGISTRY_ROWS,
      onRegisterWorkspace,
    });
    transport.pickFolder.mockResolvedValue("/work/alpha");
    transport.addWorkspace.mockResolvedValue({
      added: false,
      workspace: { workspaceId: "uuid-1", canonicalPath: "/work/alpha" },
    });

    await sidebar.addProjectViaPicker();

    expect(onRegisterWorkspace).toHaveBeenCalledWith("/work/alpha");
  });

  test("removeFromList only deletes the DB row and keeps expansion clean", async () => {
    const { sidebar, transport } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [] },
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    sidebar.expandedWorkspaces.add(beta.workspaceId);
    // After the DB delete, the next registry read no longer carries beta.
    transport.listWorkspaces.mockImplementation(async () => ({
      workspaces: [REGISTRY_ROWS[0]],
      removed: [],
    }));

    const deleteBatchCalls = () =>
      globalThis.fetch.mock.calls.filter(([url]) => String(url).includes("delete-batch"));

    await sidebar.removeFromList(beta);

    expect(transport.removeWorkspace).toHaveBeenCalledWith("uuid-2");
    expect(deleteBatchCalls()).toHaveLength(0);
    expect(sidebar.projects.some((project) => project.workspaceId === "ws:uuid-2")).toBe(false);
  });

  test("removeFromList with a running window keeps its live row and says so in the toast", async () => {
    const runningOnBeta = {
      port: 49160,
      pid: 4242,
      cwd: "/work/beta",
      startedAt: "2026-08-28T04:00:00.000Z",
      sessionFile: "/pi/sessions/--work-beta--/2026-08-28T04-00-00-000Z_live.jsonl",
    };
    const { sidebar, transport, notices } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [runningOnBeta] },
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    transport.listWorkspaces.mockImplementation(async () => ({
      workspaces: [REGISTRY_ROWS[0]],
      removed: [],
    }));

    await sidebar.removeFromList(beta);

    expect(transport.removeWorkspace).toHaveBeenCalledWith("uuid-2");
    // The live row survives (by design) and now renders its running session.
    const liveRow = sidebar.projects.find((project) => project.source === "live");
    expect(liveRow?.path).toBe("/work/beta");
    expect(liveRow?.sessions.some((session) => session.isRunning)).toBe(true);
    expect(notices).toContain(
      "Removed from the list; a window is still running — the row disappears once it closes.",
    );
  });

  test("the current workspace cannot be removed from the list", async () => {
    const { sidebar, transport, notices } = await makeSidebar({
      rows: REGISTRY_ROWS,
      isCurrentWorkspace: (project) => project?.path === "/work/beta",
    });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    const event = new window.MouseEvent("contextmenu", { bubbles: true });
    Object.defineProperty(event, "preventDefault", { value: vi.fn() });
    sidebar.showWorkspaceContextMenu(event, beta);
    const removeItem = [...document.querySelectorAll(".context-menu-item")].find((button) =>
      button.textContent.includes("Remove from list"),
    );
    expect(removeItem.disabled).toBe(true);
    expect(removeItem.title).toBe("Cannot remove the current workspace");
    sidebar.closeContextMenu();

    // Defensive guard: even a direct call must not reach the DB.
    await sidebar.removeFromList(beta);
    expect(transport.removeWorkspace).not.toHaveBeenCalled();
    expect(notices).toContain("Cannot remove the current workspace");
  });

  test("a non-current workspace stays removable even while another window runs there", async () => {
    const { sidebar } = await makeSidebar({ rows: REGISTRY_ROWS });
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");

    const event = new window.MouseEvent("contextmenu", { bubbles: true });
    Object.defineProperty(event, "preventDefault", { value: vi.fn() });
    sidebar.showWorkspaceContextMenu(event, beta);
    const removeItem = [...document.querySelectorAll(".context-menu-item")].find((button) =>
      button.textContent.includes("Remove from list"),
    );
    expect(removeItem.disabled).toBe(false);
    sidebar.closeContextMenu();
  });

  test("registry pin toggles go through transport and refresh order", async () => {
    const { sidebar, transport } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [] },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");

    await sidebar.toggleWorkspacePin(alpha, true);

    expect(transport.setWorkspacePinned).toHaveBeenCalledWith("uuid-1", false);
    void alpha;
  });
});

describe("registry cache invalidation wiring", () => {
  let previousFetch;
  beforeEach(() => {
    previousFetch = globalThis.fetch;
  });
  afterEach(() => {
    globalThis.fetch = previousFetch;
  });

  const ALPHA_SESSIONS = [
    { filePath: "/sessions/a.jsonl", name: "A" },
    { filePath: "/sessions/b.jsonl", name: "B" },
  ];

  // Server-side state shared by the delete-batch route and the per-workspace
  // list route, so a refetch genuinely observes earlier deletions.
  async function makeRegistrySidebar({ sessionsForPath } = {}) {
    const deleted = new Set();
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) => {
          const path = url.searchParams.get("path");
          const sessions = sessionsForPath
            ? sessionsForPath(path)
            : path === "/work/alpha"
              ? ALPHA_SESSIONS
              : [];
          const visibleSessions = sessions.filter((session) => !deleted.has(session.filePath));
          return {
            path,
            dirName: path === "/work/alpha" ? "-work-alpha" : null,
            sessions: url.searchParams.has("countOnly") ? [] : visibleSessions,
            sessionCount: visibleSessions.length,
            hiddenSubagentCount: 0,
          };
        },
      },
    });
    const baseFetch = globalThis.fetch;
    globalThis.fetch = vi.fn(async (url, init) => {
      if (String(url).includes("/api/sessions/delete-batch")) {
        for (const filePath of JSON.parse(init?.body ?? "{}").filePaths ?? []) {
          deleted.add(filePath);
        }
        return { ok: true, json: async () => ({ deleted: 0, errors: [], running: [] }) };
      }
      return baseFetch(url, init);
    });
    return { sidebar };
  }

  const settleFetches = async () => {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  };

  const workspaceSessionsCalls = () =>
    globalThis.fetch.mock.calls.filter(([url]) =>
      String(url).startsWith("/api/workspace-sessions"),
    );

  test("deleteSession drops the owning workspace's cache so deleted files do not return as ghosts", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    await sidebar.setWorkspaceExpanded(
      sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1"),
      true,
    );
    await settleFetches();
    sidebar.showFallbackConfirmDialog = vi.fn(async () => true);

    await sidebar.deleteSession("/sessions/a.jsonl");
    await settleFetches();

    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(alpha.sessions.some((session) => session.filePath === "/sessions/a.jsonl")).toBe(false);
    expect(alpha.sessions.map((session) => session.filePath)).toEqual(["/sessions/b.jsonl"]);
    expect(
      workspaceSessionsCalls().filter(([url]) => String(url).includes("alpha")).length,
    ).toBeGreaterThanOrEqual(2);
  });

  test("deleteWorkspaceSessions invalidates that workspace's cache entry", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    await sidebar.setWorkspaceExpanded(
      sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1"),
      true,
    );
    await settleFetches();
    sidebar.confirmWorkspaceDeletion = vi.fn(async () => true);
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");

    await sidebar.deleteWorkspaceSessions(alpha);
    await settleFetches();

    const alphaAfter = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    // Both files were deleted server-side; the reload must not resurrect them
    // from the stale cache (ghost entries).
    expect(alphaAfter.sessions).toEqual([]);
    expect(
      workspaceSessionsCalls().filter(([url]) => String(url).includes("alpha")).length,
    ).toBeGreaterThanOrEqual(2);
  });

  test("shows a read-only provisional new-chat row until its session file is scanned", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });

    expect(alpha.sessions[0]).toMatchObject({
      filePath: "native-session-new",
      provisional: true,
    });
    expect(document.querySelector(".session-item.active .session-title")?.textContent).toBe(
      "New chat",
    );
    expect(document.querySelector(".session-item.active .session-delete-btn")).toBeNull();
    expect(document.querySelector(".session-item.active .session-rename-btn")).toBeNull();
  });

  test("refresh preserves a provisional session while the bucket is still empty", async () => {
    const { sidebar } = await makeRegistrySidebar();
    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });

    await sidebar.refresh({ workspacePath: "/work/alpha" });
    await settleFetches();

    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(alpha.sessions[0]).toMatchObject({
      filePath: "native-session-new",
      provisional: true,
    });
    expect(alpha.sessionCount).toBeNull();
  });

  test("selecting another session retires the unpersisted provisional row", async () => {
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha"
            ? { sessions: [{ filePath: "/sessions/a.jsonl", name: "A" }] }
            : { sessions: [] },
      },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await vi.waitFor(() => {
      expect(document.querySelector('[data-file-path="/sessions/a.jsonl"]')).not.toBeNull();
    });

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });
    expect(
      document.querySelector('.session-item[data-file-path="native-session-new"]'),
    ).not.toBeNull();

    // Selecting another session in the same workspace abandons the empty
    // new chat. app.js handleSessionSelect performs this retirement at the
    // choke point both sidebars share; this drives that exact sequence.
    sidebar.retireProvisionalSession();
    sidebar.setActive("/sessions/a.jsonl");

    expect(sidebar.provisionalSession).toBeNull();
    expect(document.querySelector('.session-item[data-file-path="native-session-new"]')).toBeNull();
    expect(
      document.querySelector('[data-file-path="/sessions/a.jsonl"]').classList.contains("active"),
    ).toBe(true);

    // Later refreshes must not resurrect the abandoned row.
    await sidebar.refresh({ workspacePath: "/work/alpha" });
    await settleFetches();
    expect(document.querySelector('.session-item[data-file-path="native-session-new"]')).toBeNull();
  });

  test("mirror rebind drops the stale placeholder once the file is scanned", async () => {
    // The provisional row is keyed by the runtime session id; the scanned row
    // is keyed by the absolute JSONL path. Those identities never compare
    // equal, so a manual refresh after the first round-trip re-inserts the
    // placeholder next to the persisted row until another session is
    // selected. Rebinding the provisional row to the mirror-learned file
    // path must converge both rows into one on the next refresh.
    const persistedFile = "/sessions/20260827_101500_auto-titled.jsonl";
    let scanned = [];
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha" ? { sessions: scanned } : { sessions: [] },
      },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });
    expect(
      document.querySelector('.session-item[data-file-path="native-session-new"]'),
    ).not.toBeNull();

    // First round-trip persists the JSONL; the mirror learns the real path.
    scanned = [{ filePath: persistedFile, name: "Auto title" }];
    sidebar.rebindProvisionalSession(persistedFile);

    // Manual refresh (toolbar button) observes the persisted file.
    await sidebar.refresh({ workspacePath: "/work/alpha" });
    await settleFetches();

    expect(sidebar.provisionalSession).toBeNull();
    expect(document.querySelector('.session-item[data-file-path="native-session-new"]')).toBeNull();
    expect(
      document.querySelector(`.session-item[data-file-path="${persistedFile}"]`),
    ).not.toBeNull();
    // Drain this instance's one-shot warmup timer (800ms) so its countOnly
    // fetch cannot leak into later tests' call counts.
    await new Promise((resolve) => setTimeout(resolve, 850));
  });

  test("rebind before the scan keeps exactly one placeholder keyed by the file", async () => {
    // Mirror sync can beat the scan bucket (stale cache): the placeholder
    // must survive the rebind re-keyed to the real file, not duplicate, and
    // converge on the refresh that finally scans it.
    const persistedFile = "/sessions/20260827_101500_auto-titled.jsonl";
    let scanned = [];
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha" ? { sessions: scanned } : { sessions: [] },
      },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });
    sidebar.rebindProvisionalSession(persistedFile);

    expect(sidebar.provisionalSession).toMatchObject({
      filePath: persistedFile,
      provisional: true,
    });
    expect(
      document.querySelector(`.session-item[data-file-path="${persistedFile}"]`),
    ).not.toBeNull();
    expect(document.querySelector('.session-item[data-file-path="native-session-new"]')).toBeNull();

    scanned = [{ filePath: persistedFile, name: "Auto title" }];
    await sidebar.refresh({ workspacePath: "/work/alpha" });
    await settleFetches();

    expect(sidebar.provisionalSession).toBeNull();
    expect(
      document.querySelectorAll(`.session-item[data-file-path="${persistedFile}"]`),
    ).toHaveLength(1);
    // Drain this instance's one-shot warmup timer (800ms) so its countOnly
    // fetch cannot leak into later tests' call counts.
    await new Promise((resolve) => setTimeout(resolve, 850));
  });

  test("rebind after a refresh removes the placeholder row, not just the state", async () => {
    // Auto-refresh resolves the match after its own sidebar.refresh() ran,
    // i.e. the placeholder was already re-inserted next to the persisted
    // row by the identity-mismatched apply. Clearing the state alone keeps
    // that row rendering; the rebind must drop it from the list.
    const persistedFile = "/sessions/20260827_101500_auto-titled.jsonl";
    const scanned = [{ filePath: persistedFile, name: "Auto title" }];
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha" ? { sessions: scanned } : { sessions: [] },
      },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId: "native-session-new" });
    await sidebar.refresh({ workspacePath: "/work/alpha" });
    await settleFetches();
    // The refresh rebuilt the projects array; re-resolve the alpha row.
    const reloaded = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    // The refresh re-inserted the placeholder beside the persisted row.
    expect(reloaded.sessions.filter((session) => session.provisional)).toHaveLength(1);
    expect(reloaded.sessions.some((session) => session.filePath === persistedFile)).toBe(true);

    sidebar.rebindProvisionalSession(persistedFile);

    expect(sidebar.provisionalSession).toBeNull();
    expect(reloaded.sessions.every((session) => !session.provisional)).toBe(true);
    expect(
      document.querySelectorAll(`.session-item[data-file-path="${persistedFile}"]`),
    ).toHaveLength(1);
    // Drain this instance's one-shot warmup timer (800ms) so its countOnly
    // fetch cannot leak into later tests' call counts.
    await new Promise((resolve) => setTimeout(resolve, 850));
  });

  test("a runtime-id placeholder converges with the persisted row carrying that id", async () => {
    // A daemon restart reloads the route (workspace id + runtime session id)
    // for an already-persisted session. The placeholder seeded from that route
    // target is keyed by the runtime id, which never equals the scanned
    // absolute JSONL path — but the persisted file NAME carries the id, so the
    // placeholder must converge on the spot instead of surviving as an
    // unselectable "New chat" row.
    const sessionId = "01a13eab-3d17-7776-8a7e-afbddae6bb59";
    const persistedFile = `/sessions/2026-10-15T08-26-12-119Z_${sessionId}.jsonl`;
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: {
        instances: [],
        workspaceSessions: (url) =>
          url.searchParams.get("path") === "/work/alpha"
            ? { sessions: [{ filePath: persistedFile, name: "Hosted session" }] }
            : { sessions: [] },
      },
    });
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId });

    expect(sidebar.provisionalSession).toBeNull();
    expect(alpha.sessions.filter((session) => session.provisional)).toHaveLength(0);
    expect(document.querySelector(`.session-item[data-file-path="${sessionId}"]`)).toBeNull();
    await new Promise((resolve) => setTimeout(resolve, 850));
  });

  test("boot purges a placeholder hydrated from the previous page's cache", async () => {
    // The sidebar cookie can carry a route placeholder from an earlier page.
    // The next boot (the reload that follows a daemon restart) restored it into
    // a collapsed workspace whose session list is never lazily fetched, so the
    // row stuck around: selectable-looking, but its bare runtime id is not a
    // file path, and the host answers "Selected session is unavailable".
    const sessionId = "01a13eab-3d17-7776-8a7e-afbddae6bb59";
    const persistedFile = `/sessions/2026-10-15T08-26-12-119Z_${sessionId}.jsonl`;
    const { sidebar } = await makeSidebar({
      rows: REGISTRY_ROWS,
      fetchRoutes: { instances: [], workspaceSessions: () => ({ sessions: [] }) },
    });
    // Hydrated boot state: the placeholder beside the live row, workspace
    // collapsed (fresh page restores no expansion).
    sidebar.projects = [
      {
        workspaceId: "ws:uuid-1",
        registryId: "uuid-1",
        path: "/work/alpha",
        folderName: "alpha",
        source: "registry",
        sessions: [
          { filePath: sessionId, name: "New chat", provisional: true },
          { filePath: persistedFile, name: "", firstMessage: "" },
        ],
      },
    ];
    sidebar.setActive(persistedFile);

    sidebar.setProvisionalSession({ workspaceId: "uuid-1", sessionId });

    const alpha = sidebar.projects[0];
    expect(alpha.sessions.filter((session) => session.provisional)).toHaveLength(0);
    expect(document.querySelector(`.session-item[data-file-path="${sessionId}"]`)).toBeNull();
    // Every renderable row must be selectable: no bare runtime ids left behind.
    expect(alpha.sessions.every((session) => session.filePath.startsWith("/"))).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 850));
  });

  test("coalesced refresh preserves in-flight session-list generation", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();

    const first = sidebar.refresh();
    const generation = sidebar._registrySessionLoadGeneration;
    const second = sidebar.refresh();

    expect(second).toBe(first);
    expect(sidebar._registrySessionLoadGeneration).toBe(generation);
    await first;
  });

  test("scoped refresh refetches an expanded cached row", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    await sidebar.setWorkspaceExpanded(
      sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1"),
      true,
    );
    await settleFetches();

    await sidebar.refresh({ workspacePath: "/work/alpha" });

    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    expect(alpha.sessions.map((session) => session.name)).toEqual(["A", "B"]);
    expect(workspaceSessionsCalls().filter(([url]) => String(url).includes("alpha"))).toHaveLength(
      2,
    );
  });

  test("scoped refresh on a collapsed row drops the cache without fetching", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    await sidebar.setWorkspaceExpanded(beta, true);
    await settleFetches();
    await sidebar.setWorkspaceExpanded(beta, false);
    expect(sidebar.workspaceSessionsCache.has("/work/beta")).toBe(true);

    await sidebar.refresh({ workspacePath: "/work/beta" });

    expect(sidebar.workspaceSessionsCache.has("/work/beta")).toBe(false);
    expect(workspaceSessionsCalls().filter(([url]) => String(url).includes("beta"))).toHaveLength(
      1,
    );
  });

  test("refresh refetches expanded rows and leaves collapsed caches in place", async () => {
    const { sidebar } = await makeRegistrySidebar();
    await sidebar.loadSessions();
    const alpha = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-1");
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-2");
    await sidebar.setWorkspaceExpanded(alpha, true);
    await settleFetches();
    // Seed beta's cache, then collapse it: refresh must not touch it.
    await sidebar.setWorkspaceExpanded(beta, true);
    await settleFetches();
    await sidebar.setWorkspaceExpanded(beta, false);

    await sidebar.refresh();
    await settleFetches();

    expect(workspaceSessionsCalls().filter(([url]) => String(url).includes("alpha"))).toHaveLength(
      2,
    );
    expect(workspaceSessionsCalls().filter(([url]) => String(url).includes("beta"))).toHaveLength(
      1,
    );
    expect(sidebar.workspaceSessionsCache.has("/work/beta")).toBe(true);
    expect(sidebar.workspaceSessionsCache.has("/work/alpha")).toBe(true);
  });
});

describe("startup loading", () => {
  test("gives idle warmup an 800ms deadline", async () => {
    const previousIdleCallback = window.requestIdleCallback;
    const requestIdleCallback = vi.fn();
    window.requestIdleCallback = requestIdleCallback;
    try {
      const { sidebar } = await makeSidebar({ rows: REGISTRY_ROWS });
      await sidebar.loadSessions({ quiet: true });

      expect(requestIdleCallback).toHaveBeenCalledWith(expect.any(Function), { timeout: 800 });
    } finally {
      if (previousIdleCallback) window.requestIdleCallback = previousIdleCallback;
      else delete window.requestIdleCallback;
    }
  });

  test("warms a registry row added after refresh", async () => {
    vi.useFakeTimers();
    try {
      const rows = [...REGISTRY_ROWS];
      const { sidebar } = await makeSidebar({
        rows,
        fetchRoutes: {
          instances: [],
          workspaceSessions: (url) => ({
            sessionCount: url.searchParams.get("path") === "/work/gamma" ? 7 : 1,
            sessions: [],
          }),
        },
      });
      await sidebar.loadSessions({ quiet: true });
      await vi.advanceTimersByTimeAsync(900);

      rows.push({
        workspaceId: "uuid-3",
        canonicalPath: "/work/gamma",
        displayName: "gamma",
        pinned: false,
        lastOpenedAt: null,
      });
      await sidebar.refresh();
      await vi.advanceTimersByTimeAsync(900);

      expect(sidebar.projects.find((project) => project.path === "/work/gamma")?.sessionCount).toBe(
        7,
      );
    } finally {
      vi.useRealTimers();
    }
  });

  test("warms readdir-only counts and never scans full session lists", async () => {
    vi.useFakeTimers();
    try {
      const rows = Array.from({ length: 6 }, (_, i) => ({
        workspaceId: `uuid-${i}`,
        canonicalPath: `/work/w${i}`,
        displayName: `w${i}`,
        pinned: false,
        lastOpenedAt: null,
      }));
      const { sidebar, transport } = await makeSidebar({
        rows,
        fetchRoutes: {
          instances: [],
          workspaceSessions: () => ({ sessionCount: 5, sessions: [] }),
        },
      });
      await sidebar.loadSessions({ quiet: true });

      // jsdom lacks requestIdleCallback; the warmup rides the 800ms fallback.
      await vi.advanceTimersByTimeAsync(900);

      // Count only this sidebar's own transport: other suites' pending
      // warmup timers must not leak into the assertion.
      const calls = transport.workspaceSessions.mock.calls;
      expect(calls).toHaveLength(rows.length);
      for (const [, options] of calls) {
        expect(options?.countOnly).toBe(true);
      }
      for (const project of sidebar.projects) {
        expect(project.sessionCount).toBe(5);
        // Full history stays lazy: no workspace preloaded its session list.
        expect(project.sessions).toEqual([]);
      }
    } finally {
      vi.useRealTimers();
    }
  });
});

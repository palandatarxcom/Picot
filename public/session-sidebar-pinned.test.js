// ABOUTME: Verifies SessionSidebar PINNED/registry rendering through the real sidebar path.
// ABOUTME: Covers region order, DB-pin groups, context-menu actions, and fold stability.
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { initI18n } from "./i18n.js";
import { SessionSidebar } from "./sidebar/index.js";

function makeQuickInfo() {
  return {
    bindHeader: vi.fn(),
    destroy: vi.fn(),
  };
}

const REG_ALPHA_SESSIONS = [{ filePath: "/sessions/alpha.jsonl", name: "Alpha" }];
const ROW_ALPHA = {
  workspaceId: "ws:uuid-alpha",
  path: "/work/alpha",
  dirName: null,
  source: "registry",
  registryId: "uuid-alpha",
  pinned: true,
  sessions: REG_ALPHA_SESSIONS,
};
const ROW_BETA = {
  workspaceId: "ws:uuid-beta",
  path: "/work/beta",
  dirName: null,
  source: "registry",
  registryId: "uuid-beta",
  pinned: false,
  sessions: [{ filePath: "/sessions/beta.jsonl", name: "Beta" }],
};
const LIVE_ROW = {
  workspaceId: "path:/work/live",
  path: "/work/live",
  dirName: "",
  isProvisional: true,
  source: "live",
  sessions: [],
};

// Raw workspace.list rows (DB shape) for transport-level fixtures.
const DB_ROWS = [
  {
    workspaceId: "uuid-alpha",
    canonicalPath: "/work/alpha",
    displayName: "alpha",
    pinned: true,
    lastOpenedAt: null,
  },
  {
    workspaceId: "uuid-beta",
    canonicalPath: "/work/beta",
    displayName: "beta",
    pinned: false,
    lastOpenedAt: null,
  },
];

function localeStub() {
  return {
    ok: true,
    json: async () => ({
      sidebar: {
        pinned: "PINNED",
        projects: "PROJECTS",
        unavailable: "Unavailable",
        pinWorkspace: "Pin workspace",
        unpinWorkspace: "Unpin workspace",
        removeFromList: "Remove from list",
        removedFromList: "Removed from the list; directory and session files were kept.",
        openInFinder: "Open in Finder",
        deleteWorkspaceMainSessions: "Delete all main sessions",
        deleteWorkspaceMainSessionsConfirm:
          "Delete {count} main sessions permanently? Hidden subagent sessions will be kept. This cannot be undone.",
        noMainSessionsToDelete: "No main sessions to delete; hidden subagent sessions were kept.",
        deletedMainSessionsSubagentsKept:
          "Deleted {count} main sessions. {hiddenCount} hidden subagent sessions were kept.",
        sessionCountPending: "Session count will be calculated after opening this workspace.",
        workspaceActions: "Workspace actions",
        showMore: "Show more",
        showLess: "Show less",
        openProject: "Open project",
        emptySession: "Empty session",
        deleteSession: "Delete",
        newChat: "New chat in {path}",
        justNow: "Just now",
      },
    }),
  };
}

function makeTransport({ rows = DB_ROWS, removed = [] } = {}) {
  return {
    available: true,
    capabilities: { native: true, management: true },
    listWorkspaces: vi.fn(async () => ({ workspaces: rows, removed })),
    addWorkspace: vi.fn(async () => ({ added: true })),
    removeWorkspace: vi.fn(async () => ({ removed: true })),
    setWorkspacePinned: vi.fn(async () => ({})),
    getPreference: vi.fn(async () => ({ value: null })),
    setPreference: vi.fn(async () => ({})),
    listPreferences: vi.fn(async () => ({ preferences: {} })),
  };
}

function makeRegistrySidebar({ transport, onSessionSelect, onNewChat, options = {} } = {}) {
  const sidebar = new SessionSidebar(
    document.getElementById("sessions"),
    onSessionSelect ?? vi.fn(),
    onNewChat ?? vi.fn(),
    {
      quickInfo: makeQuickInfo(),
      transport,
      ...options,
    },
  );
  return sidebar;
}

// Directly install a registry dataset without the async loader; several tests
// target pure render semantics, not data fetching.
function seedProjects(sidebar, projects, pins) {
  sidebar.projects = projects;
  sidebar._registryPins = pins;
  sidebar.render();
}

const RENDER_SET = [ROW_ALPHA, LIVE_ROW];

function workspaceExpanded(workspaceId) {
  const header = document.querySelector(
    `.workspace-group[data-workspace-id="${workspaceId}"] .workspace-header`,
  );
  return header?.getAttribute("aria-expanded") === "true";
}

beforeEach(async () => {
  const dom = new JSDOM('<div id="sessions"></div>', { url: "http://localhost:3001" });
  globalThis.document = dom.window.document;
  globalThis.localStorage = dom.window.localStorage;
  globalThis.CSS = dom.window.CSS || { escape: String };
  global.fetch = vi.fn(async (url) => {
    if (String(url).includes("/locales/en.json")) return localeStub();
    if (String(url).startsWith("/api/instances")) {
      return { ok: true, json: async () => ({ instances: [] }) };
    }
    if (String(url).startsWith("/api/workspace-sessions")) {
      return { ok: true, json: async () => ({ path: "", dirName: null, sessions: [] }) };
    }
    return { ok: false, json: async () => ({}) };
  });
  await initI18n();
});

afterEach(() => vi.restoreAllMocks());

describe("SessionSidebar regions", () => {
  test("renders fixed region order and keeps live zero-session workspaces", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), []);

    const regions = Array.from(document.querySelectorAll(".pinned-group, .projects-group"));
    expect(regions.map((region) => region.className.split(" ")[0])).toEqual([
      "pinned-group",
      "projects-group",
    ]);
    expect(document.querySelector(".favourites-group")).toBeNull();
    expect(document.querySelector(".pinned-unavailable")).toBeNull();
    expect(document.querySelector(".projects-group").textContent).toContain("live");
    expect(document.querySelector(".projects-group .sidebar-section-header").textContent).toContain(
      "PROJECTS",
    );
  });

  test("session rows never expose pin controls", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);
    const item = document.querySelector('.session-item[data-file-path="/sessions/alpha.jsonl"]');
    expect(item.querySelector(".session-pin-btn")).toBeNull();
    expect(item.querySelector(".session-delete-btn")).not.toBeNull();
  });

  test("moves pinned workspaces out of Projects into Pinned", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone([ROW_ALPHA, ROW_BETA, LIVE_ROW]), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    expect(
      [...document.querySelectorAll(".pinned-group .workspace-group")].map(
        (group) => group.dataset.workspaceId,
      ),
    ).toEqual(["ws:uuid-alpha"]);
    expect(
      [...document.querySelectorAll(".projects-group .workspace-group")].map(
        (group) => group.dataset.workspaceId,
      ),
    ).toEqual(["ws:uuid-beta", "path:/work/live"]);
  });

  test("expanding a pinned registry workspace lazy-loads its sessions", async () => {
    const transport = makeTransport();
    transport.workspaceSessions = vi.fn(async () => ({
      path: "/work/alpha",
      dirName: "--work-alpha--",
      sessions: [{ filePath: "/sessions/lazy.jsonl", name: "Lazy" }],
      sessionCount: 1,
      hiddenSubagentCount: 0,
    }));
    const sidebar = makeRegistrySidebar({ transport });
    const pinnedRow = { ...structuredClone(ROW_ALPHA), sessions: [], sessionCount: 67 };
    seedProjects(
      sidebar,
      [pinnedRow, structuredClone(LIVE_ROW)],
      [{ id: "ws:uuid-alpha", path: "/work/alpha" }],
    );

    const header = document.querySelector(
      '.pinned-group .workspace-group[data-workspace-id="ws:uuid-alpha"] .workspace-header',
    );
    header.click();

    await vi.waitFor(() => {
      expect(
        document.querySelector(
          '.pinned-group .session-item[data-file-path="/sessions/lazy.jsonl"]',
        ),
      ).not.toBeNull();
    });
    // Host-bound identity is the raw registry id, not the `ws:` display id.
    expect(transport.workspaceSessions).toHaveBeenCalledWith("uuid-alpha");
  });

  test("all section headers share the chevron and no folder icon", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);
    const headers = document.querySelectorAll(".sidebar-section-header");
    expect(headers.length).toBe(2);
    headers.forEach((header) => {
      expect(header.querySelector(".section-chevron")).not.toBeNull();
      expect(header.querySelector(".folder-icon")).toBeNull();
    });
  });
});

describe("SessionSidebar context menu", () => {
  test("registry rows expose pin, finder, remove-from-list, and delete-all", () => {
    const onOpenProject = vi.fn();
    const sidebar = makeRegistrySidebar({
      transport: makeTransport(),
      options: { onOpenProject },
    });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    const header = document.querySelector(".pinned-group .workspace-header");
    header.querySelector(".workspace-more-actions-btn").click();
    const menu = document.querySelector(".sidebar-context-menu").textContent;
    // Fixture row is pinned; toggle renders its inverse action.
    expect(menu).toContain("Unpin workspace");
    expect(menu).toContain("Open in Finder");
    expect(menu).toContain("Remove from list");
    expect(menu).toContain("Delete all main sessions");

    // Second item opens Finder with the whole row object.
    document.querySelector(".sidebar-context-menu .context-menu-item:nth-child(2)").click();
    expect(onOpenProject).toHaveBeenCalledWith(
      expect.objectContaining({ workspaceId: "ws:uuid-alpha", path: "/work/alpha" }),
    );
  });

  test("live-only rows keep destructive ops but no registry pin/remove entries", () => {
    const sidebar = makeRegistrySidebar({
      transport: makeTransport(),
      options: { onOpenProject: vi.fn() },
    });
    seedProjects(sidebar, [structuredClone(LIVE_ROW)], []);

    document
      .querySelector(".projects-group .workspace-header")
      .querySelector(".workspace-more-actions-btn")
      .click();
    const menu = document.querySelector(".sidebar-context-menu").textContent;
    expect(menu).not.toContain("Pin workspace");
    expect(menu).not.toContain("Remove from list");
    expect(menu).toContain("Delete all main sessions");
  });

  test("pin action goes through the broker", async () => {
    const transport = makeTransport();
    const sidebar = makeRegistrySidebar({ transport });
    // Seed a beta-only view: an unpinned registry row exercises the pin flip.
    seedProjects(sidebar, [structuredClone(ROW_BETA), structuredClone(LIVE_ROW)], []);
    const betaHeader = document.querySelector(
      '.projects-group .workspace-group[data-workspace-id="ws:uuid-beta"] .workspace-more-actions-btn',
    );
    expect(betaHeader).not.toBeNull();

    await sidebar.toggleWorkspacePin(sidebar.projects[0], false);
    expect(transport.setWorkspacePinned).toHaveBeenCalledWith("uuid-beta", true);
  });

  test("remove-from-list deletes only the registry row via transport", async () => {
    const transport = makeTransport();
    const spyDeleteBatch = vi.fn();
    global.fetch.mockImplementation(async (url) => {
      if (String(url).includes("delete-batch")) {
        spyDeleteBatch();
        return { ok: true, json: async () => ({}) };
      }
      if (String(url).includes("/locales/en.json")) return localeStub();
      if (String(url).startsWith("/api/instances")) {
        return { ok: true, json: async () => ({ instances: [] }) };
      }
      return { ok: false, json: async () => ({}) };
    });
    const sidebar = makeRegistrySidebar({ transport });
    await sidebar.loadSessions({ quiet: true });
    const beta = sidebar.projects.find((project) => project.workspaceId === "ws:uuid-beta");
    transport.listWorkspaces.mockImplementation(async () => ({
      workspaces: [ROW_ALPHA],
      removed: [],
    }));

    await sidebar.removeFromList(beta);

    expect(transport.removeWorkspace).toHaveBeenCalledWith("uuid-beta");
    expect(spyDeleteBatch).not.toHaveBeenCalled();
    expect(sidebar.projects.some((project) => project.workspaceId === "ws:uuid-beta")).toBe(false);
  });
});

describe("SessionSidebar PINNED section", () => {
  test("renders registry-pinned groups through the shared disclosure builder", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    const header = document.querySelector(".pinned-group .sidebar-section-header");
    const body = document.querySelector(".pinned-group .sidebar-section-sessions");
    expect(header.getAttribute("role")).toBe("button");
    expect(header.getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelector(".pinned-group .workspace-group")).not.toBeNull();

    header.dispatchEvent(new KeyboardEvent("keydown", { key: " " }));
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(body.classList.contains("collapsed")).toBe(true);
  });

  test("PINNED workspace new-chat passes the workspace object with its path", () => {
    const onNewChat = vi.fn();
    const sidebar = makeRegistrySidebar({
      transport: makeTransport(),
      onNewChat,
    });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    document.querySelector(".pinned-group .workspace-new-chat-btn").click();
    expect(onNewChat).toHaveBeenCalledWith(expect.objectContaining({ path: "/work/alpha" }));
  });
});

describe("SessionSidebar fold-state stability", () => {
  test("workspaces start collapsed and sections start expanded on first render", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    expect(workspaceExpanded("ws:uuid-alpha")).toBe(false);
    expect(workspaceExpanded("path:/work/live")).toBe(false);
    expect(
      document.querySelector(".pinned-group .sidebar-section-header").getAttribute("aria-expanded"),
    ).toBe("true");
    expect(
      document
        .querySelector(".projects-group .sidebar-section-header")
        .getAttribute("aria-expanded"),
    ).toBe("true");
  });

  test("rebuilding preserves expansion; no legacy archived state exists", () => {
    const sidebar = makeRegistrySidebar({ transport: makeTransport() });
    seedProjects(sidebar, structuredClone(RENDER_SET), [
      { id: "ws:uuid-alpha", path: "/work/alpha" },
    ]);

    document
      .querySelector(
        '.pinned-group .workspace-group[data-workspace-id="ws:uuid-alpha"] .workspace-header',
      )
      .click();
    expect(workspaceExpanded("ws:uuid-alpha")).toBe(true);
    expect(workspaceExpanded("path:/work/live")).toBe(false);

    sidebar.render();

    expect(workspaceExpanded("ws:uuid-alpha")).toBe(true);
    expect(workspaceExpanded("path:/work/live")).toBe(false);
    expect(document.querySelector(".archived-group")).toBeNull();
    expect("archivedCollapsed" in sidebar).toBe(false);
  });

  test("carries expansion across provisional-to-registered reconciliation", async () => {
    let rows = [];
    const transport = makeTransport({ rows });
    Object.defineProperty(transport, "listWorkspaces", {
      value: vi.fn(async () => ({ workspaces: rows, removed: [] })),
    });
    const sidebar = makeRegistrySidebar({ transport });

    sidebar.projects = [structuredClone(LIVE_ROW)];
    sidebar.render();
    sidebar.setWorkspaceExpanded({ workspaceId: "path:/work/live" }, true);
    expect(sidebar.isWorkspaceExpanded({ workspaceId: "path:/work/live" })).toBe(true);

    rows = [
      {
        workspaceId: "uuid-live",
        canonicalPath: "/work/live",
        displayName: "live",
        pinned: false,
        lastOpenedAt: null,
      },
    ];
    await sidebar.loadSessions({ quiet: true });

    expect(sidebar.isWorkspaceExpanded({ workspaceId: "ws:uuid-live" })).toBe(true);
    expect(sidebar.isWorkspaceExpanded({ workspaceId: "path:/work/live" })).toBe(false);
  });
});

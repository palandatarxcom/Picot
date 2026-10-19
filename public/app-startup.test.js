// ABOUTME: Verifies the browser entry module initializes against the production document.
// ABOUTME: Prevents startup errors from blocking the Settings dialog and every other control.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

const daemonHarness = vi.hoisted(() => ({ options: null, page: null }));
vi.mock("./settings/daemon-section.js", () => ({
  setupDaemonSection: (options) => {
    daemonHarness.options = options;
    daemonHarness.page = { activate: vi.fn(), leave: vi.fn() };
    return daemonHarness.page;
  },
}));

const enMessages = JSON.parse(readFileSync(join(process.cwd(), "public/locales/en.json"), "utf8"));

class FakeWebSocket extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances = [];

  constructor() {
    super();
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
    FakeWebSocket.instances.push(this);
  }

  send(message) {
    const frame = JSON.parse(message);
    this.sent.push(frame);
    if (frame.type === "data_request") {
      let response = null;
      if (frame.operation === "workspace_info") {
        response = {
          isGit:
            !String(frame.workspaceId || "").includes("non-git") &&
            !String(frame.workspaceId || "").includes("nongit"),
        };
      }
      if (frame.operation === "workspace_sessions") {
        response = { sessions: [{ id: "s1", name: "JSONL name" }] };
      }
      if (response) {
        queueMicrotask(() =>
          this.onmessage?.({
            data: JSON.stringify({
              type: "data_response",
              requestId: frame.requestId,
              ok: true,
              ...response,
            }),
          }),
        );
      }
    }
    if (frame.type === "host_request") {
      let response = null;
      if (frame.operation === "workspace.list") {
        response = { workspaces: [], removed: [] };
      } else if (frame.operation === "runtime_instances") {
        response = { instances: [] };
      } else if (frame.operation === "list_skill_inventory") {
        response = { skills: [] };
      }
      if (response) {
        queueMicrotask(() =>
          this.onmessage?.({
            data: JSON.stringify({
              type: frame.operation === "workspace_info" ? "data_response" : "host_response",
              requestId: frame.requestId,
              ok: true,
              response,
            }),
          }),
        );
      }
    }
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
  }
}

beforeEach(async () => {
  const fixture = new DOMParser().parseFromString(
    readFileSync(join(process.cwd(), "public/index.html"), "utf8"),
    "text/html",
  );
  document.documentElement.replaceChildren(...fixture.documentElement.childNodes);
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
    if (String(input) === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  vi.spyOn(console, "debug").mockImplementation(() => {});
  vi.spyOn(console, "log").mockImplementation(() => {});
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
  document.documentElement.replaceChildren();
  delete globalThis.WebSocket;
  delete globalThis.fetch;
  delete globalThis.requestAnimationFrame;
  delete globalThis.ResizeObserver;
});

test("workspace shell routes the Daemon lifecycle and injects the data-plane query", async () => {
  await import("./app.js?daemon-tab-routing");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;
  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  document.getElementById("settings-btn").click();
  expect(daemonHarness.page.activate).not.toHaveBeenCalled();
  document.querySelector('[data-settings-tab="daemon"]').click();
  expect(daemonHarness.page.activate).toHaveBeenCalledTimes(1);
  expect(
    document.querySelector('[data-settings-panel="daemon"]').classList.contains("active"),
  ).toBe(true);
  const result = await daemonHarness.options.loadWorkspaceSessions("other-workspace");
  expect(result.sessions).toEqual([{ id: "s1", name: "JSONL name" }]);
  expect(
    socket.sent.some(
      (frame) =>
        frame.type === "data_request" &&
        frame.operation === "workspace_sessions" &&
        frame.workspaceId === "other-workspace",
    ),
  ).toBe(true);
  daemonHarness.page.leave.mockClear();
  document.querySelector('[data-settings-tab="general"]').click();
  expect(daemonHarness.page.leave).toHaveBeenCalledTimes(1);
});

test("places the preview workspace below the shared header", () => {
  const workspace = document.querySelector(".workspace");
  const content = document.querySelector(".workspace-content");

  expect(workspace).not.toBeNull();
  expect(workspace).toContain(document.querySelector(".header"));
  expect(workspace).toContain(content);
  expect(content).toContain(document.querySelector(".main"));
  expect(content).toContain(document.getElementById("file-preview-resizer"));
  expect(content).toContain(document.getElementById("file-preview-panel"));
  expect(content).toContain(document.getElementById("file-sidebar"));
});

test("initializes the application without reporting existing i18n keys as missing", async () => {
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

  await import("./app.js?startup-regression");

  expect(warn).not.toHaveBeenCalledWith(expect.stringContaining("[i18n] missing key:"));

  document.getElementById("settings-btn").click();
  expect(document.getElementById("settings-panel").classList.contains("hidden")).toBe(false);
});

test("switches the shared right sidebar between Info, Files, and Git tabs", async () => {
  await import("./app.js?git-sidebar-tabs");

  const infoTab = document.getElementById("file-sidebar-info-tab");
  const filesTab = document.getElementById("file-sidebar-files-tab");
  const gitTab = document.getElementById("file-sidebar-git-tab");
  const infoPanel = document.getElementById("info-panel");
  const fileList = document.getElementById("file-list");
  const gitPanel = document.getElementById("git-panel");

  expect(infoTab).not.toBeNull();
  expect(filesTab).not.toBeNull();
  expect(gitTab).not.toBeNull();
  expect(infoPanel).not.toBeNull();
  expect(infoTab.compareDocumentPosition(filesTab) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(filesTab.compareDocumentPosition(gitTab) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(filesTab.getAttribute("aria-selected")).toBe("true");

  infoTab.click();
  expect(infoTab.getAttribute("aria-selected")).toBe("true");
  expect(infoPanel.classList.contains("hidden")).toBe(false);
  expect(filesTab.classList.contains("hidden")).toBe(false);

  gitTab.click();
  expect(gitTab.getAttribute("aria-selected")).toBe("true");
  expect(fileList.classList.contains("hidden")).toBe(true);
  expect(gitPanel.classList.contains("hidden")).toBe(false);
  expect(gitPanel.textContent).toContain("No Git status loaded");
  expect(infoPanel.classList.contains("hidden")).toBe(true);

  filesTab.click();
  expect(fileList.classList.contains("hidden")).toBe(false);
  expect(gitPanel.classList.contains("hidden")).toBe(true);
});

test("opens the file sidebar and switches tabs from header indicator pills", async () => {
  await import("./app.js?indicator-sidebar-tabs");

  const fileSidebar = document.getElementById("file-sidebar");
  const filesTab = document.getElementById("file-sidebar-files-tab");
  const gitTab = document.getElementById("file-sidebar-git-tab");
  const workspaceIndicator = document.getElementById("workspace-indicator");
  const gitIndicator = document.getElementById("git-branch-indicator");

  // Start from a known collapsed state so the pills must expand it.
  fileSidebar.classList.add("collapsed");

  workspaceIndicator.click();
  expect(fileSidebar.classList.contains("collapsed")).toBe(false);
  expect(filesTab.classList.contains("active")).toBe(true);
  expect(gitTab.classList.contains("active")).toBe(false);

  fileSidebar.classList.add("collapsed");
  gitIndicator.click();
  expect(fileSidebar.classList.contains("collapsed")).toBe(false);
  expect(filesTab.classList.contains("active")).toBe(false);
  expect(gitTab.classList.contains("active")).toBe(true);
});

test("header pills always open-and-focus: never collapse, and flash even when already open", async () => {
  await import("./app.js?indicator-always-open");

  const fileSidebar = document.getElementById("file-sidebar");
  const filesTab = document.getElementById("file-sidebar-files-tab");
  const gitTab = document.getElementById("file-sidebar-git-tab");
  const workspaceIndicator = document.getElementById("workspace-indicator");
  const gitIndicator = document.getElementById("git-branch-indicator");

  // Sidebar already expanded, already on the Files tab — clicking the path
  // pill must NOT toggle it closed, and must flash the Files tab.
  fileSidebar.classList.remove("collapsed");
  filesTab.classList.add("active");
  gitTab.classList.remove("active");

  workspaceIndicator.click();
  expect(fileSidebar.classList.contains("collapsed")).toBe(false);
  expect(filesTab.classList.contains("active")).toBe(true);
  expect(filesTab.classList.contains("flash-highlight")).toBe(true);

  // Same check for the Git pill when the sidebar is open on Files.
  gitIndicator.click();
  expect(fileSidebar.classList.contains("collapsed")).toBe(false);
  expect(gitTab.classList.contains("active")).toBe(true);
  expect(gitTab.classList.contains("flash-highlight")).toBe(true);
});

test("retries Git status when workspace generation arrives after opening Git", async () => {
  await import("./app.js?git-status-after-bootstrap");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  document.getElementById("file-sidebar-git-tab").click();
  expect(socket.sent).toHaveLength(0);

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });

  expect(socket.sent).toContainEqual({
    type: "host_request",
    protocolVersion: 2,
    requestId: "git-1",
    workspaceGeneration: 7,
    operation: "git_status",
    args: {},
  });
});

test("updates the git branch pill when a git_status frame arrives", async () => {
  await import("./app.js?git-pill-status");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "git_status",
      requestId: "git-1",
      workspaceGeneration: 7,
      snapshot: { snapshotId: "snap-1", branch: "main", entries: [] },
    }),
  });

  const pill = document.getElementById("git-branch-indicator");
  expect(pill.classList.contains("hidden")).toBe(false);
  expect(pill.textContent).toContain("main");
});

test("re-probes git status when the generation arrives after a mirror sync", async () => {
  await import("./app.js?git-probe-retry");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;
  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });

  // Foreground snapshot for a git workspace BEFORE owner_bootstrap: the
  // probe must be swallowed (generation unknown) but re-armed, not
  // permanently marked as probed.
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: { workspaceId: "workspace:/work/repo", sessionId: "session-1", instanceId: "i-1" },
      sequence: 1,
      state: { pi: { sessionFile: "/s/1.jsonl" }, messages: [], stats: {} },
    }),
  });
  await new Promise((resolve) => setTimeout(resolve, 50));
  const probesBefore = socket.sent.filter((frame) => frame.operation === "git_status");
  expect(probesBefore).toHaveLength(0);

  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  await new Promise((resolve) => setTimeout(resolve, 50));

  const probes = socket.sent.filter((frame) => frame.operation === "git_status");
  expect(probes.length).toBeGreaterThanOrEqual(1);
  expect(probes.at(-1)).toMatchObject({ workspaceGeneration: 7, args: {} });
});

test("surfaces terminal start failures and refreshes the terminal list", async () => {
  await import("./app.js?terminal-start-failure");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  document.querySelector("[data-terminal-toggle]").click();
  const createRequest = socket.sent.find((message) => message.payload?.type === "terminal_create");
  expect(createRequest).toBeDefined();
  const initialListCount = socket.sent.filter(
    (message) => message.payload?.type === "terminal_list",
  ).length;

  socket.onmessage({
    data: JSON.stringify({
      type: "terminal_command_failed",
      requestId: createRequest.requestId,
      error: "Git for Windows was not found.",
    }),
  });

  expect(document.querySelector("[data-terminal-start-error]")?.textContent).toBe(
    "Git for Windows was not found. Install it or choose another profile.",
  );
  expect(socket.sent.filter((message) => message.payload?.type === "terminal_list")).toHaveLength(
    initialListCount + 1,
  );
});

test("does not show an unclassified terminal command failure as a start error", async () => {
  await import("./app.js?terminal-unrelated-failure");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  const before = socket.sent.length;
  socket.onmessage({
    data: JSON.stringify({
      type: "terminal_command_failed",
      requestId: "unknown-request",
      error: "stale workspace",
    }),
  });

  expect(document.querySelector("[data-terminal-start-error]")).toBeNull();
  expect(socket.sent.length).toBe(before + 1);
  expect(socket.sent.at(-1).payload).toEqual({ type: "terminal_list" });
});

test("shows the not-a-git message only for the current status probe", async () => {
  await import("./app.js?git-not-a-repo");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  document.getElementById("file-sidebar-git-tab").click();
  expect(socket.sent).toHaveLength(0);

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  const panel = document.getElementById("git-panel");

  // A stale failure that is not the current status probe (superseded probe or
  // a concurrent diff/write) must not flip the panel into not-a-repository.
  socket.onmessage({
    data: JSON.stringify({
      type: "git_command_failed",
      requestId: "git-999",
      workspaceGeneration: 7,
      error: "fatal: not a git repository (or any of the parent directories): .git",
    }),
  });
  expect(panel.textContent).not.toContain("This workspace is not a Git repository");

  // The actual status probe fails because the workspace is not a Git
  // repository; HostServer answers with a git_command_failed frame instead of
  // git_status, and only that requestId flips the panel.
  socket.onmessage({
    data: JSON.stringify({
      type: "git_command_failed",
      requestId: "git-1",
      workspaceGeneration: 7,
      error: "fatal: not a git repository (or any of the parent directories): .git",
    }),
  });
  expect(panel.textContent).toContain("This workspace is not a Git repository");
  expect(panel.textContent).not.toContain("No Git status loaded");
});

test("hides the Git tab and returns to Files when the workspace is not a Git repository", async () => {
  await import("./app.js?git-tab-hidden-non-repo");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  const filesTab = document.getElementById("file-sidebar-files-tab");
  const gitTab = document.getElementById("file-sidebar-git-tab");
  expect(gitTab.classList.contains("hidden")).toBe(false);

  // Open the Git tab, then the status probe confirms the workspace is not a
  // Git repository.
  gitTab.click();
  expect(gitTab.getAttribute("aria-selected")).toBe("true");

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "git_command_failed",
      requestId: "git-1",
      workspaceGeneration: 7,
      error: "fatal: not a git repository (or any of the parent directories): .git",
    }),
  });

  // The Git entry disappears and the sidebar bounces back to the Files tab.
  expect(gitTab.classList.contains("hidden")).toBe(true);
  expect(filesTab.getAttribute("aria-selected")).toBe("true");
  expect(gitTab.getAttribute("aria-selected")).toBe("false");
});

test("restores the Git tab once a status probe proves the workspace is a Git repository", async () => {
  await import("./app.js?git-tab-shown-repo");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  const gitTab = document.getElementById("file-sidebar-git-tab");
  // Simulate a prior non-Git discovery that hid the tab.
  gitTab.classList.add("hidden");

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "git_status",
      workspaceGeneration: 7,
      snapshot: { entries: [], totalEntryCount: 0, returnedEntryCount: 0 },
    }),
  });

  expect(gitTab.classList.contains("hidden")).toBe(false);
});

test("proactively hides the Git tab via workspace-info on the first mirror sync", async () => {
  // The lazy click-probe is the fallback; the authoritative /api/workspace-info
  // probe must hide the tab on its own, without any user interaction.
  globalThis.fetch = vi.fn(async (input) => {
    const url = String(input);
    if (url.startsWith("/api/workspace-info")) {
      return new Response(JSON.stringify({ isGit: false }));
    }
    if (url === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
    }
    if (url === "/api/sessions") {
      // Resolve immediately so the deferred mirror sync replays without the
      // 2.5s load-retry backoff a 404 would trigger.
      return new Response(JSON.stringify({ projects: [] }));
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  await import("./app.js?git-tab-proactive-hide");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  const gitTab = document.getElementById("file-sidebar-git-tab");
  expect(gitTab.classList.contains("hidden")).toBe(false);

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  // Mirror snapshots carry the server's `workspace:<cwd>` routing id
  // (withRouteMeta); the Git-entry probe must re-query it verbatim.
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: { workspaceId: "workspace:/tmp/non-git", sessionId: "/sessions/demo.jsonl" },
      state: { pi: { entries: [], sessionFile: "/sessions/demo.jsonl" } },
    }),
  });

  await vi.waitFor(() => expect(gitTab.classList.contains("hidden")).toBe(true));
  expect(
    socket.sent.some(
      (frame) => frame.type === "data_request" && frame.operation === "workspace_info",
    ),
  ).toBe(true);
});

test("re-hides the Git tab when switching to a non-Git workspace while the panel is open", async () => {
  globalThis.fetch = vi.fn(async (input) => {
    const url = String(input);
    if (url.startsWith("/api/workspace-info")) {
      // /work/git-a is a repo; /work/nongit-b is not.
      return new Response(JSON.stringify({ isGit: !url.includes("nongit-b") }));
    }
    if (url === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
    }
    if (url === "/api/sessions") {
      return new Response(JSON.stringify({ projects: [] }));
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  await import("./app.js?git-tab-switch-open");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  const gitTab = document.getElementById("file-sidebar-git-tab");
  const filesTab = document.getElementById("file-sidebar-files-tab");

  // Enter a Git workspace and open the Git tab.
  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 7, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: { workspaceId: "workspace:/work/git-a", sessionId: "/sessions/a.jsonl" },
      state: { pi: { entries: [], sessionFile: "/sessions/a.jsonl" } },
    }),
  });
  await vi.waitFor(() => expect(gitTab.classList.contains("hidden")).toBe(false));
  gitTab.click();
  await vi.waitFor(() => expect(gitTab.getAttribute("aria-selected")).toBe("true"));

  // Switch to the non-Git workspace; the Git tab must hide on its own.
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 8, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: { workspaceId: "workspace:/work/nongit-b", sessionId: "/sessions/b.jsonl" },
      state: { pi: { entries: [], sessionFile: "/sessions/b.jsonl" } },
    }),
  });

  await vi.waitFor(() => expect(gitTab.classList.contains("hidden")).toBe(true));
  expect(filesTab.getAttribute("aria-selected")).toBe("true");
});

test("hides the Git tab on entry to a non-Git workspace via the entry status probe even when workspace-info is unavailable", async () => {
  // Real-world failure mode: the /api/workspace-info fast path 404s, leaving
  // only the git status probe to drive visibility. The probe must fire on
  // workspace entry even when the Git panel is CLOSED, so a non-Git workspace
  // hides its tab without requiring a click.
  globalThis.fetch = vi.fn(async (input) => {
    const url = String(input);
    if (url.startsWith("/api/workspace-info")) {
      return new Response(JSON.stringify({ error: "Unknown workspace" }), { status: 404 });
    }
    if (url === "/locales/en.json") {
      return new Response(JSON.stringify(enMessages));
    }
    if (url === "/api/sessions") {
      return new Response(JSON.stringify({ projects: [] }));
    }
    return new Response(JSON.stringify({}), { status: 404 });
  });
  await import("./app.js?git-tab-entry-probe-closed");
  const socket = FakeWebSocket.instances.at(-1);
  socket.readyState = FakeWebSocket.OPEN;

  const gitTab = document.getElementById("file-sidebar-git-tab");
  expect(gitTab.classList.contains("hidden")).toBe(false);

  socket.onmessage({ data: JSON.stringify({ type: "hello_ack", protocolVersion: 2 }) });
  socket.onmessage({
    data: JSON.stringify({ type: "owner_bootstrap", workspaceGeneration: 9, instances: [] }),
  });
  socket.onmessage({
    data: JSON.stringify({
      type: "runtime_snapshot",
      target: { workspaceId: "workspace:/work/nongit-entry", sessionId: "/sessions/non-git.jsonl" },
      state: { pi: { entries: [], sessionFile: "/sessions/non-git.jsonl" } },
    }),
  });

  // The entry probe must be sent even though the Git panel is closed.
  await vi.waitFor(() => {
    const probe = socket.sent.find(
      (frame) => frame.type === "host_request" && frame.operation === "git_status",
    );
    expect(probe).toBeTruthy();
    return probe;
  });
  const probe = socket.sent.find(
    (frame) => frame.type === "host_request" && frame.operation === "git_status",
  );

  // The non-Git status probe fails, which must hide the Git tab on its own.
  socket.onmessage({
    data: JSON.stringify({
      type: "git_command_failed",
      requestId: probe.requestId,
      workspaceGeneration: 9,
      error: "fatal: not a git repository (or any of the parent directories): .git",
    }),
  });
  await vi.waitFor(() => expect(gitTab.classList.contains("hidden")).toBe(true));
});

test("persists the selected sidebar tab and restores it on reload", async () => {
  // Simulate a session where the user picked the Git tab, then reloads.
  const storage = new Map([
    ["pi-studio-file-sidebar", "open"],
    ["pi-studio-file-sidebar-tab", "git"],
  ]);
  vi.stubGlobal("localStorage", {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => storage.set(key, String(value)),
    removeItem: (key) => storage.delete(key),
  });

  await import("./app.js?git-tab-persistence");

  const gitTab = document.getElementById("file-sidebar-git-tab");
  const gitPanel = document.getElementById("git-panel");
  const fileList = document.getElementById("file-list");

  // The stored Git tab must be restored on startup.
  expect(gitTab.getAttribute("aria-selected")).toBe("true");
  expect(gitPanel.classList.contains("hidden")).toBe(false);
  expect(fileList.classList.contains("hidden")).toBe(true);

  // Switching to Files must persist that choice.
  document.getElementById("file-sidebar-files-tab").click();
  expect(storage.get("pi-studio-file-sidebar-tab")).toBe("files");
});

test("a deep session link with no runtime still boots the page (PM C10①)", async () => {
  // A browser (no desktop capability) opening a canonical session URL gets the
  // shell from the host, but `/v2/bootstrap` answers 401/404. The page must
  // still render — an unhandled rejection here leaves a blank tab.
  window.history.replaceState({}, "", "/workspaces/ws-uuid-1/sessions/session-a");
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  try {
    await expect(import("./app.js?deep-link-degraded")).resolves.toBeDefined();
    expect(document.getElementById("settings-btn")).not.toBeNull();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("rendering without one"),
      expect.anything(),
    );
  } finally {
    window.history.replaceState({}, "", "/");
  }
});

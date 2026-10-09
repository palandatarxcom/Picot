// ABOUTME: Lists sessions grouped by project and handles session switching.
// ABOUTME: Coordinates pinned workspaces and live session state for the sidebar.

/**
 * Session Sidebar - Lists sessions grouped by project, handles switching
 */

import { onLocaleChange, t } from "../i18n.js";
import { createIcon } from "../icons.js";
import { buildSidebarSection, buildSidebarWorkspaceGroup } from "../sidebar-workspace-group.js";
import { cacheSidebarProjects } from "../workspace/nav-state-cache.js";
import { basenameLocalPath } from "../workspace/path-utils.js";
import {
  mergeRegistryWorkspaces,
  registryPinsFromProjects,
  resolvePinnedWorkspaceGroups,
  workspacePathKey,
} from "../workspace-projects.js";
import {
  applySessionItemActionVisibility,
  buildSessionItem as buildSessionItemNode,
  formatSessionTime,
  getSessionDisplayTitle,
} from "./build-session-item.js";
import { buildFlattenedSessionTree } from "./session-tree-model.js";

function readJsonArray(key) {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "[]");
    return Array.isArray(value) ? value : [];
  } catch {
    return [];
  }
}

function createActionIcon(kind, options = {}) {
  const size = typeof options === "number" ? options : options.size || 16;
  const iconName = kind === "folder" ? "folder" : kind;
  return createIcon(iconName, { size });
}

function appendHighlightedText(container, text, query) {
  const source = String(text || "");
  if (!query) {
    container.textContent = source;
    return;
  }
  const expression = new RegExp(query.replace(/[.*+?^${}()|[\\]\\]/g, "\\$&"), "gi");
  let cursor = 0;
  for (const match of source.matchAll(expression)) {
    if (match.index > cursor)
      container.appendChild(document.createTextNode(source.slice(cursor, match.index)));
    const mark = document.createElement("mark");
    mark.textContent = match[0];
    container.appendChild(mark);
    cursor = match.index + match[0].length;
  }
  if (cursor < source.length) container.appendChild(document.createTextNode(source.slice(cursor)));
}

export class SessionSidebar {
  constructor(container, onSessionSelect, onNewChat, options = {}) {
    this.projectSessionInitialLimit = 5;
    this.projectSessionStep = 10;
    this.container = container;
    // Provisional-row retirement happens at the app-level selection choke
    // point (app.js handleSessionSelect) so the Focus sidebar — which wires
    // its own onSessionSelect — shares the same behavior.
    this.onSessionSelect = onSessionSelect;
    this.onNewChat = onNewChat;
    this.onOpenProject = options.onOpenProject || null;
    this.onRegisterWorkspace = options.onRegisterWorkspace || null;
    this.onSessionNotice = options.onSessionNotice || null;
    this.onSessionDeleted = options.onSessionDeleted || null;
    this.getLiveInstances = options.getLiveInstances || null;
    this.getFocusWorkspacePath = options.getFocusWorkspacePath || null;
    this.isFocusActive = options.isFocusActive || null;
    this.onFocusRefresh = options.onFocusRefresh || null;
    this.onWorkspaceFocus = options.onWorkspaceFocus || null;
    // Explicit focus-enable seam. Landing passes `() => source === "registry"`
    // to enable Focus for every registered row (there is no active session or
    // current workspace); callers that omit it keep the classic
    // active-session-or-current-workspace gating.
    this.canFocusWorkspace = options.canFocusWorkspace || null;
    this.isCurrentWorkspace = options.isCurrentWorkspace || null;
    this.activeSessionFile = null;
    this.projects = [];
    // Instance-level fold state. Workspaces default to collapsed (empty set =
    // none expanded); only IDs added here render expanded. Kept in memory only,
    // so a fresh app start always begins with every workspace collapsed while
    // subsequent reloads (refresh button, + new session, workspace pin/unpin) preserve
    // the user's current expand/collapse choices.
    this.expandedWorkspaces = new Set();
    this.pinnedCollapsed = false;
    this.projectsCollapsed = false;
    this.searchQuery = "";
    this.unread = new Set(readJsonArray("pi-studio-unread"));
    this.transport = options.transport || null;
    // Per-workspace lazy session cache keyed by canonical path; invalidated
    // by local session mutations and refreshed per workspace on demand.
    this.workspaceSessionsCache = new Map();
    this._registryBusyRows = new Set();
    this._registryBusyPromises = new Map();
    this._registryCountBusyRows = new Set();
    this._registryWarmupScheduled = false;
    this._registrySessionLoadGeneration = 0;
    this._refreshPromise = null;
    this._refreshQueue = Promise.resolve();
    this._queuedRefreshes = new Map();
    this._refreshRegistryAvailable = null;
    this._refreshScopeKey = null;
    // A fresh native runtime exists before Pi persists its first JSONL. Keep
    // one read-only row so its owning workspace and active chat stay visible.
    this.provisionalSession = null;
    // Keyed-DOM row caches so routine refreshes reuse unchanged nodes
    // (hover/scroll/overlays survive) instead of rebuilding the whole tree.
    this._projectRowCache = new Map();
    this._pinnedSectionCache = null;
    this._registryPins = null;
    this.statusItemsByPath = new Map();
    this.streamingFiles = new Set();
    // Last runtimeInstances() result; feeds isLiveSession between refreshes.
    this.liveInstancesSnapshot = [];
    this.projectVisibleSessionCounts = new Map();
    this.contextMenu = null;
    // `loadSeq` counts issued loads; `loadCommitted` is the highest seq that has
    // actually rendered. We discard a response only when a *newer* one has
    // already committed (out-of-order arrival), never just because a newer load
    // was issued — an in-flight later load must not starve an earlier fetch that
    // already returned fresh data (e.g. the first response that observes a
    // brand-new session's just-written .jsonl).
    this.loadSeq = 0;
    this.loadCommitted = 0;
    this.loadInvalidatedThrough = 0;

    // Close context menu on click anywhere
    document.addEventListener("click", () => {
      this.closeContextMenu();
    });
    document.addEventListener("contextmenu", (e) => {
      if (!e.target.closest(".workspace-header, .sidebar-context-menu")) this.closeContextMenu();
    });

    this.unsubscribeLocaleChange = onLocaleChange(() => {
      if (!this.container || this.container.children.length === 0) return;
      if (this.loadSeq > this.loadCommitted) return; // load in-flight
      const savedScroll = this.container.scrollTop;
      this.render();
      this.container.scrollTop = savedScroll;
    });
  }

  saveUnread() {
    localStorage.setItem("pi-studio-unread", JSON.stringify(Array.from(this.unread)));
  }

  isUnread(filePath) {
    return this.unread.has(filePath);
  }

  isStreaming(filePath) {
    return this.streamingFiles.has(filePath);
  }

  markUnread(filePath) {
    if (!filePath) return;
    if (filePath === this.activeSessionFile) return;
    if (this.unread.has(filePath)) return;
    this.unread.add(filePath);
    this.saveUnread();
    this.applyStatusToItem(filePath);
  }

  markRead(filePath) {
    if (!filePath) return;
    if (!this.unread.has(filePath)) return;
    this.unread.delete(filePath);
    this.saveUnread();
    this.applyStatusToItem(filePath);
  }

  setStreaming(filePath, streaming) {
    if (!filePath) return;
    const had = this.streamingFiles.has(filePath);
    if (streaming && !had) {
      this.streamingFiles.add(filePath);
    } else if (!streaming && had) {
      this.streamingFiles.delete(filePath);
    } else {
      return;
    }
    this.applyStatusToItem(filePath);
  }

  clearStreaming() {
    if (this.streamingFiles.size === 0) return;
    const files = Array.from(this.streamingFiles);
    this.streamingFiles.clear();
    files.forEach((f) => {
      this.applyStatusToItem(f);
    });
  }

  rebuildStatusIndex() {
    this.statusItemsByPath = new Map();
    for (const item of this.container.querySelectorAll(".session-item[data-file-path]")) {
      const filePath = item.dataset.filePath;
      if (!filePath) continue;
      const items = this.statusItemsByPath.get(filePath) || new Set();
      items.add(item);
      this.statusItemsByPath.set(filePath, items);
    }
  }

  applyStatusToItem(filePath) {
    const items = this.statusItemsByPath?.get(filePath) || [];
    items.forEach((el) => {
      if (!el.isConnected) return;
      el.classList.toggle("unread", this.unread.has(filePath));
      el.classList.toggle("streaming", this.streamingFiles.has(filePath));
      el.classList.toggle("mirror-live", this.streamingFiles.has(filePath));
    });
    // Streaming/live flips re-evaluate the action gate on the rendered row,
    // so a session that just started working loses its buttons in place.
    const renameBlocked = Boolean(this.renameBlockedReason(filePath));
    const deleteBlocked = Boolean(this.deletionBlockedReason(filePath));
    items.forEach((el) => {
      if (!el.isConnected) return;
      applySessionItemActionVisibility(el, { renameBlocked, deleteBlocked });
    });
  }

  async deleteSession(filePath) {
    if (!filePath) return false;
    const blocked = this.deletionBlockedReason(filePath);
    if (blocked) {
      this.onSessionNotice?.(blocked);
      return false;
    }
    const ok = await this.confirmSessionDeletion(1);
    if (!ok) return false;

    let deleted = false;
    try {
      if (typeof this.transport?.sessionDeleteBatch !== "function") {
        throw new Error("Host session delete unavailable");
      }
      const data = await this.transport.sessionDeleteBatch([filePath]);
      const running = new Set(data.running || []);
      const errors = new Set(data.errors || []);
      if (running.has(filePath)) {
        this.onSessionNotice?.(t("sidebar.deleteSessionRunning"));
        return false;
      }
      deleted = !errors.has(filePath);
    } catch (err) {
      console.error("[Sidebar] deleteSession failed:", err);
      this.onSessionNotice?.(t("sidebar.deleteSessionFailed"));
      return false;
    }
    if (!deleted) {
      this.onSessionNotice?.(t("sidebar.deleteSessionFailed"));
      return false;
    }
    // The owning registry row may serve its session list from the lazy cache;
    // drop that entry so the reload below cannot resurrect the deleted file.
    const owner = this.projects.find(
      (project) =>
        Array.isArray(project.sessions) &&
        project.sessions.some((session) => session?.filePath === filePath),
    );
    if (owner?.source === "registry") this.invalidateWorkspaceSessions(owner.path);
    this.onSessionDeleted?.(filePath);
    await this.refresh();
    return true;
  }

  async confirmSessionDeletion(count) {
    const message =
      count === 1
        ? t("sidebar.deleteSessionConfirmOne", { count })
        : t("sidebar.deleteSessionConfirmMany", { count });
    return this.showFallbackConfirmDialog(message);
  }

  confirmWorkspaceDeletion(workspaceName, count) {
    return new Promise((resolve) => {
      const overlay = document.createElement("div");
      overlay.className = "sidebar-confirm-overlay";
      const dialog = document.createElement("div");
      dialog.className = "sidebar-confirm-dialog workspace-delete-confirm-dialog";
      dialog.setAttribute("role", "dialog");
      dialog.setAttribute("aria-modal", "true");
      dialog.setAttribute("aria-label", t("sidebar.deleteWorkspaceMainSessions"));

      const message = document.createElement("div");
      message.className = "sidebar-confirm-message";
      message.textContent = t("sidebar.deleteWorkspaceMainSessionsConfirm", { count });

      const prompt = document.createElement("div");
      prompt.className = "workspace-delete-confirm-prompt";
      prompt.textContent = t("sidebar.deleteWorkspaceNamePrompt");

      const expected = document.createElement("code");
      expected.className = "workspace-delete-confirm-name";
      expected.textContent = workspaceName;

      const label = document.createElement("label");
      label.className = "workspace-delete-confirm-label";
      label.textContent = t("sidebar.deleteWorkspaceNameLabel");
      const input = document.createElement("input");
      input.type = "text";
      input.className = "workspace-delete-confirm-input";
      input.autocomplete = "off";
      input.setAttribute("aria-label", t("sidebar.deleteWorkspaceNameLabel"));
      label.appendChild(input);

      const warning = document.createElement("div");
      warning.className = "workspace-delete-warning";
      warning.hidden = true;
      warning.textContent = t("sidebar.deleteWorkspaceNameWarning");

      const actions = document.createElement("div");
      actions.className = "sidebar-confirm-actions";
      const cancel = document.createElement("button");
      cancel.type = "button";
      cancel.className = "sidebar-confirm-no";
      cancel.textContent = t("actions.cancel");
      const confirm = document.createElement("button");
      confirm.type = "button";
      confirm.className = "sidebar-confirm-yes";
      confirm.textContent = t("actions.delete");
      actions.append(cancel, confirm);
      dialog.append(message, prompt, expected, label, warning, actions);
      overlay.appendChild(dialog);

      const cleanup = (result) => {
        document.removeEventListener("keydown", onKeyDown);
        overlay.remove();
        resolve(result);
      };
      const onKeyDown = (event) => {
        if (event.key === "Escape") cleanup(false);
      };
      const updateWarning = () => {
        warning.hidden = input.value.length === 0 || input.value === workspaceName;
      };
      input.addEventListener("input", updateWarning);
      cancel.addEventListener("click", () => cleanup(false));
      confirm.addEventListener("click", () => {
        if (input.value !== workspaceName) {
          warning.hidden = false;
          input.focus();
          return;
        }
        cleanup(true);
      });
      overlay.addEventListener("click", (event) => {
        if (event.target === overlay) cleanup(false);
      });
      document.addEventListener("keydown", onKeyDown);
      document.body.appendChild(overlay);
      input.focus();
    });
  }

  showFallbackConfirmDialog(message) {
    return new Promise((resolve) => {
      const overlay = document.createElement("div");
      overlay.className = "sidebar-confirm-overlay";
      const dialog = document.createElement("div");
      dialog.className = "sidebar-confirm-dialog";
      dialog.setAttribute("role", "dialog");
      dialog.setAttribute("aria-modal", "true");
      dialog.setAttribute("aria-label", t("sidebar.deleteSessionAriaLabel"));
      const messageElement = document.createElement("div");
      messageElement.className = "sidebar-confirm-message";
      messageElement.textContent = message;
      const actions = document.createElement("div");
      actions.className = "sidebar-confirm-actions";
      const cancel = document.createElement("button");
      cancel.type = "button";
      cancel.className = "sidebar-confirm-no";
      cancel.textContent = t("actions.cancel");
      const confirm = document.createElement("button");
      confirm.type = "button";
      confirm.className = "sidebar-confirm-yes";
      confirm.textContent = t("actions.delete");
      actions.append(cancel, confirm);
      dialog.append(messageElement, actions);
      overlay.appendChild(dialog);

      const cleanup = (result) => {
        document.removeEventListener("keydown", onKeyDown);
        overlay.remove();
        resolve(result);
      };

      const onKeyDown = (event) => {
        if (event.key === "Escape") cleanup(false);
      };

      overlay.addEventListener("click", (event) => {
        if (event.target === overlay) cleanup(false);
      });

      overlay.querySelector(".sidebar-confirm-no").addEventListener("click", () => cleanup(false));
      overlay.querySelector(".sidebar-confirm-yes").addEventListener("click", () => cleanup(true));

      document.addEventListener("keydown", onKeyDown);
      document.body.appendChild(overlay);
    });
  }

  isRegistryAvailable() {
    return Boolean(
      this.transport?.available &&
        this.transport.capabilities?.native &&
        typeof this.transport.listWorkspaces === "function",
    );
  }

  async loadRegistryProjects(seq) {
    const [listResult, instances] = await Promise.all([
      this.transport.listWorkspaces(),
      this.fetchLiveInstances(),
    ]);
    if (seq <= this.loadInvalidatedThrough || seq < this.loadCommitted) return this.projects;
    const rows = Array.isArray(listResult?.workspaces) ? listResult.workspaces : [];
    const removedRows = Array.isArray(listResult?.removed) ? listResult.removed : [];
    const merged = mergeRegistryWorkspaces(rows, instances, this.projects);

    for (const reconciliation of merged.reconciliations) {
      if (this.expandedWorkspaces.delete(reconciliation.fromId)) {
        this.expandedWorkspaces.add(reconciliation.toId);
      }
    }
    // Lazy cache survives refreshes; attach cached sessions immediately so
    // expanded rows do not flash empty while the network round-trip runs.
    for (const project of merged.projects) {
      if (project.source !== "registry") continue;
      const cached = this.workspaceSessionsCache.get(workspacePathKey(project.path));
      if (cached) project.sessions = cached;
    }

    this._registryPins = registryPinsFromProjects(merged.projects);
    this.loadCommitted = seq;
    this.projects = merged.projects;
    this.applyProvisionalSession();

    if (removedRows.length > 0) {
      this.onSessionNotice?.(t("sidebar.workspaceMissingRemoved"));
    }

    this.render();
    try {
      cacheSidebarProjects(this.projects);
    } catch {
      /* caching is best-effort */
    }

    // Refresh sessions for every currently expanded registry row.
    for (const project of this.projects) {
      if (
        project.source === "registry" &&
        this.isWorkspaceExpanded(project) &&
        !this._registryBusyRows.has(project.workspaceId) &&
        !this._registryCountBusyRows.has(project.workspaceId) &&
        !this.workspaceSessionsCache.has(workspacePathKey(project.path))
      ) {
        void this.ensureWorkspaceSessions(project);
      }
    }
    // Counts are one-shot per page; re-running after every registry refresh
    // caused an N+1 burst and made the sidebar appear to refresh forever.
    this.scheduleRegistryWarmup();
    return this.projects;
  }

  /**
   * Idle-time warmup for the most recently used registry rows so the first
   * expansion rarely waits on a cold cache. Strictly best-effort: failures
   * and busy-row contention are ignored.
   */
  scheduleRegistryWarmup() {
    if (this._registryWarmupScheduled) return;
    this._registryWarmupScheduled = true;
    // One-shot per page: readdir-only counts so every row shows its real
    // badge immediately. Full session lists stay lazy — they load when the
    // workspace expands (setWorkspaceExpanded), never at startup.
    const warm = async () => {
      const registryRows = this.projects.filter((project) => project.source === "registry");
      await Promise.allSettled(
        registryRows.map((project) => this.ensureWorkspaceSessions(project, { countOnly: true })),
      );
      this.applyProvisionalSession();
      this.render();
    };
    if (typeof window !== "undefined" && typeof window.requestIdleCallback === "function") {
      window.requestIdleCallback(() => warm(), { timeout: 800 });
    } else {
      setTimeout(() => warm(), 800);
    }
  }

  /**
   * Lazily load one registry workspace's sessions on first expand.
   * Cache key is the canonical path; invalidation clears exactly that path.
   */
  async ensureWorkspaceSessions(project, { force = false, countOnly = false } = {}) {
    if (project?.source !== "registry") return;
    const loadGeneration = this._registrySessionLoadGeneration;
    const cacheKey = workspacePathKey(project.path);
    if (!cacheKey) return;
    // Host-bound identity is the raw DB uuid (registryId). The merged display
    // id carries a `ws:` prefix for UI identity; sending it to the host makes
    // workspace_root fail with workspace_not_found and the row stays empty.
    const hostWorkspaceId = project.registryId || project.workspaceId;
    // Count mode: readdir-only header badge refresh; full mode: lazy history.
    if (countOnly) {
      if (typeof project.sessionCount === "number" && !force) return;
      if (this._registryCountBusyRows.has(project.workspaceId)) return;
      this._registryCountBusyRows.add(project.workspaceId);
      try {
        if (typeof this.transport?.workspaceSessions !== "function") {
          throw new Error("Host workspace sessions unavailable");
        }
        const data = await this.transport.workspaceSessions(hostWorkspaceId, {
          countOnly: true,
        });
        if (loadGeneration !== this._registrySessionLoadGeneration) return;
        project.sessionCount = typeof data?.sessionCount === "number" ? data.sessionCount : null;
        project.hiddenSubagentCount =
          typeof data?.hiddenSubagentCount === "number" ? data.hiddenSubagentCount : null;
      } catch (error) {
        console.error("[Sidebar] workspace count refresh failed:", error);
      } finally {
        this._registryCountBusyRows.delete(project.workspaceId);
      }
      return;
    }
    const inFlight = this._registryBusyPromises.get(project.workspaceId);
    if (inFlight) {
      if (!force) return;
      await inFlight;
    }
    if (!force && this.workspaceSessionsCache.has(cacheKey)) {
      project.sessions = this.workspaceSessionsCache.get(cacheKey);
      this.render();
      return;
    }
    this._registryBusyRows.add(project.workspaceId);
    const loadPromise = (async () => {
      try {
        if (typeof this.transport?.workspaceSessions !== "function") {
          throw new Error("Host workspace sessions unavailable");
        }
        const data = await this.transport.workspaceSessions(hostWorkspaceId);
        if (loadGeneration !== this._registrySessionLoadGeneration) return;
        const sessions = Array.isArray(data?.sessions) ? data.sessions : [];
        this.workspaceSessionsCache.set(cacheKey, sessions);
        project.dirName = data?.dirName ?? null;
        project.sessionCount = typeof data?.sessionCount === "number" ? data.sessionCount : null;
        project.hiddenSubagentCount =
          typeof data?.hiddenSubagentCount === "number" ? data.hiddenSubagentCount : null;
        project.sessions = sessions;
        this.applyProvisionalSession();
        this.render();
      } catch (error) {
        console.error("[Sidebar] workspace session load failed:", error);
      } finally {
        this._registryBusyRows.delete(project.workspaceId);
        this._registryBusyPromises.delete(project.workspaceId);
      }
    })();
    this._registryBusyPromises.set(project.workspaceId, loadPromise);
    await loadPromise;
  }

  /** Drop the cached history of one workspace; next expand refetches it. */
  invalidateWorkspaceSessions(path) {
    const key = workspacePathKey(path);
    if (key) this.workspaceSessionsCache.delete(key);
    for (const project of this.projects) {
      if (project.source === "registry" && workspacePathKey(project.path) === key) {
        project.sessions = [];
      }
    }
  }

  /**
   * Single sidebar data-refresh entry. Startup, the refresh button,
   * registry-change broadcasts, sidebar mutations, and new-session
   * persistence all use this path. It reloads registry rows, invalidates
   * expanded-row caches, preserves keyed row/fold state, and re-warms counts.
   * Selecting an existing session is not a data refresh: it only changes the
   * active row and loads chat history in the main surface.
   */
  refresh(options = {}) {
    const targetKey = workspacePathKey(options.workspacePath);
    const registryAvailable = this.isRegistryAvailable();
    if (
      this._refreshPromise &&
      this._refreshRegistryAvailable === registryAvailable &&
      this._refreshScopeKey === targetKey
    ) {
      return this._refreshPromise;
    }
    const queued = this._queuedRefreshes.get(targetKey);
    if (queued) return queued;
    const refreshPromise = this._refreshQueue.then(() => {
      this._queuedRefreshes.delete(targetKey);
      return this._startRefresh(options);
    });
    this._queuedRefreshes.set(targetKey, refreshPromise);
    this._refreshQueue = refreshPromise.catch(() => {});
    return refreshPromise;
  }

  _startRefresh({ workspacePath = null } = {}) {
    const targetKey = workspacePathKey(workspacePath);
    const registryAvailable = this.isRegistryAvailable();
    // Coalesce before changing the request generation. Otherwise a second
    // click invalidates the first refresh's in-flight list response, then
    // returns its promise; expanded rows stay visibly empty until retry.
    if (
      this._refreshPromise &&
      this._refreshRegistryAvailable === registryAvailable &&
      this._refreshScopeKey === targetKey
    ) {
      return this._refreshPromise;
    }
    this._registrySessionLoadGeneration += 1;
    const focusWorkspacePath = workspacePathKey(this.getFocusWorkspacePath?.());
    const shouldRefreshProject = (project) =>
      this.isWorkspaceExpanded(project) ||
      (focusWorkspacePath && workspacePathKey(project.path) === focusWorkspacePath);
    this._registryBusyRows.clear();
    this._registryCountBusyRows.clear();
    for (const project of this.projects) {
      if (
        project.source === "registry" &&
        shouldRefreshProject(project) &&
        (!targetKey || workspacePathKey(project.path) === targetKey)
      ) {
        this.workspaceSessionsCache.delete(workspacePathKey(project.path));
      }
    }
    // A newly persisted session can belong to a collapsed row. Invalidate its
    // cache too; the next expansion must not serve the pre-persistence list.
    if (targetKey) this.invalidateWorkspaceSessions(workspacePath);
    this._registryWarmupScheduled = false;
    const refreshPromise = this.loadSessions()
      .then(async (projects) => {
        const targets = (projects || []).filter(
          (project) =>
            project.source === "registry" &&
            shouldRefreshProject(project) &&
            (!targetKey || workspacePathKey(project.path) === targetKey),
        );
        await Promise.all(targets.map((project) => this.ensureWorkspaceSessions(project)));
        return projects;
      })
      .finally(() => {
        if (this._refreshPromise === refreshPromise) {
          this._refreshPromise = null;
          this._refreshRegistryAvailable = null;
          this._refreshScopeKey = null;
        }
      });
    this._refreshPromise = refreshPromise;
    this._refreshRegistryAvailable = registryAvailable;
    this._refreshScopeKey = targetKey;
    return refreshPromise;
  }

  setProvisionalSession({ workspaceId, sessionId, sessionFile = null } = {}) {
    if (typeof workspaceId !== "string" || !workspaceId) return;
    const filePath = typeof sessionFile === "string" && sessionFile ? sessionFile : sessionId;
    if (typeof filePath !== "string" || !filePath) return;
    this.provisionalSession = {
      workspaceId,
      sessionId,
      filePath,
      name: t("sidebar.newSession"),
      timestamp: new Date().toISOString(),
      mtime: Date.now(),
      provisional: true,
    };
    this.applyProvisionalSession();
    this.render();
  }

  applyProvisionalSession() {
    const pending = this.provisionalSession;
    if (!pending) return;
    const project = this.projects.find(
      (candidate) =>
        candidate?.registryId === pending.workspaceId ||
        candidate?.workspaceId === pending.workspaceId,
    );
    if (!project) return;
    const sessions = Array.isArray(project.sessions) ? project.sessions : [];
    if (
      sessions.some(
        (session) =>
          !session?.provisional &&
          (session?.filePath === pending.filePath || session?.id === pending.sessionId),
      )
    ) {
      this.provisionalSession = null;
      return;
    }
    project.sessions = [pending, ...sessions.filter((session) => !session?.provisional)];
    if (typeof project.sessionCount === "number") {
      project.sessionCount = Math.max(project.sessionCount, project.sessions.length);
    }
  }

  /** Drop the provisional row and its pending state: the unpersisted
    this.provisionalSession = null;
  /** Rebind the provisional row to the runtime's persisted JSONL path.
   * The route session id and the scanned row never compare equal (runtime
   * id vs absolute file path), so without this every later refresh
   * re-inserts the placeholder next to the persisted row. Call once the
   * mirror learns the real file; if the scanned list already has it, the
   * placeholder converges away on the spot. */
  rebindProvisionalSession(filePath) {
    const pending = this.provisionalSession;
    if (!pending || typeof filePath !== "string" || !filePath || pending.filePath === filePath) {
      return;
    }
    for (const project of this.projects) {
      if (!Array.isArray(project?.sessions)) continue;
      project.sessions = project.sessions.filter((session) => session !== pending);
    }
    pending.filePath = filePath;
    this.applyProvisionalSession();
    this.render();
  }

  retireProvisionalSession() {
    const pending = this.provisionalSession;
    if (!pending) return;
    this.provisionalSession = null;
    for (const project of this.projects) {
      if (!Array.isArray(project?.sessions)) continue;
      project.sessions = project.sessions.filter(
        (session) => session?.filePath !== pending.filePath,
      );
    }
  }

  async fetchLiveInstances() {
    try {
      const data = await this.transport?.runtimeInstances?.();
      const instances = Array.isArray(data?.instances) ? data.instances : [];
      this.liveInstancesSnapshot = instances;
      return instances;
    } catch {
      return [];
    }
  }

  /**
   * Load (or reload) the session list. On native desktops the source is the
   * DB registry via broker `workspace.list`. Browser/LAN surfaces fall back
   * to live-instance visibility only; history browsing is desktop-native.
   */
  async loadSessions({ retries = 4, retryDelayMs = 250, quiet = false } = {}) {
    void retries;
    void retryDelayMs;
    const seq = ++this.loadSeq;
    try {
      if (this.isRegistryAvailable()) {
        return await this.loadRegistryProjects(seq);
      }
    } catch (error) {
      console.error("[Sidebar] registry load failed:", error);
    }
    // Browser/LAN surface: no broker-native access. Live windows stay visible;
    // history browsing requires the desktop registry by design.
    return this.loadLiveOnlySessions(seq, { quiet });
  }

  async loadLiveOnlySessions(seq, { quiet = false } = {}) {
    if (!quiet) this.renderSkeleton();
    const instances = await this.fetchLiveInstances();
    if (seq <= this.loadInvalidatedThrough || seq < this.loadCommitted) return this.projects;
    const merged = mergeRegistryWorkspaces([], instances, this.projects);
    for (const reconciliation of merged.reconciliations) {
      if (this.expandedWorkspaces.delete(reconciliation.fromId)) {
        this.expandedWorkspaces.add(reconciliation.toId);
      }
    }
    this._registryPins = null;
    this.loadCommitted = seq;
    this.projects = merged.projects;
    this.render();
    return this.projects;
  }

  renderSkeleton() {
    this.container.replaceChildren();
    for (let index = 0; index < 6; index += 1) {
      const skeleton = document.createElement("div");
      skeleton.className = "session-skeleton";
      const title = document.createElement("div");
      title.className = "session-skeleton-title";
      const meta = document.createElement("div");
      title.textContent = "";
      skeleton.append(title, meta);
      this.container.appendChild(skeleton);
    }
  }

  invalidateSessionLoads() {
    this.loadInvalidatedThrough = Math.max(this.loadInvalidatedThrough, this.loadSeq);
  }

  setSearchQuery(query) {
    this.searchQuery = query.toLowerCase().trim();

    // Clear pending full-text search
    if (this._searchTimer) clearTimeout(this._searchTimer);

    if (!this.searchQuery) {
      this._searchResults = null;
      this.applySearch();
      return;
    }

    // Instant: filter titles
    this.applySearch();

    // Debounced: full-text search (300ms)
    if (this.searchQuery.length >= 2) {
      this._searchTimer = setTimeout(() => this.fullTextSearch(this.searchQuery), 300);
    }
  }

  async fullTextSearch(query) {
    // Don't search if query changed since debounce
    if (query !== this.searchQuery) return;

    try {
      // Scope full-text search to the currently listed project paths so the
      // result surface matches what the sidebar shows.
      if (typeof this.transport?.searchSessions !== "function") {
        throw new Error("Host session search unavailable");
      }
      const data = await this.transport.searchSessions(query);
      if (query !== this.searchQuery) return; // stale

      this._searchResults = data.results || [];
      this.renderSearchResults();
    } catch (err) {
      console.error("[Sidebar] Search failed:", err);
    }
  }

  renderSearchResults() {
    if (!this._searchResults || this._searchResults.length === 0) return;

    // Remove previous search results section
    const existing = this.container.querySelector(".search-results-group");
    if (existing) existing.remove();

    const group = document.createElement("div");
    group.className = "search-results-group";

    const header = document.createElement("div");
    header.className = "project-header search-results-header";
    const searchIcon = document.createElement("span");
    searchIcon.setAttribute("aria-hidden", "true");
    const searchGlyph = createActionIcon("search", 14);
    if (searchGlyph) searchIcon.appendChild(searchGlyph);
    const label = document.createElement("span");
    label.textContent = t("sidebar.messageMatches");
    const count = document.createElement("span");
    count.className = "project-count";
    count.textContent = String(this._searchResults.length);
    header.append(searchIcon, label, count);
    group.appendChild(header);

    const sessionsDiv = document.createElement("div");
    sessionsDiv.className = "project-sessions";

    for (const result of this._searchResults) {
      const item = document.createElement("div");
      item.className = "session-item search-result-item";
      item.dataset.filePath = result.filePath;
      item.dataset.name = String(result.sessionName || "").toLowerCase();
      item.dataset.firstMessage = String(result.firstMessage || "").toLowerCase();

      if (result.filePath === this.activeSessionFile) {
        item.classList.add("active");
      }

      const title = getSessionDisplayTitle({
        name: result.sessionName,
        firstMessage: result.firstMessage,
      });
      const snippet = result.matches[0]?.snippet || "";
      const matchCount = result.matches.length;
      const time = this.formatTime(result.sessionTimestamp);

      const titleRow = document.createElement("div");
      titleRow.className = "session-title-row";
      const titleElement = document.createElement("div");
      titleElement.className = "session-title";
      titleElement.title = title;
      titleElement.textContent = title;
      titleRow.appendChild(titleElement);
      const renameBlocked = this.renameBlockedReason(result.filePath);
      const renameButton = document.createElement("button");
      renameButton.type = "button";
      renameButton.className = "session-rename-btn";
      renameButton.title = renameBlocked || t("sidebar.rename");
      renameButton.setAttribute("aria-label", renameBlocked || t("sidebar.renameSessionAriaLabel"));
      if (renameBlocked) {
        renameButton.disabled = true;
        renameButton.classList.add("action-hidden");
      }
      const renameIcon = createActionIcon("pencil", 13);
      if (renameIcon) renameButton.appendChild(renameIcon);
      renameButton.addEventListener("click", (event) => {
        event.stopPropagation();
        if (renameButton.disabled) return;
        this.startRename(item, {
          filePath: result.filePath,
          name: result.sessionName || "",
          firstMessage: result.firstMessage || "",
        });
      });
      titleRow.appendChild(renameButton);
      const snippetElement = document.createElement("div");
      snippetElement.className = "search-snippet";
      appendHighlightedText(snippetElement, snippet, this.searchQuery);
      const meta = document.createElement("div");
      meta.className = "session-meta";
      meta.textContent = time;
      if (matchCount > 1) meta.append(` · ${t("sidebar.matchCount", { count: matchCount })}`);
      item.append(titleRow, snippetElement, meta);

      // Find the matching project/session to pass to onSessionSelect
      item.addEventListener("contextmenu", (event) => {
        this.showSessionContextMenu(event, item, {
          filePath: result.filePath,
          name: result.sessionName || "",
          firstMessage: result.firstMessage || "",
        });
      });
      item.addEventListener("click", () => {
        for (const project of this.projects) {
          const session = project.sessions.find((s) => s.filePath === result.filePath);
          if (session) {
            this.revealSessionInWorkspace(project, session);
            this.onSessionSelect(session, project);
            return;
          }
        }
        // Session not in loaded list (unlikely) — try switching by path
        this.onSessionSelect(
          { filePath: result.filePath, name: result.sessionName },
          { path: result.project },
        );
      });

      sessionsDiv.appendChild(item);
    }

    group.appendChild(sessionsDiv);
    // Insert at top of container
    this.container.insertBefore(group, this.container.firstChild);
    this.rebuildStatusIndex();
  }

  highlightMatch(text, query) {
    const fragment = document.createDocumentFragment();
    appendHighlightedText(fragment, text, query);
    return fragment;
  }
  applySearch() {
    if (!this.searchQuery) {
      this.container.querySelectorAll(".session-item").forEach((el) => {
        el.classList.remove("hidden");
      });
      this.container.querySelectorAll(".project-group, .pinned-group").forEach((el) => {
        el.style.display = "";
      });
      const searchGroup = this.container.querySelector(".search-results-group");
      if (searchGroup) searchGroup.remove();
      return;
    }

    this.container.querySelectorAll(".project-group").forEach((group) => {
      let hasVisible = false;
      const projectMatches = (group.dataset.projectSearchText || "").includes(this.searchQuery);
      group.querySelectorAll(".session-item").forEach((item) => {
        const matches = projectMatches || this.sessionItemMatchesSearch(item);
        item.classList.toggle("hidden", !matches);
        if (matches) hasVisible = true;
      });
      group.style.display = hasVisible ? "" : "none";
    });
  }

  sessionItemMatchesSearch(item) {
    const searchable = [
      item.dataset.name || "",
      item.dataset.firstMessage || "",
      item.dataset.projectSearchText || "",
    ];
    return searchable.some((value) => value.includes(this.searchQuery));
  }

  setActive(filePath) {
    if (this.activeSessionFile === filePath) return;
    this.activeSessionFile = filePath;
    if (filePath && this.unread.has(filePath)) {
      this.unread.delete(filePath);
      this.saveUnread();
    }
    // Workspace header actions depend on which project owns the active
    // session. Render through the keyed row cache so the Focus button appears
    // immediately without a sidebar data refresh.
    this.render();
  }

  clearActive() {
    this.activeSessionFile = null;
    this.container.querySelectorAll(".session-item").forEach((el) => {
      el.classList.remove("active");
    });
    this.container.querySelectorAll(".session-item[data-file-path]").forEach((el) => {
      this.applyStatusToItem(el.dataset.filePath);
    });
  }

  // ═══════════════════════════════════════
  // Context Menu
  // ═══════════════════════════════════════

  showWorkspaceContextMenu(event, workspace) {
    event.preventDefault();
    this.closeContextMenu();

    const registryRow = workspace?.source === "registry";
    const isPinned = Boolean(registryRow && workspace.pinned);
    const items = [
      ...(registryRow && this.isRegistryAvailable()
        ? [
            {
              iconKind: "pin",
              label: isPinned ? t("sidebar.unpinWorkspace") : t("sidebar.pinWorkspace"),
              action: () => void this.toggleWorkspacePin(workspace, isPinned),
            },
          ]
        : []),
      {
        iconKind: "folder",
        label: t("sidebar.openInFinder"),
        action: () => this.onOpenProject?.(workspace),
      },
      // Registry rows offer list-removal without touching the directory or
      // its session files — unlike the destructive batch delete below. The
      // current workspace itself is never removable: its window cannot be
      // closed, so un-registering it would strand a live row permanently.
      ...(registryRow && this.isRegistryAvailable()
        ? [
            {
              iconKind: "x",
              label: t("sidebar.removeFromList"),
              disabledReason: this.isCurrentWorkspace?.(workspace)
                ? t("sidebar.removeDisabledCurrent")
                : null,
              action: () => void this.removeFromList(workspace),
            },
          ]
        : []),
      {
        iconKind: "trash-2",
        label: t("sidebar.deleteWorkspaceMainSessions"),
        action: () => this.deleteWorkspaceSessions(workspace),
      },
    ];

    const menu = document.createElement("div");
    menu.className = "sidebar-context-menu";
    menu.setAttribute("role", "menu");
    for (const item of items) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "context-menu-item";
      row.setAttribute("role", "menuitem");

      const icon = document.createElement("span");
      icon.className = `context-menu-icon${item.iconClass ? ` ${item.iconClass}` : ""}`;
      icon.setAttribute("aria-hidden", "true");
      if (item.iconKind) {
        const iconNode = createActionIcon(item.iconKind);
        if (iconNode) icon.appendChild(iconNode);
      }
      const label = document.createElement("span");
      label.textContent = item.label;
      row.append(icon, label);
      if (item.disabledReason) {
        row.disabled = true;
        row.title = item.disabledReason;
        row.setAttribute("aria-label", item.disabledReason);
      }
      row.addEventListener("click", (clickEvent) => {
        clickEvent.stopPropagation();
        this.closeContextMenu();
        item.action();
      });
      menu.appendChild(row);
    }

    document.body.appendChild(menu);
    const rect = menu.getBoundingClientRect();
    let x = event.clientX;
    let y = event.clientY;
    if (x + rect.width > window.innerWidth) x = window.innerWidth - rect.width - 8;
    if (y + rect.height > window.innerHeight) y = window.innerHeight - rect.height - 8;
    menu.style.left = `${x}px`;
    menu.style.top = `${y}px`;
    this.contextMenu = menu;
  }

  /**
   * DB pin/unpin for registry rows; cookie store stays the browser fallback.
   * Refresh reloads server-side ordering (pinned DESC, last_opened DESC).
   */
  async toggleWorkspacePin(workspace, currentlyPinned) {
    if (workspace?.source !== "registry" || !this.isRegistryAvailable()) return;
    try {
      await this.transport.setWorkspacePinned(workspace.registryId, !currentlyPinned);
    } catch (error) {
      console.error("[Sidebar] workspace pin failed:", error);
      return;
    }
    await this.refresh();
  }

  /** Remove a registry row only; directories and session files untouched. */
  async removeFromList(workspace) {
    if (workspace?.source !== "registry" || !this.isRegistryAvailable()) return;
    // The current workspace's window cannot be closed; refuse the
    // un-registration rather than stranding a live row the user cannot
    // dismiss (mirrors the delete-disabled protections on sessions).
    if (this.isCurrentWorkspace?.(workspace)) {
      this.onSessionNotice?.(t("sidebar.removeDisabledCurrent"));
      return;
    }
    let removed = false;
    try {
      await this.transport.removeWorkspace(workspace.registryId);
      this.invalidateWorkspaceSessions(workspace.path);
      this.expandedWorkspaces.delete(workspace.workspaceId);
      this._projectRowCache.delete(workspace.workspaceId);
      removed = true;
    } catch (error) {
      console.error("[Sidebar] remove from list failed:", error);
    } finally {
      await this.refresh();
    }
    if (!removed) return;
    // After the reload a still-running window keeps a temporary live row;
    // say so instead of implying the workspace vanished entirely.
    const key = workspacePathKey(workspace.path);
    const stillRunning = this.projects.some(
      (project) => project.source === "live" && workspacePathKey(project.path) === key,
    );
    this.onSessionNotice?.(
      t(stillRunning ? "sidebar.removedFromListStillRunning" : "sidebar.removedFromList"),
    );
  }

  /** Toolbar entry: pick a folder via broker, register it, expand the row. */
  async addProjectViaPicker() {
    if (!this.isRegistryAvailable()) {
      this.onSessionNotice?.(t("sidebar.addProjectDesktopOnly"));
      return;
    }
    let pickedPath = "";
    try {
      const picked = await this.transport.pickFolder();
      pickedPath = typeof picked === "string" ? picked : (picked?.path ?? "");
    } catch {
      return; // user cancelled or picker unavailable
    }
    if (!pickedPath) return;
    let result;
    try {
      result = await this.transport.addWorkspace(pickedPath);
    } catch (error) {
      this.onSessionNotice?.(`${t("sidebar.addFailed")}: ${String(error?.message || error)}`);
      return;
    }
    const added = result?.added !== false;
    if (result?.workspace?.workspaceId) {
      const workspaceId = `ws:${result.workspace.workspaceId}`;
      this.expandedWorkspaces.add(workspaceId);
    }
    if (typeof this.onRegisterWorkspace === "function") {
      const launched = await this.onRegisterWorkspace(
        result?.workspace?.canonicalPath || pickedPath,
      );
      if (launched !== false) return;
    }
    // Non-native callers retain the old local refresh behavior. Native callers
    // navigate to the new owner-bound session, whose first sidebar load is the
    // authoritative refresh and avoids rendering an empty intermediate row.
    await this.refresh();
    if (added && result?.workspace?.workspaceId) {
      const project = this.projects.find(
        (candidate) =>
          candidate.source === "registry" && candidate.registryId === result.workspace.workspaceId,
      );
      if (project) await this.ensureWorkspaceSessions(project);
    }
  }

  deletionBlockedReason(filePath) {
    if (filePath === this.activeSessionFile) return t("sidebar.deleteDisabledActive");
    if (this.streamingFiles.has(filePath)) return t("sidebar.deleteDisabledStreaming");
    if (this.isLiveSession(filePath)) return t("sidebar.deleteDisabledRunning");
    return null;
  }

  /** Rename is blocked only while the session is the open one or has a
   * running turn. An idle session with a background runtime stays renameable:
   * renaming appends one `session_info` name event instead of rewriting the
   * transcript. (Delete is stricter and still refuses live runtimes.) */
  renameBlockedReason(filePath) {
    if (filePath === this.activeSessionFile) return t("sidebar.renameDisabledActive");
    if (this.streamingFiles.has(filePath)) return t("sidebar.renameDisabledStreaming");
    return null;
  }

  isLiveSession(filePath) {
    const live =
      typeof this.getLiveInstances === "function"
        ? this.getLiveInstances()
        : this.liveInstancesSnapshot;
    return Array.isArray(live) && live.some((instance) => instance?.sessionFile === filePath);
  }

  // Deletes every deletable session of a workspace in one confirmed batch.
  // Paths the server reports as `running` or `errors` remain protected;
  // only confirmed deletions are removed after the session list reloads.
  async deleteWorkspaceSessions(workspace) {
    if (!workspace) return;
    const inFlight = this._registryBusyPromises.get(workspace.workspaceId);
    if (inFlight) await inFlight;
    const currentWorkspace =
      this.projects.find(
        (project) => workspacePathKey(project.path) === workspacePathKey(workspace.path),
      ) || workspace;
    const workspaceKey = workspacePathKey(currentWorkspace.path);
    if (
      currentWorkspace.source === "registry" &&
      (inFlight ||
        (!this.workspaceSessionsCache.has(workspaceKey) &&
          (!Array.isArray(currentWorkspace.sessions) || currentWorkspace.sessions.length === 0)))
    ) {
      await this.ensureWorkspaceSessions(currentWorkspace, { force: true });
    }
    const filePaths = (currentWorkspace.sessions || [])
      .map((session) => session?.filePath)
      .filter(
        (filePath) =>
          typeof filePath === "string" && filePath && this.deletionBlockedReason(filePath) === null,
      );
    if (filePaths.length === 0) {
      if (workspace.hiddenSubagentCount > 0) {
        this.onSessionNotice?.(t("sidebar.noMainSessionsToDelete"));
      }
      return;
    }
    const workspaceName =
      currentWorkspace?.folderName ||
      basenameLocalPath(currentWorkspace?.path) ||
      currentWorkspace?.path ||
      t("sidebar.unavailable");
    const ok = await this.confirmWorkspaceDeletion(workspaceName, filePaths.length);
    if (!ok) return;

    let data = { deleted: 0, running: [], errors: [] };
    try {
      if (typeof this.transport?.sessionDeleteBatch !== "function") {
        throw new Error("Host session delete unavailable");
      }
      data = await this.transport.sessionDeleteBatch(filePaths);
      if ((data.running || []).length > 0) {
        this.onSessionNotice?.(t("sidebar.deleteSessionRunning"));
      }
    } catch (err) {
      console.error("[Sidebar] deleteWorkspaceSessions failed:", err);
      this.onSessionNotice?.(t("sidebar.deleteSessionFailed"));
    }

    // A batch that deleted nothing while reporting per-path errors is a
    // failure, not a silent no-op: every confirmed path was rejected.
    if (data.deleted === 0 && (data.errors || []).length > 0) {
      this.onSessionNotice?.(t("sidebar.deleteSessionFailed"));
    }

    // Batch deletes must invalidate this workspace's cache or the reload
    // below would restore the deleted sessions from it (ghost entries).
    this.invalidateWorkspaceSessions(workspace?.path);
    for (const filePath of filePaths) {
      if (!(data.errors || []).includes(filePath)) this.onSessionDeleted?.(filePath);
    }
    await this.refresh({ workspacePath: currentWorkspace.path });
    const refreshedWorkspace = this.projects.find(
      (project) => workspacePathKey(project.path) === workspacePathKey(currentWorkspace.path),
    );
    let hiddenSubagentCount = Math.max(
      currentWorkspace.hiddenSubagentCount || 0,
      refreshedWorkspace?.hiddenSubagentCount || 0,
    );
    if (
      data.deleted > 0 &&
      refreshedWorkspace?.source === "registry" &&
      typeof refreshedWorkspace.hiddenSubagentCount !== "number"
    ) {
      await this.ensureWorkspaceSessions(refreshedWorkspace, { force: true });
      hiddenSubagentCount = Math.max(
        hiddenSubagentCount,
        refreshedWorkspace.hiddenSubagentCount || 0,
      );
    }
    if (data.deleted > 0 && hiddenSubagentCount > 0) {
      this.onSessionNotice?.(
        t("sidebar.deletedMainSessionsSubagentsKept", {
          count: data.deleted,
          hiddenCount: hiddenSubagentCount,
        }),
      );
    }
  }

  closeContextMenu() {
    if (this.contextMenu) {
      this.contextMenu.remove();
      this.contextMenu = null;
    }
  }

  showSessionContextMenu(event, itemEl, session) {
    event?.preventDefault();
    this.closeContextMenu();
    const renameBlocked = session ? this.renameBlockedReason(session.filePath) : null;
    if (renameBlocked) return;
    const menu = document.createElement("div");
    menu.className = "sidebar-context-menu";
    menu.setAttribute("role", "menu");
    const rename = document.createElement("button");
    rename.type = "button";
    rename.className = "context-menu-item";
    rename.setAttribute("role", "menuitem");
    rename.textContent = t("sidebar.rename");
    rename.addEventListener("click", (clickEvent) => {
      clickEvent.stopPropagation();
      this.closeContextMenu();
      this.startRename(itemEl, session);
    });
    menu.appendChild(rename);
    document.body.appendChild(menu);
    const rect = menu.getBoundingClientRect();
    const anchor = event || itemEl.getBoundingClientRect();
    const clientX = event ? event.clientX : anchor.left;
    const clientY = event ? event.clientY : anchor.bottom;
    const x = Math.min(clientX, window.innerWidth - rect.width - 8);
    const y = Math.min(clientY, window.innerHeight - rect.height - 8);
    menu.style.left = `${Math.max(8, x)}px`;
    menu.style.top = `${Math.max(8, y)}px`;
    this.contextMenu = menu;
  }

  renameSession(filePath, session, itemEl) {
    const targetItem =
      itemEl ||
      Array.from(this.container.querySelectorAll(".session-item")).find(
        (item) => item.dataset.filePath === filePath,
      );
    if (targetItem) this.startRename(targetItem, { ...session, filePath });
  }

  startRename(itemEl, session = null) {
    const filePath = session?.filePath || itemEl?.dataset?.filePath;
    if (filePath && this.renameBlockedReason(filePath)) return;
    const titleEl = itemEl.querySelector(".session-title");
    if (!titleEl || itemEl.querySelector(".session-rename-input")) return;
    const target = session ||
      this.projects
        .flatMap((project) => project.sessions || [])
        .find((candidate) => candidate.filePath === itemEl.dataset.filePath) || {
        filePath: itemEl.dataset.filePath,
        name: itemEl.dataset.name || "",
        firstMessage: itemEl.dataset.firstMessage || "",
      };
    const currentName = target.name || "";
    const input = document.createElement("input");
    input.className = "session-rename-input";
    input.value = currentName;
    input.placeholder = target.firstMessage || t("sidebar.renameInputPlaceholder");
    input.setAttribute("aria-label", t("sidebar.renameSessionAriaLabel"));
    titleEl.replaceWith(input);
    input.focus();
    input.select();

    let finished = false;
    let submitting = false;
    const showError = (message) => {
      let error = itemEl.querySelector(".session-rename-error");
      if (!error) {
        error = document.createElement("div");
        error.className = "session-rename-error";
        input.parentElement?.appendChild(error);
      }
      error.textContent = message;
    };
    const restore = () => {
      if (finished) return;
      finished = true;
      const replacement = document.createElement("div");
      replacement.className = "session-title";
      replacement.title = getSessionDisplayTitle(target);
      replacement.textContent = getSessionDisplayTitle(target);
      input.replaceWith(replacement);
      itemEl.querySelector(".session-rename-error")?.remove();
    };
    let canRetry = false;
    const commit = async () => {
      if (submitting || finished) return;
      const newName = input.value.trim();
      if (!newName) {
        showError(t("sidebar.renameErrorInvalid"));
        input.focus();
        return;
      }
      if (newName === currentName) {
        restore();
        return;
      }
      submitting = true;
      input.disabled = true;
      input.classList.add("busy");
      try {
        try {
          if (typeof this.transport?.sessionRename !== "function") {
            throw new Error("Host session rename unavailable");
          }
          await this.transport.sessionRename(target.filePath, newName);
        } catch (error) {
          canRetry = error?.code === "session_rename_failed";
          throw error;
        }
        this.invalidateSessionLoads();
        target.name = newName;
        for (const project of this.projects) {
          for (const candidate of project.sessions || []) {
            if (candidate.filePath === target.filePath) candidate.name = newName;
          }
        }
        this.container.querySelectorAll(".session-item").forEach((row) => {
          if (row.dataset.filePath !== target.filePath) return;
          row.dataset.name = newName.toLowerCase();
          const title = row.querySelector(".session-title");
          if (title) {
            title.textContent = newName;
            title.title = newName;
          }
        });
        restore();
        await this.refresh();
      } catch (error) {
        submitting = false;
        input.disabled = false;
        input.classList.remove("busy");
        const message = error instanceof Error ? error.message : t("sidebar.renameErrorServer");
        showError(message);
        if (
          canRetry &&
          !itemEl.querySelector(".session-rename-retry") &&
          input.disabled === false
        ) {
          const retry = document.createElement("button");
          retry.type = "button";
          retry.className = "session-rename-retry";
          retry.textContent = t("sidebar.renameRetry");
          const keepEditorFocused = (event) => event.preventDefault();
          retry.addEventListener("pointerdown", keepEditorFocused);
          retry.addEventListener("mousedown", keepEditorFocused);
          retry.addEventListener("click", () => commit());
          itemEl.querySelector(".session-rename-error")?.append(" ", retry);
        }
        input.focus();
      }
    };
    input.addEventListener("blur", () => {
      if (!submitting) restore();
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        commit();
      } else if (event.key === "Escape") {
        event.preventDefault();
        restore();
      }
    });
  }

  async exportSession(session) {
    try {
      const sessionId = session?.id;
      if (!sessionId || !this.transport?.exportSession) return;
      const result = await this.transport.exportSession(sessionId);
      if (!result?.exportUrl) return;
      const response = await fetch(result.exportUrl);
      if (!response.ok) return;
      const blob = await response.blob();
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `${sessionId}.html`;
      document.body.appendChild(anchor);
      anchor.click();
      anchor.remove();
      URL.revokeObjectURL(url);
    } catch (error) {
      console.error("[Sidebar] session export failed:", error);
    }
  }

  // ═══════════════════════════════════════
  // Render
  // ═══════════════════════════════════════

  buildSessionItem(session, project, options = {}) {
    const {
      showDeleteButton = false,
      deletionBlockedReason = null,
      renameBlockedReason = null,
      onDelete = null,
      treeDepth = 0,
      treeIsLast = true,
      treeAncestorChain = null,
    } = options;
    const isProvisional = session?.provisional === true;
    const onSelect =
      !isProvisional && this.onSessionSelect
        ? (selectedSession, selectedProject) =>
            this.onSessionSelect(selectedSession, selectedProject)
        : null;
    return buildSessionItemNode({
      session,
      project,
      isActive: session?.provisional === true || session.filePath === this.activeSessionFile,
      isUnread: this.unread.has(session.filePath),
      isStreaming: this.streamingFiles.has(session.filePath),
      showPinButton: false,
      showDeleteButton: isProvisional ? false : showDeleteButton,
      deletionBlockedReason: deletionBlockedReason ?? this.deletionBlockedReason(session.filePath),
      renameBlockedReason: renameBlockedReason ?? this.renameBlockedReason(session.filePath),
      projectSearchText: this.getProjectSearchText(project),
      formattedTime: this.formatTime(session.mtime ?? session.timestamp),
      treeDepth,
      treeIsLast,
      treeAncestorChain,
      onSelect,
      onDelete: isProvisional ? null : onDelete || ((filePath) => this.deleteSession(filePath)),
      onRename: isProvisional
        ? null
        : (filePath, session, item) => this.renameSession(filePath, session, item),
      onContextMenu: isProvisional
        ? null
        : (event, item, session) => this.showSessionContextMenu(event, item, session),
      createIcon: createActionIcon,
    });
  }

  getProjectVisibilityKey(project) {
    return project?.path || project?.dirName || "";
  }

  /**
   * Pinned-workspace render state on native desktops: DB registry rows.
   */
  getRenderablePinState() {
    // Pinned-workspace render state now comes solely from DB registry rows.
    return { workspaces: this._registryPins ?? [] };
  }

  // Snapshot of pinned workspace IDs, used to detect newly-pinned workspaces
  // across pin-store notifications. Falls back to getRenderableState for stubs
  // that do not expose getState.
  snapshotPinnedWorkspaceIds() {
    return new Set(
      (this.getRenderablePinState().workspaces || []).map((w) => w?.id).filter(Boolean),
    );
  }

  // Stable expansion key for a workspace record. Prefers the canonical
  // workspaceId; falls back to the path/dirName visibility key so test
  // fixtures and live-only workspaces without an id still track distinctly.
  getWorkspaceExpansionKey(workspace) {
    return workspace?.workspaceId || this.getProjectVisibilityKey(workspace) || "";
  }

  isWorkspaceExpanded(workspace) {
    const key = this.getWorkspaceExpansionKey(workspace);
    return Boolean(key) && this.expandedWorkspaces.has(key);
  }

  setWorkspaceExpanded(workspace, expanded) {
    const key = this.getWorkspaceExpansionKey(workspace);
    if (!key) return;
    if (expanded) {
      this.expandedWorkspaces.add(key);
      // Registry rows load their history lazily on first expansion.
      void this.ensureWorkspaceSessions(workspace);
    } else {
      this.expandedWorkspaces.delete(key);
    }
  }

  /** A search hit can live under a collapsed workspace group: expand the
   *  group and center the row, so the selected session is actually visible
   *  instead of highlighted somewhere the user cannot see. */
  revealSessionInWorkspace(project, session) {
    const key = this.getWorkspaceExpansionKey(project);
    if (key && !this.expandedWorkspaces.has(key)) {
      this.setWorkspaceExpanded(project, true);
      this.render();
    }
    requestAnimationFrame(() => {
      const row = this.container.querySelector(
        `.session-item[data-file-path="${CSS.escape(session.filePath)}"]`,
      );
      row?.scrollIntoView({ block: "center" });
    });
  }

  getProjectVisibleSessionCount(project, sessionCount) {
    const key = this.getProjectVisibilityKey(project);
    const stored = this.projectVisibleSessionCounts.get(key);
    if (typeof stored === "number" && Number.isFinite(stored)) {
      return Math.max(this.projectSessionInitialLimit, Math.min(sessionCount, Math.floor(stored)));
    }
    return Math.min(sessionCount, this.projectSessionInitialLimit);
  }

  setProjectVisibleSessionCount(project, sessionCount) {
    const key = this.getProjectVisibilityKey(project);
    if (!key) return;
    this.projectVisibleSessionCounts.set(
      key,
      Math.max(this.projectSessionInitialLimit, sessionCount),
    );
  }

  buildProjectSessionsToggleRow(project, visibleCount, totalCount) {
    const hasMore = visibleCount < totalCount;
    const canShowLess = visibleCount > this.projectSessionInitialLimit;
    if (!hasMore && !canShowLess) return null;

    const toggleRow = document.createElement("div");
    toggleRow.className = "project-sessions-toggle-row";

    if (hasMore) {
      const showMoreButton = document.createElement("button");
      showMoreButton.type = "button";
      showMoreButton.className = "project-sessions-toggle";
      showMoreButton.textContent = t("sidebar.showMore");
      showMoreButton.addEventListener("click", (event) => {
        event.stopPropagation();
        this.setProjectVisibleSessionCount(project, visibleCount + this.projectSessionStep);
        this.render();
      });
      toggleRow.appendChild(showMoreButton);
    }

    if (canShowLess) {
      const showLessButton = document.createElement("button");
      showLessButton.type = "button";
      showLessButton.className = "project-sessions-toggle project-sessions-toggle-less";
      showLessButton.textContent = t("sidebar.showLess");
      showLessButton.addEventListener("click", (event) => {
        event.stopPropagation();
        this.setProjectVisibleSessionCount(
          project,
          Math.max(this.projectSessionInitialLimit, visibleCount - this.projectSessionStep),
        );
        this.render();
      });
      toggleRow.appendChild(showLessButton);
    }

    return toggleRow;
  }
  renderPinnedSection() {
    const state = this.getRenderablePinState();
    const pinnedGroups = resolvePinnedWorkspaceGroups({
      pinState: state,
      projects: this.projects,
    });
    const { section } = buildSidebarSection({
      region: "pinned",
      titleKey: "sidebar.pinned",
      count: pinnedGroups.length,
      expanded: !this.pinnedCollapsed,
      onToggle: (expanded) => {
        this.pinnedCollapsed = !expanded;
      },
      renderSessions: (body) => {
        for (const pinned of pinnedGroups) {
          const workspace = pinned.workspace;
          const unavailableFilePath = pinned.sessions[0]?.filePath || "";
          const workspacePath = workspace?.path || "";
          const folderName =
            workspace?.folderName ||
            basenameLocalPath(workspacePath) ||
            unavailableFilePath ||
            t("sidebar.unavailable");
          const workspaceId = workspace?.workspaceId || `pinned-session:${unavailableFilePath}`;
          // Expansion and lazy-load must carry the full registry project
          // (source/path/registryId). A bare {workspaceId} object makes
          // ensureWorkspaceSessions bail at its source!="registry" guard,
          // so a pinned workspace expands to an empty session list until
          // it is unpinned back into the Projects region.
          const expansionWorkspace = workspace || { workspaceId };
          const pinnedActive =
            !pinned.unavailable &&
            Array.isArray(pinned.sessions) &&
            pinned.sessions.some((s) => s?.filePath === this.activeSessionFile);
          const pinnedCurrent = !pinned.unavailable && this.isCurrentWorkspace?.(workspace);
          const focusEnabled = this._workspaceFocusEnabled(
            workspace,
            pinnedActive || pinnedCurrent,
          );
          const { group } = buildSidebarWorkspaceGroup({
            workspaceId,
            folderName,
            workspacePath,
            // Pinned and Projects sections render the same workspace object;
            // use host's authoritative count in both places. The array may be
            // a lazy cache and can lag one session behind the registry count.
            sessionCount:
              workspace?.sessionCount === null
                ? null
                : (workspace?.sessionCount ?? pinned.sessions.length),
            expanded: this.isWorkspaceExpanded(expansionWorkspace),
            onToggle: (expanded) => this.setWorkspaceExpanded(expansionWorkspace, expanded),
            onNewChat: !pinned.unavailable && workspace ? () => this.onNewChat(workspace) : null,
            onContextMenu:
              !pinned.unavailable && workspace
                ? (event) => this.showWorkspaceContextMenu(event, workspace)
                : null,
            onMoreActions:
              !pinned.unavailable && workspace
                ? (event) => this.showWorkspaceContextMenu(event, workspace)
                : null,
            focusEnabled,
            onFocus: focusEnabled ? () => this.onWorkspaceFocus?.(workspace) : null,
            renderSessions: (container) => {
              if (pinned.unavailable) {
                const unavailable = document.createElement("div");
                unavailable.className = "pinned-unavailable";
                unavailable.textContent =
                  workspacePath || unavailableFilePath || t("sidebar.unavailable");
                container.appendChild(unavailable);

                const staleHint = document.createElement("div");
                staleHint.className = "pinned-unavailable";
                staleHint.textContent = t("sidebar.removedFromList");
                container.appendChild(staleHint);
                return;
              }

              const pinnedSessions = pinned.sessions;
              const pinnedRows = buildFlattenedSessionTree(pinnedSessions);
              const pinnedVisibleCount = this.getProjectVisibleSessionCount(
                workspace,
                pinnedRows.length,
              );
              const pinnedToRender = pinnedRows.slice(0, pinnedVisibleCount);
              for (const row of pinnedToRender) {
                container.appendChild(
                  this.buildSessionItem(row.session, workspace, {
                    showDeleteButton: true,
                    treeDepth: row.depth,
                    treeIsLast: row.isLast,
                    treeAncestorChain: row.ancestorChain,
                  }),
                );
              }
              const pinnedToggle = this.buildProjectSessionsToggleRow(
                workspace,
                pinnedToRender.length,
                pinnedRows.length,
              );
              if (pinnedToggle) container.appendChild(pinnedToggle);
            },
          });
          group.classList.add("pinned-workspace-group");

          body.appendChild(group);
        }
      },
    });
    section.className = `pinned-group ${section.className}`;
    this.container.appendChild(section);
  }

  /**
   * Build-or-reuse one workspace row keyed by workspaceId. A matching
   * signature returns the previous DOM node so hover state, scroll position
   * inside long lists, and quick-info overlays survive routine refreshes.
   */
  projectRowNode(project) {
    const visibleSessions = project.sessions || [];
    const visibleRows = buildFlattenedSessionTree(visibleSessions);
    // Pagination ("show more") mutates only the visible slice; include it in
    // the signature so those clicks still rebuild/reuse the correct DOM.
    const visibleCount = this.getProjectVisibleSessionCount(project, visibleRows.length);
    const signature = JSON.stringify([
      project.workspaceId,
      project.folderName || "",
      project.path || "",
      this.isWorkspaceExpanded(project),
      visibleCount,
      project.sessionCount ?? null,
      project.hiddenSubagentCount ?? null,
      visibleRows.map((row) => [
        row.session.filePath,
        row.session.name ?? null,
        row.session.parentSession ?? null,
        Number(row.session.mtime) || 0,
        row.depth,
        row.ancestorKey,
        Boolean(row.session.isRunning),
        row.session.port ?? null,
        this.unread.has(row.session.filePath),
        this.streamingFiles?.has(row.session.filePath) || false,
        row.session.filePath === this.activeSessionFile,
        this.isLiveSession(row.session.filePath), // delete gate reads the live snapshot
      ]),
      this.searchQuery ? "searching" : "browse",
      project.workspaceId === "" ? String(Math.random()) : "stable", // live rows always rebuild
    ]);
    const cached = this._projectRowCache.get(project.workspaceId);
    if (cached && cached.signature === signature) return cached;

    const sessionsToRender = this.searchQuery ? visibleRows : visibleRows.slice(0, visibleCount);
    const projectActive = Array.isArray(project.sessions)
      ? project.sessions.some((s) => s?.filePath === this.activeSessionFile)
      : false;
    const focusEnabled = this._workspaceFocusEnabled(
      project,
      projectActive || this.isCurrentWorkspace?.(project),
    );
    const { group } = buildSidebarWorkspaceGroup({
      workspaceId: project.workspaceId,
      folderName:
        project.folderName ||
        basenameLocalPath(project.path) ||
        project.path ||
        t("sidebar.unavailable"),
      workspacePath: project.path,
      // Collapsed rows that have not loaded details yet show the cheap
      // readdir count; expanded rows show the precise loaded-session count.
      // Running-instance rows always count so a live window never shows 0.
      sessionCount:
        project.sessionCount === null
          ? null
          : Math.max(project.sessionCount ?? 0, visibleSessions.length),
      expanded: this.isWorkspaceExpanded(project),
      onToggle: (expanded) => this.setWorkspaceExpanded(project, expanded),
      onNewChat: this.onNewChat ? () => this.onNewChat(project) : null,
      onContextMenu: (event) => this.showWorkspaceContextMenu(event, project),
      onMoreActions: (event) => this.showWorkspaceContextMenu(event, project),
      focusEnabled,
      onFocus: focusEnabled ? () => this.onWorkspaceFocus?.(project) : null,
      renderSessions: (sessionsDiv) => {
        for (const row of sessionsToRender) {
          sessionsDiv.appendChild(
            this.buildSessionItem(row.session, project, {
              showDeleteButton: true,
              treeDepth: row.depth,
              treeIsLast: row.isLast,
              treeAncestorChain: row.ancestorChain,
            }),
          );
        }
        if (!this.searchQuery) {
          const toggleRow = this.buildProjectSessionsToggleRow(
            project,
            sessionsToRender.length,
            visibleRows.length,
          );
          if (toggleRow) sessionsDiv.appendChild(toggleRow);
        }
      },
    });
    group.dataset.projectSearchText = this.getProjectSearchText(project);
    const entry = { signature, group };
    this._projectRowCache.set(project.workspaceId, entry);
    return entry;
  }

  /**
   * Whether the Focus entry renders for this workspace row. The explicit
   * `canFocusWorkspace` seam wins when provided; otherwise the classic
   * active-session-or-current-workspace predicate applies.
   */
  _workspaceFocusEnabled(project, activeOrCurrent) {
    if (this.canFocusWorkspace) return Boolean(this.canFocusWorkspace(project));
    return Boolean(activeOrCurrent) && !!this.onWorkspaceFocus;
  }

  render() {
    // While Focus mode owns the sidebar, delegate any render request to the
    // focus view instead of rebuilding the normal session list. This keeps
    // loadSessions()/setActive() refreshes from clobbering the focus view.
    if (this.isFocusActive?.() && typeof this.onFocusRefresh === "function") {
      this.onFocusRefresh();
      return;
    }
    this.container.replaceChildren();

    this.renderPinnedSection();

    const projectRows = this.projects.filter(
      (project) => project.source !== "registry" || !project.pinned,
    );
    const { section: projectsSection, sessionsContainer: projectsGroup } = buildSidebarSection({
      region: "projects",
      titleKey: "sidebar.projects",
      count: projectRows.length,
      expanded: !this.projectsCollapsed,
      onToggle: (expanded) => {
        this.projectsCollapsed = !expanded;
      },
    });
    projectsSection.className = `projects-group ${projectsSection.className}`;
    const seenRowKeys = new Set();
    for (const project of projectRows) {
      seenRowKeys.add(project.workspaceId);
      const { group } = this.projectRowNode(project);
      projectsGroup.appendChild(group);
    }
    // Drop cached nodes for rows that disappeared (removed live rows, etc.)
    for (const cacheKey of [...this._projectRowCache.keys()]) {
      if (!seenRowKeys.has(cacheKey)) this._projectRowCache.delete(cacheKey);
    }
    this.container.appendChild(projectsSection);

    const pinState = this.getRenderablePinState();
    if (this.projects.length === 0 && pinState.workspaces.length === 0) {
      this.renderEmptyState({ append: true });
    }
    for (const [cacheKey, cached] of this._projectRowCache) {
      if (!this.projects.some((project) => project.workspaceId === cacheKey)) {
        this._projectRowCache.delete(cacheKey);
      } else if (cached) {
        void cached;
      }
    }

    if (this.searchQuery) this.applySearch();
    this.rebuildStatusIndex();
  }

  renderEmptyState({ append = false } = {}) {
    const empty = document.createElement("div");
    empty.className = "session-empty-state";
    const hint = document.createElement("div");
    hint.className = "session-empty-hint";
    hint.textContent = t("sidebar.emptyRegistryHint");
    empty.appendChild(hint);
    const openButton = document.createElement("button");
    openButton.type = "button";
    openButton.className = "session-empty-open-project";
    const addProjectLabel = t("sidebar.addProject");
    openButton.title = addProjectLabel;
    openButton.setAttribute("aria-label", addProjectLabel);
    openButton.textContent = `+${addProjectLabel}`;
    openButton.addEventListener("click", () => this.onOpenProject?.());
    empty.appendChild(openButton);
    if (append) this.container.appendChild(empty);
    else this.container.replaceChildren(empty);
  }

  getProjectSearchText(project) {
    const path = typeof project?.path === "string" ? project.path : "";
    const dirName = typeof project?.dirName === "string" ? project.dirName : "";
    const shortPath = basenameLocalPath(path) || path;
    return [shortPath, dirName, path].join(" ").toLowerCase();
  }

  formatTime(isoTimestamp) {
    return formatSessionTime(isoTimestamp);
  }
}

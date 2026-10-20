// ABOUTME: Persists ephemeral SPA state across host-origin workspace routes.
// ABOUTME: Without this, a full-page route change resets scroll position,
// ABOUTME: sidebar expansion, input drafts, and the sidebar session list — the
// ABOUTME: user perceives "everything jumps back to initial state".
//
// Storage channel: document.cookie, NOT sessionStorage/localStorage. Cookies
// are shared by the host-origin routes while Web Storage remains origin-bound.

import { LOCAL_HOST, resolveHostFromUrl } from "../app/host-connection.js";

const STATE_COOKIE = "pi-studio:nav-state";
const SIDEBAR_CACHE_COOKIE = "pi-studio:sidebar-cache";
const SIDEBAR_CACHE_SHARD_PREFIX = "pi-studio:sidebar-cache:shard";
// Per-cookie payload cap, comfortably under the ~4KB browser limit.
const COOKIE_MAX_CHARS = 3500;
const MAX_SHARDS = 20;
const MAX_DRAFT_LENGTH = 1500;

/**
 * Sidebar-cache key suffix for this window's host (contract 4: cache keys are
 * host-namespaced, or a remote host's sidebar hydrates the local projection).
 * The local host keeps the bare keys, so cookies written before this change
 * stay valid and nothing local has to migrate.
 */
function sidebarHostSuffix() {
  const host = resolveHostFromUrl();
  return host === LOCAL_HOST ? "" : `-host-${host}`;
}

function readCookie(name) {
  try {
    const prefix = `${name}=`;
    for (const part of document.cookie.split(";")) {
      const trimmed = part.trim();
      if (trimmed.startsWith(prefix)) {
        return decodeURIComponent(trimmed.slice(prefix.length));
      }
    }
  } catch {
    // document.cookie can throw in sandboxed contexts; treat as missing.
  }
  return "";
}

function writeCookie(name, value, maxAgeSeconds) {
  try {
    // biome-ignore lint/suspicious/noDocumentCookie: synchronous write required at boot/navigation; Cookie Store API is async and not suitable here
    document.cookie = `${name}=${encodeURIComponent(value)}; Path=/; Max-Age=${maxAgeSeconds}; SameSite=Lax`;
  } catch {
    // Best-effort; state simply won't survive if cookies are blocked.
  }
}

function deleteCookie(name) {
  try {
    // biome-ignore lint/suspicious/noDocumentCookie: synchronous delete required before/after navigation; Cookie Store API is async
    document.cookie = `${name}=; Path=/; Max-Age=0; SameSite=Lax`;
  } catch {
    // Best-effort.
  }
}

/**
 * Snapshot the ephemeral UI state that a cross-port navigation would
 * otherwise lose. Called right before `window.location.assign()` swaps
 * to a new pi instance, i.e. inside `showSwapOverlay`.
 *
 * @param {object} state
 * @param {number|null} [state.messageScroll]
 * @param {number|null} [state.sidebarScroll]
 * @param {string} [state.inputDraft]
 * @param {Set<string>|string[]} [state.expandedWorkspaces]
 * @param {string} [state.searchQuery]
 */
export function snapshotNavState(state) {
  try {
    const serializable = {
      ts: Date.now(),
      messageScroll: typeof state.messageScroll === "number" ? state.messageScroll : null,
      sidebarScroll: typeof state.sidebarScroll === "number" ? state.sidebarScroll : null,
      // Keep the draft small: a cookie holds ~4KB total, and the draft is the
      // largest variable field.
      inputDraft:
        typeof state.inputDraft === "string" ? state.inputDraft.slice(0, MAX_DRAFT_LENGTH) : "",
      expandedWorkspaces: state.expandedWorkspaces ? Array.from(state.expandedWorkspaces) : [],
      searchQuery: typeof state.searchQuery === "string" ? state.searchQuery : "",
    };
    writeCookie(STATE_COOKIE, JSON.stringify(serializable), 90);
  } catch {
    // Best-effort; state simply won't survive the navigation.
  }
}

/**
 * Read and clear the snapshot taken before the last cross-port navigation.
 * Returns `null` if no snapshot exists or it is stale (> 60s old).
 *
 * The staleness window prevents a stale snapshot from overwriting fresh
 * state on a manual reload that wasn't a workspace swap.
 */
export function consumeNavState(maxAgeMs = 60_000) {
  try {
    const raw = readCookie(STATE_COOKIE);
    if (!raw) return null;
    deleteCookie(STATE_COOKIE);
    const state = JSON.parse(raw);
    if (typeof state.ts !== "number" || Date.now() - state.ts > maxAgeMs) return null;
    return {
      messageScroll: typeof state.messageScroll === "number" ? state.messageScroll : null,
      sidebarScroll: typeof state.sidebarScroll === "number" ? state.sidebarScroll : null,
      inputDraft: typeof state.inputDraft === "string" ? state.inputDraft : "",
      expandedWorkspaces: Array.isArray(state.expandedWorkspaces)
        ? new Set(state.expandedWorkspaces)
        : new Set(),
      searchQuery: typeof state.searchQuery === "string" ? state.searchQuery : "",
    };
  } catch {
    return null;
  }
}

// ── Sidebar cache ────────────────────────────────────────────────────────────
// The resolved project tree is cached so the *next* page boot can render the
// sidebar instantly before the registry list responds. The projection preserves
// every field `SessionSidebar.render()` / `buildSessionItem` reads; sessions
// that lack a filePath (would break row rendering) are dropped, and projects
// without identity/path are dropped too.

function toSessionCache(s) {
  return {
    filePath: typeof s.filePath === "string" ? s.filePath : null,
    name: typeof s.name === "string" ? s.name : "",
    firstMessage: typeof s.firstMessage === "string" ? s.firstMessage : "",
    timestamp: s.timestamp ?? null,
    mtime: typeof s.mtime === "number" ? s.mtime : null,
    ctime: s.ctime ?? null,
    tmux: Boolean(s.tmux),
    parentSession: typeof s.parentSession === "string" ? s.parentSession : null,
  };
}

function toProjectCache(p) {
  return {
    workspaceId: typeof p.workspaceId === "string" ? p.workspaceId : null,
    // The raw DB uuid: hydrated landing rows must refetch sessions with the
    // host-bound id, not the `ws:` display id (which the host rejects with
    // workspace_not_found, leaving the row visibly empty).
    registryId: typeof p.registryId === "string" ? p.registryId : null,
    path: typeof p.path === "string" ? p.path : null,
    folderName: typeof p.folderName === "string" ? p.folderName : null,
    // Registry rows carry null until their first lazy load; keep the null so
    // hydrated rows match live objects instead of drifting to "".
    dirName: typeof p.dirName === "string" ? p.dirName : null,
    pinned: Boolean(p.pinned),
    isProvisional: Boolean(p.isProvisional),
    source: typeof p.source === "string" ? p.source : "",
    activityAt: typeof p.activityAt === "number" ? p.activityAt : 0,
    lastActivityAt: typeof p.lastActivityAt === "number" ? p.lastActivityAt : 0,
    sessions: Array.isArray(p.sessions)
      ? p.sessions
          // The provisional placeholder is per-page UI state keyed by the
          // *runtime* session id, not a file. Caching it made the next boot
          // (the reload after a daemon restart) restore an unselectable row:
          // selecting it routes the bare id as `sessionPath`, which the host
          // cannot canonicalize ("Selected session is unavailable").
          .filter((session) => session?.provisional !== true)
          .map(toSessionCache)
          .filter((s) => typeof s.filePath === "string" && s.filePath.length > 0)
      : [],
  };
}

/**
 * True when a cached project has the identity fields the sidebar needs to
 * render a workspace group and is safe to hand to `sidebar.projects`.
 */
export function isCompleteSidebarProject(project) {
  return Boolean(
    project &&
      typeof project.workspaceId === "string" &&
      project.workspaceId.length > 0 &&
      typeof project.path === "string" &&
      project.path.length > 0 &&
      Array.isArray(project.sessions),
  );
}

function clearSidebarCache() {
  const suffix = sidebarHostSuffix();
  deleteCookie(`${SIDEBAR_CACHE_COOKIE}${suffix}`);
  for (let i = 0; i < MAX_SHARDS; i += 1) {
    deleteCookie(`${SIDEBAR_CACHE_SHARD_PREFIX}:${i}${suffix}`);
  }
}

/**
 * Cache the sidebar's resolved project/session tree so the *next* page boot
 * can render it instantly before the registry list responds. Called after
 * every successful `loadSessions`.
 */
export function cacheSidebarProjects(projects) {
  try {
    if (!Array.isArray(projects)) return;
    const compact = projects.map(toProjectCache).filter(isCompleteSidebarProject);
    const payload = JSON.stringify({ ts: Date.now(), projects: compact });
    // The sidebar tree can exceed one cookie (~4KB); shard across several.
    // If it still does not fit, skip caching — the sidebar just loads the
    // normal way, which is the pre-feature behaviour.
    const shards = [];
    for (let offset = 0; offset < payload.length; offset += COOKIE_MAX_CHARS) {
      shards.push(payload.slice(offset, offset + COOKIE_MAX_CHARS));
    }
    if (shards.length === 0 || shards.length > MAX_SHARDS) {
      clearSidebarCache();
      return;
    }
    const suffix = sidebarHostSuffix();
    clearSidebarCache();
    shards.forEach((shard, index) => {
      writeCookie(`${SIDEBAR_CACHE_SHARD_PREFIX}:${index}${suffix}`, shard, 300);
    });
    writeCookie(
      `${SIDEBAR_CACHE_COOKIE}${suffix}`,
      JSON.stringify({ ts: Date.now(), shards: shards.length }),
      300,
    );
  } catch {
    // Best-effort; sidebar just loads the normal way if caching fails.
  }
}

/**
 * Read the cached sidebar projection. Returns `null` if no cache exists, it
 * is stale (> 2 minutes), or any shard is missing/corrupt. Incomplete
 * projects are filtered out so the caller never hydrates broken rows.
 */
export function readCachedSidebarProjects(maxAgeMs = 120_000) {
  try {
    const suffix = sidebarHostSuffix();
    const metaRaw = readCookie(`${SIDEBAR_CACHE_COOKIE}${suffix}`);
    if (!metaRaw) return null;
    const meta = JSON.parse(metaRaw);
    if (typeof meta.ts !== "number" || Date.now() - meta.ts > maxAgeMs) {
      clearSidebarCache();
      return null;
    }
    if (typeof meta.shards !== "number" || meta.shards < 1) return null;
    let payload = "";
    for (let i = 0; i < meta.shards; i += 1) {
      const shard = readCookie(`${SIDEBAR_CACHE_SHARD_PREFIX}:${i}${suffix}`);
      if (!shard) {
        clearSidebarCache();
        return null;
      }
      payload += shard;
    }
    const data = JSON.parse(payload);
    if (typeof data.ts !== "number" || Date.now() - data.ts > maxAgeMs) {
      clearSidebarCache();
      return null;
    }
    if (!Array.isArray(data.projects)) return null;
    const complete = data.projects.filter(isCompleteSidebarProject);
    return complete.length > 0 ? complete : null;
  } catch {
    return null;
  }
}

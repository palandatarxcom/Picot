// ABOUTME: Per-session composer drafts: bounded identity-keyed store with DB
// ABOUTME: persistence and a synchronous in-memory write-through cache.

/**
 * Composer draft store (C4). A draft belongs to a session identity (see
 * composer-session-identity.js); keys live in a dedicated versioned
 * preference namespace (`composer.draft.v1.<encoded-identity>`), never the
 * global ui.* keys, so the store can list and delete by its own namespace.
 *
 * The in-memory Map is the synchronous read path (rendering never awaits);
 * every mutation writes through to the DB-backed preferences channel when
 * available and stays memory-only otherwise (web/LAN surfaces).
 *
 * Bounds (documented contract): per-draft text length is clamped and the
 * entry count is capped with least-recently-updated eviction, so a long-lived
 * app cannot grow the store without limit.
 */
const DRAFT_KEY_PREFIX = "composer.draft.v1.";
export const DEFAULT_MAX_DRAFT_ENTRIES = 100;
export const DEFAULT_MAX_DRAFT_CHARS = 10000;

export function draftKeyFor(identity) {
  return `${DRAFT_KEY_PREFIX}${encodeURIComponent(String(identity))}`;
}

export function createComposerDraftStore({
  preferences,
  maxEntries = DEFAULT_MAX_DRAFT_ENTRIES,
  maxDraftChars = DEFAULT_MAX_DRAFT_CHARS,
  now = () => Date.now(),
} = {}) {
  const cache = new Map(); // identity -> { text, updatedAt }
  const available = () =>
    Boolean(
      preferences &&
        typeof preferences.available === "function" &&
        preferences.available() &&
        typeof preferences.set === "function",
    );

  /** Seed the cache from the DB namespace. Idempotent: also retried once the
   *  host capability arrives (the first call can run before it, when the DB
   *  channel is unavailable). Entries already in the cache are newer writes
   *  from this session — the retry never clobbers them with stale DB rows. */
  async function loadAll() {
    if (!available() || typeof preferences.list !== "function") return;
    try {
      const stored = await preferences.list(DRAFT_KEY_PREFIX);
      const entries = Object.entries(stored || {});
      entries
        .sort((a, b) => (a[1]?.updatedAt ?? 0) - (b[1]?.updatedAt ?? 0))
        .forEach(([key, value]) => {
          const identity = safeDecode(key);
          if (!identity || cache.has(identity)) return;
          const text = typeof value?.text === "string" ? value.text : "";
          cache.set(identity, { text: clampDraft(text), updatedAt: value?.updatedAt ?? 0 });
        });
      evictOverCapacity();
    } catch (error) {
      console.warn("[draft-store] load failed:", error);
    }
  }

  function safeDecode(key) {
    if (!key.startsWith(DRAFT_KEY_PREFIX)) return null;
    try {
      return decodeURIComponent(key.slice(DRAFT_KEY_PREFIX.length));
    } catch {
      return null;
    }
  }

  function clampDraft(text) {
    return typeof text === "string" ? text.slice(0, maxDraftChars) : "";
  }

  function evictOverCapacity() {
    while (cache.size > maxEntries) {
      let oldestIdentity = null;
      let oldestAt = Number.POSITIVE_INFINITY;
      for (const [identity, entry] of cache) {
        if (entry.updatedAt < oldestAt) {
          oldestAt = entry.updatedAt;
          oldestIdentity = identity;
        }
      }
      if (!oldestIdentity) break;
      cache.delete(oldestIdentity);
      if (available()) void preferences.remove(draftKeyFor(oldestIdentity)).catch(() => {});
    }
  }

  /** Synchronous read for rendering. */
  function get(identity) {
    return cache.get(identity)?.text ?? "";
  }

  /**
   * Write-through save. An empty text deletes the draft. Resolves true when
   * the text is durable (or there is none), false when a reload would lose it:
   * no persistence channel, or a failed write (F-04 switch/quit guard).
   */
  async function save(identity, text) {
    const clamped = clampDraft(text);
    if (!clamped) {
      await clear(identity);
      return true;
    }
    cache.set(identity, { text: clamped, updatedAt: now() });
    evictOverCapacity();
    if (!available()) return false;
    try {
      await preferences.set(draftKeyFor(identity), {
        text: clamped,
        updatedAt: cache.get(identity).updatedAt,
      });
      return true;
    } catch (error) {
      console.warn("[draft-store] persist failed:", error);
      return false;
    }
  }

  async function clear(identity) {
    cache.delete(identity);
    if (!available()) return;
    try {
      await preferences.remove(draftKeyFor(identity));
    } catch (error) {
      console.warn("[draft-store] remove failed:", error);
    }
  }

  /**
   * Remove every draft whose identity belongs to a persisted session file —
   * the session delete/archive cleanup path. Raw file paths match identities
   * of the form `file:<path>`.
   */
  async function clearForSessionFile(rawSessionFile) {
    const identity = `file:${rawSessionFile}`;
    if (cache.has(identity)) await clear(identity);
    // Also sweep any stored key that resolves to this file (defensive:
    // the store may hold a DB row the cache never loaded).
    if (available() && typeof preferences.list === "function") {
      try {
        const stored = await preferences.list(draftKeyFor(identity));
        for (const key of Object.keys(stored || {})) {
          if (safeDecode(key) === identity) await preferences.remove(key);
        }
      } catch {
        /* best-effort sweep */
      }
    }
  }

  return { loadAll, get, save, clear, clearForSessionFile };
}

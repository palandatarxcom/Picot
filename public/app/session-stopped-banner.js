// ABOUTME: B′ in-place stopped-session state — per-session stop banner keyed
// ABOUTME: by session file, cleared when a fresh runtime spawns for it.
import { t } from "../i18n.js";

const BANNER_ID = "session-stopped-banner";

/**
 * Tracks which sessions lost their runtime (stopped or crashed) and paints
 * the "session stopped" banner at the top of the chat area for whichever
 * session the UI is currently viewing. `mark` only records; callers decide
 * when to `sync` (stop event, transcript re-render on navigation back).
 *
 * A null sessionFile keys the "foreground runtime whose file is not yet
 * known" case (stop racing the first snapshot); it pairs with a null
 * activeSessionFile in `isStopped`/`sync` — no real session can collide.
 */
export function createSessionStoppedBanner({ chatEl } = {}) {
  const stopped = new Map(); // sessionFile | null -> "stopped" | "crashed"

  function label(kind) {
    return kind === "crashed" ? t("workspace.sessionEnded") : t("workspace.sessionStopped");
  }

  return {
    mark(sessionFile, kind) {
      if (kind !== "stopped" && kind !== "crashed") return;
      stopped.set(sessionFile ?? null, kind);
    },
    clear(sessionFile) {
      stopped.delete(sessionFile ?? null);
    },
    isStopped(sessionFile) {
      return stopped.has(sessionFile ?? null);
    },
    sync(activeSessionFile) {
      const kind = stopped.get(activeSessionFile ?? null);
      const existing = chatEl?.querySelector(`#${BANNER_ID}`);
      if (!kind) {
        existing?.remove();
        return;
      }
      const banner = existing || document.createElement("div");
      banner.id = BANNER_ID;
      banner.className = "session-stopped-banner";
      banner.dataset.kind = kind;
      banner.textContent = label(kind);
      if (banner.parentElement !== chatEl) chatEl?.prepend(banner);
    },
  };
}

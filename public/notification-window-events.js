// ABOUTME: Multi-window §7 routed-notification listeners: the DOM events the app-side Rust
// executor dispatches into the window it picked (navigate / open).

/**
 * Notification window events (multi-window spec §7, plan T3).
 *
 * The app-side Rust executor is the single decision point for a clicked
 * notification: it picks one window and reaches into it with `window.eval`,
 * which dispatches one of two DOM events on the document. These listeners are
 * the receiving half — passive by construction, they never decide which
 * window moves.
 *
 * - `picot:notification-navigate` (rule 2): the chosen window is bound to the
 *   target workspace — route it to the session through the caller's session
 *   select flow (the sidebar's own path, transitions included).
 * - `picot:notification-open` (rule 3): no window holds the workspace — ride
 *   the production `transport.openWorkspace` chain, whose daemon-side
 *   reuse/focus guards decide spawn vs. focus-existing.
 *
 * Both app windows and the landing wire this: a landing window can be the
 * carrier of the open request even though it never navigates in-window.
 */

import { t } from "./i18n.js";

const NAVIGATE_EVENT = "picot:notification-navigate";
const OPEN_EVENT = "picot:notification-open";

export function setupNotificationWindowEvents({ handleSessionSelect, transport, onNotice }) {
  if (typeof handleSessionSelect === "function") {
    document.addEventListener(NAVIGATE_EVENT, (event) => {
      const { sessionFile, cwd } = event.detail || {};
      if (!sessionFile) return;
      try {
        void handleSessionSelect({ filePath: sessionFile, cwd: cwd || undefined });
      } catch (error) {
        console.error("[Notifications] failed to route to the finished session:", error);
      }
    });
  }

  if (transport && typeof transport.openWorkspace === "function") {
    document.addEventListener(OPEN_EVENT, (event) => {
      const { cwd, sessionFile } = event.detail || {};
      if (!cwd || !sessionFile) return;
      transport
        .openWorkspace(cwd, { sessionPath: sessionFile, openWindow: true })
        .then((result) => {
          if (result?.focusedExisting && typeof onNotice === "function") {
            onNotice(t("workspace.focusedExisting"));
          }
        })
        .catch(() => {
          if (typeof onNotice === "function") {
            onNotice(t("sidebar.openInNewWindowFailed"));
          }
        });
    });
  }
}

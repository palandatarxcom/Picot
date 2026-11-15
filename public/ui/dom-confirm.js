// ABOUTME: Renders a Promise-based modal confirm that works inside the Tauri WebView.
// ABOUTME: window.confirm is silently false there — no native dialog bridge exists,
// ABOUTME: so every confirm-gated flow degrades into a quiet no without this.

import { t } from "../i18n.js";

/**
 * Show a modal confirm and resolve with the user's decision.
 * Reuses the sidebar's confirm-dialog styles (`.sidebar-confirm-*`), settles
 * exactly once, and treats Escape, backdrop clicks, and the cancel button the
 * same way: `false`.
 *
 * @param {string} message Body text (already localized by the caller).
 * @param {object} [options]
 * @param {string} [options.confirmLabel] Confirm button label (defaults to
 *   `actions.confirm`).
 * @param {boolean} [options.danger] Marks the confirm button destructive.
 * @returns {Promise<boolean>}
 */
export function showDomConfirm(message, { confirmLabel = null, danger = false } = {}) {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "sidebar-confirm-overlay dom-confirm-overlay";
    const dialog = document.createElement("div");
    dialog.className = "sidebar-confirm-dialog";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    dialog.setAttribute("aria-label", t("dialogs.confirm"));

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
    if (danger) confirm.classList.add("danger");
    confirm.textContent = confirmLabel || t("dialogs.confirm");
    actions.append(cancel, confirm);
    dialog.append(messageElement, actions);
    overlay.appendChild(dialog);

    let settled = false;
    const cleanup = (result) => {
      if (settled) return;
      settled = true;
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
    cancel.addEventListener("click", () => cleanup(false));
    confirm.addEventListener("click", () => cleanup(true));

    document.addEventListener("keydown", onKeyDown);
    document.body.appendChild(overlay);
    confirm.focus();
  });
}

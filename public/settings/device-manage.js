// ABOUTME: Settings → Daemon → Paired devices: the safe device list and the
// ABOUTME: revoke confirm that drops a device's stored access.

import { t as i18n, onLocaleChange } from "../i18n.js";

/**
 * The two paired-device commands (spec §3.4, plan T9). Same lazy-invoke shape
 * as the daemon section's transport: a plain browser tab must fail at the call,
 * not at import time, and its failures stay silent.
 */
export function tauriDeviceTransport() {
  const call = (command, args) => {
    const host = typeof window === "undefined" ? globalThis : window;
    const invoke = host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke;
    if (typeof invoke !== "function") {
      return Promise.reject(new Error("Tauri IPC is unavailable in this shell"));
    }
    return invoke(command, args);
  };
  return {
    available: () => {
      const host = typeof window === "undefined" ? globalThis : window;
      return (
        typeof (host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke) === "function"
      );
    },
    list: () => call("relay_device_list"),
    revoke: (deviceId) => call("relay_revoke_device", { deviceId }),
  };
}

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

/** `pairedAt` is epoch seconds on the wire (`paired_devices.paired_at`). */
function pairedAtText(seconds, locale) {
  if (typeof seconds !== "number" || !Number.isFinite(seconds)) return "—";
  try {
    return new Date(seconds * 1000).toLocaleString(locale || undefined);
  } catch {
    return "—";
  }
}

/**
 * Wire the paired-devices block. daemon-section drives the page: its poll calls
 * `refresh()` and its leave calls `leave()`.
 *
 * @param {{
 *   root?: Document,
 *   transport?: ReturnType<typeof tauriDeviceTransport>,
 *   t?: (key: string, params?: Record<string, string|number>) => string,
 *   locale?: () => string,
 * }} [options]
 */
export function setupDeviceManage({
  root = document,
  transport = tauriDeviceTransport(),
  t = i18n,
  locale,
} = {}) {
  const list = root.getElementById("device-manage-list");
  const empty = root.getElementById("device-manage-empty");
  const noticeEl = root.getElementById("device-manage-notice");
  const confirmRow = root.getElementById("device-revoke-confirm");
  const confirmText = root.getElementById("device-revoke-confirm-text");
  const confirmBtn = root.getElementById("btn-device-revoke-confirm");
  const cancelBtn = root.getElementById("btn-device-revoke-cancel");
  if (!list || !empty || !noticeEl || !confirmRow || !confirmText || !confirmBtn || !cancelBtn) {
    return { refresh: async () => [], leave: () => {} };
  }

  /** The last list reply, or null while unknown. */
  let devices = null;
  let noticeMsg = null;
  /** Why the last read or action failed; `kind` decides who clears it. */
  let noticeKind = null; // "read" | "action" | null
  /** The device whose revoke awaits the user's answer, or null. */
  let target = null;
  let busy = false;
  let active = false;
  const ipcAvailable = () =>
    typeof transport.available === "function" ? transport.available() !== false : true;

  function setNotice(message, kind) {
    noticeMsg = message || null;
    noticeKind = noticeMsg ? (kind ?? null) : null;
    noticeEl.textContent = noticeMsg ?? "";
    noticeEl.classList.toggle("hidden", !noticeMsg);
  }

  function render() {
    const rows = Array.isArray(devices) ? devices : [];
    list.replaceChildren(
      ...rows.map((device) => {
        const row = root.createElement("div");
        row.className = "device-row";
        row.dataset.deviceId = String(device?.deviceId ?? "");
        const id = root.createElement("span");
        id.className = "settings-label-main device-row-id";
        id.textContent = String(device?.deviceId ?? "—");
        const meta = root.createElement("span");
        meta.className = "settings-label-sub device-row-meta";
        meta.textContent = [
          t("settings.daemon.devices.pairedAt", {
            time: pairedAtText(device?.pairedAt, locale?.()),
          }),
          device?.owner
            ? t("settings.daemon.devices.owner", { owner: String(device.owner) })
            : t("settings.daemon.devices.ownerUnknown"),
        ].join(" · ");
        const revoke = root.createElement("button");
        revoke.type = "button";
        revoke.className = "settings-value-btn is-danger";
        revoke.dataset.revokeId = String(device?.deviceId ?? "");
        revoke.textContent = t("settings.daemon.devices.revoke");
        revoke.disabled = busy;
        row.append(id, meta, revoke);
        return row;
      }),
    );
    empty.classList.toggle("hidden", rows.length > 0);
    confirmRow.classList.toggle("hidden", target === null);
    confirmText.textContent = target
      ? t("settings.daemon.devices.revokeConfirm", { device: target })
      : "";
    confirmBtn.disabled = busy;
    cancelBtn.disabled = busy;
    setNotice(noticeMsg);
  }

  /**
   * Read the list. Never throws: a failed read is a notice and "unknown", never
   * a stale list shown as current. An open confirm keeps its device.
   */
  async function refresh() {
    active = true;
    if (!ipcAvailable()) {
      devices = null;
      render();
      return [];
    }
    try {
      const reply = await transport.list();
      devices = Array.isArray(reply?.devices) ? reply.devices : [];
      if (noticeKind === "read") setNotice(null);
    } catch (error) {
      if (!active) return [];
      devices = null;
      setNotice(t("settings.daemon.devices.readFailed", { reason: errorMessage(error) }), "read");
    }
    render();
    return Array.isArray(devices) ? devices : [];
  }

  /** Revoke one device, then re-read: only fresh data shows what survived. */
  async function revoke(deviceId) {
    if (busy) return;
    busy = true;
    target = null;
    render();
    try {
      await transport.revoke(deviceId);
      setNotice(t("settings.daemon.devices.revoked", { device: deviceId }), "action");
    } catch (error) {
      setNotice(
        t("settings.daemon.devices.revokeFailed", { reason: errorMessage(error) }),
        "action",
      );
    } finally {
      busy = false;
    }
    await refresh();
  }

  const onListClick = (event) => {
    const button = event.target?.closest?.("button[data-revoke-id]");
    if (!button || busy) return;
    target = button.dataset.revokeId;
    render();
  };

  const onConfirm = () => {
    if (!target || busy) return;
    void revoke(target);
  };

  const onCancel = () => {
    if (busy) return;
    target = null;
    render();
  };

  list.addEventListener("click", onListClick);
  confirmBtn.addEventListener("click", onConfirm);
  cancelBtn.addEventListener("click", onCancel);

  // The section is reused on re-entry, so this one subscription stays for the
  // page's lifetime (the pair block follows the same rule).
  onLocaleChange(() => {
    if (active) render();
  });

  return {
    refresh,
    /** Close the block: drop the pending confirm. */
    leave() {
      active = false;
      target = null;
    },
  };
}

// ABOUTME: Settings → General → Daemon → Pair Device (relay Phase 2, spec §3.2): the
// ABOUTME: Paseo-style two-state surface — consent while relay is off, QR + link +
// warning while it is on — wired by daemon-section onto its shared poll.

import { t as i18n, onLocaleChange } from "../i18n.js";
import { createIcon } from "../icons.js";

/**
 * The Tauri IPC transport for the three relay commands (`relay_status` /
 * `relay_set_enabled` / `relay_offer`, plan T6 slice 2). Same lazy-invoke
 * shape as the daemon section's transport: a plain browser tab must fail at
 * the call, not at import time, and its failures stay silent (PM C10②).
 */
export function tauriRelayTransport() {
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
    status: () => call("relay_status"),
    setEnabled: (enabled) => call("relay_set_enabled", { enabled }),
    offer: () => call("relay_offer"),
  };
}

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

/** How long the copy button shows its confirmation before flipping back. */
export const COPY_FEEDBACK_MS = 2000;

/** The transport's connection states, snake_case on the wire (spec §4.1). */
const RELAY_STATES = ["offline", "connecting", "online"];

/**
 * Wire the Pair Device block. daemon-section drives this page: its poll calls
 * `refresh(daemonStatus)` (the status document carries `keepRunningAfterQuit`,
 * the consent's soft-warning input) and its leave closes it.
 *
 * @param {{
 *   root?: Document,
 *   transport?: ReturnType<typeof tauriRelayTransport>,
 *   t?: (key: string, params?: Record<string, string|number>) => string,
 * }} [options]
 */
export function setupPairDeviceSection({
  root = document,
  transport = tauriRelayTransport(),
  t = i18n,
} = {}) {
  const row = root.getElementById("setting-pair-device");
  const openBtn = root.getElementById("btn-pair-device-open");
  const noticeEl = root.getElementById("pair-device-notice");
  const consent = root.getElementById("pair-device-consent");
  const consentTitle = root.getElementById("pair-device-consent-title");
  const consentText = root.getElementById("pair-device-consent-text");
  const keepHint = root.getElementById("pair-device-keep-running-hint");
  const shield = root.getElementById("pair-device-shield");
  const cancelBtn = root.getElementById("btn-pair-device-cancel");
  const enableBtn = root.getElementById("btn-pair-device-enable");
  const offerRow = root.getElementById("pair-device-offer");
  const stateEl = root.getElementById("pair-device-state");
  const qrCanvas = root.getElementById("pair-device-qr");
  const urlInput = root.getElementById("pair-device-url");
  const expiredEl = root.getElementById("pair-device-expired");
  const copyBtn = root.getElementById("btn-pair-device-copy");
  const regenerateBtn = root.getElementById("btn-pair-device-regenerate");
  const disableBtn = root.getElementById("btn-pair-device-disable");
  if (
    !row ||
    !openBtn ||
    !noticeEl ||
    !consent ||
    !consentTitle ||
    !consentText ||
    !keepHint ||
    !cancelBtn ||
    !enableBtn ||
    !offerRow ||
    !stateEl ||
    !qrCanvas ||
    !urlInput ||
    !expiredEl ||
    !copyBtn ||
    !regenerateBtn ||
    !disableBtn
  ) {
    return { activate: async () => {}, leave: () => {}, refresh: async () => {} };
  }

  /** The last `relay_status` document, or null while unknown. */
  let relayStatus = null;
  /** The last minted offer `{url, expiresAt}`, or null while none is live. */
  let offer = null;
  /** The daemon's keepRunningAfterQuit, or null while unknown. */
  let keepRunning = null;
  let consentOpen = false;
  let active = false;
  /** One action in flight (enable/disable/regenerate): clicks are refused. */
  let busy = false;
  /** Why the last read or action failed; `kind` decides who clears it. */
  let noticeMsg = null;
  let noticeKind = null; // "read" | "action" | null
  let copyTimer = null;

  const ipcAvailable = () =>
    typeof transport.available === "function" ? transport.available() !== false : true;

  const shieldIcon = createIcon("shield", { size: 28, document: root });
  if (shieldIcon && shield) shield.replaceChildren(shieldIcon);

  function setNotice(message, kind) {
    noticeMsg = message;
    noticeKind = message ? kind : null;
    if (!message) {
      noticeEl.textContent = "";
      noticeEl.classList.add("hidden");
      return;
    }
    noticeEl.textContent = message;
    noticeEl.classList.remove("hidden");
  }

  function stateLabel(state) {
    return RELAY_STATES.includes(state)
      ? t(`settings.daemon.pairDevice.state.${state}`)
      : String(state ?? "—");
  }

  /**
   * Render the QR from the offer URL (the same vendor call the LAN QR uses).
   * A missing or failing renderer hides the canvas — the read-only link next
   * to it stays the honest fallback — and says so once in the notice.
   */
  async function renderQr(url) {
    if (!url) {
      qrCanvas.classList.add("hidden");
      return;
    }
    try {
      const qr = globalThis.PicotQr;
      if (!qr?.toCanvas) throw new Error("QR renderer unavailable");
      await qr.toCanvas(qrCanvas, url, { width: 200 });
      qrCanvas.classList.remove("hidden");
    } catch {
      qrCanvas.classList.add("hidden");
      setNotice(t("misc.qrUnavailable"), "action");
    }
  }

  /**
   * Spec §2.10: `expiresAt` is epoch seconds and `now == expiresAt` is still
   * valid on the daemon side; the ms comparison only ever errs one second
   * early, never shows a dead code as live.
   */
  function offerExpired() {
    return (
      offer !== null && typeof offer.expiresAt === "number" && Date.now() > offer.expiresAt * 1000
    );
  }

  function render() {
    const enabled = relayStatus?.enabled === true;
    const expired = enabled && offerExpired();
    row.hidden = enabled;
    offerRow.hidden = !enabled;
    // The offer state replaces the consent wholesale, open or not.
    consent.hidden = enabled || !consentOpen;
    if (enabled) {
      stateEl.textContent = `${stateLabel(relayStatus?.state)} · ${relayStatus?.endpoint ?? "—"}`;
      urlInput.value = offer?.url ?? "";
      // An expired offer is a dead code (spec §2.10): hide the QR, keep the
      // URL row with the expired marker — Regenerate is the way out.
      expiredEl.hidden = !expired;
      if (expired) {
        qrCanvas.classList.add("hidden");
      } else {
        void renderQr(offer?.url ?? null);
      }
    } else {
      expiredEl.hidden = true;
      consentTitle.textContent = t("settings.daemon.pairDevice.enableTitle");
      consentText.textContent = t("settings.daemon.pairDevice.enableDescription", {
        endpoint: relayStatus?.endpoint ?? "—",
      });
      keepHint.textContent = t("settings.daemon.pairDevice.keepRunningHint");
      // The warning only asserts what is known: unknown policy stays quiet.
      keepHint.hidden = keepRunning !== false;
    }
    setNotice(noticeMsg, noticeKind);
  }

  /**
   * Mint (or re-mint) an offer. Each call supersedes the previous unconsumed
   * token (D12), so this runs on the enable transition and on Regenerate —
   * never on the 5s poll, whose job is only the status line.
   */
  async function loadOffer() {
    const fresh = await transport.offer();
    if (!fresh || typeof fresh !== "object" || typeof fresh.url !== "string" || !fresh.url) {
      throw new Error("the daemon returned no offer URL");
    }
    offer = fresh;
  }

  /**
   * Read the relay state. `daemonStatus` is the daemon section's status
   * document when it has one (the consent warning reads keepRunningAfterQuit
   * from it). Never throws: a failed read is a notice, and both the status
   * and the offer are dropped rather than shown as current.
   */
  async function refresh(daemonStatus = null) {
    active = true;
    if (daemonStatus && typeof daemonStatus === "object") {
      keepRunning = daemonStatus.keepRunningAfterQuit === true;
    }
    try {
      const fresh = await transport.status();
      relayStatus = fresh && typeof fresh === "object" ? fresh : null;
      if (noticeKind === "read") setNotice(null);
      if (relayStatus?.enabled !== true) offer = null;
      if (relayStatus?.enabled === true && !offer) {
        try {
          await loadOffer();
        } catch (error) {
          if (ipcAvailable()) {
            setNotice(
              t("settings.daemon.pairDevice.offerFailed", {
                reason: errorMessage(error),
              }),
              "action",
            );
          }
        }
      }
    } catch (error) {
      relayStatus = null;
      offer = null;
      if (ipcAvailable()) {
        setNotice(
          t("settings.daemon.pairDevice.readFailed", { reason: errorMessage(error) }),
          "read",
        );
      }
    }
    render();
    return relayStatus;
  }

  openBtn.addEventListener("click", () => {
    consentOpen = true;
    render();
  });
  cancelBtn.addEventListener("click", () => {
    consentOpen = false;
    render();
  });

  enableBtn.addEventListener("click", async () => {
    if (busy) return;
    busy = true;
    enableBtn.disabled = true;
    try {
      await transport.setEnabled(true);
      consentOpen = false;
      await refresh();
    } catch (error) {
      if (ipcAvailable()) {
        setNotice(
          t("settings.daemon.pairDevice.enableFailed", { reason: errorMessage(error) }),
          "action",
        );
      }
      render();
    } finally {
      busy = false;
      enableBtn.disabled = false;
    }
  });

  copyBtn.addEventListener("click", async () => {
    if (!offer?.url) return;
    try {
      await navigator.clipboard?.writeText?.(offer.url);
      copyBtn.textContent = t("settings.daemon.pairDevice.copied");
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copyBtn.textContent = t("settings.daemon.pairDevice.copy");
      }, COPY_FEEDBACK_MS);
    } catch {
      // Clipboard denied: the read-only link stays selectable, that is the
      // fallback — no error notice for a copy the user can do by hand.
    }
  });

  regenerateBtn.addEventListener("click", async () => {
    if (busy) return;
    busy = true;
    try {
      await loadOffer();
      // The previous code died with this mint (D12): say so, once.
      setNotice(t("settings.daemon.pairDevice.regenerateNotice"), "action");
    } catch (error) {
      if (ipcAvailable()) {
        setNotice(
          t("settings.daemon.pairDevice.offerFailed", { reason: errorMessage(error) }),
          "action",
        );
      }
    } finally {
      busy = false;
      render();
    }
  });

  disableBtn.addEventListener("click", async () => {
    if (busy) return;
    busy = true;
    try {
      await transport.setEnabled(false);
      setNotice(null);
      consentOpen = false;
      await refresh();
    } catch (error) {
      if (ipcAvailable()) {
        setNotice(
          t("settings.daemon.pairDevice.disableFailed", { reason: errorMessage(error) }),
          "action",
        );
      }
      render();
    } finally {
      busy = false;
    }
  });

  // Every dynamic string is rebuilt on a locale change; the relay facts and
  // the offer URL are locale-free, so nothing is re-fetched.
  const unsubscribe = onLocaleChange(() => {
    if (!active) return;
    render();
  });

  return {
    /** Open the block: read once (daemon-section's poll re-reads from here). */
    async activate(daemonStatus = null) {
      active = true;
      consentOpen = false;
      await refresh(daemonStatus);
    },
    /** Close the block: drop the pending consent and the copy feedback. */
    leave() {
      active = false;
      consentOpen = false;
      clearTimeout(copyTimer);
      copyBtn.textContent = t("settings.daemon.pairDevice.copy");
      unsubscribe();
    },
    refresh,
  };
}

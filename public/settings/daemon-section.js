// ABOUTME: Settings → General → Daemon section: the daemon's status row, the app-private
// ABOUTME: keepRunningAfterQuit toggle, and the restart transaction's confirm flow.

import { t as i18n, onLocaleChange } from "../i18n.js";

/**
 * How often the status row refreshes while the Settings panel is open. The
 * daemon can die or be replaced without telling this page (plan D12/T9), so the
 * row is a poll, not an event subscription.
 */
export const POLL_INTERVAL_MS = 5000;

/**
 * The Tauri IPC transport: `daemon_status` / `daemon_set_keep_running` /
 * `daemon_restart` are the app's three commands (plan D8/T10).
 *
 * `withGlobalTauri` puts `__TAURI__.core.invoke` on the WebView global, so no
 * npm dependency is needed (the same path the rest of the shell uses). The
 * lookup is lazy per call: the WebView global is not guaranteed at module-eval
 * time, and a shell without IPC (a plain browser) must fail visibly at the call
 * instead of at import time.
 */
export function tauriDaemonTransport() {
  const call = (command, args) => {
    const host = typeof window === "undefined" ? globalThis : window;
    const invoke = host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke;
    if (typeof invoke !== "function") {
      return Promise.reject(new Error("Tauri IPC is unavailable in this shell"));
    }
    return invoke(command, args);
  };
  return {
    status: () => call("daemon_status"),
    setKeepRunning: (value) => call("daemon_set_keep_running", { value }),
    restart: () => call("daemon_restart"),
  };
}

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

/**
 * Wire the Daemon section.
 *
 * @param {{
 *   root?: Document,
 *   transport?: ReturnType<typeof tauriDaemonTransport>,
 *   t?: (key: string, params?: Record<string, string|number>) => string,
 * }} [options]
 */
export function setupDaemonSection({
  root = document,
  transport = tauriDaemonTransport(),
  t = i18n,
} = {}) {
  const section = root.getElementById("setting-daemon-section");
  const statusValue = root.getElementById("setting-daemon-status-value");
  const noticeEl = root.getElementById("daemon-section-notice");
  const toggle = root.getElementById("toggle-daemon-keep-running");
  const note = root.getElementById("daemon-keep-running-note");
  const restartBtn = root.getElementById("btn-daemon-restart");
  const confirmRow = root.getElementById("daemon-restart-confirm");
  const agentsLabel = root.getElementById("daemon-restart-agents-label");
  const agentsList = root.getElementById("daemon-restart-agents");
  const confirmBtn = root.getElementById("btn-daemon-restart-confirm");
  const cancelBtn = root.getElementById("btn-daemon-restart-cancel");
  if (!section || !statusValue || !toggle || !restartBtn || !confirmRow) {
    return {
      activate: async () => {},
      leave: () => {},
      refresh: async () => null,
    };
  }

  /** The last status document, or `null` while unknown. */
  let status = null;
  /** Why the last *read* failed, if it did: cleared by the next good read. */
  let readError = null;
  /** Why the last explicit action failed, if it did: cleared by that action
   *  succeeding (a routine poll read never clears it — the user has to see it). */
  let actionError = null;
  /** Whether the sub-copy currently shows the post-save explanation. */
  let noteSaved = false;
  /** Whether the panel is open: polling runs only then. */
  let active = false;
  /** Whether a restart transaction is in flight on this page. */
  let busy = false;
  /** Whether the confirm block is awaiting the user's decision. */
  let confirmOpen = false;
  let pollTimer = null;

  function setNotice(message) {
    if (!message) {
      noticeEl.textContent = "";
      noticeEl.classList.add("hidden");
      return;
    }
    noticeEl.textContent = message;
    noticeEl.classList.remove("hidden");
  }

  /** The line under the section title: the policy description, or the "saved" copy. */
  function setNote(saved = false) {
    note.textContent = saved
      ? t("settings.daemon.keepRunningSaved")
      : t("settings.daemon.keepRunningDescription");
  }

  function renderToggle(on) {
    toggle.classList.toggle("on", on);
    toggle.setAttribute("aria-pressed", String(on));
  }

  function renderStatus() {
    if (!status || typeof status !== "object") {
      statusValue.textContent = t("settings.daemon.unknown");
      statusValue.removeAttribute("data-tone");
      return;
    }
    const running = status.running === true;
    const version = running && status.version ? ` · v${status.version}` : "";
    statusValue.textContent = `${t(running ? "settings.daemon.running" : "settings.daemon.stopped")}${version}`;
    statusValue.dataset.tone = running ? "ok" : "warn";
    renderToggle(status.keepRunningAfterQuit === true);
  }

  function render() {
    renderStatus();
    setNote(noteSaved);
    // Precedence: what the user just tried, then why the read failed, then the
    // daemon's own reason for being down.
    const statusNotice =
      status && status.running === false && status.reason
        ? t("settings.daemon.stoppedReason", { reason: String(status.reason) })
        : null;
    setNotice(actionError || readError || statusNotice);
  }

  /**
   * Ask the app for the current state. Never throws: a failed read is a notice
   * plus "unavailable", and the last known status is dropped rather than shown
   * as if it were current.
   */
  async function refresh() {
    if (busy) return status;
    try {
      const fresh = await transport.status();
      status = fresh && typeof fresh === "object" ? fresh : null;
      readError = null;
    } catch (error) {
      status = null;
      readError = t("settings.daemon.readFailed", { reason: errorMessage(error) });
    }
    render();
    return status;
  }

  function stopPolling() {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  function closeConfirm() {
    confirmOpen = false;
    confirmRow.hidden = true;
    agentsList.replaceChildren();
    agentsLabel.textContent = "";
  }

  /** One list row per live runtime: its workspace root and its session id. */
  function agentRow(agent) {
    const li = root.createElement("li");
    const workspace = agent?.cwd || agent?.workspaceId || "—";
    const session = agent?.sessionId ? String(agent.sessionId) : "";
    li.textContent = session ? `${workspace} · ${session}` : String(workspace);
    return li;
  }

  async function openConfirm() {
    restartBtn.disabled = true;
    const fresh = await refresh();
    restartBtn.disabled = false;
    if (!fresh) {
      // The agent list is what the user is approving; without it the confirm
      // would be a blind "stop everything". The notice already says why.
      return;
    }
    const agents = Array.isArray(fresh.agents) ? fresh.agents : [];
    agentsLabel.textContent = agents.length
      ? t("settings.daemon.restartAgentCount", { count: agents.length })
      : t("settings.daemon.restartNoAgents");
    agentsList.replaceChildren(...agents.map(agentRow));
    confirmRowText();
    confirmOpen = true;
    confirmRow.hidden = false;
    confirmBtn.disabled = false;
    cancelBtn.disabled = false;
    confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
  }

  /** The confirm's own sentence, normalized so markup whitespace never shows. */
  function confirmRowText() {
    const text = root.getElementById("daemon-restart-confirm-text");
    if (text) text.textContent = t("settings.daemon.restartConfirm");
  }

  /**
   * Run the T9 restart transaction. The app migrates every window (this one
   * included) to the new daemon's origin, so in the shipped app a success
   * usually ends with this page reloaded at the same route — the status refresh
   * below is for the paths where it is not.
   */
  async function restart() {
    busy = true;
    confirmBtn.disabled = true;
    cancelBtn.disabled = true;
    confirmBtn.textContent = t("settings.daemon.restarting");
    try {
      await transport.restart();
      actionError = null;
      closeConfirm();
      status = null;
      // The transaction is finished: stop suppressing reads, then pick up the
      // replacement's identity (a new pid/version), which is the point of the
      // status refresh (plan T9 step 7).
      busy = false;
      await refresh();
    } catch (error) {
      // The transaction aborted: the window is still here, and this is the only
      // place the failure is visible.
      actionError = t("settings.daemon.restartFailed", { reason: errorMessage(error) });
      render();
      confirmBtn.disabled = false;
      cancelBtn.disabled = false;
      confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
    } finally {
      busy = false;
    }
  }

  toggle.addEventListener("click", async () => {
    if (toggle.disabled) return;
    const next = !toggle.classList.contains("on");
    // Optimistic: the write is the whole operation, and a failure rolls back.
    renderToggle(next);
    setNote(true);
    try {
      await transport.setKeepRunning(next);
      actionError = null;
      noteSaved = true;
    } catch (error) {
      renderToggle(!next);
      setNote(false);
      actionError = t("settings.daemon.saveFailed", { reason: errorMessage(error) });
      render();
    }
  });

  restartBtn.addEventListener("click", () => {
    if (busy || confirmOpen) return;
    void openConfirm();
  });
  confirmBtn.addEventListener("click", () => {
    if (busy) return;
    void restart();
  });
  cancelBtn.addEventListener("click", () => {
    if (busy) return;
    closeConfirm();
  });

  // Every dynamic string is rebuilt on a locale change; the status values
  // themselves are locale-free, so nothing is re-fetched.
  const unsubscribe = onLocaleChange(() => {
    if (!active) return;
    confirmRowText();
    render();
    if (confirmOpen) {
      agentsLabel.textContent = agentsList.children.length
        ? t("settings.daemon.restartAgentCount", { count: agentsList.children.length })
        : t("settings.daemon.restartNoAgents");
      confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
      cancelBtn.textContent = t("settings.daemon.restartCancel");
    }
  });

  return {
    /** Open the section: fetch once, then poll while it stays open. */
    async activate() {
      active = true;
      actionError = null;
      readError = null;
      noteSaved = false;
      closeConfirm();
      confirmRowText();
      await refresh();
      stopPolling();
      pollTimer = setInterval(() => {
        void refresh();
      }, POLL_INTERVAL_MS);
    },
    /** Close the section: stop polling and drop the pending confirm. */
    leave() {
      active = false;
      stopPolling();
      closeConfirm();
      unsubscribe();
    },
    refresh,
  };
}

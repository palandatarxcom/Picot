// ABOUTME: Settings → Daemon section: the daemon's status row, the app-private
// ABOUTME: keepRunningAfterQuit toggle, the restart transaction's confirm flow, and the
// ABOUTME: permanent running-agents list with its per-runtime stop.

import { t as i18n, onLocaleChange } from "../i18n.js";
import { bindDialogEscape } from "../ui/dialog-escape.js";
import { runtimeKey, setupDaemonRuntimes } from "./daemon-runtimes.js";
import { setupPairDeviceSection } from "./pair-device-section.js";

/**
 * How often the status row refreshes while the Settings panel is open. The
 * daemon can die or be replaced without telling this page (plan D12/T9), so the
 * row is a poll, not an event subscription.
 */
export const POLL_INTERVAL_MS = 5000;

/**
 * The Tauri IPC transport: `daemon_status` / `daemon_set_keep_running` /
 * `daemon_restart` / `daemon_agent_stop` are the app's four commands (plan
 * D8/T10 and B2).
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
    /**
     * Whether this shell can reach the app-level commands at all. A plain
     * browser tab has no Tauri IPC and therefore no daemon relationship: its
     * failures are not the user's to act on, so the section stays quiet
     * instead of leaking a Tauri-internal error into the page (PM finding
     * C10②).
     */
    available: () => {
      const host = typeof window === "undefined" ? globalThis : window;
      return (
        typeof (host.__TAURI__?.core?.invoke ?? globalThis.__TAURI__?.core?.invoke) === "function"
      );
    },
    status: () => call("daemon_status"),
    setKeepRunning: (value) => call("daemon_set_keep_running", { value }),
    restart: () => call("daemon_restart"),
    // The one-runtime stop (plan B2). The triple is forwarded verbatim: the
    // daemon refuses anything it cannot identify exactly.
    stopAgent: (args) => call("daemon_agent_stop", args),
  };
}

function errorMessage(error) {
  return String(error?.message || error || "unknown error");
}

/**
 * The coordinator's eight runtime states, snake_case on the wire (admin.rs
 * `RuntimeState`). The restart confirm shows the daemon's own state per row, so
 * an operator sees a mid-turn agent as `working`, not just a boolean.
 */
const AGENT_STATES = [
  "starting",
  "trusting",
  "ready",
  "working",
  "idle",
  "suspended",
  "crashed",
  "stopped",
];

/**
 * Wire the Daemon section.
 *
 * @param {{
 *   root?: Document,
 *   transport?: ReturnType<typeof tauriDaemonTransport>,
 *   loadWorkspaceSessions?: (workspaceId: string) => Promise<{sessions: object[]}>,
 *   t?: (key: string, params?: Record<string, string|number>) => string,
 * }} [options]
 */
export function setupDaemonSection({
  root = document,
  transport = tauriDaemonTransport(),
  loadWorkspaceSessions,
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
  const confirmBtn = root.getElementById("btn-daemon-restart-confirm");
  const cancelBtn = root.getElementById("btn-daemon-restart-cancel");
  const agentsBlock = root.getElementById("daemon-agents");
  const agentsEmpty = root.getElementById("daemon-agents-empty");
  const stopConfirmRow = root.getElementById("daemon-stop-confirm");
  const stopConfirmTarget = root.getElementById("daemon-stop-confirm-target");
  const stopConfirmBtn = root.getElementById("btn-daemon-stop-confirm");
  const stopCancelBtn = root.getElementById("btn-daemon-stop-cancel");
  if (
    !section ||
    !statusValue ||
    !toggle ||
    !restartBtn ||
    !confirmRow ||
    !agentsBlock ||
    !agentsEmpty ||
    !stopConfirmRow ||
    !stopConfirmTarget ||
    !stopConfirmBtn ||
    !stopCancelBtn
  ) {
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
  /** Whether this shell has the app-level IPC (a plain browser tab has not). */
  const ipcAvailable = () =>
    typeof transport.available === "function" ? transport.available() !== false : true;
  /** Whether the panel is open: polling runs only then. */
  let active = false;
  /** Whether a restart transaction is in flight on this page. */
  let busy = false;
  /** Whether the confirm block is awaiting the user's decision. */
  let confirmOpen = false;
  /** The Escape binding of the open confirm modal, if any. */
  let unbindEscape = null;
  /** The running-agent count the open confirm is speaking about. */
  let confirmCount = 0;
  /** The working agent whose Stop is awaiting the user's decision, if any. */
  let stopTarget = null;
  /** The instance id of the agent whose stop is in flight, if any. */
  let stoppingId = null;
  let pollTimer = null;
  let refreshRound = 0;
  let activation = 0;
  const runtimes = setupDaemonRuntimes({
    root,
    t,
    stateLabel: agentStateLabel,
    onStop: requestStop,
    onSelect: closeStopConfirm,
    loadWorkspaceSessions,
  });

  // The Pair Device block lives in this same section (relay Phase 2, spec
  // §3.2): its lifecycle and poll are this section's, so its reads ride
  // refreshAll and its leave rides this section's leave.
  const pairDevice = setupPairDeviceSection({ root, t });

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
    renderAgents();
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
    const round = ++refreshRound;
    try {
      const fresh = await transport.status();
      if (!active || round !== refreshRound) return null;
      status = fresh && typeof fresh === "object" ? fresh : null;
      readError = null;
    } catch (error) {
      if (!active || round !== refreshRound) return null;
      status = null;
      // A shell without the app-level IPC (a plain browser tab) has no daemon
      // to read: it stays “unavailable” and silent rather than telling the
      // user that Tauri internals are missing (PM finding C10②).
      readError = ipcAvailable()
        ? t("settings.daemon.readFailed", { reason: errorMessage(error) })
        : null;
    }
    render();
    runtimes.refreshNames();
    return status;
  }

  /**
   * One poll tick: the daemon row first, then the pair block with the fresh
   * status document (its consent warning reads keepRunningAfterQuit from it).
   */
  async function refreshAll() {
    const visit = activation;
    const fresh = await refresh();
    if (!active || visit !== activation) return;
    await pairDevice.refresh(fresh);
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
    confirmCount = 0;
    agentsLabel.textContent = "";
    unbindEscape?.();
    unbindEscape = null;
  }

  /** A row's identity line: workspace root (or id) and session id. */
  function agentLabel(agent) {
    const workspace = agent?.cwd || agent?.workspaceId || "—";
    const session = agent?.sessionId ? String(agent.sessionId) : "";
    return session ? `${workspace} · ${session}` : String(workspace);
  }

  /** The instance id used to display the in-flight stop. */
  const agentKey = (agent) => (agent?.instanceId == null ? null : String(agent.instanceId));

  /**
   * The permanent list. Rebuilt from the last status document, so a poll (5s)
   * and a locale change both land here; the empty-state node is the markup's,
   * kept last so it is never duplicated.
   */
  function renderAgents() {
    const agents = status && Array.isArray(status.agents) ? status.agents : [];
    if (stopTarget && !agents.some((agent) => runtimeKey(agent) === runtimeKey(stopTarget)))
      closeStopConfirm();
    runtimes.render(agents, { busy, stoppingId });
    renderStopConfirm();
  }

  /** A row's Stop click: a working agent asks first, anything else stops now. */
  function requestStop(agent) {
    if (busy || stoppingId !== null) return;
    if (agent?.state === "working") {
      stopTarget = agent;
      renderStopConfirm();
      return;
    }
    void stopAgent(agent);
  }

  function renderStopConfirm() {
    stopConfirmRow.hidden = stopTarget === null;
    stopConfirmTarget.textContent = stopTarget ? agentLabel(stopTarget) : "";
    stopConfirmBtn.disabled = busy || stoppingId !== null;
    stopCancelBtn.disabled = busy || stoppingId !== null;
  }

  function closeStopConfirm() {
    stopTarget = null;
    renderStopConfirm();
  }

  /**
   * Stop one exact runtime, then re-read the truth: only a fresh status shows
   * what actually survived, so the row is never updated optimistically.
   */
  async function stopAgent(agent) {
    stoppingId = agentKey(agent);
    stopTarget = null;
    renderStopConfirm();
    renderAgents();
    try {
      await transport.stopAgent({
        workspaceId: agent?.workspaceId,
        sessionId: agent?.sessionId,
        instanceId: agent?.instanceId,
      });
      actionError = null;
    } catch (error) {
      actionError = ipcAvailable()
        ? t("settings.daemon.stopFailed", { reason: errorMessage(error) })
        : null;
    } finally {
      stoppingId = null;
    }
    await refresh();
  }

  /**
   * A row's state copy. A state the coordinator no longer reports (`null`) has
   * no label of its own: the row shows the placeholder rather than a fabricated
   * one.
   */
  function agentStateLabel(state) {
    return AGENT_STATES.includes(state) ? t(`settings.daemon.agentState.${state}`) : "—";
  }

  async function openConfirm() {
    const visit = activation;
    restartBtn.disabled = true;
    const fresh = await refresh();
    restartBtn.disabled = false;
    if (!active || visit !== activation) return;
    if (!fresh) {
      // The count is what the user is approving; without it the confirm would
      // be a blind "stop everything". The notice already says why.
      return;
    }
    const agents = Array.isArray(fresh.agents) ? fresh.agents : [];
    confirmCount = agents.length;
    agentsLabel.textContent = agents.length
      ? t("settings.daemon.restartAgentCount", { count: agents.length })
      : t("settings.daemon.restartNoAgents");
    confirmRowText();
    confirmOpen = true;
    confirmRow.hidden = false;
    // Escape is the modal's keyboard cancel, with the same busy guard as the
    // cancel button: the transaction in flight must not be dismissed.
    unbindEscape = bindDialogEscape(
      () => {
        if (busy) return;
        closeConfirm();
      },
      { isActive: () => confirmOpen },
    );
    confirmBtn.disabled = false;
    cancelBtn.disabled = false;
    confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
    cancelBtn.focus();
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
    // Every row's Stop is disabled for the duration of the transaction.
    renderAgents();
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
      // place the failure is visible — except in a shell with no IPC at all,
      // where there was never a restart to attempt (PM finding C10②).
      actionError = ipcAvailable()
        ? t("settings.daemon.restartFailed", { reason: errorMessage(error) })
        : null;
      render();
      confirmBtn.disabled = false;
      cancelBtn.disabled = false;
      confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
    } finally {
      busy = false;
      renderAgents();
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
      actionError = ipcAvailable()
        ? t("settings.daemon.saveFailed", { reason: errorMessage(error) })
        : null;
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
  stopConfirmBtn.addEventListener("click", () => {
    if (busy || stopTarget === null) return;
    void stopAgent(stopTarget);
  });
  stopCancelBtn.addEventListener("click", () => {
    if (busy) return;
    closeStopConfirm();
  });

  // Every dynamic string is rebuilt on a locale change; the status values
  // themselves are locale-free, so nothing is re-fetched.
  onLocaleChange(() => {
    if (!active) return;
    confirmRowText();
    render();
    if (confirmOpen) {
      agentsLabel.textContent = confirmCount
        ? t("settings.daemon.restartAgentCount", { count: confirmCount })
        : t("settings.daemon.restartNoAgents");
      confirmBtn.textContent = t("settings.daemon.restartConfirmAction");
      cancelBtn.textContent = t("settings.daemon.restartCancel");
    }
  });

  return {
    /** Open the section: fetch once, then poll while it stays open. */
    async activate() {
      const visit = ++activation;
      active = true;
      actionError = null;
      readError = null;
      noteSaved = false;
      closeConfirm();
      closeStopConfirm();
      confirmRowText();
      await refreshAll();
      if (!active || visit !== activation) return;
      stopPolling();
      pollTimer = setInterval(() => {
        void refreshAll();
      }, POLL_INTERVAL_MS);
    },
    /** Close the section: stop polling and drop the pending confirm. */
    leave() {
      active = false;
      activation++;
      refreshRound++;
      runtimes.invalidate();
      pairDevice.leave();
      stopPolling();
      closeConfirm();
      closeStopConfirm();
      // The section is reused on re-entry; keep its one locale subscription.
    },
    refresh,
  };
}

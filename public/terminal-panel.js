// ABOUTME: Native-owner-only Terminal Panel: DOM, tab bar, collapse/expand,
// ABOUTME: height clamp, workspace checkpoint/reattach, and close-risk participant.
// ABOUTME: Remote/LAN/mobile clients render no terminal surface at all.

import { t } from "./i18n.js";
import { createIcon, setButtonIcon } from "./icons.js";

const MIN_HEIGHT_PX = 160;
const DEFAULT_HEIGHT_RATIO = 0.3;
const MAX_HEIGHT_RATIO = 0.7;

export function formatTerminalStartError(message) {
  if (typeof message === "string" && message.includes("Git for Windows")) {
    return t("terminal.gitBashMissing");
  }
  return message || t("terminal.statusFailed");
}

/**
 * TerminalPanel owns the panel DOM, tab bar, collapse/expand behavior, height
 * clamping, and its participation in workspace transition + window close.
 *
 * It depends on a thin `client` facade (create/close/restart/checkpointAll/...)
 * that wraps TerminalClient, plus a locale subscription and an available-height
 * probe. It never holds owner/root/port/capability — those stay host-owned.
 */
export class TerminalPanel {
  constructor({ native, client, subscribeLocale, getAvailableHeight } = {}) {
    this.native = Boolean(native);
    this.client = client;
    this.getAvailableHeight = getAvailableHeight || (() => 800);
    this.expanded = false;
    this.heightPx = null;
    /** Ordered tab metadata: { terminalId, generation, label, profileId, status } */
    this.tabs = [];
    this.activeTerminalId = null;
    this.locked = false;
    /**
     * Who holds the interaction lock. The panel stays frozen while any holder
     * is present, so a workspace transition coming back up cannot unfreeze a
     * window-close transaction that is still settling.
     */
    this.lockHolders = new Set();
    /** Remote attachment availability: anything but "ready" blocks new tabs. */
    this.availability = "ready";
    this.unavailable = false;
    this.availabilityRetry = null;
    /** Availability copy retained so a later mount can render it (plan §4). */
    this.availabilityMessage = "";
    this.availabilityRetryLabel = "";
    this.activityByTab = new Set();
    this.tabContainers = new Map();
    this.tabButtons = new Map();
    this._restartNoticeShown = false;
    this._dragRefitTimer = null;
    this.enlarged = false;
    this.toggleEl = null;
    this.root = null;
    this.tabBarEl = null;
    this.bodyEl = null;
    this.resizeObserver = null;
    this.headerResizeObserver = null;
    this.unsubscribeLocale = null;
    if (subscribeLocale) {
      this.unsubscribeLocale = subscribeLocale(() => this.applyLocale());
    }
  }

  /** Build the toggle + panel DOM. A non-native client renders nothing. */
  mount({ toggleContainer, panelContainer }) {
    if (!this.native || !toggleContainer || !panelContainer) {
      return;
    }
    this.toggleEl = document.createElement("button");
    this.toggleEl.type = "button";
    this.toggleEl.className = "terminal-toggle panel-toggle-btn";
    this.toggleEl.dataset.terminalToggle = "";
    this.toggleEl.setAttribute("aria-pressed", "false");
    this.toggleEl.setAttribute("aria-label", t("terminal.toggle"));
    this.toggleEl.setAttribute("title", t("terminal.toggle"));
    this.toggleEl.appendChild(createIcon("terminal", { size: 16 }));
    this.toggleEl.addEventListener("click", () => this.toggle());

    this.root = document.createElement("section");
    this.root.className = "terminal-panel hidden";
    this.root.dataset.terminalPanel = "";
    this.root.setAttribute("aria-label", "Terminal");

    const resizer = document.createElement("div");
    resizer.className = "terminal-resizer";
    resizer.setAttribute("role", "separator");
    resizer.setAttribute("aria-orientation", "horizontal");
    resizer.tabIndex = 0;
    resizer.setAttribute("aria-label", t("terminal.ariaResizer"));
    resizer.addEventListener("pointerdown", (event) => this._beginResize(event));
    resizer.addEventListener("keydown", (event) => this._keyboardResize(event));

    this.tabBarEl = document.createElement("div");
    this.tabBarEl.className = "terminal-tab-bar";
    this.tabBarEl.setAttribute("role", "tablist");

    this.bodyEl = document.createElement("div");
    this.bodyEl.className = "terminal-body";
    if (typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.client?.refitAll?.());
      this.resizeObserver.observe(this.bodyEl);
    }

    // A horizontal bar, never an X: this only hides the panel — tabs and their
    // PTYs survive. The tab-level X and the window-close dialog are the real
    // closes, so the panel control must not look like one of them.
    const collapseButton = document.createElement("button");
    collapseButton.type = "button";
    collapseButton.className = "terminal-collapse";
    collapseButton.dataset.terminalCollapse = "";
    collapseButton.title = t("terminal.collapse");
    collapseButton.setAttribute("aria-label", t("terminal.collapse"));
    setButtonIcon(collapseButton, "minus", { size: 16 });
    collapseButton.addEventListener("click", () => this.collapse());

    this.enlargeButton = document.createElement("button");
    this.enlargeButton.type = "button";
    this.enlargeButton.className = "terminal-enlarge";
    this.enlargeButton.dataset.terminalEnlarge = "";
    this.enlargeButton.title = t("terminal.enlarge");
    this.enlargeButton.setAttribute("aria-label", t("terminal.enlarge"));
    this.enlargeButton.appendChild(createIcon("maximize", { size: 14 }));
    this.enlargeButton.addEventListener("click", () => this.toggleEnlarge());

    const newTabButton = document.createElement("button");
    newTabButton.type = "button";
    newTabButton.className = "terminal-new-tab";
    newTabButton.dataset.terminalNewTab = "";
    newTabButton.appendChild(createIcon("plus", { size: 16 }));
    newTabButton.title = t("terminal.newTab");
    newTabButton.addEventListener("click", () => {
      if (!this.locked && !this.unavailable) this.client?.create?.("default");
    });
    this.tabBarEl.append(newTabButton, this.enlargeButton, collapseButton);
    this.root.append(resizer, this.tabBarEl, this.bodyEl);
    toggleContainer.appendChild(this.toggleEl);
    panelContainer.appendChild(this.root);
    this._watchHeaderHeight(panelContainer);
    this.applyLocale();
    // The attachment state can land before the panel exists (a Remote page
    // resolves capability during startup), so re-render what was stored.
    this._renderAvailability();
  }

  _watchHeaderHeight(panelContainer) {
    const header = panelContainer.querySelector?.(".header");
    if (!header || !this.root) return;
    const sync = () => {
      const height = header.getBoundingClientRect?.().height || header.offsetHeight || 0;
      this.root.style.setProperty("--terminal-header-offset", `${Math.ceil(height)}px`);
    };
    sync();
    if (typeof ResizeObserver !== "undefined") {
      this.headerResizeObserver = new ResizeObserver(sync);
      this.headerResizeObserver.observe(header);
    }
  }

  isExpanded() {
    return this.expanded;
  }

  toggle() {
    if (this.expanded) {
      this.collapse();
    } else {
      void this.expand();
    }
  }

  /** Expand the panel. The first native expansion lazily creates one default tab. */
  async expand() {
    if (!this.native || this.expanded || this.locked) {
      return;
    }
    this.expanded = true;
    this.root.classList.remove("hidden");
    this.toggleEl.setAttribute("aria-pressed", "true");
    this.clearActivity();
    if (this.unavailable) {
      // Read-only status: the panel explains why the terminal is blocked and
      // offers its one recovery action. No PTY is created here (D2), and the
      // message must be reachable by clicking the toggle like any other panel.
      this.layoutHeight();
      this._renderAvailability();
      return;
    }
    const restored = this.tabs.filter((tab) => tab.status === "restoredMetadata");
    if (restored.length > 0) {
      // Fresh app run with persisted metadata: re-create one shell per saved
      // tab. Live PTYs/checkpoints are never restored (design contract).
      this._showRestartNotice();
      for (const tab of restored) {
        await this.client?.create?.(tab.profileId || "default");
      }
    } else if (this.tabs.length === 0) {
      await this.client?.create?.("default");
    }
    this.layoutHeight();
    // xterm opened into a hidden container renders at zero size; refit every
    // tab now that the panel body is visible.
    this.client?.refitAll?.();
  }

  collapse() {
    this.expanded = false;
    this.enlarged = false;
    this.enlargeButton?.classList.remove("enlarged");
    this.root?.classList.remove("enlarged");
    this.root?.classList.add("hidden");
    this.toggleEl?.setAttribute("aria-pressed", "false");
    this._updateToggleAffordance();
  }

  /** Toggle the panel between its default height and covering the whole workspace. */
  toggleEnlarge() {
    this.enlarged = !this.enlarged;
    if (this.enlarged) {
      this.root?.classList.add("enlarged");
      if (this.root) this.root.style.height = "";
    } else {
      this.root?.classList.remove("enlarged");
      const available = this.getAvailableHeight() || 800;
      this.setHeight(Math.round(available * DEFAULT_HEIGHT_RATIO));
    }
    this.enlargeButton?.classList.toggle("enlarged", this.enlarged);
    const labelKey = this.enlarged ? "terminal.restore" : "terminal.enlarge";
    const iconName = this.enlarged ? "minimize" : "maximize";
    this.enlargeButton?.setAttribute("aria-label", t(labelKey));
    if (this.enlargeButton) {
      this.enlargeButton.title = t(labelKey);
      setButtonIcon(this.enlargeButton, iconName, { size: 14 });
    }
    this.client?.refitAll?.();
  }

  /** Clamp a requested pixel height to [MIN_HEIGHT_PX, 70% of available]. */
  setHeight(px) {
    const available = this.getAvailableHeight() || 800;
    const max = Math.round(available * MAX_HEIGHT_RATIO);
    const clamped = Math.min(max, Math.max(MIN_HEIGHT_PX, Math.round(px)));
    this.heightPx = clamped;
    if (this.root) {
      this.root.style.height = `${clamped}px`;
    }
    this.client?.refitAll?.();
    return clamped;
  }

  // ---- Workspace transition participant ----

  /** Take or release one lock source: the panel stays locked while any holds it. */
  setLockHolder(holder, held) {
    if (held) this.lockHolders.add(holder);
    else this.lockHolders.delete(holder);
    this.locked = this.lockHolders.size > 0;
  }

  /** Freeze interaction and checkpoint running tabs before a host commit. */
  async beforeWorkspaceTransition() {
    this.setLockHolder("transition", true);
    // Checkpoint regardless of expanded state: a collapsed panel still has
    // background PTYs whose screen must be recoverable after the reload.
    try {
      await this.client?.checkpointAll?.();
    } catch (error) {
      this.setLockHolder("transition", false);
      throw error;
    }
    return true;
  }

  /**
   * The host commit succeeded and this page keeps its binding: give back the
   * transition lock. Only the transition's own hold is released — a close
   * transaction keeps the panel frozen until it settles.
   */
  finishWorkspaceTransition() {
    this.setLockHolder("transition", false);
  }

  cancelWorkspaceTransition() {
    this.setLockHolder("transition", false);
  }

  // ---- Window close participant ----

  /** Report live terminals for the single close summary dialog. */
  getCloseRisk() {
    const live = this.tabs.filter((tab) => tab.status === "running" || tab.status === "creating");
    return {
      terminalTabs: live.map((tab) => ({
        terminalId: tab.terminalId,
        label: tab.label,
      })),
    };
  }

  /** On approved discard, close every live terminal; on cancel, leave them. */
  async settleCloseRisk(decision) {
    if (decision === "discard") {
      this.setLockHolder("close", true);
      try {
        await this.client?.closeAll?.();
      } catch {
        // Best-effort cleanup; the host remains the final authority.
      }
    } else {
      this.setLockHolder("close", false);
    }
  }

  /** The window-close coordinator's lock: separate holder from a transition. */
  setInteractionLocked(locked) {
    this.setLockHolder("close", Boolean(locked));
  }

  /**
   * Surface the Remote attachment state (plan 2026-11-03 §4). Anything other
   * than `ready` blocks new tabs and explains why; `onRetry` renders the one
   * explicit recovery action the state allows (never an automatic retry loop).
   */
  setAvailability(state, { message = "", onRetry = null, retryLabel = "" } = {}) {
    this.availability = state;
    this.unavailable = state !== "ready";
    this.availabilityRetry = onRetry;
    this.availabilityMessage = message;
    this.availabilityRetryLabel = retryLabel;
    this._renderAvailability();
  }

  /** Render the stored availability state into the panel body (if mounted). */
  _renderAvailability() {
    this.bodyEl?.querySelector("[data-terminal-availability]")?.remove();
    if (this.availability === "ready" || !this.bodyEl || !this.availabilityMessage) return;
    const el = document.createElement("div");
    el.className = "terminal-start-error";
    el.dataset.terminalAvailability = this.availability;
    el.setAttribute("role", "status");
    const text = document.createElement("span");
    text.textContent = this.availabilityMessage;
    el.appendChild(text);
    if (this.availabilityRetry) {
      const retry = document.createElement("button");
      retry.type = "button";
      retry.dataset.terminalRetry = "";
      // Blocked bindings need "Reload" (re-navigate the page); a stopped runtime
      // needs "Retry" (re-query). One action per state, named for what it does.
      retry.textContent = this.availabilityRetryLabel || t("terminal.retry");
      retry.addEventListener("click", () => this.availabilityRetry?.());
      el.appendChild(retry);
    }
    this.bodyEl.prepend(el);
  }

  // ---- Tab metadata (host events update these) ----

  /** Replace tab metadata from a terminal_listed response. */
  setTabs(tabs) {
    this.tabs = Array.isArray(tabs) ? tabs : [];
    if (this.activeTerminalId && !this.tabs.some((t) => t.terminalId === this.activeTerminalId)) {
      this.activeTerminalId = this.tabs[0]?.terminalId ?? null;
    } else if (!this.activeTerminalId) {
      this.activeTerminalId = this.tabs[0]?.terminalId ?? null;
    }
    this._renderTabBar();
    this._updateToggleAffordance();
    const failed = this.tabs.find((tab) => tab.status === "failed" && tab.failReason);
    if (failed) {
      this.showStartError(formatTerminalStartError(failed.failReason));
    } else {
      this.clearStartError();
    }
    if (this.tabs.length === 0 && this.expanded) {
      this.collapse();
    }
  }

  /**
   * Drop the local projection because the binding it described is gone (plan §4,
   * B3). Sends nothing: the daemon keeps its PTYs, and a later authoritative
   * list re-projects whatever the host still owns. The availability banner is a
   * separate element and stays visible.
   */
  clearProjection() {
    this.tabs = [];
    this.activeTerminalId = null;
    this.activityByTab.clear();
    this._renderTabBar();
    this._updateToggleAffordance();
    this.clearStartError();
  }

  /** Surface a host-side start failure inside the open panel. */
  showStartError(message) {
    this.clearStartError();
    if (!this.bodyEl || !message) return;
    const el = document.createElement("div");
    el.className = "terminal-start-error";
    el.dataset.terminalStartError = "";
    el.setAttribute("role", "alert");
    el.textContent = message;
    this.bodyEl.prepend(el);
  }

  clearStartError() {
    this.bodyEl?.querySelector("[data-terminal-start-error]")?.remove();
  }

  /** Record background output for a tab (cleared when the user views the panel). */
  markActivity(terminalId) {
    if (terminalId) this.activityByTab.add(terminalId);
    this._updateToggleAffordance();
  }

  clearActivity(terminalId = null) {
    if (terminalId) {
      this.activityByTab.delete(terminalId);
    } else {
      this.activityByTab.clear();
    }
    this._updateToggleAffordance();
  }

  /** Sidebar projection: live terminal count + whether any has background output. */
  getProjection() {
    return {
      count: this.tabs.length,
      hasActivity: this.activityByTab.size > 0,
    };
  }

  /** Keep the toolbar toggle icon-only; terminal counts stay out of the toolbar. */
  _updateToggleAffordance() {
    if (!this.toggleEl) return;
    this.toggleEl.classList.remove("has-activity");
    delete this.toggleEl.dataset.terminalCount;
  }

  _renderTabBar() {
    if (!this.tabBarEl) return;
    for (const btn of this.tabButtons.values()) btn.parentElement?.remove();
    this.tabButtons.clear();
    const newTabBtn = this.tabBarEl.querySelector("[data-terminal-new-tab]");
    for (const tab of this.tabs) {
      const item = document.createElement("div");
      item.className = "terminal-tab-item";
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "terminal-tab";
      btn.dataset.terminalId = tab.terminalId;
      btn.setAttribute("role", "tab");
      const active = tab.terminalId === this.activeTerminalId;
      btn.classList.toggle("active", active);
      item.classList.toggle("active", active);
      btn.setAttribute("aria-selected", active ? "true" : "false");
      btn.tabIndex = active ? 0 : -1;
      btn.addEventListener("keydown", (event) => this._tabKeydown(event, tab.terminalId));
      const label = document.createElement("span");
      label.className = "terminal-tab-label";
      label.textContent = tab.label || tab.terminalId;
      btn.appendChild(label);
      const close = document.createElement("button");
      close.type = "button";
      close.className = "terminal-tab-close";
      setButtonIcon(close, "x", { size: 12 });
      close.setAttribute("aria-label", "Close terminal tab");
      close.addEventListener("click", (event) => {
        event.stopPropagation();
        const live = tab.status === "running" || tab.status === "creating";
        if (live && !window.confirm(`Close ${tab.label || "terminal"}?`)) return;
        this.client?.close?.(tab.terminalId, tab.generation);
      });
      const restart = document.createElement("button");
      restart.type = "button";
      restart.className = "terminal-tab-restart";
      setButtonIcon(restart, "rotate-cw", { size: 12 });
      restart.setAttribute("aria-label", t("terminal.retry"));
      restart.title = t("terminal.retry");
      restart.addEventListener("click", (event) => {
        event.stopPropagation();
        this.client?.restart?.(tab.terminalId, tab.generation);
      });
      btn.addEventListener("click", () => this.setActiveTerminalId(tab.terminalId));
      item.append(btn, close, restart);
      this.tabButtons.set(tab.terminalId, btn);
      if (newTabBtn) {
        this.tabBarEl.insertBefore(item, newTabBtn);
      } else {
        this.tabBarEl.appendChild(item);
      }
    }
    this._applyActiveContainer();
  }

  setActiveTerminalId(terminalId) {
    this.activeTerminalId = terminalId;
    this.clearActivity(terminalId);
    if (!this.tabButtons.has(terminalId)) return;
    for (const [id, btn] of this.tabButtons) {
      const active = id === terminalId;
      btn.classList.toggle("active", active);
      btn.setAttribute("aria-selected", active ? "true" : "false");
      // Mirror active onto the wrapping item so CSS can target close/restart
      // via a robust descendant selector instead of a fragile sibling one.
      btn.parentElement?.classList.toggle("active", active);
    }
    this._applyActiveContainer();
    // The newly-shown xterm must refit to its container and take focus so the
    // user's keystrokes reach it.
    this.client?.refitTab?.(terminalId);
    this.client?.focusTab?.(terminalId);
  }

  /** Per-tab xterm container; created lazily inside the panel body. */
  getTabContainer(terminalId) {
    if (!this.bodyEl) return null;
    let el = this.tabContainers.get(terminalId);
    if (!el) {
      el = document.createElement("div");
      el.className = "terminal-tab-container hidden";
      el.dataset.terminalId = terminalId;
      this.bodyEl.appendChild(el);
      this.tabContainers.set(terminalId, el);
    }
    return el;
  }

  _applyActiveContainer() {
    const live = new Set(this.tabs.map((t) => t.terminalId));
    for (const id of [...this.tabContainers.keys()]) {
      if (!live.has(id)) {
        this.tabContainers.get(id)?.remove();
        this.tabContainers.delete(id);
      }
    }
    for (const [id, el] of this.tabContainers) {
      el.classList.toggle("hidden", id !== this.activeTerminalId);
    }
  }

  destroy() {
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    this.headerResizeObserver?.disconnect();
    this.headerResizeObserver = null;
    this.unsubscribeLocale?.();
    this.unsubscribeLocale = null;
    for (const btn of this.tabButtons.values()) btn.parentElement?.remove();
    for (const el of this.tabContainers.values()) el.remove();
    this.tabButtons.clear();
    this.tabContainers.clear();
    this.toggleEl?.remove();
    this.root?.remove();
    this.toggleEl = null;
    this.root = null;
    this.tabBarEl = null;
    this.bodyEl = null;
  }

  layoutHeight() {
    if (this.heightPx == null) {
      const available = this.getAvailableHeight() || 800;
      this.setHeight(Math.round(available * DEFAULT_HEIGHT_RATIO));
    } else {
      this.setHeight(this.heightPx);
    }
  }

  _showRestartNotice() {
    if (!this.bodyEl || this._restartNoticeShown) {
      return;
    }
    this._restartNoticeShown = true;
    const notice = document.createElement("div");
    notice.className = "terminal-restart-notice";
    notice.textContent = t("terminal.restartNotice");
    this.bodyEl.prepend(notice);
    // Auto-dismiss so it doesn't outlive the session that produced it.
    setTimeout(() => notice.remove(), 8000);
  }

  _beginResize(event) {
    if (this.locked) return;
    event.preventDefault();
    const startY = event.clientY;
    const startHeight = this.heightPx || this.root?.clientHeight || 400;
    const move = (e) => {
      const dy = startY - e.clientY;
      this.setHeight(startHeight + dy);
      this._scheduleDragRefit();
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (this._dragRefitTimer) {
        clearTimeout(this._dragRefitTimer);
        this._dragRefitTimer = null;
      }
      this.client?.refitAll?.();
      this.client?.setPanelHeight?.(this.heightPx);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  _scheduleDragRefit() {
    if (this._dragRefitTimer) return;
    this._dragRefitTimer = setTimeout(() => {
      this._dragRefitTimer = null;
      this.client?.refitAll?.();
    }, 100);
  }

  _keyboardResize(event) {
    if (this.locked) return;
    const step = event.shiftKey ? 80 : 20;
    const current = this.heightPx || 400;
    if (event.key === "ArrowUp") {
      this.setHeight(current + step);
      event.preventDefault();
    } else if (event.key === "ArrowDown") {
      this.setHeight(current - step);
      event.preventDefault();
    }
  }

  _tabKeydown(event, terminalId) {
    const ids = this.tabs.map((tab) => tab.terminalId);
    const idx = ids.indexOf(terminalId);
    if (idx === -1) return;
    let next = null;
    if (event.key === "ArrowRight") next = ids[(idx + 1) % ids.length];
    else if (event.key === "ArrowLeft") next = ids[(idx - 1 + ids.length) % ids.length];
    else if (event.key === "Home") next = ids[0];
    else if (event.key === "End") next = ids[ids.length - 1];
    if (next) {
      this.setActiveTerminalId(next);
      this.tabButtons.get(next)?.focus();
      event.preventDefault();
    }
  }

  applyLocale() {
    // Re-render so i18n-driven titles/labels follow a live locale switch.
    const toggleLabel = t("terminal.toggle");
    this.toggleEl?.setAttribute("aria-label", toggleLabel);
    this.toggleEl?.setAttribute("title", toggleLabel);
    this._renderTabBar();
    this._renderAvailability();
  }
}

// ABOUTME: Info right-side panel — session file path with a copy action plus a
// ABOUTME: scrollable Pi session tree that synchronizes with chat anchors.

/**
 * Info panel — the right-side panel from the 2026-08-21 design.
 *
 * Two independent sections:
 * 1. **Session file** (fixed, never scrolls away): the active session's jsonl
 *    path with a copy action.
 * 2. **Session history** (own vertical scroll): the session tree projected by
 *    `session-tree.js` from Pi's authoritative entries + leafId.
 *
 * Interaction contract (design):
 * - Clicking an ACTIVE node scrolls the main chat to the rendered message
 *   (`[data-entry-id]` anchor); it never changes leaf or composer.
 * - Inactive nodes are non-navigable divs (no fake buttons, not focusable).
 * - Inactive branches collapse at their fork point; the summary row is a real
 *   expand/collapse button; only full-tree leaves expose "Resume branch"
 *   (real button, keyboard-reachable, hover-revealed).
 */

import { createIcon } from "./icons.js";
import { buildSessionTree } from "./session-tree.js";
import { displayLocalPath, displayNativePath } from "./workspace/path-utils.js";

// Live appends keep the panel's cache current turn to turn, but entries can
// be persisted without a message_end enrichment (compaction summaries,
// enrichment failures). Calibrate by age: once the cache has gone this long
export class InfoPanel {
  /**
   * @param {{
   *   panel: HTMLElement,
   *   t: (key: string, params?: object) => string,
   *   onNavigateLeaf: (entryId: string) => void,
   *   onSelectEntry: (entryId: string) => void,
   *   isStreaming: () => boolean,
   * }} options
   */
  constructor({
    panel,
    t,
    onNavigateLeaf,
    onSelectEntry,
    isStreaming,
    ensureEntryMounted = null,
    taskAnalysis = null,
  }) {
    this.panel = panel;
    this.t = t;
    this.onNavigateLeaf = onNavigateLeaf || (() => {});
    this.onSelectEntry = onSelectEntry || (() => {});
    this.isStreaming = isStreaming || (() => false);
    this.taskAnalysis = taskAnalysis;
    // P2 seam: reveals a gate-folded turn before anchor lookup (app.js).
    this.ensureEntryMounted = ensureEntryMounted;
    this.sessionFile = "";
    this.expandedBranches = new Set();
    this.tree = null;
    // Authoritative Pi entries + leafId cache. Full snapshots (pi's live
    // get_entries / the disk data plane) replace it wholesale; every refresh
    // re-fetches — there is no incremental append.
    this.entries = null;
    this.leafId = null;
    this.selectedEntryId = null;
    this._pendingSelectedScroll = false;
    this._copiedTimer = null;
    this._buildDom();
  }

  _buildDom() {
    const p = this.panel;
    p.classList.add("info-panel");
    p.setAttribute("aria-label", this.t("infoPanel.title"));
    p.replaceChildren();

    this.sessionSection = document.createElement("section");
    this.sessionSection.className = "info-panel-session";

    this.historySection = document.createElement("section");
    this.historySection.className = "info-panel-history";
    this.historySection.setAttribute("aria-labelledby", "info-panel-history-heading");

    p.append(
      this.sessionSection,
      ...[this.taskAnalysis?.element].filter(Boolean),
      this.historySection,
    );
    this._renderSessionFile();
    this._renderHistory();
  }

  // ── Session file ────────────────────────────────────────────────────────

  _renderSessionFile() {
    const t = this.t;
    const section = this.sessionSection;
    section.replaceChildren();

    section.setAttribute("aria-label", t("infoPanel.title"));
    this.pathEl = document.createElement("div");
    // Same surface as the file sidebar's path (font, colour, head ellipsis):
    // one path style across panels.
    this.pathEl.className = "file-sidebar-path info-panel-path";
    this.pathEl.id = "info-panel-session-file";
    this.pathEl.textContent = displayLocalPath(this.sessionFile) || "—";
    this.pathEl.title = displayNativePath(this.sessionFile);

    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "icon-btn info-panel-copy";
    copy.setAttribute("aria-label", t("infoPanel.copyPath"));
    copy.title = t("infoPanel.copyPath");
    copy.append(createIcon("copy", { size: 14 }));
    copy.addEventListener("click", () => void this._copySessionFile(copy));

    section.append(this.pathEl, copy);
  }

  async _copySessionFile(button) {
    const text = this.sessionFile;
    if (!text) return;
    const defaultLabel = this.t("infoPanel.copyPath");
    let label = defaultLabel;
    try {
      await navigator.clipboard.writeText(text);
      label = this.t("infoPanel.copied");
    } catch {
      // No clipboard (jsdom, denied permission): say so instead of pretending
      // the path was copied.
      label = this.t("infoPanel.copyFailed");
    }
    button.title = label;
    button.setAttribute("aria-label", label);
    clearTimeout(this._copiedTimer);
    this._copiedTimer = setTimeout(() => {
      button.title = defaultLabel;
      button.setAttribute("aria-label", defaultLabel);
    }, 1200);
  }

  /** Update the active session's jsonl path. */
  updateSessionFile(path) {
    this.sessionFile = path || "";
    if (!this.pathEl) return;
    this.pathEl.textContent = displayLocalPath(this.sessionFile) || "—";
    this.pathEl.title = displayNativePath(this.sessionFile);
  }

  // ── Session history ─────────────────────────────────────────────────────

  _renderHistory() {
    const t = this.t;
    const section = this.historySection;
    section.replaceChildren();

    const heading = document.createElement("div");
    heading.className = "info-panel-history-heading";
    const h3 = document.createElement("h3");
    h3.className = "info-panel-title";
    h3.id = "info-panel-history-heading";
    h3.dataset.i18n = "infoPanel.sessionHistory";
    h3.textContent = t("infoPanel.sessionHistory");
    const status = document.createElement("span");
    status.className = "info-panel-status";
    status.textContent = `● ${t("infoPanel.activePath")}`;
    heading.append(h3, status);

    this.treeScroll = document.createElement("div");
    this.treeScroll.className = "info-panel-tree-scroll";
    this.treeScroll.setAttribute("role", "tree");
    this.treeScroll.setAttribute("aria-labelledby", "info-panel-history-heading");

    section.append(heading, this.treeScroll);
    this._renderTree();
  }

  /** Replace the tree data (Pi-authoritative snapshot) and re-render. */
  updateTree({ entries, leafId } = {}) {
    this.entries = Array.isArray(entries) ? entries : [];
    this.leafId = leafId ?? null;
    this._rebuildTree();
  }

  _rebuildTree() {
    this.tree = buildSessionTree({ entries: this.entries, leafId: this.leafId });
    // Prune expand-state for branches that no longer exist.
    const alive = new Set();
    const collect = (rows) => {
      for (const row of rows) {
        if (row.kind === "branch") {
          alive.add(row.entryId);
          collect(row.rows);
        }
      }
    };
    collect(this.tree.rows);
    for (const id of [...this.expandedBranches]) {
      if (!alive.has(id)) this.expandedBranches.delete(id);
    }
    this._renderTree();
    if (this._pendingSelectedScroll && this.selectedEntryId) {
      this._pendingSelectedScroll = false;
      this.scrollToSelectedEntry();
    }
  }

  _renderTree() {
    if (!this.treeScroll) return;
    this.treeScroll.replaceChildren();
    const rows = this.tree?.rows ?? [];
    if (rows.length === 0) {
      const empty = document.createElement("p");
      empty.className = "info-panel-tree-empty";
      empty.textContent = this.t("infoPanel.empty");
      this.treeScroll.append(empty);
      return;
    }
    for (const row of rows) {
      this.treeScroll.append(
        row.kind === "branch" ? this._renderBranch(row) : this._nodeRowEl(row, true),
      );
    }
  }

  /**
   * A node row. `topLevel` marks rows rendered directly in the active-path
   * list (vs rows inside a collapsed branch's expanded group).
   */
  _nodeRowEl(row, topLevel = false) {
    const t = this.t;
    const el = document.createElement(row.isActive ? "button" : "div");
    el.className = "info-panel-row";
    if (topLevel) el.classList.add("info-panel-row-top");
    el.dataset.entryId = row.entryId;
    el.style.setProperty("--row-depth", String(row.depth));
    if (row.isActive) {
      el.type = "button";
      el.classList.add("active");
      // Same tree semantics as inactive rows: every row under the role=tree
      // container identifies as a treeitem; the aria-label carries role +
      // preview text.
      el.setAttribute("role", "treeitem");
      el.setAttribute(
        "aria-label",
        `${roleName(t, row)}: ${row.previewText || previewFallback(t, row)}`,
      );
      el.addEventListener("click", (event) => {
        if (event.target.closest(".info-panel-resume")) return;
        this._scrollToMessage(row.entryId);
      });
    } else {
      el.classList.add("inactive");
      el.setAttribute("role", "treeitem");
      el.setAttribute("aria-disabled", "true");
    }
    if (this.selectedEntryId === row.entryId) el.classList.add("selected");
    if (row.isCurrentLeaf) {
      el.classList.add("current-leaf");
      // The marker class is visual-only; expose the current leaf to assistive
      // technology as well.
      el.setAttribute("aria-current", "true");
    }

    const role = document.createElement("span");
    role.className = `info-panel-role ${row.role === "user" ? "user" : "assistant"}`;
    role.textContent = row.role === "user" ? "●" : "✦";
    role.setAttribute("aria-hidden", "true");

    const preview = document.createElement("span");
    preview.className = "info-panel-preview";
    preview.textContent = row.previewText || previewFallback(t, row);

    el.append(role, preview);

    // Resume affordance: only on full-tree leaves of an inactive branch.
    if (!row.isActive && row.isFullLeaf) {
      const resume = document.createElement("button");
      resume.type = "button";
      resume.className = "info-panel-resume";
      resume.textContent = t("infoPanel.resumeBranch");
      resume.setAttribute(
        "aria-label",
        `${t("infoPanel.resumeBranch")}: ${row.previewText || previewFallback(t, row)}`,
      );
      if (this.isStreaming()) {
        resume.disabled = true;
        resume.title = t("infoPanel.resumeStreamingBlocked");
      }
      resume.addEventListener("click", (event) => {
        event.stopPropagation();
        if (!resume.disabled) this.onNavigateLeaf(row.entryId);
      });
      el.append(resume);
    }
    return el;
  }

  /** Collapsed inactive branch: summary toggle + (expanded) subtree rows. */
  _renderBranch(branch) {
    const t = this.t;
    const wrap = document.createElement("div");
    wrap.className = "info-panel-branch";
    wrap.dataset.branchId = branch.entryId;

    const expanded = this.expandedBranches.has(branch.entryId);
    const summary = document.createElement("button");
    summary.type = "button";
    summary.className = "info-panel-branch-summary";
    summary.style.setProperty("--row-depth", String(branch.depth));
    summary.setAttribute("aria-expanded", String(expanded));
    summary.setAttribute(
      "aria-label",
      `${t("infoPanel.branch")} · ${t("infoPanel.turns", { count: branch.turnCount })}`,
    );
    const caret = document.createElement("span");
    caret.className = "info-panel-caret";
    caret.textContent = expanded ? "▾" : "▸";
    caret.setAttribute("aria-hidden", "true");
    const text = document.createElement("span");
    text.className = "info-panel-preview";
    text.textContent = `${t("infoPanel.branch")} · ${t("infoPanel.turns", { count: branch.turnCount })}`;
    summary.append(caret, text);
    summary.addEventListener("click", () => {
      if (this.expandedBranches.has(branch.entryId)) {
        this.expandedBranches.delete(branch.entryId);
      } else {
        this.expandedBranches.add(branch.entryId);
      }
      this._renderTree();
    });
    wrap.append(summary);

    if (expanded) {
      const group = document.createElement("div");
      group.className = "info-panel-branch-group";
      group.setAttribute("role", "group");
      for (const row of branch.rows) {
        group.append(row.kind === "branch" ? this._renderBranch(row) : this._nodeRowEl(row));
      }
      wrap.append(group);
    }
    return wrap;
  }

  selectEntry(entryId) {
    this.selectedEntryId = entryId == null ? null : String(entryId);
    const rows = this.treeScroll?.querySelectorAll(".info-panel-row") || [];
    for (const row of rows) {
      row.classList.toggle("selected", row.dataset.entryId === this.selectedEntryId);
    }
    // A live selection suppresses the current-leaf marker's background: two
    // simultaneous accent tints read as two selections. The selected
    // current-leaf row keeps its (stronger) selected highlight via :not().
    this.treeScroll?.classList.toggle("has-selection", this.selectedEntryId != null);
  }

  scrollToSelectedEntry() {
    if (!this.selectedEntryId || this.panel.classList.contains("hidden")) {
      this._pendingSelectedScroll = Boolean(this.selectedEntryId);
      return;
    }
    const row = this.treeScroll?.querySelector(
      `[data-entry-id="${escapeAttribute(this.selectedEntryId)}"]`,
    );
    if (!row) {
      this._pendingSelectedScroll = true;
      return;
    }
    this._pendingSelectedScroll = false;
    row.scrollIntoView({ behavior: "smooth", block: "center" });
  }

  _scrollToMessage(entryId) {
    this.selectEntry(entryId);
    // Attribute-safe escape (Pi entry ids are generated hex, but stay robust
    // for any id without depending on CSS.escape availability).
    const findAnchor = (id) => {
      const safeId = escapeAttribute(id);
      return [...document.querySelectorAll(`[data-entry-id="${safeId}"]`)].find(
        (candidate) => !this.panel.contains(candidate),
      );
    };
    let target = findAnchor(entryId);
    if (!target && typeof this.ensureEntryMounted === "function" && !this._revealAttempted) {
      // The history fold gate (P2) may hold this turn unmounted; reveal it
      // through the app seam, then locate the anchor it just mounted. The
      // one-shot flag keeps the retry from recursing when no anchor exists
      // even after the reveal (the parent walk handles that case).
      this._revealAttempted = true;
      Promise.resolve(this.ensureEntryMounted(entryId))
        .catch(() => {})
        .then(() => {
          this._revealAttempted = false;
          this._scrollToMessage(entryId);
        });
      return;
    }
    if (!target) {
      // Folded rows — error-retry assistants on the active path — have no
      // anchor of their own (the transcript anchors one row per turn).
      // Spec (2026-08-21-info-panel-design.md 交互表): a row without an exact
      // message anchor still “滚动主聊天” via its nearest rendered ancestor
      // — the turn that contains it. Inactive-branch rows never reach here:
      // per the same spec they carry no click-to-scroll at all.
      let parentId = this.entries?.find((e) => e?.id === entryId)?.parentId ?? null;
      while (parentId && !target) {
        target = findAnchor(parentId);
        if (!target) {
          parentId = this.entries?.find((e) => e?.id === parentId)?.parentId ?? null;
        }
      }
    }
    // Diagnostic: distinguishes "no anchor anywhere in transcript" from
    // "found but scroll failed" in one console paste.
    if (!target) {
      console.warn("[InfoPanel] no transcript anchor for entry", entryId);
      return;
    }
    target.scrollIntoView({ behavior: "smooth", block: "center" });
    target.classList.add("info-panel-flash");
    target.addEventListener("animationend", () => target.classList.remove("info-panel-flash"), {
      once: true,
    });
  }
}

// Local helpers — kept module-private.

function roleName(t, row) {
  return row.role === "user" ? t("infoPanel.roleUser") : t("infoPanel.roleAssistant");
}

function escapeAttribute(value) {
  return String(value).replace(/["\\]/g, "\\$&");
}

function previewFallback(t, row) {
  if (row.statusOnly) {
    return row.stopReason === "aborted" ? t("infoPanel.statusAborted") : t("infoPanel.statusError");
  }
  if (row.hasImages) return t("infoPanel.imageMessage");
  return "";
}

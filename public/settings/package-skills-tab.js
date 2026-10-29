// ABOUTME: Renders the Packages skills tab under Settings > Skills.
// ABOUTME: Shows bundled skill candidates from configured Pi packages. Switches are disabled placeholders.

import { onLocaleChange, t } from "../i18n.js";
import { createIcon } from "../icons.js";
import { normalizeLocalPath } from "../workspace/path-utils.js";
import { renderGroupStatusControl } from "./skills-group-status-control.js";

/**
 * @typedef {Object} PackageSkillCandidate
 * @property {string} id
 * @property {string} canonicalPath
 * @property {string} relativePath
 * @property {string} name
 * @property {string} description
 * @property {boolean} enabled
 * @property {Array<{path?:string;message:string}>} diagnostics
 *
 * @typedef {Object} PackageSkillCard
 * @property {string} id
 * @property {string} source
 * @property {string} identity
 * @property {"global"|"project"} scope
 * @property {string|undefined} effectivePackageRoot
 * @property {string|undefined} version
 * @property {PackageSkillCandidate[]} candidates
 * @property {Array<{path?:string;message:string}>} diagnostics
 *
 * @typedef {Object} PackageSkillInventory
 * @property {"global"|"project"} scope
 * @property {boolean} trusted
 * @property {PackageSkillCard[]} packages
 * @property {Array<{path?:string;message:string}>} diagnostics
 */

function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value;
    else if (key === "checked" || key === "disabled") node[key] = Boolean(value);
    else if (key === "title") node.title = value;
    else if (key === "dataset") {
      for (const [dk, dv] of Object.entries(value)) node.dataset[dk] = dv;
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2).toLowerCase(), value);
    } else if (key === "aria") {
      for (const [ak, av] of Object.entries(value)) node.setAttribute(`aria-${ak}`, av);
    } else if (value !== undefined && value !== null) {
      node.setAttribute(key, String(value));
    }
  }
  for (const child of [].concat(children)) {
    if (child == null || child === false) continue;
    node.appendChild(typeof child === "string" ? document.createTextNode(child) : child);
  }
  return node;
}

/**
 * Set up the Packages skills tab.
 *
 * @param {Object} opts
 * @param {HTMLElement} opts.container
 * @param {(cmd:Object)=>Promise<Object>} opts.rpcCommand
 * @param {(message:string)=>void} [opts.showSuccess]
 * @param {(message:string)=>void} [opts.showError]
 * @returns {{activate:()=>Promise<void>,setScope:(scope:"global"|"project")=>Promise<void>,refresh:()=>Promise<void>,destroy:()=>void}}
 */
export function setupPackageSkillsTab({ container, rpcCommand, showSuccess, showError }) {
  let scope = "global";
  /** @type {PackageSkillInventory|null} */
  let inventory = null;
  let hasActivated = false;
  /** @type {Record<string,number>} */
  const scopeCounts = {};
  const expandedCards = new Set();
  let loadSeq = 0;
  let errorMessage = null;
  const pendingTargets = new Set();
  const unsubscribeLocale = onLocaleChange(() => render());

  function renderLoading() {
    container.replaceChildren(
      el("div", { class: "skills-loading", text: t("settings.packageSkills.loading") }),
    );
  }

  function renderError(message) {
    errorMessage = message || t("settings.packageSkills.loadFailed");
    container.replaceChildren(
      el("div", { class: "skills-error" }, [
        el("span", { text: errorMessage }),
        el("button", {
          type: "button",
          class: "skills-rescan",
          text: t("settings.skills.rescan"),
          onClick: () => void load(scope),
        }),
      ]),
    );
  }

  /**
   * @param {"global"|"project"} nextScope
   */
  async function load(nextScope = scope) {
    scope = nextScope;
    errorMessage = null;
    const seq = ++loadSeq;
    renderLoading();
    /** @type {{success?:boolean;data?:PackageSkillInventory;error?:string}|undefined} */
    let response;
    try {
      response = await rpcCommand({ type: "list_package_skill_inventory", scope });
    } catch (e) {
      if (seq !== loadSeq) return;
      inventory = null;
      renderError(e instanceof Error ? e.message : t("settings.packageSkills.loadFailed"));
      return;
    }
    if (seq !== loadSeq) return;
    if (!response?.success) {
      inventory = null;
      renderError(response?.error);
      return;
    }
    inventory = response.data;
    // The effective package list is the combined set; counts reflect the
    // selected scope's emphasis but the list itself is not filtered.
    scopeCounts[scope] = inventory.packages.filter((p) => p.scope === scope).length;
    render();
  }

  async function activate() {
    if (hasActivated) return;
    hasActivated = true;
    await load(scope);
  }

  async function setScope(nextScope) {
    if (nextScope === scope) return;
    await load(nextScope);
  }

  function refresh() {
    return load(scope);
  }

  /** The project entry exists only when the project actually contributes
   * packages (the Discovered tab's rule); landing has no project at all. The
   * open untrusted project scope stays reachable so its notice is not a
   * one-way door. */
  function scopeTabsToRender() {
    if (!inventory) return ["global"];
    if (inventory.packages.some((card) => card.scope === "project")) return ["global", "project"];
    return scope === "project" && !inventory.trusted ? ["global", "project"] : ["global"];
  }

  function renderScopeTabs() {
    const tabs = el("div", {
      class: "skills-scope-tabs",
      role: "group",
      aria: { label: t("settings.packageSkills.title") },
    });
    for (const s of scopeTabsToRender()) {
      const count = scopeCounts[s];
      const active = s === scope;
      tabs.appendChild(
        el(
          "button",
          {
            type: "button",
            class: `skills-scope-tab${active ? " active" : ""}`,
            "aria-pressed": String(active),
            dataset: { scope: s },
            onClick: () => {
              if (s !== scope) void load(s);
            },
          },
          [
            t(`settings.skills.${s}`),
            count !== undefined ? el("small", { text: ` ${count}` }) : null,
          ],
        ),
      );
    }
    return tabs;
  }

  async function setEnabled(
    card,
    candidate,
    enabled,
    { notify = true, rejectOnFailure = false, sequence = true } = {},
  ) {
    const targetId = `${card.identity}::${candidate.relativePath}`;
    const mutationScope = card.scope;
    const seq = sequence ? ++loadSeq : loadSeq;
    pendingTargets.add(targetId);
    render();
    let response;
    try {
      response = await rpcCommand({
        type: "set_package_skill_enabled",
        scope: mutationScope,
        target: { packageIdentity: card.identity, relativePath: candidate.relativePath },
        enabled,
      });
    } catch (e) {
      pendingTargets.delete(targetId);
      if (sequence && seq !== loadSeq) return;
      errorMessage = e instanceof Error ? e.message : t("settings.packageSkills.loadFailed");
      showError?.(errorMessage);
      render();
      if (rejectOnFailure) throw e;
      return null;
    }
    pendingTargets.delete(targetId);
    if (sequence && seq !== loadSeq) return;
    if (!response?.success) {
      errorMessage = response?.error || t("settings.packageSkills.loadFailed");
      showError?.(errorMessage);
      render();
      if (rejectOnFailure) throw new Error(errorMessage);
      return null;
    }
    errorMessage = null;
    inventory = response.data.inventory;
    if (notify) showSuccess?.(t("settings.skills.savedRestartRequired"));
    render();
    return response;
  }

  function renderSwitch(card, candidate) {
    const targetId = `${card.identity}::${candidate.relativePath}`;
    return el("input", {
      type: "checkbox",
      class: "skills-switch",
      checked: candidate.enabled,
      disabled: pendingTargets.has(targetId) || (card.scope === "project" && !inventory?.trusted),
      dataset: { skillToggle: targetId },
      aria: { label: `${t("settings.skills.enableSkill")}: ${candidate.name}` },
      onChange: (event) => void setEnabled(card, candidate, event.currentTarget.checked),
    });
  }

  async function setAllEnabled(card, enabled) {
    const results = await Promise.allSettled(
      card.candidates.map((candidate) =>
        setEnabled(card, candidate, enabled, {
          notify: false,
          rejectOnFailure: true,
          sequence: false,
        }),
      ),
    );
    const failures = results.filter((result) => result.status === "rejected");
    await load(card.scope);
    if (failures.length > 0) {
      errorMessage = t("settings.packageSkills.bulkFailure", {
        failed: failures.length,
        total: card.candidates.length,
      });
      showError?.(errorMessage);
      render();
      return;
    }
    showSuccess?.(t("settings.skills.savedRestartRequired"));
  }

  function renderEnableAllAffordance(card) {
    const total = card.candidates.length;
    const enabled = card.candidates.filter((candidate) => candidate.enabled).length;
    const state = total > 0 && enabled === total ? "all-on" : enabled === 0 ? "all-off" : "mixed";
    return renderGroupStatusControl({
      state,
      enabled,
      total,
      disabled: card.scope === "project" && !inventory?.trusted,
      dataset: { packageEnableAll: card.id, groupState: state },
      ariaLabel: `${t("settings.packageSkills.enableAll")}: ${card.source}`,
      onToggle: (next) => void setAllEnabled(card, next),
    });
  }

  /**
   * @param {PackageSkillCard} card
   */
  function renderCard(card) {
    const expanded = expandedCards.has(card.id);
    const installed = Boolean(card.effectivePackageRoot);
    const header = el("div", { class: "skills-group-header" }, [
      el(
        "button",
        {
          type: "button",
          class: `skills-expand${expanded ? "" : " collapsed"}`,
          aria: {
            label: `${t("settings.packageSkills.package")}: ${card.source}`,
            expanded: String(expanded),
            controls: `skills-group-list-${card.id}`,
          },
          onClick: () => {
            if (expandedCards.has(card.id)) expandedCards.delete(card.id);
            else expandedCards.add(card.id);
            render();
          },
        },
        createIcon("chevron-down", { size: 14 }),
      ),
      el("div", { class: "skills-group-info" }, [
        el("div", { class: "skills-group-name", text: card.source }),
        el("div", { class: "skills-group-source" }, [
          el("span", {
            class: "package-skills-card-scope",
            text: t(`settings.packageSkills.${card.scope}`),
          }),
          card.version
            ? el("span", { class: "package-skills-card-version", text: `v${card.version}` })
            : null,
          el("span", {
            class: "package-skills-card-count",
            text: t("settings.packageSkills.candidateCount", { count: card.candidates.length }),
          }),
        ]),
      ]),
      renderEnableAllAffordance(card),
    ]);

    if (!installed) {
      header.appendChild(
        el("div", {
          class: "package-skills-not-installed",
          text: t("settings.packageSkills.notInstalled"),
        }),
      );
    }

    for (const d of card.diagnostics ?? []) {
      header.appendChild(el("div", { class: "package-skills-diagnostic", text: d.message }));
    }

    const listing = el(
      "div",
      {
        id: `skills-group-list-${card.id}`,
        class: "skills-group-listing",
      },
      expanded && installed ? card.candidates.map((cand) => renderCandidate(card, cand)) : [],
    );

    return el(
      "section",
      {
        class: `skills-group${expanded ? "" : " closed"}${installed ? "" : " not-installed"}`,
        dataset: { packageCard: card.id },
      },
      [header, listing],
    );
  }

  /**
   * @param {PackageSkillCard} card
   * @param {PackageSkillCandidate} candidate
   */
  function renderCandidate(card, candidate) {
    const canonicalPath = normalizeLocalPath(candidate.canonicalPath);
    return el("div", { class: "skills-skill-row", dataset: { candidate: candidate.name } }, [
      el("div", { class: "skills-skill-info" }, [
        el("div", { class: "skills-skill-name", text: candidate.name }),
        el("div", { class: "skills-skill-description", text: candidate.description }),
        el("div", { class: "skills-skill-path" }, [
          el("code", {
            class: "package-skills-candidate-relative",
            text: candidate.relativePath,
            title: canonicalPath,
            aria: {
              label: `${t("settings.packageSkills.canonicalPath")}: ${canonicalPath}`,
            },
          }),
        ]),
        ...(candidate.diagnostics ?? []).map((d) =>
          el("div", { class: "package-skills-diagnostic", text: d.message }),
        ),
      ]),
      renderSwitch(card, candidate),
    ]);
  }

  function render() {
    const scrollTop = container.scrollTop;

    if (!inventory) {
      if (errorMessage) renderError(errorMessage);
      else renderLoading();
      return;
    }

    const untrusted = scope === "project" && !inventory.trusted;

    const fragment = document.createDocumentFragment();

    fragment.appendChild(
      el("div", { class: "skills-header" }, [
        el("div", {}, [
          el("h3", {
            class: "settings-section-title",
            text: t("settings.packageSkills.title"),
          }),
          el("p", {
            class: "skills-intro",
            text: t("settings.packageSkills.description"),
          }),
          el("p", {
            class: "package-skills-bundled-note",
            text: t("settings.packageSkills.bundledCandidates"),
          }),
        ]),
        el("button", {
          type: "button",
          class: "skills-rescan",
          text: t("settings.skills.rescan"),
          onClick: refresh,
        }),
      ]),
    );

    fragment.appendChild(renderScopeTabs());

    if (untrusted) {
      fragment.appendChild(
        el("div", {
          class: "skills-notice",
          text: t("settings.packageSkills.projectUntrusted"),
        }),
      );
    } else {
      const emphasized = inventory.packages.filter((p) => p.scope === scope);
      const skillCount = emphasized.reduce(
        (total, card) => total + (card.candidates?.length ?? 0),
        0,
      );
      fragment.appendChild(
        el("div", { class: "skills-scope-meta" }, [
          el("span", {
            text: t("settings.packageSkills.scopeSummary", {
              skills: skillCount,
              packages: emphasized.length,
            }),
          }),
        ]),
      );
    }

    for (const d of inventory.diagnostics ?? []) {
      fragment.appendChild(
        el("div", {
          class: "package-skills-diagnostic package-skills-diagnostic-global",
          text: d.message,
        }),
      );
    }

    if (inventory.packages.length === 0) {
      fragment.appendChild(
        el("div", { class: "skills-empty", text: t("settings.packageSkills.empty") }),
      );
    } else {
      const list = el("div", { class: "skills-group-list" });
      for (const card of inventory.packages) list.appendChild(renderCard(card));
      fragment.appendChild(list);
    }

    container.replaceChildren(fragment);
    container.scrollTop = scrollTop;
  }

  function destroy() {
    unsubscribeLocale?.();
    container.replaceChildren();
  }

  return { activate, setScope, refresh, destroy };
}

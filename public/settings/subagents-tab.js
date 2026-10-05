// ABOUTME: Displays host-owned subagent disk candidates and restricted read-only definitions.
// ABOUTME: Keeps scope and workspace requests isolated without asserting unverified runtime winners.

import { t } from "../i18n.js";
import { appendModelOptions, loadModelChoices } from "./package-extension-settings.js";

// Definitions and Packages are two views of one scope: the same host response
// feeds both, so the scope tab row stays the only request axis.
const SUBTABS = ["definitions", "packages"];

const CONFLICT_CODES = new Set([
  "stale_generation",
  "revision_conflict",
  "inventory_revision_conflict",
  "candidate_stale",
]);

// Host diagnostics arrive as English (source, message) pairs. KNOWN_DIAGNOSTICS
// maps the fixed messages to i18n so a localized page never mixes raw English in;
// unrecognized text is shown verbatim instead of being swallowed. The candidate
// parity note is mapped to the status row's own key: it states the same fact.
const STATUS_CANDIDATE_KEY = "settings.subagents.status.candidate";
const KNOWN_DIAGNOSTICS = {
  "discovery budget exhausted": "diagnostics.scanBudget",
  "unreadable source": "diagnostics.scanUnreadableSource",
  "unreadable entry": "diagnostics.scanUnreadableEntry",
  "symlink source omitted": "diagnostics.symlinkOmitted",
  "invalid settings; discovery incomplete": "diagnostics.invalidSettings",
  "extension project root differs from workspace root; project writes disabled":
    "diagnostics.projectRootMismatch",
  "out-of-scope occupancy not verified; no definition body read": "diagnostics.outOfScope",
  "builtins disabled by settings; runtime names unverified": "diagnostics.builtinsDisabled",
  "installed extension builtins unavailable; names unknown": "diagnostics.builtinsUnavailable",
  "invalid manifest": "diagnostics.invalidManifest",
  "manifest path outside package root": "diagnostics.manifestOutsidePackageRoot",
  "unreadable, oversized or non-UTF-8 definition": "diagnostics.definitionUnreadable",
  "missing frontmatter": "diagnostics.missingFrontmatter",
  "unsupported frontmatter": "diagnostics.unsupportedFrontmatter",
  "unterminated frontmatter": "diagnostics.unterminatedFrontmatter",
  "invalid package name": "diagnostics.invalidPackageName",
  "duplicate runtime name or alias; runtime winner unverified": "diagnostics.nameCollision",
  "live snapshot not supplied; disk candidates only": "diagnostics.parityUnverified",
  "winner or out-of-scope occupancy unverified": "status.candidate",
  "external or unknown runner": "diagnostics.externalRunner",
  "shadowed by a higher-precedence definition": "diagnostics.shadowed",
  "same-level duplicate name; scan-order winner unverifiable": "diagnostics.sameLevelDuplicate",
};

const diagnosticKey = (message) => {
  const relative = KNOWN_DIAGNOSTICS[message];
  return relative ? `settings.subagents.${relative}` : null;
};

// Selects speak strings; saved JSON booleans read back as their string
// options and are converted back to booleans on save.
const controlValue = (value) => {
  if (value === false) return "false";
  if (value == null) return "";
  return String(value);
};

// Levels the host accepts as a thinking override. The catalog carries no
// per-model capability data, so they are all offered and the hint says so.
const THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

// The host's own shape for a free-text model id: a nonempty provider and a
// model part, no whitespace anywhere. Deliberately not narrower than the
// writer's check.
const MODEL_ID = /^[^/\s]+\/.+$/;

function text(tag, className, value) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  node.textContent = value == null ? "" : String(value);
  return node;
}

export function setupSubagentsTab({
  container,
  transport,
  getWorkspaceIdentity,
  configGateway,
  landingOnly = false,
  confirmDiscard = () => globalThis.confirm?.("Discard unsaved subagent changes?") ?? false,
}) {
  if (!container) throw new Error("Subagents settings container is required");
  // An injected model catalog ({id}) turns the model control into a picker of
  // real ids; without one it degrades to a validated text input. A catalog
  // Cached model choices from the shared bridge loader (the same source the
  // rpiv-advisor picker and the composer use: catalog + scoped models).
  let cachedModels = null;
  let cachedScopedIds = null;
  const catalog = () => cachedModels;
  const refreshCatalog = async () => {
    if (!configGateway) return;
    const choices = await loadModelChoices(configGateway);
    cachedModels = choices.models.length > 0 ? choices.models : null;
    cachedScopedIds = choices.scopedIds;
  };
  const modelOverrideId = (model) =>
    model?.provider ? `${model.provider}/${model.id}` : model?.id;
  const option = (select, value, caption) => {
    const node = document.createElement("option");
    node.value = value;
    node.textContent = caption;
    select.append(node);
  };
  let active = "global";
  let subtab = "definitions";
  let inventory = null;
  let selectedId = null;
  let raw = null;
  let loadingDetail = false;
  let error = null;
  let token = 0;
  let identityKey = null;
  let visible = false;
  let mode = "detail";
  let notice = "";
  let saving = false;
  const drafts = new Map();
  const draftKey = (id) => `${identityKey}:${active}:${id}`;
  const getDraft = (id) => drafts.get(draftKey(id)) || {};
  const setDraft = (id, patch) => drafts.set(draftKey(id), { ...getDraft(id), ...patch });
  const identity = () => (landingOnly ? null : getWorkspaceIdentity?.() || null);
  const keyOf = (value) =>
    value ? `${value.workspaceId}:${value.workspaceGeneration}` : "landing";
  const entriesForScope = () => {
    const entries = inventory?.entries || [];
    return entries.filter(
      (entry) =>
        entry.sourceScope === active ||
        entry.additionalScopes?.includes(active) ||
        (active === "global" && entry.source === "builtin"),
    );
  };
  // The sub-tabs are a pure projection of the one inventory the scope request
  // already returned: both lists come from the same response, so switching a
  // sub-tab never refetches. Built-in extension agents are read-only and belong
  // to the Packages projection, not to the writable Custom one.
  const isPackageView = (entry) => entry.source === "package" || entry.source === "builtin";
  const visibleEntries = () =>
    entriesForScope().filter((entry) => isPackageView(entry) === (subtab === "packages"));
  // Known host diagnostics render in the page's language; unknown ones keep the
  // raw source and message rather than being swallowed.
  const writeDiagnosticText = (diagnostic) => {
    const key = diagnosticKey(diagnostic.message);
    if (key === STATUS_CANDIDATE_KEY) return null;
    return key ? t(key) : `${diagnostic.source}: ${diagnostic.message}`;
  };
  const _diagnosticText = (diagnostic) => {
    const key = diagnosticKey(diagnostic.message);
    return key && key !== STATUS_CANDIDATE_KEY
      ? `${diagnostic.source}: ${t(key)}`
      : `${diagnostic.source}: ${diagnostic.message}`;
  };
  // "Also available in" is relative to the view: the scopes that still hold
  // this definition once the current one is removed. Never the view itself,
  // whichever field (sourceScope or additionalScopes) named it.
  const otherScopes = (entry) =>
    [...new Set([entry.sourceScope, ...(entry.additionalScopes || [])])].filter(
      (scope) => scope && scope !== active,
    );

  function switchScope(next) {
    if (next === active) return;
    active = next;
    subtab = SUBTABS[0];
    selectedId = null;
    raw = null;
    void load();
  }

  function switchSubtab(next) {
    if (next === subtab) return;
    subtab = next;
    if (!visibleEntries().some((entry) => entry.id === selectedId)) {
      selectedId = null;
      raw = null;
    }
    render();
  }

  function render() {
    // Rebuilding the DOM would otherwise snap the master list back to the top
    // on every selection render; carry the scroll offset across.
    const previousScroll = container.querySelector(".subagents-master")?.scrollTop ?? 0;
    container.replaceChildren();
    const frame = text("div", "subagents-view settings-section", "");
    const scopeTabs = text("div", "subagents-scopes extensions-page-tabs", "");
    scopeTabs.setAttribute("role", "tablist");
    const scopes = identity() && !landingOnly ? ["global", "project"] : ["global"];
    // Two independent tablists: the scope row cycles only scopes, the sub-tab
    // row only sub-tabs, so arrow keys never cross from one into the other.
    const wireTabKeys = (button, list, current, onSelect, selector) => {
      button.addEventListener("keydown", (event) => {
        if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        let next = list[0];
        if (event.key === "End") next = list.at(-1);
        else if (event.key !== "Home") {
          const step = event.key === "ArrowRight" ? 1 : -1;
          next = list[(list.indexOf(current) + step + list.length) % list.length];
        }
        onSelect(next);
        queueMicrotask(() => container.querySelector(selector(next))?.focus());
      });
    };
    for (const scope of scopes) {
      const button = text(
        "button",
        "subagents-scope extensions-page-tab",
        t(`settings.subagents.scopes.${scope}`),
      );
      button.type = "button";
      button.dataset.subagentsScope = scope;
      button.setAttribute("role", "tab");
      button.setAttribute("aria-selected", String(active === scope));
      button.tabIndex = active === scope ? 0 : -1;
      button.addEventListener("click", () => switchScope(scope));
      wireTabKeys(button, scopes, scope, switchScope, (next) => `[data-subagents-scope="${next}"]`);
      scopeTabs.append(button);
    }
    frame.append(scopeTabs);
    // Custom and Packages are two projections of the loaded scope, so the
    // sub-tab row re-renders from the cached inventory and never refetches. It
    // reuses the Skills page-tab classes so both pages share one tab style.
    const subTabRow = text("div", "skills-scope-tabs", "");
    subTabRow.setAttribute("role", "tablist");
    for (const name of SUBTABS) {
      const button = text(
        "button",
        `skills-scope-tab${subtab === name ? " active" : ""}`,
        t(`settings.subagents.subtabs.${name}`),
      );
      button.type = "button";
      button.dataset.subagentsSubtab = name;
      button.setAttribute("role", "tab");
      button.setAttribute("aria-selected", String(subtab === name));
      button.tabIndex = subtab === name ? 0 : -1;
      button.addEventListener("click", () => switchSubtab(name));
      wireTabKeys(
        button,
        SUBTABS,
        name,
        switchSubtab,
        (next) => `[data-subagents-subtab="${next}"]`,
      );
      subTabRow.append(button);
    }
    // Scope description (PM copy) above the sub-tab segmented control.
    frame.append(text("p", "settings-help", t(`settings.subagents.scopeDescription.${active}`)));
    frame.append(subTabRow);
    // Counter line between the sub-tab row and the master/detail layout,
    // mirroring the Skills page's list-count placement.
    if (inventory && !error) {
      const entries = visibleEntries();
      let countLine;
      if (subtab === "packages") {
        const pkgs = new Set(
          entries.filter((e) => e.source === "package").map((e) => e.packageIdentity || ""),
        ).size;
        countLine = t("settings.subagents.count.packages")
          .replace("{agents}", String(entries.length))
          .replace("{packages}", String(pkgs));
      } else {
        countLine = t("settings.subagents.count.definitions").replace(
          "{count}",
          String(entries.length),
        );
      }
      frame.append(text("p", "settings-help", countLine));
    }
    if (notice) frame.append(text("p", "subagents-feedback", notice));
    // Master/detail reuses the Extensions package-manager shell (page tabs,
    // master card, detail card) so Subagents reads like its sibling Settings
    // pages. Loading/error/empty live inside the master card, the way the
    // Installed tab renders its empty state.
    const layout = text("div", "subagents-layout pkg-manager-layout", "");
    const master = text("div", "subagents-master pkg-manager-groups", "");
    const detail = text("section", "subagents-detail pkg-manager-detail", "");
    const retryButton = () => {
      const retry = text(
        "button",
        "subagents-retry settings-value-btn",
        t("settings.subagents.rescan"),
      );
      retry.type = "button";
      retry.addEventListener("click", () => {
        void load();
      });
      return retry;
    };
    // Detail metadata rows reuse the package-manager status grid; `wrap` lets a
    // long definition path break instead of being clipped.
    const metaRow = (caption, value, wrap = false) => {
      const line = text("div", "pkg-manager-status-row", "");
      line.append(text("span", "", `${caption}: `), text("span", wrap ? "is-wrap" : "", value));
      return line;
    };
    if (error) {
      const message = CONFLICT_CODES.has(error.code)
        ? t("settings.subagents.state.conflict")
        : t("settings.subagents.state.error");
      master.append(text("p", "subagents-error", message), retryButton());
    } else if (!inventory) {
      master.append(text("p", "subagents-loading", t("settings.subagents.state.loading")));
    } else {
      const entries = visibleEntries();
      if (!entries.length) {
        master.append(
          text("p", "subagents-empty", t("settings.subagents.state.empty")),
          retryButton(),
        );
      }
      const appendRow = (entry) => {
        const row = text("button", "subagents-row pkg-manager-sidebar-row", "");
        row.type = "button";
        row.dataset.candidateId = entry.id;
        row.setAttribute("aria-pressed", String(selectedId === entry.id));
        if (selectedId === entry.id) row.classList.add("is-selected");
        row.append(text("div", "pkg-manager-sidebar-name", entry.runtimeName));
        const alsoIn = otherScopes(entry);
        const badges = [
          ...(entry.source === "package" ? [entry.source] : []),
          ...(entry.savedOverride?.disabled === true
            ? [t("settings.subagents.status.disabledOverride")]
            : []),
          ...(alsoIn.length
            ? [t("settings.subagents.alsoIn").replace("{scopes}", alsoIn.join(", "))]
            : []),
        ];
        if (badges.length) {
          const meta = text("div", "pkg-manager-sidebar-meta", "");
          for (const badge of badges) meta.append(text("span", "subagents-badge", badge));
          row.append(meta);
        }
        row.addEventListener("click", () => {
          void select(entry);
        });
        master.append(row);
      };
      const appendGroup = (caption, members) => {
        if (!members.length) return;
        master.append(text("h4", "subagents-group-header pkg-manager-group-header", caption));
        for (const entry of members) appendRow(entry);
      };
      if (subtab === "packages") {
        // One group per package identity: the group header is that identity.
        // Built-in extension agents are the same read-only projection and get
        // their own group rather than being listed among user definitions.
        const packages = new Map();
        const builtin = [];
        for (const entry of entries) {
          if (entry.source === "builtin") {
            builtin.push(entry);
            continue;
          }
          const packageName = entry.packageIdentity || t("settings.subagents.groups.package");
          packages.set(packageName, [...(packages.get(packageName) || []), entry]);
        }
        for (const [packageName, members] of packages) appendGroup(packageName, members);
        appendGroup(t("settings.subagents.groups.builtinExtension"), builtin);
      } else {
        for (const source of ["user", "project"]) {
          appendGroup(
            t(`settings.subagents.groups.${source}`),
            entries.filter((entry) => entry.source === source),
          );
        }
      }
      // The add affordance closes the master list, matching the MCP page.
      // Creating writes a definition, so it belongs to the Custom sub-tab only.
      if (subtab === "definitions") {
        const add = text(
          "button",
          "subagents-new models-provider-add",
          t("settings.subagents.create.title"),
        );
        add.type = "button";
        add.addEventListener("click", () => {
          mode = "new";
          selectedId = null;
          raw = null;
          render();
        });
        master.append(add);
      }
      const selected = entries.find((entry) => entry.id === selectedId);
      if (mode === "new" && subtab === "definitions") {
        const header = text("div", "subagents-detail-header pkg-manager-detail-header", "");
        header.append(text("h4", "subagents-detail-title", t("settings.subagents.create.title")));
        detail.append(header);
        for (const field of ["name", "description", "prompt"]) {
          const caption = text("label", "subagents-field", "");
          caption.append(
            text("span", "subagents-field-label", t(`settings.subagents.create.${field}`)),
          );
          const input = text(
            field === "prompt" ? "textarea" : "input",
            "subagents-create-input",
            "",
          );
          input.name = field;
          input.value = getDraft("new")[field] || "";
          input.addEventListener("input", () => setDraft("new", { [field]: input.value }));
          caption.append(input);
          detail.append(caption);
        }
        const submit = text(
          "button",
          "subagents-create settings-value-btn pkg-manager-btn is-primary",
          t("settings.subagents.create.submit"),
        );
        submit.type = "button";
        submit.disabled = inventory.resolutionContext.mode !== "verified";
        submit.addEventListener("click", () => {
          void create();
        });
        detail.append(submit);
        if (submit.disabled)
          detail.append(
            text("p", "subagents-write-diagnostic", t("settings.subagents.create.disabled")),
          );
      } else if (selected) {
        const header = text("div", "subagents-detail-header pkg-manager-detail-header", "");
        // The enable/disable switch lives on the detail name row, right-aligned,
        // matching where the MCP and Skills pages put their toggles.
        const nameRow = text("div", "subagents-name-row", "");
        nameRow.append(text("h4", "subagents-name", selected.runtimeName));
        // Enable semantics like the MCP/Skill switches: agents default to
        // enabled, so the switch is ON unless a disabled override exists.
        const isEnabled = selected.savedOverride?.disabled !== true;
        const toggle = text(
          "button",
          `settings-toggle subagents-detail-toggle${isEnabled ? " on" : ""}`,
          "",
        );
        toggle.type = "button";
        toggle.setAttribute(
          "aria-label",
          `${selected.runtimeName}: ${t("settings.subagents.detail.enable")}`,
        );
        toggle.setAttribute("aria-pressed", String(isEnabled));
        toggle.disabled = !(selected.writeQualified && selected.nativeOverrideSupported);
        toggle.addEventListener("click", () => {
          void writeDisabled(selected, isEnabled);
        });
        nameRow.append(toggle);
        header.append(nameRow);
        detail.append(header);
        const grid = text("div", "pkg-manager-status-grid", "");
        for (const [field, value, wrap] of [
          ["runtimeName", selected.runtimeName, false],
          ["source", selected.source, false],
          ["scope", selected.sourceScope, false],
          ["path", selected.filePath || t("settings.subagents.detail.noFile"), true],
        ]) {
          grid.append(metaRow(t(`settings.subagents.detail.${field}`), value, wrap));
        }
        if (selected.packageIdentity)
          grid.append(metaRow(t("settings.subagents.detail.package"), selected.packageIdentity));
        detail.append(grid);
        detail.append(
          text("p", "subagents-status", t(`settings.subagents.status.${selected.status}`)),
        );
        // A parity note that only restates the candidate status row above is
        // dropped; a distinct write blocker is always shown.
        const note = selected.writeDiagnostic && writeDiagnosticText(selected.writeDiagnostic);
        if (note) detail.append(text("p", "subagents-write-diagnostic", note));
        if (selected.savedOverride) {
          detail.append(
            text("h5", "subagents-subheading", t("settings.subagents.detail.savedOverride")),
          );
          const saved = text("div", "pkg-manager-status-grid", "");
          for (const field of ["model", "thinking"])
            saved.append(
              metaRow(
                t(`settings.subagents.detail.${field}`),
                selected.savedOverride[field] ?? t("settings.subagents.detail.none"),
              ),
            );
          for (const field of ["advertise", "disabled"])
            if (selected.savedOverride[field] != null)
              saved.append(
                metaRow(
                  t(`settings.subagents.detail.${field}`),
                  String(selected.savedOverride[field]),
                ),
              );
          detail.append(saved);
        }
        if (selected.inferredValue && inventory.resolutionContext.mode === "verified") {
          detail.append(
            text(
              "p",
              "subagents-inferred",
              `${t("settings.subagents.detail.inferred")}: ${selected.inferredValue.model ?? ""} ${selected.inferredValue.thinking ?? ""} (${selected.inferredValue.source ?? ""})`,
            ),
          );
        }
        // Name-level write qualification is the only gate: disk-candidates-only
        // parity still permits name-scoped saves, while an external runner or a
        // blocked name stays read-only.
        const canEdit = Boolean(selected.writeQualified && selected.nativeOverrideSupported);
        const models = catalog();
        const draft = getDraft(selected.id);
        const keepCurrent = (value) =>
          t("settings.subagents.detail.keepCurrent").replace("{value}", value);
        const commit = (field) => (value) => {
          setDraft(selected.id, { [field]: value });
          save.disabled = !canEdit || saving || !Object.keys(getDraft(selected.id)).length;
        };
        // The write target is the active scope's settings file, never the
        // definition file the row points at.
        const layerPath = inventory.workspaceRoot || inventory.projectRoot;
        let layerScope = t("settings.subagents.detail.writeLayerGlobal");
        if (active === "project") {
          layerScope = layerPath
            ? t("settings.subagents.detail.writeLayerProject").replace("{root}", layerPath)
            : t("settings.subagents.detail.writeLayerProjectUnknown");
        }
        detail.append(
          text(
            "p",
            "subagents-write-layer subagents-reload",
            t("settings.subagents.detail.writeLayer")
              .replace("{scope}", layerScope)
              .replace("{runtimeName}", selected.runtimeName),
          ),
        );
        const form = text("div", "subagents-overrides", "");
        for (const field of ["model", "thinking", "advertise"]) {
          const saved = selected.savedOverride?.[field];
          const current = controlValue(draft[field] === undefined ? saved : draft[field]);
          const isBoolean = field === "advertise";
          const isSelect = isBoolean || field === "thinking";
          const caption = text("label", "subagents-field", "");
          caption.append(
            text("span", "subagents-field-label", t(`settings.subagents.detail.${field}`)),
          );
          if (field === "model" && models) {
            // rpiv-advisor picker pattern: a native <select> with scoped ★
            // and all-enabled optgroups, plus "Not set" as the first option.
            const select = text("select", "subagents-override-input", "");
            select.setAttribute("aria-label", t("settings.subagents.detail.model"));
            select.disabled = !canEdit;
            const option = (value, textContent) => {
              const node = document.createElement("option");
              node.value = value;
              node.textContent = textContent;
              select.append(node);
            };
            option("", t("settings.subagents.detail.notSet"));
            appendModelOptions(select, { models, scopedIds: cachedScopedIds ?? [] });
            if (current) {
              const known = models.some((model) => modelOverrideId(model) === current);
              if (known) {
                select.value = current;
              } else {
                option(current, keepCurrent(current));
                select.value = current;
              }
            }
            select.addEventListener("change", () => {
              commit(field)(select.value);
            });
            caption.append(select);
            form.append(caption);
            continue;
          }
          const control = text(isSelect ? "select" : "input", "subagents-override-input", "");
          control.setAttribute("aria-label", t(`settings.subagents.detail.${field}`));
          if (isSelect) {
            option(
              control,
              "",
              t(
                isBoolean
                  ? "settings.subagents.detail.inheritDefinition"
                  : "settings.subagents.detail.noLayerOverride",
              ),
            );
            if (isBoolean) {
              option(control, "true", t("settings.subagents.detail.booleanOn"));
              option(control, "false", t("settings.subagents.detail.booleanOff"));
            } else if (field === "thinking") {
              option(control, "false", t("settings.subagents.detail.thinkingFalse"));
              for (const level of THINKING_LEVELS) option(control, level, level);
              // A saved level the list does not know stays selectable instead of
              // reading back as an empty override.
              if (current && current !== "false" && !THINKING_LEVELS.includes(current))
                option(control, current, keepCurrent(current));
            }
            control.value = current;
            control.addEventListener("change", () => commit(field)(control.value));
          } else {
            control.value = current;
            control.addEventListener("input", () => commit(field)(control.value));
          }
          control.disabled = !canEdit;
          caption.append(control);
          // No catalog data on per-model levels: every level is offered and the
          // hint points at the runtime as the authority.
          if (isSelect && field === "thinking")
            caption.append(
              text(
                "p",
                "subagents-thinking-hint subagents-reload",
                t("settings.subagents.detail.thinkingHint"),
              ),
            );
          form.append(caption);
        }
        const save = text(
          "button",
          "subagents-save settings-value-btn pkg-manager-btn is-primary",
          t("settings.subagents.detail.save"),
        );
        save.type = "button";
        save.disabled = !canEdit || saving || !Object.keys(getDraft(selected.id)).length;
        save.addEventListener("click", () => {
          void saveOverride(selected);
        });
        form.append(save);
        detail.append(
          form,
          text("p", "subagents-reload", t("settings.subagents.detail.reloadNotice")),
        );
        if (selected.filePath && selected.source !== "builtin") {
          if (loadingDetail)
            detail.append(text("p", "", t("settings.subagents.detail.rawLoading")));
          else if (raw !== null) detail.append(text("pre", "subagents-raw", raw));
          else detail.append(text("p", "", t("settings.subagents.detail.rawUnavailable")));
        }
      }
    }
    layout.append(master, detail);
    frame.append(layout);
    // Page-level inventory diagnostics stay host-side only: the five
    // out-of-scope notices repeat on every render and carry no action for
    // the user. Per-agent write diagnostics in the detail view remain.
    container.append(frame);
    const masterNode = frame.querySelector(".subagents-master");
    if (masterNode) masterNode.scrollTop = previousScroll;
  }

  // Row-switch write path: toggling disabled never touches the detail form's
  // drafts; the other three fields are explicit keeps.
  async function writeDisabled(entry, nextDisabled) {
    if (saving) return;
    if (!entry.writeQualified || !entry.nativeOverrideSupported) return;
    saving = true;
    render();
    const request = token;
    try {
      const payload = {
        scope: active,
        candidateId: entry.id,
        runtimeName: entry.runtimeName,
        expectedRevision: inventory.settingsRevisions[active],
        model: { op: "keep" },
        thinking: { op: "keep" },
        advertise: { op: "keep" },
        disabled: nextDisabled ? { op: "set", value: true } : { op: "clear" },
      };
      if (active === "project") Object.assign(payload, identity());
      const result = await transport.setSubagentOverride(payload);
      if (request !== token || !visible) return;
      inventory = result.inventory;
      notice = t("settings.subagents.detail.saved");
    } catch (failure) {
      if (request !== token || !visible) return;
      notice = CONFLICT_CODES.has(failure.code)
        ? t("settings.subagents.state.conflict")
        : String(failure.message || failure);
    } finally {
      saving = false;
      if (visible) render();
    }
  }
  async function saveOverride(entry) {
    // A pending save blocks a second one: double-clicking must not write twice.
    if (saving) return;
    if (!entry.writeQualified || !entry.nativeOverrideSupported) return;
    const changes = getDraft(entry.id);
    // A catalog-backed dropdown only offers ids the catalog reported; the
    // free-text fallback applies the host's own provider/model shape.
    if (
      changes.model !== undefined &&
      changes.model !== "" &&
      !catalog() &&
      !MODEL_ID.test(changes.model)
    ) {
      notice = t("settings.subagents.detail.invalidModel");
      render();
      return;
    }
    const action = (field) => {
      if (changes[field] === undefined) return { op: "keep" };
      if (changes[field] === "") return { op: "clear" };
      // Boolean selects speak strings; the host accepts JSON booleans only.
      if (field === "advertise" || field === "disabled") {
        return { op: "set", value: changes[field] === "true" };
      }
      const value = field === "thinking" && changes[field] === "false" ? false : changes[field];
      return { op: "set", value };
    };
    saving = true;
    render();
    const request = token;
    try {
      const payload = {
        scope: active,
        candidateId: entry.id,
        runtimeName: entry.runtimeName,
        expectedRevision: inventory.settingsRevisions[active],
        model: action("model"),
        thinking: action("thinking"),
        advertise: action("advertise"),
        disabled: action("disabled"),
      };
      if (active === "project") Object.assign(payload, identity());
      const result = await transport.setSubagentOverride(payload);
      if (request !== token || !visible) return;
      drafts.delete(draftKey(entry.id));
      inventory = result.inventory;
      notice = t("settings.subagents.detail.saved");
    } catch (failure) {
      // The draft survives: the user retries against the same input.
      if (request !== token || !visible) return;
      notice = CONFLICT_CODES.has(failure.code)
        ? t("settings.subagents.state.conflict")
        : String(failure.message || failure);
    } finally {
      saving = false;
      if (visible) render();
    }
  }

  async function create() {
    if (inventory?.resolutionContext?.mode !== "verified") return;
    const fields = getDraft("new");
    if (!fields.name?.trim() || !fields.description?.trim() || !fields.prompt?.trim()) {
      notice = t("settings.subagents.create.invalid");
      render();
      return;
    }
    const request = token;
    try {
      const payload = {
        scope: active,
        ...fields,
        expectedInventoryRevision: inventory.inventoryRevision,
        expectedRevision: inventory.settingsRevisions[active],
        confirmShadowedIds: [],
      };
      if (active === "project") Object.assign(payload, identity());
      const result = await transport.createSubagent(payload);
      if (request !== token || !visible) return;
      drafts.delete(draftKey("new"));
      inventory = result.inventory;
      mode = "detail";
      notice = t("settings.subagents.detail.saved");
    } catch (failure) {
      if (request !== token || !visible) return;
      notice = CONFLICT_CODES.has(failure.code)
        ? t("settings.subagents.state.conflict")
        : String(failure.message || failure);
    }
    render();
  }

  async function load() {
    const request = ++token;
    error = null;
    inventory = null;
    raw = null;
    saving = false;
    loadingDetail = false;
    render();
    try {
      const result = await transport.listSubagents(
        active,
        active === "project" ? identity() : null,
      );
      if (request !== token || !visible || keyOf(identity()) !== identityKey) return;
      inventory = result;
      if (!entriesForScope().some((entry) => entry.id === selectedId)) {
        selectedId = null;
        raw = null;
      }
    } catch (failure) {
      if (request !== token || !visible) return;
      error = failure;
    }
    render();
  }

  async function select(entry) {
    selectedId = entry.id;
    mode = "detail";
    raw = null;
    loadingDetail = Boolean(entry.filePath && entry.source !== "builtin");
    const request = ++token;
    render();
    if (!loadingDetail) return;
    try {
      const result = await transport.getSubagentDetail(
        active,
        entry.id,
        active === "project" ? identity() : null,
      );
      if (request !== token || !visible || keyOf(identity()) !== identityKey) return;
      raw = result.rawDefinition ?? null;
    } catch {
      if (request !== token || !visible) return;
      raw = null;
    }
    loadingDetail = false;
    render();
  }

  return {
    async activate() {
      const current = keyOf(identity());
      if (current !== identityKey) {
        ++token;
        identityKey = current;
        inventory = null;
        selectedId = null;
        raw = null;
        active = "global";
        subtab = SUBTABS[0];
        mode = "detail";
        notice = "";
      }
      visible = true;
      // Kick off the shared bridge loader so the model dropdown has items
      // by the time the user reaches a detail view; re-render on arrival.
      void refreshCatalog()
        .then(() => {
          if (visible) render();
        })
        .catch(() => {});
      await load();
    },
    leave() {
      if (
        [...drafts].some(
          ([key, value]) => key.includes(":project:") && Object.values(value).some(Boolean),
        )
      ) {
        if (!confirmDiscard()) return false;
        for (const key of drafts.keys()) if (key.includes(":project:")) drafts.delete(key);
      }
      visible = false;
      ++token;
      return true;
    },
    resetProject() {
      ++token;
      inventory = null;
      selectedId = null;
      raw = null;
    },
  };
}

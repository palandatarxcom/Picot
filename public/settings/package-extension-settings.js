// ABOUTME: Per-package settings renderers mounted at the bottom of the Extensions package detail page.
// ABOUTME: Advisor is the first entry; packages without a renderer render nothing.

import { t } from "../i18n.js";
import { filterModelsByCatalogVisibility, splitModelsByScope } from "../models/selection.js";

/**
 * The composer's model list, from the composer's own two sources: the Picot
 * catalog (per-model enable/visibility) and the workspace-scoped model ids.
 * Both surfaces read them over the bridge — workspace runtime or landing
 * config runtime — so a picker here and the composer cannot drift apart.
 */
export async function loadModelChoices(configGateway) {
  if (!configGateway) return { models: [], scopedIds: [] };
  const [catalog, scoped] = await Promise.all([
    configGateway.call("list_model_catalog").catch(() => null),
    configGateway.call("list_scoped_models").catch(() => null),
  ]);
  // Same rule as the composer: an unreadable catalog fails closed instead of
  // re-exposing every available model after the user curated the list.
  if (!catalog?.ok) return { models: [], scopedIds: [], catalogOk: false };
  const listed = (catalog.data?.providers ?? []).flatMap((provider) => provider.models ?? []);
  return {
    models: filterModelsByCatalogVisibility(listed, catalog),
    scopedIds: scoped?.ok && Array.isArray(scoped.data?.modelIds) ? scoped.data.modelIds : [],
    catalogOk: true,
  };
}

/** An empty picker reads as broken; say why, with the composer's copy. */
function noteWhenCatalogUnavailable(target, choices) {
  if (choices.catalogOk || choices.models.length > 0) return;
  const note = document.createElement("p");
  note.className = "settings-help";
  note.textContent = t("models.unavailableHelp");
  target.appendChild(note);
}

function modelDisplayName(model) {
  const key = `${model.provider}/${model.id}`;
  return model.name && model.name !== model.id ? `${model.name} (${key})` : key;
}

/** Composer parity: scoped models first, then every other enabled model. */
export function appendModelOptions(select, { models, scopedIds }) {
  const { scoped, remaining } = splitModelsByScope(models, scopedIds);
  for (const [label, group] of [
    [t("models.scoped"), scoped],
    [t("models.allEnabled"), remaining],
  ]) {
    if (group.length === 0) continue;
    const optgroup = document.createElement("optgroup");
    optgroup.label = label;
    for (const model of group) {
      const option = document.createElement("option");
      option.value = `${model.provider}/${model.id}`;
      option.textContent = modelDisplayName(model);
      optgroup.appendChild(option);
    }
    select.appendChild(optgroup);
  }
}

/**
 * Package source → settings renderer. `dep` names the dependency that
 * renderer needs: host control ops ride the transport (landing and workspace
 * alike), bridge ops need a config gateway (the workspace runtime or the
 * landing config runtime). A package without its dependency renders nothing.
 */
const SETTINGS_RENDERERS = new Map([
  ["npm:@juicesharp/rpiv-advisor", { dep: "configGateway", render: renderAdvisorSettings }],
  ["npm:@ff-labs/pi-fff", { dep: "transport", render: renderFffSettings }],
  ["npm:@juicesharp/rpiv-todo", { dep: "transport", render: renderTodoSettings }],
  ["npm:@juicesharp/rpiv-ask-user-question", { dep: "transport", render: renderAskUserSettings }],
  ["npm:@dietrichgebert/ponytail", { dep: "transport", render: renderPonytailSettings }],
  ["npm:@sting8k/pi-vcc", { dep: "transport", render: renderVccSettings }],
  ["npm:@narumitw/pi-goal", { dep: "transport", render: renderGoalSettings }],
  ["git:github.com/jonjonrankin/pi-caveman", { dep: "transport", render: renderCavemanSettings }],
  ["npm:pi-cache-optimizer", { dep: "transport", render: renderCacheOptimizerSettings }],
  ["npm:pi-lens", { dep: "transport", render: renderLensSettings }],
  ["npm:@narumitw/pi-plan-mode", { dep: "configGateway", render: renderPlanModeSettings }],
  ["npm:pi-web-access", { dep: "configGateway", render: renderWebAccessSettings }],
]);

/** datarx-safety-guard-pi (git source) reaches `pi list` as a bare ssh URL or
 * the normalized git: form — matched by suffix (2026-09-21 design spec). */
const SAFETY_GUARD_RENDERER = { dep: "configGateway", render: renderSafetyGuardSettings };
/** datarx-essential is installed from a local path, so `pi list` reports a
 * path ending in `datarx-essential` (or `datarx-essential.git`) — matched by
 * suffix like the safety guard, never by an exact source string. */
const DATARX_ESSENTIAL_RENDERER = { dep: "transport", render: renderDatarxEssentialSettings };
const DATARX_ESSENTIAL_SOURCE = /(^|[/\\])datarx-essential(\.git)?$/;
/** Host control-plane config ops. `transport.<method>()` resolves with the
 * op's payload and rejects on failure, while these renderers check a plain
 * `ok` flag and read the payload's fields at the top level. Translate once
 * here so no renderer has to know which plane it rides (2026-09-21: they were
 * fed the raw payload and every host-plane page rendered "load failed"). */
const HOST_CONFIG_METHODS = [
  "getAskUserConfig",
  "setAskUserConfig",
  "getTodoConfig",
  "setTodoConfig",
  "getPonytailConfig",
  "setPonytailConfig",
  "getVccConfig",
  "setVccConfig",
  "getGoalConfig",
  "setGoalConfig",
  "getCavemanConfig",
  "setCavemanConfig",
  "getCacheOptimizerConfig",
  "setCacheOptimizerConfig",
  "getLensConfig",
  "setLensConfig",
  "getBraveSearchConfig",
  "setBraveSearchConfig",
  "getTavilySearchConfig",
  "setTavilySearchConfig",
];

function withOkFlag(transport) {
  if (!transport) return transport;
  const wrapped = Object.create(transport);
  for (const method of HOST_CONFIG_METHODS) {
    if (typeof transport[method] !== "function") continue;
    wrapped[method] = (...args) =>
      Promise.resolve()
        .then(() => transport[method](...args))
        .then(
          (payload) =>
            payload && typeof payload === "object"
              ? { ...payload, ok: true }
              : { ok: true, data: payload },
          (error) => ({ ok: false, error: error?.message ?? String(error) }),
        );
  }
  return wrapped;
}

function findSettingsRenderer(source) {
  return (
    SETTINGS_RENDERERS.get(source) ??
    (DATARX_ESSENTIAL_SOURCE.test(source) ? DATARX_ESSENTIAL_RENDERER : undefined) ??
    (source.endsWith("datarx-safety-guard-pi.git") ? SAFETY_GUARD_RENDERER : undefined)
  );
}

/** A renderer that throws (missing transport method, dead gateway) must leave
 * a row on the detail page — never an unhandled rejection that takes down the
 * host page and the test runner with it. */
function appendSettingsFailure(detailEl, error) {
  const row = document.createElement("div");
  row.className = "pkg-ext-settings pkg-ext-error";
  row.textContent = String(error?.message || error);
  detailEl.append(row);
}

export function renderExtensionSettings(detailEl, pkg, { configGateway, transport } = {}) {
  const source = typeof pkg?.source === "string" ? pkg.source : "";
  const entry = findSettingsRenderer(source);
  if (!entry) return;
  const dependency = entry.dep === "configGateway" ? configGateway : withOkFlag(transport);
  if (!dependency) return;
  try {
    Promise.resolve(entry.render(detailEl, pkg, dependency)).catch((error) =>
      appendSettingsFailure(detailEl, error),
    );
  } catch (error) {
    appendSettingsFailure(detailEl, error);
  }
}

/**
 * rpiv-ask-user-question: questionnaire overlay collapse shortcut. Single
 * field, host control ops (transport-only, works at landing); per-render
 * config reads in-package make edits immediate.
 */
async function renderAskUserSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionAskUser.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionAskUser.hint");
  section.appendChild(hint);

  const keyInput = document.createElement("input");
  keyInput.type = "text";
  keyInput.placeholder = "ctrl+]";
  keyInput.spellcheck = false;
  const keyError = document.createElement("span");
  keyError.className = "pkg-ext-notice";
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(
    fieldRow(t("settings.extensionAskUser.collapseKeyLabel"), keyInput, keyError),
    status,
  );
  detailEl.appendChild(section);

  const result = await transport
    .getAskUserConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  if (typeof result.values?.collapseKey === "string") keyInput.value = result.values.collapseKey;

  keyInput.addEventListener("change", async () => {
    keyError.textContent = "";
    const raw = keyInput.value.trim();
    const saved = await transport
      .setAskUserConfig({ key: "collapseKey", value: raw === "" ? null : raw })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      keyError.textContent = saved.error || "save failed";
      return;
    }
    if (typeof saved.values?.collapseKey === "string") keyInput.value = saved.values.collapseKey;
    status.textContent = t("settings.saved");
  });
}

/**
 * rpiv-todo: overlay line budget + collapse shortcut. Host control ops
 * (transport-only, works at landing); the package re-reads its config on
 * every render, so edits apply immediately — the hint says so.
 */
async function renderTodoSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionTodo.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionTodo.hint");
  section.appendChild(hint);

  const lineInput = document.createElement("input");
  lineInput.type = "number";
  lineInput.min = "3";
  lineInput.placeholder = "12";
  const keyInput = document.createElement("input");
  keyInput.type = "text";
  keyInput.placeholder = "ctrl+shift+t";
  keyInput.spellcheck = false;
  const lineError = document.createElement("span");
  lineError.className = "pkg-ext-notice";
  const keyError = document.createElement("span");
  keyError.className = "pkg-ext-notice";

  const lineRow = fieldRow(t("settings.extensionTodo.maxLinesLabel"), lineInput, lineError);
  const keyRow = fieldRow(t("settings.extensionTodo.collapseKeyLabel"), keyInput, keyError);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(lineRow, keyRow, status);
  detailEl.appendChild(section);

  const result = await transport
    .getTodoConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const values = result.values ?? {};
  if (typeof values.maxWidgetLines === "number") lineInput.value = String(values.maxWidgetLines);
  if (typeof values.collapseKey === "string") keyInput.value = values.collapseKey;

  lineInput.addEventListener("change", async () => {
    lineError.textContent = "";
    const raw = lineInput.value.trim();
    const value = raw === "" ? null : Number.parseInt(raw, 10);
    if (value !== null && (!Number.isInteger(value) || value < 3)) {
      lineError.textContent = t("settings.extensionTodo.maxLinesError");
      return;
    }
    const saved = await transport
      .setTodoConfig({ key: "maxWidgetLines", value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      lineError.textContent = saved.error || "save failed";
      return;
    }
    status.textContent = t("settings.saved");
  });

  keyInput.addEventListener("change", async () => {
    keyError.textContent = "";
    const raw = keyInput.value.trim();
    const value = raw === "" ? null : raw;
    const saved = await transport
      .setTodoConfig({ key: "collapseKey", value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      // The host validator mirrors the package grammar; surface it verbatim.
      keyError.textContent = saved.error || "save failed";
      return;
    }
    if (typeof saved.values?.collapseKey === "string") keyInput.value = saved.values.collapseKey;
    status.textContent = t("settings.saved");
  });
}

/**
 * Advisor: reviewer model + reasoning effort, save-on-change. The GUI's
 * effect ceiling is the next session_start (advisor re-reads the file per
 * session); the hint line keeps that expectation honest.
 */
async function renderAdvisorSettings(detailEl, _pkg, configGateway) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionAdvisor.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionAdvisor.hint");
  section.appendChild(hint);

  const modelSelect = document.createElement("select");
  const modelRow = fieldRow(t("settings.extensionAdvisor.modelLabel"), modelSelect);
  const effortSelect = document.createElement("select");
  const effortNotice = document.createElement("span");
  effortNotice.className = "pkg-ext-notice";
  const effortRow = fieldRow(
    t("settings.extensionAdvisor.effortLabel"),
    effortSelect,
    effortNotice,
  );
  const status = document.createElement("div");
  status.className = "pkg-ext-status";

  section.append(modelRow, effortRow, status);
  detailEl.appendChild(section);

  const result = await configGateway
    .call("advisor.config.get")
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = String(result.error ?? "load failed");
    return;
  }
  const { modelKey, effort, models } = result.data;
  const levelsByKey = new Map(models.map((m) => [m.key, m.levels]));

  const offOption = document.createElement("option");
  offOption.value = "";
  offOption.textContent = t("settings.extensionAdvisor.off");
  modelSelect.appendChild(offOption);
  // Same picker list as the composer (enabled + scoped); the op still owns the
  // per-model effort levels, so the two lists share one source of truth.
  const choices = await loadModelChoices(configGateway);
  appendModelOptions(modelSelect, choices);
  noteWhenCatalogUnavailable(section, choices);
  // A stored model outside the available list stays visible as its raw key so
  // the select never silently displays blank; the user can re-point or disable.
  if (modelKey && ![...modelSelect.options].some((o) => o.value === modelKey)) {
    const stale = document.createElement("option");
    stale.value = modelKey;
    stale.textContent = modelKey;
    modelSelect.appendChild(stale);
  }
  modelSelect.value = modelKey ?? "";

  function rebuildEffortOptions() {
    effortSelect.replaceChildren();
    const offEffort = document.createElement("option");
    offEffort.value = "";
    offEffort.textContent = t("settings.extensionAdvisor.effortOff");
    effortSelect.appendChild(offEffort);
    for (const level of levelsByKey.get(modelSelect.value) ?? []) {
      const option = document.createElement("option");
      option.value = level;
      option.textContent = level;
      effortSelect.appendChild(option);
    }
    effortSelect.disabled = !modelSelect.value;
  }
  rebuildEffortOptions();
  effortSelect.value = effort ?? "";
  // Last confirmed-saved pair — a failed save rolls the selects back here so
  // a write failure never changes what the controls display.
  let lastSaved = { modelKey: modelSelect.value, effort: effortSelect.value };

  async function save() {
    const attempted = { modelKey: modelSelect.value, effort: effortSelect.value };
    const saved = await configGateway
      .call("advisor.config.set", {
        modelKey: attempted.modelKey || null,
        effort: attempted.effort || null,
      })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (saved.ok) {
      lastSaved = attempted;
      status.textContent = t("settings.extensionAdvisor.saved");
      return;
    }
    modelSelect.value = lastSaved.modelKey;
    rebuildEffortOptions();
    effortSelect.value = lastSaved.effort;
    effortNotice.textContent = "";
    status.textContent = t("settings.extensionAdvisor.saveFailed", {
      message: String(saved.error ?? "save failed"),
    });
  }

  modelSelect.addEventListener("change", () => {
    const previousEffort = effortSelect.value;
    rebuildEffortOptions();
    // replaceChildren resets the select to its first option — restore the
    // previous effort when the new model still supports it, else reset to off.
    if (previousEffort && (levelsByKey.get(modelSelect.value) ?? []).includes(previousEffort)) {
      effortSelect.value = previousEffort;
      effortNotice.textContent = "";
    } else {
      effortSelect.value = "";
      effortNotice.textContent = t("settings.extensionAdvisor.effortReset");
    }
    void save();
  });
  effortSelect.addEventListener("change", () => {
    effortNotice.textContent = "";
    void save();
  });
}

function fieldRow(labelText, control, trailing) {
  // General-page row contract: label left, control (+ trailing) right.
  const row = document.createElement("div");
  row.className = "settings-row";
  const label = document.createElement("span");
  label.className = "settings-label";
  label.textContent = labelText;
  const controls = document.createElement("span");
  controls.className = "pkg-ext-controls";
  controls.append(control);
  if (trailing) controls.appendChild(trailing);
  row.append(label, controls);
  return row;
}

const FFF_MODES = ["tools-and-ui", "tools-only", "override"];
const FFF_TOGGLES = [
  "enableFsRootScanning",
  "enableHomeDirScanning",
  "warnOnHomeDirScan",
  "followSymlinks",
];

/** pi-fff: startup config read once at module load — every write needs a
 * Picot restart to apply (the fixed hint says so). Schema is
 * additionalProperties:false; the host op guarantees a schema-clean file. */
async function renderFffSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  detailEl.appendChild(section);
  await buildFffSection(section, transport);
}

async function buildFffSection(section, transport) {
  section.replaceChildren();
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionFff.title");
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionFff.hint");
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(title, hint, status);

  let data;
  try {
    data = await transport.getFffConfig();
  } catch (error) {
    status.textContent = error?.message ?? String(error);
    return;
  }
  const { envShadowed, flagShadowed, shadowNames, invalid } = data;
  // UI state owns a copy — the op payload (and any fixture holding it) must
  // never be mutated by control handlers.
  const values = { ...(data.values ?? {}) };
  const shadowed = new Set([...(envShadowed ?? []), ...(flagShadowed ?? [])]);

  async function setKey(key, value) {
    try {
      await transport.setFffConfig({ key, value });
      status.textContent = t("settings.extensionFff.saved");
      return true;
    } catch (error) {
      status.textContent = t("settings.extensionFff.saveFailed", {
        message: error?.message ?? String(error),
      });
      return false;
    }
  }

  if (invalid) {
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    error.textContent = `${t("settings.extensionFff.invalidConfig")} ${invalid.reason ?? ""}`;
    const reset = document.createElement("button");
    reset.type = "button";
    reset.className = "pkg-ext-btn-danger";
    reset.textContent = t("settings.extensionFff.reset");
    let armed = false;
    let disarmTimer = 0;
    reset.addEventListener("click", async () => {
      if (!armed) {
        // Two-click inline confirm: destructive reset requires intent twice.
        armed = true;
        reset.textContent = t("settings.extensionFff.resetConfirm");
        clearTimeout(disarmTimer);
        disarmTimer = setTimeout(() => {
          armed = false;
          reset.textContent = t("settings.extensionFff.reset");
        }, 3000);
        return;
      }
      try {
        await transport.setFffConfig({ reset: true });
        await buildFffSection(section, transport);
      } catch (error) {
        status.textContent = t("settings.extensionFff.saveFailed", {
          message: error?.message ?? String(error),
        });
      }
    });
    error.appendChild(reset);
    section.appendChild(error);
    return;
  }

  function badgeFor(field) {
    if (!shadowed.has(field)) return null;
    const badge = document.createElement("span");
    badge.className = "pkg-ext-badge";
    badge.textContent = t("settings.extensionFff.shadowBadge", {
      name: shadowNames?.[field] ?? field,
    });
    return badge;
  }

  // Mode: 3-way segmented control + a one-line description of the active mode.
  const modeRow = document.createElement("div");
  modeRow.className = "settings-row";
  const modeLabel = document.createElement("span");
  modeLabel.className = "settings-label";
  modeLabel.textContent = t("settings.extensionFff.modeLabel");
  const segment = document.createElement("div");
  segment.className = "pkg-ext-segment";
  const modeBadge = badgeFor("mode");
  const desc = document.createElement("div");
  desc.className = "pkg-ext-desc";
  function renderDesc() {
    desc.textContent = t(`settings.extensionFff.modeDesc.${values.mode}`);
  }
  for (const mode of FFF_MODES) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `pkg-ext-segment-btn${values.mode === mode ? " is-on" : ""}`;
    btn.textContent = t(`settings.extensionFff.mode.${mode}`);
    btn.disabled = shadowed.has("mode");
    btn.addEventListener("click", async () => {
      if (values.mode === mode) return;
      if (await setKey("mode", mode)) {
        values.mode = mode;
        for (const other of segment.children) other.classList.remove("is-on");
        btn.classList.add("is-on");
        renderDesc();
      }
    });
    segment.appendChild(btn);
  }
  renderDesc();
  const modeControls = document.createElement("span");
  modeControls.className = "pkg-ext-controls";
  modeControls.append(segment);
  if (modeBadge) modeControls.appendChild(modeBadge);
  modeRow.append(modeLabel, modeControls);
  section.append(modeRow, desc);

  // Four boolean toggles — extensions-page switch pattern.
  for (const field of FFF_TOGGLES) {
    const row = document.createElement("div");
    row.className = "settings-row";
    const label = document.createElement("span");
    label.className = "settings-label";
    label.textContent = t(`settings.extensionFff.${field}`);
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `pkg-manager-toggle${values[field] ? " is-on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(Boolean(values[field])));
    toggle.setAttribute("aria-label", t(`settings.extensionFff.${field}`));
    toggle.appendChild(document.createElement("span"));
    toggle.disabled = shadowed.has(field);
    toggle.addEventListener("click", async () => {
      const next = !values[field];
      if (await setKey(field, next)) {
        values[field] = next;
        toggle.classList.toggle("is-on", next);
        toggle.setAttribute("aria-checked", String(next));
      }
    });
    const controls = document.createElement("span");
    controls.className = "pkg-ext-controls";
    controls.append(toggle);
    const badge = badgeFor(field);
    if (badge) controls.appendChild(badge);
    row.append(label, controls);
    section.appendChild(row);
  }

  // Advanced: the two db paths collapse behind a native disclosure.
  const advanced = document.createElement("details");
  advanced.className = "pkg-ext-advanced";
  const summary = document.createElement("summary");
  summary.textContent = t("settings.extensionFff.advanced");
  advanced.appendChild(summary);
  for (const field of ["frecencyDbPath", "historyDbPath"]) {
    const input = document.createElement("input");
    input.type = "text";
    input.value = typeof values[field] === "string" ? values[field] : "";
    input.placeholder = t("settings.extensionFff.dbManaged");
    input.disabled = shadowed.has(field);
    const row = fieldRow(t(`settings.extensionFff.${field}`), input, badgeFor(field));
    input.addEventListener("change", () => {
      // Empty input clears the key back to the fff-managed default.
      void setKey(field, input.value.trim() || null);
    });
    advanced.appendChild(row);
  }
  section.appendChild(advanced);
}

/**
 * Ponytail: default mode + two visibility switches. Host control ops
 * (transport-only, landing-capable); env-shaded keys disable with a badge
 * naming the exact variable (fff pattern). Effect ceiling: new sessions.
 */
async function renderPonytailSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionPonytail.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionPonytail.hint");
  section.appendChild(hint);

  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.appendChild(status);
  detailEl.appendChild(section);

  const result = await transport
    .getPonytailConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const shadowed = new Set(result.envShadowed ?? []);
  const badgeFor = (field) => {
    if (!shadowed.has(field)) return null;
    const badge = document.createElement("span");
    badge.className = "pkg-ext-badge";
    badge.textContent = t("settings.extensionFff.shadowBadge", {
      name: result.shadowNames?.[field] ?? field,
    });
    return badge;
  };

  // Mode: 3-way segmented (lite/full/ultra); "use default" clears the key.
  const modes = ["lite", "full", "ultra"];
  const segment = document.createElement("div");
  segment.className = "pkg-ext-segment";
  const storedMode = typeof result.defaultMode === "string" ? result.defaultMode : null;
  const modeLabel = document.createElement("span");
  modeLabel.className = "settings-label";
  modeLabel.textContent = t("settings.extensionPonytail.modeLabel");
  const modeRow = document.createElement("div");
  modeRow.className = "settings-row";
  for (const mode of modes) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `pkg-ext-segment-btn${storedMode === mode ? " is-on" : ""}`;
    btn.textContent = t(`settings.extensionPonytail.mode_${mode}`);
    btn.disabled = shadowed.has("defaultMode");
    btn.addEventListener("click", async () => {
      const saved = await transport
        .setPonytailConfig({ key: "defaultMode", value: mode })
        .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
      if (!saved.ok) {
        status.textContent = saved.error || "save failed";
        return;
      }
      for (const other of segment.children) other.classList.remove("is-on");
      btn.classList.add("is-on");
      status.textContent = t("settings.saved");
    });
    segment.appendChild(btn);
  }
  const clearBtn = document.createElement("button");
  clearBtn.type = "button";
  clearBtn.className = "pkg-ext-clear-btn";
  clearBtn.textContent = t("settings.extensionPonytail.useDefault");
  clearBtn.disabled = shadowed.has("defaultMode");
  clearBtn.addEventListener("click", async () => {
    const saved = await transport
      .setPonytailConfig({ key: "defaultMode", value: null })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    for (const other of segment.children) other.classList.remove("is-on");
    status.textContent = t("settings.saved");
  });
  const modeControls = document.createElement("span");
  modeControls.className = "pkg-ext-controls";
  modeControls.append(segment, clearBtn);
  const modeBadge = badgeFor("defaultMode");
  if (modeBadge) modeControls.append(modeBadge);
  modeRow.append(modeLabel, modeControls);
  section.append(modeRow);

  // Two boolean switches — extensions-page switch pattern.
  for (const field of ["quietStartup", "hideStatus"]) {
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `settings-toggle${result[field] === true ? " on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(result[field] === true));
    toggle.disabled = shadowed.has(field);
    toggle.addEventListener("click", async () => {
      const next = !(toggle.getAttribute("aria-checked") === "true");
      const saved = await transport
        .setPonytailConfig({ key: field, value: next })
        .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
      if (!saved.ok) {
        status.textContent = saved.error || "save failed";
        return;
      }
      toggle.classList.toggle("on", next);
      toggle.setAttribute("aria-checked", String(next));
      status.textContent = t("settings.saved");
    });
    const label = document.createElement("span");
    label.className = "settings-label";
    label.textContent = t(`settings.extensionPonytail.${field}Label`);
    const row = document.createElement("div");
    row.className = "settings-row";
    const controls = document.createElement("span");
    controls.className = "pkg-ext-controls";
    controls.append(toggle);
    const badge = badgeFor(field);
    if (badge) controls.append(badge);
    row.append(label, controls);
    section.append(row);
  }
}

/**
 * pi-vcc: four compaction-behavior booleans with load-bearing descriptions.
 * Host control ops (transport-only, landing-capable); PI_VCC_CONFIG_PATH
 * relocates the file out of Picot's write scope — that state renders the
 * whole section read-only with one badge.
 */
async function renderVccSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionVcc.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionVcc.hint");
  section.appendChild(hint);

  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.appendChild(status);
  detailEl.appendChild(section);

  const result = await transport
    .getVccConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  if (result.relocatedByEnv) {
    const badge = document.createElement("span");
    badge.className = "pkg-ext-badge";
    badge.textContent = t("settings.extensionVcc.relocatedBadge", {
      name: "PI_VCC_CONFIG_PATH",
    });
    section.appendChild(badge);
  }
  const readOnly = Boolean(result.relocatedByEnv);

  const fields = [
    "overrideDefaultCompaction",
    "smartKeepTail",
    "continueAfterThresholdCompact",
    "debug",
  ];
  for (const field of fields) {
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `settings-toggle${result.values?.[field] === true ? " on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(result.values?.[field] === true));
    toggle.disabled = readOnly;
    toggle.addEventListener("click", async () => {
      const next = !(toggle.getAttribute("aria-checked") === "true");
      const saved = await transport
        .setVccConfig({ key: field, value: next })
        .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
      if (!saved.ok) {
        status.textContent = saved.error || "save failed";
        return;
      }
      toggle.classList.toggle("on", next);
      toggle.setAttribute("aria-checked", String(next));
      status.textContent = t("settings.saved");
    });
    const label = document.createElement("span");
    label.className = "settings-label settings-label-stack";
    const main = document.createElement("span");
    main.className = "settings-label-main";
    main.textContent = t(`settings.extensionVcc.${field}Label`);
    const sub = document.createElement("span");
    sub.className = "settings-label-sub";
    sub.textContent = t(`settings.extensionVcc.${field}Desc`);
    label.append(main, sub);
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, toggle);
    section.append(row);
  }
}

/**
 * pi-goal: continuation limits + rpc gate. Host control ops
 * (transport-only, landing-capable). The package normalizes the whole file
 * strictly, so an invalid pre-existing file renders read-only with a
 * two-click reset to the known-good defaults document (fff pattern).
 */
async function renderGoalSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";

  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionGoal.title");
  section.appendChild(title);

  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionGoal.hint");
  section.appendChild(hint);

  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.appendChild(status);
  detailEl.appendChild(section);

  const result = await transport
    .getGoalConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  if (result.invalid) {
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    error.textContent = result.invalid.reason || "invalid config";
    const resetBtn = document.createElement("button");
    resetBtn.type = "button";
    resetBtn.className = "pkg-ext-clear-btn";
    resetBtn.textContent = t("settings.extensionGoal.reset");
    let armed = false;
    resetBtn.addEventListener("click", async () => {
      if (!armed) {
        armed = true;
        resetBtn.textContent = t("settings.extensionGoal.resetConfirm");
        return;
      }
      const saved = await transport
        .setGoalConfig({ reset: true })
        .catch((error_) => ({ ok: false, error: error_?.message ?? String(error_) }));
      if (!saved.ok) {
        status.textContent = saved.error || "reset failed";
        return;
      }
      // Re-render the section with the fresh defaults.
      section.remove();
      await renderGoalSettings(detailEl, _pkg, transport);
    });
    section.append(error, resetBtn);
    return;
  }

  const settings = result.settings ?? {};
  const rpcToggle = document.createElement("button");
  rpcToggle.type = "button";
  rpcToggle.className = `settings-toggle${settings.rpc?.enabled === true ? " on" : ""}`;
  rpcToggle.setAttribute("role", "switch");
  rpcToggle.setAttribute("aria-checked", String(settings.rpc?.enabled === true));
  rpcToggle.addEventListener("click", async () => {
    const next = !(rpcToggle.getAttribute("aria-checked") === "true");
    const saved = await transport
      .setGoalConfig({ key: "rpc.enabled", value: next })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    rpcToggle.classList.toggle("on", next);
    rpcToggle.setAttribute("aria-checked", String(next));
    status.textContent = t("settings.saved");
  });
  const rpcLabel = document.createElement("span");
  rpcLabel.className = "settings-label settings-label-stack";
  const rpcMain = document.createElement("span");
  rpcMain.className = "settings-label-main";
  rpcMain.textContent = t("settings.extensionGoal.rpcLabel");
  const rpcSub = document.createElement("span");
  rpcSub.className = "settings-label-sub";
  rpcSub.textContent = t("settings.extensionGoal.rpcDesc");
  rpcLabel.append(rpcMain, rpcSub);
  // The package accepts a removed legacy setting and only warns about it
  // (docs/settings.md); mirror that instead of blocking the file.
  if (result.legacyExperimentalGoals) {
    const legacy = document.createElement("p");
    legacy.className = "settings-help";
    legacy.textContent = t("settings.extensionGoal.legacyWarning");
    section.append(legacy);
  }
  const rpcRow = document.createElement("div");
  rpcRow.className = "settings-row";
  rpcRow.append(rpcLabel, rpcToggle);
  section.append(rpcRow);

  for (const field of ["automaticTurns", "noProgressTurns"]) {
    const stored = settings.continuationLimits?.[field];
    const input = document.createElement("input");
    input.type = "number";
    input.min = "1";
    input.value = typeof stored === "number" ? String(stored) : "";
    input.disabled = stored === null;
    const unlimited = document.createElement("input");
    unlimited.type = "checkbox";
    unlimited.checked = stored === null;
    const unlimitedLabel = document.createElement("span");
    unlimitedLabel.className = "settings-label";
    unlimitedLabel.textContent = t("settings.extensionGoal.unlimited");
    unlimited.addEventListener("change", async () => {
      const saved = await transport
        .setGoalConfig({
          key: `continuationLimits.${field}`,
          value: unlimited.checked ? null : Number.parseInt(input.value || "1", 10),
        })
        .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
      if (!saved.ok) {
        status.textContent = saved.error || "save failed";
        unlimited.checked = !unlimited.checked;
        return;
      }
      input.disabled = unlimited.checked;
      status.textContent = t("settings.saved");
    });
    input.addEventListener("change", async () => {
      const turns = Number.parseInt(input.value, 10);
      if (!Number.isInteger(turns) || turns < 1) {
        status.textContent = t("settings.extensionGoal.limitError");
        return;
      }
      const saved = await transport
        .setGoalConfig({ key: `continuationLimits.${field}`, value: turns })
        .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
      if (!saved.ok) {
        status.textContent = saved.error || "save failed";
        return;
      }
      status.textContent = t("settings.saved");
    });
    const label = document.createElement("span");
    label.className = "settings-label";
    label.textContent = t(`settings.extensionGoal.${field}Label`);
    const row = document.createElement("div");
    row.className = "settings-row";
    const controls = document.createElement("span");
    controls.className = "pkg-ext-controls";
    controls.append(input, unlimitedLabel, unlimited);
    row.append(label, controls);
    section.append(row);
  }
}

/**
 * pi-caveman: default level (8-way select) + status toggle. Host control
 * ops (transport-only, landing-capable); level applies to new sessions.
 */
async function renderCavemanSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionCaveman.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionCaveman.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await transport
    .getCavemanConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const levelSelect = document.createElement("select");
  for (const level of [
    "off",
    "lite",
    "full",
    "ultra",
    "wenyan-lite",
    "wenyan",
    "wenyan-ultra",
    "micro",
  ]) {
    const option = document.createElement("option");
    option.value = level;
    option.textContent = level;
    option.selected = result.effective?.defaultLevel === level;
    levelSelect.append(option);
  }
  levelSelect.addEventListener("change", async () => {
    const saved = await transport
      .setCavemanConfig({ key: "defaultLevel", value: levelSelect.value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    status.textContent = t("settings.saved");
  });
  const levelLabel = document.createElement("span");
  levelLabel.className = "settings-label";
  levelLabel.textContent = t("settings.extensionCaveman.levelLabel");
  const levelRow = document.createElement("div");
  levelRow.className = "settings-row";
  levelRow.append(levelLabel, levelSelect);
  section.append(levelRow);

  const statusToggle = document.createElement("button");
  statusToggle.type = "button";
  statusToggle.className = `settings-toggle${result.effective?.showStatus === true ? " on" : ""}`;
  statusToggle.setAttribute("role", "switch");
  statusToggle.setAttribute("aria-checked", String(result.effective?.showStatus === true));
  statusToggle.addEventListener("click", async () => {
    const next = !(statusToggle.getAttribute("aria-checked") === "true");
    const saved = await transport
      .setCavemanConfig({ key: "showStatus", value: next })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    statusToggle.classList.toggle("on", next);
    statusToggle.setAttribute("aria-checked", String(next));
    status.textContent = t("settings.saved");
  });
  const statusLabel = document.createElement("span");
  statusLabel.className = "settings-label";
  statusLabel.textContent = t("settings.extensionCaveman.statusLabel");
  const statusRow = document.createElement("div");
  statusRow.className = "settings-row";
  statusRow.append(statusLabel, statusToggle);
  section.append(statusRow);
}

/**
 * pi-cache-optimizer: footer stats mode (the only writable) + read-only env
 * opt-out rows + read-only omit list. Host control ops, landing-capable.
 */
async function renderCacheOptimizerSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionCacheOptimizer.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionCacheOptimizer.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await transport
    .getCacheOptimizerConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  if (result.invalid) {
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    error.textContent = result.invalid.reason || "invalid config";
    const note = document.createElement("p");
    note.className = "settings-help";
    note.textContent = t("settings.extensionCacheOptimizer.invalidNote");
    section.append(error, note);
    return;
  }

  const modeSelect = document.createElement("select");
  for (const mode of ["total", "session", "process"]) {
    const option = document.createElement("option");
    option.value = mode;
    option.textContent = t(`settings.extensionCacheOptimizer.mode_${mode}`);
    option.selected = result.effectiveFooterMode === mode;
    modeSelect.append(option);
  }
  modeSelect.disabled = result.footerModeSource === "env";
  modeSelect.addEventListener("change", async () => {
    const saved = await transport
      .setCacheOptimizerConfig({ key: "footerMode", value: modeSelect.value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    status.textContent = t("settings.saved");
  });
  const modeLabel = document.createElement("span");
  modeLabel.className = "settings-label";
  modeLabel.textContent = t("settings.extensionCacheOptimizer.footerModeLabel");
  const sourceBadge = document.createElement("span");
  sourceBadge.className = "pkg-ext-badge";
  sourceBadge.textContent = t(
    `settings.extensionCacheOptimizer.source_${result.footerModeSource ?? "default"}`,
  );
  const modeRow = document.createElement("div");
  modeRow.className = "settings-row";
  const modeControls = document.createElement("span");
  modeControls.className = "pkg-ext-controls";
  modeControls.append(modeSelect, sourceBadge);
  modeRow.append(modeLabel, modeControls);
  section.append(modeRow);

  if (Array.isArray(result.omitList) && result.omitList.length > 0) {
    const omitTitle = document.createElement("p");
    omitTitle.className = "settings-help";
    omitTitle.textContent = t("settings.extensionCacheOptimizer.omitTitle", {
      count: result.omitList.length,
    });
    const omitList = document.createElement("p");
    omitList.className = "settings-help";
    omitList.textContent = result.omitList.join(", ");
    section.append(omitTitle, omitList);
  }

  const envTitle = document.createElement("p");
  envTitle.className = "settings-label";
  envTitle.textContent = t("settings.extensionCacheOptimizer.envTitle");
  section.append(envTitle);
  for (const [name, on] of Object.entries(result.envSwitches ?? {})) {
    const row = document.createElement("div");
    row.className = "settings-row";
    const label = document.createElement("span");
    label.className = "settings-label settings-label-sub";
    label.textContent = name;
    const state = document.createElement("span");
    state.className = "pkg-ext-badge";
    state.textContent = on
      ? t("settings.extensionCacheOptimizer.envOn")
      : t("settings.extensionCacheOptimizer.envOff");
    row.append(label, state);
    section.append(row);
  }
}

/**
 * pi-lens: curated toggles grouped by concern, global layer editable with
 * per-key source badges (env/project shadows disable the switch, fff
 * pattern). Host control ops, landing-capable (project tier needs a cwd).
 */
async function renderLensSettings(detailEl, _pkg, transport) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionLens.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionLens.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await transport
    .getLensConfig()
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  if (result.relocatedByEnv) {
    const badge = document.createElement("span");
    badge.className = "pkg-ext-badge";
    badge.textContent = t("settings.extensionLens.relocatedBadge", {
      name: "PI_LENS_CONFIG_PATH",
    });
    section.append(badge);
  }
  if (!result.projectFile) {
    const note = document.createElement("p");
    note.className = "settings-help";
    note.textContent = t("settings.extensionLens.projectHint");
    section.append(note);
  }

  const groups = [
    { label: "runtimeGroup", keys: ["lens.enabled", "lsp.enabled"] },
    {
      label: "feedbackGroup",
      keys: ["format.enabled", "autofix.enabled", "tests.enabled", "delta.enabled"],
    },
    {
      label: "guardGroup",
      keys: [
        "guard.enabled",
        "guard.sharedCheckout",
        "readGuard.enabled",
        "contextInjection.enabled",
      ],
    },
    {
      label: "reportGroup",
      keys: [
        "turnSummary.enabled",
        "actionableWarnings.enabled",
        "actionableWarnings.includeLspCodeActions",
        "actionableWarnings.autoFix.enabled",
        "actionableWarnings.deltaOnly",
        "ui.compactToolLine",
      ],
    },
    {
      label: "analyzerGroup",
      keys: [
        "tools.lazy",
        "analyzers.knip.enabled",
        "analyzers.jscpd.enabled",
        "analyzers.madge.enabled",
        "analyzers.gitleaks.enabled",
        "analyzers.govulncheck.enabled",
        "analyzers.deadCode.enabled",
        "analyzers.complexity.enabled",
      ],
    },
  ];
  const sourceText = { env: "env", project: "project", global: null, default: null };

  for (const group of groups) {
    const groupTitle = document.createElement("p");
    groupTitle.className = "settings-label";
    groupTitle.textContent = t(`settings.extensionLens.${group.label}`);
    section.append(groupTitle);
    for (const key of group.keys) {
      const effective = result.effective?.[key];
      const source = result.sources?.[key];
      const toggle = document.createElement("button");
      toggle.type = "button";
      toggle.className = `settings-toggle${effective === true ? " on" : ""}`;
      toggle.setAttribute("role", "switch");
      toggle.setAttribute("aria-checked", String(effective === true));
      // env/project shadows are read-only surfaces for the global editor.
      toggle.disabled = source === "env" || source === "project";
      toggle.addEventListener("click", async () => {
        const next = !(toggle.getAttribute("aria-checked") === "true");
        const saved = await transport
          .setLensConfig({ key, value: next })
          .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
        if (!saved.ok) {
          status.textContent = saved.error || "save failed";
          return;
        }
        toggle.classList.toggle("on", next);
        toggle.setAttribute("aria-checked", String(next));
        status.textContent = t("settings.saved");
      });
      const label = document.createElement("span");
      label.className = "settings-label settings-label-sub";
      label.textContent = key;
      const row = document.createElement("div");
      row.className = "settings-row";
      const controls = document.createElement("span");
      controls.className = "pkg-ext-controls";
      controls.append(toggle);
      const shadowName = sourceText[source];
      if (shadowName) {
        const badge = document.createElement("span");
        badge.className = "pkg-ext-badge";
        badge.textContent = t("settings.extensionLens.shadowBadge", { source: shadowName });
        controls.append(badge);
      }
      row.append(label, controls);
      section.append(row);
    }
  }

  const advancedTitle = document.createElement("p");
  advancedTitle.className = "settings-label";
  advancedTitle.textContent = t("settings.extensionLens.advancedGroup");
  section.append(advancedTitle);
  const filesInput = document.createElement("input");
  filesInput.type = "number";
  filesInput.min = "1";
  filesInput.value = String(result.effective?.maxProjectFiles ?? 8000);
  filesInput.addEventListener("change", async () => {
    const value = Number.parseInt(filesInput.value, 10);
    if (!Number.isInteger(value) || value < 1) {
      status.textContent = t("settings.extensionLens.limitError");
      return;
    }
    const saved = await transport
      .setLensConfig({ key: "maxProjectFiles", value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return;
    }
    status.textContent = t("settings.saved");
  });
  const filesLabel = document.createElement("span");
  filesLabel.className = "settings-label";
  filesLabel.textContent = t("settings.extensionLens.maxProjectFilesLabel");
  const filesRow = document.createElement("div");
  filesRow.className = "settings-row";
  filesRow.append(filesLabel, filesInput);
  section.append(filesRow);
}

/**
 * Shared .env editor section for the datarx-essential search extensions
 * (brave-search and its twin tavily-search). Each reads <NAME>_API_KEY /
 * <NAME>_RESULT_COUNT from the global Pi agent .env (~/.pi/agent/.env). Host
 * control ops (transport-only, so landing renders it too). The key is a
 * write-only surface: the host returns the tail mask, never the plaintext, so
 * the input starts empty and the status line is the only place a stored key
 * shows. `localePrefix` picks the locale block and `getConfig` / `setConfig`
 * the extension's host ops, so both sections share this one builder.
 */
async function buildSearchEnvSettingsSection(
  detailEl,
  // transport itself is unused: opts injects the extension's get/set methods.
  _transport,
  { localePrefix, getConfig, setConfig },
) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t(`settings.${localePrefix}.title`);
  section.appendChild(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t(`settings.${localePrefix}.hint`);
  section.appendChild(hint);

  const keyInput = document.createElement("input");
  keyInput.type = "password";
  keyInput.spellcheck = false;
  keyInput.autocomplete = "off";
  keyInput.placeholder = t(`settings.${localePrefix}.keyPlaceholder`);

  const countInput = document.createElement("input");
  countInput.type = "number";
  countInput.min = "1";
  countInput.max = "20";
  countInput.placeholder = "5";
  const countNotice = document.createElement("span");
  countNotice.className = "pkg-ext-notice";

  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(
    fieldRow(t(`settings.${localePrefix}.keyLabel`), keyInput),
    fieldRow(t(`settings.${localePrefix}.countLabel`), countInput, countNotice),
    status,
  );
  detailEl.appendChild(section);

  let current = null;
  let message = "";
  function renderStatus() {
    status.replaceChildren();
    const rows = [];
    if (current?.globalKeyMasked) {
      rows.push(
        t(`settings.${localePrefix}.configuredAt`, {
          path: current.globalPath ?? "",
          mask: current.globalKeyMasked,
        }),
      );
    }
    if (!rows.length && !message) {
      status.textContent = t(`settings.${localePrefix}.notConfigured`);
      return;
    }
    for (const line of rows) {
      const row = document.createElement("div");
      row.className = "pkg-ext-status-line";
      row.textContent = line;
      status.appendChild(row);
    }
    if (message) {
      const row = document.createElement("div");
      row.className = "pkg-ext-status-message";
      row.textContent = message;
      status.appendChild(row);
    }
  }
  function say(text) {
    message = text;
    renderStatus();
  }

  const result = await getConfig().catch((error) => ({
    ok: false,
    error: error?.message ?? String(error),
  }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  current = result;
  // `null` (not "") is "nothing typed this session": "" is a real instruction
  // to clear the stored line, so the two states must not collapse. The host
  // never hands back the plaintext, so "did the user touch the field" is the
  // only signal separating an untouched empty input from a clear.
  let lastSaved = { key: null, count: result.defaultCount ?? null };
  let keyTouched = false;
  countInput.value = lastSaved.count === null ? "" : String(lastSaved.count);
  renderStatus();

  function rollback() {
    keyInput.value = lastSaved.key ?? "";
    keyTouched = false;
    countInput.value = lastSaved.count === null ? "" : String(lastSaved.count);
  }

  async function save() {
    countNotice.textContent = "";
    const rawCount = countInput.value.trim();
    let count = null;
    if (rawCount !== "") {
      count = Number(rawCount);
      if (!Number.isInteger(count) || count < 1 || count > 20) {
        // Client-side ceiling mirrors the host validator — nothing is sent.
        countNotice.textContent = t(`settings.${localePrefix}.countError`);
        // Only this field is restored: a key the user pasted but has not
        // saved must survive (a full rollback would wipe the plaintext).
        countInput.value = lastSaved.count === null ? "" : String(lastSaved.count);
        return;
      }
    }
    const typedKey = keyInput.value.trim();
    // An empty field the user never laid hands on says nothing about the
    // stored line; a field they emptied is an explicit clear (null = leave
    // the line alone, "" = remove it).
    let key = null;
    if (typedKey !== "") key = typedKey;
    else if (keyTouched) key = "";
    const attempted = { key, count };
    if (attempted.key === lastSaved.key && attempted.count === lastSaved.count) {
      return;
    }
    // Global-only: the payload carries no scope/cwd, just the changed fields.
    const payload = {};
    if (attempted.key !== lastSaved.key && attempted.key !== null) payload.apiKey = attempted.key;
    if (attempted.count !== lastSaved.count) payload.defaultCount = attempted.count;
    const saved = await setConfig(payload).catch((error) => ({
      ok: false,
      error: error?.message ?? String(error),
    }));
    if (!saved.ok) {
      rollback();
      say(
        t(`settings.${localePrefix}.saveFailed`, {
          message: String(saved.error ?? "save failed"),
        }),
      );
      return;
    }
    lastSaved = {
      // A cleared line leaves nothing stored, so the field is back to its
      // untouched state instead of "an empty value that equals the saved one".
      key: attempted.key === "" ? null : attempted.key,
      count: saved.defaultCount ?? null,
    };
    keyTouched = false;
    current = saved;
    // The input keeps only what the user typed — the stored key is never read
    // back into the DOM, so no plaintext ever round-trips through the UI.
    countInput.value = lastSaved.count === null ? "" : String(lastSaved.count);
    say(t(`settings.${localePrefix}.saved`));
  }

  for (const control of [keyInput, countInput]) {
    control.addEventListener("change", () => {
      countNotice.textContent = "";
      void save();
    });
  }
  // Typing (or pasting) marks the field as handled; a "change" alone on a
  // pristine field must not be read as "clear the stored key".
  keyInput.addEventListener("input", () => {
    keyTouched = true;
  });
}

/**
 * datarx-essential: the brave-search and tavily-search extensions side by
 * side. Both are global-.env twins, so they share one section builder; each
 * calls its own host get/set ops.
 */
async function renderDatarxEssentialSettings(detailEl, _pkg, transport) {
  await buildSearchEnvSettingsSection(detailEl, transport, {
    localePrefix: "extensionBraveSearch",
    getConfig: transport.getBraveSearchConfig.bind(transport),
    setConfig: transport.setBraveSearchConfig.bind(transport),
  });
  await buildSearchEnvSettingsSection(detailEl, transport, {
    localePrefix: "extensionTavily",
    getConfig: transport.getTavilySearchConfig.bind(transport),
    setConfig: transport.setTavilySearchConfig.bind(transport),
  });
}

/**
 * pi-plan-mode: plan/implementation thinking, implementation model
 * (live catalog), retention, export path, safe subcommands, shortcut.
 * Bridge ops via the config gateway — the config runtime makes this render
 * at landing too; effect ceiling is the next session start.
 */
async function renderPlanModeSettings(detailEl, _pkg, configGateway) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionPlanMode.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionPlanMode.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await configGateway
    .call("planMode.config.get")
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const data = result.data ?? {};
  if (data.invalid) {
    // An unreadable file is reported, never rendered as "defaults": the ops
    // refuse to write onto it, so the page must not pretend to edit it.
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    error.textContent = data.invalid.reason || "invalid config";
    const note = document.createElement("p");
    note.className = "settings-help";
    note.textContent = t("settings.extensionPlanMode.invalidNote");
    section.append(error, note);
    return;
  }
  const settings = data.settings ?? {};

  const save = async (key, value) => {
    const saved = await configGateway
      .call("planMode.config.set", { key, value })
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return false;
    }
    status.textContent = t("settings.saved");
    return true;
  };

  const selectRow = (labelKey, options, current, onchange) => {
    const select = document.createElement("select");
    for (const option of options) {
      const el = document.createElement("option");
      el.value = option;
      el.textContent = option;
      el.selected = option === current;
      select.append(el);
    }
    select.addEventListener("change", () => onchange(select.value));
    const label = document.createElement("span");
    label.className = "settings-label";
    label.textContent = t(labelKey);
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, select);
    section.append(row);
    return select;
  };

  const PLAN_LEVELS = ["inherit", "off", "minimal", "low", "medium", "high", "xhigh", "max"];
  const IMPL_LEVELS = PLAN_LEVELS.slice(1);
  selectRow(
    "settings.extensionPlanMode.thinkingLabel",
    PLAN_LEVELS,
    typeof settings.thinkingLevel === "string" ? settings.thinkingLevel : "inherit",
    (value) => void save("thinkingLevel", value),
  );

  // Implementation model: the composer's own picker list (enabled + scoped)
  // plus the follow-plan-model clear row.
  const modelSelect = document.createElement("select");
  const followOption = document.createElement("option");
  followOption.value = "";
  followOption.textContent = t("settings.extensionPlanMode.followPlanModel");
  modelSelect.append(followOption);
  const storedModel =
    typeof settings.defaultImplementationModel === "string"
      ? settings.defaultImplementationModel
      : "";
  const choices = await loadModelChoices(configGateway);
  appendModelOptions(modelSelect, choices);
  if (storedModel && ![...modelSelect.options].some((o) => o.value === storedModel)) {
    const stale = document.createElement("option");
    stale.value = storedModel;
    stale.textContent = storedModel;
    modelSelect.append(stale);
  }
  modelSelect.value = storedModel;
  noteWhenCatalogUnavailable(section, choices);
  modelSelect.addEventListener(
    "change",
    () => void save("defaultImplementationModel", modelSelect.value || null),
  );
  const modelLabel = document.createElement("span");
  modelLabel.className = "settings-label";
  modelLabel.textContent = t("settings.extensionPlanMode.implModelLabel");
  const modelRow = document.createElement("div");
  modelRow.className = "settings-row";
  modelRow.append(modelLabel, modelSelect);
  section.append(modelRow);

  selectRow(
    "settings.extensionPlanMode.implThinkingLabel",
    IMPL_LEVELS,
    typeof settings.defaultImplementationThinkingLevel === "string"
      ? settings.defaultImplementationThinkingLevel
      : "off",
    (value) => void save("defaultImplementationThinkingLevel", value),
  );
  selectRow(
    "settings.extensionPlanMode.retentionLabel",
    ["clear-on-start", "clear-after-first-run", "keep"],
    typeof settings.implementationPlanRetention === "string"
      ? settings.implementationPlanRetention
      : "clear-on-start",
    (value) => void save("implementationPlanRetention", value),
  );

  const exportInput = document.createElement("input");
  exportInput.type = "text";
  exportInput.placeholder = "PLAN.md";
  exportInput.value =
    typeof settings.defaultPlanExportPath === "string" ? settings.defaultPlanExportPath : "";
  exportInput.addEventListener(
    "change",
    () => void save("defaultPlanExportPath", exportInput.value.trim() || null),
  );
  const exportLabel = document.createElement("span");
  exportLabel.className = "settings-label";
  exportLabel.textContent = t("settings.extensionPlanMode.exportPathLabel");
  const exportRow = document.createElement("div");
  exportRow.className = "settings-row";
  exportRow.append(exportLabel, exportInput);
  section.append(exportRow);

  const shortcutInput = document.createElement("input");
  shortcutInput.type = "text";
  shortcutInput.spellcheck = false;
  shortcutInput.value = typeof settings.toggleShortcut === "string" ? settings.toggleShortcut : "";
  shortcutInput.addEventListener(
    "change",
    () => void save("toggleShortcut", shortcutInput.value.trim() || null),
  );
  const shortcutLabel = document.createElement("span");
  shortcutLabel.className = "settings-label";
  shortcutLabel.textContent = t("settings.extensionPlanMode.shortcutLabel");
  const shortcutRow = document.createElement("div");
  shortcutRow.className = "settings-row";
  shortcutRow.append(shortcutLabel, shortcutInput);
  section.append(shortcutRow);

  // Advanced: raw JSON editors for defaultPlanTools + safeSubcommands.
  const advanced = document.createElement("details");
  const summary = document.createElement("summary");
  summary.textContent = t("settings.extensionPlanMode.advancedGroup");
  advanced.append(summary);
  for (const [key, i18nKey] of [
    ["defaultPlanTools", "planToolsLabel"],
    ["safeSubcommands", "safeSubcommandsLabel"],
  ]) {
    const areaLabel = document.createElement("p");
    areaLabel.className = "settings-label";
    areaLabel.textContent = t(`settings.extensionPlanMode.${i18nKey}`);
    const area = document.createElement("textarea");
    area.rows = 4;
    area.spellcheck = false;
    area.value = JSON.stringify(settings[key] ?? (key === "safeSubcommands" ? {} : []), null, 2);
    const notice = document.createElement("span");
    notice.className = "pkg-ext-notice";
    area.addEventListener("change", async () => {
      notice.textContent = "";
      let parsed;
      try {
        parsed = JSON.parse(area.value);
      } catch {
        notice.textContent = t("settings.extensionPlanMode.invalidJson");
        return;
      }
      const ok = await save(key, parsed);
      if (!ok) notice.textContent = status.textContent;
    });
    advanced.append(areaLabel, area, notice);
  }
  section.append(advanced);
}

/**
 * pi-extension-safety-guard: master switch, rule categories, protected
 * paths, context steppers, auto-review model. Bridge ops via the config
 * gateway (landing-capable through the config runtime); per-event reads
 * make edits immediate. Allow stores stay read-only counts.
 */
async function renderSafetyGuardSettings(detailEl, _pkg, configGateway) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionSafetyGuard.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionSafetyGuard.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await configGateway
    .call("safetyGuard.config.get")
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const data = result.data ?? {};
  if (data.invalid) {
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    error.textContent = data.invalid.reason || "invalid config";
    const note = document.createElement("p");
    note.className = "settings-help";
    note.textContent = t("settings.extensionSafetyGuard.invalidNote");
    section.append(error, note);
    return;
  }
  const config = data.config ?? {};
  const readOnly = Boolean(data.relocatedByEnv);
  if (readOnly) {
    const badge = document.createElement("span");
    badge.className = "pkg-ext-badge";
    badge.textContent = t("settings.extensionSafetyGuard.relocatedBadge", {
      name: "PI_SAFETY_GUARD_CONFIG_FILE",
    });
    section.append(badge);
  }

  const post = async (payload) => {
    const saved = await configGateway
      .call("safetyGuard.config.set", payload)
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return false;
    }
    status.textContent = t("settings.saved");
    return true;
  };
  const save = (key, value) => post({ key, value });

  const switchRow = (labelText, key, current) => {
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `settings-toggle${current === true ? " on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(current === true));
    toggle.disabled = readOnly;
    toggle.addEventListener("click", async () => {
      const next = !(toggle.getAttribute("aria-checked") === "true");
      if (await save(key, next)) {
        toggle.classList.toggle("on", next);
        toggle.setAttribute("aria-checked", String(next));
      }
    });
    const label = document.createElement("span");
    label.className = "settings-label settings-label-sub";
    label.textContent = labelText;
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, toggle);
    section.append(row);
    return toggle;
  };

  switchRow(t("settings.extensionSafetyGuard.masterLabel"), "enabled", config.enabled !== false);

  const categoriesTitle = document.createElement("p");
  categoriesTitle.className = "settings-label";
  categoriesTitle.textContent = t("settings.extensionSafetyGuard.categoriesGroup");
  section.append(categoriesTitle);
  for (const category of [
    "git",
    "filesystem",
    "docker",
    "package",
    "system",
    "database",
    "secrets",
  ]) {
    const current = config.categories?.[category] !== false;
    switchRow(
      t(`settings.extensionSafetyGuard.category_${category}`),
      `categories.${category}`,
      current,
    );
  }

  const pathsTitle = document.createElement("p");
  pathsTitle.className = "settings-label";
  pathsTitle.textContent = t("settings.extensionSafetyGuard.protectedPathsGroup");
  section.append(pathsTitle);
  switchRow(
    t("settings.extensionSafetyGuard.protectWrite"),
    "protectedPaths.write",
    config.protectedPaths?.write !== false,
  );
  switchRow(
    t("settings.extensionSafetyGuard.protectEdit"),
    "protectedPaths.edit",
    config.protectedPaths?.edit !== false,
  );

  const stepperRow = (labelText, key, current) => {
    const input = document.createElement("input");
    input.type = "number";
    input.min = "0";
    input.max = "20";
    input.value = String(current ?? 3);
    input.disabled = readOnly;
    input.addEventListener("change", async () => {
      const value = Number.parseInt(input.value, 10);
      if (!Number.isInteger(value) || value < 0 || value > 20) {
        status.textContent = t("settings.extensionSafetyGuard.rangeError");
        return;
      }
      await save(key, value);
    });
    const label = document.createElement("span");
    label.className = "settings-label";
    label.textContent = labelText;
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, input);
    section.append(row);
  };
  stepperRow(
    t("settings.extensionSafetyGuard.contextBefore"),
    "contextLines.before",
    config.contextLines?.before,
  );
  stepperRow(
    t("settings.extensionSafetyGuard.contextAfter"),
    "contextLines.after",
    config.contextLines?.after,
  );

  const autoTitle = document.createElement("p");
  autoTitle.className = "settings-label";
  autoTitle.textContent = t("settings.extensionSafetyGuard.autoReviewGroup");
  section.append(autoTitle);
  switchRow(
    t("settings.extensionSafetyGuard.autoReviewLabel"),
    "autoReview.enabled",
    config.autoReview?.enabled === true,
  );
  // Reviewer model: the composer's own picker list (enabled + scoped), so the
  // guard can only be pointed at a model the session can actually reach.
  const modelSelect = document.createElement("select");
  const unsetOption = document.createElement("option");
  unsetOption.value = "";
  unsetOption.textContent = t("settings.extensionSafetyGuard.modelUnset");
  modelSelect.append(unsetOption);
  const modelChoices = await loadModelChoices(configGateway);
  appendModelOptions(modelSelect, modelChoices);
  const storedModel = config.autoReview?.model;
  const storedKey =
    storedModel?.provider && storedModel?.modelId
      ? `${storedModel.provider}/${storedModel.modelId}`
      : "";
  if (storedKey && ![...modelSelect.options].some((o) => o.value === storedKey)) {
    const stale = document.createElement("option");
    stale.value = storedKey;
    stale.textContent = storedKey;
    modelSelect.append(stale);
  }
  modelSelect.value = storedKey;
  modelSelect.disabled = readOnly;
  modelSelect.addEventListener("change", async () => {
    const raw = modelSelect.value;
    const [provider, ...rest] = raw.split("/");
    const modelId = rest.join("/");
    // Provider + modelId are one package-level value: an `entries` batch
    // keeps them from landing as a mismatched pair (and clears both together).
    await post({
      entries: [
        { key: "autoReview.model.provider", value: raw ? provider : null },
        { key: "autoReview.model.modelId", value: raw ? modelId : null },
      ],
    });
  });
  noteWhenCatalogUnavailable(section, modelChoices);
  const modelLabel = document.createElement("span");
  modelLabel.className = "settings-label";
  modelLabel.textContent = t("settings.extensionSafetyGuard.modelLabel");
  const modelRow = document.createElement("div");
  modelRow.className = "settings-row";
  modelRow.append(modelLabel, modelSelect);
  section.append(modelRow);

  const levelSelect = document.createElement("select");
  for (const level of ["off", "minimal", "low", "medium", "high", "xhigh", "max"]) {
    const option = document.createElement("option");
    option.value = level;
    option.textContent = level;
    option.selected =
      (typeof config.autoReview?.model?.thinkingLevel === "string"
        ? config.autoReview.model.thinkingLevel
        : "off") === level;
    levelSelect.append(option);
  }
  levelSelect.disabled = readOnly;
  levelSelect.addEventListener("change", async () => {
    await save("autoReview.model.thinkingLevel", levelSelect.value);
  });
  const levelLabel = document.createElement("span");
  levelLabel.className = "settings-label";
  levelLabel.textContent = t("settings.extensionSafetyGuard.thinkingLevelLabel");
  const levelRow = document.createElement("div");
  levelRow.className = "settings-row";
  levelRow.append(levelLabel, levelSelect);
  section.append(levelRow);

  const allow = document.createElement("p");
  allow.className = "settings-help";
  allow.textContent = t("settings.extensionSafetyGuard.allowCounts", {
    count: data.allowCounts?.global ?? 0,
  });
  section.append(allow);
}

/**
 * pi-web-access: provider keys (masked transport — previews only, full
 * keys never transit the bridge), proxy/endpoint rows, toggles, answer
 * model. Bridge ops via the config gateway; per-module config cache makes
 * the ceiling a Pi restart. Clearing is a two-click confirm on populated
 * rows (never accidental).
 */
async function renderWebAccessSettings(detailEl, _pkg, configGateway) {
  const section = document.createElement("div");
  section.className = "pkg-ext-settings";
  const title = document.createElement("h4");
  title.className = "pkg-ext-title";
  title.textContent = t("settings.extensionWebAccess.title");
  section.append(title);
  const hint = document.createElement("p");
  hint.className = "settings-help";
  hint.textContent = t("settings.extensionWebAccess.hint");
  section.append(hint);
  const status = document.createElement("div");
  status.className = "pkg-ext-status";
  section.append(status);
  detailEl.append(section);

  const result = await configGateway
    .call("webaccess.config.get")
    .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
  if (!result.ok) {
    status.textContent = result.error || "load failed";
    return;
  }
  const data = result.data ?? {};
  if (data.invalid) {
    const error = document.createElement("div");
    error.className = "pkg-ext-error";
    // The package never quotes file text back (its own text is the secret)
    // — the reason carries no content either.
    error.textContent = data.invalid.reason || "invalid config";
    const note = document.createElement("p");
    note.className = "settings-help";
    note.textContent = t("settings.extensionWebAccess.invalidNote");
    section.append(error, note);
    return;
  }
  const envKeyed = new Set(data.envKeyed ?? []);

  const post = async (payload) => {
    const saved = await configGateway
      .call("webaccess.config.set", payload)
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!saved.ok) {
      status.textContent = saved.error || "save failed";
      return false;
    }
    status.textContent = t("settings.saved");
    return true;
  };
  const save = (key, value) => post({ key, value });

  const secretRow = (key) => {
    const field = data.fields?.[key] ?? { configured: false };
    const input = document.createElement("input");
    input.type = "password";
    input.placeholder = field.configured
      ? t("settings.extensionWebAccess.configuredPreview", { preview: field.preview ?? "" })
      : "";
    input.autocomplete = "off";
    const notice = document.createElement("span");
    notice.className = "pkg-ext-notice";
    const clearBtn = document.createElement("button");
    clearBtn.type = "button";
    clearBtn.className = "pkg-ext-clear-btn";
    clearBtn.textContent = t("settings.extensionWebAccess.clear");
    clearBtn.hidden = !field.configured;
    let armed = false;
    clearBtn.addEventListener("click", async () => {
      if (!armed) {
        armed = true;
        clearBtn.textContent = t("settings.extensionWebAccess.clearConfirm");
        return;
      }
      if (await save(key, null)) {
        clearBtn.hidden = true;
        input.placeholder = "";
      }
    });
    input.addEventListener("change", async () => {
      notice.textContent = "";
      const raw = input.value;
      if (raw === "") return; // save-on-change writes only when non-empty
      const saved = await save(key, raw);
      // The field is the only place the full key exists: clear it either
      // way, so a rejected save never leaves plaintext sitting in the DOM.
      input.value = "";
      if (saved) {
        input.placeholder = t("settings.extensionWebAccess.configuredPreview", {
          preview: raw.slice(-4),
        });
        clearBtn.hidden = false;
        clearBtn.textContent = t("settings.extensionWebAccess.clear");
        armed = false;
      }
    });
    const label = document.createElement("span");
    label.className = "settings-label settings-label-sub";
    label.textContent = key;
    if (envKeyed.has(key)) {
      const badge = document.createElement("span");
      badge.className = "pkg-ext-badge";
      badge.textContent = t("settings.extensionWebAccess.envBadge");
      label.append(" ", badge);
    }
    const row = document.createElement("div");
    row.className = "settings-row";
    const controls = document.createElement("span");
    controls.className = "pkg-ext-controls";
    controls.append(input, clearBtn, notice);
    row.append(label, controls);
    section.append(row);
  };

  const searchTitle = document.createElement("p");
  searchTitle.className = "settings-label";
  searchTitle.textContent = t("settings.extensionWebAccess.searchKeysGroup");
  section.append(searchTitle);
  for (const key of [
    "openaiApiKey",
    "braveApiKey",
    "exaApiKey",
    "perplexityApiKey",
    "geminiApiKey",
    "mistralApiKey",
    "serpapiApiKey",
    "xaiApiKey",
  ]) {
    secretRow(key);
  }

  const extractTitle = document.createElement("p");
  extractTitle.className = "settings-label";
  extractTitle.textContent = t("settings.extensionWebAccess.extractKeysGroup");
  section.append(extractTitle);
  for (const key of [
    "jinaApiKey",
    "firecrawlApiKey",
    "tinyfishApiKey",
    "search1apiApiKey",
    "searchinfinityApiKey",
    "queritApiKey",
    "bochaApiKey",
    "valyuApiKey",
    "anysearchApiKey",
    "datalabApiKey",
    "crawl4aiApiToken",
    "brightdataApiKey",
  ]) {
    secretRow(key);
  }

  const nonSecretTitle = document.createElement("p");
  nonSecretTitle.className = "settings-label";
  nonSecretTitle.textContent = t("settings.extensionWebAccess.endpointsGroup");
  section.append(nonSecretTitle);
  for (const key of [
    "proxy",
    "openaiResponsesUrl",
    "searxngBaseUrl",
    "crawl4aiBaseUrl",
    "brightdataSerpZone",
    "brightdataUnlockerZone",
  ]) {
    const input = document.createElement("input");
    input.type = "text";
    const stored = data.nonSecrets?.[key];
    input.value = typeof stored === "string" ? stored : "";
    input.placeholder = key;
    input.addEventListener("change", () => void save(key, input.value.trim() || null));
    const label = document.createElement("span");
    label.className = "settings-label settings-label-sub";
    label.textContent = key;
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, input);
    section.append(row);
  }
  for (const key of ["allowBrowserCookies", "image.enabled"]) {
    const stored = data.nonSecrets?.[key];
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `settings-toggle${stored === true ? " on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(stored === true));
    toggle.addEventListener("click", async () => {
      const next = !(toggle.getAttribute("aria-checked") === "true");
      if (await save(key, next)) {
        toggle.classList.toggle("on", next);
        toggle.setAttribute("aria-checked", String(next));
      }
    });
    const label = document.createElement("span");
    label.className = "settings-label settings-label-sub";
    label.textContent = key;
    const row = document.createElement("div");
    row.className = "settings-row";
    row.append(label, toggle);
    section.append(row);
  }

  const answerTitle = document.createElement("p");
  answerTitle.className = "settings-label";
  answerTitle.textContent = t("settings.extensionWebAccess.answerModelGroup");
  section.append(answerTitle);
  // Answer model rides the composer's picker list (enabled + scoped), the
  // same one advisor and the other extension pages show.
  const answerSelect = document.createElement("select");
  const answerUnset = document.createElement("option");
  answerUnset.value = "";
  answerUnset.textContent = t("settings.extensionWebAccess.modelUnset");
  answerSelect.append(answerUnset);
  const answerChoices = await loadModelChoices(configGateway);
  appendModelOptions(answerSelect, answerChoices);
  const answer = data.routing?.answerModel ?? {};
  const answerKey = answer.provider && answer.modelId ? `${answer.provider}/${answer.modelId}` : "";
  if (answerKey && ![...answerSelect.options].some((o) => o.value === answerKey)) {
    const stale = document.createElement("option");
    stale.value = answerKey;
    stale.textContent = answerKey;
    answerSelect.append(stale);
  }
  answerSelect.value = answerKey;
  answerSelect.addEventListener("change", async () => {
    const raw = answerSelect.value;
    const [provider, ...rest] = raw.split("/");
    const modelId = rest.join("/");
    // The package only accepts the pair together: one entries batch (and the
    // same rule clears both halves).
    await post({
      entries: [
        { key: "fetch.answerProvider", value: raw ? provider : null },
        { key: "fetch.answerModel", value: raw ? modelId : null },
      ],
    });
  });
  noteWhenCatalogUnavailable(section, answerChoices);
  const answerLabel = document.createElement("span");
  answerLabel.className = "settings-label";
  answerLabel.textContent = t("settings.extensionWebAccess.answerModelLabel");
  const answerRow = document.createElement("div");
  answerRow.className = "settings-row";
  answerRow.append(answerLabel, answerSelect);
  section.append(answerRow);
}

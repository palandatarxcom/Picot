// ABOUTME: Project MCP override detail — name/source plus the three snapshot fields only.
// ABOUTME: A DOM renderer with callbacks; the page owns gateway authority, revisions and drafts.

import { t } from "../i18n.js";

const EXPOSURES = ["codemode", "direct", "deferred", "hidden"];
const EXPOSURE_SET = new Set(EXPOSURES);

/**
 * @typedef {{exposure: string, toolExposureText: string}} McpOverrideDraft
 * @typedef {(options: {
 *   item: Object,
 *   draft: McpOverrideDraft,
 *   onToggle: (intent: {disable: boolean}) => void,
 *   onSave: (values: {enabled: boolean, exposure: string, toolExposure: Object}) => void,
 *   onRemove: () => void,
 *   onDraftChange: (draft: McpOverrideDraft) => void,
 *   status?: Object,
 *   pending?: boolean,
 * }) => HTMLElement} McpOverrideRenderer
 */

/**
 * Override detail: an explicit three-field snapshot of a global server. No
 * connection inputs, no sign-in button and no "inherit" option — dropping the
 * snapshot is the Remove action, not a per-field fallback.
 * @type {McpOverrideRenderer}
 */
export function renderMcpOverrideDetail({
  item,
  draft,
  onToggle,
  onSave,
  onRemove,
  onDraftChange,
  status = null,
  pending = false,
}) {
  const invalid = typeof item.validationError === "string" && item.validationError.length > 0;
  const enabled = Boolean(item.effective?.enabled);
  const wrap = document.createElement("div");
  wrap.className = "mcp-entry";

  const toggleRow = document.createElement("div");
  toggleRow.className = "mcp-toggle-row";
  const toggleLabel = document.createElement("span");
  toggleLabel.className = "mcp-toggle-label";
  toggleLabel.textContent = t("settings.mcp.enable");
  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = `pkg-manager-toggle${enabled ? " is-on" : ""}`;
  toggle.setAttribute("role", "switch");
  toggle.setAttribute("aria-checked", String(enabled));
  toggle.setAttribute("aria-label", t("settings.mcp.enable"));
  toggle.disabled = invalid || pending;
  toggle.appendChild(document.createElement("span"));
  toggle.addEventListener("click", () => {
    // Immediate save of one field: the page keeps unsaved exposure/map drafts.
    onToggle({ disable: enabled });
  });
  toggleRow.append(toggleLabel, toggle);
  wrap.appendChild(toggleRow);

  const head = document.createElement("div");
  head.className = "mcp-entry-head";
  const title = document.createElement("h4");
  title.textContent = item.name;
  head.appendChild(title);
  wrap.appendChild(head);

  const source = document.createElement("div");
  source.className = "mcp-source";
  source.textContent = `${t("settings.mcp.sourceLabel")}: ${item.sourceFile}`;
  wrap.appendChild(source);

  const hint = document.createElement("div");
  hint.className = "mcp-source";
  hint.textContent = t("settings.mcp.override.sourceHint", { name: item.name });
  wrap.appendChild(hint);

  // Authentication belongs to the global server this entry overrides: point
  // there instead of offering a sign-in control on a project-only row.
  if (status?.state === "needs-auth") {
    const authHint = document.createElement("div");
    authHint.className = "mcp-field-help";
    authHint.dataset.hint = "needs-auth";
    authHint.textContent = t("settings.mcp.override.needsAuthHint");
    wrap.appendChild(authHint);
  }

  if (invalid) {
    const error = document.createElement("div");
    error.className = "mcp-group-error";
    error.textContent = item.validationError;
    wrap.appendChild(error);
    const actions = document.createElement("div");
    actions.className = "mcp-form-actions";
    actions.appendChild(removeButton(onRemove));
    wrap.appendChild(actions);
    return wrap;
  }

  const form = document.createElement("form");
  form.className = "mcp-form";
  form.addEventListener("submit", (event) => event.preventDefault());

  const exposureSelect = document.createElement("select");
  for (const value of EXPOSURES) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = t(`settings.mcp.form.exposure_${value}`);
    exposureSelect.appendChild(option);
  }
  exposureSelect.value = EXPOSURE_SET.has(draft.exposure) ? draft.exposure : "codemode";
  exposureSelect.addEventListener("change", () =>
    onDraftChange({ exposure: exposureSelect.value, toolExposureText: mapInput.value }),
  );

  const mapInput = document.createElement("textarea");
  mapInput.rows = 4;
  mapInput.value = draft.toolExposureText;
  mapInput.addEventListener("input", () =>
    onDraftChange({ exposure: exposureSelect.value, toolExposureText: mapInput.value }),
  );

  const help = document.createElement("div");
  help.className = "mcp-field-help";
  help.textContent = t("settings.mcp.override.mapHelp");

  const errorLine = document.createElement("div");
  errorLine.className = "mcp-field-error";
  errorLine.hidden = true;

  const actions = document.createElement("div");
  actions.className = "mcp-form-actions";
  const save = document.createElement("button");
  save.type = "submit";
  save.className = "mcp-btn mcp-btn-primary";
  save.textContent = t("settings.mcp.save");
  save.disabled = pending;
  actions.appendChild(save);
  actions.appendChild(removeButton(onRemove));

  form.addEventListener("submit", () => {
    const parsed = parseMapText(mapInput.value);
    if (typeof parsed === "string") {
      showError(errorLine, parsed);
      return; // keep the draft: the user's text stays in the field
    }
    if (!EXPOSURE_SET.has(exposureSelect.value)) {
      showError(errorLine, t("settings.mcp.override.exposureInvalid"));
      return;
    }
    errorLine.hidden = true;
    onSave({ enabled, exposure: exposureSelect.value, toolExposure: parsed });
  });

  form.append(
    fieldRow(t("settings.mcp.form.exposure"), exposureSelect),
    fieldRow(t("settings.mcp.override.toolExposure"), mapInput),
    help,
    errorLine,
    actions,
  );
  wrap.appendChild(form);
  return wrap;
}

function removeButton(onRemove) {
  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "mcp-btn mcp-btn-danger";
  remove.textContent = t("settings.mcp.override.remove");
  remove.addEventListener("click", () => onRemove());
  return remove;
}

function fieldRow(labelText, control) {
  const row = document.createElement("label");
  row.className = "mcp-field";
  const label = document.createElement("span");
  label.className = "mcp-field-label";
  label.textContent = labelText;
  row.append(label, control);
  return row;
}

function showError(node, text) {
  node.hidden = false;
  node.textContent = text;
}

/** Returns the validated map or an error message; empty text is invalid JSON, not `{}`. */
function parseMapText(text) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return t("settings.mcp.override.mapInvalid");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return t("settings.mcp.override.mapInvalid");
  }
  for (const [tool, exposure] of Object.entries(parsed)) {
    if (!EXPOSURE_SET.has(exposure)) {
      return t("settings.mcp.override.mapValueInvalid", { tool });
    }
  }
  return parsed;
}

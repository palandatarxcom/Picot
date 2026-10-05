// ABOUTME: Settings → MCP page — two native layer tabs (user + project mcp.json), each a master/detail view.
// ABOUTME: Orphaned adapter/shared config layers surface as one-click migration banners into the native files.
// ABOUTME: Live connection state, browser sign-in, and sign-out come from the host `pi mcp` ops.

import { onLocaleChange, t } from "../i18n.js";
import { createMcpLoginDialog } from "./mcp-login-dialog.js";
import { renderMcpOverrideDetail } from "./mcp-override-detail.js";

/**
 * @typedef {{name:string, entry:Object, sourceFile:string, editable:true, enabled:boolean, kind?:string, effective?:Object, identity?:Object, validationError?:string, revision?:string}} McpListEntry
 * @typedef {{id:string, sourceFile:string, missing:string[]}} McpMigrationTarget
 * @typedef {{name:string, scope?:string, state:string, transport?:string, tools?:unknown[], error?:string}} McpServerStatus
 * @typedef {{groups: Record<string, McpListEntry[]>, groupErrors: Record<string, string|undefined>, migrations: McpMigrationTarget[], projectAvailable?: boolean, projectTrusted?: boolean, revisions?: {piGlobal:string, project:string|null}}} McpListData
 * @typedef {{workspaceId:string, sessionId:string, instanceId?:string}} RuntimeTarget
 * @typedef {{context:string, target:RuntimeTarget|null}} McpBinding
 */

// Long master-row error text is summarized; the full message stays on `title`.
const ERROR_SUMMARY_CHARS = 48;

/**
 * Binds the host transport (`WsTransport` control ops) to the login surface this
 * page consumes. Kept here so every host entry (landing + workspace shell)
 * wires MCP sign-in identically, and so the page stays testable with a stub.
 */
export function createMcpHostOps(transport) {
  return {
    start: (name) => transport.mcpLoginStart(name),
    cancel: (operationId) => transport.mcpLoginCancel(operationId),
    status: (operationId) => transport.mcpLoginStatus(operationId),
    logout: (name) => transport.mcpLogout(name),
    serverStatus: (options) => transport.mcpServerStatus(options),
    subscribe: (listener) => transport.onMcpLoginUpdate(listener),
  };
}

export function setupMcpPage({
  masterEl,
  detailEl,
  tabs,
  navItem: _navItem,
  configGateway,
  // Host-plane MCP login surface (WS host_request → `pi mcp login|logout|list`).
  // Absent on transports without the host ops: the page then degrades to the
  // plain config list (no badges, no sign-in buttons).
  mcpLogin = null,
  openExternal = null,
  captionEl = null,
  migrationsEl = null,
  // Workspace identity (workspace + generation) and the live routing triple.
  // Landing passes neither: global-only mode, where project mutations reject.
  getContextKey = () => "mcp-page",
  getRuntimeTarget = null,
}) {
  /** @type {McpListData | null} */
  let data = null;
  /**
   * The immutable context + routing triple `data` was read for. Rendered
   * controls act on this binding, never on whatever target is current now.
   * @type {McpBinding | null}
   */
  let dataBinding = null;
  /** True while the page has no inventory because a stale one was dropped. */
  let refreshRequired = false;
  let activeTab = "piGlobal";
  /** @type {Map<string, {name: string}>} per-tab selection */
  const selections = new Map();
  let mode = "view"; // view | add
  let loadSeq = 0;
  let statusText = "";
  /** @type {{all: McpServerStatus[]} | null} */
  let statuses = null;
  let statusError = "";
  let loginDialog = null;
  let disposed = false;
  /** Unsaved override drafts, keyed by source file + name + revision. */
  const drafts = new Map();
  /** Override name whose enabled switch is waiting for an acknowledgement. */
  let pendingToggle = null;
  let batchPending = false;
  let statusSeq = 0;
  let statusGeneration = 0;
  let statusDiagnostics = { errors: [], note: undefined };
  let lastContextKey = null;

  const unsubscribeLocale = onLocaleChange(() => render());

  function scopeLabel(scope) {
    return t(`settings.mcp.groups.${scope}`);
  }

  function groupEntries() {
    return data?.groups[activeTab] ?? [];
  }

  function findEntry(name) {
    return groupEntries().find((e) => e.name === name) ?? null;
  }

  async function load() {
    const seq = ++loadSeq;
    const binding = captureMcpBindingSafe();
    const result = await call("mcp_list_servers", {}, binding);
    if (seq !== loadSeq) return;
    // A reload started in another workspace must not repaint this one.
    if (binding && !isMcpBindingCurrent(binding)) return;
    data = result.ok ? result.data : null;
    dataBinding = data ? binding : null;
    refreshRequired = false;
    statusText = result.ok ? "" : String(result.error ?? t("settings.mcp.status.unavailable"));
    if (selected()) {
      const name = selected().name;
      if (!groupEntries().some((e) => e.name === name)) selections.delete(activeTab);
    }
    ensureSelection();
    render();
  }

  async function activate() {
    syncContext();
    await load();
    await loadStatus();
  }

  /**
   * Workspace change (or first activation) drops every piece of per-workspace
   * UI state: rows, drafts, diagnostics and in-flight status results.
   */
  function syncContext() {
    const key = getContextKey();
    if (key === lastContextKey) return;
    lastContextKey = key;
    data = null;
    dataBinding = null;
    refreshRequired = false;
    selections.clear();
    drafts.clear();
    mode = "view";
    pendingToggle = null;
    statusText = "";
    invalidateStatus();
  }

  /**
   * Live per-server state from the host (`pi mcp list --json`, 60s host-side
   * TTL). Queried on page activation and after every sign-in/sign-out — never
   * polled: an in-flight login polls through `mcp_login_status` instead.
   *
   * A `refresh: true` query must be *issued* before any await the page teardown
   * can interrupt (the inventory reload): it invalidates the host-side cache,
   * so it has to leave the page even when the reply never repaints anything.
   */
  async function loadStatus(options) {
    if (!mcpLogin || disposed) return;
    const context = getContextKey();
    const generation = statusGeneration;
    const seq = ++statusSeq;
    const result = await Promise.resolve()
      .then(() => mcpLogin.serverStatus(options))
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    // Only the newest query of the current context may publish: a slow reply
    // from before a save must not restore the state that save invalidated.
    if (
      disposed ||
      context !== getContextKey() ||
      generation !== statusGeneration ||
      seq !== statusSeq
    ) {
      return;
    }
    if (result?.ok) {
      statuses = indexStatus(result.servers);
      statusError = "";
      statusDiagnostics = { errors: result.errors ?? [], note: result.note };
    } else {
      statuses = null;
      statusError = String(result?.error ?? t("settings.mcp.status.unavailable"));
      statusDiagnostics = { errors: [], note: undefined };
    }
    render();
  }

  /** Any accepted configuration write invalidates the CLI disk view. */
  function invalidateStatus() {
    statusGeneration += 1;
    statusSeq += 1;
    statuses = null;
    statusError = "";
    statusDiagnostics = { errors: [], note: undefined };
  }

  function indexStatus(servers) {
    return {
      all: (Array.isArray(servers) ? servers : []).filter(
        (server) => server && typeof server.name === "string",
      ),
    };
  }

  function scopeOf(scope) {
    return String(scope).toLowerCase().includes("project") ? "project" : "piGlobal";
  }

  /**
   * Status identity, not name matching. Pi reports a project override as the
   * effective *global* server (global scope + global source) plus the project
   * override path, so an override row only accepts a report that names that
   * exact triple. A full definition accepts a report from its own file in its
   * own scope. Invalid rows, lost bases and other workspaces stay unknown
   * instead of borrowing a same-named report.
   */
  function statusForItem(item) {
    const reports = statuses?.all ?? [];
    if (item.kind === "override") {
      const identity = item.identity;
      if (!identity?.source || !identity?.override) return null;
      return (
        reports.find(
          (report) =>
            report.name === item.name &&
            scopeOf(report.scope) === "piGlobal" &&
            report.source === identity.source &&
            report.override === identity.override,
        ) ?? null
      );
    }
    if (item.kind === "invalid") return null;
    return (
      reports.find(
        (report) =>
          report.name === item.name &&
          report.source === item.sourceFile &&
          scopeOf(report.scope) === activeTab,
      ) ?? null
    );
  }

  function transportOf(item, status) {
    if (status?.transport) return String(status.transport);
    // pi 0.99 has no legacy SSE transport: a configured `url` is HTTP, and
    // everything else is a local stdio command.
    return typeof item.entry?.url === "string" && item.entry.url ? "http" : "stdio";
  }

  /** The adapter/shared copy runs only after the user clicks a migration
   * banner's action; the list op detects but never writes. */
  async function migrate(target, renderScope) {
    let binding;
    try {
      binding = actionBinding(renderScope);
    } catch (error) {
      setStatus(error?.message ?? String(error));
      render();
      return;
    }
    const result = await call("mcp_migrate_adapter_config", { target }, binding);
    if (!isMcpBindingCurrent(binding)) return;
    const migrated = result.ok ? (result.data?.migrated ?? []) : [];
    setStatus(
      result.ok && migrated.length > 0
        ? t("settings.mcp.saved")
        : String(result.data?.error ?? result.error ?? t("settings.mcp.migrationFailed")),
    );
    await load();
  }

  /** Native MCP ships with every Pi 0.99+ runtime, so the page is always
   * available; kept as an async method for the landing nav wiring. */
  async function refreshAvailability() {
    return true;
  }

  /**
   * A rendered control belongs to the context + routing triple it was drawn
   * for. Acting on a stale control must reject instead of sending the click to
   * whatever workspace is current now; the action binding is captured only
   * after that check.
   */
  function actionBinding(renderScope) {
    if (renderScope && !isMcpBindingCurrent(renderScope)) {
      throw new Error(t("settings.mcp.targetChanged"));
    }
    return captureMcpBinding();
  }

  function sameMcpTarget(a, b) {
    return Boolean(
      a &&
        b &&
        a.workspaceId === b.workspaceId &&
        a.sessionId === b.sessionId &&
        (a.instanceId ?? "") === (b.instanceId ?? ""),
    );
  }

  /**
   * Freeze the routing triple the user acted on. Captured synchronously at the
   * user action: readiness can resolve much later, and a switch in between must
   * not re-target the request at the new workspace.
   */
  function captureMcpBinding() {
    if (disposed) throw new Error(t("settings.mcp.status.unavailable"));
    if (!getRuntimeTarget) return { context: getContextKey(), target: null }; // landing: global-only
    const target = getRuntimeTarget();
    if (
      typeof target?.workspaceId !== "string" ||
      !target.workspaceId ||
      typeof target?.sessionId !== "string" ||
      !target.sessionId ||
      (target.instanceId !== undefined && typeof target.instanceId !== "string")
    ) {
      throw new Error(t("settings.mcp.noSession"));
    }
    return {
      context: getContextKey(),
      target: Object.freeze({
        workspaceId: target.workspaceId,
        sessionId: target.sessionId,
        ...(target.instanceId !== undefined ? { instanceId: target.instanceId } : {}),
      }),
    };
  }

  /** Inventory/status reads tolerate a missing target; mutations never do. */
  function captureMcpBindingSafe() {
    try {
      return captureMcpBinding();
    } catch {
      return null;
    }
  }

  function isMcpBindingCurrent(binding) {
    if (disposed) return false;
    if (!binding) return true;
    if (binding.context !== getContextKey()) return false;
    if (!binding.target) return true;
    return sameMcpTarget(binding.target, getRuntimeTarget?.());
  }

  function assertMcpBindingCurrent(binding) {
    if (!isMcpBindingCurrent(binding)) {
      throw new Error(t("settings.mcp.targetChanged"));
    }
  }

  /**
   * Gateway rejects (timeout / no target / transport failure) normalize to the
   * same {ok:false} shape the handlers already render — models-page.js
   * precedent. Without this, a rejected call strands the click handler as an
   * unhandled rejection with no user feedback for the full 30s timeout.
   *
   * Every MCP request carries its initiating binding: `options.target` pins the
   * routing triple and the gateway's synchronous `beforeSend` re-checks it after
   * readiness, immediately before dispatch. A stale binding rejects instead of
   * falling back to the current target.
   */
  function call(op, params, binding) {
    try {
      const active = binding === undefined ? captureMcpBinding() : binding;
      if (getRuntimeTarget && !active?.target) throw new Error(t("settings.mcp.noSession"));
      assertMcpBindingCurrent(active);
      const options = {
        ...(active?.target ? { target: active.target } : {}),
        beforeSend: () => assertMcpBindingCurrent(active),
      };
      return configGateway.call(op, params, options).catch((error) => ({
        ok: false,
        error: error?.message ?? String(error),
      }));
    } catch (error) {
      return Promise.resolve({ ok: false, error: error?.message ?? String(error) });
    }
  }

  function selected() {
    return selections.get(activeTab) ?? null;
  }

  /** An empty detail pane reads as a broken grey page; default to the
   * first master row whenever a tab has no selection. */
  function ensureSelection() {
    if (selected()) return;
    const first = groupEntries()[0];
    if (first) selections.set(activeTab, { name: first.name });
  }

  function setStatus(text) {
    statusText = text;
    renderStatus();
  }

  function renderStatus() {
    const el = detailEl.querySelector(".mcp-detail-status");
    if (el) el.textContent = statusText;
  }

  function render() {
    dropStaleInventory();
    renderTabs();
    renderCaption();
    renderMaster();
    renderDetail();
  }

  /**
   * The inventory belongs to the binding it was read for. A repaint after the
   * target moved on (workspace switch, session/instance adoption) must not
   * restyle those rows as the new target's: acting on them would write into a
   * workspace the user never looked at. Drop the rows and every control that
   * carries their revision, and require an explicit refresh instead.
   */
  function dropStaleInventory() {
    if (!data || !dataBinding) return;
    if (isMcpBindingCurrent(dataBinding)) return;
    data = null;
    dataBinding = null;
    refreshRequired = true;
    selections.clear();
    mode = "view";
    pendingToggle = null;
    if (!statusText) statusText = t("settings.mcp.targetChanged");
  }

  /** Tab-level caption outside the master list: scope label + entry count. */
  function renderCaption() {
    if (!data) return;
    const entries = groupEntries();
    const el = captionEl ?? document.getElementById("mcp-tab-caption");
    if (el) el.textContent = `${scopeLabel(activeTab)} · ${entries.length}`;
  }

  function renderTabs() {
    // No active workspace → no project layer at all: hide the project tab
    // instead of showing an always-empty list (landing cold start).
    const projectAvailable = data?.projectAvailable !== false;
    if (!projectAvailable && activeTab === "project") activeTab = "piGlobal";
    for (const btn of tabs) {
      const isActive = btn.dataset.mcpTab === activeTab;
      btn.classList.toggle("extensions-page-tab", true);
      btn.setAttribute("aria-selected", isActive ? "true" : "false");
      btn.classList.toggle("hidden", btn.dataset.mcpTab === "project" && !projectAvailable);
    }
  }

  function renderMaster() {
    masterEl.replaceChildren();
    (migrationsEl ?? document.getElementById("mcp-migrations"))?.replaceChildren();
    if (!data) {
      // No usable inventory: either the read failed or the rows belonged to a
      // target that moved on. The only safe action is an explicit refresh.
      if (!refreshRequired) return;
      const refresh = document.createElement("button");
      refresh.type = "button";
      refresh.className = "models-provider-add";
      refresh.dataset.action = "mcp-refresh";
      refresh.textContent = t("settings.mcp.refresh");
      refresh.addEventListener("click", () => {
        if (refresh.disabled) return;
        refresh.disabled = true;
        void activate();
      });
      masterEl.appendChild(refresh);
      return;
    }
    const renderScope = dataBinding;
    const entries = groupEntries();

    if (statusError) {
      const note = document.createElement("div");
      note.className = "mcp-group-error mcp-status-error";
      note.textContent = t("settings.mcp.status.unavailable");
      note.title = statusError;
      masterEl.appendChild(note);
    } else if (statusDiagnostics.errors.length > 0 || statusDiagnostics.note) {
      // CLI diagnostics are page-level status, separate from the save/import
      // summary. The host only ever sends fixed safe text here.
      const note = document.createElement("div");
      note.className = "mcp-group-error mcp-status-diagnostics";
      const parts = [];
      if (statusDiagnostics.errors.length > 0) {
        parts.push(t("settings.mcp.status.errors", { count: statusDiagnostics.errors.length }));
      }
      if (statusDiagnostics.note) parts.push(statusDiagnostics.note);
      note.textContent = parts.join(" ");
      masterEl.appendChild(note);
    }

    if (data.groupErrors?.[activeTab]) {
      const err = document.createElement("div");
      err.className = "mcp-group-error";
      err.textContent = data.groupErrors[activeTab];
      masterEl.appendChild(err);
    }
    if (activeTab === "project" && entries.length === 0 && !data.groupErrors?.project) {
      const empty = document.createElement("div");
      empty.className = "mcp-group-error";
      empty.textContent = t("settings.mcp.noProject");
      masterEl.appendChild(empty);
    }

    for (const item of entries) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "pkg-manager-sidebar-row";
      if (selected()?.name === item.name) row.classList.add("is-selected");
      const name = document.createElement("div");
      name.className = "pkg-manager-sidebar-name";
      name.textContent = item.name;
      row.appendChild(name);
      const meta = document.createElement("div");
      meta.className = "pkg-manager-sidebar-meta";
      const live = statusForItem(item);
      const dot = document.createElement("span");
      dot.className = `pkg-manager-status-dot ${dotClassFor(item, live)}`;
      meta.appendChild(dot);
      const src = document.createElement("span");
      src.textContent = basename(item.sourceFile);
      src.title = item.sourceFile;
      meta.appendChild(src);
      // A disabled entry shows the config chip, never a live report: the host
      // caches `pi mcp list` for 60s, so the cached state can predate the
      // switch and would otherwise contradict the row it belongs to.
      const statusBadge = item.enabled ? renderStatusBadge(live) : null;
      if (statusBadge) meta.appendChild(statusBadge);
      if (!item.enabled) meta.appendChild(renderDisabledBadge());
      row.appendChild(meta);
      row.addEventListener("click", () => {
        selections.set(activeTab, { name: item.name });
        mode = "view";
        render();
      });
      masterEl.appendChild(row);
    }

    // Add affordance sits at the bottom of the master list (dashed, same
    // pattern as the Models page's provider add button) — pi-owned tabs only.
    const add = document.createElement("button");
    add.type = "button";
    add.className = "models-provider-add";
    add.textContent = t("settings.mcp.addMcp");
    add.addEventListener("click", () => {
      mode = "add";
      render();
    });
    masterEl.appendChild(add);

    // Batch snapshot of every global server into this project. Only offered
    // where a trusted project exists: there is nothing to import into
    // otherwise, and the backend would reject it anyway.
    if (activeTab === "project" && data.projectTrusted) {
      const importBtn = document.createElement("button");
      importBtn.type = "button";
      importBtn.className = "models-provider-add";
      importBtn.dataset.action = "mcp-import-global";
      importBtn.textContent = t("settings.mcp.importGlobal");
      importBtn.disabled = batchPending;
      importBtn.addEventListener("click", () => {
        if (importBtn.disabled) return;
        importBtn.disabled = true;
        void importGlobalOverrides(renderScope);
      });
      masterEl.appendChild(importBtn);
    }

    // Migration notices live OUTSIDE the master/detail layout entirely:
    // migrating is the user's call and must not compete with the live view.
    for (const target of data.migrations ?? []) {
      const notice = document.createElement("div");
      notice.className = "mcp-legacy-notice";
      const text = document.createElement("span");
      // Full path, not basename: adapter, shared, and native files all end
      // in mcp.json / mcp-adapter.json, so the directory is the identifier.
      text.textContent = t("settings.mcp.migrateNotice")
        .replace("{file}", target.sourceFile)
        .replace("{count}", String(target.missing.length));
      const action = document.createElement("button");
      action.type = "button";
      action.className = "mcp-legacy-migrate";
      action.textContent = t("settings.mcp.migrate");
      // Disable on first click: the op is fast, but a double-fire would run
      // twice; the re-render after load() replaces this button anyway.
      action.addEventListener("click", () => {
        action.disabled = true;
        void migrate(target.id, renderScope);
      });
      notice.append(text, action);
      (migrationsEl ?? document.getElementById("mcp-migrations") ?? masterEl).appendChild(notice);
    }
  }

  function basename(filePath) {
    const idx = filePath.lastIndexOf("/");
    return idx === -1 ? filePath : filePath.slice(idx + 1);
  }

  /**
   * Master-row dot: a disabled entry is disabled whatever the cached live
   * report says; only an enabled entry lets the live state win over `enabled`.
   */
  function dotClassFor(item, live) {
    if (!item.enabled) return "is-disabled";
    switch (live?.state) {
      case "connected":
        return "is-loaded";
      case "needs-auth":
        return "is-installed";
      // A failed server must not keep the accent "healthy" dot just because
      // its config entry is enabled; the badge text carries the detail.
      case "error":
        return "is-disabled";
      default:
        return "is-loaded";
    }
  }

  function stateClass(state) {
    return String(state ?? "unknown").replace(/[^a-z-]/gi, "") || "unknown";
  }

  /** Compact per-row state chip from `mcp_server_status`. */
  function renderStatusBadge(status) {
    if (!status) return null;
    const state = String(status.state ?? "");
    const badge = document.createElement("span");
    badge.className = `mcp-badge mcp-status-badge is-${stateClass(state)}`;
    if (state === "connected") {
      const count = Array.isArray(status.tools) ? status.tools.length : 0;
      badge.textContent =
        count > 0
          ? t("settings.mcp.status.connected", { count })
          : t("settings.mcp.status.connectedNoTools");
    } else if (state === "needs-auth") {
      badge.textContent = t("settings.mcp.status.needsAuth");
    } else if (state === "disabled") {
      badge.textContent = t("settings.mcp.status.disabled");
    } else if (state === "error") {
      // Summary in the chip, full text on `title` (never rendered raw).
      const detail = String(status.error ?? "");
      badge.textContent = summarize(detail) || t("settings.mcp.status.error");
      badge.title = detail;
    } else {
      badge.textContent = state || t("settings.mcp.status.unknown");
    }
    return badge;
  }

  function renderDisabledBadge() {
    const badge = document.createElement("span");
    badge.className = "mcp-badge";
    badge.dataset.disabledBadge = "";
    badge.textContent = t("settings.mcp.disabledBadge");
    return badge;
  }

  function summarize(text) {
    const collapsed = text.replace(/\s+/g, " ").trim();
    return collapsed.length > ERROR_SUMMARY_CHARS
      ? `${collapsed.slice(0, ERROR_SUMMARY_CHARS - 1)}…`
      : collapsed;
  }

  /**
   * Sign-in / sign-out affordances for the selected row. Sign-out needs a live
   * connected HTTP server; sign-in needs Pi to report `needs-auth`. A missing
   * report is unknown state — never an inference that the project is untrusted
   * (the config plane already reports trust explicitly) and never a reason to
   * offer an action `/mcp login` cannot complete.
   */
  function renderOAuthRow(item) {
    if (!mcpLogin) return null;
    const live = statusForItem(item);
    const isHttp = /http/i.test(transportOf(item, live));
    const state = live?.state ?? null;
    const canSignOut = isHttp && state === "connected";
    const canSignIn = isHttp && item.enabled && state === "needs-auth";
    if (!canSignIn && !canSignOut) return null;

    const row = document.createElement("div");
    row.className = "mcp-toggle-row";
    // Same rule as the master row: a disabled config hides the cached live
    // badge. Sign-out above survives it — clearing stored credentials does
    // not depend on the entry being enabled.
    const badge = item.enabled ? renderStatusBadge(live) : renderDisabledBadge();
    row.appendChild(badge);

    if (canSignIn) {
      const signIn = actionButton("mcp-login", t("settings.mcp.signIn"), "mcp-btn mcp-btn-primary");
      signIn.addEventListener("click", () => startLogin(item.name));
      row.appendChild(signIn);
    }
    if (canSignOut) {
      const signOut = actionButton(
        "mcp-logout",
        t("settings.mcp.signOut"),
        "mcp-btn mcp-btn-danger",
      );
      signOut.addEventListener("click", () => {
        signOut.disabled = true;
        void signOutServer(item.name);
      });
      row.appendChild(signOut);
    }
    return row;
  }

  function actionButton(action, label, className) {
    const node = document.createElement("button");
    node.type = "button";
    node.className = className;
    node.dataset.action = action;
    node.textContent = label;
    return node;
  }

  function startLogin(name) {
    if (!mcpLogin) return;
    loginDialog?.destroy();
    loginDialog = createMcpLoginDialog({
      name,
      start: () => mcpLogin.start(name),
      cancel: (operationId) => mcpLogin.cancel(operationId),
      status: (operationId) => mcpLogin.status(operationId),
      subscribe: (listener) => mcpLogin.subscribe(listener),
      openExternal: (url) => openAuthUrl(url),
      // The dialog closes itself on success; the badge turns connected, so a
      // fresh status + list read is the confirmation.
      onSuccess: async () => {
        setStatus(t("settings.mcp.signedIn"));
        await loadStatus();
        await load();
      },
    });
    void loginDialog.start();
  }

  async function signOutServer(name) {
    if (!mcpLogin) return;
    const result = await Promise.resolve()
      .then(() => mcpLogin.logout(name))
      .catch((error) => ({ ok: false, error: error?.message ?? String(error) }));
    if (!result?.ok) {
      setStatus(String(result?.error ?? "logout failed"));
      render();
      return;
    }
    setStatus(t("settings.mcp.saved"));
    await loadStatus();
    await load();
  }

  /** Same host opener the other native pages use; a non-native client has no
   * opener, so the URL shown in the dialog stays the fallback. */
  function openAuthUrl(url) {
    if (!url) return;
    if (!openExternal) return;
    Promise.resolve(openExternal(url)).catch((error) => {
      console.error("[mcp] failed to open authorization URL:", error);
    });
  }

  function renderDetail() {
    detailEl.replaceChildren();
    const renderScope = dataBinding;
    const body = document.createElement("div");
    body.className = "mcp-detail-body";
    const status = document.createElement("div");
    status.className = "mcp-detail-status";
    status.textContent = statusText;
    body.appendChild(status);

    if (!data) {
      detailEl.replaceChildren(body);
      return;
    }
    if (mode === "add") {
      body.appendChild(renderForm(activeTab, null, renderScope));
      detailEl.replaceChildren(body);
      return;
    }
    const sel = selected();
    if (!sel) {
      detailEl.replaceChildren(body);
      return;
    }
    const item = findEntry(sel.name);
    if (!item) {
      detailEl.replaceChildren(body);
      return;
    }
    // Override and invalid rows get the three-field detail; full definitions
    // keep the existing connection editor unchanged.
    const overrideLike = item.kind === "override" || item.kind === "invalid";
    body.appendChild(
      overrideLike ? renderOverride(item, renderScope) : renderEntry(item, renderScope),
    );
    detailEl.replaceChildren(body);
  }

  function draftKey(item) {
    return [item.sourceFile, item.name, item.revision ?? ""].join("\u0000");
  }

  /**
   * Adopt the acknowledged toggle result in the in-memory row so every repaint
   * between the acknowledgement and the inventory reload renders one revision.
   * `load()` replaces the row with the file's truth; until then the row must not
   * keep the revision the toggle just invalidated.
   */
  function applyToggleAcknowledgement(item, result) {
    const revision = result?.revision ?? item.revision;
    item.revision = revision;
    if (typeof result?.enabled === "boolean") {
      item.enabled = result.enabled;
      if (item.effective) item.effective = { ...item.effective, enabled: result.enabled };
    }
    return revision;
  }

  function overrideDraft(item) {
    const key = draftKey(item);
    const existing = drafts.get(key);
    if (existing) return { key, draft: existing };
    const draft = {
      exposure: item.effective?.exposure ?? "codemode",
      toolExposureText: JSON.stringify(item.effective?.toolExposure ?? {}, null, 2),
    };
    drafts.set(key, draft);
    return { key, draft };
  }

  function renderOverride(item, renderScope) {
    const { key, draft } = overrideDraft(item);
    return renderMcpOverrideDetail({
      item,
      draft,
      status: statusForItem(item),
      pending: pendingToggle === item.name,
      onDraftChange: (next) => {
        drafts.set(key, next); // unsaved text survives re-renders and locale repaints
      },
      onToggle: (intent) => {
        if (batchPending) return;
        void toggleOverride(item, intent, renderScope);
      },
      onSave: (values) => {
        if (batchPending) return;
        void saveOverride(item, values, renderScope);
      },
      onRemove: () => {
        if (batchPending) return;
        void removeOverride(item, renderScope);
      },
    });
  }

  /** enabled is one field, saved on its own; other unsaved fields stay drafted. */
  async function toggleOverride(item, intent, renderScope) {
    let binding;
    try {
      binding = actionBinding(renderScope);
    } catch (error) {
      setStatus(error?.message ?? String(error));
      render();
      return;
    }
    const key = draftKey(item);
    pendingToggle = item.name;
    render();
    const result = await call(
      "mcp_toggle_server",
      {
        scope: "project",
        name: item.name,
        disable: intent.disable,
        expectedRevision: item.revision,
        expectedGlobalRevision: data?.revisions?.piGlobal,
      },
      binding,
    );
    pendingToggle = null;
    if (!isMcpBindingCurrent(binding)) return;
    if (!result.ok) {
      setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
      render();
      return;
    }
    // The acknowledgement carries a new revision: move the unsaved draft to it
    // so a switch click never discards exposure/map text the user typed.
    // Adopt the acknowledged revision and enabled state in the in-memory row
    // *before* repainting: the inventory reload below may take a while or fail,
    // and repainting the invalidated revision would rebuild a default draft on
    // the old key — losing whatever the user types while the reload is in
    // flight. With the row advanced, the pane keeps one revision and one draft
    // identity for the whole ack→reload window.
    applyToggleAcknowledgement(item, result.data);
    // Read the draft now, not before the request: the map field stayed editable
    // while the toggle was pending and `onDraftChange` replaces the stored
    // object. When the acknowledgement repeats the revision (a no-op) the key
    // is unchanged, so the draft must stay where it is instead of being moved
    // and deleted under itself.
    const nextKey = draftKey(item);
    const draft = drafts.get(key);
    if (draft && nextKey !== key) {
      drafts.set(nextKey, draft);
      drafts.delete(key);
    }
    invalidateStatus();
    render();
    // Issue the host refresh before the reload it may not survive.
    void loadStatus({ refresh: true });
    await load();
    if (!isMcpBindingCurrent(binding)) return;
    // load() clears the status line, so the operation result is set after it.
    setStatus(t("settings.mcp.reloadRequired"));
  }

  async function saveOverride(item, values, renderScope) {
    let binding;
    try {
      binding = actionBinding(renderScope);
    } catch (error) {
      setStatus(error?.message ?? String(error));
      render();
      return;
    }
    const result = await call(
      "mcp_save_server",
      {
        scope: "project",
        name: item.name,
        kind: "override",
        intent: "edit",
        entry: {
          enabled: values.enabled,
          exposure: values.exposure,
          toolExposure: values.toolExposure,
        },
        expectedRevision: item.revision,
        expectedGlobalRevision: data?.revisions?.piGlobal,
      },
      binding,
    );
    if (!isMcpBindingCurrent(binding)) return;
    if (!result.ok) {
      // Stale/conflicting revision: the draft stays, the user reloads.
      setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
      render();
      return;
    }
    drafts.delete(draftKey(item));
    invalidateStatus();
    render();
    // Issue the host refresh before the reload it may not survive.
    void loadStatus({ refresh: true });
    await load();
    if (!isMcpBindingCurrent(binding)) return;
    setStatus(t("settings.mcp.reloadRequired"));
  }

  async function removeOverride(item, renderScope) {
    let binding;
    try {
      binding = actionBinding(renderScope);
    } catch (error) {
      setStatus(error?.message ?? String(error));
      render();
      return;
    }
    const result = await call(
      "mcp_delete_server",
      { scope: "project", name: item.name, expectedRevision: item.revision },
      binding,
    );
    if (!isMcpBindingCurrent(binding)) return;
    if (!result.ok) {
      setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
      render();
      return;
    }
    drafts.delete(draftKey(item));
    invalidateStatus();
    render();
    // Issue the host refresh before the reload it may not survive.
    void loadStatus({ refresh: true });
    await load();
    if (!isMcpBindingCurrent(binding)) return;
    setStatus(t("settings.mcp.override.removeWarning"));
  }

  /**
   * One batch write of every valid global server as an explicit three-field
   * snapshot. The initiating binding is frozen before the request, so a
   * workspace switch while readiness opens rejects instead of importing into
   * the new workspace.
   */
  async function importGlobalOverrides(renderScope) {
    let binding;
    try {
      binding = actionBinding(renderScope);
    } catch (error) {
      batchPending = false;
      setStatus(error?.message ?? String(error));
      render();
      return;
    }
    batchPending = true;
    render();
    const result = await call("mcp_import_global_overrides", {}, binding);
    batchPending = false;
    if (!isMcpBindingCurrent(binding)) return;
    if (!result.ok) {
      setStatus(t("settings.mcp.importFailed", { error: String(result.error ?? "") }));
      render();
      return;
    }
    const summary = importSummary(result.data);
    invalidateStatus();
    render();
    // Issue the host refresh before the reload it may not survive.
    void loadStatus({ refresh: true });
    await load();
    if (!isMcpBindingCurrent(binding)) return;
    // load() clears the status line; the operation result is sticky.
    setStatus(summary);
  }

  function importSummary(result) {
    const imported = Array.isArray(result?.imported) ? result.imported : [];
    const skipped = Array.isArray(result?.skipped) ? result.skipped : [];
    const existing = skipped.filter((s) => s.reason === "existing").length;
    const conflicts = skipped.filter((s) => s.reason !== "existing").length;
    const counts = t("settings.mcp.importSummary", {
      added: imported.length,
      existing,
      skipped: conflicts,
    });
    const details = skipped.map((skip) =>
      skip.reason === "namespace-conflict"
        ? t("settings.mcp.importSkip.namespaceConflict", {
            name: skip.name,
            conflict: skip.conflictWith ?? "",
          })
        : skip.reason === "invalid-global"
          ? t("settings.mcp.importSkip.invalidGlobal", {
              name: skip.name,
              detail: skip.detail ?? "",
            })
          : t("settings.mcp.importSkip.existing", { name: skip.name }),
    );
    return details.length > 0 ? `${counts} ${details.join("; ")}` : counts;
  }

  function renderEntry(item, renderScope) {
    const wrap = document.createElement("div");
    wrap.className = "mcp-entry";

    wrap.appendChild(renderToggle(item, renderScope));

    const head = document.createElement("div");
    head.className = "mcp-entry-head";
    const title = document.createElement("h4");
    title.textContent = item.name;
    head.appendChild(title);
    if (!item.editable) {
      const badge = document.createElement("span");
      badge.className = "mcp-badge";
      badge.textContent = t("settings.mcp.readOnlyBadge");
      head.appendChild(badge);
    }
    wrap.appendChild(head);

    const source = document.createElement("div");
    source.className = "mcp-source";
    source.textContent = `${t("settings.mcp.sourceLabel")}: ${item.sourceFile}`;
    wrap.appendChild(source);

    // Native eligibility diagnostic: pi rejects this definition, so it is
    // neither an import candidate nor an override base. The legacy editor
    // below keeps its contract (JSONC, `mcp-servers`, array commands); this
    // line only explains why native Pi does not read the entry.
    if (typeof item.validationError === "string" && item.validationError.length > 0) {
      const diagnostic = document.createElement("div");
      diagnostic.className = "mcp-group-error";
      diagnostic.dataset.diagnostic = "native-ineligible";
      diagnostic.textContent = item.validationError;
      wrap.appendChild(diagnostic);
    }

    const oauthRow = renderOAuthRow(item);
    if (oauthRow) wrap.appendChild(oauthRow);

    if (item.editable) {
      const form = renderForm(activeTab, item.name, renderScope);
      wrap.appendChild(form);
    } else {
      const pre = document.createElement("pre");
      pre.className = "mcp-entry-raw";
      pre.textContent = JSON.stringify(item.entry, null, 2);
      wrap.appendChild(pre);
    }

    return wrap;
  }

  function renderToggle(item, renderScope) {
    // Extensions-page switch pattern: role=switch + pkg-manager-toggle.
    const row = document.createElement("div");
    row.className = "mcp-toggle-row";
    const label = document.createElement("span");
    label.className = "mcp-toggle-label";
    label.textContent = t("settings.mcp.enable");
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = `pkg-manager-toggle${item.enabled ? " is-on" : ""}`;
    toggle.setAttribute("role", "switch");
    toggle.setAttribute("aria-checked", String(item.enabled));
    toggle.setAttribute("aria-label", t("settings.mcp.enable"));
    toggle.appendChild(document.createElement("span"));
    toggle.addEventListener("click", async () => {
      if (batchPending) return;
      let binding;
      try {
        binding = actionBinding(renderScope);
      } catch (error) {
        setStatus(error?.message ?? String(error));
        render();
        return;
      }
      const result = await call(
        "mcp_toggle_server",
        {
          scope: activeTab,
          name: item.name,
          disable: item.enabled,
          expectedRevision: item.revision,
        },
        binding,
      );
      if (!isMcpBindingCurrent(binding)) return;
      if (result.ok) {
        invalidateStatus();
        setStatus(t("settings.mcp.saved"));
        render();
        // Issue the host refresh before the reload it may not survive.
        void loadStatus({ refresh: true });
        await load();
      } else setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
    });
    row.append(label, toggle);
    return row;
  }

  /**
   * Edit form for pi-owned entries; add form when (scope, name) are null.
   * Array-form `command` displays joined with spaces and round-trips the
   * original array untouched unless the user edits the field.
   */
  function renderForm(scope, name, renderScope) {
    // The rendered row (not whatever the list holds at click time) owns the
    // revision and the target this form may act on.
    const renderedItem = name ? findEntry(name) : null;
    const existing = renderedItem?.entry ?? null;
    const isEdit = renderedItem !== null;
    const renderedRevision = isEdit ? renderedItem?.revision : data?.revisions?.[scope];
    const form = document.createElement("form");
    form.className = "mcp-form";
    form.addEventListener("submit", (e) => e.preventDefault());

    const nameInput = document.createElement("input");
    nameInput.type = "text";
    nameInput.required = true;
    nameInput.value = name ?? "";
    nameInput.disabled = isEdit;
    const nameRow = fieldRow(t("settings.mcp.form.name"), nameInput);

    const typeSelect = document.createElement("select");
    const stdioOpt = document.createElement("option");
    stdioOpt.value = "stdio";
    stdioOpt.textContent = t("settings.mcp.form.stdio");
    const remoteOpt = document.createElement("option");
    remoteOpt.value = "remote";
    remoteOpt.textContent = t("settings.mcp.form.remote");
    typeSelect.append(stdioOpt, remoteOpt);
    typeSelect.value = existing?.url ? "remote" : "stdio";
    const typeRow = fieldRow(t("settings.mcp.form.type"), typeSelect);

    const originalCommand = existing?.command;
    const commandIsArray = Array.isArray(originalCommand);
    const commandInput = document.createElement("input");
    commandInput.type = "text";
    commandInput.placeholder = "npx";
    commandInput.value = commandIsArray
      ? originalCommand.join(" ")
      : typeof originalCommand === "string"
        ? originalCommand
        : "";
    let commandDirty = false;
    commandInput.addEventListener("input", () => {
      commandDirty = true;
    });
    const commandRow = fieldRow(t("settings.mcp.form.command"), commandInput);

    const urlInput = document.createElement("input");
    urlInput.type = "text";
    urlInput.placeholder = "https://mcp.example.com/mcp";
    urlInput.value = typeof existing?.url === "string" ? existing.url : "";
    const urlRow = fieldRow(t("settings.mcp.form.url"), urlInput);

    const argsInput = document.createElement("textarea");
    argsInput.rows = 3;
    argsInput.placeholder = "-y\nchrome-devtools-mcp@latest";
    const args = existing?.args;
    if (Array.isArray(args)) argsInput.value = args.join("\n");
    const argsRow = fieldRow(t("settings.mcp.form.args"), argsInput);

    const envInput = document.createElement("textarea");
    envInput.rows = 3;
    // biome-ignore lint/suspicious/noTemplateCurlyInString: MCP ${VAR} placeholder, shown as literal text
    envInput.placeholder = "API_KEY=${MY_API_KEY}";
    const env = existing?.env;
    if (env && typeof env === "object") {
      envInput.value = Object.entries(env)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n");
    }
    const envRow = fieldRow(t("settings.mcp.form.env"), envInput);

    const headersInput = document.createElement("textarea");
    headersInput.rows = 2;
    // biome-ignore lint/suspicious/noTemplateCurlyInString: MCP ${VAR} placeholder, shown as literal text
    headersInput.placeholder = "Authorization=Bearer ${TOKEN}";
    const headers = existing?.headers;
    if (headers && typeof headers === "object") {
      headersInput.value = Object.entries(headers)
        .map(([k, v]) => `${k}=${v}`)
        .join("\n");
    }
    const headersRow = fieldRow(t("settings.mcp.form.headers"), headersInput);

    // Exposure mirrors pi's own /mcp picker: codemode (default) / deferred /
    // direct / hidden. Saving "codemode" omits the key, matching pi's
    // updateMcpServerConfig (config.ts:145 deletes the default value).
    const exposureSelect = document.createElement("select");
    for (const value of ["codemode", "direct", "deferred", "hidden"]) {
      const opt = document.createElement("option");
      opt.value = value;
      opt.textContent = t(`settings.mcp.form.exposure_${value}`);
      exposureSelect.appendChild(opt);
    }
    exposureSelect.value =
      existing?.exposure === "direct" ||
      existing?.exposure === "deferred" ||
      existing?.exposure === "hidden"
        ? existing.exposure
        : "codemode";
    const exposureRow = fieldRow(t("settings.mcp.form.exposure"), exposureSelect);

    const syncTransport = () => {
      const remote = typeSelect.value === "remote";
      commandRow.classList.toggle("hidden", remote);
      argsRow.classList.toggle("hidden", remote);
      envRow.classList.toggle("hidden", remote);
      urlRow.classList.toggle("hidden", !remote);
      headersRow.classList.toggle("hidden", !remote);
    };
    typeSelect.addEventListener("change", syncTransport);
    syncTransport();

    // Save + Delete share one action row — Delete only exists for edits.
    const actions = document.createElement("div");
    actions.className = "mcp-form-actions";
    const save = document.createElement("button");
    save.type = "submit";
    save.className = "mcp-btn mcp-btn-primary";
    save.textContent = t("settings.mcp.save");
    actions.appendChild(save);
    if (isEdit) {
      const del = document.createElement("button");
      del.type = "button";
      del.className = "mcp-btn mcp-btn-danger";
      del.textContent = t("settings.mcp.delete");
      del.addEventListener("click", async () => {
        if (batchPending) return;
        let binding;
        try {
          binding = actionBinding(renderScope);
        } catch (error) {
          setStatus(error?.message ?? String(error));
          render();
          return;
        }
        const result = await call(
          "mcp_delete_server",
          { scope, name, expectedRevision: renderedRevision },
          binding,
        );
        if (!isMcpBindingCurrent(binding)) return;
        if (result.ok) {
          selections.delete(activeTab);
          mode = "view";
          invalidateStatus();
          setStatus(t("settings.mcp.saved"));
          render();
          // Issue the host refresh before the reload it may not survive.
          void loadStatus({ refresh: true });
          await load();
        } else setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
      });
      actions.appendChild(del);
    }
    form.addEventListener("submit", async () => {
      const entry = { ...(existing ?? {}) };
      if (typeSelect.value === "remote") {
        if (!urlInput.value.trim()) {
          setStatus(t("settings.mcp.form.urlRequired"));
          return;
        }
        entry.url = urlInput.value.trim();
        delete entry.command;
        delete entry.args;
        parseKeyValue(headersInput.value, entry, "headers");
      } else {
        if (!commandInput.value.trim() && !commandIsArray) {
          setStatus(t("settings.mcp.form.commandRequired"));
          return;
        }
        // Unmodified array command round-trips verbatim; an edit collapses
        // it to a single string command (args field still applies).
        if (commandIsArray && !commandDirty) entry.command = originalCommand;
        else entry.command = commandInput.value.trim();
        delete entry.url;
        delete entry.headers;
        const argsLines = argsInput.value
          .split("\n")
          .map((l) => l.trim())
          .filter(Boolean);
        if (argsLines.length > 0) entry.args = argsLines;
        else delete entry.args;
        parseKeyValue(envInput.value, entry, "env");
      }
      if (exposureSelect.value === "codemode") delete entry.exposure;
      else entry.exposure = exposureSelect.value;
      if (batchPending) return;
      const targetName = isEdit ? name : nameInput.value.trim();
      let binding;
      try {
        binding = actionBinding(renderScope);
      } catch (error) {
        setStatus(error?.message ?? String(error));
        render();
        return;
      }
      const result = await call(
        "mcp_save_server",
        {
          scope,
          name: targetName,
          kind: "definition",
          intent: isEdit ? "edit" : "create",
          entry,
          expectedRevision: renderedRevision,
        },
        binding,
      );
      if (!isMcpBindingCurrent(binding)) return;
      if (result.ok) {
        selections.set(activeTab, { name: targetName });
        mode = "view";
        invalidateStatus();
        setStatus(t("settings.mcp.saved"));
        render();
        // Issue the host refresh before the reload it may not survive.
        void loadStatus({ refresh: true });
        await load();
      } else setStatus(String(result.error ?? t("settings.mcp.status.unavailable")));
    });

    form.append(
      nameRow,
      typeRow,
      commandRow,
      urlRow,
      argsRow,
      envRow,
      headersRow,
      exposureRow,
      actions,
    );
    return form;
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

  function parseKeyValue(text, entry, key) {
    const pairs = text
      .split("\n")
      .map((l) => l.trim())
      .filter(Boolean)
      .map((l) => {
        const idx = l.indexOf("=");
        if (idx <= 0) return null;
        return [l.slice(0, idx), l.slice(idx + 1)];
      })
      .filter(Boolean);
    if (pairs.length > 0) entry[key] = Object.fromEntries(pairs);
    else delete entry[key];
  }

  for (const btn of tabs) {
    btn.addEventListener("click", () => {
      const tab = btn.dataset.mcpTab;
      if (!tab || tab === activeTab) return;
      activeTab = tab;
      mode = "view";
      ensureSelection();
      render();
    });
  }

  function destroy() {
    disposed = true;
    loginDialog?.destroy();
    loginDialog = null;
    drafts.clear();
    unsubscribeLocale();
  }

  return { activate, refreshAvailability, destroy };
}

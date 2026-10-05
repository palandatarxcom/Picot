// ABOUTME: Provides one broker-backed transport for Pi, native, workspace, and ephemeral commands.
// ABOUTME: Keeps request routing owner-scoped and independent of the desktop bridge.

/**
 * Transport layer — the single surface the frontend uses to drive process /
 * window lifecycle and native operations.
 *
 * Historically every control op went through Tauri IPC via a browser bridge.
 * That hard-wired the UI to the desktop app: a mobile / remote client could
 * not drive those commands.
 *
 * Now there is ONE transport: every host op is a `host_request` sent over
 * HostServer v2 WebSocket and awaited via a correlated response. Runtime ops
 * use `runtime_request`; data ops use `data_request`.
 */

// Long/interactive ops must not be killed by the default 30s control timeout:
// the folder picker waits for the user, the updater download streams for a while.
const NO_TIMEOUT = 0;
const SPAWN_TIMEOUT_MS = 60000;
const PACKAGE_TIMEOUT_MS = 120000;

export class WsTransport {
  constructor(wsClient, env = globalThis.window || globalThis) {
    this.wsClient = wsClient;
    this.env = env;
  }

  get available() {
    return Boolean(this.wsClient);
  }

  // Host capability state from authenticated v2 hello_ack.
  get capabilities() {
    return this.wsClient?.capabilities || { native: false };
  }

  get hasUpdater() {
    return this.capabilities.native;
  }

  _control(command, args = {}, options = {}) {
    if (!this.wsClient) {
      return Promise.reject(new Error("Transport is not connected"));
    }
    return this.wsClient.sendControl(command, args, options);
  }

  // ── Process / window lifecycle (create project, sessions, instances) ───────

  openWorkspace(cwd, options = {}) {
    return this._control(
      "open_workspace",
      {
        cwd,
        sessionPath: options.sessionPath ?? null,
        forceNewSession: options.forceNewSession ?? false,
        openWindow: options.openWindow ?? true,
        waitForHealth: options.waitForHealth ?? true,
        waitForSessions: options.waitForSessions ?? false,
      },
      { timeoutMs: SPAWN_TIMEOUT_MS },
    );
  }

  // Runtime mutations use canonical v2 `runtime_request`; they never target a
  // Pi HTTP/WS endpoint and never carry a legacy port hint.
  fork(entryId) {
    return this.wsClient.sendRuntime({ type: "fork", entryId });
  }

  // ── Native-only ops (need an OS host; reject when capabilities.native=false) ─
  // ── Versions / packages ────────────────────────────────────────────────────

  getPiVersion() {
    return this._control("get_pi_version", {});
  }

  getAppVersion() {
    return this._control("get_app_version", {});
  }

  isDev() {
    return this._control("is_dev", {});
  }

  listPiPackages() {
    return this._control("list_pi_packages", {});
  }

  checkPiPackageUpdates() {
    return this._control("check_pi_package_updates", {}, { timeoutMs: PACKAGE_TIMEOUT_MS });
  }

  installPiPackage(source) {
    return this._control("install_pi_package", { source }, { timeoutMs: PACKAGE_TIMEOUT_MS });
  }

  removePiPackage(source, { local = false } = {}) {
    return this._control("remove_pi_package", { source, local }, { timeoutMs: PACKAGE_TIMEOUT_MS });
  }

  updatePiPackage(source, { local = false } = {}) {
    return this._control("update_pi_package", { source, local }, { timeoutMs: PACKAGE_TIMEOUT_MS });
  }

  setPiPackageDisabled(source, scope, disabled, cwd = "") {
    return this._control(
      "set_pi_package_disabled",
      { source, scope, disabled, cwd },
      { timeoutMs: PACKAGE_TIMEOUT_MS },
    );
  }

  // pi-fff config lives on the host control plane (works on the landing page,
  // no Pi process needed) — unlike advisor settings which need the bridge's
  // in-process model registry.
  getFffConfig() {
    return this._control("get_fff_config", {});
  }
  // Landing bridge-service runtime (landing-bridge-runtime spec v2): lazy
  // spawn, idempotent host-side; descriptor carries instanceId + generation.
  spawnConfigRuntime() {
    return this._control("spawn_config_runtime", {}, { timeoutMs: SPAWN_TIMEOUT_MS });
  }
  hasAnyCredentials() {
    return this._control("has_any_credentials", {});
  }

  setFffConfig(payload) {
    return this._control("set_fff_config", payload);
  }
  getTodoConfig() {
    return this._control("get_todo_config", {});
  }
  setTodoConfig(payload) {
    return this._control("set_todo_config", payload);
  }
  getAskUserConfig() {
    return this._control("get_askuser_config", {});
  }
  setAskUserConfig(payload) {
    return this._control("set_askuser_config", payload);
  }
  getPonytailConfig() {
    return this._control("get_ponytail_config", {});
  }
  setPonytailConfig(payload) {
    return this._control("set_ponytail_config", payload);
  }
  getVccConfig() {
    return this._control("get_vcc_config", {});
  }
  setVccConfig(payload) {
    return this._control("set_vcc_config", payload);
  }
  getGoalConfig() {
    return this._control("get_goal_config", {});
  }
  setGoalConfig(payload) {
    return this._control("set_goal_config", payload);
  }
  getCavemanConfig() {
    return this._control("get_caveman_config", {});
  }
  setCavemanConfig(payload) {
    return this._control("set_caveman_config", payload);
  }
  getCacheOptimizerConfig() {
    return this._control("get_cache_optimizer_config", {});
  }
  setCacheOptimizerConfig(payload) {
    return this._control("set_cache_optimizer_config", payload);
  }
  getLensConfig(cwd) {
    return this._control("get_lens_config", { cwd: cwd ?? null });
  }
  setLensConfig(payload) {
    return this._control("set_lens_config", payload);
  }

  restartRuntime(workspaceId, sessionId) {
    return this._control(
      "restart_runtime",
      { workspaceId, sessionId },
      { timeoutMs: SPAWN_TIMEOUT_MS },
    );
  }

  // ── Native-only ops (need an OS host; reject when capabilities.native=false) ─

  // ── Workspace registry & app preferences (app-global, Native-only) ────────

  listWorkspaces() {
    return this._control("workspace.list", {});
  }

  addWorkspace(path) {
    return this._control("workspace.add", { path });
  }

  removeWorkspace(workspaceId) {
    return this._control("workspace.remove", { workspaceId });
  }

  setWorkspacePinned(workspaceId, pinned) {
    return this._control("workspace.pin", { workspaceId, pinned });
  }

  getPreference(key) {
    return this._control("preference.get", { key });
  }

  setPreference(key, value) {
    return this._control("preference.set", { key, value });
  }

  deletePreference(key) {
    return this._control("preference.delete", { key });
  }

  piPathStatus() {
    return this._control("pi_path_status", {});
  }

  piPathConfigure(enabled) {
    return this._control("pi_path_configure", { enabled });
  }

  listPreferences(prefix = "") {
    return this._control("preference.list", { prefix });
  }

  pickFolder() {
    return this._control("pick_folder", {}, { timeoutMs: NO_TIMEOUT });
  }

  pickSkillSource() {
    return this._control("pick_skill_source", {}, { timeoutMs: NO_TIMEOUT });
  }

  // Discovered-skills inventory rides host control ops (no Pi runtime):
  // the host scans the same agent/project settings files the bridge reads.
  listSkillInventory(scope) {
    return this._control("list_skill_inventory", { scope });
  }

  setSkillEnabled(scope, target, enabled) {
    return this._control("set_skill_enabled", { scope, target, enabled });
  }

  scanSkillInstallSource(sourceId) {
    return this._control("skill_scan_install_source", { sourceId }, { timeoutMs: NO_TIMEOUT });
  }

  installSkillLinks(request) {
    return this._control("skill_install_links", request, { timeoutMs: NO_TIMEOUT });
  }

  pickImageFiles(initialDir) {
    return this._control(
      "pick_image_files",
      { initialDir: initialDir || null },
      { timeoutMs: NO_TIMEOUT },
    );
  }

  listInstalledApps() {
    return this._control("list_installed_apps", {});
  }

  exportSession(sessionId) {
    return this._control("session_export", { sessionId });
  }

  // Turn files card (2026-09-19 spec): per-file working-tree stats, frozen
  // at turn end. Non-git workspaces reject → caller degrades to a plain list.
  gitTurnStats(paths) {
    return this._control("git_turn_stats", { paths });
  }

  // ── MCP sign-in / sign-out (host-spawned `pi mcp`, no session runtime) ────
  // The Rust host runs the embedded `pi mcp login|logout|list` itself, so these
  // ride the control plane like pi_path_status — never a Pi runtime target.

  mcpLoginStart(name) {
    return this._control("mcp_login_start", { name }, { timeoutMs: SPAWN_TIMEOUT_MS });
  }

  mcpLoginCancel(operationId) {
    return this._control("mcp_login_cancel", { operationId });
  }

  mcpLoginStatus(operationId) {
    return this._control("mcp_login_status", { operationId });
  }

  // `pi mcp logout` spawns a process (and the host invalidates its report
  // cache), so it gets the same headroom as a spawn rather than the 30s default.
  mcpLogout(name) {
    return this._control("mcp_logout", { name }, { timeoutMs: SPAWN_TIMEOUT_MS });
  }

  // `pi mcp list --json` really connects to each server; the host caches the
  // result for 60s, so this is a page-activation cost, not a poll.
  // `{refresh:true}` asks the host to drop that cache after a configuration
  // write; the default read keeps the TTL.
  mcpServerStatus(options = {}) {
    const args = options?.refresh === true ? { refresh: true } : {};
    return this._control("mcp_server_status", args, { timeoutMs: SPAWN_TIMEOUT_MS });
  }

  /**
   * Owner-scoped `mcpLoginUpdate` frames (`{ operationId, status, authUrl?,
   * error? }`) from the host login runner. Returns an unsubscribe function; a
   * listener only ever sees non-secret progress for the caller's operations.
   */
  onMcpLoginUpdate(listener) {
    if (!this.wsClient) return () => {};
    const handler = (event) => listener(event?.detail?.payload ?? null);
    this.wsClient.addEventListener("mcpLoginUpdate", handler);
    return () => this.wsClient.removeEventListener("mcpLoginUpdate", handler);
  }

  // ── Host data plane (v2 `data_request`) ──────────────────────────────────────
  // Paths are workspace-relative; `workspaceId` is carried by the envelope from
  // the client's own authoritative route. `file_read`/`file_write`/`file_raw`
  // helpers land together with the preview-panel migration that needs them.

  fileMentions(query, root) {
    return this.wsClient.sendData("file_mentions", { query, root });
  }

  listFiles(path = "") {
    return this.wsClient.sendData("list_files", { path });
  }

  listSessions() {
    return this.wsClient.sendData("list_sessions", {});
  }

  searchSessions(query) {
    return this.wsClient.sendData("search_sessions", { query });
  }

  costDashboard({ range = "30d", granularity = "day", scope = "all", models = "" } = {}) {
    return this.wsClient.sendData("cost_dashboard", {
      range,
      granularity,
      scope,
      models,
    });
  }

  fileRead(path, { signal } = {}) {
    return this.wsClient.sendData("file_read", { path }, { signal });
  }

  fileWrite(path, content, { expectedModifiedAtMs = null, force = false } = {}) {
    return this.wsClient.sendData("file_write", {
      path,
      content,
      expectedModifiedAtMs,
      force,
      idempotencyKey: `ui-file-write-${crypto.randomUUID?.() || Date.now()}`,
    });
  }

  // Single-item workspace-file mutations (Files panel). Each carries its own
  // idempotency key so a double-submit replays instead of racing.

  /** `parentPath` is workspace-relative; `.` is the workspace root. */
  fileCreate(parentPath, name, kind) {
    return this.wsClient.sendData("file_create", {
      parentPath,
      name,
      kind,
      idempotencyKey: `ui-file-create-${crypto.randomUUID?.() || Date.now()}`,
    });
  }

  fileRename(path, name) {
    return this.wsClient.sendData("file_rename", {
      path,
      name,
      idempotencyKey: `ui-file-rename-${crypto.randomUUID?.() || Date.now()}`,
    });
  }

  fileDelete(path) {
    return this.wsClient.sendData("file_delete", {
      path,
      idempotencyKey: `ui-file-delete-${crypto.randomUUID?.() || Date.now()}`,
    });
  }

  fileRawUrl(path) {
    const query = new URLSearchParams({
      workspaceId: this.wsClient.workspaceId || "",
      path,
    });
    return `/v2/files/raw?${query}`;
  }

  /** Raw bytes over the authenticated data plane. Subresource loads
   * (<img>/pdf.js) cannot carry the desktop-capability header the
   * /v2/files/raw HTTP route requires, so previews fetch base64 here. */
  fileRaw(path) {
    return this.wsClient.sendData("file_raw", { path });
  }

  sessionHistory(sessionId, sessionFile) {
    return this.wsClient.sendData("session_history", { sessionId, sessionFile });
  }

  sessionRename(filePath, name) {
    return this._control("session_rename", { filePath, name });
  }

  sessionDeleteBatch(filePaths) {
    return this._control("session_delete_batch", { filePaths });
  }

  workspaceInfo() {
    return this.wsClient.sendData("workspace_info", {});
  }

  workspaceSessions(workspaceId = null, { countOnly = false } = {}) {
    return this.wsClient.sendData("workspace_sessions", {
      ...(workspaceId ? { workspaceId } : {}),
      ...(countOnly ? { countOnly: true } : {}),
    });
  }

  /** Codex reset-credit ledger (spec 2026-09-22): open returns the
   * idempotency-keyed operationId; settle records the outcome. */
  resetCreditOpen() {
    return this.wsClient.sendData("reset_credit_open", {});
  }

  resetCreditSettle({ operationId, ambiguous }) {
    return this.wsClient.sendData("reset_credit_settle", { operationId, ambiguous });
  }

  /** Browser panes (spec 2026-09-22): child webviews in the file-preview
   * panel. Rect coordinates are logical (CSS) pixels relative to the window. */
  browserPaneCreate({ paneId, windowLabel, url, x, y, width, height }) {
    return this.wsClient.sendData("browser_pane_create", {
      paneId,
      windowLabel,
      url,
      x,
      y,
      width,
      height,
    });
  }

  browserPaneSetRect({ paneId, x, y, width, height }) {
    return this.wsClient.sendData("browser_pane_set_rect", { paneId, x, y, width, height });
  }

  browserPaneSetVisible({ paneId, visible }) {
    return this.wsClient.sendData("browser_pane_set_visible", { paneId, visible });
  }

  /** Promise-style eval: resolves with the JSON serialization of the
   * expression's value (or rejects on timeout). */
  browserPaneEval({ paneId, js }) {
    return this.wsClient.sendData("browser_pane_eval", { paneId, js });
  }

  browserPaneNavigate({ paneId, url }) {
    return this.wsClient.sendData("browser_pane_navigate", { paneId, url });
  }

  browserPaneUrl({ paneId }) {
    return this.wsClient.sendData("browser_pane_url", { paneId });
  }

  browserPaneDestroy({ paneId }) {
    return this.wsClient.sendData("browser_pane_destroy", { paneId });
  }

  officecliWatchStart({ file }) {
    return this.wsClient.sendData("officecli_watch_start", { file });
  }

  officecliWatchStop({ file }) {
    return this.wsClient.sendData("officecli_watch_stop", { file });
  }

  officecliWatchStatus({ file }) {
    return this.wsClient.sendData("officecli_watch_status", { file });
  }

  officecliWatchMark({ file, path }) {
    return this.wsClient.sendData("officecli_watch_mark", { file, path });
  }

  hostHealth() {
    return this._control("host_health", {});
  }

  runtimeInstances() {
    return this._control("runtime_instances", {});
  }

  // Agent/config text files are host-owned: the read/write keeps the exact bytes
  // the editor showed, and the lock + atomic replace stay in `host_config`.
  agentTextFileGet(name, scope = "global") {
    return this._control("agent_text_file_get", { name, scope });
  }

  agentTextFilePut(name, content, scope = "global") {
    return this._control("agent_text_file_put", { name, content, scope });
  }

  // JSON config files additionally get Pi's lock protocol and, for model config,
  // the host-side backup plus restart notice.
  settingsGet(name, scope = "global") {
    return this._control("settings_get", { name, scope });
  }

  settingsPut(name, value, scope = "global") {
    return this._control("settings_put", { name, value, scope });
  }

  // Subagent controls accept scope and host-issued candidate IDs, never client paths.
  listSubagents(scope, workspaceIdentity = null) {
    return this._control("subagents_inventory", { scope, ...(workspaceIdentity || {}) });
  }

  getSubagentDetail(scope, candidateId, workspaceIdentity = null) {
    return this._control("subagents_get_detail", {
      scope,
      candidateId,
      ...(workspaceIdentity || {}),
    });
  }

  createSubagent(payload) {
    return this._control("subagents_create", payload);
  }

  setSubagentOverride(payload) {
    return this._control("subagents_set_override", payload);
  }

  // D4 mobile entry: LAN reachability + pairing-token minting. The mint is
  // desktop-only on the host side and refuses while the host is loopback-only.
  mobileAccessInfo() {
    return this._control("mobile_access_info", {});
  }

  mobilePairingCreate() {
    return this._control("mobile_pairing_create", {});
  }

  loadSessionUiProfile(expectedSessionId) {
    return this._control("session_ui_profile_load", { expectedSessionId });
  }

  saveSessionUiProfile(expectedSessionId, profile) {
    return this._control("session_ui_profile_save", { expectedSessionId, ...profile });
  }

  openInApp(path, { appName = null, command = null } = {}) {
    return this._control("open_in_app", { path, appName, command });
  }

  openExternal(url) {
    return this._control("open_external", { url });
  }

  openDevtools() {
    return this._control("open_devtools", {});
  }

  // ── Auto-updater ────────────────────────────────────────────────────────────

  checkForUpdate() {
    return this._control("check_for_update", {}, { timeoutMs: SPAWN_TIMEOUT_MS });
  }

  downloadAndInstallUpdate(onProgress) {
    return this._control("download_and_install_update", {}, { onProgress, timeoutMs: NO_TIMEOUT });
  }

  relaunchApp() {
    // The host restarts the process, so the control_response typically never
    // arrives (the socket drops first). Swallow only the expected disconnect;
    // surface all other errors to avoid hiding real restart failures.
    return this._control("relaunch_app", {}).catch((err) => {
      const message = String(err?.message || err || "");
      if (/websocket disconnected/i.test(message)) {
        console.warn("[transport] relaunch response not received (app restarting):", err);
        return;
      }
      throw err;
    });
  }

  // ── Ephemeral chats (Side Chat / Quick Chat) ───────────────────────────────

  createEphemeral(kind, options = {}) {
    return this._control("ephemeral_create", { kind, ...options }, { timeoutMs: SPAWN_TIMEOUT_MS });
  }
  replaceQuickChat() {
    return this._control("ephemeral_replace_quick", {}, { timeoutMs: SPAWN_TIMEOUT_MS });
  }
  closeEphemeral(instanceId, generation) {
    return this._control("ephemeral_close", { instanceId, generation });
  }
  getEphemeralBootstrap() {
    return this._control("ephemeral_bootstrap", {});
  }
  updateEphemeralUi(instanceId, generation, patch) {
    return this._control("ephemeral_update_ui", { instanceId, generation, ...patch });
  }
  // Forward an owner-scoped ephemeral RPC (prompt/abort/model/etc.). The broker
  // derives the owner from the authenticated connection, never from the payload.
  sendEphemeral(instanceId, generation, payload) {
    return this.wsClient?.sendEphemeral(instanceId, generation, payload) ?? null;
  }

  // ── Workspace transitions (same-cwd switch vs cross-workspace navigation) ──

  prepareWorkspaceTarget(targetCwd, options = {}) {
    return this._control(
      "workspace_target_prepare",
      {
        targetCwd,
        sessionPath: options.sessionPath ?? null,
        forceNewSession: options.forceNewSession ?? false,
        reuseExisting: options.reuseExisting ?? false,
      },
      { timeoutMs: SPAWN_TIMEOUT_MS },
    );
  }
  commitWorkspaceTransition(transitionGeneration) {
    return this._control("workspace_transition_commit", { transitionGeneration });
  }
  cancelWorkspaceTransition(transitionGeneration) {
    return this._control("workspace_transition_cancel", { transitionGeneration });
  }
  approveWindowClose(requestId) {
    return this._control("window_close_approve", { requestId });
  }
  cancelWindowClose(requestId) {
    return this._control("window_close_cancel", { requestId });
  }
}

let singleton = null;

export function createTransport({ wsClient, env = globalThis.window || globalThis } = {}) {
  return new WsTransport(wsClient, env);
}

export function initTransport(opts) {
  singleton = createTransport(opts);
  return singleton;
}

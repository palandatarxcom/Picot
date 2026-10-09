import { describe, expect, test, vi } from "vitest";
import { createTransport, WsTransport } from "./transport.js";

function fakeWsClient(capabilities = { native: true }) {
  return {
    capabilities,
    sendControl: vi.fn((command) => Promise.resolve(`ok:${command}`)),
    sendRuntime: vi.fn((command) => Promise.resolve({ command })),
    sendData: vi.fn((operation) => Promise.resolve({ operation })),
  };
}

describe("WsTransport", () => {
  test("create project (openWorkspace) sends an open_workspace control command", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: { location: { port: "47821" } } });

    await transport.openWorkspace("/tmp/proj", { forceNewSession: true, openWindow: false });

    expect(ws.sendControl).toHaveBeenCalledWith(
      "open_workspace",
      expect.objectContaining({ cwd: "/tmp/proj", forceNewSession: true, openWindow: false }),
      expect.objectContaining({ timeoutMs: expect.any(Number) }),
    );
  });

  test("exportSession sends a session_export control command", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: {} });

    await transport.exportSession("session-123");

    expect(ws.sendControl).toHaveBeenCalledWith("session_export", { sessionId: "session-123" }, {});
  });

  test("fileMentions sends a file_mentions data request", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: {} });

    await transport.fileMentions("src/comp");

    expect(ws.sendData).toHaveBeenCalledWith("file_mentions", { query: "src/comp" });
  });

  test("sessionHistory sends the scanned session ID and exact JSONL path", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.sessionHistory(
      "session-123",
      "/sessions/--workspace--/2026-09-03_session-123.jsonl",
    );

    expect(ws.sendData).toHaveBeenCalledWith("session_history", {
      sessionId: "session-123",
      sessionFile: "/sessions/--workspace--/2026-09-03_session-123.jsonl",
    });
  });

  test("mobile entry controls send host control commands", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: {} });

    await transport.mobileAccessInfo();
    await transport.mobilePairingCreate();

    expect(ws.sendControl).toHaveBeenCalledWith("mobile_access_info", {}, {});
    expect(ws.sendControl).toHaveBeenCalledWith("mobile_pairing_create", {}, {});
  });

  test("mcp sign-in ops use the host control plane with spawn-grade timeouts", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.mcpLoginStart("sentry");
    await transport.mcpLoginCancel("op-1");
    await transport.mcpLoginStatus("op-1");
    await transport.mcpLogout("sentry");
    await transport.mcpServerStatus();

    expect(ws.sendControl).toHaveBeenCalledWith(
      "mcp_login_start",
      { name: "sentry" },
      { timeoutMs: 60000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith("mcp_login_cancel", { operationId: "op-1" }, {});
    expect(ws.sendControl).toHaveBeenCalledWith("mcp_login_status", { operationId: "op-1" }, {});
    expect(ws.sendControl).toHaveBeenCalledWith(
      "mcp_logout",
      { name: "sentry" },
      { timeoutMs: 60000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith("mcp_server_status", {}, { timeoutMs: 60000 });
  });

  test("environment ops ride the host control plane with per-op timeouts", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.checkEnvironment();
    await transport.startEnvironmentInstall({ tool: "git", action: "install" });
    await transport.getEnvironmentInstall();
    await transport.cancelEnvironmentInstall();

    // The check runs six version processes serially (5s timeout + 1s pipe
    // drain each), so it needs more than the 30s default control timeout.
    expect(ws.sendControl).toHaveBeenCalledWith("environment_check", {}, { timeoutMs: 60000 });
    // Starting only spawns the embedded Pi: a spawn-grade timeout, never the
    // 15-minute maintenance budget.
    expect(ws.sendControl).toHaveBeenCalledWith(
      "environment_install_start",
      { tool: "git", action: "install" },
      { timeoutMs: 60000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith("environment_install_status", {}, {});
    expect(ws.sendControl).toHaveBeenCalledWith("environment_install_cancel", {}, {});

    // The completion banner is a plain data frame: no workspace or session, so
    // the Environment page can notify from Landing too (the host still applies
    // the desktop-owner gate and the user's own notification preference).
    await transport.notifyEnvironmentFinished({ title: "Environment", body: "uv finished" });
    expect(ws.sendData).toHaveBeenCalledWith("show_task_notification", {
      title: "Environment",
      body: "uv finished",
    });
  });

  test("onMcpLoginUpdate forwards only the frame payload and unsubscribes", () => {
    const listeners = new Map();
    const ws = {
      ...fakeWsClient(),
      addEventListener: (type, handler) => listeners.set(type, handler),
      removeEventListener: (type) => listeners.delete(type),
    };
    const transport = new WsTransport(ws, {});
    const seen = [];
    const unsubscribe = transport.onMcpLoginUpdate((payload) => seen.push(payload));

    listeners.get("mcpLoginUpdate")({
      detail: { type: "mcpLoginUpdate", payload: { operationId: "op-1", status: "pending" } },
    });
    expect(seen).toEqual([{ operationId: "op-1", status: "pending" }]);
    // A frame without a payload must not throw into the WS dispatch loop.
    listeners.get("mcpLoginUpdate")({ detail: {} });
    expect(seen.at(-1)).toBeNull();

    unsubscribe();
    expect(listeners.has("mcpLoginUpdate")).toBe(false);
  });

  test("fork sends a canonical runtime request without a port", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.fork("entry-123");

    expect(ws.sendRuntime).toHaveBeenCalledWith({ type: "fork", entryId: "entry-123" });
  });

  test("session UI profile methods use native host control commands", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});
    const profile = { provider: "anthropic", modelId: "claude-sonnet", thinkingLevel: "high" };

    await transport.loadSessionUiProfile("/sessions/a.jsonl");
    await transport.saveSessionUiProfile("/sessions/a.jsonl", profile);

    expect(ws.sendControl).toHaveBeenCalledWith(
      "session_ui_profile_load",
      { expectedSessionId: "/sessions/a.jsonl" },
      {},
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "session_ui_profile_save",
      { expectedSessionId: "/sessions/a.jsonl", ...profile },
      {},
    );
  });

  test("package management methods send exact broker payloads and timeouts", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.checkPiPackageUpdates();
    await transport.removePiPackage("npm:foo", { local: true });
    await transport.updatePiPackage("npm:foo", { local: true });
    await transport.setPiPackageDisabled("npm:foo", "project", true, "/workspace");
    await transport.restartRuntime("workspace-1", "session-1");

    expect(ws.sendControl).toHaveBeenCalledWith(
      "check_pi_package_updates",
      {},
      { timeoutMs: 120000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "remove_pi_package",
      { source: "npm:foo", local: true },
      { timeoutMs: 120000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "update_pi_package",
      { source: "npm:foo", local: true },
      { timeoutMs: 120000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "set_pi_package_disabled",
      { source: "npm:foo", scope: "project", disabled: true, cwd: "/workspace" },
      { timeoutMs: 120000 },
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "restart_runtime",
      { workspaceId: "workspace-1", sessionId: "session-1" },
      { timeoutMs: 60000 },
    );
  });

  test("native ops map to their control commands", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: { location: { port: "47821" } } });

    await transport.pickFolder();
    await transport.openExternal("https://example.com");

    expect(ws.sendControl).toHaveBeenCalledWith("pick_folder", {}, { timeoutMs: 0 });
    expect(ws.sendControl).toHaveBeenCalledWith(
      "open_external",
      { url: "https://example.com" },
      {},
    );
  });

  test("pickImageFiles sends a pick_image_files control command with initialDir and no timeout", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: { location: { port: "47821" } } });

    await transport.pickImageFiles("/tmp/workspace");

    expect(ws.sendControl).toHaveBeenCalledWith(
      "pick_image_files",
      { initialDir: "/tmp/workspace" },
      { timeoutMs: 0 },
    );
  });

  test("pickImageFiles sends null initialDir when no path is provided", async () => {
    const ws = fakeWsClient();
    const transport = createTransport({ wsClient: ws, env: { location: { port: "47821" } } });

    await transport.pickImageFiles();

    expect(ws.sendControl).toHaveBeenCalledWith(
      "pick_image_files",
      { initialDir: null },
      { timeoutMs: 0 },
    );
  });

  test("capabilities reflect the underlying ws client", () => {
    const transport = new WsTransport(fakeWsClient({ native: false }), {});
    expect(transport.capabilities.native).toBe(false);
    expect(transport.hasUpdater).toBe(false);
  });

  test("downloadAndInstallUpdate forwards the progress callback with no timeout", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});
    const onProgress = () => {};

    await transport.downloadAndInstallUpdate(onProgress);

    expect(ws.sendControl).toHaveBeenCalledWith(
      "download_and_install_update",
      {},
      { onProgress, timeoutMs: 0 },
    );
  });

  test("relaunchApp swallows the disconnect that follows a host restart", async () => {
    const ws = {
      capabilities: { native: true },
      sendControl: vi.fn(() => Promise.reject(new Error("WebSocket disconnected"))),
    };
    const transport = new WsTransport(ws, {});

    await expect(transport.relaunchApp()).resolves.toBeUndefined();
  });

  test("ephemeral lifecycle methods issue their control commands", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.createEphemeral("side-chat");
    await transport.replaceQuickChat();
    await transport.closeEphemeral("inst-1", 2);
    await transport.getEphemeralBootstrap();
    await transport.updateEphemeralUi("inst-1", 2, { title: "Hi", unread: true });

    expect(ws.sendControl).toHaveBeenCalledWith(
      "ephemeral_create",
      { kind: "side-chat" },
      expect.objectContaining({ timeoutMs: expect.any(Number) }),
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "ephemeral_close",
      {
        instanceId: "inst-1",
        generation: 2,
      },
      {},
    );
    expect(ws.sendControl).toHaveBeenCalledWith("ephemeral_bootstrap", {}, {});
    expect(ws.sendControl).toHaveBeenCalledWith(
      "ephemeral_update_ui",
      {
        instanceId: "inst-1",
        generation: 2,
        title: "Hi",
        unread: true,
      },
      {},
    );
  });

  test("workspace transition + close methods issue their control commands", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.prepareWorkspaceTarget("/tmp/b", { forceNewSession: true });
    await transport.commitWorkspaceTransition(7);
    await transport.cancelWorkspaceTransition(7);
    await transport.approveWindowClose("close-1");

    expect(ws.sendControl).toHaveBeenCalledWith(
      "workspace_target_prepare",
      expect.objectContaining({ targetCwd: "/tmp/b", forceNewSession: true }),
      expect.objectContaining({ timeoutMs: expect.any(Number) }),
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "workspace_transition_commit",
      {
        transitionGeneration: 7,
      },
      {},
    );
    expect(ws.sendControl).toHaveBeenCalledWith(
      "window_close_approve",
      { requestId: "close-1" },
      {},
    );
  });

  test("sendEphemeral forwards to the wsClient and returns its requestId", () => {
    const ws = {
      capabilities: { native: true },
      sendControl: vi.fn(),
      sendEphemeral: vi.fn(() => "ep-9"),
    };
    const transport = new WsTransport(ws, {});
    expect(transport.sendEphemeral("inst-1", 3, { type: "prompt" })).toBe("ep-9");
    expect(ws.sendEphemeral).toHaveBeenCalledWith("inst-1", 3, { type: "prompt" });
  });

  test("brave-search config ops use the host control plane without a cwd", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.getBraveSearchConfig();
    await transport.setBraveSearchConfig({ apiKey: "", defaultCount: 9 });

    // Global-only: the get carries no cwd and the set no scope.
    expect(ws.sendControl).toHaveBeenNthCalledWith(1, "get_brave_search_config", {}, {});
    expect(ws.sendControl).toHaveBeenNthCalledWith(
      2,
      "set_brave_search_config",
      { apiKey: "", defaultCount: 9 },
      {},
    );
  });

  test("tavily-search config ops use the host control plane without a cwd", async () => {
    const ws = fakeWsClient();
    const transport = new WsTransport(ws, {});

    await transport.getTavilySearchConfig();
    await transport.setTavilySearchConfig({ apiKey: "", defaultCount: 9 });

    // Global-only: the get carries no cwd and the set no scope.
    expect(ws.sendControl).toHaveBeenNthCalledWith(1, "get_tavily_search_config", {}, {});
    expect(ws.sendControl).toHaveBeenNthCalledWith(
      2,
      "set_tavily_search_config",
      { apiKey: "", defaultCount: 9 },
      {},
    );
  });
});

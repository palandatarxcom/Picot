// @vitest-environment jsdom

// ABOUTME: Backend integration for project MCP actions: real ConfigGateway + readiness + real /picot-config handler.
// ABOUTME: Proves a normal import writes the initiating workspace and a stale click never writes the new one.

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { setMessages } from "../i18n.js";
import { ConfigGateway } from "./config-gateway.js";
import { createConfigReadiness } from "./config-readiness.js";
import { setupMcpPage } from "./mcp-page.js";

vi.mock("@earendil-works/pi-coding-agent", () => ({
  createAgentSession: vi.fn(),
  ModelRuntime: { create: vi.fn() },
  SessionManager: { inMemory: vi.fn(), listAll: vi.fn(), open: vi.fn() },
}));

setMessages({
  settings: {
    mcp: {
      title: "MCP",
      groups: { piGlobal: "Global", project: "Current project" },
      importGlobal: "Import global settings",
      importSummary: "Added {added}, already present {existing}, skipped {skipped}.",
      importFailed: "Import failed: {error}",
      importSkip: {
        existing: "{name}: already in this project",
        namespaceConflict: "{name}: collides with {conflict}",
        invalidGlobal: "{name}: not a valid global server ({detail})",
      },
      reloadRequired: "Saved. Reload the session to apply.",
      noSession: "No active session for MCP configuration.",
      targetChanged: "The workspace changed before the MCP request was sent.",
      sourceLabel: "Source",
      addMcp: "+ Add MCP",
      save: "Save",
      delete: "Delete",
      enable: "Enable",
      saved: "Saved.",
      disabledBadge: "disabled",
      noProject: "no project servers",
      projectUntrusted: "Project not trusted",
      status: {
        connected: "Connected · {count} tools",
        connectedNoTools: "Connected",
        needsAuth: "Sign in required",
        disabled: "Disabled",
        error: "Error",
        unknown: "Unknown state",
        unavailable: "Live MCP status unavailable.",
        errors: "{count} server(s) reported errors.",
        trustNote: "Project MCP servers are not trusted in this session.",
      },
      override: {
        sourceHint: "Overrides global server {name}. Connection settings stay global.",
        toolExposure: "Tool exposure map (JSON)",
        mapHelp: "Whole-map replacement. {} clears per-tool rules.",
        mapInvalid: "Enter a JSON object, for example {}.",
        mapValueInvalid: 'Tool "{tool}" must map to codemode, direct, deferred or hidden.',
        exposureInvalid: "Choose a tool exposure.",
        remove: "Remove project override",
        removeWarning: "Removes this entry from the project mcp.json only.",
        baseLost: "The global server this override belongs to is gone.",
        staleDraft: "The file changed on disk.",
      },
      form: {
        exposure: "Tool exposure",
        exposure_codemode: "Default (codemode)",
        exposure_direct: "Direct",
        exposure_deferred: "Deferred",
        exposure_hidden: "Hidden",
      },
    },
  },
});

const tempRoots = [];

/** One isolated Picot config backend (its own HOME/agent root) plus a project. */
async function createBackend(label) {
  const home = mkdtempSync(join(tmpdir(), `picot-mcp-${label}-`));
  tempRoots.push(home);
  const agentDir = join(home, ".pi", "agent");
  const projectRoot = join(home, "workspace");
  mkdirSync(agentDir, { recursive: true });
  mkdirSync(projectRoot, { recursive: true });
  writeFileSync(
    join(agentDir, "mcp.json"),
    `${JSON.stringify({ mcpServers: { docs: { command: "npx", args: ["-y", "docs"] } } }, null, 2)}\n`,
    "utf8",
  );
  writeFileSync(
    join(agentDir, "trust.json"),
    `${JSON.stringify({ [realpathSync(projectRoot)]: true }, null, 2)}\n`,
    "utf8",
  );

  const previousHome = process.env.HOME;
  vi.resetModules();
  // Hermetic: the agent root honors PI_CODING_AGENT_DIR (like Pi and the Rust
  // launcher), so an inherited value must not win over the temp HOME.
  vi.stubEnv("PI_CODING_AGENT_DIR", "");
  process.env.HOME = home;
  let handlePicotConfig;
  try {
    ({ handlePicotConfig } = await import("../../extensions/picot-config.ts"));
  } finally {
    if (previousHome === undefined) delete process.env.HOME;
    else process.env.HOME = previousHome;
  }
  return {
    home,
    agentDir,
    projectRoot,
    projectFile: join(projectRoot, ".pi", "mcp.json"),
    handlePicotConfig,
  };
}

function projectBytes(backend) {
  return existsSync(backend.projectFile) ? readFileSync(backend.projectFile, "utf8") : null;
}

/**
 * Runtime that routes every accepted config prompt into the real backend of the
 * target workspace, with that workspace's host marker, cwd and trust store.
 */
function createRoutingRuntime({ gatewayRef, backends, sent }) {
  return {
    request: vi.fn(async (command, target) => {
      const payload = JSON.parse(command.message.slice("/picot-config ".length));
      sent.push({ op: payload.op, params: payload.params, target });
      const backend = backends[target.workspaceId];
      const previousMarker = process.env.PI_STUDIO_MCP_PROJECT_ROOT;
      process.env.PI_STUDIO_MCP_PROJECT_ROOT = backend.projectRoot;
      let result;
      try {
        result = await backend.handlePicotConfig(payload.op, payload.params, {
          cwd: backend.projectRoot,
          isProjectTrusted: () => true,
        });
      } finally {
        if (previousMarker === undefined) delete process.env.PI_STUDIO_MCP_PROJECT_ROOT;
        else process.env.PI_STUDIO_MCP_PROJECT_ROOT = previousMarker;
      }
      queueMicrotask(() =>
        gatewayRef.gateway.consumeNotify({
          message: JSON.stringify({ __picotConfig: payload.id, ...result }),
        }),
      );
      return { acceptance: "accepted" };
    }),
  };
}

function mountHarness(backends) {
  const sent = [];
  const routing = { workspaceId: "A", sessionId: "sA", instanceId: "iA" };
  const gatewayRef = {};
  const runtime = createRoutingRuntime({ gatewayRef, backends, sent });
  const readiness = createConfigReadiness({
    targetKeyOf: () =>
      routing.workspaceId && routing.sessionId
        ? [routing.workspaceId, routing.sessionId, routing.instanceId ?? ""].join("\u0000")
        : null,
  });
  const gateway = new ConfigGateway({
    runtime,
    getTarget: () => routing,
    waitUntilReady: readiness.waitUntilReady,
  });
  gatewayRef.gateway = gateway;

  const masterEl = document.createElement("div");
  const detailEl = document.createElement("div");
  const tabs = document.createElement("div");
  for (const key of ["piGlobal", "project"]) {
    const btn = document.createElement("button");
    btn.dataset.mcpTab = key;
    tabs.appendChild(btn);
  }
  const page = setupMcpPage({
    masterEl,
    detailEl,
    tabs: tabs.querySelectorAll("[data-mcp-tab]"),
    navItem: null,
    configGateway: gateway,
    captionEl: document.createElement("p"),
    migrationsEl: document.createElement("div"),
    getRuntimeTarget: () => routing,
    getContextKey: () => `${routing.workspaceId}:${routing.sessionId}`,
  });
  return {
    sent,
    routing,
    gateway,
    readiness,
    page,
    masterEl,
    detailEl,
    tabs,
    setRouting: (next) => Object.assign(routing, next),
  };
}

function clickTab(tabs, key) {
  tabs.querySelector(`[data-mcp-tab="${key}"]`).click();
}

afterEach(() => {
  vi.unstubAllEnvs();
  for (const dir of tempRoots.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("project MCP import through the real backend", () => {
  it("writes the initiating workspace and leaves the other workspace untouched", async () => {
    // Sequential: each backend captures HOME at import time, so concurrent
    // module resets could bind a backend to the other workspace's agent root.
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeB = projectBytes(b);
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("test");
    await activating;
    expect(h.sent.map((entry) => entry.op)).toContain("mcp_list_servers");

    clickTab(h.tabs, "project");
    h.masterEl.querySelector('[data-action="mcp-import-global"]').click();
    await vi.waitFor(() => expect(projectBytes(a)).not.toBeNull());

    expect(JSON.parse(projectBytes(a))).toEqual({
      mcpServers: { docs: { enabled: true, exposure: "codemode", toolExposure: {} } },
    });
    expect(projectBytes(b)).toBe(beforeB);
    expect(h.sent.every((entry) => entry.target.workspaceId === "A")).toBe(true);
    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain("Added 1"),
    );
  });

  it("never writes the new workspace when the target switches before dispatch", async () => {
    // Sequential: each backend captures HOME at import time, so concurrent
    // module resets could bind a backend to the other workspace's agent root.
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeA = projectBytes(a);
    const beforeB = projectBytes(b);
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("test");
    await activating;
    clickTab(h.tabs, "project");
    h.masterEl.querySelector('[data-action="mcp-import-global"]').click();
    // Switch synchronously, inside the readiness-resolved window.
    h.setRouting({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });

    await Promise.resolve();
    await Promise.resolve();
    expect(projectBytes(a)).toBe(beforeA);
    expect(projectBytes(b)).toBe(beforeB);
    expect(h.sent.some((entry) => entry.op === "mcp_import_global_overrides")).toBe(false);
    expect(h.sent.every((entry) => entry.target.workspaceId === "A")).toBe(true);
  });

  it("never writes the new workspace when its readiness gate opens after the switch", async () => {
    // Sequential: each backend captures HOME at import time, so concurrent
    // module resets could bind a backend to the other workspace's agent root.
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeA = projectBytes(a);
    const beforeB = projectBytes(b);
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("A");
    await activating;
    clickTab(h.tabs, "project");
    // Same workspace, new routing triple: the gate closes again for the click.
    h.setRouting({ instanceId: "iA2" });
    h.masterEl.querySelector('[data-action="mcp-import-global"]').click();
    h.setRouting({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });
    h.readiness.noteForegroundSnapshot("B");

    await Promise.resolve();
    await Promise.resolve();
    expect(projectBytes(a)).toBe(beforeA);
    expect(projectBytes(b)).toBe(beforeB);
    expect(h.sent.some((entry) => entry.op === "mcp_import_global_overrides")).toBe(false);
  });

  it("writes nothing when the page is disposed before the send", async () => {
    // Sequential: each backend captures HOME at import time, so concurrent
    // module resets could bind a backend to the other workspace's agent root.
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeA = projectBytes(a);
    const beforeB = projectBytes(b);
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("test");
    await activating;
    clickTab(h.tabs, "project");
    h.masterEl.querySelector('[data-action="mcp-import-global"]').click();
    h.page.destroy();

    await Promise.resolve();
    await Promise.resolve();
    expect(projectBytes(a)).toBe(beforeA);
    expect(projectBytes(b)).toBe(beforeB);
    expect(h.sent.some((entry) => entry.op === "mcp_import_global_overrides")).toBe(false);
  });

  it("routes project definition CRUD and the adapter migration through the bound target", async () => {
    // Sequential: each backend captures HOME at import time, so concurrent
    // module resets could bind a backend to the other workspace's agent root.
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeB = projectBytes(b);
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("test");
    await activating;
    clickTab(h.tabs, "project");

    // Create a project definition through the add form.
    h.masterEl.querySelector(".models-provider-add").click();
    const form = h.detailEl.querySelector(".mcp-form");
    form.querySelector('input[placeholder="npx"]').value = "repo-tool";
    const nameInput = form.querySelector('input[type="text"]');
    nameInput.value = "repoTool";
    form.dispatchEvent(new Event("submit"));
    await vi.waitFor(() =>
      expect(JSON.parse(projectBytes(a)).mcpServers.repoTool).toEqual({ command: "repo-tool" }),
    );

    // Toggle it off, then delete it: both stay on the initiating target.
    await vi.waitFor(() => expect(h.masterEl.textContent).toContain("repoTool"));
    h.masterEl.querySelector(".pkg-manager-sidebar-row").click();
    await vi.waitFor(() => expect(h.detailEl.querySelector('[role="switch"]')).not.toBeNull());
    h.detailEl.querySelector('.mcp-entry [role="switch"]').click();
    await vi.waitFor(() =>
      expect(JSON.parse(projectBytes(a)).mcpServers.repoTool).toEqual({
        command: "repo-tool",
        enabled: false,
      }),
    );

    expect(projectBytes(b)).toBe(beforeB);
    expect(h.sent.every((entry) => entry.target.workspaceId === "A")).toBe(true);
    expect(h.sent.filter((entry) => entry.op === "mcp_save_server").length).toBeGreaterThan(0);
    expect(h.sent.filter((entry) => entry.op === "mcp_toggle_server").length).toBe(1);
  });

  it("hides the import and rejects the backend op for an untrusted workspace", async () => {
    const a = await createBackend("a");
    const b = await createBackend("b");
    const beforeA = projectBytes(a);
    const beforeB = projectBytes(b);
    // A trust store that withdraws the permission the runtime claims.
    writeFileSync(
      join(a.agentDir, "trust.json"),
      `${JSON.stringify({ [realpathSync(a.projectRoot)]: false })}\n`,
    );
    const h = mountHarness({ A: a, B: b });

    const activating = h.page.activate();
    h.readiness.noteForegroundSnapshot("test");
    await activating;
    clickTab(h.tabs, "project");
    // The page never offers a project write it cannot perform.
    expect(h.masterEl.querySelector('[data-action="mcp-import-global"]')).toBeNull();

    // The backend refuses the same op even when the runtime claims trust.
    const previousMarker = process.env.PI_STUDIO_MCP_PROJECT_ROOT;
    process.env.PI_STUDIO_MCP_PROJECT_ROOT = a.projectRoot;
    try {
      await expect(
        a.handlePicotConfig(
          "mcp_import_global_overrides",
          {},
          {
            cwd: a.projectRoot,
            isProjectTrusted: () => true,
          },
        ),
      ).resolves.toMatchObject({ ok: false, error: expect.stringMatching(/trust/i) });
    } finally {
      if (previousMarker === undefined) delete process.env.PI_STUDIO_MCP_PROJECT_ROOT;
      else process.env.PI_STUDIO_MCP_PROJECT_ROOT = previousMarker;
    }
    expect(projectBytes(a)).toBe(beforeA);
    expect(projectBytes(b)).toBe(beforeB);
  });
});

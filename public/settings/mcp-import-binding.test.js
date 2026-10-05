// @vitest-environment jsdom

// ABOUTME: End-to-end binding test for MCP project actions across a REAL ConfigGateway + readiness gate.
// ABOUTME: Proves a click never re-targets the new workspace, even when readiness resolves after the switch.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { setMessages } from "../i18n.js";
import { ConfigGateway } from "./config-gateway.js";
import { createConfigReadiness } from "./config-readiness.js";
import { setupMcpPage } from "./mcp-page.js";

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

const LIST = {
  ok: true,
  data: {
    groups: {
      piGlobal: [
        {
          name: "docs",
          entry: { command: "x" },
          sourceFile: "/a/mcp.json",
          editable: true,
          enabled: true,
          kind: "definition",
          revision: "g",
        },
      ],
      project: [],
    },
    groupErrors: {},
    migrations: [],
    projectAvailable: true,
    projectTrusted: true,
    revisions: { piGlobal: "g", project: "p" },
  },
};

/** Records every dispatched request with the routing triple it was sent to. */
function createRuntime() {
  const requests = [];
  return {
    requests,
    request: vi.fn((command, target, options) => {
      requests.push({
        target,
        options,
        id: JSON.parse(command.message.slice("/picot-config ".length)).id,
      });
      return Promise.resolve({ acceptance: "accepted" });
    }),
  };
}

function targetKeyOf(routing) {
  if (!routing?.workspaceId || !routing?.sessionId) return null;
  return [routing.workspaceId, routing.sessionId, routing.instanceId ?? ""].join("\u0000");
}

function harness() {
  const runtime = createRuntime();
  let routing = { workspaceId: "A", sessionId: "sA", instanceId: "iA" };
  const readiness = createConfigReadiness({ targetKeyOf: () => targetKeyOf(routing) });
  const gateway = new ConfigGateway({
    runtime,
    getTarget: () => routing,
    waitUntilReady: readiness.waitUntilReady,
  });
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
    runtime,
    gateway,
    page,
    masterEl,
    detailEl,
    tabs,
    setRouting: (next) => {
      routing = next;
    },
    mutateRouting: (patch) => {
      routing = { ...routing, ...patch };
    },
    readiness,
  };
}

function settle(gateway, runtime, index, data = LIST.data) {
  gateway.consumeNotify({
    message: JSON.stringify({ __picotConfig: runtime.requests[index].id, ok: true, data }),
  });
}

function clickTab(tabs, key) {
  tabs.querySelector(`[data-mcp-tab="${key}"]`).click();
}

function clickImport(masterEl) {
  masterEl.querySelector('[data-action="mcp-import-global"]').click();
}

function importRequests(runtime) {
  return runtime.requests.filter((r) => r.options?.idempotencyKey?.startsWith("cfg-"));
}

async function activateWithReadyGate({ gateway, runtime, page, readiness }) {
  const activated = page.activate();
  readiness.noteForegroundSnapshot("test");
  await vi.waitFor(() => expect(runtime.requests).toHaveLength(1));
  settle(gateway, runtime, 0);
  await activated;
}

beforeEach(() => {
  document.body.replaceChildren();
});

describe("MCP project actions across the real gateway and readiness gate", () => {
  it("sends the normal import exactly once, to the initiating target", async () => {
    const h = harness();
    await activateWithReadyGate(h);
    clickTab(h.tabs, "project");
    clickImport(h.masterEl);

    await vi.waitFor(() => expect(h.runtime.requests).toHaveLength(2));
    const sent = h.runtime.requests[1];
    expect(sent.target).toEqual({ workspaceId: "A", sessionId: "sA", instanceId: "iA" });

    settle(h.gateway, h.runtime, 1, {
      imported: ["docs"],
      skipped: [],
      changed: true,
      path: "/ws/.pi/mcp.json",
      revision: "p2",
    });
    // Accepted mutation reloads the inventory, then publishes the summary.
    await vi.waitFor(() => expect(h.runtime.requests).toHaveLength(3));
    settle(h.gateway, h.runtime, 2);
    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain("Added 1"),
    );
  });

  it("never dispatches to the new workspace when the target switches before the send", async () => {
    const h = harness();
    await activateWithReadyGate(h);
    clickTab(h.tabs, "project");
    clickImport(h.masterEl);
    // Synchronous switch: readiness already resolved, the send happens in the
    // next microtask, exactly the window the guard must cover.
    h.setRouting({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });

    await Promise.resolve();
    await Promise.resolve();
    // The stale result is dropped: only the initial inventory read was sent.
    expect(h.runtime.requests).toHaveLength(1);
    expect(h.runtime.requests.some((r) => r.target.workspaceId === "B")).toBe(false);
    expect(h.detailEl.querySelector(".mcp-detail-status").textContent).not.toContain("Added");
  });

  it("rejects instead of re-targeting when the readiness gate opens for another workspace", async () => {
    const h = harness();
    const activated = h.page.activate();
    h.readiness.noteForegroundSnapshot("A"); // A becomes ready
    await vi.waitFor(() => expect(h.runtime.requests).toHaveLength(1));
    settle(h.gateway, h.runtime, 0);
    await activated;

    clickTab(h.tabs, "project");
    // A new routing triple in the same workspace closes the gate again: the
    // click is captured against a target that is not ready yet.
    h.mutateRouting({ instanceId: "iA2" });
    clickImport(h.masterEl);
    h.setRouting({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });
    // B's gate opens only now; A's waiter must fail rather than fall through.
    h.readiness.noteForegroundSnapshot("B");

    await Promise.resolve();
    await Promise.resolve();
    expect(h.runtime.requests).toHaveLength(1);
    expect(h.runtime.requests.some((r) => r.target.workspaceId === "B")).toBe(false);
  });

  it("sends nothing when the page is disposed before the guarded send", async () => {
    const h = harness();
    await activateWithReadyGate(h);
    clickTab(h.tabs, "project");
    clickImport(h.masterEl);
    h.page.destroy();

    await Promise.resolve();
    await Promise.resolve();
    expect(h.runtime.requests).toHaveLength(1);
  });

  it("rejects a same-workspace session adoption before the send", async () => {
    const h = harness();
    await activateWithReadyGate(h);
    clickTab(h.tabs, "project");
    clickImport(h.masterEl);
    h.mutateRouting({ sessionId: "sA2", instanceId: "iA2" });

    await Promise.resolve();
    await Promise.resolve();
    expect(h.runtime.requests).toHaveLength(1);
    expect(h.detailEl.querySelector(".mcp-detail-status").textContent).not.toContain("Added");
  });

  it("keeps the frozen triple even when the routing object is mutated in place after dispatch", async () => {
    const h = harness();
    await activateWithReadyGate(h);
    clickTab(h.tabs, "project");
    clickImport(h.masterEl);
    await vi.waitFor(() => expect(h.runtime.requests).toHaveLength(2));
    // The request already left: it must still carry the original triple.
    h.mutateRouting({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });
    expect(h.runtime.requests[1].target).toEqual({
      workspaceId: "A",
      sessionId: "sA",
      instanceId: "iA",
    });
    expect(importRequests(h.runtime)).toHaveLength(2);
  });
});

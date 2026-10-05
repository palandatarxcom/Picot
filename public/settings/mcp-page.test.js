// @vitest-environment jsdom

// ABOUTME: Verifies the MCP settings page: two native layer tabs, per-entry toggle payloads,
// ABOUTME: array-command round-trip, adapter/shared migration banners, gateway error surfacing.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { WsTransport } from "../app/transport.js";
import { setMessages } from "../i18n.js";
import { createMcpHostOps, setupMcpPage } from "./mcp-page.js";

setMessages({
  settings: {
    mcp: {
      title: "MCP",
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
      groups: { piGlobal: "Global", project: "Current project" },
      readOnlyBadge: "read-only",
      disabledBadge: "disabled",
      effectHint: "hint",
      addMcp: "+ Add MCP",
      noProject: "no project servers",
      sourceLabel: "Source",
      sectionsAriaLabel: "MCP layers",
      save: "Save",
      delete: "Delete",
      enable: "Enable",
      disable: "Disable",
      saved: "Saved.",
      refresh: "Refresh",
      migrateNotice: "Old config {file} has {count} server(s) native Pi does not read.",
      migrate: "Migrate",
      migrationFailed: "Migration failed.",
      signIn: "Sign in",
      signOut: "Sign out",
      signedIn: "Signed in.",
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
      login: {
        title: "Sign in to {name}",
        preparing: "Starting sign-in…",
        waiting: "Complete authorization in your browser.",
        openBrowser: "Open browser",
        cancel: "Cancel",
        retry: "Try again",
        failed: "Sign-in failed.",
        cancelled: "Sign-in cancelled.",
      },
      form: {
        name: "Name",
        type: "Type",
        stdio: "stdio",
        remote: "remote",
        command: "Command",
        url: "URL",
        args: "Args",
        env: "Env",
        headers: "Headers",
        urlRequired: "url required",
        commandRequired: "command required",
        exposure: "Tool exposure",
        exposure_codemode: "Default (codemode, via script search)",
        exposure_direct: "Direct",
        exposure_deferred: "Deferred (tool search)",
        exposure_hidden: "Hidden",
      },
    },
  },
});

function makeGateway(result) {
  return { call: vi.fn().mockResolvedValue(result) };
}

const LIST = {
  ok: true,
  data: {
    groups: {
      piGlobal: [
        {
          name: "context7",
          // biome-ignore lint/suspicious/noTemplateCurlyInString: MCP ${VAR} placeholder data
          entry: { command: "npx", args: ["-y", "@upstash/context7-mcp"], env: { K: "${V}" } },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
        {
          name: "chrome-devtools",
          entry: { command: ["npx", "-y", "chrome-devtools-mcp"], futureField: { a: 1 } },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
        {
          name: "paused",
          entry: { url: "https://paused.example", enabled: false },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: false,
        },
      ],
      project: [
        {
          name: "repoTool",
          entry: { command: "run repo" },
          sourceFile: "/ws/repo/.pi/mcp.json",
          editable: true,
          enabled: true,
        },
      ],
    },
    groupErrors: {},
    migrations: [],
    projectAvailable: true,
  },
};

function mount(gateway, extra = {}) {
  const masterEl = document.createElement("div");
  const detailEl = document.createElement("div");
  const tabs = document.createElement("div");
  for (const key of ["piGlobal", "project"]) {
    const btn = document.createElement("button");
    btn.dataset.mcpTab = key;
    tabs.appendChild(btn);
  }
  const navItem = document.createElement("button");
  navItem.className = "hidden";
  const captionEl = document.createElement("p");
  const migrationsEl = document.createElement("div");
  const page = setupMcpPage({
    masterEl,
    detailEl,
    tabs: tabs.querySelectorAll("[data-mcp-tab]"),
    navItem,
    configGateway: gateway,
    captionEl,
    migrationsEl,
    ...extra,
  });
  return { page, masterEl, detailEl, tabs, navItem, captionEl, migrationsEl };
}

function clickRow(masterEl, name) {
  const row = Array.from(masterEl.querySelectorAll(".pkg-manager-sidebar-row")).find((r) =>
    r.textContent.includes(name),
  );
  row.click();
}

function rowFor(masterEl, name) {
  return Array.from(masterEl.querySelectorAll(".pkg-manager-sidebar-row")).find((r) =>
    r.textContent.includes(name),
  );
}

function clickTab(tabs, key) {
  tabs.querySelector(`[data-mcp-tab="${key}"]`).click();
}

// Live-status fixture in the shape the host really returns (`pi mcp list --json`
// projected): scope, source file and state all take part in identity matching.
// `remoteProj` is deliberately absent (pi omits servers from untrusted projects).
const GLOBAL_MCP_FILE = "/home/u/.pi/agent/mcp.json";
const STATUS_SERVERS = [
  {
    name: "context7",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "stdio",
    state: "connected",
    tools: [{}, {}, {}],
  },
  {
    name: "sentry",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "http",
    state: "needs-auth",
    tools: [],
  },
  {
    name: "remote-ok",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "http",
    state: "connected",
    tools: [{}],
  },
  {
    name: "flaky",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "http",
    state: "error",
    error: "connect ECONNREFUSED 127.0.0.1:9999",
    tools: [],
  },
  {
    name: "paused",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "http",
    state: "disabled",
    tools: [],
  },
  {
    name: "chrome-devtools",
    scope: "global",
    source: GLOBAL_MCP_FILE,
    transport: "stdio",
    state: "connected",
    tools: [],
  },
];

const LIST_OAUTH = {
  ok: true,
  data: {
    groups: {
      piGlobal: [
        ...LIST.data.groups.piGlobal,
        {
          name: "sentry",
          entry: { url: "https://mcp.sentry.dev/mcp" },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
        {
          name: "remote-ok",
          entry: { url: "https://mcp.ok/mcp" },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
        {
          name: "flaky",
          entry: { url: "https://mcp.flaky/mcp" },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
        {
          name: "ghost",
          entry: { url: "https://mcp.ghost/mcp" },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
        },
      ],
      project: [
        ...LIST.data.groups.project,
        {
          name: "remoteProj",
          entry: { url: "https://mcp.proj/mcp" },
          sourceFile: "/ws/repo/.pi/mcp.json",
          editable: true,
          enabled: true,
        },
      ],
    },
    groupErrors: {},
    migrations: [],
    projectAvailable: true,
  },
};

/** Host-plane harness: the MCP login ops ride the WS host channel, not the
 * runtime config gateway — the page must be able to tell them apart. */
function makeMcpLogin(servers = STATUS_SERVERS) {
  let listener = null;
  return {
    start: vi.fn(async () => ({ ok: true, operationId: "op-1" })),
    cancel: vi.fn(async () => ({ ok: true, cancelled: true })),
    status: vi.fn(async () => ({ ok: true, status: "pending" })),
    logout: vi.fn(async () => ({ ok: true })),
    serverStatus: vi.fn(async () => ({ ok: true, servers })),
    subscribe: vi.fn((next) => {
      listener = next;
      return () => {
        listener = null;
      };
    }),
    emit: (payload) => listener?.(payload),
  };
}

describe("mcp-page", () => {
  beforeEach(() => {
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("auto-selects the first master row on load and on tab switch", async () => {
    const { page, detailEl, masterEl, tabs } = mount(makeGateway(LIST));
    await page.activate();
    // Default tab (user) auto-selects its first entry without any click.
    expect(detailEl.textContent).toContain("context7");
    expect(
      masterEl.querySelector(".pkg-manager-sidebar-row").classList.contains("is-selected"),
    ).toBe(true);

    clickTab(tabs, "project");
    expect(detailEl.textContent).toContain("repoTool");
  });

  it("hides the project tab entirely when no workspace is active", async () => {
    const landing = {
      ok: true,
      data: {
        ...LIST.data,
        groups: { piGlobal: LIST.data.groups.piGlobal, project: [] },
        projectAvailable: false,
      },
    };
    const { page, tabs, captionEl } = mount(makeGateway(landing));
    await page.activate();
    const projectBtn = tabs.querySelector('[data-mcp-tab="project"]');
    expect(projectBtn.classList.contains("hidden")).toBe(true);
    expect(tabs.querySelector('[data-mcp-tab="piGlobal"]').classList.contains("hidden")).toBe(
      false,
    );
    // A hidden tab cannot become active even if clicked programmatically.
    projectBtn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(tabs.querySelector('[data-mcp-tab="piGlobal"]').getAttribute("aria-selected")).toBe(
      "true",
    );
    expect(captionEl.textContent).toBe(`Global · ${LIST.data.groups.piGlobal.length}`);
  });

  it("renders two tabs, both with an add button; availability is always true", async () => {
    const { page, masterEl, tabs, navItem } = mount(makeGateway(LIST));
    await page.activate();
    expect(await page.refreshAvailability()).toBe(true);
    const tabButtons = Array.from(tabs.querySelectorAll("[data-mcp-tab]"));
    expect(tabButtons.map((b) => b.getAttribute("aria-selected"))).toEqual(["true", "false"]);
    expect(masterEl.querySelector(".models-provider-add")).not.toBeNull(); // user tab
    clickTab(tabs, "project");
    expect(masterEl.querySelector(".models-provider-add")).not.toBeNull(); // project tab
    expect(navItem.className).toBe("hidden"); // page never touches the nav anymore
  });

  it("disabled entry shows the badge; enabled entries do not", async () => {
    const { page, masterEl } = mount(makeGateway(LIST));
    await page.activate();
    const paused = rowFor(masterEl, "paused");
    expect(paused.querySelector("[data-disabled-badge]").textContent).toBe("disabled");
    clickRow(masterEl, "context7");
    // Enabled rows carry no config chip: any badge there comes from live state.
    expect(
      rowFor(masterEl, "context7").querySelectorAll(".mcp-badge, [data-disabled-badge]").length,
    ).toBe(0);
  });

  it("user tab: editable form saves a normalized entry with the piGlobal scope", async () => {
    const gateway = makeGateway(LIST);
    const { page, masterEl, detailEl } = mount(gateway);
    await page.activate();
    clickRow(masterEl, "context7");
    const form = detailEl.querySelector(".mcp-form");
    expect(form).not.toBeNull();
    expect(form.querySelector('input[placeholder="npx"]').value).toBe("npx");
    // ${VAR} placeholders are shown literally, never interpolated.
    // biome-ignore lint/suspicious/noTemplateCurlyInString: MCP ${VAR} placeholder data
    expect(form.querySelector('textarea[placeholder^="API_KEY="]').value).toContain("K=${V}");
    form.dispatchEvent(new Event("submit"));
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_save_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const payload = gateway.call.mock.calls.find((c) => c[0] === "mcp_save_server")[1];
    expect(payload.scope).toBe("piGlobal");
    expect(payload.name).toBe("context7");
    expect(payload.entry.command).toBe("npx");
    // biome-ignore lint/suspicious/noTemplateCurlyInString: MCP ${VAR} placeholder data
    expect(payload.entry.env).toEqual({ K: "${V}" });
  });

  it("array command: joined display, unmodified save passes the original array through", async () => {
    const gateway = makeGateway(LIST);
    const { page, masterEl, detailEl } = mount(gateway);
    await page.activate();
    clickRow(masterEl, "chrome-devtools");
    const form = detailEl.querySelector(".mcp-form");
    const command = form.querySelector('input[placeholder="npx"]');
    expect(command.value).toBe("npx -y chrome-devtools-mcp"); // joined display
    form.dispatchEvent(new Event("submit")); // command untouched
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_save_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const payload = gateway.call.mock.calls.find((c) => c[0] === "mcp_save_server")[1];
    expect(payload.entry.command).toEqual(["npx", "-y", "chrome-devtools-mcp"]); // verbatim array
    expect(payload.entry.futureField).toEqual({ a: 1 }); // unknown keys preserved
  });

  it("exposure select defaults to codemode and round-trips the choice", async () => {
    const gateway = makeGateway(LIST);
    const { page, masterEl, detailEl } = mount(gateway);
    await page.activate();
    clickRow(masterEl, "context7"); // no exposure in config
    const form = detailEl.querySelector(".mcp-form");
    const exposure = Array.from(form.querySelectorAll("select")).at(-1);
    expect(exposure.value).toBe("codemode");
    form.dispatchEvent(new Event("submit"));
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_save_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const payload = gateway.call.mock.calls.find((c) => c[0] === "mcp_save_server")[1];
    expect("exposure" in payload.entry).toBe(false); // default stays omitted

    exposure.value = "direct";
    form.dispatchEvent(new Event("submit"));
    await vi.waitFor(() => {
      const saves = gateway.call.mock.calls.filter((c) => c[0] === "mcp_save_server");
      expect(saves.at(-1)[1].entry.exposure).toBe("direct");
    });

    exposure.value = "codemode";
    form.dispatchEvent(new Event("submit"));
    await vi.waitFor(() => {
      const saves = gateway.call.mock.calls.filter((c) => c[0] === "mcp_save_server");
      expect("exposure" in saves.at(-1)[1].entry).toBe(false); // back to default removes the key
    });
  });

  it("detail: exactly one switch at the top; clicking sends scope+name+disable", async () => {
    const gateway = makeGateway(LIST);
    const { page, masterEl, detailEl } = mount(gateway);
    await page.activate();
    clickRow(masterEl, "context7");
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBe(1); // no duplicate
    const toggle = detailEl.querySelector('.mcp-entry [role="switch"]');
    expect(toggle.getAttribute("aria-checked")).toBe("true"); // enabled
    toggle.click();
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_toggle_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const payload = gateway.call.mock.calls.find((c) => c[0] === "mcp_toggle_server")[1];
    expect(payload).toEqual({ scope: "piGlobal", name: "context7", disable: true });
  });

  it("project tab entry carries the project scope in its toggle payload", async () => {
    const gateway = makeGateway(LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "repoTool");
    detailEl.querySelector('.mcp-entry [role="switch"]').click();
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_toggle_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const payload = gateway.call.mock.calls.find((c) => c[0] === "mcp_toggle_server")[1];
    expect(payload.scope).toBe("project");
  });

  it("migration banners: one per available target, click migrates then reloads", async () => {
    const withMigrations = {
      ok: true,
      data: {
        ...LIST.data,
        migrations: [
          {
            id: "adapterGlobal",
            sourceFile: "/home/u/.pi/agent/mcp-adapter.json",
            missing: ["zread"],
          },
          {
            id: "sharedGlobal",
            sourceFile: "/home/u/.agents/mcp.json",
            missing: ["grep", "first"],
          },
        ],
      },
    };
    const afterMigrate = { ok: true, data: { ...LIST.data, migrations: [] } };
    const call = vi
      .fn()
      .mockResolvedValueOnce(withMigrations) // activate() load
      .mockResolvedValueOnce({ ok: true, data: { migrated: ["zread"], skipped: [], lossy: [] } })
      .mockResolvedValueOnce(afterMigrate); // reload after migrate
    const { page, masterEl, migrationsEl } = mount({ call });

    await page.activate();
    const banners = migrationsEl.querySelectorAll(".mcp-legacy-notice");
    expect(banners.length).toBe(2);
    // Notices live OUTSIDE the master list entirely (below the layout).
    expect(masterEl.querySelector(".mcp-legacy-notice")).toBeNull();
    expect(banners[0].textContent).toContain("mcp-adapter.json");
    expect(banners[0].textContent).toContain("1"); // missing count
    expect(banners[1].textContent).toContain(".agents/mcp.json");
    expect(banners[1].textContent).toContain("2");

    banners[0].querySelector(".mcp-legacy-migrate").click();
    await new Promise((r) => setTimeout(r, 0));
    await new Promise((r) => setTimeout(r, 0));
    expect(call.mock.calls.map((c) => c[0])).toEqual([
      "mcp_list_servers",
      "mcp_migrate_adapter_config",
      "mcp_list_servers",
    ]);
    expect(call.mock.calls[1][1]).toEqual({ target: "adapterGlobal" });
    expect(migrationsEl.querySelectorAll(".mcp-legacy-notice").length).toBe(0);
  });

  it("save and delete share one action row in the edit form; add has no delete", async () => {
    const { page, masterEl, detailEl } = mount(makeGateway(LIST));
    await page.activate();
    clickRow(masterEl, "context7");
    const actions = detailEl.querySelector(".mcp-form-actions");
    expect(actions.textContent).toContain("Save");
    expect(actions.textContent).toContain("Delete");

    const add2 = mount(makeGateway(LIST));
    await add2.page.activate();
    add2.masterEl.querySelector(".models-provider-add").click();
    const addActions = add2.detailEl.querySelector(".mcp-form-actions");
    expect(addActions.textContent).toContain("Save");
    expect(addActions.textContent).not.toContain("Delete");
  });

  it("gateway rejection surfaces as an error status instead of an unhandled rejection", async () => {
    const gateway = { call: vi.fn().mockRejectedValue(new Error("request timed out")) };
    const { page, masterEl, detailEl } = mount(gateway);
    await page.activate();
    expect(masterEl.children.length).toBe(0); // load failed → empty master
    expect(detailEl.textContent).toContain("request timed out");
  });

  describe("live status badges", () => {
    async function mountWithStatus(extra = {}) {
      const gateway = makeGateway(LIST_OAUTH);
      const mcpLogin = makeMcpLogin();
      const mounted = mount(gateway, { mcpLogin, openExternal: vi.fn(), ...extra });
      await mounted.page.activate();
      return { ...mounted, gateway, mcpLogin };
    }

    it("queries mcp_server_status once per activation and never polls the page", async () => {
      const { mcpLogin } = await mountWithStatus();
      expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(1);
      expect(mcpLogin.status).not.toHaveBeenCalled();
      // Idle time must not produce a second status query (the host report has
      // real connection cost; only an in-flight login polls).
      await new Promise((resolve) => setTimeout(resolve, 30));
      expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(1);
    });
    it("merges state, tool count, and error detail onto the matching rows", async () => {
      const { masterEl } = await mountWithStatus();

      const connected = rowFor(masterEl, "context7").querySelector(".mcp-status-badge");
      expect(connected.className).toContain("is-connected");
      expect(connected.textContent).toEqual("Connected · 3 tools");

      const needsAuth = rowFor(masterEl, "sentry").querySelector(".mcp-status-badge");
      expect(needsAuth.className).toContain("is-needs-auth");
      expect(needsAuth.textContent).toEqual("Sign in required");

      const error = rowFor(masterEl, "flaky").querySelector(".mcp-status-badge");
      expect(error.className).toContain("is-error");
      expect(error.textContent).toContain("ECONNREFUSED");
      expect(error.title).toEqual("connect ECONNREFUSED 127.0.0.1:9999");

      // A disabled entry carries the config chip, not a live report: see
      // "a disabled config entry never shows the cached live state".
      const disabled = rowFor(masterEl, "paused").querySelector("[data-disabled-badge]");
      expect(disabled.textContent).toBe("disabled");
    });

    it("a disabled config entry never shows the cached live state", async () => {
      // The host caches `pi mcp list --json` for 60s, so a report read before
      // the switch was flipped can outlive it: the config flag wins.
      const reports = STATUS_SERVERS.map((server) =>
        server.name === "paused" ? { ...server, state: "connected", tools: [{}] } : server,
      );
      const { masterEl } = await mountWithStatus({ mcpLogin: makeMcpLogin(reports) });

      const row = rowFor(masterEl, "paused");
      const dot = row.querySelector(".pkg-manager-status-dot");
      expect(dot.className).toContain("is-disabled");
      expect(dot.className).not.toContain("is-loaded");
      expect(row.querySelector("[data-disabled-badge]").textContent).toBe("disabled");
      // A stale "connected" report must not leak a badge onto the row either.
      expect(row.querySelector(".mcp-status-badge")).toBeNull();
      expect(row.textContent).not.toContain("Connected");

      // Enabled entries keep their live-state semantics untouched.
      const live = rowFor(masterEl, "context7");
      expect(live.querySelector(".pkg-manager-status-dot").className).toContain("is-loaded");
      expect(live.querySelector(".mcp-status-badge").textContent).toBe("Connected · 3 tools");
    });
    it("a disabled entry keeps its detail badge and sign-out despite a cached live report", async () => {
      const reports = STATUS_SERVERS.map((server) =>
        server.name === "paused" ? { ...server, state: "connected", tools: [{}] } : server,
      );
      const { masterEl, detailEl } = await mountWithStatus({ mcpLogin: makeMcpLogin(reports) });

      clickRow(masterEl, "paused");
      expect(detailEl.querySelector("[data-disabled-badge]")).not.toBeNull();
      expect(detailEl.textContent).not.toContain("Connected");
      // Clearing stored credentials does not depend on the entry being enabled.
      expect(detailEl.querySelector('[data-action="mcp-logout"]')).not.toBeNull();
    });
    it("offers sign-in only for http rows awaiting authorization", async () => {
      const { masterEl, detailEl } = await mountWithStatus();

      clickRow(masterEl, "context7"); // connected stdio
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();
      expect(detailEl.querySelector('[data-action="mcp-logout"]')).toBeNull();

      clickRow(masterEl, "sentry"); // needs-auth http
      const signIn = detailEl.querySelector('[data-action="mcp-login"]');
      expect(signIn).not.toBeNull();
      expect(signIn.disabled).toBe(false);
      expect(signIn.textContent).toEqual("Sign in");
      expect(detailEl.querySelector('[data-action="mcp-logout"]')).toBeNull();

      // Config-disabled rows keep a disabled badge and no sign-in affordance.
      clickRow(masterEl, "paused");
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();

      // An `error` row cannot be fixed by `/mcp login` (headers auth, env
      // problems): no sign-in promise, the error badge carries the detail.
      clickRow(masterEl, "flaky");
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();

      // No live report at all (status query failed / server not listed):
      // no button either — the row stays a plain config entry.
      clickRow(masterEl, "ghost");
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();
    });

    it("renders the tab caption outside the master list and follows tab switches", async () => {
      const { masterEl, captionEl, tabs } = await mountWithStatus();
      const groupCount = LIST_OAUTH.data.groups.piGlobal.length;
      expect(captionEl.textContent).toBe(`Global · ${groupCount}`);
      // The caption lives outside the master list; the list itself starts
      // with a row, not the scope header.
      expect(masterEl.textContent).not.toContain(`Global · ${groupCount}`);

      clickTab(tabs, "project");
      expect(captionEl.textContent).toBe(
        `Current project · ${LIST_OAUTH.data.groups.project.length}`,
      );
    });

    it("offers sign-out only for connected http rows", async () => {
      const { masterEl, detailEl } = await mountWithStatus();
      clickRow(masterEl, "remote-ok");
      const signOut = detailEl.querySelector('[data-action="mcp-logout"]');
      expect(signOut).not.toBeNull();
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();
    });

    it("offers no sign-in for an unreported project server and never infers trust", async () => {
      const { masterEl, detailEl, tabs } = await mountWithStatus();
      clickTab(tabs, "project");
      clickRow(masterEl, "remoteProj");

      // No report means unknown state, not "untrusted": the page shows no
      // OAuth control and no trust claim it cannot back with a report.
      expect(detailEl.querySelector('[data-action="mcp-login"]')).toBeNull();
      expect(detailEl.textContent).not.toContain("Project not trusted");

      // A server pi DOES report awaiting authorization keeps the normal flow.
      clickTab(tabs, "piGlobal");
      clickRow(masterEl, "sentry");
      expect(detailEl.querySelector('[data-action="mcp-login"]').disabled).toBe(false);
    });

    it("sign-in mounts the dialog and refreshes status and list on success", async () => {
      const { gateway, masterEl, detailEl, mcpLogin } = await mountWithStatus();
      clickRow(masterEl, "sentry");
      detailEl.querySelector('[data-action="mcp-login"]').click();

      await vi.waitFor(() => expect(mcpLogin.start).toHaveBeenCalledWith("sentry"));
      expect(document.querySelector(".mcp-login-dialog-backdrop")).not.toBeNull();

      const listCalls = gateway.call.mock.calls.length;
      mcpLogin.emit({ operationId: "op-1", status: "succeeded" });

      await vi.waitFor(() => {
        expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(2);
        expect(gateway.call.mock.calls.length).toBe(listCalls + 1);
      });
      expect(document.querySelector(".mcp-login-dialog-backdrop")).toBeNull();
      expect(gateway.call.mock.calls.at(-1)[0]).toBe("mcp_list_servers");
    });

    it("sign-out calls mcp_logout and refreshes status and list", async () => {
      const { gateway, masterEl, detailEl, mcpLogin } = await mountWithStatus();
      clickRow(masterEl, "remote-ok");
      detailEl.querySelector('[data-action="mcp-logout"]').click();

      await vi.waitFor(() => expect(mcpLogin.logout).toHaveBeenCalledWith("remote-ok"));
      await vi.waitFor(() => expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(2));
      await vi.waitFor(() => expect(gateway.call.mock.calls.at(-1)[0]).toBe("mcp_list_servers"));
      expect(gateway.call.mock.calls.at(-1)[0]).toBe("mcp_list_servers");
    });

    it("a failed status query degrades to the plain config list", async () => {
      const gateway = makeGateway(LIST_OAUTH);
      const mcpLogin = makeMcpLogin();
      mcpLogin.serverStatus = vi.fn(async () => ({ ok: false, error: "pi mcp list failed" }));
      const { page, masterEl } = mount(gateway, { mcpLogin, openExternal: vi.fn() });
      await page.activate();

      expect(masterEl.querySelectorAll(".mcp-status-badge").length).toBe(0);
      expect(masterEl.querySelector(".mcp-status-error").textContent).toContain(
        "Live MCP status unavailable.",
      );
      expect(rowFor(masterEl, "sentry")).not.toBeUndefined();
    });

    it("a rejected status query never breaks the page", async () => {
      const gateway = makeGateway(LIST_OAUTH);
      const mcpLogin = makeMcpLogin();
      mcpLogin.serverStatus = vi.fn(async () => {
        throw new Error("Transport is not connected");
      });
      const { page, masterEl } = mount(gateway, { mcpLogin, openExternal: vi.fn() });
      await page.activate();

      expect(masterEl.querySelectorAll(".mcp-status-badge").length).toBe(0);
      expect(rowFor(masterEl, "sentry")).not.toBeUndefined();
    });
  });

  describe("host login surface adapter", () => {
    it("carries a real WS mcpLoginUpdate frame through the transport into the dialog", async () => {
      const listeners = new Map();
      const wsClient = {
        capabilities: { native: true },
        addEventListener: (type, handler) => listeners.set(type, handler),
        removeEventListener: (type) => listeners.delete(type),
        sendControl: vi.fn(async (op) => {
          if (op === "mcp_login_start") return { ok: true, operationId: "op-1" };
          if (op === "mcp_server_status") return { ok: true, servers: STATUS_SERVERS };
          return { ok: true };
        }),
      };
      const { page, masterEl, detailEl } = mount(makeGateway(LIST_OAUTH), {
        mcpLogin: createMcpHostOps(new WsTransport(wsClient, {})),
        openExternal: vi.fn(),
      });
      await page.activate();
      clickRow(masterEl, "sentry");
      detailEl.querySelector('[data-action="mcp-login"]').click();

      await vi.waitFor(() =>
        expect(wsClient.sendControl).toHaveBeenCalledWith(
          "mcp_login_start",
          { name: "sentry" },
          expect.anything(),
        ),
      );
      // Exactly the host frame shape the Rust runner emits.
      listeners.get("mcpLoginUpdate")({
        detail: { type: "mcpLoginUpdate", payload: { operationId: "op-1", status: "succeeded" } },
      });

      await vi.waitFor(() =>
        expect(document.querySelector(".mcp-login-dialog-backdrop")).toBeNull(),
      );
      expect(wsClient.sendControl).toHaveBeenCalledWith("mcp_server_status", {}, expect.anything());
    });

    it("forwards the page login surface onto the transport control ops", async () => {
      const transport = {
        mcpLoginStart: vi.fn(async () => ({ ok: true, operationId: "op-1" })),
        mcpLoginCancel: vi.fn(async () => ({ ok: true, cancelled: true })),
        mcpLoginStatus: vi.fn(async () => ({ ok: true, status: "pending" })),
        mcpLogout: vi.fn(async () => ({ ok: true })),
        mcpServerStatus: vi.fn(async () => ({ ok: true, servers: [] })),
        onMcpLoginUpdate: vi.fn(() => () => {}),
      };
      const ops = createMcpHostOps(transport);
      const listener = vi.fn();

      await ops.start("sentry");
      await ops.cancel("op-1");
      await ops.status("op-1");
      await ops.logout("sentry");
      await ops.serverStatus();
      ops.subscribe(listener);

      expect(transport.mcpLoginStart).toHaveBeenCalledWith("sentry");
      expect(transport.mcpLoginCancel).toHaveBeenCalledWith("op-1");
      expect(transport.mcpLoginStatus).toHaveBeenCalledWith("op-1");
      expect(transport.mcpLogout).toHaveBeenCalledWith("sentry");
      expect(transport.mcpServerStatus).toHaveBeenCalledWith(undefined);
      expect(transport.onMcpLoginUpdate).toHaveBeenCalledWith(listener);
    });
  });
});

// ── Project override detail, batch import and action-bound dispatch ──────

const PROJECT_REVISIONS = { piGlobal: "global-rev", project: "project-rev" };

const OVERRIDE_LIST = {
  ok: true,
  data: {
    groups: {
      piGlobal: [
        {
          name: "docs",
          entry: { url: "https://example.test/mcp" },
          sourceFile: "/home/u/.pi/agent/mcp.json",
          editable: true,
          enabled: true,
          kind: "definition",
          revision: "global-rev",
        },
      ],
      project: [
        {
          name: "docs",
          entry: { exposure: "direct" },
          sourceFile: "/ws/repo/.pi/mcp.json",
          editable: true,
          enabled: false,
          kind: "override",
          effective: { enabled: false, exposure: "direct", toolExposure: { "delete_*": "hidden" } },
          identity: {
            scope: "global",
            source: "/home/u/.pi/agent/mcp.json",
            override: "/ws/repo/.pi/mcp.json",
          },
          revision: "project-rev",
        },
        {
          name: "orphan",
          entry: { enabled: false },
          sourceFile: "/ws/repo/.pi/mcp.json",
          editable: true,
          enabled: false,
          kind: "invalid",
          validationError:
            'server "orphan" needs "command" or "url", or a global server to override',
          revision: "project-rev",
        },
        {
          name: "repoTool",
          entry: { command: "run repo" },
          sourceFile: "/ws/repo/.pi/mcp.json",
          editable: true,
          enabled: true,
          kind: "definition",
          revision: "project-rev",
        },
      ],
    },
    groupErrors: {},
    migrations: [],
    projectAvailable: true,
    projectTrusted: true,
    revisions: PROJECT_REVISIONS,
  },
};

describe("mcp-page project overrides", () => {
  it("renders the three-field detail for an override and hides connection controls", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    const entry = detailEl.querySelector(".mcp-entry");
    expect(entry.querySelectorAll("input")).toHaveLength(0);
    expect(entry.querySelectorAll("select")).toHaveLength(1);
    expect(entry.querySelectorAll("textarea")).toHaveLength(1);
    expect(entry.querySelector('[role="switch"]').getAttribute("aria-checked")).toBe("false");
    expect(entry.querySelector("textarea").value).toBe(
      JSON.stringify({ "delete_*": "hidden" }, null, 2),
    );
    expect(entry.querySelector(".mcp-login")).toBeNull();
    expect(
      [...entry.querySelectorAll(".mcp-form-actions button")].map((b) => b.textContent),
    ).toEqual(["Save", "Remove project override"]);
  });

  it("saves the explicit three-field snapshot with revisions and the frozen binding", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const target = { workspaceId: "w1", sessionId: "s1", instanceId: "i1" };
    const { page, masterEl, detailEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => target,
      getContextKey: () => "w1:7",
    });
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    detailEl.querySelector("form").dispatchEvent(new Event("submit"));
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_save_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const [, payload, options] = gateway.call.mock.calls.find((c) => c[0] === "mcp_save_server");
    expect(payload).toEqual({
      scope: "project",
      name: "docs",
      kind: "override",
      intent: "edit",
      entry: { enabled: false, exposure: "direct", toolExposure: { "delete_*": "hidden" } },
      expectedRevision: "project-rev",
      expectedGlobalRevision: "global-rev",
    });
    expect(options.target).toEqual(target);
    expect(typeof options.beforeSend).toBe("function");
  });

  it("toggles only the enabled field with the rendered revision", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => ({ workspaceId: "w1", sessionId: "s1" }),
    });
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_toggle_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const [, payload] = gateway.call.mock.calls.find((c) => c[0] === "mcp_toggle_server");
    expect(payload).toEqual({
      scope: "project",
      name: "docs",
      disable: false,
      expectedRevision: "project-rev",
      expectedGlobalRevision: "global-rev",
    });
  });

  it("removes the override and explains that the global server still applies", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    detailEl.querySelector(".mcp-btn-danger").click();
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_delete_server",
        expect.anything(),
        expect.anything(),
      ),
    );
    const [, payload] = gateway.call.mock.calls.find((c) => c[0] === "mcp_delete_server");
    expect(payload).toEqual({ scope: "project", name: "docs", expectedRevision: "project-rev" });
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Removes this entry from the project mcp.json only.",
      ),
    );
  });

  it("shows an invalid project entry as remove-only with its diagnostic", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "orphan");

    const entry = detailEl.querySelector(".mcp-entry");
    expect(entry.querySelector("form")).toBeNull();
    expect(entry.querySelector(".mcp-group-error").textContent).toMatch(
      /global server to override/,
    );
    expect(entry.querySelector('[role="switch"]').disabled).toBe(true);
    expect(entry.querySelector(".mcp-btn-danger").disabled).toBe(false);
  });

  it("keeps full project definitions on the existing connection editor", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "repoTool");
    const form = detailEl.querySelector(".mcp-form");
    expect(form.querySelector('input[placeholder="npx"]').value).toBe("run repo");
    expect(detailEl.querySelector(".mcp-entry-raw")).toBeNull();
  });
});

describe("mcp-page batch import", () => {
  it("offers the import only for a trusted project and sends one bound batch request", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    const target = { workspaceId: "w1", sessionId: "s1" };
    const { page, masterEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => target,
      getContextKey: () => "w1:7",
    });
    await page.activate();
    expect(masterEl.querySelector('[data-action="mcp-import-global"]')).toBeNull();
    clickTab(tabs, "project");
    const button = masterEl.querySelector('[data-action="mcp-import-global"]');
    expect(button).not.toBeNull();
    expect(button.textContent).toBe("Import global settings");

    button.click();
    button.click(); // second click while the batch is pending is ignored
    await vi.waitFor(() =>
      expect(gateway.call).toHaveBeenCalledWith(
        "mcp_import_global_overrides",
        {},
        expect.anything(),
      ),
    );
    const calls = gateway.call.mock.calls.filter((c) => c[0] === "mcp_import_global_overrides");
    expect(calls).toHaveLength(1);
    expect(calls[0][2].target).toEqual(target);
  });

  it("hides the import without project trust", async () => {
    const gateway = makeGateway({
      ok: true,
      data: {
        ...OVERRIDE_LIST.data,
        projectTrusted: false,
        groups: { ...OVERRIDE_LIST.data.groups, project: [] },
      },
    });
    const { page, masterEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => ({ workspaceId: "w1", sessionId: "s1" }),
    });
    await page.activate();
    clickTab(tabs, "project");
    expect(masterEl.querySelector('[data-action="mcp-import-global"]')).toBeNull();
  });

  it("reports added/existing/skipped counts with skip names and survives the list refresh", async () => {
    const gateway = {
      call: vi.fn(async (op) => {
        if (op === "mcp_import_global_overrides") {
          return {
            ok: true,
            data: {
              imported: ["docs", "paused"],
              skipped: [
                { name: "keep", reason: "existing" },
                { name: "dev-tools", reason: "namespace-conflict", conflictWith: "dev_tools" },
                { name: "broken", reason: "invalid-global", detail: "command must be a string" },
              ],
              changed: true,
              path: "/ws/repo/.pi/mcp.json",
              revision: "new-rev",
            },
          };
        }
        return OVERRIDE_LIST;
      }),
    };
    const { page, masterEl, detailEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => ({ workspaceId: "w1", sessionId: "s1" }),
    });
    await page.activate();
    clickTab(tabs, "project");
    masterEl.querySelector('[data-action="mcp-import-global"]').click();

    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Added 2, already present 1, skipped 2.",
      ),
    );
    const text = detailEl.querySelector(".mcp-detail-status").textContent;
    expect(text).toContain("keep: already in this project");
    expect(text).toContain("dev-tools: collides with dev_tools");
    expect(text).toContain("broken: not a valid global server (command must be a string)");
  });

  it("surfaces an import failure without pretending success", async () => {
    const gateway = {
      call: vi.fn(async (op) =>
        op === "mcp_import_global_overrides"
          ? { ok: false, error: "Project MCP config is unreadable" }
          : OVERRIDE_LIST,
      ),
    };
    const { page, masterEl, detailEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => ({ workspaceId: "w1", sessionId: "s1" }),
    });
    await page.activate();
    clickTab(tabs, "project");
    masterEl.querySelector('[data-action="mcp-import-global"]').click();
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Import failed: Project MCP config is unreadable",
      ),
    );
  });

  it("rejects project mutations when the routing target disappears", async () => {
    const gateway = makeGateway(OVERRIDE_LIST);
    let target = { workspaceId: "w1", sessionId: "s1" };
    const { page, masterEl, detailEl, tabs } = mount(gateway, { getRuntimeTarget: () => target });
    await page.activate();
    clickTab(tabs, "project");
    target = null; // the workspace is being torn down
    masterEl.querySelector('[data-action="mcp-import-global"]').click();
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    expect(gateway.call.mock.calls.some((c) => c[0] === "mcp_import_global_overrides")).toBe(false);
  });

  it("drops a stale import result when the workspace changed while it was in flight", async () => {
    let target = { workspaceId: "w1", sessionId: "s1" };
    let resolveImport;
    const pending = new Promise((resolve) => {
      resolveImport = resolve;
    });
    const gateway = {
      call: vi.fn(async (op, _params, options) => {
        if (op !== "mcp_import_global_overrides") return OVERRIDE_LIST;
        await pending;
        try {
          options?.beforeSend?.();
        } catch (error) {
          return { ok: false, error: error.message };
        }
        return { ok: true, data: { imported: ["docs"], skipped: [], changed: true } };
      }),
    };
    const { page, masterEl, detailEl, tabs } = mount(gateway, {
      getRuntimeTarget: () => target,
      getContextKey: () => `${target.workspaceId}:1`,
    });
    await page.activate();
    clickTab(tabs, "project");
    masterEl.querySelector('[data-action="mcp-import-global"]').click();
    // The workspace switches while the batch is still waiting.
    target = { workspaceId: "w2", sessionId: "s2" };
    resolveImport();
    // The stale result is discarded: no summary, and no follow-up reload for
    // the workspace the user already left.
    await vi.waitFor(() =>
      expect(
        gateway.call.mock.calls.filter((c) => c[0] === "mcp_import_global_overrides"),
      ).toHaveLength(1),
    );
    await Promise.resolve();
    expect(detailEl.querySelector(".mcp-detail-status").textContent).not.toContain("Added");
    expect(gateway.call.mock.calls.filter((c) => c[0] === "mcp_list_servers")).toHaveLength(1);
  });
});

describe("mcp-page status freshness", () => {
  function deferredLogin() {
    const pending = [];
    return {
      pending,
      ops: {
        start: vi.fn(),
        cancel: vi.fn(),
        status: vi.fn(),
        logout: vi.fn(),
        subscribe: vi.fn(() => () => {}),
        serverStatus: vi.fn(() => new Promise((resolve) => pending.push(resolve))),
      },
    };
  }

  const CONNECTED = {
    ok: true,
    servers: [
      {
        name: "context7",
        scope: "global",
        source: "/home/u/.pi/agent/mcp.json",
        state: "connected",
        transport: "stdio",
        tools: [{}, {}],
      },
    ],
    errors: [],
  };
  const DISABLED_WITH_ERRORS = {
    ok: true,
    servers: [
      {
        name: "context7",
        scope: "global",
        source: "/home/u/.pi/agent/mcp.json",
        state: "disabled",
        transport: "stdio",
        tools: [],
      },
    ],
    errors: ["MCP configuration error; see Pi logs"],
    note: "The project .pi/mcp.json is ignored because the project is not trusted.",
  };

  it("does not let an older success overwrite the refreshed status", async () => {
    const gateway = makeGateway(LIST);
    const { pending, ops } = deferredLogin();
    const { page, masterEl, detailEl } = mount(gateway, { mcpLogin: ops });
    const activating = page.activate();
    await vi.waitFor(() => expect(ops.serverStatus).toHaveBeenCalledTimes(1));
    pending[0](CONNECTED); // activation query (context A)
    await activating;
    expect(rowFor(masterEl, "context7").textContent).toContain("Connected · 2 tools");

    // A save invalidates the page's status view and starts a refresh query.
    clickRow(masterEl, "context7");
    detailEl.querySelector('.mcp-entry [role="switch"]').click();
    await vi.waitFor(() => expect(ops.serverStatus).toHaveBeenCalledTimes(2));
    expect(ops.serverStatus.mock.calls[1][0]).toEqual({ refresh: true });
    pending[1](DISABLED_WITH_ERRORS);
    await vi.waitFor(() => expect(rowFor(masterEl, "context7").textContent).toContain("Disabled"));

    // The older query only resolves now: its connected badge must not return.
    expect(pending).toHaveLength(2);
    pending[0](CONNECTED);
    await Promise.resolve();
    await Promise.resolve();
    expect(rowFor(masterEl, "context7").textContent).toContain("Disabled");
    expect(rowFor(masterEl, "context7").textContent).not.toContain("Connected");
  });

  it("does not let an older failure clear the refreshed status or raise a query error", async () => {
    const gateway = makeGateway(LIST);
    const { pending, ops } = deferredLogin();
    const { page, masterEl, detailEl } = mount(gateway, { mcpLogin: ops });
    const activating = page.activate();
    await vi.waitFor(() => expect(ops.serverStatus).toHaveBeenCalledTimes(1));
    pending[0](CONNECTED);
    await activating;

    clickRow(masterEl, "context7");
    detailEl.querySelector('.mcp-entry [role="switch"]').click();
    await vi.waitFor(() => expect(ops.serverStatus).toHaveBeenCalledTimes(2));
    pending[1](DISABLED_WITH_ERRORS);
    await vi.waitFor(() => expect(rowFor(masterEl, "context7").textContent).toContain("Disabled"));

    // The stale query fails after the refresh landed: no error banner, no
    // cleared badge, and the diagnostics stay those of the newest response.
    pending[0]({ ok: false, error: "MCP status unavailable" });
    await Promise.resolve();
    await Promise.resolve();
    expect(masterEl.querySelector(".mcp-status-error")).toBeNull();
    expect(rowFor(masterEl, "context7").textContent).toContain("Disabled");
  });

  it("renders envelope diagnostics separately from the operation summary", async () => {
    const gateway = makeGateway(LIST);
    const { pending, ops } = deferredLogin();
    const { page, masterEl } = mount(gateway, { mcpLogin: ops });
    const activating = page.activate();
    await vi.waitFor(() => expect(ops.serverStatus).toHaveBeenCalledTimes(1));
    pending[0](DISABLED_WITH_ERRORS);
    await activating;

    const diagnostics = masterEl.querySelector(".mcp-status-diagnostics");
    expect(diagnostics).not.toBeNull();
    expect(diagnostics.textContent).toContain("1 server(s) reported errors.");
    expect(diagnostics.textContent).toContain("not trusted");
    // Diagnostics are page-level status, never the save/import summary line.
    expect(document.querySelector(".mcp-detail-status")).toBeNull();
  });

  it("fires the post-toggle status refresh before the reload it must survive", async () => {
    // The host caches `pi mcp list` for 60s. If the refresh only started after
    // the inventory reload, closing the page inside that window would leave the
    // cache serving the pre-toggle report to the next activation.
    const pendingLists = [];
    let deferList = false;
    const gateway = {
      call: vi.fn((op) => {
        if (op === "mcp_list_servers") {
          return deferList
            ? new Promise((resolve) => pendingLists.push(resolve))
            : Promise.resolve(LIST);
        }
        return Promise.resolve(LIST);
      }),
    };
    const mcpLogin = makeMcpLogin();
    const { page, masterEl, detailEl } = mount(gateway, { mcpLogin, openExternal: vi.fn() });
    await page.activate();
    expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(1);

    deferList = true; // the reload that follows the toggle never comes back
    clickRow(masterEl, "context7");
    detailEl.querySelector('.mcp-entry [role="switch"]').click();

    await vi.waitFor(() => expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(2));
    expect(mcpLogin.serverStatus.mock.calls[1][0]).toEqual({ refresh: true });
    expect(pendingLists).toHaveLength(1); // ...while the reload is still in flight

    // The page dies before the reload lands: the host invalidate already went
    // out, so the next activation cannot read the pre-toggle cache.
    page.destroy();
    pendingLists[0](LIST);
    await Promise.resolve();
    expect(mcpLogin.serverStatus).toHaveBeenCalledTimes(2);
  });
});

// ── Stale controls, status identity and draft survival (review regressions) ──

describe("mcp-page stale rendered controls", () => {
  function staleHarness(extra = {}) {
    const gateway = makeGateway(OVERRIDE_LIST);
    let target = { workspaceId: "A", sessionId: "sA", instanceId: "iA" };
    const mounted = mount(gateway, {
      getRuntimeTarget: () => target,
      getContextKey: () => `${target.workspaceId}:${target.sessionId}`,
      ...extra,
    });
    return { ...mounted, gateway, setTarget: (next) => (target = next) };
  }

  it("never sends from a control rendered for another workspace", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    // The workspace moves on without a re-render: the old A control is stale.
    h.setTarget({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });
    h.detailEl.querySelector('[role="switch"]').click();

    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_toggle_server")).toBe(false);
  });

  it("never sends a definition form or its delete from a stale render", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "repoTool");
    h.setTarget({ workspaceId: "B", sessionId: "sB" });
    h.detailEl.querySelector(".mcp-form").dispatchEvent(new Event("submit"));
    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_save_server")).toBe(false);
  });

  it("never sends the import button rendered for another workspace", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    h.setTarget({ workspaceId: "B", sessionId: "sB" });
    h.masterEl.querySelector('[data-action="mcp-import-global"]').click();
    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_import_global_overrides")).toBe(
      false,
    );
  });

  it("drops the stale inventory instead of repainting it under the new target", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    h.setTarget({ workspaceId: "B", sessionId: "sB", instanceId: "iB" });
    h.detailEl.querySelector('[role="switch"]').click();

    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    // The old rows are gone: a second click has no control to re-target at B.
    expect(h.masterEl.querySelectorAll(".pkg-manager-sidebar-row")).toHaveLength(0);
    expect(h.detailEl.querySelector('[role="switch"]')).toBeNull();
    expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_toggle_server")).toBe(false);
  });

  it("drops on a non-action repaint and reloads only for an explicit refresh", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    h.setTarget({ workspaceId: "B", sessionId: "sB" });
    // A tab repaint is not an action, but it must not launder A's rows either.
    clickTab(h.tabs, "piGlobal");
    clickTab(h.tabs, "project");
    expect(h.masterEl.querySelectorAll(".pkg-manager-sidebar-row")).toHaveLength(0);
    const listCalls = () =>
      h.gateway.call.mock.calls.filter((c) => c[0] === "mcp_list_servers").length;
    expect(listCalls()).toBe(1);

    h.masterEl.querySelector('[data-action="mcp-refresh"]').click();
    await vi.waitFor(() => expect(listCalls()).toBe(2));
    await vi.waitFor(() =>
      expect(h.masterEl.querySelectorAll(".pkg-manager-sidebar-row").length).toBeGreaterThan(0),
    );
  });

  it("drops the inventory on a same-workspace session adoption repaint", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    h.setTarget({ workspaceId: "A", sessionId: "sA2", instanceId: "iA2" });
    clickTab(h.tabs, "piGlobal");
    clickTab(h.tabs, "project");
    expect(h.masterEl.querySelectorAll(".pkg-manager-sidebar-row")).toHaveLength(0);
    expect(h.detailEl.querySelector('[role="switch"]')).toBeNull();
  });

  it("becomes actionable again only after refreshing against the current target", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    h.setTarget({ workspaceId: "B", sessionId: "sB" });
    clickTab(h.tabs, "piGlobal");
    clickTab(h.tabs, "project");
    h.setTarget({ workspaceId: "A", sessionId: "sA", instanceId: "iA" });
    h.masterEl.querySelector('[data-action="mcp-refresh"]').click();
    await vi.waitFor(() =>
      expect(h.masterEl.querySelector('[data-action="mcp-import-global"]')).not.toBeNull(),
    );
    clickRow(h.masterEl, "docs");
    h.detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() =>
      expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_toggle_server")).toBe(true),
    );
  });

  it("rejects a control rendered for the previous session of the same workspace", async () => {
    const h = staleHarness();
    await h.page.activate();
    clickTab(h.tabs, "project");
    clickRow(h.masterEl, "docs");
    h.setTarget({ workspaceId: "A", sessionId: "sA2", instanceId: "iA2" });
    h.detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() =>
      expect(h.detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "The workspace changed before the MCP request was sent.",
      ),
    );
    expect(h.gateway.call.mock.calls.some((c) => c[0] === "mcp_toggle_server")).toBe(false);
  });
});

describe("mcp-page override status identity", () => {
  const IDENTITY_LIST = {
    ok: true,
    data: {
      groups: {
        piGlobal: [
          {
            name: "docs",
            entry: { url: "https://example.test/mcp" },
            sourceFile: "/agent/mcp.json",
            editable: true,
            enabled: true,
            kind: "definition",
            revision: "g",
          },
        ],
        project: [
          {
            name: "docs",
            entry: { exposure: "direct" },
            sourceFile: "/ws/.pi/mcp.json",
            editable: true,
            enabled: true,
            kind: "override",
            effective: { enabled: true, exposure: "direct", toolExposure: {} },
            identity: { scope: "global", source: "/agent/mcp.json", override: "/ws/.pi/mcp.json" },
            revision: "p",
          },
          {
            name: "orphan",
            entry: { enabled: false },
            sourceFile: "/ws/.pi/mcp.json",
            editable: true,
            enabled: false,
            kind: "invalid",
            validationError: 'server "orphan" needs a global server to override',
            revision: "p",
          },
        ],
      },
      groupErrors: {},
      migrations: [],
      projectAvailable: true,
      projectTrusted: true,
      revisions: { piGlobal: "g", project: "p" },
    },
  };

  async function mountIdentity(reports) {
    const gateway = makeGateway(IDENTITY_LIST);
    const mcpLogin = {
      start: vi.fn(),
      cancel: vi.fn(),
      status: vi.fn(),
      logout: vi.fn(),
      subscribe: vi.fn(() => () => {}),
      serverStatus: vi.fn(async () => ({ ok: true, servers: reports, errors: [] })),
    };
    const mounted = mount(gateway, { mcpLogin });
    await mounted.page.activate();
    clickTab(mounted.tabs, "project");
    return mounted;
  }

  const CONNECTED = {
    name: "docs",
    scope: "global",
    source: "/agent/mcp.json",
    override: "/ws/.pi/mcp.json",
    transport: "http",
    state: "connected",
    tools: [{}],
  };

  it("matches an override by global scope, global source and override path", async () => {
    const { masterEl } = await mountIdentity([CONNECTED]);
    const badge = rowFor(masterEl, "docs").querySelector(".mcp-status-badge");
    expect(badge).not.toBeNull();
    expect(badge.textContent).toContain("Connected");
  });

  it("refuses to lend a same-named report from another source, scope or path", async () => {
    for (const wrong of [
      { ...CONNECTED, source: "/other/agent/mcp.json" },
      { ...CONNECTED, override: "/other/.pi/mcp.json" },
      { ...CONNECTED, scope: "project", override: undefined },
      { ...CONNECTED, override: undefined },
    ]) {
      const { masterEl } = await mountIdentity([wrong]);
      expect(rowFor(masterEl, "docs").querySelector(".mcp-status-badge")).toBeNull();
    }
  });

  it("never lends a report to an invalid or base-less override row", async () => {
    const { masterEl } = await mountIdentity([
      { ...CONNECTED, name: "orphan", scope: "global", source: "/ws/.pi/mcp.json" },
    ]);
    expect(rowFor(masterEl, "orphan").querySelector(".mcp-status-badge")).toBeNull();
  });

  it("matches a definition only from its own file and scope", async () => {
    const report = {
      name: "docs",
      scope: "global",
      source: "/agent/mcp.json",
      transport: "http",
      state: "connected",
      tools: [{}],
    };
    const { masterEl, tabs } = await mountIdentity([report]);
    clickTab(tabs, "piGlobal");
    clickRow(masterEl, "docs");
    expect(rowFor(masterEl, "docs").querySelector(".mcp-status-badge")).not.toBeNull();

    const other = await mountIdentity([{ ...report, source: "/elsewhere/mcp.json" }]);
    clickTab(other.tabs, "piGlobal");
    expect(rowFor(other.masterEl, "docs").querySelector(".mcp-status-badge")).toBeNull();
  });
});

describe("mcp-page override draft survival", () => {
  it("moves an unsaved map and exposure draft to the acknowledged revision", async () => {
    let revision = "p1";
    const gateway = {
      call: vi.fn(async (op) => {
        if (op === "mcp_toggle_server") {
          revision = "p2";
          return { ok: true, data: { revision: "p2", changed: true, enabled: true } };
        }
        if (op === "mcp_list_servers") {
          return {
            ok: true,
            data: {
              ...OVERRIDE_LIST.data,
              revisions: { piGlobal: "global-rev", project: revision },
              groups: {
                ...OVERRIDE_LIST.data.groups,
                project: OVERRIDE_LIST.data.groups.project.map((entry) =>
                  entry.name === "docs"
                    ? { ...entry, revision, effective: { ...entry.effective, enabled: true } }
                    : entry,
                ),
              },
            },
          };
        }
        return OVERRIDE_LIST;
      }),
    };
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    // Unsaved edits: a map the user typed and an exposure change.
    const textarea = detailEl.querySelector("textarea");
    textarea.value = '{ "read": "hidden" }';
    textarea.dispatchEvent(new Event("input"));
    const select = detailEl.querySelector("select");
    select.value = "deferred";
    select.dispatchEvent(new Event("change"));

    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() =>
      expect(gateway.call.mock.calls.some((c) => c[0] === "mcp_toggle_server")).toBe(true),
    );
    await vi.waitFor(() => {
      const rendered = detailEl.querySelector("textarea");
      expect(rendered.value).toBe('{ "read": "hidden" }');
      expect(detailEl.querySelector("select").value).toBe("deferred");
    });
  });
});

describe("mcp-page toggle draft handoff (review regressions)", () => {
  /** Gateway whose toggle acknowledgement is resolved by the test. */
  function pendingToggleGateway(getRevision) {
    let settle;
    const gateway = {
      call: vi.fn((op) => {
        if (op === "mcp_toggle_server") {
          return new Promise((resolve) => {
            settle = resolve;
          });
        }
        if (op === "mcp_list_servers") {
          const revision = getRevision();
          return Promise.resolve({
            ok: true,
            data: {
              ...OVERRIDE_LIST.data,
              revisions: { ...PROJECT_REVISIONS, project: revision },
              groups: {
                ...OVERRIDE_LIST.data.groups,
                project: OVERRIDE_LIST.data.groups.project.map((entry) =>
                  entry.name === "docs" ? { ...entry, revision } : entry,
                ),
              },
            },
          });
        }
        return Promise.resolve(OVERRIDE_LIST);
      }),
    };
    return { gateway, settle: (value) => settle(value) };
  }

  it("keeps edits typed while the toggle is pending and moves them to the acked revision", async () => {
    let revision = "project-rev";
    const { gateway, settle } = pendingToggleGateway(() => revision);
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    let textarea = detailEl.querySelector("textarea");
    textarea.value = '{"before":"hidden"}';
    textarea.dispatchEvent(new Event("input"));
    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() => expect(settle).toBeTruthy());

    // Edited while the acknowledgement is still in flight: the ack must carry
    // the latest draft, not the object captured before the request.
    textarea = detailEl.querySelector("textarea");
    textarea.value = '{"during":"hidden"}';
    textarea.dispatchEvent(new Event("input"));

    revision = "project-rev-2";
    settle({ ok: true, data: { revision: "project-rev-2", changed: true, enabled: true } });
    // Synchronize on the acknowledgement being fully handled (reload included).
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Saved. Reload the session to apply.",
      ),
    );
    expect(detailEl.querySelector("textarea").value).toBe('{"during":"hidden"}');
  });

  it("keeps the draft when a no-op acknowledgement repeats the revision", async () => {
    const { gateway, settle } = pendingToggleGateway(() => "project-rev");
    const { page, masterEl, detailEl, tabs } = mount(gateway);
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");

    const textarea = detailEl.querySelector("textarea");
    textarea.value = '{"keep":"hidden"}';
    textarea.dispatchEvent(new Event("input"));
    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() => expect(settle).toBeTruthy());
    settle({ ok: true, data: { revision: "project-rev", changed: false, enabled: false } });

    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Saved. Reload the session to apply.",
      ),
    );
    expect(detailEl.querySelector("textarea").value).toBe('{"keep":"hidden"}');
  });
});

describe("mcp-page toggle reload window (review regressions)", () => {
  /**
   * Gateway that acknowledges the toggle immediately and can hold the
   * follow-up inventory reload open, so the ack→reload window is observable.
   */
  function reloadWindowGateway() {
    const state = { revision: "project-rev", enabled: false, deferList: false, pending: [] };
    const listAt = () => ({
      ok: true,
      data: {
        ...OVERRIDE_LIST.data,
        revisions: { ...PROJECT_REVISIONS, project: state.revision },
        groups: {
          ...OVERRIDE_LIST.data.groups,
          project: OVERRIDE_LIST.data.groups.project.map((entry) =>
            entry.name === "docs"
              ? {
                  ...entry,
                  revision: state.revision,
                  enabled: state.enabled,
                  effective: { ...entry.effective, enabled: state.enabled },
                }
              : entry,
          ),
        },
      },
    });
    const gateway = {
      call: vi.fn((op) => {
        if (op === "mcp_toggle_server") {
          state.revision = "project-rev-2";
          state.enabled = true;
          return Promise.resolve({
            ok: true,
            data: { revision: "project-rev-2", changed: true, enabled: true },
          });
        }
        if (op === "mcp_list_servers") {
          if (state.deferList) return new Promise((resolve) => state.pending.push(resolve));
          return Promise.resolve(listAt());
        }
        return Promise.resolve(OVERRIDE_LIST);
      }),
    };
    return { gateway, state, listAt };
  }

  async function openDocs(gateway) {
    const mounted = mount(gateway);
    await mounted.page.activate();
    clickTab(mounted.tabs, "project");
    clickRow(mounted.masterEl, "docs");
    return mounted;
  }

  function setDraft(detailEl, { map, exposure }) {
    if (map !== undefined) {
      const textarea = detailEl.querySelector("textarea");
      textarea.value = map;
      textarea.dispatchEvent(new Event("input"));
    }
    if (exposure !== undefined) {
      const select = detailEl.querySelector("select");
      select.value = exposure;
      select.dispatchEvent(new Event("change"));
    }
  }

  it("keeps the acknowledged revision and both draft fields across a delayed reload", async () => {
    const { gateway, state, listAt } = reloadWindowGateway();
    const { masterEl, detailEl } = await openDocs(gateway);
    setDraft(detailEl, { map: '{"user":"hidden"}', exposure: "hidden" });

    state.deferList = true; // the post-ack inventory reload stays in flight
    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() => expect(state.pending).toHaveLength(1));

    // The ack already advanced the row to the new revision: the pane must not
    // fall back to the effective values while the reload is still pending.
    expect(detailEl.querySelector("textarea").value).toBe('{"user":"hidden"}');
    expect(detailEl.querySelector("select").value).toBe("hidden");
    // The switch shows the state the host acknowledged.
    expect(detailEl.querySelector('[role="switch"]').getAttribute("aria-checked")).toBe("true");

    // Input typed inside the window belongs to the acknowledged revision.
    setDraft(detailEl, { map: '{"after":"codemode"}', exposure: "deferred" });

    state.pending[0](listAt());
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".mcp-detail-status").textContent).toContain(
        "Saved. Reload the session to apply.",
      ),
    );
    expect(detailEl.querySelector("textarea").value).toBe('{"after":"codemode"}');
    expect(detailEl.querySelector("select").value).toBe("deferred");
    expect(masterEl.querySelectorAll(".pkg-manager-sidebar-row").length).toBeGreaterThan(0);
  });

  it("keeps window input across a failed reload and shows it again after a retry", async () => {
    const { gateway, state } = reloadWindowGateway();
    const { page, masterEl, detailEl, tabs } = await openDocs(gateway);
    setDraft(detailEl, { map: '{"user":"hidden"}' });

    state.deferList = true;
    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() => expect(state.pending).toHaveLength(1));
    setDraft(detailEl, { map: '{"window":"hidden"}', exposure: "deferred" });
    state.pending[0]({ ok: false, error: "reload failed" });

    // A failed reload clears the inventory (existing contract) ...
    await vi.waitFor(() =>
      expect(masterEl.querySelectorAll(".pkg-manager-sidebar-row")).toHaveLength(0),
    );

    // ... and the retry repaints the row at the acknowledged revision with the
    // text typed inside the window, not the effective values.
    state.deferList = false;
    await page.activate();
    clickTab(tabs, "project");
    clickRow(masterEl, "docs");
    await vi.waitFor(() =>
      expect(detailEl.querySelector("textarea").value).toBe('{"window":"hidden"}'),
    );
    expect(detailEl.querySelector("select").value).toBe("deferred");
  });
});

describe("mcp-page full-definition native diagnostic", () => {
  it("shows why a definition is not a native candidate without disabling its editor", async () => {
    const diagnostic = "command must be a string or an array of strings";
    const list = {
      ok: true,
      data: {
        ...LIST.data,
        groups: {
          ...LIST.data.groups,
          piGlobal: [{ ...LIST.data.groups.piGlobal[0], validationError: diagnostic }],
        },
      },
    };
    const { page, detailEl } = mount(makeGateway(list));
    await page.activate();

    const node = detailEl.querySelector('[data-diagnostic="native-ineligible"]');
    expect(node).not.toBeNull();
    expect(node.textContent).toBe(diagnostic);
    // The legacy editor keeps its contract: the form is still editable.
    expect(detailEl.querySelector(".mcp-form")).not.toBeNull();
  });
});

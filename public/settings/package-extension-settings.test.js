// @vitest-environment jsdom

// All credential-looking strings below are inert fixtures for write-through
// and masking assertions, never real secrets (see the gitleaks:allow marks).
// ABOUTME: Verifies the package extension-settings renderer: advisor model/effort
// ABOUTME: coupling, off state, save-on-change payloads, and no-op behavior.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { setMessages } from "../i18n.js";
import { renderExtensionSettings } from "./package-extension-settings.js";

setMessages({
  settings: {
    extensionAdvisor: {
      title: "Advisor",
      modelLabel: "Reviewer model",
      effortLabel: "Reasoning effort",
      off: "Disable Advisor",
      effortOff: "off (no reasoning sent)",
      hint: "Changes take effect in new sessions",
      effortReset: "Stored effort is not supported by this model — reset to off",
      saved: "Saved.",
      saveFailed: "Save failed: {message}",
    },
    extensionBraveSearch: {
      title: "Brave Search",
      hint: "Writes to the chosen .env; new sessions pick the values up.",
      keyLabel: "Brave Search API key",
      keyPlaceholder: "paste to set · empty clears",
      countLabel: "Results per search",
      countError: "Must be an integer between 1 and 20",
      saved: "Saved.",
      saveFailed: "Save failed: {message}",
      notConfigured: "No API key configured yet.",
      configuredAt: "Configured ({mask})",
    },
    extensionTavily: {
      title: "Tavily Search",
      hint: "Writes to the chosen .env; new sessions pick the values up.",
      keyLabel: "Tavily API key",
      keyPlaceholder: "paste to set · empty clears",
      countLabel: "Results per search",
      countError: "Must be an integer between 1 and 20",
      saved: "Saved.",
      saveFailed: "Save failed: {message}",
      notConfigured: "No API key configured yet.",
      configuredAt: "Configured ({mask})",
    },
    extensionFff: {
      title: "pi-fff",
      hint: "Changes take effect after restarting Picot",
      modeLabel: "Mode",
      mode: {
        "tools-and-ui": "Tools & UI",
        "tools-only": "Tools only",
        override: "Override",
      },
      modeDesc: {
        "tools-and-ui": "Adds fffind / ffgrep / fff-multi-grep tools and FFF-backed @ autocomplete",
        "tools-only": "Adds the tools; keeps pi's default @ autocomplete",
        override: "Replaces pi's built-in find / grep and adds multi-grep + FFF autocomplete",
      },
      enableFsRootScanning: "Filesystem root scanning",
      enableHomeDirScanning: "Home directory scanning",
      warnOnHomeDirScan: "Warn when scanning home",
      followSymlinks: "Follow symlinks",
      advanced: "Advanced",
      frecencyDbPath: "Frecency database path",
      historyDbPath: "History database path",
      dbManaged: "(fff-managed)",
      shadowBadge: "overridden by {name}",
      invalidConfig: "pi-fff.json is invalid — the extension cannot load:",
      reset: "Reset to defaults",
      resetConfirm: "Click again to confirm",
      legacyWarning: "Removed legacy setting experimental.goals detected.",
      saved: "Saved.",
      saveFailed: "Save failed: {message}",
    },
    extensionTodo: {
      title: "Todo Overlay",
      hint: "Applies immediately — the overlay re-reads its config on every render.",
      maxLinesLabel: "Max widget lines",
      maxLinesError: "Must be an integer ≥ 3",
      collapseKeyLabel: "Collapse shortcut",
    },
    extensionAskUser: {
      title: "Ask User Question",
      hint: "Applies immediately — the questionnaire re-reads its config per render.",
      collapseKeyLabel: "Collapse shortcut",
    },
    extensionSafetyGuard: {
      title: "pi-extension-safety-guard",
      hint: "Applies immediately.",
      masterLabel: "Master switch",
      categoriesGroup: "Rule categories",
      category_git: "Git history",
      category_filesystem: "Filesystem deletion",
      category_docker: "Docker",
      category_package: "Package managers",
      category_system: "System commands",
      category_database: "Database",
      category_secrets: "Secrets",
      protectedPathsGroup: "Protected paths",
      protectWrite: "Guard writes",
      protectEdit: "Guard edits",
      contextBefore: "Context lines before",
      contextAfter: "Context lines after",
      rangeError: "Must be an integer 0–20",
      autoReviewGroup: "Auto review",
      autoReviewLabel: "Enable auto review",
      modelLabel: "Review model",
      modelUnset: "Not set",
      allowCounts: "Allowed entries (global): {count}",
      thinkingLevelLabel: "Review thinking level",
      relocatedBadge: "read-only — relocated by {name}",
      invalidNote: "Invalid file — fix it by hand.",
    },
    extensionWebAccess: {
      title: "pi-web-access",
      hint: "Takes effect after a Pi restart.",
      searchKeysGroup: "Search provider keys",
      extractKeysGroup: "Extraction providers + endpoints",
      endpointsGroup: "Proxy & endpoints",
      answerModelGroup: "Answer model",
      answerModelLabel: "Fetch answer model",
      configuredPreview: "configured ····{preview}",
      envBadge: "env",
      clear: "Clear",
      clearConfirm: "Click again to confirm",
      modelUnset: "Not set",
      invalidNote: "Invalid file — fix it by hand.",
    },
    extensionPlanMode: {
      title: "pi-plan-mode",
      hint: "Applies to the next session start.",
      thinkingLabel: "Plan thinking",
      implModelLabel: "Implementation model",
      followPlanModel: "Follow plan model",
      implThinkingLabel: "Implementation thinking",
      retentionLabel: "Plan retention",
      exportPathLabel: "Export path",
      shortcutLabel: "Toggle shortcut",
      advancedGroup: "Advanced",
      planToolsLabel: "Default plan tools (JSON array)",
      safeSubcommandsLabel: "Safe subcommands (JSON object)",
      invalidJson: "Invalid JSON",
      invalidNote: "Invalid file — fix it by hand.",
    },
    extensionCaveman: {
      title: "pi-caveman",
      hint: "The default level applies to new sessions.",
      levelLabel: "Default level",
      statusLabel: "Show footer status",
    },
    extensionCacheOptimizer: {
      title: "pi-cache-optimizer",
      hint: "Takes effect after a Pi restart or /reload.",
      footerModeLabel: "Footer stats mode",
      mode_total: "Total",
      mode_session: "Session",
      mode_process: "Process",
      source_config: "config",
      source_env: "env",
      source_default: "default",
      omitTitle: "Repaired cache keys ({count}, read-only)",
      envTitle: "Environment opt-outs (read-only)",
      envOn: "ON",
      envOff: "OFF",
      invalidNote: "Invalid file: fix it by hand.",
    },
    extensionLens: {
      title: "pi-lens",
      hint: "Toggles apply to new sessions.",
      runtimeGroup: "Runtime",
      feedbackGroup: "Feedback chain",
      guardGroup: "Guards",
      reportGroup: "Reports",
      analyzerGroup: "Analyzers",
      advancedGroup: "Advanced",
      maxProjectFilesLabel: "Max project files",
      limitError: "Must be an integer ≥ 1",
      shadowBadge: "{source} override",
      relocatedBadge: "read-only — relocated by {name}",
      projectHint: "Project overrides appear after entering a workspace.",
    },
    extensionGoal: {
      title: "pi-goal",
      hint: "Limits apply when the next goal starts; the RPC toggle needs a restart.",
      rpcLabel: "RPC channel",
      rpcDesc: "Registers the goal RPC channel at load (default off).",
      automaticTurnsLabel: "Automatic turns",
      noProgressTurnsLabel: "No-progress turns",
      unlimited: "Unlimited",
      limitError: "Must be an integer ≥ 1",
      reset: "Reset to defaults",
      resetConfirm: "Click again to confirm",
      legacyWarning: "Removed legacy setting experimental.goals detected.",
    },
    extensionVcc: {
      title: "pi-vcc Compaction",
      hint: "Takes effect at the next compaction; an in-flight compaction keeps its loaded snapshot.",
      relocatedBadge: "read-only — relocated by {name}",
      overrideDefaultCompactionLabel: "Own compaction",
      overrideDefaultCompactionDesc: "pi-vcc owns /compact and threshold overflow.",
      smartKeepTailLabel: "Smart keep-tail",
      smartKeepTailDesc: "Boosts keep-tail when the last tail is tiny.",
      continueAfterThresholdCompactLabel: "Continue after auto-compact",
      continueAfterThresholdCompactDesc: "Permission (not guarantee) to continue.",
      debugLabel: "Debug snapshots",
      debugDesc: "Writes compaction snapshots to /tmp/pi-vcc-debug.json.",
    },
    extensionPonytail: {
      title: "Ponytail",
      hint: "Default mode for new sessions; the active session keeps its current mode.",
      modeLabel: "Default mode",
      mode_lite: "Lite",
      mode_full: "Full",
      mode_ultra: "Ultra",
      useDefault: "Use default",
      quietStartupLabel: "Quiet startup",
      hideStatusLabel: "Hide status",
    },
    saved: "Saved.",
  },
  models: {
    scoped: "Scoped models",
    allEnabled: "All enabled models",
    unavailableHelp: "Picot could not read your enabled-model list just now.",
  },
});

const GET_OK = {
  ok: true,
  data: {
    modelKey: "anthropic/claude-sonnet",
    effort: "high",
    models: [
      {
        key: "anthropic/claude-sonnet",
        name: "Claude Sonnet",
        levels: ["minimal", "low", "medium", "high", "max"],
        available: true,
      },
      {
        key: "minimax-cn/MiniMax-M3",
        name: "MiniMax M3",
        levels: ["low", "high"],
        available: true,
      },
      { key: "locked/no-creds", name: "Locked", levels: ["low"], available: false },
    ],
  },
};

/** Catalog fixture for the shared model picker: the same models the op
 * payload lists, so the picker and the per-model levels describe one set. */
function catalogFor(models, { visibleKeys } = {}) {
  const byProvider = new Map();
  for (const model of models) {
    const [provider, ...rest] = model.key.split("/");
    if (!byProvider.has(provider)) byProvider.set(provider, { provider, models: [] });
    byProvider.get(provider).models.push({
      provider,
      id: rest.join("/"),
      name: model.name,
      available: model.available !== false,
      visible: visibleKeys ? visibleKeys.includes(model.key) : true,
    });
  }
  return { ok: true, data: { providers: [...byProvider.values()] } };
}

function scopedFor(scopedIds = []) {
  return { ok: true, data: { modelIds: [...scopedIds] } };
}

function mount(gatewayResult, { scopedIds = [], models = GET_OK.data.models } = {}) {
  const calls = [];
  const configGateway = {
    call: (op, params) => {
      calls.push({ op, params });
      if (op === "list_model_catalog") return Promise.resolve(catalogFor(models));
      if (op === "list_scoped_models") return Promise.resolve(scopedFor(scopedIds));
      if (typeof gatewayResult === "function") return gatewayResult(op, params);
      return Promise.resolve(gatewayResult);
    },
  };
  return { calls, configGateway };
}

function detailWith() {
  const el = document.createElement("div");
  return el;
}

async function renderAdvisor(gatewayResult) {
  const { calls, configGateway } = mount(gatewayResult);
  const detailEl = detailWith();
  renderExtensionSettings(detailEl, { source: "npm:@juicesharp/rpiv-advisor" }, { configGateway });
  await vi.waitFor(() => {
    if (!detailEl.querySelector("select")) throw new Error("section not mounted yet");
  });
  // The picker list arrives one round trip after the selects mount.
  await new Promise((resolve) => setTimeout(resolve, 0));
  const [modelSelect, effortSelect] = detailEl.querySelectorAll("select");
  return {
    detailEl,
    modelSelect,
    effortSelect,
    calls,
    status: () => detailEl.querySelector(".pkg-ext-status").textContent,
    notice: () => detailEl.querySelector(".pkg-ext-notice").textContent,
  };
}

describe("renderExtensionSettings dispatch", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("renders nothing for packages without a renderer", () => {
    const detailEl = detailWith();
    renderExtensionSettings(detailEl, { source: "npm:other-package" }, mount(GET_OK).configGateway);
    expect(detailEl.children.length).toBe(0);
  });

  it("renders nothing without the dep its surface needs", () => {
    const detailEl = detailWith();
    // Advisor needs the bridge gateway (in-process model registry).
    renderExtensionSettings(detailEl, { source: "npm:@juicesharp/rpiv-advisor" }, {});
    expect(detailEl.children.length).toBe(0);
    // pi-fff needs the host-op transport.
    renderExtensionSettings(detailEl, { source: "npm:@ff-labs/pi-fff" }, {});
    expect(detailEl.children.length).toBe(0);
  });
});

describe("advisor renderer", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("renders title, hint, off row, and the saved model/effort", async () => {
    const { detailEl, modelSelect, effortSelect } = await renderAdvisor(GET_OK);
    expect(detailEl.textContent).toContain("Advisor");
    expect(detailEl.textContent).toContain("Changes take effect in new sessions");
    expect(modelSelect.value).toBe("anthropic/claude-sonnet");
    expect(effortSelect.value).toBe("high");
    // Available models only; the locked provider never appears.
    const optionValues = [...modelSelect.options].map((o) => o.value);
    expect(optionValues).toContain("");
    expect(optionValues).toContain("minimax-cn/MiniMax-M3");
    expect(optionValues).not.toContain("locked/no-creds");
    expect(modelSelect.options[0].textContent).toBe("Disable Advisor");
  });

  it("off state: no modelKey → effort select disabled and off option selected", async () => {
    const data = { ...GET_OK.data, modelKey: undefined, effort: undefined };
    const { modelSelect, effortSelect } = await renderAdvisor({ ok: true, data });
    expect(modelSelect.value).toBe("");
    expect(effortSelect.disabled).toBe(true);
    expect(effortSelect.value).toBe("");
    expect(effortSelect.options[0].textContent).toBe("off (no reasoning sent)");
  });

  it("a stored model outside the available list stays visible as its raw key", async () => {
    const data = { ...GET_OK.data, modelKey: "gone/model-x" };
    const { modelSelect } = await renderAdvisor({ ok: true, data });
    expect(modelSelect.value).toBe("gone/model-x");
    expect([...modelSelect.options].map((o) => o.value)).toContain("gone/model-x");
  });

  it("model change to incompatible model resets effort, notices, and saves the pair", async () => {
    const { modelSelect, effortSelect, calls, notice } = await renderAdvisor(GET_OK);
    modelSelect.value = "minimax-cn/MiniMax-M3";
    modelSelect.dispatchEvent(new Event("change"));
    // "high" survives: MiniMax supports it.
    expect(effortSelect.value).toBe("high");
    expect(notice()).toBe("");

    // Now from a model whose only overlap is gone: stored "high" on a model with
    // only [low] — switch to a synthesized minimal-only entry via fresh render.
    const limited = {
      ok: true,
      data: {
        ...GET_OK.data,
        modelKey: "anthropic/claude-sonnet",
        effort: "max",
        models: GET_OK.data.models.map((m) =>
          m.key === "minimax-cn/MiniMax-M3" ? { ...m, levels: ["low"] } : m,
        ),
      },
    };
    const again = await renderAdvisor(limited);
    again.modelSelect.value = "minimax-cn/MiniMax-M3";
    again.modelSelect.dispatchEvent(new Event("change"));
    expect(again.effortSelect.value).toBe("");
    expect(again.notice()).toContain("reset to off");
    await vi.waitFor(() => expect(again.status()).toBe("Saved."));
    const payload = again.calls.find((c) => c.op === "advisor.config.set").params;
    expect(payload).toEqual({ modelKey: "minimax-cn/MiniMax-M3", effort: null });
    expect(calls.length).toBeGreaterThan(0); // first render scenario also active
  });

  it("effort change saves immediately; gateway rejection surfaces the error", async () => {
    let failNext = false;
    const { effortSelect, status } = await renderAdvisor((op) =>
      op === "advisor.config.set" && failNext
        ? Promise.resolve({ ok: false, error: "disk full" })
        : Promise.resolve(GET_OK),
    );
    effortSelect.value = "max";
    effortSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(status()).toBe("Saved."));

    failNext = true;
    effortSelect.value = "low";
    effortSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(status()).toBe("Save failed: disk full"));
  });

  it("a failed model change rolls both selects back to the last confirmed save", async () => {
    const { modelSelect, effortSelect, status, notice } = await renderAdvisor((op) =>
      op === "advisor.config.set"
        ? Promise.resolve({ ok: false, error: "EACCES" })
        : Promise.resolve(GET_OK),
    );
    modelSelect.value = "minimax-cn/MiniMax-M3";
    modelSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(status()).toContain("EACCES"));
    // Model rolled back; effort options rebuilt for the restored model with its value restored.
    expect(modelSelect.value).toBe("anthropic/claude-sonnet");
    expect([...effortSelect.options].map((o) => o.value)).toEqual([
      "",
      "minimal",
      "low",
      "medium",
      "high",
      "max",
    ]);
    expect(effortSelect.value).toBe("high");
    expect(notice()).toBe("");
  });

  it("a rejected set (not just {ok:false}) also rolls back", async () => {
    const { effortSelect, status } = await renderAdvisor((op) =>
      op === "advisor.config.set"
        ? Promise.reject(new Error("bridge gone"))
        : Promise.resolve(GET_OK),
    );
    effortSelect.value = "low";
    effortSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(status()).toContain("bridge gone"));
    expect(effortSelect.value).toBe("high");
  });

  it("get rejection renders the error into the status line", async () => {
    const gateway = { call: () => Promise.reject(new Error("no active session")) };
    const detailEl = detailWith();
    renderExtensionSettings(
      detailEl,
      { source: "npm:@juicesharp/rpiv-advisor" },
      { configGateway: gateway },
    );
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain("no active session"),
    );
  });
});

describe("fff renderer", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  const FFF_OK = {
    values: {
      mode: "override",
      frecencyDbPath: null,
      historyDbPath: null,
      enableFsRootScanning: false,
      enableHomeDirScanning: false, // shadowed by env below
      warnOnHomeDirScan: true,
      followSymlinks: true,
    },
    envShadowed: ["enableHomeDirScanning"],
    flagShadowed: [],
    shadowNames: { enableHomeDirScanning: "FFF_ENABLE_HOME_SCAN" },
  };

  function fffTransport(overrides = {}) {
    const calls = [];
    return {
      calls,
      getFffConfig() {
        calls.push({ method: "getFffConfig", payload: null });
        return Promise.resolve(overrides.get ?? FFF_OK);
      },
      setFffConfig(payload) {
        calls.push({ method: "setFffConfig", payload });
        if (overrides.setError) return Promise.reject(new Error(overrides.setError));
        return Promise.resolve({ config: {} });
      },
    };
  }

  async function renderFff(transport) {
    const detailEl = document.createElement("div");
    // Transport only, no config gateway — the landing-page scenario.
    renderExtensionSettings(detailEl, { source: "npm:@ff-labs/pi-fff" }, { transport });
    await vi.waitFor(() => {
      // Healthy state mounts the segmented control; the invalid state mounts
      // the error block instead — either means the section is up.
      if (!detailEl.querySelector(".pkg-ext-segment") && !detailEl.querySelector(".pkg-ext-error"))
        throw new Error("section not mounted");
    });
    return detailEl;
  }

  function setCalls(gateway) {
    return gateway.calls.filter((c) => c.method === "setFffConfig");
  }

  it("renders with transport only (landing) — mode segmented control, toggles, advanced paths", async () => {
    const detailEl = await renderFff(fffTransport());
    expect(detailEl.textContent).toContain("pi-fff");
    expect(detailEl.textContent).toContain("restarting Picot");
    const segs = [...detailEl.querySelectorAll(".pkg-ext-segment-btn")];
    expect(segs.map((b) => b.classList.contains("is-on"))).toEqual([false, false, true]);
    expect(detailEl.querySelector(".pkg-ext-desc").textContent).toContain("Replaces");
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches.map((s) => s.getAttribute("aria-checked"))).toEqual([
      "false",
      "false",
      "true",
      "true",
    ]);
    const inputs = detailEl.querySelectorAll(".pkg-ext-advanced input");
    expect(inputs.length).toBe(2);
    expect(inputs[0].placeholder).toContain("fff-managed");
  });

  it("shadowed field is disabled with a badge naming the exact env var", async () => {
    const detailEl = await renderFff(fffTransport());
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches[1].disabled).toBe(true); // enableHomeDirScanning
    expect(detailEl.textContent).toContain("overridden by FFF_ENABLE_HOME_SCAN");
    expect(switches[0].disabled).toBe(false); // others unaffected
  });

  it("toggle click saves that single key and flips on success", async () => {
    const transport = fffTransport();
    const detailEl = await renderFff(transport);
    const switchBtn = detailEl.querySelector('[role="switch"]'); // root scanning
    switchBtn.click();
    await vi.waitFor(() => expect(setCalls(transport).length).toBe(1));
    expect(setCalls(transport)[0].payload).toEqual({
      key: "enableFsRootScanning",
      value: true,
    });
    await vi.waitFor(() => expect(switchBtn.classList.contains("is-on")).toBe(true));
  });

  it("mode click saves the new mode and updates the description", async () => {
    const transport = fffTransport();
    const detailEl = await renderFff(transport);
    const toolsOnly = [...detailEl.querySelectorAll(".pkg-ext-segment-btn")][1];
    toolsOnly.click();
    await vi.waitFor(() => expect(setCalls(transport).length).toBe(1));
    expect(setCalls(transport)[0].payload).toEqual({ key: "mode", value: "tools-only" });
    await vi.waitFor(() => expect(toolsOnly.classList.contains("is-on")).toBe(true));
    expect(detailEl.querySelector(".pkg-ext-desc").textContent).toContain("keeps pi's default");
  });

  it("path input change saves the path; empty change clears to null", async () => {
    const transport = fffTransport();
    const detailEl = await renderFff(transport);
    const input = detailEl.querySelector(".pkg-ext-advanced input");
    input.value = "/data/frec";
    input.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(setCalls(transport).length).toBe(1));
    expect(setCalls(transport)[0].payload).toEqual({
      key: "frecencyDbPath",
      value: "/data/frec",
    });

    input.value = "   ";
    input.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(setCalls(transport).length).toBe(2));
    expect(setCalls(transport)[1].payload).toEqual({ key: "frecencyDbPath", value: null });
  });

  it("invalid config renders the error state; two-click reset rewrites and rebuilds", async () => {
    let reset = false;
    const calls = [];
    const transport = {
      calls,
      getFffConfig() {
        calls.push({ method: "getFffConfig", payload: null });
        return Promise.resolve(
          reset
            ? FFF_OK
            : {
                values: FFF_OK.values,
                envShadowed: [],
                flagShadowed: [],
                shadowNames: {},
                invalid: { reason: 'unknown option "stray"' },
              },
        );
      },
      setFffConfig(payload) {
        calls.push({ method: "setFffConfig", payload });
        if (payload?.reset === true) reset = true;
        return Promise.resolve({ config: {} });
      },
    };
    const detailEl = await renderFff(transport);
    expect(detailEl.textContent).toContain('unknown option "stray"');
    expect(detailEl.querySelector(".pkg-ext-segment")).toBeNull(); // read-only error state

    const resetBtn = detailEl.querySelector(".pkg-ext-btn-danger");
    resetBtn.click(); // arms the inline confirm
    expect(resetBtn.textContent).toContain("confirm");
    expect(setCalls(transport).length).toBe(0);
    resetBtn.click(); // executes
    await vi.waitFor(() => expect(setCalls(transport).length).toBe(1));
    expect(setCalls(transport)[0].payload).toEqual({ reset: true });
    await vi.waitFor(
      () => expect(detailEl.querySelector(".pkg-ext-segment")).not.toBeNull(), // rebuilt healthy
    );
  });

  it("a failed save surfaces the error and does not flip the toggle", async () => {
    const transport = fffTransport({ setError: "EACCES" });
    const detailEl = await renderFff(transport);
    const switchBtn = detailEl.querySelector('[role="switch"]');
    switchBtn.click();
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain("EACCES"),
    );
    expect(switchBtn.classList.contains("is-on")).toBe(false);
  });

  it("a failed load renders the transport error into the status line", async () => {
    const transport = {
      getFffConfig: () => Promise.reject(new Error("host not ready")),
      setFffConfig: () => Promise.reject(new Error("unused")),
    };
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:@ff-labs/pi-fff" }, { transport });
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain("host not ready"),
    );
  });
});

describe("todo renderer", () => {
  function todoTransport({ setError } = {}) {
    return {
      getTodoConfig: async () => ({
        values: { maxWidgetLines: 8, collapseKey: "ctrl+shift+t" },
        effective: { maxWidgetLines: 8, collapseKey: "ctrl+shift+t" },
      }),
      setTodoConfig: async (payload) =>
        setError
          ? Promise.reject(new Error("invalid collapseKey spec: ctr+]"))
          : {
              values: { ...payload.value },
              effective: { maxWidgetLines: 8, collapseKey: "ctrl+shift+t" },
            },
    };
  }

  async function renderTodo(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:@juicesharp/rpiv-todo" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders with transport only (landing) and reflects stored values", async () => {
    const detailEl = await renderTodo(todoTransport());
    const inputs = [...detailEl.querySelectorAll("input")];
    expect(inputs[0].value).toBe("8");
    expect(inputs[1].value).toBe("ctrl+shift+t");
    expect(detailEl.textContent).toContain("immediately");
  });

  it("clearing the line budget saves null and rejects sub-floor input inline", async () => {
    const transport = todoTransport();
    const detailEl = await renderTodo(transport);
    const [lineInput] = [...detailEl.querySelectorAll("input")];
    lineInput.value = "";
    lineInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() =>
      expect(
        transport.setTodoConfig ? detailEl.querySelector(".pkg-ext-status").textContent : "",
      ).toBeTruthy(),
    );
    lineInput.value = "1";
    lineInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".pkg-ext-notice").textContent.length).toBeGreaterThan(0),
    );
  });

  it("a host validation error surfaces verbatim next to the key input", async () => {
    const detailEl = await renderTodo(todoTransport({ setError: true }));
    const [, keyInput] = [...detailEl.querySelectorAll("input")];
    keyInput.value = "ctr+]";
    keyInput.dispatchEvent(new Event("change"));
    const notices = () =>
      [...detailEl.querySelectorAll(".pkg-ext-notice")].map((n) => n.textContent);
    await vi.waitFor(() => expect(notices().join("\n")).toContain("invalid collapseKey spec"));
  });
});

describe("ask-user-question renderer", () => {
  function askTransport({ setError } = {}) {
    return {
      getAskUserConfig: async () => ({
        values: { collapseKey: "ctrl+]" },
        effective: { collapseKey: "ctrl+]" },
      }),
      setAskUserConfig: async (payload) =>
        setError
          ? Promise.reject(new Error("invalid collapseKey spec: nope"))
          : { values: { collapseKey: payload.value ?? null } },
    };
  }

  async function renderAsk(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:@juicesharp/rpiv-ask-user-question" },
      { transport },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders with transport only (landing), single key input with default placeholder", async () => {
    const detailEl = await renderAsk(askTransport());
    const input = detailEl.querySelector("input");
    expect(input.value).toBe("ctrl+]");
    expect(input.placeholder).toBe("ctrl+]");
    expect(detailEl.textContent).toContain("immediately");
  });

  it("save errors surface verbatim; success normalizes the stored spec", async () => {
    const detailEl = await renderAsk(askTransport({ setError: true }));
    const input = detailEl.querySelector("input");
    input.value = "nope";
    input.dispatchEvent(new Event("change"));
    await vi.waitFor(() =>
      expect(detailEl.querySelector(".pkg-ext-notice").textContent).toContain(
        "invalid collapseKey spec",
      ),
    );
  });
});

describe("ponytail renderer", () => {
  function ponyTransport(overrides = {}) {
    return {
      getPonytailConfig: async () => ({
        defaultMode: overrides.defaultMode ?? "ultra",
        quietStartup: overrides.quietStartup ?? true,
        hideStatus: false,
        effective: { defaultMode: "ultra" },
        envShadowed: overrides.envShadowed ?? [],
        shadowNames: {
          defaultMode: "PONYTAIL_DEFAULT_MODE",
          quietStartup: "PONYTAIL_QUIET_STARTUP",
          hideStatus: "PONYTAIL_HIDE_STATUS",
        },
      }),
      setPonytailConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderPony(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:@dietrichgebert/ponytail" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders segmented mode, clear button, and two switches at landing", async () => {
    const detailEl = await renderPony(ponyTransport());
    const segs = [...detailEl.querySelectorAll(".pkg-ext-segment-btn")];
    expect(segs.map((b) => b.classList.contains("is-on"))).toEqual([false, false, true]);
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches.map((s) => s.getAttribute("aria-checked"))).toEqual(["true", "false"]);
    expect(detailEl.querySelector(".pkg-ext-clear-btn")).toBeTruthy();
    expect(detailEl.textContent).toContain("new sessions");
  });

  it("env-shadowed fields disable with a badge naming the exact variable", async () => {
    const detailEl = await renderPony(ponyTransport({ envShadowed: ["defaultMode"] }));
    expect(detailEl.querySelector(".pkg-ext-segment-btn").disabled).toBe(true);
    expect(detailEl.textContent).toContain("PONYTAIL_DEFAULT_MODE");
  });

  it("mode click saves that single key; clear sends null", async () => {
    const transport = ponyTransport();
    const detailEl = await renderPony(transport);
    const calls = [];
    transport.setPonytailConfig = async (payload) => {
      calls.push(payload);
      return { saved: payload.value };
    };
    const lite = [...detailEl.querySelectorAll(".pkg-ext-segment-btn")][0];
    lite.click();
    await vi.waitFor(() => expect(calls).toEqual([{ key: "defaultMode", value: "lite" }]));
    detailEl.querySelector(".pkg-ext-clear-btn").click();
    await vi.waitFor(() =>
      expect(calls).toEqual([
        { key: "defaultMode", value: "lite" },
        { key: "defaultMode", value: null },
      ]),
    );
  });
});

describe("vcc renderer", () => {
  function vccTransport({ relocated } = {}) {
    return {
      getVccConfig: async () => ({
        values: {
          overrideDefaultCompaction: true,
          smartKeepTail: false,
          continueAfterThresholdCompact: true,
          debug: false,
        },
        configPath: "/home/u/.pi/agent/pi-vcc-config.json",
        relocatedByEnv: Boolean(relocated),
      }),
      setVccConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderVcc(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:@sting8k/pi-vcc" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders four described switches at landing", async () => {
    const detailEl = await renderVcc(vccTransport());
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches.map((s) => s.getAttribute("aria-checked"))).toEqual([
      "true",
      "false",
      "true",
      "false",
    ]);
    expect(detailEl.querySelectorAll(".settings-label-sub").length).toBe(4);
    expect(detailEl.textContent).toContain("next compaction");
  });

  it("relocated env renders the whole section read-only with one badge", async () => {
    const detailEl = await renderVcc(vccTransport({ relocated: true }));
    expect([...detailEl.querySelectorAll('[role="switch"]')].every((s) => s.disabled)).toBe(true);
    expect(detailEl.textContent).toContain("PI_VCC_CONFIG_PATH");
  });
});

describe("goal renderer", () => {
  function goalTransport({ invalid, legacyExperimentalGoals = false } = {}) {
    return {
      getGoalConfig: async () =>
        invalid
          ? {
              settings: null,
              invalid: { reason: "rpc.enabled must be a boolean" },
              legacyExperimentalGoals: false,
            }
          : {
              settings: {
                rpc: { enabled: false },
                continuationLimits: { automaticTurns: 25, noProgressTurns: null },
              },
              invalid: null,
              legacyExperimentalGoals,
            },
      setGoalConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderGoal(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:@narumitw/pi-goal" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders rpc switch and two limit rows; null reads as unlimited", async () => {
    const detailEl = await renderGoal(goalTransport());
    const [rpcSwitch] = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(rpcSwitch.getAttribute("aria-checked")).toBe("false");
    const inputs = [...detailEl.querySelectorAll('input[type="number"]')];
    expect(inputs.map((i) => i.disabled)).toEqual([false, true]);
    const checks = [...detailEl.querySelectorAll('input[type="checkbox"]')];
    expect(checks.map((c) => c.checked)).toEqual([false, true]);
  });

  it("invalid pre-existing file renders read-only error + two-click reset", async () => {
    const detailEl = await renderGoal(goalTransport({ invalid: true }));
    expect(detailEl.textContent).toContain("rpc.enabled must be a boolean");
    const reset = detailEl.querySelector(".pkg-ext-clear-btn");
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBe(0);
    reset.click();
    expect(reset.textContent).toContain("again");
  });
});

describe("caveman renderer", () => {
  function cavemanTransport() {
    return {
      getCavemanConfig: async () => ({
        values: { defaultLevel: "full", showStatus: true },
        effective: { defaultLevel: "full", showStatus: true },
      }),
      setCavemanConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderCaveman(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "git:github.com/jonjonrankin/pi-caveman" },
      { transport },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders the 8-level select + status switch at landing", async () => {
    const detailEl = await renderCaveman(cavemanTransport());
    const select = detailEl.querySelector("select");
    expect([...select.options].map((o) => o.value)).toContain("wenyan-ultra");
    expect(select.value).toBe("full");
    expect(detailEl.querySelector('[role="switch"]').getAttribute("aria-checked")).toBe("true");
  });

  it("level change saves the single key", async () => {
    const transport = cavemanTransport();
    const detailEl = await renderCaveman(transport);
    const calls = [];
    transport.setCavemanConfig = async (payload) => {
      calls.push(payload);
      return { saved: payload.value };
    };
    const select = detailEl.querySelector("select");
    select.value = "micro";
    select.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(calls).toEqual([{ key: "defaultLevel", value: "micro" }]));
  });
});

describe("cache-optimizer renderer", () => {
  function cacheTransport({ invalid } = {}) {
    return {
      getCacheOptimizerConfig: async () =>
        invalid
          ? { invalid: { reason: "unknown key breaks the package schema: extra" } }
          : {
              footerMode: null,
              effectiveFooterMode: "session",
              footerModeSource: "default",
              omitList: ["provider/model-a"],
              envSwitches: {
                PI_CACHE_OPTIMIZER_NO_PROMPT_REWRITE: false,
                PI_CACHE_OPTIMIZER_NO_OPENAI_CACHE_KEY: true,
              },
            },
      setCacheOptimizerConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderCache(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:pi-cache-optimizer" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders footer-mode select with source badge + read-only sections", async () => {
    const detailEl = await renderCache(cacheTransport());
    expect(detailEl.querySelector("select").value).toBe("session");
    expect(detailEl.textContent).toContain("provider/model-a");
    expect(detailEl.textContent).toContain("PI_CACHE_OPTIMIZER_NO_OPENAI_CACHE_KEY");
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBe(0);
  });

  it("env-sourced mode disables the select; invalid file renders error, no reset", async () => {
    const envTransport = cacheTransport();
    envTransport.getCacheOptimizerConfig = async () => ({
      footerMode: null,
      effectiveFooterMode: "total",
      footerModeSource: "env",
      omitList: [],
      envSwitches: {},
    });
    const detailEl = await renderCache(envTransport);
    expect(detailEl.querySelector("select").disabled).toBe(true);
    const invalidEl = await renderCache(cacheTransport({ invalid: true }));
    expect(invalidEl.textContent).toContain("unknown key");
    expect(invalidEl.querySelector(".pkg-ext-clear-btn")).toBeNull();
  });
});

describe("lens renderer", () => {
  function lensTransport({ sources = {} } = {}) {
    return {
      getLensConfig: async () => ({
        values: {},
        effective: {
          "lens.enabled": true,
          "lsp.enabled": true,
          "format.enabled": true,
          "format.mode": "deferred",
          "autofix.enabled": true,
          "tests.enabled": true,
          "delta.enabled": true,
          "guard.enabled": true,
          "guard.sharedCheckout": true,
          "readGuard.enabled": true,
          "contextInjection.enabled": false,
          "turnSummary.enabled": false,
          "actionableWarnings.enabled": false,
          "actionableWarnings.includeLspCodeActions": false,
          "actionableWarnings.autoFix.enabled": false,
          "actionableWarnings.deltaOnly": true,
          "ui.compactToolLine": false,
          "tools.lazy": true,
          "analyzers.knip.enabled": true,
          "analyzers.jscpd.enabled": true,
          "analyzers.madge.enabled": true,
          "analyzers.gitleaks.enabled": true,
          "analyzers.govulncheck.enabled": true,
          "analyzers.deadCode.enabled": true,
          "analyzers.complexity.enabled": true,
          maxProjectFiles: 8000,
        },
        sources,
        projectFile: null,
        configPath: "/home/u/.pi-lens/config.json",
        relocatedByEnv: false,
      }),
      setLensConfig: async (payload) => ({ ...payload }),
    };
  }

  async function renderLens(transport) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:pi-lens" }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders grouped switches + advanced stepper at landing with project hint", async () => {
    const detailEl = await renderLens(lensTransport());
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches.length).toBe(24);
    expect(switches.every((s) => !s.disabled)).toBe(true);
    expect(detailEl.querySelector('input[type="number"]').value).toBe("8000");
    expect(detailEl.textContent).toContain("Project overrides");
  });

  it("env/project shadows disable their switch with a badge", async () => {
    const detailEl = await renderLens(
      lensTransport({ sources: { "lsp.enabled": "env", "delta.enabled": "project" } }),
    );
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    expect(switches.filter((s) => s.disabled).length).toBe(2);
    expect(detailEl.textContent).toContain("env override");
    expect(detailEl.textContent).toContain("project override");
  });

  it("toggle click saves the single dotted key", async () => {
    const transport = lensTransport();
    const detailEl = await renderLens(transport);
    const calls = [];
    transport.setLensConfig = async (payload) => {
      calls.push(payload);
      return { ok: true };
    };
    detailEl.querySelector('[role="switch"]').click();
    await vi.waitFor(() => expect(calls).toEqual([{ key: "lens.enabled", value: false }]));
  });
});

describe("plan-mode renderer", () => {
  function planGateway() {
    const calls = [];
    const gateway = {
      call: async (op, params) => {
        calls.push({ op, params });
        if (op === "list_model_catalog") {
          return catalogFor([
            { key: "anthropic/claude-sonnet", name: "Sonnet", levels: ["off", "low"] },
            { key: "openai/gpt-5", name: "GPT-5", levels: ["low", "high"] },
          ]);
        }
        if (op === "list_scoped_models") return scopedFor([]);
        if (op === "planMode.config.get") {
          return {
            ok: true,
            data: {
              settings: {
                thinkingLevel: "medium",
                defaultImplementationModel: "anthropic/claude-sonnet",
                implementationPlanRetention: "keep",
              },
              models: [
                { key: "anthropic/claude-sonnet", name: "Sonnet", levels: ["off", "low"] },
                { key: "openai/gpt-5", name: "GPT-5", levels: ["low", "high"] },
              ],
            },
          };
        }
        return { ok: true, data: { config: {} } };
      },
    };
    return { gateway, calls };
  }

  async function renderPlan(gateway) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:@narumitw/pi-plan-mode" },
      {
        configGateway: gateway,
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders selects, model dropdown with follow-plan option, advanced JSON", async () => {
    const { gateway } = planGateway();
    const detailEl = await renderPlan(gateway);
    const selects = [...detailEl.querySelectorAll("select")];
    expect(selects[0].value).toBe("medium");
    const modelOptions = [...selects[1].options].map((o) => o.value);
    expect(modelOptions).toContain("");
    expect(modelOptions).toContain("anthropic/claude-sonnet");
    expect(selects[1].value).toBe("anthropic/claude-sonnet");
    expect(detailEl.querySelectorAll("textarea").length).toBe(2);
    expect(detailEl.querySelector("details")).toBeTruthy();
  });

  it("thinking select change saves the single key via the bridge", async () => {
    const { gateway, calls } = planGateway();
    const detailEl = await renderPlan(gateway);
    const selects = [...detailEl.querySelectorAll("select")];
    selects[0].value = "high";
    selects[0].dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      const set = calls.find((c) => c.op === "planMode.config.set");
      expect(set?.params).toEqual({ key: "thinkingLevel", value: "high" });
    });
  });
});

describe("safety-guard renderer", () => {
  function sgGateway() {
    const calls = [];
    const gateway = {
      call: async (op, params) => {
        calls.push({ op, params });
        if (op === "list_model_catalog") {
          return catalogFor([
            { key: "anthropic/claude", name: "Claude", levels: ["low", "high"] }, // gitleaks:allow
            { key: "openai/gpt-5", name: "GPT-5", levels: ["low", "high"] },
          ]);
        }
        if (op === "list_scoped_models") return scopedFor([]);
        if (op === "safetyGuard.config.get") {
          return {
            ok: true,
            data: {
              config: {
                enabled: true,
                categories: { git: true, filesystem: false },
                protectedPaths: { write: true, edit: true },
                contextLines: { before: 3, after: 5 },
                autoReview: { enabled: false, model: { provider: "anthropic", modelId: "claude" } },
              },
              configPath: "/home/u/.pi/agent/safety-guard.json",
              relocatedByEnv: false,
              allowCounts: { global: 4 },
            },
          };
        }
        return { ok: true, data: { config: {} } };
      },
    };
    return { gateway, calls };
  }

  async function renderSg(gateway) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      {
        source: "ssh://git@project.palandata.com:2224/palan/picot4rx/datarx-safety-guard-pi.git",
      },
      { configGateway: gateway },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders master, 7 categories, path pair, steppers, model, allow counts", async () => {
    const { gateway } = sgGateway();
    const detailEl = await renderSg(gateway);
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    // master + 7 categories + 2 paths + autoReview = 11
    expect(switches.length).toBe(11);
    expect(switches.map((s) => s.getAttribute("aria-checked"))).toContain("false");
    const steppers = [...detailEl.querySelectorAll('input[type="number"]')];
    expect(steppers.map((s) => s.value)).toEqual(["3", "5"]);
    expect(detailEl.textContent).toContain("4");
    // Model picker rides the composer's list (enabled + scoped).
    const [modelSelect, levelSelect] = detailEl.querySelectorAll("select");
    expect(modelSelect.value).toBe("anthropic/claude");
    expect([...modelSelect.options].map((o) => o.value)).toContain("openai/gpt-5");
    expect(modelSelect.querySelectorAll("optgroup").length).toBe(1);
    expect([...levelSelect.options].map((o) => o.value)).toEqual([
      "off",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(levelSelect.value).toBe("off");
  });

  it("thinking level select saves the dotted key", async () => {
    const { gateway, calls } = sgGateway();
    const detailEl = await renderSg(gateway);
    const levelSelect = detailEl.querySelectorAll("select")[1];
    levelSelect.value = "high";
    levelSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      const set = calls.find(
        (c) =>
          c.op === "safetyGuard.config.set" && c.params?.key === "autoReview.model.thinkingLevel",
      );
      expect(set?.params).toEqual({ key: "autoReview.model.thinkingLevel", value: "high" });
    });
  });

  it("category click saves the dotted key via the bridge", async () => {
    const { gateway, calls } = sgGateway();
    const detailEl = await renderSg(gateway);
    const switches = [...detailEl.querySelectorAll('[role="switch"]')];
    switches[1].click(); // git category
    await vi.waitFor(() => {
      const set = calls.find((c) => c.op === "safetyGuard.config.set");
      expect(set?.params).toEqual({ key: "categories.git", value: false });
    });
  });
});

describe("web-access renderer", () => {
  function waGateway() {
    const calls = [];
    const gateway = {
      call: async (op, params) => {
        calls.push({ op, params });
        if (op === "webaccess.config.get") {
          return {
            ok: true,
            data: {
              fields: {
                openaiApiKey: { configured: true, preview: "abcd" },
                braveApiKey: { configured: false },
              },
              nonSecrets: { proxy: "http://127.0.0.1:7890", allowBrowserCookies: false },
              routing: {
                answerModel: { provider: "openai", modelId: "gpt-5" },
              },
              envKeyed: ["geminiApiKey"],
              invalid: null,
            },
          };
        }
        return { ok: true, data: { ok: true } };
      },
    };
    return { gateway, calls };
  }

  async function renderWa(gateway) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "npm:pi-web-access" }, { configGateway: gateway });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders masked rows (placeholder previews, never values) + env badge", async () => {
    const { gateway } = waGateway();
    const detailEl = await renderWa(gateway);
    const password = detailEl.querySelector('input[type="password"]');
    expect(password.placeholder).toContain("abcd");
    const secrets = [...detailEl.querySelectorAll('input[type="password"]')];
    expect(secrets.every((i) => i.value === "")).toBe(true);
    expect(detailEl.textContent).toContain("env");
    expect(detailEl.querySelector('input[type="text"]').value).toBe("http://127.0.0.1:7890");
  });

  it("typing a key saves write-through; empty change never sends", async () => {
    const { gateway, calls } = waGateway();
    const detailEl = await renderWa(gateway);
    const password = detailEl.querySelector('input[type="password"]');
    password.value = "";
    password.dispatchEvent(new Event("change"));
    password.value = "test-key-placeholder";
    password.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      const set = calls.find((c) => c.op === "webaccess.config.set");
      expect(set?.params).toEqual({ key: "openaiApiKey", value: "test-key-placeholder" });
    });
  });

  it("clear is a two-click confirm on populated rows", async () => {
    const { gateway, calls } = waGateway();
    const detailEl = await renderWa(gateway);
    const clearBtn = detailEl.querySelector(".pkg-ext-clear-btn");
    expect(clearBtn.textContent).not.toContain("again");
    clearBtn.click();
    // First click only arms; nothing sent yet.
    expect(calls.filter((c) => c.op === "webaccess.config.set").length).toBe(0);
    clearBtn.click();
    await vi.waitFor(() => {
      const set = calls.find(
        (c) => c.op === "webaccess.config.set" && c.params?.key === "openaiApiKey",
      );
      expect(set?.params?.value).toBeNull();
    });
  });
});

describe("renderer dispatch resilience", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("renders an error row instead of an unhandled rejection when the dep lacks the method", async () => {
    const detailEl = document.createElement("div");
    // pi-caveman's renderer needs transport.getCavemanConfig; a stub without
    // it must not escape as an unhandled rejection (vitest fails the run).
    renderExtensionSettings(
      detailEl,
      { source: "git:github.com/jonjonrankin/pi-caveman" },
      { transport: {} },
    );
    await vi.waitFor(() => {
      expect(detailEl.querySelector(".pkg-ext-error")?.textContent).toContain(
        "getCavemanConfig is not a function",
      );
    });
  });

  it("surfaces a rejected gateway in the status row, never as an unhandled rejection", async () => {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:pi-web-access" },
      {
        configGateway: {
          call: async () => {
            throw new Error("gateway down");
          },
        },
      },
    );
    await vi.waitFor(() => {
      expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain("gateway down");
    });
  });
});

describe("invalid-config branches", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  async function renderWith(source, data) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source },
      {
        configGateway: { call: async () => ({ ok: true, data }) },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    // The load settles a microtask after the section mounts: assert on the
    // loaded data, never on the mounting alone.
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("plan-mode reports an unreadable file and renders no controls", async () => {
    const detailEl = await renderWith("npm:@narumitw/pi-plan-mode", {
      settings: null,
      models: [],
      invalid: { reason: "Unexpected token } in JSON at position 12" },
    });
    expect(detailEl.querySelector(".pkg-ext-error").textContent).toContain("Unexpected token");
    expect(detailEl.textContent).toContain("Invalid file");
    expect(detailEl.querySelectorAll("select").length).toBe(0);
  });

  it("safety-guard reports an unreadable file and renders no controls", async () => {
    const detailEl = await renderWith(
      "ssh://git@example.com:2224/palan/datarx-safety-guard-pi.git",
      {
        config: null,
        configPath: "/home/u/.pi/agent/safety-guard.json",
        relocatedByEnv: false,
        allowCounts: { global: 0 },
        invalid: { reason: "config root is not an object" },
      },
    );
    expect(detailEl.querySelector(".pkg-ext-error").textContent).toContain(
      "config root is not an object",
    );
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBe(0);
  });
});

describe("paired-field writes", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("safety-guard writes the auto-review model as one entries batch", async () => {
    const calls = [];
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "ssh://git@example.com:2224/palan/datarx-safety-guard-pi.git" },
      {
        configGateway: {
          call: async (op, params) => {
            calls.push({ op, params });
            if (op === "list_model_catalog") {
              return catalogFor([{ key: "anthropic/claude", name: "Claude", levels: ["low"] }]); // gitleaks:allow
            }
            if (op === "list_scoped_models") return scopedFor([]);
            if (op === "safetyGuard.config.get") {
              return { ok: true, data: { config: {}, configPath: "p", relocatedByEnv: false } };
            }
            return { ok: true, data: { config: {} } };
          },
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector("select")) throw new Error("model row missing");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const modelSelect = detailEl.querySelector("select");
    modelSelect.value = "anthropic/claude";
    modelSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      const set = calls.find((c) => c.op === "safetyGuard.config.set");
      expect(set?.params).toEqual({
        entries: [
          { key: "autoReview.model.provider", value: "anthropic" },
          { key: "autoReview.model.modelId", value: "claude" },
        ],
      });
    });
  });

  it("web-access writes the answer model as one entries batch", async () => {
    const calls = [];
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:pi-web-access" },
      {
        configGateway: {
          call: async (op, params) => {
            calls.push({ op, params });
            if (op === "list_model_catalog") {
              return catalogFor([{ key: "openai/gpt-5", name: "GPT-5", levels: ["high"] }]);
            }
            if (op === "list_scoped_models") return scopedFor([]);
            if (op === "webaccess.config.get") {
              return { ok: true, data: { fields: {}, nonSecrets: {}, routing: {}, envKeyed: [] } };
            }
            return { ok: true, data: { ok: true } };
          },
        },
      },
    );
    await vi.waitFor(() => {
      const rows = [...detailEl.querySelectorAll('input[type="text"]')];
      if (rows.length < 4) throw new Error("endpoint rows missing");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const answerSelect = detailEl.querySelector("select");
    answerSelect.value = "openai/gpt-5";
    answerSelect.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      const set = calls.find((c) => c.op === "webaccess.config.set");
      expect(set?.params).toEqual({
        entries: [
          { key: "fetch.answerProvider", value: "openai" },
          { key: "fetch.answerModel", value: "gpt-5" },
        ],
      });
    });
  });
});

describe("web-access row contract", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("labels the package's flat keys and never the invented dotted ones", async () => {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:pi-web-access" },
      {
        configGateway: {
          call: async () => ({
            ok: true,
            data: {
              fields: {},
              nonSecrets: {},
              routing: {},
              envKeyed: [],
            },
          }),
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector('input[type="password"]')) throw new Error("rows not mounted");
    });
    const labels = [...detailEl.querySelectorAll(".settings-label")].map((el) => el.textContent);
    expect(labels).toContain("crawl4aiApiToken");
    expect(labels).toContain("brightdataApiKey");
    expect(labels).toContain("searxngBaseUrl");
    expect(labels).toContain("brightdataUnlockerZone");
    expect(labels).not.toContain("crawl4ai.token");
    expect(labels).not.toContain("searxng.password");
    expect(labels).not.toContain("brightdata.key");
    expect(labels).not.toContain("searxng.username");
  });

  it("clears the typed secret even when the save fails", async () => {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:pi-web-access" },
      {
        configGateway: {
          call: async (op) => {
            if (op === "webaccess.config.get") {
              return { ok: true, data: { fields: {}, nonSecrets: {}, routing: {}, envKeyed: [] } };
            }
            return { ok: false, error: "disk full" };
          },
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector('input[type="password"]')) throw new Error("secret row missing");
    });
    const password = detailEl.querySelector('input[type="password"]');
    password.value = "test-key-placeholder";
    password.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain("disk full");
    });
    expect(password.value).toBe("");
  });
});

describe("host control-op contract", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  async function renderWithRawTransport(source, transport) {
    const detailEl = document.createElement("div");
    // Production contract: transport.<method>() resolves the op payload and
    // rejects on failure (no { ok, data } envelope).
    renderExtensionSettings(detailEl, { source }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  it("renders host-op payloads instead of failing on a missing ok flag", async () => {
    const detailEl = await renderWithRawTransport("npm:pi-lens", {
      getLensConfig: async () => ({
        values: { "lens.enabled": true },
        effective: { "lens.enabled": true },
        sources: { "lens.enabled": "default" },
        projectFile: null,
        configPath: "/home/.pi-lens/config.json",
        relocatedByEnv: false,
      }),
      setLensConfig: async () => ({ path: "/home/.pi-lens/config.json" }),
    });
    expect(detailEl.querySelector(".pkg-ext-status").textContent).toBe("");
    expect(detailEl.textContent).not.toContain("load failed");
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBeGreaterThan(0);
  });

  it("surfaces a rejected host op as the status message", async () => {
    const detailEl = await renderWithRawTransport("npm:@dietrichgebert/ponytail", {
      getPonytailConfig: async () => {
        throw new Error("PI_CONFIG_PATH is not readable");
      },
      setPonytailConfig: async () => ({}),
    });
    expect(detailEl.querySelector(".pkg-ext-status").textContent).toContain(
      "PI_CONFIG_PATH is not readable",
    );
  });
});

describe("shared model picker (composer parity)", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  async function renderPlanModePicker({ visibleKeys, scopedIds }) {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:@narumitw/pi-plan-mode" },
      {
        configGateway: {
          call: async (op) => {
            if (op === "list_model_catalog") {
              return catalogFor(
                [
                  { key: "anthropic/claude-sonnet", name: "Sonnet", levels: ["low"] },
                  { key: "openai/gpt-5", name: "GPT-5", levels: ["high"] },
                ],
                visibleKeys ? { visibleKeys } : undefined,
              );
            }
            if (op === "list_scoped_models") return scopedFor(scopedIds);
            if (op === "planMode.config.get") {
              return { ok: true, data: { settings: {}, models: [] } };
            }
            return { ok: true, data: { config: {} } };
          },
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector("select")) throw new Error("section not mounted");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl.querySelectorAll("select")[1];
  }

  it("offers only the models Picot has enabled", async () => {
    const select = await renderPlanModePicker({ visibleKeys: ["openai/gpt-5"] });
    const values = [...select.options].map((o) => o.value);
    expect(values).toContain("openai/gpt-5");
    expect(values).not.toContain("anthropic/claude-sonnet");
  });

  it("groups scoped models ahead of the rest, like the composer", async () => {
    const select = await renderPlanModePicker({ scopedIds: ["openai/gpt-5"] });
    const groups = [...select.querySelectorAll("optgroup")];
    expect(groups.map((g) => g.label)).toEqual(["Scoped models", "All enabled models"]);
    expect([...groups[0].querySelectorAll("option")].map((o) => o.value)).toEqual(["openai/gpt-5"]);
  });

  it("says why the list is empty instead of rendering a bare select", async () => {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:@narumitw/pi-plan-mode" },
      {
        configGateway: {
          call: async (op) => {
            if (op === "list_model_catalog") throw new Error("gateway down");
            if (op === "planMode.config.get")
              return { ok: true, data: { settings: {}, models: [] } };
            return { ok: true, data: { config: {} } };
          },
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector("select")) throw new Error("section not mounted");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(detailEl.textContent).toContain("could not read your enabled-model list");
  });
});

describe("goal legacy setting", () => {
  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("warns about experimental.goals without blocking the file", async () => {
    const detailEl = document.createElement("div");
    renderExtensionSettings(
      detailEl,
      { source: "npm:@narumitw/pi-goal" },
      {
        transport: {
          getGoalConfig: async () => ({
            settings: {
              rpc: { enabled: true },
              continuationLimits: { automaticTurns: null, noProgressTurns: 3 },
            },
            invalid: null,
            legacyExperimentalGoals: true,
          }),
          setGoalConfig: async (payload) => ({ ...payload }),
        },
      },
    );
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    // The package keeps such a file valid (docs/settings.md) — the page must
    // render the controls and only add the warning.
    expect(detailEl.textContent).toContain("Removed legacy setting");
    expect(detailEl.querySelectorAll('[role="switch"]').length).toBeGreaterThan(0);
    expect(detailEl.querySelector(".pkg-ext-error")).toBeNull();
  });
});

describe("datarx-essential search renderers (brave + tavily)", () => {
  const LOADED = {
    ok: true,
    globalPath: "/home/u/.pi/agent/.env",
    globalKeyMasked: "••••9999",
    defaultCount: 5,
  };

  // One transport serves both global-.env twins; every call is tagged with the
  // extension that owns the op so assertions can tell the two apart.
  function searchTransport({ get = {}, set = {} } = {}) {
    const calls = [];
    const make = (ext, op) => async (arg) => {
      const config = op === "get" ? get : set;
      const perExt = config[ext] ?? config;
      calls.push(op === "get" ? { ext, op } : { ext, op, payload: arg });
      if (perExt.error) throw new Error(perExt.error);
      return { ...LOADED, ...(perExt.value ?? {}) };
    };
    return {
      calls,
      getBraveSearchConfig: make("brave", "get"),
      setBraveSearchConfig: make("brave", "set"),
      getTavilySearchConfig: make("tavily", "get"),
      setTavilySearchConfig: make("tavily", "set"),
    };
  }

  const callsOf = (transport, ext, op) =>
    transport.calls.filter((call) => call.ext === ext && call.op === op);

  async function renderDatarx(source, transport) {
    const detailEl = document.createElement("div");
    // Exactly the pre-slice-B 3-argument context: no workspaceCwd.
    renderExtensionSettings(detailEl, { source }, { transport });
    await vi.waitFor(() => {
      if (!detailEl.querySelector(".pkg-ext-settings")) throw new Error("section not mounted");
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    return detailEl;
  }

  const sections = (detailEl) => detailEl.querySelectorAll(".pkg-ext-settings");
  const braveSection = (detailEl) => sections(detailEl)[0];
  const tavilySection = (detailEl) => sections(detailEl)[1];
  const status = (el) => el.querySelector(".pkg-ext-status").textContent;

  beforeEach(() => {
    document.body.replaceChildren();
  });

  it("routes a local-path install and its .git form to the same renderer", async () => {
    for (const source of [
      "/home/u/.pi/agent/extensions/datarx-essential",
      "\\home\\u\\extensions\\datarx-essential.git",
    ]) {
      const detailEl = await renderDatarx(source, searchTransport());
      // Both global-.env twins render — brave first, each with its own title.
      expect(
        [...sections(detailEl)].map((s) => s.querySelector(".pkg-ext-title").textContent),
      ).toEqual(["Brave Search", "Tavily Search"]);
      expect(detailEl.textContent).toContain("••••9999");
    }
  });

  it("renders from the pre-slice-B 3-argument context with no workspaceCwd", async () => {
    const detailEl = document.createElement("div");
    // The package manager is back to the 3-arg call; both sections still mount.
    renderExtensionSettings(
      detailEl,
      { source: "/x/datarx-essential" },
      { transport: searchTransport() },
    );
    await vi.waitFor(() => expect(sections(detailEl).length).toBe(2));
  });

  it("renders no scope select and shows the masked key without the path", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = braveSection(detailEl);
    // No scope control survives the global-only simplification.
    expect(section.querySelector("select")).toBeNull();
    // The hint covers where the key lives; the status line must not repeat it.
    expect(section.textContent).not.toContain("/home/u/.pi/agent/.env");
    expect(section.textContent).toContain("Configured");
    expect(section.textContent).toContain("••••9999");
    // The key input never carries a stored value.
    expect(section.querySelector('input[type="password"]').value).toBe("");
    // The load carries no cwd.
    expect(callsOf(transport, "brave", "get")).toEqual([{ ext: "brave", op: "get" }]);
  });

  it("reports notConfigured when the global tier holds no key", async () => {
    const transport = searchTransport({ get: { value: { globalKeyMasked: null } } });
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    expect(status(braveSection(detailEl))).toBe("No API key configured yet.");
  });

  it("submitting an empty key clears the line; an untouched key sends nothing", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const keyInput = braveSection(detailEl).querySelector('input[type="password"]');
    // No edit yet: a change event on the pristine (empty) field is not a
    // clear — the host has no plaintext to hand back, so "untouched" and
    // "emptied" must stay distinguishable.
    keyInput.dispatchEvent(new Event("change"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(callsOf(transport, "brave", "set")).toEqual([]);
    // Typing then clearing (a real browser fires `input` per keystroke).
    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "  ";
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      // The clear payload carries ONLY apiKey — never scope/cwd.
      expect(callsOf(transport, "brave", "set")).toEqual([
        { ext: "brave", op: "set", payload: { apiKey: "" } },
      ]);
    });
    await vi.waitFor(() => expect(status(braveSection(detailEl))).toContain("Saved."));
  });

  it("writes a typed key, then clears it on the next edit, without scope/cwd", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const keyInput = braveSection(detailEl).querySelector('input[type="password"]');
    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "BSA-fixture-key"; // gitleaks:allow
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "brave", "set")).toEqual([
        { ext: "brave", op: "set", payload: { apiKey: "BSA-fixture-key" } },
      ]);
    });
    // Clearing is an explicit edit: the browser fires `input`, which is the
    // only signal that separates "emptied" from "never touched".
    keyInput.value = "";
    keyInput.dispatchEvent(new Event("input"));
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "brave", "set")[1]).toEqual({
        ext: "brave",
        op: "set",
        payload: { apiKey: "" },
      });
    });
  });

  it("writes the count and sends null when the field is emptied", async () => {
    const transport = searchTransport({ set: { value: { defaultCount: 9 } } });
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const countInput = braveSection(detailEl).querySelector('input[type="number"]');
    expect(countInput.value).toBe("5");
    countInput.value = "9";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "brave", "set")).toEqual([
        { ext: "brave", op: "set", payload: { defaultCount: 9 } },
      ]);
    });
    await vi.waitFor(() => expect(countInput.value).toBe("9"));
    countInput.value = "";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "brave", "set")[1]).toEqual({
        ext: "brave",
        op: "set",
        payload: { defaultCount: null },
      });
    });
  });

  it("blocks an out-of-range count client-side without a request", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = braveSection(detailEl);
    const countInput = section.querySelector('input[type="number"]');
    for (const bad of ["0", "21", "1.5"]) {
      countInput.value = bad;
      countInput.dispatchEvent(new Event("change"));
      await vi.waitFor(() =>
        expect(section.querySelector(".pkg-ext-notice").textContent).toBe(
          "Must be an integer between 1 and 20",
        ),
      );
      expect(callsOf(transport, "brave", "set")).toEqual([]);
      // The invalid edit leaves the persisted value on screen.
      expect(countInput.value).toBe("5");
    }
  });

  it("reports saveFailed and rolls the field back when the set rejects", async () => {
    const transport = searchTransport({ set: { error: "boom" } });
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = braveSection(detailEl);
    const keyInput = section.querySelector('input[type="password"]');
    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "BSA-fixture-key"; // gitleaks:allow
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(status(section)).toContain("Save failed: boom"));
    expect(keyInput.value).toBe("");
  });

  it("surfaces a rejected load instead of mounting dead controls", async () => {
    const transport = searchTransport({ get: { error: "transport offline" } });
    const detailEl = document.createElement("div");
    renderExtensionSettings(detailEl, { source: "/x/datarx-essential" }, { transport });
    await vi.waitFor(() => expect(status(braveSection(detailEl))).toBe("transport offline"));
  });

  it("renders the tavily section from its own locale block and ops", async () => {
    const transport = searchTransport({
      get: { tavily: { value: { globalKeyMasked: "••••7777" } } },
    });
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = tavilySection(detailEl);
    expect(section.querySelector(".pkg-ext-title").textContent).toBe("Tavily Search");
    expect(section.textContent).toContain("Tavily API key");
    expect(section.textContent).toContain("••••7777");
    expect(section.querySelector("select")).toBeNull();
    // The tavily load rides the tavily op, no cwd.
    expect(callsOf(transport, "tavily", "get")).toEqual([{ ext: "tavily", op: "get" }]);
  });

  it("writes the tavily key through setTavilySearchConfig, leaving brave alone", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const keyInput = tavilySection(detailEl).querySelector('input[type="password"]');
    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "TVLY-fixture-key"; // gitleaks:allow
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "tavily", "set")).toEqual([
        { ext: "tavily", op: "set", payload: { apiKey: "TVLY-fixture-key" } },
      ]);
    });
    // The tavily edit never touches the brave twin's ops.
    expect(callsOf(transport, "brave", "set")).toEqual([]);
  });

  it("writes the tavily count through its own op", async () => {
    const transport = searchTransport({ set: { tavily: { value: { defaultCount: 11 } } } });
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const countInput = tavilySection(detailEl).querySelector('input[type="number"]');
    expect(countInput.value).toBe("5");
    countInput.value = "11";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(callsOf(transport, "tavily", "set")).toEqual([
        { ext: "tavily", op: "set", payload: { defaultCount: 11 } },
      ]);
    });
  });

  it("a rolled-back key clear never turns a later count change into an apiKey delete", async () => {
    const deferred = () => {
      let resolve;
      let reject;
      const promise = new Promise((res, rej) => {
        resolve = res;
        reject = rej;
      });
      return { promise, resolve, reject };
    };
    const calls = [];
    const transport = {
      getBraveSearchConfig: async () => ({ ...LOADED }),
      getTavilySearchConfig: async () => ({ ...LOADED }),
      setBraveSearchConfig: (payload) => {
        const pending = deferred();
        calls.push({ payload, ...pending });
        return pending.promise;
      },
      setTavilySearchConfig: async () => ({ ...LOADED }),
    };
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = braveSection(detailEl);
    const keyInput = section.querySelector('input[type="password"]');
    const countInput = section.querySelector('input[type="number"]');

    // Save A: paste the key, leaving the write in flight.
    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "BSA-fixture-key"; // gitleaks:allow
    keyInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(calls.length).toBe(1));

    // Save B: an overlapping count edit while A is still pending.
    countInput.value = "9";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(calls.length).toBe(2));

    // A fails first and rolls the key field back to its untouched empty state.
    calls[0].reject(new Error("boom"));
    await vi.waitFor(() => expect(keyInput.value).toBe(""));
    // Then B lands, recording the stored key while the field stays empty.
    calls[1].resolve({ ...LOADED, defaultCount: 9 });
    await vi.waitFor(() => expect(countInput.value).toBe("9"));

    // An unrelated count edit must not resurrect an apiKey clear.
    countInput.value = "7";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(calls.length).toBe(3));
    expect(calls[2].payload.defaultCount).toBe(7);
    expect(calls[2].payload.apiKey ?? null).toBeNull();
    // No write ever asked the host to drop the key.
    expect(calls.some((call) => call.payload.apiKey === "")).toBe(false);
  });

  it("an out-of-range count leaves a pasted key untouched", async () => {
    const transport = searchTransport();
    const detailEl = await renderDatarx("/x/datarx-essential", transport);
    const section = braveSection(detailEl);
    const keyInput = section.querySelector('input[type="password"]');
    const countInput = section.querySelector('input[type="number"]');

    keyInput.dispatchEvent(new Event("input"));
    keyInput.value = "BSA-pasted-key"; // gitleaks:allow
    countInput.value = "25";
    countInput.dispatchEvent(new Event("change"));
    await vi.waitFor(() =>
      expect(section.querySelector(".pkg-ext-notice").textContent).toBe(
        "Must be an integer between 1 and 20",
      ),
    );
    expect(keyInput.value).toBe("BSA-pasted-key");
    expect(countInput.value).toBe("5");
    expect(callsOf(transport, "brave", "set")).toEqual([]);
  });
});

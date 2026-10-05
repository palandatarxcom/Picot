// @vitest-environment jsdom

// ABOUTME: Locks the Settings > Subagents tab to the host wire contract.
// ABOUTME: Covers scoped master/detail, metadata-only lists, byte-exact raw, state handling, safety.

import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setMessages } from "../i18n.js";

const LOCALES = ["en", "zh", "ja", "es"];
const REQUIRED_COPY = [
  "title",
  "scopes.global",
  "scopes.project",
  "subtabs.definitions",
  "subtabs.packages",
  "state.conflict",
  "state.error",
  "create.disabled",
  "detail.reloadNotice",
  "detail.package",
  "rescan",
  "groups.builtinExtension",
  "diagnostics.outOfScope",
  "detail.writeLayer",
  "detail.writeLayerGlobal",
  "detail.writeLayerProjectUnknown",
  "detail.keepCurrent",
  "detail.noLayerOverride",
  "detail.thinkingFalse",
  "detail.thinkingHint",
  "detail.advertise",
  "detail.enable",
  "detail.inheritDefinition",
  "detail.booleanOn",
  "detail.booleanOff",
  "status.disabledOverride",
];

describe("subagents locales", () => {
  it("provides actionable scope and error copy in all supported languages", () => {
    for (const locale of LOCALES) {
      const messages = JSON.parse(readFileSync(`public/locales/${locale}.json`, "utf8")).settings
        .subagents;
      for (const key of REQUIRED_COPY) {
        const value = key.split(".").reduce((node, part) => node?.[part], messages);
        expect(value, `${locale}: ${key}`).toBeTruthy();
      }
    }
  });
});

import { WsTransport } from "../app/transport.js";
import { setupSubagentsTab } from "./subagents-tab.js";

// The English copy the panel renders is the shipped locale file, not a copy of
// it: a fixture that mirrored the module's own table is what let 80 keys live
// outside the locale files while these tests stayed green.
const EN_MESSAGES = JSON.parse(readFileSync("public/locales/en.json", "utf8"));

setMessages(EN_MESSAGES);

const HOSTILE_RAW =
  '---\nname: evil\ndescription: <img src=x onerror="window.__pwned=1">\n---\n<script>window.__pwned=2</script>\n';

function candidate(overrides = {}) {
  return {
    id: "c-user-1",
    runtimeName: "coder",
    localName: "coder",
    source: "user",
    sourceScope: "global",
    packageIdentity: null,
    filePath: "/home/u/.pi/agent/agents/coder.md",
    parsedFields: { name: "coder", description: "writes code", runner: "native" },
    status: "candidate",
    winnerId: null,
    readOnly: true,
    nativeOverrideSupported: true,
    writeQualified: false,
    writeDiagnostic: { source: "parity", message: "winner or out-of-scope occupancy unverified" },
    savedOverride: { model: null, thinking: null, advertise: null, disabled: null },
    inferredValue: null,
    settingsRevision: "rev-1",
    ...overrides,
  };
}

function inventory(entries, overrides = {}) {
  return {
    agentRoot: "/home/u/.pi/agent",
    workspaceRoot: null,
    projectRoot: null,
    resolutionContext: {
      mode: "disk-candidates-only",
      reason: "live /run winner not observable",
      projectWritesAllowed: false,
    },
    entries,
    diagnostics: [
      {
        source: ".agents/",
        message: "out-of-scope occupancy not verified; no definition body read",
      },
    ],
    inventoryRevision: "inv-1",
    settingsRevisions: { global: "rev-1", project: "project-rev" },
    ...overrides,
  };
}

function hostError(code, message = code) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeTransport() {
  return {
    listSubagents: vi.fn(),
    getSubagentDetail: vi.fn(),
    createSubagent: vi.fn(),
    setSubagentOverride: vi.fn(),
  };
}

let container;

function setup(
  transport,
  {
    identity = { workspaceId: "w1", workspaceGeneration: 3 },
    landingOnly = false,
    confirmDiscard,
    configGateway,
  } = {},
) {
  let current = identity;
  const page = setupSubagentsTab({
    container,
    transport,
    getWorkspaceIdentity: () => current,
    landingOnly,
    confirmDiscard,
    configGateway,
  });
  return { page, setIdentity: (next) => (current = next) };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const tabLabels = () =>
  [...container.querySelectorAll("[data-subagents-scope]")].map((b) => b.dataset.subagentsScope);
const subtabLabels = () =>
  [...container.querySelectorAll("[data-subagents-subtab]")].map((b) => b.dataset.subagentsSubtab);
const selectedSubtab = () =>
  container.querySelector('[data-subagents-subtab][aria-selected="true"]')?.dataset.subagentsSubtab;
const clickSubtab = (name) => container.querySelector(`[data-subagents-subtab="${name}"]`).click();
const rowNames = () =>
  [...container.querySelectorAll(".subagents-row")].map(
    (row) => row.querySelector(".pkg-manager-sidebar-name").textContent,
  );
const rowBadges = () =>
  [...container.querySelectorAll(".subagents-row .subagents-badge")].map((b) => b.textContent);
const groupHeaders = () =>
  [...container.querySelectorAll(".subagents-group-header")].map((h) => h.textContent);

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
});

afterEach(() => {
  container.remove();
});

describe("subagents transport", () => {
  it("sends fixed host operations without workspace identity on global requests", async () => {
    const sendControl = vi.fn().mockResolvedValue({ entries: [] });
    const transport = new WsTransport({ sendControl });
    await transport.listSubagents("global");
    await transport.getSubagentDetail("project", "id-1", {
      workspaceId: "w1",
      workspaceGeneration: 3,
    });
    await transport.createSubagent({ scope: "global", name: "coder" });
    await transport.setSubagentOverride({ scope: "global", candidateId: "id-1" });
    expect(sendControl.mock.calls.map(([op, args]) => [op, args])).toEqual([
      ["subagents_inventory", { scope: "global" }],
      [
        "subagents_get_detail",
        { scope: "project", candidateId: "id-1", workspaceId: "w1", workspaceGeneration: 3 },
      ],
      ["subagents_create", { scope: "global", name: "coder" }],
      ["subagents_set_override", { scope: "global", candidateId: "id-1" }],
    ]);
  });
});

describe("subagents tab", () => {
  it("shows Global first and default-selected; global requests carry no project identity", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    const { page } = setup(transport);
    await page.activate();
    expect(tabLabels()).toEqual(["global", "project"]);
    const globalTab = container.querySelector('[data-subagents-scope="global"]');
    expect(globalTab.getAttribute("aria-selected")).toBe("true");
    expect(transport.listSubagents).toHaveBeenCalledWith("global", null);
  });

  it("keeps Current project selectable for a registered workspace with exact identity", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [candidate({ id: "c-p", runtimeName: "proj", sourceScope: "project", source: "project" })],
        {
          workspaceRoot: "/ws",
          projectRoot: "/ws",
        },
      ),
    );
    const { page } = setup(transport);
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    expect(transport.listSubagents).toHaveBeenLastCalledWith("project", {
      workspaceId: "w1",
      workspaceGeneration: 3,
    });
    expect(container.textContent).toContain("proj");
    expect(container.textContent).not.toContain("writes code");
  });

  it("hides the project tab without a workspace identity and on landingOnly", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([]));
    const noIdentity = setup(transport, { identity: null });
    await noIdentity.page.activate();
    expect(tabLabels()).toEqual(["global"]);

    const landing = setup(transport, { landingOnly: true });
    await landing.page.activate();
    expect(tabLabels()).toEqual(["global"]);
    expect(transport.listSubagents).toHaveBeenCalledWith("global", null);
  });

  it("projects Custom without package rows and Packages grouped by package identity", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate(),
        candidate({
          id: "c-pkg-a",
          runtimeName: "tool.a",
          source: "package",
          packageIdentity: "tool",
          filePath: "/home/u/.pi/agent/npm/node_modules/tool/agents/tool.md",
        }),
        candidate({
          id: "c-pkg-b",
          runtimeName: "tool.b",
          source: "package",
          packageIdentity: "tool",
          filePath: "/home/u/.pi/agent/npm/node_modules/tool/agents/b.md",
        }),
        candidate({
          id: "c-pkg-c",
          runtimeName: "other.c",
          source: "package",
          packageIdentity: "other",
          filePath: "/home/u/.pi/agent/npm/node_modules/other/agents/c.md",
        }),
        candidate({
          id: "c-builtin",
          runtimeName: "builtin-agent",
          source: "builtin",
          filePath: null,
        }),
      ]),
    );
    const { page } = setup(transport);
    await page.activate();
    // Custom lists only the scope's own files: package sources and the
    // read-only built-ins belong to the Packages sub-tab.
    expect(groupHeaders()).toEqual(["Your definitions"]);
    expect(rowNames()).toEqual(["coder"]);
    // Creating writes a definition, so the add affordance is Custom-only.
    expect(container.querySelector(".subagents-new")).not.toBeNull();

    clickSubtab("packages");
    // Grouped by package identity, headed by the identity itself; built-ins are
    // read-only and get their own group instead of a package identity.
    expect(groupHeaders()).toEqual([
      "tool",
      "other",
      "pi-subagents built-in extension (read-only)",
    ]);
    expect(rowNames()).toEqual(["tool.a", "tool.b", "other.c", "builtin-agent"]);
    const first = container.querySelector(".subagents-row");
    expect(first.querySelector(".pkg-manager-sidebar-name").textContent).toBe("tool.a");
    expect(rowBadges()).toEqual(["package", "package", "package"]);
    expect(container.querySelector(".subagents-new")).toBeNull();
  });

  it("reads also-available-in relative to the view and never self-references", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate({
          id: "c-shared",
          runtimeName: "shared.s",
          source: "package",
          sourceScope: "project",
          packageIdentity: "shared",
          additionalScopes: ["global"],
        }),
        // In scope through additionalScopes alone: the badge still must not
        // point back at the view that is already showing it.
        candidate({ id: "c-global", runtimeName: "global.only" }),
      ]),
    );
    const { page } = setup(transport);
    await page.activate();
    expect(rowBadges()).toEqual([]);
    clickSubtab("packages");
    // One row per candidate: a dual-scope package identity is not duplicated.
    expect(rowNames()).toEqual(["shared.s"]);
    expect(rowBadges()).toEqual(["package", "Also available in: project"]);
    expect(container.textContent).not.toContain("Also available in: global");

    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    clickSubtab("packages");
    expect(rowNames()).toEqual(["shared.s"]);
    expect(rowBadges()).toEqual(["package", "Also available in: global"]);
  });

  it("renders loading, empty, generic error with retry, and conflict states", async () => {
    const transport = makeTransport();
    const pending = deferred();
    transport.listSubagents.mockReturnValueOnce(pending.promise);
    const { page } = setup(transport);
    const activating = page.activate();
    expect(container.textContent).toContain("Loading…");
    pending.resolve(inventory([]));
    await activating;
    expect(container.textContent).toContain("No agent definitions in this scope.");

    transport.listSubagents.mockRejectedValueOnce(hostError("config_unavailable", "boom"));
    container.querySelector(".subagents-retry").click();
    await flush();
    expect(container.textContent).toContain(EN_MESSAGES.settings.subagents.state.error);

    transport.listSubagents.mockRejectedValueOnce(hostError("stale_generation"));
    container.querySelector(".subagents-retry").click();
    await flush();
    expect(container.textContent).toContain(EN_MESSAGES.settings.subagents.state.conflict);
    expect(transport.listSubagents).toHaveBeenCalledTimes(3);
  });

  it("lists metadata only and fetches byte-exact raw definition by candidate id", async () => {
    const raw = "---\nname: real-name\ndescription: d\naliases: nick\n---\n\nPROMPT BODY  \n";
    const entry = candidate({
      id: "c-raw",
      runtimeName: "real-name",
      localName: "real-name",
      parsedFields: { name: "real-name", description: "d", runner: "native" },
    });
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([entry]));
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-raw",
      rawDefinition: raw,
      diagnostic: null,
    });
    const { page } = setup(transport);
    await page.activate();
    expect(container.textContent).not.toContain("PROMPT BODY");
    container.querySelector(".subagents-row").click();
    await flush();
    expect(transport.getSubagentDetail).toHaveBeenCalledWith("global", "c-raw", null);
    const pre = container.querySelector(".subagents-raw");
    expect(pre).not.toBeNull();
    // Host bytes exactly, including the trailing whitespace a parsedFields
    // reconstruction would strip.
    expect(pre.textContent).toBe(raw);
  });

  it("never issues a detail request or raw panel for a file-less builtin source", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([candidate({ id: "c-b", runtimeName: "b", source: "builtin", filePath: null })]),
    );
    const { page } = setup(transport);
    await page.activate();
    clickSubtab("packages");
    container.querySelector(".subagents-row").click();
    await flush();
    expect(transport.getSubagentDetail).not.toHaveBeenCalled();
    expect(container.querySelector(".subagents-raw")).toBeNull();
    expect(container.textContent).toContain("No definition file");
  });

  it("disk-candidates-only: no winner/effective label, save stays disabled, reload notice shown", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-user-1",
      rawDefinition: "---\nname: coder\n---\n",
      diagnostic: null,
    });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    const save = container.querySelector(".subagents-save");
    expect(save).not.toBeNull();
    expect(save.disabled).toBe(true);
    // Status copy states the unverified-candidate fact; nothing on the page
    // labels the entry as the effective/winner agent, and the tab membership
    // ("Global") is never claimed as its effective scope.
    const status = container.querySelector(".subagents-status");
    expect(status.textContent).toBe("Disk candidate — runtime winner unverified");
    expect(container.querySelector("[data-winner]")).toBeNull();
    expect(container.querySelector("[data-effective]")).toBeNull();
    // The notice is asserted against the shipped locale copy, not a paraphrase:
    // it is the only place the panel tells the user how a save takes effect.
    expect(container.textContent).toContain(EN_MESSAGES.settings.subagents.detail.reloadNotice);
  });

  it("disables override controls with a diagnostic for external/unknown runners", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate({
          id: "c-ext",
          runtimeName: "ext",
          parsedFields: { name: "ext", description: "mcp backed", runner: "mcp" },
          nativeOverrideSupported: false,
        }),
      ]),
    );
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    for (const input of container.querySelectorAll(".subagents-override-input")) {
      expect(input.disabled).toBe(true);
    }
    expect(container.querySelector(".subagents-save").disabled).toBe(true);
    // The host's parity note only restates the candidate status row, so the
    // detail pane drops it rather than echoing raw English back.
    expect(container.textContent).not.toContain("winner or out-of-scope occupancy unverified");
  });

  it("shadowed-style details expose no jump-to-winner link and no enabled save", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate({ id: "c-a", runtimeName: "twin" }),
        candidate({ id: "c-b", runtimeName: "twin", sourceScope: "project", source: "project" }),
      ]),
    );
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-a",
      rawDefinition: "---\nname: twin\n---\nx",
      diagnostic: null,
    });
    const { page } = setup(transport);
    await page.activate();
    container.querySelectorAll(".subagents-row")[0].click();
    await flush();
    expect(container.querySelector(".subagents-jump")).toBeNull();
    const save = container.querySelector(".subagents-save");
    expect(save === null || save.disabled).toBe(true);
  });

  it("renders hostile YAML as inert text — no script or img executes", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([candidate({ id: "c-x", runtimeName: "evil" })]),
    );
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-x",
      rawDefinition: HOSTILE_RAW,
      diagnostic: null,
    });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(globalThis.window.__pwned).toBeUndefined();
    expect(container.querySelector(".subagents-raw").textContent).toBe(HOSTILE_RAW);
  });

  it("invalidates stale entries on workspace/generation change and ignores late promises", async () => {
    const transport = makeTransport();
    const first = deferred();
    const second = deferred();
    transport.listSubagents.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const { page, setIdentity } = setup(transport);
    const activatingA = page.activate();
    setIdentity({ workspaceId: "w1", workspaceGeneration: 4 });
    const activatingB = page.activate();
    // The old workspace's inventory resolves late: it must never render.
    first.resolve(inventory([candidate({ id: "old", runtimeName: "stale-entry" })]));
    await activatingA;
    expect(container.textContent).not.toContain("stale-entry");
    second.resolve(inventory([candidate({ id: "new", runtimeName: "fresh-entry" })]));
    await activatingB;
    await flush();
    expect(container.textContent).toContain("fresh-entry");
  });

  it("does not retain stale definition text when an inventory refresh keeps the candidate ID", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-user-1",
      rawDefinition: "old prompt",
    });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.querySelector(".subagents-raw").textContent).toBe("old prompt");
    await page.activate();
    expect(container.textContent).not.toContain("old prompt");
  });

  it("resetProject drops the cached project inventory so the next activate refetches", async () => {
    const transport = makeTransport();
    const project = inventory([
      candidate({ id: "c-p2", runtimeName: "p2", sourceScope: "project", source: "project" }),
    ]);
    transport.listSubagents.mockResolvedValue(project);
    const { page } = setup(transport);
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    expect(transport.listSubagents).toHaveBeenCalledTimes(2);
    page.resetProject();
    await page.activate();
    expect(transport.listSubagents).toHaveBeenCalledTimes(3);
    expect(transport.listSubagents).toHaveBeenLastCalledWith("project", {
      workspaceId: "w1",
      workspaceGeneration: 3,
    });
  });

  it("shows saved layer fields and an evidenced inferred value only when present", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [
          candidate({
            id: "c-saved",
            savedOverride: { model: "prov/model", thinking: "high" },
            inferredValue: { model: "prov/model", thinking: "medium", source: "project layer" },
          }),
        ],
        { resolutionContext: { mode: "verified", reason: null, projectWritesAllowed: true } },
      ),
    );
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.textContent).toContain("prov/model");
    expect(container.textContent).toContain("high");
    expect(container.textContent).toContain("project layer");
    // Without evidence the inferred row must not render at all.
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.textContent).not.toContain("project layer");
  });

  it("surfaces out-of-scope sources as diagnostics only, without any detail affordance", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    const { page } = setup(transport);
    await page.activate();
    // Page-level diagnostics are no longer rendered; they carry no user action.
    expect(container.querySelector(".subagents-diagnostics")).toBeNull();
  });
});

describe("subagents draft actions", () => {
  it("keeps only the changed model and preserves saved thinking:false", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [candidate({ writeQualified: true, savedOverride: { model: "a/b", thinking: false } })],
        { resolutionContext: { mode: "verified", reason: null, projectWritesAllowed: true } },
      ),
    );
    transport.getSubagentDetail.mockResolvedValue({ rawDefinition: "---\nname: coder\n---\nbody" });
    transport.setSubagentOverride.mockResolvedValue({ inventory: inventory([]) });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    const model = container.querySelector('[aria-label="Model"]');
    expect(model.disabled).toBe(false);
    model.value = "a/c";
    model.dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({
        scope: "global",
        candidateId: "c-user-1",
        expectedRevision: "rev-1",
        model: { op: "set", value: "a/c" },
        thinking: { op: "keep" },
        advertise: { op: "keep" },
        disabled: { op: "keep" },
      }),
    );
  });

  it("guards project draft loss when leaving after a workspace switch", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([]));
    const confirmDiscard = vi.fn(() => false);
    const { page, setIdentity } = setup(transport, { confirmDiscard });
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    container.querySelector(".subagents-new").click();
    const prompt = container.querySelector('[name="prompt"]');
    prompt.value = "keep me";
    prompt.dispatchEvent(new Event("input", { bubbles: true }));
    setIdentity({ workspaceId: "w2", workspaceGeneration: 4 });
    expect(page.leave()).toBe(false);
    expect(confirmDiscard).toHaveBeenCalled();
  });

  it("retains a new-agent prompt across scope switches and refuses unqualified writes", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-new").click();
    const prompt = container.querySelector('[name="prompt"]');
    prompt.value = "Do this safely";
    prompt.dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    container.querySelector('[data-subagents-scope="global"]').click();
    await flush();
    expect(container.querySelector('[name="prompt"]').value).toBe("Do this safely");
    expect(container.querySelector(".subagents-create").disabled).toBe(true);
    expect(transport.createSubagent).not.toHaveBeenCalled();
  });
});

describe("subagents creation safety", () => {
  it("does not invent a target settings revision from the first candidate", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [candidate({ id: "global", sourceScope: "global", settingsRevision: "global-rev" })],
        { resolutionContext: { mode: "verified", reason: null, projectWritesAllowed: true } },
      ),
    );
    transport.createSubagent.mockResolvedValue({ inventory: inventory([]) });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    container.querySelector(".subagents-new").click();
    for (const [field, value] of Object.entries({
      name: "helper",
      description: "help",
      prompt: "Do work",
    })) {
      const input = container.querySelector(`[name="${field}"]`);
      input.value = value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }
    container.querySelector(".subagents-create").click();
    await flush();
    expect(transport.createSubagent).toHaveBeenCalledWith(
      expect.objectContaining({
        expectedRevision: "project-rev",
        confirmShadowedIds: [],
        expectedInventoryRevision: "inv-1",
        scope: "project",
      }),
    );
  });
});

// Name-level writes are allowed under disk-candidates-only parity: the controls
// gate on the candidate's own snapshot qualification, never on a verified
// runtime winner, and the model control is a picker of real catalog ids.
describe("subagents row disabled switch", () => {
  const qualifiedRow = (entries, overrides = {}) =>
    inventory(
      (Array.isArray(entries) ? entries : [entries]).map((entry) =>
        candidate({
          writeQualified: true,
          writeDiagnostic: null,
          savedOverride: { model: null, thinking: null, advertise: null, disabled: null },
          ...entry,
        }),
      ),
      overrides,
    );
  it("renders the enable switch on the detail name row, defaulting to on", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualifiedRow([
        {
          id: "c-on",
          runtimeName: "on-agent",
          savedOverride: { model: null, thinking: null, advertise: null, disabled: null },
        },
      ]),
    );
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-on",
      rawDefinition: "---\nname: on-agent\n---\nx",
    });
    const { page } = setup(transport);
    await page.activate();
    // Master rows carry no switch; selecting a row reveals it on the name line.
    expect(container.querySelector(".subagents-row .settings-toggle")).toBeNull();
    container.querySelector(".subagents-row").click();
    await flush();
    const nameRow = container.querySelector(".subagents-name-row");
    expect(nameRow).not.toBeNull();
    const toggle = nameRow.querySelector(".subagents-detail-toggle");
    expect(toggle.classList.contains("on")).toBe(true);
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    // Right-aligned: the switch is the last child of the name row.
    expect(nameRow.lastElementChild).toBe(toggle);
  });

  it("toggling writes immediately with keeps and refreshes the saved state", async () => {
    const transport = makeTransport();
    const withDisabled = (value) =>
      qualifiedRow({
        savedOverride: { model: null, thinking: null, advertise: null, disabled: value },
      });
    transport.listSubagents.mockResolvedValueOnce(withDisabled(null));
    transport.setSubagentOverride.mockResolvedValueOnce({ inventory: withDisabled(true) });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    container.querySelector(".subagents-detail-toggle").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({
        scope: "global",
        candidateId: expect.any(String),
        model: { op: "keep" },
        thinking: { op: "keep" },
        advertise: { op: "keep" },
        disabled: { op: "set", value: true },
        expectedRevision: "rev-1",
      }),
    );
    // After disabling, the enable switch reads off.
    expect(container.querySelector(".subagents-detail-toggle").classList.contains("on")).toBe(
      false,
    );
    // Toggling back clears the override instead of writing false.
    transport.setSubagentOverride.mockResolvedValueOnce({ inventory: withDisabled(null) });
    container.querySelector(".subagents-detail-toggle").click();
    await flush();
    expect(transport.setSubagentOverride.mock.calls[1][0].disabled).toEqual({ op: "clear" });
  });

  it("keeps the switch inert for names the host refuses to write", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([candidate({ id: "c-no", runtimeName: "nope", writeQualified: false })]),
    );
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    const toggle = container.querySelector(".subagents-detail-toggle");
    expect(toggle.disabled).toBe(true);
    toggle.click();
    await flush();
    expect(transport.setSubagentOverride).not.toHaveBeenCalled();
  });

  it("keeps the master scroll position across selection renders", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualifiedRow([
        { id: "c-1", runtimeName: "a1" },
        { id: "c-2", runtimeName: "a2" },
      ]),
    );
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-1",
      rawDefinition: "---\nname: a1\n---\nx",
    });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-master").scrollTop = 120;
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.querySelector(".subagents-master").scrollTop).toBe(120);
  });
});

describe("subagents name-level override editing", () => {
  // Composer-native shape: bare ids plus a provider; the page composes
  // `${provider}/${id}` when writing the override.
  const CATALOG = [
    { provider: "openai", id: "gpt-5" },
    { provider: "anthropic", id: "claude-x" },
  ];
  const qualified = (entry = {}, overrides = {}) =>
    inventory(
      [
        candidate({
          writeQualified: true,
          writeDiagnostic: null,
          savedOverride: { model: null, thinking: null, advertise: null, disabled: null },
          ...entry,
        }),
      ],
      overrides,
    );
  const details = (transport) =>
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-user-1",
      rawDefinition: "---\nname: coder\n---\nbody",
      diagnostic: null,
    });
  const openRow = async () => {
    container.querySelector(".subagents-row").click();
    await flush();
  };

  it("renders three-state selectors, saved booleans, and a disabled override badge", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualified({
        savedOverride: { model: null, thinking: null, advertise: false, disabled: true },
      }),
    );
    details(transport);
    const { page } = setup(transport);
    await page.activate();
    expect(rowBadges()).toEqual(["Disabled (override)"]);
    await openRow();
    const saved = container.querySelector(
      ".subagents-detail .subagents-subheading + .pkg-manager-status-grid",
    );
    expect(saved.textContent).toContain("Show in parent agent directory: false");
    expect(saved.querySelectorAll(".pkg-manager-status-row")).toHaveLength(4);
    // The disabled dropdown is gone from the form; only advertise keeps a
    // three-state select. The name-row toggle owns enable/disable.
    expect(container.querySelector('[aria-label="Disable"]')).toBeNull();
    const select = container.querySelector('[aria-label="Show in parent agent directory"]');
    expect(select.tagName).toBe("SELECT");
    expect([...select.options].map((item) => item.value)).toEqual(["", "true", "false"]);
    expect(select.options[0].textContent).toBe("Unset (inherit definition)");
    expect(select.value).toBe("false");
    expect(select.disabled).toBe(false);
    expect(container.querySelector(".subagents-save").disabled).toBe(true);
  });

  it("sends boolean true and false alongside model/thinking in one payload", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    const change = (name, value, event = "change") => {
      const control = container.querySelector(`[aria-label="${name}"]`);
      if (!control) return; // disabled select removed; toggle owns it
      control.value = value;
      control.dispatchEvent(new Event(event, { bubbles: true }));
    };
    change("Model", "openai/gpt-5", "input");
    change("Thinking", "high");
    change("Show in parent agent directory", "true");
    change("Disable", "false");
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledTimes(1);
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({
        model: { op: "set", value: "openai/gpt-5" },
        thinking: { op: "set", value: "high" },
        advertise: { op: "set", value: true },
        disabled: { op: "keep" },
      }),
    );
    const payload = transport.setSubagentOverride.mock.calls[0][0];
    expect(payload.advertise.value).toBe(true);
    expect(payload.disabled).toEqual({ op: "keep" });
  });

  it("clears saved booleans when returned to unset and keeps untouched fields", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualified({
        savedOverride: { model: "a/b", thinking: "high", advertise: true, disabled: false },
      }),
    );
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    for (const name of ["Show in parent agent directory"]) {
      const control = container.querySelector(`[aria-label="${name}"]`);
      control.value = "";
      control.dispatchEvent(new Event("change", { bubbles: true }));
    }
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({
        model: { op: "keep" },
        thinking: { op: "keep" },
        advertise: { op: "clear" },
        disabled: { op: "keep" },
      }),
    );
  });

  it("shows unset booleans without falsely displaying saved override rows", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    const { page } = setup(transport);
    await page.activate();
    expect(rowBadges()).toEqual([]);
    await openRow();
    const saved = container.querySelector(
      ".subagents-detail .subagents-subheading + .pkg-manager-status-grid",
    );
    expect(saved.querySelectorAll(".pkg-manager-status-row")).toHaveLength(2);
    for (const name of ["Show in parent agent directory"])
      expect(container.querySelector(`[aria-label="${name}"]`).value).toBe("");
  });

  it("enables editing and saves when the name is qualified without a verified winner", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport);
    await page.activate();
    // The parity notice stays: no winner is claimed, only a name-level write.
    await openRow();
    const model = container.querySelector('[aria-label="Model"]');
    expect(model.disabled).toBe(false);
    expect(container.querySelector(".subagents-save").disabled).toBe(true);
    model.value = "openai/gpt-5";
    model.dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({
        scope: "global",
        candidateId: "c-user-1",
        runtimeName: "coder",
        expectedRevision: "rev-1",
        model: { op: "set", value: "openai/gpt-5" },
        thinking: { op: "keep" },
      }),
    );
    expect(container.textContent).toContain("Saved to disk");
  });

  it("still refuses external runners and unqualified names with zero host writes", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate({
          id: "c-ext",
          runtimeName: "ext",
          parsedFields: { name: "ext", description: "mcp backed", runner: "mcp" },
          nativeOverrideSupported: false,
        }),
        candidate({ id: "c-blocked", runtimeName: "blocked" }),
      ]),
    );
    details(transport);
    const { page } = setup(transport);
    await page.activate();
    for (const row of container.querySelectorAll(".subagents-row")) {
      row.click();
      await flush();
      for (const control of container.querySelectorAll(".subagents-override-input"))
        expect(control.disabled).toBe(true);
      const save = container.querySelector(".subagents-save");
      expect(save.disabled).toBe(true);
      save.click();
      await flush();
    }
    expect(transport.setSubagentOverride).not.toHaveBeenCalled();
  });

  it("renders the rpiv-advisor select with scoped groups and Not set", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualified({ savedOverride: { model: "inherit", thinking: false } }),
    );
    details(transport);
    const { page } = setup(transport, {
      configGateway: {
        call: async (op) =>
          op === "list_model_catalog"
            ? {
                ok: true,
                data: {
                  providers: CATALOG.map((c) => ({
                    provider: c.provider,
                    models: [{ provider: c.provider, id: c.id, available: true, visible: true }],
                  })),
                },
              }
            : { ok: true, data: { modelIds: [] } },
      },
    });
    await page.activate();
    await flush(); // let refreshCatalog resolve
    await openRow();
    const model = container.querySelector('[aria-label="Model"]');
    expect(model.tagName).toBe("SELECT");
    // "Not set" is the first option; catalog models are present.
    expect(model.options[0].textContent).toBe("Not set (inherit parent)");
    expect(model.textContent).toContain("gpt-5");
    // A saved out-of-catalog "inherit" is preserved as a selectable option.
    expect(model.value).toBe("inherit");
    expect([...model.options].some((o) => o.textContent.includes("Keep current: inherit"))).toBe(
      true,
    );
    // Selecting a catalog model commits the composed provider/id.
    model.value = "openai/gpt-5";
    model.dispatchEvent(new Event("change", { bubbles: true }));
    expect(model.value).toBe("openai/gpt-5");
    const thinking = container.querySelector('[aria-label="Thinking"]');
    expect(thinking.tagName).toBe("SELECT");
    expect(thinking.value).toBe("false");
    expect([...thinking.options].map((option) => option.value)).toEqual([
      "",
      "false",
      "off",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
    expect(container.querySelector(".subagents-thinking-hint").textContent).toContain(
      "/subagents-models",
    );
  });

  it("degrades to a validated text input without a catalog and accepts multi-segment ids", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    const modelInput = () => container.querySelector('[aria-label="Model"]');
    expect(modelInput().tagName).toBe("INPUT");
    modelInput().value = "bogus";
    modelInput().dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Use a provider/model ID.");
    // The writer's own shape: a nonempty provider plus a model part, no
    // whitespace — a multi-segment model id must not be narrower than the host.
    expect(modelInput().value).toBe("bogus");
    modelInput().value = "openrouter/vendor/model-v2";
    modelInput().dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({ model: { op: "set", value: "openrouter/vendor/model-v2" } }),
    );
  });

  it("sends clear via the clear button and a JSON boolean false for the off switch", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualified({ savedOverride: { model: "openai/gpt-5", thinking: "high" } }),
    );
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport, {
      configGateway: {
        call: async (op) =>
          op === "list_model_catalog"
            ? {
                ok: true,
                data: {
                  providers: [
                    {
                      provider: CATALOG[0].provider,
                      models: [
                        {
                          provider: CATALOG[0].provider,
                          id: CATALOG[0].id,
                          available: true,
                          visible: true,
                        },
                      ],
                    },
                  ],
                },
              }
            : { ok: true, data: { modelIds: [] } },
      },
    });
    await page.activate();
    await flush(); // let refreshCatalog resolve
    await openRow();
    const thinking = container.querySelector('[aria-label="Thinking"]');
    expect(container.querySelector('[aria-label="Model"]').value).toBe("openai/gpt-5");
    expect(thinking.value).toBe("high");
    // Clear = select "Not set" (the first option with value "")
    const modelSelect = container.querySelector('[aria-label="Model"]');
    modelSelect.value = "";
    modelSelect.dispatchEvent(new Event("change", { bubbles: true }));
    thinking.value = "false";
    thinking.dispatchEvent(new Event("change", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({ model: { op: "clear" }, thinking: { op: "set", value: false } }),
    );
    // The host's schema wants a JSON boolean here, not the select's "false".
    expect(transport.setSubagentOverride.mock.calls[0][0].thinking.value).toBe(false);
  });

  it("targets the write layer's revision rather than the candidate's own", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      qualified(
        { settingsRevision: "candidate-rev" },
        { settingsRevisions: { global: "layer-rev", project: "project-rev" } },
      ),
    );
    details(transport);
    transport.setSubagentOverride.mockResolvedValue({ inventory: qualified() });
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    const model = container.querySelector('[aria-label="Model"]');
    model.value = "openai/gpt-5";
    model.dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledWith(
      expect.objectContaining({ expectedRevision: "layer-rev" }),
    );
  });

  it("blocks a second write while the first one is still in flight", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    const pending = deferred();
    transport.setSubagentOverride.mockReturnValue(pending.promise);
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    const model = container.querySelector('[aria-label="Model"]');
    model.value = "openai/gpt-5";
    model.dispatchEvent(new Event("input", { bubbles: true }));
    container.querySelector(".subagents-save").click();
    const save = container.querySelector(".subagents-save");
    expect(save.disabled).toBe(true);
    save.click();
    await flush();
    expect(transport.setSubagentOverride).toHaveBeenCalledTimes(1);
    pending.resolve({ inventory: qualified() });
    await flush();
    expect(container.textContent).toContain("Saved to disk");
  });

  it("states the write layer above the save area, global first", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(qualified());
    details(transport);
    const { page } = setup(transport);
    await page.activate();
    await openRow();
    const hint = container.querySelector(".subagents-write-layer");
    expect(hint.textContent).toBe(
      "Write layer: Global ~/.pi/agent/settings.json · name-level override coder · definition files are unchanged",
    );
    const form = container.querySelector(".subagents-overrides");
    expect(hint.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(hint.textContent).toContain("definition files are unchanged");
  });

  it("names the project settings file when the root is known and degrades without it", async () => {
    const project = (overrides) =>
      qualified(
        { id: "c-p", runtimeName: "proj", source: "project", sourceScope: "project" },
        overrides,
      );
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      project({ workspaceRoot: "/ws", projectRoot: "/ws" }),
    );
    details(transport);
    const { page } = setup(transport);
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    await openRow();
    expect(container.querySelector(".subagents-write-layer").textContent).toContain(
      "Current project /ws/.pi/settings.json",
    );

    transport.listSubagents.mockResolvedValue(project({}));
    await page.activate();
    await openRow();
    expect(container.querySelector(".subagents-write-layer").textContent).toContain(
      "Current project .pi/settings.json",
    );
  });
});

// The page is a visual/structural rebuild on the Extensions package-manager
// shell: the assertions below lock the composed classes and the master/detail
// hierarchy, not just the copy and the wire contract above.
describe("subagents layout structure", () => {
  async function activated(entries = [candidate()], overrides = {}) {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory(entries, overrides));
    transport.getSubagentDetail.mockResolvedValue({
      candidateId: "c-user-1",
      rawDefinition: "---\nname: coder\n---\nbody",
      diagnostic: null,
    });
    const { page } = setup(transport);
    await page.activate();
    return { transport, page };
  }

  it("reuses the settings section and the extensions page-tab shell", async () => {
    await activated();
    const root = container.querySelector(".subagents-view");
    expect(root).not.toBeNull();
    expect(root.classList.contains("settings-section")).toBe(true);
    const tabs = container.querySelector(".subagents-scopes");
    expect(tabs.classList.contains("extensions-page-tabs")).toBe(true);
    expect(tabs.getAttribute("role")).toBe("tablist");
    for (const button of container.querySelectorAll("[data-subagents-scope]")) {
      expect(button.classList.contains("extensions-page-tab")).toBe(true);
      expect(button.getAttribute("role")).toBe("tab");
      expect(button.getAttribute("aria-selected")).toBe(
        String(button.dataset.subagentsScope === "global"),
      );
    }
  });

  it("builds the master/detail layout out of the package-manager card classes", async () => {
    await activated();
    const layout = container.querySelector(".subagents-layout");
    expect(layout.classList.contains("pkg-manager-layout")).toBe(true);
    expect(layout.querySelector(".subagents-master").classList.contains("pkg-manager-groups")).toBe(
      true,
    );
    expect(layout.querySelector(".subagents-detail").classList.contains("pkg-manager-detail")).toBe(
      true,
    );
    expect(
      container
        .querySelector(".subagents-group-header")
        .classList.contains("pkg-manager-group-header"),
    ).toBe(true);
    const add = container.querySelector(".subagents-new");
    expect(add.classList.contains("models-provider-add")).toBe(true);
    // The add affordance closes the master list, as on the MCP page.
    expect(add.parentElement.classList.contains("subagents-master")).toBe(true);
  });

  it("nests the row name and the also-in badge inside the package-manager row", async () => {
    await activated([
      candidate({
        id: "c-shared",
        runtimeName: "shared.s",
        source: "package",
        sourceScope: "global",
        additionalScopes: ["project"],
      }),
    ]);
    clickSubtab("packages");
    const row = container.querySelector(".subagents-row");
    expect(row.classList.contains("pkg-manager-sidebar-row")).toBe(true);
    expect(row.querySelector(".pkg-manager-sidebar-name").textContent).toBe("shared.s");
    const meta = row.querySelector(".pkg-manager-sidebar-meta");
    expect(meta).not.toBeNull();
    // The package source badge and the also-in badge share the meta row.
    expect(rowBadges()).toEqual(["package", "Also available in: project"]);
    // No status dot: nothing may imply a verified runtime winner.
    expect(row.querySelector(".pkg-manager-status-dot")).toBeNull();
  });

  it("keeps .is-selected in step with aria-pressed on the selected row", async () => {
    await activated([
      candidate({ id: "c-1", runtimeName: "one" }),
      candidate({ id: "c-2", runtimeName: "two" }),
    ]);
    const rows = () => [...container.querySelectorAll(".subagents-row")];
    expect(rows().every((row) => !row.classList.contains("is-selected"))).toBe(true);
    rows()[1].click();
    await flush();
    const [first, second] = rows();
    expect(second.getAttribute("aria-pressed")).toBe("true");
    expect(second.classList.contains("is-selected")).toBe(true);
    expect(first.getAttribute("aria-pressed")).toBe("false");
    expect(first.classList.contains("is-selected")).toBe(false);
  });

  it("renders loading, empty, and error states inside the master card", async () => {
    const transport = makeTransport();
    const pending = deferred();
    transport.listSubagents.mockReturnValueOnce(pending.promise);
    const { page } = setup(transport);
    const activating = page.activate();
    expect(container.querySelector(".subagents-master .subagents-loading")).not.toBeNull();
    pending.resolve(inventory([]));
    await activating;
    expect(container.querySelector(".subagents-master .subagents-empty")).not.toBeNull();

    transport.listSubagents.mockRejectedValueOnce(hostError("config_unavailable", "boom"));
    container.querySelector(".subagents-master .subagents-retry").click();
    await flush();
    expect(container.querySelector(".subagents-master .subagents-error")).not.toBeNull();
    expect(container.querySelector(".subagents-master .subagents-retry")).not.toBeNull();
  });

  it("lays detail metadata out in the package-manager status grid with a wrapping path", async () => {
    await activated();
    container.querySelector(".subagents-row").click();
    await flush();
    const header = container.querySelector(".subagents-detail .pkg-manager-detail-header");
    expect(header).not.toBeNull();
    expect(header.querySelector(".subagents-name").textContent).toBe("coder");
    const grid = container.querySelector(".subagents-detail .pkg-manager-status-grid");
    expect(grid).not.toBeNull();
    const rows = [...grid.querySelectorAll(".pkg-manager-status-row")];
    expect(rows.map((row) => row.querySelector("span:first-child").textContent.trim())).toEqual([
      "Runtime name:",
      "Source:",
      "Scope:",
      "File:",
    ]);
    const path = rows[3].querySelector("span.is-wrap");
    expect(path).not.toBeNull();
    expect(path.textContent).toContain("/agents/coder.md");
  });

  it("gives every new-agent field a visible label wrapping its input", async () => {
    await activated();
    container.querySelector(".subagents-new").click();
    for (const field of ["name", "description", "prompt"]) {
      const input = container.querySelector(`[name="${field}"]`);
      const caption = input.closest("label");
      expect(caption).not.toBeNull();
      expect(caption.classList.contains("subagents-field")).toBe(true);
      const captionText = caption.querySelector("span.subagents-field-label");
      expect(captionText).not.toBeNull();
      expect(captionText.textContent).toBeTruthy();
    }
  });

  it("gives override inputs visible labels while keeping their aria-label", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory([candidate({ writeQualified: true })], {
        resolutionContext: { mode: "verified", reason: null, projectWritesAllowed: true },
      }),
    );
    transport.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: "x" });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    for (const field of ["Model", "Thinking", "Show in parent agent directory"]) {
      const input = container.querySelector(`[aria-label="${field}"]`);
      expect(input.classList.contains("subagents-override-input")).toBe(true);
      const caption = input.closest("label");
      expect(caption).not.toBeNull();
      expect(caption.querySelector("span.subagents-field-label").textContent).toBe(field);
    }
  });

  it("keeps the raw definition as inert preformatted text in a capped panel", async () => {
    const raw = "---\nname: coder\n---\n\nBODY  \n";
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    transport.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: raw });
    const { page } = setup(transport);
    await page.activate();
    // Lazily fetched: nothing raw is rendered until the row is selected.
    expect(container.querySelector(".subagents-raw")).toBeNull();
    container.querySelector(".subagents-row").click();
    await flush();
    const pre = container.querySelector("pre.subagents-raw");
    expect(pre).not.toBeNull();
    expect(pre.children).toHaveLength(0);
    expect(pre.textContent).toBe(raw);
  });
});

// Definitions and Packages are two projections of one already-loaded scope.
describe("subagents sub-tabs", () => {
  const PACKAGE = candidate({
    id: "c-pkg",
    runtimeName: "pkg.tool",
    source: "package",
    packageIdentity: "tool",
    filePath: "/home/u/.pi/agent/npm/node_modules/tool/agents/tool.md",
  });

  it("offers both sub-tabs in each scope and defaults to Custom", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), PACKAGE]));
    const { page } = setup(transport);
    await page.activate();
    const row = container.querySelector(".skills-scope-tabs");
    expect(row.getAttribute("role")).toBe("tablist");
    expect(subtabLabels()).toEqual(["definitions", "packages"]);
    expect(selectedSubtab()).toBe("definitions");
    expect(container.querySelector('[data-subagents-subtab="definitions"]').classList).toContain(
      "active",
    );
    expect(container.querySelector('[data-subagents-subtab="packages"]').classList).not.toContain(
      "active",
    );

    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    expect(subtabLabels()).toEqual(["definitions", "packages"]);
    expect(selectedSubtab()).toBe("definitions");
    for (const button of container.querySelectorAll("[data-subagents-subtab]")) {
      expect(button.getAttribute("role")).toBe("tab");
    }
  });

  it("resets the sub-tab to Definitions when the scope changes", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), PACKAGE]));
    const { page } = setup(transport);
    await page.activate();
    clickSubtab("packages");
    expect(selectedSubtab()).toBe("packages");

    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    expect(selectedSubtab()).toBe("definitions");
    expect(transport.listSubagents).toHaveBeenLastCalledWith("project", {
      workspaceId: "w1",
      workspaceGeneration: 3,
    });
  });

  it("switches sub-tabs from the cached inventory without a new host request", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), PACKAGE]));
    const { page } = setup(transport);
    await page.activate();
    expect(transport.listSubagents).toHaveBeenCalledTimes(1);
    clickSubtab("packages");
    clickSubtab("definitions");
    clickSubtab("packages");
    expect(transport.listSubagents).toHaveBeenCalledTimes(1);
    expect(transport.listSubagents).toHaveBeenLastCalledWith("global", null);
    expect(transport.getSubagentDetail).not.toHaveBeenCalled();
  });

  it("shows the package identity as detail metadata when the host reports one", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), PACKAGE]));
    transport.getSubagentDetail.mockResolvedValue({ candidateId: "c-pkg", rawDefinition: "x" });
    const { page } = setup(transport);
    await page.activate();
    clickSubtab("packages");
    container.querySelector(".subagents-row").click();
    await flush();
    const captions = [
      ...container
        .querySelector(".subagents-detail .pkg-manager-status-grid")
        .querySelectorAll(".pkg-manager-status-row"),
    ].map((node) => node.querySelector("span:first-child").textContent.trim());
    expect(captions).toEqual(["Runtime name:", "Source:", "Scope:", "File:", "Package:"]);
    expect(container.querySelector(".subagents-detail").textContent).toContain("tool");
  });

  it("keeps the scope and sub-tab tablists independently cyclic for keyboard users", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), PACKAGE]));
    const { page } = setup(transport);
    await page.activate();
    const press = (selector, key) =>
      container
        .querySelector(selector)
        .dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));

    // The scope row never lands focus inside the sub-tab row.
    press('[data-subagents-scope="global"]', "ArrowRight");
    await flush();
    expect(document.activeElement.dataset.subagentsScope).toBe("project");
    expect(document.activeElement.dataset.subagentsSubtab).toBeUndefined();
    expect(transport.listSubagents).toHaveBeenCalledTimes(2);

    // The sub-tab row cycles within itself and never refetches.
    press('[data-subagents-subtab="definitions"]', "ArrowRight");
    await flush();
    expect(selectedSubtab()).toBe("packages");
    expect(document.activeElement.dataset.subagentsSubtab).toBe("packages");
    expect(transport.listSubagents).toHaveBeenCalledTimes(2);

    press('[data-subagents-subtab="packages"]', "ArrowLeft");
    await flush();
    expect(selectedSubtab()).toBe("definitions");
    press('[data-subagents-subtab="definitions"]', "End");
    await flush();
    expect(selectedSubtab()).toBe("packages");
    press('[data-subagents-subtab="packages"]', "Home");
    await flush();
    expect(selectedSubtab()).toBe("definitions");

    // Home on the scope row returns to Global and re-defaults the sub-tab.
    clickSubtab("packages");
    press('[data-subagents-scope="project"]', "Home");
    await flush();
    expect(document.activeElement.dataset.subagentsScope).toBe("global");
    expect(selectedSubtab()).toBe("definitions");
    expect(transport.listSubagents).toHaveBeenCalledTimes(3);
  });
});

// Built-ins are read-only extension agents: they ride the Packages projection of
// the Global scope only, never the writable Custom list or the project tab.
describe("subagents built-in placement", () => {
  const BUILTIN = candidate({
    id: "c-builtin",
    runtimeName: "builtin-agent",
    source: "builtin",
    filePath: null,
  });

  it("shows built-ins only in the Global Packages group", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), BUILTIN]));
    const { page } = setup(transport);
    await page.activate();
    expect(rowNames()).toEqual(["coder"]);
    expect(container.textContent).not.toContain("builtin-agent");

    clickSubtab("packages");
    expect(groupHeaders()).toEqual(["pi-subagents built-in extension (read-only)"]);
    expect(rowNames()).toEqual(["builtin-agent"]);
  });

  it("never lists built-ins on the project tab", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate(), BUILTIN]));
    const { page } = setup(transport);
    await page.activate();
    container.querySelector('[data-subagents-scope="project"]').click();
    await flush();
    clickSubtab("packages");
    expect(groupHeaders()).toEqual([]);
    expect(container.textContent).not.toContain("builtin-agent");
    expect(container.textContent).not.toContain("pi-subagents built-in extension");
  });
});

// The page translates known host diagnostics without letting raw English leak
// into a translated UI, and without hiding why a write is blocked.
describe("subagents diagnostics and wording", () => {
  afterEach(() => setMessages(EN_MESSAGES));

  const zhMessages = () => JSON.parse(readFileSync("public/locales/zh.json", "utf8"));

  it("locks the reviewed zh wording for packages, custom, and rescan", () => {
    const zh = zhMessages().settings.subagents;
    expect(zh.groups.package).toBe("扩展包");
    expect(zh.detail.package).toBe("扩展包");
    expect(zh.subtabs.packages).toBe("扩展包");
    expect(zh.subtabs.definitions).toBe("自定义");
    expect(zh.rescan).toBe("重新扫描");
    expect(zh.retry).toBeUndefined();
  });

  it("locks the reviewed zh write-layer and override-control wording", () => {
    const zh = zhMessages().settings.subagents.detail;
    expect(zh.writeLayer).toBe("写入层：{scope} · 名字级覆盖 {runtimeName} · 不修改定义文件");
    expect(zh.writeLayerGlobal).toBe("全局 ~/.pi/agent/settings.json");
    expect(zh.writeLayerProject).toBe("当前项目 {root}/.pi/settings.json");
    expect(zh.writeLayerProjectUnknown).toBe("当前项目 .pi/settings.json");
    expect(zh.noLayerOverride).toBe("无本层覆盖");
    expect(zh.keepCurrent).toBe("保留当前：{value}");
    expect(zh.thinkingFalse).toBe("关闭(false)");
    expect(zh.thinkingHint).toContain("/subagents-models");
    expect(zh.advertise).toBe("在父代理目录中展示");
    expect(zh.enable).toBe("启用");
    expect(zh.inheritDefinition).toBe("未设置（继承定义）");
    expect(zh.booleanOn).toBe("开(true)");
    expect(zh.booleanOff).toBe("关(false)");
    expect(zhMessages().settings.subagents.status.disabledOverride).toBe("已停用(覆盖)");
  });

  it("renders the zh sub-tab, built-in group, and rescan button", async () => {
    setMessages(zhMessages());
    const transport = makeTransport();
    transport.listSubagents.mockRejectedValueOnce(hostError("config_unavailable", "boom"));
    const { page } = setup(transport);
    await page.activate();
    expect(container.querySelector(".subagents-retry").textContent).toBe("重新扫描");

    transport.listSubagents.mockResolvedValue(
      inventory([
        candidate(),
        candidate({ id: "c-b", runtimeName: "b", source: "builtin", filePath: null }),
      ]),
    );
    container.querySelector(".subagents-retry").click();
    await flush();
    const subtab = (name) => container.querySelector(`[data-subagents-subtab="${name}"]`);
    expect(subtab("definitions").textContent).toBe("自定义");
    expect(subtab("packages").textContent).toBe("扩展包");
    clickSubtab("packages");
    expect(groupHeaders()).toEqual(["pi-subagents 扩展内置（只读）"]);
  });

  it("localizes known diagnostics, keeps unknown text, and keeps an independent write blocker", async () => {
    setMessages(zhMessages());
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [
          candidate({
            writeDiagnostic: {
              source: "project",
              message:
                "extension project root differs from workspace root; project writes disabled",
            },
          }),
        ],
        {
          diagnostics: [
            { source: "scan", message: "symlink source omitted" },
            { source: "custom", message: "undocumented host note" },
          ],
        },
      ),
    );
    const { page } = setup(transport);
    await page.activate();
    expect(container.querySelector(".subagents-diagnostics")).toBeNull();

    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.querySelector(".subagents-write-diagnostic").textContent).toContain(
      "已禁用项目写入",
    );
    expect(container.textContent).not.toContain("winner or out-of-scope occupancy unverified");
  });

  it("drops the parity note that only restates the candidate status row", async () => {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(inventory([candidate()]));
    transport.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: "x" });
    const { page } = setup(transport);
    await page.activate();
    container.querySelector(".subagents-row").click();
    await flush();
    expect(container.querySelector(".subagents-write-diagnostic")).toBeNull();
    expect(container.querySelector(".subagents-status").textContent).toBe(
      "Disk candidate — runtime winner unverified",
    );
  });
});

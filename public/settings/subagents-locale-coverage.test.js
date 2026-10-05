// @vitest-environment jsdom

// ABOUTME: Renders the Subagents settings tab through every branch and fails when a
// ABOUTME: key it asks for is missing from a locale, or when locale copy is never used.

import { readFileSync } from "node:fs";
import { beforeAll, describe, expect, it, vi } from "vitest";

const LOCALES = ["en", "zh", "ja", "es"];

const messagesFor = (locale) => JSON.parse(readFileSync(`public/locales/${locale}.json`, "utf8"));

const lookupValue = (messages, key) => {
  let node = messages;
  for (const part of key.split(".")) {
    if (node == null || typeof node !== "object") return undefined;
    node = node[part];
  }
  return typeof node === "string" ? node : undefined;
};

// Mocked t() records every key the renderer asks for — including keys assembled
// from template literals that a source scan cannot expand. The value it returns
// matches the real i18n contract: the active string, or the key itself when the
// lookup fails (which is exactly the "untranslated" case this gate catches).
const { requested } = vi.hoisted(() => ({ requested: new Set() }));

vi.mock("../i18n.js", async (importOriginal) => {
  const actual = await importOriginal();
  const { readFileSync: read } = await import("node:fs");
  const en = JSON.parse(read("public/locales/en.json", "utf8"));
  return {
    ...actual,
    t: (key, params = {}) => {
      requested.add(key);
      const value = lookupValue(en, key);
      if (typeof value !== "string") return key;
      return value.replace(/\{(\w+)\}/g, (_, name) =>
        params[name] === undefined ? `{${name}}` : String(params[name]),
      );
    },
  };
});

import { setupSubagentsTab } from "./subagents-tab.js";

const MODULE_SOURCE = readFileSync("public/settings/subagents-tab.js", "utf8");
const SUBAGENTS = "settings.subagents.";

// Namespaces en.json carries but no render path reaches. Pinning them here keeps
// the "unused copy" assertion below honest instead of blanket-permissive.
const NEVER_RENDERED = new Set([`${SUBAGENTS}title`, `${SUBAGENTS}diskOnlyMode`]);

// The host-message → locale-key map inside the module names its keys relative to
// the namespace. Reading the messages out of the module itself means a new host
// message cannot be added without its locale key being checked here.
const HOST_MESSAGES = [
  ...MODULE_SOURCE.matchAll(/"([^"]+)":\s*"(?:diagnostics|status)\.[A-Za-z0-9]+",?/g),
].map((match) => match[1]);

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const hostError = (code) => {
  const error = new Error(code);
  error.code = code;
  return error;
};

function deferred() {
  let resolve;
  const promise = new Promise((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

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
    writeDiagnostic: null,
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
    diagnostics: [],
    inventoryRevision: "inv-1",
    settingsRevisions: { global: "rev-1", project: "project-rev" },
    ...overrides,
  };
}

const VERIFIED = {
  resolutionContext: { mode: "verified", reason: null, projectWritesAllowed: true },
};

function makeTransport() {
  return {
    listSubagents: vi.fn(),
    getSubagentDetail: vi.fn(),
    createSubagent: vi.fn(),
    setSubagentOverride: vi.fn(),
  };
}

const WORKSPACE = { workspaceId: "w1", workspaceGeneration: 1 };

const CATALOG = [
  { provider: "openai", id: "gpt-5" },
  { provider: "anthropic", id: "claude-x" },
];

const catalogGateway = (models = CATALOG) => ({
  call: async (op) =>
    op === "list_model_catalog"
      ? {
          ok: true,
          data: {
            providers: models.map((model) => ({
              provider: model.provider,
              models: [{ provider: model.provider, id: model.id, available: true, visible: true }],
            })),
          },
        }
      : { ok: true, data: { modelIds: [] } },
});

let container;

function setup(transport, { configGateway, identity = WORKSPACE } = {}) {
  return setupSubagentsTab({
    container,
    transport,
    getWorkspaceIdentity: () => identity,
    confirmDiscard: () => true,
    configGateway,
  });
}

const scopeTab = (scope) => container.querySelector(`[data-subagents-scope="${scope}"]`);
const subtab = (name) => container.querySelector(`[data-subagents-subtab="${name}"]`);
const rows = () => [...container.querySelectorAll(".subagents-row")];

async function openRow(index = 0) {
  rows()[index].click();
  await flush();
}

async function renderAllBranches() {
  container = document.createElement("div");

  // Loading, error, and conflict: every failure branch owns its own copy.
  const failing = makeTransport();
  failing.listSubagents.mockRejectedValueOnce(hostError("boom"));
  let page = setup(failing);
  await page.activate(); // state.error + rescan

  failing.listSubagents.mockRejectedValueOnce(hostError("stale_generation"));
  container.querySelector(".subagents-retry").click(); // state.conflict
  await flush();

  const pending = makeTransport();
  const gate = deferred();
  pending.listSubagents.mockReturnValue(gate.promise);
  page = setup(pending);
  const activation = page.activate(); // state.loading renders before the request settles
  gate.resolve(
    inventory([
      candidate({ id: "c-user-1", writeQualified: true, writeDiagnostic: null }),
      candidate({
        id: "c-project-1",
        runtimeName: "reviewer",
        source: "project",
        sourceScope: "project",
        additionalScopes: ["global"],
      }),
      candidate({
        id: "c-pkg-1",
        runtimeName: "pkg-agent",
        source: "package",
        sourceScope: "global",
        packageIdentity: "npm:pkg-a",
      }),
      candidate({
        id: "c-builtin-1",
        runtimeName: "builtin-agent",
        source: "builtin",
        filePath: null,
        nativeOverrideSupported: false,
      }),
      candidate({
        id: "c-other-1",
        runtimeName: "alone",
        status: "candidate",
        inferredValue: { model: "openai/gpt-5", thinking: "high", source: "definition" },
      }),
    ]),
  );
  await activation;

  // Empty branch, plus the project scope tab: its own description copy.
  const empty = makeTransport();
  empty.listSubagents.mockResolvedValue(inventory([]));
  page = setup(empty);
  await page.activate();
  scopeTab("project").click();
  await flush();

  // Rich detail view: overrides form, saved override grid, badges, raw block.
  const rich = makeTransport();
  const richInventory = inventory(
    [
      candidate({
        id: "c-user-1",
        writeQualified: true,
        writeDiagnostic: null,
        additionalScopes: ["project"],
        packageIdentity: "npm:pkg-a",
        savedOverride: {
          model: "openai/gpt-5",
          thinking: "ultra",
          advertise: true,
          disabled: true,
        },
      }),
      candidate({ id: "c-project-1", runtimeName: "reviewer", source: "project" }),
      candidate({
        id: "c-pkg-1",
        runtimeName: "pkg-agent",
        source: "package",
        packageIdentity: "npm:pkg-a",
      }),
      candidate({
        id: "c-pkg-2",
        runtimeName: "unnamed-pkg-agent",
        source: "package",
        packageIdentity: null,
      }),
      candidate({ id: "c-nofile-1", runtimeName: "file-less", filePath: null }),
      candidate({
        id: "c-inferred-1",
        runtimeName: "inferred-agent",
        inferredValue: { model: "openai/gpt-5", thinking: "high", source: "definition" },
      }),
      candidate({
        id: "c-builtin-1",
        runtimeName: "builtin-agent",
        source: "builtin",
        filePath: null,
      }),
    ],
    {
      ...VERIFIED,
      workspaceRoot: "/ws",
      projectRoot: "/ws",
      diagnostics: [{ source: "scan", message: "symlink source omitted" }],
    },
  );
  rich.listSubagents.mockResolvedValue(richInventory);
  const detailGate = deferred();
  rich.getSubagentDetail.mockReturnValueOnce(detailGate.promise).mockResolvedValue({
    candidateId: "c-user-1",
    rawDefinition: "---\nname: coder\n---\nbody",
  });
  rich.createSubagent.mockResolvedValue({ inventory: richInventory });
  rich.setSubagentOverride.mockResolvedValue({ inventory: richInventory });
  page = setup(rich, { configGateway: catalogGateway() });
  await page.activate();
  await flush(); // model catalog arrives
  await openRow(0); // detail.rawLoading while the definition is in flight
  detailGate.resolve({ candidateId: "c-user-1", rawDefinition: "---\nname: coder\n---\nbody" });
  await flush();

  const toggle = container.querySelector(".subagents-detail-toggle");
  if (toggle && !toggle.disabled) toggle.click(); // writeDisabled path
  await flush();

  // Unknown raw definition: detail.rawUnavailable instead of a pre block.
  rich.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: null });
  await openRow(0);

  // An inferred value renders only for verified candidates; a file-less
  // definition says so instead of pointing at a path.
  await openRow(rows().findIndex((row) => row.dataset.candidateId === "c-inferred-1"));
  await openRow(rows().findIndex((row) => row.dataset.candidateId === "c-nofile-1"));

  // Packages sub-tab: package-identity groups and the built-in extension group.
  subtab("packages").click();
  await flush();
  await openRow(0);

  // Create form: the invalid path, then the saved path.
  subtab("definitions").click();
  await flush();
  container.querySelector(".subagents-new").click();
  const fill = (name, value) => {
    const input = container.querySelector(`.subagents-create-input[name="${name}"]`);
    input.value = value;
    input.dispatchEvent(new Event("input", { bubbles: true }));
  };
  fill("name", "helper");
  fill("description", "helps");
  container.querySelector(".subagents-create").click(); // create.invalid
  await flush();
  fill("prompt", "do work");
  container.querySelector(".subagents-create").click(); // detail.saved
  await flush();
  expect(page).toBeTruthy();

  // Write layer naming: both scopes, with and without a known project root,
  // plus the free-text model fallback's own rejection copy.
  for (const { scope, root } of [
    { scope: "global", root: "/ws" },
    { scope: "project", root: "/ws" },
    { scope: "project", root: null },
  ]) {
    const writable = makeTransport();
    const writableInventory = inventory(
      [
        candidate({
          id: "c-user-1",
          writeQualified: true,
          writeDiagnostic: null,
          additionalScopes: ["project"],
          savedOverride: { model: "openai/gpt-5", thinking: null, advertise: null, disabled: null },
        }),
      ],
      { ...VERIFIED, workspaceRoot: root, projectRoot: root },
    );
    writable.listSubagents.mockResolvedValue(writableInventory);
    writable.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: "x" });
    writable.setSubagentOverride.mockResolvedValue({ inventory: writableInventory });
    container = document.createElement("div");
    page = setup(writable);
    await page.activate();
    if (scope === "project") {
      scopeTab("project").click();
      await flush();
    }
    await openRow(0);
    const modelInput = container.querySelector("input.subagents-override-input");
    if (modelInput) {
      modelInput.value = "not-a-model-id";
      modelInput.dispatchEvent(new Event("input", { bubbles: true }));
      container.querySelector(".subagents-save").click(); // detail.invalidModel
      await flush();
    }
  }

  // Read-only inventory: create.disabled on the submit button.
  const readOnly = makeTransport();
  const readOnlyInventory = inventory([
    candidate({ id: "c-user-1", writeQualified: false, writeDiagnostic: null }),
  ]);
  readOnly.listSubagents.mockResolvedValue(readOnlyInventory);
  readOnly.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: "x" });
  container = document.createElement("div");
  page = setup(readOnly);
  await page.activate();
  container.querySelector(".subagents-new")?.click();

  // Host diagnostics: every known message maps to its own locale key, so each
  // one is fed through a write-diagnostic render.
  for (const message of HOST_MESSAGES) {
    const transport = makeTransport();
    transport.listSubagents.mockResolvedValue(
      inventory(
        [
          candidate({
            id: "c-user-1",
            writeQualified: true,
            writeDiagnostic: { source: "host", message },
          }),
        ],
        VERIFIED,
      ),
    );
    transport.getSubagentDetail.mockResolvedValue({ candidateId: "c-user-1", rawDefinition: null });
    container = document.createElement("div");
    const diagnosticPage = setup(transport);
    await diagnosticPage.activate();
    await openRow(0);
  }

  expect(page).toBeTruthy();
}

// Landing has no workspace identity: one scope tab and no project copy.
async function renderLandingScope() {
  container = document.createElement("div");
  const transport = makeTransport();
  transport.listSubagents.mockResolvedValue(inventory([]));
  const page = setupSubagentsTab({
    container,
    transport,
    getWorkspaceIdentity: () => null,
    landingOnly: true,
    confirmDiscard: () => true,
  });
  await page.activate();
}

describe("subagents locale coverage", () => {
  beforeAll(async () => {
    await renderAllBranches();
    await renderLandingScope();
  });

  it("walks the whole namespace from the render path", () => {
    // Every host diagnostic message in the module was fed through a render.
    expect(HOST_MESSAGES.length).toBeGreaterThan(15);
    expect([...requested].some((key) => key.startsWith(SUBAGENTS))).toBe(true);
  });

  for (const locale of LOCALES) {
    it(`every key the renderer asks for exists in ${locale}.json`, () => {
      const messages = messagesFor(locale);
      const missing = [...requested]
        .filter((key) => key.startsWith(SUBAGENTS))
        .filter((key) => lookupValue(messages, key) === undefined);
      expect([...new Set(missing)], `${locale}.json missing: ${missing.join(", ")}`).toEqual([]);
    });
  }

  it("no settings.subagents key is unreachable from the render path", () => {
    const en = messagesFor("en");
    const expected = [];
    const collect = (node, prefix) => {
      for (const [key, value] of Object.entries(node)) {
        const path = `${prefix}.${key}`;
        if (value !== null && typeof value === "object" && !Array.isArray(value)) {
          collect(value, path);
        } else {
          expected.push(path);
        }
      }
    };
    collect(en.settings.subagents, SUBAGENTS.slice(0, -1));
    const unused = expected.filter((key) => !requested.has(key) && !NEVER_RENDERED.has(key));
    expect(unused, `unused copy in en.json: ${unused.join(", ")}`).toEqual([]);
  });
});

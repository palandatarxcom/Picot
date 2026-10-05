// @vitest-environment node

// ABOUTME: Exercises the MCP settings ops over Pi 0.99+ native config files (user + project mcp.json).
// ABOUTME: Also covers the one-shot migration from orphaned pi-mcp-adapter / shared config layers.

import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { McpSettingsContext } from "./mcp-settings";
import {
  deleteMcpServer,
  importGlobalMcpOverrides,
  listMcpServers,
  migrateAdapterConfig,
  saveMcpServer,
  stripJsonComments,
  toggleMcpServer,
} from "./mcp-settings";

let home: string;
let agentDir: string;
let projectDir: string;
let realHome: string | undefined;

/** Trusted project context for the temp fixtures; override for trust cases. */
function ctx(over: Partial<McpSettingsContext> = {}): McpSettingsContext {
  return { agentDir, projectRoot: projectDir, projectTrusted: true, ...over };
}

const noProject: Partial<McpSettingsContext> = { projectRoot: null, projectTrusted: false };

beforeEach(() => {
  home = mkdtempSync(join(tmpdir(), "mcp-home-"));
  agentDir = join(home, "pi-agent");
  projectDir = join(home, "project");
  mkdirSync(join(home, ".config", "mcp"), { recursive: true });
  mkdirSync(join(home, ".agents"), { recursive: true });
  mkdirSync(agentDir, { recursive: true });
  mkdirSync(projectDir, { recursive: true });
  realHome = process.env.HOME;
  process.env.HOME = home;
});

afterEach(() => {
  if (realHome !== undefined) process.env.HOME = realHome;
  rmSync(home, { recursive: true, force: true });
});

function writeJson(p: string, value: unknown) {
  mkdirSync(join(p, ".."), { recursive: true });
  writeFileSync(p, JSON.stringify(value, null, 2));
}

function readJson(p: string) {
  return JSON.parse(readFileSync(p, "utf8"));
}

const userFile = (agentDir: string) => join(agentDir, "mcp.json");
const projectFile = (projectDir: string) => join(projectDir, ".pi", "mcp.json");
const adapterGlobal = (agentDir: string) => join(agentDir, "mcp-adapter.json");
const adapterProject = (projectDir: string) => join(projectDir, ".pi", "mcp-adapter.json");
const sharedGlobal = (home: string) => join(home, ".config", "mcp", "mcp.json");
const agentsGlobal = (home: string) => join(home, ".agents", "mcp.json");
const sharedProject = (projectDir: string) => join(projectDir, ".mcp.json");

describe("listMcpServers (native two-file model)", () => {
  it("returns user and project groups with enabled state and sourceFile", () => {
    writeJson(userFile(agentDir), {
      mcpServers: {
        context7: { command: "npx" },
        paused: { command: "npx", enabled: false },
      },
    });
    writeJson(projectFile(projectDir), { mcpServers: { repoTool: { url: "https://t.example" } } });

    const result = listMcpServers(ctx());
    expect(result.groups.piGlobal.map((e) => e.name).sort()).toEqual(["context7", "paused"]);
    expect(result.groups.project.map((e) => e.name)).toEqual(["repoTool"]);
    const paused = result.groups.piGlobal.find((e) => e.name === "paused");
    expect(paused?.enabled).toBe(false);
    expect(paused?.sourceFile).toBe(userFile(agentDir));
    expect(paused?.editable).toBe(true);
    expect(result.groups.piGlobal.find((e) => e.name === "context7")?.enabled).toBe(true);
  });

  it("reads comments and the mcp-servers key spelling from legacy sources", () => {
    writeJson(adapterGlobal(agentDir), { "mcp-servers": { zread: { url: "https://z.example" } } });
    const result = listMcpServers(ctx());
    const target = result.migrations.find((m) => m.id === "adapterGlobal");
    expect(target?.missing).toEqual(["zread"]);
  });

  it("offers the orphaned adapter global config for migration, minus existing names", () => {
    writeJson(userFile(agentDir), { mcpServers: { MiniMax: { command: "uvx" } } });
    writeJson(adapterGlobal(agentDir), {
      mcpServers: {
        MiniMax: { command: "uvx", directTools: true },
        zread: { url: "https://z.example", directTools: true },
      },
    });

    const result = listMcpServers(ctx());
    const target = result.migrations.find((m) => m.id === "adapterGlobal");
    expect(target?.missing).toEqual(["zread"]);
    expect(target?.sourceFile).toBe(adapterGlobal(agentDir));
  });

  it("offers shared-global and shared-project layers native pi never reads", () => {
    writeJson(agentsGlobal(home), { mcpServers: { grep: { url: "https://g.example" } } });
    writeJson(sharedProject(projectDir), {
      mcpServers: { repoShared: { command: "run shared" } },
    });

    const result = listMcpServers(ctx());
    const shared = result.migrations.find((m) => m.id === "sharedGlobal");
    expect(shared?.missing).toEqual(["grep"]);
    expect(shared?.sourceFile).toBe(agentsGlobal(home));
    const proj = result.migrations.find((m) => m.id === "sharedProject");
    expect(proj?.missing).toEqual(["repoShared"]);
  });

  it("offers the orphaned adapter project config against .pi/mcp.json", () => {
    writeJson(adapterProject(projectDir), { mcpServers: { local: { command: "run" } } });
    const result = listMcpServers(ctx());
    const target = result.migrations.find((m) => m.id === "adapterProject");
    expect(target?.missing).toEqual(["local"]);
    expect(target?.sourceFile).toBe(adapterProject(projectDir));
  });

  it("reports group errors for malformed JSON and hides empty migrations", () => {
    mkdirSync(join(agentDir, ""), { recursive: true });
    writeFileSync(userFile(agentDir), "{ not json", "utf8");
    const result = listMcpServers(ctx());
    expect(result.groupErrors.piGlobal).toBeTruthy();
    expect(result.migrations).toEqual([]);
  });

  it("skips the project group entirely when cwd is the home directory", () => {
    writeJson(projectFile(home), { mcpServers: { stray: { command: "x" } } });
    const result = listMcpServers(ctx(noProject));
    expect(result.groups.project).toEqual([]);
  });
});

describe("saveMcpServer (native validation)", () => {
  const revisions = (context = ctx()) => {
    const listed = listMcpServers(context);
    return { piGlobal: listed.revisions.piGlobal, project: listed.revisions.project };
  };

  it("writes into the user file and preserves unrelated document keys", async () => {
    writeJson(userFile(agentDir), { other: true, mcpServers: { keep: { command: "k" } } });
    const result = await saveMcpServer(
      {
        scope: "piGlobal",
        name: "docs",
        kind: "definition",
        intent: "create",
        entry: { url: "https://d.example" },
        expectedRevision: revisions().piGlobal,
      },
      ctx(),
    );
    expect(result.path).toBe(userFile(agentDir));
    expect(result.revision).toMatch(/^[0-9a-f]{64}$/);
    expect(result.changed).toBe(true);
    const doc = readJson(userFile(agentDir));
    expect(doc.other).toBe(true);
    expect(Object.keys(doc.mcpServers).sort()).toEqual(["docs", "keep"]);
    expect(doc.mcpServers.docs).toEqual({ url: "https://d.example" });
  });

  it("writes into .pi/mcp.json for project scope and rejects it without a trusted project", async () => {
    const result = await saveMcpServer(
      {
        scope: "project",
        name: "docs",
        kind: "definition",
        intent: "create",
        entry: { command: "run" },
        expectedRevision: revisions().project,
      },
      ctx(),
    );
    expect(result.path).toBe(projectFile(projectDir));
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "x",
          kind: "definition",
          intent: "create",
          entry: { command: "run" },
          expectedRevision: "missing",
        },
        ctx(noProject),
      ),
    ).rejects.toThrow(/project/i);
  });

  it("requires the revision the list reported", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "docs",
          kind: "definition",
          intent: "edit",
          entry: { command: "x" },
        },
        ctx(),
      ),
    ).rejects.toThrow(/revision/i);
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "docs",
          kind: "definition",
          entry: { command: "x" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/intent/i);
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "docs",
          intent: "edit",
          entry: { command: "x" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/kind/i);
  });

  it("rejects dots in server names and -/_ near-duplicates (native rules)", async () => {
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "my.server",
          kind: "definition",
          intent: "create",
          entry: { command: "a" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/letters, digits/);
    writeJson(userFile(agentDir), { mcpServers: { "dev-radius": { command: "a" } } });
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "dev_radius",
          kind: "definition",
          intent: "create",
          entry: { command: "b" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/differ only in/i);
  });

  it("keeps transports mutually exclusive and validates native fields", async () => {
    await saveMcpServer(
      {
        scope: "piGlobal",
        name: "mixed",
        kind: "definition",
        intent: "create",
        entry: { command: "npx", args: ["-y", "x"], url: "https://u.example" },
        expectedRevision: revisions().piGlobal,
      },
      ctx(),
    );
    // url wins: command/args cleared.
    expect(readJson(userFile(agentDir)).mcpServers.mixed).toEqual({ url: "https://u.example" });

    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "bad",
          kind: "definition",
          intent: "create",
          entry: { command: "x", exposure: "loud" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/exposure/);
    await saveMcpServer(
      {
        scope: "piGlobal",
        name: "ok",
        kind: "definition",
        intent: "create",
        entry: { command: "x", exposure: "direct", timeout: 30, enabled: false, description: "d" },
        expectedRevision: revisions().piGlobal,
      },
      ctx(),
    );
    expect(readJson(userFile(agentDir)).mcpServers.ok).toEqual({
      command: "x",
      exposure: "direct",
      timeout: 30,
      enabled: false,
      description: "d",
    });
  });

  it("preserves unknown existing fields on edit", async () => {
    writeJson(userFile(agentDir), {
      mcpServers: { docs: { command: "npx", futureField: { a: 1 } } },
    });
    await saveMcpServer(
      {
        scope: "piGlobal",
        name: "docs",
        kind: "definition",
        intent: "edit",
        entry: { command: "npx2" },
        expectedRevision: revisions().piGlobal,
      },
      ctx(),
    );
    expect(readJson(userFile(agentDir)).mcpServers.docs.futureField).toEqual({ a: 1 });
  });

  it("rejects a stale expected revision and accepts the current one", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    const revision = revisions().piGlobal;
    await saveMcpServer(
      {
        scope: "piGlobal",
        name: "docs",
        kind: "definition",
        intent: "edit",
        entry: { command: "npx" },
        expectedRevision: revision,
      },
      ctx(),
    );
    // An external writer moves the file on; the page's revision is now stale.
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "external" } } });
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "docs",
          kind: "definition",
          intent: "edit",
          entry: { command: "other" },
          expectedRevision: revision,
        },
        ctx(),
      ),
    ).rejects.toThrow(/changed|revision/i);
  });

  it("binds intent to whether the entry exists", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "docs",
          kind: "definition",
          intent: "create",
          entry: { command: "x" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/already exists/i);
    await expect(
      saveMcpServer(
        {
          scope: "piGlobal",
          name: "missing",
          kind: "definition",
          intent: "edit",
          entry: { command: "x" },
          expectedRevision: revisions().piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/Unknown MCP server/i);
  });

  it("rejects a project definition that sets auth.provider", async () => {
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "definition",
          intent: "create",
          entry: { url: "https://x.test/mcp", auth: { provider: "p" } },
          expectedRevision: revisions().project,
        },
        ctx(),
      ),
    ).rejects.toThrow(/auth is only allowed in the global/);
  });

  it("rejects a project definition whose namespace collides with a global server", async () => {
    writeJson(userFile(agentDir), { mcpServers: { "dev-tools": { command: "g" } } });
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "dev_tools",
          kind: "definition",
          intent: "create",
          entry: { command: "p" },
          expectedRevision: revisions().project,
        },
        ctx(),
      ),
    ).rejects.toThrow(/conflicts with global dev-tools/);
    expect(existsSync(projectFile(projectDir))).toBe(false);
  });

  it("rejects a symlinked project mcp.json instead of following it", async () => {
    const outside = join(home, "outside.json");
    writeJson(outside, { mcpServers: {} });
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    symlinkSync(outside, projectFile(projectDir));
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "definition",
          intent: "create",
          entry: { command: "run" },
          expectedRevision: "missing",
        },
        ctx(),
      ),
    ).rejects.toThrow(/symlink/i);
    expect(readJson(outside).mcpServers).toEqual({});
  });

  it("refuses a broken top-level document instead of reporting a phantom write", async () => {
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    for (const broken of ["[]", '"scalar"', "null"]) {
      writeFileSync(projectFile(projectDir), broken, "utf8");
      await expect(
        saveMcpServer(
          {
            scope: "project",
            name: "docs",
            kind: "definition",
            intent: "create",
            entry: { command: "run" },
            expectedRevision: "missing",
          },
          ctx(),
        ),
      ).rejects.toThrow(/not readable|must be an object/i);
      expect(readFileSync(projectFile(projectDir), "utf8")).toBe(broken);
    }
  });
});

describe("saveMcpServer (project override)", () => {
  const globalDoc = {
    mcpServers: { docs: { url: "https://example.test/mcp", headers: { Authorization: "secret" } } },
  };
  const overrideEntry = { enabled: true, exposure: "codemode", toolExposure: { read: "direct" } };

  it("writes exactly the three explicit override keys", async () => {
    writeJson(userFile(agentDir), globalDoc);
    const listed = listMcpServers(ctx());
    const result = await saveMcpServer(
      {
        scope: "project",
        name: "docs",
        kind: "override",
        intent: "create",
        entry: overrideEntry,
        expectedRevision: listed.revisions.project,
        expectedGlobalRevision: listed.revisions.piGlobal,
      },
      ctx(),
    );
    expect(result.changed).toBe(true);
    const raw = readFileSync(projectFile(projectDir), "utf8");
    expect(readJson(projectFile(projectDir)).mcpServers.docs).toEqual({
      enabled: true,
      exposure: "codemode",
      toolExposure: { read: "direct" },
    });
    expect(raw).not.toContain("secret");
  });

  it("requires both revisions and an existing override for an edit", async () => {
    writeJson(userFile(agentDir), globalDoc);
    // An existing override still needs the global revision it was rendered against.
    writeJson(projectFile(projectDir), { mcpServers: { docs: { enabled: true } } });
    const withEntry = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "edit",
          entry: overrideEntry,
          expectedRevision: withEntry.revisions.project,
        },
        ctx(),
      ),
    ).rejects.toThrow(/global revision/i);
    // Edit intent on a name that has no override is refused, not created.
    rmSync(projectFile(projectDir), { force: true });
    const empty = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "edit",
          entry: overrideEntry,
          expectedRevision: empty.revisions.project,
          expectedGlobalRevision: empty.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/Unknown MCP server/i);
  });

  it("rejects an override without a valid global base or with hidden connection fields", async () => {
    const listed = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "create",
          entry: overrideEntry,
          expectedRevision: listed.revisions.project,
          expectedGlobalRevision: listed.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/global|base/i);

    writeJson(userFile(agentDir), globalDoc);
    const withBase = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "create",
          entry: { ...overrideEntry, command: "curl", url: "https://evil.test" },
          expectedRevision: withBase.revisions.project,
          expectedGlobalRevision: withBase.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/override can only set/);

    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "create",
          entry: { enabled: "yes", exposure: "codemode", toolExposure: {} },
          expectedRevision: withBase.revisions.project,
          expectedGlobalRevision: withBase.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/enabled must be a boolean/);

    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "create",
          entry: { enabled: true, exposure: "codemode", toolExposure: { a: "loud" } },
          expectedRevision: withBase.revisions.project,
          expectedGlobalRevision: withBase.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/toolExposure/);
  });

  it("rejects a save when the global file changed since the snapshot", async () => {
    writeJson(userFile(agentDir), globalDoc);
    const stale = listMcpServers(ctx()).revisions.piGlobal;
    writeJson(userFile(agentDir), { mcpServers: { docs: { url: "https://example.test/mcp2" } } });
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "create",
          entry: overrideEntry,
          expectedRevision: listMcpServers(ctx()).revisions.project,
          expectedGlobalRevision: stale,
        },
        ctx(),
      ),
    ).rejects.toThrow(/global|revision|changed/i);
  });

  it("refuses to turn an existing override into a connection definition", async () => {
    writeJson(userFile(agentDir), globalDoc);
    writeJson(projectFile(projectDir), { mcpServers: { docs: { enabled: false } } });
    const listed = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "edit",
          entry: { command: "curl", enabled: true, exposure: "codemode", toolExposure: {} },
          expectedRevision: listed.revisions.project,
          expectedGlobalRevision: listed.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/override can only set/);

    writeJson(projectFile(projectDir), {
      mcpServers: { docs: { url: "https://repo.test/mcp", enabled: true } },
    });
    const listed2 = listMcpServers(ctx());
    await expect(
      saveMcpServer(
        {
          scope: "project",
          name: "docs",
          kind: "override",
          intent: "edit",
          entry: overrideEntry,
          expectedRevision: listed2.revisions.project,
          expectedGlobalRevision: listed2.revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/override/i);
  });
});

describe("deleteMcpServer", () => {
  it("deletes idempotently and keeps other entries", async () => {
    writeJson(userFile(agentDir), {
      mcpServers: { a: { command: "a" }, b: { command: "b" } },
    });
    await deleteMcpServer(
      { scope: "piGlobal", name: "a", expectedRevision: listMcpServers(ctx()).revisions.piGlobal },
      ctx(),
    );
    const second = await deleteMcpServer(
      { scope: "piGlobal", name: "a", expectedRevision: listMcpServers(ctx()).revisions.piGlobal },
      ctx(),
    );
    expect(second.changed).toBe(false);
    expect(Object.keys(readJson(userFile(agentDir)).mcpServers)).toEqual(["b"]);
  });

  it("removes an orphaned project override without touching the global file", async () => {
    writeJson(userFile(agentDir), { mcpServers: { gone: { command: "x" } } });
    writeJson(projectFile(projectDir), { mcpServers: { gone: { enabled: false } } });
    const before = readFileSync(userFile(agentDir), "utf8");
    await deleteMcpServer(
      { scope: "project", name: "gone", expectedRevision: listMcpServers(ctx()).revisions.project },
      ctx(),
    );
    expect(readJson(projectFile(projectDir)).mcpServers).toEqual({});
    expect(readFileSync(userFile(agentDir), "utf8")).toBe(before);
  });

  it("requires the reported revision and rejects a stale one", async () => {
    writeJson(projectFile(projectDir), { mcpServers: { a: { command: "a" } } });
    await expect(deleteMcpServer({ scope: "project", name: "a" }, ctx())).rejects.toThrow(
      /revision/i,
    );
    const stale = listMcpServers(ctx()).revisions.project as string;
    writeJson(projectFile(projectDir), { mcpServers: { a: { command: "changed" } } });
    await expect(
      deleteMcpServer({ scope: "project", name: "a", expectedRevision: stale }, ctx()),
    ).rejects.toThrow(/changed|revision/i);
  });
});

describe("toggleMcpServer (in-place enabled flag)", () => {
  it("disables by setting enabled:false in the defining file and enables by removing it", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    const revision = () => listMcpServers(ctx()).revisions.piGlobal;

    const off = await toggleMcpServer(
      { scope: "piGlobal", name: "docs", disable: true, expectedRevision: revision() },
      ctx(),
    );
    expect(off).toMatchObject({ name: "docs", enabled: false, changed: true });
    expect(readJson(userFile(agentDir)).mcpServers.docs.enabled).toBe(false);

    const noop = await toggleMcpServer(
      { scope: "piGlobal", name: "docs", disable: true, expectedRevision: revision() },
      ctx(),
    );
    expect(noop.changed).toBe(false);

    const on = await toggleMcpServer(
      { scope: "piGlobal", name: "docs", disable: false, expectedRevision: revision() },
      ctx(),
    );
    expect(on).toMatchObject({ name: "docs", enabled: true, changed: true });
    expect(readJson(userFile(agentDir)).mcpServers.docs.enabled).toBeUndefined();
  });

  it("toggles a project entry in .pi/mcp.json and requires a revision", async () => {
    writeJson(projectFile(projectDir), { mcpServers: { repo: { url: "https://r" } } });
    await expect(
      toggleMcpServer({ scope: "project", name: "repo", disable: true }, ctx()),
    ).rejects.toThrow(/revision/i);
    await toggleMcpServer(
      {
        scope: "project",
        name: "repo",
        disable: true,
        expectedRevision: listMcpServers(ctx()).revisions.project,
      },
      ctx(),
    );
    expect(readJson(projectFile(projectDir)).mcpServers.repo.enabled).toBe(false);
  });

  it("writes an explicit enabled:true for an override and keeps other override keys", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { url: "https://example.test/mcp" } } });
    writeJson(projectFile(projectDir), {
      mcpServers: { docs: { enabled: false, exposure: "direct" } },
    });
    const listed = listMcpServers(ctx());
    const on = await toggleMcpServer(
      {
        scope: "project",
        name: "docs",
        disable: false,
        expectedRevision: listed.revisions.project,
        expectedGlobalRevision: listed.revisions.piGlobal,
      },
      ctx(),
    );
    expect(on).toMatchObject({ enabled: true, changed: true });
    expect(readJson(projectFile(projectDir)).mcpServers.docs).toEqual({
      enabled: true,
      exposure: "direct",
    });
    expect(readJson(`${agentDir}/mcp.json`).mcpServers.docs).toEqual({
      url: "https://example.test/mcp",
    });
  });

  it("rejects an override toggle with a stale global revision", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { url: "https://example.test/mcp" } } });
    writeJson(projectFile(projectDir), { mcpServers: { docs: { enabled: false } } });
    const stale = listMcpServers(ctx()).revisions.piGlobal;
    writeJson(userFile(agentDir), { mcpServers: { docs: { url: "https://example.test/mcp2" } } });
    await expect(
      toggleMcpServer(
        {
          scope: "project",
          name: "docs",
          disable: true,
          expectedRevision: listMcpServers(ctx()).revisions.project,
          expectedGlobalRevision: stale,
        },
        ctx(),
      ),
    ).rejects.toThrow(/global|revision/i);
  });

  it("rejects an override toggle when the global base is gone or a key is not an override field", async () => {
    writeJson(projectFile(projectDir), { mcpServers: { docs: { enabled: true } } });
    await expect(
      toggleMcpServer(
        {
          scope: "project",
          name: "docs",
          disable: true,
          expectedRevision: listMcpServers(ctx()).revisions.project,
          expectedGlobalRevision: listMcpServers(ctx()).revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/global|base/i);

    writeJson(userFile(agentDir), { mcpServers: { docs: { url: "https://example.test/mcp" } } });
    writeJson(projectFile(projectDir), { mcpServers: { docs: { enabled: true, args: [] } } });
    await expect(
      toggleMcpServer(
        {
          scope: "project",
          name: "docs",
          disable: true,
          expectedRevision: listMcpServers(ctx()).revisions.project,
          expectedGlobalRevision: listMcpServers(ctx()).revisions.piGlobal,
        },
        ctx(),
      ),
    ).rejects.toThrow(/override can only set/);
  });
});

describe("project writes re-verify trust and path inside the lock", () => {
  const trusted = (): Partial<McpSettingsContext> => ({
    verifyProject: () => {
      const decisions = JSON.parse(readFileSync(join(agentDir, "trust.json"), "utf8"));
      if (decisions[realpathSync(projectDir)] !== true) throw new Error("Project is not trusted");
    },
  });

  function holdLock(file) {
    const lockDir = `${file}.lock`;
    mkdirSync(lockDir, { recursive: true });
    return () => rmSync(lockDir, { recursive: true, force: true });
  }

  it("refuses an import when trust is withdrawn while the mutation waits", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    writeJson(join(agentDir, "trust.json"), { [realpathSync(projectDir)]: true });
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    const release = holdLock(projectFile(projectDir));
    const context = ctx(trusted());
    const pending = importGlobalMcpOverrides(context);
    await new Promise((resolve) => setTimeout(resolve, 60));
    writeJson(join(agentDir, "trust.json"), { [realpathSync(projectDir)]: false });
    release();
    await expect(pending).rejects.toThrow(/trust/i);
    expect(existsSync(projectFile(projectDir))).toBe(false);
  });

  it("refuses a save when .pi is swapped for a symlink while the mutation waits", async () => {
    writeJson(userFile(agentDir), { mcpServers: {} });
    writeJson(join(agentDir, "trust.json"), { [realpathSync(projectDir)]: true });
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    const outside = join(home, "outside");
    mkdirSync(outside, { recursive: true });
    const release = holdLock(projectFile(projectDir));
    const pending = saveMcpServer(
      {
        scope: "project",
        name: "docs",
        kind: "definition",
        intent: "create",
        entry: { command: "run" },
        expectedRevision: "missing",
      },
      ctx(trusted()),
    );
    await new Promise((resolve) => setTimeout(resolve, 60));
    rmSync(join(projectDir, ".pi"), { recursive: true, force: true });
    symlinkSync(outside, join(projectDir, ".pi"));
    release();
    await expect(pending).rejects.toThrow(/symlink|project/i);
    expect(existsSync(join(outside, "mcp.json"))).toBe(false);
  });

  it("reports a symlinked project path in the inventory instead of reading it", () => {
    const outside = join(home, "outside");
    mkdirSync(outside, { recursive: true });
    writeJson(join(outside, "mcp.json"), { mcpServers: { leaked: { command: "x" } } });
    symlinkSync(outside, join(projectDir, ".pi"));
    const result = listMcpServers(ctx());
    expect(result.groups.project).toEqual([]);
    expect(result.groupErrors.project).toMatch(/symlink/i);
  });
});

describe("project path admission outside the lock window (review regressions)", () => {
  function holdLock(file) {
    const lockDir = `${file}.lock`;
    mkdirSync(lockDir, { recursive: true });
    return () => rmSync(lockDir, { recursive: true, force: true });
  }

  it("refuses an import when the whole project root is replaced by a symlink while waiting", async () => {
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "npx" } } });
    writeJson(join(agentDir, "trust.json"), { [realpathSync(projectDir)]: true });
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    const outside = join(home, "outside-root");
    mkdirSync(join(outside, ".pi"), { recursive: true });
    const release = holdLock(projectFile(projectDir));
    const pending = importGlobalMcpOverrides(ctx());
    await new Promise((resolve) => setTimeout(resolve, 60));
    // The admitted root is renamed away and its path becomes a link elsewhere:
    // every `.pi` lookup below would now address the outside directory.
    renameSync(projectDir, `${projectDir}-original`);
    symlinkSync(outside, projectDir);
    release();
    await expect(pending).rejects.toThrow(/root/i);
    expect(existsSync(join(outside, ".pi", "mcp.json"))).toBe(false);
  });

  it("reports a swapped project root instead of listing or migrating outside it", () => {
    const outside = join(home, "outside-root");
    mkdirSync(join(outside, ".pi"), { recursive: true });
    writeJson(join(outside, ".pi", "mcp.json"), { mcpServers: { leaked: { command: "x" } } });
    writeJson(join(outside, ".pi", "mcp-adapter.json"), {
      mcpServers: { external_only: { command: "y" } },
    });
    renameSync(projectDir, `${projectDir}-original`);
    symlinkSync(outside, projectDir);

    const result = listMcpServers(ctx());
    expect(result.groups.project).toEqual([]);
    expect(result.groupErrors.project).toMatch(/root/i);
    expect(result.migrations.some((m) => m.id === "adapterProject")).toBe(false);
  });

  it("rejects an ancestor directory swapped for a symlink instead of reading through it", () => {
    const nest = join(home, "nest", "deep");
    mkdirSync(join(nest, ".pi"), { recursive: true });
    writeJson(join(nest, ".pi", "mcp.json"), { mcpServers: { own: { command: "x" } } });
    const outside = join(home, "outside-ancestor", "deep");
    mkdirSync(join(outside, ".pi"), { recursive: true });
    writeJson(join(outside, ".pi", "mcp.json"), { mcpServers: { leaked: { command: "y" } } });
    renameSync(join(home, "nest"), join(home, "nest-old"));
    symlinkSync(join(home, "outside-ancestor"), join(home, "nest"));

    const result = listMcpServers(ctx({ projectRoot: join(home, "nest", "deep") }));
    expect(result.groups.project).toEqual([]);
    expect(result.groupErrors.project).toMatch(/root/i);
  });

  it("never scans project migration sources behind a rejected .pi path", () => {
    const outside = join(home, "outside-pi");
    mkdirSync(outside, { recursive: true });
    writeJson(join(outside, "mcp.json"), { mcpServers: {} });
    writeJson(join(outside, "mcp-adapter.json"), {
      mcpServers: { external_only: { command: "x" } },
    });
    symlinkSync(outside, join(projectDir, ".pi"));

    const result = listMcpServers(ctx());
    expect(result.groups.project).toEqual([]);
    expect(result.groupErrors.project).toMatch(/symlink/i);
    expect(result.migrations.some((m) => m.id === "adapterProject")).toBe(false);
  });

  it("treats an unreadable migration target as unknown, not as empty", () => {
    writeJson(adapterGlobal(agentDir), { mcpServers: { zread: { command: "x" } } });
    // A directory read fails with EISDIR, which must not read as ENOENT.
    mkdirSync(userFile(agentDir), { recursive: true });
    const result = listMcpServers(ctx());
    expect(result.migrations.some((m) => m.id === "adapterGlobal")).toBe(false);
  });
});

describe("importGlobalMcpOverrides", () => {
  it("snapshots every valid global server, including disabled ones, without credentials", async () => {
    writeJson(userFile(agentDir), {
      autoEnableCodemode: false,
      mcpServers: {
        docs: { url: "https://example.test/mcp", headers: { Authorization: "secret" } },
        paused: { command: "run", enabled: false, exposure: "direct" },
        mapped: { command: "m", toolExposure: { "delete_*": "hidden" } },
      },
    });

    const first = await importGlobalMcpOverrides(ctx());
    expect(first.imported).toEqual(["docs", "paused", "mapped"]);
    expect(first.skipped).toEqual([]);
    expect(first.changed).toBe(true);
    expect(first.path).toBe(projectFile(projectDir));
    expect(first.revision).toMatch(/^[0-9a-f]{64}$/);

    const doc = readJson(projectFile(projectDir));
    expect(doc).toEqual({
      mcpServers: {
        docs: { enabled: true, exposure: "codemode", toolExposure: {} },
        paused: { enabled: false, exposure: "direct", toolExposure: {} },
        mapped: { enabled: true, exposure: "codemode", toolExposure: { "delete_*": "hidden" } },
      },
    });
    const bytes = readFileSync(projectFile(projectDir), "utf8");
    expect(bytes).not.toContain("secret");
    expect(bytes).not.toContain("autoEnableCodemode");
    const globalBytes = readFileSync(userFile(agentDir), "utf8");

    const second = await importGlobalMcpOverrides(ctx());
    expect(second).toMatchObject({ changed: false, imported: [] });
    expect(second.skipped.map((s) => s.reason)).toEqual(["existing", "existing", "existing"]);
    expect(readFileSync(projectFile(projectDir), "utf8")).toBe(bytes);
    expect(readFileSync(userFile(agentDir), "utf8")).toBe(globalBytes);
  });

  it("skips existing names verbatim and reports namespace collisions without renaming", async () => {
    writeJson(userFile(agentDir), {
      mcpServers: {
        docs: { command: "d" },
        "dev-tools": { command: "g" },
        broken: { command: ["npx", "-y"] },
      },
    });
    writeJson(projectFile(projectDir), {
      mcpServers: {
        docs: { command: "repo-docs", args: ["--x"] },
        dev_tools: { enabled: false },
      },
    });

    const result = await importGlobalMcpOverrides(ctx());
    expect(result.imported).toEqual([]);
    expect(result.changed).toBe(false);
    expect(result.skipped).toEqual([
      { name: "docs", reason: "existing" },
      {
        name: "dev-tools",
        reason: "namespace-conflict",
        conflictWith: "dev_tools",
        detail: "collides with existing project entry dev_tools",
      },
      { name: "broken", reason: "invalid-global", detail: expect.stringContaining("command") },
    ]);
    expect(readJson(projectFile(projectDir)).mcpServers.docs).toEqual({
      command: "repo-docs",
      args: ["--x"],
    });
  });

  it("keeps unrelated project content and writes once for a partial import", async () => {
    writeJson(userFile(agentDir), { mcpServers: { a: { command: "a" }, b: { command: "b" } } });
    writeJson(projectFile(projectDir), { other: 1, mcpServers: { a: { enabled: false } } });
    const result = await importGlobalMcpOverrides(ctx());
    expect(result.imported).toEqual(["b"]);
    expect(readJson(projectFile(projectDir))).toEqual({
      other: 1,
      mcpServers: {
        a: { enabled: false },
        b: { enabled: true, exposure: "codemode", toolExposure: {} },
      },
    });
  });

  it("refuses the whole batch when either native file is broken", async () => {
    writeFileSync(userFile(agentDir), "{ not json", "utf8");
    await expect(importGlobalMcpOverrides(ctx())).rejects.toThrow();
    expect(existsSync(projectFile(projectDir))).toBe(false);

    writeJson(userFile(agentDir), { mcpServers: { a: { command: "a" } } });
    mkdirSync(join(projectDir, ".pi"), { recursive: true });
    writeFileSync(projectFile(projectDir), "[]", "utf8");
    await expect(importGlobalMcpOverrides(ctx())).rejects.toThrow(/object|mcpServers/);
    expect(readFileSync(projectFile(projectDir), "utf8")).toBe("[]");
  });

  it("rejects import without a trusted project root and before creating directories", async () => {
    writeJson(userFile(agentDir), { mcpServers: { a: { command: "a" } } });
    await expect(importGlobalMcpOverrides(ctx({ projectTrusted: false }))).rejects.toThrow(
      /trust/i,
    );
    await expect(importGlobalMcpOverrides(ctx(noProject))).rejects.toThrow(/project/i);
    expect(existsSync(join(projectDir, ".pi"))).toBe(false);
  });

  it("does not write when no server is missing", async () => {
    writeJson(userFile(agentDir), { mcpServers: {} });
    const result = await importGlobalMcpOverrides(ctx());
    expect(result).toMatchObject({ imported: [], skipped: [], changed: false });
    expect(existsSync(projectFile(projectDir))).toBe(false);
  });
});

describe("listMcpServers metadata", () => {
  it("classifies project definitions, overrides and invalid entries with effective values", () => {
    writeJson(userFile(agentDir), {
      mcpServers: {
        docs: { url: "https://example.test/mcp", toolExposure: { "delete_*": "hidden" } },
        paused: { command: "p", enabled: false },
        broken: { command: ["npx"] },
      },
    });
    writeJson(projectFile(projectDir), {
      mcpServers: {
        repo: { command: "run" },
        docs: { exposure: "hidden" },
        orphan: { enabled: false },
        "not an object": 5,
      },
    });

    const result = listMcpServers(ctx());
    expect(result.projectAvailable).toBe(true);
    expect(result.projectTrusted).toBe(true);

    const kinds = Object.fromEntries(result.groups.project.map((e) => [e.name, e.kind]));
    expect(kinds).toEqual({
      repo: "definition",
      docs: "override",
      orphan: "invalid",
      "not an object": "invalid",
    });

    const docs = result.groups.project.find((e) => e.name === "docs");
    expect(docs?.effective).toEqual({
      enabled: true,
      exposure: "hidden",
      toolExposure: { "delete_*": "hidden" },
    });
    expect(docs?.identity).toEqual({
      scope: "global",
      source: userFile(agentDir),
      override: projectFile(projectDir),
    });
    expect(docs?.revision).toBe(result.revisions.project);

    const orphan = result.groups.project.find((e) => e.name === "orphan");
    expect(orphan?.effective).toBeUndefined();
    expect(orphan?.validationError).toMatch(/global|base/i);
    expect(orphan?.enabled).toBe(false);

    const globalBroken = result.groups.piGlobal.find((e) => e.name === "broken");
    expect(globalBroken?.kind).toBe("definition");
    expect(globalBroken?.validationError).toMatch(/command/);
    expect(result.groupErrors.piGlobal).toBeUndefined();
  });

  it("reports revisions and never creates the project file", () => {
    const result = listMcpServers(ctx());
    expect(result.revisions.piGlobal).toBe("missing");
    expect(result.revisions.project).toBe("missing");
    expect(existsSync(projectFile(projectDir))).toBe(false);
  });

  it("does not read project contents without trust but still exposes the tab", () => {
    writeJson(projectFile(projectDir), { mcpServers: { repo: { command: "run" } } });
    const result = listMcpServers(ctx({ projectTrusted: false }));
    expect(result.projectAvailable).toBe(true);
    expect(result.projectTrusted).toBe(false);
    expect(result.groups.project).toEqual([]);
    expect(result.revisions.project).toBeNull();
    expect(result.groupErrors.project).toMatch(/trust/i);
  });

  it("reports non-record entries as invalid rows instead of crashing the inventory", () => {
    writeJson(userFile(agentDir), {
      mcpServers: { good: { command: "g" }, nulled: null, listed: [], numbered: 5 },
    });
    writeJson(projectFile(projectDir), { mcpServers: { nulled: null, listed: ["x"] } });

    const result = listMcpServers(ctx());
    const kinds = Object.fromEntries(result.groups.piGlobal.map((e) => [e.name, e.kind]));
    expect(kinds).toEqual({
      good: "definition",
      nulled: "invalid",
      listed: "invalid",
      numbered: "invalid",
    });
    expect(result.groups.piGlobal.find((e) => e.name === "nulled")?.validationError).toMatch(
      /must be an object/,
    );
    expect(result.groups.project.map((e) => [e.name, e.kind])).toEqual([
      ["nulled", "invalid"],
      ["listed", "invalid"],
    ]);
  });

  it("reports an unreadable file instead of treating it as empty", () => {
    writeFileSync(userFile(agentDir), "{ not json", "utf8");
    const result = listMcpServers(ctx());
    expect(result.groupErrors.piGlobal).toBeTruthy();
    expect(result.revisions.piGlobal).toBe("missing");
  });
});

describe("migrateAdapterConfig", () => {
  it("merges only missing adapter-global entries with field mapping and lossy reporting", async () => {
    writeJson(userFile(agentDir), { mcpServers: { MiniMax: { command: "uvx" } } });
    writeJson(adapterGlobal(agentDir), {
      mcpServers: {
        MiniMax: { command: "old", directTools: true },
        zread: { url: "https://z", directTools: true },
        paused: { command: "p", disabled: true },
        weird: { command: "w", directTools: false, inheritEnv: true, lifecycle: "x" },
      },
    });

    const result = await migrateAdapterConfig({ target: "adapterGlobal" }, ctx());
    expect(result.migrated.sort()).toEqual(["paused", "weird", "zread"]);
    expect(result.skipped).toEqual(["MiniMax"]);
    expect(result.lossy).toEqual(["weird"]);

    const doc = readJson(userFile(agentDir));
    expect(doc.mcpServers.MiniMax).toEqual({ command: "uvx" }); // untouched
    expect(doc.mcpServers.zread).toEqual({ url: "https://z", exposure: "direct" });
    expect(doc.mcpServers.paused).toEqual({ command: "p", enabled: false });
    expect(doc.mcpServers.weird).toEqual({ command: "w" });
    // Source file always stays; migration is a copy, not a move.
    expect(readJson(adapterGlobal(agentDir)).mcpServers.zread).toBeDefined();
  });

  it("migrates the adapter project config into .pi/mcp.json", async () => {
    writeJson(adapterProject(projectDir), {
      mcpServers: { local: { command: "run", directTools: true } },
    });
    const result = await migrateAdapterConfig({ target: "adapterProject" }, ctx());
    expect(result.migrated).toEqual(["local"]);
    expect(readJson(projectFile(projectDir)).mcpServers.local).toEqual({
      command: "run",
      exposure: "direct",
    });
  });

  it("migrates shared-global layers (later-wins among them) into the user file", async () => {
    writeJson(sharedGlobal(home), {
      mcpServers: { grep: { url: "https://g1" }, first: { command: "a" } },
    });
    writeJson(agentsGlobal(home), { mcpServers: { grep: { url: "https://g2" } } });

    const result = await migrateAdapterConfig({ target: "sharedGlobal" }, ctx());
    expect(result.migrated.sort()).toEqual(["first", "grep"]);
    const doc = readJson(userFile(agentDir));
    expect(doc.mcpServers.grep).toEqual({ url: "https://g2" });
    expect(doc.mcpServers.first).toEqual({ command: "a" });
  });

  it("migrates the shared project .mcp.json into .pi/mcp.json", async () => {
    writeJson(sharedProject(projectDir), { mcpServers: { repoShared: { command: "run" } } });
    const result = await migrateAdapterConfig({ target: "sharedProject" }, ctx());
    expect(result.migrated).toEqual(["repoShared"]);
    expect(readJson(projectFile(projectDir)).mcpServers.repoShared).toEqual({ command: "run" });
  });

  it("is a no-op when nothing is missing", async () => {
    writeJson(adapterGlobal(agentDir), { mcpServers: { docs: { command: "x" } } });
    writeJson(userFile(agentDir), { mcpServers: { docs: { command: "x" } } });
    const result = await migrateAdapterConfig({ target: "adapterGlobal" }, ctx());
    expect(result).toEqual({ migrated: [], skipped: ["docs"], lossy: [] });
  });

  it("rejects unknown targets and untrusted project migration", async () => {
    await expect(migrateAdapterConfig({ target: "adapter" }, ctx())).rejects.toThrow(/target/);
    writeJson(adapterProject(projectDir), { mcpServers: { local: { command: "run" } } });
    await expect(
      migrateAdapterConfig({ target: "adapterProject" }, ctx({ projectTrusted: false })),
    ).rejects.toThrow(/trust|project/i);
    expect(existsSync(projectFile(projectDir))).toBe(false);
  });
});

describe("stripJsonComments", () => {
  it("strips comments and trailing commas string-aware", () => {
    const raw = `{
      // line comment
      "a": "http://x", /* block */
      "b": [1, 2,],
    }`;
    expect(JSON.parse(stripJsonComments(raw))).toEqual({ a: "http://x", b: [1, 2] });
  });
});

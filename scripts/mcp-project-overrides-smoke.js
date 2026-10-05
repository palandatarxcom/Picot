#!/usr/bin/env bun
// ABOUTME: Isolated embedded-Pi smoke for project MCP override import and toolExposure map semantics.
// ABOUTME: Runs entirely inside a caller-supplied scratch root: no real HOME, credentials, network or child processes.

import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import {
  deleteMcpServer,
  importGlobalMcpOverrides,
  listMcpServers,
  saveMcpServer,
} from "../extensions/mcp-settings.ts";

const USAGE = "usage: bun scripts/mcp-project-overrides-smoke.js --binary <pi> --temp-root <dir>";
const MIN_PI_VERSION = [1, 0, 1];
const CLI_TIMEOUT_MS = 120_000;

/** The only transport labels the host may expose; the raw URL/command never leaves Pi. */
export const SAFE_TRANSPORTS = ["http", "stdio"];
export const EXPOSURES = ["codemode", "direct", "deferred", "hidden"];

export function usageError(message) {
  return new Error(`${message}\n${USAGE}`);
}

export function parseSmokeArgs(argv) {
  let binary;
  let tempRoot;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--binary") binary = argv[++index];
    else if (arg === "--temp-root") tempRoot = argv[++index];
    else if (arg === "--help" || arg === "-h") throw usageError("help requested");
    else throw usageError(`unexpected argument: ${arg}`);
  }
  if (!binary || !isAbsolute(binary)) throw usageError("--binary must be an absolute path");
  if (!tempRoot || !isAbsolute(tempRoot)) throw usageError("--temp-root must be an absolute path");

  const binaryStat = statOrNull(binary);
  if (!binaryStat?.isFile()) throw usageError(`--binary is not a file: ${binary}`);
  if ((binaryStat.mode & 0o111) === 0) throw usageError(`--binary is not executable: ${binary}`);

  if (lstatSync(tempRoot).isSymbolicLink()) throw usageError("--temp-root must not be a symlink");
  const rootStat = statOrNull(tempRoot);
  if (!rootStat?.isDirectory()) throw usageError(`--temp-root is not a directory: ${tempRoot}`);
  const canonicalRoot = realpathSync(tempRoot);
  for (const protectedPath of protectedPaths()) {
    if (canonicalRoot === protectedPath || isAncestorOf(canonicalRoot, protectedPath)) {
      throw usageError(`--temp-root must not be the real ${protectedPath} or its ancestor`);
    }
  }
  if (readdirSync(canonicalRoot).length > 0) throw usageError("--temp-root must be empty");
  return { binary: realpathSync(binary), tempRoot: canonicalRoot };
}

function statOrNull(target) {
  try {
    return lstatSync(target);
  } catch {
    return null;
  }
}

function protectedPaths() {
  const home = resolve(homedir());
  return [home, join(home, ".pi", "agent"), join(home, ".pi", "tmp"), join(home, ".pi")];
}

function isAncestorOf(candidate, target) {
  return target.startsWith(`${candidate}${"/"}`) && candidate !== target;
}

/** `1.0.2` / `v1.0.2-1-g200387122` → [1, 0, 2]; null when unparseable. */
export function parsePiVersion(text) {
  const match = String(text ?? "")
    .trim()
    .match(/^v?(\d+)\.(\d+)\.(\d+)/);
  if (!match) return null;
  return [Number(match[1]), Number(match[2]), Number(match[3])];
}

export function versionAtLeast(version, floor) {
  for (let index = 0; index < floor.length; index += 1) {
    const left = version?.[index] ?? 0;
    if (left > floor[index]) return true;
    if (left < floor[index]) return false;
  }
  return true;
}

/**
 * Validate one `pi mcp list --json` run: exit 0 and exit 1 both carry a usable
 * report; anything else is a failure. Mirrors the host runner's parser so the
 * smoke cannot accept a shape the app rejects.
 */
export function parseCliReport(stdout, status) {
  if (status !== 0 && status !== 1) {
    throw new Error(`pi mcp list exited with code ${status}`);
  }
  let report;
  try {
    report = JSON.parse(stdout);
  } catch {
    throw new Error("pi mcp list returned invalid JSON");
  }
  if (!report || typeof report !== "object" || Array.isArray(report)) {
    throw new Error("pi mcp list report is not an object");
  }
  if (!Array.isArray(report.servers)) throw new Error("report needs a servers array");
  if (!Array.isArray(report.errors)) throw new Error("report needs an errors array");
  for (const row of report.servers) {
    if (!row || typeof row !== "object" || Array.isArray(row)) {
      throw new Error("report row is not an object");
    }
    if (typeof row.name !== "string" || typeof row.scope !== "string") {
      throw new Error("report row needs name and scope strings");
    }
    if (typeof row.source !== "string" || typeof row.state !== "string") {
      throw new Error("report row needs source and state strings");
    }
    if (typeof row.enabled !== "boolean") throw new Error("report row needs a boolean enabled");
    if (!EXPOSURES.includes(row.exposure)) throw new Error("report row exposure is not canonical");
    if (!Array.isArray(row.tools)) throw new Error("report row needs a tools array");
    if (row.toolExposure !== undefined) {
      if (!row.toolExposure || typeof row.toolExposure !== "object") {
        throw new Error("report row toolExposure must be an object");
      }
      for (const exposure of Object.values(row.toolExposure)) {
        if (!EXPOSURES.includes(exposure)) throw new Error("toolExposure value is not canonical");
      }
    }
  }
  return report;
}

/** Effective exposure of one discovered tool, per Pi's own report semantics. */
export function effectiveExposure(report, tool) {
  return report?.toolExposure?.[tool] ?? report?.exposure;
}

export function findRow(report, name) {
  return report.servers.find((row) => row.name === name) ?? null;
}

class Smoke {
  constructor() {
    this.failures = [];
    this.log = [];
  }

  ok(label) {
    this.log.push(`ok   ${label}`);
  }

  fail(label, detail) {
    this.failures.push(`${label}: ${detail}`);
    this.log.push(`FAIL ${label}: ${detail}`);
  }

  check(label, condition, detail) {
    if (condition) this.ok(label);
    else this.fail(label, detail);
  }

  equal(label, actual, expected) {
    const left = JSON.stringify(actual);
    const right = JSON.stringify(expected);
    this.check(label, left === right, `expected ${right}, got ${left}`);
  }
}

function writeJson(file, value) {
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

function readJson(file) {
  return JSON.parse(readFileSync(file, "utf8"));
}

function runCli(binary, cwd, env) {
  const result = spawnSync(binary, ["mcp", "list", "--json"], {
    cwd,
    env,
    encoding: "utf8",
    timeout: CLI_TIMEOUT_MS,
  });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

/** One CLI round trip parsed into a report; a malformed run is a smoke failure. */
function cliReport(smoke, label, callCli) {
  const result = callCli();
  try {
    const report = parseCliReport(result.stdout, result.status);
    return { report, exit: result.status };
  } catch (error) {
    smoke.fail(label, String(error?.message ?? error));
    return { report: null, exit: result.status };
  }
}

/** A deterministic stdio MCP server: JSON-RPC over stdin/stdout, no network, no children. */
export function fixtureSource() {
  return `import { createInterface } from "node:readline";
const tools = ["read_item", "delete_one", "delete_exact", "other"].map((name) => ({
  name, description: name, inputSchema: { type: "object", properties: {} },
}));
const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  let result;
  if (request.method === "initialize") {
    result = {
      protocolVersion: request.params.protocolVersion,
      capabilities: { tools: {} },
      serverInfo: { name: "picot-map-smoke", version: "1.0.0" },
    };
  } else if (request.method === "tools/list") result = { tools };
  else if (request.method === "ping") result = {};
  else {
    process.stdout.write(\`\${JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code: -32601, message: "Method not found" } })}\\n\`);
    continue;
  }
  process.stdout.write(\`\${JSON.stringify({ jsonrpc: "2.0", id: request.id, result })}\\n\`);
}
`;
}

function secretShaped(label) {
  return `secret-${label}-${randomUUID()}`;
}

async function runSmoke({ binary, tempRoot }) {
  const smoke = new Smoke();
  const scratch = mkdtempSync(join(tempRoot, "mcp-smoke-"));
  const home = join(scratch, "home");
  const agent = join(scratch, "agent");
  const project = join(scratch, "project");
  const fixturePath = join(scratch, "fixture.mjs");
  for (const dir of [home, agent, project]) mkdirSync(dir, { recursive: true });
  writeFileSync(fixturePath, fixtureSource(), "utf8");

  const env = {
    HOME: home,
    PI_CODING_AGENT_DIR: agent,
    PATH: process.env.PATH ?? "",
    TMPDIR: scratch,
  };
  const globalFile = join(agent, "mcp.json");
  const projectFile = join(project, ".pi", "mcp.json");
  const context = { agentDir: agent, projectRoot: project, projectTrusted: true };
  const callCli = () => runCli(binary, project, env);

  try {
    // Preflight: the resolved binary itself, never the pin.
    const version = spawnSync(binary, ["--version"], { encoding: "utf8", env });
    const parsedVersion = parsePiVersion(version.stdout);
    smoke.check(
      "binary version >= 1.0.1",
      parsedVersion !== null && versionAtLeast(parsedVersion, MIN_PI_VERSION),
      `got ${JSON.stringify(version.stdout.trim())}`,
    );
    if (smoke.failures.length > 0) return smoke;

    writeJson(join(agent, "trust.json"), { [realpathSync(project)]: true });

    // ── Disabled evidence phase ────────────────────────────────────────────
    const stdioSecret = secretShaped("env");
    const httpSecret = secretShaped("header");
    writeJson(globalFile, {
      mcpServers: {
        "unused-stdio": {
          command: "unused-mcp-command",
          env: { TOKEN: stdioSecret },
          enabled: false,
          exposure: "direct",
          toolExposure: { "delete_*": "hidden" },
        },
        "unused-http": {
          url: "https://example.test/mcp",
          headers: { Authorization: `Bearer ${httpSecret}` },
          enabled: false,
        },
      },
    });
    const globalBytes = readFileSync(globalFile, "utf8");

    const imported = await importGlobalMcpOverrides(context);
    smoke.equal("import adds both disabled servers", imported.imported, [
      "unused-stdio",
      "unused-http",
    ]);
    const projectDoc = readJson(projectFile);
    smoke.equal(
      "stdio snapshot is exactly three keys",
      Object.keys(projectDoc.mcpServers["unused-stdio"]).sort(),
      ["enabled", "exposure", "toolExposure"],
    );
    smoke.equal(
      "stdio snapshot keeps explicit disabled values",
      projectDoc.mcpServers["unused-stdio"],
      {
        enabled: false,
        exposure: "direct",
        toolExposure: { "delete_*": "hidden" },
      },
    );
    smoke.equal(
      "http snapshot materializes defaults around the disabled state",
      projectDoc.mcpServers["unused-http"],
      {
        enabled: false,
        exposure: "codemode",
        toolExposure: {},
      },
    );
    const projectBytes = readFileSync(projectFile, "utf8");
    smoke.check(
      "no connection or credential bytes copied",
      !projectBytes.includes("unused-mcp-command") &&
        !projectBytes.includes("example.test") &&
        !projectBytes.includes(stdioSecret) &&
        !projectBytes.includes(httpSecret),
      "project file leaked a connection or credential value",
    );
    smoke.check(
      "global file unchanged by import",
      readFileSync(globalFile, "utf8") === globalBytes,
      "global bytes changed",
    );
    const repeat = await importGlobalMcpOverrides(context);
    smoke.check(
      "repeat import is a no-write no-op",
      repeat.changed === false &&
        repeat.imported.length === 0 &&
        readFileSync(projectFile, "utf8") === projectBytes,
      "repeat import changed the project file",
    );

    const { report: disabled } = cliReport(smoke, "disabled phase list", callCli);
    smoke.check(
      "disabled report has no config errors",
      disabled?.errors.length === 0,
      JSON.stringify(disabled?.errors),
    );
    for (const name of ["unused-stdio", "unused-http"]) {
      const row = findRow(disabled, name);
      smoke.check(`${name} is reported disabled`, row?.state === "disabled", JSON.stringify(row));
      smoke.check(
        `${name} reports no tools`,
        Array.isArray(row?.tools) && row.tools.length === 0,
        JSON.stringify(row?.tools),
      );
      smoke.check(
        `${name} keeps global scope/source plus project override path`,
        row?.scope === "global" && row?.source === globalFile && row?.override === projectFile,
        JSON.stringify(row),
      );
      smoke.check(
        `${name} has no toolExposure in a disabled report`,
        row?.toolExposure === undefined,
        JSON.stringify(row?.toolExposure),
      );
      // The CLI reports the raw transport; the host runner projects it to a
      // safe `http`/`stdio` label before it reaches the WebView (asserted in
      // the Rust runner tests). Here the raw value proves the override
      // inherits the global connection instead of carrying its own.
      smoke.check(
        `${name} inherits the global transport`,
        typeof row?.transport === "string" && row.transport.length > 0,
        String(row?.transport),
      );
    }
    smoke.check(
      "disabled stdio keeps the snapshot exposure",
      findRow(disabled, "unused-stdio")?.exposure === "direct",
      String(findRow(disabled, "unused-stdio")?.exposure),
    );
    smoke.check(
      "disabled stdio inherits the global command",
      findRow(disabled, "unused-stdio")?.transport === "unused-mcp-command",
      String(findRow(disabled, "unused-stdio")?.transport),
    );
    smoke.check(
      "disabled http inherits the global url",
      findRow(disabled, "unused-http")?.transport === "https://example.test/mcp",
      String(findRow(disabled, "unused-http")?.transport),
    );

    // Base removal: the override stays, the report must flag it.
    await deleteMcpServer(
      {
        scope: "piGlobal",
        name: "unused-http",
        expectedRevision: listMcpServers(context).revisions.piGlobal,
      },
      context,
    );
    const { report: orphanReport, exit: orphanExit } = cliReport(
      smoke,
      "missing base list",
      callCli,
    );
    smoke.check("missing base exits 1", orphanExit === 1, `exit ${orphanExit}`);
    smoke.check(
      "missing base is reported as an error without a borrowed row",
      (orphanReport?.errors.length ?? 0) > 0 && findRow(orphanReport, "unused-http") === null,
      JSON.stringify({ errors: orphanReport?.errors.length ?? 0 }),
    );
    await deleteMcpServer(
      {
        scope: "project",
        name: "unused-http",
        expectedRevision: listMcpServers(context).revisions.project,
      },
      context,
    );
    await deleteMcpServer(
      {
        scope: "project",
        name: "unused-stdio",
        expectedRevision: listMcpServers(context).revisions.project,
      },
      context,
    );

    // ── Active map evidence phase ──────────────────────────────────────────
    writeJson(globalFile, {
      mcpServers: {
        "map-test": {
          command: process.execPath,
          args: [fixturePath],
          exposure: "codemode",
          toolExposure: { "delete_*": "hidden", read_item: "direct" },
        },
      },
    });
    const { report: baselineReport, exit: baselineExit } = cliReport(
      smoke,
      "baseline list",
      callCli,
    );
    smoke.check(
      "baseline (no override) exits 0 with no errors",
      baselineExit === 0,
      `exit ${baselineExit}`,
    );
    smoke.check(
      "baseline report has no errors",
      baselineReport?.errors.length === 0,
      JSON.stringify(baselineReport?.errors),
    );
    smoke.check(
      "baseline connects and lists four fixture tools",
      findRow(baselineReport, "map-test")?.state === "connected" &&
        findRow(baselineReport, "map-test")?.tools.length === 4,
      JSON.stringify(findRow(baselineReport, "map-test")),
    );

    const mapImport = await importGlobalMcpOverrides(context);
    smoke.equal(
      "map-test import snapshots the global map",
      readJson(projectFile).mcpServers["map-test"],
      {
        enabled: true,
        exposure: "codemode",
        toolExposure: { "delete_*": "hidden", read_item: "direct" },
      },
    );
    smoke.equal("map-test import reports one added server", mapImport.imported, ["map-test"]);

    const { report: initial } = cliReport(smoke, "initial override list", callCli);
    smoke.equal(
      "initial map exposes read_item directly",
      effectiveExposure(findRow(initial, "map-test"), "read_item"),
      "direct",
    );
    smoke.equal(
      "initial map hides delete_one",
      effectiveExposure(findRow(initial, "map-test"), "delete_one"),
      "hidden",
    );
    smoke.equal(
      "initial map hides delete_exact",
      effectiveExposure(findRow(initial, "map-test"), "delete_exact"),
      "hidden",
    );
    smoke.equal(
      "initial map leaves other at codemode",
      effectiveExposure(findRow(initial, "map-test"), "other"),
      "codemode",
    );

    // Global changes must not follow the snapshot (no synchronization).
    writeJson(globalFile, {
      mcpServers: {
        "map-test": {
          command: process.execPath,
          args: [fixturePath],
          exposure: "deferred",
          toolExposure: { other: "direct" },
        },
      },
    });
    const { report: snapshotted } = cliReport(smoke, "snapshot list", callCli);
    smoke.equal(
      "snapshot ignores a changed global map (read_item)",
      effectiveExposure(findRow(snapshotted, "map-test"), "read_item"),
      "direct",
    );
    smoke.equal(
      "snapshot ignores a changed global map (other)",
      effectiveExposure(findRow(snapshotted, "map-test"), "other"),
      "codemode",
    );
    smoke.equal(
      "snapshot keeps the project map on disk",
      readJson(projectFile).mcpServers["map-test"].toolExposure,
      { "delete_*": "hidden", read_item: "direct" },
    );

    const saveOverrideMap = async (map) => {
      const listed = listMcpServers(context);
      const result = await saveMcpServer(
        {
          scope: "project",
          name: "map-test",
          kind: "override",
          intent: "edit",
          entry: { enabled: true, exposure: "codemode", toolExposure: map },
          expectedRevision: listed.revisions.project,
          expectedGlobalRevision: listed.revisions.piGlobal,
        },
        context,
      );
      return result;
    };

    await saveOverrideMap({ "delete_*": "hidden", "*": "deferred", delete_exact: "direct" });
    const { report: replaced } = cliReport(smoke, "whole-map list", callCli);
    const replacedRow = findRow(replaced, "map-test");
    smoke.equal(
      "whole-map replacement sends read_item to deferred",
      effectiveExposure(replacedRow, "read_item"),
      "deferred",
    );
    smoke.equal(
      "whole-map replacement hides delete_one",
      effectiveExposure(replacedRow, "delete_one"),
      "hidden",
    );
    smoke.equal(
      "exact name beats a wildcard despite being last",
      effectiveExposure(replacedRow, "delete_exact"),
      "direct",
    );
    smoke.equal(
      "wildcard sends other to deferred",
      effectiveExposure(replacedRow, "other"),
      "deferred",
    );

    await saveOverrideMap({ "*": "deferred", "delete_*": "hidden", delete_exact: "direct" });
    const { report: reordered } = cliReport(smoke, "reordered map list", callCli);
    smoke.equal(
      "first wildcard wins when patterns are reordered",
      effectiveExposure(findRow(reordered, "map-test"), "delete_one"),
      "deferred",
    );
    smoke.equal(
      "exact name still wins after reordering",
      effectiveExposure(findRow(reordered, "map-test"), "delete_exact"),
      "direct",
    );

    await saveOverrideMap({});
    const { report: cleared } = cliReport(smoke, "empty map list", callCli);
    const clearedRow = findRow(cleared, "map-test");
    for (const tool of ["read_item", "delete_one", "delete_exact", "other"]) {
      smoke.equal(
        `empty map returns ${tool} to the server exposure`,
        effectiveExposure(clearedRow, tool),
        "codemode",
      );
    }
    smoke.check(
      "empty map leaves no effective toolExposure entry",
      clearedRow?.toolExposure === undefined,
      JSON.stringify(clearedRow?.toolExposure),
    );
    smoke.equal(
      "empty map is stored as an explicit empty object",
      readJson(projectFile).mcpServers["map-test"].toolExposure,
      {},
    );

    // Removing the override restores the global (deferred + other:direct).
    const listed = listMcpServers(context);
    await deleteMcpServer(
      { scope: "project", name: "map-test", expectedRevision: listed.revisions.project },
      context,
    );
    smoke.check(
      "override removal deletes only the project entry",
      readJson(projectFile).mcpServers["map-test"] === undefined,
      "project entry survived removal",
    );
    const { report: restored } = cliReport(smoke, "restored global list", callCli);
    const restoredRow = findRow(restored, "map-test");
    smoke.check(
      "removed override reports the global identity",
      restoredRow?.override === undefined,
      String(restoredRow?.override),
    );
    smoke.equal("removal restores the global exposure", restoredRow?.exposure, "deferred");
    smoke.equal(
      "removal restores the global map",
      effectiveExposure(restoredRow, "other"),
      "direct",
    );
    smoke.equal(
      "removal restores global map for the other tools",
      effectiveExposure(restoredRow, "read_item"),
      "deferred",
    );

    return smoke;
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

async function main() {
  let options;
  try {
    options = parseSmokeArgs(process.argv.slice(2));
  } catch (error) {
    console.error(String(error?.message ?? error));
    process.exit(2);
  }
  const smoke = await runSmoke(options);
  for (const line of smoke.log) console.log(line);
  if (smoke.failures.length > 0) {
    console.error(`\n${smoke.failures.length} smoke assertion(s) failed`);
    process.exit(1);
  }
  console.log(`\nall ${smoke.log.length} smoke assertions passed`);
}

if (import.meta.main) {
  await main();
}

export { runSmoke };

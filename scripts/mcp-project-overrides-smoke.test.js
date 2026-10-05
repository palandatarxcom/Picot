// @vitest-environment node

// ABOUTME: Contract tests for the isolated embedded-Pi MCP override smoke script.
// ABOUTME: Guards the scratch-root safety rules, the real CLI envelope shape and the map helpers.

import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  effectiveExposure,
  findRow,
  fixtureSource,
  parseCliReport,
  parsePiVersion,
  parseSmokeArgs,
  versionAtLeast,
} from "./mcp-project-overrides-smoke.js";

const tempDirs = [];

function scratch(prefix = "mcp-smoke-test-") {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  tempDirs.push(dir);
  return dir;
}

function fakeBinary(dir) {
  const path = join(dir, "pi");
  writeFileSync(path, "#!/bin/sh\nexit 0\n", { mode: 0o700 });
  return path;
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("parseSmokeArgs", () => {
  it("requires an absolute executable binary and an absolute empty temp root", () => {
    const root = scratch();
    const binary = fakeBinary(root);
    const tempRoot = join(root, "temp-root");
    mkdirSync(tempRoot);

    expect(parseSmokeArgs(["--binary", binary, "--temp-root", tempRoot])).toEqual({
      binary: realpathSync(binary),
      tempRoot: realpathSync(tempRoot),
    });
    expect(() => parseSmokeArgs(["--temp-root", tempRoot])).toThrow(/--binary/);
    expect(() => parseSmokeArgs(["--binary", "pi", "--temp-root", tempRoot])).toThrow(/absolute/);
    expect(() => parseSmokeArgs(["--binary", binary, "--temp-root", "tmp"])).toThrow(/absolute/);
    expect(() => parseSmokeArgs(["--binary", binary])).toThrow(/--temp-root/);
    expect(() => parseSmokeArgs(["--binary", binary, "--temp-root", tempRoot, "--x"])).toThrow(
      /unexpected argument/,
    );
  });

  it("rejects a non-executable binary, a non-empty root and a symlinked root", () => {
    const root = scratch();
    const binary = fakeBinary(root);
    const plain = join(root, "not-executable");
    writeFileSync(plain, "text", { mode: 0o600 });
    const tempRoot = join(root, "temp-root");
    mkdirSync(tempRoot);

    expect(() => parseSmokeArgs(["--binary", plain, "--temp-root", tempRoot])).toThrow(
      /not executable/,
    );

    writeFileSync(join(tempRoot, "stray"), "x");
    expect(() => parseSmokeArgs(["--binary", binary, "--temp-root", tempRoot])).toThrow(/empty/);

    const emptyRoot = join(root, "empty-root");
    mkdirSync(emptyRoot);
    const link = join(root, "link-root");
    symlinkSync(emptyRoot, link);
    expect(() => parseSmokeArgs(["--binary", binary, "--temp-root", link])).toThrow(/symlink/);
  });

  it("refuses the real home, agent dir or their ancestors", () => {
    const root = scratch();
    const binary = fakeBinary(root);
    expect(() => parseSmokeArgs(["--binary", binary, "--temp-root", resolve(homedir())])).toThrow(
      /real/,
    );
    expect(() =>
      parseSmokeArgs(["--binary", binary, "--temp-root", join(homedir(), ".pi", "agent")]),
    ).toThrow();
  });
});

describe("version preflight", () => {
  it("parses release and dev versions and compares against the floor", () => {
    expect(parsePiVersion("1.0.2\n")).toEqual([1, 0, 2]);
    expect(parsePiVersion("v1.0.2-1-g200387122")).toEqual([1, 0, 2]);
    expect(parsePiVersion("not a version")).toBeNull();
    expect(versionAtLeast([1, 0, 2], [1, 0, 1])).toBe(true);
    expect(versionAtLeast([1, 0, 1], [1, 0, 1])).toBe(true);
    expect(versionAtLeast([1, 0, 0], [1, 0, 1])).toBe(false);
    expect(versionAtLeast([0, 99, 9], [1, 0, 1])).toBe(false);
  });
});

describe("parseCliReport", () => {
  const row = {
    name: "docs",
    scope: "global",
    source: "/agent/mcp.json",
    enabled: true,
    exposure: "codemode",
    transport: "npx -y docs",
    state: "connected",
    tools: ["read"],
  };

  it("accepts exit 0 and exit 1 envelopes", () => {
    const envelope = JSON.stringify({ servers: [row], errors: [] });
    expect(parseCliReport(envelope, 0).servers).toHaveLength(1);
    expect(
      parseCliReport(JSON.stringify({ servers: [], errors: ["bad config"] }), 1).errors,
    ).toEqual(["bad config"]);
  });

  it("rejects arrays, missing fields, foreign exits and malformed rows", () => {
    expect(() => parseCliReport("[]", 0)).toThrow(/not an object/);
    expect(() => parseCliReport('{"servers":[]}', 0)).toThrow(/errors array/);
    expect(() => parseCliReport('{"servers":[],"errors":[]}', 2)).toThrow(/exited with code 2/);
    expect(() => parseCliReport("not json", 0)).toThrow(/invalid JSON/);
    expect(() => parseCliReport('{"servers":[{"name":1}],"errors":[]}', 0)).toThrow(/scope/);
    expect(() =>
      parseCliReport(
        JSON.stringify({ servers: [{ ...row, exposure: "codemode-deferred" }], errors: [] }),
        0,
      ),
    ).toThrow(/exposure/);
    expect(() =>
      parseCliReport(
        JSON.stringify({ servers: [{ ...row, toolExposure: { a: "loud" } }], errors: [] }),
        0,
      ),
    ).toThrow(/toolExposure/);
  });

  it("never derives a tool map from a disabled row", () => {
    const disabled = { ...row, enabled: false, state: "disabled", tools: [] };
    const report = parseCliReport(JSON.stringify({ servers: [disabled], errors: [] }), 0);
    expect(report.servers[0].toolExposure).toBeUndefined();
    // Effective exposure falls back to the server exposure, never a fabricated map.
    expect(effectiveExposure(report.servers[0], "anything")).toBe("codemode");
    expect(findRow(report, "docs")?.state).toBe("disabled");
    expect(findRow(report, "missing")).toBeNull();
  });
});

describe("fixtureSource", () => {
  it("is a deterministic stdio JSON-RPC server with no network or child processes", () => {
    const source = fixtureSource();
    expect(source).toContain("node:readline");
    expect(source).toContain("tools/list");
    expect(source).toContain("picot-map-smoke");
    for (const forbidden of ["fetch(", "http.request", "spawn", "net.connect", "WebSocket"]) {
      expect(source).not.toContain(forbidden);
    }
    expect(fixtureSource()).toBe(source);
  });
});

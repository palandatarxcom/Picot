// ABOUTME: Checks the test runner against sacrificial files outside its writable root.
// ABOUTME: Verifies inherited configuration paths and symlink writes cannot escape.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const fixture = mkdtempSync(join(tmpdir(), "picot-sandbox-check-"));
try {
  const target = join(fixture, "models.json");
  writeFileSync(target, "protected fixture");
  const attack = `
    const assert = require("node:assert/strict");
    const fs = require("node:fs");
    const cp = require("node:child_process");
    const path = require("node:path");
    const target = process.argv[1];
    assert.notEqual(process.env.PI_CODING_AGENT_DIR, path.dirname(target));
    assert.ok(process.env.HOME.startsWith(process.env.PICOT_TEST_SANDBOX_ROOT + "/"));
    const local = path.join(process.env.HOME, "allowed.json");
    fs.writeFileSync(local, "allowed");
    assert.equal(fs.readFileSync(local, "utf8"), "allowed");
    const extensionProbe = path.resolve("extensions/dist/.sandbox-write-probe");
    assert.throws(() => fs.writeFileSync(extensionProbe, "denied"), /EPERM|EACCES/);
    for (const action of [
      () => fs.writeFileSync(target, "damaged"),
      () => fs.unlinkSync(target),
      () => fs.renameSync(target, local + ".moved"),
      () => fs.chmodSync(target, 0o600),
    ]) assert.throws(action, /EPERM|EACCES/);
    const link = path.join(process.env.HOME, "models.json");
    fs.symlinkSync(target, link);
    assert.throws(() => fs.writeFileSync(link, "damaged"), /EPERM|EACCES/);
    const child = cp.spawnSync(process.execPath, ["-e",
      'try { require("node:fs").writeFileSync(process.argv[1], "child damage"); process.exit(10); } catch(e) { process.exit(["EPERM", "EACCES"].includes(e.code) ? 0 : 11); }', target]);
    assert.equal(child.status, 0);
    assert.equal(fs.readFileSync(target, "utf8"), "protected fixture");
  `;
  const result = spawnSync(
    process.execPath,
    [resolve("scripts/test-sandbox.js"), "--command", process.execPath, "-e", attack, target],
    {
      env: { ...process.env, HOME: fixture, PI_CODING_AGENT_DIR: fixture },
      encoding: "utf8",
    },
  );
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.equal(readFileSync(target, "utf8"), "protected fixture");
  const direct = spawnSync(process.execPath, ["-e", 'import("./vitest.config.js")'], {
    env: { ...process.env, PICOT_TEST_SANDBOX_ROOT: "", PICOT_TEST_WRITE_GUARD: "" },
    encoding: "utf8",
  });
  assert.notEqual(direct.status, 0);
  assert.match(direct.stderr, /direct Vitest is unsafe/);
  const bare = spawnSync(process.execPath, ["-e", 'import("./vitest.config.js")'], {
    env: { ...process.env, PICOT_TEST_SANDBOX_ROOT: fixture, PICOT_TEST_WRITE_GUARD: target },
    encoding: "utf8",
  });
  assert.notEqual(bare.status, 0);
  assert.match(bare.stderr, /refusing to run without kernel isolation/);
  if (process.argv.includes("--build-extensions")) {
    const build = spawnSync(
      process.execPath,
      [resolve("scripts/test-sandbox.js"), "--command", "bun", "run", "build:extensions"],
      { encoding: "utf8" },
    );
    assert.equal(build.status, 0, build.stderr || build.stdout);
    assert.ok(
      readFileSync("extensions/dist/picot-bridge.mjs", "utf8").includes(
        "function resolveMcpProjectRoot",
      ),
    );
  }
  console.log(
    "PASS: isolated roots; direct, delete, rename, chmod, symlink and child writes denied.",
  );
} finally {
  rmSync(fixture, { recursive: true, force: true });
}

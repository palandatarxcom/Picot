// ABOUTME: Verifies the daemon cleanup helper's safety rules on macOS: it never signals a
// ABOUTME: reused pid, never prints the instance's admin token, and only deletes dead files.

import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const SCRIPT = resolve("scripts/cleanup-picot-daemon.sh");
const isMac = process.platform === "darwin";
const describeMac = isMac ? describe : describe.skip;

let dirs = [];

function fixture(instance) {
  const dir = mkdtempSync(join(tmpdir(), "picot-cleanup-"));
  dirs.push(dir);
  if (instance) {
    writeFileSync(join(dir, "picot-daemon.json"), JSON.stringify(instance));
    writeFileSync(join(dir, "picot-daemon.lock"), "");
  }
  writeFileSync(join(dir, "picot-daemon.log"), "log stays\n");
  return dir;
}

function run(dir, ...args) {
  const result = spawnSync("bash", [SCRIPT, "--data-dir", dir, ...args], { encoding: "utf8" });
  return {
    status: result.status,
    output: `${result.stdout}${result.stderr}`,
  };
}

const token = "admin-token-must-never-be-printed";
const document = (pid) => ({
  pid,
  host_origin: "http://127.0.0.1:1",
  admin_origin: "http://127.0.0.1:2",
  admin_token: token,
  version: "0.5.0",
  build_id: "test",
  started_at: "2026-10-22T00:00:00Z",
});

afterEach(() => {
  for (const dir of dirs) rmSync(dir, { recursive: true, force: true });
  dirs = [];
});

describeMac("cleanup-picot-daemon.sh", () => {
  it("reports nothing to clean when no daemon was ever recorded", () => {
    const dir = fixture(null);
    const { status, output } = run(dir, "--yes");
    expect(status).toBe(0);
    expect(output).toContain("Nothing to clean up");
    expect(existsSync(join(dir, "picot-daemon.json"))).toBe(false);
  });

  it("clears a stale instance and lock but keeps the log, without printing the token", () => {
    const dir = fixture(document(999999));
    const { status, output } = run(dir, "--yes");
    expect(status).toBe(0);
    expect(output).toContain("stale");
    expect(output).not.toContain(token);
    expect(existsSync(join(dir, "picot-daemon.json"))).toBe(false);
    expect(existsSync(join(dir, "picot-daemon.lock"))).toBe(false);
    expect(readFileSync(join(dir, "picot-daemon.log"), "utf8")).toContain("log stays");
  });

  it("refuses to signal a live pid that is not picot-daemon, and keeps the files", () => {
    const dir = fixture(document(process.pid));
    const { status, output } = run(dir, "--yes");
    expect(status).toBe(1);
    expect(output).toContain("not picot-daemon");
    expect(output).not.toContain(token);
    expect(existsSync(join(dir, "picot-daemon.json"))).toBe(true);
    expect(existsSync(join(dir, "picot-daemon.lock"))).toBe(true);
  });

  it("refuses the same mismatched pid under --dry-run instead of pretending it is safe", () => {
    const dir = fixture(document(process.pid));
    const { status, output } = run(dir, "--dry-run");
    expect(status).toBe(1);
    expect(output).toContain("not picot-daemon");
    expect(existsSync(join(dir, "picot-daemon.json"))).toBe(true);
  });

  it("changes nothing under --dry-run for a stale instance", () => {
    const dir = fixture(document(999999));
    const { status, output } = run(dir, "--dry-run");
    expect(status).toBe(0);
    expect(output).toContain("Would remove");
    expect(existsSync(join(dir, "picot-daemon.json"))).toBe(true);
    expect(existsSync(join(dir, "picot-daemon.lock"))).toBe(true);
  });
});

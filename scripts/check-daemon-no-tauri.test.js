// @vitest-environment node

// ABOUTME: Pins the daemon Tauri guard's lock-graph semantics on synthetic graphs.
// ABOUTME: Proves the Windows-only exception is recomputed each run and cannot hide the framework.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { isFrameworkTauri, lockGraphVerdicts, parseLock } from "./check-daemon-no-tauri.js";

/** Minimal Cargo.lock text in the real v4 shape (name/version/dependencies array). */
function lockText(blocks) {
  return blocks
    .map(([name, dependencies = []]) => {
      const lines = ["[[package]]", `name = "${name}"`, 'version = "1.0.0"'];
      if (dependencies.length) {
        lines.push(
          "dependencies = [",
          ...dependencies.map((dependency) => ` "${dependency}",`),
          "]",
        );
      }
      return lines.join("\n");
    })
    .join("\n\n");
}

function verdictsFor(blocks, crate) {
  return lockGraphVerdicts(parseLock(lockText(blocks)), crate);
}

function failures(verdicts) {
  return verdicts.filter((verdict) => verdict.level === "fail");
}

function passes(verdicts) {
  return verdicts.filter((verdict) => verdict.level === "pass");
}

const WINDOWS_EXCEPTION = "tauri-winrt-notification";

describe("daemon Tauri guard: name classification", () => {
  it("treats every tauri-named package as framework except the allowlisted Windows backend", () => {
    expect(isFrameworkTauri("tauri")).toBe(true);
    expect(isFrameworkTauri("tauri-utils")).toBe(true);
    expect(isFrameworkTauri("tauri-plugin-shell")).toBe(true);
    expect(isFrameworkTauri("notify-rust")).toBe(false);
    expect(isFrameworkTauri("tauria")).toBe(false);
    expect(isFrameworkTauri(WINDOWS_EXCEPTION)).toBe(false);
  });
});

describe("daemon Tauri guard: lock graph", () => {
  it("tolerates the notify-rust Windows backend while it stays framework-free", () => {
    const verdicts = verdictsFor(
      [
        ["picot-host", ["notify-rust"]],
        ["notify-rust", [WINDOWS_EXCEPTION, "mac-notification-sys"]],
        [WINDOWS_EXCEPTION, ["thiserror", "windows"]],
        ["thiserror", []],
        ["windows", []],
        ["mac-notification-sys", []],
      ],
      "picot-host",
    );

    expect(failures(verdicts)).toEqual([]);
    expect(passes(verdicts).map((verdict) => verdict.message)).toEqual([
      expect.stringContaining(WINDOWS_EXCEPTION),
    ]);
  });

  it("goes red when the allowlisted crate starts pulling the framework in", () => {
    // The regression the supervisor's requirement pins: a notify-rust upgrade
    // (or a maintained fork) that reaches the framework must fail the guard, so
    // the exception can never decay into a static snapshot.
    const verdicts = verdictsFor(
      [
        ["picot-host", ["notify-rust"]],
        ["notify-rust", [WINDOWS_EXCEPTION]],
        [WINDOWS_EXCEPTION, ["windows", "tauri"]],
        ["windows", []],
        ["tauri", []],
      ],
      "picot-host",
    );

    // Both findings are correct: the nested framework path, and the framework
    // name itself being reachable.
    expect(failures(verdicts).map((verdict) => verdict.message)).toContainEqual(
      expect.stringContaining(
        `allowlisted non-framework ${WINDOWS_EXCEPTION} reaches the Tauri framework: tauri`,
      ),
    );
  });

  it("goes red when the allowlisted crate vanishes from the lock", () => {
    const verdicts = verdictsFor(
      [
        ["picot-host", [WINDOWS_EXCEPTION]],
        ["windows", []],
      ],
      "picot-host",
    );

    expect(failures(verdicts)).toHaveLength(1);
    expect(failures(verdicts)[0].message).toContain("stale allowlist");
  });

  it("still fails on any other tauri-named package anywhere in the closure", () => {
    const verdicts = verdictsFor(
      [
        ["picot-host", ["axum"]],
        ["axum", ["tauri-utils"]],
        ["tauri-utils", []],
      ],
      "picot-host",
    );

    expect(failures(verdicts).map((verdict) => verdict.message)).toEqual([
      "picot-host reaches tauri in the resolved graph: tauri-utils",
    ]);
  });

  it("fails when the crate itself is missing from the lock", () => {
    const verdicts = verdictsFor([["axum", []]], "picot-host");

    expect(failures(verdicts).map((verdict) => verdict.message)).toEqual([
      expect.stringContaining("absent from the lock"),
    ]);
  });

  it("keeps the real lock green for both daemon crates", () => {
    const lock = parseLock(readFileSync("src-tauri/Cargo.lock", "utf8"));

    for (const crate of ["picot-host", "picot-daemon"]) {
      expect(failures(lockGraphVerdicts(lock, crate))).toEqual([]);
    }
  });
});

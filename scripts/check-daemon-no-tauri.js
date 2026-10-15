// ABOUTME: Guards that the shared host crate and the daemon binary stay Tauri-free.
// ABOUTME: Fails on a tauri dependency (manifest or resolved lock graph) or a tauri:: reference in source.
/**
 * Phase 1 invariant (plan D1, Dr. Lin decision 4): `crates/picot-host` and
 * `crates/picot-daemon` must not depend on `tauri` directly or transitively,
 * and must not reference `tauri::` in their sources. No transitional state.
 *
 * Three independent checks:
 *   1. Cargo.toml: no `tauri*` dependency key and no rename onto a tauri crate.
 *   2. Cargo.lock: walk each crate's resolved dependency closure; no reachable
 *      package may be named `tauri` / `tauri-*`, with ONE named exception that
 *      is re-verified on every run (`tauri-winrt-notification`; see
 *      NON_FRAMEWORK_TAURI_CRATES for the chain and the reasoning). Cargo.lock
 *      v4 *is* the resolved graph cargo would link, so this is the transitive
 *      check, and it stays read-only and offline-safe — unlike `cargo tree`,
 *      which the sandboxed `bun run test` chain cannot run (HOME/CARGO_HOME are
 *      redirected to a scratch root and every write outside it is denied).
 *      Manifest-vs-lock drift is reported so a stale lock cannot hide a new
 *      dependency.
 *   3. Sources: no `tauri::` path, `use tauri`, or `extern crate tauri` under
 *      crates/. Literal `tauri://` URLs stay allowed: they are URL strings, not
 *      a compile-time dependency.
 *
 * Run:  bun scripts/check-daemon-no-tauri.js
 * Exit: 0 = OK, 1 = violation
 */

import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const cratesDir = resolve(root, "src-tauri/crates");
const lockPath = resolve(root, "src-tauri/Cargo.lock");
const CRATES = ["picot-host", "picot-daemon"];
const TAURI_PACKAGE = /^tauri(-|$)/;
const TAURI_KEY = /^tauri(-[a-z0-9-]+)?$/;
const DEPENDENCY_SECTION = /(^|\.)(build-|dev-)?dependencies$/;

/**
 * Windows-only, non-framework exception to check 2 (T1 review, 2026-10-15;
 * decision: narrow the check to the invariant's meaning instead of weakening the
 * pattern).
 *
 * Chain: `picot-host` -> `notify-rust` -> (declared under
 * `[target.'cfg(target_os = "windows")'.dependencies]`, non-optional, renamed
 * via `package = "tauri-winrt-notification"`) `tauri-winrt-notification`.
 *
 * Why it is tolerable:
 *   - it is notify-rust's Windows toast backend and links no part of the Tauri
 *     framework (its resolved closure is thiserror/windows/windows-version);
 *   - it does not even compile on macOS, and picot-host itself gates notify-rust
 *     to `cfg(target_os = "macos")`;
 *   - Cargo.lock is target-agnostic, so a lock-graph walk cannot see the cfg
 *     gate;
 *   - the plan keeps notify-rust in picot-host on purpose (implementation plan,
 *     Verified Context line 51 lists it among the host deps that move; Task 2
 *     Step 3 moves the notification bundle identifier into `DaemonState`), so
 *     the notification data-plane op (`host_server.rs`, `show_task_notification`)
 *     stays daemon-side.
 *
 * Each name listed here is tolerated ONLY while its own resolved closure is
 * framework-free — recomputed from the lock on every run, never a static
 * snapshot: if a notify-rust upgrade really pulls the framework in, this guard
 * goes red. Anything else named `tauri` / `tauri-*` still fails as before.
 */
const NON_FRAMEWORK_TAURI_CRATES = new Set(["tauri-winrt-notification"]);

/** A `tauri`/`tauri-*` package that is NOT one of the allowlisted non-framework crates. */
export function isFrameworkTauri(name) {
  return TAURI_PACKAGE.test(name) && !NON_FRAMEWORK_TAURI_CRATES.has(name);
}

let errors = 0;

function fail(message) {
  console.error(`  FAIL  ${message}`);
  errors++;
}

function pass(message) {
  console.log(`  OK    ${message}`);
}

// ── Cargo.toml parsing ──────────────────────────────────────────────────────

/** Dependency key -> crate name it resolves to (honouring `package = "..."`). */
export function manifestDependencies(text) {
  const dependencies = new Map();
  let inDependencies = false;
  let pending = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (pending) {
      pending.body.push(line);
      pending.depth += deltas(line);
      if (pending.depth <= 0) {
        const rename = pending.body.join(" ").match(/package\s*=\s*"([^"]+)"/);
        dependencies.set(pending.key, rename ? rename[1] : pending.key);
        pending = null;
      }
      continue;
    }
    const section = line.match(/^\[(.+)\]$/);
    if (section) {
      inDependencies = DEPENDENCY_SECTION.test(section[1].trim());
      continue;
    }
    if (!inDependencies) continue;
    const entry = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.*)$/);
    if (!entry) continue;
    const body = entry[2].replace(/\s*#.*$/, "");
    const depth = deltas(body);
    if (depth > 0) {
      pending = { key: entry[1], body: [body], depth };
      continue;
    }
    const rename = body.match(/package\s*=\s*"([^"]+)"/);
    dependencies.set(entry[1], rename ? rename[1] : entry[1]);
  }
  return dependencies;
}

function deltas(line) {
  return (line.match(/\{/g) ?? []).length - (line.match(/\}/g) ?? []).length;
}

// ── Cargo.lock parsing ─────────────────────────────────────────────────────

/** crate name -> [{ version, dependencies: [name, ...] }] */
export function parseLock(text) {
  const packages = new Map();
  let entry = null;
  let inDependencies = false;
  const flush = () => {
    if (!entry?.name) return;
    if (!packages.has(entry.name)) packages.set(entry.name, []);
    packages.get(entry.name).push(entry);
  };
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line === "[[package]]") {
      if (!inDependencies) flush();
      entry = { name: null, version: null, dependencies: [] };
      inDependencies = false;
      continue;
    }
    if (!entry) continue;
    if (inDependencies) {
      if (line === "]") {
        inDependencies = false;
      } else {
        const dependency = line.replace(/^"|",?$/g, "").trim();
        if (dependency) entry.dependencies.push(dependency.split(" ")[0]);
      }
      continue;
    }
    const field = line.match(/^([A-Za-z0-9_]+)\s*=\s*(.*)$/);
    if (!field) continue;
    if (field[1] === "name") entry.name = field[2].replace(/"/g, "");
    if (field[1] === "version") entry.version = field[2].replace(/"/g, "");
    if (field[1] === "dependencies") {
      const value = field[2].trim();
      const inline = value.replace(/^\[/, "").replace(/\]$/, "").trim();
      if (inline) {
        for (const name of inline.split(",")) {
          const bare = name.replace(/"/g, "").trim();
          if (bare) entry.dependencies.push(bare.split(" ")[0]);
        }
      } else if (!value.endsWith("]")) {
        inDependencies = true;
      }
    }
  }
  if (!inDependencies) flush();
  return packages;
}

export function reachableFrom(packages, roots) {
  const seen = new Set();
  const queue = [...roots];
  while (queue.length) {
    const name = queue.pop();
    if (seen.has(name)) continue;
    seen.add(name);
    for (const entry of packages.get(name) ?? []) {
      for (const dependency of entry.dependencies) {
        if (!seen.has(dependency)) queue.push(dependency);
      }
    }
  }
  return seen;
}

/**
 * Pure core of check 2: every Tauri-related finding for one crate in a parsed
 * lock graph. Kept free of printing and of the filesystem so the regression
 * suite can pin the semantics — including the red path of the allowlisted
 * exception — with synthetic lock graphs (`scripts/check-daemon-no-tauri.test.js`).
 *
 * @param {Map<string, {dependencies: string[]}[]>} lock parsed Cargo.lock
 * @param {string} crate crate name to walk from
 * @returns {{level: "pass" | "fail", message: string}[]}
 */
export function lockGraphVerdicts(lock, crate) {
  const entries = lock.get(crate);
  if (!entries?.length) {
    return [
      {
        level: "fail",
        message: `${crate} is absent from the lock (stale lock: run cargo check)`,
      },
    ];
  }

  const graph = reachableFrom(lock, [crate]);
  const verdicts = [];
  const framework = [];
  const exceptions = [];
  for (const name of [...graph].filter((entry) => TAURI_PACKAGE.test(entry)).sort()) {
    if (isFrameworkTauri(name)) {
      framework.push(name);
      continue;
    }
    exceptions.push(name);
    // Re-verified on every run: a tolerated crate stays tolerated only while its
    // own closure stays outside the framework.
    const closure = lock.has(name) ? [...reachableFrom(lock, [name])] : null;
    const nested = closure === null ? null : closure.filter(isFrameworkTauri).sort();
    if (nested === null) {
      verdicts.push({
        level: "fail",
        message:
          `${crate} reaches ${name}, which is allowlisted as non-framework but is ` +
          "absent from the lock (stale allowlist)",
      });
    } else if (nested.length) {
      verdicts.push({
        level: "fail",
        message: `${crate}: allowlisted non-framework ${name} reaches the Tauri framework: ${nested.join(", ")}`,
      });
    } else {
      verdicts.push({
        level: "pass",
        message: `${crate} tolerates ${name}: Windows-only, no framework in its own closure (${closure.length} packages)`,
      });
    }
  }

  if (framework.length) {
    verdicts.push({
      level: "fail",
      message: `${crate} reaches tauri in the resolved graph: ${framework.join(", ")}`,
    });
  } else if (!exceptions.length) {
    verdicts.push({
      level: "pass",
      message: `${crate} resolved dependency closure is Tauri-free (${graph.size} packages)`,
    });
  }
  return verdicts;
}

// ── Source scanning ────────────────────────────────────────────────────────

function sourceFiles(dir) {
  const files = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      files.push(...sourceFiles(path));
      continue;
    }
    if (/\.(rs|toml)$/.test(name)) files.push(path);
  }
  return files;
}

const TAURI_SOURCE_PATTERNS = [
  // `tauri::` paths, but not literal `tauri://` URLs.
  { label: "tauri:: path", pattern: /tauri::(?!\/)/ },
  { label: "use tauri", pattern: /(^|\n)\s*(pub\s+)?use\s+tauri\b/ },
  { label: "extern crate tauri", pattern: /\bextern\s+crate\s+tauri\b/ },
];

// ── Checks ─────────────────────────────────────────────────────────────────

function main() {
  console.log("Checking daemon crates for Tauri coupling...");

  if (!existsSync(cratesDir)) {
    fail(`missing ${relative(root, cratesDir)}: the workspace split has not landed`);
  }
  if (!existsSync(lockPath)) {
    fail(`missing ${relative(root, lockPath)}`);
  }
  if (errors > 0) {
    console.error(`Daemon Tauri guard failed (${errors} violation(s)).`);
    process.exitCode = 1;
    return;
  }

  const lock = parseLock(readFileSync(lockPath, "utf8"));

  for (const crate of CRATES) {
    const manifestPath = join(cratesDir, crate, "Cargo.toml");
    if (!existsSync(manifestPath)) {
      fail(`missing ${relative(root, manifestPath)}`);
      continue;
    }
    const dependencies = manifestDependencies(readFileSync(manifestPath, "utf8"));

    for (const [key, name] of dependencies) {
      if (TAURI_KEY.test(key) || TAURI_KEY.test(name)) {
        fail(`${crate}/Cargo.toml declares a tauri dependency (${key} -> ${name})`);
      }
    }

    const entries = lock.get(crate);
    if (!entries?.length) {
      fail(`${crate} is absent from ${relative(root, lockPath)} (stale lock: run cargo check)`);
      continue;
    }
    const cached = new Set(entries.flatMap((entry) => entry.dependencies));
    for (const [key, name] of dependencies) {
      if (!cached.has(name)) {
        fail(`${crate}/Cargo.toml dependency ${key} is missing from the lock entry (stale lock)`);
      }
    }

    for (const verdict of lockGraphVerdicts(lock, crate)) {
      if (verdict.level === "fail") fail(verdict.message);
      else pass(verdict.message);
    }
  }

  for (const file of sourceFiles(cratesDir)) {
    const text = readFileSync(file, "utf8");
    for (const { label, pattern } of TAURI_SOURCE_PATTERNS) {
      if (pattern.test(text)) {
        fail(`${relative(root, file)} references Tauri (${label})`);
      }
    }
  }

  if (errors > 0) {
    console.error(`Daemon Tauri guard failed (${errors} violation(s)).`);
    process.exitCode = 1;
  } else {
    console.log("Daemon Tauri guard passed.");
  }
}

// A guard must never silently no-op, so the default is to run: Bun sets
// `import.meta.main`; under plain node the checks still run unless a test runner
// (VITEST) imported this module for its pure helpers.
const runChecks = import.meta.main ?? !process.env.VITEST;
if (runChecks) main();

// ABOUTME: Runs Picot tests with disposable configuration roots and kernel write restrictions.
// ABOUTME: Fails closed when the macOS sandbox is unavailable; never falls back to bare tests.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repo = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), ".."));
const args = process.argv.slice(2);
if (process.platform !== "darwin") {
  console.error("Test sandbox requires macOS sandbox-exec; no unsandboxed fallback is allowed.");
  process.exit(1);
}
const session = realpathSync(mkdtempSync(join(tmpdir(), "picot-test-sandbox-")));
const root = join(session, "writable");
let status = 1;
try {
  mkdirSync(root);
  const guard = join(session, "write-guard");
  writeFileSync(guard, "sandbox guard");
  for (const dir of ["home", "agent", "tmp", "cache", "config", "data"]) {
    mkdirSync(join(root, dir));
  }
  const command =
    args[0] === "--command"
      ? args.slice(1)
      : ["node", "node_modules/vitest/vitest.mjs", ...(args.length ? args : ["run"])];
  if (!command.length) throw new Error("Missing sandbox command");
  const writable = [
    root,
    join(repo, "public/vendor"),
    join(repo, "node_modules/.vite"),
    join(repo, "coverage"),
  ];
  if (command.length === 3 && command.join(" ") === "bun run build:extensions") {
    const extensions = join(repo, "extensions");
    const output = join(extensions, "dist");
    if (
      realpathSync(extensions) !== extensions ||
      (existsSync(output) && realpathSync(output) !== output)
    ) {
      throw new Error("Extension build output must not resolve outside its declared directory");
    }
    writable.push(output);
  }
  const profile = [
    "(version 1)",
    "(allow default)",
    "(deny file-write*)",
    ...writable.map((path) => `(allow file-write* (subpath ${JSON.stringify(path)}))`),
    '(allow file-write* (literal "/dev/null") (literal "/dev/tty") (regex #"^/dev/fd/[0-9]+$"))',
  ].join("\n");
  const env = {
    ...process.env,
    HOME: join(root, "home"),
    USERPROFILE: join(root, "home"),
    APPDATA: join(root, "config"),
    PI_CODING_AGENT_DIR: join(root, "agent"),
    TMPDIR: join(root, "tmp"),
    TMP: join(root, "tmp"),
    TEMP: join(root, "tmp"),
    XDG_CONFIG_HOME: join(root, "config"),
    XDG_CACHE_HOME: join(root, "cache"),
    XDG_DATA_HOME: join(root, "data"),
    PICOT_TEST_SANDBOX_ROOT: root,
    PICOT_TEST_WRITE_GUARD: guard,
  };
  delete env.HOMEDRIVE;
  delete env.HOMEPATH;
  delete env.PI_STUDIO_MCP_PROJECT_ROOT;
  const result = spawnSync("/usr/bin/sandbox-exec", ["-p", profile, ...command], {
    cwd: repo,
    env,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.signal) console.error(`Sandboxed tests terminated by ${result.signal}`);
  status = result.status ?? 1;
} catch (error) {
  console.error(`Test sandbox failed: ${error.message}`);
} finally {
  rmSync(session, { recursive: true, force: true });
}
process.exitCode = status;

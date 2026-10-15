#!/usr/bin/env node
// ABOUTME: Downloads the pinned Pi-Bolt JIT runtime (darwin-arm64 / windows-x64) into src-tauri/resources/pi-bolt/.
// ABOUTME: Companion to fetch-pi-binary.js — the official pi tree in resources/pi stays the default runtime.

const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const https = require("node:https");
const { spawnSync } = require("node:child_process");

const ROOT = path.resolve(__dirname, "..");
const VERSION_FILE = path.join(__dirname, "pi-bolt-version.json");
const CACHE_DIR = path.join(ROOT, ".cache", "pi-bolt-binaries");
const OUT_DIR = path.join(ROOT, "src-tauri", "resources", "pi-bolt");
const VERSION_MARKER = path.join(OUT_DIR, ".version");

function info(msg) {
  console.log(`[fetch-pi-bolt] ${msg}`);
}

function warn(msg) {
  console.warn(`[fetch-pi-bolt] WARN: ${msg}`);
}

function fail(msg) {
  console.error(`[fetch-pi-bolt] FAIL: ${msg}`);
  process.exit(1);
}

function platformAssetName() {
  // Reuses the PI_TARGET_PLATFORM vocabulary of fetch-pi-binary.js so
  // cross-build tooling drives both scripts with the same override.
  const override = process.env.PI_BOLT_TARGET_PLATFORM || process.env.PI_TARGET_PLATFORM;
  const platform = process.platform;
  const arch = process.arch;
  let key;
  if (override) {
    key = override;
  } else if (platform === "darwin" && arch === "arm64") {
    key = "darwin-arm64";
  } else if (platform === "win32" && arch === "x64") {
    key = "windows-x64";
  } else {
    key = `${platform === "win32" ? "windows" : platform}-${arch}`;
  }
  if (key === "darwin-arm64") {
    return {
      key,
      archiveName: "pi-bolt-darwin-arm64-jit.tar.gz",
      binaryName: "pi",
      isZip: false,
    };
  }
  if (key === "windows-x64") {
    return {
      key,
      archiveName: "pi-bolt-win32-x64-jit.zip",
      binaryName: "pi-bolt.exe",
      isZip: true,
    };
  }
  fail(
    `Pi-Bolt JIT covers darwin-arm64 and windows-x64 only (got ${key}). ` +
      `For this platform keep the official runtime: bun run fetch:pi`,
  );
}

function loadLockedVersion() {
  let raw;
  try {
    raw = fs.readFileSync(VERSION_FILE, "utf8");
  } catch (err) {
    fail(`Could not read ${VERSION_FILE}: ${err.message}`);
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    fail(`Invalid JSON in ${VERSION_FILE}: ${err.message}`);
  }
  if (!parsed.version || typeof parsed.version !== "string") {
    fail(`Missing "version" string in ${VERSION_FILE}`);
  }
  const sha256 = parsed.sha256 && typeof parsed.sha256 === "object" ? parsed.sha256 : {};
  // Pi-Bolt is a third-party fork binary: the sha256 pin is the only trust
  // anchor on this path (upstream pi is official, pi-bolt is not), so an
  // unpinned asset is a hard error rather than a warning.
  for (const name of ["pi-bolt-darwin-arm64-jit.tar.gz", "pi-bolt-win32-x64-jit.zip"]) {
    if (!/^[a-f0-9]{64}$/.test(sha256[name] ?? "")) {
      fail(
        `Missing valid sha256 for ${name} in ${VERSION_FILE}. Take it from the release's SHA256SUMS asset.`,
      );
    }
  }
  return {
    version: parsed.version.trim(),
    piVersion: typeof parsed.piVersion === "string" ? parsed.piVersion.trim() : "",
    sha256,
  };
}

function isUpToDate(version, binaryName) {
  if (!fs.existsSync(VERSION_MARKER)) return false;
  let current;
  try {
    current = fs.readFileSync(VERSION_MARKER, "utf8").trim();
  } catch {
    return false;
  }
  if (current !== version) return false;
  return fs.existsSync(path.join(OUT_DIR, binaryName));
}

function ensureDirEmpty(dir) {
  if (fs.existsSync(dir)) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  fs.mkdirSync(dir, { recursive: true });
}

function downloadTo(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    const cleanup = (err) => {
      file.close();
      fs.unlink(dest, () => {});
      reject(err);
    };
    const handle = (res) => {
      // Follow redirects (GitHub release downloads always redirect to S3).
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        downloadTo(res.headers.location, dest).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        cleanup(new Error(`HTTP ${res.statusCode} for ${url}`));
        return;
      }
      res.pipe(file);
      file.on("finish", () => file.close(() => resolve()));
      file.on("error", cleanup);
    };
    https
      .get(url, { headers: { "User-Agent": "picot-fetch-pi-bolt" } }, handle)
      .on("error", cleanup);
  });
}

function sha256OfFile(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

function flattenSingleWrapper(outDir) {
  // Pi-Bolt archives may wrap the payload in one top-level directory whose
  // name varies by asset. If (and only if) the archive produced exactly one
  // top-level entry and it is a directory, promote its contents.
  const entries = fs.readdirSync(outDir);
  if (entries.length !== 1) return;
  const wrapper = path.join(outDir, entries[0]);
  if (!fs.statSync(wrapper).isDirectory()) return;
  // Rename first: the wrapper may share its name with a child (e.g. pi/).
  const tmp = path.join(outDir, `__bolt_tmp_${process.pid}`);
  fs.renameSync(wrapper, tmp);
  for (const entry of fs.readdirSync(tmp)) {
    fs.renameSync(path.join(tmp, entry), path.join(outDir, entry));
  }
  fs.rmdirSync(tmp);
}

function extractTarGz(archivePath, outDir) {
  const result = spawnSync("tar", ["-xzf", archivePath, "-C", outDir], {
    stdio: "inherit",
  });
  if (result.status !== 0) {
    fail(`tar extraction failed (exit ${result.status})`);
  }
  flattenSingleWrapper(outDir);
}

function sleepSync(ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    // Busy-wait is fine here: extraction retry loop, ms is small.
  }
}

function extractZip(archivePath, outDir) {
  const maxAttempts = 5;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    // tar (libarchive) unpacks zip on macOS and on Windows 10 1803+.
    const tarResult = spawnSync("tar", ["-xf", archivePath, "-C", outDir], {
      stdio: "inherit",
    });
    if (tarResult.status === 0) {
      flattenSingleWrapper(outDir);
      return;
    }
    const viaPowerShell =
      process.platform === "win32"
        ? spawnSync(
            "powershell",
            [
              "-NoProfile",
              "-ExecutionPolicy",
              "Bypass",
              "-Command",
              `$ErrorActionPreference='Stop'; Expand-Archive -Force -Path '${archivePath}' -DestinationPath '${outDir}'`,
            ],
            { stdio: "inherit" },
          )
        : null;
    if (viaPowerShell && viaPowerShell.status === 0) {
      flattenSingleWrapper(outDir);
      return;
    }
    if (attempt < maxAttempts) {
      warn(`zip extraction failed (attempt ${attempt}/${maxAttempts}); retrying in 1s…`);
      sleepSync(1000);
    } else {
      fail(`zip extraction failed after ${maxAttempts} attempts`);
    }
  }
}

async function main() {
  const { version, piVersion, sha256 } = loadLockedVersion();
  const asset = platformAssetName();

  info(`locked Pi-Bolt version: ${version}${piVersion ? ` (Pi ${piVersion}, JIT variant)` : ""}`);
  info(`target asset: ${asset.archiveName}`);

  if (isUpToDate(version, asset.binaryName)) {
    info(`already up to date at ${OUT_DIR}; skipping.`);
    return;
  }

  fs.mkdirSync(CACHE_DIR, { recursive: true });
  const cachedArchive = path.join(CACHE_DIR, `${version}-${asset.archiveName}`);
  const expectedSha = sha256[asset.archiveName];

  if (fs.existsSync(cachedArchive)) {
    const actual = sha256OfFile(cachedArchive);
    if (actual === expectedSha) {
      info(`using cached archive: ${cachedArchive}`);
    } else {
      warn(`cached archive checksum mismatch; re-downloading.`);
      fs.unlinkSync(cachedArchive);
    }
  }

  if (!fs.existsSync(cachedArchive)) {
    const url = `https://github.com/opensec-git/Pi-Bolt/releases/download/bolt-v${version}/${asset.archiveName}`;
    info(`downloading ${url}`);
    try {
      await downloadTo(url, cachedArchive);
    } catch (err) {
      fail(
        `download failed: ${err.message}\n` +
          `  - check network connectivity\n` +
          `  - verify bolt-v${version} exists at https://github.com/opensec-git/Pi-Bolt/releases`,
      );
    }
  }

  const actual = sha256OfFile(cachedArchive);
  if (actual !== expectedSha) {
    try {
      fs.unlinkSync(cachedArchive);
    } catch {}
    fail(
      `sha256 mismatch for ${asset.archiveName}: expected ${expectedSha}, got ${actual}. Cached archive removed.`,
    );
  }
  info(`sha256 verified.`);

  info(`extracting to ${OUT_DIR}`);
  ensureDirEmpty(OUT_DIR);
  if (asset.isZip) {
    extractZip(cachedArchive, OUT_DIR);
  } else {
    extractTarGz(cachedArchive, OUT_DIR);
  }

  const binPath = path.join(OUT_DIR, asset.binaryName);
  if (!fs.existsSync(binPath)) {
    fail(
      `extraction succeeded but ${binPath} is missing. ` +
        `The release archive layout may have changed.`,
    );
  }
  if (!asset.isZip) {
    try {
      fs.chmodSync(binPath, 0o755);
    } catch {}
  }
  // No ad-hoc codesign here, unlike fetch-pi-binary.js: the darwin payload is
  // a shim + a 200MB AOT executable whose embedded signature the engine
  // validates at launch. Re-signing risks "image is not one for this engine"
  // fallback to bytecode. Plain copies have spawned repeatedly under macOS
  // AMFI without SIGKILL; revisit only if that ever changes.

  fs.writeFileSync(VERSION_MARKER, version, "utf8");
  info(`installed Pi-Bolt ${version} -> ${binPath}`);
}

main().catch((err) => {
  fail(`unexpected error: ${err.stack || err.message || err}`);
});

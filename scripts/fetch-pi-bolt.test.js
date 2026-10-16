// ABOUTME: Verifies the Pi-Bolt lockfile and the wiring that fetches it alongside the official pi.
// ABOUTME: Keeps the third-party JIT runtime pinned, checksummed, and clearly non-default.
import fs from "node:fs";
import path from "node:path";
import { describe, expect, test } from "vitest";

const root = path.resolve(import.meta.dirname, "..");
const read = (relativePath) => fs.readFileSync(path.join(root, relativePath), "utf8");

describe("pi-bolt distribution", () => {
  test("pins the Pi-Bolt JIT release and checksums for both supported platforms", () => {
    const lock = JSON.parse(read("scripts/pi-bolt-version.json"));

    expect(lock.version).toMatch(/^\d+\.\d+\.\d+$/);
    expect(lock.variant).toBe("jit");
    expect(lock.sha256["pi-bolt-darwin-arm64-jit.tar.gz"]).toMatch(/^[a-f0-9]{64}$/);
    expect(lock.sha256["pi-bolt-win32-x64-jit.zip"]).toMatch(/^[a-f0-9]{64}$/);
  });

  test("fetch script downloads from the Pi-Bolt repo and deflects unsupported platforms to fetch:pi", () => {
    const script = read("scripts/fetch-pi-bolt.js");
    expect(script).toContain("opensec-git/Pi-Bolt/releases/download");
    expect(script).toContain("bun run fetch:pi");
    expect(script).toContain("src-tauri", "resources", "pi-bolt");
  });
});

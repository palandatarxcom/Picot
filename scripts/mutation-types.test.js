// ABOUTME: Locks the shared mutation command manifest against Rust validation paths.
// ABOUTME: Prevents foundation changes from altering production mutation classification implicitly.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const manifest = JSON.parse(readFileSync("shared/mutation-types.json", "utf8"));
const router = readFileSync("src-tauri/crates/picot-host/src/host_router.rs", "utf8");
const manager = readFileSync("src-tauri/crates/picot-host/src/native_pi_manager.rs", "utf8");

describe("shared mutation types", () => {
  it("contains exactly 14 unique command names", () => {
    expect(manifest).toHaveLength(14);
    expect(new Set(manifest).size).toBe(14);
  });

  it("keeps both Rust production paths on the shared manifest", () => {
    expect(router).toContain("use crate::mutation_types::is_mutation;");
    expect(manager).toContain("use crate::mutation_types::is_mutation;");
    expect(router).not.toMatch(/fn is_mutation\s*\(/);
    expect(manager).not.toMatch(/fn is_mutation\s*\(/);
  });
});

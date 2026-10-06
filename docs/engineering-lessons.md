# Engineering Lessons — Native Runtime Migration

Date: 2026-08-30 ｜ Status: P8 documentation deliverable

## Overview

This document captures the engineering lessons from Picot's native runtime
migration (P0 through P8). Each lesson is release-blocking in the sense that
ignoring it caused a real bug, delayed a phase, or required a design reversal.

## Lessons

### 1. Protocol shape belongs in the wire contract, not in test fixtures

**What happened**: The P1 turn-abort gate read `turnId` from the Pi RPC
*response*. The in-memory test fixture encoded this wrong assumption. The
real Pi runtime never puts `turnId` in responses — it arrives only on
`runtime_event` frames. The gate was inert against a real runtime.

**Fix**: Bind turns from the event pump (where `turnId` actually appears),
not from the response. Fixtures must encode the *real* protocol shape.

**Rule**: Every wire-level fixture must be derived from a real frame capture,
not from reading the implementation. A fixture that agrees with the code it
tests proves nothing.

### 2. Concurrent agents in one working tree require commit-first discipline

**What happened**: Multiple agents worked P4/P5/P6/P7 in the same tree. A
cleanup sweep deleted `cost_compat.rs` and `p4-cost-parity.mjs` as
"unreferenced files" because they weren't yet wired into the module tree.

**Fix**: Commit new modules immediately after creation, before wiring. Tracked
files survive orphan sweeps; untracked files don't.

**Rule**: In a shared working tree, `write → commit → wire` beats
`write → wire → commit`.

### 3. Path containment must be separator-aware (`strip_prefix`, not `starts_with`)

**What happened**: Session-file deletion used `to_string_lossy().starts_with()`
for containment. A path like `~/.pi/agent/sessions-evil/x.jsonl` passed the
prefix check (shares the string prefix with `sessions/`) but lives outside
the session root.

**Fix**: `canonical.strip_prefix(&root).is_err()` — component-wise comparison
that requires the actual directory boundary.

**Rule**: Never use string-prefix for filesystem containment. Always
`Path::strip_prefix` or equivalent component-level check.

### 4. String comparison for running-process protection is bypassable by path respelling

**What happened**: Delete-batch compared raw request strings against running
session-file paths. A respelled path (`/root/./a.jsonl`) or symlinked spelling
bypassed the guard.

**Fix**: Canonicalize both sides before comparing. `A == B` on canonical paths
is robust against `./`, `..`, and symlink aliases.

**Rule**: Identity checks on filesystem paths must operate on canonical forms.

### 5. JS number semantics differ from Rust f64 serialization

**What happened**: The cost-dashboard parity test failed because Rust's
`serde_json` serializes `1.0` as `"1.0"` while JavaScript's `JSON.stringify`
emits `"1"`. The payloads were numerically identical but byte-different.

**Fix**: Normalize integral floats to integer values (`js_value_number`)
before JSON serialization, matching JS's single number type.

**Rule**: When doing field-by-field JSON parity between Rust and TypeScript,
handle the number formatting gap explicitly.

### 6. `current_registered_context` re-checks belong on every arm, not just the handshake

**What happened**: The WebSocket data-plane arm trusted the handshake-time
workspace binding. A socket held open across a workspace transition kept
write access to the old workspace.

**Fix**: Every dispatch arm (Host, Data, Runtime, Subscribe) re-reads the
owner registry to verify the current workspace generation.

**Rule**: Handshake context is an identity proof, not an authorization
session. Re-read authority on every admission.

### 7. Export tokens need generation binding, not just TTL

**What happened**: Session-export tokens were TTL-bound and one-shot but
carried no workspace-generation context. A token issued before a workspace
transition remained redeemable after the transition invalidated the
workspace.

**Fix**: Bind `(owner, generation)` into the grant; redemption checks the
owner's *current* generation via a closure against the live registry.
Workspace transitions call `revoke_session_exports(owner)`.

**Rule**: One-shot tokens for workspace-scoped resources must bind the
workspace generation and be revoked on generation change.

### 8. The `cargo test --ignored` flag moved to the binary in cargo 1.97

**What happened**: `cargo test --ignored <filter>` stopped working — cargo
now suggests `--ignore-rust-version` instead. The P1.11 smoke script silently
failed.

**Fix**: Use the libtest-level flag: `cargo test <filter> -- --ignored
--nocapture`.

**Rule**: Pin CLI invocations in scripts; cargo's top-level flag surface
changes across versions.

### 9. Fixture files must be valid JSON objects (check closing braces)

**What happened**: Two test fixtures wrote JSONL lines missing the closing
`}`, produced by a `format!` string that lost the trailing `}}`. The parser
silently skipped the malformed lines, making the test fail with a confusing
"not found" error.

**Fix**: Check the generated fixture file content when a "missing data" test
failure is otherwise unexplainable.

**Rule**: When `format!` produces structured data, verify the output matches
the intended shape before debugging the consumer.

### 10. OAuth cross-owner queries must not leak operation existence

**What happened**: `OAuthManager::status` returned `StaleGeneration` for
foreign owner IDs, revealing that the operation exists (even though the
querier has no right to see it).

**Fix**: Return `OperationNotFound` uniformly for cross-owner lookups —
same error as a non-existent operation.

**Rule**: Error codes must not form an oracle for resource existence across
authorization boundaries.

### 11. Adversarial review cannot substitute for a design review

**What happened**: An "environment check + install" feature was built as a
process-supervision subsystem (~11,700 lines): one embedded-Pi process per
tool, provable cross-platform process-tree containment, Windows Job Objects
with suspended creation and handle-ownership tables, restart-survivable
cleanup state, typed cleanup verdicts, inode-anchored temp deletion. Four
rounds of adversarial code review each returned ~10 blocking defects and
never passed the code. Deleting the subsystem and reducing "install" to a
single `pi --no-session -p "<prompt>"` call left ~1,034 lines.

**Fix**: Ask whether the mechanism needs to exist before reviewing how it is
implemented. The AI performs the install; nothing else was required.

**Rule**: When review rounds keep finding ever-deeper defects in the same
design, the evidence points at the design, not at the code needing another
pass. Stop, restate the goal, and cut. A review that is correct about a
wrong design is still a wrong plan.

### 12. Structural source lines must not be rewritten by line-oriented bulk edits

**What happened**: `sed 's/^#[cfg(test)]$//'` over a Rust source file removed
all three `#[cfg(test)]` gates at once; the production build then reported
test-only imports as unused. The same class of risk applies to `#[cfg(...)]`,
attributes, and macro delimiters.

**Fix**: `git checkout -- <file>` and redo with targeted edits.

**Rule**: Never bulk-replace structural lines (attributes, gates, macros) with
line-oriented tools. Use anchored edits. When a bulk edit has already gone
wrong, restore from git instead of patching on top of the damage.

### 13. An anchor insertion keeps the anchor line — check for duplication

**What happened**: `@INS.BEFORE`/`@INS.AFTER` preserve the anchor line. Twice
the anchor line was also written into the inserted block, producing a
duplicated `fn read_bounded` definition and a duplicated
`## Office 文件原生预览` heading in `ARCHITECTURE.md`.

**Rule**: After any anchor-based insertion, inspect the anchor line
immediately for duplication.

### 14. Before deleting a "no callers" method, find the state chain it serves

**What happened**: `Installer::last()` was deleted as dead code. It was the
accessor for the terminal snapshot: `status()` returned a snapshot only while
a job was running, so the page cleared its result panel as soon as the run
finished, and a user who left Settings and came back could not tell which
tool had been installed or updated.

**Fix**: Return the last snapshot when idle, with a regression test that keeps
polling after the run ends.

**Rule**: Contact with a state machine, snapshot, or lifecycle accessor cannot
be established by call sites alone — verify whether a UI or recovery path
depends on the value surviving before calling it dead.

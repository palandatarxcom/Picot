# Picot agent guide

This file contains repository-wide development rules. Product architecture,
feature invariants, transport paths, security boundaries, and module ownership
live in [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Read first

- Read the applicable `ARCHITECTURE.md` section and its linked design documents
  before changing UI behavior, persistence, workspace I/O, or cross-process
  communication.
- Before changing a browser/server adapter, popup/overlay, or shared-state
  rerender behavior, read and apply [`docs/engineering-lessons.md`](docs/engineering-lessons.md).
- Update `ARCHITECTURE.md` when an implementation materially changes its
  architecture, invariants, lifecycle, security boundary, or validation
  contract. Changes to LAN access, cross-platform paths, or static serving also
  require the corresponding architecture update.

## Documentation

- Design spec（`docs/superpowers/specs/`）用中文写；implementation plan（`docs/superpowers/plans/`）用英文写。
- 文档交付前用 `writing-clearly-and-concisely` skill 润色；中文文档再用 `humanizer-zh` skill 去 AI 味。

## Agent memory

This repo maintains an agent memory bank at `.memory/MEMORY.md` (gitignored,
local-only). **Read it before starting work** in this repo: it holds decision
logs, lessons from past mistakes, and a topic index under `.memory/topics/`.
Batch notes live in `.memory/notes/`. To record new decisions/lessons after a
work session, use the `update-memory` skill.

Dr. Lin's hand edits there always win over agent merges.

## Pi references

- [RPC protocol](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)
- [SDK](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/sdk.md)
- [Session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md)
- [JSON mode](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/json.md)

## Toolchain

Use **Bun** exclusively. Never run `npm install` or `npm ci`; they create a
stray `package-lock.json` that conflicts with `bun.lock`.
### Bun lockfile discipline

Tauri CLI 2.11.2+ hard-blocks `tauri build` when any `@tauri-apps/<plugin>`
npm version drifts in major/minor from its Rust crate counterpart (PR
`tauri-apps/tauri#13993`). Bare `bun install` silently resolves carets like
`^2` to the latest npm release, which can drift past the Rust minor that
`Cargo.lock` has frozen. The build then fails for the next person with a
mysterious "Found version mismatched Tauri packages" error and a half-built
`src-tauri/target/`.

Rules:
- **Never** run `bun install` bare. Always `bun install --frozen-lockfile`
  (or `--lockfile-only` if you only want to refresh the lockfile from the
  current `package.json`).
- After `bun add <pkg>`, run `bun install --lockfile-only` and commit
  `bun.lock` together with `package.json` — never one without the other.
- `scripts/build.sh` `install_deps` refuses to proceed if `bun.lock` has
  uncommitted changes, with a hint to revert. Trust it; the check exists
  to surface drift at the gate, not after a 5-minute build.

```bash
bun install --frozen-lockfile
bun run dev
bun run test
bun run check
bun run check:rust
bun run build:extensions
```

Useful focused test form:

```bash
bun run test:focused public/settings-save-status.test.js
```

## Test filesystem safety

- Run Vitest through the sandboxed `test`, `test:focused`, `test:coverage`, or
  `test:watch` scripts. Direct Vitest is rejected before test modules load.
- The current write sandbox supports macOS only. If it cannot start, stop;
  do not fall back to bare tests. Other platforms need a verified sandbox.
- Before changing a shared root resolver, enumerate its read/write consumers
  and isolate every directory override before loading production modules.
- If tests touch real user files, stop and report affected paths immediately.
  Preserve evidence; restoration requires Dr. Lin's approval and a verified
  baseline, including symlink targets. See `ARCHITECTURE.md#测试文件系统边界`.

## Frontend and extension checks

Biome is the JS/TS formatter and linter.

```bash
bun run check       # lint, format, and design check
bun run check:fix   # safe automatic fixes
bun run lint
bun run format
bun run format:fix
```

After editing `.js` or `.ts` under `public/` or `extensions/`, run `bun run check`.
After editing extension sources under `extensions/`, run `bun run build:extensions`.

## Module discipline

The WebView is vanilla JavaScript with no framework.

- Keep one concern per file; do not add unrelated logic for convenience.
- Keep `app.js` as an orchestrator. Put new feature logic in a dedicated module
  and import it explicitly.
- Extract a feature adding roughly 50 lines or more into its own module.
- Do not mutate shared state as an import side effect.
- Use kebab-case filenames that describe one responsibility.
- For loopback access, filesystem paths, static assets, or locale coverage,
  run the full `bun run test` suite before completion.

## Verification

- After Rust edits, run `bun run check:rust`; do not use `tauri build` or
  `cargo build` merely to verify a fix.
- After frontend or extension edits, run `bun run check`; run the focused test
  first, then the relevant broader suite.
- `bun run test` includes Vitest and Tauri capability validation.
- Do not claim completion with failing tests or undocumented intentional
  warnings.

## Embedded Pi version

The embedded binary is the only Pi runtime Picot launches; do not rely on a
user-installed `pi` from `$PATH`. To upgrade it, follow the verified procedure
in [`ARCHITECTURE.md`](ARCHITECTURE.md#如何读这个仓库): change
`scripts/pi-version.json`, run `bun run fetch:pi`, smoke-test the embedded
binary and `bun run dev`, then commit only the version pin—not
`src-tauri/resources/pi/`.

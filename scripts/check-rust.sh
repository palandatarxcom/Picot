#!/usr/bin/env bash
# Run Rust type-check + clippy lint over the workspace (app package, picot-host, picot-daemon).
# Use after every fix to catch compile-time errors (E0282, E0061, deprecated v1 APIs, etc.)
# before invoking `tauri build`.

set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST="$SCRIPT_DIR/../src-tauri/Cargo.toml"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found in PATH (looked under \$HOME/.cargo/bin)" >&2
  exit 127
fi

echo "==> cargo check (workspace, all targets)"

# The app package's build.rs (tauri-build) copies `bundle.externalBin` into the
# target dir on every cargo invocation and fails hard when it is missing
# ("resource path `binaries/picot-daemon-<triple>` doesn't exist"). That file is
# a prebuild artifact — like `src-tauri/resources/pi` — so point at the
# one-liner instead of letting the tauri-build error be the first hint.
if [ "$(uname -s)" = "Darwin" ]; then
  HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
  SIDECAR="$SCRIPT_DIR/../src-tauri/binaries/picot-daemon-$HOST_TRIPLE"
  if [ ! -f "$SIDECAR" ]; then
    echo "error: missing daemon sidecar $SIDECAR" >&2
    echo "       (bundle.externalBin, plan Task 11); run: bun run build:daemon:sidecar" >&2
    exit 1
  fi
fi

cargo check --manifest-path "$MANIFEST" --workspace --all-targets

echo "==> cargo clippy (workspace, warnings as errors)"
cargo clippy --manifest-path "$MANIFEST" --workspace --all-targets -- -D warnings

echo "==> cargo test (workspace, unit)"
if ! cargo test --manifest-path "$MANIFEST" --workspace --quiet; then
    echo "    unit tests failed" >&2
    exit 1
fi

if cargo fmt --version >/dev/null 2>&1; then
    echo "==> cargo fmt --check (advisory, workspace)"
    if ! cargo fmt --manifest-path "$MANIFEST" --all --check >/dev/null 2>&1; then
        echo "    formatting drift detected; run 'cargo fmt --manifest-path $MANIFEST --all' to fix"
    fi
fi

echo "==> all rust checks passed"

#!/usr/bin/env bash
# ABOUTME: One sandboxed session for Rust checks: isolated HOME/Pi/Cargo roots, real cargo handed off.
# ABOUTME: Single layer only — enters scripts/test-sandbox.js once and refuses nested or unisolated runs.

# Rust needs writable roots the JS test entry never asked for: Cargo's target
# directory and Cargo home. Both must live inside the disposable sandbox root,
# because the sandbox denies every other write — including this repository's
# own `src-tauri/target`. The registry the daemon's orphan sweep reads
# (`$HOME/.pi/picot-runtimes`) becomes disposable for the same reason: an
# inherited real HOME is what put a developer's runtimes at risk.
#
# Two modes, one layer:
#
#   scripts/rust-test-sandbox.sh -- <command> [args...]   (outer)
#     resolves the installed cargo, then enters scripts/test-sandbox.js once
#     with this same script as the inner session.
#   scripts/rust-test-sandbox.sh --self-check             (outer, harmless)
#     proves the isolation with no product command at all.
#   scripts/rust-test-sandbox.sh --session <command>...   (inner only)
#     proves the kernel boundary and the root containment, then execs.
#
# Therefore `launcher → test-sandbox → cargo` is the only shape. The inner
# session never re-enters the sandbox, and a run that is already inside one is
# refused instead of nested. A marker alone is not proof: `--session` re-checks
# the root containment and the kernel's write denial before any command runs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd -P)"
SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"

refuse() {
  printf 'rust-test-sandbox: %s\n' "$1" >&2
  exit 2
}

# Containment is component-aware: a string prefix would accept a sibling
# directory that merely shares the root's name (`…/writable-evil`).
under() {
  case "$1" in
    "$2"/*) return 0 ;;
    *) return 1 ;;
  esac
}

# Physical absolute path of an existing path, symlinks followed (no GNU
# `realpath` on macOS). A respelled or symlinked path must not pass a
# containment check by string comparison (engineering lesson #4).
canon() {
  local path="$1" target
  while [ -L "$path" ]; do
    target="$(readlink "$path")"
    case "$target" in
      /*) path="$target" ;;
      *) path="$(dirname "$path")/$target" ;;
    esac
  done
  if [ -d "$path" ]; then
    (cd "$path" && pwd -P)
  else
    printf '%s/%s\n' "$(cd "$(dirname "$path")" && pwd -P)" "$(basename "$path")"
  fi
}

if [ "${1:-}" = "--session" ]; then
  shift
  ROOT_RAW="${PICOT_TEST_SANDBOX_ROOT:-}"
  [ -n "$ROOT_RAW" ] || refuse "no session marker: refusing to run Rust without kernel isolation"
  [ -d "$ROOT_RAW" ] || refuse "session root is not a directory: $ROOT_RAW"
  ROOT="$(canon "$ROOT_RAW")"

  expect_root() {
    local name="$1" value="$2" resolved
    [ -n "$value" ] || refuse "$name is unset inside the session"
    resolved="$(canon "$value")"
    under "$resolved" "$ROOT" || refuse "$name escapes the session root: $resolved"
    printf '%s' "$resolved"
  }

  HOME_DIR="$(expect_root HOME "${HOME:-}")"
  AGENT_DIR="$(expect_root PI_CODING_AGENT_DIR "${PI_CODING_AGENT_DIR:-}")"
  expect_root TMPDIR "${TMPDIR:-}" >/dev/null
  export CARGO_TARGET_DIR="$ROOT/cargo-target"
  export CARGO_HOME="$ROOT/cargo-home"
  mkdir -p "$CARGO_TARGET_DIR" "$CARGO_HOME" \
    || refuse "cannot create the Cargo roots under $ROOT"
  expect_root CARGO_TARGET_DIR "$CARGO_TARGET_DIR" >/dev/null
  expect_root CARGO_HOME "$CARGO_HOME" >/dev/null

  # Inherited real-relay / project overrides would point the session at
  # something outside it.
  unset PICOT_RELAY_ENDPOINT PICOT_RELAY_TLS PI_STUDIO_PORT PI_STUDIO_MCP_PROJECT_ROOT

  # The daemon's orphan sweep reads this registry at boot; inside the session
  # it must be a fresh, writable, disposable directory.
  REGISTRY_DIR="$HOME_DIR/.pi/picot-runtimes"
  mkdir -p "$REGISTRY_DIR" || refuse "cannot create the isolated runtime registry: $REGISTRY_DIR"
  printf 'probe\n' >"$REGISTRY_DIR/.isolation-probe" \
    || refuse "the isolated runtime registry is not writable: $REGISTRY_DIR"
  rm -f "$REGISTRY_DIR/.isolation-probe"

  CARGO_BIN="${PICOT_TEST_CARGO:-}"
  [ -n "$CARGO_BIN" ] || refuse "no cargo executable handed off by the launcher"
  [ -x "$CARGO_BIN" ] || refuse "handed-off cargo is not executable: $CARGO_BIN"
  CARGO_BIN="$(canon "$CARGO_BIN")"
  under "$CARGO_BIN" "$ROOT" && refuse "cargo executable resolves inside the session root: $CARGO_BIN"
  CARGO_VERSION="$(cargo --version 2>&1)" || refuse "handed-off cargo does not run: $CARGO_BIN"

  # The kernel boundary, proven rather than assumed: a write outside the root
  # and a write into the repository's own build directory must both fail.
  deny_write() {
    # Subshell: the shell, not the command, reports an unrunnable redirect.
    if ( printf 'probe\n' >"$1" ) 2>/dev/null; then
      rm -f "$1"
      refuse "the sandbox allowed a write to $1"
    fi
  }
  deny_write "$REPO/src-tauri/target/.rust-test-sandbox-probe"
  deny_write "$(dirname "$ROOT")/.rust-test-sandbox-probe"

  printf 'rust-test-sandbox: root=%s home=%s pi-agent=%s cargo-target=%s cargo-home=%s registry=%s cargo=%s (%s)\n' \
    "$ROOT" "$HOME_DIR" "$AGENT_DIR" "$CARGO_TARGET_DIR" "$CARGO_HOME" "$REGISTRY_DIR" "$CARGO_BIN" "$CARGO_VERSION" >&2
  printf 'rust-test-sandbox: denied writes: %s and %s\n' \
    "$REPO/src-tauri/target" "$(dirname "$ROOT")" >&2
  printf 'rust-test-sandbox: session isolation verified\n' >&2
  [ "$#" -ge 1 ] || refuse "missing command in the session"
  exec "$@"
fi

if [ -n "${PICOT_TEST_SANDBOX_ROOT:-}" ]; then
  refuse "already inside a test sandbox; refusing a nested session"
fi

if [ "${1:-}" = "--self-check" ]; then
  shift
  [ "$#" -eq 0 ] || refuse "--self-check takes no command"
  set -- /bin/bash -c 'printf "rust-test-sandbox: self-check ok\n"'
elif [ "${1:-}" = "--" ]; then
  shift
else
  refuse "usage: bash scripts/rust-test-sandbox.sh -- <command> [args...] (or --self-check)"
fi
[ "$#" -ge 1 ] || refuse "missing command after --"

command -v node >/dev/null 2>&1 || refuse "node not found on PATH"
CARGO_ON_PATH="$(command -v cargo || true)"
[ -n "$CARGO_ON_PATH" ] || refuse "cargo not found on PATH"
CARGO_REAL="$(canon "$CARGO_ON_PATH")"
[ -x "$CARGO_REAL" ] || refuse "cargo is not executable: $CARGO_REAL"
export PICOT_TEST_CARGO="$CARGO_REAL"

exec node "$SCRIPT_DIR/test-sandbox.js" --command /bin/bash "$SELF" --session "$@"

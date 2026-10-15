#!/usr/bin/env bash
#
# Picot daemon sidecar staging (plan Task 11 Step 1; macOS-only per D9).
#
# Builds `picot-daemon` in release mode and stages it where Tauri's
# `bundle.externalBin` resolution looks for it:
#
#   src-tauri/binaries/picot-daemon-<target-triple>
#
# The triple suffix is not cosmetic: `tauri_utils::resources::external_binaries`
# rewrites every `bundle.externalBin` entry `binaries/picot-daemon` into
# `binaries/picot-daemon-<target-triple>` (plus `.exe` on Windows), so a file
# without the suffix is never found. The bundler then strips the suffix again
# when it copies the binary next to the app executable, which is what
# `daemon_client::manager::resolve_daemon_binary` (release branch) resolves:
# `Picot.app/Contents/MacOS/picot-daemon`.
#
# Why a script and not just a function inside scripts/build.sh
# -----------------------------------------------------------
# `tauri-build`'s build.rs copies `bundle.externalBin` into the target dir on
# *every* cargo invocation of the app package (dev included) and fails hard when
# the file is missing ("resource path `binaries/picot-daemon-<triple>` doesn't
# exist"). The file is therefore a build prerequisite for `cargo check` /
# `cargo test` / IDE builds on macOS — the same class of prerequisite as
# `bun run fetch:pi` (which `bundle.resources` `./resources/pi` needs). This
# script is the one-command way to satisfy it (`bun run build:daemon:sidecar`)
# without running a full `./scripts/build.sh`; build.sh calls it too.
#
# Usage:
#   ./scripts/build-daemon-sidecar.sh [target-triple]
#
#   target-triple  defaults to $TAURI_ENV_TARGET_TRIPLE (set by the Tauri CLI
#                  for beforeDevCommand/beforeBuildCommand hooks), then to the
#                  host triple.
#
# Non-macOS targets are a no-op (exit 0): Phase 1 packages the daemon for macOS
# only (plan D9). Keeping the skip here rather than at every call site means the
# shared Tauri hook can call this unconditionally — the Windows/Linux builds in
# .github/workflows/release.yml stay exactly as they were.
#
# `universal-apple-darwin` (scripts/build.sh mac-universal) builds both arch
# sidecars *and* `lipo`s them into picot-daemon-universal-apple-darwin, so both
# resolution flavors work: a per-arch build.rs pass (each arch file) and a
# universal bundler pass (the fat file).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
MANIFEST="$PROJECT_ROOT/src-tauri/Cargo.toml"
BINARIES_DIR="$PROJECT_ROOT/src-tauri/binaries"
DAEMON_BIN="picot-daemon"

# Same rustup-over-Homebrew discipline as scripts/check-rust.sh: Homebrew rust
# has no `rustup target add` and cannot cross-compile.
export PATH="$HOME/.cargo/bin:$PATH"

log() { echo -e "[sidecar] $*"; }
log_error() { echo -e "[sidecar] ERROR: $*" >&2; }

host_triple() {
	rustc -vV | sed -n 's/^host: //p'
}

resolve_target() {
	if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
		echo "$1"
		return
	fi
	if [ -n "${TAURI_ENV_TARGET_TRIPLE:-}" ]; then
		echo "$TAURI_ENV_TARGET_TRIPLE"
		return
	fi
	host_triple
}

# cargo build the daemon for one triple and stage it under the suffix the
# bundler resolves.
build_one() {
	local triple="$1"
	if [ "$triple" != "$(host_triple)" ]; then
		# Cross-compiling (Intel from Apple Silicon): idempotent, like build.sh.
		rustup target add "$triple" >/dev/null 2>&1 || true
	fi
	log "cargo build --release -p $DAEMON_BIN --target $triple"
	cargo build --manifest-path "$MANIFEST" --release --target "$triple" -p "$DAEMON_BIN"

	local artifact="$PROJECT_ROOT/src-tauri/target/$triple/release/$DAEMON_BIN"
	if [ ! -f "$artifact" ]; then
		log_error "cargo reported success but $artifact is missing"
		exit 1
	fi
	mkdir -p "$BINARIES_DIR"
	install -m 755 "$artifact" "$BINARIES_DIR/$DAEMON_BIN-$triple"
	log "staged $BINARIES_DIR/$DAEMON_BIN-$triple"
}

main() {
	local target
	target="$(resolve_target "$@")"

	case "$target" in
	universal-apple-darwin)
		build_one aarch64-apple-darwin
		build_one x86_64-apple-darwin
		local fat="$BINARIES_DIR/$DAEMON_BIN-universal-apple-darwin"
		lipo -create -output "$fat" \
			"$BINARIES_DIR/$DAEMON_BIN-aarch64-apple-darwin" \
			"$BINARIES_DIR/$DAEMON_BIN-x86_64-apple-darwin"
		log "staged $fat (arm64 + x86_64)"
		;;
	*-apple-darwin)
		build_one "$target"
		;;
	*)
		log "skipping: the daemon sidecar is macOS-only in Phase 1 (plan D9); target=$target"
		;;
	esac
}

main "$@"

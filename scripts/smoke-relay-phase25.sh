#!/usr/bin/env bash
#
# Picot relay phase-2.5 — real-relay end-to-end driver (five protocol items).
#
# Runs `src-tauri/crates/picot-host/tests/relay_real_relay_e2e.rs` against the
# **company relay** (`paseo.021521.xyz:443`) with a real `picot-daemon` process
# on a throwaway isolation root. The test owns the admin channel (loopback WS)
# and the per-host bridge proxy; this script owns the process, the root, and
# the post-run hygiene checks (no leftover daemon, no leftover instance/lock
# file, no credential in any log).
#
# The daemon's `HOME` is the isolation root's `home/`, so its `~/.pi` and
# platform logs never touch the developer's real state. The admin token is read
# by the test from the instance file — it never appears in argv or output.
#
# Usage:
#   scripts/smoke-relay-phase25.sh [--iso ROOT] [--keep] [--no-build]
#
# Exit code = number of failed checks (0 = all green).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(dirname "$SCRIPT_DIR")"
TAURI="$REPO/src-tauri"
DAEMON_BIN="$TAURI/target/debug/picot-daemon"

ISO=""
KEEP=0
BUILD=1
while [ $# -gt 0 ]; do
	case "$1" in
	--iso)
		ISO="$2"
		shift 2
		;;
	--keep)
		KEEP=1
		shift
		;;
	--no-build)
		BUILD=0
		shift
		;;
	-h | --help)
		sed -n '2,25p' "${BASH_SOURCE[0]}"
		exit 0
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if [ -z "$ISO" ]; then
	ISO="$HOME/tmp/picot-relay-25-$(date +%Y%m%d-%H%M%S)"
fi

FAILED=0
say() { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
check() { # check "<what>" <0|1>
	if [ "$2" = 0 ]; then
		say "ok   - $1"
	else
		say "FAIL - $1"
		FAILED=$((FAILED + 1))
	fi
}
wait_file() { # wait_file PATH SECONDS
	local waited=0
	while [ ! -f "$1" ] && [ "$waited" -lt $(($2 * 10)) ]; do
		sleep 0.1
		waited=$((waited + 1))
	done
	[ -f "$1" ]
}
wait_gone() { # wait_gone PID SECONDS
	local waited=0
	while kill -0 "$1" 2>/dev/null && [ "$waited" -lt $(($2 * 10)) ]; do
		sleep 0.1
		waited=$((waited + 1))
	done
	! kill -0 "$1" 2>/dev/null
}

step "0. isolation root"
mkdir -p "$ISO/home" "$ISO/data" "$ISO/static" "$ISO/workspace" || exit 2
echo "<html>picot e2e</html>" >"$ISO/static/index.html"
say "iso: $ISO"

if [ "$BUILD" = 1 ]; then
	step "1. build the daemon"
	(cd "$TAURI" && cargo build -p picot-daemon) || exit 2
fi
check "the daemon binary exists" "$([ -x "$DAEMON_BIN" ] && echo 0 || echo 1)"
[ -x "$DAEMON_BIN" ] || exit 2

step "2. start the daemon on the isolation root"
# The default dial plan is the company relay with TLS; PICOT_RELAY_ENDPOINT is
# pinned so a developer's shell override cannot redirect the run.
HOME="$ISO/home" PICOT_RELAY_ENDPOINT="paseo.021521.xyz:443" \
	"$DAEMON_BIN" --data-dir "$ISO/data" --config-dir "$ISO/data" --static-dir "$ISO/static" \
	>"$ISO/daemon.stdout.log" 2>&1 &
DPID=$!
INSTANCE="$ISO/data/picot-daemon.json"
LOCK="$ISO/data/picot-daemon.lock"
DLOG="$ISO/data/picot-daemon.log"
if wait_file "$INSTANCE" 20; then
	say "daemon pid=$DPID published $INSTANCE"
	check "the instance file is published" 0
else
	say "TIMEOUT waiting for $INSTANCE (see $ISO/daemon.stdout.log)"
	check "the instance file is published" 1
	kill -9 "$DPID" 2>/dev/null
	exit "$FAILED"
fi

step "3. relay e2e (five protocol items)"
PICOT_REAL_RELAY=1 PICOT_REAL_RELAY_ISO="$ISO" \
	timeout 400 cargo test --manifest-path "$TAURI/Cargo.toml" -p picot-host \
	--test relay_real_relay_e2e -- --nocapture
TEST_CODE=$?
say "cargo test exit code: $TEST_CODE"
check "the five real-relay acceptance items pass (exit $TEST_CODE)" \
	"$([ "$TEST_CODE" = 0 ] && echo 0 || echo 1)"

step "4. teardown hygiene"
# The suite ends with `daemon.stop`; a failed run leaves the daemon alive and
# this SIGTERM drives the same graceful drain.
if kill -0 "$DPID" 2>/dev/null; then
	say "the daemon was still running (test did not stop it): SIGTERM pid=$DPID"
	kill -TERM "$DPID" 2>/dev/null
fi
wait_gone "$DPID" 20
wait "$DPID" 2>/dev/null
CODE=$?
check "the daemon process is gone" "$(! kill -0 "$DPID" 2>/dev/null && echo 0 || echo 1)"
check "the daemon exited 0 (exit code $CODE)" "$([ "$CODE" = 0 ] && echo 0 || echo 1)"
check "the instance file was removed" "$([ ! -e "$INSTANCE" ] && echo 0 || echo 1)"
check "the lock file was removed" "$([ ! -e "$LOCK" ] && echo 0 || echo 1)"

step "5. credential redaction in the daemon's own logs"
for LOG in "$DLOG" "$ISO/daemon.stdout.log"; do
	[ -f "$LOG" ] || continue
	LEAKS=$(grep -cE "picot_device_[A-Za-z0-9]{8,}|picot_pair_[A-Za-z0-9]{8,}" "$LOG")
	say "$(basename "$LOG"): $LEAKS credential-shaped token(s)"
	check "$(basename "$LOG") carries no device/pairing token" \
		"$([ "$LEAKS" = 0 ] && echo 0 || echo 1)"
done

if [ "$KEEP" = 0 ]; then
	rm -rf "$ISO"
else
	say "kept: $ISO"
fi

say ""
if [ "$FAILED" = 0 ]; then
	say "SMOKE OK"
else
	say "SMOKE FAILED: $FAILED check(s)"
fi
exit "$FAILED"

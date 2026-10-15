#!/usr/bin/env bash
#
# Picot daemon Phase 1 — isolated DMG smoke (plan Task 11 Step 4).
#
# Proves the packaged app really drives its sidecar: the .app is copied out of
# the DMG, launched with `HOME` pointing at a throwaway directory, and the
# daemon's whole lifecycle is asserted from process tables + its own logs:
#
#   spawn → /health 200 → SIGKILL the app (daemon survives, reparented) →
#   relaunch adopts the *same* daemon → graceful quit stops it → nothing left.
#
# Everything lands under /tmp. The **red line** is the developer's real state:
# `$HOME/Library/Application Support/com.palandata.picot` (the real app data
# dir), `~/.pi` (real agent state) and `/Applications/Picot.app` are never
# written to; the script asserts the real data dir's mtime and sqlite hash are
# unchanged before it reports success. The isolation root must be a **new**
# directory with no symlink components (see caveat 1 at the end of this file:
# `/tmp` is a symlink on macOS and the packaged app then cannot resolve its
# resource dir); it defaults to `$HOME/tmp/picot-dmg-smoke-XXXXXX`.
#
# Usage:
#   scripts/smoke-dmg-isolated.sh [--dmg PATH] [--iso ROOT] [--keep]
#
# Exit code = number of failed checks (0 = all green).
#
# Two environment facts this scripts around, both verified the hard way:
#
#   1. The release build drives the sidecar by itself (T12 deleted the
#      development-only in-process daemon mode and its environment switch):
#      there is no variable to set, so the smoke asserts the path every build
#      takes.
#   2. `-ApplePersistenceIgnoreState YES` is passed in argv. After any crash of
#      the app, macOS writes `~/Library/Application Support/CrashReporter/
#      picot_*.plist` and the next launch blocks forever on an AppKit
#      "quit unexpectedly — reopen windows?" modal *before* Tauri's setup runs
#      (verified with `sample`). The default suppresses that restore pass, so a
#      headless run cannot hang on a dialog.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(dirname "$SCRIPT_DIR")"

DMG=""
ISO=""
ISO_GIVEN=0
KEEP=0
while [ $# -gt 0 ]; do
	case "$1" in
	--dmg)
		DMG="$2"
		shift 2
		;;
	--iso)
		ISO="$2"
		ISO_GIVEN=1
		shift 2
		;;
	--keep)
		KEEP=1
		shift
		;;
	-h | --help)
		sed -n '2,40p' "${BASH_SOURCE[0]}"
		exit 0
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if [ -z "$DMG" ]; then
	DMG=$(ls -t "$REPO"/src-tauri/target/*/release/bundle/dmg/Picot_*.dmg 2>/dev/null | head -n 1)
fi
if [ -z "$DMG" ] || [ ! -f "$DMG" ]; then
	echo "error: no DMG found — pass --dmg PATH (or run scripts/build.sh mac-arm)" >&2
	exit 2
fi
if [ -z "$ISO" ]; then
	ISO=$(mktemp -d "${PICOT_SMOKE_HOME:-$HOME/tmp}/picot-dmg-smoke-XXXXXX")
fi
# The root must not pre-exist: a name the developer actually uses (an app data
# dir, a checkout) is never touched by accident. The default above is the one
# directory this script creates itself.
if [ "$ISO_GIVEN" = 1 ] && [ -e "$ISO" ]; then
	echo "error: the isolation root must not exist yet (got $ISO)" >&2
	exit 2
fi
# A symlinked component anywhere in the path breaks the packaged app's
# resource-dir resolution (`/tmp` -> `/private/tmp` on macOS): the daemon then
# gets a CWD-relative static dir. Refuse up front instead of mis-testing.
ISO_REAL=$(python3 -c 'import os,sys;print(os.path.realpath(sys.argv[1]))' "$ISO")
if [ "$ISO_REAL" != "$ISO" ]; then
	echo "error: the isolation root must contain no symlink components (got $ISO -> $ISO_REAL)" >&2
	exit 2
fi

APP="$ISO/app/Picot.app"
EHOME="$ISO/home"
DATA="$EHOME/Library/Application Support/com.palandata.picot"
DLOG="$DATA/picot-daemon.log"
INST="$DATA/picot-daemon.json"
EVIDENCE="$ISO.evidence"
REAL_DATA="$HOME/Library/Application Support/com.palandata.picot"
REAL_DB="$REAL_DATA/picot.sqlite3"

T0=$(date +%s)
FAILS=0
CHECKS=()
APP1=""
APP2=""
LAST_PID=""
APP_BIN=""

say() { echo "[t+$(( $(date +%s) - T0 ))s] $*"; }
step() {
	echo
	echo "=========== $* ==========="
}
check() { # label 0|1 [detail]
	if [ "$2" = 0 ]; then
		CHECKS+=("PASS  $1")
		echo "  PASS  $1"
	else
		CHECKS+=("FAIL  $1")
		echo "  FAIL  $1 ${3:-}"
		FAILS=$((FAILS + 1))
	fi
}
psrow() { ps -o pid=,ppid=,stat=,command= -p "${1:-0}" 2>/dev/null | cut -c1-150; }
inst_field() { python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get(sys.argv[2],""))' "$INST" "$1" 2>/dev/null; }

kill_app() { # pid [signal]
	[ -n "${1:-}" ] || return 0
	kill "-${2:-9}" "$1" 2>/dev/null
}
launch_app() { # logfile -> LAST_PID (APP is the bundle path, never the pid)
	HOME="$EHOME" "$APP_BIN" \
		-ApplePersistenceIgnoreState YES >>"$1" 2>&1 &
	LAST_PID=$!
}
wait_for_line() { # file pattern seconds
	local i=0
	while [ "$i" -lt "$3" ]; do
		grep -q "$2" "$1" 2>/dev/null && return 0
		sleep 1
		i=$((i + 1))
	done
	return 1
}

cleanup() {
	kill_app "$APP1"
	kill_app "$APP2"
	[ "$KEEP" = 1 ] && return 0
	rm -rf "$ISO" 2>/dev/null
}
trap cleanup EXIT

step "0. preflight"
say "dmg:  $DMG"
say "iso:  $ISO"
mkdir -p "$ISO/app" "$EHOME" "$EVIDENCE"
APP_BIN="$APP/Contents/MacOS/picot"
REAL_MTIME_BEFORE=$(stat -f '%m' "$REAL_DATA" 2>/dev/null)
REAL_DB_BEFORE=$(shasum -a 256 "$REAL_DB" 2>/dev/null | awk '{print $1}')
say "real app data (must stay untouched): $REAL_DATA mtime=$REAL_MTIME_BEFORE db=${REAL_DB_BEFORE:0:16}"

MOUNT=$(mktemp -d /tmp/picot-dmg-mnt-XXXXXX)
if ! hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$MOUNT" >/dev/null; then
	echo "error: cannot mount $DMG" >&2
	exit 2
fi
ditto "$MOUNT/Picot.app" "$APP"
hdiutil detach "$MOUNT" >/dev/null
rmdir "$MOUNT"
say "copied Picot.app out of the DMG"
ls -la "$APP/Contents/MacOS/" | tail -n +2

step "1. launch 1 (isolated HOME)"
ALOG1="$EVIDENCE/app-run1.log"
launch_app "$ALOG1"
APP1=$LAST_PID
say "app pid=$APP1"
if wait_for_line "$ALOG1" "\[picot\] landing ready" 60; then
	say "landing ready"
else
	say "TIMEOUT: no landing ready (dumping log tail)"
	tail -30 "$ALOG1"
fi
grep -E "\[picot\] (started picot-daemon|external daemon|landing ready)" "$ALOG1" | sed 's/^/    /'
check "app reached landing-ready" "$(grep -q "\[picot\] landing ready" "$ALOG1" && echo 0 || echo 1)"

step "2. the sidecar: process, path, isolation"
DAEMON=$(inst_field pid)
HOST=$(inst_field hostOrigin)
say "instance file: pid=${DAEMON:-MISSING} hostOrigin=${HOST:-MISSING}"
psrow "$DAEMON" | sed 's/^/    /'
DPPID=$(ps -o ppid= -p "${DAEMON:-0}" 2>/dev/null | tr -d ' ')
check "sidecar process is running" "$(kill -0 "${DAEMON:-0}" 2>/dev/null && echo 0 || echo 1)"
check "sidecar is the copy inside the DMG app" \
	"$(ps -o command= -p "${DAEMON:-0}" 2>/dev/null | grep -q "$APP/Contents/MacOS/picot-daemon" && echo 0 || echo 1)"
check "sidecar name has no target-triple suffix" \
	"$([ ! -e "$APP/Contents/MacOS/picot-daemon-aarch64-apple-darwin" ] && echo 0 || echo 1)"
SIDECAR_STATIC=$(ps -o command= -p "${DAEMON:-0}" 2>/dev/null | sed -n 's/.*--static-dir \([^ ]*\).*/\1/p')
say "daemon --static-dir=$SIDECAR_STATIC"
grep -E "\[picot-daemon\] (boot|ready)" "$DLOG" | sed 's/^/    /'
check "daemon data-dir is the isolated HOME" \
	"$(grep -q "data-dir=$DATA" "$DLOG" && echo 0 || echo 1)"
check "daemon static-dir is the bundle's Resources/public" \
	"$([ "$SIDECAR_STATIC" = "$APP/Contents/Resources/public" ] && echo 0 || echo 1)" "(got '$SIDECAR_STATIC')"
check "daemon reuses the app's pi tree (Resources/pi)" \
	"$(grep -q "static-dir=$APP/Contents/Resources/public" "$DLOG" && [ -x "$APP/Contents/Resources/pi/pi" ] && echo 0 || echo 1)"
check "daemon is detached from the app's process group (own pgid)" \
	"$([ "$(ps -o pgid= -p "${DAEMON:-0}" 2>/dev/null | tr -d ' ')" = "${DAEMON:-x}" ] && echo 0 || echo 1)"

step "3. /health and the served UI"
HTTP=$(curl -s -o "$EVIDENCE/health.json" -w '%{http_code}' "$HOST/health")
say "GET $HOST/health -> HTTP $HTTP"
head -c 200 "$EVIDENCE/health.json" 2>/dev/null
echo
check "/health returns 200" "$([ "$HTTP" = 200 ] && echo 0 || echo 1)" "(got $HTTP)"
UI=$(curl -s -o "$EVIDENCE/index.html" -w '%{http_code}' "$HOST/index.html")
say "GET $HOST/index.html -> HTTP $UI"
check "the window's UI is served from the bundle (not the CWD)" \
	"$(cmp -s "$EVIDENCE/index.html" "$APP/Contents/Resources/public/index.html" && echo 0 || echo 1)" \
	"(HTTP $UI)"

step "4. SIGKILL the app — the daemon must survive"
kill_app "$APP1" 9
APP1=""
sleep 3
DPPID2=$(ps -o ppid= -p "${DAEMON:-0}" 2>/dev/null | tr -d ' ')
HTTP2=$(curl -s -o /dev/null -w '%{http_code}' "$HOST/health")
say "daemon ppid=$DPPID2  /health -> HTTP $HTTP2"
tail -3 "$DLOG" | sed 's/^/    /'
check "daemon alive after the app was SIGKILLed" "$(kill -0 "${DAEMON:-0}" 2>/dev/null && echo 0 || echo 1)"
check "daemon reparented to launchd (ppid 1)" "$([ "${DPPID2:-x}" = 1 ] && echo 0 || echo 1)" "(ppid=$DPPID2)"
check "/health still 200" "$([ "$HTTP2" = 200 ] && echo 0 || echo 1)" "(got $HTTP2)"

step "5. relaunch — adopt, do not respawn"
ALOG2="$EVIDENCE/app-run2.log"
launch_app "$ALOG2"
APP2=$LAST_PID
say "app pid=$APP2"
wait_for_line "$ALOG2" "\[picot\] landing ready" 60 || say "TIMEOUT: no landing ready on run 2"
sleep 2
DAEMON2=$(inst_field pid)
say "daemon pid=${DAEMON2:-MISSING} (was ${DAEMON:-MISSING}); boot lines=$(grep -c '\[picot-daemon\] boot' "$DLOG")"
grep -E "\[picot\] (adopted picot-daemon|started picot-daemon)|attached to the Picot daemon admin channel" "$ALOG2" | sed 's/^/    /'
check "relaunch adopted the same daemon" \
	"$([ -n "$DAEMON2" ] && [ "$DAEMON2" = "$DAEMON" ] && echo 0 || echo 1)" "($DAEMON2 vs $DAEMON)"
check "adoption went through the admin lease (epoch logged)" \
	"$(grep -q "adopted picot-daemon pid=$DAEMON" "$ALOG2" && echo 0 || echo 1)"
check "no extra daemon boot (single boot line)" \
	"$([ "$(grep -c '\[picot-daemon\] boot' "$DLOG")" = 1 ] && echo 0 || echo 1)"

step "6. graceful quit (SIGTERM == the Cmd-Q policy; keepRunningAfterQuit defaults off)"
kill_app "$APP2" 15
for i in $(seq 1 25); do
	kill -0 "$APP2" 2>/dev/null || break
	sleep 1
done
say "app exited after ${i}s"
APP2=""
sleep 2
psrow "$DAEMON" | sed 's/^/    /'
grep -E "\[picot\] .*(quit|stopping|daemon)" "$ALOG2" | tail -5 | sed 's/^/    /'
tail -4 "$DLOG" | sed 's/^/    /'
check "daemon stopped by the quit policy" "$(kill -0 "${DAEMON:-0}" 2>/dev/null && echo 1 || echo 0)"
# `daemon.stop` drains through `DaemonState::drain` (the reply is "accepted",
# then the drain runs), so the process's own `exit code 0 (daemon.stop)` line is
# the graceful-exit evidence on this path; `shutdown_graceful`'s "graceful
# shutdown" line belongs to the signal path only (picot-daemon/src/main.rs).
check "daemon drained and exited cleanly on daemon.stop" \
	"$(grep -q "\[picot-daemon\] exit code 0 (daemon.stop)" "$DLOG" && echo 0 || echo 1)"
check "instance file removed" "$([ ! -f "$INST" ] && echo 0 || echo 1)"
check "lock file removed" "$([ ! -f "$DATA/picot-daemon.lock" ] && echo 0 || echo 1)"

step "7. no isolated process survives"
pgrep -fl "$ISO" | grep -v "smoke-dmg-isolated" | sed 's/^/    /'
check "no picot process left under $ISO" \
	"$(pgrep -f "$ISO/app/Picot.app" >/dev/null 2>&1 && echo 1 || echo 0)"

step "8. red line: the developer's real state"
REAL_MTIME_AFTER=$(stat -f '%m' "$REAL_DATA" 2>/dev/null)
REAL_DB_AFTER=$(shasum -a 256 "$REAL_DB" 2>/dev/null | awk '{print $1}')
say "mtime $REAL_MTIME_BEFORE -> $REAL_MTIME_AFTER ; db ${REAL_DB_BEFORE:0:16} -> ${REAL_DB_AFTER:0:16}"
check "real app data dir untouched (mtime + sqlite hash)" \
	"$([ "$REAL_MTIME_BEFORE" = "$REAL_MTIME_AFTER" ] && [ "$REAL_DB_BEFORE" = "$REAL_DB_AFTER" ] && echo 0 || echo 1)"
check "real ~/.pi untouched" \
	"$(find "$HOME/.pi" -newermt "@$T0" -print -quit 2>/dev/null | grep -q . && echo 1 || echo 0)"

step "9. summary"
printf '%s\n' "${CHECKS[@]}"
say "FAILS=$FAILS (evidence: $EVIDENCE)"
say "cleanup: ${ISO} (app copy + isolated HOME)"
exit "$FAILS"

# Known launch-context caveats (2026-10-16, from the T11 smoke session):
# 1. Copy the .app to a path with NO symlink components (/tmp is a symlink on
#    macOS and tauri's StartingBinary rejects symlinked exe paths, which makes
#    resource_dir() fail -> static bundle resolution dies, and the daemon is
#    handed a CWD-relative `public`). The script now refuses such a root.
# 2. Repeated SIGKILL of the app writes crash reports; macOS may gate startup
#    behind an invisible persistent-state restore prompt when the app is
#    launched outside LaunchServices. Clean DiagnosticReports/picot-*.ips
#    between kill-matrix runs, or drive the final matrix via Finder/`open`.
# 3. There is no mode switch to set any more: the app always attaches to the
#    sidecar (T12).

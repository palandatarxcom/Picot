#!/usr/bin/env bash
# ABOUTME: Stops a Picot daemon that outlived a killed Picot app, then clears its stale
# ABOUTME: instance and lock files. Verifies the recorded pid still belongs to picot-daemon first.

# The daemon is spawned detached in its own process group, so `kill -9 Picot`
# leaves it running on purpose. This script is the manual way out: it reads the
# daemon's own instance file, proves that pid still runs picot-daemon (a reused
# pid must never be signalled), asks before doing anything, and only then stops
# it and removes the files a dead daemon leaves behind. The log is kept.

set -uo pipefail

DATA_DIR=""
ASSUME_YES=0
DRY_RUN=0
GRACE_SECONDS=10

usage() {
	cat <<'USAGE'
Usage: cleanup-picot-daemon.sh [--data-dir DIR] [--yes] [--dry-run] [--grace SECONDS]

  --data-dir DIR   Picot app data directory (default: derived from this app bundle)
  --yes, -y        Do not ask for confirmation
  --dry-run        Report what would happen and change nothing
  --grace SECONDS  Time a SIGTERM gets before escalating to SIGKILL (default 10)
USAGE
}

while [ $# -gt 0 ]; do
	case "$1" in
	--data-dir)
		DATA_DIR="${2:-}"
		shift 2
		;;
	--yes | -y)
		ASSUME_YES=1
		shift
		;;
	--dry-run)
		DRY_RUN=1
		shift
		;;
	--grace)
		GRACE_SECONDS="${2:-10}"
		shift 2
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "cleanup-picot-daemon: unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
done

die() {
	echo "cleanup-picot-daemon: $*" >&2
	exit 1
}

# The bundle identifier decides the data directory, and reading it from the
# enclosing Info.plist keeps this script correct if the identifier ever changes.
default_data_dir() {
	local here plist identifier
	here=$(cd "$(dirname "$0")" && pwd)
	plist="$here/../Info.plist"
	identifier=""
	if [ -f "$plist" ] && [ -x /usr/libexec/PlistBuddy ]; then
		identifier=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$plist" 2>/dev/null || true)
	fi
	if [ -z "$identifier" ]; then
		# Fallback for running the script outside the bundle. It must stay in
		# sync with `identifier` in src-tauri/tauri.conf.json.
		identifier="com.palandata.picot"
	fi
	printf '%s' "$HOME/Library/Application Support/$identifier"
}

# Only the pid is read: the same file carries the daemon's admin token and that
# must never reach this script's output.
instance_pid() {
	local file="$1" value
	if command -v plutil >/dev/null 2>&1; then
		value=$(plutil -extract pid raw -o - "$file" 2>/dev/null || true)
	fi
	if [ -z "${value:-}" ] && command -v python3 >/dev/null 2>&1; then
		value=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["pid"])' "$file" 2>/dev/null || true)
	fi
	printf '%s' "${value:-}"
}

# A recorded pid is only trustworthy while it still names picot-daemon. Anything
# else means the daemon is gone (or the pid was reused) and nothing may be
# signalled.
process_name() {
	ps -p "$1" -o comm= 2>/dev/null | awk 'NR==1 {print $1}'
}

pid_alive() {
	kill -0 "$1" 2>/dev/null
}

is_daemon_pid() {
	local pid="$1" name base
	name=$(process_name "$pid")
	[ -n "$name" ] || return 1
	base=$(basename "$name")
	[ "$base" = "picot-daemon" ]
}

remove_dead_files() {
	local instance="$1" lock="$2"
	if [ "$DRY_RUN" = "1" ]; then
		echo "Would remove $instance"
		echo "Would remove $lock"
		return 0
	fi
	rm -f "$instance" "$lock"
	echo "Removed stale instance and lock files."
}

if [ -z "$DATA_DIR" ]; then
	DATA_DIR=$(default_data_dir)
fi

INSTANCE="$DATA_DIR/picot-daemon.json"
LOCK="$DATA_DIR/picot-daemon.lock"
LOG="$DATA_DIR/picot-daemon.log"

if [ ! -f "$INSTANCE" ]; then
	echo "No daemon instance file at $INSTANCE"
	echo "Nothing to clean up: no Picot daemon is recorded for this installation."
	exit 0
fi

PID=$(instance_pid "$INSTANCE")
case "$PID" in
'' | *[!0-9]*)
	die "the instance file has no usable pid ($INSTANCE); inspect it by hand"
	;;
esac

if ! pid_alive "$PID"; then
	echo "Picot daemon pid $PID is not running; the instance file is stale."
	remove_dead_files "$INSTANCE" "$LOCK"
	echo "Daemon log kept at $LOG"
	exit 0
fi

if ! is_daemon_pid "$PID"; then
	die "pid $PID is alive but is not picot-daemon (pid reuse?); refusing to signal it.
Inspect $INSTANCE by hand before removing it; nothing was changed."
fi

echo "Picot daemon is running as pid $PID"
if [ "$DRY_RUN" = "1" ]; then
	echo "Would send SIGTERM, wait up to ${GRACE_SECONDS}s, escalate to SIGKILL if needed,"
	echo "then remove $INSTANCE and $LOCK (log kept at $LOG)."
	exit 0
fi

if [ "$ASSUME_YES" != "1" ]; then
	if [ ! -t 0 ]; then
		die "confirmation required; re-run with --yes when stdin is not a terminal"
	fi
	printf 'Stop it now? [y/N] '
	read -r answer
	case "$answer" in
	y | Y | yes | YES) ;;
	*)
		echo "Cancelled; nothing was changed."
		exit 1
		;;
	esac
fi

kill -TERM "$PID" 2>/dev/null || die "SIGTERM failed for pid $PID"

waited=0
while pid_alive "$PID" && [ "$waited" -lt "$GRACE_SECONDS" ]; do
	sleep 1
	waited=$((waited + 1))
done

if pid_alive "$PID"; then
	# Re-check identity: the daemon may have exited and handed the pid to an
	# unrelated process while we waited.
	is_daemon_pid "$PID" || die "pid $PID changed identity while waiting; refusing to SIGKILL"
	echo "Daemon did not stop within ${GRACE_SECONDS}s; sending SIGKILL."
	pgid=$(ps -p "$PID" -o pgid= 2>/dev/null | awk 'NR==1 {print $1}')
	if [ "${pgid:-}" = "$PID" ]; then
		# The daemon leads its own process group (spawned with process_group(0)),
		# so signalling the group also reaches the pi children it started.
		kill -KILL "-$PID" 2>/dev/null || kill -KILL "$PID" 2>/dev/null || true
	else
		kill -KILL "$PID" 2>/dev/null || true
	fi
	sleep 1
fi

if pid_alive "$PID"; then
	die "pid $PID is still alive; no files were removed"
fi

remove_dead_files "$INSTANCE" "$LOCK"
echo "Picot daemon stopped. Log kept at $LOG"

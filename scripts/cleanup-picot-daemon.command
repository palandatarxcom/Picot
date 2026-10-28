#!/usr/bin/env bash
# ABOUTME: Finder entry point for the daemon cleanup helper shipped in Picot.app.
# ABOUTME: Runs the sibling cleanup-picot-daemon.sh and keeps the window open on failure.

here=$(cd "$(dirname "$0")" && pwd)
if bash "$here/cleanup-picot-daemon.sh" "$@"; then
	status=0
else
	status=$?
fi

# Double-clicked from Finder the window would vanish before the operator can
# read the outcome, so pause only when this is an interactive terminal.
if [ -t 0 ]; then
	printf '\nPress Return to close this window. '
	read -r _
fi
exit "$status"

#!/usr/bin/env bash
# Print Picot's Pi-upstream integration surface as a stable, copy-pasteable list.
# Used by the pi-upgrade-impact skill. Run from the Picot repository root.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

EXCLUDES="node_modules|extensions/dist/|src-tauri/target/|\\.test\\."

section() { printf '\n=== %s ===\n' "$1"; }

section "Pinned Pi version"
head -5 scripts/pi-version.json

section "Direct SDK imports"
grep -rn "@earendil-works/pi-coding-agent" extensions/ public/ src-tauri/src/ src-tauri/crates/picot-host/src/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

section "SessionManager usage"
grep -rn "SessionManager\." extensions/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

section "Extension registrations"
grep -rn "registerCommand\|registerTool\|registerFlag" extensions/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

section "Extension ctx calls"
grep -rn "ctx\.\(navigateTree\|sendUserMessage\|reload\|ui\)" extensions/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

section "WebView session events consumed"
grep -rn "message_start\|message_update\|message_end\|tool_execution_start\|tool_execution_update\|tool_execution_end\|agent_start\|agent_end\|model_select\|thinking_level_select" public/app/ extensions/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

section "Extension event listeners"
grep -rn "pi\.on(\|on(\"agent\|on(\"tool\|on(\"model\|on(\"thinking" extensions/ 2>/dev/null | grep -Ev "$EXCLUDES" || true

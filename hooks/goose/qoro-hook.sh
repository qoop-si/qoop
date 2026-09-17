#!/bin/sh
# qoro hook for goose.
#
# Install: create a plugin directory with a `hooks/hooks.json` mapping the
# lifecycle events to this script, project scope at
# `.agents/plugins/qoro/hooks/hooks.json` or user scope at
# `~/.agents/plugins/qoro/hooks/hooks.json`:
#
#   { "hooks": {
#       "SessionStart": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }],
#       "UserPromptSubmit": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }],
#       "PreToolUse": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }],
#       "PostToolUse": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }],
#       "Stop": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }],
#       "SessionEnd": [{ "hooks": [{ "type": "command",
#         "command": "${PLUGIN_ROOT}/qoro-hook.sh" }] }]
#   } }
#
# goose pipes the event payload as JSON on stdin (`event`, `session_id`,
# `tool_name`, ...). Subagent lifecycle events are not emitted by goose, so
# there is nothing to filter here.
# Event reference: https://goose-docs.ai/docs/guides/context-engineering/hooks/

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
command -v qoro >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

report="$(python3 -c '
import json, sys
try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
name = payload.get("event") or ""
if name in ("SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse"):
    print("working --visible-working")
elif name in ("Stop", "SessionEnd"):
    print("idle --visible-idle")
else:
    raise SystemExit(1)
' 2>/dev/null)" || exit 0

# shellcheck disable=SC2086
qoro hook-report --state $report >/dev/null 2>&1 || true
exit 0

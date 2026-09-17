#!/bin/sh
# qoro hook for Claude Code.
#
# Install: add to `.claude/settings.json` (project) or `~/.claude/settings.json`
# (user), one matcher per event:
#
#   {
#     "hooks": {
#       "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PreToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PostToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "Notification": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "SubagentStop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "Stop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "SessionEnd": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }]
#     }
#   }
#
# Reads Claude's hook JSON from stdin (`hook_event_name`), reports through
# `qoro hook-report`, and exits zero in every failure case: a hook that errors
# fails open and the screen reading keeps working with no hooks installed.
# Event reference: https://docs.anthropic.com/en/docs/claude-code/hooks

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
name = payload.get("hook_event_name") or ""
if name in ("SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse",
             "SubagentStop"):
    print("working --visible-working")
elif name in ("Notification", "PermissionRequest"):
    print("blocked --visible-blocker")
elif name in ("Stop", "SessionEnd"):
    print("idle --visible-idle")
else:
    raise SystemExit(1)
' 2>/dev/null)" || exit 0

# shellcheck disable=SC2086
qoro hook-report --state $report >/dev/null 2>&1 || true
exit 0

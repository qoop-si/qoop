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
# there is nothing to filter here. The `session_id` is passed through as
# `--session-id`, which is what lets a tab come back to this conversation
# after a restart rather than to whichever one is newest.
# Event reference: https://goose-docs.ai/docs/guides/context-engineering/hooks/

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
# The installer puts qoro in ~/.local/bin, which a stock Mac does not have on PATH until
# the person adds it: looked for there too, rather than every hook going quiet.
command -v qoro >/dev/null 2>&1 || PATH="${HOME:-/nonexistent}/.local/bin:$PATH"
command -v qoro >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

report="$(python3 -c '
import json, sys
try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
name = payload.get("event") or ""
# A session starting is not a turn, so an idle screen after it is not news.
if name == "SessionStart":
    state = "starting"
elif name in ("UserPromptSubmit", "PreToolUse", "PostToolUse"):
    state = "working --visible-working"
elif name in ("Stop", "SessionEnd"):
    state = "idle --visible-idle"
else:
    raise SystemExit(1)
# The conversation this report belongs to. Refused unless it looks like an
# id and nothing else: this string ends up on a command line.
session = str(payload.get("session_id") or "")
if session and len(session) <= 128 and all(
    c.isalnum() or c in "-_." for c in session
):
    state += " --session-id " + session
print(state)
' 2>/dev/null)" || exit 0

# shellcheck disable=SC2086
qoro hook-report --state $report >/dev/null 2>&1 || true
exit 0

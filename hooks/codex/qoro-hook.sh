#!/bin/sh
# qoro hook for Codex CLI.
#
# Codex command hooks run per event, so the event-to-state mapping lives in
# the hooks config rather than in payload parsing here. Install in
# `~/.codex/hooks.json` (user) or `.codex/hooks.json` (project, trusted):
#
#   { "hooks": {
#       "SessionStart": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh working" }] }],
#       "PermissionRequest": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh blocked" }] }],
#       "Stop": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh idle" }] }]
#   } }
#
# plus optional `PreToolUse`/`PostToolUse` entries reporting `working`.
# Put this script's directory on PATH or use an absolute command path.
# A hook that errors fails open and the screen reading keeps working.
# Event reference: https://developers.openai.com/codex/hooks
#
# Every hook payload carries `session_id` on stdin. It is passed through as
# `--session-id`, which is what lets a tab come back to this conversation
# after a restart rather than to whichever one in the directory is newest.
# Refused unless it looks like an id and nothing else: without python3, or
# with a payload that has none, the report goes out with no id, same as
# before.

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
command -v qoro >/dev/null 2>&1 || exit 0

state="${1:-}"
case "$state" in
  working) flags="--visible-working" ;;
  blocked) flags="--visible-blocker" ;;
  idle) flags="--visible-idle" ;;
  starting | done | failed | unknown) flags="" ;;
  *) exit 0 ;;
esac

session=""
if command -v python3 >/dev/null 2>&1; then
  session="$(python3 -c '
import json, sys
try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(1)
session = str(payload.get("session_id") or "")
if session and len(session) <= 128 and all(
    c.isalnum() or c in "-_." for c in session
):
    print("--session-id " + session)
' 2>/dev/null)" || session=""
fi

# shellcheck disable=SC2086
qoro hook-report --state "$state" $flags $session >/dev/null 2>&1 || true
exit 0

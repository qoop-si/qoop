#!/bin/sh
# qoop hook for cursor-agent (Cursor CLI).
#
# The CLI supports a subset of the IDE's hook events; `sessionStart`, `stop`
# and `postToolUse` fire on recent builds, older builds only fire shell hooks
# (which carry no state and are not mapped here). Events that never fire simply
# never report: the screen reading covers those. Install in
# `~/.cursor/hooks.json` (user) or `.cursor/hooks.json` (project):
#
#   { "version": 1, "hooks": {
#       "sessionStart": [{ "command": "qoop-hook.sh starting" }],
#       "postToolUse": [{ "command": "qoop-hook.sh working" }],
#       "stop": [{ "command": "qoop-hook.sh idle" }]
#   } }
#
# Put this script's directory on PATH or use an absolute command path.
# A hook that errors fails open and the screen reading keeps working.
# Event reference: https://cursor.com/docs/hooks
#
# Every hook payload carries the session on stdin as `session_id` (the same
# value as `conversation_id`). It is passed through as `--session-id`, which
# is what lets a tab come back to this conversation after a restart rather
# than to whichever one in the directory is newest. Without an id the report
# goes out with no flag, same as before.

set -eu

# A terminal started before the rename carries the old names.
export QOOP_TERMINAL="${QOOP_TERMINAL:-${QORO_TERMINAL:-}}" QOOP_TOKEN="${QOOP_TOKEN:-${QORO_TOKEN:-}}"
[ -n "${QOOP_TERMINAL:-}" ] || exit 0
[ -n "${QOOP_TOKEN:-}" ] || exit 0
# The installer puts qoop in ~/.local/bin, which a stock Mac does not have on PATH until
# the person adds it: looked for there too, rather than every hook going quiet.
command -v qoop >/dev/null 2>&1 || PATH="${HOME:-/nonexistent}/.local/bin:$PATH"
command -v qoop >/dev/null 2>&1 || exit 0

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
session = str(payload.get("session_id") or payload.get("conversation_id") or "")
if session and len(session) <= 128 and all(
    c.isalnum() or c in "-_." for c in session
):
    print("--session-id " + session)
' 2>/dev/null)" || session=""
fi

# shellcheck disable=SC2086
qoop hook-report --agent cursor --state "$state" $flags $session >/dev/null 2>&1 || true
exit 0

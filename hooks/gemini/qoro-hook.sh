#!/bin/sh
# qoro hook for Gemini CLI.
#
# Gemini hooks run per event from `settings.json`, so the event-to-state
# mapping lives in the config rather than in payload parsing here. Install in
# `~/.gemini/settings.json` (user) or `.gemini/settings.json` (project):
#
#   { "hooks": {
#       "SessionStart": [{ "matcher": "startup", "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh working" }] }],
#       "BeforeAgent": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh working" }] }],
#       "AfterAgent": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh idle" }] }],
#       "SessionEnd": [{ "matcher": "exit", "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh idle" }] }]
#   } }
#
# Put this script's directory on PATH or use an absolute command path.
# A hook that errors fails open and the screen reading keeps working.
# Event reference: https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md
#
# Every hook payload carries `session_id` on stdin, and the CLI also sets
# `GEMINI_SESSION_ID`. Either is passed through as `--session-id`, which is
# what lets a restored tab ask for this conversation rather than the newest
# one in the directory. Without an id the report goes out with no flag,
# same as before.

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
  session="$(GEMINI_SESSION_ID="${GEMINI_SESSION_ID:-}" python3 -c '
import json, os, sys
try:
    payload = json.load(sys.stdin)
except Exception:
    payload = {}
session = str(payload.get("session_id") or os.environ.get("GEMINI_SESSION_ID") or "")
if session and len(session) <= 128 and all(
    c.isalnum() or c in "-_." for c in session
):
    print("--session-id " + session)
' 2>/dev/null)" || session=""
fi

# shellcheck disable=SC2086
qoro hook-report --state "$state" $flags $session >/dev/null 2>&1 || true
exit 0

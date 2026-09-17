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

# shellcheck disable=SC2086
qoro hook-report --state "$state" $flags >/dev/null 2>&1 || true
exit 0

#!/bin/sh
# qoro hook for cursor-agent (Cursor CLI).
#
# The CLI supports a subset of the IDE's hook events; `sessionStart`, `stop`
# and `postToolUse` fire on recent builds, older builds only fire shell hooks
# (which carry no state and are not mapped here). Events that never fire simply
# never report: the screen reading covers those. Install in
# `~/.cursor/hooks.json` (user) or `.cursor/hooks.json` (project):
#
#   { "version": 1, "hooks": {
#       "sessionStart": [{ "command": "qoro-hook.sh working" }],
#       "postToolUse": [{ "command": "qoro-hook.sh working" }],
#       "stop": [{ "command": "qoro-hook.sh idle" }]
#   } }
#
# Put this script's directory on PATH or use an absolute command path.
# A hook that errors fails open and the screen reading keeps working.
# Event reference: https://cursor.com/docs/hooks

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

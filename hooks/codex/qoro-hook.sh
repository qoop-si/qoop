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

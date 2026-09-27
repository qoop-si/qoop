#!/bin/sh
# qoro session hook, shared by GitHub Copilot CLI, Factory Droid, Qwen Code and
# Qoder CLI.
#
# Reports one thing: the agent's own id for its conversation, on SessionStart.
# That id is what lets a restored tab carry on this conversation rather than
# the newest one in the directory. It reports no state: the screen reading
# stays the only source of what the agent is doing.
#
# `qoro hooks install copilot droid qwen qodercli` writes this file once per
# agent and adds one SessionStart entry to that agent's user configuration:
#
#   copilot   ~/.copilot/settings.json
#     { "hooks": { "SessionStart": [{ "command": "/path/to/this.sh" }] } }
#   droid     ~/.factory/settings.json
#     { "hooks": { "SessionStart": [{ "hooks": [{ "type": "command",
#       "command": "/path/to/this.sh" }] }] } }
#   qwen      ~/.qwen/settings.json
#   qodercli  ~/.qoder/settings.json
#     { "hooks": { "SessionStart": [{ "matcher": "*", "hooks": [{
#       "type": "command", "command": "/path/to/this.sh" }] }] } }
#
# Every one of them hands the hook a JSON payload on stdin carrying
# `session_id` (Copilot does so for the PascalCase `SessionStart` event; its
# camelCase `sessionStart` payload says `sessionId`, and both are read).
# References:
#   https://docs.github.com/en/copilot/reference/hooks-reference
#   https://docs.factory.ai/cli/configuration/hooks-guide
#   https://qwenlm.github.io/qwen-code-docs/en/users/features/hooks/
#   https://docs.qoder.com/cli/hooks
#
# A hook that errors fails open: this exits zero in every case.

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
command -v qoro >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# Refused unless it looks like an id and nothing else: it ends up on a
# command line when the tab is restored.
session="$(python3 -c '
import json, sys
try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
if not isinstance(payload, dict):
    raise SystemExit(0)
event = str(payload.get("hook_event_name") or payload.get("hookEventName") or "")
if event and event.replace("_", "").lower() != "sessionstart":
    raise SystemExit(0)
session = str(payload.get("session_id") or payload.get("sessionId") or "")
# ASCII only, and never a leading dash, which a command line would read as a flag.
if (
    session
    and len(session) <= 128
    and not session.startswith("-")
    and all((c.isascii() and c.isalnum()) or c in "-_." for c in session)
):
    print(session)
' 2>/dev/null)" || session=""

[ -n "$session" ] || exit 0
qoro hook-report --session-id "$session" >/dev/null 2>&1 || true
exit 0

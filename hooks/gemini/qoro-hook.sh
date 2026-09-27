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
#       "BeforeTool": [{ "matcher": "*", "hooks": [{ "type": "command",
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
# one in the directory.
#
# The payload also names the event (`hook_event_name`). `BeforeTool` adds a
# focus event with the tool and its command or path as the label, which is
# what the sidebar shows under a working agent; `BeforeAgent` adds a `user`
# event with no label, so the prompt itself never leaves this hook. The state
# words are unchanged: the argument still decides the state.
#
# Without python3, or with a payload that does not parse, the report goes
# out with the state alone, same as before.

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

# The report runs inside python with an argv list, never through shell
# interpolation: a command or a path can hold spaces, quotes, `$`, backticks
# and newlines, and none of those may reach a shell word.
if command -v python3 >/dev/null 2>&1; then
  QORO_HOOK_STATE="$state" QORO_HOOK_FLAGS="$flags" \
    GEMINI_SESSION_ID="${GEMINI_SESSION_ID:-}" python3 -c '
import json, os, subprocess, sys

try:
    payload = json.load(sys.stdin)
except Exception:
    payload = {}
if not isinstance(payload, dict):
    payload = {}


def one_line(text, limit=120):
    return " ".join(str(text).split())[:limit]


def first_fileish(tool_input):
    if not isinstance(tool_input, dict):
        return ""
    for key in ("command", "file_path", "absolute_path", "pattern", "path", "dir_path", "url"):
        value = tool_input.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


args = ["qoro", "hook-report", "--state", os.environ["QORO_HOOK_STATE"]]
if os.environ.get("QORO_HOOK_FLAGS"):
    args.append(os.environ["QORO_HOOK_FLAGS"])
session = str(payload.get("session_id") or os.environ.get("GEMINI_SESSION_ID") or "")
if session and len(session) <= 128 and all(c.isalnum() or c in "-_." for c in session):
    args += ["--session-id", session]
name = payload.get("hook_event_name") or ""
if name == "BeforeTool":
    tool = str(payload.get("tool_name") or "").strip()
    arg = first_fileish(payload.get("tool_input"))
    label = one_line(f"{tool} {arg}".strip())
    args += ["--event", "tool"]
    if label:
        args += ["--label", label]
elif name == "BeforeAgent":
    args += ["--event", "user"]
elif name == "AfterAgent":
    # The turn ended: its last tool is not what the agent is doing any more.
    args += ["--event", "done"]
subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
' 2>/dev/null && exit 0
fi

# shellcheck disable=SC2086
qoro hook-report --state "$state" $flags >/dev/null 2>&1 || true
exit 0

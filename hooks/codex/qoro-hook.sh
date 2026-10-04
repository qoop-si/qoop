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
#       "UserPromptSubmit": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh working" }] }],
#       "PreToolUse": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh working" }] }],
#       "PermissionRequest": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh blocked" }] }],
#       "Stop": [{ "hooks": [{ "type": "command",
#         "command": "qoro-hook.sh idle" }] }]
#   } }
#
# plus an optional `PostToolUse` entry reporting `working`.
# Put this script's directory on PATH or use an absolute command path.
# A hook that errors fails open and the screen reading keeps working.
# Event reference: https://developers.openai.com/codex/hooks
#
# Every hook payload carries `session_id` on stdin. It is passed through as
# `--session-id`, which is what lets a tab come back to this conversation
# after a restart rather than to whichever one in the directory is newest.
# Refused unless it looks like an id and nothing else.
#
# The payload also names the event (`hook_event_name`). `PreToolUse` and
# `PermissionRequest` add a focus event with the tool and its command or
# path as the label, which is what the sidebar shows under a working agent;
# `UserPromptSubmit` adds a `user` event with no label, so the prompt itself
# never leaves this machine's hook. The state words are unchanged: the
# argument still decides the state.
#
# Without python3, or with a payload that does not parse, the report goes
# out with the state alone, same as before.

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
# The installer puts qoro in ~/.local/bin, which a stock Mac does not have on PATH until
# the person adds it: looked for there too, rather than every hook going quiet.
command -v qoro >/dev/null 2>&1 || PATH="${HOME:-/nonexistent}/.local/bin:$PATH"
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
  QORO_HOOK_STATE="$state" QORO_HOOK_FLAGS="$flags" python3 -c '
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
    for key in ("command", "cmd", "file_path", "path", "pattern"):
        value = tool_input.get(key)
        if isinstance(value, list):
            # A patch rides in the command list as its second part: file content,
            # never a label. The tool name says enough.
            if value[:1] == ["apply_patch"]:
                return "apply_patch"
            value = " ".join(str(part) for part in value)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


name = payload.get("hook_event_name") or ""
state = os.environ["QORO_HOOK_STATE"]
flags = os.environ.get("QORO_HOOK_FLAGS") or ""
# A session starting is not a turn. Said here and not only by the argument,
# because an entry installed earlier still passes `working` for it.
if name == "SessionStart":
    state, flags = "starting", ""
# An edit is real work in this terminal: qoro opens its task record now if it has none, so
# the record never waits on the agent to open it.
if name == "PreToolUse" and (payload.get("tool_name") == "apply_patch" or first_fileish(payload.get("tool_input")) == "apply_patch"):
    try:
        subprocess.run(["qoro", "task", "begin"], capture_output=True, timeout=2)
    except Exception:
        pass
args = ["qoro", "hook-report", "--state", state]
if flags:
    args.append(flags)
session = str(payload.get("session_id") or "")
if session and len(session) <= 128 and all(c.isalnum() or c in "-_." for c in session):
    args += ["--session-id", session]
if name in ("PreToolUse", "PermissionRequest"):
    tool = str(payload.get("tool_name") or "").strip()
    arg = first_fileish(payload.get("tool_input"))
    label = one_line(f"{tool} {arg}".strip())
    args += ["--event", "tool" if name == "PreToolUse" else "blocked"]
    if label:
        args += ["--label", label]
elif name == "UserPromptSubmit":
    args += ["--event", "user"]
    # The head of the request, as the Claude hook sends it: what a task qoro opens is
    # named after.
    prompt = one_line(payload.get("prompt") or "")
    if prompt:
        args += ["--label", prompt]
elif name == "Stop":
    # The turn ended: its last tool is not what the agent is doing any more.
    args += ["--event", "done"]
try:
    subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
except Exception:
    pass
# A session starting or resuming: tell the agent the short rules for recording its task,
# and the task it has open. After the report, so the state is never late for it; quiet on
# any failure, and qoro itself prints nothing in the qass space. Codex adds this output
# to what the agent reads.
# With every request too, one line about the task: given once, the rules were read and then
# not followed.
if name in ("SessionStart", "UserPromptSubmit"):
    ask = ["qoro", "task", "rules"] + (["--turn"] if name == "UserPromptSubmit" else [])
    try:
        got = subprocess.run(ask, capture_output=True, timeout=2, text=True)
        rules = got.stdout.strip()
        if got.returncode == 0 and rules:
            sys.stdout.write(rules + "\n")
    except Exception:
        pass
' 2>/dev/null && exit 0
fi

# shellcheck disable=SC2086
qoro hook-report --state "$state" $flags >/dev/null 2>&1 || true
exit 0

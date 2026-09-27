#!/bin/sh
# qoro hook for Claude Code.
#
# Install: add to `.claude/settings.json` (project) or `~/.claude/settings.json`
# (user), one matcher per event:
#
#   {
#     "hooks": {
#       "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PreToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PostToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "Notification": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "SubagentStop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "Stop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }],
#       "SessionEnd": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoro-hook.sh" }] }]
#     }
#   }
#
# Reads Claude's hook JSON from stdin (`hook_event_name`), reports through
# `qoro hook-report`, and exits zero in every failure case: a hook that errors
# fails open and the screen reading keeps working with no hooks installed.
# Event reference: https://docs.anthropic.com/en/docs/claude-code/hooks
#
# State mapping is unchanged by focus events: the state words below are what
# they always were, and `--event/--label` only adds a display row for the
# focus view. `Stop`/`SessionEnd` still report `idle`, with `--event done`
# beside it; the push behaviour does not move, because pushes derive from
# state alone.
#
# Event map:
#   SessionStart, SubagentStop -> working, no event
#   UserPromptSubmit -> working + `--event user` (prompt head, best-effort)
#   PreToolUse, PostToolUse -> working + `--event tool` (`tool_name` plus
#     the first file-ish `tool_input` field, a command's description before
#     the command itself, best-effort)
#   PermissionRequest -> blocked + `--event blocked` (tool label, best-effort)
#   Notification (`permission_prompt`, `elicitation_dialog`,
#     `elicitation_url_dialog`, `agent_needs_input`, `quota_auto_resume_stale`,
#     or no `notification_type` from an older build)
#     -> blocked + `--event blocked`, no label (it carries no tool); every other
#     notification type reports nothing
#   Stop, SessionEnd -> idle + `--event done`, no label

set -eu

[ -n "${QORO_TERMINAL:-}" ] || exit 0
[ -n "${QORO_TOKEN:-}" ] || exit 0
command -v qoro >/dev/null 2>&1 || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# The report runs inside python with an argv list, never through shell
# interpolation: a prompt head or file path can hold spaces, quotes, `$`,
# backticks and newlines, and none of those may reach a shell word.
#
# The payload is read first and the program travels in argv, because a
# heredoc would otherwise take python's stdin for itself and the hook JSON
# would never arrive: every event would decode nothing and report nothing.
input=$(cat)
prog=$(cat <<'PY'
import json
import subprocess
import sys


def first_fileish(tool_input):
    if not isinstance(tool_input, dict):
        return ""
    # A command's own description comes before the command: it is what the agent
    # says it is doing, in its words, the line its own interface shows.
    for key in ("file_path", "path", "description", "command", "pattern"):
        value = tool_input.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


def one_line(text, limit=120):
    single = " ".join(str(text).split())
    return single[:limit]


try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)

name = payload.get("hook_event_name") or ""
if name in ("SessionStart", "SubagentStop"):
    state, flags, event, label = "working", "--visible-working", "", ""
elif name == "UserPromptSubmit":
    state, flags, event = "working", "--visible-working", "user"
    label = one_line(payload.get("prompt") or "")
elif name in ("PreToolUse", "PostToolUse"):
    state, flags, event = "working", "--visible-working", "tool"
    tool = str(payload.get("tool_name") or "").strip()
    arg = first_fileish(payload.get("tool_input"))
    head = f"{tool} {arg}".strip()
    label = one_line(head or tool)
elif name == "PermissionRequest":
    state, flags, event = "blocked", "--visible-blocker", "blocked"
    tool = str(payload.get("tool_name") or "").strip()
    arg = first_fileish(payload.get("tool_input"))
    head = f"{tool} {arg}".strip()
    label = one_line(head or tool)
elif name == "Notification":
    # Only the kinds that wait on the person: a question, a setup question for a
    # teammate, or a resume that waits for Enter. `idle_prompt` fires a minute after
    # the agent finished, and `auth_success`, `agent_completed` and the rest ask
    # nothing; `Stop` already said idle. No kind at all is an older build, where
    # every notification was a question.
    kind = payload.get("notification_type")
    if kind not in (None, "", "permission_prompt", "elicitation_dialog",
                    "elicitation_url_dialog", "agent_needs_input",
                    "quota_auto_resume_stale"):
        raise SystemExit(0)
    state, flags, event, label = "blocked", "--visible-blocker", "blocked", ""
elif name in ("Stop", "SessionEnd"):
    state, flags, event, label = "idle", "--visible-idle", "done", ""
else:
    raise SystemExit(0)

# The conversation this report belongs to. It is what lets a tab come back
# to this conversation after a restart rather than to whichever one in the
# directory is newest. Refused unless it looks like an id and nothing else:
# this string ends up on a command line.
session = str(payload.get("session_id") or "")
if session and (len(session) > 128 or not all(c.isalnum() or c in "-_." for c in session)):
    session = ""

args = ["qoro", "hook-report", "--state", state, flags]
if event:
    args += ["--event", event]
if label:
    args += ["--label", label]
if session:
    args += ["--session-id", session]
try:
    subprocess.run(args, timeout=5, capture_output=True)
except Exception:
    pass
PY
)
printf '%s' "$input" | python3 -c "$prog" >/dev/null 2>&1 || true
exit 0

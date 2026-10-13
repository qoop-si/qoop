#!/bin/sh
# qoop hook for Gemini CLI.
#
# Gemini hooks run per event from `settings.json`, so the event-to-state
# mapping lives in the config rather than in payload parsing here. Install in
# `~/.gemini/settings.json` (user) or `.gemini/settings.json` (project):
#
#   { "hooks": {
#       "SessionStart": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh starting" }] }],
#       "BeforeAgent": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh working" }] }],
#       "BeforeTool": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh working" }] }],
#       "AfterTool": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh subagent" }] }],
#       "AfterAgent": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh idle" }] }],
#       "SessionEnd": [{ "matcher": "*", "hooks": [{ "type": "command",
#         "command": "qoop-hook.sh idle" }] }]
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
# A sub-agent runs as one `invoke_agent` tool call: `BeforeTool` says it started and
# `AfterTool` (argument `subagent`, never a state) that it ended; `AfterAgent` says none
# runs. Counted beside the tab with `--subagent-start/--subagent-end/--subagents`.
#
# Without python3, or with a payload that does not parse, the report goes
# out with the state alone, same as before.

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
  subagent) flags="" ;;
  *) exit 0 ;;
esac

# The report runs inside python with an argv list, never through shell
# interpolation: a command or a path can hold spaces, quotes, `$`, backticks
# and newlines, and none of those may reach a shell word.
if command -v python3 >/dev/null 2>&1; then
  QOOP_HOOK_STATE="$state" QOOP_HOOK_FLAGS="$flags" \
    GEMINI_SESSION_ID="${GEMINI_SESSION_ID:-}" python3 -c '
import hashlib, json, os, subprocess, sys, tempfile


def worktree_mark(cwd):
    # What the files and the history in cwd look like now, or None outside git. A repo with no
    # commit yet has no HEAD: those parts stay empty and the files still count. No optional locks,
    # so the hook never takes `index.lock` from under the git the agent runs.
    try:
        parts = []
        for git in (["status", "--porcelain=v1", "-uall"], ["diff", "HEAD"], ["rev-parse", "HEAD"]):
            got = subprocess.run(["git", "--no-optional-locks", "-C", cwd] + git, capture_output=True, timeout=2)
            if got.returncode != 0 and git[0] == "status":
                return None
            parts.append(got.stdout if got.returncode == 0 else b"")
        return hashlib.sha256(b"\0".join(parts)).hexdigest()
    except Exception:
        return None


def turn_file(payload):
    session = str(payload.get("session_id") or "")
    if not session or len(session) > 128 or not all(c.isalnum() or c in "-_." for c in session):
        session = "none"
    base = os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir()
    name = "qoop-turn-%d-%s-%s" % (os.getuid(), os.environ.get("QOOP_TERMINAL", "0"), session)
    return os.path.join(base, name)


def turn(payload, starts, ends):
    # A turn that changed the files or the history, by any tool, is real work: qoop opens the
    # task record at its end if none is open. Marked when the request arrives, compared when the
    # turn ends in the same directory; the mark is removed either way.
    name = payload.get("hook_event_name") or ""
    if name not in (starts, ends):
        return
    cwd = str(payload.get("cwd") or os.getcwd())
    path = turn_file(payload)
    try:
        if name == starts:
            mark = worktree_mark(cwd)
            if not mark:
                os.remove(path)
                return
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, "w") as f:
                f.write(cwd + "\n" + mark)
            return
        with open(path) as f:
            was_cwd, _, was = f.read().partition("\n")
        os.remove(path)
        mark = worktree_mark(cwd)
        if mark and was and was_cwd == cwd and mark != was:
            subprocess.run(["qoop", "task", "begin"], capture_output=True, timeout=2)
    except Exception:
        pass


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
    # The description a shell command carries comes first: it is what the agent says it is
    # doing, in its words.
    for key in ("description", "command", "file_path", "absolute_path", "pattern", "path",
                "dir_path", "url"):
        value = tool_input.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


name = payload.get("hook_event_name") or ""
state = os.environ["QOOP_HOOK_STATE"]
flags = os.environ.get("QOOP_HOOK_FLAGS") or ""


def subagents(*flag):
    # Its sub-agents, counted beside the tab: never a state.
    try:
        subprocess.run(["qoop", "hook-report", *flag], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
    except Exception:
        pass


# A sub-agent is one `invoke_agent` call, and its payload has no call id: the call is named
# by what it was asked, the same before and after it.
# ponytail: two identical calls at once count as one.
called = ""
if payload.get("tool_name") == "invoke_agent":
    asked = json.dumps(payload.get("tool_input"), sort_keys=True)
    called = hashlib.sha256(asked.encode()).hexdigest()[:16]
if state == "subagent":
    if called and name == "AfterTool":
        subagents("--subagent-end", called)
    raise SystemExit(0)
if called and name == "BeforeTool":
    subagents("--subagent-start", called)
# A session starting is not a turn. Said here and not only by the argument,
# because an entry installed earlier still passes `working` for it.
if name == "SessionStart":
    state, flags = "starting", ""
# An edit is real work in this terminal: qoop opens its task record now if it has none, so
# the record never waits on the agent to open it.
if name == "BeforeTool" and payload.get("tool_name") in ("write_file", "replace"):
    try:
        subprocess.run(["qoop", "task", "begin"], capture_output=True, timeout=2)
    except Exception:
        pass
args = ["qoop", "hook-report", "--state", state]
if flags:
    args.append(flags)
session = str(payload.get("session_id") or os.environ.get("GEMINI_SESSION_ID") or "")
if session and len(session) <= 128 and all(c.isalnum() or c in "-_." for c in session):
    args += ["--session-id", session]
if name == "BeforeTool":
    tool = str(payload.get("tool_name") or "").strip()
    arg = first_fileish(payload.get("tool_input"))
    label = one_line(f"{tool} {arg}".strip())
    args += ["--event", "tool"]
    if label:
        args += ["--label", label]
elif name == "BeforeAgent":
    args += ["--event", "user"]
    # The head of the request, as the Claude hook sends it: what a task qoop opens is
    # named after.
    prompt = one_line(payload.get("prompt") or "")
    if prompt:
        args += ["--label", prompt]
elif name == "AfterAgent":
    # The turn ended: its last tool is not what the agent is doing any more.
    args += ["--event", "done"]
try:
    subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
except Exception:
    pass
# Sub-agents run inside the turn: none outlives it, and one whose end never came goes too.
if name == "AfterAgent":
    subagents("--subagents", "")
# Mark or compare the turn after the report, so the state never waits on its git calls.
turn(payload, "BeforeAgent", "AfterAgent")
# A session starting or resuming: tell the agent the short rules for recording its task,
# and the task it has open. After the report, so the state is never late for it; quiet on
# any failure, and qoop itself prints nothing in the qass space. Gemini reads only JSON
# here, so the text goes inside its own envelope.
# With every request too, one line about the task: given once, the rules were read and then
# not followed.
if name in ("SessionStart", "BeforeAgent"):
    ask = ["qoop", "task", "rules"] + (["--turn"] if name == "BeforeAgent" else [])
    try:
        # Run where the agent works (the payload cwd), as the other calls do: the repo
        # block follows it.
        here = str(payload.get("cwd") or "")
        here = here if os.path.isdir(here) else None
        got = subprocess.run(ask, capture_output=True, timeout=2, text=True, cwd=here)
        rules = got.stdout.strip()
        if got.returncode == 0 and rules:
            sys.stdout.write(json.dumps({"hookSpecificOutput": {"hookEventName": name, "additionalContext": rules}}))
    except Exception:
        pass
' 2>/dev/null && exit 0
fi

# Without python a sub-agent event has nothing to say: it is never a state.
[ "$state" = subagent ] && exit 0
# shellcheck disable=SC2086
qoop hook-report --state "$state" $flags >/dev/null 2>&1 || true
exit 0

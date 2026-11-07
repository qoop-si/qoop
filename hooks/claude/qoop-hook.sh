#!/bin/sh
# qoop hook for Claude Code.
#
# Install: add to `.claude/settings.json` (project) or `~/.claude/settings.json`
# (user), one matcher per event:
#
#   {
#     "hooks": {
#       "SessionStart": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "PreToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "PostToolUse": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "Notification": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "PermissionRequest": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "SubagentStart": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "SubagentStop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "Stop": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "StopFailure": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }],
#       "SessionEnd": [{ "matcher": "", "hooks": [{ "type": "command",
#         "command": "/path/to/qoop-hook.sh" }] }]
#     }
#   }
#
# Reads Claude's hook JSON from stdin (`hook_event_name`), reports through
# `qoop hook-report`, and exits zero in every failure case: a hook that errors
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
#   SessionStart -> starting, no event; nothing when `source` is `compact`
#   SubagentStart, SubagentStop -> `--subagent-start` / `--subagent-end` with
#     its `agent_id`: counted beside the tab, never its state
#   anything else a subagent fired (the payload carries `agent_id`) -> nothing:
#     it describes the subagent, not this terminal
#   UserPromptSubmit -> working + `--event user` (prompt head, best-effort)
#   PreToolUse, PostToolUse -> working + `--event tool` (`tool_name` plus
#     the first file-ish `tool_input` field, a command's description before
#     the command itself, best-effort)
#   PostToolUse of AskUserQuestion -> working + `--event user`, no label (his answer)
#   PermissionRequest -> blocked + `--event blocked` (tool label, best-effort)
#   Notification (`permission_prompt`, `elicitation_dialog`,
#     `elicitation_url_dialog`, `agent_needs_input`, `quota_auto_resume_stale`,
#     or no `notification_type` from an older build)
#     -> blocked + `--event blocked`, labelled with the type for the two question
#     dialogs (the daemon drafts an inbox card for one still up after 120 s), no label
#     for the rest (they carry no tool); every other notification type reports nothing
#   Stop -> idle + `--event done`, labelled with the last paragraph of its last message
#     (the payload's own, else the transcript's); the daemon keeps it only when it asks the
#     owner something, and files it as a draft card when the agent waits on them;
#     SessionEnd -> idle + `--event done`, no label
#   StopFailure (the turn ended on an API error or a usage limit) -> failed,
#     no event: the agent stopped and will not go on by itself
#   Stop, StopFailure with a `background_tasks` list -> also `--subagents` with
#     the ids of its running subagents: any other it was counting has ended

set -eu

# A terminal started before the rename carries the old names.
export QOOP_TERMINAL="${QOOP_TERMINAL:-${QORO_TERMINAL:-}}" QOOP_TOKEN="${QOOP_TOKEN:-${QORO_TOKEN:-}}"
[ -n "${QOOP_TERMINAL:-}" ] || exit 0
[ -n "${QOOP_TOKEN:-}" ] || exit 0
# A backgrounded session runs under Claude's own daemon, which inherited the terminal and
# token of whichever tab started it first: that tab is not where this session lives, and
# reporting there puts another conversation, its title and its subagents in that tab.
# `CLAUDE_JOB_DIR` is what reaches the hook: Claude keeps `CLAUDE_CODE_SESSION_KIND` from it.
[ -z "${CLAUDE_JOB_DIR:-}" ] || exit 0
# The installer puts qoop in ~/.local/bin, which a stock Mac does not have on PATH until
# the person adds it: looked for there too, rather than every hook going quiet.
command -v qoop >/dev/null 2>&1 || PATH="${HOME:-/nonexistent}/.local/bin:$PATH"
command -v qoop >/dev/null 2>&1 || exit 0
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
import hashlib
import json
import os
import subprocess
import sys
import tempfile


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


def paragraphs_of(text, count):
    """The last `count` paragraphs of a message, oldest first, each as one line with each
    line's markdown off. (A backtick is chr(96): one written out would end the shell's
    command substitution.)"""
    paragraphs, paragraph = [], []
    for text_line in reversed(text.splitlines()):
        bare = text_line.strip().strip("*_#> -" + chr(96)).strip()
        if bare:
            paragraph.insert(0, bare)
        elif paragraph:
            paragraphs.insert(0, " ".join(paragraph))
            paragraph = []
            if len(paragraphs) == count:
                break
    if paragraph and len(paragraphs) < count:
        paragraphs.insert(0, " ".join(paragraph))
    return paragraphs


def paragraph_of(text):
    """The last paragraph of a message as one line. Its last paragraph, whatever it says: the
    daemon decides whether it asks them (a "?", or an offer such as "if you like, I can ...")."""
    last = paragraphs_of(text, 1)
    return ends_kept(last[0]) if last else ""


def said_of(text):
    """The turn's closing words for the owner's card: the paragraph before the last (what it
    just did, at most 300 characters) and the last (its question, at most 600), a blank line
    between. Cut at a word, never in the middle."""
    parts = paragraphs_of(text, 2)
    if not parts:
        return ""
    limits = [300, 600][-len(parts):]
    return "\n\n".join(cut_at_word(p, n) for p, n in zip(parts, limits))


def cut_at_word(text, limit):
    if len(text) <= limit:
        return text
    head = text[: limit - 1]
    at = head.rfind(" ")
    return (head[:at] if at > limit // 2 else head).rstrip() + "\u2026"


def last_paragraph(payload):
    """The last paragraph of the turn's last message as one line, and the turn's closing words
    for the card (`said_of`): the Stop payload's own "last_assistant_message" (the transcript
    is written after the hook can run, so it may not hold the turn yet), else read from the end
    of the transcript; "" for both when the turn has none or it cannot be read."""
    said = payload.get("last_assistant_message")
    if isinstance(said, str) and said.strip():
        return paragraph_of(said), said_of(said)
    path = payload.get("transcript_path")
    if not isinstance(path, str) or not path:
        return "", ""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 262144))
            lines = f.read().decode("utf-8", "replace").splitlines()
    except Exception:
        return "", ""
    for line in reversed(lines):
        try:
            entry = json.loads(line)
        except Exception:
            continue
        message = entry.get("message") if isinstance(entry, dict) else None
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        # Their last prompt ends the turn read back: a question before it was already answered.
        if message.get("role") == "user":
            results = isinstance(content, list) and any(
                isinstance(c, dict) and c.get("type") == "tool_result" for c in content)
            if not results:
                return "", ""
            continue
        if message.get("role") != "assistant":
            continue
        texts = [c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") == "text"] if isinstance(content, list) else []
        if not texts:
            continue
        joined = "\n".join(texts)
        return paragraph_of(joined), said_of(joined)
    return "", ""


def ends_kept(text, limit=120):
    """One line of at most `limit` characters that keeps both ends of a longer text: an offer
    is often near its start ("if you like, I can ...") and a question near its end."""
    single = " ".join(str(text).split())
    if len(single) <= limit:
        return single
    half = (limit - 5) // 2
    return single[:half] + " ... " + single[-half:]


def worktree_mark(cwd):
    """What the files and the history in cwd look like now, or None outside git. A repo with no
    commit yet has no HEAD: those parts stay empty and the files still count. No optional locks,
    so the hook never takes `index.lock` from under the git the agent runs."""
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
    """A turn that changed the files or the history, by any tool, is real work: qoop opens the
    task record at its end if none is open. Marked when the request arrives, compared when the
    turn ends in the same directory; the mark is removed either way."""
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
    raise SystemExit(0)

name = payload.get("hook_event_name") or ""
# An edit, by the agent or by one of its subagents, is real work in this terminal: qoop opens
# its task record now if it has none, so the record never waits on the agent to open it.
# Before the subagent filter below, which is about state, not about the work.
if name == "PreToolUse" and payload.get("tool_name") in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
    try:
        subprocess.run(["qoop", "task", "begin"], capture_output=True, timeout=2)
    except Exception:
        pass


def subagents(*flag):
    # The agent's sub-agents, counted beside the tab: never a state.
    try:
        subprocess.run(["qoop", "hook-report", "--agent", "claude", *flag], capture_output=True, timeout=5)
    except Exception:
        pass


agent = str(payload.get("agent_id") or "")
flag = {"SubagentStart": "--subagent-start", "SubagentStop": "--subagent-end"}.get(name)
if agent and flag:
    subagents(flag, agent)
# A subagent's tool calls and its end are not this terminal's turn. Reported, a
# background agent finishing after the turn ended put the tab back to working,
# and the screen's idle composer took it away again a moment later.
if agent:
    raise SystemExit(0)
# The closing words a turn end carries for the owner's card; only `Stop` has any.
said = ""
if name == "SessionStart":
    # A session opening, resuming or clearing is not a turn: `starting`, so an
    # agent the screen then reads as idle is not news that it finished. A
    # compaction says nothing: inside a turn the tab is working already, and
    # Claude also compacts an idle session before its prompt cache expires, with
    # no turn and so no `Stop` after it: reported as working, that tab stayed so.
    if payload.get("source") == "compact":
        state, flags, event, label = "", "", "", ""
    else:
        state, flags, event, label = "starting", "", "", ""
elif name == "UserPromptSubmit":
    state, flags, event = "working", "--visible-working", "user"
    label = one_line(payload.get("prompt") or "")
elif name == "PostToolUse" and payload.get("tool_name") == "AskUserQuestion":
    # His answer in the question dialog reached the agent: an answer as a typed prompt is,
    # so a task waiting on him goes back to work. No label: it is no request, never a goal.
    state, flags, event, label = "working", "--visible-working", "user", ""
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
    # A question dialog says which: the daemon drafts an inbox card for one still up later.
    label = kind if kind in ("elicitation_dialog", "elicitation_url_dialog") else ""
    state, flags, event = "blocked", "--visible-blocker", "blocked"
elif name == "Stop":
    state, flags, event = "idle", "--visible-idle", "done"
    label, said = last_paragraph(payload)
elif name == "SessionEnd":
    state, flags, event, label = "idle", "--visible-idle", "done", ""
elif name == "StopFailure":
    state, flags, event, label = "failed", "", "", ""
else:
    raise SystemExit(0)

# The conversation this report belongs to. It is what lets a tab come back
# to this conversation after a restart rather than to whichever one in the
# directory is newest. Refused unless it looks like an id and nothing else:
# this string ends up on a command line.
session = str(payload.get("session_id") or "")
if session and (len(session) > 128 or not all(c.isalnum() or c in "-_." for c in session)):
    session = ""

# The turn is over: a subagent missing from its own list of what still runs has ended, even
# when its end never came. Before the report: the daemon reads a turn's end as a question for
# the owner only when none of the agent's sub-agents still runs, so the count it reads must be
# this turn's own.
tasks = payload.get("background_tasks")
if name in ("Stop", "StopFailure") and isinstance(tasks, list):
    running = [
        str(t.get("id")) for t in tasks
        if isinstance(t, dict) and t.get("type") == "subagent"
        and t.get("status") == "running" and t.get("id")
    ]
    subagents("--subagents", ",".join(running))

args = ["qoop", "hook-report", "--agent", "claude", "--state", state]
if flags:
    args.append(flags)
if event:
    args += ["--event", event]
if label:
    args += ["--label", label]
if said:
    args += ["--said", said]
if session:
    args += ["--session-id", session]
if state:
    try:
        subprocess.run(args, timeout=5, capture_output=True)
    except Exception:
        pass
# Mark or compare the turn after the report, so the state never waits on its git calls.
turn(payload, "UserPromptSubmit", "Stop")

# A session starting, resuming, clearing or compacting: tell the agent the short rules for
# recording its task, and the task it has open, so they survive a compaction too. After the
# report, so the state is never late for it; quiet on any failure, and qoop itself prints
# nothing in qass's own space.
# With every request, one line about the task: given once, the rules were read and then not
# followed.
if name in ("SessionStart", "UserPromptSubmit"):
    ask = ["qoop", "task", "rules"] + (["--turn"] if name == "UserPromptSubmit" else [])
    try:
        # Run where the agent works (the payload cwd), as the other calls do: the repo
        # block follows it.
        here = str(payload.get("cwd") or "")
        here = here if os.path.isdir(here) else None
        got = subprocess.run(ask, capture_output=True, timeout=2, text=True, cwd=here)
        rules = got.stdout.strip()
        if got.returncode == 0 and rules:
            sys.stdout.write(rules + "\n")
    except Exception:
        pass
PY
)
# Standard output is the agent's context on SessionStart and nothing anywhere else: the
# program prints only the rules, only then.
printf '%s' "$input" | python3 -c "$prog" 2>/dev/null || true
exit 0

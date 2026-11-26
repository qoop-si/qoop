#!/bin/sh
# qass's Stop hook. First, a question for the owner is a card. When this turn's last message
# asks them something (a line ending in "?", or answer words such as `api A` that name no open
# card, nor one ended in the last ten minutes) and the turn wrote no card (`escalate.sh new
# ... -`, or `escalate.sh show`), the stop is blocked once with one sentence; `stop_hook_active` lets the next stop through, so it never
# loops. Second, when that lets the stop through: the skill installed beside this script is not
# the one qass was last told to read (`@QASS_DATA@/skill.log`, its last line), so the stop is
# blocked once to have it read again, and a line is added. A resumed conversation keeps the skill
# text it loaded weeks ago, and this script is read from disk on every stop, so a new install
# reaches the running qass here. The log is one for all of qass's conversations: a fresh one
# after a later install reads its skill once more, which costs one Read; the very first stop
# only writes the log, since there is no older install whose skill it could have kept. Third, the
# voice: `qoop qass persona` printing other text than qass was last given (`@QASS_DATA@/voice.log`)
# blocks the stop once with the new voice, or with "plain again". `cardcheck.sh voice` (the
# UserPromptSubmit hook) does that voice check alone and prints the text plainly, never a block:
# a block there would erase the owner's message. Any failure lets the stop through: this hook
# must never hold qass up.
command -v python3 >/dev/null 2>&1 || exit 0
input=$(cat)
prog=$(cat <<'PY'
import hashlib, json, os, re, subprocess, sys, time


def changed(log, now, tell_when_new):
    # Whether `now` differs from the last hash in `log` (a missing log tells only when
    # `tell_when_new`); a new hash is appended.
    try:
        with open(log) as f:
            told = (f.read().split() or [""])[-1]
    except OSError:
        told = None
    if told == now:
        return False
    with open(log, "a") as f:
        f.write(f"{int(time.time())} {now}\n")
    return told is not None or tell_when_new


def voice_changed(quiet=False, then=""):
    # What to tell qass when its voice changed since it was last given one, else None; `then`
    # says what to do with it. A failing `qoop qass persona` tells nothing: plain is not known
    # then. `quiet`: qass read or set the voice itself this turn, so the new one is only noted.
    try:
        got = subprocess.run(["qoop", "qass", "persona"], capture_output=True, timeout=5)
    except Exception:
        return None
    if got.returncode != 0:
        return None
    block = got.stdout.decode("utf-8", "replace").strip()
    if not changed("@QASS_DATA@/voice.log", hashlib.sha256(got.stdout).hexdigest()[:12], bool(block)) or quiet:
        return None
    if not block:
        return "Your voice is plain again: drop the voice you were given and speak as your skill says." + then
    return "Your voice changed; speak in it from here." + then + "\n" + block


if sys.argv[2:3] == ["voice"]:
    said = voice_changed()
    if said:
        sys.stdout.buffer.write((said + "\n").encode("utf-8"))
    sys.exit(0)

payload = json.loads(sys.stdin.read() or "{}")
if payload.get("stop_hook_active"):
    sys.exit(0)
path = payload.get("transcript_path")
with open(path, "rb") as f:
    f.seek(0, 2)
    f.seek(max(0, f.tell() - 524288))
    lines = f.read().decode("utf-8", "replace").splitlines()

# The turn: every entry after the owner's (or a wake's) last prompt, which is a user entry
# that is not a tool result.
turn = []
for line in reversed(lines):
    try:
        entry = json.loads(line)
    except Exception:
        continue
    message = entry.get("message") if isinstance(entry, dict) else None
    if not isinstance(message, dict):
        continue
    content = message.get("content")
    if message.get("role") == "user":
        results = isinstance(content, list) and any(
            isinstance(c, dict) and c.get("type") == "tool_result" for c in content)
        if not results:
            break
        continue
    if message.get("role") == "assistant" and isinstance(content, list):
        turn.append(content)

texts, commands = [], []
for content in turn:
    for c in content:
        if not isinstance(c, dict):
            continue
        if c.get("type") == "text" and not texts:
            texts.append(c.get("text", ""))
        if c.get("type") == "tool_use":
            commands.append(str((c.get("input") or {}).get("command", "")))
last = texts[0] if texts else ""

def let_through():
    # The stop goes through, unless the skill or the voice changed since qass was last told.
    # ponytail: SKILL.md alone stands for the skill; its other files are read when it names them.
    skill = os.path.join(sys.argv[1], "SKILL.md")
    try:
        with open(skill, "rb") as f:
            now = hashlib.sha256(f.read()).hexdigest()[:12]
    except OSError:
        now = None
    # No log yet: a fresh install, nothing older to re-read.
    if now and changed("@QASS_DATA@/skill.log", now, False):
        print(json.dumps({"decision": "block", "reason":
            f"Your skill changed since this conversation last read it. Read {skill} whole now (one "
            "Read, no offset or limit) and work by it from here; then end this turn as you were."}))
        sys.exit(0)
    # Only a command that sets the voice or prints the one in use: `ls`, a preview or a grep is not.
    said = voice_changed(any(re.search(r"config set qass\.persona|qass persona(\s+custom\s+-)?\s*($|[;&|)<]|\d?>)", c)
                             for c in commands),
                         " End this turn as you were, writing nothing more.")
    if said:
        print(json.dumps({"decision": "block", "reason": said}))
    sys.exit(0)


if any("escalate.sh" in cmd and (re.search(r"\s-\s*<<", cmd) or " show " in cmd + " ") for cmd in commands):
    let_through()

tick = chr(96)
asked = [l for l in last.splitlines() if l.strip().strip("*_#> -" + tick).strip().endswith("?")]
handles = set(re.findall(tick + r"([a-z0-9][a-z0-9-]*) [A-Da-d]" + tick, last))
if handles or asked:
    # The cards they can see, and those ended in the last ten minutes (an answer just passed on
    # names its card). No listing: answer words cannot be told from a pointer, so only a line
    # that asks still needs a card.
    try:
        got = subprocess.run(["qoop", "ask", "ls", "--json"], capture_output=True, text=True, timeout=5)
        listing = json.loads(got.stdout)
        recent = time.time() - 600
        cards = listing.get("live", []) + [
            c for c in listing.get("ended", []) if (c.get("closed_at") or 0) >= recent]
    except Exception:
        print("cardcheck: qoop ask ls --json failed; answer words go unchecked", file=sys.stderr)
        cards, handles = [], set()
    names = {c.get("body", {}).get("handle") for c in cards} | {c.get("card") for c in cards}
    names.discard(None)
    handles -= names
    # A line that names a live card (its handle or its number) points at it: no new question.
    asked = [l for l in asked if not any(re.search(r"\b" + re.escape(n) + r"\b", l) for n in names)]
if not asked and not handles:
    let_through()
print(json.dumps({"decision": "block", "reason":
    "You asked the owner something in chat and wrote no card this turn. Write it as a card "
    "(escalate.sh new ... - <<'END'), then point to it in one line by its handle. If it was "
    "not a question for them, end your message without one."}))
PY
)
printf '%s' "$input" | python3 -c "$prog" "$(dirname "$0")" "${1:-}" 2>/dev/null || true

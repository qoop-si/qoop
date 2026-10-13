#!/bin/sh
# qass's Stop hook. First, a question for the owner is a card. When this turn's last message
# asks him something (a line ending in "?", or answer words such as `dub A` that name no open
# card, nor one ended in the last ten minutes) and the turn wrote no card (`escalate.sh new
# ... -`, or `escalate.sh show`), the stop is blocked once with one sentence; `stop_hook_active` lets the next stop through, so it never
# loops. Second, when that lets the stop through: the skill installed beside this script is not
# the one qass was last told to read (`@QASS_DATA@/skill.log`, its last line), so the stop is
# blocked once to have it read again, and a line is added. A resumed conversation keeps the skill
# text it loaded weeks ago, and this script is read from disk on every stop, so a new install
# reaches the running qass here. The log is one for all of qass's conversations: a fresh one
# after an install reads its skill once more, which costs one Read. Any failure lets the stop
# through: this hook must never hold qass up.
command -v python3 >/dev/null 2>&1 || exit 0
input=$(cat)
prog=$(cat <<'PY'
import hashlib, json, os, re, subprocess, sys, time

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

def skill_changed():
    # ponytail: SKILL.md alone stands for the skill; its other files are read when it names them.
    skill = os.path.join(sys.argv[1], "SKILL.md")
    log = "@QASS_DATA@/skill.log"
    try:
        with open(skill, "rb") as f:
            now = hashlib.sha256(f.read()).hexdigest()[:12]
    except OSError:
        return
    try:
        with open(log) as f:
            told = (f.read().split() or [""])[-1]
    except OSError:
        told = ""
    if told == now:
        return
    with open(log, "a") as f:
        f.write(f"{int(time.time())} {now}\n")
    print(json.dumps({"decision": "block", "reason":
        f"Your skill changed since this conversation last read it. Read {skill} whole now (one "
        "Read, no offset or limit) and work by it from here; then end this turn as you were."}))
    sys.exit(0)


if any("escalate.sh" in cmd and (re.search(r"\s-\s*<<", cmd) or " show " in cmd + " ") for cmd in commands):
    skill_changed()
    sys.exit(0)

tick = chr(96)
asked = [l for l in last.splitlines() if l.strip().strip("*_#> -" + tick).strip().endswith("?")]
handles = set(re.findall(tick + r"([a-z0-9][a-z0-9-]*) [A-Da-d]" + tick, last))
if handles or asked:
    # The cards he can see, and those ended in the last ten minutes (an answer just passed on
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
    skill_changed()
    sys.exit(0)
print(json.dumps({"decision": "block", "reason":
    "You asked the owner something in chat and wrote no card this turn. Write it as a card "
    "(escalate.sh new ... - <<'END'), then point to it in one line by its handle. If it was "
    "not a question for him, end your message without one."}))
PY
)
printf '%s' "$input" | python3 -c "$prog" "$(dirname "$0")" 2>/dev/null || true

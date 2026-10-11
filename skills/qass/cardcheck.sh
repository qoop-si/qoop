#!/bin/sh
# qass's Stop hook: a question for the owner is a card first. When this turn's last message asks
# him something (a line ending in "?", or answer words such as `dub A` that name no open card)
# and the turn wrote no card (`escalate.sh new ... -`, or `escalate.sh show`), the stop is
# blocked once with one sentence; `stop_hook_active` lets the next stop through, so it never
# loops. Any failure lets the stop through: this hook must never hold qass up.
command -v python3 >/dev/null 2>&1 || exit 0
input=$(cat)
prog=$(cat <<'PY'
import json, re, subprocess, sys

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
if any("escalate.sh" in cmd and (re.search(r"\s-\s*<<", cmd) or " show " in cmd + " ") for cmd in commands):
    sys.exit(0)

tick = chr(96)
asked = [l for l in last.splitlines() if l.strip().strip("*_#> -" + tick).strip().endswith("?")]
handles = set(re.findall(tick + r"([a-z0-9][a-z0-9-]*) [A-Da-d]" + tick, last))
if handles or asked:
    try:
        got = subprocess.run(["qoop", "ask", "ls", "--json"], capture_output=True, text=True, timeout=3)
        cards = json.loads(got.stdout).get("live", [])
    except Exception:
        cards = []
    names = {c.get("body", {}).get("handle") for c in cards} | {c.get("card") for c in cards}
    names.discard(None)
    handles -= names
    # A line that names a live card (its handle or its number) points at it: no new question.
    asked = [l for l in asked if not any(re.search(r"\b" + re.escape(n) + r"\b", l) for n in names)]
if not asked and not handles:
    sys.exit(0)
print(json.dumps({"decision": "block", "reason":
    "You asked the owner something in chat and wrote no card this turn. Write it as a card "
    "(escalate.sh new ... - <<'END'), then point to it in one line by its handle. If it was "
    "not a question for him, end your message without one."}))
PY
)
printf '%s' "$input" | python3 -c "$prog" 2>/dev/null || true

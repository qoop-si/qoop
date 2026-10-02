"""qass gate. Run by gate.sh; see there for what it is for.

`tool`: reads a PreToolUse payload on stdin, exits 0 to allow and 2 to deny.
Anything unexpected is a deny.
`owner`: reads a UserPromptSubmit payload, appends an owner row, exits 0.
"""

import json
import os
import re
import subprocess
import sys
import time
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
HOME = Path.home().resolve()
# Filled in by `qoro skills install` (see `qoro paths`): qass's own state and working folder,
# and the owner's files.
QASS = Path("@QASS_DATA@").resolve()
OWNER = Path("@QASS_CONFIG@").resolve()
LOG = QASS / "gate.jsonl"
GOALS = QASS / "goals.md"
POLICY = OWNER / "policy"
SKILLS = HOME / ".claude" / "skills"
LESSONS = OWNER / "lessons.md"
LESSONS_MAX = 2000      # chars; full means merge or remove before adding (Hermes keeps 2,200)
# A lesson names a situation and ends in a careful action and its source, nothing after:
# "- <when, 160 chars at most> -> escalate (E7)". Nothing in it can let qass act alone.
LESSON = re.compile(r"- [^\n]{1,160} -> (?:escalate|wait|decline|ask for proof)\.? "
                    r"\((?:E[1-9][0-9]{0,5}|owner [0-9]{4}-[0-9]{2}-[0-9]{2})\)")

# Outside single quotes a command may hold only these; inside them, anything but '.
SAFE = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_./:=@%+,-")
NUM = re.compile(r"[0-9]{1,6}")
KEYS = {"enter", "esc", "up", "down", "tab"}
# An owner answer names items (P3, E4) and answers them; a question answers nothing.
P_ITEM = re.compile(r"\bP([1-9][0-9]{0,5})\b")
E_ITEM = re.compile(r"\bE([1-9][0-9]{0,5})\b")
# A yes to a proposal is exactly "ok P3" or "P3 P4 ok", nothing more: words cannot be parsed
# for a hedge ("tamam ama şimdi yapma") or a spoken question ("P3 tamam mı").
OK = r"(?:ok|okay|yes|tamam|evet)"
PS = r"P[1-9][0-9]{0,5}(?:\s+P[1-9][0-9]{0,5})*"
P_OK = re.compile(rf"{OK}\s+{PS}|{PS}\s+{OK}", re.IGNORECASE)
# What the owner calls an item instead of its number: one word of the agent's sidebar name.
HANDLE = re.compile(r"[a-z0-9][a-z0-9-]{1,29}")
NOT_HANDLE = {"ok", "okay", "yes", "tamam", "evet", "no", "hayir", "yok", "dur"}
RELAY_HEAD = re.compile(r"The owner answered E([1-9][0-9]{0,5}): ")
# One choice on an escalation card; the owner can always answer in his own words instead.
OPTION = re.compile(r"[A-D]: [^\n]{1,200}")

FENCE = "Do not commit, push, merge, delete, deploy, or contact anything external in this step; ask first."
FENCE_SHIP = (
    "You may commit only through /commit, push this branch and open a PR for it. "
    "Do not merge, delete, deploy, or contact anything external in this step; ask first."
)
FENCE_MERGE = (
    "You may commit only through /commit, push this branch, open a PR for it and "
    "rebase-merge that PR. Do not delete, deploy, or contact anything external in this "
    "step; ask first."
)
FENCE_GRANTS = {
    FENCE: set(),
    FENCE_SHIP: {"commit", "push-branch", "open-pr"},
    FENCE_MERGE: {"commit", "push-branch", "open-pr", "merge-own-pr"},
}
# Relays the owner's own answer to an escalation; the prompt quotes him word for word.
FENCE_OWNER = "Those are the words of the owner: do only what they allow, and ask before anything more."

READ_ONLY_QORO = {"agents", "context", "read", "history"}
FREE_TOOLS = {"ToolSearch", "TodoWrite"}
PER_WORKER = 3
PER_HOUR = 10
SPAWNED_ALIVE = 4
AGENTS = {"claude", "opencode"}
CARD_FRESH = 86400   # a lone letter answers the one card younger than this


class Deny(Exception):
    pass


def words(command):
    """Split a command the way sh would, refusing anything sh would expand."""
    if "\n" in command or "\r" in command:
        raise Deny("one line only")
    out, cur, quoted = [], None, False
    for ch in command:
        if quoted:
            if ch == "'":
                quoted = False
            else:
                cur += ch
        elif ch == "'":
            quoted = True
            cur = cur or ""
        elif ch == " ":
            if cur is not None:
                out.append(cur)
                cur = None
        elif ch in SAFE:
            cur = (cur or "") + ch
        else:
            raise Deny(f"character {ch!r} outside single quotes")
    if quoted:
        raise Deny("unterminated quote")
    if cur is not None:
        out.append(cur)
    if not out:
        raise Deny("empty command")
    return out


def flags(rest, allowed):
    """Check `--flag value` pairs (or bare flags) against allowed: name -> validator."""
    i = 0
    while i < len(rest):
        name = rest[i]
        if name not in allowed:
            raise Deny(f"flag {name} not allowed")
        check = allowed[name]
        if check is None:
            i += 1
            continue
        if i + 1 >= len(rest) or not check(rest[i + 1]):
            raise Deny(f"bad value for {name}")
        i += 2


def is_script(word, name):
    """Whether a word names one of this skill's scripts, by whatever path reaches it."""
    try:
        return Path(word).resolve() == HERE / name
    except (OSError, ValueError):
        return False


def is_num(v):
    return bool(NUM.fullmatch(v))


def run(args):
    return subprocess.run(args, capture_output=True, text=True, timeout=4, check=True).stdout


def remotes():
    """The names in `qoro remote ls`."""
    return {line.split()[0] for line in run(["qoro", "remote", "ls"]).splitlines()
            if line.split() and re.fullmatch(r"[A-Za-z0-9_-]+", line.split()[0])}


def qoro(machine, *args):
    """argv for a qoro command on this Mac (machine None) or on a remote, through on.sh."""
    return [str(HERE / "on.sh"), machine, *args] if machine else ["qoro", *args]


def rows(machine=None):
    return json.loads(run(qoro(machine, "agents", "--json")))


def sleeping(machine=None):
    """The tabs asleep on a machine, as strings. A remote too old to say has none."""
    return {str(t) for sp in json.loads(run(qoro(machine, "ls", "--json")))
            for t in sp.get("sleeping") or []}


def log_rows():
    if not LOG.exists():
        return []
    return [json.loads(line) for line in LOG.read_text().splitlines() if line.strip()]


def append(row):
    QASS.mkdir(parents=True, exist_ok=True)
    row = {"ts": time.time(), **row}
    with LOG.open("a") as f:
        f.write(json.dumps(row, ensure_ascii=False) + "\n")


def policy():
    mode, grants = "propose", set()
    if POLICY.exists():
        for line in POLICY.read_text().splitlines():
            parts = line.split()
            if parts[:1] == ["mode"] and len(parts) == 2 and parts[1] in ("propose", "auto"):
                mode = parts[1]
            elif parts[:1] == ["grant"] and len(parts) == 3:
                grants.add((str(Path(parts[1]).expanduser().resolve()), parts[2]))
    return mode, grants


def repo_root(cwd):
    common = run(["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"])
    return str(Path(common.strip()).resolve().parent)


def one_line(text):
    """The owner's words as a one-line prompt can carry them: ’ for the quote character."""
    return " ".join(text.split()).replace("'", "’")


def clauses(text):
    """An answer to several items is one clause per item: "P3 yes, P4 no; E5 evet"."""
    return [c for c in re.split(r"[,;\n]|\band\b|\bve\b", text) if c.strip()]


def norm(word):
    """A word as the owner may type it: no case, no accents, ı as i, no trailing punctuation."""
    word = unicodedata.normalize("NFKD", word.replace("ı", "i").replace("İ", "i")).lower()
    return "".join(ch for ch in word if not unicodedata.combining(ch)).strip(".,!:;")


def is_handle(word):
    return bool(HANDLE.fullmatch(word)) and word not in NOT_HANDLE \
        and not re.fullmatch(r"[ep][0-9]+", word)


def answered(text, cards, now):
    """The open cards a message answers, each with the letter it picks or None: a clause that
    names the card's E<n> or starts with its handle, with more in it than that, and is not a
    question; or a lone letter while exactly one card younger than a day is open. A number
    may also answer a card already passed on (a correction); that is the caller's."""
    out = {}
    fresh = [n for n, c in cards.items() if now - c["ts"] < CARD_FRESH and not c.get("done")]
    for c in clauses(text):
        c = c.strip().rstrip(".!")
        # "korea ok" is a yes to the proposal korea, never an answer to a card.
        if "?" in c or not c or (not E_ITEM.search(c) and re.fullmatch(
                rf"{OK}\s+\S+|\S+\s+{OK}", c, re.IGNORECASE)):
            continue
        found = [(n, f"E{n}") for n in map(int, E_ITEM.findall(c)) if E_ITEM.sub("", c).strip()]
        first, rest = c.split()[0], c.split()[1:]
        # A handle finds the newest open card that has it.
        named = [n for n, card in cards.items() if rest and not card.get("done")
                 and card.get("handle") and norm(first) == card["handle"]]
        found += [(max(named), first)] if named else []
        for n, token in found:
            m = re.fullmatch(rf"{re.escape(token)}\s+([A-Da-d])", c)
            out[n] = m.group(1).upper() if m else None
        if re.fullmatch(r"[A-Da-d]", c) and len(fresh) == 1:
            out[fresh[0]] = c.upper()
    return out


def said_yes(text, proposals):
    """The P numbers a message says yes to: clauses that are exactly "ok P3" or "P3 ok", or
    "ok <handle>" / "<handle> ok" for a proposal recorded with that handle."""
    yes = set()
    for c in clauses(text):
        c = c.strip().rstrip(".!")
        if P_OK.fullmatch(c):
            yes |= {int(n) for n in P_ITEM.findall(c)}
        elif m := re.fullmatch(rf"{OK}\s+(\S+)|(\S+)\s+{OK}", c, re.IGNORECASE):
            word = norm(m.group(1) or m.group(2))
            if word in proposals:
                yes.add(proposals[word])
    return yes


def approvals(history, now):
    """Writes the owner's ok to proposals still allows: one per P number he said yes to,
    counted once, and only for an hour (an old unused ok must not open a new write)."""
    left, seen, proposals = 0, set(), {}
    for r in history:
        if "proposed" in r:
            proposals[r["handle"]] = int(r["proposed"][1:])
        elif r.get("owner"):
            new = said_yes(r.get("text", ""), proposals) - seen
            seen |= new
            if now - r.get("ts", 0) < 3600:
                left += len(new)
        elif r.get("write") and r.get("spent", True) and not r.get("relay"):
            left = max(0, left - 1)
    return left


def cards_state(history):
    """Open cards (asked, not passed on yet) and the ones he answered:
    (number -> card, number -> (terminal, his words, the options he was shown, his pick)).
    By its number he can still answer a card already passed on, to correct himself."""
    asked, cards, waiting = {}, {}, {}
    for r in history:
        if "escalated" in r:
            asked[int(r["escalated"][1:])] = cards[int(r["escalated"][1:])] = {
                "terminal": r.get("terminal"), "options": r.get("options") or {},
                "handle": r.get("handle"), "ts": r.get("ts", 0)}
        elif r.get("owner"):
            found = answered(r.get("text", ""), asked, r.get("ts", 0))
            for n, pick in found.items():
                by_number = any(n in map(int, E_ITEM.findall(c)) for c in clauses(r["text"]))
                if n in cards or (n in asked and by_number):
                    waiting[n] = (asked[n]["terminal"], one_line(r["text"]),
                                  asked[n]["options"], pick)
        elif r.get("relay"):
            n = int(r["relay"][1:])
            waiting.pop(n, None)
            if n in cards:
                cards.pop(n)["done"] = True
    return cards, waiting


def open_proposals(history):
    """Proposals recorded and not yet said yes to: handle -> number."""
    props = {}
    for r in history:
        if "proposed" in r:
            props[r["handle"]] = int(r["proposed"][1:])
        elif r.get("owner"):
            yes = said_yes(r.get("text", ""), props)
            props = {h: n for h, n in props.items() if n not in yes}
    return props


def relays(history):
    return cards_state(history)[1]


def check_write(kind, terminal, generation, text=None, keys=None, machine=None):
    """The bounds on anything that types into a worker or starts one, on this Mac or on a
    remote (machine): there a worker is `ubox/2` for caps, cards and relays, its rows come
    from that machine, and no grant reaches it."""
    me = os.environ.get("QORO_TERMINAL", "")
    if not me:
        raise Deny("qass must run inside a qoro terminal")
    mode, grants = policy()
    history = log_rows()
    now = time.time()
    last_owner = max((i for i, r in enumerate(history) if r.get("owner")), default=-1)
    live = rows(machine) if kind != "spawn" else rows()
    relay = False
    number, terminal = terminal, (f"{machine}/{terminal}" if machine else terminal)

    if kind == "wake":
        if terminal == me:
            raise Deny("never write to the qass tab")
        if number not in sleeping(machine):
            raise Deny(f"terminal {terminal} is not asleep")
        # Waking it to pass on his answer to its card needs no further ok.
        relay = any(w[0] == terminal for w in relays(history).values()) and "wake"
    elif kind == "spawn":
        alive = {(r["terminal"], r["generation"]) for r in live}
        spawned = {(r["spawned"], r["gen"]) for r in history if "spawned" in r}
        if len(spawned & alive) >= SPAWNED_ALIVE:
            raise Deny(f"{SPAWNED_ALIVE} spawned workers are already alive")
    else:
        if terminal == me:
            raise Deny("never write to the qass tab")
        row = next((r for r in live if str(r["terminal"]) == number), None)
        if row is None or not row.get("agent"):
            raise Deny(f"terminal {terminal} is not an agent")
        if str(row["generation"]) != generation:
            raise Deny(f"terminal {terminal} is now generation {row['generation']}")
        if text is not None:
            if ".qass" in text.lower() or "qoro/qass" in text.lower():
                raise Deny("a prompt may not name qass's own files")
            head = RELAY_HEAD.match(text)
            if head or text.rstrip().endswith(FENCE_OWNER):
                if not head:
                    raise Deny(f'a relay is exactly: The owner answered E<n>: "<his words>". '
                               f"[He chose <letter>: <that option>.] {FENCE_OWNER}")
                n = int(head.group(1))
                waiting = relays(history).get(n)
                if waiting is None:
                    raise Deny(f"E{n} has no answer of the owner waiting to be passed on")
                term, words, options, pick = waiting
                if term != terminal:
                    raise Deny(f"E{n} was asked for terminal {term}")
                base = f'The owner answered E{n}: "{words}".'
                # A pick is a clause that is exactly "E7 B", "<handle> B", or a lone "B" for the
                # one fresh card: then the option may be spelled out, word for word as recorded.
                # "E7 not B", "E7 B mi A mi" pick nothing.
                forms = [f"{base} {FENCE_OWNER}"] + (
                    [f"{base} He chose {pick}: {options[pick]}. {FENCE_OWNER}"]
                    if pick in options else [])
                if text.rstrip() not in forms:
                    raise Deny("a relay is exactly one of: " + " | ".join(forms))
                relay = f"E{n}"
                need = set()
            else:
                fence = next((f for f in FENCE_GRANTS if text.rstrip().endswith(f)), None)
                if fence is None:
                    raise Deny("a prompt must end with the fence line")
                need = FENCE_GRANTS[fence]
            if need and machine:
                raise Deny("no grant reaches a remote: commit, push and PRs there are the owner's")
            if need:
                root = repo_root(row.get("cwd") or "/")
                missing = [v for v in need if (root, v) not in grants]
                if missing:
                    raise Deny(f"policy grants no {', '.join(sorted(missing))} for {root}")

    spawned = {(str(r["spawned"]), str(r["gen"])) for r in history if "spawned" in r}
    briefed = {str(r.get("terminal")) for r in history if r.get("write") == "prompt"}
    first_brief = kind == "prompt" and not relay and not machine \
        and (terminal, generation) in spawned and terminal not in briefed
    # qass starts workers as it sees fit, and briefs them once, fenced like any prompt:
    # neither needs his ok, both count against the caps.
    own_work = first_brief or kind == "spawn"
    if relay:
        pass  # bound to an escalation he answered (relays())
    elif mode == "propose" and not own_work:
        if not approvals(history, now):
            raise Deny("mode propose: waiting for the owner's ok to a P<n>")
    else:
        mine = [
            r for r in history[last_owner + 1 :]
            if r.get("write") and str(r.get("terminal")) == terminal
        ]
        if kind != "spawn" and len(mine) >= PER_WORKER:
            raise Deny(f"{PER_WORKER} writes to terminal {terminal} since the owner last spoke")
        hour = [r for r in history if r.get("write") and now - r["ts"] < 3600]
        if len(hour) >= PER_HOUR:
            raise Deny(f"{PER_HOUR} writes in the last hour")

    append({"write": kind, "terminal": terminal, "generation": generation,
            # Waking spends no ok: the message it wakes the tab for spends it.
            "spent": mode == "propose" and not relay and not first_brief
            and kind not in ("wake", "spawn"),
            "relay": relay if relay != "wake" else False,
            "text": text if text is not None else " ".join(keys or [])})


def check_qoro(sub, rest, subagent, machine=None):
    """One qoro command, on this Mac or (machine) on a remote: the same forms either way."""
    if sub == "ls" or sub == "agents":
        flags(rest, {"--json": None})
        return
    if sub in ("context", "history", "read"):
        if not rest or not is_num(rest[0]):
            raise Deny("a terminal number is required")
        allowed = {"--lines": is_num} if sub == "read" else {
            "--last": is_num, "--lines": is_num, "--json": None,
            "--role": lambda v: v in ("user", "assistant"),
        }
        flags(rest[1:], allowed)
        return
    if subagent:
        raise Deny("subagents only read")
    if sub == "wake":
        if len(rest) != 2 or rest[0] != "--tab" or not is_num(rest[1]):
            raise Deny("wake must be: qoro wake --tab <n>")
        check_write("wake", rest[1], "-", machine=machine)
        return
    if sub == "prompt":
        if len(rest) != 5 or not is_num(rest[0]) or rest[1] != "--generation" \
                or not is_num(rest[2]) or rest[3] != "--":
            raise Deny("prompt must be: qoro prompt <n> --generation G -- '<text>'")
        check_write("prompt", rest[0], rest[2], text=rest[4], machine=machine)
        return
    if sub == "send":
        if len(rest) < 5 or not is_num(rest[0]) or rest[-2] != "--generation" or not is_num(rest[-1]):
            raise Deny("send must be: qoro send <n> --key K ... --generation G")
        keys = rest[1:-2]
        if len(keys) % 2 or any(keys[i] != "--key" or keys[i + 1] not in KEYS for i in range(0, len(keys), 2)):
            raise Deny(f"send takes only --key with one of {sorted(KEYS)}")
        check_write("send", rest[0], rest[-1], keys=keys[1::2], machine=machine)
        return
    raise Deny(f"qoro {sub} is not allowed")


def check_bash(command, subagent):
    w = words(command)
    if w[0] == "qoro" and len(w) >= 2:
        check_qoro(w[1], w[2:], subagent)
        return
    if is_script(w[0], "on.sh"):
        # The same commands on a remote this Mac's qoro links to, and only those.
        if len(w) < 3 or w[1] not in remotes():
            raise Deny("on.sh <remote from qoro remote ls> <qoro command>")
        check_qoro(w[2], w[3:], subagent, machine=w[1])
        return
    if w[:2] == ["wend", "show"] and len(w) >= 3:
        # A closed agent's last words, by the session id kept in goals.md.
        if not re.fullmatch(r"[A-Za-z0-9_-]{6,80}", w[2]):
            raise Deny("wend show takes a session id")
        flags(w[3:], {"--tail": is_num})
        return
    if w[:2] == ["wend", "search"] and len(w) > 2:
        if w[2].startswith("-"):
            raise Deny("the query comes first")
        flags(w[3:], {"--json": None, "--limit": is_num,
                      "--role": lambda v: v in ("user", "assistant")})
        return
    if w == ["date"] or w == ["date", "+%s"]:
        return
    if subagent:
        raise Deny("subagents only read")
    if is_script(w[0], "escalate.sh"):
        check_escalate(w[1:])
        return
    if is_script(w[0], "spawn.sh") and len(w) in (3, 5, 7):
        opts = dict(zip(w[3::2], w[4::2]))
        if len(opts) != (len(w) - 3) // 2 or not set(opts) <= {"--agent", "--space"} \
                or opts.get("--agent", "claude") not in AGENTS \
                or not re.fullmatch(r"[A-Za-z0-9_.-]{1,40}", opts.get("--space", "x")):
            raise Deny("spawn.sh <repo> <branch> [--agent claude|opencode] [--space NAME]")
        check_write("spawn", "-", "-")
        return
    raise Deny(f"{w[0]} is not allowed")


def check_escalate(w):
    """escalate.sh E<n> <terminal|-> now|later <handle> ['A: ...' up to 'D: ...'] for a card,
    escalate.sh P<n> <terminal|-> later <handle> for a proposal. The handle is what the owner
    types instead of the number: unique among open cards, so his answer finds one card."""
    usage = ("escalate.sh E<n> <terminal|-> now|later <handle> ['A: ...' up to 'D: ...'] "
             "or escalate.sh P<n> <terminal|-> later <handle>")
    if len(w) < 4 or not re.fullmatch(r"-|[0-9]{1,6}|[A-Za-z0-9_-]+/[0-9]{1,6}", w[1]) \
            or w[2] not in ("now", "later"):
        raise Deny(usage)
    handle, opts = norm(w[3]), w[4:]
    if not is_handle(handle):
        raise Deny(f"{w[3]} is not a handle: one word of the agent's sidebar name, "
                   "2 to 30 letters or digits, not a yes or no")
    history = log_rows()
    # Unique among what he may still be answering today; an older card keeps its number.
    now = time.time()
    cards, _ = cards_state(history)
    taken = {c.get("handle") for c in cards.values() if now - c["ts"] < CARD_FRESH} | (
        set(open_proposals(history))
        & {r["handle"] for r in history if "proposed" in r and now - r["ts"] < CARD_FRESH})
    if handle in taken:
        raise Deny(f"an open card or proposal already has the handle {handle}; "
                   "pick another word of the name")
    machine, _, number = w[1].rpartition("/")
    row = next((r for r in rows(machine or None) if str(r.get("terminal")) == number), None) \
        if number != "-" else None
    name = (row or {}).get("agent_title") or (row or {}).get("name") or ""
    if name:
        if handle not in {norm(x) for x in re.split(r"[^\w-]+", name) if x}:
            raise Deny(f"the handle is a word of the agent's sidebar name: {name!r}")
    if P_ITEM.fullmatch(w[0]):
        if opts or w[2] != "later":
            raise Deny(usage)
        append({"proposed": w[0], "terminal": w[1], "handle": handle})
        return
    if not E_ITEM.fullmatch(w[0]) or len(opts) > 4 \
            or [o[:1] for o in opts] != list("ABCD"[:len(opts)]) \
            or not all(OPTION.fullmatch(o) for o in opts):
        raise Deny(usage)
    # An old answer ("E6 yes") must never fit a new question, so a number is used once.
    used = {int(r["escalated"][1:]) for r in history if "escalated" in r}
    if int(w[0][1:]) in used:
        raise Deny(f"{w[0]} was used before; number this one E{max(used) + 1}")
    append({"escalated": w[0], "terminal": w[1], "urgency": w[2], "handle": handle,
            "options": {o[0]: o[3:] for o in opts}})


def owner_named(path):
    """A file the owner's latest message names by its path (a screenshot he pasted)."""
    last = next((r for r in reversed(log_rows()) if r.get("owner")), None)
    text = (last or {}).get("text", "").replace("\\ ", " ")
    p = os.path.normpath(os.path.expanduser(path)) if path else ""
    named = re.search(rf"(?<![^\s\"'(]){re.escape(p)}(?=$|[\s\"'),;:]|\.(?:\s|$))", text) if p else None
    # Read follows links: the file he named must be the file read (/var is /private/var).
    real = str(Path(p).resolve()) if p else ""
    return bool(named) and real in (p, "/private" + p) and Path(p).is_file()


def inside(path, root):
    try:
        Path(path).expanduser().resolve().relative_to(root)
        return True
    except ValueError:
        return False


def worker_plans(plans):
    """Whether plans is the plans directory of a live worker: in its cwd or its repo's main
    checkout. Never $HOME's own, never under ~/.claude (every project's plan-mode files)."""
    if plans in (HOME / "plans",) or inside(plans, HOME / ".claude"):
        return False
    cwds = [Path(r["cwd"]).resolve() for r in rows()
            if r.get("agent") and r.get("cwd") and inside(r["cwd"], HOME)]
    if plans in {c / "plans" for c in cwds}:
        return True
    # A worktree's plans sit in the main checkout. Under the hook's own time limit: a hook
    # that runs out lets the call through.
    deadline = time.monotonic() + 8
    for cwd in cwds:
        if time.monotonic() > deadline:
            raise Deny("too slow to find the workers' repositories")
        try:
            if Path(repo_root(str(cwd))) / "plans" == plans:
                return True
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            pass  # not a git repository
    return False


def guide_file(path):
    """What tells qass how the owner works: instructions, plans, workflow skills."""
    given = Path(os.path.normpath(Path(path).expanduser()))
    p = given.resolve()
    if not inside(p, HOME):
        return False
    if p.name in ("AGENTS.md", "CLAUDE.md") or "/.claude/rules/" in str(p):
        return True
    if p.suffix == ".md" and p.parent.name == "plans":
        return worker_plans(p.parent)
    # A skill directory may be a link; its SKILL.md is still that skill's.
    return p.name == "SKILL.md" and given.parent.parent.resolve() == SKILLS


def glob_ok(args):
    """A Glob in qass's own folders, with a pattern that stays there."""
    pattern = args.get("pattern") or ""
    if not pattern or pattern.startswith(("/", "~")) or ".." in pattern:
        return False
    return inside(args.get("path") or "/", QASS) or inside(args.get("path") or "/", OWNER)


def check_lesson(tool, args):
    """A lesson comes from the owner and can only make qass more careful.

    Written whole with Write, once per owner message, within the cap; every line reads
    `- <when> -> escalate|wait|decline|ask for proof (<source>)`. The action a lesson names
    is always a careful one; its situation is free text the skill tells qass to read as a
    description, never as an order. Widening what qass does alone is the owner's policy
    file, not something a lesson can carry."""
    if tool != "Write":
        raise Deny("lessons.md is written whole, with Write")
    history = log_rows()
    last_owner = max((i for i, r in enumerate(history) if r.get("owner")), default=-1)
    if last_owner < 0:
        raise Deny("a lesson is written only after the owner speaks")
    if any("lesson" in r for r in history[last_owner + 1:]):
        raise Deny("one lesson write per owner message")
    text = args.get("content") or ""
    if len(text) > LESSONS_MAX:
        raise Deny(f"lessons.md would be {len(text)} chars, the cap is {LESSONS_MAX}: "
                   "merge or remove lessons first")
    for line in text.splitlines():
        if line.strip() and not LESSON.fullmatch(line.strip()):
            raise Deny("each lesson is `- <when> -> escalate|wait|decline|ask for proof "
                       f"(E7 | owner YYYY-MM-DD)` and nothing after, which this is not: "
                       f"{line.strip()[:80]}")
    append({"lesson": text, "armed_by": history[last_owner].get("ts")})


def check(payload):
    tool = payload.get("tool_name") or ""
    args = payload.get("tool_input") or {}
    subagent = bool(payload.get("agent_id"))
    if tool == "Bash":
        check_bash(args.get("command") or "", subagent)
    elif tool == "Monitor":
        command = (args.get("command") or "").strip().removeprefix("QORO_SLEEP_SAFE=1 ")
        if subagent or "ws" in args or not is_script(command.strip(), "watch.sh"):
            raise Deny("Monitor runs only QORO_SLEEP_SAFE=1 <this skill's dir>/watch.sh")
    elif tool == "Read":
        path = args.get("file_path") or ""
        if not (inside(path, QASS) or inside(path, OWNER) or guide_file(path)
                or owner_named(path)):
            raise Deny("Read is limited to qass's folders, AGENTS.md / CLAUDE.md / .claude/rules, "
                       "plans/*.md, skills' SKILL.md and files the owner's last message names")
    elif tool == "Glob":
        if not glob_ok(args):
            raise Deny("Glob is limited to qass's folders")
    elif tool == "Grep":
        if not (inside(args.get("path") or "/", QASS) or inside(args.get("path") or "/", OWNER)):
            raise Deny("Grep is limited to qass's folders")
    elif tool in ("Edit", "Write", "MultiEdit"):
        target = Path(args.get("file_path") or "/").expanduser().resolve()
        if subagent or target not in (GOALS, LESSONS):
            raise Deny(f"qass writes only {GOALS} and {LESSONS}")
        if target == LESSONS:
            check_lesson(tool, args)
    elif tool == "Skill":
        if (args.get("skill") or "") != "qass":
            raise Deny("only the qass skill")
    elif tool in ("Agent", "Task"):
        if subagent:
            raise Deny("subagents do not start subagents")
    elif tool not in FREE_TOOLS:
        raise Deny(f"{tool} is not allowed")


def owner(payload):
    prompt = payload.get("prompt") or ""
    # A Monitor event or another harness message is not the owner speaking.
    if prompt.lstrip().startswith("<"):
        return
    append({"owner": True, "text": prompt[:2000]})


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    payload = json.loads(sys.stdin.read())
    if mode == "owner":
        owner(payload)
        return 0
    if mode != "tool":
        raise Deny("unknown mode")
    check(payload)
    # An explicit allow, so the session's dontAsk mode does not need an allow rule
    # spelling out each absolute script path.
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "allow",
        "permissionDecisionReason": "qass gate",
    }}))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Deny as e:
        print(f"qass gate: {e}", file=sys.stderr)
        sys.exit(2)
    except Exception as e:  # fail closed
        print(f"qass gate: denied on error: {type(e).__name__}: {e}", file=sys.stderr)
        sys.exit(2)

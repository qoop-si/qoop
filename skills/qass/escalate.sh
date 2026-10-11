#!/bin/sh
# qass escalate: make the owner's phone ring for one question.
#
# qass's own tab reports `blocked`, which the qoop bridge pushes as Urgent and
# the notch shows as "wants". The state has to last long enough for the
# bridge to list it: while this Bash call runs, no tool hook and no idle
# composer can take it away, so it holds for 3 s. The bridge sends one push
# per terminal per 30 s and drops a state that is gone when that window ends,
# so a second escalation first waits the window out. Run it alone in its
# message: a parallel tool call would report `working` over it.
#
#   escalate.sh E<n> <terminal it is about, or -> now|later <handle> ['A: ...' ...]
#   escalate.sh P<n> <terminal it is about, or -> later <handle>
#   escalate.sh E<n> <terminal or -> now|later <handle> -   the card on standard input (a heredoc), kept
#                            in the owner's inbox: see "Cards" in SKILL.md for its format. A card that
#                            breaks the format prints every problem and the format, exits 2 and stores
#                            nothing. The trailing `-` is what asks for the block: without it
#                            standard input is never read.
#   escalate.sh done E<n> [note]  the card is handled (answered in the chat, by you, or gone)
#   escalate.sh show E<n>    the card qass is showing him now: his next message answers it
#   escalate.sh next E|P     the number for a new card (E) or proposal (P): never one used
#                            before, even when it is long answered and gone from goals.md
#
# The handle is the word of the agent's sidebar name the owner types to answer.
set -eu

id="${1:?usage: escalate.sh E<n> <terminal or -> now|later <handle> [options]}"
if [ "$id" = show ]; then
    echo "showing ${2:-}"
    exit 0
fi
if [ "$id" = done ]; then
    card="${2:?usage: escalate.sh done E<n> [note]}"
    shift 2
    note="${*:-closed}"
    if out="$(qoop ask close "$card" --note "$note" 2>&1)"; then
        echo "$out"
    else
        # Never in the way of the work: the card may be unknown to an older daemon.
        echo "noted $card (not in the inbox: $out)"
    fi
    exit 0
fi
if [ "$id" = next ]; then
    kind="${2:-E}"
    case "$kind" in E | P) ;; *) echo "usage: escalate.sh next E|P" >&2; exit 2 ;; esac
    dir="@QASS_DATA@"
    mkdir -p "$dir"
    # One count for cards and proposals. A first count starts above every number goals.md has
    # ever shown, so a fresh counter never hands out an E6 an old "E6 yes" could still answer.
    # Written whole through a rename, so a reader never sees it half written.
    # ponytail: no lock, one qass per machine asks for numbers; lock it if that changes.
    n="$(cat "$dir/next-card" 2>/dev/null || true)"
    case "$n" in '' | *[!0-9]*)
        seen="$(grep -oE '\b[EP][0-9]+\b' "$dir/goals.md" 2>/dev/null | tr -d EP | sort -n | tail -1 || true)"
        n=$(( ${seen:-0} + 1 )) ;;
    esac
    echo $((n + 1)) > "$dir/next-card.new" && mv -f "$dir/next-card.new" "$dir/next-card"
    echo "$kind$n"
    exit 0
fi
terminal_arg="${2:--}"
urgency="${3:-now}"
handle="${4:-$id}"
dir="@QASS_DATA@"
mkdir -p "$dir"
# A card for later is only written down, in goals.md. Only now rings.
# The options as given, so qass shows him exactly what a letter will send.
shift 4 2>/dev/null || true
if [ "${1:-}" = - ]; then
    # The card block on standard input, strictly checked and kept by qoop. Exit 2: it breaks the
    # format (nothing stored: the problems are printed, fix it and run again). Exit 3: this qoop
    # cannot keep it (an older or absent daemon): the card text is printed and the ring goes on.
    agent="$terminal_arg"
    rc=0
    qoop ask add "$id" --agent "$agent" --handle "$handle" --urgency "$urgency" || rc=$?
    case "$rc" in
    0) ;;
    3) echo "the inbox needs a restart of qoop; the card stays in the chat only" ;;
    *) exit "$rc" ;;
    esac
else
    for o in "$@"; do echo "  $o"; done
fi
if [ "${urgency:-now}" = later ]; then
    echo "noted $id"
    exit 0
fi
last="$(cat "$dir/last-escalation" 2>/dev/null || echo 0)"
wait=$((last + 31 - $(date +%s)))
[ "$wait" -le 0 ] || sleep "$wait"
qoop hook-report --state blocked --visible-blocker --event blocked --label "needs you: $handle"
date +%s > "$dir/last-escalation"
sleep 3
echo "escalated $id"

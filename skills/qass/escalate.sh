#!/bin/sh
# qass escalate: make the owner's phone ring for one question.
#
# qass's own tab reports `blocked`, which the qoro bridge pushes as Urgent and
# the notch shows as "wants". The state has to last long enough for the
# bridge to list it: while this Bash call runs, no tool hook and no idle
# composer can take it away, so it holds for 3 s. The bridge sends one push
# per terminal per 30 s and drops a state that is gone when that window ends,
# so a second escalation first waits the window out. Run it alone in its
# message: a parallel tool call would report `working` over it.
#
#   escalate.sh E<n> <terminal it is about, or -> now|later <handle> ['A: ...' ...]
#   escalate.sh P<n> <terminal it is about, or -> later <handle>
#
# The handle is the word of the agent's sidebar name the owner types to answer.
set -eu

id="${1:?usage: escalate.sh E<n> <terminal or -> now|later <handle> [options]}"
urgency="${3:-now}"
handle="${4:-$id}"
dir="@QASS_DATA@"
mkdir -p "$dir"
# A card for later is only written down; the gate keeps its options. Only now rings.
# The options exactly as the gate recorded them, so the owner sees what a letter will send.
shift 4 2>/dev/null || true
for o in "$@"; do echo "  $o"; done
if [ "${urgency:-now}" = later ]; then
    echo "noted $id"
    exit 0
fi
last="$(cat "$dir/last-escalation" 2>/dev/null || echo 0)"
wait=$((last + 31 - $(date +%s)))
[ "$wait" -le 0 ] || sleep "$wait"
qoro hook-report --state blocked --visible-blocker --event blocked --label "needs you: $handle"
date +%s > "$dir/last-escalation"
sleep 3
echo "escalated $id"

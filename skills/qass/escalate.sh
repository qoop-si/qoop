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
#   escalate.sh E<n> <terminal it is about, or ->
set -eu

id="${1:?usage: escalate.sh E<n> <terminal or ->}"
dir="$HOME/.qass"
mkdir -p "$dir"
last="$(cat "$dir/last-escalation" 2>/dev/null || echo 0)"
wait=$((last + 31 - $(date +%s)))
[ "$wait" -le 0 ] || sleep "$wait"
qoro hook-report --state blocked --visible-blocker --event blocked --label "needs you: $id"
date +%s > "$dir/last-escalation"
sleep 3
echo "escalated $id"

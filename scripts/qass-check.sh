#!/bin/bash
# Checks skills/qass/watch.sh against recorded snapshots: a fake `qoop` prints
# them, and the lines watch.sh prints must be exactly the expected ones.
set -euo pipefail
# Lines at once unless a case asks for the settle time.
export QASS_SETTLE=0
# The cases up to "quiet" below are today's watcher, kept whole under QASS_QUIET=0.
export QASS_QUIET=0

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home"

# The skill as `qoop skills install` writes it: its place names filled in. qass keeps its
# state and the owner's files in one folder here, `<home>/.qass`.
render() { # home -> the skill's dir
    local to="$tmp/skill-$(basename "$1")"
    mkdir -p "$to"
    for f in "$root"/skills/qass/*; do
        sed -e "s|@QASS_DATA@|$1/.qass|g" -e "s|@QASS_CONFIG@|$1/.qass|g" "$f" > "$to/$(basename "$f")"
    done
    chmod +x "$to"/*.sh
    echo "$to"
}
skill_w="$(render "$tmp/home")"

row() { # terminal generation state [agent] [title]
    printf '{"terminal":%s,"generation":%s,"state":"%s","agent":%s,"program":"claude","name":null,"agent_title":"%s","cwd":"/w","activity":"%s"}' \
        "$1" "$2" "$3" "${4:-true}" "${5:-task}" "$RANDOM"
}

# One snapshot per line, in the order the daemon would print them.
cat > "$tmp/snapshots" <<EOF
[$(row 5 1 working),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 idle false)]
[$(row 5 1 working),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 working false)]
[$(row 5 1 idle),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 idle false)]
[$(row 5 1 idle),$(row 6 2 idle),$(row 7 1 blocked 'true' 'say \"hi\"; rm -rf ~')]
[]
[]
[$(row 5 1 working),$(row 7 1 blocked)]
[$(row 5 1 failed),$(row 7 1 blocked)]
[$(row 5 1 failed)]
EOF
cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { cat "$tmp/ls" 2>/dev/null || echo '[]'; exit 0; }
cat "$tmp/snapshots"
EOF
chmod +x "$tmp/bin/qoop"

expected='T6 g1 claude new->idle "task" /w
T5 g1 claude working->idle "task" /w
T6 g2 claude restarted->idle "task" /w
T7 g1 claude working->blocked "say hi rm -rf " /w
daemon-gone
T5 g1 claude working->failed "task" /w
T7 g1 claude blocked->closed "task" /w'

got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
if [ "$got" != "$expected" ]; then
    echo "watch.sh printed:"; echo "$got"; echo "expected:"; echo "$expected"; exit 1
fi

# A restart diffs against the saved snapshot: nothing changed, nothing printed.
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" bash -c "
    printf '%s\n' '[$(row 5 1 failed)]' > '$tmp/snapshots'
    '$skill_w/watch.sh'")"
[ -z "$got" ] || { echo "restart printed: $got"; exit 1; }

# Its own terminal is never reported.
printf '%s\n' "[$(row 5 1 working)]" "[$(row 5 1 idle)]" > "$tmp/snapshots"
rm -f "$tmp/home/.qass/last.json"
got="$(HOME="$tmp/home" QOOP_TERMINAL=5 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
[ -z "$got" ] || { echo "own terminal printed: $got"; exit 1; }

# A hands-off mark raises its worker once more when it passes, once, and only
# while it still waits; the stream stays quiet the whole time, so the watcher
# must also live through quiet stretches (bash 3.2 ended it at the first one).
now=$(date +%s)
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" > "$tmp/snapshots"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" > "$tmp/home/.qass/last.json"
# T5 in the line shape of a worker with a task, T6 and T7 in the older one: both are read.
printf -- '- T5 g1 0a1b2c3d "fix x" task:12; hands off until %s\n- T6 g1 y: task; hands off until %s\n- T7 g1 z: task; hands off until %s\n' \
    $((now + 3)) $((now - 1)) $((now + 999)) > "$tmp/home/.qass/goals.md"
cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { echo '[]'; exit 0; }
cat "$tmp/snapshots"; exec sleep 22 2>/dev/null
EOF
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" perl -e 'alarm 24; exec @ARGV' "$skill_w/watch.sh" || true)"
[ "$got" = 'T5 g1 claude hands-off->idle "task" /w' ] || { echo "hands-off printed: $got"; exit 1; }

# A tab that slept is not a tab that closed.
printf '%s\n' "[$(row 5 1 idle)]" "[$(row 6 1 working)]" > "$tmp/snapshots"
echo '[{"name":"a","sleeping":[5]}]' > "$tmp/ls"
cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { cat "$tmp/ls"; exit 0; }
cat "$tmp/snapshots"
EOF
rm -f "$tmp/home/.qass/last.json" "$tmp/home/.qass/goals.md"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
[ "$got" = 'T5 g1 claude new->idle "task" /w
T5 g1 claude idle->sleeping "task" /w' ] || { echo "sleep printed: $got"; exit 1; }
rm -f "$tmp/ls"

# A remote's stream comes in beside this Mac's, its lines named after the remote.
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
printf '%s\n' "[$(row 2 3 working)]" "[$(row 2 3 idle)]" > "$tmp/remote-snapshots"
cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = remote ] && { echo "ubox  fake-host"; exit 0; }
[ "\$1" = ls ] && { echo '[]'; exit 0; }
sleep 2; cat "$tmp/snapshots"
EOF
cat > "$tmp/bin/ssh" <<EOF
#!/bin/sh
case "\$*" in *watch*) cat "$tmp/remote-snapshots" ;; *) echo '[]' ;; esac
EOF
chmod +x "$tmp/bin/ssh"
rm -f "$tmp/home/.qass/"*.json "$tmp/home/.qass/goals.md"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null | sort)"
want="$(printf '%s\n' 'T5 g1 claude new->idle "task" /w' 'ubox/T2 g3 claude working->idle "task" /w' 'ubox/link-down' | sort)"
[ "$got" = "$want" ] || { echo "remote printed: $got"; exit 1; }
rm -f "$tmp/bin/ssh"

# A flapping agent is said once, after it settles; one that went back to work is not said.
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working)]" > "$tmp/home/.qass/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 idle)]" "[$(row 5 1 working),$(row 6 1 working)]" \
    "[$(row 5 1 idle),$(row 6 1 working)]" "[$(row 5 1 working),$(row 6 1 working)]" \
    "[$(row 5 1 idle),$(row 6 1 working)]" > "$tmp/snapshots"
: > "$tmp/home/.qass/goals.md"
cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { echo '[]'; exit 0; }
[ "\$1" = remote ] && exit 0
cat "$tmp/snapshots"; exec sleep 14 2>/dev/null
EOF
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 QASS_SETTLE=2 PATH="$tmp/bin:$PATH" perl -e 'alarm 15; exec @ARGV' "$skill_w/watch.sh" || true)"
[ "$got" = 'T5 g1 claude working->idle "task" /w' ] || { echo "settle printed: $got"; exit 1; }

# A row with a task carries it at the end of its line, claim and all.
task_row='{"terminal":8,"generation":1,"state":"%s","agent":true,"program":"claude","name":null,"agent_title":"fix","cwd":"/w","task":{"id":12,"goal":"x","stage":"verifying","claimed":true}}'
printf '%s\n' "[$(printf "$task_row" working)]" "[$(printf "$task_row" idle)]" > "$tmp/snapshots"
rm -f "$tmp/home/.qass/last.json" "$tmp/home/.qass/settle"*
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
[ "$got" = 'T8 g1 claude working->idle "fix" /w task:12:verifying:claimed' ] \
    || { echo "task line printed: $got"; exit 1; }

# ---- quiet: the default watcher --------------------------------------------------------------
export QASS_QUIET=1
q="$tmp/home/.qass"
fresh() { rm -rf "$q"; mkdir -p "$q"; }
# A fake qoop that prints the snapshots in $tmp/snapshots, one every $2 seconds, then keeps the
# stream open $1 seconds; `ls` prints $tmp/ls.
stream() { # hold-open-seconds [gap-seconds]
    cat > "$tmp/bin/qoop" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { cat "$tmp/ls" 2>/dev/null || echo '[]'; exit 0; }
[ "\$1" = remote ] && { cat "$tmp/remotes" 2>/dev/null; exit 0; }
while IFS= read -r l; do printf '%s\n' "\$l"; sleep ${2:-0}; done < "$tmp/snapshots"
exec sleep $1
EOF
    chmod +x "$tmp/bin/qoop"
}
# The watcher for $1 seconds, each line stamped with the seconds since it started.
timed() {
    local start
    start="$(perl -MTime::HiRes=time -e 'printf "%.2f", time')"
    # Stopped with TERM, the way Monitor stops it, so its own way out takes every child along.
    ( HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null &
      w=$!; sleep "$1"; kill -TERM "$w" 2>/dev/null; wait "$w" 2>/dev/null || true ) |
        perl -MTime::HiRes=time -ne "\$|=1; printf '%.1f %s', time() - $start, \$_" || true
}
lines() { cut -d' ' -f2-; }                      # stamped -> plain
within() { awk -v a="$1" -v b="$2" '{ if ($1 < a || $1 > b) bad = 1 } END { exit bad }'; }
one_block() { awk 'NR == 1 { f = $1 } { if ($1 - f > 0.2) bad = 1 } END { exit bad }'; }
rm -f "$tmp/ls" "$tmp/remotes"

# The same day as the first case: the same lines, an idle with no record of qass held until a
# line that is said now takes it along, and nothing lost.
fresh; stream 0
cat > "$tmp/snapshots" <<EOF
[$(row 5 1 working),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 idle false)]
[$(row 5 1 working),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 working false)]
[$(row 5 1 idle),$(row 6 1 idle),$(row 7 1 working),$(row 9 1 idle false)]
[$(row 5 1 idle),$(row 6 2 idle),$(row 7 1 blocked)]
[]
[]
[$(row 5 1 working),$(row 7 1 blocked)]
[$(row 5 1 failed),$(row 7 1 blocked)]
[$(row 5 1 failed)]
EOF
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
want='T6 g1 claude new->idle "task" /w
T6 g2 claude restarted->idle "task" /w
T5 g1 claude working->idle "task" /w
T7 g1 claude working->blocked "task" /w
daemon-gone
T5 g1 claude working->failed "task" /w
T7 g1 claude blocked->closed "task" /w'
[ "$got" = "$want" ] || { echo "quiet day printed:"; echo "$got"; exit 1; }
grep -q 'long: no record T5 g1' "$q/quiet.log" || { echo "a long line left no reason"; exit 1; }
! grep -q '^7 ' "$q/said" || { echo "a closed agent stayed said"; exit 1; }

# A daemon that leaves with agents on it is said once, and not again after a restart; one that
# leaves with none is not said at all.
fresh; stream 0
printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"
printf '%s\n' '[]' > "$tmp/snapshots"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ "$got" = daemon-gone ] || { echo "gone with an agent printed: $got"; exit 1; }
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ -z "$got" ] || { echo "a restart said the gone daemon again: $got"; exit 1; }
fresh
printf '%s\n' "[$(row 9 1 idle false)]" > "$q/last.json"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ -z "$got" ] || { echo "gone with no agent printed: $got"; exit 1; }
grep -q 'daemon gone, no agent' "$q/quiet.log" || { echo "a silent gone was not logged"; exit 1; }

# A line said now takes what is held with it, in one block: the held idle (no record) came 3 s
# earlier and waited for it.
fresh; stream 12 3
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" "[$(row 5 1 idle),$(row 6 1 blocked)]" > "$tmp/snapshots"
got="$(timed 8)"
[ "$(echo "$got" | lines)" = 'T6 g1 claude working->blocked "task" /w
T5 g1 claude working->idle "task" /w' ] || { echo "blocked with held printed: $got"; exit 1; }
echo "$got" | one_block && echo "$got" | within 2.5 6 || { echo "not one block when said: $got"; exit 1; }

# Agents qass prompted (records from its own terminal) that stop within half a minute come as
# one block after the short hold.
fresh; stream 25 2
now=$(date +%s)
printf '%s 99 T5\n%s 99 T6\n%s 99 T7\n' $now $now $now > "$q/prompted.log"
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working),$(row 7 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working),$(row 7 1 working)]" \
    "[$(row 5 1 idle),$(row 6 1 idle),$(row 7 1 working)]" \
    "[$(row 5 1 idle),$(row 6 1 idle),$(row 7 1 idle)]" > "$tmp/snapshots"
got="$(QASS_SETTLE=2 timed 14)"
[ "$(echo "$got" | lines | sort)" = "$(printf '%s\n' 'T5 g1 claude working->idle "task" /w' \
    'T6 g1 claude working->idle "task" /w' 'T7 g1 claude working->idle "task" /w')" ] \
    || { echo "prompted three printed: $got"; exit 1; }
echo "$got" | one_block || { echo "three short lines not one block: $got"; exit 1; }

# An idle with no record waits the long hold (here 15 s), and one written by another terminal
# too; one written by qass waits only the short hold.
for who in none 88 99; do
    fresh; stream 30
    case "$who" in none) ;; *) echo "$(date +%s) $who T5" > "$q/prompted.log" ;; esac
    printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
    printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
    got="$(QASS_SETTLE=2 QASS_QUIET_HOLD=15 timed 24)"
    [ "$(echo "$got" | lines)" = 'T5 g1 claude working->idle "task" /w' ] || { echo "hold $who printed: $got"; exit 1; }
    case "$who" in
        99) echo "$got" | within 0 12 || { echo "qass's own line waited: $got"; exit 1; } ;;
        *) echo "$got" | within 15 24 || { echo "a line of $who did not wait: $got"; exit 1; } ;;
    esac
done
# A log it cannot read gives the short hold, never the long one.
fresh; stream 20
echo "$(date +%s) 88 T5" > "$q/prompted.log"; chmod 000 "$q/prompted.log"
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
got="$(QASS_SETTLE=2 QASS_QUIET_HOLD=600 timed 14)"
chmod 644 "$q/prompted.log"
echo "$got" | within 0 12 && [ -n "$got" ] || { echo "an unreadable log held long: $got"; exit 1; }

# A short line after a long one for the same agent keeps the earlier due time.
fresh; stream 25 3
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle)]" "[$(row 5 1 working)]" "[$(row 5 1 idle)]" > "$tmp/snapshots"
( sleep 4; echo "$(date +%s) 99 T5" >> "$q/prompted.log" ) &
got="$(QASS_SETTLE=2 QASS_QUIET_HOLD=600 timed 14)"
wait
echo "$got" | within 0 13 && [ -n "$got" ] || { echo "a short line after a long one waited: $got"; exit 1; }

# A repeat of an agent waiting on the owner never wakes qass on its own; it expires to the log.
fresh; stream 20
wh='{"terminal":5,"generation":1,"state":"%s","agent":true,"program":"claude","name":null,"agent_title":"x","cwd":"/w","task":{"id":12,"goal":"x","stage":"waiting-human","claimed":false}}'
echo "5 1 idle waiting-human $(( $(date +%s) - 100 ))" > "$q/said"
printf '%s\n' "[$(printf "$wh" working)]" > "$q/last.json"
printf '%s\n' "[$(printf "$wh" idle)]" > "$tmp/snapshots"
got="$(QASS_SETTLE=2 QASS_QUIET_HOLD=5 timed 14)"
[ -z "$got" ] || { echo "a waiting repeat printed: $got"; exit 1; }
grep -q 'ride expired T5' "$q/quiet.log" || { echo "a ride did not expire to the log"; exit 1; }
# The same agent turning waiting-human for the first time is said at once.
fresh; stream 0
echo "5 1 idle implementing $(( $(date +%s) - 100 ))" > "$q/said"
printf '%s\n' "[$(printf "$wh" working)]" > "$q/last.json"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ "$got" = 'T5 g1 claude working->idle "x" /w task:12:waiting-human' ] || { echo "new waiting-human printed: $got"; exit 1; }

# A sleep after an idle already said goes only with another block; a held sleep is said at its
# time although the agent has left the rows.
fresh; stream 20
echo "5 1 idle - $(( $(date +%s) - 100 ))" > "$q/said"
echo '[{"name":"a","sleeping":[5]}]' > "$tmp/ls"
printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"
printf '%s\n' "[$(row 6 1 working)]" > "$tmp/snapshots"
got="$(QASS_QUIET_HOLD=5 timed 14)"
[ -z "$got" ] || { echo "a sleep after a said idle printed: $got"; exit 1; }
fresh; stream 30
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
got="$(QASS_QUIET_HOLD=12 timed 24)"
[ "$(echo "$got" | lines)" = 'T5 g1 claude working->sleeping "task" /w' ] && echo "$got" | within 12 24 \
    || { echo "a held sleep printed: $got"; exit 1; }
rm -f "$tmp/ls"

# A held line outlives a restart of the watcher and is said when due.
fresh; stream 0
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 QASS_QUIET_HOLD=12 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ -z "$got" ] && grep -q '^5 1 long ' "$q/held" || { echo "nothing was held: $got"; exit 1; }
stream 30
got="$(QASS_QUIET_HOLD=12 timed 24)"
[ "$(echo "$got" | lines)" = 'T5 g1 claude working->idle "task" /w' ] || { echo "a held line was lost on restart: $got"; exit 1; }

# Lines held from a snapshot are checked against that snapshot, not the one before it: an idle
# and a blocked in one snapshot both come out, and a restart keeps its new generation.
fresh; stream 0
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 blocked)]" > "$tmp/snapshots"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ "$got" = 'T6 g1 claude working->blocked "task" /w
T5 g1 claude working->idle "task" /w' ] || { echo "one snapshot, held and said, printed: $got"; exit 1; }
fresh
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 2 idle),$(row 6 1 blocked)]" > "$tmp/snapshots"
got="$(HOME="$tmp/home" QOOP_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null)"
[ "$got" = 'T5 g2 claude restarted->idle "task" /w
T6 g1 claude working->blocked "task" /w' ] && grep -q '^5 2 idle ' "$q/said" \
    || { echo "restart beside blocked printed: $got"; exit 1; }

# A short line due within half a minute of another goes in the same block, though it was not
# due yet when the first one was.
fresh; stream 25 5
now=$(date +%s); printf '%s 99 T5\n%s 99 T6\n' $now $now > "$q/prompted.log"
printf '%s\n' "[$(row 5 1 working),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" "[$(row 5 1 idle),$(row 6 1 idle)]" > "$tmp/snapshots"
got="$(QASS_SETTLE=8 timed 24)"
[ "$(echo "$got" | wc -l | tr -d ' ')" = 2 ] && echo "$got" | one_block && echo "$got" | within 8 12 \
    || { echo "a line due soon did not join the block: $got"; exit 1; }

# A ride line goes with the next block.
fresh; stream 12 3
echo "5 1 idle waiting-human $(( $(date +%s) - 100 ))" > "$q/said"
printf '%s\n' "[$(printf "$wh" working),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(printf "$wh" idle),$(row 6 1 working)]" "[$(printf "$wh" idle),$(row 6 1 blocked)]" > "$tmp/snapshots"
got="$(timed 8)"
[ "$(echo "$got" | lines)" = 'T6 g1 claude working->blocked "task" /w
T5 g1 claude working->idle "x" /w task:12:waiting-human' ] || { echo "a ride did not go with a block: $got"; exit 1; }

# A long line over a long one keeps the first deadline: here 15 s, not 15 s after the second.
fresh; stream 30 4
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle)]" "[$(row 5 1 working)]" "[$(row 5 1 idle)]" > "$tmp/snapshots"
got="$(QASS_QUIET_HOLD=15 timed 27)"
[ -n "$got" ] && echo "$got" | within 14 22 || { echo "a second long line pushed the deadline: $got"; exit 1; }

# A record of qass older than the agent's last said line is not about this stop.
fresh; stream 30
echo "5 1 idle - $(date +%s)" > "$q/said"
echo "$(( $(date +%s) - 100 )) 99 T5" > "$q/prompted.log"
printf '%s\n' "[$(row 5 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
got="$(QASS_SETTLE=2 QASS_QUIET_HOLD=15 timed 24)"
[ -n "$got" ] && echo "$got" | within 15 24 || { echo "an old record of qass made a line short: $got"; exit 1; }

# A passed hands-off mark is said at once, and takes a held line with it.
fresh; stream 20
printf -- '- T5 g1 y: task; hands off until %s\n' $(( $(date +%s) - 1 )) > "$q/goals.md"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" > "$q/last.json"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 idle)]" > "$tmp/snapshots"
got="$(timed 14)"
[ "$(echo "$got" | lines)" = 'T5 g1 claude hands-off->idle "task" /w
T6 g1 claude working->idle "task" /w' ] && echo "$got" | one_block || { echo "quiet hands-off printed: $got"; exit 1; }

# A remote whose daemon left with an agent is said once; a restart says nothing again.
fresh; stream 20
echo "ubox  fake-host" > "$tmp/remotes"
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"; printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"
printf '%s\n' "[$(row 2 3 idle)]" > "$q/last-ubox.json"
cat > "$tmp/bin/ssh" <<EOF
#!/bin/sh
case "\$*" in *watch*) echo '[]'; exec sleep 30 ;; esac
echo '[]'
EOF
chmod +x "$tmp/bin/ssh"
got="$(timed 5)"
[ "$(echo "$got" | lines)" = ubox/daemon-gone ] || { echo "remote gone printed: $got"; exit 1; }
got="$(timed 5)"
[ -z "$got" ] || { echo "a restart said the remote gone again: $got"; exit 1; }

# A remote link: a drop under QASS_LINKDOWN says nothing; a longer one says link-down once, and
# link-up when it is back; a restart while down does not say it again.
fresh; stream 40
echo "ubox  fake-host" > "$tmp/remotes"
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"
printf '%s\n' "[$(row 2 3 idle)]" > "$q/last-ubox.json"
fake_ssh() { # seconds the link stays down
    cat > "$tmp/bin/ssh" <<EOF
#!/bin/sh
up=\$(( \$(cat "$tmp/ssh-start") + $1 ))
case "\$*" in *watch*) [ "\$(date +%s)" -ge "\$up" ] || exit 255; echo '[$(row 2 3 idle)]'; exec sleep 40 ;; esac
echo '[]'
EOF
    chmod +x "$tmp/bin/ssh"; date +%s > "$tmp/ssh-start"
}
fake_ssh 3
got="$(QASS_LINKDOWN=8 timed 14)"
[ -z "$got" ] || { echo "a short drop printed: $got"; exit 1; }
# Down past a tick but under QASS_LINKDOWN: still nothing.
fresh; printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"; printf '%s\n' "[$(row 2 3 idle)]" > "$q/last-ubox.json"
fake_ssh 12
got="$(QASS_LINKDOWN=30 timed 22)"
[ -z "$got" ] || { echo "a drop under the limit printed: $got"; exit 1; }
fresh; printf '%s\n' "[$(row 5 1 idle)]" > "$q/last.json"; printf '%s\n' "[$(row 2 3 idle)]" > "$q/last-ubox.json"
fake_ssh 12
got="$(QASS_LINKDOWN=5 timed 22)"
[ "$(echo "$got" | lines)" = 'ubox/link-down
ubox/link-up' ] || { echo "a long drop printed: $got"; exit 1; }
touch "$q/down-ubox" "$q/down-ubox.said"
fake_ssh 999
got="$(QASS_LINKDOWN=5 timed 13)"
[ -z "$got" ] || { echo "a restart while down printed: $got"; exit 1; }
rm -f "$tmp/bin/ssh" "$tmp/remotes"
export QASS_QUIET=0

# Card and proposal numbers come from one count that starts above every number goals.md shows,
# and never hand the same number out twice.
rm -f "$tmp/home/.qass/next-card"
printf -- '# Waiting on you\nE53 · T5 · cv\n- P90 · wend: send x\n' > "$tmp/home/.qass/goals.md"
got="$("$skill_w/escalate.sh" next E) $("$skill_w/escalate.sh" next P) $("$skill_w/escalate.sh" next E)"
[ "$got" = "E91 P92 E93" ] || { echo "numbers: $got"; exit 1; }
printf -- '# Waiting on you\n' > "$tmp/home/.qass/goals.md"
got="$("$skill_w/escalate.sh" next E)"
[ "$got" = "E94" ] || { echo "a removed card freed its number: $got"; exit 1; }
rm -f "$tmp/home/.qass/next-card" "$tmp/home/.qass/goals.md"
got="$("$skill_w/escalate.sh" next E)"
[ "$got" = "E1" ] || { echo "a first count: $got"; exit 1; }
echo half > "$tmp/home/.qass/next-card"
printf -- 'E2E tests over HTTP20, then card E7\n' > "$tmp/home/.qass/goals.md"
got="$("$skill_w/escalate.sh" next E)"
[ "$got" = "E8" ] || { echo "a broken count, or a word read as a card: $got"; exit 1; }

echo "qass watch: ok"

#!/bin/bash
# qass watch: one line per agent change that may need the assistant.
#
# Reads `qoro agents --watch --json` (the whole row array on every change) of this Mac and
# of every remote in `qoro remote ls`, and prints a line only when an agent reaches a
# state that waits on somebody (idle, blocked, failed, done, sleeping), or sleeps, or
# closes. A row also changes on every tool call (activity, cwd), so only terminal,
# generation and state are compared. Its own terminal is skipped.
#
# The last snapshot of each machine is kept in last.json (this Mac) or last-<remote>.json
# in qass's data folder and diffed against on start, so a restart does not lose what
# changed while nothing was watching. No saved snapshot means an empty one: every agent
# already waiting is printed once.
#
# A worker qass left alone because the owner had just typed there carries
# `hands off until <unix time>` on its goals.md line. Once that time passes and
# it still waits, it is printed once more, as `hands-off->idle`.
#
# Started as `QORO_SLEEP_SAFE=1 watch.sh`: that mark on the command line lets qoro put the
# tab to sleep while this runs, and this ends by itself once the agent that started it is
# gone (asleep or closed), within the 10 s tick.
#
# Lines: T52 g3 claude working->idle "fix login" /home/me/src/app      (this Mac)
#        ubox/T2 g3 claude working->idle "fix login" /home/me/src/app  (remote ubox)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd -P)"
dir="@QASS_DATA@"   # filled in by `qoro skills install`
mkdir -p "$dir"
self="${QORO_TERMINAL:-0}"

defs='
def rows: map(select(.agent == true and ($at != "" or .terminal != $self)))
          | map({key: (.terminal | tostring), value: .}) | from_entries;
def waits: . as $s | ["idle", "blocked", "failed", "done", "sleeping"] | index($s) != null;
def clean: tostring | gsub("[^\\p{L}\\p{N} ._/-]"; "") | .[0:40];
def place: tostring | gsub("[^\\p{L}\\p{N} ._/~-]"; "");
def line($r; $from):
  "\($at)T\($r.terminal) g\($r.generation) \($r.program // "-" | clean) \($from)->\($r.state)"
  + " \"\(($r.name // $r.agent_title // "") | clean)\" \($r.cwd // "-" | place)";
'
# $a and $b are row arrays; prints the lines for what changed from $a to $b.
diff="$defs"'
($a | rows) as $old | ($b | rows) as $new
| ($new | to_entries[] | .value as $r | $old[.key] as $o
   | if $o == null then (if ($r.state | waits) then line($r; "new") else empty end)
     elif $o.generation != $r.generation then
       (if ($r.state | waits) then line($r; "restarted") else empty end)
     elif $o.state != $r.state and ($r.state | waits) then line($r; $o.state)
     else empty end),
  ($old | to_entries[] | select($new[.key] == null) | .value
   | . as $r | ($sleeping | index($r.terminal)) as $asleep
   | line($r + {state: (if $asleep != null then "sleeping" else "closed" end)}; $r.state))
'
# Whether a row of $a is gone from $b: only then is `qoro ls` worth asking.
vanished="$defs"'(($a | rows | keys) - ($b | rows | keys)) | length > 0'

# $b is a row array and $due terminal numbers; prints those that still wait.
again="$defs"'
($b | rows) as $new | $due[] | $new[.] // empty | select(.state | waits) | line(.; "hands-off")'

# Lines out one write each, so lines of two machines never run into each other.
say() {
    local l
    while IFS= read -r l; do printf '%s\n' "$l"; done <<< "$1"
    touch "$dir/last-wake"
}

# A machine's own files: "" is this Mac, any other label a remote.
snapshot_file() { if [ -z "$1" ]; then echo "$dir/last.json"; else echo "$dir/last-$1.json"; fi; }
gone_file() { if [ -z "$1" ]; then echo "$dir/gone"; else echo "$dir/gone-$1"; fi; }
at_of() { if [ -z "$1" ]; then echo ""; else echo "$1/"; fi; }

# The tabs asleep on a machine, as a JSON array of terminal numbers.
sleeping_on() {
    local s
    if [ -z "$1" ]; then
        s="$(qoro ls --json 2>/dev/null | jq -c '[.[].sleeping[]?]' 2>/dev/null | head -1 || true)"
    else
        s="$("$here/on.sh" "$1" ls --json 2>/dev/null | jq -c '[.[].sleeping[]?]' 2>/dev/null | head -1 || true)"
    fi
    case "$s" in \[*\]) echo "$s" ;; *) echo '[]' ;; esac
}

# Reads one machine's snapshots on stdin and prints its lines. Anything that is not a row
# array (a login banner, a broken line) is passed over, never fatal.
follow() {
    local label="$1" at prev snap out sleeping file gone
    at="$(at_of "$label")"
    file="$(snapshot_file "$label")"
    gone="$(gone_file "$label")"
    prev="$(cat "$file" 2>/dev/null || true)"
    [ -n "$prev" ] || prev='[]'
    while IFS= read -r snap; do
        case "$snap" in \[*) ;; *) continue ;; esac
        if [ -n "$label" ] && [ -e "$dir/down-$label" ]; then
            echo "${at}link-up"
            rm -f "$dir/down-$label"
        fi
        if [ "$snap" = "[]" ]; then
            # The daemon went away. Say it once; whatever comes back is a new start,
            # not a wave of closes and opens.
            [ -e "$gone" ] || echo "${at}daemon-gone"
            touch "$gone"
            continue
        fi
        if [ ! -e "$gone" ]; then
            sleeping='[]'
            if jq -en --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --arg at "$at" "$vanished" >/dev/null 2>&1; then
                # A tab that slept leaves the agent rows too; `qoro ls` still names it.
                sleeping="$(sleeping_on "$label")"
            fi
            out="$(jq -rn --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --arg at "$at" --argjson sleeping "$sleeping" "$diff" 2>/dev/null)" || continue
            if [ -n "$out" ]; then
                say "$out"
            fi
        fi
        rm -f "$gone"
        prev="$snap"
        printf '%s\n' "$snap" > "$file.tmp" && mv "$file.tmp" "$file"
    done
}

# A remote's stream, kept up: when the link drops it says so once and tries again, every
# 5 s at first and at most every 5 minutes. A drop is not the remote's daemon going away:
# when the link is back its first snapshot is compared with the last one, so what changed
# meanwhile is still said.
follow_remote() {
    local name="$1" wait=5 down="$dir/down-$1" since
    rm -f "$down" "$(gone_file "$name")"   # marks of an earlier run say nothing of this one
    while kill -0 $$ 2>/dev/null; do
        since="$(date +%s)"
        # exec: the stream's ssh is this loop's own child, so it goes when the loop goes.
        follow "$name" < <(exec "$here/on.sh" --master "$name" agents --watch --json 2>/dev/null)
        # Only a link that held a minute starts the wait over: one that drops at once
        # backs off, so a flapping link cannot wake qass again and again.
        [ $(( $(date +%s) - since )) -lt 60 ] || wait=5
        # A remote with no daemon ends its stream too; that was said as daemon-gone.
        if [ ! -e "$down" ] && [ ! -e "$(gone_file "$name")" ]; then
            echo "$name/link-down"
            touch "$down"
        fi
        sleep "$wait"
        wait=$((wait * 2 > 300 ? 300 : wait * 2))
    done
}

# Workers whose hands-off time has passed and that were not printed for it yet, as
# "<remote or -> <terminal>".
due() {
    local now label t until
    [ -f "$dir/goals.md" ] || return 0
    now="$(date +%s)"
    sed -nE 's/^- (([A-Za-z0-9_-]+)\/)?T([0-9]+) .*hands off until ([0-9]+).*/\2 \3 \4/p' \
        "$dir/goals.md" |
        while read -r label t until; do
            [ -n "$until" ] || { until="$t"; t="$label"; label=""; }
            [ "$until" -le "$now" ] || continue
            grep -qx "${label:+$label/}$t $until" "$dir/rechecked" 2>/dev/null && continue
            echo "${label:+$label/}$t $until" >> "$dir/rechecked"
            echo "${label:--} $t"
        done
}

# Every 10 s, apart from the streams: the hands-off marks that have passed, against each
# machine's last snapshot. A `read -t` would do it in the loops, but /bin/bash on macOS
# (3.2) returns the same status for a timeout as for the end of the stream.
(
    # Asked to stop, it takes its sleep with it: that sleep holds the output open.
    trap 'kill "$nap" 2>/dev/null; exit 0' TERM
    while :; do
        sleep 10 &
        nap=$!
        wait "$nap"
        kill -0 $$ 2>/dev/null || exit 0
        # The agent's background shell is our parent; with the agent gone it is launchd's.
        if [ "$(ps -o ppid= -p "$PPID" 2>/dev/null | tr -d ' ')" = 1 ]; then
            kill -TERM $$ 2>/dev/null
            exit 0
        fi
        due | while read -r label t; do
            [ "$label" = - ] && label=""
            # With that machine's daemon away its last snapshot is stale: wait.
            [ ! -e "$(gone_file "$label")" ] || continue
            snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
            out="$(jq -rn --argjson b "$snap" --argjson due "[\"$t\"]" --argjson self "$self" \
                --arg at "$(at_of "$label")" "$again" 2>/dev/null)" || continue
            if [ -n "$out" ]; then
                say "$out"
            fi
        done
    done
) &
kids=$!

for name in $(qoro remote ls 2>/dev/null | awk '$1 ~ /^[A-Za-z0-9_-]+$/ { print $1 }'); do
    follow_remote "$name" &
    kids="$kids $!"
done
# On the way out, each stream's reader (qoro, ssh) goes with the loop that reads it. IFS
# is set here because a signal can arrive inside `IFS= read`, where it is empty.
trap 'IFS=$'"'"' \t\n'"'"'; for k in $kids $$; do pkill -P "$k" 2>/dev/null || true; done
      kill $kids 2>/dev/null || true' EXIT
# Killed rather than ended (Monitor stopping it), go through the same way out.
trap 'exit 143' TERM INT HUP

rm -f "$(gone_file "")"   # a mark left by an earlier run says nothing about this one
follow "" < <(qoro agents --watch --json)

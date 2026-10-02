#!/bin/bash
# qass watch: one line per agent change that may need the assistant.
#
# Reads `qoro agents --watch --json` (the whole row array on every change) and
# prints a line only when an agent reaches a state that waits on somebody
# (idle, blocked, failed, done, sleeping), or closes. A row also changes on
# every tool call (activity, cwd), so only terminal, generation and state are
# compared. Its own terminal is skipped.
#
# The last snapshot is kept in ~/.qass/last.json and diffed against on start,
# so a restart does not lose what changed while nothing was watching. No saved
# snapshot means an empty one: every agent already waiting is printed once.
#
# A worker qass left alone because the owner had just typed there carries
# `hands off until <unix time>` on its ~/.qass/goals.md line. Once that time
# passes and it still waits, it is printed once more, as `hands-off->idle`.
#
# Line: T52 g3 claude working->idle "fix login" /home/me/src/app
set -euo pipefail

dir="$HOME/.qass"
mkdir -p "$dir"
self="${QORO_TERMINAL:-0}"
prev="$(cat "$dir/last.json" 2>/dev/null || true)"
[ -n "$prev" ] || prev='[]'

defs='
def rows: map(select(.agent == true and .terminal != $self))
          | map({key: (.terminal | tostring), value: .}) | from_entries;
def waits: . as $s | ["idle", "blocked", "failed", "done", "sleeping"] | index($s) != null;
def clean: tostring | gsub("[^\\p{L}\\p{N} ._/-]"; "") | .[0:40];
def line($r; $from):
  "T\($r.terminal) g\($r.generation) \($r.program // "-" | clean) \($from)->\($r.state)"
  + " \"\(($r.name // $r.agent_title // "") | clean)\" \($r.cwd // "-" | tostring | gsub("[[:cntrl:]]"; ""))";
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
   | "T\(.terminal) g\(.generation) \(.program // "-" | clean) \(.state)->closed")
'

# $b is a row array and $due terminal numbers; prints those that still wait.
again="$defs"'
($b | rows) as $new | $due[] | $new[.] // empty | select(.state | waits) | line(.; "hands-off")'

# Terminals whose hands-off time has passed and that were not printed for it yet.
due() {
    local now t until
    [ -f "$dir/goals.md" ] || return 0
    now="$(date +%s)"
    sed -nE 's/^- T([0-9]+) .*hands off until ([0-9]+).*/\1 \2/p' "$dir/goals.md" |
        while read -r t until; do
            [ "$until" -le "$now" ] || continue
            grep -qx "$t $until" "$dir/rechecked" 2>/dev/null && continue
            echo "$t $until" >> "$dir/rechecked"
            echo "$t"
        done
}

# Every 10 s, apart from the stream: the hands-off marks that have passed, against the
# last snapshot. A `read -t` would do it in the loop below, but /bin/bash on macOS
# (3.2) returns the same status for a timeout as for the end of the stream.
(
    # Asked to stop, it takes its sleep with it: that sleep holds the output open.
    trap 'kill "$nap" 2>/dev/null; exit 0' TERM
    while :; do
        sleep 10 &
        nap=$!
        wait "$nap"
        kill -0 $$ 2>/dev/null || exit 0
        # With the daemon away the last snapshot is stale: wait for a fresh one.
        [ ! -e "$dir/gone" ] || continue
        ids="$(due | jq -Rn '[inputs | tostring]')" || continue
        [ "$ids" != "[]" ] || continue
        snap="$(cat "$dir/last.json" 2>/dev/null)" || continue
        out="$(jq -rn --argjson b "$snap" --argjson due "$ids" --argjson self "$self" "$again")" || continue
        if [ -n "$out" ]; then
            printf '%s\n' "$out"
            touch "$dir/last-wake"
        fi
    done
) &
ticker=$!
trap 'kill "$ticker" 2>/dev/null || true' EXIT

gone=0
while IFS= read -r snap; do
    if [ "$snap" = "[]" ]; then
        # The daemon went away. Say it once; whatever comes back is a new start,
        # not a wave of closes and opens.
        [ "$gone" = 1 ] || echo "daemon-gone"
        gone=1
        touch "$dir/gone"
        continue
    fi
    if [ "$gone" = 0 ]; then
        out="$(jq -rn --argjson a "$prev" --argjson b "$snap" --argjson self "$self" "$diff")"
        if [ -n "$out" ]; then
            printf '%s\n' "$out"
            touch "$dir/last-wake"
        fi
    fi
    gone=0
    rm -f "$dir/gone"
    prev="$snap"
    printf '%s\n' "$snap" > "$dir/last.json.tmp" && mv "$dir/last.json.tmp" "$dir/last.json"
done < <(qoro agents --watch --json)

#!/bin/bash
# qass watch: one line per agent change that may need the assistant.
#
# Reads `qoop agents --watch --json` (the whole row array on every change) of this Mac and
# of every remote in `qoop remote ls`, and prints a line only when an agent reaches a
# state that waits on somebody (idle, blocked, failed, done, sleeping), or sleeps, or
# closes. A row also changes on every tool call (activity, cwd), so only terminal,
# generation and state are compared. Its own terminal is skipped.
#
# The last snapshot of each machine is kept in last.json (this Mac) or last-<remote>.json
# in qass's data folder and diffed against on start, so a restart does not lose what
# changed while nothing was watching. No saved snapshot means an empty one: every agent
# already waiting is printed once.
#
# An agent that stops working is said once it has stayed stopped for QASS_SETTLE seconds
# (20, said on the next 10 s tick): an agent that wakes for a background notice, works a
# second and stops again, over and over, is one line once it settles, not one a turn.
#
# A worker qass left alone because the owner had just typed there carries
# `hands off until <unix time>` on its goals.md line. Once that time passes and
# it still waits, it is printed once more, as `hands-off->idle`.
#
# Started as `QOOP_SLEEP_SAFE=1 watch.sh`: that mark on the command line lets qoop put the
# tab to sleep while this runs, and this ends by itself once the agent that started it is
# gone (asleep or closed), within the 10 s tick, and after QASS_WATCH_FOR seconds (1740, under
# the Monitor's 30 minute cap), then with the line `watch-ends`: start it again.
#
# Lines: T52 g3 claude working->idle "fix login" /home/me/src/app      (this Mac)
#        ubox/T2 g3 claude working->idle "fix login" /home/me/src/app  (remote ubox)
#        ASK E75 answered a -> T224   (the owner answered a card: nothing to do, see asks_said)
# A row with an open task (`qoop task`) ends with its id and stage, and `:claimed` once the
# agent said done: ... /home/me/src/app task:12:verifying:claimed
#
# Quiet (the default; QASS_QUIET=0 gives everything above and nothing below). Every line wakes
# qass, and a wake re-reads its whole conversation, so only what needs it now is said now:
# - At once, with whatever is held: ->blocked, ->failed, ->closed, a task stage that just
#   became waiting-human, a first claim of done (:claimed), hands-off->idle, daemon-gone with agents on it, a link down for
#   QASS_LINKDOWN seconds (60). Everything else is held, one entry per agent, and printed in
#   one block, so Monitor brings several in one wake.
# - Short hold (QASS_SETTLE): a line about work qass started (the newest record in
#   prompted.log for that terminal comes from qass's own terminal, written by `qoop prompt`,
#   `qoop send` and on.sh), new, restarted and starting lines, remote agents.
# - Long hold (QASS_QUIET_HOLD, 600 s from its first line): everything else, an agent woken
#   by its own background work or driven by the owner. It rides along with any earlier block.
# - Ride: a repeat of an agent already said waiting on the owner, and a sleep after an idle
#   already said, only ever go with another block.
# - A remote whose daemon left with no agent on it, and lines that went stale, go to
#   quiet.log instead. Held lines and marks outlive a restart of this script.
# - Routine, silent unless the file `routine` in qass's folder holds `say` (the owner's word:
#   `routine off`, `routine on`): a plain rest (working or starting to idle or done, no claim,
#   not waiting-human) is held QASS_ROUTINE_HOLD seconds (1800) and said only if the agent is
#   still there, never riding along with another block; a sleep, and a daemon leaving
#   with no agent working or blocked on it, go to quiet.log only.
# - With cswap installed, a Claude agent of this Mac that stopped on its usage limit comes out
#   at once as `->limited ... resets:7:20am` (`again` when it was limited minutes before).
#
# The owner's switch, the file `watch` in qass's folder (`qoop qass watch on|off`):
# - `off`: this says nothing more and ends with the `watch-off: ...` line, at start or within a
#   tick of the switch; qass does not start it again, so nothing wakes qass until he writes.
# - `resume <stamp>` (on again): a stamp not in `watch-seen` means no watcher started since. This
#   writes it there, drops the held lines and marks every machine (`base`, `base-<remote>`): the
#   first snapshot each one sends is then the base, said nothing of, whichever run of this gets
#   it. qass gives the owner one summary of the fleet itself, and this says what changes from
#   then on. Only qoop writes `watch`; this never touches it.
# - One watcher at a time (`watch.pid`): a second one says `watch-running` and ends.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd -P)"
dir="@QASS_DATA@"   # filled in by `qoop skills install`
mkdir -p "$dir"
self="${QOOP_TERMINAL:-${QORO_TERMINAL:-0}}"
settle="${QASS_SETTLE:-20}"
quiet="${QASS_QUIET:-1}"
batch="${QASS_BATCH:-30}"
qhold="${QASS_QUIET_HOLD:-600}"
linkdown="${QASS_LINKDOWN:-60}"
vcheck="${QASS_REMOTE_VERSION_EVERY:-60}"
rhold="${QASS_ROUTINE_HOLD:-1800}"
# A Monitor is killed at 30 minutes and its one notice of that can be lost (2026-10-12: it came
# in the second an event did, and nothing watched for an hour). So this ends first, by itself.
watch_for="${QASS_WATCH_FOR:-1740}"
started="$(date +%s)"

defs='
def rows: map(select(.agent == true and ($at != "" or .terminal != $self)))
          | map({key: (.terminal | tostring), value: .}) | from_entries;
def waits: . as $s | ["idle", "blocked", "failed", "done", "sleeping"] | index($s) != null;
def clean: tostring | gsub("[^\\p{L}\\p{N} ._/-]"; "") | .[0:40];
def place: tostring | gsub("[^\\p{L}\\p{N} ._/~-]"; "");
def task: if .task == null then "" else
  " task:\(.task.id | clean):\(.task.stage | clean)" + (if .task.claimed then ":claimed" else "" end) end;
def line($r; $from):
  "\($at)T\($r.terminal) g\($r.generation) \($r.program // "-" | clean) \($from)->\($r.state)"
  + " \"\(($r.name // $r.agent_title // "") | clean)\" \($r.cwd // "-" | place)\($r | task)";
'
# $a and $b are row arrays; prints the lines for what changed from $a to $b.
diff="$defs"'
($a | rows) as $old | ($b | rows) as $new
| ($new | to_entries[] | .value as $r | $old[.key] as $o
   | if $o == null then (if ($r.state | waits) then line($r; "new") else empty end)
     elif $o.generation != $r.generation then
       (if ($r.state | waits) then line($r; "restarted") else empty end)
     elif $o.state != $r.state and ($r.state | waits) then line($r; $o.state)
     # Still at rest, its task only now waiting on the owner (the daemon moves a question
     # left unanswered there): news as well.
     elif ($r.state | waits) and ($r.task.stage? // "") == "waiting-human"
          and ($o.task.stage? // "") != "waiting-human" then line($r; $o.state)
     else empty end),
  ($old | to_entries[] | select($new[.key] == null) | .value
   | . as $r | ($sleeping | index($r.terminal)) as $asleep
   | line($r + {state: (if $asleep != null then "sleeping" else "closed" end)}; $r.state))
'
# Whether a row of $a is gone from $b: only then is `qoop ls` worth asking.
vanished="$defs"'(($a | rows | keys) - ($b | rows | keys)) | length > 0'

# $b is a row array; prints the held line for terminal $t when it still is generation $g
# in state $state.
settled="$defs"'
($b | rows)[$t] // empty | select(.generation == $g and .state == $state) | line(.; $from)'

# $b is a row array and $due terminal numbers; prints those that still wait.
again="$defs"'
($b | rows) as $new | $due[] | $new[.] // empty | select(.state | waits) | line(.; "hands-off")'

# Whether a row array $a holds an agent: a machine that goes away with none says nothing.
has_agents="$defs"'($a | rows | length) > 0'
# Whether it holds one at work or waiting at a menu: with routine silent, only then is a
# machine going away worth a word.
has_busy="$defs"'[$a | rows[] | select(.state != "idle" and .state != "done" and .state != "sleeping")] | length > 0'

# Whether routine is silent: unless the owner said `routine on` (the file holds `say`).
routine_quiet() { [ "$(cat "$dir/routine" 2>/dev/null)" != say ]; }

# Whether the owner turned watching off, and the line that ends this then: in words a qass
# holding an older skill text still follows.
watch_off() { [ "$(cat "$dir/watch" 2>/dev/null)" = off ]; }
# The `resume` stamp no watcher has started on yet, if any.
watch_unseen() {
    local w t
    { read -r w t < "$dir/watch"; } 2>/dev/null || true
    [ "${w:-}" = resume ] && [ -n "${t:-}" ] && [ "$t" != "$(cat "$dir/watch-seen" 2>/dev/null)" ] && echo "$t"
}
base_file() { if [ -z "$1" ]; then echo "$dir/base"; else echo "$dir/base-$1"; fi; }
# Says `$1` and ends, once: a second stop asked for meanwhile says nothing more.
end_with() { trap '' USR1 USR2; printf '%s\n' "$1"; exit 0; }
seen_stamp() { printf '%s\n' "$1" > "$dir/watch-seen.$$" && mv "$dir/watch-seen.$$" "$dir/watch-seen"; }
off_line='watch-off: the owner turned watching off (qoop qass watch); do not start this again until it reads on'

# Reads held entries ("<t> <gen> <class> <due> <arrival> <line>") against the row array $b and
# prints each as "P <line to say>" or "D <line>" (went stale). A sleep or a close holds while
# the agent is still away; any other line is said as the row is now, while it still waits.
recheck="$defs"'
($b | rows) as $new
| capture("^(?<t>[0-9]+) (?<g>[^ ]+) [^ ]+ [0-9]+ (?<arr>[0-9]+) (?<line>.*)$") as $h
| ($h.line | capture("^(?:[A-Za-z0-9_-]+/)?T[0-9]+ g[^ ]+ [^ ]+ (?<from>[a-z-]+)->(?<state>[a-z]+)")) as $m
| $new[$h.t] as $r
| "P \($h.arr) " as $say
| if $m.state == "sleeping" or $m.state == "closed" then
    (if $r == null then $say + $h.line else "D " + $h.line end)
  elif $r == null or (($r.state | waits) | not) then "D " + $h.line
  elif ($r.generation | tostring) == $h.g then $say + line($r; $m.from)
  else $say + line($r; "restarted") end'

# A machine's lines held until they settle: "<terminal> <generation> <from> <state> <due>".
settle_file() { if [ -z "$1" ]; then echo "$dir/settle"; else echo "$dir/settle-$1"; fi; }

# One writer at a time for the settle files: the streams add, the tick takes.
settle_locked() {
    local i rc
    for i in $(seq 50); do
        if mkdir "$dir/settle.lock" 2>/dev/null; then
            "$@"; rc=$?
            rmdir "$dir/settle.lock"
            return $rc
        fi
        sleep 0.1
    done
    "$@"
}

# Holds a working->idle or ->done line of a machine instead of saying it: a newer one for
# the same terminal replaces it, so a flapping agent keeps pushing its own line back.
held='^([A-Za-z0-9_-]+/)?T([0-9]+) g([0-9]+) [^ ]+ (working|starting)->(idle|done) '
hold() {
    local label="$1" line="$2" f
    [ "$settle" -gt 0 ] || return 1
    [[ "$line" =~ $held ]] || return 1
    f="$(settle_file "$label")"
    settle_locked eval '{ grep -v "^${BASH_REMATCH[2]} " "$f" 2>/dev/null; echo "${BASH_REMATCH[2]} ${BASH_REMATCH[3]} ${BASH_REMATCH[4]} ${BASH_REMATCH[5]} $(( $(date +%s) + settle ))"; } > "$f.tmp"; mv "$f.tmp" "$f"'
    return 0
}

# The held lines whose time has come, taken out of the file, as they were held.
take_due() {
    local f="$1" now
    now="$(date +%s)"
    [ -f "$f" ] || return 0
    awk -v now="$now" '$5 <= now' "$f"
    awk -v now="$now" '$5 > now' "$f" > "$f.tmp"; mv "$f.tmp" "$f"
}

# Lines out one write each, so lines of two machines never run into each other.
say() {
    ! watch_off || return 0
    local l
    while IFS= read -r l; do printf '%s\n' "$l"; done <<< "$1"
    touch "$dir/last-wake"
}

# A machine's own files: "" is this Mac, any other label a remote.
snapshot_file() { if [ -z "$1" ]; then echo "$dir/last.json"; else echo "$dir/last-$1.json"; fi; }
# Quiet: its held lines, "<t> <gen> <short|long|ride> <due> <arrival> <line>", and its said
# lines, "<t> <gen> <state> <stage> <arrival>". Not the settle files: an older watcher still
# running from an older install reads and clears those.
held_file() { if [ -z "$1" ]; then echo "$dir/held"; else echo "$dir/held-$1"; fi; }
said_file() { if [ -z "$1" ]; then echo "$dir/said"; else echo "$dir/said-$1"; fi; }

# One line per answer the owner gave from the inbox (phone, desk or terminal) since the last look,
# after it went out: `ASK E75 answered a -> T224` (an option, or `text`), `ASK E3 passed a -> qass`.
# qass never has to act on it: the card is `answered` or `passed` in `qoop ask ls`, and the
# worker was already told. The first look only records what is there. A card answered again
# after it came back open is a new line (its closed time is part of what is remembered).
asks_said() {
    local f="$dir/asks-seen" cur new
    cur="$(qoop ask ls --json 2>/dev/null | jq -r '(.live + .ended)[]
        | select(.status == "answered" or .status == "passed")
        | "\(.closed_at // 0) ASK \(.card) \(.status) \(.option // "text") -> \(if .status == "passed" then "qass" else (.agent // "-") end)"' 2>/dev/null)" || return 0
    if [ -f "$f" ]; then
        new="$(printf '%s\n' "$cur" | grep -vxFf "$f" || true)"
        [ -z "$new" ] || say "$(printf '%s\n' "$new" | sed -E 's/^[0-9]+ //')"
    fi
    printf '%s\n' "$cur" > "$f"
}

# Why a line was not said, for whoever wonders what qass did not hear.
note() { printf '%s %s %s\n' "$(date +%s)" "$1" "$2" >> "$dir/quiet.log" 2>/dev/null || true; }

# A line's parts, in L_LABEL L_T L_G L_FROM L_STATE L_STAGE ("-" with no task). Fails for a
# line that is not about an agent (daemon-gone, link-down).
parse_line() {
    local re='^(([A-Za-z0-9_-]+)/)?T([0-9]+) g([^ ]+) [^ ]+ ([a-z-]+)->([a-z]+)'
    local task=' task:[^: ]+:([a-z-]+)(:claimed)?( |$)'
    L_LABEL="" L_T="" L_G="" L_FROM="" L_STATE="" L_STAGE="-" L_CLAIMED=""
    [[ "$1" =~ $re ]] || return 1
    L_LABEL="${BASH_REMATCH[2]}" L_T="${BASH_REMATCH[3]}" L_G="${BASH_REMATCH[4]}"
    L_FROM="${BASH_REMATCH[5]}" L_STATE="${BASH_REMATCH[6]}"
    if [[ "$1" =~ $task ]]; then L_STAGE="${BASH_REMATCH[1]}" L_CLAIMED="${BASH_REMATCH[2]}"; fi
    return 0
}

# The said line of terminal $2 on machine $1, as "<t> <gen> <state> <stage> <arrival>".
said_of() { grep "^$2 " "$(said_file "$1")" 2>/dev/null | tail -1 || true; }

# Whether a line is said at once (see the top of this file). Run parse_line first.
urgent() {
    local s_t s_gen s_state s_stage _
    case "$1" in *daemon-gone | */link-down | */link-up) return 0 ;; esac
    parse_line "$1" || return 0
    case "$L_STATE" in blocked | failed | closed | limited) return 0 ;; esac
    [ "$L_FROM" != hands-off ] || return 0
    if [ "$L_STAGE" = waiting-human ] || [ -n "$L_CLAIMED" ]; then
        read -r s_t s_gen s_state s_stage _ <<< "$(said_of "$L_LABEL" "$L_T")"
        # A first waiting-human, or a first claim of done ("Done, checked" runs at once).
        [ "$L_STAGE" != waiting-human ] || [ "${s_stage%:claimed}" = waiting-human ] || return 0
        [ -z "$L_CLAIMED" ] || { [ "${s_gen:-}" = "$L_G" ] && [ "${s_stage:-}" != "${s_stage%:claimed}" ]; } ||
            return 0
    fi
    return 1
}

# The newest prompted.log record for terminal $1 of this Mac, as "<unix> <sender>": who wrote
# to it last through `qoop prompt` or `qoop send`. "unreadable" when the log cannot be read.
record_of() {
    local log="$dir/prompted.log"
    [ -e "$log" ] || return 0
    [ -r "$log" ] || { echo unreadable; return 0; }
    tail -n 2000 "$log" | awk -v t="T$1" '$3 == t { last = $1 " " $2 } END { if (last != "") print last }'
}

# The hold for the agent line just parsed, on machine $1: short, long or ride. A long one says
# why in quiet.log, so a write qass made that left no record shows up there.
hold_class() {
    local s_t s_gen s_state s_stage s_arr r_at r_from
    case "$L_FROM" in new | restarted | starting) echo short; return ;; esac
    [ -z "$1" ] || { echo short; return; }
    read -r s_t s_gen s_state s_stage s_arr <<< "$(said_of "" "$L_T")"
    if [ "$L_STATE" = sleeping ] && [ "${s_gen:-}" = "$L_G" ] &&
        { [ "${s_state:-}" = idle ] || [ "${s_state:-}" = done ]; } &&
        ! grep -q "^$L_T " "$(held_file "")" 2>/dev/null; then
        echo ride; return
    fi
    read -r r_at r_from <<< "$(record_of "$L_T")"
    if [ "${r_at:-}" = unreadable ]; then echo short; return; fi
    if [ -n "${r_at:-}" ] && [ "${r_from:-}" = "$self" ] && [ "$r_at" -ge "${s_arr:-0}" ]; then
        echo short; return
    fi
    if [ "$L_STAGE" = waiting-human ] && [ "${s_stage%:claimed}" = waiting-human ] &&
        [ "${s_gen:-}" = "$L_G" ] && [ "${s_state:-}" = "$L_STATE" ]; then
        echo ride; return
    fi
    echo long
}

# Holds a line, one entry per agent: a newer line takes the text, the earlier due time and the
# first arrival stand, a short or long hold outranks a ride. Runs under the settle lock.
hold_put() { # label terminal generation class line
    local f now due class o_t o_g o_class o_due o_arr _ arr
    f="$(held_file "$1")"
    now="$(date +%s)"
    class="$4" arr="$now"
    case "$class" in short) due=$((now + settle)) ;; routine) due=$((now + rhold)) ;; *) due=$((now + qhold)) ;; esac
    read -r o_t o_g o_class o_due o_arr _ <<< "$(grep "^$2 " "$f" 2>/dev/null | tail -1 || true)"
    if [ -n "${o_t:-}" ]; then
        [ "$o_due" -ge "$due" ] || due="$o_due"
        arr="$o_arr"
        # A routine line is the agent's newest word: it never inherits an earlier class.
        [ "$class" != routine ] || { due=$((now + rhold)); arr="$now"; }
        case "$class.$o_class" in
            routine.*) ;;
            short.* | *.short) class=short ;;
            long.* | *.long) class=long ;;
        esac
    fi
    { grep -v "^$2 " "$f" 2>/dev/null || true
      echo "$2 $3 $class $due $arr $5"; } > "$f.tmp"
    mv "$f.tmp" "$f"
}

# Notes each said line as its agent's last, with when it first came: a record from qass newer
# than that makes the agent's next line short. A closed agent is forgotten.
said_put() { # line arrival
    local f
    parse_line "$1" || return 0
    f="$(said_file "$L_LABEL")"
    { grep -v "^$L_T " "$f" 2>/dev/null || true
      [ "$L_STATE" = closed ] || echo "$L_T $L_G $L_STATE $L_STAGE${L_CLAIMED} $2"; } > "$f.tmp"
    mv "$f.tmp" "$f"
}

# Says lines in one block, so Monitor brings them as one wake: $1 (lines said at once, maybe
# none) first, then every held line that goes with them, each checked against the latest
# snapshot. With no $1 it says nothing until a held line is due. Runs under the settle lock;
# the held entries go only after the block is out, so a kill in between says twice, not never.
flush() {
    local now f label snap sel out l go="" block="" said="" picked=""
    # Off: everything stays held, and is dropped when the owner turns it on again.
    ! watch_off || return 0
    now="$(date +%s)"
    [ -z "$1" ] || go=1
    while IFS= read -r l; do
        [ -n "$l" ] && parse_line "$l" || continue
        # A line said at once is newer than whatever was held for its agent.
        f="$(held_file "$L_LABEL")"
        if grep -q "^$L_T " "$f" 2>/dev/null; then
            { grep -v "^$L_T " "$f" || true; } > "$f.tmp"
            mv "$f.tmp" "$f"
        fi
    done <<< "$1"
    for f in "$dir"/held "$dir"/held-*; do
        [ -s "$f" ] || continue
        case "$f" in *.tmp) continue ;; esac
        [ -n "$go" ] || ! awk -v now="$now" '$3 != "ride" && $4 <= now { d = 1 } END { exit !d }' "$f" || go=1
    done
    [ -n "$go" ] || return 0
    for f in "$dir"/held "$dir"/held-*; do
        [ -s "$f" ] || continue
        case "$f" in *.tmp) continue ;; esac
        label="${f#"$dir"/held}"; label="${label#-}"
        # With that machine's daemon away its rows are stale: keep its lines for later.
        [ ! -e "$(gone_file "$label")" ] || continue
        snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
        # Short lines first, then long, then ride: the most urgent first in the block.
        sel="$(awk -v now="$now" -v b="$batch" '($3 == "routine" ? $4 <= now : $3 != "short" || $4 <= now + b) {
            print ($3 == "short" ? 0 : $3 == "long" ? 1 : $3 == "routine" ? 2 : 3) " " NR " " $0 }' "$f" |
            sort -n -k1,1 -k2,2 | cut -d' ' -f3-)"
        [ -n "$sel" ] || continue
        # Taken only once checked: a failed check keeps them held for the next round.
        out="$(printf '%s\n' "$sel" | jq -rR --argjson b "$snap" --argjson self "$self" \
            --arg at "$(at_of "$label")" "$recheck" 2>/dev/null)" || continue
        picked="$picked$f
"
        while IFS= read -r l; do
            case "$l" in
                "P "*) l="${l#P }"; block="$block${l#* }
"; said="$said${l%% *} ${l#* }
" ;;
                "D "*) note stale "${l#D }" ;;
            esac
        done <<< "$out"
    done
    block="$(printf '%s' "$1${1:+
}$block")"
    if [ -n "$block" ]; then
        ! watch_off || return 0
        printf '%s\n' "$block"
        touch "$dir/last-wake"
    fi
    while IFS= read -r l; do [ -z "$l" ] || said_put "$l" "$now"; done <<< "$1"
    while IFS= read -r l; do [ -z "$l" ] || said_put "${l#* }" "${l%% *}"; done <<< "$said"
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        awk -v now="$now" -v b="$batch" '!($3 == "routine" ? $4 <= now : $3 != "short" || $4 <= now + b)' "$f" > "$f.tmp"
        mv "$f.tmp" "$f"
    done <<< "$picked"
}

# A ride line nothing carried for QASS_QUIET_HOLD seconds: a repeat or a sleep, not worth a
# wake of its own. Runs under the settle lock.
ride_expire() {
    local f now
    now="$(date +%s)"
    for f in "$dir"/held "$dir"/held-*; do
        [ -s "$f" ] || continue
        case "$f" in *.tmp) continue ;; esac
        awk -v now="$now" '$3 == "ride" && $4 <= now' "$f" | while IFS= read -r l; do note "ride expired" "${l#* * * * * }"; done
        awk -v now="$now" '!($3 == "ride" && $4 <= now)' "$f" > "$f.tmp"
        mv "$f.tmp" "$f"
    done
}

# A Claude agent of this Mac that stopped on its usage limit (its screen shows the limit and the
# live "continuing automatically" countdown), named when cswap is there to switch the account:
# the line comes out as ->limited with the reset time on its screen, and
# ends " again" when the same agent was limited within QASS_QUIET_HOLD seconds. Anything else,
# or a screen it cannot read, leaves the line as it was.
limit_line() { # label line -> the line to say
    local screen when again
    if [ -n "$1" ] || ! parse_line "$2" || ! command -v cswap >/dev/null 2>&1; then
        printf '%s\n' "$2"; return 0
    fi
    case "$L_FROM.$L_STATE" in working.idle | *.failed) ;; *) printf '%s\n' "$2"; return 0 ;; esac
    [[ "$2" =~ ^T[0-9]+\ g[^\ ]+\ claude\  ]] || { printf '%s\n' "$2"; return 0; }
    screen="$(perl -e 'alarm 3; exec @ARGV' qoop read "$L_T" </dev/null 2>/dev/null | grep -v '^[[:space:]]*$' | tail -n 15 || true)"
    # The live countdown too: the limit line stays in the scrollback after the agent went on.
    printf '%s\n' "$screen" | grep -qE '^[[:space:]]*(⚠[[:space:]]*)?Usage limit reached( again)? · ' &&
        printf '%s\n' "$screen" | grep -qi 'continuing automatically at' ||
        { printf '%s\n' "$2"; return 0; }
    when="$(printf '%s\n' "$screen" | grep -oE '(resets|automatically at) [0-9]{1,2}(:[0-9]{2})? ?([aApP][mM])?' |
        head -n 1 | sed -E 's/^(resets|automatically at) //' | tr -d ' ' || true)"
    again="$(settle_locked limit_note "$L_T")"
    # The same stop seen twice (its idle, then its failure): said once.
    [ "$again" != same ] || { note "limit already said" "$2"; return 0; }
    printf '%s resets:%s%s\n' "${2/->$L_STATE /->limited }" "${when:-?}" "${again:+ again}"
}

# Notes a limit of terminal $1: prints "same" when it had one under 10 s ago (one stop seen
# twice, its idle and its failure), "again" when within QASS_QUIET_HOLD seconds.
limit_note() {
    local f="$dir/limited" now t at
    now="$(date +%s)"
    read -r t at <<< "$(grep "^$1 " "$f" 2>/dev/null | tail -1 || true)"
    [ -z "${at:-}" ] || [ $((now - at)) -ge 10 ] || { echo same; return 0; }
    { grep -v "^$1 " "$f" 2>/dev/null || true; echo "$1 $now"; } > "$f.tmp"
    mv "$f.tmp" "$f"
    [ -z "${at:-}" ] || [ $((now - at)) -ge "$qhold" ] || echo again
}

# One line of a machine's diff, quiet: said now (with what is held), or held.
quiet_hold() { # label line
    local class
    parse_line "$2" || return 0
    class="$(hold_class "$1")"
    case "$class" in
        long) if [ -z "$(record_of "$L_T" 2>/dev/null)" ]; then note "long: no record" "$2"; else note "long: written by another" "$2"; fi ;;
    esac
    if [ "$class" = short ] && [ "$settle" -le 0 ]; then
        settle_locked flush "$2"
    else
        settle_locked hold_put "$1" "$L_T" "$L_G" "$class" "$2"
    fi
}

# Whether a line is routine (see the top of this file). Run after `urgent`, which parsed it.
routine() {
    parse_line "$1" || return 1
    [ "$L_STATE" != sleeping ] || return 0
    case "$L_FROM.$L_STATE" in working.idle | working.done | starting.idle | starting.done) ;; *) return 1 ;; esac
    [ "$L_STAGE" != waiting-human ] && [ -z "$L_CLAIMED" ]
}

# A routine line: a sleep is only logged, unless a line that is not routine is held for its
# agent (a question it left): then it goes as any held line; a rest is held for the routine hold.
quiet_routine() { # label line
    if [ "$L_STATE" = sleeping ]; then
        if awk -v t="$L_T" '$1 == t && $3 != "routine" { f = 1 } END { exit !f }' \
            "$(held_file "$1")" 2>/dev/null; then
            quiet_hold "$1" "$2"
        else
            note "routine: asleep" "$2"
        fi
    else
        settle_locked hold_put "$1" "$L_T" "$L_G" routine "$2"
    fi
}

# A remote link down for QASS_LINKDOWN seconds is said once; back up, said again only then.
link_down() { # name; under the settle lock
    [ -e "$dir/down-$1" ] && [ ! -e "$dir/down-$1.said" ] || return 0
    touch "$dir/down-$1.said"
    flush "$1/link-down"
}
link_up() { # name; under the settle lock
    local was=""
    [ ! -e "$dir/down-$1.said" ] || was=1
    rm -f "$dir/down-$1" "$dir/down-$1.said"
    [ -z "$was" ] || flush "$1/link-up"
}
# When a link went down, as written in its mark; an empty mark (an older watcher's) starts now.
down_since() {
    local t
    t="$(cat "$1" 2>/dev/null || true)"
    case "$t" in '' | *[!0-9]*) t="$(date +%s)"; [ ! -e "$1" ] || echo "$t" > "$1" ;; esac
    echo "$t"
}
gone_file() { if [ -z "$1" ]; then echo "$dir/gone"; else echo "$dir/gone-$1"; fi; }
at_of() { if [ -z "$1" ]; then echo ""; else echo "$1/"; fi; }

# The tabs asleep on a machine, as a JSON array of terminal numbers.
sleeping_on() {
    local s
    if [ -z "$1" ]; then
        s="$(qoop ls --json 2>/dev/null | jq -c '[.[].sleeping[]?]' 2>/dev/null | head -1 || true)"
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
        # On again after an off: qass sums the fleet up itself, so the first snapshot a
        # machine sends is the base and nothing is said of it, nor of its link or daemon then.
        if [ "$snap" != "[]" ] && [ -e "$(base_file "$label")" ]; then
            prev="$snap"
            printf '%s\n' "$snap" > "$file.tmp" && mv "$file.tmp" "$file"
            rm -f "$(base_file "$label")" "$gone" "$dir/down-$label" "$dir/down-$label.said"
            continue
        fi
        # Quiet saves the snapshot before its lines are held: a stop in between would lose
        # them, so a stop waits for the end of this snapshot's step (step_end).
        [ "$quiet" != 1 ] || { stop=""; trap 'stop=1' TERM INT HUP; trap 'stop=${stop:-ends}' USR1; trap 'stop=off' USR2; }
        # Turned off: nothing more is said, and the stop goes the way the time limit's does.
        if watch_off; then kill -USR2 $$ 2>/dev/null || true; fi
        if [ -n "$label" ] && [ -e "$dir/down-$label" ]; then
            if [ "$quiet" = 1 ]; then
                settle_locked link_up "$label"
            else
                echo "${at}link-up"
                rm -f "$dir/down-$label"
            fi
        fi
        if [ "$snap" = "[]" ]; then
            # The daemon went away. Say it once; whatever comes back is a new start,
            # not a wave of closes and opens. Quiet: only when agents went with it (a remote
            # stops its daemon once no tab is left), and the mark outlives a restart.
            if [ -e "$gone" ]; then
                :
            elif [ "$quiet" != 1 ]; then
                echo "${at}daemon-gone"
            else
                # Marked first, so what was held for this machine waits for its return.
                touch "$gone"
                busy="$has_agents"
                if routine_quiet; then busy="$has_busy"; fi
                if jq -en --argjson a "$prev" --argjson self "$self" --arg at "$at" \
                    "$busy" >/dev/null 2>&1; then
                    settle_locked flush "${at}daemon-gone"
                else
                    note "daemon gone, no agent" "${at}daemon-gone"
                fi
            fi
            touch "$gone"
            step_end
            continue
        fi
        if [ ! -e "$gone" ]; then
            sleeping='[]'
            if jq -en --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --arg at "$at" "$vanished" >/dev/null 2>&1; then
                # A tab that slept leaves the agent rows too; `qoop ls` still names it.
                sleeping="$(sleeping_on "$label")"
            fi
            out="$(jq -rn --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --arg at "$at" --argjson sleeping "$sleeping" "$diff" 2>/dev/null)" || { step_end; continue; }
            # Off: the snapshot is kept, and nothing of it is said or held.
            ! watch_off || out=""
            # Quiet checks held lines against the saved snapshot: save this one first, so a
            # line held from it is checked against the rows it came from.
            [ "$quiet" != 1 ] || { printf '%s\n' "$snap" > "$file.tmp" && mv "$file.tmp" "$file"; }
            now_lines=""
            while IFS= read -r l; do
                [ -n "$l" ] || continue
                if [ "$quiet" = 1 ]; then
                    l="$(limit_line "$label" "$l")"
                    [ -n "$l" ] || continue
                    if urgent "$l"; then now_lines="$now_lines$l
"; elif routine_quiet && routine "$l"; then quiet_routine "$label" "$l"
                    else quiet_hold "$label" "$l"; fi
                else
                    hold "$label" "$l" || now_lines="$now_lines$l
"
                fi
            done <<< "$out"
            if [ -n "$now_lines" ] && ! watch_off; then
                if [ "$quiet" = 1 ]; then
                    settle_locked flush "${now_lines%?}"
                else
                    say "${now_lines%?}"
                fi
            fi
        fi
        rm -f "$gone"
        prev="$snap"
        printf '%s\n' "$snap" > "$file.tmp" && mv "$file.tmp" "$file"
        step_end
    done
}

# The end of one snapshot's quiet step: a stop asked for during it goes ahead now.
step_end() {
    [ "$quiet" = 1 ] || return 0
    trap 'exit 143' TERM INT HUP
    trap 'end_with watch-ends' USR1
    trap 'end_with "$off_line"' USR2
    case "${stop:-}" in
        '') ;;
        ends) end_with watch-ends ;;
        off) end_with "$off_line" ;;
        *) exit 143 ;;
    esac
}

# A remote's qoop version, as its installed binary says it; empty when the link is down.
remote_version() { "$here/on.sh" "$1" --version 2>/dev/null | head -n 1 || true; }

# One remote's stream until it ends, or until that remote's qoop changes version: a remote that
# updates keeps a running `agents --watch` on its old binary, which may not say what the new one
# does (a stage-only change, 0.9.5 to 0.10.0), so the stream ends and is opened again on the new.
stream_remote() {
    # `s` is not local: bash 3.2 runs a trap after the function has unwound.
    local name="$1" was="" now n=0
    "$here/on.sh" --master "$name" agents --watch --json 2>/dev/null &
    s=$!
    trap 'kill "$s" 2>/dev/null; exit 143' TERM
    while kill -0 "$s" 2>/dev/null; do
        # In the background, so a TERM lands at once rather than after the sleep.
        sleep 1 &
        wait $! || true
        n=$((n + 1))
        [ "$vcheck" -gt 0 ] && [ $((n % vcheck)) -eq 0 ] || continue
        now="$(remote_version "$name")"
        # The first answer is the version the stream started on; none yet (a blip) asks again.
        [ -n "$was" ] || { was="$now"; continue; }
        if [ -n "$now" ] && [ "$now" != "$was" ]; then
            touch "$dir/reopen-$name"
            break
        fi
    done
    kill "$s" 2>/dev/null || true
}

# A remote's stream, kept up: when the link drops it says so once and tries again, every
# 5 s at first and at most every 5 minutes. A drop is not the remote's daemon going away:
# when the link is back its first snapshot is compared with the last one, so what changed
# meanwhile is still said.
follow_remote() {
    local name="$1" wait=5 down="$dir/down-$1" since
    rm -f "$dir/reopen-$name"
    # Marks of an earlier run say nothing of this one; quiet keeps them, so a restart says
    # neither the gone daemon nor the dropped link again.
    [ "$quiet" = 1 ] || rm -f "$down" "$(gone_file "$name")"
    while kill -0 $$ 2>/dev/null; do
        since="$(date +%s)"
        # The stream's ssh goes with the reader that ends, and that reader with this loop.
        follow "$name" < <(stream_remote "$name")
        # Ended for a new qoop there, not a drop: open it again now, with nothing to say.
        if [ -e "$dir/reopen-$name" ]; then
            rm -f "$dir/reopen-$name"
            continue
        fi
        # Only a link that held a minute starts the wait over: one that drops at once
        # backs off, so a flapping link cannot wake qass again and again.
        [ $(( $(date +%s) - since )) -lt 60 ] || wait=5
        # A remote with no daemon ends its stream too; that was said as daemon-gone.
        if [ ! -e "$down" ] && [ ! -e "$(gone_file "$name")" ]; then
            # Quiet: marked with the time, said by the tick once it lasts (link_down).
            if [ "$quiet" = 1 ]; then date +%s > "$down"; else echo "$name/link-down"; touch "$down"; fi
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

if watch_off; then
    printf '%s\n' "$off_line"
    exit 0
fi
# One watcher at a time: off and on again inside one tick would otherwise leave the old one
# running beside the new, both saying the same lines.
pidf="$dir/watch.pid"
take_pid() { ( set -C; echo $$ > "$pidf" ) 2>/dev/null; }
if ! take_pid; then
    old="$(cat "$pidf" 2>/dev/null || true)"
    if [ -n "$old" ] && kill -0 "$old" 2>/dev/null && ps -o command= -p "$old" 2>/dev/null | grep -q 'watch\.sh'; then
        echo "watch-running: one already runs"
        exit 0
    fi
    rm -f "$pidf"
    take_pid || { echo "watch-running: one already runs"; exit 0; }
    # Two that found the same stale file: the one whose pid is not there a moment later leaves.
    sleep 0.2
    [ "$(cat "$pidf" 2>/dev/null)" = "$$" ] || { echo "watch-running: one already runs"; exit 0; }
fi
drop_pid() { [ "$(cat "$pidf" 2>/dev/null)" != "$$" ] || rm -f "$pidf"; }
trap drop_pid EXIT
# Its own time is up: said, so qass starts it again (Start, step 3). Or the owner turned it off.
# Set before the tick and the remotes start, so a stop asked for while they do is never lost.
trap 'end_with watch-ends' USR1
trap 'end_with "$off_line"' USR2
# On again since the last watcher: qass sums the fleet up, so this starts from now.
resume=0
stamp="$(watch_unseen || true)"
if [ -n "$stamp" ]; then
    resume=1
    rm -f "$dir"/held "$dir"/held-* "$dir"/settle "$dir"/settle-*
    touch "$(base_file "")"
    seen_stamp "$stamp"
fi

rmdir "$dir/settle.lock" 2>/dev/null || true
if [ "$quiet" = 1 ]; then
    # Held lines and marks carry over; the logs are cut, the said lines of a week ago go.
    if [ -f "$dir/quiet.log" ]; then
        tail -n 2000 "$dir/quiet.log" > "$dir/quiet.log.tmp" && mv "$dir/quiet.log.tmp" "$dir/quiet.log"
    fi
    # ponytail: cut only past 1 MB, months of writes, so an append racing the cut is rare.
    if [ -f "$dir/prompted.log" ] && [ "$(wc -c < "$dir/prompted.log")" -gt 1048576 ]; then
        tail -n 2000 "$dir/prompted.log" > "$dir/prompted.log.tmp" && mv "$dir/prompted.log.tmp" "$dir/prompted.log"
    fi
    week=$(( $(date +%s) - 604800 ))
    for f in "$dir"/said "$dir"/said-*; do
        [ -f "$f" ] || continue
        case "$f" in *.tmp) continue ;; esac
        awk -v w="$week" '$5 >= w' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    done
else
    rm -f "$dir"/settle "$dir"/settle-* 2>/dev/null
fi

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
        if watch_off; then
            kill -USR2 $$ 2>/dev/null
            exit 0
        fi
        # Off and on again inside one tick: this one never stopped, so nothing was missed;
        # the stamp is marked seen, and a second watcher qass starts says `watch-running`.
        stamp="$(watch_unseen || true)"
        [ -z "$stamp" ] || seen_stamp "$stamp"
        if [ $(( $(date +%s) - started )) -ge "$watch_for" ]; then
            kill -USR1 $$ 2>/dev/null
            exit 0
        fi
        asks_said
        if [ "$quiet" = 1 ]; then
            settle_locked flush ""
            settle_locked ride_expire
            for f in "$dir"/down-*; do
                [ -e "$f" ] || continue
                case "$f" in *.said) continue ;; esac
                [ ! -e "$f.said" ] || continue
                [ $(( $(date +%s) - $(down_since "$f") )) -ge "$linkdown" ] || continue
                settle_locked link_down "${f#"$dir"/down-}"
            done
        fi
        for f in "$dir"/settle "$dir"/settle-*; do
            [ "$quiet" != 1 ] && [ -f "$f" ] || continue
            label="${f#"$dir"/settle}"; label="${label#-}"
            [ ! -e "$(gone_file "$label")" ] || continue
            settle_locked take_due "$f" | while read -r t g from state _; do
                snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
                out="$(jq -rn --argjson b "$snap" --arg t "$t" --argjson g "$g" --arg from "$from" \
                    --arg state "$state" --argjson self "$self" --arg at "$(at_of "$label")" \
                    "$settled" 2>/dev/null)" || continue
                if [ -n "$out" ]; then
                    say "$out"
                fi
            done
        done
        due | while read -r label t; do
            [ "$label" = - ] && label=""
            # With that machine's daemon away its last snapshot is stale: wait.
            [ ! -e "$(gone_file "$label")" ] || continue
            snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
            out="$(jq -rn --argjson b "$snap" --argjson due "[\"$t\"]" --argjson self "$self" \
                --arg at "$(at_of "$label")" "$again" 2>/dev/null)" || continue
            if [ -n "$out" ]; then
                if [ "$quiet" = 1 ]; then settle_locked flush "$out"; else say "$out"; fi
            fi
        done
    done
) &
kids=$!

# The remotes, read once and after the tick is up (a slow answer never holds it): the base
# marks of a resume and the streams name the same machines.
remotes="$(qoop remote ls 2>/dev/null | awk '$1 ~ /^[A-Za-z0-9_-]+$/ { print $1 }')"
for name in $remotes; do
    [ "$resume" = 0 ] || touch "$(base_file "$name")"
done
for name in $remotes; do
    follow_remote "$name" &
    kids="$kids $!"
done
# On the way out, each stream's reader (qoop, ssh) goes with the loop that reads it. IFS
# is set here because a signal can arrive inside `IFS= read`, where it is empty.
trap 'IFS=$'"'"' \t\n'"'"'; for k in $kids $$; do pkill -P "$k" 2>/dev/null || true; done
      kill $kids 2>/dev/null || true; drop_pid' EXIT
# Killed rather than ended (Monitor stopping it), go through the same way out.
trap 'exit 143' TERM INT HUP

[ "$quiet" = 1 ] || rm -f "$(gone_file "")"   # a mark left by an earlier run says nothing about this one
follow "" < <(qoop agents --watch --json)

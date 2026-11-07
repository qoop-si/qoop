#!/bin/bash
# qass watch: one line per agent change that may need the assistant.
#
# Reads `qoop agents --watch --json` (the whole row array on every change) of this Mac and
# of every remote in `qoop remote ls`, and prints a line only when an agent reaches a
# state that waits on somebody (idle, blocked, failed, done, sleeping), or sleeps, or
# closes. A row also changes on every tool call (activity, cwd), so only terminal,
# generation and state are compared. Its own terminal is skipped, and so is every tab the owner
# asked qass to leave alone (`qass_skip` on its row): nothing is said of it, whatever it does.
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
#        claims 12 40: ...            (done claims a day old nobody checked: see claims_said)
# A row with an open task (`qoop task`) ends with its id and stage, and `:claimed` once the
# agent said done: ... /home/me/src/app task:12:verifying:claimed
# Before the task: `stop:<id>`, the daemon's newest admitted stop of that agent (`?`: none since
# its last work), and `allowed:<verdict>@<unix>`, what `qoop qass brief` allowed when the line
# was made (`allowed:?`: no brief, see `facts`).
# After the stop, `bg:<n>` when the row counts n things still running in the background.
#
# Quiet (the default; QASS_QUIET=0 gives everything above and nothing below). Every line wakes
# qass, and a wake re-reads its whole conversation, so only what needs it now is said now:
# - At once, with whatever is held: ->blocked, ->failed, ->closed, a task stage that just
#   became waiting-human, a first claim of done (:claimed), hands-off->idle, daemon-gone with agents on it, a link down for
#   QASS_LINKDOWN seconds (60). Everything else is held, one entry per agent, and printed in
#   one block, so Monitor brings several in one wake.
# - Short hold (QASS_SETTLE): a line about work qass started (the newest record in
#   prompted.log for that terminal comes from qass's own terminal, written by `qoop prompt`,
#   `qoop send` and on.sh), new, restarted and starting lines, remote agents, and work in
#   progress that came to rest: an agent on an open task, not claimed, not waiting on the owner,
#   that no other agent drives (`work_rest`, `driven`: another running terminal, not qass, wrote
#   to it in the last hour; such an agent stays routine or long, as before). Work that stops is qass's to
#   move on in seconds, not after he did it himself. Only its first stop there, though: a stop
#   with the generation, state and stage of the agent's line said last, printed under an hour
#   ago, and no write from qass since, is the same rest again (it woke for its own background
#   notice or a teammate's message and stopped where it was): held `again`, below, a remote
#   agent's too.
# - Again: the same rest again is held QASS_QUIET_HOLD seconds (600), doubled for each time that
#   rest was said in a row with no write from qass since (`again_hold`, the said line's run: 10, 20,
#   40, then 60 minutes), and said only if the agent is still there then, never riding along with
#   another block; a snapshot with that agent working drops it (`again: dropped at work` in
#   quiet.log), and a new stop of it starts the hold over. A write from qass starts the run over.
#   An agent that keeps waking for its own notices and stopping where it was is said at most about
#   once an hour, and at least once an hour, when a stop finds its last line over an hour old.
# - Long hold (QASS_QUIET_HOLD, 600 s from its first line): everything else, an agent woken
#   by its own background work, or one another agent drives. It rides along with any earlier
#   block.
# - Ride: a repeat of an agent already said waiting on the owner, and a sleep after an idle
#   already said, only ever go with another block.
# - Background hold: an agent at rest (idle or done) whose row counts work still running in the
#   background (`background`: shells, monitors, subagents), on no claim and not waiting on the
#   owner, is said nothing of while that runs: a wait is not news. That covers a driven agent and,
#   with routine on, a rest with no task; with routine silent such a rest stays routine. The tick
#   lets it go, as its row is then (`bg:<n>` with some still running), into the short hold (or
#   `again` for work in progress that is the rest said last again) when
#   fewer run than when the hold began (none included) and it still rests, or once the hold has
#   lasted QASS_BG_LIMIT seconds (300), whichever comes first. A stop in between keeps the first
#   count and start (`bghold` in qass's folder); a stop with fewer running ends the hold and is
#   held as any line. Past its limit the hold is spent: a job that never ends (a dev server)
#   holds none of the agent's later stops, until fewer run. Said, never stopped.
#   The lines said at once above are never held so.
# - A remote whose daemon left with no agent on it, and lines that went stale, go to
#   quiet.log instead. Held lines and marks outlive a restart of this script.
# - Routine, silent unless the file `routine` in qass's folder holds `say` (the owner's word:
#   `routine off`, `routine on`): a plain rest with no open task (working or starting to idle or
#   done, no claim, not waiting-human) is held QASS_ROUTINE_HOLD seconds (1800) and said only if the agent is
#   still there, never riding along with another block; a sleep, and a daemon leaving
#   with no agent working or blocked on it, go to quiet.log only.
# - With cswap installed, a Claude agent of this Mac that stopped on its usage limit comes out
#   at once as `->limited ... resets:7:20am` (`again` when it was limited minutes before).
#
# The owner's switch, the file `watch` in qass's folder (`qoop qass watch on|off`):
# - `off`: this says nothing more and ends with the `watch-off: ...` line, at start or within a
#   tick of the switch; qass does not start it again, so nothing wakes qass until they write.
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
bglimit="${QASS_BG_LIMIT:-300}"
# A Monitor is killed at 30 minutes and its one notice of that can be lost (2026-10-12: it came
# in the second an event did, and nothing watched for an hour). So this ends first, by itself.
watch_for="${QASS_WATCH_FOR:-1740}"
started="$(date +%s)"

defs='
def rows: map(select(.agent == true and ($at != "" or .terminal != $self)
                     and (.terminal as $t | $skip | index($t)) == null))
          | map({key: (.terminal | tostring), value: .}) | from_entries;
def waits: . as $s | ["idle", "blocked", "failed", "done", "sleeping"] | index($s) != null;
def clean: tostring | gsub("[^\\p{L}\\p{N} ._/-]"; "") | .[0:40];
def place: tostring | gsub("[^\\p{L}\\p{N} ._/~-]"; "");
def task: if .task == null then "" else
  " task:\(.task.id | clean):\(.task.stage | clean)" + (if .task.claimed then ":claimed" else "" end) end;
def line($r; $from):
  "\($at)T\($r.terminal) g\($r.generation) \($r.program // "-" | clean) \($from)->\($r.state)"
  + " \"\(($r.name // $r.agent_title // "") | clean)\" \($r.cwd // "-" | place)"
  # Claude flagged a dangerous rm on this menu: a hint, the screen decides (SKILL.md, Menu row).
  # Before the task, so a line still ends `task:<id>:<stage>[:claimed]`.
  + (if $r.rule == "dangerous_rm_dialog" then " menu:dangerous" else "" end)
  + " stop:" + ($r.stop // "" | clean | if . == "" then "?" else . end)
  # Work still running in the background (shells, monitors, subagents), as the row is now.
  + (if ($r.background // 0) > 0 then " bg:\($r.background)" else "" end) + ($r | task);
'
# Every program below takes $skip, the terminals of one machine the owner asked qass to leave
# alone, so `rows` drops them: a skipped tab going idle, blocked, failed or away says nothing, and
# a flag set mid-work is not a close, nor a flag cleared a new tab. (skips_of builds it.)

# $a and $b are row arrays; prints the lines for what changed from $a to $b.
diff="$defs"'
($a | rows) as $old | ($b | rows) as $new
| ($new | to_entries[] | .value as $r | $old[.key] as $o
   | if $o == null then (if ($r.state | waits) then line($r; "new") else empty end)
     elif $o.generation != $r.generation then
       (if ($r.state | waits) then line($r; "restarted") else empty end)
     elif $o.state != $r.state and ($r.state | waits) then line($r; $o.state)
     # A new stop of an agent still at rest: it worked where no snapshot showed it, so the
     # line is the one a stop from working makes.
     elif ($r.state | waits) and $r.stop != null and $r.stop != $o.stop then line($r; "working")
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

# $b is a row array; the background hold of terminal $t, generation $g, begun with $n at work:
# with the agent at rest, its line once fewer run, or `late <line>` once the hold ran out
# ($late); `keep` while it rests or works, else `drop` (gone, restarted, blocked, or on a claim
# or the owner: those lines go their own way). A spent hold ($spent: its limit was said) says
# nothing more and goes once fewer run.
bgcheck="$defs"'
($b | rows)[$t] as $r
| if $r == null or $r.generation != $g or $r.task.claimed == true
     or ($r.task.stage? // "") == "waiting-human" then "drop"
  elif ($r.state == "idle" or $r.state == "done") | not then
    (if $r.state == "working" or $r.state == "starting" then "keep" else "drop" end)
  elif ($r.background // 0) < $n then (if $spent then "drop" else line($r; $from) end)
  elif $spent then "keep"
  elif $late then "late " + line($r; $from)
  else "keep" end'

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
# One line in watch.log (qass's folder, beside the switch's own turns that the daemon writes there)
# a start, a line it ends on, and an exit (its code): a start with no exit before the next start
# is a watcher killed, and a day with no start is a day nobody watched.
wlog() { printf '%s %s\n' "$(date +%s)" "$1" >> "$dir/watch.log" 2>/dev/null || true; }
end_with() { trap '' USR1 USR2; wlog "said ${1%%:*}"; printf '%s\n' "$1"; exit 0; }
seen_stamp() { printf '%s\n' "$1" > "$dir/watch-seen.$$" && mv "$dir/watch-seen.$$" "$dir/watch-seen"; }
off_line='watch-off: the owner turned watching off (qoop qass watch); do not start this again until it reads on'

# Reads held entries ("<t> <gen> <class> <due> <arrival> <line>") against the row array $b and
# prints each as "P <line to say>" or "D <line>" (went stale). A sleep or a close holds while
# the agent is still away; any other line is said as the row is now, while it still waits. A
# line of a tab skipped since goes stale first, whatever it was.
recheck="$defs"'
($b | rows) as $new
| capture("^(?<t>[0-9]+) (?<g>[^ ]+) [^ ]+ [0-9]+ (?<arr>[0-9]+) (?<line>.*)$") as $h
| ($h.line | capture("^(?:[A-Za-z0-9_-]+/)?T[0-9]+ g[^ ]+ [^ ]+ (?<from>[a-z-]+)->(?<state>[a-z]+)")) as $m
| $new[$h.t] as $r
| "P \($h.arr) " as $say
| if ($skip | index($h.t | tonumber)) != null then "D " + $h.line
  elif $m.state == "sleeping" or $m.state == "closed" then
    (if $r == null then $say + $h.line else "D " + $h.line end)
  elif $r == null or (($r.state | waits) | not) then "D " + $h.line
  elif ($r.generation | tostring) == $h.g then $say + line($r; $m.from)
  else $say + line($r; "restarted") end'

# The terminals of one machine that carry `qass_skip` in any of the row arrays given, as a JSON
# array. Read from all the rows, before the agent filter: a tab whose agent exited keeps its flag.
# The previous snapshot counts too, so a flag set or cleared between two snapshots never reads as
# a close or a new tab.
skips_of() {
    printf '%s\n' "$@" | jq -sc '[.[][]? | select(.qass_skip == true) | .terminal] | unique' 2>/dev/null ||
        echo '[]'
}

# A machine's lines held until they settle: "<terminal> <generation> <from> <state> <due>".
settle_file() { if [ -z "$1" ]; then echo "$dir/settle"; else echo "$dir/settle-$1"; fi; }

# One writer at a time for the settle files: the streams add, the tick takes. A flush holds it
# for up to about 2.5 s (its briefs) and the lock keeps no order, so 15 s; past that it runs
# anyway, and says so in watch.log (a lock a killed watcher left is cleared at start).
settle_locked() {
    local i rc
    for i in $(seq 150); do
        if mkdir "$dir/settle.lock" 2>/dev/null; then
            "$@"; rc=$?
            rmdir "$dir/settle.lock"
            return $rc
        fi
        sleep 0.1
    done
    wlog "lock: ran unlocked after 15 s"
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

# Agent lines with the brief's verdict (`facts`), for `say` with QASS_QUIET=0: nothing is held
# there, so a tab the owner is at goes out as `allowed:nothing:until-<unix>`.
tagged() {
    local l
    while IFS= read -r l; do printf '%s\n' "${l#* }"; done <<< "$(facts "$1")"
}

# The brief's verdict for each line of $1 about to go out, as `allowed:<verdict>@<unix>` before
# its task (A1), and until when the owner is at that tab (A2): "<unix|-> <line>" per line. One
# `qoop qass brief` per agent line, all at once, each cut at 2 s, so a block of any size waits
# about 2 s; at most 12, in the order given (the urgent first). A remote, closed or asleep line,
# one past the 12th, and a brief that failed, timed out or gave no `allowed` read `allowed:?`.
verdict='
select(type == "object" and has("allowed"))
| (.person_known == true and (.person_until // 0) > $now) as $here
| ([(if $here then .person_until else empty end), (.hands_off_until // empty)]
   | map(select(. > $now)) | max) as $until
| ((if .allowed != null then "write:\(.ending // "none")"
    else "nothing" + (if $until == null then "" else ":until-\($until)" end) end)
   + (if any(.not_now[]?; startswith("another review round")) then ":no-round" else "" end)
   + (if .last_typed_by == "agent" then ":driven" else "" end)) as $v
| "\(if $here then .person_until else "-" end) allowed:\($v)@\(.at // $now)"'
# Always run in a subshell, `$(facts ...)`.
facts() {
    local l i=0 n=0 pids="" p tag tmp now
    if ! tmp="$(mktemp -d "$dir/.facts.XXXXXX")"; then
        while IFS= read -r l; do
            if parse_line "$l"; then printf -- '- %s\n' "$(tag_line "$l" "allowed:?")"; else printf -- '- %s\n' "$l"; fi
        done <<< "$1"
        return 0
    fi
    while IFS= read -r l; do
        i=$((i + 1))
        [ -n "$l" ] && parse_line "$l" && [ -z "$L_LABEL" ] && [ "$n" -lt 12 ] || continue
        case "$L_STATE" in closed | sleeping) continue ;; esac
        n=$((n + 1))
        perl -e 'alarm 2; exec @ARGV' qoop qass brief "$L_T" --json < /dev/null > "$tmp/$i.out" 2>/dev/null &
        pids="$pids $i:$!"
    done <<< "$1"
    # By name: a bare `wait` would wait on the tick and the remote streams too. Only a brief that
    # ended well counts: one that failed or ran past its alarm reads `allowed:?`, whatever it
    # wrote. Nothing is left to kill: the alarm ends each, and this subshell neither keeps the
    # watcher's traps nor gets its signals, so no wait is cut short.
    for p in $pids; do
        if wait "${p#*:}" 2>/dev/null; then mv "$tmp/${p%%:*}.out" "$tmp/${p%%:*}" 2>/dev/null || true; fi
    done
    now="$(date +%s)" i=0
    while IFS= read -r l; do
        i=$((i + 1))
        parse_line "$l" || { printf -- '- %s\n' "$l"; continue; }
        read -r p tag <<< "$(jq -r --argjson now "$now" "$verdict" "$tmp/$i" 2>/dev/null || true)"
        [ -n "${tag:-}" ] || { p=-; tag="allowed:?"; }
        printf '%s %s\n' "$p" "$(tag_line "$l" "$tag")"
    done <<< "$1"
    rm -rf "$tmp"
}

# Puts $2 before the line's task, or before its ` resets:` with no task, or at its end. Names and
# paths are cleaned of `:`, so ` task:` and ` resets:` are found where `line` and `limit_line` put them.
tag_line() { # line tag
    case "$1" in
        *" task:"*) printf '%s %s task:%s\n' "${1%% task:*}" "$2" "${1#* task:}" ;;
        *" resets:"*) printf '%s %s resets:%s\n' "${1%% resets:*}" "$2" "${1#* resets:}" ;;
        *) printf '%s %s\n' "$1" "$2" ;;
    esac
}

# A machine's own files: "" is this Mac, any other label a remote.
snapshot_file() { if [ -z "$1" ]; then echo "$dir/last.json"; else echo "$dir/last-$1.json"; fi; }
# Quiet: its held lines, "<t> <gen> <short|long|ride|routine|person> <due> <arrival> <line>", and
# its said lines, "<t> <gen> <state> <stage> <arrival> <stop> <printed> <run>". Not the settle files: an older watcher still
# running from an older install reads and clears those.
held_file() { if [ -z "$1" ]; then echo "$dir/held"; else echo "$dir/held-$1"; fi; }
said_file() { if [ -z "$1" ]; then echo "$dir/said"; else echo "$dir/said-$1"; fi; }

# One line per answer the owner gave from the inbox (phone, desk or terminal) since the last look,
# after it went out: `ASK E75 answered a -> T224` (an option, or `text`), `ASK E3 passed a -> qass`.
# qass never has to act on it: the card is `answered` or `passed` in `qoop ask ls`, and the
# worker was already told. The first look only records what is there. A card answered again
# after it came back open is a new line (its closed time is part of what is remembered).
asks_said() {
    local f="$dir/asks-seen" cur new skipped
    cur="$(qoop ask ls --json 2>/dev/null | jq -r '(.live + .ended)[]
        | select(.status == "answered" or .status == "passed")
        | "\(.closed_at // 0) ASK \(.card) \(.status) \(.option // "text") -> \(if .status == "passed" then "qass" else (.agent // "-") end)"' 2>/dev/null)" || return 0
    if [ -f "$f" ]; then
        new="$(printf '%s\n' "$cur" | grep -vxFf "$f" || true)"
        # An ask of a tab qass leaves alone is the owner's alone: remembered, never said.
        skipped="$(skips_of "$(cat "$dir/last.json" 2>/dev/null || echo '[]')" | tr -d '[]' | tr ',' ' ')"
        new="$(printf '%s\n' "$new" | awk -v skip="$skipped" '
            BEGIN { n = split(skip, a, " "); for (i = 1; i <= n; i++) s["T" a[i]] = 1 }
            !($NF in s)')"
        [ -z "$new" ] || say "$(printf '%s\n' "$new" | sed -E 's/^[0-9]+ //')"
    fi
    printf '%s\n' "$cur" > "$f"
}

# The daemon's claim scan (every 4 h while watching is on) leaves one `claims <ids>: ...` line in
# `claims`: said once. Taken by a rename first, so two looks never say it twice.
claims_said() {
    local l
    mv "$dir/claims" "$dir/claims.said" 2>/dev/null || return 0
    l="$(cat "$dir/claims.said" 2>/dev/null || true)"
    [ -z "$l" ] || say "$l"
}

# Why a line was not said, for whoever wonders what qass did not hear.
note() { printf '%s %s %s\n' "$(date +%s)" "$1" "$2" >> "$dir/quiet.log" 2>/dev/null || true; }

# A line's parts, in L_LABEL L_T L_G L_FROM L_STATE L_STAGE ("-" with no task) L_STOP ("?" with
# none). Fails for a line that is not about an agent (daemon-gone, link-down).
parse_line() {
    local re='^(([A-Za-z0-9_-]+)/)?T([0-9]+) g([^ ]+) [^ ]+ ([a-z-]+)->([a-z]+)'
    local task=' task:[^: ]+:([a-z-]+)(:claimed)?( |$)'
    local stop=' stop:([^ ]+)'
    L_LABEL="" L_T="" L_G="" L_FROM="" L_STATE="" L_STAGE="-" L_CLAIMED="" L_STOP="?"
    [[ "$1" =~ $re ]] || return 1
    L_LABEL="${BASH_REMATCH[2]}" L_T="${BASH_REMATCH[3]}" L_G="${BASH_REMATCH[4]}"
    L_FROM="${BASH_REMATCH[5]}" L_STATE="${BASH_REMATCH[6]}"
    if [[ "$1" =~ $task ]]; then L_STAGE="${BASH_REMATCH[1]}" L_CLAIMED="${BASH_REMATCH[2]}"; fi
    if [[ "$1" =~ $stop ]]; then L_STOP="${BASH_REMATCH[1]}"; fi
    return 0
}

# The said line of terminal $2 on machine $1, as "<t> <gen> <state> <stage> <arrival> <stop>
# <printed> <run>" (an older watcher's has the first five or seven).
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

# The newest prompted.log record for terminal $1 of machine $2 (none: this Mac), as "<unix>
# <sender>": who wrote to it last through `qoop prompt`, `qoop send` or on.sh. "unreadable" when
# the log cannot be read.
record_of() { # terminal [label]
    local log="$dir/prompted.log"
    [ -e "$log" ] || return 0
    [ -r "$log" ] || { echo unreadable; return 0; }
    tail -n 2000 "$log" | awk -v t="${2:+$2/}T$1" '$3 == t { last = $1 " " $2 } END { if (last != "") print last }'
}

# The hold for the agent line $2 just parsed, on machine $1: short, long, again (a rest said
# before) or ride. A long one says why in quiet.log, so a write qass made that left no record
# shows up there.
hold_class() {
    local s_t s_gen s_state s_stage s_arr _ r_at r_from
    case "$L_FROM" in new | restarted | starting) echo short; return ;; esac
    # A remote agent: short, or `again` when it is work in progress at the rest said last again.
    if [ -n "$1" ]; then
        if work_rest; then rest_class "$2"; else echo short; fi
        return
    fi
    read -r s_t s_gen s_state s_stage s_arr _ <<< "$(said_of "" "$L_T")"
    if [ "$L_STATE" = sleeping ] && [ "${s_gen:-}" = "$L_G" ] &&
        { [ "${s_state:-}" = idle ] || [ "${s_state:-}" = done ]; } &&
        ! grep -q "^$L_T " "$(held_file "")" 2>/dev/null; then
        echo ride; return
    fi
    # The only ride a waiting-human repeat gets; a new or unknown stop goes on to the classes below.
    if handled_repeat "$1" "$2"; then echo ride; return; fi
    read -r r_at r_from <<< "$(record_of "$L_T")"
    if [ "${r_at:-}" = unreadable ]; then echo short; return; fi
    if [ -n "${r_at:-}" ] && [ "${r_from:-}" = "$self" ] && [ "$r_at" -ge "${s_arr:-0}" ]; then
        echo short; return
    fi
    # Work in progress that came to rest, not driven by another agent (`work_rest`): short, or
    # `again` when it is the rest said last again (`same_rest`).
    if work_rest; then
        rest_class "$2"; return
    fi
    echo long
}

# Whether the line just parsed, of any machine, is the rest said last again: the generation, state
# and stage of its agent's line said last, printed under an hour ago, and no write from qass since
# that line came. The agent woke without qass, for its own background notice or a teammate, and
# stopped where it was. Past the hour its stop is short again, so a worker that loops so is said
# at least once an hour.
same_rest() {
    local s_t s_gen s_state s_stage s_arr s_stop s_printed _ r_at r_from
    read -r s_t s_gen s_state s_stage s_arr s_stop s_printed _ <<< "$(said_of "$L_LABEL" "$L_T")"
    [ "${s_gen:-}" = "$L_G" ] && [ "${s_state:-}" = "$L_STATE" ] && [ "${s_stage:-}" = "$L_STAGE" ] ||
        return 1
    # An older watcher's record has no print time: its arrival stands in.
    [ $(($(date +%s) - ${s_printed:-$s_arr})) -lt 3600 ] || return 1
    read -r r_at r_from <<< "$(record_of "$L_T" "$L_LABEL")"
    [ "${r_at:-}" != unreadable ] || return 1
    [ -z "${r_at:-}" ] || [ "${r_from:-}" != "$self" ] || [ "$r_at" -lt "$s_arr" ]
}

# The class of work in progress that came to rest (`work_rest`), the line $1 just parsed: `again` for
# the rest said last again (`same_rest`, noted), else short.
rest_class() { # line
    if same_rest; then
        note "again: held" "$1"
        echo again
    else
        echo short
    fi
}

# Holds a line, one entry per agent: a newer line takes the text, the earlier due time and the
# first arrival stand, a short or long hold outranks a ride. A `person` hold (the owner at that
# tab, A2) comes with its due ($6) and sets it outright; while it is ahead, no other hold moves
# it. Runs under the settle lock.
hold_put() { # label terminal generation class line [due]
    local f now due class o_t o_g o_class o_due o_arr o_line _ arr
    f="$(held_file "$1")"
    now="$(date +%s)"
    class="$4" arr="$now"
    case "$class" in
        short) due=$((now + settle)) ;;
        routine) due=$((now + rhold)) ;;
        person) due="$6" ;;
        again) due=$((now + $(again_hold "$1" "$2"))) ;;
        *) due=$((now + qhold)) ;;
    esac
    read -r o_t o_g o_class o_due o_arr o_line <<< "$(grep "^$2 " "$f" 2>/dev/null | tail -1 || true)"
    if [ -n "${o_t:-}" ] && [ "$class" = person ]; then
        arr="$o_arr"
    elif [ -n "${o_t:-}" ] && [ "$o_class" = person ] && [ "$o_due" -gt "$now" ]; then
        class=person due="$o_due" arr="$o_arr"
    elif [ -n "${o_t:-}" ]; then
        # An `again` line over a ride (a sleep, a repeat) keeps its own backoff, not the ride's due.
        [ "$class.$o_class" = again.ride ] || [ "$o_due" -ge "$due" ] || due="$o_due"
        arr="$o_arr"
        # A routine line is the agent's newest word: it never inherits an earlier class.
        [ "$class" != routine ] || { due=$((now + rhold)); arr="$now"; }
        # A short line again: the agent stopped anew, so it settles anew from now. An agent that
        # stops more often than the settle is said once it stays stopped, not before.
        [ "$class.$o_class" != short.short ] || due=$((now + settle))
        # The same rest again on a new stop: the agent worked in between, seen or not, so its
        # hold starts over from now.
        [ "$class.$o_class" != again.again ] || [ "$(stop_of "$5")" = "$(stop_of "${o_line:-}")" ] ||
            due=$((now + $(again_hold "$1" "$2")))
        case "$class.$o_class" in
            routine.*) ;;
            short.* | *.short) class=short ;;
            long.* | *.long) class=long ;;
            again.* | *.again) class=again ;;
        esac
    fi
    { grep -v "^$2 " "$f" 2>/dev/null || true
      echo "$2 $3 $class $due $arr $5"; } > "$f.tmp"
    mv "$f.tmp" "$f"
}

# The stop of an agent line, its `stop:` tag ("" with none).
stop_of() {
    local s
    case "$1" in *" stop:"*) s="${1##* stop:}"; echo "${s%% *}" ;; esac
}

# Whether the agent line $2 just parsed, of this Mac, is the same stop again, once qass had its
# line and finished a turn after it, and nobody wrote to the agent since (A3): it rides. Noted
# then, with the print and rest times the replay reads.
handled_repeat() { # label line
    local s_t s_gen s_state s_stage s_arr s_stop s_printed h rested _
    [ -z "$1" ] && [ "$L_STOP" != "?" ] || return 1
    case "$L_STATE" in idle | done) ;; *) return 1 ;; esac
    read -r s_t s_gen s_state s_stage s_arr s_stop s_printed _ <<< "$(said_of "" "$L_T")"
    [ "${s_gen:-}" = "$L_G" ] && [ "${s_stop:-}" = "$L_STOP" ] && [ -n "${s_printed:-}" ] || return 1
    read -r h rested _ <<< "$(handled_of "$L_T" "$s_printed")"
    [ "$h" = yes ] || return 1
    note "ride: stop handled T$L_T g$L_G $L_STOP printed $s_printed rested $rested" "$2"
}

# Notes each said line as its agent's last, with when it first came (a record from qass newer
# than that makes the agent's next line short), its stop and when it went out (A3's handled
# rule). A closed agent is forgotten.
said_put() { # line arrival printed
    local f s_t s_gen s_state s_stage s_arr s_stop s_printed s_run r_at r_from run=0 _
    parse_line "$1" || return 0
    f="$(said_file "$L_LABEL")"
    # The run: how many times in a row this rest was said before, with no write from qass since.
    read -r s_t s_gen s_state s_stage s_arr s_stop s_printed s_run _ <<< "$(said_of "$L_LABEL" "$L_T")"
    read -r r_at r_from <<< "$(record_of "$L_T" "$L_LABEL")"
    if [ "${s_gen:-}" = "$L_G" ] && [ "${s_state:-}" = "$L_STATE" ] && [ "${s_stage:-}" = "$L_STAGE$L_CLAIMED" ] &&
        [ "${r_at:-}" != unreadable ] && { [ -z "${r_at:-}" ] || [ "${r_from:-}" != "$self" ] || [ "$r_at" -lt "$s_arr" ]; }; then
        run=$((${s_run:-0} + 1))
        # The same stop said again (it rode along with a block): the agent did not stop anew.
        [ "$L_STOP" = "?" ] || [ "${s_stop:-}" != "$L_STOP" ] || run="${s_run:-0}"
    fi
    { grep -v "^$L_T " "$f" 2>/dev/null || true
      [ "$L_STATE" = closed ] || echo "$L_T $L_G $L_STATE $L_STAGE${L_CLAIMED} $2 $L_STOP $3 $run"; } > "$f.tmp"
    mv "$f.tmp" "$f"
}

# The `again` hold of terminal $2 on machine $1: QASS_QUIET_HOLD doubled for each time its rest was
# said in a row (the said line's run), at most an hour: 10, 20, 40, then 60 minutes.
again_hold() { # label terminal
    local s_t s_gen s_state s_stage s_arr s_stop s_printed s_run h _
    read -r s_t s_gen s_state s_stage s_arr s_stop s_printed s_run _ <<< "$(said_of "$1" "$2")"
    h=$((qhold << (${s_run:-0} > 3 ? 3 : ${s_run:-0})))
    echo $((h < (qhold > 3600 ? qhold : 3600) ? h : (qhold > 3600 ? qhold : 3600)))
}

# Whether the stop of terminal $1 printed at $2 was handled: qass's own row (this Mac's last
# snapshot) is at rest with its last activity after that print, and nothing was written to the
# agent since. "<yes|no> <rested|-> <wrote yes|no>". An equal second is not after.
handled_of() { # terminal printed
    local rested r_at r_from wrote=no h=no
    rested="$(jq -r --argjson self "$self" '[.[]? | select(.terminal == $self
        and (.state == "idle" or .state == "done")) | .last_active_at_unix // empty][0] // empty' \
        "$dir/last.json" 2>/dev/null || true)"
    read -r r_at r_from <<< "$(record_of "$1")"
    if [ "${r_at:-}" = unreadable ] || { [ -n "${r_at:-}" ] && [ "$r_at" -ge "$2" ]; }; then wrote=yes; fi
    if [ -n "$rested" ] && [ "$rested" -gt "$2" ] && [ "$wrote" = no ]; then h=yes; fi
    echo "$h ${rested:--} $wrote"
}

# One `print:` note per agent line a block puts out, before its said record is replaced: whether
# it repeats the stop said before, whether that earlier print was handled, when it was printed,
# when qass last rested and whether the agent was written to since. The replay reads these.
print_note() { # line
    local s_t s_gen s_state s_stage s_arr s_stop s_printed _ rep=no h=no was=- rested=- wrote=no
    parse_line "$1" || return 0
    read -r s_t s_gen s_state s_stage s_arr s_stop s_printed _ <<< "$(said_of "$L_LABEL" "$L_T")"
    if [ -n "$L_LABEL" ] || [ "$L_STOP" = "?" ]; then
        h="?"
    elif [ "${s_gen:-}" = "$L_G" ] && [ "${s_stop:-}" = "$L_STOP" ] && [ -n "${s_printed:-}" ]; then
        rep=yes was="$s_printed"
        read -r h rested wrote <<< "$(handled_of "$L_T" "$s_printed")"
    fi
    note "print: ${L_LABEL:+$L_LABEL/}T$L_T g$L_G $L_STOP repeat:$rep handled:$h printed $was rested $rested wrote $wrote" ""
}

# Says lines in one block, so Monitor brings them as one wake: $1 (lines said at once, maybe
# none) first, then every held line that goes with them, each checked against the latest
# snapshot. With no $1 it says nothing until a held line is due. Each agent line gets the brief's
# verdict (`facts`); a line of a tab the owner is at (A2) is held instead, as `person`, until his
# clock runs out, and is not said. The block goes only when a line that wakes survives that: one
# of $1, or a held line that was due and is no ride; else the rides and longs stay held. Runs
# under the settle lock; the held entries go only after the block is out, so a kill in between
# says twice, not never.
flush() {
    local now f label snap sel out l go="" wake="" block="" outs="" picked="" kept="" info printed
    local p t e wp i n=0 drop="" ts o_t o_g o_class o_due _
    local -a c_arr c_wp c_go c_file c_line f_p f_line o_line
    # Off: everything stays held, and is dropped when the owner turns it on again.
    ! watch_off || return 0
    now="$(date +%s)"
    [ -z "$1" ] || go=1
    while IFS= read -r l; do
        [ -n "$l" ] || continue
        wp=0
        # A line said at once is newer than whatever was held for its agent; a person hold still
        # ahead stays its floor, should its brief fail now. A line that is no agent's (daemon-gone,
        # link-down, link-up) goes as it is.
        if parse_line "$l"; then
            f="$(held_file "$L_LABEL")"
            read -r o_t o_g o_class o_due _ <<< "$(grep "^$L_T " "$f" 2>/dev/null | tail -1 || true)"
            if [ -n "${o_t:-}" ]; then
                if [ "$o_class" = person ]; then
                    wp=1
                    [ -n "$L_LABEL" ] || [ "$o_due" -le "$now" ] || kept="$kept $L_T:$o_due"
                fi
                { grep -v "^$L_T " "$f" || true; } > "$f.tmp"
                mv "$f.tmp" "$f"
            fi
        fi
        c_arr[n]="$now" c_wp[n]="$wp" c_go[n]=1 c_file[n]="" c_line[n]="$l"; n=$((n + 1))
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
        # Short and person lines first, then long and again, then ride: the most urgent first in
        # the block. A routine, person or again line goes only once due.
        sel="$(awk -v now="$now" -v b="$batch" '(($3 == "routine" || $3 == "person" || $3 == "again") ? $4 <= now : $3 != "short" || $4 <= now + b) {
            print ($3 == "short" || $3 == "person" ? 0 : $3 == "long" || $3 == "again" ? 1 : $3 == "routine" ? 2 : 3) " " NR " " $0 }' "$f" |
            sort -n -k1,1 -k2,2 | cut -d' ' -f3-)"
        [ -n "$sel" ] || continue
        # Taken only once checked: a failed check keeps them held for the next round.
        out="$(printf '%s\n' "$sel" | jq -rR --argjson b "$snap" --argjson self "$self" \
            --argjson skip "$(skips_of "$snap")" --arg at "$(at_of "$label")" "$recheck" 2>/dev/null)" || continue
        picked="$picked$f
"
        # Each selected entry's class and due, " <t>:<class>:<due>", for what it was held as.
        info=" $(printf '%s\n' "$sel" | awk '{ printf "%s:%s:%s ", $1, $3, $4 }')"
        while IFS= read -r l; do
            case "$l" in "P "* | "D "*) ;; *) continue ;; esac
            t="${l#? }"; [ "${l%% *}" = D ] || t="${t#* }"
            parse_line "$t" || continue
            e=""
            for e in $info; do [ "${e%%:*}" != "$L_T" ] || break; e=""; done
            e="${e#*:}"
            case "$l" in
                "P "*) l="${l#P }"
                    wp=0; [ "${e%%:*}" != person ] || wp=1
                    c_go[n]=0
                    [ "${e%%:*}" = ride ] || [ "${e#*:}" -gt "$now" ] || c_go[n]=1
                    c_arr[n]="${l%% *}" c_wp[n]="$wp" c_file[n]="$f" c_line[n]="${l#* }"; n=$((n + 1)) ;;
                "D "*) l="${l#D }"
                    if [ "${e%%:*}" = person ]; then note "person: stale" "$l"; else note "stale ${e%%:*}" "$l"; fi
                    drop="$drop$L_T $f
" ;;
            esac
        done <<< "$out"
    done
    # The verdicts, then the three sets: printed, held again for a person at the tab, stale.
    i=0
    if [ "$n" -gt 0 ]; then
        while IFS= read -r l; do
            f_p[i]="${l%% *}" f_line[i]="${l#* }"; i=$((i + 1))
        done <<< "$(facts "$(printf '%s\n' "${c_line[@]}")")"
    fi
    i=0
    while [ "$i" -lt "$n" ]; do
        l="${f_line[i]:-}" p="${f_p[i]:--}"
        # A verdict missing (facts cut short): the line as it came, `allowed:?` on an agent's.
        if [ -z "$l" ]; then
            l="${c_line[i]}"
            ! parse_line "$l" || l="$(tag_line "$l" "allowed:?")"
        fi
        if parse_line "$l" && [ -z "$L_LABEL" ]; then
            case "$l" in
                *" allowed:?"*) for t in $kept; do [ "${t%%:*}" != "$L_T" ] || p="${t#*:}"; done ;;
            esac
            case "$L_STATE" in idle | done | blocked | failed) ;; *) p=- ;; esac
            if [ "$p" != - ] && [ "$p" -gt "$(date +%s)" ]; then
                hold_put "" "$L_T" "$L_G" person "$l" "$p"
                note "person: T$L_T until $p" "$l"
                i=$((i + 1)); continue
            fi
        fi
        [ "${c_go[i]}" = 0 ] || wake=1
        block="$block$l
"
        o_line[i]="$l" outs="$outs $i"
        i=$((i + 1))
    done
    # Nothing that wakes is left: no block; the selected rides and longs stay held.
    if [ -n "$wake" ]; then
        ! watch_off || return 0
        printf '%s' "$block"
        touch "$dir/last-wake"
        printed="$(date +%s)"
        for i in $outs; do
            l="${o_line[i]}"
            # A held line's entry goes once it is out.
            if [ -n "${c_file[i]}" ] && parse_line "$l"; then drop="$drop$L_T ${c_file[i]}
"; fi
            [ "${c_wp[i]}" = 0 ] || note "person: said" "$l"
            print_note "$l"
            said_put "$l" "${c_arr[i]}" "$printed"
        done
    fi
    # Only the entries that went out or went stale leave the held files.
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        ts=" "
        while IFS= read -r e; do
            [ "${e#* }" != "$f" ] || ts="$ts${e%% *} "
        done <<< "$drop"
        [ "$ts" != " " ] || continue
        awk -v ts="$ts" '!index(ts, " " $1 " ")' "$f" > "$f.tmp"
        mv "$f.tmp" "$f"
    done <<< "$picked"
}

# Drops each `again` line of machine $1 whose agent works in the row array $2: it woke and went on,
# so the rest it stopped in is over. Runs under the settle lock.
again_drop() { # label snapshot
    local f ts
    f="$(held_file "$1")"
    ts=" $(jq -r '[.[]? | select(.state == "working") | .terminal] | map(tostring) | join(" ")' <<< "$2" 2>/dev/null) "
    [ "$ts" != "  " ] || return 0
    awk -v ts="$ts" '$3 == "again" && index(ts, " " $1 " ")' "$f" | while IFS= read -r l; do
        note "again: dropped at work" "${l#* * * * * }"
    done
    awk -v ts="$ts" '!($3 == "again" && index(ts, " " $1 " "))' "$f" > "$f.tmp"
    mv "$f.tmp" "$f"
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

# A machine's background holds: "<t> <gen> <count> <since> <from>[ spent]", one per agent;
# `spent` once its limit was said.
bghold_file() { if [ -z "$1" ]; then echo "$dir/bghold"; else echo "$dir/bghold-$1"; fi; }

# Holds the agent line $2 just parsed, of machine $1, while its agent rests with work running in
# the background (see the top of this file). A plain rest with no task stays routine while routine
# is silent, and a line joins a short hold already there (one just let go). Whatever else was held
# for the agent is older news and goes, a person hold too: the line let go meets the brief again.
# A stop with fewer at work than the hold began with ends it, and takes its usual class. Fails for
# a line it does not hold. Runs under the settle lock.
bg_hold() { # label line
    local f h n o_g o_n o_s _
    BG_DROP=""
    case "$L_STATE" in idle | done) ;; *) return 1 ;; esac
    [ "$L_STAGE" != waiting-human ] && [ -z "$L_CLAIMED" ] || return 1
    f="$(bghold_file "$1")" h="$(held_file "$1")"
    n="$(jq -r --argjson t "$L_T" '[.[]? | select(.terminal == $t) | .background // 0][0] // 0' \
        "$(snapshot_file "$1")" 2>/dev/null || echo 0)"
    read -r _ o_g o_n _ _ o_s _ <<< "$(grep "^$L_T " "$f" 2>/dev/null | tail -1 || true)"
    [ "${o_g:-}" = "$L_G" ] || o_n=""
    # A spent hold (a job that never ends, a dev server): each stop takes its usual class at once.
    [ -z "$o_n" ] || [ "$n" -lt "$o_n" ] || [ -n "${o_s:-}" ] || return 0
    [ -n "$o_n" ] || [ "$n" -gt 0 ] || return 1
    ! { routine_quiet && [ "$L_STAGE" = - ] && routine "$2"; } || return 1
    ! awk -v t="$L_T" '$1 == t && $3 == "short" { f = 1 } END { exit !f }' "$h" 2>/dev/null || return 1
    # Fewer jobs than at the start: the line takes its own class, and the hold goes only once
    # that line is held there (`bg_drop`), so a crash between the two loses nothing.
    [ -z "$o_n" ] || { [ "$n" -ge "$o_n" ] || BG_DROP="$L_T"; return 1; }
    { grep -v "^$L_T " "$f" 2>/dev/null || true
      echo "$L_T $L_G $n $(date +%s) $L_FROM"; } > "$f.tmp"
    mv "$f.tmp" "$f"
    note "bg: held $n" "$2"
    [ ! -f "$h" ] || { awk -v t="$L_T" '$1 != t' "$h" > "$h.tmp"; mv "$h.tmp" "$h"; }
}

# Drops the background hold `bg_hold` left for terminal $BG_DROP of machine $1, once its stop's
# line is held in its own class.
bg_drop() {
    local f
    f="$(bghold_file "$1")"
    [ -n "${BG_DROP:-}" ] && [ -f "$f" ] || return 0
    { grep -v "^$BG_DROP " "$f" || true; } > "$f.tmp"
    mv "$f.tmp" "$f"
    BG_DROP=""
}

# On the tick, lets go each machine's background holds that are over (`bgcheck`): the line goes
# to the short hold (`again` for the rest said last again, `bg_class`), so it settles and meets
# the brief as any line, and the hold goes only once
# that line is held. A hold jq cannot read goes once past QASS_BG_LIMIT. Runs under the settle lock.
bg_check() {
    local f label snap skip now t g n since from spent r late kept
    now="$(date +%s)"
    for f in "$dir"/bghold "$dir"/bghold-*; do
        [ -s "$f" ] || continue
        case "$f" in *.tmp) continue ;; esac
        label="${f#"$dir"/bghold}"; label="${label#-}"
        [ ! -e "$(gone_file "$label")" ] || continue
        snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
        skip="$(skips_of "$snap")" kept=""
        while read -r t g n since from spent _; do
            late=false; [ $((now - since)) -lt "$bglimit" ] || late=true
            if ! r="$(jq -rn --argjson b "$snap" --arg t "$t" --argjson g "$g" --argjson n "$n" \
                --argjson late "$late" --arg from "$from" --argjson self "$self" --argjson skip "$skip" \
                --argjson spent "$([ -n "$spent" ] && echo true || echo false)" --arg at "$(at_of "$label")" \
                "$bgcheck" 2>/dev/null)"; then
                r=keep; [ "$late" = false ] || { r=drop; note "bg: unreadable" "$t $g $n $since $from $spent"; }
            fi
            # A stop with fewer jobs already went to its own class (`bg_hold`, `bg_drop`): the
            # hold is over, and handing it on again would say that stop twice. At the limit,
            # that line stands for it: the hold is spent.
            if [ "$r" != keep ] && [ -n "$r" ] && [ "$r" != drop ] &&
                awk -v t="$t" '$1 == t { f = 1 } END { exit !f }' "$(held_file "$label")" 2>/dev/null; then
                case "$r" in late\ *) r=spent ;; *) r=drop ;; esac
            fi
            case "$r" in
                keep | '') kept="$kept$t $g $n $since $from${spent:+ spent}"$'\n' ;;
                drop) ;;
                spent) kept="$kept$t $g $n $since $from spent"$'\n' ;;
                # The limit: said once, and the hold stays, spent, so a job that never ends
                # (a dev server) holds none of the agent's later stops.
                late\ *) note "bg: limit" "${r#late }"
                    hold_put "$label" "$t" "$g" "$(bg_class "${r#late }")" "${r#late }"
                    kept="$kept$t $g $n $since $from spent"$'\n' ;;
                *) note "bg: let go" "$r"; hold_put "$label" "$t" "$g" "$(bg_class "$r")" "$r" ;;
            esac
        done < "$f"
        printf '%s' "$kept" > "$f.tmp"
        mv "$f.tmp" "$f"
    done
}

# The class of a line a background hold let go: work in progress at rest takes the class its stop
# would have (`rest_class`: the rest said last again is `again`), anything else short.
bg_class() { # line
    if parse_line "$1" && work_rest; then rest_class "$1"; else echo short; fi
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
    class="$(hold_class "$1" "$2")"
    case "$class" in
        long) if [ -z "$(record_of "$L_T" 2>/dev/null)" ]; then note "long: no record" "$2"; else note "long: written by another" "$2"; fi ;;
    esac
    if [ "$class" = short ] && [ "$settle" -le 0 ]; then
        settle_locked flush "$2"
    else
        settle_locked hold_put "$1" "$L_T" "$L_G" "$class" "$2"
    fi
}

# Whether the line just parsed is work in progress that came to rest: an agent on an open task,
# not claimed, not waiting on the owner, from working or starting to idle or done. qass hears it
# in seconds (`short`), not after the routine or long hold: work that stops must not wait for him.
work_rest() {
    case "$L_FROM.$L_STATE" in working.idle | working.done | starting.idle | starting.done) ;; *) return 1 ;; esac
    [ "$L_STAGE" != - ] && [ "$L_STAGE" != waiting-human ] && [ -z "$L_CLAIMED" ] && ! driven
}

# Whether another agent drives the agent of the line just parsed (this Mac only): the newest
# prompted.log record for it is under an hour old and from a terminal still running that is not
# qass (an earlier qass tab works in qass's folder too, and a writer gone drives nothing).
driven() {
    local at from
    [ -z "$L_LABEL" ] || return 1
    read -r at from <<< "$(record_of "$L_T")"
    [ -n "${at:-}" ] && [ "$at" != unreadable ] && [ "${from:-}" != "$self" ] || return 1
    [ $(($(date +%s) - at)) -lt 3600 ] || return 1
    jq -e --argjson t "$from" --arg d "$dir" \
        'any(.[]?; .terminal == $t and ((.cwd // "") | startswith($d) | not))' \
        "$dir/last.json" > /dev/null 2>&1
}

# Whether a line is routine (see the top of this file). Run after `urgent`, which parsed it.
routine() {
    parse_line "$1" || return 1
    [ "$L_STATE" != sleeping ] || return 0
    case "$L_FROM.$L_STATE" in working.idle | working.done | starting.idle | starting.done) ;; *) return 1 ;; esac
    ! work_rest || return 1
    [ "$L_STAGE" != waiting-human ] && [ -z "$L_CLAIMED" ]
}

# A routine line: a sleep is only logged, unless a line that is not routine is held for its
# agent (a question it left): then it goes as any held line; a stop qass already handled rides
# (`handled_repeat`); any other rest is held for the routine hold.
quiet_routine() { # label line
    if [ "$L_STATE" = sleeping ]; then
        if awk -v t="$L_T" '$1 == t && $3 != "routine" { f = 1 } END { exit !f }' \
            "$(held_file "$1")" 2>/dev/null; then
            quiet_hold "$1" "$2"
        else
            note "routine: asleep" "$2"
        fi
    elif handled_repeat "$1" "$2"; then
        settle_locked hold_put "$1" "$L_T" "$L_G" ride "$2"
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
    local label="$1" at prev snap out sleeping file gone skip
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
                    --argjson skip "$(skips_of "$prev")" "$busy" >/dev/null 2>&1; then
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
            skip="$(skips_of "$prev" "$snap")"
            if jq -en --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --argjson skip "$skip" --arg at "$at" "$vanished" >/dev/null 2>&1; then
                # A tab that slept leaves the agent rows too; `qoop ls` still names it.
                sleeping="$(sleeping_on "$label")"
            fi
            out="$(jq -rn --argjson a "$prev" --argjson b "$snap" --argjson self "$self" \
                --argjson skip "$skip" --arg at "$at" --argjson sleeping "$sleeping" "$diff" 2>/dev/null)" || { step_end; continue; }
            # Off: the snapshot is kept, and nothing of it is said or held.
            ! watch_off || out=""
            # Quiet checks held lines against the saved snapshot: save this one first, so a
            # line held from it is checked against the rows it came from.
            [ "$quiet" != 1 ] || { printf '%s\n' "$snap" > "$file.tmp" && mv "$file.tmp" "$file"; }
            # An agent held `again` that works now: that rest is over (see the top of this file).
            if [ "$quiet" = 1 ] && grep -q '^[0-9]* [^ ]* again ' "$(held_file "$label")" 2>/dev/null; then
                settle_locked again_drop "$label" "$snap"
            fi
            now_lines=""
            while IFS= read -r l; do
                [ -n "$l" ] || continue
                if [ "$quiet" = 1 ]; then
                    l="$(limit_line "$label" "$l")"
                    [ -n "$l" ] || continue
                    if urgent "$l"; then now_lines="$now_lines$l
"; elif settle_locked bg_hold "$label" "$l"; then :
                    elif routine_quiet && routine "$l"; then quiet_routine "$label" "$l"
                    else quiet_hold "$label" "$l"; fi
                    [ -z "${BG_DROP:-}" ] || settle_locked bg_drop "$label"
                else
                    hold "$label" "$l" || now_lines="$now_lines$l
"
                fi
            done <<< "$out"
            if [ -n "$now_lines" ] && ! watch_off; then
                if [ "$quiet" = 1 ]; then
                    settle_locked flush "${now_lines%?}"
                else
                    say "$(tagged "${now_lines%?}")"
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
    wlog "said watch-off at start"
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
        wlog watch-running
        echo "watch-running: one already runs"
        exit 0
    fi
    rm -f "$pidf"
    take_pid || { wlog watch-running; echo "watch-running: one already runs"; exit 0; }
    # Two that found the same stale file: the one whose pid is not there a moment later leaves.
    sleep 0.2
    [ "$(cat "$pidf" 2>/dev/null)" = "$$" ] || { wlog watch-running; echo "watch-running: one already runs"; exit 0; }
fi
drop_pid() {
    local st="${1:-$?}"
    wlog "exit $st"
    [ "$(cat "$pidf" 2>/dev/null)" != "$$" ] || rm -f "$pidf"
}
trap drop_pid EXIT
# The logs keep 30 days, by the time each line starts with, so a week of watching that spans
# days off stays whole for the replay.
month=$(( $(date +%s) - 2592000 ))
cut_old() { awk -v c="$month" '$1 !~ /^[0-9]+$/ || $1 >= c' "$1" > "$1.tmp" && mv "$1.tmp" "$1"; }
[ ! -f "$dir/watch.log" ] || cut_old "$dir/watch.log"
wlog "start $$"
# Its own time is up: said, so qass starts it again (Start, step 3). Or the owner turned it off.
# Set before the tick and the remotes start, so a stop asked for while they do is never lost.
trap 'end_with watch-ends' USR1
trap 'end_with "$off_line"' USR2
# On again since the last watcher: qass sums the fleet up, so this starts from now.
resume=0
stamp="$(watch_unseen || true)"
if [ -n "$stamp" ]; then
    resume=1
    rm -f "$dir"/held "$dir"/held-* "$dir"/bghold "$dir"/bghold-* "$dir"/settle "$dir"/settle-*
    touch "$(base_file "")"
    seen_stamp "$stamp"
fi

rmdir "$dir/settle.lock" 2>/dev/null || true
# The briefs' folders a killed watcher left.
rm -rf "$dir"/.facts.*
if [ "$quiet" = 1 ]; then
    # Held lines and marks carry over; the logs are cut, the said lines of a week ago go.
    [ ! -f "$dir/quiet.log" ] || cut_old "$dir/quiet.log"
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
        claims_said
        if [ "$quiet" = 1 ]; then
            settle_locked bg_check
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
                    --arg state "$state" --argjson self "$self" --argjson skip "$(skips_of "$snap")" \
                    --arg at "$(at_of "$label")" "$settled" 2>/dev/null)" || continue
                if [ -n "$out" ]; then
                    say "$(tagged "$out")"
                fi
            done
        done
        due | while read -r label t; do
            [ "$label" = - ] && label=""
            # With that machine's daemon away its last snapshot is stale: wait.
            [ ! -e "$(gone_file "$label")" ] || continue
            snap="$(cat "$(snapshot_file "$label")" 2>/dev/null)" || continue
            out="$(jq -rn --argjson b "$snap" --argjson due "[\"$t\"]" --argjson self "$self" \
                --argjson skip "$(skips_of "$snap")" --arg at "$(at_of "$label")" "$again" 2>/dev/null)" || continue
            if [ -n "$out" ]; then
                if [ "$quiet" = 1 ]; then settle_locked flush "$out"; else say "$(tagged "$out")"; fi
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
trap 'st=$?; IFS=$'"'"' \t\n'"'"'; for k in $kids $$; do pkill -P "$k" 2>/dev/null || true; done
      kill $kids 2>/dev/null || true; drop_pid "$st"' EXIT
# Killed rather than ended (Monitor stopping it), go through the same way out.
trap 'exit 143' TERM INT HUP

[ "$quiet" = 1 ] || rm -f "$(gone_file "")"   # a mark left by an earlier run says nothing about this one
follow "" < <(qoop agents --watch --json)

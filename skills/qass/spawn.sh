#!/bin/bash
# qass spawn: a new worker in its own worktree, ready for a brief.
#
#   spawn.sh <repo> <branch> [--agent claude|opencode] [--space NAME]
#                                   prints "<terminal> <generation>"
#
# The repo must be a git repository under $HOME and not $HOME itself. A Claude
# worker runs in the owner's default permission mode with worker.json, whose
# deny rules win over the owner's user allow list; an opencode worker gets the
# same denials from worker-opencode.json, merged over the owner's opencode config.
# The space is made when missing, as `qoro new --space` does.
# The brief goes afterwards, through the gate, with `qoro prompt`.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd -P)"
repo="${1:?usage: spawn.sh <repo> <branch>}"
branch="${2:?usage: spawn.sh <repo> <branch>}"
shift 2
agent=claude
where=()
while [ $# -ge 2 ]; do
    case "$1" in
        --agent) agent="$2" ;;
        --space) where=(--space "$2") ;;
        *) echo "spawn: unknown $1" >&2; exit 1 ;;
    esac
    shift 2
done
[ $# -eq 0 ] || { echo "spawn: $1 needs a value" >&2; exit 1; }
case "$agent" in
    claude) cmd=(claude --settings "$here/worker.json") ;;
    # Inline: opencode 1.18 lists an OPENCODE_CONFIG file's rules but does not enforce
    # them (measured); OPENCODE_CONFIG_CONTENT's it does.
    opencode) cmd=(env OPENCODE_CONFIG_CONTENT="$(cat "$here/worker-opencode.json")" opencode) ;;
    *) echo "spawn: agent is claude or opencode" >&2; exit 1 ;;
esac
case "$branch" in -*) echo "spawn: bad branch $branch" >&2; exit 1 ;; esac
repo="$(cd "$repo" && pwd -P)"
home="$(cd "$HOME" && pwd -P)"
case "$repo" in "$home"/*) ;; *) echo "spawn: $repo is not under $home" >&2; exit 1 ;; esac
git -C "$repo" rev-parse --git-dir >/dev/null
git check-ref-format --branch "$branch" >/dev/null

mkdir -p "@QASS_DATA@"
cd "$repo"
# Prints `worktree=<path> branch=<name>`, then `terminal=<n>`.
t="$(qoro worktree new "$branch" ${where[@]+"${where[@]}"} -- "${cmd[@]}" | sed -n 's/^terminal=//p')"
case "$t" in '' | *[!0-9]*) echo "spawn: no terminal number from qoro worktree new" >&2; exit 1 ;; esac
# From here the worker exists: nothing below may end the script early by error.
# Claude asks once whether to trust a new folder; that and nothing else is answered
# here. Ready is the agent's own idle screen, seen twice in a row (the first frames
# draw in pieces): for Claude its input line on a screen that does not ask, for
# opencode its idle state. The shell before it may draw ❯ too, and its generation
# is the shell's, so both are read again on every pass.
answered=0
seen=0
for _ in $(seq 180); do
    row="$(qoro agents --json 2>/dev/null | jq -c ".[] | select(.terminal == $t)" 2>/dev/null || true)"
    g="$(printf '%s' "$row" | jq -r '.generation // empty' 2>/dev/null || true)"
    state="$(printf '%s' "$row" | jq -r '.state // empty' 2>/dev/null || true)"
    program="$(printf '%s' "$row" | jq -r '.program // empty' 2>/dev/null || true)"
    if [ -n "$g" ] && [ "$program" = opencode ] && [ "$agent" = opencode ]; then
        if [ "$state" = idle ]; then seen=$((seen + 1)); else seen=0; fi
        if [ "$seen" -ge 2 ]; then
            printf '{"ts":%s,"spawned":%s,"gen":%s}\n' "$(date +%s)" "$t" "$g" >> "@QASS_DATA@/gate.jsonl"
            echo "$t $g"
            exit 0
        fi
    elif [ -n "$g" ] && [ "$program" = claude ] && [ "$agent" = claude ]; then
        screen="$(qoro read "$t" 2>/dev/null || true)"
        if printf '%s' "$screen" | grep -qi "trust this folder"; then
            seen=0
            if [ "$answered" = 0 ]; then
                # The cursor starts on "No, exit"; Enter alone would close the worker.
                if printf '%s' "$screen" | grep -Eq '❯ *([0-9]+\. *)?Yes, I trust'; then
                    keys="--key enter"
                else
                    keys="--key down --key enter"
                fi
                # shellcheck disable=SC2086
                qoro send "$t" $keys --generation "$g" >/dev/null 2>&1 && answered=1
            fi
        elif [ "$state" != blocked ] && printf '%s' "$screen" | grep -q "❯"; then
            seen=$((seen + 1))
            if [ "$seen" -ge 2 ]; then
                printf '{"ts":%s,"spawned":%s,"gen":%s}\n' "$(date +%s)" "$t" "$g" >> "@QASS_DATA@/gate.jsonl"
                echo "$t $g"
                exit 0
            fi
        else
            seen=0
        fi
    fi
    sleep 0.5
done
echo "spawn: terminal $t did not reach $agent's idle screen in 90 s" >&2
exit 1

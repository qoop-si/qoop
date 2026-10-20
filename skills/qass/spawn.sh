#!/bin/bash
# qass spawn: a new worker in its own worktree, ready for a brief.
#
#   spawn.sh <repo> <branch> [--agent NAME] [--space NAME] [--tier best|mid|low]
#                                   prints "<terminal> <generation>"
#
# The repo must be a git repository under $HOME and not $HOME itself. The worker
# runs the owner's own line for that agent: NAME is claude, opencode or a name
# from their `[agents]` (config.toml), and without --agent it is `[qass] agent`,
# so a worker starts with the flags they gave qass. What it may do is in its brief. The space is made when missing, as `qoop new --space` does. The brief
# goes afterwards, with `qoop prompt`.
#
# A claude worker always starts on a model of `[qass.models]` (config.toml; opus, sonnet, haiku
# by default, the newest of each family) through ANTHROPIC_MODEL: the tier's, or with no --tier
# a model the owner's line already names (`--model`), else mid's. A Fable model is never used:
# the config falls back and the line's own one is dropped. Every other flag of their line stays.
# opencode keeps its own model. A woken worker comes back on claude's default (a restore types
# a bare `claude --resume`).
set -euo pipefail

repo="${1:?usage: spawn.sh <repo> <branch>}"
branch="${2:?usage: spawn.sh <repo> <branch>}"
shift 2
agent=
tier=
where=()
while [ $# -ge 2 ]; do
    case "$1" in
        --agent) agent="$2"; agent_given=1 ;;
        --tier) case "$2" in best | mid | low) tier="$2" ;; *) echo "spawn: --tier is best, mid or low" >&2; exit 1 ;; esac ;;
        # qass's own space is on no list the owner sees: a worker there would vanish.
        --space) [ "$2" != qass ] || { echo "spawn: the space qass is qass's own; pick another" >&2; exit 1; }
                 where=(--space "$2") ;;
        *) echo "spawn: unknown $1" >&2; exit 1 ;;
    esac
    shift 2
done
[ $# -eq 0 ] || { echo "spawn: $1 needs a value" >&2; exit 1; }
# The owner's line for that name, from their config: an `[agents]` line as written,
# else the name itself.
config="$(qoop config --json)"
[ -n "$agent" ] || agent="$(printf '%s' "$config" | jq -r '.settings["qass.agent"] // "claude"')"
line="$(printf '%s' "$config" | jq -r --arg a "$agent" '[.agents[] | select(.name == $a) | .command][0] // $a')"
case "$line" in *[\'\"\$\`\\]*) echo "spawn: the line for $agent holds quotes or \$; give spawn a plain one" >&2; exit 1 ;; esac
read -ra cmd <<<"$line"
[ "${#cmd[@]}" -gt 0 ] || { echo "spawn: the line for $agent is empty" >&2; exit 1; }
# Not typed into a shell, so a home path is spelled out here.
case "${cmd[0]}" in "~/"*) cmd[0]="$HOME/${cmd[0]#\~/}" ;; esac
# Its program: the first word that is not an assignment, by name. An assignment
# first runs through env.
program=
for w in "${cmd[@]}"; do case "$w" in *=*) ;; *) program="${w##*/}"; break ;; esac; done
case "${cmd[0]}" in *=*) cmd=(env "${cmd[@]}") ;; esac
case "$program" in
    claude | opencode) ;;
    # qass on another agent: its workers are Claude, whose ready screen this knows.
    *) [ -n "${agent_given:-}" ] && { echo "spawn: $agent runs $program; a worker runs claude or opencode" >&2; exit 1; }
       cmd=(claude); program=claude ;;
esac
if [ "$program" = claude ]; then
    # The model the owner's line already names, and the line without it.
    own= rest=()
    for ((i = 0; i < ${#cmd[@]}; i++)); do
        case "${cmd[i]}" in
            # A value only when one follows: `--model --verbose` keeps --verbose.
            --model) case "${cmd[i + 1]:-}" in '' | -*) ;; *) own="${cmd[i + 1]}"; i=$((i + 1)) ;; esac ;;
            --model=*) own="${cmd[i]#--model=}" ;;
            # Their own ANTHROPIC_MODEL= would win over the tier's (the last one counts): it is
            # their line's model, like --model.
            ANTHROPIC_MODEL=*) own="${cmd[i]#ANTHROPIC_MODEL=}" ;;
            *) rest+=("${cmd[i]}") ;;
        esac
    done
    case "$(printf '%s' "$own" | tr 'A-Z' 'a-z')" in *fable*) echo "spawn: the line's model $own is Fable, never used" >&2; own= ;; esac
    if [ -n "$tier" ] || [ -z "$own" ]; then
        use="${tier:-mid}"
        model="$(printf '%s' "$config" | jq -r --arg k "qass.models.$use" '.settings[$k] // empty')"
        printf '%s' "$config" | jq -r '.problems[]? | select(test("qass.models"))' >&2 || true
        [ -n "$model" ] || { echo "spawn: no model for tier $use" >&2; exit 1; }
        echo "spawn: claude on $model (tier $use)" >&2
        # Claude Code's own variable rather than a flag: a wrapper or stand-in that takes no
        # flags still starts, and nothing of their line changes but a model it named.
        case "${rest[0]}" in
            env) cmd=(env "ANTHROPIC_MODEL=$model" "${rest[@]:1}") ;;
            *) cmd=(env "ANTHROPIC_MODEL=$model" "${rest[@]}") ;;
        esac
    else
        cmd=("${rest[@]}" --model "$own")
    fi
elif [ -n "$tier" ]; then
    echo "spawn: $program keeps its own model; tiers choose claude models" >&2
fi
case "$branch" in -*) echo "spawn: bad branch $branch" >&2; exit 1 ;; esac
repo="$(cd "$repo" && pwd -P)"
home="$(cd "$HOME" && pwd -P)"
case "$repo" in "$home"/*) ;; *) echo "spawn: $repo is not under $home" >&2; exit 1 ;; esac
git -C "$repo" rev-parse --git-dir >/dev/null
git check-ref-format --branch "$branch" >/dev/null

mkdir -p "@QASS_DATA@"
cd "$repo"
# Prints `worktree=<path> branch=<name>`, then `terminal=<n>`.
t="$(qoop worktree new "$branch" ${where[@]+"${where[@]}"} -- "${cmd[@]}" | sed -n 's/^terminal=//p')"
case "$t" in '' | *[!0-9]*) echo "spawn: no terminal number from qoop worktree new" >&2; exit 1 ;; esac
# From here the worker exists: nothing below may end the script early by error.
# Claude asks once whether to trust a new folder; that and nothing else is answered
# here. Ready is the agent's own idle screen, seen twice in a row (the first frames
# draw in pieces): for Claude its input line on a screen that does not ask, for
# opencode its idle state. The shell before it may draw ❯ too, and its generation
# is the shell's, so both are read again on every pass.
want="$program"
answered=0
seen=0
for _ in $(seq 180); do
    row="$(qoop agents --json 2>/dev/null | jq -c ".[] | select(.terminal == $t)" 2>/dev/null || true)"
    g="$(printf '%s' "$row" | jq -r '.generation // empty' 2>/dev/null || true)"
    state="$(printf '%s' "$row" | jq -r '.state // empty' 2>/dev/null || true)"
    program="$(printf '%s' "$row" | jq -r '.program // empty' 2>/dev/null || true)"
    if [ -n "$g" ] && [ "$program" = opencode ] && [ "$want" = opencode ]; then
        if [ "$state" = idle ]; then seen=$((seen + 1)); else seen=0; fi
        if [ "$seen" -ge 2 ]; then
            echo "$t $g"
            exit 0
        fi
    elif [ -n "$g" ] && [ "$program" = claude ] && [ "$want" = claude ]; then
        screen="$(qoop read "$t" 2>/dev/null || true)"
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
                qoop send "$t" $keys --generation "$g" >/dev/null 2>&1 && answered=1
            fi
        elif [ "$state" != blocked ] && printf '%s' "$screen" | grep -q "❯"; then
            seen=$((seen + 1))
            if [ "$seen" -ge 2 ]; then
                echo "$t $g"
                exit 0
            fi
        else
            seen=0
        fi
    fi
    sleep 0.5
done
echo "spawn: terminal $t did not reach $want's idle screen in 90 s" >&2
exit 1

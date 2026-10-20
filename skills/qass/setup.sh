#!/bin/sh
# qass setup: the six-item checklist of a fresh install, each box ticked only from what is
# really there (a file, a config value, a login), never from anyone's word. See setup.md.
#
#   setup.sh status                the checklist, then `Next: type <n>` or `setup done`; writes nothing
#   setup.sh seen                  the checklist was shown once (a first start then never repeats it)
#   setup.sh projects [folder]     git main checkouts, newest commit first
#   setup.sh servers               the ssh hosts of ~/.ssh/config and of shell history, what each is in
#                                  qoop, most used first
#   setup.sh pick <path>           they picked this project (an absolute git main checkout, not $HOME),
#                                  and its space: one shell tab there, recorded
#   setup.sh drop <path>           they took it back
#   setup.sh skip <n> | unskip <n> they skip item 2 to 6, or take that back: `[-]`, never `[x]`
#   setup.sh skip-host <host>      they do not want this host in qoop
#   setup.sh ssh-space <host> [target] [--dir <folder>]
#                                  a space <host> with one tab running `ssh <target>` (the host when no
#                                  target), or with a folder a space <host>-<its name> whose tab logs in
#                                  there; recorded
#   setup.sh dirs <host> [target]  the git projects on that server, after their yes: one read-only ssh
#   setup.sh remote-space <host> <folder>
#                                  a space on the qoop remote <host> with one tab in <folder>, recorded
#   setup.sh agents                each agent qass knows that is installed, its [agents] lines, and the
#                                  flag that skips its approval prompts when its own --help has it
#   setup.sh agents-add            add the proposed lines (`qoop config set agents.<name>`), recorded
#   setup.sh agents-done           they decided how their agents start
#   setup.sh watch on|off          whether qass watches their agents (`qoop qass watch`), recorded
#   setup.sh write <file>          a new `.qoop/` file of a picked project from standard input, kept
#                                  with its hash; refused when the file is theirs (there before setup)
#   setup.sh undo [project]        remove what setup wrote and they did not change or commit since
#   setup.sh job <KEY>             the first small job qass started for them, by its board key
#
# Writes only @QASS_DATA@/setup and the files `write` makes; only `undo` deletes, only those (and
# opens tabs in pick, ssh-space and remote-space). Runs ssh only in `dirs` (one read-only listing
# that never asks for a password) and through on.sh in `remote-space`; `qoop task rules` reads
# gh's local login for item 4. Reads nothing under ~/.ssh but its config, and of shell history
# only the ssh commands whose every word fits `valid_target`: nothing else in it leaves this
# script. Unrelated to `qoop setup` (Setup files).
set -eu

state="@QASS_DATA@/setup"
roots="code coding projects src dev Developer work repos"
nl='
'

has() { [ -f "$state" ] && grep -qxF -- "$1" "$state"; }

# One line in or out of the state file, written whole through a rename.
put() {
    mkdir -p "$(dirname "$state")"
    { [ -f "$state" ] && grep -vxF -- "$1" "$state" || true; } > "$state.new"
    printf '%s\n' "$1" >> "$state.new"
    mv -f "$state.new" "$state"
}
del() {
    [ -f "$state" ] || return 0
    { grep -vxF -- "$1" "$state" || true; } > "$state.new"
    mv -f "$state.new" "$state"
}

# The main checkout of the repo holding $1, or nothing.
main_of() {
    common="$(git -C "$1" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || return 0
    case "$common" in */.git) dirname "$common" ;; esac
}

stack_of() {
    if [ -f "$1/Cargo.toml" ]; then echo rust
    elif [ -f "$1/package.json" ]; then
        if ls "$1"/bun.lock* >/dev/null 2>&1; then echo bun; else echo node; fi
    elif [ -f "$1/pyproject.toml" ]; then echo python
    elif [ -f "$1/go.mod" ]; then echo go
    else echo -
    fi
}

# Every main checkout under the roots (or the folder given), and the live agents' repos.
# ponytail: fixed roots, depth 3; a repo elsewhere comes in by `projects <folder>`.
# ~/Documents and ~/Desktop are left out: macOS stops to ask for folder access there.
found() {
    if [ -n "${1:-}" ]; then
        set -- "$1"
    else
        set --
        for r in $roots; do [ -d "$HOME/$r" ] && set -- "$@" "$HOME/$r"; done
    fi
    {
        [ $# -gt 0 ] && find "$@" -maxdepth 3 -name .git -type d -prune 2>/dev/null |
            sed 's|/\.git$||'
        # ponytail: a cwd JSON had to escape (a quote or a backslash) is left out, not decoded.
        qoop agents --json 2>/dev/null | grep -o '"cwd":"[^"]*"' | sed 's/^"cwd":"//; s/"$//' |
            grep -v '\\'
    } | while IFS= read -r d; do [ -d "$d" ] && main_of "$d"; done |
        grep -vxF "$(cd "$HOME" && pwd -P)" | sort -u
}

searched() {
    out=
    for r in $roots; do [ -d "$HOME/$r" ] && out="$out ~/$r"; done
    echo "${out# }"
}

projects() {
    found "${1:-}" | while IFS= read -r p; do
        t="$(git -C "$p" log -1 --format=%ct 2>/dev/null || echo 0)"
        printf '%s\t%s\n' "${t:-0}" "$p"
    done | sort -rn | awk -F'\t' '
        NR <= 30 { print $2 } NR > 30 { more++ } END { if (more) print "+" more " more" }' |
        { n=0; while IFS= read -r p; do
            case "$p" in +*) echo "$p"; continue ;; esac
            n=$((n + 1))
            q=no; [ -d "$p/.qoop" ] && q=yes
            when="$(git -C "$p" log -1 --format=%cr 2>/dev/null)" || when=never
            mark=; has "pick $p" && mark=" (picked)"
            printf '%2d  %s  %s  last commit %s  .qoop/ %s%s\n' "$n" "$p" "$(stack_of "$p")" "$when" "$q" "$mark"
        done; }
}

# ssh hosts: name, then its target, one per line, tab separated; odd names as "!name".
hosts() {
    f="$HOME/.ssh/config"
    [ -f "$f" ] || return 0
    awk '
        function flush(   i) {
            for (i = 1; i <= n; i++) {
                h = names[i]
                if (h ~ /[*?!]/) continue
                if (tolower(h) ~ /^(github\.com|gitlab\.com|bitbucket\.org|codeberg\.org)$/) continue
                if (tolower(hn) ~ /^(github\.com|gitlab\.com|bitbucket\.org|codeberg\.org)$/) continue
                if (h !~ /^[A-Za-z0-9_][A-Za-z0-9._-]*$/) { print "!" h; continue }
                t = (hn != "" ? hn : h); if (user != "") t = user "@" t
                if (t !~ /^([A-Za-z0-9_][A-Za-z0-9._-]*@)?[A-Za-z0-9_][A-Za-z0-9._-]*$/) { print "!" h; continue }
                print h "\t" t
            }
            n = 0; hn = ""; user = ""
        }
        { sub(/#.*/, ""); if ($0 ~ /^[ \t]*[A-Za-z]+[ \t]*=/) sub(/[ \t]*=[ \t]*/, " ") }
        tolower($1) == "host" { flush(); for (i = 2; i <= NF; i++) names[++n] = $i; next }
        tolower($1) == "match" { flush(); next }
        tolower($1) == "hostname" { hn = $2 }
        tolower($1) == "user" { user = $2 }
        END { flush() }
    ' "$f"
}

valid_host() {
    case "$1" in -* | "" | *[!A-Za-z0-9._-]*) return 1 ;; esac
}

# An ssh destination: [user@]host, plain words that never start with a dash.
valid_dest() {
    case "$1" in "" | *@*@* | *@ | *[!A-Za-z0-9._@-]* | [!A-Za-z0-9_]*) return 1 ;; esac
    case "${1#*@}" in [!A-Za-z0-9_]*) return 1 ;; esac
}

# The words of an ssh target setup uses: -p <port>, -l <user>, -i <key file>, -4, -6, then one
# destination last. Nothing else: no -o, no -J, no remote command. A history line holding any of
# those is dropped whole, so a password or a token typed in one never shows.
valid_target() {
    [ $# -gt 0 ] || return 1
    while [ $# -gt 1 ]; do
        case "$1" in
        -4 | -6) shift ;;
        -p) case "$2" in "" | *[!0-9]*) return 1 ;; esac; shift 2 ;;
        -l) case "$2" in "" | [!A-Za-z0-9_]* | *[!A-Za-z0-9._-]*) return 1 ;; esac; shift 2 ;;
        -i) case "$2" in "" | -* | *[!A-Za-z0-9._/~-]*) return 1 ;; esac; shift 2 ;;
        *) return 1 ;;
        esac
    done
    [ $# -eq 1 ] && valid_dest "$1"
}

# A folder on a server: absolute, plain characters, no `..`: what `dirs` prints and a tab cds to.
valid_folder() {
    case "$1" in /*) ;; *) return 1 ;; esac
    case "$1" in *[!A-Za-z0-9._/-]* | */../* | */.. | *//*) return 1 ;; esac
    [ "${#1}" -le 200 ]
}

# A space name from a folder's name: [A-Za-z0-9._-] only; never all digits (qoop reads those as a
# space's number), never a leading dash, never qass's own space.
name_of() {
    n="$(printf '%s' "$1" | LC_ALL=C tr -c 'A-Za-z0-9._-' '-')"
    case "$n" in "" | -*) n="p$n" ;; esac
    case "$n" in *[!0-9]*) ;; *) n="p-$n" ;; esac
    [ "$n" != qass ] || n=qass-project
    echo "$n"
}

# Whether a space name is recorded for something other than $2 (what its record says after it).
taken() {
    N="$1" W="$2" awk '
        ($1 == "space" || $1 == "ssh-dir" || $1 == "remote-space" || $1 == "ssh-space") &&
            $2 == ENVIRON["N"] {
            w = $0; sub(/^[^ ]+ [^ ]+ /, "", w); if (w != ENVIRON["W"]) f = 1
        }
        END { exit !f }' "$state" 2>/dev/null
}

# The space name recorded for $2 by a record of kind $1, or nothing.
named() {
    K="$1" W="$2" awk '
        $1 == ENVIRON["K"] { w = $0; sub(/^[^ ]+ [^ ]+ /, "", w); if (w == ENVIRON["W"]) n = $2 }
        END { if (n != "") print n }' "$state" 2>/dev/null || true
}

# Whether the `qoop ls --json` text $1 holds a space named $2.
space_in() { printf '%s' "$1" | grep -qF "\"name\":\"$2\""; }

# The space setup opens for $3 (what its record says after the name) under record kind $2, from
# the spaces now there ($1, `qoop ls --json`): the one recorded when it is still there (printed
# with a leading `=`), else a name nothing else has, neither a record nor a space of theirs
# (`undo` must never send them to remove a space setup did not make).
space_for() {
    n="$(named "$2" "$3")"
    if [ -n "$n" ] && space_in "$1" "$n"; then echo "=$n"; return 0; fi
    [ -z "$n" ] || { echo "$n"; return 0; }
    n="$4" i=1
    while taken "$n" "$3" || space_in "$1" "$n"; do i=$((i + 1)); n="$4-$i"; done
    echo "$n"
}

# The space of a picked project: one shell tab in it, unless its recorded space is still there.
# A space that does not open is said; the pick stays.
project_space() {
    n="$(space_for "$(qoop ls --json 2>/dev/null || true)" space "$1" "$(name_of "${1##*/}")")"
    case "$n" in =*) echo "space ${n#=} is there"; return 0 ;; esac
    if qoop new --space "$n" --name "$n" --cwd "$1" >/dev/null; then
        put "space $n $1"
        echo "space $n opened"
    else
        echo "the space for $1 did not open; pick it again to retry"
    fi
}

# The ssh commands of shell history (zsh, bash, fish) as `count<TAB>words`, most used first: the
# words after `ssh`, spaces folded, each command once. A line that continues another (a command
# over several lines) is left out, and so is any line with a backslash. Bytes stay bytes: zsh
# writes some of them metafied.
history_ssh() {
    {
        [ -f "$HOME/.zsh_history" ] && LC_ALL=C sed 's/^: [0-9]*:[0-9]*;//' "$HOME/.zsh_history"
        [ -f "$HOME/.bash_history" ] && LC_ALL=C grep -v '^#[0-9]' "$HOME/.bash_history"
        f="$HOME/.local/share/fish/fish_history"
        [ -f "$f" ] && LC_ALL=C sed -n 's/^- cmd: //p' "$f"
    } 2>/dev/null | LC_ALL=C awk '
        { cont = prev; prev = /\\$/ }
        cont || /\\/ { next }
        $1 == "ssh" && NF > 1 {
            w = $2; for (i = 3; i <= NF; i++) w = w " " $i
            if (!(w in c)) order[++n] = w
            c[w]++
        }
        END { for (i = 1; i <= n; i++) printf "%d\t%s\n", c[order[i]], order[i] }' |
        sort -s -t "$(printf '\t')" -k1,1nr
}

# ssh hosts as `name<TAB>target<TAB>uses<TAB>from<TAB>address`, an odd config name as `!name`:
# the config's hosts (target: the alias, so ssh keeps its Port, IdentityFile and ProxyJump;
# address: user@HostName, to match a remote by), each with how often history ssh-ed to it, and
# the 10 most used history targets that are none of them, named as a space may be (`name_of`).
# Most used first; config hosts never used last, in the config's order.
all_hosts() {
    tab="$(printf '\t')"
    hist="$(history_ssh | while IFS="$tab" read -r k w; do
        set -f
        # Split on purpose: the words of the command.
        # shellcheck disable=SC2086
        set -- $w
        set +f
        if valid_target "$@"; then printf '%s\t%s\n' "$k" "$w"; fi
    done)"
    CFG="$(hosts)" HIST="$hist" LC_ALL=C awk '
        BEGIN {
            nc = split(ENVIRON["CFG"], cl, "\n")
            for (i = 1; i <= nc; i++) {
                if (cl[i] == "") continue
                if (cl[i] ~ /^!/) { print cl[i]; continue }
                split(cl[i], f, "\t"); m++
                name[m] = f[1]; tgt[m] = f[1]; addr[m] = f[2]; from[m] = "config"; taken[f[1]] = 1
                at[f[1]] = m; at[f[2]] = m; h = f[2]; sub(/^.*@/, "", h); at[h] = m
            }
            nh = split(ENVIRON["HIST"], hl, "\n")
            for (i = 1; i <= nh; i++) {
                if (hl[i] == "") continue
                split(hl[i], f, "\t"); k = f[1]; w = f[2]
                # `ssh build`, `ssh me@build`: the config host, when nothing else was typed.
                h = w; sub(/^.*@/, "", h)
                if (w in at) { uses[at[w]] += k; continue }
                if (w !~ / / && (h in at)) { uses[at[h]] += k; continue }
                if (kept++ >= 10) continue
                h = w; sub(/^.* /, "", h); sub(/^.*@/, "", h)
                if (h ~ /^[0-9.]+$/) { nm = "h" h; gsub(/\./, "-", nm) } else { nm = h; sub(/\..*$/, "", nm) }
                if (nm ~ /^[0-9]+$/) nm = "p-" nm
                if (nm == "qass") nm = "qass-project"
                nm = substr(nm, 1, 20)
                base = nm; j = 1
                while (nm in taken) nm = base "-" (++j)
                taken[nm] = 1; m++
                name[m] = nm; tgt[m] = w; addr[m] = w; uses[m] = k; from[m] = "history"
            }
            for (i = 1; i <= m; i++)
                printf "%07d\t%07d\t%s\t%s\t%d\t%s\t%s\n", 9999999 - uses[i], i, name[i], tgt[i], uses[i], from[i], addr[i]
        }' | sort | cut -f3-
}

# What a host is in qoop: remote, ssh space, skipped, or open. A remote matches by its name, its
# whole saved target, or the host at the end of it.
host_state() {
    h="$1" t="$2"
    if qoop remote ls 2>/dev/null | H="$h" T="$t" awk '
        { r = $0; sub(/^[^ ]+ +/, "", r); d = $NF; e = ENVIRON["H"] }
        $1 == e || r == e || r == ENVIRON["T"] || substr(d, length(d) - length(e)) == "@" e { ok = 1 }
        END { exit !ok }'; then
        echo remote
    elif ssh_space "$h"; then
        echo "ssh space"
    elif has "skip-host $h"; then
        echo skipped
    else
        echo open
    fi
}

# Whether a host has an ssh space that is still there: its own, or one for a folder on it.
ssh_space() {
    ls="$(qoop ls --json 2>/dev/null)" || return 1
    if has "ssh-space $1" && space_in "$ls" "$1"; then return 0; fi
    for n in $(H="$1" awk '$1 == "ssh-dir" && $3 == ENVIRON["H"] { print $2 }' "$state" 2>/dev/null); do
        space_in "$ls" "$n" && return 0
    done
    return 1
}

# The flag that lets agent $1 run without asking before each command, only when its own --help
# lists it (read from each CLI's help on 2026-10-20); nothing for an agent without one.
skip_flag() {
    case "$1" in
    claude) f=--dangerously-skip-permissions ;;
    codex) f=--dangerously-bypass-approvals-and-sandbox ;;
    gemini) f=--yolo ;;
    opencode) f=--auto ;;
    *) return 0 ;;
    esac
    "$(agent_bin "$1")" --help 2>&1 </dev/null |
        grep -qE -- "(^|[[:space:],])$f([[:space:],=]|\$)" && echo "$f"
    return 0
}

# Where agent $1 runs from: on PATH, or Claude's own local install (reached through an alias).
agent_bin() {
    if command -v "$1" >/dev/null 2>&1; then echo "$1"
    elif [ "$1" = claude ] && [ -x "$HOME/.claude/local/claude" ]; then echo "$HOME/.claude/local/claude"
    fi
}

# The agents qass knows that are installed.
agents_here() {
    for a in claude codex gemini opencode; do [ -z "$(agent_bin "$a")" ] || echo "$a"; done
    return 0
}

# The `[agents]` lines as `<name> = <line>`; a line JSON had to escape is left out.
launchers() {
    qoop config --json 2>/dev/null | grep -o '{"name":"[^"]*","command":"[^"\\]*"}' |
        sed 's/^{"name":"\([^"]*\)","command":"\([^"]*\)"}$/\1 = \2/' || true
}

# Of the lines $2, those that run agent $1 (the first word that is not an assignment, by name).
lines_of() {
    printf '%s\n' "$2" | A="$1" awk -F' = ' 'NF > 1 {
        n = split($2, w, " "); for (i = 1; i <= n && w[i] ~ /=/; i++) ; p = w[i]; sub(/^.*\//, "", p)
        if (p == ENVIRON["A"]) printf "%s%s", (o++ ? ", " : ""), $0 }'
}

# The line A adds for agent $1, as `<name><TAB><line>`, from fixed parts only: its flag (when its
# help has one and no line of it holds that flag), the program where it runs, and a name no line
# has. Nothing else, so no text in a config line or a help page becomes a proposal.
propose_for() {
    f="$(skip_flag "$1")"
    [ -n "$f" ] || return 0
    all="$(launchers)"
    if lines_of "$1" "$all" | grep -qE -- "(^|[[:space:]])$f([[:space:],]|\$)"; then return 0; fi
    # Every name in the file, a line JSON had to escape included.
    names="$(qoop config --json 2>/dev/null | grep -o '"name":"[^"]*"' || true)"
    n="$1-auto" i=1
    while printf '%s\n' "$names" | grep -qxF "\"name\":\"$n\""; do i=$((i + 1)); n="$1-auto-$i"; done
    printf '%s\t%s %s\n' "$n" "$(agent_bin "$1")" "$f"
}

# Per agent installed: `<agent>  lines: <name> = <line>, ...  skip prompts: <flag>  propose: <name> =
# <line>`.
agents() {
    all="$(launchers)"
    agents_here | while IFS= read -r a; do
        mine="$(lines_of "$a" "$all")"
        printf '%s  lines: %s  skip prompts: %s' "$a" "${mine:--}" "$(skip_flag "$a" | grep . || echo none)"
        p="$(propose_for "$a")"
        [ -z "$p" ] || printf '  propose: %s = %s' "${p%%"$(printf '\t')"*}" "${p#*"$(printf '\t')"}"
        echo
    done
}

servers() {
    n=0
    all_hosts | while IFS="$(printf '\t')" read -r h t k from addr; do
        case "$h" in !*) echo "    ${h#!}  ignored: odd name"; continue ;; esac
        n=$((n + 1))
        times="$k times"; [ "$k" != 1 ] || times=once
        if [ "$from" = history ]; then
            said="  (history, $times)"
        elif [ "${k:-0}" -gt 0 ]; then
            said="  ($times in history)"
        else
            said=
        fi
        printf '%2d  %s  %s  %s%s\n' "$n" "$h" "$t" "$(host_state "$h" "$addr")" "$said"
    done
}

# The git projects on a server, absolute paths only, no hidden folder, at most 20; nothing it
# prints is trusted but paths `valid_folder` takes, and only its first 64 KB are read. BatchMode:
# ssh never asks for a password or a host key here; nothing is forwarded, no connection is kept.
dirs() {
    find='T=; command -v timeout >/dev/null 2>&1 && T="timeout 15"
for d in "$HOME" /srv /var/www /opt; do
    [ -d "$d" ] || continue
    m=2; [ "$d" = "$HOME" ] && m=3
    $T find "$d" -xdev -maxdepth "$m" -name .git -type d -prune 2>/dev/null
done
true'
    rc="$(mktemp)"
    out="$({ ssh -a -x -o BatchMode=yes -o ConnectTimeout=5 -o ServerAliveInterval=5 \
        -o ServerAliveCountMax=2 -o ClearAllForwardings=yes -o ControlMaster=no \
        -o ControlPath=none "$@" "sh -c '$find'" </dev/null 2>/dev/null && echo 0 > "$rc" ||
        echo "$?" > "$rc"; } |
        head -c 65536)"
    # 255 is ssh's own failure: it could not log in without asking.
    if [ "$(cat "$rc")" = 255 ]; then
        rm -f "$rc"
        echo "this server needs a password or a host-key answer first: type the folder you work in, or I open it as an ssh space"
        return 3
    fi
    rm -f "$rc"
    k=0
    printf '%s\n' "$out" | sed -n 's|/\.git$||p' | LC_ALL=C sort -u | while IFS= read -r d; do
        valid_folder "$d" || continue
        case "$d" in */.*) continue ;; esac
        k=$((k + 1))
        [ "$k" -le 20 ] || break
        printf '%2d  %s\n' "$k" "$d"
    done
    [ -n "$out" ] || echo "no git projects under ~, /srv, /var/www or /opt there: type the folder you work in"
}

# "yes" when qoop counts the project the person's own repo (its origin's owner is gh's login or
# a `qoop.owner` in the global git config),
# else "no". An empty token keeps `qoop task rules` off the daemon: the plain rules, here.
own_of() {
    if (cd "$1" && QOOP_TOKEN='' qoop task rules 2>/dev/null) | grep -q '^own repo: '; then
        echo yes
    else
        echo no
    fi
}

# Signed in: hub.json (what `qoop login` writes) is one valid JSON object holding a non-empty
# "account" and no "unlinking": true. The account's name is never printed. A strict parser for
# the flat object qoop writes (string, true, false, null or number values); anything else,
# nesting included, is not signed in.
signed_in() {
    [ -f "$1" ] || return 1
    LC_ALL=C awk '
        function bad() { exit 1 }
        function ch() { return substr(s, i, 1) }
        function skip() { while (i <= n && ch() ~ /[ \t\r\n]/) i++ }
        function str(   r, c) {
            if (ch() != "\"") bad()
            i++; r = ""
            while (i <= n) {
                c = ch()
                if (c == "\\") {
                    e = substr(s, i + 1, 1)
                    if (e == "u") { if (substr(s, i + 2, 4) !~ /^[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]$/) bad(); w = 6 }
                    else if (e ~ /^["\\\/bfnrt]$/) w = 2
                    else bad()
                    esc = 1; r = r substr(s, i, w); i += w; continue
                }
                if (c == "\"") { i++; return r }
                if (c ~ /[\001-\037]/) bad()
                r = r c; i++
            }
            bad()
        }
        { s = s $0 "\n" }
        END {
            n = length(s); i = 1
            skip(); if (ch() != "{") bad(); i++; skip()
            if (ch() == "}") { i++; k = "" } else for (;;) {
                # A key qoop writes never holds an escape: one that does is not trusted.
                skip(); esc = 0; k = str(); if (esc) bad(); skip(); if (ch() != ":") bad(); i++; skip()
                if (ch() == "\"") { v = str(); t = "s" }
                else if (substr(s, i, 4) == "true") { v = "true"; t = "b"; i += 4 }
                else if (substr(s, i, 5) == "false") { v = "false"; t = "b"; i += 5 }
                else if (substr(s, i, 4) == "null") { v = ""; t = "n"; i += 4 }
                else {
                    j = i; while (i <= n && ch() ~ /[-+.eE0-9]/) i++
                    if (substr(s, j, i - j) !~ /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][-+]?[0-9]+)?$/) bad()
                    t = "d"
                }
                if (k == "account") acc = (t == "s" && v != "")
                if (k == "unlinking") unl = (v == "true")
                skip(); c = ch(); i++
                if (c == "}") break
                if (c != ",") bad()
            }
            skip(); if (i <= n) bad()
            exit !(acc && !unl)
        }
    ' "$1"
}

box() {
    if has "skip $1"; then echo "[-]"; elif [ "$2" = yes ]; then echo "[x]"; else echo "[ ]"; fi
}

# The state file without the lines that start with $1 (a fixed string, never a pattern).
without() {
    [ -f "$state" ] || return 0
    PREFIX="$1" awk 'index($0, ENVIRON["PREFIX"]) != 1' "$state" > "$state.new" &&
        mv -f "$state.new" "$state"
}

sha() { { shasum -a 256 "$1" 2>/dev/null || sha256sum "$1"; } | cut -d' ' -f1; }

# The picked project a `.qoop/` file belongs to, or nothing: only workflow.md and project.md, as
# real files (no symlink), straight in `<picked>/.qoop/`.
owner_of() {
    case "$1" in */.qoop/workflow.md | */.qoop/project.md) ;; *) return 0 ;; esac
    [ ! -L "$1" ] && [ ! -L "${1%/*}" ] || return 0
    [ ! -e "$1" ] || [ -f "$1" ] || return 0
    r="$(cd "${1%/.qoop/*}" 2>/dev/null && pwd -P)" || return 0
    has "pick $r" && echo "$r"
}

# Take back what setup wrote: a file still as written and untracked goes; one they changed or
# committed is theirs, kept and named. Then the picks left with nothing, and how to drop a space.
undo() {
    only="${1:-}"
    [ -z "$only" ] || only="$(cd "$only" 2>/dev/null && pwd -P || echo "$only")"
    grep '^wrote ' "$state" 2>/dev/null | while IFS= read -r line; do
        rest="${line#wrote }" h="${line##* }"
        f="${rest% *}" r="${f%/.qoop/*}"
        [ -z "$only" ] || [ "$r" = "$only" ] || continue
        if [ ! -e "$f" ]; then
            del "$line"
        elif [ "$(owner_of "$f" || true)" != "$r" ]; then
            echo "kept $f: not a .qoop/ file of a picked project"
        elif [ "$(sha "$f")" != "$h" ]; then
            echo "kept $f: changed since I wrote it"
        elif ! tracked="$(git -C "$r" ls-files -- "$f" 2>/dev/null)"; then
            echo "kept $f: git did not answer for $r"
        elif [ -n "$tracked" ]; then
            echo "kept $f: in git (staged or committed), so it is yours"
        else
            rm -f -- "$f" && del "$line" && echo "removed $f"
            rmdir -- "$r/.qoop" 2>/dev/null || true
        fi
    done
    grep '^pick ' "$state" 2>/dev/null | sed 's/^pick //' | while IFS= read -r r; do
        { [ -z "$only" ] || [ "$r" = "$only" ]; } || continue
        PREFIX="wrote $r/" awk 'index($0, ENVIRON["PREFIX"]) == 1 { f = 1 } END { exit !f }' \
            "$state" 2>/dev/null || del "pick $r"
    done
    [ -n "$only" ] || [ ! -f "$state" ] || awk '
        $1 == "ssh-space" { print "space " $2 ": close its tab, then qoop space rm " $2 }
        $1 == "space" || $1 == "ssh-dir" { print "space " $2 ": close its tab, then qoop space rm " $2 }
        $1 == "remote-space" {
            print "space " $3 ":" $2 ": close its tab, then on " $3 ": qoop space rm " $2
            if (!($3 in seen)) { seen[$3] = 1; hosts[++n] = $3 }
        }
        $1 == "launcher" { print "agents." $2 ": delete its line under [agents] in config.toml (qoop config shows where)" }
        END {
            for (i = 1; i <= n; i++)
                print "qoop on " hosts[i] ": there, ~/.local/bin/qoop uninstall (it asks first); then here, qoop remote rm " hosts[i]
        }' "$state"
    if [ -n "$only" ]; then
        sp="$(named space "$only")"
        [ -z "$sp" ] || echo "space $sp: close its tab, then qoop space rm $sp"
    fi
    return 0
}

# The item's own record (not the board, which drops Done after a day): closed verified.
# Its fields in one object: the key, then (no brace between) state and outcome side by side, as the
# item is written; a task record has an outcome but no state, so it can never match.
job_is() {
    out="$(qoop task show "$1" --json 2>/dev/null)" || return 1
    printf '%s' "$out" | tr -d ' \n\t' | sed 's/"title":"[^"]*"//g' | grep -Eq "\"key\":\"$1\",[^{}]*\"state\":$2"
}
job_done() { job_is "$1" '"done","outcome":"verified"'; }

status() {
    next=

    c="$(qoop config --json 2>/dev/null || true)"
    if echo "$c" | grep -q '"exists":true' && echo "$c" | grep -q '"problems":\[\]'; then
        s1=yes d1="saved (change: qoop settings)"
    elif echo "$c" | grep -q '"exists":true'; then
        s1=no d1="config.toml has a line qoop cannot use: qoop config"
    else
        s1=no d1="not saved yet: say keep, or change one"
    fi
    later1= open1=
    has "agents done" || [ -z "$(agents_here)" ] || open1="how your agents start"
    # A switch they already turned (the pet's `watch` word) is their decision too.
    has "watch decided" || [ -f "@QASS_DATA@/watch" ] || open1="${open1:+$open1, }watching"
    if [ "$s1" = yes ] && [ -n "$open1" ]; then
        s1=no d1="saved; not decided: $open1" later1=yes
    fi

    picked="$(grep '^pick ' "$state" 2>/dev/null | sed 's/^pick //' || true)"
    if [ -z "$picked" ]; then
        # A new computer often has none of the usual folders: say so, not "found in :".
        where="$(searched)"
        [ -n "$where" ] && where="in $where" || where="(no ~/code, ~/src or such folder here)"
        s2=no d2="$(found | grep -c . || true) found $where: none picked"
    else
        s2=yes missing= nospace=
        spaces="$(qoop ls --json 2>/dev/null || true)"
        old="$IFS"; IFS="$nl"; set -f
        for p in $picked; do
            if [ ! -s "$p/.qoop/workflow.md" ] || [ ! -s "$p/.qoop/project.md" ]; then
                s2=no missing="$missing ${p##*/}"
            fi
            sp="$(named space "$p")"
            if [ -z "$sp" ] || ! space_in "$spaces" "$sp"; then
                s2=no nospace="$nospace ${p##*/}"
            fi
        done
        IFS="$old"
        set +f
        n="$(printf '%s\n' "$picked" | grep -c .)"
        if [ "$s2" = yes ]; then
            d2="$n set up"
        else
            d2="$n picked"
            [ -z "$missing" ] || d2="$d2, not written:$missing"
            [ -z "$nospace" ] || d2="$d2, no space:$nospace"
        fi
    fi

    total=0 open=0
    tab="$(printf '\t')"
    hl="$(all_hosts | grep -v '^!' || true)"
    if [ -n "$hl" ]; then
        old="$IFS"; IFS="$nl"; set -f
        for line in $hl; do
            total=$((total + 1))
            [ "$(host_state "${line%%"$tab"*}" "${line##*"$tab"}")" = open ] && open=$((open + 1))
        done
        IFS="$old"; set +f
    fi
    if [ "$total" -eq 0 ]; then
        s3=none d3="none in ~/.ssh/config or shell history"
    elif [ "$open" -eq 0 ]; then
        s3=yes d3="$total of $total configured or skipped"
    else
        s3=no d3="$total found, $open not decided"
    fi

    # Nothing to answer: what qoop's rules allow in each picked project, as qoop computes it.
    if [ -z "$picked" ]; then
        s4=none d4="after your projects"
    else
        s4=yes d4="own repo:"
        old="$IFS"; IFS="$nl"; set -f
        for p in $picked; do d4="$d4 ${p##*/} $(own_of "$p"),"; done
        IFS="$old"; set +f
        d4="${d4%,}"
    fi

    cfgdir="$(qoop paths config 2>/dev/null || true)"
    if [ -n "$cfgdir" ] && signed_in "$cfgdir/hub.json"; then
        s5=yes d5="this machine is signed in"
    elif dev="$(qoop devices ls 2>/dev/null)"; then
        if echo "$dev" | grep ' key .*  paired ' | grep -vq ', browser, expired'; then
            s5=yes d5="a phone or browser is paired"
        else
            s5=no d5='`qoop login`, then the questions reach your phone'
        fi
    else
        s5=no d5="not in this build of qoop"
    fi

    job="$(sed -n 's/^job //p' "$state" 2>/dev/null | tail -1)"
    if has "skip 2"; then
        s6=none d6="needs a project: you skipped them"
    elif [ -z "$job" ]; then
        s6=no d6="after your projects: one tiny real task, on a worktree"
    elif job_done "$job"; then
        s6=yes d6="$job is done: an agent finished it, checked"
    else
        s6=no d6="$job started: it ticks when it shows Done on the board"
    fi

    for i in 1 2 3 4 5 6; do
        eval "s=\$s$i d=\$d$i"
        if [ "$s" = none ]; then b="[-]"; else b="$(box "$i" "$s")"; fi
        # How the agents start is asked after the projects, never before them.
        if [ -z "$next" ] && [ "$b" = "[ ]" ]; then
            [ "$i" = 1 ] && [ -n "$later1" ] && [ "$s2" != yes ] && ! has "skip 2" || next=$i
        fi
        case $i in
        1) l="settings, how agents start" ;; 2) l="your projects" ;; 3) l="your servers" ;;
        4) l="what I may do without asking" ;; 5) l="your phone" ;; 6) l="a first small job" ;;
        esac
        printf '%s %s %-30s %s\n' "$b" "$i" "$l" "$d"
    done
    if [ -z "$next" ]; then echo "setup done"; exit 0; fi
    [ "$s2" = yes ] || echo "Repos somewhere else? Type: /qass setup <folder>"
    echo 'Not for you: `skip <n>`. Pause: `later` (`/qass setup` brings it back). Take back what I wrote: `undo`.'
    # Measured in the isolated run of plans/2026-10-11-qass-setup/; what was not timed gets none.
    case $next in
    1) took=" (under a minute)" ;; 2) took=" (about 2 to 3 minutes a project)" ;;
    3) took=" (about a minute)" ;; 6) took=" (about 2 minutes)" ;;
    *) took= ;;
    esac
    echo "Next: type $next$took."
}

cmd="${1:-status}"
case "$cmd" in
status) status ;;
seen) put seen ;;
agents) agents ;;
agents-done) put "agents done" ;;
watch)
    case "${2:-}" in on | off) ;; *) echo "usage: setup.sh watch on|off" >&2; exit 2 ;; esac
    qoop qass watch "$2"
    put "watch decided"
    ;;
agents-add)
    # Only what propose_for builds from fixed parts, never text read from anywhere.
    for a in $(agents_here); do
        p="$(propose_for "$a")"
        [ -n "$p" ] || continue
        n="${p%%"$(printf '\t')"*}" line="${p#*"$(printf '\t')"}"
        if qoop config set "agents.$n" "$line" >/dev/null; then
            put "launcher $n"
            echo "added $n = $line"
        else
            failed=yes
        fi
    done
    # A line that was not written leaves the question open.
    [ -z "${failed:-}" ] || exit 1
    put "agents done"
    ;;
projects) projects "${2:-}" ;;
servers) servers ;;
pick | drop)
    p="${2:?usage: setup.sh $cmd <absolute path>}"
    case "$p" in *[[:cntrl:]]*) echo "setup: a path with a control character is refused" >&2; exit 2 ;; esac
    case "$p" in /*) ;; *) echo "setup: give the absolute path" >&2; exit 2 ;; esac
    p="${p%/}"
    if [ "$cmd" = drop ]; then
        r="$(cd "$p" 2>/dev/null && pwd -P || echo "$p")"
        del "pick $r"; echo "dropped $r"; exit 0
    fi
    p="$(cd "$p" 2>/dev/null && pwd -P)" || { echo "setup: no such folder: ${2}" >&2; exit 2; }
    [ "$p" != "$(cd "$HOME" && pwd -P)" ] || { echo "setup: your home folder is not a project" >&2; exit 2; }
    m="$(main_of "$p")"
    [ -n "$m" ] || { echo "setup: $p is not a git checkout" >&2; exit 2; }
    [ "$m" = "$p" ] || { echo "setup: $p is not the main checkout; pick $m" >&2; exit 2; }
    put "pick $p"
    echo "picked $p"
    project_space "$p"
    ;;
skip | unskip)
    n="${2:?usage: setup.sh $cmd 2|3|4|5}"
    case "$n" in 2 | 3 | 4 | 5 | 6) ;; *) echo "setup: only items 2 to 6 can be skipped" >&2; exit 2 ;; esac
    if [ "$cmd" = skip ]; then put "skip $n"; else del "skip $n"; fi
    ;;
write)
    f="${2:?usage: setup.sh write <file> < content}"
    case "$f" in *[[:cntrl:]]*) echo "setup: a path with a control character is refused" >&2; exit 2 ;; esac
    r="$(owner_of "$f" || true)"
    [ -n "$r" ] || { echo "setup: $f is not a .qoop/ file of a picked project" >&2; exit 2; }
    f="$r/.qoop/${f##*/}"
    # The same checks on the path it really writes, not only on the one it was given.
    [ "$(owner_of "$f" || true)" = "$r" ] || { echo "setup: $f is not a .qoop/ file of a picked project" >&2; exit 2; }
    if [ -e "$f" ]; then
        h="$(PREFIX="wrote $f " awk 'index($0, ENVIRON["PREFIX"]) == 1 { print $NF }' "$state" 2>/dev/null)"
        [ -n "$h" ] && [ "$h" = "$(sha "$f")" ] &&
            tracked="$(git -C "$r" ls-files -- "$f" 2>/dev/null)" && [ -z "$tracked" ] || {
            echo "setup: $f was there before, or they changed or added it: it is theirs; edit it in place" >&2
            exit 2
        }
    fi
    mkdir -p "$r/.qoop"
    # A fresh name of its own, never a path someone could have put a link at.
    tmp="$(mktemp "$r/.qoop/.setup.XXXXXX")"
    cat > "$tmp" && mv -f "$tmp" "$f"
    without "wrote $f "
    put "wrote $f $(sha "$f")"
    echo "wrote $f"
    ;;
undo) undo "${2:-}" ;;
job)
    k="${2:?usage: setup.sh job <KEY>}"
    case "$k" in *[!A-Za-z0-9-]* | -*) echo "setup: '$k' is not a board key" >&2; exit 2 ;; esac
    qoop task show "$k" --json >/dev/null 2>&1 || { echo "setup: no board item $k" >&2; exit 2; }
    job_is "$k" '"(todo|doing)"' || { echo "setup: $k is not open: start a new job" >&2; exit 2; }
    without "job "
    put "job $k"
    ;;
skip-host | ssh-space | dirs)
    h="${2:?usage: setup.sh $cmd <host> [target]}"
    valid_host "$h" || { echo "setup: '$h' is not a host name it will use" >&2; exit 2; }
    if [ "$cmd" = skip-host ]; then put "skip-host $h"; echo "skipped $h"; exit 0; fi
    shift 2
    dir=
    if [ "$cmd" = ssh-space ]; then
        # The target's words, then `--dir <folder>` when one is given.
        n=$#
        while [ "$n" -gt 0 ]; do
            if [ "$1" = --dir ]; then
                dir="${2:-}"
                [ "$n" -eq 2 ] || { echo "setup: --dir <folder> comes last" >&2; exit 2; }
                shift 2
                break
            fi
            set -- "$@" "$1"
            shift
            n=$((n - 1))
        done
        [ -z "$dir" ] || valid_folder "$dir" ||
            { echo "setup: '$dir' is not a plain absolute folder" >&2; exit 2; }
    fi
    [ $# -gt 0 ] || set -- "$h"
    valid_target "$@" || { echo "setup: that is not an ssh target it will use" >&2; exit 2; }
    if [ "$cmd" = dirs ]; then dirs "$@"; exit $?; fi
    # Without a folder the record says `~`: the home ssh logs in to.
    base="$(name_of "$h")"
    [ -z "$dir" ] || base="$(name_of "$h-${dir##*/}")"
    n="$(space_for "$(qoop ls --json 2>/dev/null || true)" ssh-dir "$h ${dir:-~}" "$base")"
    case "$n" in =*) echo "space ${n#=} is there"; exit 0 ;; esac
    # Recorded only when the tab opened: a failed `qoop new` leaves the host open. A folder that
    # is gone leaves a shell there that says so, not a tab that closes at once.
    if [ -z "$dir" ]; then
        qoop new --space "$n" --name "$n" -- ssh "$@"
    else
        qoop new --space "$n" --name "$n" -- ssh -t "$@" "cd $dir || echo \"$dir is gone\"; exec \"\$SHELL\" -l"
    fi
    put "ssh-dir $n $h ${dir:-~}"
    echo "space $n opened"
    ;;
remote-space)
    h="${2:?usage: setup.sh remote-space <host> <folder>}"
    dir="${3:?usage: setup.sh remote-space <host> <folder>}"
    valid_host "$h" || { echo "setup: '$h' is not a host name it will use" >&2; exit 2; }
    valid_folder "$dir" || { echo "setup: '$dir' is not a plain absolute folder" >&2; exit 2; }
    qoop remote ls 2>/dev/null | awk -v h="$h" '$1 == h { f = 1 } END { exit !f }' ||
        { echo "setup: $h is not a qoop remote yet" >&2; exit 2; }
    on="$(dirname "$0")/on.sh"
    "$on" "$h" ensure-awake >/dev/null
    n="$(space_for "$("$on" "$h" ls --json 2>/dev/null || true)" remote-space "$h $dir" "$(name_of "${dir##*/}")")"
    case "$n" in =*) echo "space $h:${n#=} is there"; exit 0 ;; esac
    "$on" "$h" new --space "$n" --name "$n" --cwd "$dir" >/dev/null
    put "remote-space $n $h $dir"
    echo "space $h:$n opened"
    ;;
*)
    echo "usage: setup.sh status|seen|projects [folder]|servers|pick|drop <path>|skip|unskip <n>|skip-host <host>|ssh-space <host> [target] [--dir <folder>]|dirs <host> [target]|remote-space <host> <folder>" >&2
    exit 2
    ;;
esac

#!/bin/sh
# qass setup: the six-item checklist of a fresh install, each box ticked only from what is
# really there (a file, a config value, a login), never from anyone's word. See setup.md.
#
#   setup.sh status                the checklist, then `Next: type <n>` or `setup done`; writes nothing
#   setup.sh seen                  the checklist was shown once (a first start then never repeats it)
#   setup.sh projects [folder]     git main checkouts, newest commit first
#   setup.sh servers               the ssh hosts of ~/.ssh/config and what each is in qoop
#   setup.sh pick <path>           he picked this project (an absolute git main checkout, not $HOME)
#   setup.sh drop <path>           he took it back
#   setup.sh skip <n> | unskip <n> he skips item 2 to 6, or takes that back: `[-]`, never `[x]`
#   setup.sh skip-host <host>      he does not want this host in qoop
#   setup.sh ssh-space <host>      a space <host> with one tab running `ssh <host>`, recorded
#   setup.sh write <file>          a new `.qoop/` file of a picked project from standard input, kept
#                                  with its hash; refused when the file is his (there before setup)
#   setup.sh undo [project]        remove what setup wrote and he did not change or commit since
#   setup.sh job <KEY>             the first small job qass started for him, by its board key
#
# Writes only @QASS_DATA@/setup and the files `write` makes; only `undo` deletes, only those (and
# opens a tab in ssh-space). Never runs ssh or the network itself (`qoop task rules` reads gh's
# local login for item 4), and reads nothing under ~/.ssh but its config. Unrelated to
# `qoop setup` (Setup files).
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
                if (h !~ /^[A-Za-z0-9._][A-Za-z0-9._-]*$/) { print "!" h; continue }
                t = (hn != "" ? hn : h); if (user != "") t = user "@" t
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

# What a host is in qoop: remote, ssh space, skipped, or open.
host_state() {
    h="$1" t="$2"
    if qoop remote ls 2>/dev/null | awk -v h="$h" -v t="$t" '
        $1 == h || $2 == h || $2 == t || substr($2, length($2) - length(h)) == "@" h { ok = 1 } END { exit !ok }'; then
        echo remote
    elif has "ssh-space $h" && qoop ls --json 2>/dev/null | grep -qF "\"name\":\"$h\""; then
        echo "ssh space"
    elif has "skip-host $h"; then
        echo skipped
    else
        echo open
    fi
}

servers() {
    n=0
    hosts | while IFS="$(printf '\t')" read -r h t; do
        case "$h" in !*) echo "    ${h#!}  ignored: odd name"; continue ;; esac
        n=$((n + 1))
        printf '%2d  %s  %s  %s\n' "$n" "$h" "$t" "$(host_state "$h" "$t")"
    done
}

# "yes" when qoop counts the project the person's own repo (its origin's owner is gh's login),
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

# Take back what setup wrote: a file still as written and untracked goes; one he changed or
# committed is his, kept and named. Then the picks left with nothing, and how to drop a space.
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
    [ -n "$only" ] || grep '^ssh-space ' "$state" 2>/dev/null | sed 's/^ssh-space //' |
        while IFS= read -r h; do echo "space $h: close its tab, then qoop space rm $h"; done
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

    picked="$(grep '^pick ' "$state" 2>/dev/null | sed 's/^pick //' || true)"
    if [ -z "$picked" ]; then
        s2=no d2="$(found | grep -c . || true) found in $(searched): none picked"
    else
        s2=yes missing=
        old="$IFS"; IFS="$nl"; set -f
        for p in $picked; do
            if [ ! -s "$p/.qoop/workflow.md" ] || [ ! -s "$p/.qoop/project.md" ]; then
                s2=no missing="$missing ${p##*/}"
            fi
        done
        IFS="$old"
        set +f
        n="$(printf '%s\n' "$picked" | grep -c .)"
        if [ "$s2" = yes ]; then d2="$n set up"; else d2="$n picked, not written:$missing"; fi
    fi

    total=0 open=0
    tab="$(printf '\t')"
    hl="$(hosts | grep -v '^!' || true)"
    if [ -n "$hl" ]; then
        old="$IFS"; IFS="$nl"; set -f
        for line in $hl; do
            total=$((total + 1))
            [ "$(host_state "${line%%"$tab"*}" "${line#*"$tab"}")" = open ] && open=$((open + 1))
        done
        IFS="$old"; set +f
    fi
    if [ "$total" -eq 0 ]; then
        s3=none d3="none found in ~/.ssh/config"
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
        [ -z "$next" ] && [ "$b" = "[ ]" ] && next=$i
        case $i in
        1) l="qoop's settings" ;; 2) l="your projects" ;; 3) l="your servers" ;;
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
            echo "setup: $f was there before, or he changed or added it: it is his; edit it in place" >&2
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
skip-host | ssh-space)
    h="${2:?usage: setup.sh $cmd <host>}"
    valid_host "$h" || { echo "setup: '$h' is not a host name it will use" >&2; exit 2; }
    if [ "$cmd" = skip-host ]; then put "skip-host $h"; echo "skipped $h"; exit 0; fi
    # Recorded only when the tab opened: a failed `qoop new` leaves the host open.
    qoop new --space "$h" --name "$h" -- ssh "$h"
    put "ssh-space $h"
    ;;
*)
    echo "usage: setup.sh status|seen|projects [folder]|servers|pick|drop <path>|skip|unskip <n>|skip-host|ssh-space <host>" >&2
    exit 2
    ;;
esac

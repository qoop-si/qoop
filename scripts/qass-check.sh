#!/bin/bash
# Checks skills/qass/watch.sh against recorded snapshots: a fake `qoro` prints
# them, and the lines watch.sh prints must be exactly the expected ones.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home"

# The skill as `qoro skills install` writes it: its place names filled in. qass keeps its
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
cat > "$tmp/bin/qoro" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { cat "$tmp/ls" 2>/dev/null || echo '[]'; exit 0; }
cat "$tmp/snapshots"
EOF
chmod +x "$tmp/bin/qoro"

expected='T6 g1 claude new->idle "task" /w
T5 g1 claude working->idle "task" /w
T6 g2 claude restarted->idle "task" /w
T7 g1 claude working->blocked "say hi rm -rf " /w
daemon-gone
T5 g1 claude working->failed "task" /w
T7 g1 claude blocked->closed "task" /w'

got="$(HOME="$tmp/home" QORO_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
if [ "$got" != "$expected" ]; then
    echo "watch.sh printed:"; echo "$got"; echo "expected:"; echo "$expected"; exit 1
fi

# A restart diffs against the saved snapshot: nothing changed, nothing printed.
got="$(HOME="$tmp/home" QORO_TERMINAL=99 PATH="$tmp/bin:$PATH" bash -c "
    printf '%s\n' '[$(row 5 1 failed)]' > '$tmp/snapshots'
    '$skill_w/watch.sh'")"
[ -z "$got" ] || { echo "restart printed: $got"; exit 1; }

# Its own terminal is never reported.
printf '%s\n' "[$(row 5 1 working)]" "[$(row 5 1 idle)]" > "$tmp/snapshots"
rm -f "$tmp/home/.qass/last.json"
got="$(HOME="$tmp/home" QORO_TERMINAL=5 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
[ -z "$got" ] || { echo "own terminal printed: $got"; exit 1; }

# A hands-off mark raises its worker once more when it passes, once, and only
# while it still waits; the stream stays quiet the whole time, so the watcher
# must also live through quiet stretches (bash 3.2 ended it at the first one).
now=$(date +%s)
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" > "$tmp/snapshots"
printf '%s\n' "[$(row 5 1 idle),$(row 6 1 working)]" > "$tmp/home/.qass/last.json"
printf -- '- T5 g1 x: task; hands off until %s\n- T6 g1 y: task; hands off until %s\n- T7 g1 z: task; hands off until %s\n' \
    $((now + 3)) $((now - 1)) $((now + 999)) > "$tmp/home/.qass/goals.md"
cat > "$tmp/bin/qoro" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { echo '[]'; exit 0; }
cat "$tmp/snapshots"; exec sleep 22 2>/dev/null
EOF
got="$(HOME="$tmp/home" QORO_TERMINAL=99 PATH="$tmp/bin:$PATH" perl -e 'alarm 24; exec @ARGV' "$skill_w/watch.sh" || true)"
[ "$got" = 'T5 g1 claude hands-off->idle "task" /w' ] || { echo "hands-off printed: $got"; exit 1; }

# A tab that slept is not a tab that closed.
printf '%s\n' "[$(row 5 1 idle)]" "[$(row 6 1 working)]" > "$tmp/snapshots"
echo '[{"name":"a","sleeping":[5]}]' > "$tmp/ls"
cat > "$tmp/bin/qoro" <<EOF
#!/bin/sh
[ "\$1" = ls ] && { cat "$tmp/ls"; exit 0; }
cat "$tmp/snapshots"
EOF
rm -f "$tmp/home/.qass/last.json" "$tmp/home/.qass/goals.md"
got="$(HOME="$tmp/home" QORO_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh")"
[ "$got" = 'T5 g1 claude new->idle "task" /w
T5 g1 claude idle->sleeping "task" /w' ] || { echo "sleep printed: $got"; exit 1; }
rm -f "$tmp/ls"

# A remote's stream comes in beside this Mac's, its lines named after the remote.
printf '%s\n' "[$(row 5 1 idle)]" > "$tmp/snapshots"
printf '%s\n' "[$(row 2 3 working)]" "[$(row 2 3 idle)]" > "$tmp/remote-snapshots"
cat > "$tmp/bin/qoro" <<EOF
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
got="$(HOME="$tmp/home" QORO_TERMINAL=99 PATH="$tmp/bin:$PATH" "$skill_w/watch.sh" 2>/dev/null | sort)"
want="$(printf '%s\n' 'T5 g1 claude new->idle "task" /w' 'ubox/T2 g3 claude working->idle "task" /w' 'ubox/link-down' | sort)"
[ "$got" = "$want" ] || { echo "remote printed: $got"; exit 1; }
rm -f "$tmp/bin/ssh"

echo "qass watch: ok"

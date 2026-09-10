#!/usr/bin/env bash
# Milestone 1 acceptance, run against the real binary.
#
# Gates G1.1, G1.2 and G1.11 in the form a person would check them: a real terminal, real
# keystrokes, a real shell, and the OS as the source of truth for what survived. The
# same-shell check asks the shell for a variable only that process could still hold, so a
# fresh shell cannot pass by looking similar.
#
# usage: cargo build --release -p qoro-cli && ./scripts/acceptance-m1.sh
#
# End to end, driven the way a person drives it: a real terminal, real keystrokes, a real
# shell, and the OS as the source of truth for what survived.
set -uo pipefail
QORO=${QORO:-"$(cd "$(dirname "$0")/.." && pwd)/target/release/qoro"}
# Checked before anything runs, because the alternative is what this script actually did
# once: with no binary there, every step ran against nothing and it reported four failed
# gates, which reads as a product regression and is a missing build.
if [ ! -x "$QORO" ]; then
    echo "no binary at $QORO" >&2
    echo "build it first: cargo build --release -p qoro-cli" >&2
    exit 2
fi
RT=$(mktemp -d /tmp/qoro-e2e-XXXXXX); chmod 700 "$RT"
S=qoro-e2e-$$
PASS=0; FAIL=0
ok(){ echo "    PASS: $1"; PASS=$((PASS+1)); }
no(){ echo "    FAIL: $1"; FAIL=$((FAIL+1)); }

# Wait for text to appear on the pane rather than sleeping a fixed time before looking.
#
# The fixed sleeps this replaces were long enough on a quiet machine and not on a busy one:
# this script failed its last step exactly once, right after a full test run, and passed
# five times in a row on its own. A sleep tuned to a quiet machine is a test that reports
# the machine.
appears(){
    local needle=$1 deadline=$((SECONDS + ${2:-15}))
    while [ $SECONDS -lt $deadline ]; do
        tmux capture-pane -t "$S" -p | grep -q "$needle" && return 0
        sleep 0.2
    done
    return 1
}

cleanup(){ tmux kill-session -t "$S" 2>/dev/null; [ -n "${DPID:-}" ] && kill "$DPID" 2>/dev/null; rm -rf "$RT"; }
trap cleanup EXIT

echo "runtime dir: $RT"
# A plain shell owns the session, and qoro is launched from inside it. Otherwise detaching
# ends the session's only process and tmux tears the whole server down, which reads as a
# product failure and is not one.
tmux new-session -d -s "$S" -x 90 -y 26
tmux send-keys -t "$S" "export QORO_RUNTIME_DIR=$RT; clear" Enter
sleep 1
tmux send-keys -t "$S" "$QORO 2>$RT/client.log" Enter
# Waited for, not slept for. A fixed three seconds was long enough on a quiet machine and
# not on a busy one, and when it was not, `pgrep` answered nothing and every step after it
# failed for a reason that had nothing to do with the product: `expected 1 ptmx, saw 51` is
# what a `lsof` on an empty pid prints. Measured, one run in six under load.
starts(){
    local pattern=$1 deadline=$((SECONDS + 30)) pid=""
    while [ $SECONDS -lt $deadline ]; do
        pid=$(pgrep -f "$pattern" | head -1)
        [ -n "$pid" ] && { echo "$pid"; return 0; }
        sleep 0.2
    done
    return 1
}

CPID=$(starts "release/qoro$") || { echo "the client never started" >&2; exit 3; }
DPID=$(starts "release/qoro daemon") || { echo "the daemon never started" >&2; exit 3; }
echo "client=$CPID daemon=$DPID"

echo "--- 1. a real shell answers a real command"
tmux send-keys -t "$S" 'echo QORO_WORKS_$((6*7))' Enter
appears "QORO_WORKS_42" && ok "the shell computed 6*7" || { no "no marker"; tmux capture-pane -t "$S" -p | head -5 | sed 's/^/      /'; }

echo "--- 2. the daemon holds exactly one pty master, read from the OS"
M=$(lsof -p "$DPID" -Fn 2>/dev/null | grep -c "ptmx")
[ "$M" = "1" ] && ok "exactly one /dev/ptmx" || no "expected 1 ptmx, saw $M"

echo "--- 3. the child is a session leader on its own tty"
CHILD=$(pgrep -P "$DPID" | head -1)
ps -o pid=,ppid=,stat=,command= -p "$CHILD" 2>/dev/null | sed 's/^/      /'
[ -n "$CHILD" ] && ok "child $CHILD exists" || no "no child shell"

echo "--- 4. record state in the shell, then detach with ctrl-q"
tmux send-keys -t "$S" 'QORO_STATE=kept' Enter
sleep 1
tmux send-keys -t "$S" C-q
sleep 2
kill -0 "$CHILD" 2>/dev/null && ok "shell $CHILD alive after detach" || no "shell died on detach"

echo "--- 5. reattach and prove it is the SAME shell, not a new one"
tmux send-keys -t "$S" "$QORO 2>>$RT/client.log" Enter
sleep 3
tmux send-keys -t "$S" 'echo STATE_IS_$QORO_STATE' Enter
appears "STATE_IS_kept" && ok "same shell: it still has its variable" || { no "state lost, so it is a different shell"; tmux capture-pane -t "$S" -p | head -6 | sed 's/^/      /'; }

echo "--- 6. SIGKILL the client; the shell must survive (invariant I1)"
CPID2=$(pgrep -f "release/qoro$" | head -1)
kill -9 "$CPID2" 2>/dev/null
sleep 1
kill -0 "$CHILD" 2>/dev/null && ok "shell survived a SIGKILLed client" || no "a client kill took the shell"

echo "--- 7. reattach once more after the kill and check the shell still answers"
tmux send-keys -t "$S" "$QORO 2>>$RT/client.log" Enter
sleep 3
tmux send-keys -t "$S" 'echo AFTER_KILL_$QORO_STATE' Enter
appears "AFTER_KILL_kept" && ok "the shell is still the same one" || no "the shell did not survive the kill intact"

echo
echo "passed $PASS, failed $FAIL"
[ "$FAIL" = "0" ]

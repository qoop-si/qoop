#!/bin/sh
# Start an app, then read its AX tree twice, a few seconds apart, and keep a screenshot:
# whether the window draws, and whether its accessibility tree ever fills.
#   probe.sh <ax tool> <out dir> <name> <program> [args...]
set -u
ax=$1
out=$2
name=$3
shift 3
mkdir -p "$out"
"$@" > "$out/$name.stdout" 2> "$out/$name.stderr" &
pid=$!
sleep 10
"$ax" dump "$pid" > "$out/$name.ax1.txt" 2>&1
screencapture -x "$out/$name.png" 2> /dev/null
sleep 6
"$ax" dump "$pid" > "$out/$name.ax2.txt" 2>&1
echo "== $name: first read"
cat "$out/$name.ax1.txt"
echo "== $name: second read"
cat "$out/$name.ax2.txt"
echo "== $name: stderr (last lines)"
tail -n 20 "$out/$name.stderr"
kill "$pid" 2> /dev/null
wait "$pid" 2> /dev/null
exit 0

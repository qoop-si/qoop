#!/bin/bash
# qass on: run a qoop command on one of the remotes this Mac's qoop links to.
#
#   on.sh <remote> <qoop subcommand> [args...]      e.g. on.sh ubox context 2 --last 1
#   on.sh --master <remote> agents --watch --json   (watch.sh only: the long stream)
#
# The remote is a name from `qoop remote ls`; its ssh target comes from there, never from
# the caller. Every argument goes single-quoted to the remote shell (an argument holding a
# quote is refused), so prompt text arrives as one argument whatever it holds, in any
# script. The stream watch.sh keeps open is the shared connection the other calls ride on
# when they can; no call here ever leaves a connection running in the background.
set -euo pipefail

master=no
if [ "${1:-}" = --master ]; then
    # Always its own connection: riding on another process's would drop the stream the
    # moment that process ends. If the socket is taken it just runs without sharing.
    master=yes
    shift
fi
name="${1:?usage: on.sh <remote> <qoop subcommand> [args...]}"
shift
[ "$#" -gt 0 ] || { echo "usage: on.sh <remote> <qoop subcommand> [args...]" >&2; exit 2; }
target="$(qoop remote ls | awk -v n="$name" '$1 == n { print $2; exit }')"
[[ "$target" =~ ^[A-Za-z0-9][A-Za-z0-9._@-]*$ ]] || { echo "on.sh: no usable remote $name" >&2; exit 2; }
cmd='"$HOME/.local/bin/qoop"'
for a in "$@"; do
    case "$a" in *\'*) echo "on.sh: an argument holds a quote" >&2; exit 2 ;; esac
    cmd="$cmd '$a'"
done
# A write to an agent there is noted where qass counts its writes, as `qoop prompt` notes one
# here. Before the exec below, so it counts the attempt; a note that fails stops nothing.
case "$1" in prompt | send)
    from="${QOOP_TERMINAL:-${QORO_TERMINAL:-}}"
    if [ -d "@QASS_DATA@" ] && [ -n "$from" ]; then
        { printf '%s %s %s/T%s\n' "$(date +%s)" "$from" "$name" "${2:-}" >> "@QASS_DATA@/prompted.log"; } 2>/dev/null || true
    fi ;;
esac
# Short and private: a socket path past 104 bytes is refused on macOS.
sock="/tmp/qoop-$(id -u)-ssh"
mkdir -p "$sock"
chmod 700 "$sock"
exec ssh -o BatchMode=yes -o ConnectTimeout=3 -o ServerAliveInterval=15 \
    -o ServerAliveCountMax=3 -o ControlMaster="$master" -o "ControlPath=$sock/%C" \
    -- "$target" "$cmd"

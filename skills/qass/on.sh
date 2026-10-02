#!/bin/bash
# qass on: run a qoro command on one of the remotes this Mac's qoro links to.
#
#   on.sh <remote> <qoro subcommand> [args...]      e.g. on.sh ubox context 2 --last 1
#   on.sh --master <remote> agents --watch --json   (watch.sh only: the long stream)
#
# The remote is a name from `qoro remote ls`; its ssh target comes from there, never from
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
name="${1:?usage: on.sh <remote> <qoro subcommand> [args...]}"
shift
[ "$#" -gt 0 ] || { echo "usage: on.sh <remote> <qoro subcommand> [args...]" >&2; exit 2; }
target="$(qoro remote ls | awk -v n="$name" '$1 == n { print $2; exit }')"
[[ "$target" =~ ^[A-Za-z0-9][A-Za-z0-9._@-]*$ ]] || { echo "on.sh: no usable remote $name" >&2; exit 2; }
cmd='"$HOME/.local/bin/qoro"'
for a in "$@"; do
    case "$a" in *\'*) echo "on.sh: an argument holds a quote" >&2; exit 2 ;; esac
    cmd="$cmd '$a'"
done
# Short and private: a socket path past 104 bytes is refused on macOS.
sock="/tmp/qoro-$(id -u)-ssh"
mkdir -p "$sock"
chmod 700 "$sock"
exec ssh -o BatchMode=yes -o ConnectTimeout=3 -o ServerAliveInterval=15 \
    -o ServerAliveCountMax=3 -o ControlMaster="$master" -o "ControlPath=$sock/%C" \
    -- "$target" "$cmd"

#!/usr/bin/env bash
# ADR-017's replay rule, run as one command.
#
# A projection is a fold over the log and has nothing else as input. This runs the event
# crate's tests, which include replaying a log twice under a frozen clock and comparing the
# result byte for byte, and then replays a real log built by the daemon.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "--- event store and projection tests"
cargo test -q -p qoro-events

echo "--- replay determinism against a log the daemon wrote"
cargo test -q -p qoro-runtime --test gates_m4

echo "replay check passed"

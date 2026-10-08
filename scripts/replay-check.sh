#!/usr/bin/env bash
# ADR-017's replay rule, plus the ledger the daemon actually keeps, as one command.
#
# A projection is a fold over the log and has nothing else as input, and the event crate's
# tests replay a log twice under a frozen clock and compare the result byte for byte. That
# is the half that still has code behind it.
#
# The daemon used to write such a log and `gates_m4` used to replay one it built. It does
# neither now: the daemon keeps the idempotency ledger in its metadata store and writes no
# events, so the gates in that file cover the ledger and the terminal rather than the log.
# What is no longer covered anywhere is in the module comment of `gates_m4.rs`: a log that
# round-tripped through SQLite, the bounded observation buffer, the gap that records a
# loss, and the waiter that re-read. All of it went with `eventlog.rs`. See ADR-005.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "--- event store and projection tests, including replay determinism"
cargo test -q -p qoop-events

echo "--- the ledger, as the daemon uses it"
cargo test -q -p qoop-store
cargo test -q -p qoop-runtime --test gates_m4

echo "--- the ledger under the packaged binary, across a restart"
cargo test -q -p qoop-cli --test ledger

echo "event store and ledger checks passed"

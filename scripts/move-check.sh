#!/usr/bin/env bash
# ADR-020's invariant, run as one command: at every point in a move, and after a failure at
# any point, exactly one machine answers "I own this workspace".
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -q -p qoro-move
echo "move check passed"

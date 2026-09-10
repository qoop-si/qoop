#!/usr/bin/env bash
# ADR-016's rules, run as one command.
#
# A bundle is a log slice plus the projections it produces, and the two are checked against
# each other. A bundle whose projections do not match its own log has been edited.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -q -p qoro-support
echo "bundle check passed"

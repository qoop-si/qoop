#!/usr/bin/env bash
# ADR-018's rules, run as one command: a plugin is a process, its capabilities are per
# object, and there is no method that writes to a terminal.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test -q -p qoro-plugin
echo "plugin check passed"

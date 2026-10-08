#!/usr/bin/env bash
# ADR-021: the product runs on macOS and Linux, and says so.
#
# Two things are checked. The platform-specific code compiles for the platform it is on,
# which is what `cargo build` does below, and nothing user-facing claims Windows.
#
# The search is over what a person reads, not over the source: `.windows(2)` is a slice
# method and matching it would make this a check nobody can keep passing.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "--- the pty layer builds for this platform"
cargo build -q -p qoop-pty

echo "--- nothing user-facing claims Windows"
claims=$(grep -rniE 'windows' README.md docs/*.md crates/qoop-cli/src 2>/dev/null \
  | grep -viE 'not supported|under wsl|adr-021|windows has|windows analogue' || true)
if [ -n "$claims" ]; then
  echo "$claims" >&2
  echo "found a Windows claim; see ADR-021" >&2
  exit 1
fi
echo "platform check passed"

#!/bin/sh
# qass gate: the part of the assistant that does not trust the model.
#
#   gate.sh tool    PreToolUse hook for the qass session, matcher "" (every tool)
#   gate.sh owner   UserPromptSubmit hook: records that the owner spoke
#
# `tool` is a whitelist and fails closed. Claude Code lets a call through on any
# exit but 2, a timeout included, so every error, every unknown tool and every
# unknown command exits 2, and each child call carries its own short timeout.
# The owner's user allow list (git, gh, cp, ...) merges into the qass session;
# this gate is what keeps it from mattering.
#
# State lives in ~/.qass: `policy` (owner only: `mode propose|auto`, `grant
# <repo root> <verb>`) and `gate.jsonl` (this gate and the owner hook only: one
# row per write qass made, one per owner message). qass itself can edit only
# ~/.qass/goals.md.

mode="${1:-}"
here="$(cd "$(dirname "$0")" && pwd -P)"
if [ "$mode" = owner ]; then
    python3 "$here/gate.py" owner || true
    exit 0
fi
python3 "$here/gate.py" tool && exit 0
exit 2

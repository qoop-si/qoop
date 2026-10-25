# ADR-023: Usage counts

Status: accepted
Date: 2026-10-26
Amends: ADR-016 ("Counters are local", "Privacy is a property of the default")
Evidence: `crates/qoop-runtime/src/telemetry.rs`, `crates/qoop-store/src/telemetry.rs`,
`crates/qoop-runtime/src/config.rs` (`telemetry_from`, `set_telemetry`) and their tests
Reproduce: `cargo test -p qoop-runtime --lib telemetry`, then `qoop telemetry show`

## Context

ADR-016 decided that counters stay on the machine and that there is no telemetry to turn
off, because "a setting that defaults to on and can be turned off is a decision made by
whoever did not read the setting". That held while nobody needed to know how qoop is used.
It no longer does: which agents people run, how many at once, how many terminals a space
holds, how often an agent waits on its owner and for how long, which terminal apps and
versions are out there. None of it can be guessed from the releases page, and every one of
those numbers decides what gets built next.

## Decision

A release build sends one small JSON per finished UTC day to `https://qoop.si/api/telemetry`.
It is on by default. That is the owner's decision, made knowing ADR-016's argument; what
makes it acceptable is the shape of what is sent and how easy it is to see and stop:

- **Numbers and fixed names only.** Counts and gauges (agents open and working, agents by
  kind, terminals open and per space, spaces, questions created, answered, dismissed and closed
  by qass, the owner's wait, schedule runs) and values from fixed lists (version, OS, CPU,
  terminal app, multiplexer, whether a phone was connected). Never a path, a name, a command, a
  prompt, an agent's output, a host, a user, an email, an IP, a location, and never a hash or a
  length of any of these: ADR-016's point that a hash of a command line is a fingerprint of it
  stands. Every key comes from one table in the code; a received string (a client's terminal
  app) is only ever compared with that table.
- **Shown before anything is counted.** One line, held on screen for five seconds, in a real
  terminal, outside a qoop terminal, the first time a person starts qoop. Until then nothing is
  counted. There is no consent dialog and no banner on any web page.
- **Seen exactly.** `qoop telemetry show` prints every day not yet sent, as it would be sent.
- **Stopped at once.** `qoop telemetry off`, the settings row, `telemetry = false` anywhere in
  `config.toml`, or `DO_NOT_TRACK` / `QOOP_TELEMETRY=0` in the environment. Off stops counting
  and any send in flight now, and deletes every count, the random id and the notice mark;
  turning it on again shows the line again and starts a new id.
- **Pseudonymous, not anonymous.** The id is random, tied to no account, key or hardware. The
  hub stores no IP and no location; Cloudflare in front of it sees addresses as it does for
  every request.

Refusing `on` from an agent's terminal and from the phone is a guard against an agent casually
undoing the owner's off, not a security boundary: an agent runs as the same user.

The endpoint is unauthenticated, so the counts are estimates: the admin charts count distinct
installs or show medians and the 90th percentile across installs, never sums, so a few forged
rows cannot move them. A secret embedded in an open client would only pretend otherwise.

A daemon on an SSH host reached from a local interface never shows the line, so it never
counts; a person running the interface on that host sees it there.

## Consequences

- A machine that never paired a phone now talks to qoop.si a few times a day; a release build
  already did for the update check (ADR-019).
- Counting starts on the day the notice was seen; a headless daemon whose host never ran the
  interface counts nothing.
- Maxima are sampled every five minutes: an agent that lives less than one sample may not show
  in them, and the charts say so.

## Rollback

Make the sender's release gate false: the counting, the store tables and the command stay and
nothing leaves the machine, which is ADR-016 again.

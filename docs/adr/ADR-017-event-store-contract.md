# ADR-017: The event store contract

Status: accepted
Date: 2026-09-10
Amended: 2026-09-29, the daemon stopped writing the log. See "Who writes this" below.
Evidence: `crates/qoop-events/src/lib.rs` and its tests
Reproduce: `./scripts/replay-check.sh`

## Who writes this

**Nobody, today.** The crate, its store, its projections and their tests are all here and
all tested, and `scripts/replay-check.sh` runs them. The daemon does not open
`events.db`: as of 2026-09-29 it writes no `Command`, no `Outcome`, no `Observation` and no
`Gap`, and it reads the log for nothing.

What it does keep is the one part of the log it ever read, the idempotency ledger, and that
now lives in `qoro.db` as an `applied` table. ADR-005 covers where it went and what was
lost. The rest of this ADR describes a contract nothing currently depends on, kept intact
for whoever writes the first reader.

## Context

Milestone 4 is blocked on this ADR because everything after it reads from the log: the
mobile inbox, the support bundle, workspace resume, and the collaboration surface all
project from the same events. A log whose contract is discovered by its first three readers
is a log with three different contracts.

ADR-005 already settled where events live (SQLite with WAL, batched, on the blocking pool)
and what happens when the disk cannot keep up (terminals never block, commands are
acknowledged only when durable, observations are lossy and the loss is recorded). What it
did not settle is what an event *is*, which is what this decides.

Two failures are worth naming before the decision, because the decision is shaped around
avoiding them.

A log of everything is a log nobody can replay. Terminal output is megabytes a second; a
design that treats "the screen changed" as an event has a log that is mostly noise and a
replay that takes as long as the original session.

And a log of summaries is a log that cannot answer the question it was built for. If the
event is "the agent became blocked" with no record of what was seen, a support bundle can
say what we concluded and not why, which is the half that matters when the conclusion was
wrong.

## Decision

### An event is a decision or an outcome, never a sample

Three kinds, and the kind decides the durability rule:

- **Command**, what somebody asked for. Durable before the command is acknowledged
  (ADR-005 rule 2). Carries the client's idempotency key.
- **Outcome**, what happened as a result. Durable, and always paired with its command by
  the same id, so a restart can find an intent with no outcome and reconcile it.
- **Observation**, something we noticed. Lossy under pressure, and the loss is recorded as
  a gap (ADR-005 rule 3).

Terminal output is not an event, not even its range. It is history, it has its own
segmented store, and the history records how far output has reached; a range per parse slice
grew the log with how chatty a build was. Observations are written in one batch a second,
and the log keeps 30 days: long enough for forensics on a reap or a crash, and the
idempotency ledger only has to outlive a client's retry.

### Every event carries what it was derived from

An observation records the evidence, not just the conclusion: the source, the confidence,
the generation it was sampled under, and for a screen observation the matched text. This is
what makes gate G4.7's "command to state transition to event to notification to response"
reconstructable, and it is what makes a wrong conclusion diagnosable rather than merely
visible.

Evidence is subject to the same secret rules as everything else (ADR-015): a matched line
is recorded, a matched line that a secret rule redacted is recorded redacted, and the
redaction is recorded as having happened.

### The log is the truth and projections are caches

A projection is a fold over the log with no other input. That is a strong rule and it is
the one G4.1 checks: replay under a frozen clock, twice, and compare byte for byte. A
projection that reads the wall clock, a random number, or the filesystem fails that check
rather than passing by luck, which is the whole point of freezing the clock rather than
merely replaying.

Consequence: a projection may be deleted and rebuilt at any time, and a schema change to a
projection is a rebuild rather than a migration.

### Ordering is per stream, not global

Events are ordered within a terminal, within a workspace, and within a command's own
lifetime. There is no global order, because producing one means a single writer and a
single writer is the thing that makes a busy terminal slow down a quiet one.

A reader that needs to interleave two streams orders by the recorded timestamp and accepts
that the result is approximate across streams. This is stated here so that no reader has to
discover it, and so that no projection is written that depends on a total order it will not
get.

### Gaps are events

A gap is a first class event with a count and a time range, appended on the next successful
write after the pressure that caused it. A reader that sees a gap knows that the state it
has folded may be behind the world and that the fix is to re-read state rather than to wait
for more events. G4.5 turns that from a convention into a requirement: a waiter must resolve
across an injected buffer overflow, and the only way it can is if the gap obliges it to
re-read.

### The window a crash can lose is stated, not discovered

Commands and outcomes are durable before acknowledgement, so a crash loses no acknowledged
command. Observations are batched, and the batch window is the loss. On every start after an
unclean shutdown a gap event covering the interval between the last durable event and
startup is appended, because the daemon cannot know what it missed and a reader that assumes
continuity across a restart would be wrong exactly when it matters most.

## Consequences

- Every projection is rebuildable, and rebuilding is the migration story.
- A support bundle is a log slice plus the projections it produces, and the two can be
  checked against each other.
- No reader may depend on a global order, and this is checkable: a projection that folds
  two streams and produces a different answer on a reordered replay fails G4.1.
- Terminal output stays out of the log, so the log's size is a function of what people and
  agents did rather than of how chatty a build was.

## Rollback

The three kinds and the per-stream ordering are the load-bearing parts. If a global order
turns out to be required, it is a new stream that every writer appends to, which is a
performance decision made once rather than a contract change: existing readers keep working
because per-stream ordering is a weaker promise than a total one.

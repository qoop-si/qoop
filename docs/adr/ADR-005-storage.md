# ADR-005: Local storage

Status: accepted
Date: 2026-09-09

## Context
A comparable multiplexer has no database. It writes two JSON files per session with
temp file plus rename, and its only event log is a 512 entry in-memory ring that is lost
on restart. That
is adequate for layout metadata and inadequate for a durable event log, which the mobile
attention client and any audit or replay use case require.

## Decision
- SQLite with WAL for durable metadata: workspaces, terminals, setups, machines, agents,
  events, requests, messages and projections.
- Segmented append-only files for terminal history and snapshots. Terminal output never
  becomes a large SQLite row.
- All database work runs on the blocking pool with a deadline. Never on the reactor.
- Event appends are batched. A batch is one transaction.
- Structural writes that are not events keep the temp file plus rename pattern where a
  plain file is the right shape, including manual symlink resolution so a dotfile
  managed path is not replaced by a regular file.

### The overload triangle, resolved explicitly

Bounded queues, lossless semantic events and terminal independence from storage cannot
all hold when the disk stops accepting writes. The plan previously promised all three.
The resolution, stated rather than discovered in production:

1. **Terminals never block on storage.** A PTY read, a render and a keystroke never wait
   for a database write. This is absolute, and it applies to the PTY read loop most of
   all: that loop never awaits any downstream capacity, because an exiting child cannot
   complete until its tty output is drained, so a terminal task that stops draining while
   awaiting a full queue makes `exit` hang in the kernel. Every downstream send from a
   terminal task is a coalescing replace or a drop-and-count, never an await.

   Terminal input is deliberately not journalled. A restart therefore cannot reconstruct
   what a user typed, and input is classified as non-mutating for degraded-mode purposes,
   so a full disk never stops anyone typing.
2. **A command is acknowledged only after its event is durable.** A client that asks to
   create a workspace is told it succeeded only once the log says so. This is what makes
   idempotent retry meaningful.
3. **Observations, which are things we noticed rather than things we were asked to do,
   are lossy under pressure and the loss is recorded.** When the event buffer is full,
   observations are dropped and gap state is accumulated in memory as a count plus a time
   range, which is O(1) and needs no buffer slot. It is appended as a single `gap` event
   on the next successful write. Appending the gap through the buffer that is by
   definition full would lose the gap along with the observations it describes, which is
   the failure the whole mechanism exists to prevent.

   Gap state that is still unflushed is lost if the process dies, and that is precisely
   when the process is most likely to die, because the accumulator is largest while writes
   are failing. So: **on every start after an unclean shutdown, a gap event is appended
   covering the interval between the last durable event and startup.** The daemon cannot
   prove nothing was lost across an unclean stop, and claiming continuity would be the
   silent inconsistency this whole section exists to prevent.

   A subscription is therefore defined as "state snapshot at cursor C, then events after
   C", and a `gap` obliges the subscriber to re-read state rather than merely noting a
   hole. This matters because a waiter blocked on `agent.blocked` must still resolve
   across a drop; a pure event subscription that misses its trigger would be less reliable
   than the polling loop it replaces.
4. **Degraded storage mode** is entered when writes fail or exceed their deadline
   repeatedly. In degraded mode the daemon keeps every terminal running, refuses
   mutating commands with a typed retryable error, continues to serve reads from memory,
   and surfaces the condition in status output. It leaves degraded mode when a write
   succeeds, and appends a gap event covering the degraded window.

The one thing that never happens is a silent inconsistency.

### Crash windows

Every externally visible **lifecycle** side effect is preceded by an intent record and
followed by an outcome record. Lifecycle means spawning a process, tearing one down,
writing a file, or sending an outbound message. It explicitly does not mean delivering a
keystroke to a child, which is irreversible but is never journalled, per rule 1. On restart, an intent with no outcome is reconciled rather than assumed:
a spawn intent with no outcome causes the daemon to look for the process, a teardown
intent with no outcome causes teardown to re-run from its idempotent step. The window is
therefore never "did this happen", it is always "check and converge".

## Consequences
- A durable, replayable event log exists from milestone 4, which is a capability
  other multiplexers do not have at all.
- Build cost: SQLite is compiled in via a bundled feature so there is no system
  dependency.

## Rollback
`EventStore` is a trait. A different backing store can replace SQLite without touching
domain or runtime code.

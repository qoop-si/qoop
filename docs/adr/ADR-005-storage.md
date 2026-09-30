# ADR-005: Local storage

Status: accepted
Date: 2026-09-09
Amended: 2026-09-29, the event log became unwired. See "The log is not written" below.

## Context
A comparable multiplexer has no database. It writes two JSON files per session with
temp file plus rename, and its only event log is a 512 entry in-memory ring that is lost
on restart. That
is adequate for layout metadata and inadequate for a durable event log, which the mobile
attention client and any audit or replay use case require.

## Decision
- SQLite with WAL for durable metadata: workspaces, terminals, layouts, split trees,
  sleeping spaces, and the idempotency ledger.
- Segmented append-only files for terminal history and snapshots. Terminal output never
  becomes a large SQLite row.
- All database work runs on the blocking pool with a deadline. Never on the reactor.
- Structural writes keep the temp file plus rename pattern where a plain file is the right
  shape, including manual symlink resolution so a dotfile managed path is not replaced by a
  regular file.
- A new table is created unconditionally, never behind a version bump. A bump makes every
  older binary refuse the file, and the error tells the person to move it aside, which is
  deleting their workspaces. `split_tree` and `applied` each have a test that says so.

## The ledger is what the log was for

A mutating command writes down its answer before the client is told, keyed by the client's
idempotency key. A retry of that key returns the recorded answer instead of doing the work
again. This is the whole of what the daemon read from the event log, and it is what
`applied` in `qoro.db` is for. The first answer stands: a second write is ignored, because a
command that answers twice has already done the work twice by the time the second write
lands.

The table is trimmed once a start, on the same 30 day window the log used. A retry is
seconds behind the answer it missed, so the window is for forensics on a reap, not for the
retry.

## The log is not written

The event log described by ADR-017 is implemented in `qoro-events`, and its tests still
prove that a projection is a pure fold over a log. **The daemon does not write it.** Before
this change a running daemon had written hundreds of observations and commands and read
none of them: the only query was the ledger, and no crate in the workspace opened the log
besides it. The CLI, TUI, bridge, hub, remote transport and mobile path never opened it at
all, and they still do not.

Everything the log was going to feed reads somewhere else. The agent state every surface
sees comes from `AgentRegistry` and the store, written by separate paths. `FocusEvents` is
answered from daemon memory and says so at its own arm. A sleeping space's promise, that
tabs, scrollback, splits and agent conversation ids are kept, is carried by the store rows
and the history files, not by the log.

What was lost, and what it was not:

- **A `Command` intent with no `Outcome`.** Never reconciled. `unreconciled()` had no
  production caller, and what reconciles a restart is `restore_report`, which reads
  `store.open_terminals` per space and never consulted the log. Killing the daemon between
  a create's intent and its outcome already opened a duplicate tab on the retry, because
  only an `Outcome` ever wrote the ledger row. The intent changed nothing at any point in
  that trace.
- **Replay, projections, the gap that tells a reader to re-read, lossy observations.** Real
  once, all unwired. `qoro-events` still holds the fold and the `Gap` body, and its tests
  still replay a log it built in memory. What is gone with it: the bounded buffer that
  dropped under pressure, the counting of what was dropped, the code that appended the
  `gap`, and the waiter that re-read rather than folded forward. None of that is covered
  anywhere now, and the first reader of the log decides whether any of it is written again.
- **A support bundle from a running daemon** (ADR-016), which is a log slice. It could not
  be produced from one before this change either: `qoro-support` had no callers at all. The
  crate and its tests remain.

The `Command` intent is the one that needs a second reader to believe it was load-bearing,
so it is worth being exact: `EventStore::append` wrote the ledger row only inside its
`Outcome` branch, so the lookup was structurally unable to see an intent.

## A create and its answer are one write

A create writes the terminal row, its layout place and its ledger row in one transaction,
before the shell is started. Three holes close with it. A ledger write can no longer fail
after the shell is running, so a client is never told `Failed` about a live tab. A daemon
killed between the write and the answer comes back with the tab restored and the answer
that names it, so the retry is a replay. And two attempts at one create, racing on two
connections, cannot both open a tab: the lookup runs under the space's lock, after the
first attempt has started its shell or undone itself, and the insert fails on a key already
answered. A spawn that fails closes the row and takes the answer back in one transaction,
so a retry tries again. A write past its deadline still commits, so a create that finds its
own answer there carries on, and one that finds nothing undoes the write in case it lands
later. That undo is best effort: the store's lock is not FIFO, so a write stalled past its
deadline can still land after the undo and leave a tab for the next start. Closing it needs
a cancel flag checked before the commit, and it has not been worth one. The name a tab is
born with is in the same transaction. Gate G4.4 breaks the ledger under a running daemon
and checks that a create is refused with no tab left behind.

## What lives in the runtime directory, and what that costs

`qoro.db` and `history/` are both opened in the runtime directory. On macOS
`XDG_RUNTIME_DIR` is unset, so `runtime_base()` (`crates/qoro-runtime/src/paths.rs:71-86`)
falls back to `std::env::temp_dir()` and both land in `$TMPDIR/qoro-<uid>`, which macOS
clears: measured on a machine that had been up three days, none of the files at depth 2 of
that `TMPDIR` predated its boot, while the sibling cache directory beside it had the large
majority of its files older. The measurement is scoped to that depth and that machine, and
it is evidence of a policy rather than a measurement of how long anything survives.

So on macOS a sleeping space does not survive a restart today, whatever the log does. The
XDG spec is right that a runtime directory must not survive a reboot; it is right about
sockets and wrong about databases. Moving the stores to `$XDG_DATA_HOME` with a migration
is a separate change, and it is the one that makes any durability claim worth making.

## Consequences
- The daemon keeps one SQLite file instead of two, one WAL, and one set of schema,
  migration and busy-timeout handling.
- A durable, replayable event log exists in `qoro-events` and is exercised by its own tests.
  It is not what this daemon runs on.
- Build cost: SQLite is compiled in via a bundled feature so there is no system
  dependency.

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
2. **A command is acknowledged only after its answer is durable.** A client that asks for a
   terminal is told it succeeded only once the ledger row is written, which is what makes
   its retry a replay rather than a second tab. The ledger covers `CreateTerminal` and
   `CloseTerminal`, which are the two commands whose repetition is visible; a workspace
   command repeated today creates a second workspace.
3. **Observations were lossy under pressure, and the loss was recorded.** This is written
   in the past tense because the mechanism is gone with the log it belonged to, and the
   reason it is here at all is that the design it settles is the one the next writer has to
   settle again.

   When the event buffer was full, observations were dropped and the gap state accumulated
   in memory as a count plus a time range, which is O(1) and needs no buffer slot. It was
   appended as a single `gap` event on the next successful write. Appending the gap through
   the buffer that is by definition full would have lost the gap along with the
   observations it described, which is the failure the whole mechanism exists to prevent.

   Gap state still unflushed was lost if the process died, and that is precisely when the
   process is most likely to die, because the accumulator is largest while writes are
   failing. So every start after an unclean shutdown appended a gap covering the interval
   between the last durable event and startup. The daemon could not prove nothing was lost
   across an unclean stop, and claiming continuity would have been the silent
   inconsistency this whole section exists to prevent.

   A subscription was therefore defined as "state snapshot at cursor C, then events after
   C", and a `gap` obliged the subscriber to re-read state rather than merely note a hole.
   A waiter blocked on `agent.blocked` had to resolve across a drop, because a pure event
   subscription that missed its trigger would be less reliable than the polling loop it
   replaces.

   The buffer, the counter, the code that appended the gap and the waiter were all in
   `eventlog.rs`, and none of them had a production caller. Rule 1 still governs the
   terminal's path and its implementation is untouched. If the log is written again, this
   is the rule it needs, and it is the reason a projection is a fold rather than a
   subscription.
4. **Degraded storage mode** is entered when writes fail or exceed their deadline
   repeatedly. In degraded mode the daemon keeps every terminal running, refuses
   mutating commands with a typed retryable error, continues to serve reads from memory,
   and surfaces the condition in status output. It leaves degraded mode when a write
   succeeds.

   Not built. What exists is per command: a create whose write fails is refused in words
   with no tab left behind, a ledger that cannot be read is treated as "not applied", and a
   close proceeds whether or not its answer can be written, because a refused close leaves
   a shell the client cannot get rid of. No mode, no status line.

The one thing that never happens is a silent inconsistency.

### Crash windows

For `CreateTerminal` the store row is the reconciliation: the row and its ledger row are
written together before the spawn, both undone in one transaction when it fails, and
`restore_report` reopens every open row. The ledger row is what makes a retry a replay
rather than a second tab.

An earlier version of this ADR claimed that an intent with no outcome caused the daemon to
look for the process, and that a teardown intent re-ran teardown from its idempotent step.
No such code existed. `unreconciled()` had no production caller and the store row was what
restore read, which the next sentence of that version already said.

## Rollback

`qoro.db` gains a table and `SCHEMA_VERSION` does not move, so a build that predates this
one still opens the file and ignores the table. That older build reads its ledger from
`events.db`, which this one no longer writes, so a retry that spans a downgrade opens a
second tab; nothing else is lost. `events.db` is left on disk unread. `qoro-store` has no
replacement, and its schema is a ladder rather than a version check.
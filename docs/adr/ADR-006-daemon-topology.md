# ADR-006: Daemon topology

Status: accepted
Date: 2026-09-09

## Context
A comparable multiplexer runs one daemon per named session, with the default session
acting as a de facto per-machine daemon. The result is a fully parallel socket and data
directory namespace with duplicated path construction logic, which reads as retrofitted
rather than designed.

## Options
1. One daemon per machine per user.
2. One daemon per workspace.
3. One daemon per named session, with a default session.

## Decision
One daemon per machine per user. The socket lives at a single deterministic path derived
from the user id. There is no optional session namespace in v1.

Isolation concerns are addressed inside the daemon by ownership boundaries
(one `WorkspaceRuntime` per workspace, one `TerminalRuntime` per terminal), not by
running more processes.

## What one daemon does not give you

Ownership boundaries inside one process are a correctness and blast-radius mechanism.
They are not a security boundary. Every process running as this user, including every
agent the daemon launched, can connect to the daemon socket and drive any workspace.

Capability checks in the collaboration layer prevent mistakes and make actions auditable.
They do not prevent a determined agent from doing anything the user could do. Real
isolation needs a different uid, a container or a VM, which is the `ExecutionBackend`
axis and is out of scope for v1. See ADR-012. No document or marketing claim may imply
otherwise.

## Consequences
- One process registry, one event stream, one machine identity, one listener.
- A daemon crash affects every workspace on the machine. This is accepted and is the
  reason crash containment and durable state are milestone gates rather than later work.
- If isolation evidence later demands per-workspace processes, the change is contained
  because runtimes already own their state and communicate only over channels.

## Amendment, 2026-09-17: named sessions, one daemon each

The decision above stands for what it decided, and is narrowed here by evidence the
original had not seen.

What the decision got right is still right: workspaces are not a reason to run more
processes, and the isolation section below is unchanged, because more daemons of the same
uid are not a security boundary either.

What it did not have is a product requirement it turned out to block. A person running
several projects at once wants to stop one of them: end its processes, get the machine
back, and open it again later with the same tabs. Stopping is the operation that decides
the topology. Inside one daemon, stopping one session means tearing down a subset of its
terminals while it keeps serving, which is a partial teardown with no natural boundary and
a new class of half-stopped state. As one process per session it is the process ending, and
the operating system does the rest, which is a mechanism that already works and cannot be
half done.

So: a session is a named directory under the runtime directory, holding one daemon's
socket, database and history, and the unnamed default is one of them rather than a special
case. (Amended 2026-09-30: the default session's database and history now live in the data
directory; ADR-005.) `qoro ls` lists the spaces of the one daemon there is per machine, and
ADR-005 says what its directory holds and where that directory is.

Amended 2026-09-29. There is one daemon per machine now and `qoro --session` takes only
`default`; spaces are the only grouping. `qoro stop <name>` still takes a name and still
stops a daemon of that name left running from before this decision, because nothing else
reaches it. `qoro ls` lists spaces rather than sessions. ADR-005 covers the stores.

The path duplication the Context warns about is the real risk and is answered directly:
every session path is built by `paths::session_dir`, and there is no second place that
joins a session name onto a directory. A name is refused rather than sanitised if it could
be anything other than one directory under that one.

The consequences below are narrowed accordingly: a daemon crash affects every workspace in
its session rather than every workspace on the machine, which is strictly better, and one
process registry, one event stream and one listener are now per session.

## Amendment, 2026-09-28: one daemon again, spaces that sleep

The first amendment's reason was a real requirement: stop one project, get the machine
back, open it again later as it was. What it chose to answer it with, a daemon per named
session, answered it at the wrong grain. A person's projects are spaces, not sessions, and
in practice a machine ended up with one session a person used and a dozen made by test
runs; a second level of grouping above spaces was one more thing to name and pick for no
use anyone had.

So the original decision stands again: one daemon per machine per user. The runtime
directory is the one the default session had, so nothing moves. `--session default` is
still accepted, because an older qoro on the far side of ssh sends it; any other name, as
the flag or as `QORO_SESSION`, is refused with where that session's files are. They are
left on disk, never deleted and never imported.

The requirement is met per space instead. A space sleeps: every tab in it ends, its rows
stay open, and waking it reopens the same rows under the same numbers, with each agent
resumed into its own conversation. And the daemon leaves by itself once nothing is awake
and nobody is attached, so a machine with everything asleep runs nothing at all.

The first amendment's argument against doing this inside one daemon was that stopping a
subset of terminals is "a new class of half-stopped state". It is answered, not waved away:

- Asleep is durable and read before anything else: a table in the store, written in the
  same transaction as each sleeping tab's agent and conversation id, and mirrored in memory
  for the one decision that cannot wait on the store, whether a terminal ending now keeps
  its row.
- Half asleep does not exist. A space sleeps whole or not at all: every tab is vetted, a
  tab that could not come back as it is refuses the whole space, and a tab made in a
  sleeping space wakes it first, at the one place every tab is made.
- Every change of state is serialized by the space's lock: sleep, wake, a tab made or
  closed in it, the space closing. The connection's read loop never waits on that lock.
- Crash ordering is consistent both ways: a crash after the mark is written and before the
  tabs end leaves a space asleep with its rows open, which the next start does not open and
  a wake does; a crash in a wake leaves the mark, and the rows, as they were.
- Writes that outlive their caller are fenced where they run: the store refuses to change a
  sleeping tab's agent columns, however late the write arrives.

What is given up: a daemon crash affects every space on the machine again, not every space
of one session. Crash containment and durable state stay milestone gates for that reason.

## Rollback
Splitting into multiple daemons later requires a registry and a router. The ownership
boundaries chosen now are exactly what would make that possible.

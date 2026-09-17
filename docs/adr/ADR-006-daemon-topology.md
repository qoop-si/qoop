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
socket, database, history and event log, and the unnamed default is one of them rather than
a special case. `qoro --session <name>` picks one, `qoro ls` lists them, `qoro stop [name]`
ends one.

The path duplication the Context warns about is the real risk and is answered directly:
every session path is built by `paths::session_dir`, and there is no second place that
joins a session name onto a directory. A name is refused rather than sanitised if it could
be anything other than one directory under that one.

The consequences below are narrowed accordingly: a daemon crash affects every workspace in
its session rather than every workspace on the machine, which is strictly better, and one
process registry, one event stream and one listener are now per session.

## Rollback
Splitting into multiple daemons later requires a registry and a router. The ownership
boundaries chosen now are exactly what would make that possible.

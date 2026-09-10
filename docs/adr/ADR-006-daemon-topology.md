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

## Rollback
Splitting into multiple daemons later requires a registry and a router. The ownership
boundaries chosen now are exactly what would make that possible.

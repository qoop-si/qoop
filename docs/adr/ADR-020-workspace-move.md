# ADR-020: Moving a workspace between machines

Status: accepted
Date: 2026-09-10
Evidence: `crates/qoro-move/src/lib.rs` and its tests
Reproduce: `./scripts/move-check.sh`

## Context

Milestone 8 is blocked on this because a move is the operation with the worst failure mode
in the product: two machines each believing they own the same workspace, both accepting
input, both writing history, and a person unable to tell which one their agent is actually
in.

The tempting shape is a transaction. There is no transaction across two machines and a
network, and pretending otherwise produces a design whose failure handling is "this cannot
happen".

The other tempting shape is a copy, leaving the source in place. That is safe and it is not
a move: the person now has two workspaces, both live, and has to remember which one they are
supposed to use. The thing that makes a move a move is that exactly one side is
authoritative at every instant, including every instant during a failure.

## Decision

### Authority is a single value that moves, and it is never in two places

A workspace has an authority marker: this machine owns it, or it is being moved to a named
machine, or it lives elsewhere. The marker is durable before anything else in a step
happens, so a crash between two steps leaves a marker that says which step was in progress.

The invariant is stated as the thing a gate checks: at every point in the move, and after
any failure at any point, exactly one machine answers "I own this workspace". Not zero and
not two. Zero is recoverable and visible; two is neither.

### The steps are enumerated, and each one is reversible or final

```text
1 quiesce    stop accepting new input on the source, finish what is in flight
2 snapshot   write a consistent copy of metadata and history to a staging area
3 transfer   move the staging area to the target
4 verify     the target reads the snapshot and reports what it found
5 activate   the target takes authority; the source records that it no longer has it
6 cleanup    the source removes the staging area and, if asked, the working tree
```

Steps 1 to 4 are reversible: nothing on the target is live and the source still owns the
workspace, so a failure returns authority to the source and the move is abandoned. Step 5 is
the only irreversible one, and it is a single durable write on each side ordered so that the
target takes authority before the source releases it. That order is deliberate: the failure
window it leaves is one where two machines could both claim it, which is exactly the
outcome this ADR exists to prevent, so the target's claim is conditional until it sees the
source's release, and the source's release is what makes it unconditional.

A crash between the two leaves the target holding a conditional claim, which answers "I am
taking over from A" rather than "I own this", and a person is told so.

Step 6 is cleanup and cannot fail the move: the workspace is already on the target. A
cleanup failure is reported and the staging area is left behind for somebody to remove, which
is worse than tidy and better than a move that says it failed after it succeeded.

### A dirty working tree blocks a move

A move that carries a workspace and leaves uncommitted work behind is a move that loses
work. It is refused, with what is dirty named, and the override is explicit and per move.
There is no setting: a setting is a decision made once by somebody who has forgotten it.

### Cleanup goes through the teardown state machine

A process holding a file inside the tree is the ordinary case, not an edge case: an editor,
a language server, a build watcher. Removing the tree under it produces a half removed tree
and a confused process. So cleanup terminates the terminal's processes through ADR-014's
containment and escalation first, and only then removes anything.

A removal that still fails after that is reported and rolled back: what was removed is
already gone, so the rollback is to stop and say what is left, not to recreate anything.

## Consequences

- Every failure has exactly one authoritative side, which is checkable and is checked per
  step rather than in aggregate.
- The move is abandonable up to the moment of activation, and after it there is nothing to
  abandon.
- A dirty tree costs a person one explicit decision, every time, rather than one forgotten
  setting once.
- Cleanup can leave a staging area behind, and says so.

## Rollback

The authority marker is the load-bearing part. If a distributed lock ever becomes available,
it replaces the marker without changing the step list, because the steps are about what is
durable when, not about how authority is represented.

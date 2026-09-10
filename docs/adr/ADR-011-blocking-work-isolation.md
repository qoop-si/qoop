# ADR-011: Blocking work isolation and cancellation

Status: accepted
Date: 2026-09-09

## Context
Invariant I3 says a slow or dead filesystem never blocks the PTY, input, render or
accept path. The original plan satisfied that with "run it on a blocking pool with a
deadline and a cancellation token". An external review correctly rejected this: a
deadline lets the *caller* stop waiting, it does not stop the *syscall*. A `stat` on a
dead NFS mount is uninterruptible in the D state. `tokio::task::spawn_blocking` documents
that spawned blocking tasks cannot be cancelled, and that runtime shutdown waits for
them. Enough stuck calls therefore exhaust the pool, and once exhausted, unrelated work
that also needs the pool (database writes, process spawn, teardown) stops.

This is a failure that has been reported in the wild, where a disconnected NFS share froze the
whole reference server, and the naive fix reproduces it.

## Decision

Three separate mechanisms, not one pool.

### 1. Classified pools with reserved capacity

| Pool | Purpose | Threads | Admission |
|---|---|---|---|
| `lifecycle` | process spawn, teardown, reap, socket accept work | small, fixed | never shared with untrusted paths |
| `store` | SQLite and history segment writes | small, fixed | bounded queue, backpressure to producer |
| `probe` | anything touching a path the user controls: cwd resolution, git, directory walks, setup capture | fixed, generously sized | bounded admission, jobs rejected when full |

A stuck `probe` job can only consume `probe` capacity. Spawning and teardown always have
threads available. This is the difference between a degraded feature and a frozen daemon.

### 2. Abandonment, not cancellation

A `probe` job that exceeds its deadline is *abandoned*: the caller gets a timeout result
immediately, the thread stays occupied until the syscall returns, and the job is counted.
The result of an abandoned job is discarded when it eventually arrives.

The abandoned count is a first-class metric. When abandonment exceeds a threshold for a
given path prefix, that prefix is marked degraded and further probes against it are
rejected without occupying a thread at all. This is what stops one dead mount from
draining the pool over minutes.

### 3. Process isolation for the worst offenders

Operations that are both slow and known to hang (git on a network filesystem, directory
walks under a user-supplied root, setup capture) run as a child process, not a thread.
A child can be killed. A thread stuck in D state cannot.

The child is spawned with its own process group so the whole tree can be signalled, and
with a hard wall-clock limit.

## Consequences
- The daemon can lose the ability to answer "what is this terminal's cwd" while remaining
  fully able to read PTYs, accept clients, spawn and tear down.
- Degradation is visible: a degraded path prefix is reported in status output, not
  silently swallowed.
- Cost: three pools instead of one, and a per-prefix health map.

## Verification
Fault injection test: mount a filesystem that never answers (a FUSE mount that sleeps
forever, or a stopped NFS server in CI), point one terminal's cwd at it, then assert
that over the following 60 seconds
1. every other terminal still reads and writes,
2. a new terminal can still be created,
3. teardown of an unrelated terminal still completes within its deadline,
4. the abandoned counter grows and the prefix is marked degraded,
5. the daemon's thread count stays bounded.

## Rollback
None. Reverting to a single unbounded pool reintroduces the exact production failure this
ADR exists to prevent.

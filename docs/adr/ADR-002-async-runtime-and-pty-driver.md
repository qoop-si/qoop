# ADR-002: Async runtime and PTY driver

Status: accepted
Date: 2026-09-09
Evidence: `spikes/spike-runtime/src/pty.rs` and `src/teardown.rs`, macOS 15 (Darwin 25.6.0), arm64, Rust 1.98
Reproduce: `./scripts/regen-spike-results.sh` (exits 0; transcripts land in `spikes/transcripts/`)

## Context
A comparable multiplexer dedicates one blocked OS thread per pane, running a
hand-rolled `poll()` loop with a self-pipe wake. That design notes the
consequence: N panes means N kernel threads, and thread count with a default 2 MB stack
becomes the scaling constraint at high pane counts. It also means the PTY hot path never
touches the async reactor at all, which is why every other subsystem ended up on fixed
timers instead of readiness events.

The question this ADR answers is whether a PTY master can be driven by the async reactor
on our target platforms, because that is what makes invariant I11 (nothing changed means
nothing runs) reachable for terminals rather than only for renders.

## Options
1. Tokio with `AsyncFd` over the PTY master. One task per terminal, one shared reactor.
2. One OS thread per terminal blocked in `poll()`.
3. A shared thread pool each owning an epoll or kqueue set of many PTY fds, hand rolled.

## Measurements

`spike-pty`, real `/bin/sh -i` on a real openpty pair. The current transcript is embedded
in `docs/spike-results.md`, which is generated from the program's stdout rather than
typed. The decisive line is check 4: a three second timeout expires with no readable
readiness, so an idle terminal delivers no reactor event at all. What proves it is
`idle.is_err()`.

Withdrawn: the first version of this ADR cited a "reactor wakeups" counter printed by the
same spike. That counter was incremented only inside a read helper that does not run
during the measurement window, so it proved nothing. The counter has been deleted from
the program. The honest idle number is an OS measurement, `getrusage` over a three second
window with a live but silent shell, in `spike-proc` check 4.

## Decision
Tokio, multi-threaded runtime. PTY masters are registered with `AsyncFd` and driven by
one task per terminal. No thread per terminal. Blocking work runs only under ADR-011.

### Mandatory implementation rule: the full readiness classification

`AsyncFd` caches readiness. `readable()` keeps resolving immediately until a `try_io`
closure returns `WouldBlock`, at which point the reactor clears the flag. A read loop that
ignores this spins at 100 percent CPU. The first version of the spike measured a 208
nanosecond "idle wait" for exactly this reason.

The first version of this ADR stopped there, and that rule alone is not sufficient.
`spike-teardown` claim C measured what a PTY master does after its child exits and the
master is drained: `poll` reports `POLLIN | POLLHUP` and `read` returns 0 on every round,
indefinitely, with no error at all on macOS. `try_io` returning `Ok(Ok(0))` does not clear
cached readiness either, so a loop written to the letter of the original rule spins one
core at 100 percent forever, on the idle path, after the child is gone. On Linux the same
condition commonly surfaces as `EIO`, which is also not `WouldBlock`, so the same loop
spins on the error branch instead.

The complete rule, a merge gate and lint-able:

| Result of the `try_io` closure | Meaning | Required action |
|---|---|---|
| `Err(WouldBlock)` | no data right now | readiness cleared, wait again |
| `Ok(0)` | end of stream | deregister the `AsyncFd`, end the read half, begin teardown |
| `Err(EIO)` | end of stream, Linux spelling | identical to `Ok(0)` |
| any other error | fatal to this terminal | report it, tear the terminal down |

This gets its own gate, deliberately separate from the ordinary idle gate: it measures
daemon CPU for ten seconds **after** a terminal's child exits, which is where the bug
lives and where an idle gate with a live child cannot see it.

### Write path discipline

The spike wrote with `write_all` on a nonblocking descriptor. That worked only because its
writes were small, and it is not the shipping pattern: a write large enough to fill the
PTY buffer partially succeeds and then returns `WouldBlock`, losing the tail unless the
caller retains an offset.

`spike-teardown` claim D measured the real queue: delivering a 1 MiB newline-terminated
payload took over a thousand `EAGAIN` retries, so the effective tty input queue is on the
order of one kilobyte and every large paste is thousands of writable-readiness round
trips. The shipping write path keeps an explicit offset per pending write, drains on
writable readiness, interleaves with reads, and sizes its chunks to that measured queue
rather than to an arbitrary constant.

The same measurement found a limit we cannot engineer around: a payload with no line
terminator, written to a child in canonical mode, is discarded by the line discipline
while every `write` reports success. No daemon-side queue discipline changes that. It is
detected and reported, not silently called a success.

### The parse budget is a chunk size plus a check, not a bound

The plan's per-turn budget of 250 microseconds cannot be honoured by checking a clock,
because a parse call cannot be interrupted. Measured at 200x50: one `advance` over a full
64 KiB read buffer of plain text takes roughly 1235 microseconds, a 16 KiB slice roughly
425, and an 8 KiB slice lands near the budget. The mechanism is therefore to feed the read
buffer in **8 KiB sub-slices** and check elapsed time between them.

There is a floor no chunk size fixes. A single nine byte sequence, `CSI 100000 S`, was
measured at roughly 318 microseconds inside one uninterruptible call. The budget is a target
with a stated worst-case overshoot of one chunk plus the worst single sequence, not a bound,
and invariant I4 rests on it as a target. That sequence goes into the parser property corpus
so the number stays honest when the engine is swapped.

### Blocking work

Out of scope for this ADR. See ADR-011, which supersedes the earlier "dedicated pool with
a deadline and a cancellation token" language, because a deadline does not stop an
uninterruptible syscall.

## Consequences
- Terminal count scales with task count, not thread count.
- The PTY path lives on the same reactor as everything else, so readiness, not a timer,
  can drive detection (ADR-009).
- `tcgetpgrp` on the master works and is O(1), confirming the tiered foreground-process
  strategy is available to us on this platform.

## Platform scope

Supported for v1: macOS and Linux. The spike ran on macOS with kqueue. The identical
binaries run on Linux in CI (`.github/workflows/ci.yml`, job `platform-spikes`) and a
disagreement fails the build rather than quietly surviving as an assumption.

Windows is explicitly **out of scope for v1** and this ADR makes no claim about it.
ConPTY requires synchronous handles on the pseudoconsole-facing side, so neither
`AsyncFd` nor an fd-pool translates, and foreground process groups and POSIX signals have
no direct equivalent. A Windows port is a separate ADR with its own spike covering handle
topology, cancellation, close ordering, Job Objects and named-pipe ACLs. Until that ADR
exists, no document, gate or marketing claim may imply Windows support.

## Rollback
If a platform cannot drive a PTY master through its reactor, the fallback is option 3, a
small pool of threads each owning many fds. It is not option 2. One thread per terminal
is the thing being rejected.

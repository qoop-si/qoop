# ADR-009: Idle and detection scheduling policy

Status: accepted, supersedes the first draft of this ADR
Date: 2026-09-09
Evidence: `spikes/spike-runtime/src/proc.rs`
Reproduce: `./scripts/regen-spike-results.sh` (exits 0), see `docs/spike-results.md`

## Context
The most repeated failure family in multiplexers of this kind is idle CPU
that scales with pane count, a class that gets fixed and then resurfaces. The current
mitigation is a cheap targeted syscall gating a rare expensive walk, but the schedule is
still a fixed 300 to 500 ms timer per pane, forever, plus a 5 second expensive recheck
floor. A fully idle pane never stops costing wakeups.

## What the first draft of this ADR got wrong

It claimed: a foreground process group change cannot happen without either PTY output or
SIGCHLD, therefore those two sources plus a one-shot settle timer are sufficient.

That claim is false and was disproved by measurement. `spike-proc` check 2 sends
`exec sleep 4` to a shell and reports the foreground process group before and after,
together with the byte count the terminal saw. The group is unchanged and the only bytes
are the echo of the command line. The transcript is in `docs/spike-results.md`,
generated from the program's stdout rather than typed.

`exec` replaces the process image in place. The pid is unchanged, the process group is
unchanged, no child is created so there is no SIGCHLD, and the only bytes on the terminal
are the echo of the command line, which happens *before* the transition. A detector built
on the original premise sees nothing at all, forever.

The same hole applies to a background job started from a script, and to any agent CLI
that re-execs itself.

## Decision

### Sources, in order of strength

| Source | Platform | Covers | Cost when idle |
|---|---|---|---|
| PTY readability | all | content change | zero, reactor driven |
| `EVFILT_PROC` with `NOTE_EXEC` and `NOTE_EXIT` | macOS, BSD | in-place exec, exit | zero, kernel edge |
| `pidfd` readability | Linux 5.3 and later | exit only | zero, reactor driven |
| adaptive probe | Linux only, exec gap | in-place exec | one cheap read per terminal per interval |

`spike-proc` check 3 confirms `EVFILT_PROC` with `NOTE_EXEC` fires for an unprivileged
child of our own process, with no polling. On macOS and BSD the exec gap is therefore
closed by the kernel and the idle cost is genuinely zero.

Linux has no unprivileged equivalent. `pidfd` covers exit. The process connector needs
`CAP_NET_ADMIN`, `fanotify` on `PROC_EVENT_EXEC` is not a thing, and `ptrace` is not
acceptable for a process we do not own the debugging of. So on Linux the exec gap is
closed by an adaptive probe and we say so rather than pretending otherwise.

### How the kernel event source reaches the reactor

Neither tokio nor mio exposes a way to add an `EVFILT_PROC` filter to the reactor's own
kqueue; they register descriptors only. Leaving this unwritten invites two wrong turns,
both of which undo the point of ADR-002: one kqueue and one blocked thread per terminal,
which is the thread-per-pane cost we rejected, or a drain loop that forgets the readiness
rule and spins, because `kevent` returning 0 is not `WouldBlock`.

The design is therefore explicit: **one kqueue per daemon**, holding one `EVFILT_PROC`
filter per terminal leader, whose own descriptor is registered with `AsyncFd` and drained
with a zero-timeout `kevent` until it returns 0. One descriptor, one task, N filters.

Fork tracking is deliberately not used. `spike-proc` check 3 measured `fork=false` for an
in-place exec, so `NOTE_FORK` contributes nothing to the case that motivated this design,
and `NOTE_TRACK` has a documented overflow failure mode that a process re-execing in a
loop can provoke. New foreground processes are found by the cheap `tcgetpgrp` gate
instead. Nobody should reach for `NOTE_TRACK` later believing it was part of the plan.

The expensive foreground walk is rate limited per terminal, so a process execing in a loop
cannot drive unbounded probes.

Registration has a hole at exactly the moment a terminal is created. `kevent` with `EV_ADD`
and `EVFILT_PROC` on a process that has already exited returns `ESRCH`, including while it
is still an unreaped zombie, measured in review. The daemon spawns the child and then
registers, so a child that exits inside that window is unwatchable: a mistyped command, a
wrapper whose `exec` fails, an agent CLI that rejects a flag. `ESRCH` at registration
therefore means "already exited" and goes straight to the exit path; it never fails terminal
creation. The source of record for exit in that window is the PTY end-of-stream
classification in ADR-002, and the platform event-source conformance test covers it.

`EV_CLEAR` is set but is not what makes the drain terminate: measured with and without it, a
zero-timeout `kevent` returns the exec event once and then returns 0. The drain-until-zero
rule is what terminates the loop, and it is written down here so nobody removes it while
refactoring on the assumption that the flag was doing the work.

### Settle timers must be jittered too

An earlier draft applied jitter only to the Linux probe. That is not where the herd is. The
settle timer is what catches a transition with no further output, including the ordinary
case of typing `sleep 60` at a prompt, and it fires when a terminal goes quiet. Forty
terminals finishing a parallel build go quiet within the same few milliseconds and arm
forty identical timers that land in one tick, each firing the expensive walk. ADR-011's
probe pool then rejects most of them, and a rejected probe that never re-arms leaves those
terminals with a stale foreground process until some unrelated output happens, which on an
idle-by-design system may be never.

Therefore: settle timers carry deterministic per-terminal jitter, the expensive walk has a
global concurrency limit and shares one process snapshot per tick across all terminals
that need it, and a rejected or abandoned probe re-arms with backoff. There is a gate that
quiesces 100 terminals simultaneously and asserts every terminal's foreground process is
correct within a stated bound.

### The adaptive probe, Linux only

One `read` of `/proc/<pid>/stat` per terminal per interval, where the interval is a
function of observed stability:

```text
after any activity        50 ms
stable for 1 s            250 ms
stable for 10 s           2 s
stable for 60 s           15 s
stable for 10 min         60 s   (floor)
```

Correctness bound: after a terminal quiesces, its foreground process must be correct within
**one probe interval plus 100 ms**, which at the floor is 60.1 seconds and immediately after
activity is 150 ms. Gate G2.13 asserts against that number rather than against an unstated
bound.

Activity means PTY output, a client command touching the terminal, or a process event.
Any activity resets the interval to the fast end.

At the floor, 100 idle terminals cost 100 reads of a procfs file per minute. That is a
number, it is measured by the benchmark, and it is honestly not zero. It is roughly two
orders of magnitude below a fixed 300 ms per pane, which is
20 000 reads per minute for the same 100 terminals.

Probes are jittered so that terminals created together do not align into a thundering
herd, and the whole probe schedule is suspended entirely when no client is attached and
no agent is in a state that anything is waiting on.

### The rule that survives from the first draft

No periodic loop may exist in the daemon without an ADR that states why an event source
is unavailable, the frequency, the idle budget and the stop condition. This ADR is that
statement for exactly one loop: the Linux exec probe. Every other periodic loop is
rejected in review.

There is exactly one sleeping point in the daemon, the scheduler, whose deadline is the
minimum over armed one-shot timers and the probe schedule. On macOS with no terminals
armed, nothing is armed at all.

## Measurement

The idle claim is measured by the OS with `getrusage`, over a three second window with a
live but silent shell on a real PTY. The figure is in `docs/spike-results.md`,
generated from the program's stdout rather than typed, and deliberately not copied here.
Two orders of magnitude of margin is the claim; the exact microsecond count varies per run
and is not the point.


The benchmark asserts a per-platform CPU budget over a 60 second window with 100 idle
terminals, and the budget for Linux explicitly includes the probe cost. It is a merge
gate.

The previous draft cited a "reactor wakeups" counter. That counter was incremented only
inside a read helper that was not running during the measurement window, so it proved
nothing. It is withdrawn.

### The subscription, built and measured

`kqueue` with `EVFILT_PROC` was the stated plan and it holds up. What the plan did not say,
and what building it made obvious:

**A fork is a warning, not a change.** `NOTE_FORK` arrives when the shell forks, which is
before it moves the child into the foreground, so reading `tcgetpgrp` on the event still
finds the shell. The foreground group is re-read over a short settle window instead, at 5,
25 and 70 milliseconds. Without it a `sleep 5` typed at a prompt is invisible: it produces
no output, so nothing else would ever look. Removing the settle turns gate G3.3 red.

**An exec by the foreground process needs `NOTE_EXEC` specifically.** No fork, no new
process group, no output: the only thing that changes is the program behind a pid already
being watched. Removing `NOTE_EXEC` leaves every other agent gate green and turns exactly
one red, which is why that gate exists separately.

**A hundred terminals with agent tracking on cost what a hundred terminals cost.** Measured
across two consecutive sixty second windows: nothing, twice. The subscription is the reason;
a probe at any interval that could see a `sleep` would not have been.

The Linux half stays a probe, because there is no unprivileged equivalent: `pidfd` reports
exit and nothing else, and an in place `exec` changes `/proc/<pid>/exe` with no notification
at all. The schedule is a pure function of the current interval and whether anything
changed, so it is asserted directly rather than by waiting for it.

### Two things that only appear at a hundred

**Settling inline blocks the subscription.** The watcher drained the queue and then ran the
settle window before draining again, so a hundred terminals changing foreground at the same
moment were settled ten batches a second. Settling is now its own task fed by a set, so the
watcher only marks and the settles coalesce across every terminal that moved.

**The drain buffer has to fit the burst.** Thirty two events per drain looks generous and is
not: a hundred terminals starting a command together produce more than that at once, the
remainder stay in the queue, and the descriptor's readiness edge has already been consumed,
so nothing comes back for them. Measured, with everything else already fixed: 14 of 100
terminals noticed their own foreground process. With a buffer that fits the burst, 100 of
100, in 71 milliseconds against a 170 millisecond bound.

Both are the same shape as the idle failures below, in the other direction: a number that is
fine at one and wrong at a hundred, and only a gate at a hundred finds it.

### The rule broke again, in a new place

A timer per terminal, added to flush buffered history when a terminal goes quiet, ran
whether or not there was anything to flush. At a hundred terminals that is two hundred
wakeups a second, and it took the idle gate from nothing at all to 100 milliseconds of CPU a
minute. The writer now parks with no timeout while nothing is buffered.

Twice now a correct fix to something else has broken this policy, and both times the gate at
scale caught it rather than review. A timer that fires only when there is nothing to do
looks free while it is being written, which is precisely the shape this ADR exists to
forbid.

## Consequences
- macOS and BSD reach literal zero idle cost for terminal and process observation.
- Linux reaches a small, bounded, adaptive cost with a stated floor. We do not claim zero
  on Linux and no marketing material may.
- Every platform needs its own event-source implementation behind one trait. That trait
  gets a conformance test that both implementations must pass, including the in-place
  exec case that broke the first draft.

## Rollback
If the adaptive probe proves insufficient on Linux for a real agent, the next step is a
privileged helper or an opt-in `ptrace`-free eBPF source, both of which are new ADRs. It
is never a fixed unconditional timer.

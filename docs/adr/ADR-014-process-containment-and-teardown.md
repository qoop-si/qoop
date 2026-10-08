# ADR-014: Process containment and the teardown state machine

Status: accepted, supersedes the first draft of this ADR
Date: 2026-09-09
Evidence: `spikes/spike-runtime/src/proc.rs` and `src/teardown.rs`
Reproduce: `./scripts/regen-spike-results.sh` (exits 0), see `docs/spike-results.md`

## Context

The plan originally said teardown signals "the terminal's process set" with an escalating
SIGHUP, SIGTERM, SIGKILL sequence. A review pointed out that this names no race-safe
containment mechanism: descendants can fork after enumeration, pids can be reused, and
enumeration itself can over-match. Over-matching
would make unrelated sessions appear to exit together.

The first draft of this ADR replaced enumeration with "containment identity is the
session, so signalling `-pgid` is sufficient and there is nothing to race against". A
second review said that was wrong and measured it. It was reproduced here from scratch
before this ADR was changed, because a reviewer's measurement is a lead, not a fact.

## What was measured

**Claim A, confirmed, after two wrong setups.** A real interactive shell starts a
`nohup` background job. The job is in the leader's session, in its own process group, and
ignores SIGHUP. After the full SIGHUP, SIGTERM, SIGKILL escalation aimed at the leader's
process group, the leader is dead and the job is still running.

Two earlier attempts at this test proved nothing, and both are recorded because a test that
reads as a result while measuring something else is worse than no test. The first spawned
the "escaping" child from our own process, so it was a sibling in a different session and
demonstrated only that signals do not cross process groups, which nobody disputed. The
second used a plain background job, which died because `zsh` relays a hangup to its own
jobs, not because our signal reached it. Only a job that ignores SIGHUP isolates the
mechanism under test.

POSIX has no `kill(-sid)`; a session is not a signalable object. The first draft's central
claim was simply false, and enumerating is the usual answer for exactly this
reason.

**Claim B, confirmed, with a boundary worth stating.** The reason an exiting child is not
reapable is undrained tty output, not the parent holding the master open. A child that
writes 256 KiB and exits is still unreaped after four seconds while the parent holds the
master and never reads; draining it, with the master still open, reaps it immediately.

The boundary: the first version of this test wrote two bytes, and an external reviewer
running it under load saw it reap anyway. That is correct kernel behaviour rather than a
flake. A small write is usually already sitting in the master's queue by the time the slave
closes, so there is nothing left to wait for. What blocks the reap is output that has not
yet been absorbed, which needs a write larger than the buffer. The design rule is unchanged
and is required for output preservation either way; the test is what had to become
deterministic, and it now is.

This corrects an earlier finding of our own, which claimed the blocker was the open
master. Closing the master does make the reap succeed, but only because closing discards
the pending output. The first draft turned that accident into a prescribed step, which
would have silently thrown away a shell's logout message or a coding agent's final
summary on every terminal close.

**Prior finding, still valid.** `openpty` returns descriptors with `FD_CLOEXEC` clear, and
a child spawned for one terminal was measured inheriting another terminal's master and
slave. With close-on-exec set, the same child sees only its own tty.

## Decision

### Containment requires enumerating the terminal's process set, before it is destroyed

Each terminal's child is started with `setsid` and acquires the PTY slave as its
controlling terminal. Every process in that session shares that controlling terminal
device, and that device is the containment identity: it is stable and readable for as long
as a process keeps it.

Stated precisely, because the earlier phrasing overclaimed: forging is not the escape
route, leaving is. `setsid()` and `ioctl(TIOCNOTTY)` both drop the controlling terminal,
and the classic double-fork daemonise does exactly that. A `setsid npm run dev` or a
self-daemonising process manager is gone from `e_tdev` before teardown begins, so the
monotonic union that fixes the leader-death case does not help: there was never a round in
which it was in the set. This is a limit of every userspace key on Unix, not a defect in
this one, and the previous session-id design had it too. The Linux cgroup variant below is
the only path that closes it, which is an argument for that path rather than against this
ADR.

Three things had to be established by measurement before this could be written down, and
each of the first two versions of this ADR got one of them wrong.

**The set must be enumerated before the leader dies.** POSIX disassociates a controlling
terminal from its session when the controlling process terminates. Measured directly:
enumeration finds five members including the job before teardown, and zero members
afterwards. An implementation that "re-enumerates before each escalation step", which is
what the previous version of this ADR said, would enumerate an empty set from the second
step onward and silently contain nothing.

Therefore the containment set is a **monotonic union across rounds**. It is enumerated
before the first signal, re-enumerated before each subsequent step to catch a fork inside
the grace window, and the two are unioned. A process never leaves the set once it has been
seen, because leaving is exactly what the kernel makes it do.

**The enumeration API is not the obvious one, and its return value is not what it looks
like.** `sysctl` with `KERN_PROC_SESSION`, which is the correct constant name
(`KERN_PROC_SID` does not exist), returns `kinfo_proc`, which the `libc` crate does not
expose on Apple platforms; a reviewer also measured that selector returning `ENOENT` on
macOS 15, so the binding is not the only obstacle. The implementable route is
`proc_listallpids` followed by `proc_pidinfo(PROC_PIDTBSDINFO)`, filtering on `e_tdev`.

`proc_listallpids` returns a count of **pids**, not a count of bytes. An earlier version of
our own probe divided it by four, and because the list comes back newest first that
discarded the oldest three quarters of the process table, which is exactly the part
containment needs: a dev server started an hour ago sits past the cut and a `nohup sleep`
started milliseconds ago does not. Verified directly: the return value equals the index
after the last non-zero entry and equals `ps -ax`. The implementation counts non-zero
entries rather than trusting either interpretation.

`proc_pidinfo` answers for processes we own and returns `EPERM` for others, roughly 110 of
174 in one measurement. Most of that remainder belongs to other users. Some of it does not:
a user who runs `sudo vim` in a qoop terminal creates a root-owned process on our
controlling terminal that we can neither enumerate nor signal. Teardown reports that case
rather than silently succeeding. It is a documented limit of same-uid containment, in the
same spirit as ADR-012's refusal to claim isolation it does not enforce.

**Pid reuse across rounds needs a second key.** The set is captured before the first signal
and never shrinks, so a group whose members all died during the SIGHUP round is still
signalled 500 ms later. Pids are allocated sequentially and a busy machine turns over
hundreds per second, so that is a real reuse window. The set cannot be re-verified with
`e_tdev`, because that is exactly what the kernel revokes. `proc_bsdinfo` already carries
`pbi_start_tvsec` and the spike already reads that struct, so the process start time is
recorded alongside the pid at first enumeration and re-checked immediately before every
signal from the second round onward. A group with no surviving recorded member at its
recorded start time is dropped. On Linux, field 22 of `/proc/<pid>/stat` is the
equivalent.

On Linux, field 7 of `/proc/<pid>/stat` is the controlling terminal device number and the
same filter applies. Linux may additionally place the session in its own cgroup when one is
available, which turns containment into a kernel guarantee and removes the enumeration on
that path entirely.

**Signalling is by process group, never by bare pid.** Every distinct process group in the
containment set is signalled with `kill(-pgid, sig)`. Signalling a bare pid would reopen
the pid-reuse hazard; signalling only the leader's group is precisely what was measured to
fail.

```text
set = enumerate(tty_dev)          # (pid, pgid, start_time) before any signal
for round, sig in enumerate([SIGHUP, SIGTERM, SIGKILL]):
    set |= enumerate(tty_dev)     # union, never replace
    for g in distinct_pgids(set):
        if round > 0 and not any(still_alive(p) and start_time_matches(p) for p in set[g]):
            continue              # every recorded member is gone: g may be recycled
        kill(-g, sig)
    wait grace, draining the master throughout
```

The `start_time_matches` check is the pid-reuse guard and it is in the pseudocode rather
than only in prose, because it is the step an implementer would otherwise leave out.

Enumeration runs only during teardown, on the lifecycle pool, at most a handful of times
per terminal close. It never runs on any idle path, so it does not reintroduce the per-pane
process scan that the idle policy exists to eliminate.

Windows is out of scope for v1. When it arrives, a Job Object with kill-on-close assigned
after a suspended spawn and before resume is the equivalent, and it is a stronger
guarantee than either Unix path.

### Every PTY descriptor is close-on-exec

Set at creation, before any spawn can occur. The only deliberately inheritable descriptors
are the child's own slave on fds 0, 1 and 2. The integration suite asserts this against
the child's real descriptor table, not against our own bookkeeping, because our own
bookkeeping is exactly what would be wrong.

### The teardown state machine

```text
Closing
  1. close the write gate            new writes get a typed error, in-flight ones finish
  2. flush accepted writes           bounded by a deadline
  3. enumerate the containment set   BEFORE any signal, while the tty association exists
  4. START draining the master       continuously, for the whole of steps 5 to 8
  5. union-enumerate, signal SIGHUP  to every group in the set, wait 250 ms
  6. union-enumerate, signal SIGTERM to every group in the set, wait 250 ms
  7. union-enumerate, signal SIGKILL to every group in the set, wait 250 ms
  8. reap with WNOHANG polling until the deadline, still draining
  9. close the master                last, only now
Closed
```

Step 3 before step 5 is not cosmetic ordering. It is the difference between containing the
terminal's processes and containing nothing, because the set is empty after the leader
dies.

Step 4 is the correction from claim B. The master is drained, not released, and everything
read during teardown is appended to the terminal's history so the child's final output
survives. Closing early is banned and has a negative test.

Note on ordering: closing the master causes the kernel to send SIGHUP to the tty's
foreground process group. Because the master now closes last, step 4's explicit SIGHUP is
the only hangup the child sees, and there is no racing second delivery. Nobody should
reintroduce an early close on the grounds that "the kernel will hang it up anyway": the
kernel hangs up only the foreground group, so a backgrounded job gets nothing.

### Exactly one reader of the master, throughout

The PTY master is owned by the terminal task and registered with the reactor. It stays
that way for the whole of teardown. The `lifecycle` pool enumerates, signals, and performs
the bounded `WNOHANG` reaps, and reports completion back to the terminal task, which does
the final close in `Closed`.

This is stated explicitly because the previous version implied a second reader: a blocking
pool thread "draining the master" alongside the terminal task would split the child's final
output non-deterministically between two readers, which destroys the exact guarantee step 4
exists to provide, would spin on `EAGAIN` for the whole escalation on a non-blocking
descriptor, and would consume readiness the reactor believes it still owns, which is the
same class of bug as the end-of-stream spin ADR-002 wrote a table to prevent.

The alternative, deregistering the `AsyncFd` and moving the `OwnedFd` into the teardown
job, is also acceptable, but it is a transfer of ownership and must be written as one. What
is banned is two concurrent readers.

Reaping is exactly once: the state machine owns the child handle and no other code path may
call wait on it. Signalling and reaping run on the `lifecycle` pool (ADR-011), never inline
in a `Drop` on a reactor thread.

### What the drain retains, and the deadline

Reading must not stop, because stopping it is what blocks the reap. Retaining everything it
reads is a separate decision and is bounded separately: a child that ignores SIGTERM and
keeps writing is drained at full speed, and only the last 64 KiB is appended to the
terminal's history, with a counter for the discarded remainder.

The teardown deadline is **2 seconds** from entering `Closing`: 750 ms of escalation plus a
1250 ms reap window. `G1.19` asserts against that number rather than against an implied sum
of per-step waits.

Teardown of multiple terminals runs concurrently, bounded by the `lifecycle` pool. Closing
a hundred terminals serially at 2 seconds each would be over three minutes, which nothing
in the previous version forbade.

### Crash during teardown

Reconstructing a half-finished teardown after a daemon crash requires durable state, which
milestone 1 does not have. Rather than leave an accepted ADR depending on a milestone's
out-of-scope list, the behaviour is stated per milestone:

- Through M3: a daemon killed mid-teardown leaves an orphaned process tree with no record.
  This is documented, not silently hoped for, and it is what the M1 scope allows.
- From M4, when the durable log exists: the `Closing` state is journalled, and the next
  start resumes teardown from step 3, which is idempotent. Amended 2026-09-30: this was
  never built, and the log it needed is no longer written (ADR-005). A daemon killed
  mid-teardown still leaves the tree it was tearing down.

### Orphans on Unix

If the daemon is SIGKILLed, children survive as orphans. This is intended: a client crash
and a daemon crash must both leave the work running. Reattaching to an orphan is not
possible without a live handoff, so from M4 the next daemon start records the orphan,
reports it, and offers to adopt or terminate it. It never silently leaks and never
silently kills.

### Two mechanisms that were internally consistent and did not work

Both were found by gate G1.14 once it drove a real daemon rather than the state machine
alone, and neither would have been visible to anything else: each failure looks exactly
like a terminal that had nothing to contain.

**The containment identity came from the wrong end of the pty.** The device number was read
from the master, on the stated assumption that a pty pair shares one. It does not: measured
on this machine, master 251658270 and slave 268435486. Every enumeration matched nothing, so
containment was empty at every teardown. It is now read from the slave in `spawn_terminal`,
while that descriptor is still held, and passed to the terminal task.

Evidence: `sys::tty_dev` in `crates/qoop-pty/src/sys.rs`, and
`a_fresh_pty_has_a_distinct_controlling_device`.

**Reaping the child ended teardown.** `next_step` returned `CloseMaster` as soon as the
child was reaped. In the ordinary case, a user typing `exit`, the shell is reaped within
milliseconds, so nothing was ever signalled and every job it left behind survived. The
containment set exists for exactly those processes, so it is what decides when teardown is
finished: `child_reaped && !containment.any_alive(still_alive)`.

Evidence: `a_reaped_child_does_not_end_teardown_while_an_escaped_job_runs` in
`crates/qoop-pty/src/teardown.rs`.
Reproduce: `cargo test -p qoop-pty teardown`

### Five more, found by reviewing the code against this document

Each one was internally consistent, passed every gate that existed, and did not do what
this ADR says.

**A grace window that was not waited.** The drain returned as soon as the master reported
end of stream, which is the state teardown is normally entered from, so HUP, TERM and KILL
were delivered 1.9 milliseconds apart. Measured: a job's SIGTERM handler was killed 372
microseconds into a 50 millisecond cleanup, so anything that flushes on SIGTERM was killed
mid flush on every terminal close. The same defect turned the post escalation reap into a
spin of roughly 1.8 million iterations a second against the deadline. The window is now
waited out whatever the descriptor says.
Evidence: `g1_14_a_job_that_handles_sigterm_gets_its_grace_window`.

**Everything drained was thrown away.** Step 4 of this document says what is read during
teardown is appended to the terminal's history, and `retain_tail` was written for it and
never called. Gate G1.15 passed by ordering: there the child writes and then exits, so the
write is read by the ordinary loop before end of stream. A terminal closed on request has no
such luck, and a shell's `EXIT` trap printed into a sink. The drained tail is now fed to the
engine and rendered.
Evidence: `output_produced_during_teardown_reaches_the_screen`.

**Three error paths skipped teardown entirely.** A failing `TIOCSWINSZ`, or an error from
either readiness, propagated out of the terminal task, and the child, its foreground job and
everything it started kept running with the terminal gone and no record. Teardown now runs
however the loop ends.

**Round zero signalled without the pid reuse guard.** The exemption was justified by the set
being freshly enumerated, which stopped being true when the set became something maintained
for the terminal's whole life. The round that signalled without the guard was the round with
the oldest entries: a pid recorded at ten in the morning, long dead, signalled at six in the
evening when the number belonged to something else. The exemption is gone, and the set is
pruned of dead members at every scan, which also bounds it: a terminal that has run a day of
builds otherwise reaches ten thousand recorded members and 2.6 milliseconds of syscalls per
teardown loop iteration.

**A process we cannot inspect was skipped silently.** This document promises that a `sudo`
job on our controlling terminal is reported rather than silently succeeded past, and
`proc_pidinfo` refusing a process we do not own was simply a `continue`. The enumerator now
counts them and teardown says so.

### The set is maintained, not built at teardown

The kernel revokes the tty association when the controlling process terminates, so an
enumeration started at teardown finds nothing whenever the child exited by itself, which is
the common case. The set is therefore refreshed while the child is alive, on output,
throttled by `CONTAINMENT_SCAN_INTERVAL`.

The interval is a real trade. A scan costs 360 microseconds, so 50 milliseconds means 7
milliseconds of CPU per second of continuous output and nothing at all while idle. It is
also the width of the window in which a job can be started and the terminal closed without
that job ever being seen. The first value was 250 milliseconds, which sounded conservative
and was useless: a scripted session that starts a background job and exits runs end to end
in about 110 milliseconds, so the scan never ran once.

50 milliseconds was still a window, and on a faster machine it was the whole story: the
cleanup gate starts `nohup ... &`, prints, and types `exit` inside one interval, so the only
scan in the terminal's life was the one at its start, and the job survived teardown on every
run. What closes it is the session, which the revoke does not erase. `getsid` of a process
whose session leader has died still answers with the dead leader's pid, measured on macOS
and true on Linux, and neither kernel hands out a pid that is still a session id. So
teardown also absorbs every process in the leader's session, read once, before its first
`try_wait` can reap the leader: after the reap the number may lead another terminal's
session. The fallback teardown, which runs after a panic and cannot know whether the child
was already reaped, does not read the session at all. On Linux this also catches a job
that dropped its tty with `TIOCNOTTY`. A job that called `setsid` has left the session too,
and is still caught only if a scan saw it first.

## Consequences

- Teardown costs one process enumeration before the first signal plus one per escalation
  step, at most four per terminal close. That is a real cost and it is confined to a path that runs once per terminal, not
  to any periodic path.
- A terminal's final output is preserved, which is what a user actually reads when
  something fails at exit.
- The daemon holds the master open slightly longer than the first draft did, which is
  correct rather than merely tolerable.
- Milestones before M4 have no teardown crash recovery, and that is now written down.

## Verification

- Claim A regression: a `nohup` background job started by a real shell, in the leader's
  session and in its own process group, is terminated by teardown. Two weaker versions of
  this gate would have passed while the realistic case leaked: one used a plain `fork`,
  which inherits the parent's group, and one used a plain background job, which dies
  because the shell relays a hangup rather than because teardown reached it.
- Enumeration ordering: a negative test that enumerates only after the first signal finds
  an empty set, so the before-first-signal requirement cannot silently regress.
- fork-during-close: a grandchild spawned inside the grace window is caught by the next
  enumeration.
- unrelated-process-survival: a process in a different session with an adjacent pid is
  never signalled.
- fd-table assertion: a child holds exactly one tty device, read from the OS.
- enumerator completeness: the enumerator's scanned pid count matches `ps -ax` within a
  small tolerance, so a truncating enumerator cannot pass by finding only recent processes.
- pid reuse: a recorded group whose members have all exited is not signalled in a later
  round, asserted by recording a start time and replacing the process.
- output contiguity: the preserved final message is complete and contiguous, not merely
  present, which is what a second concurrent reader would break.
- output preservation: a child that prints a message and exits has that message in the
  terminal's history after teardown. This is the positive test for step 3.
- early-close negative test: with the master closed before step 4, the message is missing.
  The test asserts the failure, so the ordering cannot silently regress.
- reap within deadline while draining, and a bounded `WNOHANG` poll, never a blocking wait.

## Rollback

None. The two things being rejected are `kill(-pgid)` as sufficient containment, and
closing the master before the child has finished writing.

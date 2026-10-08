# Spike results

GENERATED FILE. Do not edit `plans/01_SPIKE_RESULTS.md` by hand. Edit
`plans/01_SPIKE_RESULTS.tmpl.md` and run `scripts/regen-spike-results.sh`.

Every fenced transcript below is the literal stdout of the committed program, substituted
by the script. It exists because an adversarial review caught hand-edited transcripts in
the previous version of this file: durations had been reformatted and one load-bearing
field had been deleted, in a document whose first line claimed nothing in it was
theoretical. CI regenerates this file and fails the build on any diff.

Platform: macOS 15 (Darwin 25.6.0), arm64, 24 GB, 12 cores, Rust 1.98, release build.
Code: `spikes/spike-runtime/src/{pty,term,proc,teardown,mem}.rs`
Reproduce: `./scripts/regen-spike-results.sh`

Everything below is macOS-only until the same binaries run in Linux CI. That gate is part
of the milestone 1 exit criteria, not a later nicety.

## spike-pty: is the async reactor a viable PTY driver? (ADR-002)

```text
[1] async read/write on PTY master: PASS (83 bytes in 7.863375ms)
[2] tcgetpgrp on master: PASS (Some(25015))
[3] resize observed by child (stty size): PASS
[4] idle 3s with no output: PASS (settled after 4 rounds / 8 bytes; no readiness for 3.002016958s)
[5] child exit status propagates: PASS (Some(7), 14 trailing bytes)
SPIKE-PTY: ALL PASS
exit=0
```

Verdict: `tokio::io::unix::AsyncFd` drives a real PTY master correctly. One task per
terminal replaces one blocked OS thread per pane.

Check 4 is the evidence for the idle path: the 3 second timeout fired, so no readable
readiness arrived at all. What proves it is `idle.is_err()`, nothing else.

Withdrawn: an earlier version of this spike also printed a "reactor wakeups" counter, and
ADR-002 cited it. The counter was incremented only inside a read helper that does not run
during the idle window, so it measured nothing. It has been deleted from the program
rather than reinterpreted in prose.

Discovered, and now a mandatory implementation rule: `AsyncFd` caches readiness.
`readable()` keeps resolving instantly until a `try_io` closure returns `WouldBlock`,
which is what clears the flag. The first version of this spike measured a 208 nanosecond
"idle wait" for exactly that reason. A read loop that ignores this spins one core at 100
percent, which is precisely the idle-CPU class we exist to beat. See also spike-teardown
claim C, which shows that `WouldBlock` alone is not a sufficient rule.

Not proven here: any scaling claim. This spike ran one terminal on a two-worker runtime.
The one-task-per-terminal advantage is asserted, not measured, until gate G2.1.

## spike-term: is a pure-Rust engine good enough? (ADR-003)

```text
[1] plain text into grid: PASS (["hello qoop", "second line"])
[2] SGR colour reaches cell state: PASS (c='R', fg=Named(Red), flags=Flags(BOLD))
[3] bounded scrollback: PASS (history=1000, screen=24, total=1024)
[4] damage tracking: PASS (quiet lines=[0, 23], after write to row 5 lines=[4, 23])
[5] resize to 132x40: PASS (cols=132, lines=40, last non-empty Some("line 2999"))
[6] parse throughput: 125 MiB/s (1.00 MiB in 7.989541ms)

SPIKE-TERM: ALL PASS
exit=0
```

Verdict: `alacritty_terminal` 0.26 provides grid, styling, bounded scrollback,
line-precise damage and resize, at throughput with two orders of magnitude of headroom
over a chatty agent.

Damage is line-precise: a single cell written at row 5 reports line 4 plus the cursor
line, not a full repaint. `damage()` always folds in cursor damage, so a quiet terminal
reports the cursor line rather than an empty set. That is a detail the renderer must know.

Not proven here: Unicode width and grapheme clustering, alternate screen, margins, tab
stops, saved cursor, terminal query responses, graphics fallback, or any comparison
against another engine. Those are the content of the milestone 2 conformance gate, and
the check 5 resize assertion verifies dimensions only, not content preservation.

## spike-proc: process observation and the honest idle number (ADR-009)

```text
[0] openpty default FD_CLOEXEC: master=false, slave=false (both false means every spawn leaks every other terminal's fds unless we set it)
[1] fd inheritance: without CLOEXEC the child sees ["/dev/ptmx", "/dev/ttys044", "/dev/ttys045"]; with CLOEXEC it sees ["/dev/ttys047"] -> PASS
[2] silent exec transition: pgrp Some(25039) -> Some(25039), bytes emitted after the command line: 14
    note: pgrp is unchanged by exec, so a tcgetpgrp comparison alone cannot see it
[3] kqueue EVFILT_PROC on an unprivileged child: exec=true fork=false -> PASS
[4] OS-measured idle cost over 3.001041833s: 33µs of CPU (poll rc=0, revents=0x0, drained 16 prompt bytes) -> PASS

SPIKE-PROC: ALL PASS
exit=0
```

### Finding 1: PTY descriptors are not close-on-exec, and the leak is real

`openpty` returns descriptors with `FD_CLOEXEC` clear. Measured directly against the
child's real descriptor table: without close-on-exec a child spawned for one terminal
inherits another terminal's master and slave; with it, the child sees only its own tty.

A leaked slave means the master never sees end of stream, so a terminal never reports its
child as gone. A leaked master breaks hangup semantics. Both are silent.

### Finding 2: the idle-detection premise in the first draft of ADR-009 was false

The claim was that a foreground process change cannot happen without PTY output or a
child exit. Check 2 sends `exec sleep 4` to a shell: the foreground process group is
unchanged, because `exec` replaces the image in place with the same pid and pgid, and the
only bytes on the terminal are the echo of the command line itself, which happens before
the transition. A detector built on that premise sees nothing at all, forever.

### Finding 3: there is a real kernel event source, at least on this platform

`kqueue` with `EVFILT_PROC` and `NOTE_EXEC` fires for an unprivileged child of our own
process with no polling. Check 3 also shows `fork=false` for an in-place exec, so
`NOTE_FORK` contributes nothing here and fork tracking is deliberately not part of the
design.

Linux has no unprivileged equivalent for exec notification. `pidfd` covers exit only. So
the design is: kernel events on macOS and BSD, `pidfd` plus an adaptive probe on Linux,
with the Linux floor stated rather than denied. See ADR-009.

### Finding 4: the idle claim, measured by the OS

Check 4 measures a live but silent shell on a real PTY with `getrusage`, which the kernel
computes rather than us. That is the honest form of invariant I11 and it replaces the
withdrawn counter.

Caveat that the number must carry: this is a bare `poll` on one descriptor in a process
with no tokio runtime, no blocking pools, no store and no clients. It is a floor for one
syscall, not a prediction for the daemon. The plan's 100-terminal budgets are targets
pending the first `qoop-bench` run.

### Finding 5, corrected

The first version of this document claimed that a killed session leader stays unreapable
until its controlling terminal's master is closed. That cause was wrong. See
spike-teardown claim B: the blocker is undrained tty output, and closing the master only
appears to fix it because closing discards the pending output. The corrected rule is in
ADR-014.

## spike-teardown: independently verifying four review claims

A reviewer's measurement is a lead, not a fact. Each claim below was reproduced from
scratch here before any ADR was changed.

```text
[A1] shell /bin/bash: leader pid=25051 pgid=25051 sid=25051 | job pid=25053 pgid=25053 sid=25051
     job is in the leader's session: true; job has its own process group: true; it ignores SIGHUP
[A2a] controlling-tty enumeration BEFORE teardown (dev=268435500): 2 members, job found=true
     after HUP/TERM/KILL on -25051: leader_alive=false job_alive=true -> CLAIM A CONFIRMED: process-group teardown leaves the job running
[A2b] enumeration AFTER the leader died: 0 members, job found=false
      the controlling terminal is disassociated when the session leader dies, so the containment set must be a union across rounds and can never shrink
[A2] union of enumerations: 2 members, 2 distinct groups, job found=true
     after signalling every enumerated group: job_alive=false -> PROPOSED FIX WORKS: tty enumeration reaches what pgid signalling missed
[B] exiting child with 256 KiB of undrained output, master held open, no reads for 4s: NOT REAPED (claim B supported so far)
    after draining 270336 bytes with the master STILL OPEN: REAPED code=3
    -> CLAIM B CONFIRMED: the blocker is undrained tty output, not the open master
[C] master after child exit, 5 rounds: ["rc=1 revents=0x11 read=0 errno=0", "rc=1 revents=0x11 read=0 errno=0", "rc=1 revents=0x11 read=0 errno=0", "rc=1 revents=0x11 read=0 errno=0", "rc=1 revents=0x11 read=0 errno=0"]
    read()==0 in 5/5 rounds, EIO in 0/5 -> CLAIM C CONFIRMED: end of stream is not WouldBlock, so an AsyncFd loop that only handles WouldBlock spins forever
[D] 1 MiB paste with NO newline into a canonical-mode child: wrote 1048576 bytes (EAGAIN 0) -> child received 0 bytes -> CLAIM D CONFIRMED: the line discipline discards it while write() reports success
    control, newline-terminated 1 KiB lines: wrote 1048576 (EAGAIN 654) -> child received 1048576 (100.0%)
    -> the daemon's own byte-bounded queue is not what fails; the tty line discipline is

SPIKE-TEARDOWN: COMPLETE
exit=0
```

### A. Process-group teardown does not contain a job-control shell's jobs

Confirmed, after two setups that measured the wrong thing.

The final setup: a real interactive shell starts a `nohup` background job. The job is in
the leader's session, in its own process group, and ignores SIGHUP. After the full
SIGHUP, SIGTERM, SIGKILL escalation aimed at the leader's process group, the leader is
dead and the job is still running.

The two discarded setups are recorded because a test that reads as a result while
measuring something else is worse than no test at all:

1. The first spawned the escaping child from our own process, so it was a sibling in a
   different session. It showed only that signals do not cross process groups, which
   nobody disputed. A reviewer caught this.
2. The second used a plain background job, which died. Not from our signal: `zsh` relays
   a hangup to its own jobs when it exits. Only a job that ignores SIGHUP isolates the
   mechanism under test.

### A2. The containment set must be captured before the leader dies

This was not predicted by anyone and it changes the design.

Enumerating the terminal's processes by controlling terminal device finds five members
including the job **before** teardown. After the leader dies it finds **zero**, because
POSIX disassociates a controlling terminal from its session when the controlling process
terminates.

An implementation that re-enumerates before each escalation step, which is what ADR-014
said at the time, would enumerate an empty set from the second step onward and contain
nothing at all. The containment set has to be a monotonic union across rounds, captured
before the first signal.

Signalling every distinct process group in that union does kill the job, so the mechanism
works once the ordering is right.

The enumeration API also had to be corrected twice: `KERN_PROC_SID` does not exist, the
constant is `KERN_PROC_SESSION`, and `sysctl` with it returns `kinfo_proc`, which the
`libc` crate does not expose on Apple platforms. The implementable route is
`proc_listallpids` plus `proc_pidinfo(PROC_PIDTBSDINFO)` filtered on `e_tdev`, which
answered for 110 of 174 pids in a direct probe; the rest belong to other users.

### B. The reap blocker is undrained output, not the open master

Confirmed. A child that writes 256 KiB and exits is not reapable after four seconds while
the parent holds the master and never reads. Draining it, with the master still open, reaps
it immediately.

Boundary worth recording: the first version of this test wrote two bytes, and an external
reviewer running it under load saw it reap anyway. That is correct behaviour, not a flake.
A small write is usually already in the master's queue by the time the slave closes, so
nothing is left to wait for. What blocks the reap is output not yet absorbed, which needs a
write larger than the buffer. The rule the ADR draws is unchanged; the test is what had to
become deterministic.

Consequence: ADR-014's "release the PTY master before signalling" is wrong twice. It
misidentifies the cause, and it discards the child's final output, which is exactly the
shell logout message or the agent's last summary that a user needs to see. The correct
rule is to keep draining the master until the reap completes, and close it last.

### C. End of stream is not `WouldBlock`

Confirmed. After the child exits and the master is drained, `poll` reports
`POLLIN | POLLHUP` and `read` returns 0 on every round, indefinitely, with no error. On
macOS there is no `EIO`. Under `AsyncFd`, `try_io` returning `Ok(Ok(0))` does not clear
cached readiness, so a loop written to the letter of ADR-002's original rule spins one
core at 100 percent forever, on the idle path, after the child is gone.

Consequence: the readiness rule must classify the full set. `WouldBlock` means wait again.
`Ok(0)` and `EIO` both mean end of stream, deregister and stop. Anything else is fatal to
the terminal.

### D. The line discipline discards a paste with no line terminator

Confirmed. One mebibyte of a single character with no newline, written to a canonical-mode
child, is reported as fully written with zero `EAGAIN` and the child receives nothing. The
newline-terminated control delivers 100 percent through the same path.

Consequence: a gate asserting "every pasted byte arrives at the child" is unachievable
against a canonical-mode child, and would pass or fail depending on whether the fixture
happens to contain newlines. The daemon's own byte-bounded queue and escape-aware chunker
are not what fails here. The gate must assert delivery to the terminal, assert byte-exact
delivery for a raw-mode or bracketed-paste child, and separately assert that the daemon
detects the canonical-mode condition and reports it rather than claiming success.

## spike-mem: what one terminal actually costs

```text
engine: alacritty_terminal 0.26, 100 terminals per measurement, current RSS (not maxrss),
one configuration per process so no row inherits another's high-water mark
 80x24  scrolling_history=1000  never scrolled                         102 KiB per terminal (history_size=0)
 80x24  scrolling_history=1000  filled                                2143 KiB per terminal (history_size=1000)
200x50  scrolling_history=1000  filled                                5711 KiB per terminal (history_size=1000)
200x50  scrolling_history=200   one line scrolled, 200 line budget     523 KiB per terminal (history_size=0)
200x50  scrolling_history=200   filled to a 200 line budget           5710 KiB per terminal (history_size=200)
200x50  scrolling_history=1001  filled, 1001 lines                   10884 KiB per terminal (history_size=1001)
200x50  scrolling_history=0     engine history disabled                524 KiB per terminal (history_size=0)

the step between a 200 line budget and a 1000 line budget is the point: the engine
allocates in 1000 row chunks, so a line count below 1000 buys no memory back
exit=0
```

The plan's original budget of 512 KiB per idle terminal is wrong by a factor of four at
80x24 and eleven at 200x50, and by ninety at 200x50 with a 10 000 line scrollback.
Roughly 25 bytes per cell of retained scrollback is the real number.

Two consequences, both structural rather than a tuning change:

1. The default in-memory scrollback size is now a first-order product decision, because
   it is the dominant term in daemon memory. Bounding it by line count, as the engine
   does natively, gives a per-terminal cost that varies by a factor of eleven with column
   count. Bounding it by bytes does not.
2. A gate worded as "100 idle terminals, RSS per terminal below budget" measures the
   empty case, because the engine's storage grows lazily. The first row above costs
   101 KiB and would pass a 512 KiB budget while hiding a twentyfold error. The gate must
   fill every terminal's scrollback before measuring.

## What these results changed

| Document | Change |
|---|---|
| ADR-002 | Withdraw the wakeup-counter evidence. Extend the readiness rule to `Ok(0)` and `EIO`. Defer blocking-work policy to ADR-011 instead of restating it. |
| ADR-003 | Keep the engine. Add the unproven conformance areas, the effects channel, and a byte-bounded in-memory scrollback derived from the measured cost per cell. |
| ADR-009 | Premise was wrong. Rewritten around kernel event sources plus an honest Linux floor. |
| ADR-014 | Containment must enumerate the session, not assume `kill(-pgid)` covers it. Drain the master until reap completes, close it last. |
| Plan, PTY section | Close-on-exec as a stated invariant with an fd-table assertion. Read-buffer sizing against the measured tty queue. |
| Plan, section 7 | Memory budget restated per cell with a byte-bounded ring, and the idle gate must fill scrollback before measuring. |
| Plan, gates | The paste gate rescoped to what the OS permits. A post-exit idle gate added, because that is where claim C lives. |

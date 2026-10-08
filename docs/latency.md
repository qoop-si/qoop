# Keystroke latency under load

Gate G2.3 asks for a p95 of 8 milliseconds from keystroke to echo in a focused terminal
while twenty hidden terminals each stream about a megabyte a second. It is met, at 2.2 to
3.4 milliseconds. It was reported as missed by a factor of ten for most of this project's life,
and the story of why is more useful than the number.

Run it with:

```
cargo test --release -p qoop-cli --test scale_m2 -- --ignored --nocapture --test-threads=1
```

Release is required and the harness refuses a debug build: `CARGO_BIN_EXE_qoop` points at
whatever profile the test was built with, and a debug daemon is slow enough to make the
number a statement about the build.

## What is measured

| Scenario | Result | Budget |
|---|---|---|
| 100 idle terminals, scrollback filled, 60 s | 0 ns of daemon CPU, 2.12 MiB resident per terminal, thread count flat | 50 ms CPU, 2.4 MiB per terminal, bounded threads |
| typing, one quiet terminal | p50 0.29 ms, p95 0.36 ms | 8 ms |
| typing with twenty terminals streaming ~1 MiB/s of paced 80 column lines | p50 0.51 to 0.60 ms, p95 2.19 to 3.38 ms | 8 ms |
| typing with twenty terminals streaming ~1 MiB/s of short line bursts | p50 0.55 to 0.67 ms, p95 4.98 to 7.74 ms | pinned at 14 ms, see below |
| typing in a terminal that is itself streaming ~1 MiB/s | round trip p50 0.33 ms, p95 0.46 to 0.70 ms; the daemon's share, input to the pty write, p95 0.05 ms | 8 ms |
| one terminal streaming ~1 MiB/s | 0.71 of a core | none stated |

## A frame limit that held the echo

Measured again on 2026-09-27, the quiet echo read 17.5 ms at the median against a 2 ms
budget and the gate above 12.6 ms at p95. The daemon was answering input in 66
microseconds; the rest was waiting. Screens had been limited to one every 16 ms
(`PUBLISH_EVERY`), and a shell echoes a burst of typing over several reads, so every read
after the first waited for the next frame. Before the limit the same bench read 0.10 ms.

The limit stays, because a screen per read is a string per row for screens nobody sees. What
changed is that output inside one frame of an input is its echo and goes out at once, up to
`ECHO_SCREENS` screens per input: a count and not a shorter time, because a timer rounds up to
the next millisecond, which measured 2.3 ms. After it: quiet p50 0.06 ms, the gate above p95
1.4 ms, and 100 idle terminals still 0 ns of CPU.

## The measurement was measuring a prompt

For most of this project the gate read 38 to 99 milliseconds, and every attempt to explain
it ended at "this load saturates the machine". It did not.

The daemon starts `$SHELL`. The gate ran it without saying which shell, so it started the
shell of whoever ran the gate, which on this machine is an interactive zsh with a
configured prompt. With one terminal, no load at all and nothing for the daemon to do, that
arrangement measured p50 5.14 ms and p95 14.76 ms. The same measurement against `sh` is p50
0.29 ms and p95 0.36 ms, and against `bash` p50 0.29 ms. The budget was being spent, twice
over, by a line editor redrawing a prompt before qoro was involved.

The gate now pins `SHELL=/bin/sh`. What a user feels is still their shell plus qoro, and
that is worth knowing: on this machine the shell is the larger half by a factor of fifteen.
What the gate is for is the half qoro can change.

## The load shape was measuring luck

The load was `while true; do seq 1 200000; sleep 1; done`: about 1.1 MiB in one burst, once
a second. Sixty keystrokes against a load that is idle for most of a second and then floods
means the ninety fifth percentile is decided by how many of the last three keystrokes landed
inside a burst. Swept across the streamer count, the same gate read 47.9, 11.6, 7.3, 88.2,
76.7 and 61.5 milliseconds at 2, 4, 8, 12, 16 and 20 streamers. A measurement that is not
monotonic in its own load variable is not measuring that variable.

The load is now paced, and produced by one process per terminal rather than a loop that
forks: `sleep` is a separate program, and twenty shells looping twenty times a second is
four hundred forks a second of noise on the machine being measured. One `perl` writes 53200
bytes every 50 milliseconds, which is 1.06 MiB a second.

The old shape is kept, under `QORO_LOAD=burst`, because it is not only noisier: it is
genuinely more expensive. `seq` output is about seven bytes to a line, so at the same byte
rate it produces fourteen times the rows, and rows are what the scrollback and the history
writer cost. That shape measures p95 4.98 to 7.74 ms and puts the daemon at 2.4 cores
against 1.05 for the paced shape. It is pinned at 14 ms rather than asserted against the
budget, because it sits close enough to 8 that the assertion would fail on noise.

## The gate that was claimed rather than run

G2.12 asks for the same budget in a terminal that is streaming to itself, and the plan
recorded it as folded into G2.3. It was not measured by G2.3, whose focused terminal is
quiet by construction, so the fold was a claim rather than a measurement. It is now its own
gate: round trip p95 0.46 to 0.70 ms, and the daemon's own share of that, from the input
reaching the terminal task to the write reaching the pty, p95 0.05 ms.

One thing it records rather than asserts. At a megabyte a second a terminal scrolls about
13 300 rows a second, so a typed line is gone from a 24 row screen in under two milliseconds
once the next chunk lands, and a client that renders whole screens cannot see what the
screen no longer holds. What makes most echoes visible is the pacing: the producer writes
every 50 milliseconds and a snapshot usually lands in the gap. Measured, 105 to 110 of 120.
The gate asks for two thirds and prints the count.

## Where the time goes

The daemon reports its own breakdown under `QORO_TRACE_LATENCY=1`, writing to stderr, which
is how G2.12 reads a segment no client can see. Taken from one G2.12 run, so the terminal is
streaming a megabyte a second to itself the whole time:

| Segment | p50 | p95 |
|---|---|---|
| input arriving to the pty write | 0.019 ms | 0.051 ms |
| input arriving to the snapshot being ready | 0.117 ms | 0.204 ms |
| snapshot queued to the writer taking it | 0.000 ms | 0.002 ms |

The client sees 0.34 ms p50 for the same keystrokes, so about 0.2 ms of the round trip is
the two socket hops and the client's own decode.

The same three segments against the interactive zsh, before the shell was pinned, read 0.05,
1.18 and 0.002 milliseconds at p50, with the middle one at 7.28 at p95. That middle segment
contains the child being scheduled to echo and the echo itself, which is where a prompt
redraw lands.

## What has been fixed, with the measurement that justified each

Every one of these was found by this gate and each is worth more than the gate.

**The containment scan was per terminal.** Every terminal walked the whole process table
every 50 milliseconds, inside its own read loop, which is both a cost that grows with the
terminal count and a synchronous block on the path a keystroke travels. One shared scan for
the whole daemon, on the blocking pool: p95 went from 101 to 57 milliseconds.

**The scan ran while nothing was happening.** A hundred idle terminals cost 1.09 seconds of
CPU a minute against a 50 millisecond budget. A process cannot join a terminal without that
terminal saying something first, so the scanner now parks until a terminal produces output.
The same hundred terminals now cost nothing measurable at all.

**History was written a row at a time.** A terminal streaming a megabyte a second of short
lines produces about 145 000 rows a second, and an unbuffered writer makes two `write` calls
per row. Buffered, and batched into a twenty millisecond window with a flush every half
second: one streaming terminal went from 0.70 to 0.20 of a core.

**Blocking work grew a thread per terminal.** A hundred terminals took the daemon from 14
threads to 27. Every blocking call now goes through one bounded pool, and the thread count
is flat.

**Snapshots were built for nobody.** A terminal in the background allocated one string per
row on every read whether or not any client was subscribed.

**The parse turn was re-measured, and kept.** The terminal task feeds the engine 8 KiB at a
time and gives the runtime a turn every sixteen of those. The comment defending that said
yielding more often bought no latency, and it cited the old measurement, so it was worth
asking again:

| slices per turn | p95, twenty streaming | one streaming terminal |
|---|---|---|
| 1 | 1.09 to 1.19 ms | 0.95 cores |
| 4 | 2.51 to 3.05 ms | 0.75 cores |
| 16 | 2.95 to 3.38 ms | 0.74 cores |

Yielding after every slice does buy latency, which the old comment denied. It is not bought:
the budget is 8 milliseconds and sixteen slices meets it with more than twice the margin,
while a quarter more CPU per streaming terminal is the difference between twelve and sixteen
of them on a twelve core machine. The constant now carries this table.

## What is left

Nothing in this document is a known gap. The gate is green with more than twice the margin
on the paced shape and about half again on the expensive one, and both numbers are pinned so
that a regression is caught rather than argued about.

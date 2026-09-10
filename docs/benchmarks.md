# Benchmark record

The milestone 1 benchmark harness is `crates/qoro-bench`. It measures the scenarios in
section 7 of the master plan and prints both a table and one JSON line per scenario, so a
run can be diffed against an earlier one without anyone retyping a number.

Run with:

```
cargo run --release -p qoro-bench
```

Every figure below came from that command. Nothing here is a target.

## Run of 2026-09-10

```
qoro binary under test: /Users/me/coding/qoro/target/debug/qoro
git rev: unavailable (no commit checked out, or not a git checkout)
input latency path: real socket, real pty, real shell echo; daemon in this process, so one process boundary and the client's terminal handling are not included

scenario                                           unit        p50        p90        p95        p99        max     n  budget
qoro --version                                       ms      2.573      2.729      2.851      3.272      3.272    50  PASS p95 2.851 vs budget 50.000 (p95 under 50 ms, section 7)
attach to a running daemon                           ms      4.060      4.234      5.473      5.590      5.590    30  PASS p95 5.473 vs budget 75.000 (p95 under 75 ms, section 7)
input latency, local, quiet                          ms      0.232      0.266      0.273      0.302      0.326   200  PASS p50 0.232 vs budget 2.000 (p50 under 2 ms, section 7)
input latency, streaming ~1 MiB/s to itself          ms      0.277      0.427      0.502      0.595      0.630   200  PASS p95 0.502 vs budget 8.000 (p95 under 8 ms, section 7)
memory per terminal, 80x24 scrolled                 KiB   2544.000   2576.000   2576.000   2576.000   2576.000     3  ---- no stated budget
memory per terminal, 200x50 scrolled                KiB   5760.000   5792.000   5792.000   5792.000   5792.000     3  ---- no stated budget

{"name":"qoro --version","unit":"ms","p50":2.573042,"p90":2.729416,"p99":3.27225,"samples":50,"timestamp_unix":1789002854,"git_rev":null,"error":null}
{"name":"attach to a running daemon","unit":"ms","p50":4.059959,"p90":4.233583,"p99":5.590125,"samples":30,"timestamp_unix":1789002854,"git_rev":null,"error":null}
{"name":"input latency, local, quiet","unit":"ms","p50":0.23225,"p90":0.265792,"p99":0.302167,"samples":200,"timestamp_unix":1789002854,"git_rev":null,"error":null}
{"name":"input latency, streaming ~1 MiB/s to itself","unit":"ms","p50":0.277,"p90":0.427292,"p99":0.594792,"samples":200,"timestamp_unix":1789002854,"git_rev":null,"error":null}
{"name":"memory per terminal, 80x24 scrolled","unit":"KiB","p50":2544.0,"p90":2576.0,"p99":2576.0,"samples":3,"timestamp_unix":1789002854,"git_rev":null,"error":null}
{"name":"memory per terminal, 200x50 scrolled","unit":"KiB","p50":5760.0,"p90":5792.0,"p99":5792.0,"samples":3,"timestamp_unix":1789002854,"git_rev":null,"error":null}
```

Notes printed by the same run:

```
note: 7 R-prefixed echoes were scrolled away before any snapshot carried them and are excluded as unobservable, not as slow
```

The per-terminal memory scenarios are reported and not checked against section 7's figure,
because they are not the same measurement: the budget was derived from a process holding a
hundred terminals, where allocator reuse amortises across them, and these are one
terminal's delta in a fresh process. Gate G2.1 measures the budgeted quantity, at a hundred
terminals, and reports 2.12 MiB each.

## What the input latency scenarios do and do not include

They drive a real daemon over a real unix socket, through a real pty, and wait for the
shell's echo to appear in a rendered snapshot. The daemon runs inside the benchmark process,
so one process boundary and the client's own terminal handling are excluded. The version and
attach scenarios do run the shipped binary, which is why they are three orders of magnitude
slower.

## Echoes that never arrive are not slow round trips

In the self-streaming scenario, about one echo in two hundred never appears in any snapshot.
That was first recorded at the timeout, which reported a two second latency. Raising the
timeout to twelve seconds changed nothing: the same fraction still never arrived. The cause
is not slowness, it is the render lane coalescing per terminal (ADR-013) while the terminal
scrolls a megabyte a second, so the state that showed the echo is superseded before it is
written to the socket.

Those samples are now counted as unobservable and excluded, with the count printed. The
quiet scenario has no such effect, so a timeout there is a real sample and is recorded at
the timeout rather than dropped.

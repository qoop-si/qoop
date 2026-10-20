# How it is built

## What is measured

Numbers, from `cargo run --release -p qoop-bench` and the scale gates, on a twelve core
machine, measured on 2026-09-27 in one release run with other builds sharing the machine
(load average 8 to 17). Nothing here is a target.

| | |
|---|---|
| 100 idle terminals, 60 s | 0 to 10 ms of daemon CPU across runs, against a 50 ms budget |
| memory per scrolled terminal | 1.2 to 2.2 MiB resident at 80 columns, over a hundred of them, across runs |
| attach to a terminal with 7.1 million rows of history | 145 microseconds |
| keystroke to echo, quiet | 0.06 ms at the median |
| keystroke to echo, twenty terminals each streaming 1 MiB/s | 1.4 ms at the ninety fifth percentile, against an 8 ms budget |
| `qoop --version` | 3.1 ms at the ninety fifth percentile |

That last row was the hard one, and for most of this project it read 38 to 99 milliseconds.
None of that was the code. The measurement started whatever shell the person running it
used, so it was timing an interactive prompt redraw, and it delivered its megabyte in one
burst a second, so the percentile came down to which keystrokes happened to land in a burst.
[docs/latency.md](https://github.com/qoop-si/qoop/blob/main/docs/latency.md) has both mistakes, the numbers before and after, and the
one change the corrected measurement then argued for.

## Decisions and gates

Every decision that was hard is written down as a decision record in
[docs/adr](https://github.com/qoop-si/qoop/blob/main/docs/adr), with what was measured and what the first version got wrong. Several of
them are corrections: the containment identity read from the wrong end of a pty, teardown
that ended when the child was reaped, a parser bound that a three byte prefix walked past.

The keyboard corpus gate G1.3 checks key encodings against, and where each row came from:
[docs/keyboard-corpus.md](https://github.com/qoop-si/qoop/blob/main/docs/keyboard-corpus.md). Every benchmark run so far:
[docs/benchmarks.md](https://github.com/qoop-si/qoop/blob/main/docs/benchmarks.md).

Every claim about this program is a gate, and a gate is a command that exits zero. They live
in the tests next to the code they are about, and each one names itself in the test's own
doc comment.

```
cargo test --workspace     # the gates that do not need a wall clock

# what a person does, driven through tmux, against the release binary
cargo build --release -p qoop-cli && ./scripts/acceptance-m1.sh
```

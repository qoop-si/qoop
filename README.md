# qoro

A portable runtime for human and coding-agent workspaces.

```
cd my-project
qoro
```

That reaches a real shell, with an index of your terminals beside it. No account, no wizard,
no manifest. Press ctrl-q and the shell keeps running; run `qoro` again and you are back in
the same one.

```
 1 zsh              │~/my-project $ cargo test
>2 claude   working │
 3 sh               │
                    │
ctrl-a n next  ctrl-a c new  ctrl-a x close  ctrl-a d detach
```

The index says which terminal each agent is in and what it is doing, and it changes when the
process does, including for a command that prints nothing at all. `ctrl-a` then a key moves
between terminals, makes one or closes one. Nothing polls: eight terminals open and nobody
typing costs 105 microseconds of client CPU a minute.

Runs on macOS and Linux. On Windows, under WSL. There is no native Windows build, and
[ADR-021](docs/adr/ADR-021-windows.md) says what one would have to include.

## What it is for

A terminal multiplexer is a good place to keep a coding agent, and a bad one to keep several.
The things that go wrong are the same every time: the agent finished twenty minutes ago and
nobody noticed, the session came back with the right command line and the wrong session, a
background job outlived the window that started it, and an update restarted everything.

So the parts that usually get bolted on are the parts this is built out of. A terminal knows
which agent is in it and which generation of that agent. A workspace can be captured and run
somewhere else, exactly, including the agent's own session. What scrolls off goes to disk as
rows. Closing a terminal contains what it started.

## Getting started

```
cargo build --release
./target/release/qoro
```

`qoro --help` lists the whole command surface, which is also written down in
[docs/cli-surface.md](docs/cli-surface.md) and checked against the binary by a test.

## What is measured

Numbers, from `cargo run --release -p qoro-bench` and the scale gates, on a twelve core
machine. Nothing here is a target.

| | |
|---|---|
| 100 idle terminals, 60 s | 10 ms of daemon CPU, against a 50 ms budget |
| memory per scrolled terminal | 2.07 MiB at 80 columns, over a hundred of them |
| attach to a terminal with 7.1 million rows of history | 211 microseconds |
| keystroke to echo, quiet | 0.25 ms at the median |
| keystroke to echo, twenty terminals each streaming 1 MiB/s | 2.8 ms at the ninety fifth percentile, against an 8 ms budget |
| `qoro --version` | 3.6 to 4.8 ms at the ninety fifth percentile, across runs |

That last row was the hard one, and for most of this project it read 38 to 99 milliseconds.
None of that was the code. The measurement started whatever shell the person running it
used, so it was timing an interactive prompt redraw, and it delivered its megabyte in one
burst a second, so the percentile came down to which keystrokes happened to land in a burst.
[docs/latency.md](docs/latency.md) has both mistakes, the numbers before and after, and the
one change the corrected measurement then argued for.

## How it is built

Every decision that was hard is written down as a decision record in
[docs/adr](docs/adr), with what was measured and what the first version got wrong. Several of
them are corrections: the containment identity read from the wrong end of a pty, teardown
that ended when the child was reaped, a parser bound that a three byte prefix walked past.

Every claim about this program is a gate, and a gate is a command that exits zero. There are
eighty seven of them, they live in the tests next to the code they are about, and each one
names itself in the test's own doc comment.

```
cargo test --workspace     # the gates that do not need a wall clock

# what a person does, driven through tmux, against the release binary
cargo build --release -p qoro-cli && ./scripts/acceptance-m1.sh
```

## Licence

AGPL-3.0-or-later.

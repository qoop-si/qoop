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
 needs you                  1 │ 1 zsh  2 claude  +
 ● claude          my-project │~/my-project $ cargo test
──────────────────────────────┤
 spaces                 tree +│
─▾ my-project ──────────── 1 ─│
   ● claude                   │
─· notes ─────────────────────│
terminal 2 is blocked
```

The index says which space each agent is in and what it is doing: every space is a rule
with its agents under it, and `ctrl-a f` folds one away. An agent waiting for an answer is
pinned at the top, lights its row and rings the bell, so you find out without looking, even
when its space is folded. The name is the one you would say, not the runtime underneath it:
an agent installed as a node package reads as itself, not as `node`.

`ctrl-a` then a key moves between terminals, makes one or closes one; `ctrl-q` detaches. The
bottom row names the keys that work where you are, and every command while `ctrl-a` waits
for its letter. Under a working agent the column says what it is doing (`editing
sidebar.rs`, `cargo test`), from the agent's own hook. `ctrl-a M` opens the focused agent's
last answer as a page, and `QORO_USAGE=1` shows what each agent costs in CPU and memory.
Nothing polls: eight terminals open and nobody typing costs 105 microseconds of client CPU a
minute, and the state is read off a terminal's screen only once it stops changing. The one
motion is the spinner and sweep beside an agent that is working and on screen: about 0.3
percent of a core while they turn, nothing once the agent stops, and `QORO_MOTION=0` or
`NO_COLOR` keeps them still.

On a Mac the same news sits in the notch, with no window open: your pet, an amber badge
counting the agents that need you, and how many are working and idle. A click drops the
list, and a click on a row switches the interface to that terminal and brings its window
forward (not when the interface runs inside tmux, whose server has no window to find; the
switch still happens). The installer turns it on; `qoro notch uninstall` turns it off, and
`qoro notch install` back on. It is built into the macOS binary, so it needs no download or
signature of its own.

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

Put it somewhere on your path and install the hooks and the skill for the agents you use:

```
qoro hooks install
qoro skills install
```

That writes each agent's hook and adds one entry to that agent's own configuration, beside
whatever is already in it. The skill lands beside the agent's own skills the same way.
Both are optional: every agent is read from its screen without a hook, and the skill
only teaches an agent what the commands are for. What a hook buys is a state known
the moment it changes rather than after it is drawn, and the agent's own id for its
conversation, which is what lets a stopped session come back to that conversation
rather than to whichever one in the directory is newest. `qoro hooks status` says
what is installed and `qoro hooks uninstall` takes out exactly what was added;
`qoro skills status` and `qoro skills uninstall` do the same for the skill.
How a screen is read, and how to override a misreading, is in
[docs/manifests.md](docs/manifests.md); what each hook installs is in
[docs/hooks.md](docs/hooks.md).

On a fresh Mac or Linux box, `scripts/install.sh` does all three in one go: binary, hooks,
skill. Run it as yourself, not with sudo: the binary goes to `~/.local/bin/qoro` (or
`QORO_DEST`), and it says so if that directory is not on your PATH. The releases are
private, so it and `qoro update` need a GitHub token: `GH_TOKEN` or `GITHUB_TOKEN`, or a
`gh` login.

Sessions are named, and a stopped one comes back as itself:

```
qoro --session work     open one, or make it
qoro ls                 what there is, and what is running
qoro stop work          end it: the tabs come back on the next open
```

The interface reads `~/.config/qoro/config.toml` (or `$XDG_CONFIG_HOME/qoro/config.toml`)
when it starts and again whenever the settings box opens (`ctrl-a m`, settings). Every
key is optional:

```toml
prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
theme = "auto"      # auto | dark | light
[notify]
desktop = true      # tell the desktop when an agent needs you and you are not looking
sound = false
```

A value it cannot use keeps its default and is named on the hint row. The settings box
shows what is in effect and where a notification would go. How long an idle terminal
lives stays the daemon's `QORO_IDLE_TIMEOUT_MINUTES`.

### Other machines

A qoro on a server keeps running when you log out, like the one here does. Add it once and
its spaces are listed beside this machine's. From the interface, the `+` beside "spaces"
offers `connect a machine`: type its ssh address, pick one of the sessions already running
there or name a new one, and the screen moves to it. From a shell:

```
qoro remote add devbox me@devbox      runs ssh in the foreground, checks the qoro there
qoro                                  devbox:api, devbox:web under this machine's spaces
qoro --remote devbox                  only that machine
```

It is the system `ssh` with your own `~/.ssh/config`, so jump hosts, keys and agent
forwarding work as they do for `ssh devbox`. The interface itself never asks for anything:
a key in your ssh agent just works, and after `qoro remote add` or `qoro --remote`, which
can ask, ssh keeps that connection shared for ten minutes. A host that would need a password
or a touch outside that shows as offline with ssh's reason. Everything in a remote space runs
over there: a new tab, a split, a space made with `ctrl-a w` while you are in one. Naming a
new space `devbox:tests` from anywhere makes it on devbox. If the connection drops, its
spaces read as offline within about fifteen seconds and come back on their own, with the same
tabs, at most ten seconds after the network does, or at once when you reach for one of them.
A machine with no qoro is offered one: this version, for its platform, put in its
`~/.local/bin` over the same ssh and run once there before it is kept. A release downloaded
for another platform is checked against its checksum first. A qoro already there is never
replaced. `qoro agents`, `send` and `read` still
speak only to this machine's daemon.

### Your phone

`qoro pair --hub wss://your-hub` prints a QR code; the qoro app scans it, shows the six
digits you type back on the Mac, and from then on lists every agent on that Mac with the one
waiting for you first. You can answer it, type into any terminal, and open or close one, and
the phone is told when an agent blocks, finishes or fails. The phone and the Mac talk end to
end encrypted through a relay, `qoro-hub`, that sees neither the screens nor the keys, and a
push carries a fixed sentence and four numbers, nothing from a screen. Setup, the hub and
what it can see: [docs/mobile.md](docs/mobile.md).

`qoro --help` lists the whole command surface, which is also written down in
[docs/cli-surface.md](docs/cli-surface.md) and checked against the binary by a test.

Prebuilt binaries for macOS (Apple silicon and Intel) and Linux (x86_64 and arm64, glibc
2.17 and newer) live on GitHub Releases, all built on one Mac with `./scripts/release.sh
0.1.0`: GitHub only hosts the files, nothing is compiled there. `qoro update --check` notes
what is newest, the sidebar signs itself `0.3.0>0.4.0` while something is, `ctrl-a U` says
how it installs, and `qoro update` installs it once no session is running. How a release is
cut and how an update lands: [docs/update.md](docs/update.md).

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

The keyboard corpus gate G1.3 checks key encodings against, and where each row came from:
[docs/keyboard-corpus.md](docs/keyboard-corpus.md). Every benchmark run so far:
[docs/benchmarks.md](docs/benchmarks.md).

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

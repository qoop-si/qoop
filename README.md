# qoop

qoop keeps your coding agents (Claude Code, Codex and the like) in one terminal, shows what
each one is doing, and tells you when one needs you, on your computer or on your phone. Close
the window and they keep working; open it again and everything is where you left it.

## Get started

You need a Mac or a Linux computer, and an agent you already use, such as
[Claude Code](https://docs.anthropic.com/en/docs/claude-code) or
[Codex](https://github.com/openai/codex). qoop runs it; it does not replace it.

1. **Get your install line.** Sign in at [qoop.si/app](https://qoop.si/app). The page shows a
   line made for you (it needs a seat: ask for one at [qoop.si](https://qoop.si)). Paste it
   into Terminal:

   ```
   curl -fsSL https://qoop.si/i/<your link> | sh
   ```

   It puts `qoop` in `~/.local/bin`, adds a small hook to each agent it finds, and asks a few
   questions (your pet, sounds, light or dark). Every answer can be changed later with
   `qoop settings`.

2. **Sign this computer in**, so your account knows it is yours:

   ```
   qoop login
   ```

   It prints a link and a code. Open the link, sign in, type the code, then answer `y` in
   Terminal.

3. **Open your project in qoop:**

   ```
   cd my-project
   qoop
   ```

   You are in a shell inside qoop. Type `claude` (or `codex`) there to start an agent.

   The column on the left lists your agents. A dot pattern beside each one says what it is
   doing: breathing while it works, an empty ring when it wants an answer from you.

4. **Close the window whenever you like** (`ctrl-q`). The agent keeps working. Type `qoop`
   to come back.

5. **Optional:** open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the
   same account to see and answer your agents from anywhere, and add a server with
   `qoop remote add <name> <user@host>` so agents keep working while your laptop sleeps.

While no agent is running, the row above the keys says how to start one.

## Words you will see

| word | what it means |
|---|---|
| agent | an AI coding program, such as Claude Code or Codex, running in one of qoop's tabs |
| tab | one terminal; a tab with an agent in it is listed in the column |
| space | a group of tabs for one project, named after it |
| qass | qoop's own helper: it watches your agents, answers the easy questions the way you would, and passes the rest to you |
| server | another computer you reach with ssh; its spaces show up beside your own |

## Keys you will use

Most keys are `ctrl-a`, then one letter.

| keys | what they do |
|---|---|
| `ctrl-a c` | a new tab |
| `ctrl-a tab` | go to the agent that needs you |
| `ctrl-a space` | find anything: a tab, an agent, a space |
| `ctrl-a Q`, or a click on the pet | open qass, or go back |
| `ctrl-a i`, or a click on qass's `asks you` count | the questions qass keeps for you, beside qass's chat: one key answers |
| `ctrl-a ?` | every key, with a search |
| `ctrl-q` | leave; everything keeps running |

## Your phone

Open [qoop.si/app](https://qoop.si/app) on your phone and sign in with the account your
computer signed in with (`qoop login`). Your computers show up by themselves, with the agent
that is waiting for you first. You can answer it, type into any terminal, open or close one,
start an agent in a space, and put a space to sleep or wake it. Notifications need the qoop
app on the phone (the web page cannot send them): turn them on there and the phone tells you
when an agent blocks, finishes or fails.

A phone can also be paired with one computer without an account: run `qoop pair` there,
scan the code it shows with the qoop app, and type the six digits the phone shows into the
computer.

The phone and the computer talk end to end encrypted through a relay that sees neither the
screens nor the keys, and a notification carries a fixed sentence and four numbers, nothing
from a screen. `qoop login`, `qoop pair` and the phone side come with the prebuilt release;
they are not part of this repository, so a build from source has no phone part.

## How it works

```
cd my-project
qoop
```

That reaches a real shell, with an index of your terminals beside it. No manifest to write.
Press ctrl-q and the shell keeps running; run `qoop` again and you are back in
the same one, on the terminal you left: the same space, the same tab, the same pane of a split.

```
 ⠯⠇claude                         12s │ 1 zsh  2 claude  +
 agents                         tree +│~/my-project $ cargo test
▾my-project (1)   ▐+▌ ▐sleep▌ ▐⋯▌ ▐x▌ │
 ⠯⠇claude                             │
   └ claude · my-project              │
                                      │
 notes            ▐+▌         ▐⋯▌ ▐x▌ │
terminal 2 is blocked
```

The index says which space each agent is in and what it is doing: every space is its name
with its agents under it (only agents: every tab is on the bar above the terminal). The name
and the agents' titles wear the space's colour, calm agents a quieter step of it, so a space
reads as one block and the ones working or waiting stand out of it. The mark before a
space's name says it is open (`▾`) or folded (`▸`); a click on the line or `ctrl-a f` folds
it. An agent waiting for an answer is named in the one row at the
top, with a mark for each one waiting after it, and `ctrl-a tab`
goes to it: wants first, then failed, then finished. It also lights its row and rings the
bell, so you find out without looking, even when its space is folded. The name is the one
you would say, not the runtime underneath it: an agent installed as a node package reads as
itself, not as `node`.

The search chip under the title (one row, ember brackets, as wide as the inbox tray above it)
filters the column by space name, agent name, state and directory. `ctrl-a a` gives the column
the keyboard: `j` `k` or the arrows move over its rows,
`enter` opens one, `/` types in the search, `s` sleeps the row, `S` every space, `w` wakes it,
`n` opens a new tab, `x` closes (asking first, as its `x` does), `f` folds, and `q` or `esc`
gives the keyboard back. Each live agent's row ends in a moon: a click puts that one tab to
sleep.

The column opens thirty eight columns wide, and a drag on its edge pulls it anywhere from
twenty four to half the window. Its foot signs it with the q in thick dots, one `●` a cell,
its tail in ember, and your picked pet standing beside it on the
menu row, the last row of the column. A click on the pet is `ctrl-a Q`: qass, or back. A column
too narrow for both falls back to the word in braille.

A space's own line ends in four controls, plain words in the same columns on every line whatever the name
is: `+` starts a terminal or an agent in the space (an agent asks for its name first),
`sleep` puts every tab of the space to sleep and `wake` brings every sleeping tab of it back, `⋯` opens
the space's settings, its name and its colour, and `x` asks whether to close it. The
question is drawn on the line itself, with `y` in the place `x` was in and `no` beside it,
so a second click in the same place answers yes, and a `y` closes the space's tabs and then
the space. `ctrl-a R`
opens the settings too, `ctrl-a Z` sleeps the tab you are in and `ctrl-a x` closes the tab
you are on.

Each agent wears a small square of nine dots in its state's colour. While it works the
centre breathes: its arms come in clockwise and go back out, each breath starting one arm
further round. When it stops the dots settle into a picture: an empty ring when it wants
you, an X when it failed, the full square when it finished, one dot in the middle when it
is idle, a line across the middle while it sleeps. The shape says it as well as the colour, so it reads without colour
too. A split tab names each pane on its top edge and marks only the pane the keyboard is in,
with an ember curve at each of its corners; the agent the keyboard is on wears the same
corners in the column.

`ctrl-a` then a key moves between terminals, makes one or closes one; `ctrl-q` detaches. The
bottom row names the keys that work where you are, and every command while `ctrl-a` waits
for its letter. Under a working agent the column says what it is doing (`editing
sidebar.rs`, `cargo test`), from the agent's own hook. `ctrl-a M` opens the focused agent's
last answer as a page, and `QOOP_USAGE=1` shows what each agent costs in CPU and memory.
Nothing polls: eight terminals open and nobody typing costs 105 microseconds of client CPU a
minute, and the state is read off a terminal's screen only once it stops changing. The one
motion is the filling ring and sweep beside an agent that is working and on screen: about 0.3
percent of a core while they turn, nothing once the agent stops, and `QOOP_MOTION=0` or
`NO_COLOR` keeps them still.

On a Mac the same news sits in the notch, with no window open: your pet, an amber badge
counting the agents that need you, and how many are working and idle, across every
running session (a session started later shows up as it starts). A click drops the list,
each row naming its session when there is more than one; a click on a row opens that agent's terminal right under it, live and in its own
colours, the same grid the interface draws and never resized for it. Scrolling up goes
back past the screen: into the terminal's history, or for an agent on the full screen,
into its conversation. A click on it types into it: every key goes to that terminal as
pressed, so arrows move through an agent's menu, Enter picks, Esc cancels, and a click
outside stops. When qoop cannot read a terminal, the box says what qoop said about it, or
that it could not read it, instead of sitting empty. "open in qoop" switches the interface
there and brings its app forward
(inside tmux the switch happens and nothing is raised, since the tmux server has no app to
find). The gear in the list opens a settings window: the notch on or off, notifications,
theme, prefix key and pet, all written to `~/.config/qoop/config.toml`, which `qoop
config` prints and `qoop config set` changes. The installer turns the notch on; `qoop notch
uninstall` turns it off, and `qoop notch install` back on. It is built into the macOS binary, so it needs no download or signature
of its own.

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

## Build from source

The prebuilt release (see Get started) is the easy way. To build it yourself:

```
cargo build --release
./target/release/qoop
```

Put it somewhere on your path and install the hooks and the skill for the agents you use:

```
qoop hooks install
qoop skills install
```

That writes each agent's hook and adds one entry to that agent's own configuration, beside
whatever is already in it. The skill lands beside the agent's own skills the same way.
Both are optional: every agent is read from its screen without a hook, and the skill
only teaches an agent what the commands are for. What a hook buys is a state known
the moment it changes rather than after it is drawn, and the agent's own id for its
conversation, which is what lets a tab that slept or stopped come back to that
conversation rather than to whichever one in the directory is newest. `qoop hooks status` says
what is installed and `qoop hooks uninstall` takes out exactly what was added;
`qoop skills status` and `qoop skills uninstall` do the same for the skill.
How a screen is read, and how to override a misreading, is in
[docs/manifests.md](docs/manifests.md); what each hook installs is in
[docs/hooks.md](docs/hooks.md).

On a fresh Mac or Linux box, `scripts/install.sh` does it all in one go: binary, hooks,
skill, and on a Mac the notch and the font of qoop's icons. Run it as yourself, not with sudo: the binary goes to `~/.local/bin/qoop` (or
`QOOP_DEST`), and it says so if that directory is not on your PATH. The releases are
read from `https://qoop.si/releases`, which `qoop update` reads too; no token is needed.

## qass

The skill install also brings qass, for every agent above, an assistant that sits above every
other agent: it is told when one stops, reads where that work stands (its task, its plan
file, the repo's rules), answers what it can the way you would, and sends you only what
needs your word. You answer it in its own tab, and it passes your answer on. Start it in
its own space:

```
qoop qass
```

It opens qass in its own space on the agent `qoop settings` names (`qass runs on`: claude by
default, codex, gemini, opencode, or one of your own `[agents]` lines, such as
`claudes = "claude --model sonnet"`), or shows or wakes the one you have. Close it with the
`x` on its row.
Its rules live in the skill itself: what it answers alone, what it asks you, and what it
never does. It writes to an agent within those rules without asking you first; tell it "ask me
first" and it keeps that until you say otherwise. In a repository whose GitHub
`origin` belongs to the login `gh` is signed in as, every agent is told the standing way
(`qoop task rules`): commit through /commit, push, open a PR, rebase-and-merge its own PR,
install and clean up; elsewhere it asks first, and delete, release, production, money and
anything external always ask. A repository's `AGENTS.md` may ask for more, never allow
more. When you correct it or say how you want things, it keeps the rule in
`$(qoop paths qass-config)/lessons.md` (3,000 characters at most, loaded at every start); a lesson can
let it settle a kind of reversible step at once, never anything that always asks. `qoop paths`
lists every folder qoop writes to and how big each is.

## Sleep

A tab you are not using can sleep: it ends, nothing of it takes CPU or memory, and waking
it brings back the same number, name, split, directory and scrollback, the last screen
included, with its agent resumed into its own conversation. The rest of its space is left
as it is. A sleeping tab stays in its space, dimmed, on the bar and in the column, and keeps
the name its conversation had. A click on it wakes that tab alone and opens it. Reaching
for it from the palette shows the last screen it drew, dimmed, without waking anything:
Enter or `ctrl-a Z` wakes it once you have seen where it was. A program on the alternate
screen leaves no last screen behind, and its tab says so.

A tab sleeps whole, every pane of its split, or not at all: a command still running, an
agent that is working or has not told its conversation yet, or a job left in the background
keeps it awake, and the refusal says which pane and why. Putting a space to sleep sleeps
each tab that can and says which stayed awake. When nothing is awake and no interface is
attached, the daemon itself leaves ten seconds later, so a machine with everything asleep
runs nothing of qoop; the next `qoop` starts it and wakes the tab that slept last.

A space's row shows `+` and `⋯`; the `⋯` slides out the rest of its controls (sleep or
wake, keep awake, settings, close) and turns into `›` to put them back. With `[sleep]
idle_minutes` set, the cup on an agent's row or in a space's drawer keeps it out of idle
sleep: click it and it fills, click again and idle sleep may take it. A full cup or a
sleeping space's moon stays beside `+` while the drawer is closed. A sleep you ask for still
sleeps it.

```
ctrl-a Z                sleep the tab you are in, or wake the sleeping one you are on
ctrl-a a                the keyboard on the agents column: move, open, search, sleep
qoop sleep --tab 7      the same from a shell, by tab number
qoop sleep api          every tab of a space, by name or id
qoop wake --tab 7       wake one tab; `qoop wake api` every tab of a space
qoop wake               wake the tab that slept last
qoop ls                 every space, awake, asleep, partly asleep or stopped, and its tabs
qoop --space api        the interface with that one space in it; `qoop` shows them all
qoop stop               end everything now: what was awake comes back on the next open
```

## Settings

The interface reads `~/.config/qoop/config.toml` (or `$XDG_CONFIG_HOME/qoop/config.toml`)
when it starts, and again within two seconds of any change to it (`qoop settings`, also on
`ctrl-a m`, settings; `qoop config set`; an editor). Every key is optional:

```toml
prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
theme = "auto"      # auto | dark | light
icons = "auto"      # auto | on | off: icons where they were seen to draw, plain elsewhere
[notify]
desktop = true      # tell the desktop when an agent needs you and you are not looking
sound = false
[sleep]
idle_minutes = 0    # 0 = never; else a tab idle and unwatched this long sleeps
[daemon]
exit_when_asleep = true   # leave when nothing is awake and nobody is attached
```

A value it cannot use keeps its default and is named on the hint row. `qoop settings`
shows every setting with your pet and changes it as you go, and where a notification would
go; `qoop onboard` walks through the main ones on a first start. `[sleep]` and `[daemon]` are
the daemon's own, read by it from the same file; every key is in
[docs/config.md](docs/config.md).

## Other machines

A qoop on a server keeps running when you log out, like the one here does. Add it once and
its spaces are listed beside this machine's. From the interface, the `+` beside "spaces"
offers `connect a machine`: type its ssh address and the screen moves to it. From a
shell:

```
qoop remote add devbox me@devbox      runs ssh in the foreground, checks the qoop there
qoop                                  devbox:api, devbox:web under this machine's spaces
qoop --remote devbox                  only that machine
```

It is the system `ssh` with your own `~/.ssh/config`, so jump hosts, keys and agent
forwarding work as they do for `ssh devbox`. The interface itself never asks for anything:
a key in your ssh agent just works, and after `qoop remote add` or `qoop --remote`, which
can ask, ssh keeps that connection shared for ten minutes. A host that would need a password
or a touch outside that shows as offline with ssh's reason. Everything in a remote space runs
over there: a new tab, a split, a space made with `ctrl-a w` while you are in one. Naming a
new space `devbox:tests` from anywhere makes it on devbox. If the connection drops, its
spaces read as offline within about fifteen seconds and come back on their own, with the same
tabs, at most ten seconds after the network does, or at once when you reach for one of them.
Spaces over there sleep the same way, and a machine with everything asleep is let go: its
qoop leaves as this one would, its spaces read as asleep rather than offline, and waking
one connects again.
A machine with no qoop is offered one: this version, for its platform, put in its
`~/.local/bin` over the same ssh and run once there before it is kept. A release downloaded
for another platform is checked against its checksum first. Connecting never replaces a
qoop already there. `qoop agents`, `send` and `read` still
speak only to this machine's daemon.

A machine whose qoop is older than this one follows it on its own. When a release of this
version exists for that machine's platform, the interface puts that release's binary in place
of the old one over the same ssh, in the background, and the daemon over there moves onto it
and its tabs keep running through it where both qoops can hand them over. Otherwise it
moves at the first moment nothing there is working: a running command, a busy agent or a
background job holds it, and the interface says once what it is waiting on. Tabs keep their
numbers and agents come back in their own conversations. A build from main between releases
is never sent anywhere, and a machine whose qoop is from before this needs one update by hand there (`qoop stop`,
then the install script or `qoop update`) and follows on its own after that.

`qoop --help` lists the whole command surface, which is also written down in
[docs/cli-surface.md](docs/cli-surface.md) and checked against the binary by a test.

Prebuilt binaries for macOS (Apple silicon and Intel) and Linux (x86_64 and arm64, glibc
2.17 and newer) live at qoop.si/releases (and on GitHub Releases), all built on one Mac by the release script
of the qoop app (it builds this repository's crates with the phone part): the sites only host
the files, nothing is compiled there. `qoop update --check` notes
what is newest, the sidebar signs itself `0.3.0>0.4.0` while something is, `ctrl-a U` says
how it installs, and `qoop update` installs it and moves every machine onto it, keeping running
tabs running where it can. How a release is cut and how an update lands:
[docs/update.md](docs/update.md).

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
cargo build --release -p qoop-cli && ./scripts/acceptance-m1.sh
```

## Licence

AGPL-3.0-or-later.

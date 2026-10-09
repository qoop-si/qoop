# Releases and updates

No build ever runs on GitHub here, because private Actions minutes are gone.
GitHub only hosts the tarballs. Every build runs on this Mac, and the update runs
wherever the person reading it sits.

## Shipping a release

Releases are built outside this repository: a release is the app build, phone part included.
Each one carries four binaries, `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (the Linux ones against glibc
2.17), packed as `qoop-<triple>.tar.gz` with its sha256. The asset names are the contract:
`qoop update` and `scripts/install.sh` look for exactly `qoop-<triple>.tar.gz` for the
machine they run on (an Apple silicon Mac gets the arm64 build even from a Rosetta shell).

The first release under the name qoop also carries the old names, `qoro-<triple>.tar.gz`
holding a `qoro` file, each with its sha256. The asset is the same binary: the file inside
answers `--version` by the name it is run as, so a 0.7.x `qoro update`, which unpacks the
`qoro` entry and wants `qoro <version>` back, installs it and the new build takes over from
there. Nothing newer than that release needs them.

## Moving from qoro

Everything a machine kept under the name qoro is picked up by the first command of this
build, with nothing stopped. The details are in `crates/qoop-runtime/src/legacy.rs`, which is
deleted once no 0.7.x daemon or agent started before the rename is left:

- `~/.config/qoro`, the data directory and the log directory are renamed to `qoop`, and
  the old name stays a link to the new one.
- `~/.local/bin/qoro` becomes a link to `qoop`. A running 0.7.x daemon moves onto the path it
  was started from, so it lands on this build with its tabs ([Tabs keep running through a
  move](#tabs-keep-running-through-a-move)).
- The new socket path reaches a 0.7.x daemon that is still up; once the daemon is this
  build, the old socket path reaches it, for the phone bridge and any interface left open.
- A database a 0.7.x daemon made keeps the name `qoro.db`, and the state lock keeps the name
  `qorod.lock`: renaming either would strand a rollback and the exclusion between old and new
  daemons. New installs use `qoop.db`.
- `hooks install` and `skills install` replace the hooks and the skill of the old name. The old
  hook script stays on disk, because an agent that is already running keeps calling it; it
  goes with `hooks uninstall`.
- `QOOP_*` variables fall back to the `QORO_*` of a shell, a service or a tab that predates
  the rename, and a tab gets both.
- Codex trusts a hook by its path, and the hook has a new one: after the update, pick "Trust all" (or
  `/hooks`) once in a new Codex session, or it runs without hooks. `hooks install` says so when it
  replaces the old entry.
- The notch's login item moves from `qoro.notch` to `si.qoop.notch`.

To go back to a 0.7.x build, put its binary at `~/.local/bin/qoro` with `mv -f` (a `cp` would write
through the link into `qoop`) and make the
data directory a real one again first: a 0.7.x daemon refuses a data directory that is a link.

With the data directory shown by `qoop paths data` (say `D/qoop`) and its old name `D/qoro`, that is
`qoop stop; rm D/qoro; mv D/qoop D/qoro`. Config and logs can stay links. The database kept its old
name, so the old build finds its tabs.

## Checking and installing

```
qoop update --check   # ask the releases what is newest, note it, change nothing
qoop update           # install the newest release and move every machine onto it
```

`--check` writes the newest version it saw to the notice file beside the
sessions (`update-available` under the runtime directory). The interface never
touches the network; it only reads that file. Any other argument is refused
before anything reaches the network, so a mistyped `--check` never installs.

The releases are read from `https://qoop.si/releases`, anonymously: `<base>/latest` and
`<base>/tags/v<version>` answer in the shape of the GitHub releases API (`tag_name` and
`assets[]` with `name` and `url`), and each asset is fetched by its `url` as it stands.
`scripts/install.sh` reads the same, needs no python (Linux boxes often have none), and falls
back to `sha256sum` where `shasum` is missing. `QOOP_RELEASE_BASE` points both at a mirror, at
GitHub's own API (`https://api.github.com/repos/pat/qoop/releases`) or at a local directory
(`file:///path` holding a `latest` file in the API's shape), which is how the platform pick
is tested with no network. Only the GitHub API gets a token: `GH_TOKEN` or `GITHUB_TOKEN`
from the environment, else whatever `gh auth token` prints, handed to curl on stdin as a
config line (`-K -`, which every curl understands), never on its command line. The redirect
to the storage host is followed by a second request without the token, because curl older
than 7.58 forwarded a custom Authorization header on redirects.

The install replaces the file and then asks each daemon to move onto it. A daemon that can
hand its tabs to the new binary does it at once and nothing running is touched ("Tabs keep
running through a move" below); any other waits until nothing there is working ("Other
machines follow this one"). Either way a working agent or a running command is never ended,
and the update never waits for the move. The tarball is checksum verified when its
`.sha256` asset exists, and a listed checksum that cannot be downloaded is a refusal, not an
unverified install. The downloaded binary must report the expected version before it
replaces anything, and the swap is one atomic rename onto the file itself: launched through
a symlink on PATH, the link is followed and stays a link.

With the network gone the command fails closed on the install: nothing is downloaded, no
notice is left behind, the failure says it could not reach the releases and the exit is nonzero. It still asks the
daemons to move onto the qoop already on disk, which needs no network. That is the one yes
row in `docs/cli-surface.md`, and the gate asserts the failure.

## What the interface shows

The foot of the left column signs the build it is (`qoop 0.3.1`), and when a
check saw something newer it signs where this build stands
(`qoop 0.3.1>0.4.0`). `ctrl-a U` says the same in words: which release is
waiting and that installing it means running `qoop update` outside. The interface itself
never checks: eight terminals open and nobody typing still costs nothing. A release build's
daemon does, next section.

## Updating on its own

A binary the release script built (it sets `QOOP_RELEASE_BUILD=1`; no other build has it, a
copy made by hand included) has its daemon start `qoop update --auto` 3 to 10 minutes after
the daemon starts and every six hours after that. The daemon only schedules: the child does
the network, the install and the ask, is killed after ten minutes, and appends what it says
to `update.log` in the logs directory (`qoop paths logs`; moved aside at 100 KB). A file
named `no-auto-update` in the config directory (`qoop paths config`) turns it off; it is
looked for at every tick, so no restart is needed.

`--auto` is `qoop update` with these differences, and it is not in `--help`:

- It does nothing at all in a build without the release mark, when the directory of the
  binary is not the user's to write, or when there is no release for the platform.
- It refuses a release that lists no checksum, and one whose binary speaks another codec
  generation than this one (that is left to `qoop update`, with the usual notice).
- The swap lock (`.qoop-swap.lock`, the directory the update of a remote takes) is held only
  for the swap. When another update holds it, `--auto` gives way and logs it.
- It asks only this machine's daemon, never the saved remotes, and it asks as a watcher, so it
  is not a window.
- The ask promises that no tab is ended and no window is closed. The daemon then moves only
  when it can hand every tab over live, and only with no `qoop` window, `qoop attach` or phone
  stream connected (watchers, such as the notch and the phone bridge's own, and the Mac's
  reconnecting link to a remote do not count). Otherwise it stays on the old version and says
  why in the answer, and looks again within a minute of whatever held it going away. A
  window that connects while the tabs are being handed over drops the move, and every tab
  carries on.
- Asked with nothing newer released, it asks again whenever the daemon runs an older version
  than the file on disk, so a machine that missed its move catches up within one tick.

A daemon from before 0.9.2 ignores the promises, so it is never sent them; it moves when
`qoop update` is run by hand. The push to a remote carries the same two promises. A person's
own `qoop` window on that machine (an ssh session) holds the push; the Mac's own reconnecting
link does not.

Trust is TLS plus the checksum beside each tarball. It catches corruption, not a compromised
host: the opt out file is the answer for anyone who wants to decide each update by hand. A
release that must not spread is removed from the host (the hub falls back to the one before
it, and a downgrade is never installed).

Each move leaves one zombie process, the finished `--auto` child that asked for it: the
daemon becomes the new binary in place and nothing reaps it. It is harmless and goes with the
daemon.

## Other machines follow this one

A remote machine needs no hand update once its qoop knows how to move. When the interface
connects to a machine whose daemon runs an older version than this Mac's qoop, and a release
of this Mac's version exists for that machine's platform, it fetches that release here
(checksum verified, the same source as above), puts the binary in place of the qoop there in the background with
one atomic rename beside the real file, and asks the daemon over there to move onto it. A
daemon that can hand its tabs over moves at once and they keep running (next sections). Any
other moves at the first moment nothing on it is working: a running command, a busy agent,
an agent that has not named its conversation, or a background job holds it, and the
interface says once, on its hint row, what it is waiting on. When it moves, every tab comes
back with the same number and every agent in its own conversation. A build from main between
releases is never pushed, a qoop there that is as new or newer is never touched, and a swap
another window or another Mac is making at the same time is said rather than raced. A machine
whose daemon is from before 0.5.2 cannot be asked to move, so it needs one update by hand
there (`qoop stop`, then the install script or `qoop update`), and follows on its own from
then on.

`qoop update` does the same without an interface open, for every machine at once. After it
has installed the newest release (or when this binary already is the newest, so running it
again brings everything to this version), it asks this machine's daemon to move onto the
binary on its disk, then walks the saved remotes one after the other: it puts the release of
that version in place of the qoop there, as above, and asks the daemon there to move. Each
machine gets one line: `moving to X now` when nothing was in the way, `moves to X once
nothing is working; waiting on tab 3: sleep is running in it` while something is, `already X`
when the daemon runs it; a daemon that closes the connection with no answer is looked for
again for 20 seconds, and it is "moving" only when it comes back as the new version. A tab
that runs `qoop update` itself shows up as the thing waited on, which is expected: the move
happens right after the command exits. The command returns as soon as the daemons have
answered and never waits for a move. A remote that cannot be reached or has no release for
its platform is one line and does not change the exit status; this machine's own daemon not
moving does. Asking a remote opens a link to it, which starts its daemon when none runs.
What a move does depends on the two binaries. Where the running daemon can hand its tabs to
the new one, nothing is ended: commands, agents and their conversations simply carry on (next
section). Otherwise nothing that is working is ended, but idle tabs are ended and restored
and every agent is resumed in its own conversation. Either way a `qoop` window open on that
machine closes (run `qoop` again, everything is back), and a window attached to a remote
reconnects by itself. (The automatic path never does this: [Updating on its
own](#updating-on-its-own).) The remote's bridge service, when it has
one, is put back on the new file in the same step, since launchd refuses to start a service
whose program file was replaced.

## Tabs keep running through a move

When the qoop on disk can take over a running daemon's tabs (it answers
`qoop --handoff-formats` with the layout the running one writes), a move waits for nothing.
Nothing needs to be idle, and nothing is ended or typed again: the daemon pauses every tab
where it is, writes down what the next one needs (which descriptor is which tab, the shell's
pid and start time, the hook token, the size, the screen and the modes a program set once, the
agent's generation and conversation id), and becomes the new binary in the same process. The
shells stay its children, so a command that was running keeps its pid and prints into the same
tab afterwards, and an agent in the middle of a turn is the same agent, not a resumed one. A
program that is not a shell at its prompt is made to redraw: the window shrinks by one row and
comes back 800 ms later. What does not cross is what a screen cannot be rebuilt from: the
main screen under a program that uses the alternate one (quit `vim` after a move and the rows
it covered stay blank until the shell draws them again), a scroll region, tab stops, character
sets and the colour table a program changed, and a sequence a program had written half of at
the moment of the move. The scrollback is not carried either, but it is on disk already and
still there. The listening socket and the startup lock stay open across the move, so a client
or a hook that starts a daemon meanwhile finds one serving and starts none. Open interfaces
lose their connection for a moment, with the effect described above.

A move that cannot hand over falls back to the way it always worked: it waits until nothing
is working, then ends the tabs and brings each back (an agent resumed in its conversation).
That is what happens when the qoop on disk does not answer `--handoff-formats` with the
layout the running one writes (every build before this one), when it answers and then cannot
read the file in a dry run made before anything is given up, and when three hand overs in a
row fail (a full disk, a tab that never stops). A tab whose pty or shell cannot be read is
held to that same wait. Every agent is written down (which one, and its conversation) before
the move, so even a move that is lost after the point of no return brings it back where it
was. A shell that ends while the move is under way closes its tab, as it would have anyway.
If the exec itself fails, every tab carries on in the running daemon and it stays on its
version.

A daemon that was started before this existed has nothing to hand over with, so the first
move onto a build that has it is this fallback, here and on every remote; every move after
that is live.

The same check that holds a move also holds `qoop sleep` and the sleep an idle space falls
into: a single command that prints nothing while it leads its own foreground group (`vim`,
`less`, an `ssh` in front) now counts as running, so a space with one of those open stays
awake rather than being slept under it.

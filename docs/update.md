# Releases and updates

No build ever runs on GitHub here, because private Actions minutes are gone.
GitHub only hosts the tarballs. Every build runs on this Mac, and the update runs
wherever the person reading it sits.

## Shipping a release

Releases are built outside this repository: a release is the app build, phone part included.
Each one carries four binaries, `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` (the Linux ones against glibc
2.17), packed as `qoro-<triple>.tar.gz` with its sha256. The asset names are the contract:
`qoro update` and `scripts/install.sh` look for exactly `qoro-<triple>.tar.gz` for the
machine they run on (an Apple silicon Mac gets the arm64 build even from a Rosetta shell).

## Checking and installing

```
qoro update --check   # ask GitHub Releases what is newest, note it, change nothing
qoro update           # install the newest release and move every machine onto it
```

`--check` writes the newest version it saw to the notice file beside the
sessions (`update-available` under the runtime directory). The interface never
touches the network; it only reads that file. Any other argument is refused
before anything reaches the network, so a mistyped `--check` never installs.

The repository is private, so a GitHub token is needed: `GH_TOKEN` or
`GITHUB_TOKEN` from the environment, else whatever `gh auth token` prints. The
assets are fetched by their API URL with `Accept: application/octet-stream`
(the browser download URL 404s for a private repository, token or not). The
token is handed to curl on stdin as a config line (`-K -`, which every curl understands),
never on its command line, and only for `api.github.com`. The redirect to the storage host
is followed by a second request without the token, because curl older than 7.58 forwarded
a custom Authorization header on redirects. `scripts/install.sh` fetches the same way,
needs no python (Linux boxes often have none), and falls back to `sha256sum` where
`shasum` is missing. `QORO_RELEASE_BASE` points both at a mirror or at a local directory
(`file:///path` holding a `latest` file in the API's shape), which is how the platform pick
is tested with no network; the token never goes there.

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
notice is left behind, the failure names GitHub and the exit is nonzero. It still asks the
daemons to move onto the qoro already on disk, which needs no network. That is the one yes
row in `docs/cli-surface.md`, and the gate asserts the failure.

## What the interface shows

The foot of the left column signs the build it is (`qoro 0.3.1`), and when a
check saw something newer it signs where this build stands
(`qoro 0.3.1>0.4.0`). `ctrl-a U` says the same in words: which release is
waiting and that installing it means running `qoro update` outside. There is no background
check and no prompt to restart: eight terminals open and nobody typing still costs nothing.

## Other machines follow this one

A remote machine needs no hand update once its qoro knows how to move. When the interface
connects to a machine whose daemon runs an older version than this Mac's qoro, and a release
of this Mac's version exists for that machine's platform, it fetches that release here
(checksum verified, the same token rules as above, so a private release needs `gh auth login`
or `GH_TOKEN` on this Mac), puts the binary in place of the qoro there in the background with
one atomic rename beside the real file, and asks the daemon over there to move onto it. A
daemon that can hand its tabs over moves at once and they keep running (next sections). Any
other moves at the first moment nothing on it is working: a running command, a busy agent,
an agent that has not named its conversation, or a background job holds it, and the
interface says once, on its hint row, what it is waiting on. When it moves, every tab comes
back with the same number and every agent in its own conversation. A build from main between
releases is never pushed, a qoro there that is as new or newer is never touched, and a swap
another window or another Mac is making at the same time is said rather than raced. A machine
whose daemon is from before 0.5.2 cannot be asked to move, so it needs one update by hand
there (`qoro stop`, then the install script or `qoro update`), and follows on its own from
then on.

`qoro update` does the same without an interface open, for every machine at once. After it
has installed the newest release (or when this binary already is the newest, so running it
again brings everything to this version), it asks this machine's daemon to move onto the
binary on its disk, then walks the saved remotes one after the other: it puts the release of
that version in place of the qoro there, as above, and asks the daemon there to move. Each
machine gets one line: `moving to X now` when nothing was in the way, `moves to X once
nothing is working; waiting on tab 3: sleep is running in it` while something is, `already X`
when the daemon runs it; a daemon that closes the connection with no answer is looked for
again for 20 seconds, and it is "moving" only when it comes back as the new version. A tab
that runs `qoro update` itself shows up as the thing waited on, which is expected: the move
happens right after the command exits. The command returns as soon as the daemons have
answered and never waits for a move. A remote that cannot be reached or has no release for
its platform is one line and does not change the exit status; this machine's own daemon not
moving does. Asking a remote opens a link to it, which starts its daemon when none runs.
What a move does depends on the two binaries. Where the running daemon can hand its tabs to
the new one, nothing is ended: commands, agents and their conversations simply carry on (next
section). Otherwise nothing that is working is ended, but idle tabs are ended and restored
and every agent is resumed in its own conversation. Either way a `qoro` window open on that
machine closes (run `qoro` again, everything is back), and a window attached to a remote
reconnects by itself. The remote's bridge service, when it has
one, is put back on the new file in the same step, since launchd refuses to start a service
whose program file was replaced.

## Tabs keep running through a move

When the qoro on disk can take over a running daemon's tabs (it answers
`qoro --handoff-formats` with the layout the running one writes), a move waits for nothing.
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
sets and the colour table a program changed. The scrollback is not carried either, but it is
on disk already and still there. The listening socket and the startup lock stay open across
the move, so a client or a hook that starts a daemon meanwhile finds one serving and starts
none. Open interfaces lose their connection for a moment, with the effect described above.

A move that cannot hand over falls back to the way it always worked: it waits until nothing
is working, then ends the tabs and brings each back (an agent resumed in its conversation).
That is what happens when the qoro on disk does not answer `--handoff-formats` with the
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

The same check that holds a move also holds `qoro sleep` and the sleep an idle space falls
into: a single command that prints nothing while it leads its own foreground group (`vim`,
`less`, an `ssh` in front) now counts as running, so a space with one of those open stays
awake rather than being slept under it.

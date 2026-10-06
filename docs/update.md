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
qoro update           # install the newest release over this binary
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

The install refuses while any session is running and names what is live, so no
agent dies to a routine upgrade. Stop the named sessions first
(`qoro stop <name>`), then update. The tarball is checksum verified when its
`.sha256` asset exists, and a listed checksum that cannot be downloaded is a
refusal, not an unverified install. The downloaded binary must report the
expected version before it replaces anything, and the swap is one atomic
rename onto the file itself: launched through a symlink on PATH, the link is
followed and stays a link.

With the network gone the command fails closed: nonzero exit, the failure
names GitHub, nothing downloaded, no notice left behind. That is the one yes
row in `docs/cli-surface.md`, and the gate asserts exactly this behaviour.

## What the interface shows

The foot of the left column signs the build it is (`qoro 0.3.1`), and when a
check saw something newer it signs where this build stands
(`qoro 0.3.1>0.4.0`). `ctrl-a U` says the same in words: which release is
waiting and that installing it means detaching, stopping the sessions, and
running `qoro update` outside. There is no background check and no prompt to
restart: eight terminals open and nobody typing still costs nothing.

## Other machines follow this one

A remote machine needs no hand update once its qoro knows how to move. When the interface
connects to a machine whose daemon runs an older version than this Mac's qoro, and a release
of this Mac's version exists for that machine's platform, it fetches that release here
(checksum verified, the same token rules as above, so a private release needs `gh auth login`
or `GH_TOKEN` on this Mac), puts the binary in place of the qoro there in the background with
one atomic rename beside the real file, and asks the daemon over there to move onto it. That
daemon moves at the first moment nothing on it is working: a running command, a busy agent,
an agent that has not named its conversation, or a background job holds it, and the
interface says once, on its hint row, what it is waiting on. When it moves, every tab comes
back with the same number and every agent in its own conversation. A build from main between
releases is never pushed, a qoro there that is as new or newer is never touched, and a swap
another window or another Mac is making at the same time is said rather than raced. A machine
whose qoro is from before this needs one update by hand there (`qoro stop`, then the install
script or `qoro update`), and follows on its own from then on.

The same check that holds a move also holds `qoro sleep` and the sleep an idle space falls
into: a single command that prints nothing while it leads its own foreground group (`vim`,
`less`, an `ssh` in front) now counts as running, so a space with one of those open stays
awake rather than being slept under it.

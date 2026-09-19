# Releases and updates

No build ever runs on GitHub here, because private Actions minutes are gone.
GitHub only hosts the tarball. The build runs on this Mac, and the update runs
wherever the person reading it sits.

## Shipping a release

macOS arm64 only, from a clean tree on this machine:

```
./scripts/release.sh 0.4.0
```

The script stamps the version into every workspace member, builds the release
binary, refuses unless the binary reports the stamped version, packs
`qoro-aarch64-apple-darwin.tar.gz` with its sha256, commits the bump, tags
`v0.4.0`, pushes, and creates the GitHub release with notes from the commits
since the previous tag. The asset name is the contract: `qoro update` looks
for exactly it.

## Checking and installing

```
qoro update --check   # ask GitHub Releases what is newest, note it, change nothing
qoro update           # install the newest release over this binary
```

`--check` writes the newest version it saw to the notice file beside the
sessions (`update-available` under the runtime directory). The interface never
touches the network; it only reads that file.

The install refuses while any session is running and names what is live, so no
agent dies to a routine upgrade. Stop the named sessions first
(`qoro stop <name>`), then update. The tarball is checksum verified when its
`.sha256` asset exists, the downloaded binary must report the expected
version before it replaces anything, and the swap onto the current executable
is one atomic rename.

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

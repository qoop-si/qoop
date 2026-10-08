# Install

## From a release

```
gh api -H 'Accept: application/vnd.github.raw' repos/pat/qoop/contents/scripts/install.sh | sh
```

The script picks the build for your machine (macOS arm64 or Intel, Linux x86_64 or arm64),
checks its sha256, starts it once to confirm the version, puts it at `~/.local/bin/qoop`
(or `$QOOP_DEST`), then installs the hooks for the agents it finds on the machine and the skill.
Run it as yourself, not with sudo; it says so if `~/.local/bin` is not on your `PATH`, and it
leaves a `qoop` already at that path alone.

The releases are private for now, so the script and `qoop update` need a GitHub token:
`GH_TOKEN` or `GITHUB_TOKEN`, or a logged in `gh`. The token is only ever sent to
`api.github.com`, on curl's standard input, never on a command line.

Linux builds need glibc 2.17 or newer (CentOS 7 and anything since). On a musl system such as
Alpine the script says so and stops: build from source there.

## From source

```
cargo build --release -p qoop-cli
./target/release/qoop
```

Then put it on your `PATH` and install the hooks and the skill:

```
qoop hooks install
qoop skills install
```

Both are optional: every agent is read from its screen without a hook. See
[Hooks and skills](#hooks).

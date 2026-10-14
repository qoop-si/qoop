# Install

## From a release

```
curl -fsSL https://qoop.si/i/<your link> | sh
```

Your link is on qoop.si/app after you sign in, and in the mail with your seat. It works once.

The script picks the build for your machine (macOS arm64 or Intel, Linux x86_64 or arm64),
checks its sha256, starts it once to confirm the version, puts it at `~/.local/bin/qoop`
(or `$QOOP_DEST`), then installs the hooks for the agents it finds on the machine and the skill.
Run it as yourself, not with sudo; it says so if `~/.local/bin` is not on your `PATH`, and it
leaves a `qoop` already at that path alone.

It ends by printing the next command, `qoop login`, with the full path and the line to add to
your shell profile when `~/.local/bin` is not on your `PATH` yet. The script and `qoop update`
read the releases from `https://qoop.si/releases` over https; no account or token is involved.

Linux builds need glibc 2.17 or newer (CentOS 7 and anything since). On a musl system such as
Alpine the script says so and stops: build from source there.

## From source

```
cargo build --release -p qoop-cli
./target/release/qoop
```

Then put it on your `PATH`. qoop sets up the hook and the skill of every agent on the machine by
itself, one you install later too. To do it right away:

```
qoop hooks install
qoop skills install
```

Both are optional: every agent is read from its screen without a hook. See
[Hooks and skills](#hooks).

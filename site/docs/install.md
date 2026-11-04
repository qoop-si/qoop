# Install

## From a release

```
curl -fsSL https://qoop.si/install.sh | sh
```

The script picks the build for your machine (macOS arm64 or Intel, Linux x86_64 or arm64),
checks its sha256, starts it once to confirm the version, puts it at `~/.local/bin/qoop`
(or `$QOOP_DEST`), then installs the hooks for the agents it finds on the machine and the skill.
Run it as yourself, not with sudo; it says so if `~/.local/bin` is not on your `PATH`, and it
leaves a `qoop` already at that path alone.

On a first install it then walks you through a short setup: your pet, sounds, light or dark,
and signing this computer in to your account, which is what puts your agents on your phone.
Every answer can be changed later with `qoop settings`; a step you skipped, such as signing in,
is `qoop login`. When `~/.local/bin` is not on your `PATH` yet, it prints the line to add to
your shell profile. The script and `qoop update`
read the releases from `https://qoop.si/releases` over https; no account or token is involved.
qoop also sends a few numbers a day about how it is used, never content, once it has told you
so; [Usage counts](#telemetry) shows exactly what and how to stop it.

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

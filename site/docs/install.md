# Install

## The install line

```
curl -fsSL https://qoop.si/install.sh | sh
```

It runs on macOS (Apple silicon or Intel) and on Linux with glibc 2.17 or newer (x86_64 or
arm64), so CentOS 7 and newer, Debian, Ubuntu and Fedora. A Mac needs nothing else. Linux needs
`curl` and CA certificates, which a minimal image or container may lack. Add them first, as root
or with `sudo` in front. On Debian or Ubuntu:

```
apt-get update && apt-get install -y curl ca-certificates
```

On a minimal Fedora image (a full Fedora install has them already):

```
dnf install -y curl tar gzip
```

Linux with musl, such as Alpine, is not supported yet: the script says so and stops. Use a glibc
distribution, such as Debian or Ubuntu, or a glibc container.

Run the install line as the user who will use qoop. On a server where you log in as root, running
it as root is fine: it installs for root. Under `sudo` it stops, because the binary, the hooks and
the skill go into the home of whoever runs it.

## What the script does

It picks the build for your machine, checks its sha256, starts it once to confirm the version,
and puts it at `~/.local/bin/qoop` (or `$QOOP_DEST`). The script and `qoop update` read the
newest release from `https://qoop.si/releases/latest` over https; no account or token is
involved. When `qoop` is already on your `PATH`, or already at that path, it changes nothing and
says to run `qoop update` instead.

It never edits your shell profile: when `~/.local/bin` is not on your `PATH`, it prints the line
to add to it.

## What it changes on the machine

- **The binary**, at `~/.local/bin/qoop`.
- **Hooks** (`qoop hooks install`): qoop's hook in the settings of every agent it finds on the
  machine. For Claude Code that is `~/.claude/settings.json`, where it also adds 13 allow rules to
  `permissions.allow`, one for each `qoop task` command an agent runs on its own task (such as
  `Bash(qoop task new:*)`), so Claude does not ask before each one. They are left out when a rule
  you have, such as `Bash(qoop:*)`, already covers them. Codex runs its hook only once you trust
  it, and its sandbox needs one setting for qoop commands: see [Hooks and skills](#hooks).
- **The skill** (`qoop skills install`): a `SKILL.md` in each agent's own skills folder (for
  Claude Code, `~/.claude/skills/qoop`), and qass's own folder, which `qoop paths` lists as
  `qass-data`. Where Gemini CLI is set up, that folder is added to the folders Gemini trusts.
- **On a Mac, the notch**: your agents in the notch at the top of the screen, started at login
  by a login item, `si.qoop.notch.plist` in `~/Library/LaunchAgents`. It needs macOS 14 or newer.
  `qoop notch uninstall` takes it and its login item out.
- **On a Mac, the icon font**: the font qoop draws its few icons from, in `~/Library/Fonts`.
  `qoop icons uninstall` takes it out.

Hooks and the skill are optional: every agent is read from its screen without a hook. qoop also
sets them up by itself for an agent you install later. `qoop hooks uninstall` and
`qoop skills uninstall` remove exactly what was added; `qoop uninstall` removes all of the above
([Update and uninstall](#update)).

## The first start

On a first install, with a terminal to answer on, the script goes on to a short setup of eight
steps:

1. your pet
2. sounds
3. light or dark
4. the command key
5. notifications
6. sleep
7. qass: the agent it runs on
8. improve qass: whether qass may send qoop's maker a short note when its own way of working
   fails (ask each time, always, or never)

Each choice is saved as you make it. Then it asks:

> Sign in so this computer shows on qoop.si/app and your phone? [Y/n]

Enter or `y` runs `qoop login`, which is what puts your agents on your phone; anything else
leaves it for later with `qoop login`. When an agent qass runs on is installed, qoop then opens with qass.

`qoop settings` changes any answer later, and `qoop onboard` walks the steps again. Without a
terminal to ask on, the script skips the setup and prints the next command, `qoop login`.

qoop also sends a few numbers a day about how it is used, never content, once it has told you
so; [Usage counts](#telemetry) shows exactly what and how to stop it.

## Source

The open part's source is at [github.com/qoop-si/qoop](https://github.com/qoop-si/qoop),
under AGPL-3.0-or-later: the terminal, the daemon, the interface, agent detection, hooks and
skills. The phone bridge, the hub and the account features (sign in, your phone,
qoop.si/app) ship only in the released binary.

To build the open part, install Rust with [rustup](https://rustup.rs): the repository selects
the stable channel, and Rust 1.98 or newer is needed. Then run:

```
cargo build --release -p qoop-cli
```

The binary is `target/release/qoop`. It has no phone part: on a machine paired with your
phone, keep the released binary, or the phone bridge stops.

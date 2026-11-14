# Configuration

qoop needs no configuration. For the few things worth changing there is one file,
`~/.config/qoop/config.toml` (or `$XDG_CONFIG_HOME/qoop/config.toml`):

```toml
prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
theme = "auto"      # auto | dark | light
icons = "auto"      # auto | on | off: icons where they were seen to draw, plain elsewhere
[notify]
desktop = true
sound = false
[sleep]
idle_minutes = 0    # 0 = never; else a tab idle and unwatched this long sleeps
[daemon]
exit_when_asleep = true   # leave when nothing is awake and nobody is attached
```

| Key | Default | What it does |
|---|---|---|
| `prefix` | `"ctrl-a"` | the key before every command. `ctrl-b` suits tmux hands. `c d h i j m q s z` are refused: they are commands, backspace, tab, enter, detach or job control |
| `theme` | `"auto"` | `auto` asks the terminal for its background and follows it, including when it switches between light and dark; `dark` and `light` fix it |
| `notify.desktop` | `true` | desktop notifications, see [Notifications](#notifications) |
| `notify.sound` | `false` | a sound with each notification |
| `sleep.idle_minutes` | `0` | a tab idle and unwatched this many minutes sleeps by itself; `0` is never. See [Sleep](#sleep) |
| `daemon.exit_when_asleep` | `true` | the daemon leaves ten seconds after nothing is awake and no interface is attached |

The file is read when the interface starts, and again within two seconds of any change to it,
so an edit applies without a restart. `qoop settings` (also `ctrl-a m`, then `settings`)
changes it from a page. The page shows every setting with your pet and writes a
change as you make it; `qoop config` prints the file, the value in force for each key, and any
problem.

A mistake never stops qoop: a value it cannot use keeps that key's default, and the hint row
says which line and why (`line 3: sound is true or false`). A line that is not TOML at all
is left out and the rest of the file is still read.

## Spaces

`qoop worktree new` sets up a new worktree with what its space says: the files to copy from
the main checkout and a command to run. You write the table by hand; qoop only reads it, at
each `worktree new`, so an edit needs no restart. Nothing in the repository itself is ever
run, so a cloned repository cannot run anything.

```toml
[spaces."my app".worktree]
copy = [".env", "apps/web/.env.local"]
setup = "bun install && cp -n .env.example .env"
```

| Key | Default | What it does |
|---|---|---|
| `spaces.<name>.worktree.copy` | none | files from the main checkout to copy into the new worktree, as git glob patterns: `*` stays inside one folder, `**/` crosses folders, a folder name takes what is under it. Only files git ignores are copied, so a tracked file never is, and nothing already in the worktree is overwritten. A symlink is copied as a symlink; a relative one points from its new place, which differs when the worktree sits elsewhere (`--path`). A pattern that is empty, absolute, starts with `:` or `!`, or holds `..` is refused |
| `spaces.<name>.worktree.setup` | none | a command line run with `sh -c` in the new worktree after the copy, with `QOOP_WORKTREE` (the worktree), `QOOP_WORKTREE_SOURCE` (the main checkout), `QOOP_WORKTREE_BRANCH` and `QOOP_WORKTREE_INDEX` (0 is the main checkout, each linked worktree keeps the lowest free number from 1, for ports: `PORT=$((3000 + QOOP_WORKTREE_INDEX))`). Its output goes to stderr |

The space is `--space` when given, else the one whose folder is the repository
(`qoop space dir NAME <repo>` sets it), else the one whose folder holds it. The key is the
space's name, quoted when it holds a space or another sign: when you rename a space, rename
its table too.

When the copy refuses a file (one that would land under a symlink in the worktree), the
command fails, or the space's table has a mistake (a wrong type, a refused pattern, an
unknown key), the worktree is kept, no terminal opens, and `worktree new` exits 1. Fix it,
then `qoop worktree setup <branch>` runs the setup again; `--no-setup` skips it.

## In the environment

| Variable | What it does |
|---|---|
| `QOOP_RELEASE_BASE` | where `qoop update` and the install script look for releases (a mirror, GitHub's API, or `file:///path`; default `https://qoop.si/releases`, under which they read `latest`) |
| `DO_NOT_TRACK`, `QOOP_TELEMETRY=0` | no usage counts; written into `config.toml` as `telemetry = false` the first time qoop sees it (see [Usage counts](#telemetry)) |

`QOOP_IDLE_TIMEOUT_MINUTES` is no longer read: `idle_minutes` under `[sleep]` puts idle tabs to
sleep instead (see [Sleep](#sleep)), and `QOOP_SESSION` is refused, as named sessions became
spaces.

## Agent detection

Detection rules can be overridden per agent without a release: see
[Supported agents](#agents).

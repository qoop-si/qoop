# Configuration

qoop needs no configuration. For the few things worth changing there is one file,
`~/.config/qoop/config.toml` (or `$XDG_CONFIG_HOME/qoop/config.toml`):

```toml
prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
theme = "auto"      # auto | dark | light
[notify]
desktop = true
sound = false
```

| Key | Default | What it does |
|---|---|---|
| `prefix` | `"ctrl-a"` | the key before every command. `ctrl-b` suits tmux hands. `c d h i j m q s z` are refused: they are commands, backspace, tab, enter, detach or job control |
| `theme` | `"auto"` | `auto` asks the terminal for its background and follows it, including when it switches between light and dark; `dark` and `light` fix it |
| `notify.desktop` | `true` | desktop notifications, see [Notifications](#notifications) |
| `notify.sound` | `false` | a sound with each notification |

The file is read when the interface starts, and again within two seconds of any change to it,
so an edit applies without a restart. `qoop settings` (also `ctrl-a space`, then `settings`)
changes it from a page. The page shows every setting with your pet and writes a
change as you make it; `qoop config` prints the file, the value in force for each key, and any
problem.

A mistake never stops qoop: a value it cannot use keeps that key's default, and the hint row
says which line and why (`line 3: sound is true or false`). A line that is not TOML at all
is left out and the rest of the file is still read.

## In the environment

| Variable | What it does |
|---|---|
| `QOOP_SESSION` | which session to open (the `--session` flag wins over it) |
| `QOOP_IDLE_TIMEOUT_MINUTES` | close terminals idle this long with nothing running in the background; read by the daemon, so set it where the daemon starts. See [Sessions](#sessions) |
| `QOOP_RELEASE_BASE` | where `qoop update` and the install script look for releases (a mirror, or `file:///path`) |

## Agent detection

Detection rules can be overridden per agent without a release: see
[Supported agents](#agents).

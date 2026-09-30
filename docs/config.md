# config.toml

One file, `~/.config/qoro/config.toml` (or `$XDG_CONFIG_HOME/qoro/config.toml`), and every
key in it is optional. A value that cannot be used keeps its default and is named, by its
line, on the interface's hint row and in the settings box (`ctrl-a m`, settings); nothing in
the file ever stops qoro from starting.

The interface reads it when it starts and again whenever the settings box opens. `[sleep]`
and `[daemon]` are the daemon's, read from the same file: `daemon.exit_when_asleep` each
time it falls quiet, `sleep.idle_minutes` when it starts (so a change to it takes a
`qoro stop`, or the next time everything sleeps and the daemon leaves). A daemon on another
machine reads that machine's file. There is no other
place any of these is set: an environment variable would change with whoever happened to
start the daemon, a terminal, the notch or an ssh relay.

```toml
prefix = "ctrl-a"
theme = "auto"

[notify]
desktop = true
sound = false
done_sound = "Glass"
alert_sound = "Funk"

[sleep]
idle_minutes = 0

[daemon]
exit_when_asleep = true
```

| Key | Default | What it does |
|---|---|---|
| `prefix` | `"ctrl-a"` | the key that starts a command: `ctrl-` and one of `a b e f g k l n o p r t u v w x y` |
| `theme` | `"auto"` | `auto` asks the terminal for its background; `dark` or `light` say it |
| `notify.desktop` | `true` | tell the desktop when an agent needs you and you are not looking |
| `notify.sound` | `false` | and play a sound when it does; the install asks |
| `notify.done_sound` | `"Glass"` | the sound for an agent that finished, one of macOS's own: `Basso Blow Bottle Frog Funk Glass Hero Morse Ping Pop Purr Sosumi Submarine Tink` |
| `notify.alert_sound` | `"Funk"` | the sound for one that waits on you or failed, from the same list |
| `sleep.idle_minutes` | `0` | a space whose tabs have all been idle and unwatched this many minutes sleeps by itself, the same way `ctrl-a Z` puts it to sleep, refusals included. `0` is never, and then nothing runs for it at all |
| `daemon.exit_when_asleep` | `true` | the daemon leaves ten seconds after nothing is awake and no interface is attached, so a machine with everything asleep runs nothing of qoro; the next `qoro` starts it again. `false` keeps it up |

`QORO_IDLE_TIMEOUT_MINUTES` is no longer read. It used to close idle tabs for good; a daemon
that finds it set says so once and points here, where `sleep.idle_minutes` puts idle spaces
to sleep instead, with their tabs kept.

What a sleep keeps and what it refuses is in the README; how it is built, in
[ADR-006](adr/ADR-006-daemon-topology.md).

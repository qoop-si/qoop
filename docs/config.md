# config.toml

One file, `~/.config/qoop/config.toml` (or `$XDG_CONFIG_HOME/qoop/config.toml`), and every
key in it is optional. A value that cannot be used keeps its default and is named, by its
line, on the interface's hint row and by `qoop config`; nothing in
the file ever stops qoop from starting.

The interface reads it when it starts, and again within two seconds of any change to it
(`qoop settings`, `qoop config set`, an editor). `[sleep]`
and `[daemon]` are the daemon's, read from the same file: `daemon.exit_when_asleep` each
time it falls quiet, `sleep.idle_minutes` and `sleep.qass_idle_minutes` at its next idle check, a few
minutes at most, and from "never" at the next agent activity (no restart either way); a change
made through the daemon's socket is looked at at once. A daemon on another machine reads that
machine's file. There is no other
place any of these is set: an environment variable would change with whoever happened to
start the daemon, a terminal, the notch or an ssh relay.

```toml
prefix = "ctrl-a"
theme = "auto"
icons = "auto"

[notify]
desktop = true
sound = false
done_sound = "Glass"
alert_sound = "Funk"

[sleep]
idle_minutes = 0
qass_idle_minutes = 30

[daemon]
exit_when_asleep = true

[board]
cards = "full"
```

| Key | Default | What it does |
|---|---|---|
| `prefix` | `"ctrl-a"` | the key that starts a command: `ctrl-` and one of `a b e f g k l n o p r t u v w x y` |
| `theme` | `"auto"` | `auto` asks the terminal for its background; `dark` or `light` say it |
| `icons` | `"auto"` | the few icons qoop draws (asleep, keep awake, sleep and wake, settings, another machine) where they were seen to draw: iTerm2 and Terminal with qoop's own font, which the interface puts in `~/Library/Fonts`, and Ghostty, kitty and WezTerm, which carry the glyphs; every other terminal, tmux and an ssh session keep the plain glyphs. `on` or `off` force either |
| `notify.desktop` | `true` | tell the desktop when an agent needs you and you are not looking |
| `notify.sound` | `false` | and play a sound when it does; the install asks |
| `notify.done_sound` | `"Glass"` | the sound for an agent that finished, one of macOS's own: `Basso Blow Bottle Frog Funk Glass Hero Morse Ping Pop Purr Sosumi Submarine Tink`, or on Linux a freedesktop name (`complete`, `bell`, `message`, ...; Linux default `complete`); `qoop sound` plays them to choose |
| `notify.alert_sound` | `"Funk"` | the sound for one that waits on you or failed, from the same list (Linux default `dialog-warning`) |
| `sleep.idle_minutes` | `0` | a tab idle this many minutes, and unwatched for as long, sleeps by itself, the same way `ctrl-a Z` puts it to sleep, refusals included; the rest of its space is left as it is. A space or an agent whose cup is full (kept awake) is passed by. `0` is never, and then nothing runs for it at all |
| `sleep.qass_idle_minutes` | follows `sleep.idle_minutes` | the same for the tabs of the space named `qass`, where qass lives: `0` keeps them awake while the others sleep, a number sleeps them after that many idle minutes while the other spaces follow `sleep.idle_minutes`. qass does not wake by itself: `qoop qass`, `ctrl-a Q` or the qass page (`qoop qass page`) wakes it. The running daemon takes it up at its next idle check, as it does `sleep.idle_minutes`; from `0` or unset it starts checking at the next agent activity |
| `board.cards` | `"full"` | the board page's cards: `full` shows each card's key, how long it has been in its lane, its agent and how long that agent has worked, and one word; `quiet` is one line, with a word only on "waits on you" and "qass checks". `v` on the board, or its `[ full · quiet ]` switch, writes it |
| `daemon.exit_when_asleep` | `true` | the daemon leaves ten seconds after nothing is awake and no interface is attached, so a machine with everything asleep runs nothing of qoop; the next `qoop` starts it again. `false` keeps it up |

## These keys are per machine

`sleep.idle_minutes`, `sleep.qass_idle_minutes` and `daemon.exit_when_asleep` belong to the machine
whose file they are in: each machine has its own RAM and its own agents, so there is no account-wide
copy. To change a remote's, use `qoop settings` in a tab on that machine (the page names the machine
it is on), `ssh host qoop config set sleep.idle_minutes 30`, or the phone and web app from the release
that adds them there. Through that machine's running daemon (`SleepSettings` on its socket) only
`sleep.idle_minutes` and `daemon.exit_when_asleep` can be read and changed: a machine with no session
running has nothing to answer.

When a change takes effect: the daemon looks at the file at once when the change came through its
socket, and a tab then sleeps after its idle minutes plus up to one idle check (a minute to five minutes,
set by the number), also when idle sleep was off before; `daemon.exit_when_asleep` the next time the
daemon falls quiet, which is when the last interface detaches. A setting never puts anything to sleep by itself. Turning idle sleep on after tabs have been
idle for hours can make the next pass sleep several at once, each one vetted as always: a tab that is
working, waiting on you or kept awake is passed by.

Two writers in one daemon take turns, so neither loses the other's change. A second process writing
at the same moment (`qoop settings` in a tab, `qoop config set`) is last writer wins on the whole file,
which is replaced atomically, as any editor does.

`QOOP_IDLE_TIMEOUT_MINUTES` is no longer read. It used to close idle tabs for good; a daemon
that finds it set says so once and points here, where `sleep.idle_minutes` puts idle tabs
to sleep instead, kept.

What a sleep keeps and what it refuses is in the README; how it is built, in
[ADR-006](adr/ADR-006-daemon-topology.md).

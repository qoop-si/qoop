# Notifications

When an agent needs you (blocked on a question, failed) or finishes, qoop tells you, so you
do not have to watch its tab:

- its row in the agents panel lights up;
- a desktop notification says which agent and what happened, for example `codex is
  blocked, aider finished`, or `claude failed in review` for one;
- the bell rings when no desktop notification could be sent, and the hint row says why,
  once.

A notification is sent only when you are not already looking: the agent is not the pane
you are typing in, or the terminal window running qoop has lost focus. It waits a second and checks
again first, so an agent that flickers through a state does not ping you, and several
agents finishing together make one notification.

## Long commands in a shell tab

A command that runs in a plain shell tab (no agent ran in it) and takes 30 seconds or more
gets a note when it ends, if you cannot see that tab: it is not on screen in this window, or
the terminal window running qoop is not focused. The note says `Command finished in <tab>`, or
`Command failed (exit 3) in <tab>`. It follows the same `[notify]` switches and mute as an
agent's, and it works for a tab on a remote machine too.

To know where a command starts and ends, qoop makes new zsh and bash (4.4 or newer) tabs
report it, without editing any of your files: zsh reads a qoop `.zshenv` that hands back your own
`ZDOTDIR` and sources your `.zshenv` first, and bash starts through a qoop rc file that sources
`~/.bashrc` first. fish 4 reports it by itself. The macOS `/bin/bash` (3.2) does not, so a
command there gets no note. Only shells started after the update report it. Nothing but the
timing and the exit status is read from what they report.

To turn it off, put `export QOOP_SHELL_INTEGRATION=0` in `.zshrc` or `.bashrc`; it is read at
each prompt.

## How it reaches the desktop

qoop uses the fastest way the terminal you run it in understands:

| Where | How |
|---|---|
| iTerm2, WezTerm, Ghostty | the terminal's own notification escape (OSC 9) |
| kitty | OSC 99 |
| foot | OSC 777 |
| inside tmux with `allow-passthrough` on | the same escapes, passed through tmux |
| anywhere else on macOS | `terminal-notifier` when installed, else `osascript` |
| anywhere else on Linux | `notify-send` |
| over ssh | only the terminal escapes above, which reach the terminal on your side; a system program would show it on the far machine, so none is used |

`qoop settings` shows which one a note from that terminal takes (its desktop row), or why
there is none.

Names in a notification are passed as data, never as code: a tab named like a command
cannot run anything.

## Sound

Off by default. With `sound = true` in `[notify]`, macOS plays `Funk` when an agent needs
you and `Glass` when one finished (through `afplay`); Linux plays freedesktop sounds,
`dialog-warning` and `complete` by default, through `paplay` or `pw-play` when both are there. No
sound is played from a qoop running over ssh. A name from the other system (a Mac name read
on Linux) plays that default.

To hear them and choose, open `qoop settings` (the sound rows play as left and right change
them) or run `qoop sound`: macOS lists its own sounds, Linux the
freedesktop ones installed. The arrows move through them and play each one, space plays it
again, enter keeps it, first for a finished agent, then for one that waits on you. Piped,
it reads lines instead: a number or a name plays one, an empty line keeps it. Keeping one turns
`sound` on, `o` (`off` piped) turns it off. A running qoop takes the change within two
seconds.

## Turning it off

```toml
[notify]
desktop = false
```

in `~/.config/qoop/config.toml`. See [Configuration](#configuration).

## Troubleshooting

- **Nothing appears inside tmux:** run `tmux set -g allow-passthrough on`, or install
  `terminal-notifier` (macOS) so qoop can go around tmux.
- **On macOS, notifications from `osascript` are credited to Script Editor:** install
  `terminal-notifier` (`brew install terminal-notifier`).
- **Nothing appears at all:** check that your terminal app is allowed to send notifications
  in the system settings, and that Focus or Do Not Disturb is off.

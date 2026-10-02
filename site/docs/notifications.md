# Notifications

When an agent needs you (blocked on a question, failed) or finishes, qoro tells you, so you
do not have to watch its tab:

- its row in the agents panel lights up;
- a desktop notification says which agent and what happened, for example `codex is
  blocked, aider finished`, or `claude failed in review` for one;
- the bell rings when no desktop notification could be sent, and the hint row says why,
  once.

A notification is sent only when you are not already looking: the agent is not the pane
you are typing in, or the terminal window running qoro has lost focus. It waits a second and checks
again first, so an agent that flickers through a state does not ping you, and several
agents finishing together make one notification.

## How it reaches the desktop

qoro uses the fastest way the terminal you run it in understands:

| Where | How |
|---|---|
| iTerm2, WezTerm, Ghostty | the terminal's own notification escape (OSC 9) |
| kitty | OSC 99 |
| foot | OSC 777 |
| inside tmux with `allow-passthrough` on | the same escapes, passed through tmux |
| anywhere else on macOS | `terminal-notifier` when installed, else `osascript` |
| anywhere else on Linux | `notify-send` |
| over ssh | only the terminal escapes above, which reach the terminal on your side; a system program would show it on the far machine, so none is used |

The settings box (`ctrl-a space`, then `settings`) shows which one this session uses, or
why there is none.

Names in a notification are passed as data, never as code: a tab named like a command
cannot run anything.

## Sound

Off by default. With `sound = true` in `[notify]`, macOS plays `Funk` when an agent needs
you and `Glass` when one finished (through `afplay`); Linux plays the freedesktop
`dialog-warning` or `complete` sound through `paplay` or `pw-play` when both are there. No
sound is played from a qoro running over ssh.

To hear them and choose, run `qoro sound` (macOS): a number or a name plays one, enter
keeps it, first for a finished agent, then for one that waits on you. Keeping one turns
`sound` on, `off` turns it off. A running qoro takes the change the next time its settings
open (prefix, space, `settings`).

## Turning it off

```toml
[notify]
desktop = false
```

in `~/.config/qoro/config.toml`. See [Configuration](#configuration).

## Troubleshooting

- **Nothing appears inside tmux:** run `tmux set -g allow-passthrough on`, or install
  `terminal-notifier` (macOS) so qoro can go around tmux.
- **On macOS, notifications from `osascript` are credited to Script Editor:** install
  `terminal-notifier` (`brew install terminal-notifier`).
- **Nothing appears at all:** check that your terminal app is allowed to send notifications
  in the system settings, and that Focus or Do Not Disturb is off.

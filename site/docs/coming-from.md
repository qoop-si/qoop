# Coming from tmux

qoro keeps what tmux gets right: one server that outlives the window, a prefix key, splits,
detach and attach. What it adds is that it knows which terminals are agents and what each
one is doing.

## Words

| tmux | qoro |
|---|---|
| server | daemon, one per session |
| session | session (`qoro --session work`) |
| window | tab |
| pane | pane |
| (none) | space: a named group of tabs, usually one per project |

## Keys

Set `prefix = "ctrl-b"` in [Configuration](#configuration) and most of your hands carry
over:

| tmux | qoro |
|---|---|
| `c` new window | `c` new tab |
| `n`, `p`, `0` .. `9` | `n`, `p`, `1` .. `9` |
| `,` rename window | `r` rename tab |
| `&` kill window | `x` close tab (asks when an agent in it is still working) |
| `%`, `"` split | `v`, `-` split |
| `o` next pane, arrows | `o` next pane, `H J K L` |
| `z` zoom | `z` zoom |
| `!` break pane | `X` take the pane out as a tab |
| `ctrl-arrows` resize | `e` then `h j k l`, or drag the divider |
| `[` copy mode | `[` copy mode, vi keys, `/` and `?` search the whole history |
| `d` detach | `d` detach, or `ctrl-q` |
| `:` command prompt | `space` finds any space, agent, tab or action |

## Commands

| tmux | qoro |
|---|---|
| `tmux ls` | `qoro ls` |
| `tmux attach -t work` | `qoro --session work` |
| `tmux kill-server` | `qoro stop` |
| `tmux send-keys -t 3 'y' Enter` | `qoro send 3 y --enter` |
| `tmux capture-pane -p -t 3` | `qoro read 3` |
| `tmux new-window 'cmd'` | `qoro new -- cmd` |

Beyond these, `qoro agents`, `qoro wait` and `qoro prompt --wait` let a script or another
agent work with an agent by its state rather than by polling its screen: see
[Automation](#automation).

## Running qoro inside tmux

It works. Two things to know:

- tmux takes the prefix first: pick a different one for each (`ctrl-a` for qoro and tmux's
  own `ctrl-b` is the default pairing).
- Desktop notifications from inside tmux need `tmux set -g allow-passthrough on`, or
  `terminal-notifier` on macOS: see [Notifications](#notifications).

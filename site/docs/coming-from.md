# Coming from tmux

qoop keeps what tmux gets right: one server that outlives the window, a prefix key, splits,
detach and attach. What it adds is that it knows which terminals are agents and what each
one is doing.

## Words

| tmux | qoop |
|---|---|
| server | daemon, one per session |
| session | session (`qoop --session work`) |
| window | tab |
| pane | pane |
| (none) | space: a named group of tabs, usually one per project |

## Keys

Set `prefix = "ctrl-b"` in [Configuration](#configuration) and most of your hands carry
over:

| tmux | qoop |
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

| tmux | qoop |
|---|---|
| `tmux ls` | `qoop ls` |
| `tmux attach -t work` | `qoop --session work` |
| `tmux kill-server` | `qoop stop` |
| `tmux send-keys -t 3 'y' Enter` | `qoop send 3 y --enter` |
| `tmux capture-pane -p -t 3` | `qoop read 3` |
| `tmux new-window 'cmd'` | `qoop new -- cmd` |

Beyond these, `qoop agents`, `qoop wait` and `qoop prompt --wait` let a script or another
agent work with an agent by its state rather than by polling its screen: see
[Automation](#automation).

## Running qoop inside tmux

It works. Two things to know:

- tmux takes the prefix first: pick a different one for each (`ctrl-a` for qoop and tmux's
  own `ctrl-b` is the default pairing).
- Desktop notifications from inside tmux need `tmux set -g allow-passthrough on`, or
  `terminal-notifier` on macOS: see [Notifications](#notifications).

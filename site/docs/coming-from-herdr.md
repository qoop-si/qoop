# Coming from herdr

herdr and qoop start from the same idea: a daemon that outlives the window, a prefix key,
splits, and a sidebar that shows which agent needs you. Your spaces, tabs and panes map
across one to one, and herdr's terminal plugins run in qoop, within the limits below.

## Words

| herdr | qoop |
|---|---|
| workspace | space: a named group of tabs, usually one per project (`qoop --space work`) |
| tab | tab |
| pane | pane |
| agent | agent |

## Keys

herdr's prefix is `ctrl+b`; qoop's is `ctrl-a`. Set `prefix = "ctrl-b"` in
[Configuration](#configuration) to keep your hand on the same key. These keys differ or sit
somewhere else; the rest of herdr's keys are not carried over, and `ctrl-a ?` lists every key
qoop has:

| herdr | qoop |
|---|---|
| `c` new tab | `c` new tab |
| `n`, `p`, `1` .. `9` | `n`, `p`, `1` .. `9` |
| `shift+t` rename tab | `r` rename tab |
| `shift+x` close tab | `x` close tab (asks when an agent in it is still working) |
| `v`, `minus` split | `v`, `-` split |
| `h j k l` focus a pane | `H J K L` |
| `z` zoom | `z` zoom |
| `r` resize mode | `e` resize mode, or drag the divider |
| `[` copy mode | `[` copy mode, `/` and `?` search the whole history |
| `shift+n` new workspace | `w` new space |
| `g` goto picker | `space` finds any space, agent, tab or action |
| `b` toggle sidebar | `b` fold the left column away |
| `q` detach | `d` detach, or `ctrl-q` |

qoop does not read herdr's config file. Keys, theme and the rest are set in
[Configuration](#configuration).

## Your plugins

qoop runs herdr plugins unchanged. Install one from GitHub, and its actions show up in the
palette:

```
qoop plugin install owner/repo
```

`install` prints every command the plugin would run and asks before anything does. Then
`ctrl-a space` ends with a section for each installed plugin's actions. `qoop plugin ls` lists
what is installed, and `qoop plugin rm <id>` removes one.

What works: plugins that act on terminals, tabs and spaces. They can rename a tab, open a pane
beside an agent, type a prompt into an agent, read a pane's contents and show a notification.
`herdr-bar`, `herdr-plus`, `herdr-sidebar` and `herdr-automatic-rename` behaved the same side by
side with herdr 0.9.5. `herdr-reviewr` differs in one way: qoop holds what a plugin types into
an agent while you have a half-typed line there, so a plugin never types over your words.

The main limits:

- no floating popups: a popup opens as a split beside its pane, zoomed to fill the tab until
  it closes;
- no layout changes or moving panes (swapping and resizing panes work);
- herdr's desktop app mods and layouts are a different thing and do not run;
- a plugin's pane metadata, such as quota marks, is kept for the plugin and not drawn;
- plugins have no key bindings of their own: they run from the palette and
  `qoop plugin run <id.action>`.

[herdr plugins](#plugins) has the rest of the limits, what a plugin is allowed to do and a table
of the plugins that were tried, each with what happened.

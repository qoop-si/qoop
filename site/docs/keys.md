# Keys

Every command is the prefix, then a key. The prefix is `ctrl-a` unless you set another in
[Configuration](#configuration); everything on this page, and every key the interface names
on screen, follows the prefix you chose. `ctrl-a ctrl-a` types a literal `ctrl-a` into the
program.

## Tabs and spaces

| Key | Does |
|---|---|
| `ctrl-a c` | new tab (asks for a name; empty for the program's own) |
| `ctrl-a n`, `ctrl-a p` | next, previous tab |
| `ctrl-a 1` .. `ctrl-a 9` | the nth tab |
| `ctrl-a r` | rename the tab |
| `ctrl-a x` | close the tab; asks first when an agent in it is working, starting or blocked (`Y` closes, any other key keeps it) |
| `ctrl-a w` | new space |
| `ctrl-a s`, `ctrl-a S` | next, previous space |
| `ctrl-a R` | the space's settings: its name and colour |
| `ctrl-a W` | close the space when it is empty |

## Panes

| Key | Does |
|---|---|
| `ctrl-a v`, `ctrl-a -` | split beside, below |
| `ctrl-a H J K L` | focus the pane to that side |
| `ctrl-a o` | next pane |
| `ctrl-a z` | zoom the pane, or back |
| `ctrl-a X` | take the pane out, keeping it as a tab |
| `ctrl-a e` | resize mode: `h j k l` or arrows move the edge 5%, `H J K L` 20%, `=` equalize, `esc` done |
| `ctrl-a =` | equalize every pane |

## Finding and reading

| Key | Does |
|---|---|
| `ctrl-a space` | find anything: a space, an agent, a tab, an action |
| `ctrl-a [` | copy mode: move, select, search the whole history (see [Copy mode](#copy-mode)) |
| `ctrl-a g` | agents panel grouped by space, or ranked by urgency |
| `ctrl-a F` | an agent's focus view: your last prompt, its last turns, one row per tool call |
| `ctrl-a b` | fold the left column away |

## The rest

| Key | Does |
|---|---|
| `ctrl-a m` | the menu |
| `ctrl-a ?` | every key |
| `ctrl-a d`, `ctrl-q` | detach, leaving everything running |
| `ctrl-a U` | whether an update is waiting, and how it installs |
| `ctrl-a l` | repaint |

## Mouse

| Do | Does |
|---|---|
| click | focus a tab, a pane, a space or an agent |
| drag | select, and copy when you let go |
| drag a divider | resize the panes on either side |
| click a space's `+`, or the tab bar's | a new agent or terminal in that space, in its directory; an agent asks for its name (empty for the agent's own) |
| click a space's `⋯` | the space's settings: its name and colour |
| drag the column's edge | widen or narrow the left column, up to half the window |
| shift-drag | select over a program that took the mouse |
| wheel | scroll back through the history |

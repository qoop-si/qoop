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
| `ctrl-a Q` | qass in its own space, or back to where you were; a click on the pet in the column's foot does the same |
| `ctrl-a i` | questions: on qass's screen, qass's questions beside its chat. A new question opens nothing by itself: it lights the questions tab on the rail and the lamp under the pet; `ctrl-a i` (or a click on that tab) brings it up, and the last answer puts it away. In it `j` `k` pick a question, `a` to `d` answer with an option (sent at once), `t` types your own words, `x` puts it away, `enter` goes to that agent's tab, `tab` gives the chat the keyboard, `esc` closes it. A click on the `questions` count in qass's header does the same |
| `ctrl-a T` | fold: on qass's screen, fold the pane the keyboard is in to its strip, or open the folded one |
| `ctrl-a W` | close the space when it is empty |
| `ctrl-a Z` | sleep, wake: put the tab the keyboard is in to sleep, or wake the sleeping tab it rests on |
| `ctrl-a f` | fold: the focused space's agents fold under its rule, or open again |

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
| `ctrl-a g` | tree or priority: agents grouped by space, or ranked by priority |
| `ctrl-a a` | agents column: the keyboard on it, to move over its rows, open, sleep, search |
| `ctrl-a tab` | needs you: the agent that needs you next |
| `ctrl-a M` | last answer: the focused agent's last answer, read from its transcript, as a page |
| `ctrl-a F` | an agent's focus view: your last prompt, its last turns, one row per tool call |
| `ctrl-a t` | chat: show an agent's conversation instead of its terminal, and back. The terminal keeps running and typing still goes to it; the wheel scrolls the chat, and copy mode and the mouse copy the text you see. A prompt waiting for you shows the terminal until it is answered. Remembered per agent program (`claude`, `codex`), so the next tab of that program starts the same way |
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
| click the pet at the foot of the column | qass, or back to where you were: the same as `ctrl-a Q` |
| click the `questions` count in qass's header | open or close the questions, the same as `ctrl-a i` |
| click a space's `⋯` | the space's settings: its name and colour |
| drag the column's edge | widen or narrow the left column, up to half the window |
| shift-drag | select over a program that took the mouse |
| wheel | scroll back through the history |

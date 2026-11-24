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
| `ctrl-a Q` | qass's ledger over the pane, or put away: how qass stands with `p` to pause or resume it (`s` starts it when it is stopped, and opens its terminal), a link to the questions it asked you (`i` opens them in the Inbox), what it holds, what it did today as the daemon recorded it, its schedules (add or change them with `qoop schedule`), its rules as a count, and a line to ask it (`a`, then `enter`). `t` opens qass's own terminal, `esc` goes back. A click on the pet in the column's foot does the same |
| `ctrl-a i` | Inbox: from any tab, everything that needs you as one list, oldest first, one card each, with the count in its title. Below 100 columns it folds the agents column while it is open. In it `j` `k` move, `1` to `9` answer with the option of that number (the answer waits 3 seconds, `u` takes it back, `enter` sends it now), `h` hands the question to qass to decide, `r` types your own words, `g` goes to that agent's tab, `o` talks it through with the agent, `m` reads the whole card, `x` puts it away, `f` shows only what qass asked (again to clear), `tab` shows the Board in place of the list (read only: waiting on you, doing, todo and done; with other computers connected `c` turns to the next computer's board; `tab` again goes back), `n` starts a new agent in a new worktree (what to do, which space and agent, the branch; `tab` moves between them, `enter` starts it), `esc` closes it. A click on the `needs you` title of the agents column opens it too |
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
| `ctrl-a space` | find anything: a space, an agent, a tab, an action, an action of an installed [herdr plugin](#plugins) |
| `ctrl-a [` | copy mode: move, select, search the whole history (see [Copy mode](#copy-mode)) |
| `ctrl-a g` | priority or tree: the agents column opens on the priority list (Needs you, With qass, Finished, Working, and the idle and asleep folded to one row that `enter` or a click lists, with every computer on the last row above the foot), or shows the agents grouped by space |
| `ctrl-a a` | agents column: the keyboard on it, to move over its rows, open, sleep, search |
| `ctrl-a tab` | needs you: the agent that needs you next |
| `ctrl-a M` | last answer: the focused agent's last answer, read from its transcript, as a page |
| `ctrl-a F` | an agent's focus view: your last prompt, its last turns, one row per tool call |
| `ctrl-a t` | chat: show an agent's conversation instead of its terminal, and back. The terminal keeps running and typing still goes to it; the wheel scrolls the chat, and copy mode and the mouse copy the text you see. A prompt waiting for you shows the terminal until it is answered. Remembered per agent program (`claude`, `codex`), so the next tab of that program starts the same way |
| `ctrl-a enter` | chat: open or close the newest group or tool step in view (what it did, then its input and output). A bare `enter` always goes to the program. A click on a step does the same; with shift held the click selects text instead |
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
| click | focus a tab, a pane, a space or an agent; in a chat, open or close the step you click (shift-click selects over it instead) |
| drag | select, and copy when you let go; scroll while the button is down and the selection runs on, and every row it covers is copied, also rows scrolled out of view |
| drag a divider | resize the panes on either side |
| click a space's `+`, or the tab bar's | a new agent, terminal or agent in a worktree in that space, in its directory; an agent asks for its name (empty for the agent's own) |
| click the pet at the foot of the column | qass's ledger: the same as `ctrl-a Q` |
| click a computer on the computers line of the agents column | that computer's own screen: how it is reached and since when, and its agents as they were when it last answered; `r` tries a computer that is not reached again |
| click the `needs you` title in the agents column | open the Inbox, the same as `ctrl-a i` |
| click a space's `⋯` | the space's settings: its name and colour |
| drag the column's edge | widen or narrow the left column, up to half the window |
| shift-drag | select over a program that took the mouse |
| wheel | scroll back through the history |

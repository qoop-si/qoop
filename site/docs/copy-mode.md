# Copy mode and search

`ctrl-a [` puts a cursor on the focused pane and lets you move through everything it ever
printed: the screen and the whole history on disk behind it.

| Key | Does |
|---|---|
| `h j k l`, arrows | move |
| `w b e` | next word, previous word, end of word |
| `0 ^ $`, Home, End | start of line, first character, end of line |
| `ctrl-u ctrl-d` | half a page up, down |
| `ctrl-b ctrl-f`, PageUp PageDown | a page up, down |
| `g G` | the oldest row, the newest |
| `v` or space | start or stop a selection |
| `V` | select whole lines |
| `y` or enter | copy the selection and leave (with nothing selected, just leave) |
| `/`, `?` | search forward, backward |
| `n`, `N` | next match, previous |
| `esc` | clear the selection, then leave |
| `q` | leave |

The wheel scrolls the view and keeps the cursor inside it; a click leaves copy mode and acts
as a click. The prefix leaves too and does what it would have done, so `ctrl-a d` still
detaches.

## Search

Search looks at the screen first, then at the history on disk, however long it is. It is
case-insensitive unless the query has a capital letter. Wide characters (Chinese, Japanese,
Korean) match as typed. On a long history the search goes in steps of about 100 ms; while
it has not reached the end the status line says how far it looked, and `n` carries on from
there. Measured on a history of 7.1 million rows: a search to the very first row takes
about half a second.

## Copying

A selection is copied to the system clipboard when the terminal can reach it (pbcopy,
wl-copy, xclip, xsel), and otherwise through the terminal itself (OSC 52), including inside
tmux and screen. The status line says which. A selection reaching far back into the history
is fetched whole before it is copied, so a 2,000-line selection copies 2,000 lines.

While you are in copy mode the program keeps running and printing; the view stays where you
put it.

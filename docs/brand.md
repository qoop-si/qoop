# Brand: palette, logo, marks

The one place the product's colours and its dot language are written down. The terminal
interface (`crates/qoro-tui/src/theme.rs`), the phone (`mobile/src/ui/theme.ts`) and anything
drawn for the site take their colours from here.

## Palette

| Name | Hex | Terminal (256) | Spent on |
| --- | --- | --- | --- |
| violet | `5A00EB` | 177 (a lighter step) | a space |
| ground | `0F0F11` | | the logo's near-black |
| sky | `00BBFF` | 39 | working |
| blue | `0044D7` | | |
| cream | `FCF7F1` | 231 | text, the logo's letters |
| ember | `FD4B03` | 202 | the brand: the q's tail |
| pink | `FF8FD9` | 218 (a lighter step) | a space |
| orange | `FF7700` | 208 | wants |
| green | `00BB54` | 35 | done |
| lime | `C7F245` | 191 | a space |
| yellow (bonus) | `F7F245` | 228 (a lighter step) | a space |

A terminal draws 256 colours, so each hex lands on its nearest entry, or on a lighter step of
the same family where the exact one is too dark to read on a black ground.

States that are not in the palette keep their own inks: failed is magenta 201, idle is grey
242, asleep is lilac 140. The selected tab is pink 212.

### Space colours

Six, from the families no state spends, in wheel order: violet 177, lime 191, cyan 87 (a
lighter sky), pink 218, sky 117, yellow 228. On a light terminal: 92, 58, 23, 125, 24, 94,
every one at least 4.5 to 1 on white, since a space's colour is worn by its agents' titles.
The two closest pairs (cyan and sky; lime and yellow) are never neighbours on the wheel.

A calm agent's title (idle, done and read, asleep) wears a quieter step of its space's colour:
134, 107, 73, 175, 67, 143 on a dark terminal, 98, 64, 29, 95, 27, 100 on a light one. Each is
lighter on white, or darker on black, than its base. Lime and yellow on white are the two the
palette cannot give at 4.5 to 1 once the base holds it (64 is 4.2, 100 is 3.8); the rest hold
4.5.

## Logo

Letters on the nine-dot grid, three dots by three. The q is the full ring with one more dot
hanging under its right side: its tail, the only dot in ember. The letters are cream on the
ground, or ground on cream.

- `brand/q-mark-dark.svg`, `brand/q-mark-light.svg`: the q alone as an app icon, the nine
  dots dead centre and the tail below them, outside the centring.
- `brand/q-mark-bare-on-dark.svg`, `brand/q-mark-bare-on-light.svg`: the same with no tile.
- `brand/qoro-word-*.svg`: the word; the three rows centred on the height, the q's tail under
  the line, the r's first dot in ember too.

`brand/make.py` draws all of them (`uv run --no-project python docs/brand/make.py`).

The terminal's foot draws the q alone in thick dots, the ring of `make.py` one `●` a cell with
a blank cell between dots, so the gaps across and down read equal on a cell about half as wide
as it is tall, and the tail in ember under the ring's right column: four rows, five cells, the
pet beside it.

```text
● ● ●
●   ●
● ● ●
    ●
```

Every dot is cream but the tail, in ember as in the SVGs. Each dot is a cell of its own, so the
tail is exact. The last row of the column is the menu row: the version at the left, `menu «` at
the right with `settings` before it (from 24 columns; a click opens the settings page), and
the q sits on the four rows right above it. The picked pet stands on the menu
row at the right, five rows of half blocks with its feet on the row above the menu, and adds
only a blank row above the q, so no word moves whatever the pet. Sixteen columns hold the q
and the widest pet side by side, fewer than the narrowest the column drags to.

A column too narrow for that falls back to the word in braille (`⣖⡆⣖⡆⡖⠀⣖⡆`), cream letters on
the lower three dot rows of its row, and the q's tail as one ember dot (`⠁`) alone on the row
under it; there the r's first dot shares its cell with the r's stem and stays cream. Where
braille is not drawn that small word is `● qoro` in ember.

## State marks

Every agent wears a picture in the same three by three grid, in its state's colour, two
braille cells wide (the fourth dot column is the gap before the name):

| State | Picture |
| --- | --- |
| working | the centre breathing: arms come in clockwise and go back out, each breath one arm further round |
| wants | the empty ring |
| failed | an X |
| done | the full square |
| idle | the centre dot |
| asleep | a line across the middle |

A working frame never draws another state's picture. Where braille cannot be drawn (the
kernel console, a dumb terminal) the marks are `- \ | / ~ ? ! * . z`.

The grid lines up only where the terminal draws braille itself (Ghostty, kitty, WezTerm, VS
Code). A terminal that takes braille from the font (iTerm2, Terminal.app, Warp, Zed,
Alacritty, Windows Terminal, GNOME Terminal, Konsole) spaces its two cells apart, so there
each mark is a two by two picture in one cell and a space:

| State | Picture |
| --- | --- |
| working | three dots, the gap running round clockwise |
| wants | the rising diagonal |
| failed | the falling diagonal |
| done | the full square |
| idle | the lower left dot |
| asleep | the lower line |

A space is its name in its colour, starting in the column the marks start in, with a line of
dots in the same colour down its left edge from halfway down its own row to a foot beside its
last row.

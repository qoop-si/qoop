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
lighter sky), pink 218, sky 117, yellow 228. On a light terminal: 92, 64, 30, 125, 24, 100.
The two closest pairs (cyan and sky; lime and yellow) are never neighbours on the wheel.

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

The terminal's foot draws the word in braille (`⣖⡆⣖⡆⡖⠀⣖⡆`), cream letters on the lower three
dot rows of its row, and the q's tail as one ember dot (`⠁`) opening the row under it, right
under the q's right column, with the version beside it. A terminal cell holds one colour, so
the tail gets a cell of its own; the r's first dot shares its cell with the r's stem and
stays cream there. The pet's name sits under the version, and the picked pet in dots at the
right beside those three rows. Where braille is not drawn the foot says `● qoro` in ember.

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

A space is its name in its colour, starting in the column the marks start in, with a line of
dots in the same colour down its left edge from halfway down its own row to a foot beside its
last row.

# Terminal support: what qoop expects from the outer terminal

The interface adapts to the terminal it runs in instead of demanding a
particular one. This page says what each terminal gets, what degrades, and
which switch overrides the detection when it guesses wrong.

## Capability summary

| terminal | colours | light detect | emoji width | mouse | images | sync 2026 | kitty keys |
|---|---|---|---|---|---|---|---|
| Ghostty | truecolour | OSC 11, scheme | 2 cells | yes | kitty | yes | yes |
| iTerm2 | truecolour | OSC 11, scheme | 2 cells | yes | kitty, iterm2 | yes (3.5+) | partial |
| WezTerm | truecolour | OSC 11, scheme | 2 cells | yes | kitty, iterm2 | yes | yes |
| Kitty | truecolour | OSC 11, scheme | 2 cells | yes | kitty | yes | yes |
| VTE (GNOME Terminal and friends) | truecolour | OSC 11 recent | 2 cells | yes | placeholder | recent only | partial |
| Alacritty | truecolour | OSC 11 | 2 cells | yes | placeholder | no | partial |
| Terminal.app pre Tahoe | 256 | dark default | often 1 cell | no | placeholder | no | no |
| Terminal.app Tahoe | truecolour | probe pending | 2 cells | no | placeholder | no | no |
| Windows Terminal | truecolour | OSC 11 | 2 cells | yes | placeholder | yes | partial |
| Legacy conhost | none (Mono) | dark | mixed | no | placeholder | no | no |
| Linux console | none (Mono) | dark | 1 cell | no | placeholder | no | no |
| Dumb pipe | none (Mono) | dark | n/a | no | placeholder | no | no |

Placeholder means the picture's stand-in line (`[image 800x600, 123KB]`),
never raw bytes. Sixel pictures are captured and named but placed only with
`QOOP_GRAPHICS=sixel` or `all`: no outer sixel support is verified on a
device yet.

## Attribute degradation

Pane programs may use styles the outer terminal cannot draw. What happens
then, per style:

* Bold, inverse: everywhere, including Mono. Never dropped.
* Italic: flat (no slant) on Terminal.app and several others; text stays.
* Dim: low contrast on light grounds; readable on dark.
* Strikethrough: missing on several terminals; text stays.
* Double underline, undercurl, dotted, dashed: drawn as single underline.
* Truecolour content: mapped to the nearest 256 entry outside the themed
  0 to 15 range when the terminal takes 256 but not RGB; attributes only
  on Mono.
* Emoji, ZWJ families, flags: best effort alignment on desktop and phone;
  never used by the chrome itself, so layout never depends on them.

## Detection and overrides

Colour level order: `QOOP_COLOR` (`none`, `256`, `true`), the safety floor
(`dumb`, `linux`, legacy conhost), `NO_COLOR`, `COLORTERM`, a conservative
Apple Terminal default, `TERM` suffix hints, then 256. A leaked `COLORTERM`
never lifts the floor.

Light and dark order: config `theme`, OSC 10 and 11 answers, the scheme
report, `COLORFGBG`, then dark. Under tmux the queries wrap in DCS
passthrough; without an answer the config wins.

| switch | effect |
|---|---|
| `theme = "dark"` or `"light"` in config.toml | pins the palette, skips detection |
| `QOOP_COLOR=none` | attributes only, honours `NO_COLOR` everywhere |
| `QOOP_COLOR=256` | never truecolour, even when advertised |
| `QOOP_COLOR=true` | forces truecolour passthrough |
| `QOOP_GRAPHICS=kitty`, `iterm2`, `sixel`, `all`, `off` | pins image placement |
| `QOOP_MOTION=0` | still column, independent of colour |
| `NO_COLOR` | no colour anywhere; motion keeps its own switch |
| `qoop reset` | restores a terminal left in a bad mode |

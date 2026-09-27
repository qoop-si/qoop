# Panes and splits

A tab can be split into panes: `ctrl-a v` beside, `ctrl-a -` below. Each pane is its own
terminal; a split is not a new tab, but its agent still has its row in the agents panel.

Move between panes with `ctrl-a H J K L` or a click, cycle with `ctrl-a o`, and zoom one to
the full pane with `ctrl-a z`. `ctrl-a X` takes a pane out of the split and keeps it as a
tab.

## Resizing

- **Mouse:** drag the divider between two panes.
- **Keyboard:** `ctrl-a e` enters resize mode. `h j k l` or the arrows move the edge 5% in
  that direction, `H J K L` move it 20%, `=` equalizes, and `esc`, `enter`, `q` or `e`
  leave. Any other key leaves the mode and goes where it would have gone.
- **Equalize:** `ctrl-a =` gives every pane its share: three panes side by side get a third
  each, and in `[A | (B over C)]` A gets half.

A move that would squeeze a pane below its minimum size, or hide one, is refused and the
divider stays where it was.

The focused pane's program is told its new size. Other visible panes keep theirs and are
drawn at the size of their tile until you focus them.

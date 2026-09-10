//! A client's window onto a terminal that may be larger than it.
//!
//! ADR-007 keeps one owner and one size for the terminal itself. A client that cannot show
//! the whole grid does not get to change it, and does not get a different grid either: it
//! gets a viewport, which is an origin plus its own size, clamped to the grid.
//!
//! The viewport lives on the client, not on the daemon. Cropping in the daemon means
//! per-client state that has to be invalidated on every resize, and the saving is bytes on
//! a local socket. The other half of that decision is mouse coordinates: the client is the
//! only party that knows where its viewport is, so it is the party that translates them.

use crate::sizing::PtySize;

/// Where a client's window sits on the terminal's grid, and how big it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl Viewport {
    /// A viewport of the client's size, clamped so that it always lies inside the grid.
    ///
    /// Clamping rather than refusing: a client smaller than the terminal is the ordinary
    /// case that this exists for, and a client larger than it is not an error either, it
    /// just sees the whole thing with room to spare.
    pub fn new(grid: PtySize, col: u16, row: u16, cols: u16, rows: u16) -> Self {
        let cols = cols.min(grid.cols);
        let rows = rows.min(grid.rows);
        Self {
            col: col.min(grid.cols.saturating_sub(cols)),
            row: row.min(grid.rows.saturating_sub(rows)),
            cols,
            rows,
        }
    }

    /// Move the window so that a point is inside it, changing as little as possible.
    ///
    /// This is what follows the cursor: the window moves only when the cursor would
    /// otherwise leave it, so a client's view does not jump on every keystroke.
    pub fn follow(self, grid: PtySize, col: u16, row: u16) -> Self {
        let mut next = self;
        if col < next.col {
            next.col = col;
        } else if col >= next.col.saturating_add(next.cols) {
            next.col = col.saturating_sub(next.cols.saturating_sub(1));
        }
        if row < next.row {
            next.row = row;
        } else if row >= next.row.saturating_add(next.rows) {
            next.row = row.saturating_sub(next.rows.saturating_sub(1));
        }
        Self::new(grid, next.col, next.row, next.cols, next.rows)
    }

    /// A point on the client's own window, translated to the terminal's grid.
    ///
    /// Mouse reporting needs exactly this, and getting it wrong is not a visual glitch: the
    /// child is told a click happened somewhere it did not.
    pub fn to_grid(self, col: u16, row: u16) -> (u16, u16) {
        (
            self.col
                .saturating_add(col.min(self.cols.saturating_sub(1))),
            self.row
                .saturating_add(row.min(self.rows.saturating_sub(1))),
        )
    }

    /// The rows of a grid that this viewport shows, cropped to its own width.
    pub fn crop<'a>(self, lines: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        lines
            .into_iter()
            .skip(self.row as usize)
            .take(self.rows as usize)
            .map(|line| {
                line.chars()
                    .skip(self.col as usize)
                    .take(self.cols as usize)
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(cols: u16, rows: u16) -> PtySize {
        PtySize { cols, rows }
    }

    #[test]
    fn a_small_client_sees_a_window_and_not_a_different_terminal() {
        let v = Viewport::new(grid(180, 50), 0, 0, 50, 20);
        assert_eq!((v.cols, v.rows), (50, 20));
        let lines: Vec<String> = (0..50).map(|r| format!("{r:0>180}")).collect();
        let shown = v.crop(lines.iter().map(String::as_str));
        assert_eq!(shown.len(), 20);
        assert_eq!(shown[0].chars().count(), 50);
    }

    #[test]
    fn a_client_larger_than_the_terminal_sees_all_of_it_and_no_more() {
        let v = Viewport::new(grid(80, 24), 0, 0, 200, 60);
        assert_eq!((v.cols, v.rows), (80, 24));
        assert_eq!((v.col, v.row), (0, 0));
    }

    #[test]
    fn an_origin_past_the_edge_is_pulled_back_inside() {
        let v = Viewport::new(grid(100, 40), 90, 38, 50, 20);
        assert_eq!(v.col, 50, "the window would have hung off the right edge");
        assert_eq!(v.row, 20, "the window would have hung off the bottom");
    }

    #[test]
    fn following_the_cursor_moves_the_window_only_when_it_has_to() {
        let grid = grid(180, 50);
        let v = Viewport::new(grid, 0, 0, 50, 20);
        // Inside already: nothing moves.
        assert_eq!(v.follow(grid, 10, 10), v);
        // Past the right edge: moves by exactly enough.
        let moved = v.follow(grid, 60, 10);
        assert_eq!(moved.col, 11);
        assert_eq!(moved.row, 0, "a horizontal move must not scroll vertically");
    }

    #[test]
    fn a_click_is_translated_to_where_it_really_is() {
        let v = Viewport::new(grid(180, 50), 60, 20, 50, 20);
        assert_eq!(v.to_grid(0, 0), (60, 20));
        assert_eq!(v.to_grid(49, 19), (109, 39));
        // A click past the window's own edge is clamped, never reported outside it.
        assert_eq!(v.to_grid(500, 500), (109, 39));
    }

    #[test]
    fn cropping_a_shorter_line_does_not_invent_columns() {
        let v = Viewport::new(grid(80, 24), 40, 0, 20, 5);
        let shown = v.crop([
            "short",
            "0123456789012345678901234567890123456789012345678901234567890",
        ]);
        assert_eq!(
            shown[0], "",
            "a line that ends before the window is empty, not padded"
        );
        assert_eq!(
            shown[1].chars().count(),
            20,
            "the window is 20 columns wide"
        );
    }
}

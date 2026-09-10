//! Painting a frame, as a pure function of what the daemon said.
//!
//! No terminal library. What has to be produced is a byte string of positioned writes, and
//! the alternative was a dependency whose own idea of a frame would then have to be
//! reconciled with the one the daemon already sends. Everything here is text in, text out,
//! so a test can read a frame instead of looking at one.

use qoro_runtime::control::AgentRow;
use qoro_runtime::daemon::SnapshotWire;

use crate::index;

/// What the screen shows, which is an index, one terminal, and a line saying what the keys
/// do.
pub struct Frame<'a> {
    pub rows: &'a [AgentRow],
    pub focused: u32,
    /// The focused terminal's last snapshot. `None` before the first one arrives, which is
    /// a real state: a terminal that has produced nothing yet has nothing to draw.
    pub snapshot: Option<&'a SnapshotWire>,
    pub cols: u16,
    pub lines: u16,
    /// Shown instead of the key list, when there is something to say.
    pub message: Option<&'a str>,
}

/// The columns and rows left for the terminal once the index and the status line have
/// taken theirs.
///
/// Public because it is also what the client tells the daemon to size the child to, and a
/// second copy of that arithmetic in the caller is how a pane and its child end up
/// disagreeing about how wide they are.
pub fn pane_size(cols: u16, lines: u16) -> (u16, u16) {
    (
        cols.saturating_sub(index::WIDTH as u16 + 1).max(1),
        lines.saturating_sub(1).max(1),
    )
}

/// The escape sequence that puts the cursor at a one-based row and column.
fn at(row: usize, col: usize) -> String {
    format!("\x1b[{row};{col}H")
}

/// Paint the whole screen.
///
/// Whole, not incremental. A terminal snapshot already arrives as complete rows, so the
/// saving from a diff is one write of a few kilobytes against another, and the cost of
/// getting a diff wrong is a screen that is quietly out of date. When a frame is expensive
/// enough to matter, the daemon has already coalesced it.
pub fn paint(frame: &Frame) -> String {
    let (pane_cols, pane_rows) = pane_size(frame.cols, frame.lines);
    let mut out = String::with_capacity(frame.cols as usize * frame.lines as usize + 256);

    // Hidden while painting: a cursor walking the screen during a repaint is the flicker
    // people notice most.
    out.push_str("\x1b[?25l");

    let index_lines = index::render(frame.rows, frame.focused, pane_rows as usize);
    let blank = " ".repeat(pane_cols as usize);
    for row in 0..pane_rows as usize {
        out.push_str(&at(row + 1, 1));
        out.push_str(index_lines.get(row).map(String::as_str).unwrap_or(""));
        // The rule between the index and the terminal, drawn every row so that a terminal
        // line long enough to reach it cannot swallow it.
        out.push('\u{2502}');
        let line = frame
            .snapshot
            .and_then(|s| s.lines.get(row))
            .map(String::as_str)
            .unwrap_or("");
        out.push_str(&fit(line, pane_cols as usize, &blank));
    }

    out.push_str(&at(frame.lines as usize, 1));
    out.push_str("\x1b[7m");
    let status = frame.message.map(str::to_string).unwrap_or_else(|| {
        "ctrl-a n next  ctrl-a c new  ctrl-a x close  ctrl-a d detach".to_string()
    });
    out.push_str(&fit(
        &status,
        frame.cols as usize,
        &" ".repeat(frame.cols as usize),
    ));
    out.push_str("\x1b[0m");

    // The cursor goes where the child put it, offset into the pane, so that what the user
    // types appears where they are looking.
    if let Some(snapshot) = frame.snapshot {
        let (row, col) = snapshot.cursor;
        let row = row.min(pane_rows as usize - 1) + 1;
        let col = col.min(pane_cols as usize - 1) + index::WIDTH + 2;
        out.push_str(&at(row, col));
        out.push_str("\x1b[?25h");
    }
    out
}

/// A line padded or cut to exactly `width` columns.
///
/// Counted in characters and not bytes, because a cut inside a multi byte character puts
/// a replacement glyph on the screen, and this program has already had that bug once in
/// its parser.
fn fit(line: &str, width: usize, blank: &str) -> String {
    let count = line.chars().count();
    if count == width {
        return line.to_string();
    }
    if count > width {
        return line.chars().take(width).collect();
    }
    let mut out = String::with_capacity(width);
    out.push_str(line);
    out.push_str(&blank[..width - count]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(terminal: u32, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: Some(1),
        }
    }

    fn snapshot(lines: &[&str]) -> SnapshotWire {
        SnapshotWire {
            cols: 40,
            rows: lines.len(),
            cursor: (0, 0),
            lines: lines.iter().map(|l| l.to_string()).collect(),
        }
    }

    #[test]
    fn the_pane_is_what_is_left_after_the_index_and_the_status_line() {
        assert_eq!(pane_size(80, 24), (80 - index::WIDTH as u16 - 1, 23));
    }

    #[test]
    fn a_window_too_small_for_the_index_still_leaves_a_pane() {
        // Never zero and never a panic: a person dragging a window narrow is not an error.
        let (cols, rows) = pane_size(4, 1);
        assert!(cols >= 1 && rows >= 1);
    }

    #[test]
    fn the_terminals_output_and_the_index_are_both_in_the_frame() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            rows: &rows,
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
        });
        assert!(painted.contains("hello from the shell"));
        assert!(painted.contains("claude"));
        assert!(painted.contains("ctrl-a d detach"));
    }

    #[test]
    fn a_terminal_line_cannot_overwrite_the_rule_or_run_past_the_window() {
        let rows = [row(1, "zsh", "idle")];
        let long = "x".repeat(500);
        let snapshot = snapshot(&[&long]);
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 5,
            message: None,
        });
        let (pane_cols, _) = pane_size(40, 5);
        // Exactly the pane's width of it, and not one column more.
        assert!(painted.contains(&"x".repeat(pane_cols as usize)));
        assert!(!painted.contains(&"x".repeat(pane_cols as usize + 1)));
    }

    #[test]
    fn a_frame_with_no_snapshot_yet_still_paints() {
        let rows = [row(1, "zsh", "starting")];
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: None,
            cols: 40,
            lines: 5,
            message: Some("starting a terminal"),
        });
        assert!(painted.contains("starting a terminal"));
    }

    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        // The pane is 19 columns here, and the string is Turkish, so a byte cut would put
        // a replacement character on the screen.
        let snapshot = snapshot(&["şğüöçİĞÜÖÇşğüöçİĞÜÖÇşğüöçİĞÜÖÇ"]);
        let rows = [row(1, "zsh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 3,
            message: None,
        });
        assert!(!painted.contains('\u{fffd}'));
    }
}

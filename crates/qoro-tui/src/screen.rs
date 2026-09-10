//! Painting a frame, as a pure function of what the daemon said.
//!
//! No terminal library. What has to be produced is a byte string of positioned writes, and
//! the alternative was a dependency whose own idea of a frame would then have to be
//! reconciled with the one the daemon already sends. Everything here is text in, text out,
//! so a test can read a frame instead of looking at one.

use qoro_runtime::control::AgentRow;
use qoro_runtime::daemon::{ColourWire, SnapshotWire, SpanWire, attrs};

use crate::{index, width};

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

    let index_lines = index::render_lines(frame.rows, frame.focused, pane_rows as usize);
    for row in 0..pane_rows as usize {
        out.push_str(&at(row + 1, 1));
        // The state's colour, on the whole row. A person scanning an index is looking for
        // the one row that needs them, and a coloured word inside an otherwise grey line is
        // a word they have to find first.
        if let Some(line) = index_lines.get(row) {
            match line.colour {
                Some(colour) => {
                    out.push_str(&format!("\x1b[{}m", 30 + colour.min(7)));
                    out.push_str(&line.text);
                    out.push_str("\x1b[0m");
                }
                None => out.push_str(&line.text),
            }
        }
        // The rule between the index and the terminal, drawn every row so that a terminal
        // line long enough to reach it cannot swallow it.
        out.push('\u{2502}');
        let line = frame
            .snapshot
            .and_then(|s| s.lines.get(row))
            .map(String::as_str)
            .unwrap_or("");
        let spans = frame
            .snapshot
            .and_then(|s| s.styles.get(row))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        out.push_str(&paint_row(line, spans, pane_cols as usize));
    }

    out.push_str(&at(frame.lines as usize, 1));
    out.push_str("\x1b[7m");
    let status = frame.message.map(str::to_string).unwrap_or_else(|| {
        "ctrl-a n next  ctrl-a c new  ctrl-a x close  ctrl-a d detach".to_string()
    });
    out.push_str(&width::fit(&status, frame.cols as usize));
    out.push_str("\x1b[0m");

    // The cursor goes where the child put it, offset into the pane, so that what the user
    // types appears where they are looking.
    if let Some(snapshot) = frame.snapshot {
        let (row, col) = snapshot.cursor;
        // The cursor's column is a cell, and cells and columns are the same number only
        // until a wide character appears on that row. Measured in cells up to the cursor,
        // then painted in columns.
        let cells: String = frame
            .snapshot
            .and_then(|s| s.lines.get(row))
            .map(|line| line.chars().take(col).collect())
            .unwrap_or_default();
        let col = width::width(&width::fit(
            &cells,
            width::width(&cells).min(pane_cols as usize),
        ));
        let row = row.min(pane_rows as usize - 1) + 1;
        let col = col.min(pane_cols as usize - 1) + index::WIDTH + 2;
        out.push_str(&at(row, col));
        out.push_str("\x1b[?25h");
    }
    out
}

/// One row of the terminal, with its colour.
///
/// The runs are in the grid's columns and the text is painted in the screen's, and those
/// two are the same number only until a wide character appears on the row. So the row is
/// walked once, in cells, and each cell is emitted with the style the run says it has: a
/// style change writes one escape sequence and nothing else does.
fn paint_row(line: &str, spans: &[SpanWire], cols: usize) -> String {
    if spans.is_empty() {
        return width::fit(line, cols);
    }
    let mut out = String::with_capacity(cols * 2);
    let mut used = 0;
    let mut current: Option<&SpanWire> = None;
    // The grid puts a spacer after a wide character; it is a cell for the run arithmetic
    // and not a column for the screen, so it is counted and not painted.
    let mut skip_spacer = false;
    for (cell, c) in line.chars().enumerate() {
        if skip_spacer && c == ' ' {
            skip_spacer = false;
            continue;
        }
        skip_spacer = width::columns(c) == 2;
        let w = width::columns(c);
        if used + w > cols {
            break;
        }
        let span = spans
            .iter()
            .find(|s| cell >= s.at && cell < s.at + s.len)
            .filter(|s| !is_plain(s));
        if span.map(|s| s as *const _) != current.map(|s| s as *const _) {
            out.push_str("\x1b[0m");
            if let Some(span) = span {
                out.push_str(&sgr(span));
            }
            current = span;
        }
        out.push(c);
        used += w;
    }
    if current.is_some() {
        out.push_str("\x1b[0m");
    }
    for _ in used..cols {
        out.push(' ');
    }
    out
}

fn is_plain(s: &SpanWire) -> bool {
    s.fg.is_none() && s.bg.is_none() && s.attrs == 0
}

/// The escape sequence for one run's style.
fn sgr(span: &SpanWire) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (bit, code) in [
        (attrs::BOLD, "1"),
        (attrs::DIM, "2"),
        (attrs::ITALIC, "3"),
        (attrs::UNDERLINE, "4"),
        (attrs::INVERSE, "7"),
        (attrs::STRIKE, "9"),
    ] {
        if span.attrs & bit != 0 {
            parts.push(code.to_string());
        }
    }
    if let Some(fg) = span.fg {
        parts.push(colour(fg, true));
    }
    if let Some(bg) = span.bg {
        parts.push(colour(bg, false));
    }
    format!("\x1b[{}m", parts.join(";"))
}

/// A colour, written the way every terminal since xterm reads it.
///
/// The first sixteen palette entries get their own short codes rather than `38;5;n`,
/// because those are the ones a person has themed and some terminals treat the two spellings
/// differently.
fn colour(c: ColourWire, foreground: bool) -> String {
    match c {
        ColourWire::Indexed { i } if i < 8 => {
            format!("{}", if foreground { 30 + i } else { 40 + i })
        }
        ColourWire::Indexed { i } if i < 16 => {
            format!("{}", if foreground { 82 + i } else { 92 + i })
        }
        ColourWire::Indexed { i } => format!("{};5;{i}", if foreground { 38 } else { 48 }),
        ColourWire::Rgb { r, g, b } => {
            format!("{};2;{r};{g};{b}", if foreground { 38 } else { 48 })
        }
    }
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
            styles: Vec::new(),
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
    fn a_coloured_run_reaches_the_screen_and_is_closed_after_it() {
        let mut snapshot = snapshot(&["red then plain"]);
        snapshot.styles = vec![vec![SpanWire {
            at: 0,
            len: 3,
            fg: Some(ColourWire::Indexed { i: 1 }),
            bg: None,
            attrs: attrs::BOLD,
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 3,
            message: None,
        });
        // Bold and red on, the text, and then off again: a run that is not closed leaks its
        // colour into the rest of the screen, which is the whole pane and then the status
        // line.
        assert!(painted.contains("\x1b[1;31mred"), "{painted:?}");
        assert!(painted.contains("\x1b[0m"), "{painted:?}");
        assert!(painted.contains("then plain"));
    }

    #[test]
    fn an_unstyled_row_carries_no_escape_of_its_own() {
        // The common case, and the one that must not pay for the feature: a plain row is
        // the same bytes it was before colour existed.
        let snapshot = snapshot(&["plain"]);
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 2,
            message: None,
        });
        // Only the pane's own bytes on the first row: the status line legitimately uses
        // reverse video and resets it, and splitting on the divider alone would catch that.
        let after_divider = painted.split('\u{2502}').nth(1).unwrap_or_default();
        let pane = after_divider.split('\x1b').next().unwrap_or_default();
        assert!(pane.starts_with("plain"), "{pane:?}");
        assert!(!pane.contains('\x1b'), "{pane:?}");
    }

    #[test]
    fn a_colour_run_lines_up_with_a_wide_character() {
        // The run is in cells and the paint is in columns. A style that started on the
        // spacer of a wide character would colour the wrong glyph.
        let mut snapshot = snapshot(&["中 X"]);
        snapshot.styles = vec![vec![SpanWire {
            at: 2,
            len: 1,
            fg: Some(ColourWire::Indexed { i: 2 }),
            bg: None,
            attrs: 0,
        }]];
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 30,
            lines: 2,
            message: None,
        });
        // `X` is at cell 2, so it is the green one, and the character before it is not.
        assert!(painted.contains("\x1b[32mX"), "{painted:?}");
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

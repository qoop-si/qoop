//! Painting a frame, as a pure function of what the daemon said.
//!
//! No terminal library. What has to be produced is a byte string of positioned writes, and
//! the alternative was a dependency whose own idea of a frame would then have to be
//! reconciled with the one the daemon already sends. Everything here is text in, text out,
//! so a test can read a frame instead of looking at one.

use qoro_runtime::control::{AgentRow, WorkspaceRow};
use qoro_runtime::daemon::{ColourWire, SnapshotWire, SpanWire, attrs};

use crate::{sidebar, tabs, width};

/// What the screen shows: the tab bar across the top, the spaces and agents down the left,
/// one terminal filling the rest, and a line saying what the keys do.
pub struct Frame<'a> {
    /// Every terminal the daemon reports, across every space. The panel on the left is a
    /// list across spaces on purpose, so it takes the whole thing.
    pub rows: &'a [AgentRow],
    pub spaces: &'a [WorkspaceRow],
    /// The space whose tabs are on the bar. `None` before the daemon has answered.
    pub space: Option<i64>,
    pub focused: u32,
    /// The focused terminal's last snapshot. `None` before the first one arrives, which is
    /// a real state: a terminal that has produced nothing yet has nothing to draw.
    pub snapshot: Option<&'a SnapshotWire>,
    pub cols: u16,
    pub lines: u16,
    /// Shown instead of the key list, when there is something to say.
    pub message: Option<&'a str>,
}

/// Rows the tab bar takes, at the top of the screen.
pub const TABS: usize = 1;

/// Rows the status line takes, at the bottom.
pub const STATUS: usize = 1;

/// The columns and rows left for the terminal once the tab bar, the left column and the
/// status line have taken theirs.
///
/// Public because it is also what the client tells the daemon to size the child to, and a
/// second copy of that arithmetic in the caller is how a pane and its child end up
/// disagreeing about how wide they are.
pub fn pane_size(cols: u16, lines: u16) -> (u16, u16) {
    (
        cols.saturating_sub(left(cols) as u16).max(1),
        lines.saturating_sub((TABS + STATUS) as u16).max(1),
    )
}

/// The columns spent before the pane starts: the left column and the rule beside it, or
/// nothing at all when the window is too narrow to hold both.
///
/// Dropped rather than squeezed. The left column is a fixed width and the rule is one more,
/// so a window of twenty three columns or fewer had every row painted wider than the
/// terminal: each line wrapped, the screen slid up, and every position this module writes
/// addressed the wrong row, which also puts every click on the wrong thing. Measured at 10,
/// 20 and 23 columns, all of which painted 24. A narrow window is a real one: two panes
/// side by side in a tmux split is how it happens.
pub fn left(cols: u16) -> usize {
    // One column for the pane, or there is nothing to show and the left column is the whole
    // point of nothing.
    if (cols as usize) > sidebar::WIDTH + 2 {
        sidebar::WIDTH + 1
    } else {
        0
    }
}

/// The terminals of one space, in the order the daemon reports them.
///
/// The tab bar is per space and the panel on the left is not, so this filter lives here
/// rather than in either of them: both a click and a repaint have to agree on which
/// terminals the bar is carrying, and two copies of the filter is how they stop agreeing.
///
/// A `None` space is every terminal, which is the honest answer before the daemon has said
/// what spaces exist: showing nothing at that moment would read as a daemon with no
/// terminals in it.
pub fn in_space(rows: &[AgentRow], space: Option<i64>) -> Vec<AgentRow> {
    rows.iter()
        .filter(|r| space.is_none_or(|s| r.workspace == s))
        .cloned()
        .collect()
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

    // The tab bar spans the whole width, above both columns. Above and not beside, because
    // it belongs to the space and the space is what the column on the left selects.
    out.push_str(&at(1, 1));
    out.push_str("\x1b[2m");
    out.push_str(
        &tabs::render(
            &in_space(frame.rows, frame.space),
            frame.focused,
            frame.cols as usize,
        )
        .text,
    );
    out.push_str("\x1b[0m");

    let wide_enough = left(frame.cols) > 0;
    let panel = if wide_enough {
        sidebar::render(
            frame.spaces,
            frame.rows,
            frame.space.unwrap_or_default(),
            frame.focused,
            pane_rows as usize,
        )
    } else {
        Vec::new()
    };
    for row in 0..pane_rows as usize {
        out.push_str(&at(row + 1 + TABS, 1));
        // The state's colour, on the whole row. A person scanning this column is looking
        // for the one row that needs them, and a coloured word inside an otherwise grey
        // line is a word they have to find first.
        if let Some(line) = panel.get(row) {
            match line.colour {
                Some(colour) => {
                    out.push_str(&format!("\x1b[{}m", 30 + colour.min(7)));
                    out.push_str(&line.text);
                    out.push_str("\x1b[0m");
                }
                None => out.push_str(&line.text),
            }
        }
        // The rule between the column and the terminal, drawn every row so that a terminal
        // line long enough to reach it cannot swallow it.
        if wide_enough {
            out.push('\u{2502}');
        }
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
    // The prefix once, then the letters. Spelled out for every key it took 96 columns, and
    // a person on an eighty column window was told about the first three commands and not
    // about the one that gets them out.
    let status = frame.message.map(str::to_string).unwrap_or_else(|| {
        "ctrl-a: n tab  c new  x close  s space  w new space  d detach".to_string()
    });
    out.push_str(&width::fit(&status, frame.cols as usize));
    out.push_str("\x1b[0m");

    // The cursor goes where the child put it, offset into the pane, so that what the user
    // types appears where they are looking.
    if let Some(snapshot) = frame.snapshot {
        let (row, cell) = snapshot.cursor;
        let col = cursor_column(
            snapshot.lines.get(row).map(String::as_str).unwrap_or(""),
            cell,
        );
        let row = row.min(pane_rows as usize - 1) + 1 + TABS;
        let col = col.min(pane_cols as usize - 1) + left(frame.cols) + 1;
        out.push_str(&at(row, col));
        out.push_str("\x1b[?25h");
    }
    out
}

/// The screen column the cursor sits at, counted from zero, given the row it is on and the
/// grid cell it is in.
///
/// Two conversions, and the old arithmetic got both of them backwards.
///
/// A snapshot row arrives right trimmed, so a cursor to the right of the last printed
/// character is in a cell the string does not contain. Taking that many characters returned
/// fewer, and the cursor landed left of where the person was typing: measured against a
/// `sh` prompt, which prints `"$ "` and arrives as `"$"`, the cursor sat on top of the `$`
/// on every line anybody typed. The cells past the end are spaces the engine dropped, and
/// each of them is one column.
///
/// The other way, the grid's spacer after a wide character is a cell and not a column, so
/// counting it moved the cursor one column right for every wide character on the row.
fn cursor_column(line: &str, cell: usize) -> usize {
    let mut cells = 0;
    let mut cols = 0;
    let mut spacer = false;
    for c in line.chars() {
        if cells == cell {
            break;
        }
        cells += 1;
        if spacer && c == ' ' {
            spacer = false;
            continue;
        }
        spacer = width::columns(c) == 2;
        cols += width::columns(c);
    }
    cols + (cell - cells)
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

    fn spaces() -> Vec<WorkspaceRow> {
        vec![WorkspaceRow {
            id: 1,
            name: "app".to_string(),
            terminals: 1,
        }]
    }

    fn row(terminal: u32, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            workspace: 1,
            agent: true,
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
        assert_eq!(pane_size(80, 24), (80 - sidebar::WIDTH as u16 - 1, 22));
    }

    #[test]
    fn a_window_too_small_for_the_index_still_leaves_a_pane() {
        // Never zero and never a panic: a person dragging a window narrow is not an error.
        let (cols, rows) = pane_size(4, 1);
        assert!(cols >= 1 && rows >= 1);
    }

    #[test]
    fn the_terminals_output_the_tabs_and_both_panels_are_in_the_frame() {
        let rows = [row(1, "zsh", "idle"), row(2, "claude", "working")];
        let snapshot = snapshot(&["hello from the shell"]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            space: Some(1),
            focused: 2,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
        });
        assert!(painted.contains("hello from the shell"));
        // Once on the tab and once in the agents panel: the bar says what is open in this
        // space and the panel says what it is doing.
        assert!(painted.matches("claude").count() >= 2, "{painted:?}");
        assert!(painted.contains("d detach"));
        assert!(painted.contains("spaces"), "no spaces panel: {painted:?}");
        assert!(
            painted.contains("app"),
            "the space is not named: {painted:?}"
        );
    }

    #[test]
    fn a_terminal_line_cannot_overwrite_the_rule_or_run_past_the_window() {
        let rows = [row(1, "zsh", "idle")];
        let long = "x".repeat(500);
        let snapshot = snapshot(&[&long]);
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            space: Some(1),
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
            spaces: &spaces(),
            space: Some(1),
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
            spaces: &spaces(),
            space: Some(1),
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
            spaces: &spaces(),
            space: Some(1),
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
            spaces: &spaces(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 30,
            lines: 2,
            message: None,
        });
        // `X` is at cell 2, so it is the green one, and the character before it is not.
        assert!(painted.contains("\x1b[32mX"), "{painted:?}");
    }

    /// Where the cursor goes is where the person is looking, and both conversions it needs
    /// were wrong in opposite directions.
    #[test]
    fn the_cursor_column_counts_columns_and_not_cells() {
        // A right trimmed row: the shell printed `"$ "` and the snapshot carries `"$"`, so
        // the cursor is in a cell the string does not have. It is one column past the `$`.
        assert_eq!(cursor_column("$", 2), 2);
        assert_eq!(cursor_column("", 0), 0);
        assert_eq!(cursor_column("", 7), 7);
        // Plain text, where cells and columns are the same number.
        assert_eq!(cursor_column("abc", 3), 3);
        assert_eq!(cursor_column("abc", 1), 1);
        // A wide character is two columns in one cell, and the grid's spacer after it is a
        // cell that is no columns at all.
        assert_eq!(cursor_column("\u{4e2d} X", 3), 3);
        assert_eq!(cursor_column("\u{4e2d} X", 1), 2);
        assert_eq!(cursor_column("\u{4e2d} \u{4e2d} ", 4), 4);
    }

    #[test]
    fn the_cursor_lands_on_the_column_the_shell_is_typing_at() {
        let mut snapshot = snapshot(&["$"]);
        snapshot.cursor = (0, 2);
        let rows = [row(1, "sh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 80,
            lines: 24,
            message: None,
        });
        // One row down for the tab bar, and the pane starts after the column and its rule.
        let want = format!("\x1b[{};{}H", 1 + TABS, 2 + sidebar::WIDTH + 2);
        assert!(
            painted.ends_with(&format!("{want}\x1b[?25h")),
            "{painted:?}"
        );
    }

    /// A window too narrow for the left column is a real one: two panes side by side in a
    /// tmux split. Painting the column anyway made every row wider than the terminal, so
    /// every row wrapped, the screen slid, and every position written after that addressed
    /// the wrong row.
    #[test]
    fn a_window_too_narrow_for_the_left_column_paints_no_wider_than_it_is() {
        let rows = [row(1, "sh", "idle")];
        let snapshot = snapshot(&["x".repeat(200).as_str()]);
        for cols in [1u16, 5, 10, 20, 23, 24, 25, 40] {
            let painted = paint(&Frame {
                rows: &rows,
                spaces: &spaces(),
                space: Some(1),
                focused: 1,
                snapshot: Some(&snapshot),
                cols,
                lines: 6,
                message: None,
            });
            // Every positioned run, measured as a terminal measures it.
            for run in painted.split("\x1b[").skip(1) {
                let Some(text) = run.strip_suffix("").and_then(|r| r.split_once('H')) else {
                    continue;
                };
                let drawn = text.1.split('\x1b').next().unwrap_or_default();
                assert!(
                    width::width(drawn) <= cols as usize,
                    "{cols} columns, painted {} of them: {drawn:?}",
                    width::width(drawn)
                );
            }
        }
    }

    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        // The pane is 19 columns here, and the string is Turkish, so a byte cut would put
        // a replacement character on the screen.
        let snapshot = snapshot(&["şğüöçİĞÜÖÇşğüöçİĞÜÖÇşğüöçİĞÜÖÇ"]);
        let rows = [row(1, "zsh", "idle")];
        let painted = paint(&Frame {
            rows: &rows,
            spaces: &spaces(),
            space: Some(1),
            focused: 1,
            snapshot: Some(&snapshot),
            cols: 40,
            lines: 3,
            message: None,
        });
        assert!(!painted.contains('\u{fffd}'));
    }
}

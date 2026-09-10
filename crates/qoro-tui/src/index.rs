//! The index column: every terminal, down the left of the screen.
//!
//! Pure text, deliberately. The caller paints attributes over these lines, so the focused
//! row carries a marker character as well: a screenshot, a test assertion, a pipe into a
//! file and a terminal with no colour all have to answer "which one has my keyboard".
//!
//! Widths here are counted in characters, not in rendered cells, so a full width glyph
//! (CJK, most emoji) occupies one column of the budget and two of the screen. The
//! alternative is a unicode width table, which is a dependency and a set of choices about
//! ambiguous width characters that this crate does not need to make yet. If double width
//! names start pushing the divider around, that table is the upgrade.

use qoro_runtime::control::AgentRow;

/// Columns the index occupies.
///
/// Fixed rather than negotiated. The terminal beside the index is a real PTY whose width
/// has to be decided before its child starts, and an index that grew or shrank with the
/// longest agent name would reflow that child every time a state changed.
pub const WIDTH: usize = 20;

/// Characters held back for the name when a long state string would otherwise eat the row.
/// A row showing only its state does not say whose state it is.
const MIN_NAME: usize = 3;

/// What a terminal is called when its foreground program cannot be read and nothing is
/// standing in the way. It is a shell far more often than it is anything else.
const NO_PROGRAM: &str = "shell";

/// Marks the row that has the keyboard. A character and not just an attribute, because
/// attributes do not survive being copied out of a terminal.
const FOCUS_MARKER: char = '>';

/// The lines to paint in the index column, top to bottom.
///
/// Always exactly `height` lines of exactly [`WIDTH`] characters, so the caller can paint
/// them blindly: a short line would leave the previous frame's pixels behind, and a long
/// one would spill into the terminal on the right.
///
/// `focused` need not appear in `rows`. The daemon's answer and the client's idea of the
/// focused terminal are two messages that can cross, and a client that panicked on the
/// overlap would die exactly when a terminal exited.
pub fn render(rows: &[AgentRow], focused: u32, height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    let focus_at = rows.iter().position(|r| r.terminal == focused);
    // Scroll only as far as it takes to bring the focused row back into view, and never
    // past the end, so the list stays still while the focus moves inside the window.
    let first = match focus_at {
        Some(i) => i
            .saturating_sub(height - 1)
            .min(rows.len().saturating_sub(height)),
        None => 0,
    };
    let blank = " ".repeat(WIDTH);
    (0..height)
        .map(|line| match rows.get(first + line) {
            Some(row) => render_row(row, row.terminal == focused),
            None => blank.clone(),
        })
        .collect()
}

/// One row: number, name, state, padded to the full width.
///
/// The state is pinned to the right edge and the name takes what is left, so the eye can
/// scan one column for states without every name having to be cut to the same length.
fn render_row(row: &AgentRow, focused: bool) -> String {
    let marker = if focused { FOCUS_MARKER } else { ' ' };
    let head = format!("{marker}{} ", row.terminal);
    let head_len = head.chars().count();
    if head_len + MIN_NAME >= WIDTH {
        // A terminal number wide enough to crowd out the name. Nothing useful left to lay
        // out, so show what fits rather than computing a negative budget.
        return fit(&head, WIDTH);
    }
    let rest = WIDTH - head_len;
    // An unrecognised state is rendered as itself, so a client from an older build still
    // says something true about a state it has never heard of. It is only ever shortened,
    // never replaced by "unknown".
    // An unknown state is drawn as nothing. Every row of a workspace with no agent in it
    // would otherwise read `unknown`, which is a column of noise that trains the eye to
    // skip the one column that matters when an agent really is blocked. The program name
    // already says what is in the terminal.
    let shown = if row.state == "unknown" {
        ""
    } else {
        &row.state
    };
    let state = fit_end(shown, rest - MIN_NAME - 1);
    let name = fit(&label(row), rest - state.chars().count() - 1);
    format!("{head}{name} {state}")
}

/// What to call the terminal.
///
/// A wrapper is announced as a wrapper. Printing its own name in the program's place would
/// tell a person their agent is called `docker`, and they would believe it.
fn label(row: &AgentRow) -> String {
    match (&row.program, &row.behind) {
        // The last path segment, because the column is twenty characters wide and
        // `/opt/homebrew/bin/claude` truncated to fit reads as `/opt/homebre`, which
        // identifies nothing. The daemon sends the whole path on purpose, so a client that
        // has room can show it.
        (Some(program), _) => program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(program)
            .to_string(),
        (None, Some(wrapper)) => format!("behind {wrapper}"),
        (None, None) => NO_PROGRAM.to_string(),
    }
}

/// Cut or pad to exactly `width` characters, counting characters so that a cut never lands
/// inside a multi byte character and produces a replacement glyph or a panic.
fn fit(text: &str, width: usize) -> String {
    // Columns, not characters. A program with a wide character in its name would otherwise
    // draw past the index and over the rule, which is the same arithmetic error the pane
    // had: see `crate::width`.
    crate::width::fit(text, width)
}

/// Cut to at most `width` characters without padding, for the piece that is already flush
/// against the right edge.
fn fit_end(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = crate::width::columns(c);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

#[cfg(test)]
mod tests {
    // A full path is shown by its last segment: see `label`.

    use super::*;

    fn row(terminal: u32, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: None,
        }
    }

    fn bare(terminal: u32) -> AgentRow {
        AgentRow {
            terminal,
            program: None,
            behind: None,
            state: "idle".to_string(),
            generation: None,
        }
    }

    #[test]
    fn every_line_is_exactly_the_index_width() {
        let rows = [row(1, "claude", "working"), row(2, "codex", "blocked")];
        for line in render(&rows, 1, 6) {
            assert_eq!(crate::width::width(&line), WIDTH, "{line:?}");
        }
    }

    #[test]
    fn a_short_list_is_padded_to_the_full_height_with_blank_lines() {
        let lines = render(&[row(1, "claude", "idle")], 1, 4);
        assert_eq!(lines.len(), 4);
        assert!(lines[1..].iter().all(|l| *l == " ".repeat(WIDTH)));
    }

    #[test]
    fn a_row_shows_the_number_then_the_name_then_the_state() {
        let lines = render(&[row(7, "claude", "working")], 0, 1);
        assert_eq!(lines[0], " 7 claude    working");
    }

    #[test]
    fn the_focused_row_is_marked_in_plain_text() {
        let rows = [row(1, "claude", "idle"), row(2, "codex", "idle")];
        let lines = render(&rows, 2, 2);
        assert!(lines[0].starts_with(' '), "{:?}", lines[0]);
        assert!(lines[1].starts_with(FOCUS_MARKER), "{:?}", lines[1]);
    }

    #[test]
    fn a_long_list_scrolls_so_the_focused_row_stays_visible() {
        let rows: Vec<AgentRow> = (1..=10).map(|n| row(n, "claude", "idle")).collect();
        for focused in 1..=10 {
            let lines = render(&rows, focused, 3);
            assert_eq!(lines.len(), 3);
            let marked = lines.iter().filter(|l| l.starts_with(FOCUS_MARKER)).count();
            assert_eq!(marked, 1, "terminal {focused} fell out of the window");
        }
    }

    #[test]
    fn a_name_too_long_for_the_column_is_truncated_on_a_character_boundary() {
        // Turkish, so every cut point is inside a multi byte character if bytes are sliced.
        let lines = render(&[row(1, "çalışkanöğütücü", "idle")], 1, 1);
        assert_eq!(lines[0].chars().count(), WIDTH);
        assert!(lines[0].starts_with(">1 çalışkanöğ"), "{:?}", lines[0]);
        assert!(lines[0].ends_with(" idle"));
    }

    #[test]
    fn an_emoji_name_is_truncated_between_glyphs() {
        let lines = render(&[row(1, &"🌍".repeat(20), "idle")], 1, 1);
        // Columns and not characters: an emoji is one character and two columns, and a row
        // measured in characters is twice as wide as the column it was drawn into.
        assert_eq!(crate::width::width(&lines[0]), WIDTH);
        assert!(!lines[0].contains('\u{fffd}'));
        assert!(lines[0].contains('🌍'));
    }

    #[test]
    fn a_wrapper_is_shown_as_standing_in_the_way_and_never_as_the_program() {
        let agent = AgentRow {
            terminal: 3,
            program: None,
            behind: Some("docker".to_string()),
            state: "idle".to_string(),
            generation: None,
        };
        let lines = render(&[agent], 3, 1);
        assert_eq!(lines[0], ">3 behind docke idle");
        assert!(!lines[0].starts_with(">3 docker"));
    }

    #[test]
    fn a_terminal_with_nothing_readable_is_called_a_shell() {
        let lines = render(&[bare(4)], 4, 1);
        assert!(lines[0].contains(NO_PROGRAM), "{:?}", lines[0]);
    }

    #[test]
    fn an_unrecognised_state_renders_as_itself_rather_than_as_unknown() {
        let lines = render(&[row(1, "claude", "rebasing")], 1, 1);
        assert!(lines[0].ends_with("rebasing"), "{:?}", lines[0]);
        assert!(!lines[0].contains("unknown"));
    }

    #[test]
    fn an_absurdly_long_state_is_cut_but_still_leaves_some_of_the_name() {
        let lines = render(&[row(1, "claude", &"z".repeat(80))], 1, 1);
        assert_eq!(lines[0].chars().count(), WIDTH);
        assert!(lines[0].starts_with(">1 cla"), "{:?}", lines[0]);
    }

    #[test]
    fn a_height_of_zero_renders_nothing() {
        assert!(render(&[row(1, "claude", "idle")], 1, 0).is_empty());
    }

    #[test]
    fn a_focused_terminal_that_is_not_listed_does_not_panic() {
        let lines = render(&[row(1, "claude", "idle")], 99, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| !l.starts_with(FOCUS_MARKER)));
        assert!(render(&[], 99, 2).iter().all(|l| l.trim().is_empty()));
    }

    #[test]
    fn a_terminal_number_too_wide_for_the_column_still_yields_one_full_line() {
        let lines = render(&[row(u32::MAX, "claude", "idle")], u32::MAX, 1);
        assert_eq!(lines[0].chars().count(), WIDTH);
    }
}

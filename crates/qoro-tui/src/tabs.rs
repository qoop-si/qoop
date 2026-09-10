//! The tab bar: the terminals of the focused space, along the top of the pane.
//!
//! Every terminal of the space is here, agent or not. The agents panel in
//! [`crate::sidebar`] answers "who needs me" and is allowed to leave a plain shell out; this
//! bar answers "what did I open", and a shell a person started and then could not reach
//! again is a terminal they have lost.
//!
//! Pure text, as everywhere else in this crate. The focused tab is bracketed rather than
//! merely highlighted, so it survives a screenshot, a test assertion and a terminal with no
//! colour. The brackets sit where the tab's own padding would be, so the bar does not shift
//! sideways every time the focus moves.

use crate::sidebar::Line;
use qoro_runtime::control::AgentRow;

/// The tab that makes a new terminal.
///
/// Always drawn, even when the tabs themselves had to be cut short, because it is how a
/// person makes a terminal without knowing a key binding. A bar that hid it once there were
/// too many tabs would hide it exactly when somebody is working fast enough to need it.
const NEW: &str = " + ";

/// The most columns one tab's label may take.
///
/// A program installed under a long path is already shortened to its last segment, but a
/// long segment is still possible, and one of them must not be allowed to push every other
/// terminal off the bar.
const MAX_LABEL: usize = 12;

/// The tab bar, painted into exactly `cols` columns.
///
/// One line, so one colour, and therefore no colour: a bar carries several terminals in
/// several states and a single hint for all of them would be a hint about none of them. The
/// caller paints the focused tab from the brackets it can see in the text.
pub fn render(terminals: &[AgentRow], focused: u32, cols: usize) -> Line {
    let bar: String = segments(terminals, focused, cols)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    Line {
        text: exactly(&bar, cols),
        colour: None,
    }
}

/// Which terminal the tab bar drew at one column, for a click.
///
/// `None` on the `+` and on the padding past the last tab, so a click on empty bar does
/// nothing rather than reaching the nearest terminal.
pub fn hit(terminals: &[AgentRow], focused: u32, cols: usize, col: usize) -> Option<u32> {
    let mut used = 0;
    for (text, terminal) in segments(terminals, focused, cols) {
        used += crate::width::width(&text);
        if col < used {
            return terminal;
        }
    }
    None
}

/// The pieces of the bar, left to right, each with the terminal a click on it means.
///
/// One function and not two, because [`render`] and [`hit`] have to agree about where every
/// tab starts, and the same running total written out twice is how a click opens the tab
/// next to the one a person aimed at.
///
/// Tabs are cut from the right, and the bar only scrolls when the focused tab would be one
/// of the ones cut. The ones a person opened first keep their place until then, so the bar
/// does not reshuffle under the pointer as terminals come and go.
///
/// Scrolling is not decoration. Measured with twelve terminals in an eighty column window:
/// with the focus on the twelfth, the bar was byte for byte the bar of a client with nothing
/// focused. No brackets anywhere, no way to click back to it, and nothing on screen saying
/// which terminal the keyboard was in.
fn segments(terminals: &[AgentRow], focused: u32, cols: usize) -> Vec<(String, Option<u32>)> {
    let budget = cols.saturating_sub(crate::width::width(NEW));
    let mut used = 0;
    let mut out = Vec::with_capacity(terminals.len() + 1);
    let first = scrolled(terminals, focused, budget);
    for (at, row) in terminals.iter().enumerate().skip(first) {
        let text = tab(row, at + 1, row.terminal == focused);
        let wide = crate::width::width(&text);
        if used + wide > budget {
            break;
        }
        used += wide;
        out.push((text, Some(row.terminal)));
    }
    out.push((NEW.to_string(), None));
    out
}

/// The first tab the bar draws.
///
/// Zero until the focused tab does not fit, then the least it takes to bring it back. Least,
/// so the bar moves by one tab when the focus moves by one tab rather than jumping the
/// focused tab to the left edge, which is a whole bar of new labels for one keystroke.
fn scrolled(terminals: &[AgentRow], focused: u32, budget: usize) -> usize {
    let Some(at) = terminals.iter().position(|r| r.terminal == focused) else {
        return 0;
    };
    let mut first = 0;
    while first < at {
        let fits = terminals[first..=at]
            .iter()
            .enumerate()
            .map(|(i, row)| crate::width::width(&tab(row, first + i + 1, row.terminal == focused)))
            .sum::<usize>()
            <= budget;
        if fits {
            break;
        }
        first += 1;
    }
    first
}

/// One tab: its label, with a space on each side, or brackets when it has the keyboard.
///
/// The brackets take the place of the spaces rather than adding to them, so every tab keeps
/// the same width whether or not it is focused.
fn tab(row: &AgentRow, position: usize, focused: bool) -> String {
    let (open, close) = if focused { ('[', ']') } else { (' ', ' ') };
    format!("{open}{}{close}", label(row, position))
}

/// What a tab is called: its place on this bar, and the program in it when there is one.
///
/// A number is always there because the program is not distinguishing. A space with three
/// shells in it draws three tabs reading `sh`, and a person clicking between them cannot
/// tell which one they are on or which one they were on.
///
/// Its place, and not the terminal's own number, because the number on a tab is the number
/// a person presses. Terminal numbers are handed out across the whole daemon, so a second
/// space opens with terminals 5 and 6 in it: a bar reading `5 claude  6 codex` invites
/// `ctrl-a 5`, which is a tab that does not exist here, while the tab that key does reach
/// is labelled with a number nobody typed. Places also stay within one digit, which the
/// terminal numbers stop doing after the ninth terminal of the day.
fn label(row: &AgentRow, position: usize) -> String {
    match &row.program {
        Some(program) => {
            let name = program
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(program);
            clip(&format!("{position} {name}"))
        }
        None => position.to_string(),
    }
}

/// Cut a label to at most [`MAX_LABEL`] columns.
fn clip(text: &str) -> String {
    crate::width::fit(text, MAX_LABEL.min(crate::width::width(text)))
}

/// Cut or pad to exactly `cols` columns.
///
/// Not [`crate::width::fit`], which also drops the space a grid puts after a full width
/// character to hold its second column. This bar is text this module composed, so a space
/// in it is a space somebody meant, and dropping one would move every tab after it one
/// column left of where [`hit`] says it is.
fn exactly(text: &str, cols: usize) -> String {
    let mut out = String::with_capacity(cols);
    let mut used = 0;
    for c in text.chars() {
        let wide = crate::width::columns(c);
        if used + wide > cols {
            break;
        }
        out.push(c);
        used += wide;
    }
    for _ in used..cols {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(terminal: u32, program: &str) -> AgentRow {
        AgentRow {
            terminal,
            workspace: 1,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: "idle".to_string(),
            generation: None,
        }
    }

    fn shell(terminal: u32) -> AgentRow {
        AgentRow {
            terminal,
            workspace: 1,
            // The bar carries a plain shell too: it answers "what did I open", not "who
            // needs me".
            agent: false,
            program: None,
            behind: None,
            state: "unknown".to_string(),
            generation: None,
        }
    }

    fn sample() -> Vec<AgentRow> {
        vec![
            shell(1),
            agent(2, "tue"),
            agent(3, "private"),
            agent(4, "pet"),
        ]
    }

    #[test]
    fn the_bar_is_exactly_the_width_it_was_given() {
        for cols in [0usize, 1, 2, 3, 4, 9, 20, 40, 120] {
            let line = render(&sample(), 2, cols);
            assert_eq!(crate::width::width(&line.text), cols, "{:?}", line.text);
        }
    }

    #[test]
    fn a_wide_label_is_measured_in_columns_and_not_in_characters() {
        let rows = vec![agent(1, &"\u{4e2d}".repeat(8)), agent(2, "x")];
        for cols in [0usize, 5, 12, 30] {
            let line = render(&rows, 1, cols);
            assert_eq!(crate::width::width(&line.text), cols, "{:?}", line.text);
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn a_tab_is_named_after_its_program_and_a_bare_terminal_after_its_number() {
        let line = render(&sample(), 9, 40);
        assert_eq!(line.text.trim_end(), " 1  2 tue  3 private  4 pet  +");
    }

    #[test]
    fn the_focused_tab_is_marked_in_plain_text() {
        let line = render(&sample(), 3, 40);
        assert!(line.text.contains("[3 private]"), "{:?}", line.text);
        assert!(!line.text.contains("[2 tue]"));
    }

    #[test]
    fn focusing_a_tab_does_not_move_the_ones_after_it() {
        let unfocused = render(&sample(), 9, 40);
        for focused in 1..=4 {
            let line = render(&sample(), focused, 40);
            assert_eq!(line.text.len(), unfocused.text.len(), "{:?}", line.text);
        }
    }

    #[test]
    fn the_new_tab_button_survives_a_bar_with_more_tabs_than_columns() {
        let many: Vec<AgentRow> = (1..=30).map(|n| agent(n, "claude")).collect();
        for cols in [3usize, 4, 10, 25, 60] {
            let line = render(&many, 1, cols);
            assert!(line.text.contains('+'), "{cols}: {:?}", line.text);
            assert_eq!(crate::width::width(&line.text), cols);
        }
    }

    #[test]
    fn tabs_past_the_right_edge_are_dropped_and_the_ones_before_them_are_not() {
        let line = render(&sample(), 1, 16);
        // Room for `[1]`, ` 2 tue ` and ` + `, and not for `private`.
        assert_eq!(line.text, "[1] 2 tue  +    ");
        assert!(!line.text.contains("private"));
    }

    #[test]
    fn a_long_program_name_cannot_push_every_other_tab_off_the_bar() {
        let rows = vec![agent(1, &"z".repeat(40)), agent(2, "codex")];
        let line = render(&rows, 1, 40);
        assert!(line.text.contains("codex"), "{:?}", line.text);
        assert!(!line.text.contains(&"z".repeat(MAX_LABEL + 1)));
    }

    #[test]
    fn hit_returns_the_terminal_the_bar_drew_at_every_column() {
        let rows = sample();
        for cols in [0usize, 3, 16, 40] {
            let text = render(&rows, 3, cols).text;
            for col in 0..cols {
                let got = hit(&rows, 3, cols, col);
                let want = rows.iter().enumerate().find(|(at, r)| {
                    // Where the label of this terminal was drawn, found in the painted bar
                    // rather than recomputed, so the test is not a copy of the code.
                    let name = if r.terminal == 3 {
                        format!("[{}]", label(r, at + 1))
                    } else {
                        format!(" {} ", label(r, at + 1))
                    };
                    match text.find(&name) {
                        Some(byte) => {
                            let start = crate::width::width(&text[..byte]);
                            col >= start && col < start + crate::width::width(&name)
                        }
                        None => false,
                    }
                });
                assert_eq!(got, want.map(|(_, r)| r.terminal), "column {col} of {cols}");
            }
            assert_eq!(hit(&rows, 3, cols, cols), None, "past the right edge");
        }
    }

    /// The bar has to answer "which terminal has the keyboard" at every focus, or it is a
    /// bar that answers it only for the terminals somebody happened to open first.
    #[test]
    fn the_focused_tab_is_on_the_bar_however_many_tabs_there_are() {
        let many: Vec<AgentRow> = (1..=12).map(|n| agent(n, "claude")).collect();
        for cols in [20usize, 40, 80, 200] {
            for focused in 1..=12u32 {
                let line = render(&many, focused, cols);
                let clickable: Vec<u32> = (0..cols)
                    .filter_map(|col| hit(&many, focused, cols, col))
                    .collect();
                assert!(
                    line.text.contains('['),
                    "{cols} columns, focus {focused}: nothing is marked: {:?}",
                    line.text
                );
                assert!(
                    clickable.contains(&focused),
                    "{cols} columns, focus {focused}: the focused tab cannot be clicked: {:?}",
                    line.text
                );
            }
        }
    }

    /// One keystroke must not repaint the whole bar with labels a person has not seen.
    #[test]
    fn the_bar_scrolls_by_one_tab_and_not_by_a_barful() {
        let many: Vec<AgentRow> = (1..=12).map(|n| agent(n, "claude")).collect();
        let at_seven = render(&many, 7, 40).text;
        let at_eight = render(&many, 8, 40).text;
        // Terminal 7 is still on the bar after moving to 8, so the person's eye keeps its
        // place. A bar that jumped the focused tab to the left edge would have dropped it.
        assert!(at_seven.contains("7 claude"), "{at_seven:?}");
        assert!(at_eight.contains("7 claude"), "{at_eight:?}");
    }

    #[test]
    fn a_click_on_the_new_tab_button_or_on_the_padding_is_not_a_terminal() {
        let rows = sample();
        let text = render(&rows, 1, 40).text;
        let plus = text.find('+').unwrap_or_else(|| panic!("{text:?}"));
        assert_eq!(hit(&rows, 1, 40, plus), None);
        assert_eq!(hit(&rows, 1, 40, 39), None);
    }

    #[test]
    fn an_empty_space_still_offers_a_way_to_make_a_terminal() {
        let line = render(&[], 1, 10);
        assert_eq!(line.text, " +        ");
        assert_eq!(hit(&[], 1, 10, 1), None);
    }
}

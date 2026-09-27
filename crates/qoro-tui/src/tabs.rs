//! The tab bar: the terminals of the focused space, along the top of the pane.
//!
//! Every terminal of the space is here, agent or not. The agents panel in
//! [`crate::sidebar`] answers "who needs me" and is allowed to leave a plain shell out; this
//! bar answers "what did I open", and a shell a person started and then could not reach
//! again is a terminal they have lost.
//!
//! Pure text, as everywhere else in this crate. The focused tab is colour,
//! not brackets: the pill background says which tab has the keyboard, and
//! every tab keeps the same text with or without it, so the bar never shifts
//! sideways when the focus moves.

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
/// One line and one space, so one space colour: the bar carries the terminals of
/// the focused space, and every unfocused tab wears that space's ink while the
/// focused tab keeps the pink pill. The caller paints each piece from the ink
/// [`painted`] marks it with.
/// How one piece of the bar is painted.
///
/// Three, not a colour per state. A tab bar carries several terminals in several states
/// and colouring each one turns the bar into a row of confetti: what a person needs from
/// it is which tab has the keyboard, and the panel on the left is where states live.
/// The space colour is identity, not state, so it is the one colour the bar spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// The tab with the keyboard.
    Focused,
    /// Every other tab, in the focused space's own colour.
    Other,
    /// The new tab button and the padding past the last tab.
    Quiet,
}

/// The bar as pieces to paint, left to right, each with how it is painted.
///
/// The painter needs the pieces and not one string, because the focused tab is a pill: a
/// background that starts and stops, which one string with one colour cannot express. The
/// same segments [`render`] and [`hit`] use, so all three agree by construction.
pub fn painted(
    terminals: &[AgentRow],
    focused: u32,
    cols: usize,
    unseen: &crate::sidebar::Unseen,
) -> Vec<(String, Ink)> {
    let mut used = 0;
    let mut out: Vec<(String, Ink)> = segments(terminals, focused, cols, unseen)
        .into_iter()
        .map(|(text, hit)| {
            used += crate::width::width(&text);
            let ink = match hit {
                Some(Hit::Terminal(terminal)) if terminal == focused => Ink::Focused,
                Some(Hit::Terminal(_)) => Ink::Other,
                Some(Hit::New) | None => Ink::Quiet,
            };
            (text, ink)
        })
        .collect();
    if used < cols {
        out.push((" ".repeat(cols - used), Ink::Quiet));
    }
    out
}

pub fn render(
    terminals: &[AgentRow],
    focused: u32,
    cols: usize,
    unseen: &crate::sidebar::Unseen,
) -> Line {
    let bar: String = segments(terminals, focused, cols, unseen)
        .into_iter()
        .map(|(text, _)| text)
        .collect();
    Line {
        text: exactly(&bar, cols),
        colour: None,
        accent: None,
        indicator: None,
        runs: Vec::new(),
    }
}

/// What a click on the tab bar means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The tab drawn at that column.
    Terminal(u32),
    /// The `+` that makes a new terminal.
    New,
}

/// Which terminal the tab bar drew at one column, for a click.
///
/// `None` on the padding past the last tab, so a click on empty bar does
/// nothing rather than reaching the nearest terminal.
pub fn hit(
    terminals: &[AgentRow],
    focused: u32,
    cols: usize,
    col: usize,
    unseen: &crate::sidebar::Unseen,
) -> Option<Hit> {
    if col >= cols {
        return None;
    }
    let mut used = 0;
    for (text, terminal) in segments(terminals, focused, cols, unseen) {
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
fn segments(
    terminals: &[AgentRow],
    focused: u32,
    cols: usize,
    unseen: &crate::sidebar::Unseen,
) -> Vec<(String, Option<Hit>)> {
    let budget = cols.saturating_sub(crate::width::width(NEW));
    let mut used = 0;
    let mut out = Vec::with_capacity(terminals.len() + 1);
    let first = scrolled(terminals, focused, budget, unseen);
    for (at, row) in terminals.iter().enumerate().skip(first) {
        let text = tab(row, at + 1, focused, unseen);
        let wide = crate::width::width(&text);
        if used + wide > budget {
            break;
        }
        used += wide;
        out.push((text, Some(Hit::Terminal(row.terminal))));
    }
    out.push((NEW.to_string(), Some(Hit::New)));
    out
}

/// The first tab the bar draws.
///
/// Zero until the focused tab does not fit, then the least it takes to bring it back. Least,
/// so the bar moves by one tab when the focus moves by one tab rather than jumping the
/// focused tab to the left edge, which is a whole bar of new labels for one keystroke.
fn scrolled(
    terminals: &[AgentRow],
    focused: u32,
    budget: usize,
    unseen: &crate::sidebar::Unseen,
) -> usize {
    let Some(at) = terminals.iter().position(|r| r.terminal == focused) else {
        return 0;
    };
    let mut first = 0;
    while first < at {
        let fits = terminals[first..=at]
            .iter()
            .enumerate()
            .map(|(i, row)| crate::width::width(&tab(row, first + i + 1, focused, unseen)))
            .sum::<usize>()
            <= budget;
        if fits {
            break;
        }
        first += 1;
    }
    first
}

/// One tab: its label, with a space on each side.
///
/// The focused tab reads the same text as an unfocused one: [`painted`] marks
/// it with [`Ink::Focused`] instead, so the bar never shifts sideways when
/// the focus moves.
fn tab(row: &AgentRow, position: usize, focus: u32, unseen: &crate::sidebar::Unseen) -> String {
    format!(" {} ", label(row, position, focus, unseen))
}

/// The state dot a tab carries, so the bar answers "what is it doing" without
/// leaving the top line. Same glyphs as the agents panel, minus idle: every
/// calm tab ending in `o` is a row of marks that mean nothing is happening,
/// competing with the ones that mean something. Silence is drawn as silence.
///
/// A slept tab keeps its `z`: sleeping is a persistent calm state, not news
/// and never a bell, but unlike idle it is a change worth one quiet mark.
/// `z` collides with no other tab mark.
fn dot(state: &str) -> char {
    match state {
        "working" | "starting" => '*',
        "blocked" => '!',
        "failed" => 'x',
        "done" => '+',
        "sleeping" => 'z',
        _ => '.',
    }
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
fn label(row: &AgentRow, position: usize, focus: u32, unseen: &crate::sidebar::Unseen) -> String {
    // The dot only when there is a state to show. A terminal nothing has said
    // anything about carried a `.`, so a fresh window drew a row of tabs each
    // ending in a full stop: four marks that mean "no information", competing
    // with the three that mean something. Silence is drawn as silence.
    // A finished-while-away idle keeps the done glyph: same qoro glyph, no
    // new colour on a bar that carries none.
    let mark = if crate::sidebar::tab_is_new(row, focus, unseen) {
        " +".to_string()
    } else {
        match dot(&row.state) {
            UNOBSERVED => String::new(),
            glyph => format!(" {glyph}"),
        }
    };
    // A name a person chose outlives the program: a tab called `release` stays `release`
    // while it runs a shell, then cargo, then an agent, and a bar of chosen names is a bar
    // somebody can read without looking at what happens to be running in each one.
    if let Some(name) = &row.name {
        return clip(&format!("{position} {name}{mark}"));
    }
    match &row.program {
        Some(program) => {
            let name = program
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(program);
            clip(&format!("{position} {name}{mark}"))
        }
        None => clip(&format!("{position}{mark}")),
    }
}

/// What [`dot`] answers for a terminal nothing has observed.
const UNOBSERVED: char = '.';

/// Cut a label to at most [`MAX_LABEL`] columns.
fn clip(text: &str) -> String {
    crate::width::fit(text, MAX_LABEL.min(crate::width::width(text)))
}

/// Cut or pad to exactly `cols` columns, keeping every space: this bar is text this
/// module composed, and dropping one would move every tab after it off where [`hit`] says.
fn exactly(text: &str, cols: usize) -> String {
    crate::width::text(text, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(terminal: u32, program: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace: 1,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: "idle".to_string(),
            generation: None,
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
        }
    }

    fn shell(terminal: u32) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace: 1,
            // The bar carries a plain shell too: it answers "what did I open", not "who
            // needs me".
            agent: false,
            program: None,
            behind: None,
            state: "unknown".to_string(),
            generation: None,
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
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
            let line = render(&sample(), 2, cols, &crate::sidebar::Unseen::new());
            assert_eq!(crate::width::width(&line.text), cols, "{:?}", line.text);
        }
    }

    #[test]
    fn a_wide_label_is_measured_in_columns_and_not_in_characters() {
        let rows = vec![agent(1, &"\u{4e2d}".repeat(8)), agent(2, "x")];
        for cols in [0usize, 5, 12, 30] {
            let line = render(&rows, 1, cols, &crate::sidebar::Unseen::new());
            assert_eq!(crate::width::width(&line.text), cols, "{:?}", line.text);
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn a_tab_is_named_after_its_program_and_a_bare_terminal_after_its_number() {
        let line = render(&sample(), 9, 40, &crate::sidebar::Unseen::new());
        assert_eq!(line.text.trim_end(), " 1  2 tue  3 private  4 pet  +");
    }

    /// A name somebody chose beats the name of whatever happens to be running, because the
    /// program changes under it and the name is how they find the tab again.
    #[test]
    fn a_named_tab_is_called_what_it_was_named() {
        let mut rows = sample();
        rows[1].name = Some("release".to_string());
        let line = render(&rows, 9, 60, &crate::sidebar::Unseen::new());
        assert!(line.text.contains("2 release"), "{:?}", line.text);
        assert!(!line.text.contains("2 tue"), "{:?}", line.text);
        // And the state still rides along, since the name says nothing about that.
        rows[1].state = "blocked".to_string();
        let line = render(&rows, 9, 60, &crate::sidebar::Unseen::new());
        assert!(line.text.contains("2 release !"), "{:?}", line.text);
    }

    #[test]
    fn the_focused_tab_is_marked_by_ink_and_not_by_text() {
        // The text reads the same focused or not: the pill background carries
        // it, so the bar never shifts when the focus moves.
        let plain = render(&sample(), 9, 40, &crate::sidebar::Unseen::new());
        let focused = render(&sample(), 3, 40, &crate::sidebar::Unseen::new());
        assert_eq!(plain.text, focused.text);
        assert!(!focused.text.contains('['), "{:?}", focused.text);
        let pieces = painted(&sample(), 3, 40, &crate::sidebar::Unseen::new());
        assert!(
            pieces
                .iter()
                .any(|(text, ink)| text.contains("3 private") && *ink == Ink::Focused),
            "{pieces:?}"
        );
        assert!(
            pieces
                .iter()
                .filter(|(_, ink)| *ink == Ink::Focused)
                .count()
                == 1,
            "exactly one tab owns the keyboard: {pieces:?}"
        );
    }

    #[test]
    fn focusing_a_tab_does_not_move_the_ones_after_it() {
        let unfocused = render(&sample(), 9, 40, &crate::sidebar::Unseen::new());
        for focused in 1..=4 {
            let line = render(&sample(), focused, 40, &crate::sidebar::Unseen::new());
            assert_eq!(line.text, unfocused.text, "{:?}", line.text);
        }
    }

    #[test]
    fn the_new_tab_button_survives_a_bar_with_more_tabs_than_columns() {
        let many: Vec<AgentRow> = (1..=30).map(|n| agent(n, "claude")).collect();
        for cols in [3usize, 4, 10, 25, 60] {
            let line = render(&many, 1, cols, &crate::sidebar::Unseen::new());
            assert!(line.text.contains('+'), "{cols}: {:?}", line.text);
            assert_eq!(crate::width::width(&line.text), cols);
        }
    }

    #[test]
    fn tabs_past_the_right_edge_are_dropped_and_the_ones_before_them_are_not() {
        let line = render(&sample(), 1, 12, &crate::sidebar::Unseen::new());
        // Room for ` 1 `, ` + `, and nothing else.
        assert_eq!(line.text, " 1  +       ");
        assert!(!line.text.contains("tue"));
    }

    #[test]
    fn a_long_program_name_cannot_push_every_other_tab_off_the_bar() {
        let rows = vec![agent(1, &"z".repeat(40)), agent(2, "codex")];
        let line = render(&rows, 1, 40, &crate::sidebar::Unseen::new());
        assert!(line.text.contains("codex"), "{:?}", line.text);
        assert!(!line.text.contains(&"z".repeat(MAX_LABEL + 1)));
    }

    #[test]
    fn hit_returns_the_terminal_the_bar_drew_at_every_column() {
        let rows = sample();
        for cols in [0usize, 3, 16, 40] {
            let text = render(&rows, 3, cols, &crate::sidebar::Unseen::new()).text;
            for col in 0..cols {
                let got = hit(&rows, 3, cols, col, &crate::sidebar::Unseen::new());
                let want = rows.iter().enumerate().find(|(at, r)| {
                    // Where the label of this terminal was drawn, found in the painted bar
                    // rather than recomputed, so the test is not a copy of the code.
                    let name = format!(" {} ", label(r, at + 1, 3, &crate::sidebar::Unseen::new()));
                    match text.find(&name) {
                        Some(byte) => {
                            let start = crate::width::width(&text[..byte]);
                            col >= start && col < start + crate::width::width(&name)
                        }
                        None => false,
                    }
                });
                match want.map(|(_, r)| r.terminal) {
                    Some(terminal) => {
                        assert_eq!(got, Some(Hit::Terminal(terminal)), "column {col} of {cols}")
                    }
                    // Off a tab is the new button or the padding past it, never a
                    // terminal next to the one aimed at.
                    None => assert!(
                        got.is_none_or(|h| h == Hit::New),
                        "column {col} of {cols} reached a terminal it did not aim at: {got:?}"
                    ),
                }
            }
            assert_eq!(
                hit(&rows, 3, cols, cols, &crate::sidebar::Unseen::new()),
                None,
                "past the right edge"
            );
        }
    }

    /// The bar has to answer "which terminal has the keyboard" at every focus, or it is a
    /// bar that answers it only for the terminals somebody happened to open first.
    #[test]
    fn the_focused_tab_is_on_the_bar_however_many_tabs_there_are() {
        let many: Vec<AgentRow> = (1..=12).map(|n| agent(n, "claude")).collect();
        for cols in [20usize, 40, 80, 200] {
            for focused in 1..=12u32 {
                let line = render(&many, focused, cols, &crate::sidebar::Unseen::new());
                let pieces = painted(&many, focused, cols, &crate::sidebar::Unseen::new());
                let clickable: Vec<u32> = (0..cols)
                    .filter_map(|col| {
                        match hit(&many, focused, cols, col, &crate::sidebar::Unseen::new()) {
                            Some(Hit::Terminal(terminal)) => Some(terminal),
                            _ => None,
                        }
                    })
                    .collect();
                assert!(
                    pieces.iter().any(|(_, ink)| *ink == Ink::Focused),
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
        let at_seven = render(&many, 7, 40, &crate::sidebar::Unseen::new()).text;
        let at_eight = render(&many, 8, 40, &crate::sidebar::Unseen::new()).text;
        // Terminal 7 is still on the bar after moving to 8, so the person's eye keeps its
        // place. A bar that jumped the focused tab to the left edge would have dropped it.
        assert!(at_seven.contains("7 claude"), "{at_seven:?}");
        assert!(at_eight.contains("7 claude"), "{at_eight:?}");
    }

    #[test]
    fn a_click_on_the_new_tab_button_makes_a_terminal_and_on_padding_does_nothing() {
        let rows = sample();
        let text = render(&rows, 1, 40, &crate::sidebar::Unseen::new()).text;
        let plus = text.find('+').unwrap_or_else(|| panic!("{text:?}"));
        let plus_col = crate::width::width(&text[..plus]) + 1;
        assert_eq!(
            hit(&rows, 1, 40, plus_col, &crate::sidebar::Unseen::new()),
            Some(Hit::New)
        );
        assert_eq!(hit(&rows, 1, 40, 39, &crate::sidebar::Unseen::new()), None);
    }

    #[test]
    fn an_empty_space_still_offers_a_way_to_make_a_terminal() {
        let line = render(&[], 1, 10, &crate::sidebar::Unseen::new());
        assert_eq!(line.text, " +        ");
        assert_eq!(
            hit(&[], 1, 10, 1, &crate::sidebar::Unseen::new()),
            Some(Hit::New)
        );
    }

    /// A slept tab carries a `z`: its own mark, colliding with nothing, and
    /// an unknown state still draws silence for old rows.
    #[test]
    fn a_slept_tab_carries_its_own_mark() {
        assert_eq!(dot("sleeping"), 'z');
        for (state, mark) in [
            ("working", '*'),
            ("starting", '*'),
            ("blocked", '!'),
            ("failed", 'x'),
            ("done", '+'),
        ] {
            assert_eq!(dot(state), mark);
            assert_ne!(mark, 'z', "{state} collides with sleeping");
        }
        assert_eq!(dot("idle"), UNOBSERVED);
        assert_eq!(dot("whatever-comes-next"), UNOBSERVED);
        let mut rows = sample();
        rows[1].state = "sleeping".to_string();
        let line = render(&rows, 9, 60, &crate::sidebar::Unseen::new());
        assert!(line.text.contains("2 tue z"), "{:?}", line.text);
    }
}

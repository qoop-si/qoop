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

/// Rows the bar takes: the tabs' names, and under each the folder it runs in.
pub const ROWS: usize = 2;

/// The close on the focused tab, in its top right corner. Every tab keeps the column,
/// blank when it is not focused, so the bar never shifts sideways when the focus moves.
const CLOSE: &str = "\u{00d7}";

/// The curve down each tab's left edge that binds its name to its folder: the top half
/// beside the name, the bottom half beside the folder.
const CURVE: [&str; ROWS] = ["\u{256d}", "\u{2570}"];

/// The columns between one tab and the next, so a folder never reads as its neighbour's.
const GAP: &str = "   ";

/// How one piece of the bar is painted.
///
/// Not a colour per state. A tab bar carries several terminals in several states and
/// colouring each one turns the bar into a row of confetti: what a person needs from it is
/// which tab has the keyboard, and the panel on the left is where states live. The space
/// colour is identity, not state, so it is the one colour the bar spends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    /// The name of the tab with the keyboard.
    Focused,
    /// The curve beside it and its close, in the brand's ember.
    FocusedMark,
    /// Every other tab's name, in the focused space's own colour.
    Other,
    /// The curve beside every other tab, a quiet rule.
    Curve,
    /// The folder under a tab.
    Sub,
    /// The new tab button, the gaps and the padding past the last tab.
    Quiet,
}

/// The bar as pieces to paint, one list per row, left to right, each with how it is painted.
///
/// The painter needs the pieces and not one string, because a tab is several colours: its
/// curve, its name, its close. The same segments [`hit`] uses, so the two agree by
/// construction.
pub fn painted(terminals: &[AgentRow], focused: u32, cols: usize) -> [Vec<(String, Ink)>; ROWS] {
    let mut rows: [Vec<(String, Ink)>; ROWS] = [Vec::new(), Vec::new()];
    let mut used = 0;
    for seg in segments(terminals, focused, cols) {
        used += seg.wide;
        for (row, pieces) in rows.iter_mut().zip(seg.rows) {
            row.extend(pieces);
        }
    }
    if used < cols {
        for row in &mut rows {
            row.push((" ".repeat(cols - used), Ink::Quiet));
        }
    }
    rows
}

/// What a click on the tab bar means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The tab drawn at that column.
    Terminal(u32),
    /// The close on the focused tab.
    Close(u32),
    /// The `+` that makes a new terminal.
    New,
}

/// What the tab bar drew at one cell, for a click: `row` counts from the bar's top.
///
/// `None` on the padding past the last tab, so a click on empty bar does
/// nothing rather than reaching the nearest terminal.
pub fn hit(
    terminals: &[AgentRow],
    focused: u32,
    cols: usize,
    col: usize,
    row: usize,
) -> Option<Hit> {
    if col >= cols || row >= ROWS {
        return None;
    }
    let mut used = 0;
    for seg in segments(terminals, focused, cols) {
        let start = used;
        used += seg.wide;
        if col < used {
            return match seg.hit {
                Some(Hit::Terminal(terminal)) if row == 0 && seg.close == Some(col - start) => {
                    Some(Hit::Close(terminal))
                }
                other => other,
            };
        }
    }
    None
}

/// One piece of the bar, a tab or the new tab button: what it paints on each row, all
/// `wide` columns, and where its close is when it has one.
struct Segment {
    rows: [Vec<(String, Ink)>; ROWS],
    wide: usize,
    close: Option<usize>,
    hit: Option<Hit>,
}

/// The pieces of the bar, left to right, each with what a click on it means.
///
/// One function and not two, because [`painted`] and [`hit`] have to agree about where every
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
fn segments(terminals: &[AgentRow], focused: u32, cols: usize) -> Vec<Segment> {
    // A column of air after the rule the pane starts at, so the first curve does not lean
    // on it. Never wider than the bar.
    let margin = cols.min(1);
    let budget = cols.saturating_sub(margin + crate::width::width(NEW));
    let mut used = 0;
    let mut out = Vec::with_capacity(terminals.len() + 2);
    out.push(Segment {
        rows: [
            vec![(" ".repeat(margin), Ink::Quiet)],
            vec![(" ".repeat(margin), Ink::Quiet)],
        ],
        wide: margin,
        close: None,
        hit: None,
    });
    let first = scrolled(terminals, focused, budget);
    for (at, row) in terminals.iter().enumerate().skip(first) {
        let seg = tab(row, at + 1, row.terminal == focused);
        // The gap only counts once another tab follows it.
        if used + seg.wide - GAP.len() > budget {
            break;
        }
        used += seg.wide;
        out.push(seg);
    }
    // The last tab drawn needs no gap: the new tab button brings its own space.
    if let Some(last) = out.last_mut().filter(|seg| seg.hit.is_some()) {
        for row in &mut last.rows {
            row.pop();
        }
        last.wide -= GAP.len();
    }
    // Cut to the columns there are: on a bar narrower than the button, a whole button
    // autowraps its last cell into the pane below.
    let wide = (cols - margin).min(crate::width::width(NEW));
    out.push(Segment {
        rows: [
            vec![(crate::width::text(NEW, wide), Ink::Quiet)],
            vec![(" ".repeat(wide), Ink::Quiet)],
        ],
        wide,
        close: None,
        hit: Some(Hit::New),
    });
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
            .map(|(i, row)| tab(row, first + i + 1, false).wide)
            .sum::<usize>()
            - GAP.len()
            <= budget;
        if fits {
            break;
        }
        first += 1;
    }
    first
}

/// One tab: a curve down its left edge binding its label on top to its folder under it,
/// the close's column at its top right, and the gap before the next tab.
///
/// ```text
/// ╭ 1 claude     ×   ╭ 2 codex
/// ╰ ~/coding/wend    ╰ …/qoro-wt-events-log
/// ```
///
/// Only the inks and the close differ between a focused tab and the same tab unfocused,
/// never a width.
fn tab(row: &AgentRow, position: usize, focused: bool) -> Segment {
    let name = format!(" {}", label(row, position));
    let path = format!(" {}", folder(row));
    // Two blank columns at least between the name and the close, one after the folder.
    let inner = (crate::width::width(&name) + 3).max(crate::width::width(&path) + 1);
    let pad =
        |text: &str, to: usize| format!("{text}{}", " ".repeat(to - crate::width::width(text)));
    let (name_ink, mark_ink) = if focused {
        (Ink::Focused, Ink::FocusedMark)
    } else if row.state == "sleeping" {
        // Asleep: drawn in the folder's quiet ink, so it reads as there but not running.
        (Ink::Sub, Ink::Curve)
    } else {
        (Ink::Other, Ink::Curve)
    };
    let corner = if focused {
        CLOSE.to_string()
    } else {
        " ".to_string()
    };
    Segment {
        rows: [
            vec![
                (CURVE[0].to_string(), mark_ink),
                (pad(&name, inner - 1), name_ink),
                (corner, mark_ink),
                (GAP.to_string(), Ink::Quiet),
            ],
            vec![
                (CURVE[1].to_string(), mark_ink),
                (pad(&path, inner), Ink::Sub),
                (GAP.to_string(), Ink::Quiet),
            ],
        ],
        wide: 1 + inner + GAP.len(),
        close: focused.then_some(inner),
        hit: Some(Hit::Terminal(row.terminal)),
    }
}

/// The folder a tab runs in, for the row under its label.
fn folder(row: &AgentRow) -> String {
    path_of(row.cwd.as_deref(), std::env::var("HOME").ok().as_deref())
}

/// The most columns a tab's path takes before its start gives way to an ellipsis.
const MAX_PATH: usize = 20;

/// A folder as a path with the home folder as `~`, and past [`MAX_PATH`] columns its end:
/// the end is what tells two worktrees of one repository apart.
fn path_of(cwd: Option<&str>, home: Option<&str>) -> String {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return String::new();
    };
    let cwd = match cwd.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    };
    let home = home
        .map(|home| home.trim_end_matches('/'))
        .filter(|home| !home.is_empty());
    let path = match home {
        Some(home) if cwd == home => "~".to_string(),
        Some(home) if cwd.starts_with(&format!("{home}/")) => format!("~{}", &cwd[home.len()..]),
        _ => cwd.to_string(),
    };
    if crate::width::width(&path) <= MAX_PATH {
        return path;
    }
    let mut end = String::new();
    let mut used = 0;
    for c in path.chars().rev() {
        let wide = crate::width::width(c.encode_utf8(&mut [0; 4]));
        if used + wide > MAX_PATH - 1 {
            break;
        }
        used += wide;
        end.insert(0, c);
    }
    format!("\u{2026}{end}")
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
///
/// No state mark: the agents panel beside the bar already says what each one is doing.
fn label(row: &AgentRow, position: usize) -> String {
    // A name a person chose outlives the program: a tab called `release` stays `release`
    // while it runs a shell, then cargo, then an agent, and a bar of chosen names is a bar
    // somebody can read without looking at what happens to be running in each one.
    if let Some(name) = &row.name {
        return clip(&format!("{position} {name}"));
    }
    match &row.program {
        Some(program) => {
            let name = program
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(program);
            clip(&format!("{position} {name}"))
        }
        None => clip(&format!("{position}")),
    }
}

/// Cut a label to at most [`MAX_LABEL`] columns.
fn clip(text: &str) -> String {
    crate::width::fit(text, MAX_LABEL.min(crate::width::width(text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::Line;

    /// The bar's rows exactly as [`painted`] hands them to the screen: the tests pin what
    /// is painted, not a copy of it that cuts where the painted bar did not.
    fn rows_of(terminals: &[AgentRow], focused: u32, cols: usize) -> [String; ROWS] {
        painted(terminals, focused, cols)
            .map(|pieces| pieces.into_iter().map(|(text, _)| text).collect())
    }

    /// The top row, the one with the names on it.
    fn render(terminals: &[AgentRow], focused: u32, cols: usize) -> Line {
        let [top, _] = rows_of(terminals, focused, cols);
        Line {
            fill: None,
            text: top,
            colour: None,
            accent: None,
            indicator: None,
            runs: Vec::new(),
        }
    }

    fn agent(terminal: u32, program: &str) -> AgentRow {
        AgentRow {
            keep_awake: false,
            news: false,
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
            discards: None,
        }
    }

    fn shell(terminal: u32) -> AgentRow {
        AgentRow {
            keep_awake: false,
            news: false,
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
            discards: None,
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
            for row in rows_of(&sample(), 2, cols) {
                assert_eq!(crate::width::width(&row), cols, "{row:?}");
            }
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
        let line = render(&sample(), 9, 80);
        assert_eq!(
            line.text.trim_end(),
            " \u{256d} 1      \u{256d} 2 tue      \u{256d} 3 private      \u{256d} 4 pet    +"
        );
    }

    /// A name somebody chose beats the name of whatever happens to be running, because the
    /// program changes under it and the name is how they find the tab again.
    #[test]
    fn a_named_tab_is_called_what_it_was_named() {
        let mut rows = sample();
        rows[1].name = Some("release".to_string());
        let line = render(&rows, 9, 60);
        assert!(line.text.contains("2 release"), "{:?}", line.text);
        assert!(!line.text.contains("2 tue"), "{:?}", line.text);
        // And no state mark rides along: the agents panel says what it is doing.
        rows[1].state = "blocked".to_string();
        let line = render(&rows, 9, 60);
        assert!(line.text.contains(" 2 release "), "{:?}", line.text);
        assert!(!line.text.contains('!'), "{:?}", line.text);
    }

    #[test]
    fn the_focused_tab_is_marked_by_ink_and_its_close_alone() {
        // Only the close differs in text: the inks carry the focus, so the bar never
        // shifts when the focus moves.
        let plain = render(&sample(), 9, 80);
        let focused = render(&sample(), 3, 80);
        assert_eq!(plain.text, focused.text.replace('\u{00d7}', " "));
        assert_eq!(
            focused.text.matches('\u{00d7}').count(),
            1,
            "{:?}",
            focused.text
        );
        let [top, _] = painted(&sample(), 3, 80);
        assert!(
            top.iter()
                .any(|(text, ink)| text.contains("3 private") && *ink == Ink::Focused),
            "{top:?}"
        );
        assert_eq!(
            top.iter().filter(|(_, ink)| *ink == Ink::Focused).count(),
            1,
            "exactly one tab owns the keyboard: {top:?}"
        );
        // Its curve and its close are ember, every other curve is a quiet rule.
        let marks: Vec<&str> = top
            .iter()
            .filter(|(_, ink)| *ink == Ink::FocusedMark)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(marks, ["\u{256d}", "\u{00d7}"], "{top:?}");
        assert_eq!(
            top.iter()
                .filter(|(text, ink)| *ink == Ink::Curve && text == "\u{256d}")
                .count(),
            3,
            "{top:?}"
        );
    }

    #[test]
    fn focusing_a_tab_does_not_move_the_ones_after_it() {
        let unfocused = rows_of(&sample(), 9, 80);
        for focused in 1..=4 {
            let rows = rows_of(&sample(), focused, 80);
            assert_eq!(rows[0].replace('\u{00d7}', " "), unfocused[0], "{rows:?}");
            assert_eq!(rows[1], unfocused[1], "{rows:?}");
        }
    }

    /// The close sits in the top right corner of the focused tab's block, a click on it
    /// is the close and the cell under it is the tab, and each tab's folder is under its
    /// name.
    #[test]
    fn the_close_is_the_focused_tabs_top_right_corner_and_the_folder_sits_under_it() {
        let mut rows = sample();
        rows[2].cwd = Some("/srv/app-feat-x".to_string());
        let [top, under] = rows_of(&rows, 3, 60);
        let corner = crate::width::width(&top[..top.find('\u{00d7}').expect("no close")]);
        let name = crate::width::width(&top[..top.find("3 private").unwrap()]);
        assert!(corner > name, "{top:?}");
        assert_eq!(hit(&rows, 3, 60, corner, 0), Some(Hit::Close(3)));
        assert_eq!(hit(&rows, 3, 60, corner, 1), Some(Hit::Terminal(3)));
        assert_eq!(hit(&rows, 3, 60, name, 0), Some(Hit::Terminal(3)));
        assert_eq!(hit(&rows, 3, 60, name, 1), Some(Hit::Terminal(3)));
        let folder = under
            .find("/srv/app-feat-x")
            .expect("no path under the tab");
        assert_eq!(crate::width::width(&under[..folder]), name, "{under:?}");
        // No other tab carries a close.
        let [unfocused, _] = rows_of(&rows, 9, 60);
        assert!(!unfocused.contains('\u{00d7}'), "{unfocused:?}");
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
        let line = render(&sample(), 1, 12);
        // Room for the first tab, ` + `, and nothing else.
        assert_eq!(line.text, " \u{256d} 1  \u{00d7} +   ");
        assert!(!line.text.contains("tue"));
    }

    /// The path under a tab: home as `~`, and a long one keeps its end.
    #[test]
    fn the_path_under_a_tab_is_home_relative_and_keeps_its_end() {
        let home = Some("/Users/me");
        assert_eq!(path_of(Some("/Users/me"), home), "~");
        assert_eq!(path_of(Some("/Users/me/"), home), "~");
        assert_eq!(
            path_of(Some("/Users/me/coding/wend"), home),
            "~/coding/wend"
        );
        assert_eq!(path_of(Some("/Users/meg/a"), home), "/Users/meg/a");
        assert_eq!(path_of(Some("/"), home), "/");
        assert_eq!(path_of(None, home), "");
        assert_eq!(path_of(Some("  "), home), "");
        let long = path_of(Some("/Users/me/coding/qoro-wt-events-log"), home);
        assert_eq!(long, "\u{2026}/qoro-wt-events-log");
        assert_eq!(crate::width::width(&long), MAX_PATH);
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
            // Where each tab starts and where the new button starts, found in the painted
            // bar rather than recomputed, so the test is not a copy of the code. A tab
            // runs from its label to the next tab.
            let starts: Vec<(usize, u32)> = rows
                .iter()
                .enumerate()
                .filter_map(|(at, r)| {
                    let byte = text.find(&format!("\u{256d} {} ", label(r, at + 1)))?;
                    Some((crate::width::width(&text[..byte]), r.terminal))
                })
                .collect();
            let plus = text
                .find(" + ")
                .map(|byte| crate::width::width(&text[..byte]));
            for col in 0..cols {
                let want = starts
                    .iter()
                    .rfind(|(start, _)| *start <= col)
                    .map(|(_, terminal)| *terminal)
                    .filter(|_| plus.is_none_or(|p| col < p));
                for row in 0..ROWS {
                    let got = hit(&rows, 3, cols, col, row);
                    match want {
                        Some(terminal) => assert!(
                            got == Some(Hit::Terminal(terminal))
                                || (row == 0 && terminal == 3 && got == Some(Hit::Close(3))),
                            "column {col} row {row} of {cols}: {got:?}"
                        ),
                        // Off a tab is the new button or the padding past it, never a
                        // terminal next to the one aimed at.
                        None => assert!(
                            got.is_none_or(|h| h == Hit::New),
                            "column {col} of {cols} reached a terminal it did not aim at: {got:?}"
                        ),
                    }
                }
            }
            assert_eq!(hit(&rows, 3, cols, cols, 0), None, "past the right edge");
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
                let [pieces, _] = painted(&many, focused, cols);
                let clickable: Vec<u32> = (0..cols)
                    .filter_map(|col| match hit(&many, focused, cols, col, 1) {
                        Some(Hit::Terminal(terminal)) => Some(terminal),
                        _ => None,
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
        let at_seven = render(&many, 7, 40).text;
        let at_eight = render(&many, 8, 40).text;
        // Terminal 7 is still on the bar after moving to 8, so the person's eye keeps its
        // place. A bar that jumped the focused tab to the left edge would have dropped it.
        assert!(at_seven.contains("7 claude"), "{at_seven:?}");
        assert!(at_eight.contains("7 claude"), "{at_eight:?}");
    }

    #[test]
    fn a_click_on_the_new_tab_button_makes_a_terminal_and_on_padding_does_nothing() {
        let rows = sample();
        let text = render(&rows, 1, 80).text;
        let plus = text.find('+').unwrap_or_else(|| panic!("{text:?}"));
        let plus_col = crate::width::width(&text[..plus]) + 1;
        assert_eq!(hit(&rows, 1, 80, plus_col, 0), Some(Hit::New));
        assert_eq!(hit(&rows, 1, 80, 79, 0), None);
    }

    #[test]
    fn an_empty_space_still_offers_a_way_to_make_a_terminal() {
        let line = render(&[], 1, 10);
        assert_eq!(line.text, "  +       ");
        assert_eq!(hit(&[], 1, 10, 1, 0), Some(Hit::New));
    }
}

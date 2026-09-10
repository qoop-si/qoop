//! The left column: the spaces above, the agents below.
//!
//! Two panels and not one list, because the two questions a person asks of this column are
//! different questions. "Which project am I in" is answered by a short list that barely
//! changes, and "who needs me" is answered by a long list that changes constantly. Stacked,
//! the short list stays put while the long one moves under it, so the eye keeps its place.
//!
//! The agents panel crosses spaces on purpose. An agent that blocked in a space a person is
//! not looking at is exactly the one they need to be told about, and a panel that only
//! listed the current space would hide it until they went looking.
//!
//! Pure text here. The caller paints attributes over these lines, so
//! the focused space and the focused terminal carry a marker character too: a screenshot, a
//! test assertion, a pipe into a file and a terminal with no colour all have to answer
//! "which one has my keyboard".

use qoro_runtime::control::{AgentRow, WorkspaceRow};

/// One painted row of the chrome: its text, and the colour its state gives it.
///
/// The colour travels with the text because the state is known here: a painter that had to
/// parse the state back out of the string it was handed would be reading text this module
/// just finished writing.
pub struct Line {
    pub text: String,
    /// An ansi palette index, or `None` for a row with nothing to say.
    pub colour: Option<u8>,
}

/// The colour a state earns.
///
/// Deliberately few: a person scanning this column is looking for the one row that needs
/// them, and a column where every row is a different colour is a column with no signal in
/// it. Blocked and failed are the two that mean "come here", so they are the two that are
/// warm.
pub fn colour_for(state: &str) -> Option<u8> {
    match state {
        "blocked" => Some(3),
        "failed" => Some(1),
        "working" => Some(6),
        "done" => Some(2),
        "starting" => Some(4),
        _ => None,
    }
}

/// Columns the left column occupies.
///
/// Fixed rather than negotiated: the terminal beside it is a real PTY whose width has to
/// be decided before its child starts,
/// and a column that grew with the longest space name would reflow that child every time
/// somebody created a workspace.
pub const WIDTH: usize = 22;

/// Titles, which are rows of the panels and not decoration the caller adds.
///
/// A person opening this for the first time sees two lists of names with nothing saying
/// what either is. Keeping the titles inside the returned rows also keeps the arithmetic in
/// one place: a caller that drew its own titles would have to know how many rows this
/// function had already spent on them.
const SPACES_TITLE: &str = "spaces";
const AGENTS_TITLE: &str = "agents";

/// Marks the space or the terminal that has the keyboard. Characters and not just
/// attributes, because attributes do not survive being copied out of a terminal.
const FOCUSED: char = '\u{25cf}';
const UNFOCUSED: char = '\u{25cb}';

/// Separates the space from the terminal on an agent's first line.
const JOIN: &str = "\u{b7}";

/// Where an agent's second line starts, so the program and its state read as detail
/// belonging to the line above rather than as another agent.
const INDENT: usize = 5;

/// Columns held back for the program name when a long state would otherwise eat the row.
/// A row showing only its state does not say whose state it is.
const MIN_NAME: usize = 3;

/// The state a terminal has when nothing has been able to say what it is doing.
const UNKNOWN: &str = "unknown";

/// What to call an agent whose identity the daemon could not read.
///
/// Not `shell`. A row only reaches this panel because an agent is registered against it,
/// so naming it after the thing it is known not to be would be the one word on screen that
/// is false.
const UNNAMED: &str = "agent";

/// The largest fraction of the column the spaces panel may take, as a divisor.
///
/// Spaces are few and agents are many, and the panel that has to be scrolled is the one
/// that should get the room. A third is enough for the handful of projects a person has
/// open and leaves the rest to the list that actually grows.
const SPACES_SHARE: usize = 3;

/// What the left column drew on one screen row, so a click can act on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Space(i64),
    Terminal(u32),
}

/// Both panels of the left column, painted into `height` rows.
///
/// Always exactly `height` lines of exactly [`WIDTH`] columns, so the caller can paint them
/// blindly: a short line would leave the previous frame's pixels behind, and a long one
/// would spill into the terminal on the right.
///
/// Neither `focused_space` nor `focused_terminal` need appear in the lists. The daemon's
/// answer and the client's idea of what has the keyboard are two messages that can cross,
/// and a client that panicked on the overlap would die exactly when a terminal exited.
pub fn render(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
) -> Vec<Line> {
    rows(spaces, agents, focused_space, focused_terminal, height)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

/// What a click on one screen row of the left column means, counted from zero.
///
/// `None` for a title and for a blank row, so a click on the word `agents` does nothing
/// rather than something arbitrary.
pub fn hit(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    row: usize,
) -> Option<Hit> {
    rows(spaces, agents, focused_space, focused_terminal, height)
        .into_iter()
        .nth(row)
        .and_then(|(_, hit)| hit)
}

/// Every row of the column, with what a click on it means.
///
/// One function and not two, because [`render`] and [`hit`] have to agree on every row and
/// the same scrolling sum written out twice is how a click lands on the row above the one a
/// person aimed at. Building the answer once makes the agreement true by construction, and
/// the test below only has to confirm it.
fn rows(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let split = spaces_rows(spaces.len(), height);
    let mut out = spaces_panel(spaces, focused_space, split);
    out.extend(agents_panel(
        spaces,
        agents,
        focused_terminal,
        height - split,
    ));
    out
}

/// How many rows the spaces panel gets, its title included.
///
/// What it needs, capped at a share of the column. Capped rather than proportional so that
/// a person with two spaces does not stare at eight blank rows above their agents.
fn spaces_rows(spaces: usize, height: usize) -> usize {
    (1 + spaces).min((height / SPACES_SHARE).max(1)).min(height)
}

/// The spaces panel: its title, then one row per space, scrolled to keep the focused one on
/// screen and padded out to the rows it was given.
fn spaces_panel(spaces: &[WorkspaceRow], focused: i64, height: usize) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(title(SPACES_TITLE), None)];
    let window = height - 1;
    let first = scrolled(
        spaces.len(),
        spaces.iter().position(|s| s.id == focused),
        window,
    );
    for offset in 0..window {
        out.push(match spaces.get(first + offset) {
            Some(space) => (
                Line {
                    // No colour: a workspace has no state to have a colour about, and a
                    // column where everything is coloured has no signal left in it.
                    text: fit(&format!(" {} {}", mark(space.id == focused), space.name)),
                    colour: None,
                },
                Some(Hit::Space(space.id)),
            ),
            None => blank(),
        });
    }
    out
}

/// The agents panel: its title, then two rows per agent, scrolled to keep the focused one on
/// screen.
///
/// Terminals that are not running an agent are dropped here and only here. They are still
/// tabs, because a person needs to reach the shell they opened, but they are not something
/// that can want attention, and a panel that listed them would bury the rows that do.
///
/// The row's own flag decides, not its state. Filtering on the state string instead looked
/// the same until a shell ran a command nobody has detection rules for, at which point its
/// state became `unknown` and the row vanished from the panel mid task, which reads as a
/// terminal that died.
fn agents_panel(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused: u32,
    height: usize,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(title(AGENTS_TITLE), None)];
    let body = height - 1;
    if body == 0 {
        return out;
    }
    let listed: Vec<&AgentRow> = agents.iter().filter(|a| a.agent).collect();
    // Two rows per agent, so an odd body leaves room for one more first line than it does
    // for whole agents. That last agent is drawn half on purpose: a cut off row is how a
    // list says there is more of it below, and the alternative is a blank row that says
    // nothing. Scrolling still anchors on whole agents, so the focused one is never the
    // half.
    let window = body.div_ceil(2);
    let whole = (body / 2).max(1);
    let first = scrolled(
        listed.len(),
        listed.iter().position(|a| a.terminal == focused),
        whole,
    );
    for offset in 0..window {
        match listed.get(first + offset) {
            Some(agent) => {
                let hit = Some(Hit::Terminal(agent.terminal));
                out.push((
                    Line {
                        text: fit(&format!(
                            " {} {} {JOIN} {}",
                            mark(agent.terminal == focused),
                            space_name(spaces, agent.workspace),
                            agent.terminal
                        )),
                        colour: colour_for(&agent.state),
                    },
                    hit,
                ));
                out.push((
                    Line {
                        text: detail(agent),
                        colour: colour_for(&agent.state),
                    },
                    hit,
                ));
            }
            None => {
                out.push(blank());
                out.push(blank());
            }
        }
    }
    out.truncate(height);
    out
}

/// An agent's second line: the program on the left, its state flush against the right edge.
///
/// The state is pinned right so the eye can scan one column for it without every program
/// name having to be cut to the same length first.
fn detail(agent: &AgentRow) -> String {
    let body = WIDTH.saturating_sub(INDENT);
    // An unknown state is drawn as nothing. It is a claim about nothing, so a column of the
    // word `unknown` trains the eye to skip the one column that matters when an agent
    // really is blocked, and it spends seven of the seventeen columns this line has on
    // saying so. Measured: a terminal behind a wrapper rendered as `behind do  unknown`,
    // where the two words that were cut are the only ones that identified it.
    let shown = if agent.state == UNKNOWN {
        ""
    } else {
        &agent.state
    };
    let state = clip(shown, body.saturating_sub(MIN_NAME + 1));
    let name = fit_to(
        &label(agent),
        body.saturating_sub(crate::width::width(&state) + 1),
    );
    fit(&format!("{}{name} {state}", " ".repeat(INDENT)))
}

/// What to call the program in the terminal.
///
/// The last path segment, because a column this narrow renders `/opt/homebrew/bin/claude`
/// as `/opt/homebre`, which identifies nothing. A wrapper is announced as a wrapper: a
/// person shown `docker` would conclude their agent is called docker.
fn label(agent: &AgentRow) -> String {
    match (&agent.program, &agent.behind) {
        (Some(program), _) => program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(program)
            .to_string(),
        (None, Some(wrapper)) => format!("behind {wrapper}"),
        (None, None) => UNNAMED.to_string(),
    }
}

/// The name of the space an agent belongs to.
///
/// Falls back to the number when the space is not in the list. The two lists arrive in two
/// messages that can cross, and an agent that vanished from the panel for one frame because
/// its space had not landed yet would be a flicker nobody could explain.
fn space_name(spaces: &[WorkspaceRow], id: i64) -> String {
    spaces
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// The first entry of a panel that is on screen.
///
/// Scrolls only as far as it takes to bring the focused entry back into view, and never past
/// the end, so a list stays still while the focus moves inside the window.
fn scrolled(len: usize, focused: Option<usize>, window: usize) -> usize {
    match focused {
        Some(i) if window > 0 => i.saturating_sub(window - 1).min(len.saturating_sub(window)),
        _ => 0,
    }
}

/// A panel title, which is a row like any other and therefore the full width.
fn title(text: &str) -> Line {
    Line {
        text: fit(&format!(" {text}")),
        colour: None,
    }
}

/// A row with nothing on it, and nothing to do when it is clicked.
fn blank() -> (Line, Option<Hit>) {
    (
        Line {
            text: " ".repeat(WIDTH),
            colour: None,
        },
        None,
    )
}

/// The marker for a row that has the keyboard, and for one that does not.
fn mark(focused: bool) -> char {
    if focused { FOCUSED } else { UNFOCUSED }
}

/// Cut or pad to exactly [`WIDTH`] columns.
fn fit(text: &str) -> String {
    fit_to(text, WIDTH)
}

/// Cut or pad to exactly `cols` columns.
///
/// Columns and not characters: a program or a space named with a full width glyph would
/// otherwise draw past the column and over the terminal beside it, which is the arithmetic
/// error [`crate::width`] exists to stop repeating.
fn fit_to(text: &str, cols: usize) -> String {
    crate::width::fit(text, cols)
}

/// Cut to at most `cols` columns without padding, for the piece that is already flush
/// against the right edge.
fn clip(text: &str, cols: usize) -> String {
    crate::width::fit(text, cols).trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.to_string(),
            terminals: 1,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            workspace,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: None,
        }
    }

    fn shell(terminal: u32, workspace: i64) -> AgentRow {
        AgentRow {
            terminal,
            workspace,
            agent: false,
            program: Some("sh".to_string()),
            behind: None,
            state: "idle".to_string(),
            generation: None,
        }
    }

    fn sample() -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
        (
            vec![space(1, "app"), space(2, "personal"), space(3, "dub")],
            vec![
                agent(1, 1, "claude", "working"),
                shell(2, 1),
                agent(3, 2, "codex", "idle"),
            ],
        )
    }

    #[test]
    fn every_line_is_exactly_the_column_width() {
        let (spaces, agents) = sample();
        for height in [1usize, 2, 3, 5, 9, 14, 40] {
            let lines = render(&spaces, &agents, 1, 1, height);
            assert_eq!(lines.len(), height, "height {height}");
            for line in &lines {
                assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
            }
        }
    }

    #[test]
    fn a_wide_name_is_measured_in_columns_and_not_in_characters() {
        let spaces = vec![space(1, &"\u{4e2d}".repeat(30))];
        let agents = vec![agent(1, 1, &"\u{1f30d}".repeat(30), "working")];
        for line in render(&spaces, &agents, 1, 1, 12) {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn both_panels_carry_their_own_title() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.trim_end()).collect();
        assert_eq!(texts[0], format!(" {SPACES_TITLE}"));
        assert!(
            texts.iter().any(|t| *t == format!(" {AGENTS_TITLE}")),
            "{texts:?}"
        );
    }

    #[test]
    fn the_focused_space_and_the_focused_terminal_are_marked_in_plain_text() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 2, 3, 14);
        let marked: Vec<&str> = lines
            .iter()
            .map(|l| l.text.trim_end())
            .filter(|t| t.contains(FOCUSED))
            .collect();
        assert_eq!(marked.len(), 2, "{marked:?}");
        assert!(marked.iter().any(|t| t.contains("personal")), "{marked:?}");
        assert!(marked.iter().any(|t| t.ends_with("3")), "{marked:?}");
    }

    #[test]
    fn an_agent_shows_its_space_and_terminal_then_its_program_and_state() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        let head = texts
            .iter()
            .position(|t| t == &format!(" {FOCUSED} app {JOIN} 1"))
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert_eq!(texts[head + 1], "     claude    working");
    }

    #[test]
    fn a_terminal_that_is_not_running_an_agent_is_left_out_of_the_agents_panel() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 20);
        // Terminal 2 is a plain shell. Terminals 1 and 3 have agents and are listed.
        let listed: Vec<Option<Hit>> = (0..20)
            .map(|row| hit(&spaces, &agents, 1, 1, 20, row))
            .collect();
        assert!(listed.contains(&Some(Hit::Terminal(1))));
        assert!(listed.contains(&Some(Hit::Terminal(3))));
        assert!(!listed.contains(&Some(Hit::Terminal(2))));
        assert!(!lines.iter().any(|l| l.text.contains("sh ")));
    }

    #[test]
    fn an_agent_row_carries_the_colour_of_its_state_and_a_title_carries_none() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        assert_eq!(lines[0].colour, None, "the spaces title");
        let coloured: Vec<Option<u8>> = lines
            .iter()
            .filter(|l| l.text.contains("claude") || l.text.contains(&format!("app {JOIN} 1")))
            .map(|l| l.colour)
            .collect();
        assert_eq!(coloured, vec![colour_for("working"), colour_for("working")]);
        assert!(
            lines
                .iter()
                .filter(|l| l.text.contains("personal") && !l.text.contains(JOIN))
                .all(|l| l.colour.is_none())
        );
    }

    #[test]
    fn each_panel_scrolls_on_its_own_to_keep_its_focused_row_visible() {
        let spaces: Vec<WorkspaceRow> = (1..=8).map(|n| space(n, &format!("s{n}"))).collect();
        let agents: Vec<AgentRow> = (1..=12)
            .map(|n| agent(n as u32, 1, "claude", "idle"))
            .collect();
        for focused_space in 1..=8i64 {
            for focused_terminal in 1..=12u32 {
                let drawn: Vec<Hit> = (0..15)
                    .filter_map(|row| {
                        hit(&spaces, &agents, focused_space, focused_terminal, 15, row)
                    })
                    .collect();
                assert!(
                    drawn.contains(&Hit::Space(focused_space)),
                    "space {focused_space} fell out"
                );
                assert!(
                    drawn.contains(&Hit::Terminal(focused_terminal)),
                    "terminal {focused_terminal} fell out"
                );
            }
        }
    }

    #[test]
    fn the_spaces_panel_never_takes_more_than_its_share_of_the_column() {
        let spaces: Vec<WorkspaceRow> = (1..=30).map(|n| space(n, &format!("s{n}"))).collect();
        let agents = vec![agent(1, 1, "claude", "idle")];
        let lines = render(&spaces, &agents, 1, 1, 30);
        let agents_title = lines
            .iter()
            .position(|l| l.text.trim_end() == format!(" {AGENTS_TITLE}"))
            .unwrap_or_else(|| panic!("no agents panel"));
        assert_eq!(agents_title, 10);
    }

    #[test]
    fn a_height_too_small_for_both_panels_still_returns_that_many_lines() {
        let (spaces, agents) = sample();
        for height in 0..6 {
            let lines = render(&spaces, &agents, 1, 1, height);
            assert_eq!(lines.len(), height);
            assert!(lines.iter().all(|l| crate::width::width(&l.text) == WIDTH));
        }
    }

    #[test]
    fn hit_returns_exactly_what_render_drew_on_every_row() {
        let (spaces, agents) = sample();
        for height in [1usize, 2, 3, 4, 7, 9, 14, 25] {
            let lines = render(&spaces, &agents, 2, 3, height);
            for (row, line) in lines.iter().enumerate() {
                let text = line.text.trim_end().to_string();
                let got = hit(&spaces, &agents, 2, 3, height, row);
                let want = if text == format!(" {SPACES_TITLE}")
                    || text == format!(" {AGENTS_TITLE}")
                    || text.is_empty()
                {
                    None
                } else if let Some(s) = spaces
                    .iter()
                    .find(|s| text == format!(" {} {}", mark(s.id == 2), s.name))
                {
                    Some(Hit::Space(s.id))
                } else {
                    // Everything left is one of the two lines of an agent, and both of them
                    // belong to that agent.
                    let head = if text.contains(JOIN) {
                        row
                    } else {
                        row.saturating_sub(1)
                    };
                    let name = lines[head].text.trim_end().to_string();
                    agents
                        .iter()
                        .find(|a| name.ends_with(&format!("{JOIN} {}", a.terminal)))
                        .map(|a| Hit::Terminal(a.terminal))
                };
                assert_eq!(got, want, "row {row} of {height}: {text:?}");
            }
            assert_eq!(hit(&spaces, &agents, 2, 3, height, height), None);
        }
    }

    #[test]
    fn a_focus_that_is_in_neither_list_does_not_panic() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 99, 99, 12);
        assert_eq!(lines.len(), 12);
        assert!(!lines.iter().any(|l| l.text.contains(FOCUSED)));
        assert_eq!(render(&[], &[], 1, 1, 5).len(), 5);
    }

    #[test]
    fn an_agent_in_a_space_the_client_has_not_heard_of_shows_its_number() {
        let agents = vec![agent(4, 77, "codex", "blocked")];
        let lines = render(&[], &agents, 1, 4, 8);
        assert!(
            lines
                .iter()
                .any(|l| l.text.contains(&format!("77 {JOIN} 4"))),
            "{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_wrapper_is_shown_as_standing_in_the_way_and_never_as_the_program() {
        let agents = vec![AgentRow {
            terminal: 5,
            workspace: 1,
            agent: true,
            program: None,
            behind: Some("docker".to_string()),
            state: "idle".to_string(),
            generation: None,
        }];
        let lines = render(&[space(1, "app")], &agents, 1, 5, 8);
        assert!(
            lines.iter().any(|l| l.text.contains("behind")),
            "{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(!lines.iter().any(|l| l.text.contains("     docker")));
    }

    /// A terminal behind a wrapper is the case with the longest name and the least to say
    /// about its state, so it is the one the word `unknown` used to push off the row.
    #[test]
    fn an_unknown_state_is_drawn_as_nothing_and_leaves_the_name_the_whole_row() {
        let agents = vec![AgentRow {
            terminal: 5,
            workspace: 1,
            agent: true,
            program: None,
            behind: Some("docker".to_string()),
            state: UNKNOWN.to_string(),
            generation: None,
        }];
        let lines = render(&[space(1, "app")], &agents, 1, 5, 8);
        assert!(
            lines.iter().any(|l| l.text.contains("behind docker")),
            "{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(!lines.iter().any(|l| l.text.contains(UNKNOWN)));
    }

    #[test]
    fn an_absurdly_long_state_is_cut_but_still_leaves_some_of_the_program() {
        let agents = vec![agent(1, 1, "claude", &"z".repeat(80))];
        let lines = render(&[space(1, "app")], &agents, 1, 1, 8);
        let detail = lines
            .iter()
            .find(|l| l.text.contains('z'))
            .unwrap_or_else(|| panic!("no detail line"));
        assert_eq!(crate::width::width(&detail.text), WIDTH);
        assert!(detail.text.starts_with("     cla"), "{:?}", detail.text);
    }
}

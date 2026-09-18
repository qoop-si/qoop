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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// What this row is, which is what decides how it is painted. `None` for a row with
    /// nothing to say about itself, which the painter draws as ordinary text.
    pub colour: Option<crate::theme::Role>,
    /// A run inside the row painted in a role of its own: the column it starts at, how
    /// many columns it covers, and what it is.
    ///
    /// One run and not a general list of them, because there is exactly one thing on a row
    /// that is not about the row's own state: the name of the space it belongs to, in that
    /// space's colour. A row saying two things in two colours would be a row a person
    /// parses rather than reads.
    pub accent: Option<(usize, usize, crate::theme::Role)>,
}

/// The colour a state earns.
///
/// Deliberately few: a person scanning this column is looking for the one row that needs
/// them, and a column where every row is a different colour is a column with no signal in
/// it. Blocked and failed are the two that mean "come here", so they are the two that are
/// warm.
pub fn colour_for(state: &str) -> Option<crate::theme::Role> {
    match state {
        "blocked" | "failed" | "working" | "starting" | "done" => Some(crate::theme::of(state)),
        // Idle and unobserved rows are the background of this column, and a colour on
        // every row is a column with no signal in it.
        _ => None,
    }
}

/// Columns the left column occupies.
///
/// Fixed rather than negotiated: the terminal beside it is a real PTY whose width has to
/// be decided before its child starts, and a column that grew with the longest space name
/// would reflow that child every time somebody created a workspace.
///
/// Thirty. A comparable multiplexer ships twenty six as a default a person can drag
/// between eighteen and thirty six, and the wide end of that range is the right default
/// here: this column carries a space name, a separator, a mark, a name and a state word
/// flush against the right edge, and at twenty two the state had nothing left. Thirty
/// leaves a real project name room to be read rather than recognised from its first eight
/// characters.
pub const WIDTH: usize = 30;

/// Columns the left column occupies when it is folded away.
///
/// Four: a state mark and the space it needs. Enough to keep the one thing a
/// glance wants, which is colour and count, and cheap enough that folding is worth doing
/// on a narrow window rather than a last resort.
pub const COLLAPSED: usize = 4;

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

/// The column a space's name starts at in the spaces panel: a leading space and the focus
/// mark, then a space.
const SPACE_NAME_AT: usize = 3;

/// The column an agent's first row starts its names at: a leading space, the
/// focus mark, and a space.
const AGENT_NAME_AT: usize = 3;

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

/// The percentage of the column the spaces panel holds, its title and the blank rows under
/// it included.
///
/// A little under half. The spaces list is short and barely changes, the agents list is
/// long and changes constantly, so the growing one gets the larger half and the anchored
/// one gets a place that does not move.
const SPACES_TOP_SHARE: usize = 45;

/// Blank rows held under the spaces, so the panel below starts somewhere of its own.
const BREATHE: usize = 2;

/// The fewest rows the spaces panel takes, its title included, however few spaces there
/// are. Below this the two panels stop reading as two.
const SPACES_FLOOR: usize = 4;

/// What the left column drew on one screen row, so a click can act on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Space(i64),
    Terminal(u32),
    /// A collapsed group: a click opens it.
    Fold(Group),
    /// The `new` on the row between the panels: a terminal in the space on screen.
    New,
    /// The `menu` beside it: everything that is not a space, a tab or an agent.
    Menu,
    /// The mark at the foot of the column: fold the whole column away, and back.
    Collapse,
}

/// One collapsible group of the agents panel.
///
/// `NeedsYou` is not here on purpose: blocked and failed rows are the reason the
/// panel exists, so they are always open and never fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Running,
    Finished,
    Idle,
}

impl Group {
    fn name(self) -> &'static str {
        match self {
            Group::Running => "running",
            Group::Finished => "finished",
            Group::Idle => "idle",
        }
    }
}

/// Which idle groups are expanded.
///
/// Needs-you (`blocked`, `failed`) and running (`working`, `starting`) are always
/// open: they are what the panel is for. `finished` (`done`) and the rest start
/// closed, so nine rows become three. A click on a fold line opens that group;
/// `ctrl-a f` toggles both at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fold {
    pub finished_open: bool,
    pub idle_open: bool,
}

impl Fold {
    /// Every group open: what the panel drew before folding existed.
    pub fn all_open() -> Self {
        Fold {
            finished_open: true,
            idle_open: true,
        }
    }

    /// Needs-you and running open, the rest closed.
    pub fn default_closed() -> Self {
        Fold {
            finished_open: false,
            idle_open: false,
        }
    }

    /// Open one group, leaving the other as it was.
    pub fn open(&mut self, group: Group) {
        match group {
            Group::Running => {}
            Group::Finished => self.finished_open = true,
            Group::Idle => self.idle_open = true,
        }
    }

    /// Both open or both closed, whichever is the change.
    pub fn toggle_all(&mut self) {
        if self.finished_open && self.idle_open {
            *self = Fold::default_closed();
        } else {
            *self = Fold::all_open();
        }
    }

    fn is_open(self, group: Group) -> bool {
        match group {
            Group::Running => true,
            Group::Finished => self.finished_open,
            Group::Idle => self.idle_open,
        }
    }
}

impl Default for Fold {
    fn default() -> Self {
        Fold::default_closed()
    }
}

/// The group an agent state belongs to.
fn group_of(state: &str) -> Group {
    match state {
        "blocked" | "failed" => Group::Running,
        "working" | "starting" => Group::Running,
        "done" => Group::Finished,
        _ => Group::Idle,
    }
}

/// Whether the state always stays open, whatever the fold says.
fn needs_you(state: &str) -> bool {
    matches!(state, "blocked" | "failed")
}

/// A state nothing has observed yet.
///
/// Not idleness: a new agent folds away the moment it starts if unknown counts
/// as idle, and the panel would hide every arrival until detection settles. What
/// has not been looked at stays on screen.
fn unobserved(state: &str) -> bool {
    state == UNKNOWN
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

/// Both panels of the left column with idle groups folded, painted into `height` rows.
///
/// Same contract as [`render`]: exactly `height` lines of exactly [`WIDTH`]
/// columns. Collapsed groups become one counted line each, so the list reads
/// instead of being skimmed. Empty groups leave no line at all.
pub fn render_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    fold: &Fold,
) -> Vec<Line> {
    rows_folded(
        spaces,
        agents,
        focused_space,
        focused_terminal,
        height,
        fold,
    )
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

/// What a click on one screen row of the folded column means, counted from zero.
///
/// A click on a fold line opens that group; the caller repaints with the new fold.
pub fn hit_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    fold: &Fold,
    row: usize,
) -> Option<Hit> {
    rows_folded(
        spaces,
        agents,
        focused_space,
        focused_terminal,
        height,
        fold,
    )
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
    // The unfolded panel shows every agent, so what it wants is a title and two
    // lines each.
    let wanted = 1 + 2 * agents.iter().filter(|a| a.agent).count();
    let split = spaces_rows(spaces.len(), height, wanted);
    let mut out = spaces_panel(spaces, focused_space, split);
    // A rule between the panels, glued to both like a border, so the column
    // reads as two stacked boxes. It takes its row from the agents side, and
    // only when the agents side keeps at least its title: a divider that ate
    // the title would divide nothing from nothing.
    let rest = height - split;
    if rest >= 2 {
        out.push(divider());
        out.extend(agents_panel(spaces, agents, focused_terminal, rest - 1));
    } else {
        out.extend(agents_panel(spaces, agents, focused_terminal, rest));
    }
    out
}

/// How many rows the spaces panel gets, its title included.
///
/// A share of the column, held whether or not the spaces fill it, so the two panels sit in
/// the same places on every screen. Content hugging put the whole column in a clump at the
/// top with the rest of it black: the agents panel moved up and down as spaces came and
/// went, and a list whose heading moves is a list a person has to find before they can
/// read it. The blank rows under the spaces are not waste, they are what makes the agents
/// panel a place.
///
/// It still shrinks to what is needed when what is needed is less than the share, but only
/// on a column too short to give both panels room: below that a fixed share would leave a
/// two line agents panel under eight blanks.
fn spaces_rows(spaces: usize, height: usize, agents_wanted: usize) -> usize {
    // The column divides at a share of its height, so the lower panel is a place: its
    // heading sits in the same row whatever is in the upper one. Hugging the content
    // instead put both panels in a clump at the top with the rest of the column black,
    // and moved the agents heading up and down as spaces came and went, which is a
    // heading a person has to find before they can read it.
    //
    // The share yields when the lower panel actually needs the room. That is the one that
    // grows, and a division holding its half while nine agents scrolled underneath would
    // be a place bought at the price of the thing it is a place for.
    let share = (height * SPACES_TOP_SHARE / 100).max(1);
    // Never less than what the spaces themselves need, plus room to breathe under them,
    // and never demanding more than the share on their behalf: the list scrolls inside
    // whatever it gets, so thirty spaces do not get to ask for thirty rows.
    let least = (1 + spaces + BREATHE)
        .min(share)
        .max(SPACES_FLOOR)
        .min(height);
    let most = height.saturating_sub(agents_wanted + 1).max(least);
    share.max(least).min(most).min(height)
}

/// The spaces panel: its title, then one row per space, scrolled to keep the focused one on
/// screen and padded out to the rows it was given.
fn spaces_panel(spaces: &[WorkspaceRow], focused: i64, height: usize) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(title(SPACES_TITLE), None)];
    // The last row of this panel is the two things that are not a space: starting one more
    // terminal, and everything else. They sit at the foot of the upper panel rather than in
    // a bar of their own, because the column has two panels and a third strip would be a
    // third place to look.
    let footer = usize::from(height > 2);
    let window = height - 1 - footer;
    let first = scrolled(
        spaces.len(),
        spaces.iter().position(|s| s.id == focused),
        window,
    );
    for offset in 0..window {
        out.push(match spaces.get(first + offset) {
            Some(space) => (
                Line {
                    // No state colour on the row: a workspace has no state to have one
                    // about. Its name carries its own colour instead, which is the same
                    // colour the agents panel puts on every agent's mark that lives here.
                    text: fit(&format!(" {} {}", mark(space.id == focused), space.name)),
                    colour: None,
                    accent: Some((
                        SPACE_NAME_AT,
                        crate::width::width(&space.name),
                        crate::theme::space_of(space.id),
                    )),
                },
                Some(Hit::Space(space.id)),
            ),
            None => blank(),
        });
    }
    if footer == 1 {
        out.push(footer_row());
    }
    out
}

/// The agents panel: its title, then two rows per agent, scrolled to keep the
/// focused one's first row on screen.
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
    if listed.is_empty() {
        // Said, rather than left as a void. An empty panel under a title reads as a panel
        // that is broken; an empty panel that says what would go in it reads as a panel
        // waiting for one.
        out.push((
            Line {
                text: fit(" no agents yet"),
                colour: Some(crate::theme::Role::Faint),
                accent: None,
            },
            None,
        ));
        while out.len() < height {
            out.push(blank());
        }
        return out;
    }
    // Two rows per agent, so the window is twice the rows there are. The scroll
    // keeps the focused agent's first row on screen with room for its second:
    // a focused pair split across the foot of the panel reads as one agent
    // becoming two.
    let mut content: Vec<(Line, Option<Hit>)> = Vec::new();
    for agent in &listed {
        push_agent_lines(&mut content, spaces, agent, focused);
    }
    let focus_row = content
        .iter()
        .position(|(_, hit)| *hit == Some(Hit::Terminal(focused)));
    let first = match focus_row {
        Some(i) if content.len() > body => i
            .saturating_sub(body.saturating_sub(2))
            .min(content.len() - body),
        _ => 0,
    };
    for offset in 0..body {
        match content.get(first + offset) {
            Some(row) => out.push(row.clone()),
            None => out.push(blank()),
        }
    }
    out.truncate(height);
    out
}

/// The two rows an open agent takes, with what a click on either means.
///
/// First the project and the tab, then the session underneath: `app · release`
/// above `claude`. Both rows reach the same terminal, so a click on either
/// lands on the agent. The space is always named on the first row, which is
/// what retired the shared-label suffix: with the space on every row there is
/// no shared label left to qualify.
///
/// One helper and not the same four pushes at each caller, because the unfolded
/// panel and the folded one have to draw the same rows for the same agent.
fn push_agent_lines(
    out: &mut Vec<(Line, Option<Hit>)>,
    spaces: &[WorkspaceRow],
    agent: &AgentRow,
    focused: u32,
) {
    let hit = Some(Hit::Terminal(agent.terminal));
    let tab = label(agent);
    let space = spaces.iter().find(|s| s.id == agent.workspace);
    let head = match space {
        Some(space) => format!(
            " {} {} \u{b7} {tab}",
            mark(agent.terminal == focused),
            space.name
        ),
        // A space the client has not heard of yet: the row still draws, under
        // the tab alone, rather than vanishing until the index catches up.
        None => format!(" {} {tab}", mark(agent.terminal == focused)),
    };
    // The state is cut before the names are, but never so far that the space
    // and some of the tab disappear: a row showing only a state does not say
    // whose state it is.
    let (space_cols, dot_cols) = match space {
        Some(space) => (crate::width::width(&space.name), 3),
        None => (0, 0),
    };
    let head_least = AGENT_NAME_AT + space_cols + dot_cols + MIN_NAME;
    let state = clip(
        shown_state(&agent.state),
        WIDTH.saturating_sub(head_least + 1),
    );
    let room = WIDTH.saturating_sub(crate::width::width(&state) + 1);
    out.push((
        Line {
            text: format!("{} {state}", fit_to(&head, room)),
            colour: colour_for(&agent.state),
            // Which space this agent lives in, as the colour of its mark. The mark's
            // shape says where the keyboard is, its colour says which space the rows
            // belong to, and neither needs a number beside it.
            accent: Some((1, 1, crate::theme::space_of(agent.workspace))),
        },
        hit,
    ));
    // The session underneath: what is actually running in the tab, in the quiet
    // colour, so the first row keeps the eye and the second keeps the answer.
    out.push((
        Line {
            text: fit_to(&format!("  {}", session_program(agent)), WIDTH),
            colour: Some(crate::theme::Role::Faint),
            accent: None,
        },
        hit,
    ));
}

/// What runs in the tab: the program's last path segment, because a column
/// this narrow renders `/opt/homebrew/bin/claude` as `/opt/homebre`. Never the
/// chosen name: that is already on the row above, and repeating it would cost
/// the one line that says what the session actually is.
fn session_program(agent: &AgentRow) -> String {
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

/// The state, or nothing when nothing has observed one.
///
/// An unknown state is drawn as nothing. It is a claim about nothing, so a column of the
/// word `unknown` trains the eye to skip the one column that matters when an agent really
/// is blocked.
fn shown_state(state: &str) -> &str {
    if state == UNKNOWN { "" } else { state }
}

/// Every row of the folded column, with what a click on it means.
///
/// Same contract as [`rows`]: [`render_folded`] and [`hit_folded`] share this one
/// function so a click lands on the row a person aimed at by construction.
/// Also shared with the screen painter, which needs the hits to mark the
/// keyboard's own rows: the text alone cannot say which row is selected.
pub(crate) fn rows_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    fold: &Fold,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = rows_open(
        spaces,
        agents,
        focused_space,
        focused_terminal,
        height,
        fold,
    );
    // The mark that folds the column away, at its foot, where the eye lands last.
    if let Some(last) = out.last_mut() {
        *last = collapse_row(false);
    }
    out
}

/// The column folded away to a strip: a state mark per row, and nothing else.
///
/// What a glance wants is colour and count, and both survive four columns. The names come
/// back with the column, and the terminal gets twenty six columns in the meantime.
pub(crate) fn rows_collapsed(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
) -> Vec<(Line, Option<Hit>)> {
    let mut out: Vec<(Line, Option<Hit>)> = Vec::new();
    for (at, space) in spaces.iter().enumerate() {
        if out.len() + 2 >= height {
            break;
        }
        out.push((
            Line {
                text: strip_space(mark(space.id == focused_space), at + 1),
                colour: None,
                accent: Some((1, 1, crate::theme::space_of(space.id))),
            },
            Some(Hit::Space(space.id)),
        ));
    }
    out.push((
        Line {
            text: " ".repeat(COLLAPSED),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
        },
        None,
    ));
    let listed: Vec<&AgentRow> = agents.iter().filter(|a| a.agent).collect();
    for agent in listed {
        if out.len() + 2 > height {
            break;
        }
        out.push((
            Line {
                text: strip(mark(agent.terminal == focused_terminal)),
                colour: colour_for(&agent.state),
                accent: Some((1, 1, crate::theme::space_of(agent.workspace))),
            },
            Some(Hit::Terminal(agent.terminal)),
        ));
    }
    while out.len() + 1 < height {
        out.push((
            Line {
                text: " ".repeat(COLLAPSED),
                colour: None,
                accent: None,
            },
            None,
        ));
    }
    if out.len() < height {
        out.push(collapse_row(true));
    }
    out.truncate(height);
    out
}

/// One row of the folded strip: the focus mark, in the space's colour.
fn strip(mark: char) -> String {
    let text = format!(" {mark}");
    let pad = COLLAPSED.saturating_sub(crate::width::width(&text));
    format!("{text}{}", " ".repeat(pad))
}

/// One row of the folded strip for a space: the focus mark and its index among
/// spaces. Spaces keep theirs; with no names on the strip the index is what
/// tells one space row from another.
fn strip_space(mark: char, n: usize) -> String {
    let text = format!(" {mark}{n}");
    let pad = COLLAPSED.saturating_sub(crate::width::width(&text));
    format!("{text}{}", " ".repeat(pad))
}

/// The row that folds the column away, and unfolds it again.
fn collapse_row(collapsed: bool) -> (Line, Option<Hit>) {
    let width = if collapsed { COLLAPSED } else { WIDTH };
    let mark = if collapsed { '\u{bb}' } else { '\u{ab}' };
    if collapsed {
        return (
            Line {
                text: format!("{}{mark} ", " ".repeat(width.saturating_sub(2))),
                colour: Some(crate::theme::Role::Faint),
                accent: None,
            },
            Some(Hit::Collapse),
        );
    }
    // The foot of the column says what this is and what version of it, the way a product
    // signs its own window, and the fold mark sits at the far end of the same row rather
    // than taking a row of its own: the column is worth more to a person as list than as
    // furniture.
    let brand = format!(" {BRAND_MARK} {BRAND}");
    let version = format!("{VERSION} ");
    // The row is the mark and name, a space, the version, the gap, then the fold mark and
    // its own trailing space.
    let gap =
        width.saturating_sub(crate::width::width(&brand) + 1 + crate::width::width(&version) + 2);
    (
        Line {
            text: format!("{brand} {version}{}{mark} ", " ".repeat(gap)),
            colour: Some(crate::theme::Role::Faint),
            // The mark and the name together, so the accent reads as one wordmark rather
            // than as a coloured bullet next to grey text.
            accent: Some((1, crate::width::width(BRAND) + 2, crate::theme::Role::Brand)),
        },
        Some(Hit::Collapse),
    )
}

/// The product's name, as it signs itself.
const BRAND: &str = "qoro";
/// The mark beside it. A filled dot, which every terminal font has, because a mark drawn
/// from a glyph half of them lack is a mark that renders as a box on the other half.
const BRAND_MARK: char = '\u{25cf}';
/// Which build this is, read from the crate rather than written down twice.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The column as it is when it is not folded away.
fn rows_open(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    fold: &Fold,
) -> Vec<(Line, Option<Hit>)> {
    // What the lower panel wants, asked before the column is divided. The division is a
    // place and not a consequence of how many spaces there happen to be, but a panel that
    // held its half while the list below it scrolled would be a place at the wrong price.
    let (content, shown, total) = agents_content(spaces, agents, focused_terminal, fold);
    let split = spaces_rows(spaces.len(), height, content.len() + 1);
    let mut out = spaces_panel(spaces, focused_space, split);
    let rest = height - split;
    if rest >= 2 {
        out.push(divider());
        out.extend(lay_out_agents(
            content,
            shown,
            total,
            focused_terminal,
            rest - 1,
        ));
    } else {
        out.extend(lay_out_agents(
            content,
            shown,
            total,
            focused_terminal,
            rest,
        ));
    }
    out
}

/// The agents panel with quiet groups folded into one counted line each.
///
/// Open agents draw exactly the rows [`agents_panel`] draws for them, so an
/// expanded panel is the panel a person already knows. What changes is only that
/// a closed group is one line carrying its count, and a click on it opens it.
/// The group holding the focused terminal is always open: hiding the keyboard's
/// own row behind a fold would read as a terminal that died.
fn agents_content(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused: u32,
    fold: &Fold,
) -> (Vec<(Line, Option<Hit>)>, usize, usize) {
    let listed: Vec<&AgentRow> = agents.iter().filter(|a| a.agent).collect();
    if listed.is_empty() {
        // Said, rather than left as a void, for the reason the unfolded panel says it.
        return (
            vec![(
                Line {
                    text: fit(" no agents yet"),
                    colour: Some(crate::theme::Role::Faint),
                    accent: None,
                },
                None,
            )],
            0,
            0,
        );
    }
    let focused_state = listed
        .iter()
        .find(|a| a.terminal == focused)
        .map(|a| a.state.as_str());
    // A group is open when the fold says so, or when the keyboard is inside it.
    let open = |group: Group| {
        if focused_state.is_some_and(|s| group_of(s) == group) {
            true
        } else {
            fold.is_open(group)
        }
    };
    // What needs a person comes first, in daemon order, then the rest that is open,
    // in daemon order. Daemon order within each half, so a short list draws exactly
    // the rows the unfolded panel draws.
    let mut content: Vec<(Line, Option<Hit>)> = Vec::new();
    let mut shown = 0;
    let needs: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| needs_you(&a.state))
        .copied()
        .collect();
    let fresh: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| !needs_you(&a.state) && unobserved(&a.state))
        .copied()
        .collect();
    let running: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| {
            !needs_you(&a.state)
                && !unobserved(&a.state)
                && group_of(&a.state) == Group::Running
                && open(Group::Running)
        })
        .copied()
        .collect();
    let finished: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| {
            !needs_you(&a.state)
                && !unobserved(&a.state)
                && group_of(&a.state) == Group::Finished
                && open(Group::Finished)
        })
        .copied()
        .collect();
    let idle: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| {
            !needs_you(&a.state)
                && !unobserved(&a.state)
                && group_of(&a.state) == Group::Idle
                && open(Group::Idle)
        })
        .copied()
        .collect();
    // An open group carries its own header with its count, so nine rows read as
    // labelled groups rather than nine identical rows. A group that has just
    // arrived and not been observed yet draws bare: it is news, not a group.
    if !needs.is_empty() {
        content.push(group_header(
            "needs you",
            needs.len(),
            colour_for("blocked"),
        ));
    }
    for agent in needs {
        push_agent_lines(&mut content, spaces, agent, focused);
        shown += 1;
    }
    for agent in fresh {
        push_agent_lines(&mut content, spaces, agent, focused);
        shown += 1;
    }
    if !running.is_empty() {
        content.push(group_header(
            "running",
            running.len(),
            colour_for("working"),
        ));
    }
    for agent in running {
        push_agent_lines(&mut content, spaces, agent, focused);
        shown += 1;
    }
    if !finished.is_empty() {
        content.push(group_header("finished", finished.len(), colour_for("done")));
    }
    for agent in finished {
        push_agent_lines(&mut content, spaces, agent, focused);
        shown += 1;
    }
    if !idle.is_empty() {
        // Quiet, like the panel titles. Every other group header is the colour of what it
        // holds, and idle holds the things that want nothing, so a header painted as
        // ordinary text would be the loudest calm thing in the column.
        content.push(group_header(
            "idle",
            idle.len(),
            Some(crate::theme::Role::Title),
        ));
    }
    for agent in idle {
        push_agent_lines(&mut content, spaces, agent, focused);
        shown += 1;
    }
    for group in [Group::Finished, Group::Idle] {
        let n = listed
            .iter()
            .filter(|a| {
                !needs_you(&a.state)
                    && !unobserved(&a.state)
                    && group_of(&a.state) == group
                    && !open(group)
            })
            .count();
        if n > 0 && !open(group) {
            content.push(fold_line(group, n));
        }
    }
    let _ = focused;
    (content, shown, listed.len())
}

/// One built agents panel, scrolled and padded into the rows it was given.
fn lay_out_agents(
    content: Vec<(Line, Option<Hit>)>,
    shown: usize,
    total: usize,
    focused: u32,
    height: usize,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(folded_title(shown, total), None)];
    let body = height - 1;
    if body == 0 {
        return out;
    }
    // Flat scroll keeping the focused agent's first line visible with room for
    // its second. Fold lines ride along at the end rather than being pinned:
    // pinning them is a second scroll region and a second chance for a click
    // to disagree with a paint.
    let focus_row = content
        .iter()
        .position(|(_, hit)| *hit == Some(Hit::Terminal(focused)));
    let first = match focus_row {
        Some(i) if content.len() > body => i
            .saturating_sub(body.saturating_sub(2))
            .min(content.len() - body),
        _ => 0,
    };
    for offset in 0..body {
        match content.get(first + offset) {
            Some(row) => out.push(row.clone()),
            None => out.push(blank()),
        }
    }
    out.truncate(height);
    out
}

/// One open group's title: its name and how many agents are under it.
///
/// A header with nothing under it is a promise the panel does not keep, so the
/// caller only pushes one for a group that drew at least one agent. The count
/// sits flush right, where the agent rows carry their states, so the column
/// keeps one edge the eye can scan.
fn group_header(
    title: &str,
    count: usize,
    colour: Option<crate::theme::Role>,
) -> (Line, Option<Hit>) {
    let count = count.to_string();
    let left = fit_to(&format!(" {title}"), WIDTH.saturating_sub(count.len() + 1));
    (
        Line {
            text: format!("{left} {count}"),
            colour,
            accent: None,
        },
        None,
    )
}

/// One collapsed group: its count, and what a click on it does.
fn fold_line(group: Group, count: usize) -> (Line, Option<Hit>) {
    (
        Line {
            text: fit(&format!(" \u{25b8} {count} {}", group.name())),
            colour: Some(crate::theme::Role::Faint),
            accent: None,
        },
        Some(Hit::Fold(group)),
    )
}

/// The agents title, carrying how much of the list is on screen.
///
/// The full list draws the plain title, so an expanded panel is byte for byte the
/// panel it folds from.
fn folded_title(shown: usize, total: usize) -> Line {
    if shown == total {
        title(AGENTS_TITLE)
    } else {
        Line {
            text: fit(&format!(" {AGENTS_TITLE} {shown} of {total}")),
            colour: Some(crate::theme::Role::Title),
            accent: None,
        }
    }
}

/// What to call the program in the terminal.
///
/// A name a person chose outlives the program: a tab called `release` stays `release`
/// while it runs a shell, then cargo, then an agent. The tab bar already spells it this
/// way, and the panel spells it the same, so the two halves of the screen name the same
/// terminal the same way. Without a chosen name the program's last path segment answers,
/// because a column this narrow renders `/opt/homebrew/bin/claude` as `/opt/homebre`,
/// which identifies nothing. A wrapper is announced as a wrapper: a person shown
/// `docker` would conclude their agent is called docker.
fn label(agent: &AgentRow) -> String {
    if let Some(name) = agent.name.as_deref().filter(|n| !n.is_empty()) {
        return name.to_string();
    }
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

/// The row at the foot of the spaces panel: `new` on the left, `menu` on the right.
///
/// One row for the two things that are neither a space nor an agent. Which half a click
/// lands on decides which it meant, and the halves are the two words themselves rather than
/// the halves of the column: a click on the empty middle is a click on neither.
fn footer_row() -> (Line, Option<Hit>) {
    let left = format!(" {NEW_LABEL}");
    let right = format!("{MENU_LABEL} ");
    let gap = WIDTH.saturating_sub(crate::width::width(&left) + crate::width::width(&right));
    (
        Line {
            text: format!("{left}{}{right}", " ".repeat(gap)),
            colour: Some(crate::theme::Role::Faint),
            accent: None,
        },
        // The row itself answers nothing: the caller asks again with the column.
        None,
    )
}

/// What a click at one column of the footer row means.
pub fn footer_hit(col: usize) -> Option<Hit> {
    if col <= crate::width::width(NEW_LABEL) {
        return Some(Hit::New);
    }
    if col + 1 >= WIDTH.saturating_sub(crate::width::width(MENU_LABEL)) {
        return Some(Hit::Menu);
    }
    None
}

const NEW_LABEL: &str = "new";
const MENU_LABEL: &str = "menu";

/// A panel title, which is a row like any other and therefore the full width.
fn title(text: &str) -> Line {
    Line {
        text: fit(&format!(" {text}")),
        colour: Some(crate::theme::Role::Title),
        accent: None,
    }
}

/// The rule between the spaces panel and the agents panel: one row of box
/// drawing, clickable as nothing, so a click on it does nothing rather than
/// something arbitrary.
fn divider() -> (Line, Option<Hit>) {
    (
        Line {
            text: "\u{2500}".repeat(WIDTH),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
        },
        None,
    )
}

/// A row with nothing on it, and nothing to do when it is clicked.
fn blank() -> (Line, Option<Hit>) {
    (
        Line {
            text: " ".repeat(WIDTH),
            colour: None,
            accent: None,
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
///
/// Not [`crate::width::fit`], which also drops the space a grid puts after a full width
/// character to hold its second column. These rows are text this module composed, so a
/// space in one is a space somebody meant: measured, an agent whose name was two full width
/// characters had a pad space eaten and its state stopped a column short of the right edge,
/// and a space with a wide name lost the space in front of the separator.
fn fit_to(text: &str, cols: usize) -> String {
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

/// Cut to at most `cols` columns without padding, for the piece that is already flush
/// against the right edge.
fn clip(text: &str, cols: usize) -> String {
    fit_to(text, cols).trim_end().to_string()
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
            name: None,
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
            name: None,
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
        // Terminal 3 is the focused agent: the mark, its space, then its label.
        // Which space it lives in is said twice, once by the colour on its
        // mark and once by the word beside it: the row answers "where" before
        // it answers "what".
        assert!(
            marked
                .iter()
                .any(|t| t.starts_with(&format!(" {FOCUSED} personal \u{b7} codex"))),
            "{marked:?}"
        );
    }

    #[test]
    fn an_agent_shows_its_space_then_its_program_and_state() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        let head = texts
            .iter()
            .position(|t| t.starts_with(&format!(" {FOCUSED} app \u{b7} claude")))
            .unwrap_or_else(|| panic!("{texts:?}"));
        // Two rows, and together they carry everything: the mark, the space,
        // the program, and the state flush against the right edge of the first.
        assert_eq!(texts[head], " \u{25cf} app \u{b7} claude        working");
        assert_eq!(texts[head + 1], "  claude");
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
    fn an_agent_row_carries_the_colour_of_its_state_and_a_title_carries_no_state() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        // A title has a look of its own, and it is never a state's: a panel heading that
        // came out the colour of `blocked` would be a heading somebody answers.
        assert_eq!(
            lines[0].colour,
            Some(crate::theme::Role::Title),
            "the spaces title"
        );
        let coloured: Vec<Option<crate::theme::Role>> = lines
            .iter()
            .filter(|l| l.text.contains("claude"))
            .map(|l| l.colour)
            .collect();
        // Two rows for the one agent: the first wears the state's colour, the
        // session underneath stays quiet.
        assert_eq!(
            coloured,
            vec![colour_for("working"), Some(crate::theme::Role::Faint)]
        );
        // A space row has no state to have a colour about; it wears its own instead, as an
        // accent on its name rather than on the whole row.
        assert!(
            lines
                .iter()
                .filter(|l| l.text.contains("personal"))
                .all(|l| l.colour.is_none() && l.accent.is_some())
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
        // Thirty spaces cannot take the whole column: the panel below them is the one that
        // grows, so the short list is held to its share and scrolls inside it.
        assert_eq!(agents_title, 14);
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
                    // Everything left is one agent's two rows: the first names
                    // the space and the tab, the second the session under it.
                    // Both reach the same terminal.
                    agents
                        .iter()
                        .find(|a| {
                            let space = spaces
                                .iter()
                                .find(|s| s.id == a.workspace)
                                .map(|s| s.name.as_str())
                                .unwrap_or("");
                            let first = if space.is_empty() {
                                format!(" {} {}", mark(a.terminal == 3), label(a))
                            } else {
                                format!(" {} {} \u{b7} {}", mark(a.terminal == 3), space, label(a))
                            };
                            text.starts_with(&first)
                                || (text.starts_with("  ")
                                    && text.trim_end() == format!("  {}", session_program(a)))
                        })
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
    fn an_agent_in_a_space_the_client_has_not_heard_of_still_draws_its_row() {
        let agents = vec![agent(4, 77, "codex", "blocked")];
        let lines = render(&[], &agents, 1, 4, 8);
        assert!(
            lines.iter().any(|l| l.text.starts_with(" \u{25cf} codex")),
            "{:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    fn named(terminal: u32, workspace: i64, name: &str, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: Some(name.to_string()),
            workspace,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: None,
        }
    }

    fn agent_texts(spaces: &[WorkspaceRow], agents: &[AgentRow], focused: u32) -> Vec<String> {
        render(spaces, agents, 1, focused, 20)
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect()
    }

    /// The user's panel: four rows reading `opencode` identify nothing. The space
    /// is named on every first row, so each pair says which one needs them.
    #[test]
    fn agents_sharing_a_label_carry_their_space_names() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 2, "opencode", "idle"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        assert!(
            texts.iter().any(|t| t.contains("app \u{b7} opencode")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("personal \u{b7} opencode")),
            "{texts:?}"
        );
    }

    /// Every first row names its space: the mark's colour says where it lives,
    /// and the word beside it says it again, so no label ever stands on its
    /// colour alone.
    #[test]
    fn a_label_nobody_shares_still_names_its_space() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 2, "opencode", "idle"),
            agent(3, 1, "claude", "idle"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        assert!(
            texts.iter().any(|t| t.contains("app \u{b7} claude")),
            "{texts:?}"
        );
        // And the session underneath still says what runs in the tab.
        assert!(texts.iter().any(|t| *t == "  claude"), "{texts:?}");
    }

    /// A chosen name picked twice in two spaces is still two pairs that cannot
    /// be told from one another, so each first row names its space.
    #[test]
    fn a_chosen_name_shared_by_two_agents_names_both_spaces() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            named(1, 1, "release", "opencode", "idle"),
            named(2, 2, "release", "claude", "idle"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        assert!(
            texts.iter().any(|t| t.contains("app \u{b7} release")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("personal \u{b7} release")),
            "{texts:?}"
        );
        // And each session underneath still says what runs in its tab.
        assert!(texts.iter().any(|t| *t == "  opencode"), "{texts:?}");
        assert!(texts.iter().any(|t| *t == "  claude"), "{texts:?}");
    }

    /// Two tabs of one space share the space's name as well as the program, and
    /// still tell apart: their states differ, and each pair keeps its own hits.
    #[test]
    fn two_tabs_of_one_space_share_the_space_name_but_not_the_state() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 1, "opencode", "blocked"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        let first = texts
            .iter()
            .find(|t| t.contains("app \u{b7} opencode") && t.ends_with("idle"))
            .unwrap_or_else(|| panic!("{texts:?}"));
        let second = texts
            .iter()
            .find(|t| t.contains("app \u{b7} opencode") && t.ends_with("blocked"))
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert_ne!(first, second);
        // Both pairs reach their own terminal, first row and session row alike.
        let hits: Vec<Option<Hit>> = (0..20)
            .map(|row| hit(&spaces, &agents, 1, 1, 20, row))
            .collect();
        assert!(hits.contains(&Some(Hit::Terminal(1))));
        assert!(hits.contains(&Some(Hit::Terminal(2))));
    }

    #[test]
    fn a_wrapper_is_shown_as_standing_in_the_way_and_never_as_the_program() {
        let agents = vec![AgentRow {
            terminal: 5,
            name: None,
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
            name: None,
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

    /// The panel composes its own rows, so every space in one was put there deliberately.
    /// Cutting them with the grid's rule, where a space after a full width character is a
    /// spacer the engine added, eats padding this module meant and slides the right hand
    /// column one place left.
    #[test]
    fn a_wide_name_does_not_cost_the_row_a_space_the_panel_put_there() {
        let agents = vec![agent(1, 1, "\u{4e2d}\u{4e2d}", "idle")];
        let lines = render(&[space(1, "\u{4e2d}\u{6587}")], &agents, 1, 1, 8);
        // The space's own row: the pad after its name is one this module wrote, and the
        // grid's rule would have eaten it.
        let space_row = lines
            .iter()
            .find(|l| l.text.contains('\u{6587}'))
            .unwrap_or_else(|| panic!("no space row"));
        assert_eq!(crate::width::width(&space_row.text), WIDTH);
        let row = lines
            .iter()
            .find(|l| l.text.contains("idle"))
            .unwrap_or_else(|| panic!("no agent row"));
        // The state is still flush against the right edge.
        assert!(row.text.ends_with("idle"), "{:?}", row.text);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    #[test]
    fn an_absurdly_long_state_is_cut_but_still_leaves_some_of_the_program() {
        let agents = vec![agent(1, 1, "claude", &"z".repeat(80))];
        let lines = render(&[space(1, "app")], &agents, 1, 1, 8);
        let row = lines
            .iter()
            .find(|l| l.text.contains('z'))
            .unwrap_or_else(|| panic!("no agent row"));
        assert_eq!(crate::width::width(&row.text), WIDTH);
        // The mark, the space and some of the program survive the cut: a row
        // showing only a state does not say whose state it is.
        assert!(
            row.text.starts_with(" \u{25cf} app \u{b7} cla"),
            "{:?}",
            row.text
        );
    }

    fn many_agents() -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
        (
            vec![space(1, "app"), space(2, "webshop")],
            vec![
                agent(1, 1, "codex", "blocked"),
                agent(2, 2, "claude", "failed"),
                agent(3, 1, "claude", "working"),
                agent(4, 2, "claude", "working"),
                agent(5, 1, "claude", "done"),
                agent(6, 1, "claude", "idle"),
                agent(7, 2, "claude", "idle"),
                agent(8, 2, "claude", "idle"),
                agent(9, 1, "claude", "idle"),
            ],
        )
    }

    #[test]
    fn folded_needs_you_and_running_are_open_and_the_rest_is_two_lines() {
        let (spaces, agents) = many_agents();
        let fold = Fold::default_closed();
        let lines = render_folded(&spaces, &agents, 2, 1, 30, &fold);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // Needs-you and running agents are drawn as usual, each under its own
        // header carrying its count flush right.
        assert!(texts.iter().any(|t| t.contains("codex")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("working")), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("needs you") && t.trim_end().ends_with("2")),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("running") && t.trim_end().ends_with("2")),
            "{texts:?}"
        );
        // Finished and idle are one counted line each.
        assert!(texts.iter().any(|t| t.contains("1 finished")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("4 idle")), "{texts:?}");
        // Four agents open, so eight rows: each agent's pair, needs-you first,
        // then running.
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            8,
            "{texts:?}"
        );
        // The title says how much is shown: 4 agents of 9.
        assert!(texts.iter().any(|t| *t == " agents 4 of 9"), "{texts:?}");
        for line in &lines {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
    }

    #[test]
    fn folded_an_idle_agent_collapses_while_a_working_one_stays() {
        let (spaces, agents) = sample();
        let fold = Fold::default_closed();
        let folded = render_folded(&spaces, &agents, 1, 1, 14, &fold);
        let texts: Vec<String> = folded
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // The working agent keeps its rows, the idle one becomes one line.
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("1 idle")), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("codex")),
            "the idle agent's own rows are still drawn: {texts:?}"
        );
    }

    #[test]
    fn folded_an_empty_group_leaves_no_line() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "blocked")];
        let lines = render_folded(&spaces, &agents, 1, 1, 10, &Fold::default_closed());
        // The blocked agent draws two rows under its own header and nothing else:
        // no fold lines for groups with nobody in them, and the plain title.
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("needs you") && t.trim_end().ends_with("1")),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("idle")), "{texts:?}");
        assert!(texts.iter().any(|t| *t == " agents"), "{texts:?}");
    }

    #[test]
    fn folded_a_click_on_a_fold_line_opens_that_group() {
        let (spaces, agents) = many_agents();
        let fold = Fold::default_closed();
        let height = 30;
        let row = render_folded(&spaces, &agents, 2, 1, height, &fold)
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .position(|t| t.contains("4 idle"))
            .expect("no idle fold line");
        assert_eq!(
            hit_folded(&spaces, &agents, 2, 1, height, &fold, row),
            Some(Hit::Fold(Group::Idle))
        );
        let mut opened = fold;
        opened.open(Group::Idle);
        let texts: Vec<String> = render_folded(&spaces, &agents, 2, 1, height, &opened)
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(
            !texts.iter().any(|t| t.contains("4 idle")),
            "the fold line is still there: {texts:?}"
        );
        assert!(texts.iter().any(|t| *t == " agents 8 of 9"), "{texts:?}");
    }

    #[test]
    fn folded_the_group_holding_the_keyboard_is_open_even_when_folded() {
        let (spaces, agents) = many_agents();
        // Terminal 5 is done and would fold, but it has the keyboard.
        let lines = render_folded(&spaces, &agents, 1, 5, 30, &Fold::default_closed());
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains(" agents 5 of 9")),
            "{texts:?}"
        );
        // And a click on its rows still reaches it.
        let hits: Vec<Option<Hit>> = (0..30)
            .map(|row| hit_folded(&spaces, &agents, 1, 5, 30, &Fold::default_closed(), row))
            .collect();
        assert!(hits.contains(&Some(Hit::Terminal(5))));
    }

    #[test]
    fn folded_hit_returns_what_folded_render_drew_on_every_row() {
        let (spaces, agents) = many_agents();
        for fold in [Fold::default_closed(), Fold::all_open()] {
            let lines = render_folded(&spaces, &agents, 2, 1, 20, &fold);
            assert_eq!(lines.len(), 20);
            for (row, line) in lines.iter().enumerate() {
                let got = hit_folded(&spaces, &agents, 2, 1, 20, &fold, row);
                let text = line.text.trim_end().to_string();
                match got {
                    Some(Hit::Fold(group)) => {
                        assert!(text.contains(group.name()), "row {row}: {text:?}");
                    }
                    Some(Hit::Terminal(t)) => {
                        assert!(
                            text.contains("claude") || text.contains("codex"),
                            "row {row}: {text:?} for terminal {t}"
                        );
                    }
                    Some(Hit::Space(_) | Hit::New | Hit::Menu | Hit::Collapse) | None => {}
                }
            }
            assert_eq!(hit_folded(&spaces, &agents, 2, 1, 20, &fold, 20), None);
        }
    }

    #[test]
    fn folded_an_unobserved_agent_stays_on_screen() {
        // Unknown is not idleness: it is "not looked at yet", and folding it hides
        // every arrival until detection settles.
        let agents = vec![AgentRow {
            terminal: 7,
            name: None,
            workspace: 2,
            agent: true,
            program: Some("claude".to_string()),
            behind: None,
            state: UNKNOWN.to_string(),
            generation: None,
        }];
        let lines = render_folded(
            &[space(1, "app"), space(2, "webshop")],
            &agents,
            1,
            1,
            10,
            &Fold::default_closed(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("idle")),
            "the unobserved agent folded: {texts:?}"
        );
    }

    #[test]
    fn toggle_all_opens_everything_and_then_folds_again() {
        let mut fold = Fold::default_closed();
        fold.toggle_all();
        assert_eq!(fold, Fold::all_open());
        let (spaces, agents) = many_agents();
        let texts: Vec<String> = render_folded(&spaces, &agents, 2, 1, 30, &fold)
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts.iter().any(|t| *t == " agents"),
            "a fully open panel carries the plain title: {texts:?}"
        );
        fold.toggle_all();
        assert_eq!(fold, Fold::default_closed());
    }

    /// The foot of the column signs the product, and still folds it away.
    #[test]
    fn the_foot_of_the_column_carries_the_mark_the_name_and_the_version() {
        let (line, hit) = collapse_row(false);
        assert_eq!(
            crate::width::width(&line.text),
            WIDTH,
            "the row has to fill the column exactly or the one under it shifts: {:?}",
            line.text
        );
        assert!(line.text.contains(BRAND), "{:?}", line.text);
        assert!(line.text.contains(BRAND_MARK), "{:?}", line.text);
        assert!(
            line.text.contains(VERSION),
            "a person reporting a bug has to be able to read the version off the screen: {:?}",
            line.text
        );
        assert!(
            line.text.contains('\u{ab}'),
            "the fold mark shares the row rather than losing it: {:?}",
            line.text
        );
        assert_eq!(hit, Some(Hit::Collapse), "and clicking the row still folds");
        // The mark and the name wear the accent, and nothing else on the row does.
        let (at, len, role) = line.accent.expect("a wordmark is accented");
        assert_eq!(role, crate::theme::Role::Brand);
        let painted: String = line.text.chars().skip(at).take(len).collect();
        assert_eq!(painted, format!("{BRAND_MARK} {BRAND}"), "{:?}", line.text);

        // Folded to a strip there is no room for a name, and the fold mark is the row.
        let (strip, _) = collapse_row(true);
        assert_eq!(crate::width::width(&strip.text), COLLAPSED);
        assert!(!strip.text.contains(BRAND));
    }
}

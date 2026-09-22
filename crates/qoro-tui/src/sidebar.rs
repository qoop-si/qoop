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
//! An agent's first row opens with its state dot, painted in the state's
//! own colour through [`Line::indicator`]. The caller paints the raised
//! background over the keyboard's rows, so the dot never marks selection:
//! a screenshot, a test assertion, a pipe into a file and a terminal with
//! no colour all read the same static rows.

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
    /// The row's opening sign: the state dot at the start of an agent's
    /// first row, in the state's own colour. `None` on every other row:
    /// selection is the raised background the painter adds, never a mark.
    pub indicator: Option<(usize, usize, crate::theme::Role)>,
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

/// The column a space's name starts at in the spaces panel: one leading space.
/// No focus mark: the keyboard's row is the raised background alone, so a
/// screenshot, a test assertion and a pipe into a file all answer the same.
const SPACE_NAME_AT: usize = 1;

/// The state dot at the start of an agent's first row: one leading space,
/// then the dot. The dot says the state in the state's own colour, so no
/// state word is drawn on the right. Selection stays the raised background
/// the painter adds, never a second dot.
const DOT: char = '\u{25cf}';
/// The column the state dot sits at: one leading space.
const DOT_AT: usize = 1;
/// Columns the state dot covers: one glyph, one column.
const DOT_LEN: usize = 1;

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
    /// The `+` at the corner of the spaces title: a space, named first.
    NewSpace,
    /// The `menu` at the foot of the spaces panel: everything that is not a space, a tab or an agent.
    Menu,
    /// The mark at the foot of the column: fold the whole column away, and back.
    Collapse,
}

/// How the agents panel orders its rows: daemon order with what needs a
/// person first, or flat by urgency.
///
/// Grouped is the panel a person already knows. Priority is the same rows
/// sorted so the one that needs a person is always on top: the inbox view of
/// the fleet. Both are flat lists: the state dot carries the state, and no
/// header tree sits above calm rows. The title names the order. Toggled with
/// `ctrl-a g`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Grouped,
    Priority,
}

impl View {
    /// The other one: a toggle carries no state of its own.
    pub fn toggle(self) -> View {
        match self {
            View::Grouped => View::Priority,
            View::Priority => View::Grouped,
        }
    }
}

/// The done rows a person has already looked at, as (terminal, generation).
///
/// Client-local, like the focus itself: one client reading a done agent does
/// not mark it read for another. The generation travels with the terminal so
/// a new agent reusing a terminal number does not inherit the old one's
/// read receipt. Pruned by the caller when rows arrive; this module only reads.
pub type Seen = std::collections::BTreeSet<(u32, u64)>;

/// Rows that finished while the keyboard was away, as (terminal, generation).
///
/// For one event only: `done` is daemon-reported, but a screen
/// agent that goes working to idle while unfocused reads as plain `idle`
/// without this. The daemon never produces it, the client derives it from the
/// transition, and it stays client-local like `Seen`. Colours and headers
/// stay qoro's own; only the distinction is shared.
pub type Unseen = std::collections::BTreeSet<(u32, u64)>;

/// The generation that identifies this row's agent, or zero when the daemon
/// predates generations.
pub fn gen_of(agent: &AgentRow) -> u64 {
    agent.generation.unwrap_or(0)
}

/// Whether this done row is news: finished, and the keyboard has not been
/// here since. The focused row is never unread: standing on it is reading it.
pub fn is_unread(agent: &AgentRow, focused: u32, seen: &Seen) -> bool {
    agent.state == "done"
        && agent.terminal != focused
        && !seen.contains(&(agent.terminal, gen_of(agent)))
}

/// Whether this row is news, daemon or derived.
///
/// A daemon `done` nobody looked at, or an idle or done row that finished
/// while the keyboard was away. The focused row is never new: standing on it
/// is reading it. Same sage dot as `done`, no new theme; the folded panel
/// lists it under its own open `new` header.
pub fn is_new(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> bool {
    if agent.terminal == focused {
        return false;
    }
    if is_unread(agent, focused, seen) {
        return true;
    }
    matches!(agent.state.as_str(), "idle" | "done")
        && unseen.contains(&(agent.terminal, gen_of(agent)))
}

/// Whether the tab bar should mark this terminal as freshly finished.
///
/// Tabs carry no `Seen`: a daemon `done` and a derived idle-after-work both
/// read as the done glyph until focused. Same glyph as `done`, no new colour.
pub fn tab_is_new(agent: &AgentRow, focused: u32, unseen: &Unseen) -> bool {
    agent.terminal != focused
        && matches!(agent.state.as_str(), "idle" | "done")
        && unseen.contains(&(agent.terminal, gen_of(agent)))
}

/// Where a row sorts in the priority view: what wants a person first, then
/// what is alive, then what is settled. Daemon order breaks ties, so the view
/// is a stable sort of the grouped one rather than a second list.
fn priority_of(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> u8 {
    match agent.state.as_str() {
        "blocked" => 0,
        "failed" => 1,
        _ if is_new(agent, focused, seen, unseen) => 2,
        "working" => 3,
        "starting" => 4,
        "done" => 5,
        "idle" => 6,
        _ => 7,
    }
}

/// Whether the animation tick has anything to move.
///
/// Nothing: the panel is fully static. Every state is its dot at the
/// start of the first row, an unread done keeps the done sage on its dot,
/// and no tick ever repaints. This is what keeps the loop's founding rule
/// (no wake without an arrival) and stops the full-screen flicker the
/// spinner's eight frames a second caused.
pub fn wants_tick(_rows: &[AgentRow], _focused: u32, _seen: &Seen, _unseen: &Unseen) -> bool {
    false
}

/// Whether the state reads as running: working or starting.
///
/// Blocked and failed never reach this: the caller lifts them into the
/// needs-you section first.
fn is_running(state: &str) -> bool {
    matches!(state, "working" | "starting")
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

/// Both panels of the left column, painted into `height` rows.
///
/// Same contract as [`render`]: exactly `height` lines of exactly [`WIDTH`]
/// columns. One flat list: every agent draws its rows, nothing folds and no
/// group header sits above calm rows.
///
/// Eight arguments and not a context struct: every caller already holds each
/// piece separately, and a struct would move the same list one line down.
#[allow(clippy::too_many_arguments)]
pub fn render_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    tick: u64,
    view: View,
    seen: &Seen,
    unseen: &Unseen,
) -> Vec<Line> {
    rows_folded(
        spaces,
        agents,
        focused_space,
        focused_terminal,
        height,
        tick,
        view,
        seen,
        unseen,
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

/// What a click on one screen row of the column means, counted from zero.
///
/// Eight arguments, like [`render_folded`]: same pieces, same reason.
#[allow(clippy::too_many_arguments)]
pub fn hit_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    row: usize,
    view: View,
    seen: &Seen,
    unseen: &Unseen,
) -> Option<Hit> {
    rows_folded(
        spaces,
        agents,
        focused_space,
        focused_terminal,
        height,
        // Hit testing does not depend on the animation frame.
        0,
        view,
        seen,
        unseen,
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
///
/// The `+` at the title's right corner makes a space directly: the same
/// naming box `ctrl-a w` opens, without the key first.
fn spaces_panel(spaces: &[WorkspaceRow], focused: i64, height: usize) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(spaces_title(), None)];
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
                    // colour the agents panel puts on the space name there.
                    // No focus mark: the selected row is the raised background
                    // alone, so there is no hollow circle on every row and no
                    // filled white dot on the selected one.
                    text: fit(&format!(" {}", space.name)),
                    colour: None,
                    accent: Some((
                        SPACE_NAME_AT,
                        crate::width::width(&space.name),
                        crate::theme::space_of(space.id),
                    )),
                    indicator: None,
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
                indicator: None,
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
        // The unfolded panel does not animate: it is the test-facing surface,
        // and frame zero is the spinner's opening frame. The live folded panel
        // below spends the real tick.
        push_agent_lines(
            &mut content,
            spaces,
            agent,
            focused,
            0,
            &Seen::new(),
            &Unseen::new(),
        );
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
/// First the session, then where it lives: ` ● abc123ef` above
/// `release - app`. Both rows reach the same terminal, so a click on either
/// lands on the agent.
///
/// Static: the state is the dot at the start of the first row, in the
/// state's own colour, and no state word is drawn on the right. An unread
/// done keeps the done sage on its dot. `tick` is ignored so a still panel
/// is byte for byte the panel it was. The keyboard's row is the raised
/// background the painter adds; no filled dot ever marks selection.
///
/// One helper and not the same pushes at each caller, because the unfolded
/// panel and the folded one have to draw the same rows for the same agent.
fn push_agent_lines(
    out: &mut Vec<(Line, Option<Hit>)>,
    spaces: &[WorkspaceRow],
    agent: &AgentRow,
    focused: u32,
    _tick: u64,
    seen: &Seen,
    unseen: &Unseen,
) {
    let hit = Some(Hit::Terminal(agent.terminal));
    let tab = label(agent);
    let space = spaces.iter().find(|s| s.id == agent.workspace);
    // Line one names the session: the agent's own conversation id, and
    // nothing else. The program never stands beside it: four rows reading
    // `opencode` identify nothing, and the tab below already says what runs
    // where. Static: the dot carries the state so the name keeps the row.
    let session = session_name(agent);
    let fresh = is_new(agent, focused, seen, unseen);
    // A derived idle-after-work keeps the done sage: it is news, not calm.
    let dot = if fresh {
        crate::theme::of("done")
    } else {
        crate::theme::of(&agent.state)
    };
    out.push((
        Line {
            text: fit(&format!(" {DOT} {session}")),
            colour: None,
            // The space lives on the second row now, so the first row carries
            // no space accent: the keyboard's row is the raised background.
            accent: None,
            indicator: Some((DOT_AT, DOT_LEN, dot)),
        },
        hit,
    ));
    // Where it lives, underneath: the tab and the space, in the quiet
    // colour, so the first row keeps the eye and the second keeps the place.
    // A space the client has not heard of yet still draws, under the tab
    // alone, rather than vanishing until the index catches up. The space keeps
    // its colour here, so it still wears one colour in both panels.
    let (place, place_accent) = match space {
        Some(space) => (
            format!("{tab} - {}", space.name),
            Some((
                2 + crate::width::width(&tab) + 3,
                crate::width::width(&space.name),
                crate::theme::space_of(agent.workspace),
            )),
        ),
        None => (tab.clone(), None),
    };
    out.push((
        Line {
            text: fit_to(&format!("  {place}"), WIDTH),
            colour: Some(crate::theme::Role::Faint),
            accent: place_accent,
            indicator: None,
        },
        hit,
    ));
}

/// What the first row names: the agent's own conversation title, and only
/// that. The title is what the agent calls the conversation, and it follows
/// renames on its own because the daemon re-resolves it while the
/// conversation runs. Without a title the shortened conversation id answers,
/// and without that the tab's own label, which prefers a chosen name over
/// whatever happens to be running. No program beside any of them: the row
/// below already says what runs in which tab, and a program prefix is how
/// four live sessions all read `opencode`.
fn session_name(agent: &AgentRow) -> String {
    if let Some(title) = agent.agent_title.as_deref() {
        let title = title.trim();
        if !title.is_empty() {
            return title.to_string();
        }
    }
    match short_session(agent.agent_session.as_deref()) {
        Some(short) => short,
        None => label(agent),
    }
}

/// The conversation id, shortened for a thirty column row. A basename,
/// because hooks report paths as well as ids, and a prefix when even that
/// is long: the start is what a person recognises.
fn short_session(session: Option<&str>) -> Option<String> {
    let base = session?.trim();
    if base.is_empty() {
        return None;
    }
    let base = base
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(base);
    if base.chars().count() > 12 {
        Some(format!("{}…", base.chars().take(8).collect::<String>()))
    } else {
        Some(base.to_string())
    }
}

/// Every row of the column, with what a click on it means.
///
/// Same contract as [`rows`]: [`render_folded`] and [`hit_folded`] share this one
/// function so a click lands on the row a person aimed at by construction.
/// Also shared with the screen painter, which needs the hits to mark the
/// keyboard's own rows: the text alone cannot say which row is selected.
///
/// Eight arguments, like [`render_folded`]: same pieces, same reason.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rows_folded(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    tick: u64,
    view: View,
    seen: &Seen,
    unseen: &Unseen,
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
        tick,
        view,
        seen,
        unseen,
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
        // No focus mark on the strip either: the selected row is the raised
        // background alone. The index tells one space row from another.
        let _ = (focused_space, focused_terminal);
        out.push((
            Line {
                text: strip_space(at + 1),
                colour: None,
                accent: Some((1, 1, crate::theme::space_of(space.id))),
                indicator: None,
            },
            Some(Hit::Space(space.id)),
        ));
    }
    out.push((
        Line {
            text: " ".repeat(COLLAPSED),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
            indicator: None,
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
                text: strip_agent(),
                colour: colour_for(&agent.state),
                accent: Some((1, 1, crate::theme::space_of(agent.workspace))),
                indicator: None,
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
                indicator: None,
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

/// One row of the folded strip for an agent: a middle dot, in the row's state
/// colour. No circle, no filled dot for the selected row.
fn strip_agent() -> String {
    let text = " \u{b7}";
    let pad = COLLAPSED.saturating_sub(crate::width::width(text));
    format!("{text}{}", " ".repeat(pad))
}

/// One row of the folded strip for a space: its index among spaces. Spaces
/// keep theirs; with no names on the strip the index is what tells one space
/// row from another.
fn strip_space(n: usize) -> String {
    let text = format!(" {n}");
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
                indicator: None,
            },
            Some(Hit::Collapse),
        );
    }
    // The foot of the column says what this is and what version of it, the way a product
    // signs its own window, and the fold mark sits at the far end of the same row rather
    // than taking a row of its own: the column is worth more to a person as list than as
    // furniture. When `qoro update --check` saw something newer, the row says where this
    // build stands against it, so the update is found rather than remembered.
    let brand = format!(" {BRAND_MARK} {BRAND}");
    let version = match update_available() {
        Some(new) => format!("{VERSION}>{new} "),
        None => format!("{VERSION} "),
    };
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
            indicator: None,
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

/// A newer version `qoro update --check` saw, when it saw one.
///
/// Read off the notice file on every paint rather than held in state: the check runs in
/// a process outside the interface, so anything held would be stale by construction. A
/// missing file, garbage in it, or nothing newer is no notice, and the row is byte for
/// byte what it always was.
fn update_available() -> Option<String> {
    let seen = qoro_runtime::paths::read_update_notice()?;
    qoro_runtime::paths::is_newer_version(&seen, VERSION).then_some(seen)
}

/// The column as it is when it is not folded away.
///
/// Eight arguments, like [`render_folded`]: same pieces, same reason.
#[allow(clippy::too_many_arguments)]
fn rows_open(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused_space: i64,
    focused_terminal: u32,
    height: usize,
    tick: u64,
    view: View,
    seen: &Seen,
    unseen: &Unseen,
) -> Vec<(Line, Option<Hit>)> {
    // What the lower panel wants, asked before the column is divided. The division is a
    // place and not a consequence of how many spaces there happen to be, but a panel that
    // held its half while the list below it scrolled would be a place at the wrong price.
    let (content, shown, total) =
        agents_content(spaces, agents, focused_terminal, tick, view, seen, unseen);
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
            view,
        ));
    } else {
        out.extend(lay_out_agents(
            content,
            shown,
            total,
            focused_terminal,
            rest,
            view,
        ));
    }
    out
}

/// The agents panel: one flat list, needs-you first in daemon order and the
/// rest after it, then by urgency in the priority view.
///
/// Every agent draws exactly the rows [`agents_panel`] draws for it. Nothing
/// folds and nothing hides: the state dot carries the state.
#[allow(clippy::too_many_arguments)]
fn agents_content(
    spaces: &[WorkspaceRow],
    agents: &[AgentRow],
    focused: u32,
    tick: u64,
    view: View,
    seen: &Seen,
    unseen: &Unseen,
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
                    indicator: None,
                },
                None,
            )],
            0,
            0,
        );
    }
    // The priority view: the same rows sorted so what wants a person is on
    // top. Both views are flat lists; the order is the only difference.
    if view == View::Priority {
        let mut ordered: Vec<&AgentRow> = listed.to_vec();
        ordered.sort_by_key(|a| priority_of(a, focused, seen, unseen));
        let mut content: Vec<(Line, Option<Hit>)> = Vec::new();
        for agent in &ordered {
            push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        }
        return (content, ordered.len(), listed.len());
    }
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
        .filter(|a| !needs_you(&a.state) && !unobserved(&a.state) && is_running(&a.state))
        .copied()
        .collect();
    let finished: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| {
            !needs_you(&a.state)
                && !unobserved(&a.state)
                && a.state == "done"
                && !is_new(a, focused, seen, unseen)
        })
        .copied()
        .collect();
    // Rows nobody has looked at yet: daemon `done` and derived
    // idle-after-work share the sage dot.
    let new_done: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| !needs_you(&a.state) && is_new(a, focused, seen, unseen))
        .copied()
        .collect();
    let idle: Vec<&AgentRow> = listed
        .iter()
        .filter(|a| {
            !needs_you(&a.state)
                && !unobserved(&a.state)
                && !is_running(&a.state)
                && a.state != "done"
                && !is_new(a, focused, seen, unseen)
        })
        .copied()
        .collect();
    for agent in needs {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    for agent in fresh {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    for agent in new_done {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    for agent in running {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    for agent in finished {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    for agent in idle {
        push_agent_lines(&mut content, spaces, agent, focused, tick, seen, unseen);
        shown += 1;
    }
    (content, shown, listed.len())
}

/// One built agents panel, scrolled and padded into the rows it was given.
fn lay_out_agents(
    content: Vec<(Line, Option<Hit>)>,
    shown: usize,
    total: usize,
    focused: u32,
    height: usize,
    view: View,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(agents_title(view, shown, total), None)];
    let body = height - 1;
    if body == 0 {
        return out;
    }
    // Flat scroll keeping the focused agent's first line visible with room for
    // its second.
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

/// The agents title, carrying the sort order on its right.
///
/// Grouped or priority, the way the rows below are ordered: the header is the
/// one place that says which. The counts ride along when the list scrolls past
/// the window, so a clipped panel still says how much of itself is hidden.
fn agents_title(view: View, shown: usize, total: usize) -> Line {
    let mode = match view {
        View::Grouped => "grouped",
        View::Priority => "priority",
    };
    let left = if shown == total {
        format!(" {AGENTS_TITLE}")
    } else {
        format!(" {AGENTS_TITLE} {shown} of {total}")
    };
    let text = format!(
        "{}{mode}",
        fit_to(&left, WIDTH.saturating_sub(mode.len() + 1)),
    );
    Line {
        text: format!("{text} "),
        colour: Some(crate::theme::Role::Title),
        accent: None,
        indicator: None,
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

/// The row at the foot of the spaces panel: `menu` on the right.
///
/// One row for what is neither a space nor an agent: the menu. Which half a
/// click lands on decides what it meant, and the halves are the word itself
/// rather than the halves of the column: a click on the empty rest is a click
/// on nothing. The `new` half is gone: the tab bar and the spaces title both
/// carry their own `+` already.
fn footer_row() -> (Line, Option<Hit>) {
    let right = format!("{MENU_LABEL} ");
    let gap = WIDTH.saturating_sub(crate::width::width(&right));
    (
        Line {
            text: format!("{}{right}", " ".repeat(gap)),
            colour: Some(crate::theme::Role::Faint),
            accent: None,
            indicator: None,
        },
        // The row itself answers nothing: the caller asks again with the column.
        None,
    )
}

/// What a click at one column of the spaces title means.
///
/// Only the `+` cell itself, the last cell of the row: the rule column beside
/// it and the word beside that are nothing, the way the footer's halves are
/// exact cells rather than a whole row.
pub fn spaces_title_hit(col: usize) -> Option<Hit> {
    (col == WIDTH).then_some(Hit::NewSpace)
}

/// What a click at one column of the footer row means.
pub fn footer_hit(col: usize) -> Option<Hit> {
    if col + 1 >= WIDTH.saturating_sub(crate::width::width(MENU_LABEL)) {
        return Some(Hit::Menu);
    }
    None
}

const MENU_LABEL: &str = "menu";

/// A panel title, which is a row like any other and therefore the full width.
fn title(text: &str) -> Line {
    Line {
        text: fit(&format!(" {text}")),
        colour: Some(crate::theme::Role::Title),
        accent: None,
        indicator: None,
    }
}

/// The spaces title, carrying its `+` at the right corner.
///
/// The row itself answers nothing: only the corner cell makes a space, so a
/// click on the word `spaces` is a no-op rather than a space nobody asked
/// for. The caller asks again with the column, the way the footer's
/// `new`/`menu` halves do.
fn spaces_title() -> Line {
    Line {
        text: format!("{}+", fit_to(&format!(" {SPACES_TITLE}"), WIDTH - 1)),
        colour: Some(crate::theme::Role::Title),
        accent: None,
        indicator: None,
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
            indicator: None,
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
            indicator: None,
        },
        None,
    )
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
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
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            1,
            14,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<&str> = lines.iter().map(|l| l.text.trim_end()).collect();
        // The spaces title carries its `+` at the right corner; the agents
        // title names the sort order there instead.
        assert!(
            texts[0].starts_with(&format!(" {SPACES_TITLE}")) && texts[0].ends_with('+'),
            "{texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(&format!(" {AGENTS_TITLE}")) && t.ends_with("grouped")),
            "{texts:?}"
        );
    }

    #[test]
    fn the_focused_space_and_the_focused_terminal_are_found_by_hit_not_by_glyph() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 2, 3, 14);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // No selection marks anywhere: no hollow ring on every row and no
        // second dot on the selected one. The keyboard's row is the raised
        // background the painter adds. The state dot opens every agent's
        // first row, so it is expected; a spinner never is.
        for bad in ['\u{25cb}', '\u{25cc}', '\u{25a1}', '\u{2809}'] {
            assert!(
                !texts.iter().any(|t| t.contains(bad)),
                "a selection mark or spinner leaked: {texts:?}"
            );
        }
        // The focused space and agent still draw their names.
        assert!(texts.iter().any(|t| t.contains("personal")), "{texts:?}");
        // Terminal 3 is the focused agent: its session on the first row, its
        // tab and space on the second. The first row answers "what", the
        // second "where".
        assert!(
            texts.iter().any(|t| t.starts_with(" \u{25cf} codex")),
            "{texts:?}"
        );
        // And hits still know which rows have the keyboard.
        let hits: Vec<Option<Hit>> = (0..14)
            .map(|row| hit(&spaces, &agents, 2, 3, 14, row))
            .collect();
        assert!(hits.contains(&Some(Hit::Space(2))));
        assert!(hits.contains(&Some(Hit::Terminal(3))));
    }

    #[test]
    fn an_agent_shows_its_session_then_its_tab_and_space() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        let head = texts
            .iter()
            .position(|t| t.starts_with(" \u{25cf} claude"))
            .unwrap_or_else(|| panic!("{texts:?}"));
        // Two rows, and together they carry everything: the session behind
        // the state dot, and the tab and space underneath. No state word on
        // the right: the dot at the start says the state in its colour.
        assert_eq!(texts[head], " \u{25cf} claude", "{texts:?}");
        assert_eq!(texts[head + 1], "  claude - app");
        // And the dot wears the working colour.
        assert_eq!(
            lines
                .iter()
                .find(|l| l.text.trim_end() == " \u{25cf} claude")
                .and_then(|l| l.indicator),
            Some((DOT_AT, DOT_LEN, crate::theme::of("working")))
        );
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
    fn an_agent_row_carries_its_state_on_the_dot_and_a_title_carries_no_state() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        // A title has a look of its own, and it is never a state's: a panel heading that
        // came out the colour of `blocked` would be a heading somebody answers.
        assert_eq!(
            lines[0].colour,
            Some(crate::theme::Role::Title),
            "the spaces title"
        );
        let rows: Vec<&Line> = lines.iter().filter(|l| l.text.contains("claude")).collect();
        // Two rows for the one agent: the first is plain text with the
        // state's colour on its dot, the tab and space underneath stay quiet.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].colour, None);
        assert_eq!(
            rows[0].indicator,
            Some((DOT_AT, DOT_LEN, crate::theme::of("working")))
        );
        assert_eq!(rows[1].colour, Some(crate::theme::Role::Faint));
        assert_eq!(rows[1].indicator, None);
        // A space row has no state to have a colour about; it wears its own instead, as an
        // accent on its name rather than on the whole row. Agent second rows
        // (`tab - space`) are excluded: they name a space but are not space rows.
        assert!(
            lines
                .iter()
                .filter(|l| l.text.contains("personal") && !l.text.contains(" - "))
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
                let want = if text == format!(" {AGENTS_TITLE}") || text.is_empty() {
                    None
                } else if text.starts_with(&format!(" {SPACES_TITLE}")) && text.ends_with('+') {
                    // Row-granular hit: the corner cell is asked with the
                    // column elsewhere, so here the title is nothing.
                    None
                } else if let Some(s) = spaces.iter().find(|s| text == format!(" {}", s.name)) {
                    Some(Hit::Space(s.id))
                } else {
                    // Everything left is one agent's two rows: the first names
                    // the session behind the state dot, the second the tab
                    // and the space under it. Both reach the same terminal.
                    agents
                        .iter()
                        .find(|a| {
                            let space = spaces
                                .iter()
                                .find(|s| s.id == a.workspace)
                                .map(|s| s.name.as_str())
                                .unwrap_or("");
                            let first = format!(" \u{25cf} {}", session_name(a));
                            let place = if space.is_empty() {
                                label(a)
                            } else {
                                format!("{} - {}", label(a), space)
                            };
                            text.starts_with(&first)
                                || (text.starts_with("  ")
                                    && text.trim_end() == format!("  {place}"))
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
        }
    }

    fn agent_texts(spaces: &[WorkspaceRow], agents: &[AgentRow], focused: u32) -> Vec<String> {
        render(spaces, agents, 1, focused, 20)
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect()
    }

    /// The user's panel: four rows reading `opencode` identify nothing. Each
    /// pair's second row names where it lives, so each pair says which one
    /// needs them.
    #[test]
    fn agents_sharing_a_label_carry_their_space_names() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 2, "opencode", "idle"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        assert!(
            texts.iter().any(|t| t.contains("opencode - app")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("opencode - personal")),
            "{texts:?}"
        );
    }

    /// Every pair names where it lives on its second row: the mark's colour
    /// says where it lives, and the words beside it say it again, so no pair
    /// ever stands on its colour alone.
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
            texts.iter().any(|t| t.contains("claude - app")),
            "{texts:?}"
        );
        // And the second row still says where the tab lives.
        assert!(texts.iter().any(|t| *t == "  claude - app"), "{texts:?}");
    }

    /// A chosen name picked twice in two spaces is still two pairs that cannot
    /// be told from one another, so each second row names where it lives.
    #[test]
    fn a_chosen_name_shared_by_two_agents_names_both_spaces() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            named(1, 1, "release", "opencode", "idle"),
            named(2, 2, "release", "claude", "idle"),
        ];
        let texts = agent_texts(&spaces, &agents, 1);
        assert!(
            texts.iter().any(|t| t.contains("release - app")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("release - personal")),
            "{texts:?}"
        );
        // Without a reported session the first row falls back to the tab's
        // own label, so both first rows read the chosen name behind the dot.
        assert_eq!(
            texts.iter().filter(|t| *t == " \u{25cf} release").count(),
            2,
            "{texts:?}"
        );
    }

    /// Two tabs of one space share the program, and still tell apart: their
    /// dots differ in the states' colours, and each pair keeps its own hits.
    #[test]
    fn two_tabs_of_one_space_share_the_space_name_but_not_the_dot() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 1, "opencode", "blocked"),
        ];
        let lines = render(&spaces, &agents, 1, 1, 20);
        let dots: Vec<_> = lines.iter().filter_map(|l| l.indicator).collect();
        assert_eq!(
            dots,
            vec![
                (DOT_AT, DOT_LEN, crate::theme::of("idle")),
                (DOT_AT, DOT_LEN, crate::theme::of("blocked")),
            ],
            "{dots:?}"
        );
        // No state word on the right of either row.
        for line in &lines {
            for word in ["idle", "blocked", "working"] {
                assert!(
                    !line.text.trim_end().ends_with(word) || line.text.contains(" - "),
                    "a state word leaked: {:?}",
                    line.text
                );
            }
        }
        // Both pairs reach their own terminal, first row and second row alike.
        let hits: Vec<Option<Hit>> = (0..20)
            .map(|row| hit(&spaces, &agents, 1, 1, 20, row))
            .collect();
        assert!(hits.contains(&Some(Hit::Terminal(1))));
        assert!(hits.contains(&Some(Hit::Terminal(2))));
    }

    #[test]
    fn a_reported_session_names_the_first_row_alone_and_the_tab_names_the_second() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "opencode", "idle");
        a.name = Some("release".to_string());
        a.agent_session = Some("abc123".to_string());
        let texts = agent_texts(&spaces, &[a], 1);
        // The first row is the session id behind the dot: no program beside
        // it, so four live sessions never all read `opencode`.
        assert!(texts.iter().any(|t| *t == " \u{25cf} abc123"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("opencode")),
            "the program leaked onto a reported session row: {texts:?}"
        );
        assert!(texts.iter().any(|t| *t == "  release - app"), "{texts:?}");
    }

    #[test]
    fn a_reported_title_names_the_first_row_and_follows_renames() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "opencode", "idle");
        a.agent_session = Some("ses_abc".to_string());
        a.agent_title = Some("Samimi selamla\u{15f}ma sohbeti".to_string());
        let texts = agent_texts(&spaces, &[a.clone()], 1);
        // The title wins over the id: the row reads the way the agent names
        // the conversation, not the way the tab does.
        assert!(
            texts.iter().any(|t| t.contains("Samimi selamla")),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("ses_abc")),
            "the id leaked past the title: {texts:?}"
        );
        // A rename arrives on its own: the next index carries the new title.
        a.agent_title = Some("Basit kurulum".to_string());
        let texts = agent_texts(&spaces, std::slice::from_ref(&a), 1);
        assert!(
            texts.iter().any(|t| t.contains("Basit kurulum")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_long_session_is_shortened_to_its_prefix() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "claude", "idle");
        a.agent_session = Some("sess_abc123def456".to_string());
        let texts = agent_texts(&spaces, &[a], 1);
        assert!(texts.iter().any(|t| t.contains("sess_abc…")), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("sess_abc123def456")),
            "{texts:?}"
        );
    }

    #[test]
    fn without_a_session_the_first_row_falls_back_to_the_tab() {
        let spaces = vec![space(1, "app")];
        let texts = agent_texts(&spaces, &[agent(1, 1, "codex", "idle")], 1);
        // No reported session, so the tab's own label answers behind the dot.
        assert!(texts.iter().any(|t| *t == " \u{25cf} codex"), "{texts:?}");
        assert!(texts.iter().any(|t| *t == "  codex - app"), "{texts:?}");
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
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
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
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
        // The agent's first row opens with the state dot and then the name;
        // the pad after the wide name is one this module wrote.
        let row = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .unwrap_or_else(|| panic!("no agent row"));
        assert!(row.text.starts_with(" \u{25cf} "), "{:?}", row.text);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    #[test]
    fn an_absurdly_long_state_never_eats_the_session() {
        let agents = vec![agent(1, 1, "claude", &"z".repeat(80))];
        let lines = render(&[space(1, "app")], &agents, 1, 1, 8);
        // No state word is drawn at all: the row is the dot and the session,
        // however long the daemon's state string runs.
        let row = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .unwrap_or_else(|| panic!("no agent row"));
        assert_eq!(crate::width::width(&row.text), WIDTH);
        assert_eq!(row.text.trim_end(), " \u{25cf} claude", "{:?}", row.text);
        assert!(!row.text.contains('z'));
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
    fn every_agent_draws_its_rows_needs_first_and_no_headers() {
        let (spaces, agents) = many_agents();
        // Terminal 5 is done and already looked at.
        let mut seen = Seen::new();
        seen.insert((5, 0));
        let lines = render_folded(
            &spaces,
            &agents,
            2,
            1,
            40,
            0,
            View::Grouped,
            &seen,
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // Needs-you agents are drawn as usual, first.
        assert!(texts.iter().any(|t| t.contains("codex")), "{texts:?}");
        // Static: running agents carry the working dot, no spinner, and no
        // state word on the right.
        assert!(
            lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::of("working")))),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.ends_with("working")),
            "a state word leaked: {texts:?}"
        );
        // No group headers and no fold lines anywhere: the state dot carries
        // the state. (Agent programs here never contain these words, so any
        // hit is chrome.)
        for header in ["needs you", "running", "finished", "idle", "▸"] {
            assert!(
                !texts.iter().any(|t| t.contains(header)),
                "a group header leaked: {header}: {texts:?}"
            );
        }
        // All nine agents draw their pairs: needs-you first, then the rest in
        // daemon order.
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            18,
            "{texts:?}"
        );
        let first_codex = texts
            .iter()
            .position(|t| t.contains("codex"))
            .expect("codex");
        let first_claude = texts
            .iter()
            .position(|t| t.contains("claude"))
            .expect("claude");
        assert!(first_codex < first_claude, "{texts:?}");
        // The title names the sort order on its right.
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(" agents") && t.ends_with("grouped")),
            "{texts:?}"
        );
        for line in &lines {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
    }

    #[test]
    fn an_idle_agent_draws_its_rows_like_a_working_one() {
        let (spaces, agents) = sample();
        let folded = render_folded(
            &spaces,
            &agents,
            1,
            1,
            14,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = folded
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // Nothing folds: the working agent and the idle one both keep their
        // rows, each pair with its own state dot.
        assert!(texts.iter().any(|t| t.contains("claude")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("codex")), "{texts:?}");
    }
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

    /// A seen update signs the foot of the column with where this build stands.
    #[test]
    fn a_seen_update_names_itself_beside_the_version() {
        // The notice file the check leaves, under a runtime directory of this test's
        // own: no other test reads the variable, so holding it here races nothing.
        let base = tempfile::tempdir().expect("tempdir");
        // SAFETY: the only reader of this variable in this binary is the row below.
        unsafe { std::env::set_var("QORO_RUNTIME_DIR", base.path()) };
        std::fs::write(base.path().join("update-available"), "9.9.9\n").expect("notice");
        let (line, _) = collapse_row(false);
        assert!(
            line.text.contains("9.9.9"),
            "the waiting release has to be readable off the screen: {:?}",
            line.text
        );
        assert_eq!(
            crate::width::width(&line.text),
            WIDTH,
            "the longer row still fills the column exactly: {:?}",
            line.text
        );
        std::fs::remove_file(base.path().join("update-available")).expect("cleanup");
        let (plain, _) = collapse_row(false);
        assert!(
            !plain.text.contains('>'),
            "no notice is the row it always was: {:?}",
            plain.text
        );
        // SAFETY: as above.
        unsafe { std::env::remove_var("QORO_RUNTIME_DIR") };
    }

    /// A done row is unread until the keyboard has been there: not focused,
    /// and no receipt in the seen set.
    #[test]
    fn unread_is_a_done_row_nobody_has_looked_at() {
        let (spaces, agents) = many_agents();
        let _ = spaces;
        let done = agents.iter().find(|a| a.terminal == 5).unwrap();
        let empty = Seen::new();
        assert!(is_unread(done, 1, &empty));
        // Standing on it is reading it.
        assert!(!is_unread(done, 5, &empty));
        let mut seen = Seen::new();
        seen.insert((5, 0));
        assert!(!is_unread(done, 1, &seen));
        // A receipt for another generation does not cover this one.
        let mut old = Seen::new();
        old.insert((5, 7));
        assert!(is_unread(done, 1, &old));
    }

    /// An idle row that finished while the keyboard was away is news too, not
    /// calm: same `new` word and sage as a daemon done, until focused.
    #[test]
    fn new_is_an_idle_row_that_finished_while_away() {
        let (spaces, agents) = many_agents();
        let _ = spaces;
        let idle = agents.iter().find(|a| a.terminal == 7).unwrap();
        assert_eq!(idle.state, "idle");
        let seen = Seen::new();
        let mut unseen = Unseen::new();
        unseen.insert((7, 0));
        assert!(is_new(idle, 1, &seen, &unseen));
        assert!(!is_new(idle, 7, &seen, &unseen));
        assert!(tab_is_new(idle, 1, &unseen));
        assert!(!tab_is_new(idle, 7, &unseen));
        // A plain idle with no finish behind it stays calm.
        let calm = agents.iter().find(|a| a.terminal == 8).unwrap();
        assert!(!is_new(calm, 1, &seen, &Unseen::new()));
    }
    #[test]
    fn priority_view_sorts_by_urgency_without_headers() {
        let (spaces, agents) = many_agents();
        let lines = render_folded(
            &spaces,
            &agents,
            2,
            1,
            40,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        for header in ["needs you", "running", "finished"] {
            assert!(
                !texts.iter().any(|t| t.trim_start().starts_with(header)),
                "a group header leaked into the priority view: {texts:?}"
            );
        }
        // Urgency order without state words: the dots carry the states, so
        // the terminals' first appearance runs blocked, failed, the unread
        // done, then running. Terminals: 1 blocked, 2 failed, 5 unread
        // done, 3 and 4 working.
        let order: Vec<u32> = rows_folded(
            &spaces,
            &agents,
            2,
            1,
            40,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
        )
        .into_iter()
        .filter_map(|(_, hit)| match hit {
            Some(Hit::Terminal(t)) => Some(t),
            _ => None,
        })
        .collect();
        let first = |t: u32| {
            order
                .iter()
                .position(|u| *u == t)
                .unwrap_or_else(|| panic!("no terminal {t}: {order:?}"))
        };
        assert!(first(1) < first(2), "{order:?}");
        assert!(first(2) < first(5), "{order:?}");
        assert!(first(5) < first(3), "{order:?}");
        assert!(first(3) < first(6), "{order:?}");
        // Every agent draws, folded or not: nine pairs, eighteen name rows.
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            18,
            "{texts:?}"
        );
    }

    /// Static panel: the tick changes nothing, every dot keeps its colour
    /// and no state word is ever drawn.
    #[test]
    fn the_panel_is_static_across_ticks() {
        let (spaces, agents) = many_agents();
        let at = |tick: u64| {
            rows_folded(
                &spaces,
                &agents,
                2,
                1,
                40,
                tick,
                View::Grouped,
                &Seen::new(),
                &Unseen::new(),
            )
        };
        let zero: Vec<Line> = at(0).iter().map(|(l, _)| l.clone()).collect();
        let four: Vec<Line> = at(4).iter().map(|(l, _)| l.clone()).collect();
        assert_eq!(zero, four, "the tick repainted a static panel");
        // Every agent's first row opens with its state dot, steady.
        assert!(
            zero.iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::of("blocked")))),
            "no static blocked dot"
        );
        // An unread done keeps the done sage on its dot, steady.
        assert!(
            zero.iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::of("done")))),
            "no static unread dot"
        );
        // And no state word on the right of any agent row.
        for line in zero.iter().filter(|l| l.indicator.is_some()) {
            for word in ["working", "blocked", "failed", "done", "idle"] {
                assert!(
                    !line.text.trim_end().ends_with(word),
                    "a state word leaked: {:?}",
                    line.text
                );
            }
        }
    }

    #[test]
    fn view_toggle_flips_between_grouped_and_priority() {
        assert_eq!(View::Grouped.toggle(), View::Priority);
        assert_eq!(View::Priority.toggle(), View::Grouped);
        assert_eq!(View::default(), View::Grouped);
    }

    /// A blocked agent draws its two rows and nothing else: no headers for
    /// groups with nobody in them.
    #[test]
    fn a_blocked_agent_draws_two_rows_and_no_headers() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "blocked")];
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            1,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("codex")), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("idle")), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(" agents") && t.ends_with("grouped")),
            "{texts:?}"
        );
    }

    /// Only the `+` corner of the spaces title opens a space: the title row
    /// itself is a no-op, so a click on the word `spaces` never makes a space
    /// nobody asked for.
    #[test]
    fn only_the_plus_corner_of_the_spaces_title_opens_a_space() {
        let (spaces, agents) = sample();
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            1,
            14,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(texts[0].ends_with('+'), "{texts:?}");
        assert_eq!(
            hit_folded(
                &spaces,
                &agents,
                1,
                1,
                14,
                0,
                View::Grouped,
                &Seen::new(),
                &Unseen::new()
            ),
            None,
            "the title row must not open a space: {texts:?}"
        );
        assert_eq!(spaces_title_hit(5), None);
        assert_eq!(spaces_title_hit(WIDTH), Some(Hit::NewSpace));
    }

    /// The keyboard's rows are always drawn: nothing folds them away, so a
    /// focused terminal never reads as a terminal that died.
    #[test]
    fn the_focused_agent_always_draws_its_rows() {
        let (spaces, agents) = many_agents();
        for focused in [1u32, 5, 9] {
            let hits: Vec<u32> = (0..30)
                .filter_map(|row| {
                    hit_folded(
                        &spaces,
                        &agents,
                        1,
                        focused,
                        30,
                        row,
                        View::Grouped,
                        &Seen::new(),
                        &Unseen::new(),
                    )
                })
                .filter_map(|hit| match hit {
                    Hit::Terminal(t) => Some(t),
                    _ => None,
                })
                .collect();
            assert!(
                hits.contains(&focused),
                "focused {focused} vanished: {hits:?}"
            );
        }
    }

    /// An unobserved agent stays on screen: what has not been looked at stays
    /// visible until detection settles, rather than folding away as idle.
    #[test]
    fn an_unobserved_agent_stays_on_screen() {
        let spaces = vec![space(1, "app")];
        let mut agents = vec![agent(1, 1, "sh", "unknown")];
        agents[0].agent = true;
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            1,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("sh")), "{texts:?}");
    }

    /// An unread done draws inline with the done sage on its dot: news, not
    /// calm, and no header above it.
    #[test]
    fn an_unread_done_draws_inline_with_the_done_dot() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "claude", "done")];
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            2,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::of("done")))),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains('\u{25b8}')),
            "a fold line leaked: {texts:?}"
        );
    }
}

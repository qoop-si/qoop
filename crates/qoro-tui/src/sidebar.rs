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

/// Columns the left column occupies, unless the person pulled it wider or
/// narrower.
///
/// Thirty. A comparable multiplexer ships twenty six as a default a
/// person can drag between eighteen and thirty six, and the wide end of
/// that range is the right default here: this column carries a space name,
/// a separator, a mark, a name and a state word flush against the right
/// edge, and at twenty two the state had nothing left. Thirty leaves a
/// real project name room to be read rather than recognised from its first
/// eight characters.
///
/// A pulled width reflows the pane beside it, which is the price the person
/// pulling asked to pay: explicit drags reflow, arrivals never do.
pub const WIDTH: usize = 30;

/// The narrowest the open column drags to, and the widest.
///
/// Kept to the range (eighteen to thirty six); the default above
/// stays qoro's own. Below eighteen the place rows are mostly ellipsis,
/// above thirty six the pane starts paying real columns for furniture.
pub const SIDEBAR_MIN: usize = 18;
pub const SIDEBAR_MAX: usize = 36;

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
///
/// Shape says failed: failed wears a diamond while every other state wears the
/// circle, because failed's magenta must read apart from the warm alarm pair
/// at a glance, with hue and shape together rather than hue alone.
const DOT: char = '\u{25cf}';
/// The failed dot: a diamond, the same single column as the circle.
const FAILED_DOT: char = '\u{25c6}';
/// The column the state dot sits at: one leading space.
const DOT_AT: usize = 1;
/// Columns the state dot covers: one glyph, one column.
const DOT_LEN: usize = 1;

/// Which mark a state dot wears: the diamond for failed, the circle for the rest.
///
/// Colour still rides the indicator beside the text; shape rides the text itself,
/// so the pair survives colour blindness and light terminals alike.
fn dot_glyph(role: crate::theme::Role) -> char {
    match role {
        crate::theme::Role::Failed => FAILED_DOT,
        _ => DOT,
    }
}

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
    /// The `grouped` or `priority` word at the corner of the agents title:
    /// the order the rows below are in, and the switch that flips it.
    View,
    /// The mark at the foot of the column: fold the whole column away, and back.
    Collapse,
}

/// How the agents panel orders its rows: daemon order, or flat by urgency.
///
/// Grouped is the panel a person already knows: the daemon's order, kept
/// across state changes, so a row never moves under the eye. Priority is the
/// same rows sorted so the one that needs a person is always on top: the
/// inbox view of the fleet. Both are flat lists: the state dot carries the
/// state, and no header tree sits above calm rows. The title names the order.
/// Toggled with `ctrl-a g`, or with a click on the `grouped`/`priority` word
/// itself.
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
/// what is alive, then what is settled.
///
/// The rank alone, not the whole key: inside the calm ranks (`done`,
/// `idle`, the rest) the newest stamp goes first and unstamped rows sink,
/// and the sort's stability keeps daemon order as the last tie-break
/// everywhere. Urgency ranks never see a stamp, so recency cannot promote
/// a calm row above one that needs a person.
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

/// How long after its last turn an idle row still reads as just stopped,
/// and how long without one before the whole row dims.
///
/// Radar parity (`activity_fresh_minutes = 15`, `activity_stale_minutes =
/// 120`): the first 15 minutes read as fresh, then plain text out to two
/// hours, after which the row dims and sinks within its rank. Fresh versus
/// plain differ in order only, never in shape: a mark that changed shape as
/// it aged would have to be learned three times.
pub const ACTIVITY_FRESH_SECS: i64 = 900;
pub const ACTIVITY_STALE_SECS: i64 = 7200;

/// How old an idle row reads: just stopped, settled, or abandoned.
///
/// `None` is not a tier but the absence of one: rows that are not idle, or
/// idle rows from a daemon that never stamped them, paint exactly as they
/// always did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Fresh,
    Plain,
    Stale,
}

/// Which tier an idle row is in at `now_unix`, or `None` when tiers do not
/// apply to it.
///
/// A negative age (a stamp from a clock ahead of this one) reads as fresh:
/// the row just did something by every clock that matters here.
pub fn tier_of(state: &str, last_active_at_unix: Option<i64>, now_unix: i64) -> Option<Tier> {
    if state != "idle" {
        return None;
    }
    let age = now_unix.saturating_sub(last_active_at_unix?);
    if age <= ACTIVITY_FRESH_SECS {
        Some(Tier::Fresh)
    } else if age <= ACTIVITY_STALE_SECS {
        Some(Tier::Plain)
    } else {
        Some(Tier::Stale)
    }
}

/// The client's own wall clock, as unix seconds.
///
/// The same domain as the daemon's arrival stamps, which is the only reason
/// the subtraction in [`tier_of`] means anything. Read once per frame by
/// the caller and passed down, so one paint never disagrees with itself.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// When the panel next has something new to say on its own, as unix seconds.
///
/// The next tier boundary among the idle rows that carry a stamp, rounded up
/// to the minute: sub-minute precision would wake the loop for a change
/// nobody can read. `None` when no visible row has a future boundary, which
/// is the common case, and the loop then arms nothing.
///
/// One deadline and not an interval: a fleet that goes quiet wakes at most
/// twice per idle row (fresh to plain, plain to stale), and a wake that
/// paints an identical frame writes nothing to the screen. This is an
/// arrival by another name, which is what the loop's founding rule protects:
/// arrivals are cheap, polling is not. No observation work happens here,
/// only a repaint of rows already held.
pub fn wants_tick(rows: &[AgentRow], now_unix: i64) -> Option<i64> {
    const MINUTE: i64 = 60;
    rows.iter()
        .filter(|r| r.agent && r.state == "idle")
        .filter_map(|r| r.last_active_at_unix)
        .filter_map(|ts| {
            let next = if ts.saturating_add(ACTIVITY_FRESH_SECS) > now_unix {
                ts.saturating_add(ACTIVITY_FRESH_SECS)
            } else if ts.saturating_add(ACTIVITY_STALE_SECS) > now_unix {
                ts.saturating_add(ACTIVITY_STALE_SECS)
            } else {
                return None;
            };
            Some(next.saturating_add(MINUTE - 1) / MINUTE * MINUTE)
        })
        .min()
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
/// Same contract as [`render`]: exactly `height` lines of exactly `width`
/// columns. One flat list: every agent draws its rows, nothing folds and no
/// group header sits above calm rows.
///
/// Eight arguments and not a context struct: every caller already holds each
/// piece separately, and a struct would move the same list one line down.
///
/// Ten with the clock: paint-time unix seconds, read once per frame by the
/// caller so one paint never disagrees with itself.
///
/// Eleven with the width: the live panel width, defaulting to [`WIDTH`] on
/// the legacy surface below.
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
    now_unix: i64,
    width: usize,
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
        now_unix,
        width,
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
/// Ten arguments, like [`render_folded`]: same pieces, same reason. The
/// clock rides along so a click maps onto the rows the paint drew: tiers
/// and recency move rows, and a hit test on a different clock could aim at
/// the row above the one meant.
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
    now_unix: i64,
    width: usize,
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
        now_unix,
        width,
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
    // The legacy surface paints at the default width: production goes
    // through `rows_folded` with the live width.
    let mut out = spaces_panel(spaces, focused_space, split, WIDTH);
    // A rule between the panels, glued to both like a border, so the column
    // reads as two stacked boxes. It takes its row from the agents side, and
    // only when the agents side keeps at least its title: a divider that ate
    // the title would divide nothing from nothing.
    let rest = height - split;
    if rest >= 2 {
        out.push(divider(WIDTH));
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
fn spaces_panel(
    spaces: &[WorkspaceRow],
    focused: i64,
    height: usize,
    width: usize,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(spaces_title(width), None)];
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
                    text: fit(&format!(" {}", space.name), width),
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
            None => blank(width),
        });
    }
    if footer == 1 {
        out.push(footer_row(width));
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
    let mut out = vec![(title(AGENTS_TITLE, WIDTH), None)];
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
                text: fit(" no agents yet", WIDTH),
                colour: Some(crate::theme::Role::Faint),
                accent: None,
                indicator: None,
            },
            None,
        ));
        while out.len() < height {
            out.push(blank(WIDTH));
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
        // below spends the real tick. Tiers need a clock this surface does
        // not take, so it paints at zero, where no positive stamp can read
        // as stale.
        push_agent_lines(
            &mut content,
            spaces,
            agent,
            focused,
            0,
            &Seen::new(),
            &Unseen::new(),
            0,
            WIDTH,
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
            None => out.push(blank(WIDTH)),
        }
    }
    out.truncate(height);
    out
}

/// The two rows an open agent takes, with what a click on either means.
///
/// First the session, then where it lives: ` ● abc123ef` above
/// `app - release`. Both rows reach the same terminal, so a click on either
/// lands on the agent.
///
/// Static: the state is the dot at the start of the first row, in the
/// state's own colour (and failed's own diamond: shape tells failed from
/// blocked where their oranges will not), and no state word is drawn on the
/// right. An unread done keeps the done sage on its dot. `tick` is ignored so
/// a still panel is byte for byte the panel it was. The keyboard's row is the
/// raised background the painter adds; no filled dot ever marks selection.
///
/// One helper and not the same pushes at each caller, because the unfolded
/// panel and the folded one have to draw the same rows for the same agent.
///
/// Eight arguments, like the callers: every one already holds each piece
/// separately.
#[allow(clippy::too_many_arguments)]
fn push_agent_lines(
    out: &mut Vec<(Line, Option<Hit>)>,
    spaces: &[WorkspaceRow],
    agent: &AgentRow,
    focused: u32,
    _tick: u64,
    seen: &Seen,
    unseen: &Unseen,
    now_unix: i64,
    width: usize,
) {
    let hit = Some(Hit::Terminal(agent.terminal));
    let tab = label(agent);
    let space = spaces.iter().find(|s| s.id == agent.workspace);
    // Line one names the session: the agent's own conversation id, and
    // nothing else. The program never stands beside it: four rows reading
    // `opencode` identify nothing, and the tab below already says what runs
    // where. Static: the dot carries the state so the name keeps the row.
    //
    // A session longer than the row ends in an ellipsis rather than a hard
    // cut, and only then: a session that fits keeps today's bytes exactly.
    // The name resolves first and the column cut runs after, so an id that
    // already ends in `…` never grows a second one.
    let session = session_name(agent);
    // The room the name may take: the dot and its two spaces never shrink,
    // and a width below the prefix saturates rather than panics (public
    // callers may pass anything; the live path only ever sends pulled
    // widths at or above `SIDEBAR_MIN`).
    let session = shorten_middle(&session, width.saturating_sub(3));
    let fresh = is_new(agent, focused, seen, unseen);
    // A derived idle-after-work keeps the done sage: it is news, not calm.
    let dot = if fresh {
        crate::theme::of("done")
    } else {
        crate::theme::of(&agent.state)
    };
    // A stale idle row dims as a whole, but only off focus and only when it
    // is not news: standing on it is reading it, and news never shows faded.
    // The space accent is identity and never dims with the state: where the
    // agent lives stays readable while what it is doing goes quiet.
    let stale = !fresh
        && agent.terminal != focused
        && tier_of(&agent.state, agent.last_active_at_unix, now_unix) == Some(Tier::Stale);
    let first_colour = stale.then_some(crate::theme::Role::Faint);
    let dot_role = if stale {
        crate::theme::Role::Faint
    } else {
        dot
    };
    out.push((
        Line {
            text: fit(&format!(" {} {session}", dot_glyph(dot_role)), width),
            colour: first_colour,
            // The space lives on the second row now, so the first row carries
            // no space accent: the keyboard's row is the raised background.
            accent: None,
            indicator: Some((DOT_AT, DOT_LEN, dot_role)),
        },
        hit,
    ));
    // Where it lives, underneath: the space and then the tab, in the quiet
    // colour, so the first row keeps the eye and the second keeps the place.
    // The space name starts where the first row's text starts, one column in
    // from the dot's own indent. A space the client has not heard of yet
    // still draws, under the tab alone, rather than vanishing until the
    // index catches up. The space keeps its colour here, so it still wears
    // one colour in both panels.
    //
    // When the terminal sits in a directory the space name does not already
    // say, the directory's own basename joins the row between them: the
    // worktree a session runs in, in the common case, without asking git on
    // every frame. The middle shortens first and the space head and tab tail
    // are never cut for it; too narrow for even that and the row is today's.
    let place_middle: Option<String> = space.as_ref().and_then(|space| {
        let base = cwd_basename(agent.cwd.as_deref())?;
        (!base.eq_ignore_ascii_case(&space.name))
            .then(|| middle_room(&space.name, &tab, width))
            .flatten()
            .map(|room| shorten_middle(&base, room))
    });
    let (place, place_accent) = match (space, place_middle) {
        (Some(space), Some(base)) => (
            format!("{} - {base} - {tab}", space.name),
            Some((
                3,
                crate::width::width(&space.name),
                crate::theme::space_of(agent.workspace),
            )),
        ),
        (Some(space), None) => (
            format!("{} - {tab}", space.name),
            Some((
                3,
                crate::width::width(&space.name),
                crate::theme::space_of(agent.workspace),
            )),
        ),
        (None, _) => (tab.clone(), None),
    };
    out.push((
        Line {
            text: fit_to(&format!("   {place}"), width),
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

/// The directory a terminal is in, as its own basename.
///
/// Trailing slashes are not a name, the root has none, and blank is not a
/// directory: all three answer `None`, and the place row stays today's. A
/// home tilde and dots are names and pass through untouched.
fn cwd_basename(cwd: Option<&str>) -> Option<String> {
    let base = cwd?
        .trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())?;
    Some(base.to_string())
}

/// Columns left for the middle of `space - middle - tab` once the parts that
/// are never cut are paid for, or `None` when even the shortest middle costs
/// more room than the row has.
fn middle_room(space: &str, tab: &str, width: usize) -> Option<usize> {
    // Three leading spaces and two ` - ` separators around the three names.
    let fixed = 3 + crate::width::width(space) + 3 + 3 + crate::width::width(tab);
    let room = width.checked_sub(fixed)?;
    // A middle narrower than an ellipsis and one letter explains nothing;
    // the row without it says more.
    (room >= 2).then_some(room)
}

/// The middle shortened to `room` columns, tail-cut with an ellipsis, or the
/// whole middle when it already fits. Called only after [`middle_room`]
/// answered, so `room` always holds at least the ellipsis and one letter.
fn shorten_middle(middle: &str, room: usize) -> String {
    if crate::width::width(middle) <= room {
        return middle.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in middle.chars() {
        let wide = crate::width::columns(c);
        if used + wide + 1 > room {
            break;
        }
        out.push(c);
        used += wide;
    }
    out.push('…');
    out
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
/// Eight arguments, like [`render_folded`]: same pieces, same reason, plus
/// the paint-time clock and the live width: twelve in total.
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
    now_unix: i64,
    width: usize,
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
        now_unix,
        width,
    );
    // The mark that folds the column away, at its foot, where the eye lands last.
    if let Some(last) = out.last_mut() {
        *last = collapse_row(false, width);
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
        let role = colour_for(&agent.state).unwrap_or(crate::theme::Role::Idle);
        out.push((
            Line {
                text: strip_agent(role),
                colour: colour_for(&agent.state),
                // The space mark sits on the first pad cell, never on the glyph:
                // the glyph column keeps the state colour, so the strip carries
                // state and space side by side instead of painting one over the other.
                accent: Some((2, 1, crate::theme::space_of(agent.workspace))),
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
        out.push(collapse_row(true, COLLAPSED));
    }
    out.truncate(height);
    out
}

/// One row of the folded strip for an agent: the state mark, then the space's
/// own middle dot, side by side. The painter only ever changes the foreground,
/// so a space accent on a padding blank would be invisible: the space needs its
/// own visible mark. Failed is the diamond, every other state the circle, and
/// the middle dot always the space. No filled dot for the selected row.
fn strip_agent(role: crate::theme::Role) -> String {
    let text = format!(" {}\u{b7}", dot_glyph(role));
    let pad = COLLAPSED.saturating_sub(crate::width::width(&text));
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
fn collapse_row(collapsed: bool, width: usize) -> (Line, Option<Hit>) {
    collapse_row_with(collapsed, width, avatar_marks())
}

/// The picked avatar, when one is picked.
///
/// Reads the file on every paint like the update notice below it: the picker
/// runs in a process outside the interface, so anything held would be stale by
/// construction. Under test this is always none, so the thirty-odd paints the
/// suite asserts never depend on the developer's own home directory.
fn avatar_marks() -> Option<qoro_runtime::avatar::Avatar> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        qoro_runtime::avatar::load()
    }
}

/// [`collapse_row`] with the avatar passed in, so tests can paint both feet
/// without touching a home directory.
fn collapse_row_with(
    collapsed: bool,
    width: usize,
    avatar: Option<qoro_runtime::avatar::Avatar>,
) -> (Line, Option<Hit>) {
    let width = if collapsed { COLLAPSED } else { width };
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
    //
    // When an avatar is picked the single brand dot becomes its two dots, hair beside
    // body, followed by the pet's name, so the foot reads as the person's own pet
    // at a glance and not as two dots in similar blues. The product name and
    // version stay put: a person reporting a bug still reads both off the same row.
    // A narrow column drops the name first; the dots always fit.
    let brand = match avatar {
        Some(_) => format!(" {BRAND_MARK}{BRAND_MARK} {BRAND}"),
        None => format!(" {BRAND_MARK} {BRAND}"),
    };
    let version = match update_available() {
        Some(new) => format!("{VERSION}>{new} "),
        None => format!("{VERSION} "),
    };
    let name = avatar
        .as_ref()
        .map(|found| found.name())
        .unwrap_or_default();
    let room = width
        .saturating_sub(crate::width::width(&brand) + 1 + crate::width::width(&version) + 2 + 1);
    let shown = if name.is_empty() || room < 5 {
        String::new()
    } else if crate::width::width(&name) <= room {
        name
    } else {
        let mut short = name;
        while crate::width::width(&short) + 1 > room {
            short.pop();
        }
        short.push('\u{2026}');
        short
    };
    // The row is the mark and name, a space, the version and the pet name, the
    // gap, then the fold mark and its own trailing space.
    let mid = if shown.is_empty() {
        version
    } else {
        format!("{version}{shown} ")
    };
    let gap = width.saturating_sub(crate::width::width(&brand) + 1 + crate::width::width(&mid) + 2);
    let (accent, indicator) = match avatar {
        // Two inks need two runs: the hair dot opens the row, the body dot closes
        // the opening, and the painter never overlaps them by construction.
        Some(found) => (
            Some((1, 1, crate::theme::Role::AvatarHair(found.hair_ink))),
            Some((2, 1, crate::theme::Role::AvatarBody(found.body_ink))),
        ),
        None => (
            // The mark and the name together, so the accent reads as one wordmark rather
            // than as a coloured bullet next to grey text.
            Some((1, crate::width::width(BRAND) + 2, crate::theme::Role::Brand)),
            None,
        ),
    };
    (
        Line {
            text: format!("{brand} {mid}{}{mark} ", " ".repeat(gap)),
            colour: Some(crate::theme::Role::Faint),
            accent,
            indicator,
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
/// Ten arguments, like [`render_folded`]: same pieces, same reason.
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
    now_unix: i64,
    width: usize,
) -> Vec<(Line, Option<Hit>)> {
    // What the lower panel wants, asked before the column is divided. The division is a
    // place and not a consequence of how many spaces there happen to be, but a panel that
    // held its half while the list below it scrolled would be a place at the wrong price.
    let (content, shown, total) = agents_content(
        spaces,
        agents,
        focused_terminal,
        tick,
        view,
        seen,
        unseen,
        now_unix,
        width,
    );
    let split = spaces_rows(spaces.len(), height, content.len() + 1);
    let mut out = spaces_panel(spaces, focused_space, split, width);
    let rest = height - split;
    if rest >= 2 {
        out.push(divider(width));
        out.extend(lay_out_agents(
            content,
            shown,
            total,
            focused_terminal,
            rest - 1,
            view,
            width,
        ));
    } else {
        out.extend(lay_out_agents(
            content,
            shown,
            total,
            focused_terminal,
            rest,
            view,
            width,
        ));
    }
    out
}

/// The agents panel: one flat list in daemon order, then by urgency in the
/// priority view.
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
    now_unix: i64,
    width: usize,
) -> (Vec<(Line, Option<Hit>)>, usize, usize) {
    let listed: Vec<&AgentRow> = agents.iter().filter(|a| a.agent).collect();
    if listed.is_empty() {
        // Said, rather than left as a void, for the reason the unfolded panel says it.
        return (
            vec![(
                Line {
                    text: fit(" no agents yet", width),
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
    //
    // Recency breaks ties inside the calm ranks only (`done`, `idle`, and
    // the rest): urgency ranks answer "who needs a person" and the second
    // question must not reorder the first, so a burst of working activity
    // never shuffles the rows being tracked. The sort is stable, so daemon
    // order still breaks every remaining tie, and rows without a stamp sink
    // below stamped ones within their rank.
    if view == View::Priority {
        let mut ordered: Vec<&AgentRow> = listed.to_vec();
        ordered.sort_by_key(|a| {
            let rank = priority_of(a, focused, seen, unseen);
            if matches!(rank, 5..=7) {
                match a.last_active_at_unix {
                    Some(ts) => (rank, 0u8, std::cmp::Reverse(ts)),
                    None => (rank, 1u8, std::cmp::Reverse(0)),
                }
            } else {
                (rank, 0u8, std::cmp::Reverse(0))
            }
        });
        let mut content: Vec<(Line, Option<Hit>)> = Vec::new();
        for agent in &ordered {
            push_agent_lines(
                &mut content,
                spaces,
                agent,
                focused,
                tick,
                seen,
                unseen,
                now_unix,
                width,
            );
        }
        return (content, ordered.len(), listed.len());
    }
    // Grouped is the daemon's order, untouched: a row never moves under the
    // eye while its state changes. Urgency lives in the priority view, the
    // status line, the tab dots and the palette, never in this order.
    let mut content: Vec<(Line, Option<Hit>)> = Vec::new();
    for agent in &listed {
        push_agent_lines(
            &mut content,
            spaces,
            agent,
            focused,
            tick,
            seen,
            unseen,
            now_unix,
            width,
        );
    }
    (content, listed.len(), listed.len())
}

/// One built agents panel, scrolled and padded into the rows it was given.
#[allow(clippy::too_many_arguments)]
fn lay_out_agents(
    content: Vec<(Line, Option<Hit>)>,
    shown: usize,
    total: usize,
    focused: u32,
    height: usize,
    view: View,
    width: usize,
) -> Vec<(Line, Option<Hit>)> {
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![(agents_title(view, shown, total, width), None)];
    // One breath between the title and the rows: the header is a switch now,
    // and the gap keeps the first agent from reading as part of it.
    out.push(blank(width));
    let body = height.saturating_sub(2);
    if body == 0 {
        out.truncate(height);
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
            None => out.push(blank(width)),
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
/// (`shown` always equals `total` today: every listed agent draws. The branch
/// stays for the day one does not.)
fn agents_title(view: View, shown: usize, total: usize, width: usize) -> Line {
    let mode = title_mode(view);
    let left = if shown == total {
        format!(" {AGENTS_TITLE}")
    } else {
        format!(" {AGENTS_TITLE} {shown} of {total}")
    };
    let text = format!(
        "{}{mode}",
        fit_to(&left, width.saturating_sub(mode.len() + 1)),
    );
    Line {
        text: format!("{text} "),
        colour: Some(crate::theme::Role::Title),
        accent: None,
        indicator: None,
    }
}

/// The word on the right of the agents title: the order the rows below are in.
fn title_mode(view: View) -> &'static str {
    match view {
        View::Grouped => "grouped",
        View::Priority => "priority",
    }
}

/// What a click at one column of the agents title means.
///
/// Only the `grouped`/`priority` corner word flips the view: it sits at the
/// right edge, ahead of the trailing space, both words plain ASCII. A click on
/// the `agents` word itself is a click on nothing.
pub fn agents_title_hit(view: View, col: usize, width: usize) -> Option<Hit> {
    let mode = title_mode(view);
    (col + 1 >= width.saturating_sub(mode.len())).then_some(Hit::View)
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
fn footer_row(width: usize) -> (Line, Option<Hit>) {
    let right = format!("{MENU_LABEL} ");
    let gap = width.saturating_sub(crate::width::width(&right));
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
pub fn spaces_title_hit(col: usize, width: usize) -> Option<Hit> {
    (col == width).then_some(Hit::NewSpace)
}

/// What a click at one column of the footer row means.
pub fn footer_hit(col: usize, width: usize) -> Option<Hit> {
    if col + 1 >= width.saturating_sub(crate::width::width(MENU_LABEL)) {
        return Some(Hit::Menu);
    }
    None
}

const MENU_LABEL: &str = "menu";

/// A panel title, which is a row like any other and therefore the full width.
fn title(text: &str, width: usize) -> Line {
    Line {
        text: fit(&format!(" {text}"), width),
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
fn spaces_title(width: usize) -> Line {
    Line {
        text: format!(
            "{}+",
            fit_to(&format!(" {SPACES_TITLE}"), width.saturating_sub(1))
        ),
        colour: Some(crate::theme::Role::Title),
        accent: None,
        indicator: None,
    }
}

/// The rule between the spaces panel and the agents panel: one row of box
/// drawing, clickable as nothing, so a click on it does nothing rather than
/// something arbitrary.
fn divider(width: usize) -> (Line, Option<Hit>) {
    (
        Line {
            text: "\u{2500}".repeat(width),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
            indicator: None,
        },
        None,
    )
}

/// A row with nothing on it, and nothing to do when it is clicked.
fn blank(width: usize) -> (Line, Option<Hit>) {
    (
        Line {
            text: " ".repeat(width),
            colour: None,
            accent: None,
            indicator: None,
        },
        None,
    )
}

/// Cut or pad to exactly `width` columns.
fn fit(text: &str, width: usize) -> String {
    fit_to(text, width)
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

    /// The state a terminal has when nothing has been able to say what it is doing.
    const UNKNOWN: &str = "unknown";

    /// A fixed paint-time clock: tests pin behavior at exact ages, never time.
    const NOW: i64 = 1_750_000_000;

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
            last_active_at_unix: None,
            cwd: None,
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
            last_active_at_unix: None,
            cwd: None,
        }
    }

    /// An agent row stamped `age_secs` before `NOW`: tiers and recency read
    /// off the stamp, so tests pin exact ages instead of sleeping.
    fn agent_at(
        terminal: u32,
        workspace: i64,
        program: &str,
        state: &str,
        age_secs: i64,
    ) -> AgentRow {
        let mut row = agent(terminal, workspace, program, state);
        row.last_active_at_unix = Some(NOW - age_secs);
        row
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
            NOW,
            WIDTH,
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
        assert_eq!(texts[head + 1], "   app - claude");
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
                    // Either dot glyph opens the first row: the circle for every
                    // state but failed, the diamond for failed.
                    agents
                        .iter()
                        .find(|a| {
                            let space = spaces
                                .iter()
                                .find(|s| s.id == a.workspace)
                                .map(|s| s.name.as_str())
                                .unwrap_or("");
                            let first = |glyph: char| format!(" {glyph} {}", session_name(a));
                            let place = if space.is_empty() {
                                label(a)
                            } else {
                                format!("{space} - {}", label(a))
                            };
                            text.starts_with(&first(DOT))
                                || text.starts_with(&first(FAILED_DOT))
                                || (text.starts_with("   ")
                                    && text.trim_end() == format!("   {place}"))
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
            last_active_at_unix: None,
            cwd: None,
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
            texts.iter().any(|t| t.contains("app - opencode")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("personal - opencode")),
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
            texts.iter().any(|t| t.contains("app - claude")),
            "{texts:?}"
        );
        // And the second row still says where the tab lives.
        assert!(texts.iter().any(|t| *t == "   app - claude"), "{texts:?}");
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
            texts.iter().any(|t| t.contains("app - release")),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("personal - release")),
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
        assert!(texts.iter().any(|t| *t == "   app - release"), "{texts:?}");
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
        assert!(texts.iter().any(|t| *t == "   app - codex"), "{texts:?}");
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
            last_active_at_unix: None,
            cwd: None,
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
            last_active_at_unix: None,
            cwd: None,
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
    fn every_agent_draws_its_rows_in_daemon_order_and_no_headers() {
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
            NOW,
            WIDTH,
        );
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // Every agent draws its rows, in the daemon's order: terminal 1
        // (blocked) first because it is listed first, not because of what it
        // needs.
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
        // All nine agents draw their pairs, in the daemon's order.
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
            NOW,
            WIDTH,
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
        let (line, hit) = collapse_row(false, WIDTH);
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
        let (strip, _) = collapse_row(true, WIDTH);
        assert_eq!(crate::width::width(&strip.text), COLLAPSED);
        assert!(!strip.text.contains(BRAND));
    }

    /// A picked avatar signs the foot with its own two dots, hair beside body,
    /// its name beside the version, and the fold mark stays where it always was.
    #[test]
    fn a_picked_avatar_wears_its_two_inks_on_the_foot() {
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 33,
        };
        let (line, hit) = collapse_row_with(false, WIDTH, Some(pet));
        assert_eq!(
            crate::width::width(&line.text),
            WIDTH,
            "the row has to fill the column exactly: {:?}",
            line.text
        );
        assert!(line.text.contains(BRAND), "{:?}", line.text);
        assert!(line.text.contains(VERSION), "{:?}", line.text);
        assert!(line.text.contains('\u{ab}'), "{:?}", line.text);
        let marks: String = line.text.chars().skip(1).take(2).collect();
        assert_eq!(
            marks,
            format!("{BRAND_MARK}{BRAND_MARK}"),
            "hair dot beside body dot: {:?}",
            line.text
        );
        assert!(
            line.text.contains("Mohawk Pink"),
            "the pet name reads off the foot: {:?}",
            line.text
        );
        assert_eq!(
            line.accent,
            Some((1, 1, crate::theme::Role::AvatarHair(212))),
            "{line:?}"
        );
        assert_eq!(
            line.indicator,
            Some((2, 1, crate::theme::Role::AvatarBody(33))),
            "{line:?}"
        );
        assert_eq!(hit, Some(Hit::Collapse));
    }

    /// A long pet name is cut with an ellipsis rather than pushing the fold
    /// mark off the row, and a narrow column drops the name before the dots.
    #[test]
    fn a_long_name_is_cut_and_a_narrow_column_drops_it() {
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "thin4".into(),
            eyes: "big".into(),
            hair_ink: 208,
            body_ink: 33,
        };
        assert_eq!(pet.name(), "Thin 4 Blocked");
        let (wide, _) = collapse_row_with(false, WIDTH, Some(pet.clone()));
        assert_eq!(crate::width::width(&wide.text), WIDTH);
        assert!(
            wide.text.contains('\u{2026}'),
            "fourteen characters never fit at thirty columns: {wide:?}"
        );
        assert!(wide.text.contains("Thin 4 Bloc"), "{wide:?}");
        let (narrow, _) = collapse_row_with(false, 18, Some(pet));
        assert_eq!(crate::width::width(&narrow.text), 18);
        assert!(
            !narrow.text.contains("Thin"),
            "no smudge at eighteen columns: {narrow:?}"
        );
        assert!(narrow.text.contains(BRAND), "{narrow:?}");
    }

    /// Without an avatar the foot is byte for byte what it always was.
    #[test]
    fn without_an_avatar_the_foot_is_unchanged() {
        let (line, _) = collapse_row_with(false, WIDTH, None);
        assert_eq!(
            line.accent,
            Some((1, crate::width::width(BRAND) + 2, crate::theme::Role::Brand))
        );
        assert_eq!(line.indicator, None);
        assert!(line.text.contains(&format!("{BRAND_MARK} {BRAND}")));
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
        let (line, _) = collapse_row(false, WIDTH);
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
        let (plain, _) = collapse_row(false, WIDTH);
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
            NOW,
            WIDTH,
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
            NOW,
            WIDTH,
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
                NOW,
                WIDTH,
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

    /// Shape says failed: the diamond for failed, the circle for the rest, so the
    /// magenta alarm never stands on colour alone beside the warm pair. Failed is
    /// never news (`is_new` only fires for done/idle), so the diamond is unconditional.
    #[test]
    fn failed_wears_the_diamond_and_every_other_state_the_circle() {
        for state in ["working", "starting", "blocked", "done", "idle", "unknown"] {
            assert_eq!(dot_glyph(crate::theme::of(state)), DOT, "{state}");
        }
        assert_eq!(dot_glyph(crate::theme::of("failed")), FAILED_DOT);
        // And a failed agent renders the diamond in its row text, steady,
        // carrying the failed colour on its indicator.
        let agents = vec![agent(2, 2, "claude", "failed")];
        let lines = render(&[space(2, "webshop")], &agents, 2, 2, 8);
        let row = lines
            .iter()
            .find(|l| l.text.starts_with(" \u{25c6} "))
            .unwrap_or_else(|| {
                panic!(
                    "no diamond row: {:?}",
                    lines.iter().map(|l| &l.text).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            row.indicator,
            Some((DOT_AT, DOT_LEN, crate::theme::of("failed")))
        );
        // Hitting the diamond row reaches the failed terminal.
        let at = lines
            .iter()
            .position(|l| l.text.starts_with(" \u{25c6} "))
            .unwrap();
        assert_eq!(
            hit(&[space(2, "webshop")], &agents, 2, 2, 8, at),
            Some(Hit::Terminal(2))
        );
        // The folded panel draws the same diamond.
        let folded = render_folded(
            &[space(2, "webshop")],
            &agents,
            2,
            2,
            40,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        assert!(
            folded.iter().any(|l| l.text.starts_with(" \u{25c6} ")),
            "no folded diamond: {:?}",
            folded.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        // An unseen failed finish keeps the diamond: failed is never news.
        let mut unseen = Unseen::new();
        unseen.insert((2, gen_of(&agents[0])));
        let fresh_lines = render_folded(
            &[space(2, "webshop")],
            &agents,
            2,
            1,
            40,
            0,
            View::Grouped,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
        );
        assert!(
            fresh_lines.iter().any(|l| l.text.starts_with(" \u{25c6} ")),
            "unseen failed lost its diamond: {:?}",
            fresh_lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    /// The collapsed strip keeps both channels visible too: the state mark
    /// (diamond for failed, circle for the rest) in the state colour, then the
    /// space's middle dot in the space colour, so neither signal paints over
    /// the other and neither sits on an invisible blank.
    #[test]
    fn the_collapsed_strip_draws_a_diamond_for_failed() {
        let agents = vec![
            agent(1, 1, "codex", "blocked"),
            agent(2, 2, "claude", "failed"),
        ];
        let lines = rows_collapsed(&[space(1, "app"), space(2, "webshop")], &agents, 1, 1, 12);
        let texts: Vec<String> = lines.iter().map(|(l, _)| l.text.clone()).collect();
        assert!(
            texts.iter().any(|t| t.starts_with(" \u{25c6}\u{b7}")),
            "no strip diamond: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with(" \u{25cf}\u{b7}")),
            "no strip circle: {texts:?}"
        );
        // The painted result, not just the text: the state mark keeps the
        // state colour while the space dot carries the space.
        let failed = lines
            .iter()
            .find(|(l, _)| l.text.starts_with(" \u{25c6}"))
            .map(|(l, _)| l)
            .expect("no strip diamond line");
        assert_eq!(failed.colour, Some(crate::theme::of("failed")));
        assert_eq!(failed.accent, Some((2, 1, crate::theme::space_of(2))));
    }

    /// Freshness points at done: an idle agent finished while the keyboard was
    /// away wears the done circle, never a diamond, because failed is never news.
    #[test]
    fn a_fresh_idle_wears_the_done_circle() {
        let agents = vec![agent(6, 1, "claude", "idle")];
        let mut unseen = Unseen::new();
        unseen.insert((6, gen_of(&agents[0])));
        assert!(is_new(&agents[0], 1, &Seen::new(), &unseen));
        let lines = render_folded(
            &[space(1, "app")],
            &agents,
            1,
            1,
            40,
            0,
            View::Grouped,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
        );
        let row = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .expect("no agent row");
        assert!(
            row.text.starts_with(" \u{25cf} "),
            "fresh idle is not a circle: {:?}",
            row.text
        );
        assert_eq!(
            row.indicator,
            Some((DOT_AT, DOT_LEN, crate::theme::of("done")))
        );
    }

    #[test]
    fn view_toggle_flips_between_grouped_and_priority() {
        assert_eq!(View::Grouped.toggle(), View::Priority);
        assert_eq!(View::Priority.toggle(), View::Grouped);
        assert_eq!(View::default(), View::Grouped);
    }

    /// The reported bug: a working agent listed after idle ones must stay
    /// last while grouped, and so must a blocked one. Grouped never reorders;
    /// urgency is the priority view's job, where both jump to the top.
    #[test]
    fn grouped_keeps_daemon_order_while_priority_sorts_by_urgency() {
        let spaces = vec![space(1, "app")];
        for agents in [
            vec![
                agent(1, 1, "opencode", "idle"),
                agent(2, 1, "opencode", "idle"),
                agent(3, 1, "claude", "working"),
            ],
            vec![
                agent(1, 1, "opencode", "idle"),
                agent(2, 1, "opencode", "idle"),
                agent(3, 1, "claude", "blocked"),
            ],
        ] {
            for view in [View::Grouped, View::Priority] {
                let rows = rows_folded(
                    &spaces,
                    &agents,
                    1,
                    1,
                    20,
                    0,
                    view,
                    &Seen::new(),
                    &Unseen::new(),
                    NOW,
                    WIDTH,
                );
                let terminals: Vec<u32> = rows
                    .iter()
                    .filter_map(|(_, hit)| match hit {
                        Some(Hit::Terminal(t)) => Some(*t),
                        _ => None,
                    })
                    .collect();
                // Two rows per agent, first row first: dedup keeps the order.
                let mut order = Vec::new();
                for t in terminals {
                    if !order.contains(&t) {
                        order.push(t);
                    }
                }
                if view == View::Grouped {
                    assert_eq!(order, vec![1, 2, 3], "grouped reordered: {order:?}");
                } else {
                    assert_eq!(order, vec![3, 1, 2], "priority did not sort: {order:?}");
                }
            }
        }
    }

    #[test]
    fn only_the_mode_word_of_the_agents_title_flips_the_view() {
        for view in [View::Grouped, View::Priority] {
            assert_eq!(
                agents_title_hit(view, 0, WIDTH),
                None,
                "the agents word is nothing"
            );
            assert_eq!(
                agents_title_hit(view, 5, WIDTH),
                None,
                "mid title is nothing"
            );
            assert_eq!(
                agents_title_hit(view, WIDTH - 1, WIDTH),
                Some(Hit::View),
                "the mode corner flips"
            );
        }
    }

    #[test]
    fn one_breath_sits_between_the_agents_title_and_the_rows() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "idle")];
        let (content, shown, total) = agents_content(
            &spaces,
            &agents,
            1,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let laid = lay_out_agents(content, shown, total, 1, 10, View::Grouped, WIDTH);
        assert!(
            laid[0].0.text.starts_with(" agents"),
            "{:?}",
            laid[0].0.text
        );
        assert_eq!(laid[1].0.text.trim(), "", "no gap under the switch");
        assert!(laid[1].1.is_none(), "the gap is not clickable");
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
            NOW,
            WIDTH,
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
            NOW,
            WIDTH,
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
                &Unseen::new(),
                NOW,
                WIDTH
            ),
            None,
            "the title row must not open a space: {texts:?}"
        );
        assert_eq!(spaces_title_hit(5, WIDTH), None);
        assert_eq!(spaces_title_hit(WIDTH, WIDTH), Some(Hit::NewSpace));
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
                        NOW,
                        WIDTH,
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
            NOW,
            WIDTH,
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
            NOW,
            WIDTH,
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

    /// G1: a stale idle row dims as a whole and sinks below fresh idle of the
    /// same rank in the priority view, while the grouped view never moves.
    ///
    /// Dim is exact roles: first line `Faint` with a `Faint` dot, second line
    /// `Faint`, and the space accent still the space's own ink. News beats
    /// dim: a stale row the keyboard has not read keeps its done sage.
    #[test]
    fn idle_stale_dims_and_sinks_in_priority_only() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent_at(1, 1, "claude", "idle", 10_000),
            agent_at(2, 1, "codex", "idle", 60),
        ];
        let priority = render_folded(
            &spaces,
            &agents,
            1,
            9,
            14,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let order: Vec<String> = priority
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        // First lines carry the program (no titles on these rows); the fresh
        // codex sorts above the stale claude of the same rank.
        let fresh_at = order
            .iter()
            .position(|t| t.contains("codex"))
            .expect("fresh row");
        let stale_at = order
            .iter()
            .position(|t| t.contains("claude"))
            .expect("stale row");
        // Second lines read `app - <tab>`; first lines carry the session.
        // The fresh idle sorts above the stale one of the same rank.
        assert!(fresh_at < stale_at, "{order:?}");
        let stale_first = &priority[stale_at];
        assert_eq!(stale_first.colour, Some(crate::theme::Role::Faint));
        assert_eq!(
            stale_first.indicator,
            Some((DOT_AT, DOT_LEN, crate::theme::Role::Faint))
        );
        let stale_second = &priority[stale_at + 1];
        assert_eq!(stale_second.colour, Some(crate::theme::Role::Faint));
        assert_eq!(
            stale_second.accent,
            Some((3, crate::width::width("app"), crate::theme::space_of(1))),
            "the space accent never dims with the state"
        );
        // Dim is roles only: the stale row's texts read exactly as its
        // undimmed twin's. Compared per row, not per column: recency moves
        // the fresh twin above, which is the sink this test already pinned.
        let twins = render_folded(
            &spaces,
            &[
                agent_at(1, 1, "claude", "idle", 60),
                agent_at(2, 1, "codex", "idle", 60),
            ],
            1,
            9,
            14,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let twin_claude: Vec<String> = twins
            .iter()
            .filter(|l| l.text.contains("claude") || l.text.contains("app - claude"))
            .map(|l| l.text.clone())
            .collect();
        let stale_texts: Vec<String> = priority[stale_at..stale_at + 2]
            .iter()
            .map(|l| l.text.clone())
            .collect();
        assert_eq!(stale_texts, twin_claude);

        // Grouped keeps daemon order, dimmed or not.
        let grouped = render_folded(
            &spaces,
            &agents,
            1,
            9,
            14,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let gorder: Vec<String> = grouped
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect();
        let g1 = gorder
            .iter()
            .position(|t| t.contains("claude"))
            .expect("row 1");
        let g2 = gorder
            .iter()
            .position(|t| t.contains("codex"))
            .expect("row 2");
        assert!(g1 < g2, "grouped moved a row: {gorder:?}");
    }

    /// G1: staleness never fades news. A stale idle that finished while the
    /// keyboard was away keeps the done sage instead of dimming.
    #[test]
    fn news_beats_stale_dim() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent_at(1, 1, "claude", "idle", 10_000)];
        let mut unseen = Unseen::new();
        unseen.insert((1, 0));
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            9,
            14,
            0,
            View::Priority,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
        );
        assert!(
            lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::of("done")))),
            "a new row dimmed instead of keeping its sage: {:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::Role::Faint))),
            "a new row faded"
        );
        let first = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .expect("a dot row");
        assert_eq!(
            first.colour,
            None,
            "a new row's first line dimmed: {:?}",
            lines.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    /// G2: an idle row without a stamp paints byte for byte as it always did,
    /// at every clock: the whole painted column is identical, not just the
    /// dot.
    #[test]
    fn idle_without_a_stamp_paints_as_it_always_did() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "claude", "idle")];
        let paint = |now| {
            render_folded(
                &spaces,
                &agents,
                1,
                1,
                14,
                0,
                View::Grouped,
                &Seen::new(),
                &Unseen::new(),
                now,
                WIDTH,
            )
        };
        assert_eq!(paint(0), paint(NOW));
        assert_eq!(paint(NOW), paint(NOW + 1_000_000));
    }

    /// G3: tier boundaries resolve at paint, exactly at 900 and 7200 seconds.
    /// A repaint with no arrival between the two clocks shows the crossing,
    /// which is the contract the deadline arm relies on.
    #[test]
    fn tier_boundaries_resolve_at_paint() {
        assert_eq!(tier_of("idle", Some(NOW - 899), NOW), Some(Tier::Fresh));
        assert_eq!(tier_of("idle", Some(NOW - 900), NOW), Some(Tier::Fresh));
        assert_eq!(tier_of("idle", Some(NOW - 901), NOW), Some(Tier::Plain));
        assert_eq!(tier_of("idle", Some(NOW - 7199), NOW), Some(Tier::Plain));
        assert_eq!(tier_of("idle", Some(NOW - 7200), NOW), Some(Tier::Plain));
        assert_eq!(tier_of("idle", Some(NOW - 7201), NOW), Some(Tier::Stale));
        // Tiers never apply off idle, or without a stamp.
        assert_eq!(tier_of("working", Some(NOW - 99_999), NOW), None);
        assert_eq!(tier_of("idle", None, NOW), None);
        // A stamp ahead of this clock reads as just stopped, not ancient.
        assert_eq!(tier_of("idle", Some(NOW + 60), NOW), Some(Tier::Fresh));

        // No arrival between the paints: the same rows cross from plain to
        // stale on the clock alone.
        let spaces = vec![space(1, "app")];
        let agents = vec![agent_at(1, 1, "claude", "idle", 7199)];
        let paint = |now| {
            render_folded(
                &spaces,
                &agents,
                1,
                9,
                14,
                0,
                View::Priority,
                &Seen::new(),
                &Unseen::new(),
                now,
                WIDTH,
            )
        };
        let before = paint(NOW);
        assert!(
            before
                .iter()
                .all(|l| l.indicator != Some((DOT_AT, DOT_LEN, crate::theme::Role::Faint))),
            "plain row already dim"
        );
        let after = paint(NOW + 2);
        assert!(
            after
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, DOT_LEN, crate::theme::Role::Faint))),
            "stale row never dimmed without an arrival"
        );
    }

    /// G3: the deadline arm fires at most twice per idle row, minute-rounded
    /// up, and asks for nothing when there is nothing to cross.
    #[test]
    fn the_deadline_arms_once_per_crossing_and_nothing_more() {
        // Stamped a minute ago: fresh ends at +900, rounded up to the minute.
        let row = agent_at(1, 1, "claude", "idle", 60);
        assert_eq!(wants_tick(&[row], NOW), Some(((NOW + 840) + 59) / 60 * 60));
        // Plain: the stale boundary is next.
        let row = agent_at(1, 1, "claude", "idle", 1000);
        assert_eq!(wants_tick(&[row], NOW), Some(((NOW + 6200) + 59) / 60 * 60));
        // Past every boundary, unstamped, not idle, not an agent: silence.
        let mut old = agent_at(1, 1, "claude", "idle", 99_999);
        assert_eq!(wants_tick(&[old.clone()], NOW), None);
        old.state = "working".into();
        assert_eq!(wants_tick(&[old], NOW), None);
        assert_eq!(wants_tick(&[agent(1, 1, "claude", "idle")], NOW), None);
        let mut shell = agent(2, 1, "sh", "idle");
        shell.agent = false;
        shell.last_active_at_unix = Some(NOW - 60);
        assert_eq!(wants_tick(&[shell], NOW), None);
        // The earliest crossing wins across rows.
        let rows = vec![
            agent_at(1, 1, "claude", "idle", 1000),
            agent_at(2, 1, "codex", "idle", 60),
        ];
        assert_eq!(wants_tick(&rows, NOW), Some(((NOW + 840) + 59) / 60 * 60));
    }

    /// G6: recency orders the calm ranks newest-first and nothing else. A
    /// blocked row stays above every idle no matter how fresh the idle is,
    /// working rows keep daemon order under recency change, equal stamps
    /// fall back to daemon order, and unstamped rows sink within rank.
    #[test]
    fn priority_recency_breaks_calm_ties_only() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent_at(1, 1, "codex", "idle", 60),
            agent(2, 1, "claude", "blocked"),
            agent_at(3, 1, "gemini", "working", 5000),
            agent_at(4, 1, "opencode", "working", 60),
            agent_at(5, 1, "amp", "idle", 5000),
            agent(6, 1, "droid", "idle"),
        ];
        let terminal_order = |view| {
            rows_folded(
                &spaces,
                &agents,
                1,
                9,
                40,
                0,
                view,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
            )
            .into_iter()
            .filter_map(|(_, hit)| match hit {
                Some(Hit::Terminal(t)) => Some(t),
                _ => None,
            })
            // Two rows per agent: keep first appearances.
            .fold(
                (Vec::new(), std::collections::BTreeSet::new()),
                |(mut v, mut s), t| {
                    if s.insert(t) {
                        v.push(t);
                    }
                    (v, s)
                },
            )
            .0
        };
        assert_eq!(
            terminal_order(View::Priority),
            vec![2, 3, 4, 1, 5, 6],
            "blocked first, working in daemon order, idle newest-first, unstamped last"
        );
        assert_eq!(
            terminal_order(View::Grouped),
            vec![1, 2, 3, 4, 5, 6],
            "grouped keeps daemon order"
        );

        // Done rows take recency too: an older done sinks below a fresh idle
        // of the next rank down, newest done first. Marked seen first: an
        // unread done is news (rank 2), and news outranks recency by design.
        let agents = vec![
            agent_at(1, 1, "codex", "done", 5000),
            agent_at(2, 1, "claude", "idle", 60),
            agent_at(3, 1, "gemini", "done", 60),
        ];
        let mut seen = Seen::new();
        seen.insert((1, 0));
        seen.insert((3, 0));
        let done_order: Vec<u32> = rows_folded(
            &spaces,
            &agents,
            1,
            9,
            40,
            0,
            View::Priority,
            &seen,
            &Unseen::new(),
            NOW,
            WIDTH,
        )
        .into_iter()
        .filter_map(|(_, hit)| match hit {
            Some(Hit::Terminal(t)) => Some(t),
            _ => None,
        })
        .fold(
            (Vec::new(), std::collections::BTreeSet::new()),
            |(mut v, mut s), t| {
                if s.insert(t) {
                    v.push(t);
                }
                (v, s)
            },
        )
        .0;
        assert_eq!(
            done_order,
            vec![3, 1, 2],
            "done newest-first, then the fresher rank below"
        );

        // The focused row is never news even when it is the freshest: it
        // keeps its calm rank instead of rising to the new rank.
        let agents = vec![
            agent_at(1, 1, "claude", "idle", 60),
            agent_at(2, 1, "codex", "idle", 5000),
        ];
        let mut unseen = Unseen::new();
        unseen.insert((1, 0));
        unseen.insert((2, 0));
        let order: Vec<u32> = rows_folded(
            &spaces,
            &agents,
            1,
            1,
            40,
            0,
            View::Priority,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
        )
        .into_iter()
        .filter_map(|(_, hit)| match hit {
            Some(Hit::Terminal(t)) => Some(t),
            _ => None,
        })
        .fold(
            (Vec::new(), std::collections::BTreeSet::new()),
            |(mut v, mut s), t| {
                if s.insert(t) {
                    v.push(t);
                }
                (v, s)
            },
        )
        .0;
        // Terminal 2 is new (rank 2) and leads; terminal 1 is focused, so it
        // is not new and stays in its calm rank behind it.
        assert_eq!(order, vec![2, 1]);

        // Equal stamps fall back to daemon order.
        let agents = vec![
            agent_at(2, 1, "codex", "idle", 500),
            agent_at(1, 1, "claude", "idle", 500),
        ];
        let order: Vec<u32> = rows_folded(
            &spaces,
            &agents,
            1,
            9,
            40,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        )
        .into_iter()
        .filter_map(|(_, hit)| match hit {
            Some(Hit::Terminal(t)) => Some(t),
            _ => None,
        })
        .fold(
            (Vec::new(), std::collections::BTreeSet::new()),
            |(mut v, mut s), t| {
                if s.insert(t) {
                    v.push(t);
                }
                (v, s)
            },
        )
        .0;
        assert_eq!(order, vec![2, 1], "equal stamps keep daemon order");
    }

    /// A session longer than the row ends in an ellipsis rather than a hard
    /// cut, and only then: a fitting session keeps today's bytes exactly,
    /// wide titles never split, and the row still measures thirty columns.
    #[test]
    fn a_long_session_ends_in_an_ellipsis() {
        let spaces = vec![space(1, "app")];
        let fitting = agent(1, 1, "claude", "working");
        let long_title = "Implement OAuth scopes and refresh the dispatcher";
        let mut long = agent(2, 1, "claude", "working");
        long.agent_title = Some(long_title.into());
        let mut wide = agent(3, 1, "claude", "working");
        wide.agent_title = Some("程程程程程程程程程程程程程程程".into());
        let lines = render_folded(
            &spaces,
            &[fitting, long, wide],
            1,
            9,
            20,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let firsts: Vec<String> = lines
            .iter()
            .filter(|l| l.indicator.is_some())
            .map(|l| l.text.clone())
            .collect();
        assert_eq!(firsts.len(), 3);
        for first in &firsts {
            assert_eq!(crate::width::width(first), WIDTH, "{first:?}");
            assert!(!first.contains('\u{fffd}'));
        }
        // Fitting session: today's bytes, no ellipsis.
        assert_eq!(firsts[0].trim_end(), " \u{25cf} claude");
        // Cut sessions: the dot and the ellipsis frame the text.
        assert!(firsts[1].trim_end().ends_with('…'), "{:?}", firsts[1]);
        assert!(
            firsts[1].starts_with(" \u{25cf} Implement"),
            "{:?}",
            firsts[1]
        );
        assert!(firsts[2].trim_end().ends_with('…'), "{:?}", firsts[2]);
        // A session that already ends in an ellipsis is cut once, not twice.
        let mut dotty = agent(4, 1, "claude", "working");
        dotty.agent_session = Some("abcdefghijklmnop…".into());
        let lines = render_folded(
            &spaces,
            &[dotty],
            1,
            9,
            20,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let first = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .expect("a dot row");
        assert_eq!(first.text.chars().filter(|c| *c == '…').count(), 1);
        assert_eq!(crate::width::width(&first.text), WIDTH);
    }

    /// G4: the place row shows the worktree basename when the space name
    /// does not already say it, and the edge cases stay today's row.
    #[test]
    fn place_line_shows_the_worktree_basename_when_different() {
        fn second_row(space_name: &str, cwd: Option<&str>, tab_program: &str) -> String {
            let spaces = vec![space(1, space_name)];
            let mut row = agent(1, 1, tab_program, "idle");
            row.cwd = cwd.map(str::to_string);
            render_folded(
                &spaces,
                &[row],
                1,
                9,
                10,
                0,
                View::Grouped,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
            )
            .into_iter()
            .map(|l| l.text.trim_end().to_string())
            .find(|t| t.starts_with("   ") && t.contains(" - "))
            .expect("a place row")
        }
        // Different: space, worktree, tab.
        assert_eq!(
            second_row("app", Some("/home/u/app-feat-x"), "claude"),
            "   app - app-feat-x - claude"
        );
        // Trailing slash is not a name.
        assert_eq!(
            second_row("app", Some("/home/u/app-feat-x/"), "claude"),
            "   app - app-feat-x - claude"
        );
        // Same name in any case: today's row.
        assert_eq!(
            second_row("app", Some("/home/u/app"), "claude"),
            "   app - claude"
        );
        assert_eq!(
            second_row("app", Some("/home/u/app"), "claude"),
            "   app - claude"
        );
        // Root, empty, blank: no basename, today's row.
        assert_eq!(second_row("app", Some("/"), "claude"), "   app - claude");
        assert_eq!(second_row("app", Some(""), "claude"), "   app - claude");
        assert_eq!(second_row("app", Some("   "), "claude"), "   app - claude");
        // No cwd at all: today's row.
        assert_eq!(second_row("app", None, "claude"), "   app - claude");
        // A tilde and dots are names and pass through untouched.
        assert_eq!(
            second_row("app", Some("~"), "claude"),
            "   app - ~ - claude"
        );
        assert_eq!(
            second_row("app", Some("/home/u/."), "claude"),
            "   app - . - claude"
        );
    }

    /// G4: the same worktree name under two spaces stays ambiguous on
    /// purpose: each row names its own space first, so the space tells the
    /// two apart while the middle reads the same.
    #[test]
    fn the_same_basename_under_two_spaces_keeps_both_space_names() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let mut a = agent(1, 1, "claude", "idle");
        a.cwd = Some("/home/u/x".into());
        let mut b = agent(2, 2, "codex", "idle");
        b.cwd = Some("/home/u/x".into());
        let lines = render_folded(
            &spaces,
            &[a, b],
            1,
            9,
            16,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let seconds: Vec<String> = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .filter(|t| t.starts_with("   ") && t.contains(" - "))
            .collect();
        assert_eq!(seconds, vec!["   app - x - claude", "   dub - x - codex"]);
    }

    /// G4: a long middle shortens tail-first with an ellipsis while the space
    /// head and tab tail survive; too narrow for even that and the middle
    /// drops out entirely.
    #[test]
    fn place_line_shortens_the_middle_first() {
        let spaces = vec![space(1, "app")];
        let mut row = agent(1, 1, "claude", "idle");
        row.cwd = Some("/home/u/a-very-long-worktree-name-that-keeps-going".into());
        let lines = render_folded(
            &spaces,
            &[row],
            1,
            9,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let second = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .find(|t| t.starts_with("   ") && t.contains(" - "))
            .expect("a place row");
        assert!(second.starts_with("   app - "), "{second:?}");
        assert!(second.ends_with(" - claude"), "{second:?}");
        assert!(second.contains('…'), "{second:?}");
        assert_eq!(crate::width::width(&second), WIDTH);

        // A tab so long the middle cannot fit at all: today's row, intact.
        let mut row = agent(1, 1, &"c".repeat(40), "idle");
        row.cwd = Some("/home/u/somewhere".into());
        let lines = render_folded(
            &spaces,
            &[row],
            1,
            9,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let second = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .find(|t| t.starts_with("   ") && t.contains(" - "))
            .expect("a place row");
        assert!(!second.contains("somewhere"), "{second:?}");
    }

    /// G4: an overlong wide middle shortens by columns, never splitting a
    /// wide character, and the row stays thirty columns.
    #[test]
    fn place_line_shortens_a_wide_middle_by_columns() {
        let spaces = vec![space(1, "app")];
        let mut row = agent(1, 1, "claude", "idle");
        row.cwd = Some("/home/u/程程程程程程程程程程程程".into());
        let lines = render_folded(
            &spaces,
            &[row],
            1,
            9,
            10,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let second = lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .find(|t| t.starts_with("   ") && t.contains(" - "))
            .expect("a place row");
        // Room is twelve columns: five wide letters and the ellipsis, plus
        // the pad column `fit_to` adds to reach thirty.
        assert_eq!(second, "   app - 程程程程程… - claude", "{second:?}");
        assert_eq!(crate::width::width(&second), WIDTH - 1);
        let raw = lines
            .iter()
            .find(|l| l.text.trim_end().starts_with("   ") && l.text.contains(" - "))
            .expect("a raw place row");
        assert_eq!(crate::width::width(&raw.text), WIDTH);
        assert!(!second.contains('\u{fffd}'));
    }

    /// G5/G13: worktree middles, wide names and long names all still
    /// measure exactly thirty columns, and accents never leave the space name.
    #[test]
    fn every_decorated_row_still_measures_thirty_columns() {
        let spaces = vec![space(1, "app")];
        let mut rows = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "blocked"),
            agent(3, 1, "gemini", "failed"),
            agent(4, 1, "cursor", "done"),
            agent(5, 1, "aider", "starting"),
            agent(6, 1, "opencode", "idle"),
        ];
        rows[0].agent_title = Some("Implement OAuth scopes and refresh".into());
        rows[1].cwd = Some("/home/u/app-feat-x".into());
        rows[2].cwd = Some("/home/u/程".repeat(8));
        rows[5].name = Some("release".into());
        let lines = render_folded(
            &spaces,
            &rows,
            1,
            9,
            30,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        for line in &lines {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
            assert!(!line.text.contains('\u{fffd}'));
            if let Some((at, len, _)) = line.accent {
                assert!(at + len <= WIDTH, "accent runs past the row: {line:?}");
            }
        }
    }

    /// G13: a space name longer than the row cannot break the painter. The
    /// accent that would run past the row is dropped where every overflowing
    /// run is dropped, and the row still measures thirty columns. Pre-existing
    /// behavior, pinned because the place row carries the accent.
    #[test]
    fn a_space_name_longer_than_the_row_drops_its_accent_gracefully() {
        let spaces = vec![space(1, &"s".repeat(40))];
        let agents = vec![agent(1, 1, "claude", "working")];
        let lines = render_folded(
            &spaces,
            &agents,
            1,
            9,
            14,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        for line in &lines {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
    }

    /// Wrapper rows and unknown programs keep the dot alone: the wrapper
    /// reads as what stands in its way, the unknown program under its own
    /// name, neither with an accent on the first row.
    #[test]
    fn wrapper_and_unknown_rows_keep_the_dot_alone() {
        let spaces = vec![space(1, "app")];
        let mut wrapper = agent(2, 1, "sh", "working");
        wrapper.program = None;
        wrapper.behind = Some("docker".into());
        let mut strange = agent(3, 1, "sh", "working");
        strange.program = Some("Muse".into());
        let lines = render_folded(
            &spaces,
            &[wrapper, strange],
            1,
            9,
            30,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let firsts: Vec<&Line> = lines.iter().filter(|l| l.indicator.is_some()).collect();
        assert_eq!(firsts.len(), 2);
        for bare in &firsts {
            assert_eq!(bare.accent, None, "{:?}", bare.text);
        }
        assert!(
            firsts[0].text.starts_with(" \u{25cf} behind docker"),
            "{:?}",
            firsts[0].text
        );
        assert!(
            firsts[1].text.starts_with(" \u{25cf} Muse"),
            "{:?}",
            firsts[1].text
        );
    }

    /// G11: the collapsed strip is byte-for-byte what it was: worktrees
    /// upstream change nothing in four columns.
    #[test]
    fn the_collapsed_strip_ignores_worktrees() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let mut a = agent_at(1, 1, "claude", "idle", 10_000);
        a.cwd = Some("/home/u/app-feat-x".into());
        let mut b = agent(2, 2, "codex", "blocked");
        b.cwd = Some("/home/u/dub".into());
        let before = rows_collapsed(&spaces, &[a, b], 1, 1, 12);
        let plain_a = agent(1, 1, "sh", "idle");
        let plain_b = agent(2, 2, "sh", "blocked");
        let after = rows_collapsed(&spaces, &[plain_a, plain_b], 1, 1, 12);
        assert_eq!(before, after);
        for (line, _) in &before {
            assert_eq!(crate::width::width(&line.text), COLLAPSED);
        }
    }

    /// G12: reordered priority rows keep hit parity: every painted agent row
    /// still opens its own terminal, and the focused row never scrolls out.
    #[test]
    fn hit_parity_holds_under_priority_reorder() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent_at(1, 1, "codex", "idle", 60),
            agent_at(2, 1, "claude", "idle", 5000),
            agent(3, 1, "gemini", "blocked"),
        ];
        // Tiny heights collapse the panel to its title, and the fold mark
        // eats the last row: parity is pinned where agent pairs draw whole.
        for height in [10usize, 14, 30] {
            let rows = rows_folded(
                &spaces,
                &agents,
                1,
                3,
                height,
                0,
                View::Priority,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
            );
            // The focused blocked row draws somewhere inside the window.
            assert!(
                rows.iter().any(|(_, hit)| *hit == Some(Hit::Terminal(3))),
                "focused row scrolled out at height {height}"
            );
            for (row, (_, hit)) in rows.iter().enumerate() {
                assert_eq!(
                    hit_folded(
                        &spaces,
                        &agents,
                        1,
                        3,
                        height,
                        row,
                        View::Priority,
                        &Seen::new(),
                        &Unseen::new(),
                        NOW,
                        WIDTH,
                    ),
                    *hit,
                    "hit disagrees with paint at row {row}, height {height}"
                );
            }
        }
    }

    /// Phase B: the same rows paint at any pulled width and measure exactly
    /// that, titles and place rows included. The default width reads what it
    /// always read.
    #[test]
    fn the_same_rows_paint_at_any_pulled_width() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "claude", "working");
        a.cwd = Some("/home/u/app-feat-x".into());
        let mut b = agent(2, 1, "codex", "blocked");
        b.agent_title = Some("Wire retry budget into dispatcher".into());
        let agents = vec![a, b];
        for width in [SIDEBAR_MIN, 22, WIDTH, SIDEBAR_MAX] {
            let lines = render_folded(
                &spaces,
                &agents,
                1,
                9,
                20,
                0,
                View::Grouped,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                width,
            );
            for line in &lines {
                assert_eq!(
                    crate::width::width(&line.text),
                    width,
                    "width {width}: {line:?}"
                );
                assert!(!line.text.contains('\u{fffd}'));
            }
            // The worktree middle survives at every width; the narrowest
            // column keeps at least its shortened form.
            assert!(
                lines.iter().any(|l| l.text.contains("app")),
                "width {width}: space name lost"
            );
            // Corners and geometry follow the pulled width, not the default.
            assert_eq!(
                agents_title_hit(View::Grouped, width, width),
                Some(Hit::View),
                "width {width}: mode corner missed"
            );
            assert_eq!(
                spaces_title_hit(width, width),
                Some(Hit::NewSpace),
                "width {width}: plus corner missed"
            );
            assert_eq!(
                footer_hit(width - 1, width),
                Some(Hit::Menu),
                "width {width}: menu corner missed"
            );
        }
        // Default width reads what the legacy surface always read: every
        // agent row the legacy panel draws appears verbatim in the live one
        // (the live panel adds a breath under its title and a fold mark at
        // its foot, which are chrome, not rows).
        let live = render_folded(
            &spaces,
            &agents,
            1,
            9,
            20,
            0,
            View::Grouped,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let legacy = render(&spaces, &agents, 1, 9, 20);
        for row in &legacy {
            let text = row.text.trim_end();
            if text.contains("claude")
                || text.contains("codex")
                || text.contains("app -")
                || text.contains("Wire retry")
            {
                assert!(
                    live.iter().any(|l| l.text == row.text),
                    "legacy row lost: {text:?}"
                );
            }
        }
    }
}

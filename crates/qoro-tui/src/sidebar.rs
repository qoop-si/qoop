//! The left column: who needs a person on top, then every space with its agents under it.
//!
//! One column and not two panels. The two questions a person asks of it are still two
//! questions, and each keeps a place of its own: "who needs me" is the pin at the top,
//! which crosses spaces on purpose and exists only while somebody does, and "which
//! project am I in" is the tree below it, one rule per space with that space's agents
//! under it. An agent that blocked in a space a person is not looking at is exactly the
//! one they need to be told about, so the pin lists it even when its space is folded.
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// What this row is, which is what decides how it is painted. `None` for a row with
    /// nothing to say about itself, which the painter draws as ordinary text.
    pub colour: Option<crate::theme::Role>,
    /// A run inside the row painted in a role of its own: the column it starts at, how
    /// many columns it covers, and what it is. The name of a space, in that space's
    /// colour, wherever a row names one.
    pub accent: Option<(usize, usize, crate::theme::Role)>,
    /// The row's opening sign: the state dot at the start of an agent's
    /// row, or the fold arrow of a space rule, in the colour of the state it
    /// speaks for. Selection is the raised background the painter adds, never a mark.
    pub indicator: Option<(usize, usize, crate::theme::Role)>,
    /// Any further runs: the dashes of a space rule, a count, a quiet worktree name, a
    /// sweeping activity. The painter merges them with the two above by column; runs
    /// never overlap by construction. Still few per row: a row saying five things in
    /// five colours is a row a person parses rather than reads, so every run here is
    /// quieter than the row's own text or the same state colour the dot already wears.
    pub runs: Vec<(usize, usize, crate::theme::Role)>,
}

/// The colour a state earns.
///
/// Deliberately few: a person scanning this column is looking for the one row that needs
/// them, and a column where every row is a different colour is a column with no signal in
/// it. Blocked and failed are the two that mean "come here", so they are the two that are
/// warm. The settled states, done and sleeping, earn quiet hues of their own; idle stays
/// the uncoloured background, because a colour on every row is still forbidden and idle
/// is every other row.
pub fn colour_for(state: &str) -> Option<crate::theme::Role> {
    match state {
        "blocked" | "failed" | "working" | "starting" | "done" | "sleeping" => {
            Some(crate::theme::of(state))
        }
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

/// Titles, which are rows of the column and not decoration the caller adds.
///
/// A person opening this for the first time sees lists of names with nothing saying
/// what either is. Keeping the titles inside the returned rows also keeps the arithmetic in
/// one place: a caller that drew its own titles would have to know how many rows this
/// function had already spent on them.
const SPACES_TITLE: &str = "spaces";
const PIN_TITLE: &str = "needs you";

/// The most agents the pin lists before it says how many more there are.
///
/// Five: the pin is a place to glance at, and past five rows it is a second list
/// competing with the tree for the column. The rest are counted, and every one of them
/// is still a row in its space below.
const PIN_MOST: usize = 5;

/// Where an agent's dot sits under its space rule: three columns in, so the dot lines
/// up under the space name the rule carries.
const CHILD_DOT_AT: usize = 3;

/// The columns of a space rule a click folds with: the leading dash, the arrow and the
/// space after it. Anywhere else on the rule moves to the space.
pub const FOLD_CELLS: usize = 3;

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

/// What the left column drew on one screen row, so a click can act on it.
///
/// One answer per row. The rows that do two things answer with the row, and the
/// column a click landed on decides which, through [`act`]: a space rule folds at its
/// arrow and moves to the space anywhere else, the title flips the order at its word and
/// makes a space at its `+`, the foot opens the menu at its word and folds the column
/// anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Space(i64),
    Terminal(u32),
    /// The `spaces` title, with the order word and the `+` at its right.
    Title,
    /// The foot of the column: the brand, `menu`, and the mark that folds the column.
    Foot,
}

/// What a click on the column asks for, once the column it landed on is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Space(i64),
    /// Fold a space's agents away under its rule, or open them again.
    Fold(i64),
    Terminal(u32),
    /// The `+` of the title: a space, named first.
    NewSpace,
    /// The `tree` or `priority` word of the title: the order below, and its switch.
    View,
    /// Everything that is not a space, a tab or an agent.
    Menu,
    /// Fold the whole column away to its strip, and back.
    Collapse,
}

/// How the column orders its agents: the tree of spaces, or flat by urgency.
///
/// Tree is the column a person already knows: every space as a rule with its agents
/// under it in the daemon's order, kept across state changes, so a row never moves
/// under the eye. Priority is the same agents in one list sorted so the one that
/// needs a person is always on top: the inbox view of the fleet. The title names the
/// order. Toggled with `ctrl-a g`, or with a click on the `tree`/`priority` word itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Tree,
    Priority,
}

impl View {
    /// The other one: a toggle carries no state of its own.
    pub fn toggle(self) -> View {
        match self {
            View::Tree => View::Priority,
            View::Priority => View::Tree,
        }
    }
}

/// The spaces whose agents a person folded away under their rule.
///
/// Client-local, like the column's own fold and the order: folding a space here does
/// not fold it for another client. Pruned by the caller when a space goes away.
pub type Shut = std::collections::BTreeSet<i64>;

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
/// is reading it. Same sage dot as `done`, no new theme, and the pin lists it.
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
        // Slept tabs sink below idle: settled, visible, never attention.
        "sleeping" => 7,
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

/// Everything the column is drawn from, besides the rows it is given.
///
/// One value and not a dozen arguments: the paint, the hit test and the strip all read
/// the same pieces, and a list of loose arguments is how two of them came to disagree
/// about which order the pieces went in.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    pub spaces: &'a [WorkspaceRow],
    pub agents: &'a [AgentRow],
    pub focused_space: i64,
    pub focused_terminal: u32,
    /// The animation frame. Hit testing passes zero: where a row is never depends on it.
    pub tick: u64,
    pub view: View,
    pub seen: &'a Seen,
    pub unseen: &'a Unseen,
    /// Paint-time unix seconds, read once per frame by the caller so one paint never
    /// disagrees with itself.
    pub now_unix: i64,
    /// The live column width, at or above [`SIDEBAR_MIN`] on the live path.
    pub width: usize,
    pub shut: &'a Shut,
}

/// One row of the column, and what a click on it means.
type Row = (Line, Option<Hit>);

/// Whether a row wants a person: blocked, failed, or finished while nobody looked.
///
/// One answer for every place that asks: the pin at the top of the column, the colour of
/// a folded space's arrow, and the status line's triage.
pub fn needs_person(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> bool {
    matches!(agent.state.as_str(), "blocked" | "failed") || is_new(agent, focused, seen, unseen)
}

/// The most urgent row among `rows` that wants a person, the first one on a tie: blocked,
/// then failed, then finished while nobody looked. One answer for the pin, a folded
/// space's arrow, the strip and the status line's triage.
pub fn most_urgent<'a>(
    rows: impl IntoIterator<Item = &'a AgentRow>,
    focused: u32,
    seen: &Seen,
    unseen: &Unseen,
) -> Option<&'a AgentRow> {
    rows.into_iter()
        .filter(|a| needs_person(a, focused, seen, unseen))
        .fold(None, |best: Option<&AgentRow>, a| match best {
            Some(b)
                if priority_of(b, focused, seen, unseen)
                    <= priority_of(a, focused, seen, unseen) =>
            {
                Some(b)
            }
            _ => Some(a),
        })
}

/// The colour the most urgent of `rows` lends a folded space: its state's own, or done's
/// sage for news. `None` when nobody among them wants a person.
fn urgent_ink(c: &Column, rows: &[&AgentRow]) -> Option<crate::theme::Role> {
    most_urgent(rows.iter().copied(), c.focused_terminal, c.seen, c.unseen).map(|a| {
        if is_new(a, c.focused_terminal, c.seen, c.unseen) {
            crate::theme::of("done")
        } else {
            crate::theme::of(&a.state)
        }
    })
}

/// The open column, painted into `height` rows: the pin, the title, the tree or the
/// priority list, the pet and the foot.
///
/// Always exactly `height` lines of exactly `width` columns, so the caller can paint them
/// blindly: a short line would leave the previous frame's pixels behind, and a long one
/// would spill into the terminal on the right.
///
/// Shared by the paint and the hit test, so a click lands on the row a person aimed at
/// by construction: the same function built both answers.
pub(crate) fn rows_folded(
    c: &Column,
    height: usize,
    avatar: Option<&qoro_runtime::avatar::Avatar>,
) -> Vec<Row> {
    if height == 0 {
        return Vec::new();
    }
    // The foot and the pet over it are taken first, so the list above is laid out in the
    // rows that are really its own and a scroll never keeps the focused row under the pet.
    let pet = if avatar_fits(height, c.width, avatar.is_some()) {
        qoro_runtime::avatar::ART_ROWS
    } else {
        0
    };
    let room = height - 1 - pet;
    let mut out: Vec<Row> = Vec::with_capacity(height);
    // Who needs a person, across every space, in the tree only: the priority list is
    // already that list, and a pin over it would say everything twice.
    if c.view == View::Tree {
        out.extend(pin(c, room));
    }
    if out.len() < room {
        out.push((title_line(c), Some(Hit::Title)));
    }
    let body = room - out.len();
    out.extend(window(content(c), body, c, c.width));
    for _ in 0..pet {
        out.push(blank(c.width));
    }
    out.push(collapse_row_with(
        false,
        c.width,
        avatar,
        update_available().as_deref(),
    ));
    out
}

/// The open column as lines alone, for a caller that paints and never clicks.
pub fn render_folded(c: &Column, height: usize) -> Vec<Line> {
    rows_folded(c, height, None)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

/// The pin: who needs a person, most urgent first, with the space each one is in.
///
/// Nothing at all while nobody does, so a calm fleet gives the whole column to the tree.
/// At most half the rows it is offered and at most [`PIN_MOST`] agents; past that it
/// counts the rest. Every agent listed here is also a row under its own space.
fn pin(c: &Column, room: usize) -> Vec<Row> {
    let mut needing: Vec<&AgentRow> = c
        .agents
        .iter()
        // The agent the keyboard is on is not repeated: it is on screen already, and a
        // second raised row for it would be a second place the keyboard seems to be.
        .filter(|a| {
            a.agent
                && a.terminal != c.focused_terminal
                && needs_person(a, c.focused_terminal, c.seen, c.unseen)
        })
        .collect();
    // A title, one agent and the rule under them, or nothing: a pin that cannot show
    // one agent is a title with nothing under it.
    let budget = room / 2;
    if needing.is_empty() || budget < 3 {
        return Vec::new();
    }
    // Stable, so the daemon's order breaks every tie and rows do not trade places.
    needing.sort_by_key(|a| priority_of(a, c.focused_terminal, c.seen, c.unseen));
    let total = needing.len();
    let fits = (budget - 2).min(PIN_MOST);
    // Past what fits, the last row counts the rest, unless the pin has room for one
    // agent only: then it shows that agent and the title's count says the rest.
    let shown = if total <= fits || fits == 1 {
        total.min(fits)
    } else {
        fits - 1
    };
    let count = total.to_string();
    let mut out = vec![(
        Line {
            text: format!(
                "{}{count} ",
                fit_to(
                    &format!(" {PIN_TITLE}"),
                    c.width.saturating_sub(crate::width::width(&count) + 1)
                )
            ),
            colour: Some(crate::theme::Role::Title),
            ..Line::default()
        },
        None,
    )];
    for agent in needing.iter().take(shown) {
        out.push((
            flat_line(c, agent, true),
            Some(Hit::Terminal(agent.terminal)),
        ));
    }
    if shown < total && fits > 1 {
        out.push((
            Line {
                text: fit(&format!("   +{} more", total - shown), c.width),
                colour: Some(crate::theme::Role::Faint),
                ..Line::default()
            },
            None,
        ));
    }
    out.push(divider(c.width));
    out
}

/// The title: `spaces` on the left, the order word and the `+` on the right.
fn title_line(c: &Column) -> Line {
    let right = format!("{} +", title_mode(c.view));
    Line {
        text: format!(
            "{}{right}",
            fit_to(
                &format!(" {SPACES_TITLE}"),
                c.width.saturating_sub(crate::width::width(&right))
            )
        ),
        colour: Some(crate::theme::Role::Title),
        ..Line::default()
    }
}

/// The word on the right of the title: the order the agents below are in.
fn title_mode(view: View) -> &'static str {
    match view {
        View::Tree => "tree",
        View::Priority => "priority",
    }
}

/// Every row under the title, before it is scrolled into the rows it gets.
fn content(c: &Column) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let listed = c.agents.iter().filter(|a| a.agent);
    match c.view {
        View::Priority => {
            // The same agents sorted so what wants a person is on top. Recency breaks
            // ties inside the calm ranks only (`done`, `idle`, the rest): urgency answers
            // "who needs a person" and the second question must not reorder the first.
            // The sort is stable, so daemon order still breaks every remaining tie, and
            // rows without a stamp sink below stamped ones within their rank.
            let mut ordered: Vec<&AgentRow> = listed.collect();
            ordered.sort_by_key(|a| {
                let rank = priority_of(a, c.focused_terminal, c.seen, c.unseen);
                if matches!(rank, 5..=7) {
                    match a.last_active_at_unix {
                        Some(ts) => (rank, 0u8, std::cmp::Reverse(ts)),
                        None => (rank, 1u8, std::cmp::Reverse(0)),
                    }
                } else {
                    (rank, 0u8, std::cmp::Reverse(0))
                }
            });
            for agent in ordered {
                out.push((
                    flat_line(c, agent, false),
                    Some(Hit::Terminal(agent.terminal)),
                ));
                out.extend(activity_row(c, agent, DOT_AT + 2));
            }
            if out.is_empty() {
                // Said, rather than left as a void: an empty list under a title reads as
                // broken, one that says what would go in it reads as waiting for one.
                out.push((
                    Line {
                        text: fit(" no agents yet", c.width),
                        colour: Some(crate::theme::Role::Faint),
                        ..Line::default()
                    },
                    None,
                ));
            }
        }
        View::Tree => {
            for space in c.spaces {
                let kids: Vec<&AgentRow> = c
                    .agents
                    .iter()
                    .filter(|a| a.agent && a.workspace == space.id)
                    .collect();
                let open = !c.shut.contains(&space.id);
                out.push((rule(c, space, &kids, open), Some(Hit::Space(space.id))));
                // A folded space still shows the agent the keyboard is on: the fold hides
                // what a person is not looking at, never the thing they are typing into.
                for agent in kids {
                    if open || agent.terminal == c.focused_terminal {
                        out.push((
                            tree_line(c, agent, space),
                            Some(Hit::Terminal(agent.terminal)),
                        ));
                        out.extend(activity_row(c, agent, CHILD_DOT_AT + 2));
                    }
                }
            }
            // An agent in a space the client has not heard of yet still draws, under no
            // rule, rather than vanishing until the index catches up.
            for agent in c
                .agents
                .iter()
                .filter(|a| a.agent && !c.spaces.iter().any(|s| s.id == a.workspace))
            {
                out.push((
                    flat_line(c, agent, false),
                    Some(Hit::Terminal(agent.terminal)),
                ));
                out.extend(activity_row(c, agent, DOT_AT + 2));
            }
        }
    }
    out
}

/// A space as a rule: the fold arrow, the name in the space's colour, and how many
/// agents it holds, drawn in the rule's own grey.
///
/// Folded, the arrow and the count take the colour of the most urgent agent under them
/// when that agent wants a person, so a fold never hides a blocked agent silently. Open,
/// they stay grey: the agents are right there, wearing their own dots.
fn rule(c: &Column, space: &WorkspaceRow, kids: &[&AgentRow], open: bool) -> Line {
    use crate::theme::Role;
    let width = c.width;
    let hot = if open { None } else { urgent_ink(c, kids) };
    let mark = hot.unwrap_or(Role::Faint);
    let arrow = if kids.is_empty() {
        '\u{b7}'
    } else if open {
        '\u{25be}'
    } else {
        '\u{25b8}'
    };
    let count = if kids.is_empty() {
        String::new()
    } else {
        kids.len().to_string()
    };
    // `─▾ ` then the name, a space, the dashes, and ` 2 ─` (or a last dash with no
    // agents). The name gives way before the count and the arrow do.
    let tail = if count.is_empty() {
        1
    } else {
        crate::width::width(&count) + 3
    };
    let room = width.saturating_sub(FOLD_CELLS + 1 + 1 + tail);
    let name = shorten_middle(&space.name, room);
    let name_w = crate::width::width(&name);
    let fill = width.saturating_sub(FOLD_CELLS + name_w + 1 + tail);
    let dashes_at = FOLD_CELLS + name_w + 1;
    let mut runs = vec![(0, 1, Role::Rule)];
    let text = if count.is_empty() {
        runs.push((dashes_at, fill + 1, Role::Rule));
        format!("\u{2500}{arrow} {name} {}", "\u{2500}".repeat(fill + 1))
    } else {
        runs.push((dashes_at, fill, Role::Rule));
        let count_at = dashes_at + fill + 1;
        runs.push((count_at, crate::width::width(&count), mark));
        runs.push((count_at + crate::width::width(&count) + 1, 1, Role::Rule));
        format!(
            "\u{2500}{arrow} {name} {} {count} \u{2500}",
            "\u{2500}".repeat(fill)
        )
    };
    Line {
        text: fit(&text, width),
        colour: None,
        accent: Some((FOLD_CELLS, name_w, crate::theme::space_of(space.id))),
        indicator: Some((1, 1, mark)),
        runs,
    }
}

/// What an agent is doing, on a row of its own under its name, while it is doing it.
///
/// Only when the daemon sent an activity, which it does only while the agent works,
/// starts, waits for a person or has failed: a calm agent keeps its single row, so a
/// quiet column stays short. The hook's raw label is put in a person's words first
/// ([`humanize`]); a waiting agent says what it wants, in the blocked colour, and a failed
/// one in the failed colour. The row reaches the same terminal as the name above it.
fn activity_row(c: &Column, agent: &AgentRow, at: usize) -> Option<Row> {
    use crate::theme::Role;
    let (text, colour) = said(agent)?;
    let lead = format!("{}\u{2514} ", " ".repeat(at));
    let body = shorten_middle(&text, c.width.saturating_sub(crate::width::width(&lead)));
    Some((
        Line {
            text: fit(&format!("{lead}{body}"), c.width),
            colour: Some(colour),
            runs: vec![(at, 1, Role::Rule)],
            ..Line::default()
        },
        Some(Hit::Terminal(agent.terminal)),
    ))
}

/// What an agent's activity says in its state, and the colour it says it in: working
/// quietly, waiting as what it wants in the blocked colour, failed in the failed colour.
/// A waiting agent whose newest word is `thinking` has said nothing about what it wants
/// (a label-less prompt), so it says nothing rather than `wants: thinking`.
fn said(agent: &AgentRow) -> Option<(String, crate::theme::Role)> {
    use crate::theme::Role;
    let raw = agent
        .activity
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())?;
    match agent.state.as_str() {
        "blocked" if raw != "thinking" => {
            Some((format!("wants: {}", humanize(raw)), Role::Blocked))
        }
        "failed" => Some((humanize(raw), Role::Failed)),
        "working" | "starting" => Some((humanize(raw), Role::Faint)),
        _ => None,
    }
}

/// A hook's raw activity label in the words a person would use: `Edit src/a.rs` reads
/// `editing a.rs`, `Bash cargo test` reads `cargo test`, `Grep todo` reads
/// `searching todo`. Tool names from Claude Code, Codex, Gemini and opencode all land
/// on the same few verbs; a tool nobody has taught this reads as its own label.
pub fn humanize(raw: &str) -> String {
    let raw = raw.trim();
    let (tool, arg) = match raw.split_once(' ') {
        Some((tool, arg)) => (tool, arg.trim()),
        None => (raw, ""),
    };
    let base = |path: &str| {
        path.trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(path)
            .to_string()
    };
    let with = |verb: &str, what: String| {
        if what.is_empty() {
            verb.to_string()
        } else {
            format!("{verb} {what}")
        }
    };
    match tool.to_ascii_lowercase().as_str() {
        "thinking" if arg.is_empty() => "thinking".to_string(),
        "read" | "read_file" | "read_many_files" | "view" | "notebookread" => {
            with("reading", base(arg))
        }
        "edit" | "multiedit" | "write" | "notebookedit" | "write_file" | "replace"
        | "apply_patch" | "edit_file" | "patch" => with("editing", base(arg)),
        "bash" | "shell" | "exec_command" | "run_shell_command" | "local_shell" => {
            let command = [
                "bash -lc ",
                "bash -c ",
                "zsh -lc ",
                "zsh -c ",
                "sh -lc ",
                "sh -c ",
            ]
            .iter()
            .find_map(|shell| arg.strip_prefix(shell))
            // `bash -lc 'git status'` quotes its command; the quotes are the shell's.
            .map(|inner| inner.trim().trim_matches(|q| q == '\'' || q == '"'))
            .unwrap_or(arg);
            let first = command.lines().next().unwrap_or("").trim();
            if first.is_empty() {
                "running".to_string()
            } else {
                first.to_string()
            }
        }
        "grep"
        | "glob"
        | "search_file_content"
        | "ls"
        | "list"
        | "list_directory"
        | "find"
        | "codesearch" => with("searching", arg.to_string()),
        "task" | "agent" => "subagent".to_string(),
        "webfetch" | "websearch" | "web_fetch" | "google_web_search" | "fetch" => "web".to_string(),
        "todowrite" | "todoread" | "update_plan" | "todo" => "planning".to_string(),
        _ => raw.to_string(),
    }
}
/// The dot an agent wears, and whether its whole row goes quiet.
///
/// News keeps the done sage: a derived idle-after-work is news, not calm. A stale idle
/// row dims as a whole, but only off focus and only when it is not news: standing on it
/// is reading it, and news never shows faded.
fn dot_of(c: &Column, agent: &AgentRow) -> (crate::theme::Role, bool) {
    let fresh = is_new(agent, c.focused_terminal, c.seen, c.unseen);
    let stale = !fresh
        && agent.terminal != c.focused_terminal
        && tier_of(&agent.state, agent.last_active_at_unix, c.now_unix) == Some(Tier::Stale);
    let role = if stale {
        crate::theme::Role::Faint
    } else if fresh {
        crate::theme::of("done")
    } else {
        crate::theme::of(&agent.state)
    };
    (role, stale)
}

/// An agent under its space's rule: the dot, the session, and the directory it runs in,
/// quietly, when that says something the space name does not.
fn tree_line(c: &Column, agent: &AgentRow, space: &WorkspaceRow) -> Line {
    let (role, stale) = dot_of(c, agent);
    let lead = CHILD_DOT_AT + 2;
    let room = c.width.saturating_sub(lead);
    let name = shorten_middle(&session_name(agent), room);
    let mut text = format!("{}{} {name}", " ".repeat(CHILD_DOT_AT), dot_glyph(role));
    let mut runs = Vec::new();
    // The worktree a session runs in, in the common case, without asking git on every
    // frame: the directory's own basename, when it is not the space's name and there is
    // room for more than a letter of it. The name never shortens for it.
    let left = room.saturating_sub(crate::width::width(&name) + 1);
    if let Some(place) = cwd_basename(agent.cwd.as_deref())
        .filter(|base| !base.eq_ignore_ascii_case(&space.name))
        .filter(|_| left >= 2)
    {
        let place = shorten_middle(&place, left);
        runs.push((
            crate::width::width(&text) + 1,
            crate::width::width(&place),
            crate::theme::Role::Faint,
        ));
        text.push(' ');
        text.push_str(&place);
    }
    Line {
        text: fit(&text, c.width),
        colour: stale.then_some(crate::theme::Role::Faint),
        accent: None,
        indicator: Some((CHILD_DOT_AT, 1, role)),
        runs,
    }
}

/// An agent in a flat list, the pin or the priority view: the dot, the session, and the
/// space it lives in on the right, in the space's own colour.
fn flat_line(c: &Column, agent: &AgentRow, inline: bool) -> Line {
    use crate::theme::Role;
    let (role, stale) = dot_of(c, agent);
    let space = c
        .spaces
        .iter()
        .find(|s| s.id == agent.workspace)
        .map(|s| shorten_middle(&s.name, c.width / 3))
        .unwrap_or_default();
    let space_w = crate::width::width(&space);
    // The dot and its two spaces, the name, a gap, the space and a trailing space.
    let right = if space.is_empty() { 0 } else { space_w + 2 };
    let room = c.width.saturating_sub(DOT_AT + 2 + right);
    // In the pin, what the agent wants rides the same row after a dash: a folded space
    // shows no sub-row, and the pin is where a person reads why they are needed.
    // Only when the activity will really fit beside a shortened name: a name cut for an
    // activity that then does not fit would be cut for nothing.
    let said = inline
        .then(|| said(agent))
        .flatten()
        .filter(|_| room >= (room * 42 / 100).max(7) + 6);
    let name_room = if said.is_some() {
        (room * 42 / 100).max(7).min(room)
    } else {
        room
    };
    let name = shorten_middle(&session_name(agent), name_room);
    let mut head = format!("{}{} {name}", " ".repeat(DOT_AT), dot_glyph(role));
    let mut runs = Vec::new();
    let rest = room.saturating_sub(crate::width::width(&name));
    if let Some((what, ink)) = said.filter(|_| rest >= 6) {
        let what = shorten_middle(&what, rest - 3);
        let at = crate::width::width(&head);
        runs.push((at + 1, 1, Role::Rule));
        runs.push((at + 3, crate::width::width(&what), ink));
        head.push_str(" - ");
        head.push_str(&what);
    }
    let left = fit_to(&head, c.width.saturating_sub(right));
    let (text, accent) = if space.is_empty() {
        (left, None)
    } else {
        let at = crate::width::width(&left) + 1;
        (
            fit(&format!("{left} {space} "), c.width),
            Some((at, space_w, crate::theme::space_of(agent.workspace))),
        )
    };
    Line {
        text,
        colour: stale.then_some(crate::theme::Role::Faint),
        accent,
        indicator: Some((DOT_AT, 1, role)),
        runs,
    }
}

/// The rows under the title, scrolled into `body` rows and padded out to them.
///
/// Stateless: the window is whatever keeps the keyboard's agent on screen, its space's
/// rule too when both fit, and the top of the list otherwise. A list that fits never
/// scrolls, so a calm column is the same rows in the same places on every paint.
fn window(content: Vec<Row>, body: usize, c: &Column, width: usize) -> Vec<Row> {
    let len = content.len();
    let first = if len <= body {
        0
    } else {
        let target = content
            .iter()
            .rposition(|(_, hit)| *hit == Some(Hit::Terminal(c.focused_terminal)))
            .or_else(|| {
                content
                    .iter()
                    .position(|(_, hit)| *hit == Some(Hit::Space(c.focused_space)))
            });
        // The last row of the target goes at the foot of the window at the latest; the
        // rule above it stays on screen whenever the two fit together.
        target.map_or(0, |last| (last + 1).saturating_sub(body).min(len - body))
    };
    let mut out: Vec<Row> = content.into_iter().skip(first).take(body).collect();
    while out.len() < body {
        out.push(blank(width));
    }
    out
}

/// What a click asks for, from the row it landed on and the column within the sidebar it
/// landed at, counted from one.
///
/// `collapsed` is the strip: there every space row moves to the space and the foot
/// opens the column again.
pub fn act(hit: Hit, col: usize, c: &Column, collapsed: bool) -> Option<Act> {
    match hit {
        Hit::Terminal(terminal) => Some(Act::Terminal(terminal)),
        Hit::Space(id) => {
            // The arrow folds; a rule with nothing under it has nothing to fold, so
            // every cell of it moves to the space.
            let folds = !collapsed
                && c.view == View::Tree
                && col <= FOLD_CELLS
                && c.agents.iter().any(|a| a.agent && a.workspace == id);
            Some(if folds { Act::Fold(id) } else { Act::Space(id) })
        }
        Hit::Title => {
            // `+` is the last cell; the order word sits before the space in front of it.
            if col == c.width {
                return Some(Act::NewSpace);
            }
            let end = c.width.saturating_sub(2);
            let start = end.saturating_sub(title_mode(c.view).len());
            (col > start && col <= end).then_some(Act::View)
        }
        Hit::Foot => {
            if collapsed {
                return Some(Act::Collapse);
            }
            // `menu « ` closes the row: the word, a space, the mark and a space.
            let end = c.width.saturating_sub(FOOT_TAIL - MENU_LABEL.len());
            let start = end.saturating_sub(MENU_LABEL.len());
            Some(if col > start && col <= end {
                Act::Menu
            } else {
                Act::Collapse
            })
        }
    }
}

/// The columns `menu « ` takes at the right of the foot.
const FOOT_TAIL: usize = 7;

const MENU_LABEL: &str = "menu";

/// The column folded away to a strip: a space's number, then its agents' marks under it.
///
/// What a glance wants is colour and count, and both survive four columns. The names come
/// back with the column, and the terminal gets twenty six columns in the meantime. The
/// order is the tree's, so a mark sits under the number of the space it is in; a folded
/// space shows only its number, and the keyboard's own agent.
pub(crate) fn rows_collapsed(c: &Column, height: usize) -> Vec<Row> {
    if height == 0 {
        return Vec::new();
    }
    let strip_row = |agent: &AgentRow| {
        let role = colour_for(&agent.state).unwrap_or(crate::theme::Role::Idle);
        (
            Line {
                text: strip_agent(role),
                colour: colour_for(&agent.state),
                // The space mark sits on the first pad cell, never on the glyph: the glyph
                // column keeps the state colour, so the strip carries state and space side
                // by side instead of painting one over the other.
                accent: Some((2, 1, crate::theme::space_of(agent.workspace))),
                ..Line::default()
            },
            Some(Hit::Terminal(agent.terminal)),
        )
    };
    let mut content: Vec<Row> = Vec::new();
    for (at, space) in c.spaces.iter().enumerate() {
        let kids: Vec<&AgentRow> = c
            .agents
            .iter()
            .filter(|a| a.agent && a.workspace == space.id)
            .collect();
        let open = !c.shut.contains(&space.id) || kids.is_empty();
        let number = (at + 1).to_string();
        // A folded space says so with the rule's own arrow after its number, in the colour
        // of whoever under it wants a person: the strip hides names, never a blocked agent.
        let (text, indicator) = if open {
            (strip_space(at + 1), None)
        } else {
            let mark = urgent_ink(c, &kids).unwrap_or(crate::theme::Role::Faint);
            (
                fit(&format!(" {number}\u{25b8}"), COLLAPSED),
                Some((1 + number.len(), 1, mark)),
            )
        };
        content.push((
            Line {
                text,
                accent: Some((1, number.len(), crate::theme::space_of(space.id))),
                indicator,
                ..Line::default()
            },
            Some(Hit::Space(space.id)),
        ));
        for agent in kids {
            if open || agent.terminal == c.focused_terminal {
                content.push(strip_row(agent));
            }
        }
    }
    for agent in c
        .agents
        .iter()
        .filter(|a| a.agent && !c.spaces.iter().any(|s| s.id == a.workspace))
    {
        content.push(strip_row(agent));
    }
    // Scrolled like the open column, so a long fleet keeps the keyboard's row reachable.
    let mut out = window(content, height - 1, c, COLLAPSED);
    out.push(collapse_row(true, COLLAPSED));
    out
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

/// The middle shortened to `room` columns, tail-cut with an ellipsis, or the
/// whole middle when it already fits. A room under two columns still yields the
/// ellipsis, which the caller's own fit then cuts to the row.
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
    collapse_row_with(
        collapsed,
        width,
        current_avatar().as_ref(),
        update_available().as_deref(),
    )
}

/// The picked avatar, when one is picked.
///
/// Reads the file on every paint like the update notice below it: the picker
/// runs in a process outside the interface, so anything held would be stale by
/// construction. Under test this is always none, so the thirty-odd paints the
/// suite asserts never depend on the developer's own home directory.
pub(crate) fn current_avatar() -> Option<qoro_runtime::avatar::Avatar> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        qoro_runtime::avatar::load()
    }
}

/// Whether the pet itself fits above the foot: open column, wide enough for
/// sixteen art columns plus a margin each side, tall enough that five art
/// rows still leave six rows of list.
pub(crate) fn avatar_fits(height: usize, width: usize, present: bool) -> bool {
    present
        && width >= qoro_runtime::avatar::ART_COLS + 2
        && height >= qoro_runtime::avatar::ART_ROWS + 7
}

/// The pet as itself: five pre-escaped half-block rows, sixteen columns each,
/// for the overlay to stamp above the foot. Empty without colour, because dots
/// and name already say it then.
pub(crate) fn avatar_overlay(avatar: &qoro_runtime::avatar::Avatar, colour: bool) -> Vec<String> {
    if !colour {
        return Vec::new();
    }
    use qoro_runtime::avatar::{ART_COLS, art_cells};
    art_cells(avatar)
        .iter()
        .map(|row| {
            let mut line = String::new();
            let mut cur: (Option<u8>, Option<u8>) = (None, None);
            for (top, bottom) in row.iter().take(ART_COLS) {
                if *top != cur.0 {
                    line.push_str(
                        &top.map(|ink| qoro_runtime::avatar::fg(crate::theme::pet(ink)))
                            .unwrap_or_else(|| "\x1b[39m".to_string()),
                    );
                    cur.0 = *top;
                }
                if *bottom != cur.1 {
                    line.push_str(
                        &bottom
                            .map(|ink| qoro_runtime::avatar::bg(crate::theme::pet(ink)))
                            .unwrap_or_else(|| "\x1b[49m".to_string()),
                    );
                    cur.1 = *bottom;
                }
                line.push(if top.is_some() || bottom.is_some() {
                    '\u{2580}'
                } else {
                    ' '
                });
            }
            line.push_str("\x1b[0m");
            line
        })
        .collect()
}

/// Screen rows the overlay stamps, top first, for a column of `height` rows
/// starting at `top`. Pure so the paint and the tests agree on arithmetic.
pub(crate) fn avatar_overlay_rows(top: usize, height: usize) -> Vec<usize> {
    (0..qoro_runtime::avatar::ART_ROWS)
        .map(|i| top + height - 1 - qoro_runtime::avatar::ART_ROWS + i)
        .collect()
}

/// [`collapse_row`] with the avatar passed in, so tests can paint both feet
/// without touching a home directory.
/// `update` is what [`update_available`] said, passed in so a test can say it without
/// writing the notice file every other paint in the process reads.
fn collapse_row_with(
    collapsed: bool,
    width: usize,
    avatar: Option<&qoro_runtime::avatar::Avatar>,
    update: Option<&str>,
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
                runs: Vec::new(),
            },
            Some(Hit::Foot),
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
    let version = match update {
        Some(new) => format!("{VERSION}>{new} "),
        None => format!("{VERSION} "),
    };
    // `menu « ` closes the row: the way to everything that is not a space, a tab or an
    // agent, then the fold mark. Both always fit. The version gives way before them, and
    // the pet's name before the version.
    let tail = format!("{MENU_LABEL} {mark} ");
    let fixed = crate::width::width(&brand) + 1 + crate::width::width(&tail);
    let version = if fixed + crate::width::width(&version) <= width {
        version
    } else {
        String::new()
    };
    let name = avatar
        .as_ref()
        .map(|found| found.name())
        .unwrap_or_default();
    let room = width.saturating_sub(fixed + crate::width::width(&version) + 1);
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
    // gap, then the menu, the fold mark and its own trailing space.
    let mid = if shown.is_empty() {
        version
    } else {
        format!("{version}{shown} ")
    };
    let gap = width.saturating_sub(
        crate::width::width(&brand) + 1 + crate::width::width(&mid) + crate::width::width(&tail),
    );
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
            text: fit(&format!("{brand} {mid}{}{tail}", " ".repeat(gap)), width),
            colour: Some(crate::theme::Role::Faint),
            accent,
            indicator,
            runs: Vec::new(),
        },
        Some(Hit::Foot),
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

/// What to call the program in the terminal.
///
/// A name a person chose outlives the program: a tab called `release` stays `release`
/// while it runs a shell, then cargo, then an agent. The tab bar already spells it this
/// way, and the panel spells it the same, so the two halves of the screen name the same
/// terminal the same way. Without a chosen name the program's last path segment answers,
/// because a column this narrow renders `/opt/homebrew/bin/claude` as `/opt/homebre`,
/// which identifies nothing. A wrapper is announced as a wrapper: a person shown
/// `docker` would conclude their agent is called docker.
pub(crate) fn label(agent: &AgentRow) -> String {
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

/// The rule under the pin, joined to the column's edge: one row of box
/// drawing, clickable as nothing, so a click on it does nothing rather than
/// something arbitrary.
fn divider(width: usize) -> (Line, Option<Hit>) {
    (
        Line {
            text: "\u{2500}".repeat(width),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
            indicator: None,
            runs: Vec::new(),
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
            runs: Vec::new(),
        },
        None,
    )
}

/// Cut or pad to exactly `width` columns.
fn fit(text: &str, width: usize) -> String {
    fit_to(text, width)
}

/// Cut or pad to exactly `cols` columns, in columns and not characters: a program or a
/// space named with a full width glyph would otherwise draw over the terminal beside it.
fn fit_to(text: &str, cols: usize) -> String {
    crate::width::text(text, cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state a terminal has when nothing has been able to say what it is doing.
    const UNKNOWN: &str = "unknown";

    /// A fixed paint-time clock: tests pin behavior at exact ages, never time.
    const NOW: i64 = 1_750_000_000;

    /// The pieces of a column, spelled out: every test names what it varies.
    #[allow(clippy::too_many_arguments)]
    fn column<'a>(
        spaces: &'a [WorkspaceRow],
        agents: &'a [AgentRow],
        focused_space: i64,
        focused_terminal: u32,
        tick: u64,
        view: View,
        seen: &'a Seen,
        unseen: &'a Unseen,
        now_unix: i64,
        width: usize,
        shut: &'a Shut,
    ) -> Column<'a> {
        Column {
            spaces,
            agents,
            focused_space,
            focused_terminal,
            tick,
            view,
            seen,
            unseen,
            now_unix,
            width,
            shut,
        }
    }

    /// The open column at the default width in the tree, nothing seen and nothing folded.
    fn render(
        spaces: &[WorkspaceRow],
        agents: &[AgentRow],
        focused_space: i64,
        focused_terminal: u32,
        height: usize,
    ) -> Vec<Line> {
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        render_folded(
            &column(
                spaces,
                agents,
                focused_space,
                focused_terminal,
                0,
                View::Tree,
                &seen,
                &unseen,
                0,
                WIDTH,
                &shut,
            ),
            height,
        )
    }

    /// What a click on `row` of [`render`]'s column hits.
    fn hit(
        spaces: &[WorkspaceRow],
        agents: &[AgentRow],
        focused_space: i64,
        focused_terminal: u32,
        height: usize,
        row: usize,
    ) -> Option<Hit> {
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = column(
            spaces,
            agents,
            focused_space,
            focused_terminal,
            0,
            View::Tree,
            &seen,
            &unseen,
            0,
            WIDTH,
            &shut,
        );
        rows_folded(&c, height, None)
            .into_iter()
            .nth(row)
            .and_then(|(_, hit)| hit)
    }

    /// The open column with every piece named, nothing folded.
    #[allow(clippy::too_many_arguments)]
    fn folded(
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
        let shut = Shut::new();
        render_folded(
            &column(
                spaces,
                agents,
                focused_space,
                focused_terminal,
                tick,
                view,
                seen,
                unseen,
                now_unix,
                width,
                &shut,
            ),
            height,
        )
    }

    /// [`folded`] with the hits and a pet, nothing folded.
    #[allow(clippy::too_many_arguments)]
    fn rows12(
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
        avatar: Option<&qoro_runtime::avatar::Avatar>,
    ) -> Vec<Row> {
        let shut = Shut::new();
        rows_folded(
            &column(
                spaces,
                agents,
                focused_space,
                focused_terminal,
                tick,
                view,
                seen,
                unseen,
                now_unix,
                width,
                &shut,
            ),
            height,
            avatar,
        )
    }

    /// What a click on `row` of [`rows12`]'s column hits, at frame zero.
    #[allow(clippy::too_many_arguments)]
    fn hit12(
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
        avatar: Option<&qoro_runtime::avatar::Avatar>,
    ) -> Option<Hit> {
        rows12(
            spaces,
            agents,
            focused_space,
            focused_terminal,
            height,
            0,
            view,
            seen,
            unseen,
            now_unix,
            width,
            avatar,
        )
        .into_iter()
        .nth(row)
        .and_then(|(_, hit)| hit)
    }

    /// The strip at the default pieces, nothing folded.
    fn collapsed5(
        spaces: &[WorkspaceRow],
        agents: &[AgentRow],
        focused_space: i64,
        focused_terminal: u32,
        height: usize,
    ) -> Vec<Row> {
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        rows_collapsed(
            &column(
                spaces,
                agents,
                focused_space,
                focused_terminal,
                0,
                View::Tree,
                &seen,
                &unseen,
                0,
                WIDTH,
                &shut,
            ),
            height,
        )
    }

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
            slept_at_unix: None,
            activity: None,
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
            slept_at_unix: None,
            activity: None,
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

    /// [`sample`] with its codex blocked, so the pin draws above the tree.
    fn sample_pinned() -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
        let (spaces, mut agents) = sample();
        agents[2].state = "blocked".into();
        (spaces, agents)
    }

    /// Every row's text with its trailing pad cut, so an assertion reads the row.
    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.text.trim_end().to_string())
            .collect()
    }

    /// The tree at the default width with the given spaces folded, nothing seen.
    fn tree_rows(
        spaces: &[WorkspaceRow],
        agents: &[AgentRow],
        focused_terminal: u32,
        height: usize,
        seen: &Seen,
        shut: &Shut,
    ) -> Vec<Row> {
        let unseen = Unseen::new();
        rows_folded(
            &column(
                spaces,
                agents,
                1,
                focused_terminal,
                0,
                View::Tree,
                seen,
                &unseen,
                NOW,
                WIDTH,
                shut,
            ),
            height,
            None,
        )
    }

    /// The terminals the list under the title draws, in order, each once. The pin above
    /// the title repeats agents on purpose, so it is never the order a test pins.
    fn order(rows: &[Row]) -> Vec<u32> {
        let mut out = Vec::new();
        for (_, hit) in rows.iter().skip_while(|(_, h)| *h != Some(Hit::Title)) {
            if let Some(Hit::Terminal(t)) = hit
                && !out.contains(t)
            {
                out.push(*t);
            }
        }
        out
    }

    /// The terminals the pin lists, in order: every agent row above the title.
    fn pinned(rows: &[Row]) -> Vec<u32> {
        rows.iter()
            .take_while(|(_, h)| *h != Some(Hit::Title))
            .filter_map(|(_, h)| match h {
                Some(Hit::Terminal(t)) => Some(*t),
                _ => None,
            })
            .collect()
    }

    /// The row a terminal draws under its space rule: the last row that reaches it, since
    /// the pin, when it lists the same terminal, is always above the tree.
    fn tree_row(rows: &[Row], terminal: u32) -> &Line {
        rows.iter()
            .rev()
            .find(|(_, h)| *h == Some(Hit::Terminal(terminal)))
            .map(|(l, _)| l)
            .unwrap_or_else(|| panic!("terminal {terminal} is not drawn"))
    }

    /// The rule a space draws.
    fn rule_row(rows: &[Row], id: i64) -> &Line {
        rows.iter()
            .find(|(_, h)| *h == Some(Hit::Space(id)))
            .map(|(l, _)| l)
            .unwrap_or_else(|| panic!("space {id} has no rule"))
    }

    /// The text a run of a row covers, for rows of single column characters.
    fn run_text(line: &Line, at: usize, len: usize) -> String {
        line.text.chars().skip(at).take(len).collect()
    }

    #[test]
    fn every_line_is_exactly_the_column_width() {
        for (spaces, agents) in [sample(), sample_pinned()] {
            for height in [1usize, 2, 3, 5, 9, 14, 40] {
                let lines = render(&spaces, &agents, 1, 1, height);
                assert_eq!(lines.len(), height, "height {height}");
                for line in &lines {
                    assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
                }
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

    /// The one title names the column, then the order the agents below are in and the
    /// `+` that makes a space, so a person reads both switches off the row they live on.
    #[test]
    fn the_title_names_the_order_and_ends_in_the_plus() {
        let (spaces, agents) = sample();
        for (view, word) in [(View::Tree, "tree"), (View::Priority, "priority")] {
            let lines = folded(
                &spaces,
                &agents,
                1,
                1,
                14,
                0,
                view,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
            );
            let title = &lines[0];
            assert!(
                title.text.starts_with(&format!(" {SPACES_TITLE} ")),
                "{:?}",
                title.text
            );
            assert!(
                title.text.ends_with(&format!(" {word} +")),
                "{:?}",
                title.text
            );
            assert_eq!(title.colour, Some(crate::theme::Role::Title));
        }
    }

    #[test]
    fn the_focused_space_and_the_focused_terminal_are_found_by_hit_not_by_glyph() {
        let (spaces, agents) = sample();
        let texts = texts(&render(&spaces, &agents, 2, 3, 14));
        // No selection marks anywhere: no hollow ring on every row and no
        // second dot on the selected one. The keyboard's row is the raised
        // background the painter adds. The state dot opens every agent's
        // row, so it is expected; a spinner never is.
        for bad in ['\u{25cb}', '\u{25cc}', '\u{25a1}', '\u{2809}'] {
            assert!(
                !texts.iter().any(|t| t.contains(bad)),
                "a selection mark or spinner leaked: {texts:?}"
            );
        }
        // The focused space and agent still draw their names, the agent under its
        // space's rule with the same dot every other agent wears.
        assert!(texts.iter().any(|t| t.contains(" personal ")), "{texts:?}");
        assert!(texts.iter().any(|t| t == "   \u{25cf} codex"), "{texts:?}");
        // And hits still know which rows have the keyboard.
        let hits: Vec<Option<Hit>> = (0..14)
            .map(|row| hit(&spaces, &agents, 2, 3, 14, row))
            .collect();
        assert!(hits.contains(&Some(Hit::Space(2))));
        assert!(hits.contains(&Some(Hit::Terminal(3))));
    }

    /// One row per agent, right under its space's rule: the rule already says where, so
    /// the row says only what, and the next rule follows it with nothing in between.
    #[test]
    fn an_agent_is_one_row_under_its_space_rule() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts = texts(&lines);
        let head = texts
            .iter()
            .position(|t| t == "   \u{25cf} claude")
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert!(
            texts[head - 1].starts_with("\u{2500}\u{25be} app "),
            "{texts:?}"
        );
        assert!(
            texts[head + 1].starts_with("\u{2500}\u{25be} personal "),
            "{texts:?}"
        );
        // No state word on the right: the dot says the state in its colour, three
        // columns in, under the space name the rule carries.
        assert_eq!(
            lines[head].indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("working")))
        );
    }

    #[test]
    fn a_terminal_that_is_not_running_an_agent_is_left_out_of_the_column() {
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
        // And the shell is not counted on its space's rule either.
        let app = lines
            .iter()
            .find(|l| l.text.starts_with("\u{2500}\u{25be} app "))
            .expect("the app rule");
        assert!(app.text.ends_with(" 1 \u{2500}"), "{:?}", app.text);
    }

    #[test]
    fn an_agent_row_carries_its_state_on_the_dot_and_a_title_or_rule_carries_none() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        // A title has a look of its own, and it is never a state's: a heading that
        // came out the colour of `blocked` would be a heading somebody answers.
        assert_eq!(lines[0].colour, Some(crate::theme::Role::Title));
        let rows: Vec<&Line> = lines.iter().filter(|l| l.text.contains("claude")).collect();
        // One row for the one agent: plain text with the state's colour on its dot.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].colour, None);
        assert_eq!(
            rows[0].indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("working")))
        );
        // A rule has no state to have a colour about: it wears the space's own colour
        // on the name, and its open arrow stays the rule's quiet grey.
        let personal = lines
            .iter()
            .find(|l| l.text.contains(" personal "))
            .expect("the personal rule");
        assert_eq!(personal.colour, None);
        assert_eq!(
            personal.accent,
            Some((FOLD_CELLS, 8, crate::theme::space_of(2)))
        );
        assert_eq!(personal.indicator, Some((1, 1, crate::theme::Role::Faint)));
    }

    /// Every space is one rule: the arrow says open, folded or empty, the name wears the
    /// space's colour, and the count says how many agents live there. The dashes are
    /// the rule's own grey, so the rule reads as structure and never as a row to answer.
    #[test]
    fn every_space_draws_one_rule_with_its_arrow_and_count() {
        let (spaces, agents) = sample();
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        let app = rule_row(&rows, 1);
        assert_eq!(
            app.text,
            format!("\u{2500}\u{25be} app {} 1 \u{2500}", "\u{2500}".repeat(19))
        );
        assert_eq!(
            app.runs,
            vec![
                (0, 1, crate::theme::Role::Rule),
                (7, 19, crate::theme::Role::Rule),
                (27, 1, crate::theme::Role::Faint),
                (29, 1, crate::theme::Role::Rule),
            ]
        );
        // An empty space has nothing to fold and nothing to count: a dot for the arrow,
        // and the dashes run to the edge.
        let dub = rule_row(&rows, 3);
        assert_eq!(
            dub.text,
            format!("\u{2500}\u{b7} dub {}", "\u{2500}".repeat(23))
        );
        // Folded, the arrow points right and the count stays.
        let shut = Shut::from([1]);
        let rows = tree_rows(&spaces, &agents, 9, 14, &Seen::new(), &shut);
        assert!(
            rule_row(&rows, 1).text.starts_with("\u{2500}\u{25b8} app "),
            "{:?}",
            rule_row(&rows, 1).text
        );
        for line in rows.iter().map(|(l, _)| l) {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
    }

    /// The window is stateless: whatever keeps the keyboard's agent on screen, with its
    /// space's rule above it when both fit. A long tree scrolls; a short one never does.
    #[test]
    fn the_window_scrolls_to_keep_the_focused_agent_visible() {
        let spaces: Vec<WorkspaceRow> = (1..=10).map(|n| space(n, &format!("s{n}"))).collect();
        let agents: Vec<AgentRow> = (1..=30u32)
            .map(|n| agent(n, i64::from((n - 1) / 3 + 1), "claude", "idle"))
            .collect();
        for focused in 1..=30u32 {
            let home = i64::from((focused - 1) / 3 + 1);
            let drawn: Vec<Hit> = (0..15)
                .filter_map(|row| hit(&spaces, &agents, home, focused, 15, row))
                .collect();
            assert!(
                drawn.contains(&Hit::Terminal(focused)),
                "terminal {focused} fell out: {drawn:?}"
            );
            assert!(
                drawn.contains(&Hit::Space(home)),
                "the rule of {focused} fell out: {drawn:?}"
            );
        }
        // With no agent to keep, the focused space's rule is what stays.
        for focused_space in 1..=10i64 {
            let drawn: Vec<Hit> = (0..15)
                .filter_map(|row| hit(&spaces, &agents, focused_space, 99, 15, row))
                .collect();
            assert!(
                drawn.contains(&Hit::Space(focused_space)),
                "space {focused_space} fell out: {drawn:?}"
            );
        }
        // A tree that fits never scrolls: the first rule sits under the title.
        let (spaces, agents) = sample();
        assert_eq!(hit(&spaces, &agents, 3, 3, 14, 1), Some(Hit::Space(1)));
    }

    #[test]
    fn a_height_too_small_for_the_column_still_returns_that_many_lines() {
        for (spaces, agents) in [sample(), sample_pinned()] {
            for height in 0..8 {
                let lines = render(&spaces, &agents, 1, 1, height);
                assert_eq!(lines.len(), height);
                assert!(lines.iter().all(|l| crate::width::width(&l.text) == WIDTH));
            }
        }
    }

    #[test]
    fn hit_returns_exactly_what_render_drew_on_every_row() {
        for (spaces, agents) in [sample(), sample_pinned()] {
            for height in [1usize, 2, 3, 4, 7, 9, 14, 25] {
                let lines = render(&spaces, &agents, 2, 3, height);
                for (row, line) in lines.iter().enumerate() {
                    let text = line.text.trim_end();
                    let got = hit(&spaces, &agents, 2, 3, height, row);
                    let want = if text.is_empty()
                        || text.chars().all(|c| c == '\u{2500}')
                        || text.starts_with(&format!(" {PIN_TITLE}"))
                    {
                        // Blank rows, the rule under the pin and the pin's own title
                        // do nothing when clicked.
                        None
                    } else if text.starts_with(&format!(" {SPACES_TITLE}")) && text.ends_with('+') {
                        Some(Hit::Title)
                    } else if text.contains(BRAND) && text.ends_with('\u{ab}') {
                        Some(Hit::Foot)
                    } else if text.starts_with('\u{2500}') {
                        spaces
                            .iter()
                            .find(|s| text.contains(&format!(" {} ", s.name)))
                            .map(|s| Hit::Space(s.id))
                    } else {
                        // Everything left is an agent: a flat pin row with its dot at
                        // column one, or a tree row with its dot under the space name.
                        // Either glyph: the circle, or the diamond for failed.
                        agents
                            .iter()
                            .find(|a| {
                                [DOT, FAILED_DOT].iter().any(|g| {
                                    let name = session_name(a);
                                    text.starts_with(&format!(" {g} {name}"))
                                        || text.starts_with(&format!("   {g} {name}"))
                                })
                            })
                            .map(|a| Hit::Terminal(a.terminal))
                    };
                    assert_eq!(got, want, "row {row} of {height}: {text:?}");
                }
                assert_eq!(hit(&spaces, &agents, 2, 3, height, height), None);
            }
        }
    }

    #[test]
    fn a_focus_that_is_in_neither_list_does_not_panic() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 99, 99, 12);
        assert_eq!(lines.len(), 12);
        assert_eq!(render(&[], &[], 1, 1, 5).len(), 5);
    }

    /// An agent whose space the client has not heard of yet draws as a flat row after
    /// the tree, under no rule, rather than vanishing until the index catches up.
    #[test]
    fn an_agent_in_a_space_the_client_has_not_heard_of_still_draws_its_row() {
        let agents = vec![agent(4, 77, "codex", "blocked")];
        let lines = render(&[], &agents, 1, 4, 8);
        assert!(
            lines.iter().any(|l| l.text.starts_with(" \u{25cf} codex")),
            "{:?}",
            texts(&lines)
        );
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(4, 77, "codex", "idle"), agent(5, 1, "claude", "idle")];
        let rows = tree_rows(&spaces, &agents, 5, 14, &Seen::new(), &Shut::new());
        assert_eq!(order(&rows), vec![5, 4], "the stranger follows the tree");
        let stray = tree_row(&rows, 4);
        assert!(
            stray.text.starts_with(" \u{25cf} codex"),
            "{:?}",
            stray.text
        );
        assert_eq!(stray.accent, None, "no space to name");
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
            slept_at_unix: None,
            activity: None,
        }
    }

    fn agent_texts(spaces: &[WorkspaceRow], agents: &[AgentRow], focused: u32) -> Vec<String> {
        texts(&render(spaces, agents, 1, focused, 20))
    }

    /// The priority list, as text.
    fn priority_texts(spaces: &[WorkspaceRow], agents: &[AgentRow]) -> Vec<String> {
        texts(&folded(
            spaces,
            agents,
            1,
            9,
            20,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        ))
    }

    /// The user's column: four rows reading `opencode` identify nothing on their own, so
    /// each sits under the rule of the space it lives in and the rule says which.
    #[test]
    fn agents_sharing_a_label_sit_under_their_own_space_rules() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 2, "opencode", "idle"),
        ];
        let rows = tree_rows(&spaces, &agents, 1, 20, &Seen::new(), &Shut::new());
        for (terminal, id) in [(1u32, 1i64), (2, 2)] {
            let at = rows
                .iter()
                .position(|(_, h)| *h == Some(Hit::Terminal(terminal)))
                .expect("an agent row");
            assert_eq!(rows[at - 1].1, Some(Hit::Space(id)), "{terminal}");
            assert_eq!(rows[at].0.text.trim_end(), "   \u{25cf} opencode");
        }
    }

    /// Off the tree there is no rule above a row, so a flat row names its space on the
    /// right, in the space's own colour: the words say it and the colour says it again.
    #[test]
    fn a_flat_row_names_its_space_on_the_right() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 2, "opencode", "idle"),
            agent(3, 1, "claude", "idle"),
        ];
        let texts = priority_texts(&spaces, &agents);
        for want in ["opencode", "claude"] {
            assert!(
                texts
                    .iter()
                    .any(|t| t.starts_with(&format!(" \u{25cf} {want}")) && t.ends_with(" app")),
                "{texts:?}"
            );
        }
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(" \u{25cf} opencode") && t.ends_with(" personal")),
            "{texts:?}"
        );
        let lines = folded(
            &spaces,
            &agents,
            1,
            9,
            20,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        );
        let row = lines
            .iter()
            .find(|l| l.text.trim_end().ends_with(" personal"))
            .expect("the personal row");
        let (at, len, role) = row.accent.expect("the space is accented");
        assert_eq!(role, crate::theme::space_of(2));
        assert_eq!(run_text(row, at, len), "personal");
    }

    /// A chosen name picked twice in two spaces is still two rows that cannot be told
    /// apart by name, so each names where it lives: by its rule in the tree, on its
    /// right in a flat list.
    #[test]
    fn a_chosen_name_shared_by_two_agents_names_both_spaces() {
        let spaces = vec![space(1, "app"), space(2, "personal")];
        let agents = vec![
            named(1, 1, "release", "opencode", "idle"),
            named(2, 2, "release", "claude", "idle"),
        ];
        let flat = priority_texts(&spaces, &agents);
        assert!(
            flat.iter()
                .any(|t| t.starts_with(" \u{25cf} release") && t.ends_with(" app")),
            "{flat:?}"
        );
        assert!(
            flat.iter()
                .any(|t| t.starts_with(" \u{25cf} release") && t.ends_with(" personal")),
            "{flat:?}"
        );
        // Without a reported session the row falls back to the tab's own label, so
        // both tree rows read the chosen name behind the dot.
        let texts = agent_texts(&spaces, &agents, 1);
        assert_eq!(
            texts.iter().filter(|t| *t == "   \u{25cf} release").count(),
            2,
            "{texts:?}"
        );
    }

    /// Two tabs of one space share the program, and still tell apart: their
    /// dots differ in the states' colours, and each row keeps its own hit.
    #[test]
    fn two_tabs_of_one_space_share_the_space_name_but_not_the_dot() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(1, 1, "opencode", "idle"),
            agent(2, 1, "opencode", "blocked"),
        ];
        let rows = tree_rows(&spaces, &agents, 1, 20, &Seen::new(), &Shut::new());
        let dots: Vec<_> = [1u32, 2]
            .iter()
            .map(|t| tree_row(&rows, *t).indicator)
            .collect();
        assert_eq!(
            dots,
            vec![
                Some((CHILD_DOT_AT, 1, crate::theme::of("idle"))),
                Some((CHILD_DOT_AT, 1, crate::theme::of("blocked"))),
            ]
        );
        // No state word on the right of any row.
        for (line, _) in &rows {
            for word in ["idle", "blocked", "working"] {
                assert!(
                    !line.text.trim_end().ends_with(word),
                    "a state word leaked: {:?}",
                    line.text
                );
            }
        }
        assert_eq!(order(&rows), vec![1, 2]);
    }

    /// The row is the session id behind the dot, and nothing else: no program beside
    /// it, so four live sessions never all read `opencode`, and no tab label either,
    /// since the rule above already says where the row lives.
    #[test]
    fn a_reported_session_names_the_row_alone() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "opencode", "idle");
        a.name = Some("release".to_string());
        a.agent_session = Some("abc123".to_string());
        let texts = agent_texts(&spaces, &[a], 1);
        assert!(texts.iter().any(|t| t == "   \u{25cf} abc123"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("opencode")),
            "the program leaked onto a reported session row: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("release")),
            "the tab label leaked onto a calm row: {texts:?}"
        );
    }

    #[test]
    fn a_reported_title_names_the_row_and_follows_renames() {
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
    fn without_a_session_the_row_falls_back_to_the_tab() {
        let spaces = vec![space(1, "app")];
        let texts = agent_texts(&spaces, &[agent(1, 1, "codex", "idle")], 1);
        // No reported session, so the tab's own label answers behind the dot.
        assert!(texts.iter().any(|t| t == "   \u{25cf} codex"), "{texts:?}");
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
            slept_at_unix: None,
            activity: None,
        }];
        let lines = render(&[space(1, "app")], &agents, 1, 5, 8);
        assert!(
            lines.iter().any(|l| l.text.contains("behind")),
            "{:?}",
            texts(&lines)
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
            slept_at_unix: None,
            activity: None,
        }];
        let lines = render(&[space(1, "app")], &agents, 1, 5, 8);
        assert!(
            lines.iter().any(|l| l.text.contains("behind docker")),
            "{:?}",
            texts(&lines)
        );
        assert!(!lines.iter().any(|l| l.text.contains(UNKNOWN)));
    }

    /// The column composes its own rows, so every space in one was put there deliberately.
    /// Cutting them with the grid's rule, where a space after a full width character is a
    /// spacer the engine added, eats padding this module meant and slides the right hand
    /// column one place left.
    #[test]
    fn a_wide_name_does_not_cost_the_row_a_space_the_column_put_there() {
        let agents = vec![agent(1, 1, "\u{4e2d}\u{4e2d}", "idle")];
        let spaces = vec![space(1, "\u{4e2d}\u{6587}")];
        let rows = tree_rows(&spaces, &agents, 1, 8, &Seen::new(), &Shut::new());
        // The space's own rule: the space after its name is one this module wrote, and
        // the grid's rule would have eaten it.
        let rule = rule_row(&rows, 1);
        assert!(
            rule.text
                .starts_with("\u{2500}\u{25be} \u{4e2d}\u{6587} \u{2500}"),
            "{:?}",
            rule.text
        );
        assert_eq!(crate::width::width(&rule.text), WIDTH);
        // The agent's row opens with the state dot and then the name.
        let row = tree_row(&rows, 1);
        assert!(row.text.starts_with("   \u{25cf} "), "{:?}", row.text);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    #[test]
    fn an_absurdly_long_state_never_eats_the_session() {
        let agents = vec![agent(1, 1, "claude", &"z".repeat(80))];
        let rows = tree_rows(
            &[space(1, "app")],
            &agents,
            1,
            8,
            &Seen::new(),
            &Shut::new(),
        );
        // No state word is drawn at all: the row is the dot and the session,
        // however long the daemon's state string runs.
        let row = tree_row(&rows, 1);
        assert_eq!(crate::width::width(&row.text), WIDTH);
        assert_eq!(row.text.trim_end(), "   \u{25cf} claude", "{:?}", row.text);
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
    fn every_agent_draws_one_row_in_daemon_order_under_its_space() {
        let (spaces, agents) = many_agents();
        // Terminal 5 is done and already looked at.
        let mut seen = Seen::new();
        seen.insert((5, 0));
        let rows = rows12(
            &spaces,
            &agents,
            2,
            1,
            40,
            0,
            View::Tree,
            &seen,
            &Unseen::new(),
            NOW,
            WIDTH,
            None,
        );
        let lines: Vec<Line> = rows.iter().map(|(l, _)| l.clone()).collect();
        let texts = texts(&lines);
        // Every agent draws one row under its space, in the daemon's order inside it:
        // terminal 1 (blocked) first because it is listed first, not because of what
        // it needs. Urgency is the pin's job, which lists who needs a person except
        // terminal 1: it has the keyboard, so it is on screen already.
        assert_eq!(order(&rows), vec![1, 3, 5, 6, 9, 2, 4, 7, 8]);
        assert_eq!(pinned(&rows), vec![2]);
        // Static: running agents carry the working dot, no spinner, and no
        // state word on the right.
        assert!(
            lines
                .iter()
                .any(|l| l.indicator == Some((CHILD_DOT_AT, 1, crate::theme::of("working")))),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.ends_with("working")),
            "a state word leaked: {texts:?}"
        );
        // No group headers and no folded rules: the state dot carries the state.
        // (Agent programs here never contain these words, so any hit is chrome.)
        for header in ["running", "finished", "idle", "\u{25b8}"] {
            assert!(
                !texts.iter().any(|t| t.contains(header)),
                "a group header leaked: {header}: {texts:?}"
            );
        }
        // Nine tree rows, and the one the pin repeats.
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            10,
            "{texts:?}"
        );
        // The title names the order on its right.
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(&format!(" {SPACES_TITLE}")) && t.ends_with("tree +")),
            "{texts:?}"
        );
        for line in &lines {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
    }

    #[test]
    fn an_idle_agent_draws_its_row_like_a_working_one() {
        let (spaces, agents) = sample();
        let texts = texts(&folded(
            &spaces,
            &agents,
            1,
            1,
            14,
            0,
            View::Tree,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
        ));
        // Nothing folds on its own: the working agent and the idle one both keep
        // their rows, each with its own state dot.
        assert!(texts.iter().any(|t| t == "   \u{25cf} claude"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "   \u{25cf} codex"), "{texts:?}");
    }

    #[test]
    fn the_foot_of_the_column_carries_the_mark_the_name_the_version_and_the_menu() {
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
            line.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "the menu and the fold mark close the row: {:?}",
            line.text
        );
        assert_eq!(hit, Some(Hit::Foot));
        // The word opens the menu and the rest of the row folds the column.
        let (spaces, agents) = sample();
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = column(
            &spaces,
            &agents,
            1,
            1,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        let menu_at = WIDTH - 6;
        assert_eq!(act(Hit::Foot, menu_at, &c, false), Some(Act::Menu));
        assert_eq!(act(Hit::Foot, WIDTH - 1, &c, false), Some(Act::Collapse));
        assert_eq!(act(Hit::Foot, 2, &c, false), Some(Act::Collapse));
        // The mark and the name wear the accent, and nothing else on the row does.
        let (at, len, role) = line.accent.expect("a wordmark is accented");
        assert_eq!(role, crate::theme::Role::Brand);
        assert_eq!(run_text(&line, at, len), format!("{BRAND_MARK} {BRAND}"));

        // Folded to a strip there is no room for a name, and the fold mark is the row.
        let (strip, hit) = collapse_row(true, WIDTH);
        assert_eq!(crate::width::width(&strip.text), COLLAPSED);
        assert!(!strip.text.contains(BRAND));
        assert_eq!(hit, Some(Hit::Foot));
        assert_eq!(act(Hit::Foot, 2, &c, true), Some(Act::Collapse));
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
        let (line, hit) = collapse_row_with(false, WIDTH, Some(&pet), None);
        assert_eq!(
            crate::width::width(&line.text),
            WIDTH,
            "the row has to fill the column exactly: {:?}",
            line.text
        );
        assert!(line.text.contains(BRAND), "{:?}", line.text);
        assert!(line.text.contains(VERSION), "{:?}", line.text);
        assert!(line.text.contains(MENU_LABEL), "{:?}", line.text);
        assert!(line.text.contains('\u{ab}'), "{:?}", line.text);
        let marks: String = line.text.chars().skip(1).take(2).collect();
        assert_eq!(
            marks,
            format!("{BRAND_MARK}{BRAND_MARK}"),
            "hair dot beside body dot: {:?}",
            line.text
        );
        // The menu word shares the row, so at thirty columns the name is cut, and
        // at the widest column it reads whole.
        assert!(
            line.text.contains("Mohawk\u{2026}"),
            "the pet name reads off the foot: {:?}",
            line.text
        );
        let (widest, _) = collapse_row_with(false, SIDEBAR_MAX, Some(&pet), None);
        assert!(widest.text.contains("Mohawk Pink"), "{widest:?}");
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
        assert_eq!(hit, Some(Hit::Foot));
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
        let (wide, _) = collapse_row_with(false, WIDTH, Some(&pet), None);
        assert_eq!(crate::width::width(&wide.text), WIDTH);
        assert!(
            wide.text.contains('\u{2026}'),
            "fourteen characters never fit at thirty columns: {wide:?}"
        );
        assert!(wide.text.contains("Thin 4\u{2026}"), "{wide:?}");
        assert!(
            wide.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "{wide:?}"
        );
        let (narrow, _) = collapse_row_with(false, 18, Some(&pet), None);
        assert_eq!(crate::width::width(&narrow.text), 18);
        assert!(
            !narrow.text.contains("Thin"),
            "no smudge at eighteen columns: {narrow:?}"
        );
        assert!(narrow.text.contains(BRAND), "{narrow:?}");
        assert!(narrow.text.contains(MENU_LABEL), "{narrow:?}");
    }

    /// Without an avatar the foot is byte for byte what it always was.
    #[test]
    fn without_an_avatar_the_foot_is_unchanged() {
        let (line, _) = collapse_row_with(false, WIDTH, None, None);
        assert_eq!(
            line.accent,
            Some((1, crate::width::width(BRAND) + 2, crate::theme::Role::Brand))
        );
        assert_eq!(line.indicator, None);
        assert!(line.text.contains(&format!("{BRAND_MARK} {BRAND}")));
    }

    fn art_pet() -> qoro_runtime::avatar::Avatar {
        qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 33,
        }
    }

    /// The art fits an open column of decent size, and nothing else.
    #[test]
    fn art_fits_an_open_column_of_decent_size_only() {
        assert!(avatar_fits(30, 30, true));
        assert!(avatar_fits(12, 30, true));
        assert!(!avatar_fits(30, 30, false));
        assert!(!avatar_fits(30, 17, true));
        assert!(!avatar_fits(11, 30, true));
    }

    /// Thirty working agents in one space: more than a thirty row column holds, so the
    /// rows above the foot are list rows unless a pet takes them.
    fn crowd() -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
        (
            vec![space(1, "app")],
            (1..=30u32)
                .map(|i| agent(i, 1, "claude", "working"))
                .collect(),
        )
    }

    /// With a pet the five rows above the foot go blank for the overlay, the
    /// foot keeps its dots and name, and the column keeps its exact height.
    #[test]
    fn a_pet_reserves_five_blank_rows_above_the_foot() {
        let pet = art_pet();
        let (spaces, agents) = crowd();
        let rows = rows12(
            &spaces,
            &agents,
            1,
            1,
            30,
            0,
            View::Tree,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
            Some(&pet),
        );
        assert_eq!(rows.len(), 30);
        for (line, hit) in rows[24..29].iter() {
            assert_eq!(line.text, " ".repeat(WIDTH), "{line:?}");
            assert_eq!(*hit, None);
        }
        // The list is laid out in the rows above the pet: its last row is an agent.
        assert!(
            matches!(rows[23].1, Some(Hit::Terminal(_))),
            "{:?}",
            rows[23]
        );
        let (foot, hit) = &rows[29];
        assert!(foot.text.contains("Mohawk"), "{foot:?}");
        assert_eq!(*hit, Some(Hit::Foot));
    }

    /// The reservation eats list rows, not padding: without a pet those rows
    /// carry agents.
    #[test]
    fn without_a_pet_those_rows_carry_agents() {
        let (spaces, agents) = crowd();
        let plain = rows12(
            &spaces,
            &agents,
            1,
            1,
            30,
            0,
            View::Tree,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
            None,
        );
        assert!(
            plain[24..29]
                .iter()
                .all(|(_, hit)| matches!(hit, Some(Hit::Terminal(_)))),
            "thirty agents must reach rows 24 to 28"
        );
    }

    /// Overlay rows sit directly above the foot, top first.
    #[test]
    fn overlay_rows_sit_directly_above_the_foot() {
        assert_eq!(avatar_overlay_rows(1, 30), vec![25, 26, 27, 28, 29]);
    }

    /// The overlay paints the pet in its own inks: five sixteen-cell rows,
    /// each reset at the end, empty without colour.
    #[test]
    fn the_overlay_paints_the_pet_in_its_inks() {
        let rows = avatar_overlay(&art_pet(), true);
        assert_eq!(rows.len(), 5);
        for row in &rows {
            assert!(row.ends_with("\x1b[0m"), "{row:?}");
            let cells = row
                .replace("\x1b[38;5;212m", "")
                .replace("\x1b[38;5;33m", "")
                .replace("\x1b[38;5;231m", "")
                .replace("\x1b[48;5;212m", "")
                .replace("\x1b[48;5;33m", "")
                .replace("\x1b[48;5;231m", "")
                .replace("\x1b[39m", "")
                .replace("\x1b[49m", "")
                .replace("\x1b[0m", "")
                .replace(['\u{2580}', ' '], "");
            assert!(cells.is_empty(), "unaccounted cells: {row:?}");
        }
        assert!(rows[0].contains("38;5;212"), "hair on top: {:?}", rows[0]);
        assert!(avatar_overlay(&art_pet(), false).is_empty());
    }

    /// A seen update signs the foot of the column with where this build stands.
    #[test]
    fn a_seen_update_names_itself_beside_the_version() {
        // Said directly rather than through the notice file: every foot painted by any
        // test in this process reads that file, so writing it here changed their rows
        // and failed them at random.
        let (line, _) = collapse_row_with(false, WIDTH, None, Some("9.9.9"));
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
        assert!(
            line.text.contains(MENU_LABEL),
            "the notice never pushes the menu off: {:?}",
            line.text
        );
        let (plain, _) = collapse_row_with(false, WIDTH, None, None);
        assert!(
            !plain.text.contains('>'),
            "no notice is the row it always was: {:?}",
            plain.text
        );
    }

    /// A done row is unread until the keyboard has been there: not focused,
    /// and no receipt in the seen set.
    #[test]
    fn unread_is_a_done_row_nobody_has_looked_at() {
        let (_, agents) = many_agents();
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
    /// calm: the same sage as a daemon done, until focused.
    #[test]
    fn new_is_an_idle_row_that_finished_while_away() {
        let (_, agents) = many_agents();
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
    fn priority_view_sorts_by_urgency_without_headers_or_a_pin() {
        let (spaces, agents) = many_agents();
        let rows = rows12(
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
            None,
        );
        let lines: Vec<Line> = rows.iter().map(|(l, _)| l.clone()).collect();
        let texts = texts(&lines);
        for header in [PIN_TITLE, "running", "finished"] {
            assert!(
                !texts.iter().any(|t| t.trim_start().starts_with(header)),
                "a header leaked into the priority view: {texts:?}"
            );
        }
        // The title opens the column: the list is already who needs a person.
        assert_eq!(rows[0].1, Some(Hit::Title));
        assert!(texts[0].ends_with("priority +"), "{texts:?}");
        // No rules either: a flat list.
        assert!(!rows.iter().any(|(_, h)| matches!(h, Some(Hit::Space(_)))));
        // Urgency order without state words: the dots carry the states, so
        // the terminals run blocked, failed, the unread done, then running.
        // Terminals: 1 blocked, 2 failed, 5 unread done, 3 and 4 working.
        let order = order(&rows);
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
        // Every agent draws one row.
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            9,
            "{texts:?}"
        );
    }

    /// Static column: the tick changes nothing, every dot keeps its colour
    /// and no state word is ever drawn.
    #[test]
    fn the_column_is_static_across_ticks() {
        let (spaces, agents) = many_agents();
        let at = |tick: u64| {
            rows12(
                &spaces,
                &agents,
                2,
                1,
                40,
                tick,
                View::Tree,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
                None,
            )
        };
        let zero = at(0);
        assert_eq!(zero, at(4), "the tick repainted a static column");
        // Every agent's row opens with its state dot, steady.
        assert_eq!(
            tree_row(&zero, 1).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("blocked"))),
            "no static blocked dot"
        );
        // An unread done keeps the done sage on its dot, steady.
        assert_eq!(
            tree_row(&zero, 5).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("done"))),
            "no static unread dot"
        );
        // And no state word on the right of any row.
        for (line, _) in &zero {
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
        // A failed agent renders the diamond in both of its rows, steady: flat in the
        // pin and under its rule in the tree, each carrying the failed colour and each
        // reaching the failed terminal. The keyboard is elsewhere, or the pin would
        // leave the failed agent to its own row.
        let spaces = vec![space(2, "webshop")];
        let agents = vec![agent(2, 2, "claude", "failed")];
        let lines = render(&spaces, &agents, 2, 99, 8);
        for (prefix, at) in [
            (" \u{25c6} claude", DOT_AT),
            ("   \u{25c6} claude", CHILD_DOT_AT),
        ] {
            let row = lines
                .iter()
                .position(|l| l.text.starts_with(prefix))
                .unwrap_or_else(|| panic!("no diamond row {prefix:?}: {:?}", texts(&lines)));
            assert_eq!(
                lines[row].indicator,
                Some((at, 1, crate::theme::of("failed")))
            );
            assert_eq!(hit(&spaces, &agents, 2, 99, 8, row), Some(Hit::Terminal(2)));
        }
        // An unseen failed finish keeps the diamond: failed is never news.
        let mut unseen = Unseen::new();
        unseen.insert((2, gen_of(&agents[0])));
        let fresh_lines = folded(
            &spaces,
            &agents,
            2,
            1,
            40,
            0,
            View::Tree,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
        );
        assert!(
            fresh_lines
                .iter()
                .any(|l| l.text.starts_with("   \u{25c6} ")),
            "unseen failed lost its diamond: {:?}",
            texts(&fresh_lines)
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
        let lines = collapsed5(&[space(1, "app"), space(2, "webshop")], &agents, 1, 1, 12);
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

    /// The strip is the tree in four columns: each space's number, then its agents'
    /// marks right under it, so a mark sits under the space it is in. A folded space
    /// keeps its number and the keyboard's own agent only; a stranger comes last.
    #[test]
    fn the_collapsed_strip_lists_each_space_with_its_agents_under_it() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let agents = vec![
            agent(1, 2, "codex", "idle"),
            agent(2, 1, "claude", "working"),
            agent(3, 2, "claude", "blocked"),
            agent(4, 77, "claude", "idle"),
            agent(5, 1, "codex", "idle"),
        ];
        let strip = |focused: u32, shut: &Shut| {
            let (seen, unseen) = (Seen::new(), Unseen::new());
            rows_collapsed(
                &column(
                    &spaces,
                    &agents,
                    1,
                    focused,
                    0,
                    View::Tree,
                    &seen,
                    &unseen,
                    NOW,
                    WIDTH,
                    shut,
                ),
                12,
            )
        };
        let rows = strip(9, &Shut::new());
        let hits: Vec<Hit> = rows.iter().filter_map(|(_, h)| *h).collect();
        assert_eq!(
            hits,
            vec![
                Hit::Space(1),
                Hit::Terminal(2),
                Hit::Terminal(5),
                Hit::Space(2),
                Hit::Terminal(1),
                Hit::Terminal(3),
                Hit::Terminal(4),
                Hit::Foot,
            ]
        );
        assert_eq!(rows[0].0.text, " 1  ");
        assert_eq!(rows[3].0.text, " 2  ");
        assert_eq!(rows.len(), 12);
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), COLLAPSED, "{line:?}");
        }
        // Folding app keeps its number and the focused agent, and drops the other.
        let rows = strip(5, &Shut::from([1]));
        let hits: Vec<Hit> = rows.iter().filter_map(|(_, h)| *h).collect();
        assert_eq!(
            &hits[..3],
            &[Hit::Space(1), Hit::Terminal(5), Hit::Space(2)]
        );
    }

    /// Freshness points at done: an idle agent finished while the keyboard was
    /// away wears the done circle, never a diamond, because failed is never news.
    #[test]
    fn a_fresh_idle_wears_the_done_circle() {
        let agents = vec![agent(6, 1, "claude", "idle")];
        let mut unseen = Unseen::new();
        unseen.insert((6, gen_of(&agents[0])));
        assert!(is_new(&agents[0], 1, &Seen::new(), &unseen));
        let rows = rows12(
            &[space(1, "app")],
            &agents,
            1,
            1,
            40,
            0,
            View::Tree,
            &Seen::new(),
            &unseen,
            NOW,
            WIDTH,
            None,
        );
        // Both its rows, the pin's and the tree's, wear the done circle.
        let mine: Vec<&Line> = rows
            .iter()
            .filter(|(_, h)| *h == Some(Hit::Terminal(6)))
            .map(|(l, _)| l)
            .collect();
        assert_eq!(mine.len(), 2);
        for row in mine {
            let (at, _, role) = row.indicator.expect("a dot");
            assert_eq!(
                row.text.chars().nth(at),
                Some(DOT),
                "fresh idle is not a circle: {:?}",
                row.text
            );
            assert_eq!(role, crate::theme::of("done"));
        }
    }

    #[test]
    fn view_toggle_flips_between_tree_and_priority() {
        assert_eq!(View::Tree.toggle(), View::Priority);
        assert_eq!(View::Priority.toggle(), View::Tree);
        assert_eq!(View::default(), View::Tree);
    }

    /// The reported bug: a working agent listed after idle ones must stay
    /// last in the tree, and so must a blocked one. The tree never reorders;
    /// urgency is the priority view's job, where both jump to the top.
    #[test]
    fn the_tree_keeps_daemon_order_while_priority_sorts_by_urgency() {
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
            for view in [View::Tree, View::Priority] {
                let rows = rows12(
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
                    None,
                );
                let order = order(&rows);
                if view == View::Tree {
                    assert_eq!(order, vec![1, 2, 3], "the tree reordered: {order:?}");
                } else {
                    assert_eq!(order, vec![3, 1, 2], "priority did not sort: {order:?}");
                }
            }
        }
    }

    /// A column for the click tests: the sample, in the given order and width.
    fn clicks<'a>(
        spaces: &'a [WorkspaceRow],
        agents: &'a [AgentRow],
        view: View,
        width: usize,
        none: &'a (Seen, Unseen, Shut),
    ) -> Column<'a> {
        column(
            spaces, agents, 1, 1, 0, view, &none.0, &none.1, NOW, width, &none.2,
        )
    }

    /// Only the order word flips the view and only the `+` makes a space: a click on
    /// the word `spaces`, or in the gap, does nothing anybody did not ask for.
    #[test]
    fn only_the_mode_word_of_the_title_flips_the_view() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for view in [View::Tree, View::Priority] {
            let c = clicks(&spaces, &agents, view, WIDTH, &none);
            assert_eq!(act(Hit::Title, 1, &c, false), None, "the spaces word");
            assert_eq!(act(Hit::Title, 5, &c, false), None, "mid title");
            assert_eq!(act(Hit::Title, WIDTH - 1, &c, false), None, "the gap");
            assert_eq!(act(Hit::Title, WIDTH - 2, &c, false), Some(Act::View));
            assert_eq!(act(Hit::Title, WIDTH, &c, false), Some(Act::NewSpace));
        }
    }

    /// Where the painted words are, the clicks are: at every width the columns that
    /// flip the view spell the order word, the ones that open the menu spell `menu`,
    /// the `+` is the last column, and the fold cells are the rule's dash and arrow.
    #[test]
    fn act_resolves_title_foot_and_space_by_column_at_every_width() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for width in [SIDEBAR_MIN, WIDTH, SIDEBAR_MAX] {
            for view in [View::Tree, View::Priority] {
                let c = clicks(&spaces, &agents, view, width, &none);
                let rows = rows_folded(&c, 20, None);
                let title = rows
                    .iter()
                    .find(|(_, h)| *h == Some(Hit::Title))
                    .map(|(l, _)| l.text.clone())
                    .expect("a title");
                let word: String = (1..=width)
                    .filter(|col| act(Hit::Title, *col, &c, false) == Some(Act::View))
                    .filter_map(|col| title.chars().nth(col - 1))
                    .collect();
                assert_eq!(word, title_mode(view), "width {width}: {title:?}");
                assert_eq!(title.chars().nth(width - 1), Some('+'));
                assert_eq!(act(Hit::Title, width, &c, false), Some(Act::NewSpace));
                assert_eq!(act(Hit::Title, 1, &c, false), None);

                let foot = rows.last().map(|(l, _)| l.text.clone()).expect("a foot");
                let menu: String = (1..=width)
                    .filter(|col| act(Hit::Foot, *col, &c, false) == Some(Act::Menu))
                    .filter_map(|col| foot.chars().nth(col - 1))
                    .collect();
                assert_eq!(menu, MENU_LABEL, "width {width}: {foot:?}");
                for col in [1, width - 1, width] {
                    assert_eq!(
                        act(Hit::Foot, col, &c, false),
                        Some(Act::Collapse),
                        "width {width} col {col}"
                    );
                }

                // A rule folds at its dash and arrow, in the tree only, and only when
                // there is something under it to fold.
                for col in 1..=width {
                    let want = if view == View::Tree && col <= FOLD_CELLS {
                        Act::Fold(1)
                    } else {
                        Act::Space(1)
                    };
                    assert_eq!(act(Hit::Space(1), col, &c, false), Some(want));
                    assert_eq!(act(Hit::Space(3), col, &c, false), Some(Act::Space(3)));
                }
                assert_eq!(act(Hit::Space(1), 1, &c, true), Some(Act::Space(1)));
                assert_eq!(act(Hit::Foot, 1, &c, true), Some(Act::Collapse));
                assert_eq!(act(Hit::Terminal(3), 1, &c, false), Some(Act::Terminal(3)));
            }
        }
        // The fold cells are exactly the rule's dash, arrow and the space after them.
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        let head: String = rule_row(&rows, 1).text.chars().take(FOLD_CELLS).collect();
        assert_eq!(head, "\u{2500}\u{25be} ");
    }

    /// No breath between the title and the tree any more: the first rule is itself a
    /// separator, so the row under the title is already the first space.
    #[test]
    fn the_tree_opens_right_under_the_title() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "idle")];
        let rows = tree_rows(&spaces, &agents, 1, 10, &Seen::new(), &Shut::new());
        assert_eq!(rows[0].1, Some(Hit::Title));
        assert_eq!(rows[1].1, Some(Hit::Space(1)));
        assert_eq!(rows[2].1, Some(Hit::Terminal(1)));
    }

    /// A blocked agent is pinned at the top, where a person looks first, and still
    /// draws its row under its space: the pin is a place to glance, not a move. The
    /// pin ends in a rule of its own grey, clickable as nothing.
    #[test]
    fn a_blocked_agent_is_pinned_and_still_drawn_under_its_space() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "blocked")];
        let rows = tree_rows(&spaces, &agents, 9, 10, &Seen::new(), &Shut::new());
        let lines: Vec<Line> = rows.iter().map(|(l, _)| l.clone()).collect();
        let texts = texts(&lines);
        assert!(texts[0].starts_with(&format!(" {PIN_TITLE}")), "{texts:?}");
        assert!(texts[0].ends_with(" 1"), "{texts:?}");
        assert_eq!(lines[0].colour, Some(crate::theme::Role::Title));
        assert_eq!(rows[0].1, None);
        assert!(texts[1].starts_with(" \u{25cf} codex"), "{texts:?}");
        assert!(texts[1].ends_with(" app"), "{texts:?}");
        assert_eq!(rows[1].1, Some(Hit::Terminal(1)));
        assert_eq!(lines[2].text, "\u{2500}".repeat(WIDTH));
        assert_eq!(lines[2].colour, Some(crate::theme::Role::Rule));
        assert_eq!(rows[2].1, None);
        assert_eq!(rows[3].1, Some(Hit::Title));
        assert_eq!(order(&rows), vec![1]);
        assert!(!texts.iter().any(|t| t.contains("idle")), "{texts:?}");
    }

    /// The pin exists only while somebody needs a person, and lists them by what they
    /// need: blocked first, then failed, then a finish nobody has read. Working and
    /// calm agents are the tree's business alone.
    #[test]
    fn the_pin_appears_only_when_needed_and_lists_blocked_then_failed_then_unread_done() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let agents = vec![
            agent(1, 1, "claude", "done"),
            agent(2, 1, "codex", "failed"),
            agent(3, 2, "claude", "working"),
            agent(4, 2, "codex", "blocked"),
        ];
        let rows = tree_rows(&spaces, &agents, 9, 20, &Seen::new(), &Shut::new());
        assert_eq!(pinned(&rows), vec![4, 2, 1]);
        assert!(rows[0].0.text.trim_end().ends_with(" 3"), "{:?}", rows[0].0);
        // Each pinned row names the space it lives in, in the space's colour.
        let blocked = &rows[1].0;
        assert!(blocked.text.trim_end().ends_with(" dub"), "{blocked:?}");
        assert_eq!(
            blocked.accent.map(|(_, _, role)| role),
            Some(crate::theme::space_of(2))
        );
        assert_eq!(
            blocked.indicator,
            Some((DOT_AT, 1, crate::theme::Role::Blocked))
        );
        // Calm the fleet down and the pin is gone: the title opens the column.
        let mut calm = agents.clone();
        calm[1].state = "idle".into();
        calm[3].state = "working".into();
        let mut seen = Seen::new();
        seen.insert((1, 0));
        let rows = tree_rows(&spaces, &calm, 9, 20, &seen, &Shut::new());
        assert_eq!(rows[0].1, Some(Hit::Title));
        assert!(pinned(&rows).is_empty());
        assert!(
            !rows.iter().any(|(l, _)| l.text.contains(PIN_TITLE)),
            "an empty pin drew"
        );
    }

    /// Every height from nothing to a tall window, every pulled width, with and without a
    /// pet, a pin and a three digit count: always exactly the rows asked for, each exactly
    /// the width, open or folded to the strip. A short row leaves the last frame's pixels
    /// behind and a long one spills into the terminal beside the column.
    #[test]
    fn every_height_and_width_paints_exact_rows_with_pet_pin_and_big_counts() {
        let spaces = vec![
            space(1, "a-rather-long-space-name"),
            space(2, "\u{4e2d}\u{6587}"),
        ];
        let mut agents: Vec<AgentRow> = (0..120)
            .map(|i| {
                agent(
                    i + 1,
                    1,
                    "claude",
                    if i % 7 == 0 { "blocked" } else { "working" },
                )
            })
            .collect();
        agents.push(agent(500, 2, "codex", "failed"));
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 33,
        };
        let (seen, unseen) = (Seen::new(), Unseen::new());
        for shut in [Shut::new(), Shut::from([1])] {
            for width in SIDEBAR_MIN..=SIDEBAR_MAX {
                for height in 0..=40 {
                    for avatar in [None, Some(&pet)] {
                        let c = column(
                            &spaces,
                            &agents,
                            1,
                            3,
                            0,
                            View::Tree,
                            &seen,
                            &unseen,
                            NOW,
                            width,
                            &shut,
                        );
                        let rows = rows_folded(&c, height, avatar);
                        assert_eq!(rows.len(), height, "width {width} height {height}");
                        for (line, _) in &rows {
                            assert_eq!(
                                crate::width::width(&line.text),
                                width,
                                "width {width} height {height}: {:?}",
                                line.text
                            );
                            let end = crate::width::width(&line.text);
                            for (at, len, _) in line.runs.iter().chain(line.accent.iter()) {
                                assert!(at + len <= end, "a run past the row: {line:?}");
                            }
                        }
                        let strip = rows_collapsed(&c, height);
                        assert_eq!(strip.len(), height, "strip height {height}");
                        for (line, _) in &strip {
                            assert_eq!(crate::width::width(&line.text), COLLAPSED, "{line:?}");
                        }
                    }
                }
            }
        }
    }

    /// Every agent's tool names land on the same few words a person would say.
    #[test]
    fn humanize_puts_every_agents_tools_in_a_persons_words() {
        for (raw, said) in [
            (
                "Edit /Users/me/qoro/crates/qoro-tui/src/sidebar.rs",
                "editing sidebar.rs",
            ),
            ("Write src/new.rs", "editing new.rs"),
            ("apply_patch src/a.rs", "editing a.rs"),
            ("Read /x/y/README.md", "reading README.md"),
            ("read_file /x/src/main.rs", "reading main.rs"),
            ("Bash cargo test -p qoro-tui", "cargo test -p qoro-tui"),
            (
                "shell bash -lc git worktree add ../wt",
                "git worktree add ../wt",
            ),
            ("run_shell_command npm run build", "npm run build"),
            ("Grep TODO", "searching TODO"),
            ("glob **/*.rs", "searching **/*.rs"),
            ("Task", "subagent"),
            ("WebFetch https://example.com", "web"),
            ("TodoWrite", "planning"),
            ("thinking", "thinking"),
            ("mcp__crw__scrape https://x", "mcp__crw__scrape https://x"),
            ("Bash", "running"),
            ("shell bash -lc 'git status'", "git status"),
            ("list /x/src", "searching /x/src"),
        ] {
            assert_eq!(humanize(raw), said, "{raw}");
        }
    }

    /// A working agent says what it is doing on a row of its own under its name; a waiting
    /// one says what it wants in the blocked colour, and the pin says it on the same row.
    /// A calm agent keeps one row, whatever its last tool was.
    #[test]
    fn activity_rides_a_sub_row_in_the_tree_and_the_same_row_in_the_pin() {
        let spaces = vec![space(1, "app")];
        let mut working = agent(1, 1, "claude", "working");
        working.activity = Some("Edit src/sidebar.rs".into());
        let mut blocked = agent(2, 1, "codex", "blocked");
        blocked.activity = Some("Bash bun test".into());
        let mut idle = agent(3, 1, "gemini", "idle");
        idle.activity = Some("Read a.rs".into());
        let agents = vec![working, blocked, idle];
        let rows = tree_rows(&spaces, &agents, 9, 30, &Seen::new(), &Shut::new());
        let texts: Vec<String> = rows
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        let under = |terminal: u32| -> Vec<&Line> {
            rows.iter()
                .skip_while(|(_, h)| *h != Some(Hit::Title))
                .filter(|(_, h)| *h == Some(Hit::Terminal(terminal)))
                .map(|(l, _)| l)
                .collect()
        };
        let one = under(1);
        assert_eq!(one.len(), 2, "{texts:?}");
        assert_eq!(one[1].text.trim_end(), "     \u{2514} editing sidebar.rs");
        assert_eq!(one[1].colour, Some(crate::theme::Role::Faint));
        let two = under(2);
        assert_eq!(two[1].text.trim_end(), "     \u{2514} wants: bun test");
        assert_eq!(two[1].colour, Some(crate::theme::Role::Blocked));
        assert_eq!(under(3).len(), 1, "a calm agent keeps one row: {texts:?}");
        // The pin names what the waiting agent wants on its own row.
        assert!(
            texts[1].starts_with(" \u{25cf} codex - wants: bun"),
            "{texts:?}"
        );
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{line:?}");
        }
    }

    /// A waiting agent whose newest word is `thinking` has not said what it wants: no
    /// `wants: thinking` row, in the tree or in the pin.
    #[test]
    fn a_wait_with_nothing_said_draws_no_activity() {
        let spaces = vec![space(1, "app")];
        let mut blocked = agent(2, 1, "claude", "blocked");
        blocked.activity = Some("thinking".into());
        let rows = tree_rows(&spaces, &[blocked], 9, 30, &Seen::new(), &Shut::new());
        assert!(
            !rows.iter().any(|(l, _)| l.text.contains("thinking")),
            "{:?}",
            rows.iter()
                .map(|(l, _)| l.text.trim_end())
                .collect::<Vec<_>>()
        );
    }
    /// A pin with room for one agent shows that agent, and the title's count says how
    /// many more there are: a title over a `+2 more` and nobody is a pin that names no one.
    #[test]
    fn a_pin_with_room_for_one_agent_shows_it() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(8, 1, "claude", "blocked"),
            agent(9, 1, "codex", "blocked"),
        ];
        let rows = tree_rows(&spaces, &agents, 1, 8, &Seen::new(), &Shut::new());
        assert_eq!(pinned(&rows), vec![8], "{rows:?}");
        assert!(
            rows[0].0.text.trim_end().ends_with('2'),
            "the title counts both: {:?}",
            rows[0].0.text
        );
        assert!(
            !rows.iter().any(|(l, _)| l.text.contains("more")),
            "no room was left for a count row: {rows:?}"
        );
    }

    /// The pin is a glance, not a second list: five rows at most, and past that four
    /// agents and a count of the rest, so the tree keeps the column. Every agent it
    /// leaves out is still a row under its space.
    #[test]
    fn the_pin_caps_at_five_and_counts_the_rest() {
        let spaces = vec![space(1, "app")];
        let blocked = |n: u32| -> Vec<AgentRow> {
            (1..=n).map(|t| agent(t, 1, "claude", "blocked")).collect()
        };
        let five = blocked(5);
        let rows = tree_rows(&spaces, &five, 9, 40, &Seen::new(), &Shut::new());
        assert_eq!(pinned(&rows), vec![1, 2, 3, 4, 5]);
        assert!(!rows.iter().any(|(l, _)| l.text.contains("more")));

        let eight = blocked(8);
        let rows = tree_rows(&spaces, &eight, 9, 40, &Seen::new(), &Shut::new());
        assert_eq!(pinned(&rows), vec![1, 2, 3, 4]);
        assert!(rows[0].0.text.trim_end().ends_with(" 8"), "{:?}", rows[0].0);
        let more = &rows[5];
        assert_eq!(more.0.text.trim_end(), "   +4 more");
        assert_eq!(more.0.colour, Some(crate::theme::Role::Faint));
        assert_eq!(more.1, None);
        assert_eq!(rows[6].0.colour, Some(crate::theme::Role::Rule));
        assert_eq!(order(&rows), (1..=8).collect::<Vec<u32>>());

        // A short column holds the pin to half its rows.
        let rows = tree_rows(&spaces, &eight, 9, 12, &Seen::new(), &Shut::new());
        assert_eq!(pinned(&rows), vec![1, 2]);
        assert!(rows.iter().any(|(l, _)| l.text.trim_end() == "   +6 more"));
    }

    /// The priority list is already who needs a person, on top, so a pin over it would
    /// say everything twice.
    #[test]
    fn the_pin_is_hidden_in_the_priority_view() {
        let (spaces, agents) = sample_pinned();
        let rows = rows12(
            &spaces,
            &agents,
            1,
            9,
            20,
            0,
            View::Priority,
            &Seen::new(),
            &Unseen::new(),
            NOW,
            WIDTH,
            None,
        );
        assert_eq!(rows[0].1, Some(Hit::Title));
        assert!(!rows.iter().any(|(l, _)| l.text.contains(PIN_TITLE)));
        assert_eq!(
            order(&rows)[0],
            3,
            "the blocked agent leads the list instead"
        );
    }

    /// A folded space hides the agents a person is not looking at, never the one the
    /// keyboard is on: folding is tidying, not closing a tab somebody is typing into.
    #[test]
    fn a_folded_space_hides_calm_agents_but_keeps_the_focused_one() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let agents = vec![
            agent(1, 1, "claude", "idle"),
            agent(2, 1, "codex", "idle"),
            agent(3, 1, "claude", "working"),
            agent(4, 2, "codex", "idle"),
        ];
        let shut = Shut::from([1]);
        let rows = tree_rows(&spaces, &agents, 2, 14, &Seen::new(), &shut);
        assert_eq!(order(&rows), vec![2, 4]);
        let app = rule_row(&rows, 1);
        assert!(
            app.text.starts_with("\u{2500}\u{25b8} app "),
            "{:?}",
            app.text
        );
        assert!(
            app.text.ends_with(" 3 \u{2500}"),
            "the count keeps every agent"
        );
        // Off the space, the fold hides all three.
        let rows = tree_rows(&spaces, &agents, 4, 14, &Seen::new(), &shut);
        assert_eq!(order(&rows), vec![4]);
        // And the open space next to it is untouched.
        assert!(
            rule_row(&rows, 2).text.starts_with("\u{2500}\u{25be} dub "),
            "{:?}",
            rule_row(&rows, 2).text
        );
    }

    /// A fold never hides a blocked agent silently: a folded rule's arrow and count
    /// take the colour of its most urgent child that needs a person, and stay the
    /// quiet grey while every child is calm. Open, they stay grey: the dots are right
    /// there.
    #[test]
    fn a_folded_rule_wears_the_colour_of_its_most_urgent_child() {
        use crate::theme::Role;
        let spaces = vec![space(1, "app")];
        let paint = |states: &[&str], shut: &Shut| -> Line {
            let agents: Vec<AgentRow> = states
                .iter()
                .enumerate()
                .map(|(i, s)| agent(i as u32 + 1, 1, "claude", s))
                .collect();
            let rows = tree_rows(&spaces, &agents, 9, 20, &Seen::new(), shut);
            rule_row(&rows, 1).clone()
        };
        // The colour of the arrow and of the count, read off the painted runs.
        let marks = |rule: &Line| -> (Role, Role) {
            let arrow = rule.indicator.expect("an arrow").2;
            let (_, _, count) = rule
                .runs
                .iter()
                .find(|(at, len, _)| {
                    run_text(rule, *at, *len)
                        .chars()
                        .all(|c| c.is_ascii_digit())
                })
                .copied()
                .expect("a count run");
            (arrow, count)
        };
        let shut = Shut::from([1]);
        assert_eq!(
            marks(&paint(&["idle", "failed", "blocked"], &shut)),
            (Role::Blocked, Role::Blocked)
        );
        assert_eq!(
            marks(&paint(&["idle", "failed"], &shut)),
            (Role::Failed, Role::Failed)
        );
        assert_eq!(
            marks(&paint(&["idle", "done"], &shut)),
            (Role::Done, Role::Done),
            "an unread finish is news"
        );
        assert_eq!(
            marks(&paint(&["idle", "working"], &shut)),
            (Role::Faint, Role::Faint)
        );
        assert_eq!(
            marks(&paint(&["idle", "blocked"], &Shut::new())),
            (Role::Faint, Role::Faint),
            "an open rule stays grey"
        );
    }

    /// The keyboard's row is always drawn: nothing folds it away on its own, so a
    /// focused terminal never reads as a terminal that died.
    #[test]
    fn the_focused_agent_always_draws_its_row() {
        let (spaces, agents) = many_agents();
        for focused in [1u32, 5, 9] {
            let hits: Vec<u32> = (0..30)
                .filter_map(|row| {
                    hit12(
                        &spaces,
                        &agents,
                        1,
                        focused,
                        30,
                        row,
                        View::Tree,
                        &Seen::new(),
                        &Unseen::new(),
                        NOW,
                        WIDTH,
                        None,
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
        let rows = tree_rows(&spaces, &agents, 1, 10, &Seen::new(), &Shut::new());
        assert_eq!(tree_row(&rows, 1).text.trim_end(), "   \u{25cf} sh");
    }

    /// An unread done draws inline in its space with the done sage on its dot: news,
    /// not calm, and no header above it.
    #[test]
    fn an_unread_done_draws_in_its_space_with_the_done_dot() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "claude", "done")];
        let rows = tree_rows(&spaces, &agents, 2, 10, &Seen::new(), &Shut::new());
        assert_eq!(
            tree_row(&rows, 1).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("done")))
        );
        assert!(
            !rows.iter().any(|(l, _)| l.text.contains('\u{25b8}')),
            "a folded rule leaked: {rows:?}"
        );
    }

    /// A stale idle row dims as a whole and sinks below fresh idle of the
    /// same rank in the priority view, while the tree never moves.
    ///
    /// Dim is exact roles: the row `Faint` with a `Faint` dot, and the space accent
    /// still the space's own ink. And only off focus: standing on a row is reading it.
    #[test]
    fn idle_stale_dims_and_sinks_in_priority_only() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent_at(1, 1, "claude", "idle", 10_000),
            agent_at(2, 1, "codex", "idle", 60),
        ];
        let priority = folded(
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
        let listed: Vec<String> = texts(&priority);
        // The fresh codex sorts above the stale claude of the same rank.
        let fresh_at = listed
            .iter()
            .position(|t| t.contains("codex"))
            .expect("fresh row");
        let stale_at = listed
            .iter()
            .position(|t| t.contains("claude"))
            .expect("stale row");
        assert!(fresh_at < stale_at, "{listed:?}");
        let stale = &priority[stale_at];
        assert_eq!(stale.colour, Some(crate::theme::Role::Faint));
        assert_eq!(
            stale.indicator,
            Some((DOT_AT, 1, crate::theme::Role::Faint))
        );
        assert_eq!(
            stale.accent,
            Some((
                WIDTH - 4,
                crate::width::width("app"),
                crate::theme::space_of(1)
            )),
            "the space accent never dims with the state"
        );
        // Dim is roles only: the stale row's text reads exactly as its undimmed twin's.
        let twins = folded(
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
        let twin = twins
            .iter()
            .find(|l| l.text.contains("claude"))
            .expect("the twin");
        assert_eq!(stale.text, twin.text);
        assert_eq!(twin.colour, None);

        // The tree keeps daemon order, dimmed or not.
        let rows = tree_rows(&spaces, &agents, 9, 14, &Seen::new(), &Shut::new());
        assert_eq!(order(&rows), vec![1, 2], "the tree moved a row");
        assert_eq!(
            tree_row(&rows, 1).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::Role::Faint))
        );
        assert_eq!(tree_row(&rows, 1).colour, Some(crate::theme::Role::Faint));
        // Focused, the same stale row is read, and reads at full strength.
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        assert_eq!(tree_row(&rows, 1).colour, None);
        assert_eq!(
            tree_row(&rows, 1).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::of("idle")))
        );
    }

    /// Staleness never fades news. A stale idle that finished while the
    /// keyboard was away keeps the done sage instead of dimming.
    #[test]
    fn news_beats_stale_dim() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent_at(1, 1, "claude", "idle", 10_000)];
        let mut unseen = Unseen::new();
        unseen.insert((1, 0));
        let lines = folded(
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
                .any(|l| l.indicator == Some((DOT_AT, 1, crate::theme::of("done")))),
            "a new row dimmed instead of keeping its sage: {:?}",
            texts(&lines)
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, 1, crate::theme::Role::Faint))),
            "a new row faded"
        );
        let first = lines
            .iter()
            .find(|l| l.indicator.is_some())
            .expect("a dot row");
        assert_eq!(first.colour, None, "a new row dimmed: {:?}", texts(&lines));
    }

    /// An idle row without a stamp paints byte for byte as it always did,
    /// at every clock: the whole painted column is identical, not just the
    /// dot.
    #[test]
    fn idle_without_a_stamp_paints_as_it_always_did() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "claude", "idle")];
        let paint = |now| {
            folded(
                &spaces,
                &agents,
                1,
                1,
                14,
                0,
                View::Tree,
                &Seen::new(),
                &Unseen::new(),
                now,
                WIDTH,
            )
        };
        assert_eq!(paint(0), paint(NOW));
        assert_eq!(paint(NOW), paint(NOW + 1_000_000));
    }

    /// Tier boundaries resolve at paint, exactly at 900 and 7200 seconds.
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
            folded(
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
                .all(|l| l.indicator != Some((DOT_AT, 1, crate::theme::Role::Faint))),
            "plain row already dim"
        );
        let after = paint(NOW + 2);
        assert!(
            after
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, 1, crate::theme::Role::Faint))),
            "stale row never dimmed without an arrival"
        );
    }

    /// The deadline arm fires at most twice per idle row, minute-rounded
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

    /// Recency orders the calm ranks newest-first and nothing else. A
    /// blocked row stays above every idle no matter how fresh the idle is,
    /// working rows keep daemon order under recency change, equal stamps
    /// fall back to daemon order, and unstamped rows sink within rank.
    #[test]
    fn priority_recency_breaks_calm_ties_only() {
        let spaces = vec![space(1, "app")];
        let listed = |agents: &[AgentRow], focused: u32, view, seen: &Seen, unseen: &Unseen| {
            order(&rows12(
                &spaces, agents, 1, focused, 40, 0, view, seen, unseen, NOW, WIDTH, None,
            ))
        };
        let agents = vec![
            agent_at(1, 1, "codex", "idle", 60),
            agent(2, 1, "claude", "blocked"),
            agent_at(3, 1, "gemini", "working", 5000),
            agent_at(4, 1, "opencode", "working", 60),
            agent_at(5, 1, "amp", "idle", 5000),
            agent(6, 1, "droid", "idle"),
        ];
        let none = (Seen::new(), Unseen::new());
        assert_eq!(
            listed(&agents, 9, View::Priority, &none.0, &none.1),
            vec![2, 3, 4, 1, 5, 6],
            "blocked first, working in daemon order, idle newest-first, unstamped last"
        );
        assert_eq!(
            listed(&agents, 9, View::Tree, &none.0, &none.1),
            vec![1, 2, 3, 4, 5, 6],
            "the tree keeps daemon order"
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
        assert_eq!(
            listed(&agents, 9, View::Priority, &seen, &none.1),
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
        // Terminal 2 is new (rank 2) and leads; terminal 1 is focused, so it
        // is not new and stays in its calm rank behind it.
        assert_eq!(
            listed(&agents, 1, View::Priority, &none.0, &unseen),
            vec![2, 1]
        );

        // Equal stamps fall back to daemon order.
        let agents = vec![
            agent_at(2, 1, "codex", "idle", 500),
            agent_at(1, 1, "claude", "idle", 500),
        ];
        assert_eq!(
            listed(&agents, 9, View::Priority, &none.0, &none.1),
            vec![2, 1],
            "equal stamps keep daemon order"
        );
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
        let rows = tree_rows(
            &spaces,
            &[fitting, long, wide],
            9,
            20,
            &Seen::new(),
            &Shut::new(),
        );
        let rows: Vec<String> = (1..=3).map(|t| tree_row(&rows, t).text.clone()).collect();
        for row in &rows {
            assert_eq!(crate::width::width(row), WIDTH, "{row:?}");
            assert!(!row.contains('\u{fffd}'));
        }
        // Fitting session: today's bytes, no ellipsis.
        assert_eq!(rows[0].trim_end(), "   \u{25cf} claude");
        // Cut sessions: the dot and the ellipsis frame the text.
        assert!(rows[1].trim_end().ends_with('…'), "{:?}", rows[1]);
        assert!(
            rows[1].starts_with("   \u{25cf} Implement"),
            "{:?}",
            rows[1]
        );
        assert!(rows[2].trim_end().ends_with('…'), "{:?}", rows[2]);
        // A session that already ends in an ellipsis is cut once, not twice.
        let mut dotty = agent(4, 1, "claude", "working");
        dotty.agent_session = Some("abcdefghijklmnop…".into());
        let rows = tree_rows(&spaces, &[dotty], 9, 20, &Seen::new(), &Shut::new());
        let row = tree_row(&rows, 4);
        assert_eq!(row.text.chars().filter(|c| *c == '…').count(), 1);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    /// The row an agent with this cwd draws under a space of this name.
    fn worktree_row(space_name: &str, cwd: Option<&str>, program: &str) -> Line {
        let spaces = vec![space(1, space_name)];
        let mut row = agent(1, 1, program, "idle");
        row.cwd = cwd.map(str::to_string);
        tree_row(
            &tree_rows(&spaces, &[row], 9, 10, &Seen::new(), &Shut::new()),
            1,
        )
        .clone()
    }

    /// The row shows the worktree basename, quietly, when the space name does not
    /// already say it, and the edge cases keep the bare row.
    #[test]
    fn the_row_shows_the_worktree_basename_when_different() {
        let row = |cwd| {
            worktree_row("app", cwd, "claude")
                .text
                .trim_end()
                .to_string()
        };
        // Different: the session, then the worktree.
        assert_eq!(
            row(Some("/home/u/app-feat-x")),
            "   \u{25cf} claude app-feat-x"
        );
        // Trailing slash is not a name.
        assert_eq!(
            row(Some("/home/u/app-feat-x/")),
            "   \u{25cf} claude app-feat-x"
        );
        // Same name in any case: the bare row.
        assert_eq!(row(Some("/home/u/app")), "   \u{25cf} claude");
        assert_eq!(row(Some("/home/u/app")), "   \u{25cf} claude");
        // Root, empty, blank: no basename, the bare row.
        assert_eq!(row(Some("/")), "   \u{25cf} claude");
        assert_eq!(row(Some("")), "   \u{25cf} claude");
        assert_eq!(row(Some("   ")), "   \u{25cf} claude");
        // No cwd at all: the bare row.
        assert_eq!(row(None), "   \u{25cf} claude");
        // A tilde and dots are names and pass through untouched.
        assert_eq!(row(Some("~")), "   \u{25cf} claude ~");
        assert_eq!(row(Some("/home/u/.")), "   \u{25cf} claude .");
        // The worktree is a quiet run of its own, after the name and a space.
        let line = worktree_row("app", Some("/home/u/app-feat-x"), "claude");
        assert_eq!(line.runs, vec![(12, 10, crate::theme::Role::Faint)]);
        assert_eq!(run_text(&line, 12, 10), "app-feat-x");
        assert!(worktree_row("app", None, "claude").runs.is_empty());
    }

    /// The same worktree name under two spaces stays ambiguous on purpose: each row
    /// sits under its own space's rule, so the rule tells the two apart while the
    /// worktree reads the same.
    #[test]
    fn the_same_basename_under_two_spaces_sits_under_both_rules() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let mut a = agent(1, 1, "claude", "idle");
        a.cwd = Some("/home/u/x".into());
        let mut b = agent(2, 2, "codex", "idle");
        b.cwd = Some("/home/u/x".into());
        let rows = tree_rows(&spaces, &[a, b], 9, 16, &Seen::new(), &Shut::new());
        let lines: Vec<Line> = rows.iter().map(|(l, _)| l.clone()).collect();
        let texts = texts(&lines);
        assert_eq!(texts[1], rule_row(&rows, 1).text.trim_end());
        assert_eq!(texts[2], "   \u{25cf} claude x");
        assert_eq!(texts[3], rule_row(&rows, 2).text.trim_end());
        assert_eq!(texts[4], "   \u{25cf} codex x");
    }

    /// A long worktree shortens tail-first with an ellipsis while the session keeps
    /// every letter it had; a name too long to leave room drops the worktree whole.
    #[test]
    fn a_long_worktree_shortens_and_the_name_never_does() {
        let line = worktree_row(
            "app",
            Some("/home/u/a-very-long-worktree-name-that-keeps-going"),
            "claude",
        );
        assert!(
            line.text.starts_with("   \u{25cf} claude a-very"),
            "{:?}",
            line.text
        );
        assert!(line.text.ends_with('…'), "{:?}", line.text);
        assert_eq!(crate::width::width(&line.text), WIDTH);

        // A tab so long the worktree cannot fit at all: the bare row, the name cut.
        let line = worktree_row("app", Some("/home/u/somewhere"), &"c".repeat(40));
        assert!(!line.text.contains("somewhere"), "{:?}", line.text);
        assert!(line.runs.is_empty(), "{line:?}");
    }

    /// An overlong wide worktree shortens by columns, never splitting a wide
    /// character, and the row stays thirty columns.
    #[test]
    fn a_wide_worktree_shortens_by_columns() {
        let line = worktree_row("app", Some("/home/u/程程程程程程程程程程程程"), "claude");
        // Eighteen columns are left after the name: eight wide letters and the
        // ellipsis, plus the pad column `fit` adds to reach thirty.
        assert_eq!(
            line.text.trim_end(),
            "   \u{25cf} claude 程程程程程程程程…",
            "{:?}",
            line.text
        );
        assert_eq!(crate::width::width(line.text.trim_end()), WIDTH - 1);
        assert_eq!(crate::width::width(&line.text), WIDTH);
        assert_eq!(line.runs, vec![(12, 17, crate::theme::Role::Faint)]);
        assert!(!line.text.contains('\u{fffd}'));
    }

    /// Worktrees, wide names and long names all still measure exactly thirty columns
    /// in both orders, and accents and runs never leave the row.
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
        for view in [View::Tree, View::Priority] {
            let lines = folded(
                &spaces,
                &rows,
                1,
                9,
                30,
                0,
                view,
                &Seen::new(),
                &Unseen::new(),
                NOW,
                WIDTH,
            );
            for line in &lines {
                assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
                assert!(!line.text.contains('\u{fffd}'));
                for (at, len, _) in line.accent.iter().chain(&line.runs) {
                    assert!(at + len <= WIDTH, "a run past the row: {line:?}");
                }
            }
        }
    }

    /// A space name longer than the row cannot break the painter: the rule shortens
    /// the name before it gives up the arrow or the count, the accent stays on the
    /// row, and the row still measures thirty columns.
    #[test]
    fn a_space_name_longer_than_the_row_shortens_on_its_rule() {
        let spaces = vec![space(1, &"s".repeat(40))];
        let agents = vec![agent(1, 1, "claude", "working")];
        let rows = tree_rows(&spaces, &agents, 9, 14, &Seen::new(), &Shut::new());
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{:?}", line.text);
        }
        let rule = rule_row(&rows, 1);
        assert!(rule.text.ends_with(" 1 \u{2500}"), "{:?}", rule.text);
        assert!(rule.text.contains('…'), "{:?}", rule.text);
        let (at, len, _) = rule.accent.expect("the name is accented");
        assert!(at + len <= WIDTH);
    }

    /// Wrapper rows and unknown programs keep the dot alone: the wrapper
    /// reads as what stands in its way, the unknown program under its own
    /// name, neither with an accent on the row.
    #[test]
    fn wrapper_and_unknown_rows_keep_the_dot_alone() {
        let spaces = vec![space(1, "app")];
        let mut wrapper = agent(2, 1, "sh", "working");
        wrapper.program = None;
        wrapper.behind = Some("docker".into());
        let mut strange = agent(3, 1, "sh", "working");
        strange.program = Some("Muse".into());
        let rows = tree_rows(
            &spaces,
            &[wrapper, strange],
            9,
            30,
            &Seen::new(),
            &Shut::new(),
        );
        for t in [2, 3] {
            assert_eq!(tree_row(&rows, t).accent, None, "{t}");
        }
        assert!(
            tree_row(&rows, 2)
                .text
                .starts_with("   \u{25cf} behind docker"),
            "{:?}",
            tree_row(&rows, 2).text
        );
        assert!(
            tree_row(&rows, 3).text.starts_with("   \u{25cf} Muse"),
            "{:?}",
            tree_row(&rows, 3).text
        );
    }

    /// The collapsed strip is byte-for-byte what it was: worktrees
    /// upstream change nothing in four columns.
    #[test]
    fn the_collapsed_strip_ignores_worktrees() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let mut a = agent_at(1, 1, "claude", "idle", 10_000);
        a.cwd = Some("/home/u/app-feat-x".into());
        let mut b = agent(2, 2, "codex", "blocked");
        b.cwd = Some("/home/u/dub".into());
        let before = collapsed5(&spaces, &[a, b], 1, 1, 12);
        let plain_a = agent(1, 1, "sh", "idle");
        let plain_b = agent(2, 2, "sh", "blocked");
        let after = collapsed5(&spaces, &[plain_a, plain_b], 1, 1, 12);
        assert_eq!(before, after);
        for (line, _) in &before {
            assert_eq!(crate::width::width(&line.text), COLLAPSED);
        }
    }

    /// Reordered priority rows keep hit parity: every painted agent row still opens
    /// its own terminal, and the focused row never scrolls out.
    #[test]
    fn hit_parity_holds_under_priority_reorder() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent_at(1, 1, "codex", "idle", 60),
            agent_at(2, 1, "claude", "idle", 5000),
            agent(3, 1, "gemini", "blocked"),
        ];
        for height in [4usize, 6, 10, 14, 30] {
            let rows = rows12(
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
                None,
            );
            // The focused blocked row draws somewhere inside the window.
            assert!(
                rows.iter().any(|(_, hit)| *hit == Some(Hit::Terminal(3))),
                "focused row scrolled out at height {height}"
            );
            for (line, hit) in &rows {
                let Some(Hit::Terminal(t)) = hit else {
                    continue;
                };
                let a = agents.iter().find(|a| a.terminal == *t).expect("an agent");
                assert!(
                    line.text.contains(&session_name(a)),
                    "row for {t} reads {:?} at height {height}",
                    line.text
                );
            }
        }
    }

    /// Every row is exactly the pulled width at every width the column drags to, with
    /// the pin and without it, in both orders: a short row leaves pixels behind and a
    /// long one spills into the terminal on the right.
    #[test]
    fn every_row_is_exactly_the_pulled_width_with_and_without_the_pin() {
        for (spaces, agents) in [sample(), sample_pinned()] {
            let pinned_fleet = agents.iter().any(|a| a.state == "blocked");
            for width in SIDEBAR_MIN..=SIDEBAR_MAX {
                for view in [View::Tree, View::Priority] {
                    let lines = folded(
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
                        width,
                    );
                    assert_eq!(
                        lines[0].text.starts_with(&format!(" {PIN_TITLE}")),
                        pinned_fleet && view == View::Tree,
                        "width {width}: {:?}",
                        lines[0].text
                    );
                    for line in &lines {
                        assert_eq!(
                            crate::width::width(&line.text),
                            width,
                            "width {width}: {line:?}"
                        );
                        assert!(!line.text.contains('\u{fffd}'));
                    }
                }
            }
        }
    }

    /// The same rows paint at any pulled width and measure exactly that. The default
    /// width reads what it always read.
    #[test]
    fn the_same_rows_paint_at_any_pulled_width() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "claude", "working");
        a.cwd = Some("/home/u/app-feat-x".into());
        let mut b = agent(2, 1, "codex", "blocked");
        b.agent_title = Some("Wire retry budget into dispatcher".into());
        let agents = vec![a, b];
        for width in [SIDEBAR_MIN, 22, WIDTH, SIDEBAR_MAX] {
            let lines = folded(
                &spaces,
                &agents,
                1,
                9,
                20,
                0,
                View::Tree,
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
            // The space name survives on its rule at every width.
            assert!(
                lines.iter().any(|l| l.text.contains(" app ")),
                "width {width}: space name lost"
            );
            // The corners follow the pulled width, not the default.
            let none = (Seen::new(), Unseen::new(), Shut::new());
            let c = clicks(&spaces, &agents, View::Tree, width, &none);
            assert_eq!(act(Hit::Title, width, &c, false), Some(Act::NewSpace));
            assert_eq!(act(Hit::Title, width - 2, &c, false), Some(Act::View));
            assert_eq!(act(Hit::Foot, width - 3, &c, false), Some(Act::Menu));
            assert_eq!(act(Hit::Foot, width, &c, false), Some(Act::Collapse));
        }
        // The default width is what `render` paints, row for row.
        let live = folded(
            &spaces,
            &agents,
            1,
            9,
            20,
            0,
            View::Tree,
            &Seen::new(),
            &Unseen::new(),
            0,
            WIDTH,
        );
        assert_eq!(live, render(&spaces, &agents, 1, 9, 20));
    }

    /// Sleeping earns the violet indicator and sinks below idle: settled,
    /// visible, never attention. And it is never news: no bell, no unread,
    /// no "new" tab mark, even with an unseen entry sitting there.
    #[test]
    fn a_sleeping_row_is_violet_calm_bottom_and_never_news() {
        assert_eq!(colour_for("sleeping"), Some(crate::theme::Role::Sleeping));
        let mut slept = agent(7, 1, "claude", "sleeping");
        slept.generation = Some(3);
        let focused = 1;
        let seen = Seen::new();
        let mut unseen = Unseen::new();
        unseen.insert((7, 3));
        assert!(!is_unread(&slept, focused, &seen));
        assert!(!is_new(&slept, focused, &seen, &unseen));
        assert!(!tab_is_new(&slept, focused, &unseen));
        assert!(!needs_person(&slept, focused, &seen, &unseen));
        assert_eq!(
            priority_of(&slept, focused, &seen, &unseen),
            7,
            "calm bottom, below idle"
        );
        let idle = agent(8, 1, "claude", "idle");
        assert_eq!(priority_of(&idle, focused, &seen, &unseen), 6);
        // In the tree it wears the mauve dot, and never earns a pin.
        let rows = tree_rows(&[space(1, "app")], &[slept], 1, 10, &seen, &Shut::new());
        assert_eq!(
            tree_row(&rows, 7).indicator,
            Some((CHILD_DOT_AT, 1, crate::theme::Role::Sleeping))
        );
        assert!(pinned(&rows).is_empty());
    }
}

//! The left column: who needs a person next on top, then every space with its agents under
//! it.
//!
//! One column and not two panels. The two questions a person asks of it are still two
//! questions, and each keeps a place of its own: "who needs me" is the next row at the top,
//! one row that crosses spaces on purpose and never changes height, and "which project am I
//! in" is the tree below it, one rule per space with that space's agents under it. An agent
//! that blocked in a space a person is not looking at is exactly the one they need to be
//! told about, so the next row names it even when its space is folded.
//!
//! An agent's row opens with its state dot, painted in the state's own colour through
//! [`Line::indicator`]; while it works and the column moves, the dot turns a spinner.
//! The caller paints the raised background over the keyboard's rows, so the dot never
//! marks selection. At frame zero, which is what a test, a pipe and `QORO_MOTION=0` get,
//! the rows are the same static text every time.

use qoro_runtime::control::{AgentRow, SleepingTab, WorkspaceRow};

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
    /// row, or the top of a space's line, in the colour of the state it
    /// speaks for. Selection is the raised background the painter adds, never a mark.
    pub indicator: Option<(usize, usize, crate::theme::Role)>,
    /// Any further runs: a space's count and controls, a quiet worktree name, a
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
/// Thirty, dragged anywhere between [`SIDEBAR_MIN`] and half the window. Twenty
/// six is too narrow, and thirty is the right default
/// here: this column carries a space name,
/// a separator, a mark, a name and a state word flush against the right
/// edge, and at twenty two the state had nothing left. Thirty leaves a
/// real project name room to be read rather than recognised from its first
/// eight characters.
///
/// A pulled width reflows the pane beside it, which is the price the person
/// pulling asked to pay: explicit drags reflow, arrivals never do.
pub const WIDTH: usize = 30;

/// The narrowest the open column drags to. Below eighteen the place rows are mostly
/// ellipsis.
pub const SIDEBAR_MIN: usize = 18;

/// The widest the open column drags to: half the window, so long names and places fit
/// when a person wants them to, and the pane always keeps the other half.
pub fn sidebar_max(cols: u16) -> usize {
    (cols as usize / 2).max(SIDEBAR_MIN)
}

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
const SPACES_TITLE: &str = "agents";
/// The columns before what a space holds: one, the space's line, so an agent's mark sits
/// under the space's name and every row of the column starts on the same line.
const BOX_LEAD: usize = 1;

/// The column a space's name starts in: right after the space's own line, and the column
/// every agent's mark starts in, so the name lines up with the marks under it.
pub const NAME_AT: usize = 1;

/// The top of a space's line, drawn before its name: the left braille column from its second
/// dot down, so the line starts halfway down the name's row.
const LINE_TOP: char = '\u{2846}';
/// The line beside every row of an open space: the whole left braille column.
const LINE: char = '\u{2847}';
/// The line's foot, beside the space's last row: the column and one dot out at the bottom.
const LINE_FOOT: char = '\u{28c7}';

/// The line's top, middle and foot as this terminal draws them: a plain bar where braille is
/// not drawn (the kernel console, a dumb terminal), like the marks.
fn line_glyphs() -> [char; 3] {
    if crate::colorlevel::braille() {
        [LINE_TOP, LINE, LINE_FOOT]
    } else {
        ['|'; 3]
    }
}

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

/// An agent's state mark: a grid of three by three dots across two cells, in the state's
/// colour, one picture per state.
///
/// ```text
/// working  breathes: the centre with its arms coming in clockwise and going back out,
///          each breath starting one arm further round
/// wants    the empty ring          failed   an X
/// done     the full square         idle     the centre dot
/// asleep   a line across the middle, an eye shut
/// ```
///
/// The dots that run while an agent works settle into its next state's picture, so the
/// mark says one thing in one language wherever it stands. The grid takes the braille
/// cell's top three rows and three of its four columns: the fourth column stays empty and
/// is the gap before the name that a single-cell dot needed a space for, so a row is as
/// wide as it was. Without braille (the kernel console, a dumb terminal) it is a plain
/// character and a space.
fn cell_mark(role: crate::theme::Role, working: bool, tick: u64) -> String {
    use crate::theme::Role;
    if !crate::colorlevel::braille() {
        let plain = match role {
            Role::Working if working => ['-', '\\', '|', '/'][(tick % 4) as usize],
            Role::Working => '~',
            Role::Blocked => '?',
            Role::Failed => '!',
            Role::Done => '*',
            Role::Sleeping => 'z',
            _ => '.',
        };
        return format!("{plain} ");
    }
    const RING: [(usize, usize); 8] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (2, 1),
        (2, 2),
        (1, 2),
        (0, 2),
        (0, 1),
    ];
    let dots: Vec<(usize, usize)> = match role {
        Role::Working => breath(if working { tick } else { 0 }),
        Role::Blocked => RING.to_vec(),
        Role::Failed => vec![(0, 0), (2, 0), (1, 1), (0, 2), (2, 2)],
        Role::Done => (0..3).flat_map(|y| (0..3).map(move |x| (x, y))).collect(),
        Role::Sleeping => vec![(0, 1), (1, 1), (2, 1)],
        _ => vec![(1, 1)],
    };
    braille_pair(&dots)
}

/// A working agent's breath at animation frame `tick`: the centre, and the plus's arms
/// coming in clockwise from a start arm and going back out the same way.
///
/// Each breath is eight frames (the start arm alone twice, then one arm more at a time to
/// the full plus, held, then back), and the next breath starts one arm further clockwise,
/// so the loop is thirty two frames and never sits at the top. A frame lasts two of the
/// column's ticks, so a breath takes about 1.3 seconds. Frame zero, the still column, is the
/// start arm and the one after it. No frame is another state's picture: the centre is never
/// alone (idle), and left and right are never lit without top or bottom (asleep).
fn breath(tick: u64) -> Vec<(usize, usize)> {
    // The arms clockwise from the top.
    const ARM: [(usize, usize); 4] = [(1, 0), (2, 1), (1, 2), (0, 1)];
    // How many arms are in at each frame of one breath.
    const DEPTH: [usize; 8] = [1, 1, 2, 3, 4, 4, 3, 2];
    if tick == 0 {
        return vec![(1, 1), ARM[0], ARM[1]];
    }
    let frame = (tick / 2) as usize % 32;
    let base = frame / 8;
    let mut dots = vec![(1, 1)];
    dots.extend((0..DEPTH[frame % 8]).map(|i| ARM[(base + i) % 4]));
    dots
}

/// Two braille cells holding dots at `(column, row)` of a four by four grid.
fn braille_pair(dots: &[(usize, usize)]) -> String {
    // Unicode's dot numbering: the left column is bits 0, 1, 2 then 6 going down, the right
    // column 3, 4, 5 then 7.
    const BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
    let mut cells = [0u32; 2];
    for &(x, y) in dots {
        cells[x / 2] |= BIT[y][x % 2];
    }
    cells
        .iter()
        .map(|bits| char::from_u32(0x2800 + bits).unwrap_or(' '))
        .collect()
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
/// makes a space at its `+`, the foot's menu row opens the menu at its word and folds the
/// column anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Space(i64),
    Terminal(u32),
    /// The next row at the top: the agent it names, which a click reaches like its row in the
    /// tree, or nobody while nobody waits. Never raised, so the keyboard's row is only
    /// ever drawn in one place.
    Next(Option<u32>),
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
    /// Put a space to sleep, or wake it: the toggle on its rule, which reads the space
    /// from the row it was on rather than from whatever is on screen.
    Sleep(i64),
    /// The `+` on a space's rule: a new terminal or agent in that space.
    New(i64),
    /// The close on a space's rule. Only ever arms: the row asks, and the answer is a
    /// second click on the same row.
    Close(i64),
    /// `y` on an armed rule: the space, and every tab in it, goes.
    ConfirmClose(i64),
    /// Anything else on an armed rule, or on any other row while one is armed: the
    /// question goes and nothing is sent.
    CancelClose,
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

/// What one local agent costs, sampled by the client: CPU over the last interval as a
/// percent of one core (`None` until there are two samples to compare), and the memory
/// resident now, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub cpu_percent: Option<u32>,
    pub rss: u64,
}

/// Costs by terminal. Empty unless `QORO_USAGE=1`, and never holding another machine's
/// terminals: their process groups mean nothing here.
pub type Usage = std::collections::BTreeMap<u32, Cost>;

/// The narrowest column that shows costs: below it the numbers would take the name's
/// room, and the name is what the row is for.
pub const USAGE_MIN_WIDTH: usize = 26;

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
/// is reading it. Same sage dot as `done`, no new theme, and the next row names it.
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
    /// The space whose rule is asking whether it should close, or `None` while no rule is.
    /// The rule paints the question and answers only the question; every other click falls
    /// through to what the row does ordinarily.
    pub armed: Option<i64>,
    /// What each local agent costs, when the person asked to see it.
    pub usage: Option<&'a Usage>,
}

/// One row of the column, and what a click on it means.
type Row = (Line, Option<Hit>);

/// Whether a row wants a person: blocked, failed, or finished while nobody looked.
///
/// One answer for every place that asks: the queue behind the next row, the colour of a
/// folded space's line, and the status line's triage.
pub fn needs_person(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> bool {
    matches!(agent.state.as_str(), "blocked" | "failed") || is_new(agent, focused, seen, unseen)
}

/// The most urgent row among `rows` that wants a person, the first one on a tie: blocked,
/// then failed, then finished while nobody looked. One answer for a folded space's line
/// and the strip.
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

/// The colour the most urgent of `rows` lends whatever speaks for them (a folded space's
/// arrow, the strip, the status line): its state's own, or done's sage for news. `None`
/// when nobody among them wants a person.
pub fn urgency_ink<'a>(
    rows: impl IntoIterator<Item = &'a AgentRow>,
    focused: u32,
    seen: &Seen,
    unseen: &Unseen,
) -> Option<crate::theme::Role> {
    most_urgent(rows, focused, seen, unseen).map(|a| {
        if is_new(a, focused, seen, unseen) {
            crate::theme::of("done")
        } else {
            crate::theme::of(&a.state)
        }
    })
}

/// [`urgency_ink`] for rows of the column.
fn urgent_ink(c: &Column, rows: &[&AgentRow]) -> Option<crate::theme::Role> {
    urgency_ink(rows.iter().copied(), c.focused_terminal, c.seen, c.unseen)
}

/// The open column, painted into `height` rows: the next row, the title, the tree or the
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
    // The foot, with the pet in it, is taken first, so the list above is laid out in the
    // rows that are really its own and a scroll never keeps the focused row under the pet.
    let pet = pet_shown(c, height, avatar.is_some());
    // A column shorter than the foot keeps the foot's first rows.
    let foot = if pet { PET_FOOT_ROWS } else { FOOT_ROWS }.min(height);
    let room = height - foot;
    let mut out: Vec<Row> = Vec::with_capacity(height);
    if next_rows(room) > 0 {
        out.push(next_row(c));
    }
    if out.len() < room {
        out.push((title_line(c), Some(Hit::Title)));
    }
    let body = room - out.len();
    out.extend(window(content(c), body, c, c.width));
    out.extend(
        foot_rows(c.width, avatar, pet, update_available().as_deref())
            .into_iter()
            .take(foot),
    );
    out
}

/// Whether the pet stands in the foot: it fits the column, and the whole list still fits
/// above it. The pet is decoration and the list is not: when the agents need the rows,
/// the pet gives them up rather than push anyone off the column.
/// The column and the painter ask this one question, so the art never lands on a row the
/// list was given.
pub fn pet_shown(c: &Column, height: usize, present: bool) -> bool {
    // The pet is dots in its own inks: without either, the two-row foot names it instead.
    let drawn = crate::colorlevel::braille()
        && crate::colorlevel::level() != crate::colorlevel::Level::Mono;
    if !drawn || !avatar_fits(height, c.width, present) {
        return false;
    }
    let room = height - PET_FOOT_ROWS;
    next_rows(room) + 1 + content(c).len() <= room
}

/// The open column as lines alone, for a test that reads and never clicks. The paint
/// and the hit tests both go through [`rows_folded`].
#[cfg(test)]
pub(crate) fn render_folded(c: &Column, height: usize) -> Vec<Line> {
    rows_folded(c, height, None)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

/// How long a waiting agent has waited, in its largest unit: `12s`, `4m`, `2h`, `3d`.
fn waited(c: &Column, agent: &AgentRow) -> Option<String> {
    let secs = c.now_unix - agent.last_active_at_unix?;
    (secs >= 0).then(|| match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    })
}

/// Who needs a person, in the order they are best seen to: everyone who wants something,
/// then everyone who failed, then everyone who finished unread, each kind in daemon order.
///
/// The agent the keyboard is on is left out: it is already in front of the person, so a
/// blocked agent being answered is not also "next". And the queue walks on from it: while
/// the keyboard stands on a waiting agent the queue starts at the one after it and wraps,
/// so `ctrl-a tab` pressed again and again reaches every waiting agent instead of trading
/// between two that are still waiting. One answer for the next row, the key and the status
/// line's triage.
pub fn queue<'a>(
    rows: &'a [AgentRow],
    focused: u32,
    seen: &Seen,
    unseen: &Unseen,
) -> Vec<&'a AgentRow> {
    let key = |at: usize, a: &AgentRow| (priority_of(a, focused, seen, unseen), at);
    let mut queue: Vec<(usize, &AgentRow)> = rows
        .iter()
        .enumerate()
        .filter(|(_, a)| a.agent && a.terminal != focused && needs_person(a, focused, seen, unseen))
        .collect();
    // The daemon's order breaks every tie, so nobody trades places.
    queue.sort_by_key(|&(at, a)| key(at, a));
    // Where the keyboard stands in that order, when it stands on a waiting agent. A finished
    // one it just read still counts as the finish it was; anything else starts the walk
    // over from the top, where what wants a person is.
    let here = rows.iter().enumerate().find_map(|(at, a)| {
        let rank = match a.state.as_str() {
            "blocked" => 0,
            "failed" => 1,
            "done" => 2,
            _ => return None,
        };
        (a.agent && a.terminal == focused).then_some((rank, at))
    });
    if let Some(here) = here {
        let from = queue
            .iter()
            .position(|&(at, a)| key(at, a) > here)
            .unwrap_or(0);
        queue.rotate_left(from);
    }
    queue.into_iter().map(|(_, a)| a).collect()
}

/// A waiting agent's mark in the next row and the ink it wears. Only agents in the queue
/// reach here, so anything not blocked or failed finished.
fn wait_mark(agent: &AgentRow) -> (String, crate::theme::Role) {
    use crate::theme::Role;
    let role = match agent.state.as_str() {
        "blocked" => Role::Blocked,
        "failed" => Role::Failed,
        _ => crate::theme::of("done"),
    };
    (cell_mark(role, false, 0), role)
}

/// The marks the next row shows for the agents queued after the next one before it counts
/// the rest.
const QUEUE_MOST: usize = 4;

/// The fewest columns the next agent's name keeps before the next row gives up its queue
/// marks, then its waited time.
const NAME_FLOOR: usize = 6;

/// The next row: one row, always, that says who to see to next.
///
/// ```text
///  ▲ Agent mesajlaşm… 12s ✗✓✓+2
/// ```
///
/// The next agent's mark, its name and how long it has waited, then a mark per agent
/// queued after it in its kind's ink, and a count past [`QUEUE_MOST`]. With nobody
/// waiting it says so. The height never changes, so nothing under it moves when an
/// agent starts or stops waiting, and an agent is never listed twice: the next row points
/// at a row of the tree, it does not copy one.
fn next_row(c: &Column) -> Row {
    use crate::theme::Role;
    let queue = queue(c.agents, c.focused_terminal, c.seen, c.unseen);
    let Some((next, rest)) = queue.split_first() else {
        let line = Line {
            text: fit(" nothing needs you", c.width),
            colour: Some(Role::Faint),
            ..Line::default()
        };
        return (line, Some(Hit::Next(None)));
    };
    let tail = |marks: usize| -> Vec<(String, Role)> {
        let mut out: Vec<(String, Role)> = rest
            .iter()
            .take(marks)
            .map(|a| {
                let (mark, ink) = wait_mark(a);
                (mark, ink)
            })
            .collect();
        if rest.len() > marks {
            out.push((format!("+{}", rest.len() - marks), Role::Faint));
        }
        out
    };
    let time = waited(c, next).unwrap_or_default();
    // The right side gives way before the name: the marks fold into their count, then the
    // time goes. The lead (` ▲ `), the gap before the right side and the trailing space
    // are the fixed columns around them.
    let mut marks = rest.len().min(QUEUE_MOST);
    let mut timed = !time.is_empty();
    let (parts, right) = loop {
        let parts = tail(marks);
        let mut right: usize = parts.iter().map(|(p, _)| crate::width::width(p)).sum();
        if timed {
            right += crate::width::width(&time) + usize::from(!parts.is_empty());
        }
        // The trailing space is the one column past `<`.
        let fits = 3 + NAME_FLOOR + usize::from(right > 0) + right < c.width;
        if fits || (marks == 0 && !timed) {
            break (parts, right);
        }
        if marks > 0 {
            marks = 0;
        } else {
            timed = false;
        }
    };
    let room = c
        .width
        .saturating_sub(3 + usize::from(right > 0) + right + 1);
    let name = shorten_middle(&session_name(next), room);
    let (mark, ink) = wait_mark(next);
    let mut text = format!(" {mark}{name}");
    let gap = c
        .width
        .saturating_sub(crate::width::width(&text) + right + 1);
    text.push_str(&" ".repeat(gap));
    let mut runs = vec![(1, 2, ink)];
    if timed {
        runs.push((
            crate::width::width(&text),
            crate::width::width(&time),
            Role::Faint,
        ));
        text.push_str(&time);
        if !parts.is_empty() {
            text.push(' ');
        }
    }
    for (part, role) in &parts {
        runs.push((crate::width::width(&text), crate::width::width(part), *role));
        text.push_str(part);
    }
    let line = Line {
        text: fit(&text, c.width),
        colour: Some(Role::Focused),
        runs,
        ..Line::default()
    };
    (line, Some(Hit::Next(Some(next.terminal))))
}

/// The rows the next row takes at the top of the column: one when the title and a row of
/// the list still fit under it, none otherwise. In both views, so the title and the order
/// word on it stay on the row a person just clicked when the view flips.
fn next_rows(room: usize) -> usize {
    usize::from(room > 2)
}

/// A line drawn from pieces, each in its own colour, cut to the column.
fn pieces(parts: &[(&str, Option<crate::theme::Role>)], width: usize) -> Line {
    let mut text = String::new();
    let mut runs = Vec::new();
    for (part, role) in parts {
        let at = crate::width::width(&text);
        if let Some(role) = role {
            runs.push((at, crate::width::width(part), *role));
        }
        text.push_str(part);
    }
    Line {
        text: fit(&text, width),
        runs,
        ..Line::default()
    }
}

/// The title: `spaces` on the left, the order word and the `+` on the right.
///
/// A title is chrome, not content, but it starts in the same column as the agents'
/// dots below it rather than two columns left of them at the box edge, so the one
/// word a person reads first does not stick out past everything it names.
fn title_line(c: &Column) -> Line {
    // The whole fleet's cost before the order word, when costs are shown at all.
    let total = c
        .usage
        .filter(|u| !u.is_empty() && c.width >= USAGE_MIN_WIDTH)
        .map(|u| {
            let cpu = u.values().filter_map(|cost| cost.cpu_percent).sum::<u32>();
            let rss = u.values().map(|cost| cost.rss).sum::<u64>();
            format!("{cpu}% {} ", mem_text(rss))
        })
        .unwrap_or_default();
    let mode = format!("{} +", title_mode(c.view));
    // The total only when ` agents` still fits in front of it: the title names the list
    // first, and a number in its place would name nothing.
    let right = if c
        .width
        .saturating_sub(crate::width::width(&total) + crate::width::width(&mode))
        >= SPACES_TITLE.len() + 2
    {
        format!("{total}{mode}")
    } else {
        mode
    };
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
                out.push((flat_line(c, agent), Some(Hit::Terminal(agent.terminal))));
                out.push(second_row(c, agent, DOT_AT + 2, None));
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
                // The agents in the space and nothing else: the column is where agents are
                // watched, and every tab, shell or agent, is on the bar above the terminal.
                let tabs: Vec<&AgentRow> = c
                    .agents
                    .iter()
                    .filter(|a| a.workspace == space.id && a.agent)
                    .collect();
                let open = !c.shut.contains(&space.id);
                out.push((rule(c, space, &tabs, open), Some(Hit::Space(space.id))));
                // The rows are laid out for the space's inside, one column in from each edge,
                // and gathered so the space's line can run down beside them.
                let first = out.len();
                let inside = Column {
                    width: box_inside(c.width),
                    ..*c
                };
                // A folded space still shows the tab the keyboard is on: the fold hides
                // what a person is not looking at, never the thing they are typing into.
                for tab in tabs {
                    if !open && tab.terminal != c.focused_terminal {
                        continue;
                    }
                    let hit = Some(Hit::Terminal(tab.terminal));
                    out.push((boxed(tree_line(&inside, tab), c.width), hit));
                    let (line, hit) = second_row(&inside, tab, 2, place_of(tab, space));
                    out.push((boxed(line, c.width), hit));
                }
                // A sleeping space has no terminals, only the tabs a wake brings back: one
                // faint row each, under the same fold as live tabs.
                for tab in space.sleeping.iter().filter(|t| t.agent.is_some()) {
                    if open || tab.terminal == c.focused_terminal {
                        out.push((
                            boxed(sleeping_line(&inside, tab, space), c.width),
                            Some(Hit::Terminal(tab.terminal)),
                        ));
                    }
                }
                let ink = if space.asleep {
                    crate::theme::Role::Sleeping
                } else {
                    crate::theme::space_of(space.id)
                };
                with_line(&mut out[first..], ink);
                out.push((box_bottom(c.width), None));
            }
            // An agent in a space the client has not heard of yet still draws, under no
            // rule, rather than vanishing until the index catches up.
            for agent in c
                .agents
                .iter()
                .filter(|a| a.agent && !c.spaces.iter().any(|s| s.id == a.workspace))
            {
                out.push((flat_line(c, agent), Some(Hit::Terminal(agent.terminal))));
                out.push(second_row(c, agent, DOT_AT + 2, None));
            }
        }
    }
    out
}

/// A space's own row: the top of its line, the name, what it holds beside the name, and the
/// two controls in fixed columns at the right.
///
/// ```text
/// ⡆qoro (4)                + z x
/// ⡆qoro sleeping               z x
/// ```
///
/// The line starts halfway down the name's row, in the space's colour, and runs down beside
/// the space's rows (see [`with_line`]), so a space reads as one block with no box round
/// it. The name starts in the column the agents' marks start in. There is no fold arrow:
/// `ctrl-a f` folds, and a click on the line's top does too.
///
/// The name and the count beside it share one slot: the count while the space is awake,
/// the word `sleeping` while it is not, and the word gives way before the name does,
/// because a name is identity and a state is not. A sleeping space wears `Role::Sleeping`
/// on its name and its rule, so the state is in the colour as well as the word.
///
/// The two controls are [`Block`], in fixed columns at the right. Folded, the line's top,
/// the count and the controls take the colour of the most urgent agent under them when that
/// agent wants a person, so a fold never hides a blocked agent silently. Open, the line
/// keeps the space's colour and the count stays grey: the agents are right there, wearing
/// their own marks. Only agents colour it: a shell never wants a person.
fn rule(c: &Column, space: &WorkspaceRow, kids: &[&AgentRow], open: bool) -> Line {
    use crate::theme::Role;
    let width = c.width;
    let ink = if space.asleep {
        Role::Sleeping
    } else {
        crate::theme::space_of(space.id)
    };
    let agents: Vec<&AgentRow> = kids.iter().copied().filter(|a| a.agent).collect();
    let hot = if open { None } else { urgent_ink(c, &agents) };
    let mark = hot.unwrap_or(Role::Faint);
    let held = if space.asleep {
        space.sleeping.iter().filter(|t| t.agent.is_some()).count()
    } else {
        kids.len()
    };
    // A sleeping space draws the toggle whenever it has tabs to wake, because nothing of
    // it is running and `z` is then the only way back with a mouse. An awake space draws
    // it when it holds anything at all, which is the daemon's own test: `kids` is the
    // live registry filtered by workspace, so an empty `kids` is a space nothing runs in
    // and `SleepWorkspace` would refuse it.
    let block = if c.armed == Some(space.id) {
        // Armed: the row is a question, so the two controls become the two answers in the
        // same columns. A question is asked in the place it was asked from, and answered
        // by a second click on the same cells.
        armed_block(width)
    } else {
        block(width, space, !kids.is_empty())
    };
    // One slot after the name, and only one: the count while awake, the word while asleep,
    // because they are the same fact and never both true. The slot carries its own leading
    // space, so the name and the slot never touch, and the word goes first of the two: a
    // space that would starve to a single column keeps its name and says its state in
    // colour instead.
    let slot = if c.armed == Some(space.id) {
        // The question, in the slot the count and the word would have used. The name is
        // what gives way to it, because `room` below is computed from the slot: a rule
        // that is asking is the rule a person is looking at.
        QUESTION.to_string()
    } else if space.asleep && room_for(width, SLEEPING) >= MIN_NAME_COLS {
        SLEEPING.to_string()
    } else if held == 0 {
        String::new()
    } else {
        format!(" ({held})")
    };
    let slot_w = crate::width::width(&slot);
    let room = width.saturating_sub(BLOCK_TAIL).saturating_sub(slot_w);
    let name = shorten_middle(&space.name, room);
    let name_w = crate::width::width(&name);
    // ` <arrow> ` then the name, the slot (which carries the space before it when it has
    // one), the fill between them, and the three block columns. The block therefore
    // starts at `width - 4` whatever the name and the slot are, and the last column is
    // left blank, as every row of this column already is.
    let fill_at = NAME_AT + name_w + slot_w;
    let fill = width
        .saturating_sub(1)
        .saturating_sub(fill_at)
        .saturating_sub(BLOCK_W);
    let mut runs = vec![(fill_at, fill, ink)];
    if !slot.is_empty() {
        runs.push((
            NAME_AT + name_w,
            slot_w,
            if space.asleep { Role::Sleeping } else { mark },
        ));
    }
    // The two block cells, coloured by what they say rather than by which field holds
    // them. The sleeping ink means a space is asleep, so it cannot also be the affirmative
    // of a question that destroys one, and the two answers are deliberately not the same
    // weight: `y` is a word, `n` is quieter.
    for cell in [block.new, block.sleep, block.close] {
        let Some((at, w)) = cell else { continue };
        let role = match block.answer {
            // The affirmative is a word and the other is quieter, so a question cannot be
            // answered by a glance at two equal marks.
            Some(yes) if Some((at, w)) == Some(yes) => Role::Text,
            Some(_) => Role::Faint,
            None if Some((at, w)) == block.sleep && space.asleep => Role::Sleeping,
            None => mark,
        };
        runs.push((at, w, role));
    }
    let [top, _, _] = line_glyphs();
    let text = format!("{top}{name}{slot}{}{}", " ".repeat(fill), block.text());
    Line {
        text: fit(&text, width),
        colour: None,
        accent: Some((NAME_AT, name_w, ink)),
        // The line wears the space's colour, or, folded, the colour of the agent under it
        // that most wants a person, so a fold never hides one silently.
        indicator: Some((0, 1, hot.unwrap_or(ink))),
        runs,
    }
}

/// The word a sleeping space wears beside its name, when the column has room for it.
const SLEEPING: &str = " sleeping";

/// What an armed rule says instead of its name's count: the question, and nothing else.
/// The space it is about is on the row, and the notice row carries the count and the
/// reason, so the rule itself only has to ask.
const QUESTION: &str = " close?";

/// What a name must keep to stay a name. A space whose row cannot hold this many columns
/// for the name is a row that has said its name is not the most important thing on it.
const MIN_NAME_COLS: usize = 3;

/// The columns the block takes.
const BLOCK_W: usize = 5;

/// Everything on the row that is neither the name, the slot nor the fill: the fold
/// cells, the block, and the blank last column the block leaves. A name gets what is
/// left of the width once these are paid for.
const BLOCK_TAIL: usize = NAME_AT + BLOCK_W + 1;

/// The columns a name has once the slot beside it is paid for, which is what decides
/// whether the word `sleeping` fits or the count stays.
///
/// The same subtraction `rule` makes for the name's own room, so the two cannot disagree
/// about what fits.
fn room_for(width: usize, slot: &str) -> usize {
    width
        .saturating_sub(BLOCK_TAIL)
        .saturating_sub(crate::width::width(slot))
}

/// The control block at the right of a space rule: the new tab, the sleep toggle and the
/// close, in five columns that never change shape, laid out from the right edge inward.
///
/// `+` at `width - 6`, `z` at `width - 4` and `x` at `width - 2`, whenever drawn. A sleeping
/// space gets no `+`: it has to wake first, and `z` is right beside it. The block being fixed is the
/// whole point: the slot beside the name grows a column at 10 tabs, and a control that
/// moved with it would be a control nobody could learn.
///
/// A space with nothing running in it gets no `z`, because the daemon refuses to sleep an
/// empty space and a control that cannot work is worse than no control. A space that is
/// asleep always gets one, because `z` is then the only way back with a mouse: a sleeping
/// space has no tab rows to press, so there is nothing else to click.
struct Block {
    /// (zero-based column, width) of the new tab control, if it is drawn.
    new: Option<(usize, usize)>,
    /// The same for the sleep toggle.
    sleep: Option<(usize, usize)>,
    /// The same for the close.
    close: Option<(usize, usize)>,
    /// The column the affirmative is in, while the row is armed, and the other cell is
    /// then the negative. Both sit where the controls were.
    answer: Option<(usize, usize)>,
}

impl Block {
    /// The five columns, in the order they are drawn. A control that is not drawn is a
    /// blank, so the block keeps its shape and the other controls keep their columns.
    fn text(&self) -> String {
        // Armed, the two cells are the two answers; otherwise they are the two controls.
        // Either way the block is five columns and no cell ever moves.
        let [left, right] = match self.answer {
            // `answer` is the cell the affirmative is drawn in, and which cell that is
            // depends on where the person pressed: `y` stays under the pointer.
            Some(yes) => [
                if self.sleep == Some(yes) { "y" } else { "n" },
                if self.close == Some(yes) { "y" } else { "n" },
            ],
            None => [
                if self.sleep.is_some() { "z" } else { " " },
                if self.close.is_some() { "x" } else { " " },
            ],
        };
        let new = if self.new.is_some() { "+" } else { " " };
        format!("{new} {left} {right}")
    }
}

/// The two answers in the two columns the controls were, so a person who aimed at `x` finds
/// `y` and `n` under the pointer rather than somewhere else on the row.
fn armed_block(width: usize) -> Block {
    Block {
        new: None,
        sleep: Some((width.saturating_sub(4), 1)),
        close: Some((width.saturating_sub(2), 1)),
        // `y` in the cell `x` was in, because a second click in the cell that asked is
        // what a hand does without looking, and answering that click "no" would be the
        // one thing a question must not do.
        answer: Some((width.saturating_sub(2), 1)),
    }
}

/// The block of a rule that is not asking: `busy` is whether anything runs in the space.
fn block(width: usize, space: &WorkspaceRow, busy: bool) -> Block {
    Block {
        new: (!space.asleep).then_some((width.saturating_sub(6), 1)),
        sleep: (space.asleep || busy).then_some((width.saturating_sub(4), 1)),
        close: space.closable.then_some((width.saturating_sub(2), 1)),
        answer: None,
    }
}

/// A row of a space: `line`, laid out for the space's inside, one column in from each
/// edge, so its mark lines up under the space's name, beside the space's line.
fn boxed(line: Line, width: usize) -> Line {
    let shift = |(at, w, role)| (at + BOX_LEAD, w, role);
    let runs = line.runs.into_iter().map(shift).collect();
    let inside = fit(&line.text, box_inside(width));
    Line {
        text: fit(&format!(" {inside} "), width),
        colour: line.colour,
        accent: line.accent.map(shift),
        indicator: line.indicator.map(shift),
        runs,
    }
}

/// Draw a space's line down the first column of its rows: the whole column beside each one,
/// its foot beside the last. A folded space has no rows and so no line under its top.
fn with_line(rows: &mut [Row], ink: crate::theme::Role) {
    let last = rows.len().saturating_sub(1);
    let [_, middle, foot] = line_glyphs();
    for (i, (line, _)) in rows.iter_mut().enumerate() {
        let glyph = if i == last { foot } else { middle };
        let rest: String = line.text.chars().skip(1).collect();
        line.text = format!("{glyph}{rest}");
        line.runs.push((0, 1, ink));
    }
}

/// The columns inside a space: the column less one on each side.
fn box_inside(width: usize) -> usize {
    width.saturating_sub(2 * BOX_LEAD)
}

/// The end of a space: an empty row before the next one.
fn box_bottom(width: usize) -> Line {
    pieces(&[("", None)], width)
}

/// The worktree a session runs in, in the common case, without asking git on every frame:
/// the directory's own basename, when it is not the space's name.
fn place_of(agent: &AgentRow, space: &WorkspaceRow) -> Option<String> {
    cwd_basename(agent.cwd.as_deref()).filter(|base| !base.eq_ignore_ascii_case(&space.name))
}

/// An agent's second row: which tab it is, and what it is doing now or, while it rests,
/// where it runs.
///
/// Every agent has one, so a row never grows or shrinks under the eye as an agent starts
/// and stops, and the tab a person named (or the program, when nobody did) stays on
/// screen for a calm agent too. While the daemon reports an activity (working, starting,
/// waiting, failed) it follows the tab in a person's words ([`humanize`]): quietly while
/// it works, what it wants in the blocked colour, and the failure in the failed colour.
/// Otherwise the directory it runs in follows, when that says something the space name
/// does not. The row reaches the same terminal as the name above it.
fn second_row(c: &Column, agent: &AgentRow, at: usize, place: Option<String>) -> Row {
    use crate::theme::Role;
    let lead = format!("{}\u{2514} ", " ".repeat(at));
    let lead_w = crate::width::width(&lead);
    let room = c.width.saturating_sub(lead_w);
    let doing = said(agent);
    let tab = label(agent);
    // The tab gives way to what the agent is doing, never the other way round.
    let tail_w = |tail: &str| crate::width::width(tail) + 3;
    let tab_room = match (&doing, &place) {
        (Some((what, _)), _) => room.saturating_sub(tail_w(what)).max(room.min(8)),
        (None, Some(place)) => room.saturating_sub(tail_w(place)).max(room.min(12)),
        (None, None) => room,
    };
    let tab = shorten_middle(&tab, tab_room);
    let mut text = format!("{lead}{tab}");
    let mut runs = vec![(at, 1, Role::Rule)];
    let rest = room.saturating_sub(crate::width::width(&tab) + 3);
    let after = match (doing, place) {
        (Some((what, ink)), _) => Some((what, ink, true)),
        (None, Some(place)) => Some((place, Role::Faint, false)),
        (None, None) => None,
    };
    if let Some((what, ink, doing)) = after.filter(|_| rest >= 2) {
        let what = shorten_middle(&what, rest);
        let at_what = crate::width::width(&text) + 3;
        runs.push((at_what - 2, 1, Role::Rule));
        if doing && agent.state == "working" && c.tick > 0 {
            // While it works and the column moves, a band of brighter greys crosses what
            // it is doing, left to right: the row says "alive" without a word for it.
            // Only what it is doing: a worktree name is a place, and a place never moves.
            runs.extend(sweep(&what, at_what, c.tick));
        } else if ink != Role::Faint {
            runs.push((at_what, crate::width::width(&what), ink));
        }
        text.push_str(" \u{b7} ");
        text.push_str(&what);
    }
    (
        Line {
            text: fit(&text, c.width),
            colour: Some(Role::Faint),
            runs,
            ..Line::default()
        },
        Some(Hit::Terminal(agent.terminal)),
    )
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

/// A shell command without the set-up in front of it: `cd /x/y && cargo test` and
/// `S=/tmp/a; tmux send-keys` say what they run after the `cd` and the variable, and
/// that is the part a person would name.
///
/// ponytail: a variable whose quoted value holds a space splits at the space; the row
/// then opens on the rest of the value, which is no worse than the raw command.
fn bare_command(mut command: &str) -> &str {
    loop {
        command = command
            .trim_start()
            .trim_start_matches([';', '&'])
            .trim_start();
        let word = command.split_whitespace().next().unwrap_or("");
        let assigns = word.split_once('=').is_some_and(|(name, _)| {
            name.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
                && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        });
        let skip = if assigns {
            Some(word.len())
        } else if word == "cd" {
            command.find("&&").or_else(|| command.find(';'))
        } else {
            None
        };
        match skip {
            Some(at) if at < command.len() => command = &command[at..],
            _ => return command,
        }
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
            let command = bare_command(command).trim();
            if command.is_empty() {
                "running".to_string()
            } else {
                command.to_string()
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
        "task" | "agent" => with("subagent", arg.to_string()),
        "webfetch" | "websearch" | "web_fetch" | "google_web_search" | "fetch" => "web".to_string(),
        "todowrite" | "todoread" | "update_plan" | "todo" => "planning".to_string(),
        _ => raw.to_string(),
    }
}

/// The braille wheel a working agent's dot turns into while the column moves.
const SPIN: [char; 10] = [
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];

/// Frames one sweep takes to cross a row: twenty five at the run loop's eighty
/// milliseconds is two seconds, the pace Codex's own sweep keeps.
const SWEEP_TICKS: u64 = 25;

/// Whether anything the column draws is moving: a working agent whose row is on it.
///
/// A working agent folded away under its space's rule draws no row, so it moves nothing
/// and arms no timer; the keyboard's own agent always draws. The run loop asks this
/// before it arms the next frame.
/// ponytail: an agent scrolled out of the window still counts, a scroll-aware check
/// would need the window's arithmetic twice.
pub fn moves(c: &Column) -> bool {
    c.agents.iter().any(|a| {
        a.agent
            && a.state == "working"
            && (c.view == View::Priority
                || !c.shut.contains(&a.workspace)
                || a.terminal == c.focused_terminal)
    })
}

/// The mark an agent's row opens with: its state dot, or, while it works and the
/// column moves, the spinner's frame in the working colour.
///
/// Frame zero is the still column: hit testing, `QORO_MOTION=0` and a column with
/// nothing working all paint at zero, where every mark is its dot.
fn mark_of(c: &Column, agent: &AgentRow, role: crate::theme::Role) -> String {
    cell_mark(role, agent.state == "working", c.tick)
}

/// The folded strip's one-cell mark: the spinner while it works and the column moves, the
/// state dot otherwise. The strip is three columns wide, too narrow for the two-cell grid.
fn strip_mark(c: &Column, agent: &AgentRow, role: crate::theme::Role) -> char {
    if c.tick > 0 && agent.state == "working" {
        SPIN[(c.tick % SPIN.len() as u64) as usize]
    } else {
        dot_glyph(role)
    }
}

/// The sweep over `text`, which starts at column `from`, at animation frame `tick`: a
/// raised-cosine band of brighter greys, a little wider than a tenth of the text and
/// never under three columns each side, crossing left to right once every
/// [`SWEEP_TICKS`]. Cells outside the band keep the row's own colour.
fn sweep(text: &str, from: usize, tick: u64) -> Vec<(usize, usize, crate::theme::Role)> {
    let mut cells = Vec::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let wide = crate::width::columns(ch);
        cells.push((col, wide));
        col += wide;
    }
    let span = col as f64;
    let half = (span * 0.1).max(3.0);
    let pos = (tick % SWEEP_TICKS) as f64 / SWEEP_TICKS as f64 * (span + 2.0 * half) - half;
    cells
        .into_iter()
        .filter(|(_, wide)| *wide > 0)
        .filter_map(|(start, wide)| {
            let distance = (start as f64 - pos).abs() / half;
            if distance >= 1.0 {
                return None;
            }
            let lift = 0.5 * (1.0 + (std::f64::consts::PI * distance).cos());
            // Nine steps, not eleven: the ramp's top two are the focused row's own white,
            // and a sweep on the keyboard's row must not read as a second focus.
            let level = (lift * 9.0).round() as u8;
            (level > 0).then_some((from + start, wide, crate::theme::Role::Shimmer(level)))
        })
        .collect()
}

/// A cost as the row says it: `4% 212M`, or the memory alone before there are two samples.
fn cost_text(cost: Cost) -> String {
    match cost.cpu_percent {
        Some(cpu) => format!("{cpu}% {}", mem_text(cost.rss)),
        None => mem_text(cost.rss),
    }
}

/// Resident bytes as a person reads them: whole megabytes under a gigabyte, then tenths
/// of a gigabyte.
fn mem_text(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    let mib = bytes / MIB;
    if mib < 1024 {
        format!("{mib}M")
    } else {
        format!("{:.1}G", bytes as f64 / (1u64 << 30) as f64)
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

/// An agent inside its space's box, `c` being the box's inside: the dot, the session,
/// and what it costs on the right when costs are shown.
fn tree_line(c: &Column, agent: &AgentRow) -> Line {
    let (role, stale) = dot_of(c, agent);
    let lead = 2;
    // What it costs, flush right, when costs are shown and the column around the box is
    // wide enough.
    let cost = c
        .usage
        .filter(|_| c.width + 2 * BOX_LEAD >= USAGE_MIN_WIDTH)
        .and_then(|u| u.get(&agent.terminal))
        .map(|cost| cost_text(*cost));
    let tail = cost
        .as_ref()
        .map_or(0, |text| crate::width::width(text) + 2);
    let room = c.width.saturating_sub(lead + tail);
    let name = shorten_middle(&session_name(agent), room);
    let mut text = format!("{}{name}", mark_of(c, agent, role));
    let mut runs = Vec::new();
    if let Some(cost) = cost {
        let at = c.width - crate::width::width(&cost) - 1;
        text = format!("{}{cost} ", fit_to(&text, at));
        runs.push((at, crate::width::width(&cost), crate::theme::Role::Faint));
    }
    Line {
        text: fit(&text, c.width),
        colour: stale.then_some(crate::theme::Role::Faint),
        accent: None,
        indicator: Some((0, 2, role)),
        runs,
    }
}

/// A tab of a sleeping space: faint like a shell's row, with the sleeping mark and no
/// other dot, since nothing runs in it. What a person called it, else the agent that
/// comes back in it, else a shell (a sleep refuses any other program), and where.
fn sleeping_line(c: &Column, tab: &SleepingTab, space: &WorkspaceRow) -> Line {
    let name = tab.label().to_string();
    let text = match cwd_basename(Some(&tab.cwd)).filter(|b| !b.eq_ignore_ascii_case(&space.name)) {
        Some(place) => format!("{name} \u{b7} {place}"),
        None => name,
    };
    let lead = cell_mark(crate::theme::Role::Sleeping, false, 0);
    let room = c.width.saturating_sub(crate::width::width(&lead));
    Line {
        text: fit(&format!("{lead}{}", shorten_middle(&text, room)), c.width),
        colour: Some(crate::theme::Role::Faint),
        indicator: Some((0, 2, crate::theme::Role::Sleeping)),
        ..Line::default()
    }
}

/// An agent in a flat list, the priority view or a space the client has not heard of:
/// the dot, the session, and the space it lives in on the right, in the space's own colour.
fn flat_line(c: &Column, agent: &AgentRow) -> Line {
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
    let name = shorten_middle(&session_name(agent), room);
    let head = format!("{}{}{name}", " ".repeat(DOT_AT), mark_of(c, agent, role));
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
        indicator: Some((DOT_AT, 2, role)),
        runs: Vec::new(),
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
        target.map_or(0, |last| {
            let first = (last + 1).saturating_sub(body).min(len - body);
            // Never open on a second row whose name is scrolled off, unless it is the
            // target itself: a lone `└ claude` reads as broken, a blank foot does not.
            let orphan = first > 0
                && first < last
                && matches!(content[first].1, Some(Hit::Terminal(_)))
                && content[first].1 == content[first - 1].1;
            first + usize::from(orphan)
        })
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
        Hit::Terminal(terminal) | Hit::Next(Some(terminal)) => Some(Act::Terminal(terminal)),
        Hit::Next(None) => None,
        Hit::Space(id) => {
            // The folded strip is four columns and carries no controls, so a click there
            // cannot reach one however the columns would fall.
            let space = (!collapsed)
                .then(|| c.spaces.iter().find(|s| s.id == id))
                .flatten();
            if let Some(space) = space {
                let kids: Vec<&AgentRow> = c.agents.iter().filter(|a| a.workspace == id).collect();
                let drawn = if c.armed == Some(id) {
                    armed_block(c.width)
                } else {
                    block(c.width, space, !kids.is_empty())
                };
                let at = col.saturating_sub(1);
                if let Some(yes) = drawn.answer {
                    // An armed rule answers `y` and `n`, and nothing else: a row that is
                    // mid question does not fold, does not navigate and does not sleep,
                    // because a person aiming at one of those would be answering a
                    // question they did not read.
                    return Some(if at == yes.0 {
                        Act::ConfirmClose(id)
                    } else {
                        Act::CancelClose
                    });
                }
                // Not armed: the controls do what they say.
                if drawn.new.is_some_and(|(at, _)| col.saturating_sub(1) == at) {
                    return Some(Act::New(id));
                }
                if drawn
                    .sleep
                    .is_some_and(|(at, _)| col.saturating_sub(1) == at)
                {
                    return Some(Act::Sleep(id));
                }
                if drawn
                    .close
                    .is_some_and(|(at, _)| col.saturating_sub(1) == at)
                {
                    return Some(Act::Close(id));
                }
            }
            // A rule mid question folds nothing and moves nowhere: a person aiming at the
            // arrow is aiming at a question, and the only answers are the two cells.
            if c.armed == Some(id) {
                return Some(Act::CancelClose);
            }
            // The arrow folds; a rule with nothing under it has nothing to fold, so
            // every cell of it moves to the space.
            let folds = !collapsed
                && c.view == View::Tree
                && col <= NAME_AT
                && (c.agents.iter().any(|a| a.agent && a.workspace == id)
                    || c.spaces
                        .iter()
                        .any(|s| s.id == id && !s.sleeping.is_empty()));
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
                text: strip_agent(strip_mark(c, agent, role)),
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
    out.push(collapse_row());
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
    // Nothing to shorten into. The ellipsis is a column of its own and would be one too
    // many, which shifts every control after the name a column right of where `act` looks
    // for it, so a rule with no room for its name draws no name at all.
    if room == 0 {
        return String::new();
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
fn strip_agent(mark: char) -> String {
    let text = format!(" {mark}\u{b7}");
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

/// The strip's one row: the mark that unfolds the column again.
fn collapse_row() -> Row {
    (
        Line {
            text: format!("{}\u{bb} ", " ".repeat(COLLAPSED.saturating_sub(2))),
            colour: Some(crate::theme::Role::Faint),
            ..Line::default()
        },
        Some(Hit::Foot),
    )
}

/// What [`FILES`] holds: the picked pet, and a newer version when one was seen.
type Files = (Option<qoro_runtime::avatar::Avatar>, Option<String>);

/// The pet and the update notice as last read off disk.
///
/// Held, not read per paint: two file reads and a JSON parse on every frame was
/// the render thread's cost while an agent streamed. Both files are written by
/// processes outside the interface (`qoro avatar pick`, `qoro update --check`),
/// so the run loop re-reads them on the acknowledgement tick it already has,
/// and a new pet or notice shows up to one tick late.
// ponytail: process-wide, the interface has one render loop; a State field would
// thread two more arguments through rows_folded and every hit test.
static FILES: std::sync::RwLock<Files> = std::sync::RwLock::new((None, None));

/// Re-read the pet and the update notice. True when either changed, so the
/// caller repaints. Never under test, so the thirty-odd paints the suite asserts
/// never depend on the developer's own home directory.
pub(crate) fn refresh_files() -> bool {
    if cfg!(test) {
        return false;
    }
    hold(&FILES, (qoro_runtime::avatar::load(), notice_from_disk()))
}

/// Keep `now` in `held`; true when it differs from what was there.
fn hold(held: &std::sync::RwLock<Files>, now: Files) -> bool {
    let mut held = held.write().unwrap_or_else(|e| e.into_inner());
    if *held == now {
        return false;
    }
    *held = now;
    true
}

/// The picked avatar, when one is picked, as [`refresh_files`] last read it.
pub(crate) fn current_avatar() -> Option<qoro_runtime::avatar::Avatar> {
    FILES.read().unwrap_or_else(|e| e.into_inner()).0.clone()
}

/// Whether the pet itself fits in the foot: open column, wide enough for the logo and the
/// widest pet beside it with a margin each side, tall enough that the foot still leaves six
/// rows of list.
pub(crate) fn avatar_fits(height: usize, width: usize, present: bool) -> bool {
    present && width >= LOGO_COLS + PET_COLS + 3 && height >= PET_FOOT_ROWS + 6
}

/// The rows the foot of the open column takes: the logo, then the version under it.
pub(crate) const FOOT_ROWS: usize = 2;
/// The pet in the foot, in dots: at most eight cells, and three rows for its ten pixels.
pub(crate) const PET_ROWS: usize = 3;
pub(crate) const PET_COLS: usize = 8;
/// The foot with the pet in it: logo, version and the pet's name beside the pet's three rows,
/// then the menu on a row of its own.
pub(crate) const PET_FOOT_ROWS: usize = PET_ROWS + 1;
/// The logo's cells, two a letter.
const LOGO_COLS: usize = 8;

/// The pet in dots: its sixteen by ten pixels drawn in braille, two pixels across and four
/// down a cell, three rows cut to the cells any row lights, pre-escaped for the overlay to
/// stamp at the right of the foot, with its width in cells. A cell holds one colour, so it
/// wears the ink most of its lit pixels have; the eyes are left unlit, so the face reads as
/// holes in the dots. Empty without colour, because the name in the foot says it then.
pub(crate) fn avatar_overlay(
    avatar: &qoro_runtime::avatar::Avatar,
    colour: bool,
) -> (usize, Vec<String>) {
    if !colour || !crate::colorlevel::braille() {
        return (0, Vec::new());
    }
    use qoro_runtime::avatar::{ART_COLS, ART_ROWS, eye_rects, pixel};
    const BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
    let (_, eye) = eye_rects(&avatar.eyes);
    let (w, h) = (ART_COLS as i32, (ART_ROWS * 2) as i32);
    let cells: Vec<Vec<(Option<u8>, u32)>> = (0..PET_ROWS as i32)
        .map(|cy| {
            (0..PET_COLS as i32)
                .map(|cx| {
                    let mut bits = 0u32;
                    let mut inks: Vec<u8> = Vec::new();
                    for dy in 0..4 {
                        for dx in 0..2 {
                            let (x, y) = (cx * 2 + dx, cy * 4 + dy);
                            if x >= w || y >= h {
                                continue;
                            }
                            if let Some(ink) = pixel(avatar, x, y).filter(|ink| *ink != eye) {
                                bits |= BIT[dy as usize][dx as usize];
                                inks.push(ink);
                            }
                        }
                    }
                    let ink = inks
                        .iter()
                        .max_by_key(|a| inks.iter().filter(|b| b == a).count())
                        .copied();
                    (ink, bits)
                })
                .collect()
        })
        .collect();
    let lit = |x: usize| cells.iter().any(|row| row[x].1 != 0);
    let Some(lo) = (0..PET_COLS).find(|x| lit(*x)) else {
        return (0, Vec::new());
    };
    let hi = (0..PET_COLS).rfind(|x| lit(*x)).unwrap_or(lo);
    let rows = cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            let mut cur: Option<u8> = None;
            for (ink, bits) in &row[lo..=hi] {
                if ink.is_some() && *ink != cur {
                    line.push_str(&qoro_runtime::avatar::fg(crate::theme::pet(
                        ink.unwrap_or(0),
                    )));
                    cur = *ink;
                }
                line.push(char::from_u32(0x2800 + bits).unwrap_or(' '));
            }
            line.push_str("\x1b[0m");
            line
        })
        .collect();
    (hi - lo + 1, rows)
}

/// Screen rows the overlay stamps, top first, for a column of `height` rows
/// starting at `top`. Pure so the paint and the tests agree on arithmetic.
pub(crate) fn avatar_overlay_rows(top: usize, height: usize) -> Vec<usize> {
    (0..PET_ROWS)
        .map(|i| top + height - PET_FOOT_ROWS + i)
        .collect()
}

/// The word of `docs/brand/` in braille, two cells a letter (three dots and a gap), the
/// letters on the lower three dot rows of the row so the q's tail can start the next row.
const LOGO: &str = "\u{28d6}\u{2846}\u{28d6}\u{2846}\u{2856}\u{2800}\u{28d6}\u{2846}";
/// The q's tail: the top dot of the cell under the q's right column. A terminal cell holds
/// one colour, so the tail has a cell of its own and is the only dot in ember.
const LOGO_TAIL: char = '\u{2801}';

/// The product's name, as it signs itself.
const BRAND: &str = "qoro";
/// The dot beside the name where braille is not drawn.
const BRAND_MARK: char = '\u{25cf}';

/// The foot of the open column. With the pet in it, four rows: the logo, the version and
/// the pet's name down the left, the pet at the right beside them (stamped by the overlay),
/// then the menu on a row of its own. Without it, two: the logo with the menu at the right,
/// then the version with the pet's name.
///
/// ```text
/// ⣖⡆⣖⡆⡖⠀⣖⡆        ⢠⣤⣤⡄        ⣖⡆⣖⡆⡖⠀⣖⡆      menu «
///  ⠁ 0.5.0         ⢸⣛⣟⡃         ⠁ 0.5.0  Mohawk Pink
/// Mohawk Pink     ⠈⠋⠙⠁
///             menu «
/// ```
///
/// The logo is the word of `docs/brand/` in braille ([`LOGO`]), the letters in the text
/// colour; the q's tail ([`LOGO_TAIL`]) opens the row under it, the one dot in ember, in a
/// cell of its own because a terminal gives a cell one colour. Where braille is not drawn it
/// is `● qoro` in ember. The menu's
/// row answers a click the way the foot always has (the menu at its word, the fold
/// elsewhere); the other rows are text and answer nothing.
fn foot_rows(
    width: usize,
    avatar: Option<&qoro_runtime::avatar::Avatar>,
    pet: bool,
    update: Option<&str>,
) -> Vec<Row> {
    use crate::theme::Role;
    let tail = format!("{MENU_LABEL} \u{ab} ");
    let braille = crate::colorlevel::braille();
    let logo = if braille {
        LOGO.to_string()
    } else {
        format!("{BRAND_MARK} {BRAND}")
    };
    let logo_w = crate::width::width(&logo);
    let runs = vec![(1, logo_w, if braille { Role::Text } else { Role::Brand })];
    // The row under the logo opens with the q's tail, the one ember dot, alone in its cell
    // under the q's right side; the version follows it.
    let (lead, lead_runs) = if braille {
        (format!("  {LOGO_TAIL} "), vec![(2, 1, Role::Brand)])
    } else {
        (" ".to_string(), Vec::new())
    };
    // Beside the pet the words stop short of it: its widest, a space before it and a margin.
    let room = if pet {
        width.saturating_sub(PET_COLS + 2)
    } else {
        width
    };
    let w = crate::width::width;
    // A version cut short reads as another version, and a name cut short as a smudge: each
    // shows whole or not at all.
    let version = match update {
        Some(new) if w(&lead) + w(VERSION) + 1 + w(new) <= room => format!("{VERSION}>{new}"),
        _ => VERSION.to_string(),
    };
    let name = avatar.map(|found| found.name()).unwrap_or_default();
    let faint = |text: &str| Line {
        text: fit(text, width),
        colour: Some(Role::Faint),
        ..Line::default()
    };
    // The menu closes the row it is on: `menu « ` flush right.
    let menu = |lead: &str| {
        let gap = width.saturating_sub(crate::width::width(lead) + crate::width::width(&tail));
        fit(&format!("{lead}{}{tail}", " ".repeat(gap)), width)
    };
    if pet {
        let logo_row = Line {
            text: fit(&format!(" {logo}"), width),
            colour: Some(Role::Faint),
            runs,
            ..Line::default()
        };
        let version_row = Line {
            runs: lead_runs,
            ..faint(&format!("{lead}{version}"))
        };
        return vec![
            (logo_row, None),
            (version_row, None),
            (faint(&whole(format!(" {name}"), room)), None),
            (faint(&menu("")), Some(Hit::Foot)),
        ];
    }
    let top = Line {
        text: menu(&format!(" {logo}")),
        colour: Some(Role::Faint),
        runs,
        ..Line::default()
    };
    let under = whole(format!("{lead}{version}  {name}"), room);
    let under = if name.is_empty() || under.is_empty() {
        format!("{lead}{version}")
    } else {
        under
    };
    let under = Line {
        runs: lead_runs,
        ..faint(&under)
    };
    vec![(top, Some(Hit::Foot)), (under, None)]
}

/// `text` when it fits in `cols`, else nothing.
fn whole(text: String, cols: usize) -> String {
    if crate::width::width(&text) <= cols {
        text
    } else {
        String::new()
    }
}

/// Which build this is, read from the crate rather than written down twice.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A newer version `qoro update --check` saw, when it saw one, as [`refresh_files`]
/// last read it.
fn update_available() -> Option<String> {
    FILES.read().unwrap_or_else(|e| e.into_inner()).1.clone()
}

/// The notice file, read now. A missing file, garbage in it, or nothing newer is no
/// notice, and the row is byte for byte what it always was.
fn notice_from_disk() -> Option<String> {
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

    /// A wide column for the width sweeps: what half of a 120 column window drags to.
    const SIDEBAR_MAX: usize = 60;

    /// The state a terminal has when nothing has been able to say what it is doing.
    const UNKNOWN: &str = "unknown";

    /// A fixed paint-time clock: tests pin behavior at exact ages, never time.
    const NOW: i64 = 1_750_000_000;

    /// The pet and the notice are held between re-reads, and only a change asks the
    /// loop for a repaint: an idle acknowledgement tick that finds the same files
    /// paints nothing.
    #[test]
    fn the_held_files_ask_for_a_repaint_only_when_they_change() {
        let held = std::sync::RwLock::new((None, None));
        assert!(!hold(&held, (None, None)), "nothing changed");
        assert!(
            hold(&held, (None, Some("9.9.9".to_string()))),
            "a new notice"
        );
        assert!(
            !hold(&held, (None, Some("9.9.9".to_string()))),
            "the same notice"
        );
        assert_eq!(held.read().unwrap().1.as_deref(), Some("9.9.9"));
        assert!(hold(&held, (None, None)), "the notice went away");
    }

    /// Under test nothing is read off disk: every paint the suite asserts is the same
    /// on any machine, whatever pet or update notice its home directory holds.
    #[test]
    fn a_test_paint_never_reads_the_pet_or_the_notice() {
        assert!(!refresh_files());
        assert_eq!(current_avatar(), None);
        assert_eq!(update_available(), None);
    }

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
        column_armed(
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
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn column_armed<'a>(
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
        armed: Option<i64>,
    ) -> Column<'a> {
        Column {
            armed,
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
            usage: None,
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
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            news: false,
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
            pgid: None,
            discards: None,
        }
    }

    fn shell(terminal: u32, workspace: i64) -> AgentRow {
        AgentRow {
            news: false,
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
            pgid: None,
            discards: None,
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

    /// [`sample`] with its codex blocked, so the next row names it.
    fn sample_waiting() -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
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

    /// A row inside a box with its sides blanked, so what it holds reads at the columns
    /// it is drawn at: ` │ ● claude    │` is `   ● claude`. Any other row as it is.
    fn unbox(text: &str) -> String {
        let text = text.trim_end();
        match text
            .strip_prefix(" \u{2502} ")
            .and_then(|t| t.strip_suffix('\u{2502}'))
        {
            Some(inside) => format!("   {}", inside.trim_end()),
            None => text.to_string(),
        }
    }

    /// [`texts`], every boxed row read through [`unbox`].
    fn unboxed(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| unbox(&l.text)).collect()
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

    /// The terminals the list under the title draws, in order, each once. The next row above
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

    /// The agent the next row names, when there is a next row and somebody waits.
    fn next_of(rows: &[Row]) -> Option<u32> {
        rows.iter()
            .take_while(|(_, h)| *h != Some(Hit::Title))
            .find_map(|(_, h)| match h {
                Some(Hit::Next(t)) => *t,
                _ => None,
            })
    }

    /// The row under a terminal's name in the tree: which tab it is, and what it does or
    /// where. The last row that reaches it, since the next row is always above the tree.
    fn second_of(rows: &[Row], terminal: u32) -> &Line {
        rows.iter()
            .rev()
            .find(|(_, h)| *h == Some(Hit::Terminal(terminal)))
            .map(|(l, _)| l)
            .unwrap_or_else(|| panic!("terminal {terminal} is not drawn"))
    }

    /// The name row a terminal draws under its space rule: the one just above its second row.
    fn tree_row(rows: &[Row], terminal: u32) -> &Line {
        let last = rows
            .iter()
            .rposition(|(_, h)| *h == Some(Hit::Terminal(terminal)))
            .unwrap_or_else(|| panic!("terminal {terminal} is not drawn"));
        &rows[last - 1].0
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
        for (spaces, agents) in [sample(), sample_waiting()] {
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
            // Under the next row, in both views.
            let title = &lines[1];
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
        let texts = unboxed(&render(&spaces, &agents, 2, 3, 14));
        // No selection marks anywhere: no hollow ring on every row and no
        // second dot on the selected one. The keyboard's row is the raised
        // background the painter adds. The state mark opens every agent's
        // row, so its braille cells are expected there.
        for bad in ['\u{25cb}', '\u{25cc}', '\u{25a1}'] {
            assert!(
                !texts.iter().any(|t| t.contains(bad)),
                "a selection mark leaked: {texts:?}"
            );
        }
        // The focused space and agent still draw their names, the agent under its
        // space's rule with the same mark every other idle agent wears.
        assert!(texts.iter().any(|t| t.contains("personal")), "{texts:?}");
        assert!(
            texts.iter().any(|t| t == "\u{2847}\u{2810}\u{2800}codex"),
            "{texts:?}"
        );
        // And hits still know which rows have the keyboard.
        let hits: Vec<Option<Hit>> = (0..14)
            .map(|row| hit(&spaces, &agents, 2, 3, 14, row))
            .collect();
        assert!(hits.contains(&Some(Hit::Space(2))));
        assert!(hits.contains(&Some(Hit::Terminal(3))));
    }

    /// Two rows per agent, right under its space's rule: the rule already says where, so
    /// the name row says only who, and its second row says which tab. The space's shell
    /// tab follows as one row of its own, then the next rule.
    #[test]
    fn an_agent_is_two_rows_under_its_space_rule() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        let texts = unboxed(&lines);
        let head = texts
            .iter()
            .position(|t| t == "\u{2847}\u{2818}\u{2802}claude")
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert!(texts[head - 1].starts_with("\u{2846}app"), "{texts:?}");
        assert_eq!(texts[head + 1], "\u{28c7}  \u{2514} claude", "{texts:?}");
        // The tree lists agents only, so the space's plain shell tab draws no row: the
        // bottom of a space's box, an empty row before the next space starts, follows
        // straight after the agent's own two rows.
        assert!(texts[head + 2].is_empty(), "{texts:?}");
        assert!(texts[head + 3].starts_with("\u{2846}personal"), "{texts:?}");
        // No state word on the right: the mark says the state in its colour, three
        // columns in, under the space name the rule carries.
        assert_eq!(
            lines[head].indicator,
            Some((BOX_LEAD, 2, crate::theme::of("working")))
        );
    }

    /// The tree lists agents only: a tab running a plain shell has no row of its own
    /// under its space (the tab bar above the terminal is where every tab, shell or
    /// agent, is listed), so it neither draws nor reaches its terminal from the column,
    /// and it is never counted on the rule.
    #[test]
    fn a_tab_without_an_agent_has_no_row_in_its_space() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 20);
        // Terminal 2 is a plain shell. Terminals 1 and 3 have agents.
        let listed: Vec<Option<Hit>> = (0..20)
            .map(|row| hit(&spaces, &agents, 1, 1, 20, row))
            .collect();
        assert!(listed.contains(&Some(Hit::Terminal(1))));
        assert!(listed.contains(&Some(Hit::Terminal(3))));
        assert!(
            !listed.contains(&Some(Hit::Terminal(2))),
            "the shell has no row to hit: {listed:?}"
        );
        assert!(
            !lines.iter().any(|l| unbox(&l.text) == " \u{b7} sh"),
            "a plain shell drew a row: {:?}",
            texts(&lines)
        );
        let app = lines
            .iter()
            .find(|l| l.text.starts_with("\u{2846}app"))
            .expect("the app rule");
        // The count is agents only: the shell in the same space does not add to it.
        assert!(app.text.contains("app (1)"), "{:?}", app.text);
    }

    #[test]
    fn an_agent_row_carries_its_state_on_the_dot_and_a_title_or_rule_carries_none() {
        let (spaces, agents) = sample();
        let lines = render(&spaces, &agents, 1, 1, 14);
        // A title has a look of its own, and it is never a state's: a heading that
        // came out the colour of `blocked` would be a heading somebody answers.
        assert_eq!(lines[1].colour, Some(crate::theme::Role::Title));
        let rows: Vec<&Line> = lines.iter().filter(|l| l.text.contains("claude")).collect();
        // Two rows for the one agent: plain text with the state's colour on its dot, and
        // the quiet tab row under it with no dot at all.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].colour, None);
        assert_eq!(rows[1].colour, Some(crate::theme::Role::Faint));
        assert_eq!(rows[1].indicator, None);
        assert_eq!(
            rows[0].indicator,
            Some((BOX_LEAD, 2, crate::theme::of("working")))
        );
        // A rule has no state to have a colour about: it wears the space's own colour
        // on the name, and its open line stays the space's own ink since nothing under
        // it wants a person.
        let personal = lines
            .iter()
            .find(|l| l.text.contains("personal"))
            .expect("the personal rule");
        assert_eq!(personal.colour, None);
        assert_eq!(
            personal.accent,
            Some((NAME_AT, 8, crate::theme::space_of(2)))
        );
        assert_eq!(personal.indicator, Some((0, 1, crate::theme::space_of(2))));
    }

    /// Every space is one box: its line's top carries the name, what it holds beside
    /// the name, and the two controls in fixed columns at the right.
    #[test]
    fn every_space_draws_one_rule_with_its_arrow_count_and_block() {
        let (spaces, agents) = sample();
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        let app = rule_row(&rows, 1);
        assert_eq!(app.text, format!("\u{2846}app (1){}+ z x ", " ".repeat(16)));
        assert_eq!(crate::width::width(&app.text), WIDTH);

        let ink = crate::theme::space_of(1);
        // The count, its ink, and the two controls. Every run inside the row, none of
        // them overlapping. There is no arrow run any more: the line's colour rides
        // `indicator`, not `runs`.
        assert_eq!(
            app.runs,
            vec![
                (8, 16, ink),
                (4, 4, crate::theme::Role::Faint),
                (WIDTH - 6, 1, crate::theme::Role::Faint),
                (WIDTH - 4, 1, crate::theme::Role::Faint),
                (WIDTH - 2, 1, crate::theme::Role::Faint),
            ]
        );
        // An empty space has nothing to count and nothing running, so it draws no slot,
        // no sleep toggle, and the `+` and the close.
        let dub = rule_row(&rows, 3);
        assert_eq!(dub.text, format!("\u{2846}dub{}+   x ", " ".repeat(20)));
        assert_eq!(crate::width::width(&dub.text), WIDTH);

        // Folded, the line's top stays put and the count stays.
        let shut = Shut::from([1]);
        let rows = tree_rows(&spaces, &agents, 9, 14, &Seen::new(), &shut);
        assert!(
            rule_row(&rows, 1).text.starts_with("\u{2846}app (1)"),
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
        // A tree that fits never scrolls: the first rule sits under the next row and the
        // title.
        let (spaces, agents) = sample();
        assert_eq!(hit(&spaces, &agents, 3, 3, 14, 2), Some(Hit::Space(1)));
    }

    #[test]
    fn a_height_too_small_for_the_column_still_returns_that_many_lines() {
        for (spaces, agents) in [sample(), sample_waiting()] {
            for height in 0..8 {
                let lines = render(&spaces, &agents, 1, 1, height);
                assert_eq!(lines.len(), height);
                assert!(lines.iter().all(|l| crate::width::width(&l.text) == WIDTH));
            }
        }
    }

    #[test]
    fn hit_returns_exactly_what_render_drew_on_every_row() {
        for (spaces, agents) in [sample(), sample_waiting()] {
            for height in [1usize, 2, 3, 4, 7, 9, 14, 25] {
                let lines = render(&spaces, &agents, 2, 3, height);
                let mut above: Option<Hit> = None;
                for (row, line) in lines.iter().enumerate() {
                    let text = line.text.trim_end();
                    let got = hit(&spaces, &agents, 2, 3, height, row);
                    let in_tree = lines[..row]
                        .iter()
                        .any(|l| l.text.starts_with(&format!(" {SPACES_TITLE}")));
                    // A second row reaches the terminal named just above it. Only the
                    // focused terminal's, 3, may open a window with its name cut off,
                    // when that row is all the window has room for.
                    let second = match above {
                        Some(Hit::Terminal(_)) => above,
                        _ => Some(Hit::Terminal(3)),
                    };
                    // Never the foot: at height one there is no title above it yet to
                    // set `in_tree`, and its logo (braille, not the plain word) also
                    // opens with a space and a braille cell.
                    let next = !text.ends_with('\u{ab}')
                        && (text
                            .strip_prefix(' ')
                            .and_then(|t| t.chars().next())
                            .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
                            || text == " nothing needs you");
                    let want = if !in_tree && next {
                        // The next row reaches the first of the queue, or nothing while
                        // nobody waits.
                        let queue = queue(&agents, 3, &Seen::new(), &Unseen::new());
                        Some(Hit::Next(queue.first().map(|a| a.terminal)))
                    } else if text
                        .trim_start_matches(['\u{2847}', '\u{28c7}'])
                        .trim_start()
                        .starts_with('\u{2514}')
                    {
                        second
                    } else if in_tree && text.starts_with('\u{2846}') {
                        // A space's top edge moves to the space it names: the name
                        // sits right after the arrow, with nothing but a word
                        // boundary (a space, or the row's own end) after it.
                        spaces
                            .iter()
                            // The name as the rule draws it, then whatever follows: the
                            // slot beside it, a dash, or the block. A space with nothing
                            // in it has no slot, so requiring a trailing space would lose
                            // it exactly when the row is at its emptiest.
                            .find(|s| {
                                text.find(&s.name).is_some_and(|at| {
                                    at > 0
                                        && text[..at]
                                            .chars()
                                            .next_back()
                                            .is_some_and(|c| !c.is_alphanumeric())
                                })
                            })
                            .map(|s| Hit::Space(s.id))
                    } else if text.is_empty() {
                        // A space's bottom edge and the gap between two agents are
                        // blank rows: nothing to click, so nothing they reach.
                        None
                    } else if text.starts_with(&format!(" {SPACES_TITLE}")) && text.ends_with('+') {
                        Some(Hit::Title)
                    } else if (text.contains(BRAND) || text.contains(LOGO))
                        && text.ends_with('\u{ab}')
                    {
                        // The foot's one leading space would otherwise pass for a space's
                        // box now that its own lead is one column too, so the logo (or,
                        // without braille, the brand) is named before the box is opened.
                        Some(Hit::Foot)
                    } else if in_tree
                        && let Some(inside) = text.strip_prefix(['\u{2847}', '\u{28c7}'])
                    {
                        // Inside a space: the row under an agent reaches that agent, the
                        // count of the rest reaches nothing, a shell reaches its tab, and
                        // an agent's row reaches the agent its name names.
                        if inside.starts_with('\u{2514}') {
                            if in_tree { second } else { above }
                        } else if inside.starts_with('+') {
                            None
                        } else if inside.starts_with('\u{b7}') {
                            agents
                                .iter()
                                .find(|a| {
                                    !a.agent
                                        && a.program.as_deref().is_some_and(|p| inside.contains(p))
                                })
                                .map(|a| Hit::Terminal(a.terminal))
                        } else {
                            let named = inside
                                .trim_start_matches(|c: char| {
                                    ('\u{2800}'..='\u{28ff}').contains(&c)
                                })
                                .trim()
                                .split("  ")
                                .next()
                                .unwrap_or("")
                                .trim_end_matches('\u{2026}')
                                .to_string();
                            agents
                                .iter()
                                .find(|a| !named.is_empty() && session_name(a).starts_with(&named))
                                .map(|a| Hit::Terminal(a.terminal))
                        }
                    } else {
                        // Everything left is an agent in a flat row, its mark at column
                        // one: one whose space the client has not heard of.
                        agents
                            .iter()
                            .find(|a| {
                                text.strip_prefix(' ')
                                    .map(|rest| {
                                        rest.trim_start_matches(|c: char| {
                                            ('\u{2800}'..='\u{28ff}').contains(&c)
                                        })
                                        .starts_with(&session_name(a))
                                    })
                                    .unwrap_or(false)
                            })
                            .map(|a| Hit::Terminal(a.terminal))
                    };
                    assert_eq!(got, want, "row {row} of {height}: {text:?}");
                    above = want;
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
            lines
                .iter()
                .any(|l| l.text.starts_with(" \u{282f}\u{2807}codex")),
            "{:?}",
            texts(&lines)
        );
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(4, 77, "codex", "idle"), agent(5, 1, "claude", "idle")];
        let rows = tree_rows(&spaces, &agents, 5, 14, &Seen::new(), &Shut::new());
        assert_eq!(order(&rows), vec![5, 4], "the stranger follows the tree");
        let stray = tree_row(&rows, 4);
        assert!(
            stray.text.starts_with(" \u{2810}\u{2800}codex"),
            "{:?}",
            stray.text
        );
        assert_eq!(stray.accent, None, "no space to name");
    }

    fn named(terminal: u32, workspace: i64, name: &str, program: &str, state: &str) -> AgentRow {
        AgentRow {
            news: false,
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
            pgid: None,
            discards: None,
        }
    }

    fn agent_texts(spaces: &[WorkspaceRow], agents: &[AgentRow], focused: u32) -> Vec<String> {
        unboxed(&render(spaces, agents, 1, focused, 20))
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
            assert_eq!(unbox(&rows[at].0.text), "\u{2847}\u{2810}\u{2800}opencode");
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
                texts.iter().any(|t| {
                    t.starts_with(&format!(" \u{2810}\u{2800}{want}")) && t.ends_with(" app")
                }),
                "{texts:?}"
            );
        }
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with(" \u{2810}\u{2800}opencode") && t.ends_with(" personal")),
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
                .any(|t| t.starts_with(" \u{2810}\u{2800}release") && t.ends_with(" app")),
            "{flat:?}"
        );
        assert!(
            flat.iter()
                .any(|t| t.starts_with(" \u{2810}\u{2800}release") && t.ends_with(" personal")),
            "{flat:?}"
        );
        // Without a reported session the row falls back to the tab's own label, so
        // both tree rows read the chosen name behind the mark.
        let texts = agent_texts(&spaces, &agents, 1);
        assert_eq!(
            texts
                .iter()
                .filter(|t| *t == "\u{2847}\u{2810}\u{2800}release")
                .count(),
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
                Some((BOX_LEAD, 2, crate::theme::of("idle"))),
                Some((BOX_LEAD, 2, crate::theme::of("blocked"))),
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

    /// The name row is the session id behind the dot, and nothing else: no program
    /// beside it, so four live sessions never all read `opencode`. The tab label a
    /// person chose rides the second row, under the name.
    #[test]
    fn a_reported_session_names_the_row_alone() {
        let spaces = vec![space(1, "app")];
        let mut a = agent(1, 1, "opencode", "idle");
        a.name = Some("release".to_string());
        a.agent_session = Some("abc123".to_string());
        let texts = agent_texts(&spaces, &[a], 1);
        assert!(
            texts.iter().any(|t| t == "\u{2847}\u{2810}\u{2800}abc123"),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("opencode")),
            "the program leaked onto a reported session row: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "\u{28c7}  \u{2514} release"),
            "the tab label is missing under the name: {texts:?}"
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
        // No reported session, so the tab's own label answers behind the mark.
        assert!(
            texts.iter().any(|t| t == "\u{2847}\u{2810}\u{2800}codex"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_wrapper_is_shown_as_standing_in_the_way_and_never_as_the_program() {
        let agents = vec![AgentRow {
            news: false,
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
            pgid: None,
            discards: None,
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
            news: false,
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
            pgid: None,
            discards: None,
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
            rule.text.starts_with("\u{2846}\u{4e2d}\u{6587} (1)"),
            "{:?}",
            rule.text
        );
        assert_eq!(crate::width::width(&rule.text), WIDTH);
        // The agent's row opens with the space's line, then the state mark and the name.
        let row = tree_row(&rows, 1);
        assert!(
            row.text.starts_with("\u{2847}\u{2810}\u{2800}"),
            "{:?}",
            row.text
        );
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
        assert_eq!(
            unbox(&row.text),
            "\u{2847}\u{2810}\u{2800}claude",
            "{:?}",
            row.text
        );
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
        // Every agent draws its name row under its space, in the daemon's order inside it:
        // terminal 1 (blocked) first because it is listed first, not because of what
        // it needs. Urgency is the next row's job, which names the one after the keyboard's
        // own terminal 1: that one is already in front of the person.
        assert_eq!(order(&rows), vec![1, 3, 5, 6, 9, 2, 4, 7, 8]);
        assert_eq!(next_of(&rows), Some(2));
        // At frame zero running agents carry the working dot, no spinner, and no
        // state word on the right.
        assert!(
            lines
                .iter()
                .any(|l| l.indicator == Some((BOX_LEAD, 2, crate::theme::of("working")))),
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
        // Nine tree rows, and the one the next row names.
        assert_eq!(
            texts
                .iter()
                .filter(|t| !t.trim_start().starts_with(['\u{2514}', '\u{2570}']))
                .filter(|t| !t.contains('\u{2514}'))
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
        let texts = unboxed(&folded(
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
        // their rows, each with its own state mark.
        assert!(
            texts.iter().any(|t| t == "\u{2847}\u{2818}\u{2802}claude"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "\u{2847}\u{2810}\u{2800}codex"),
            "{texts:?}"
        );
    }

    #[test]
    fn the_foot_of_the_column_carries_the_mark_the_version_and_the_menu() {
        let rows = foot_rows(WIDTH, None, false, None);
        assert_eq!(rows.len(), 2, "the logo row and the version row");
        let (top, hit) = &rows[0];
        assert_eq!(
            crate::width::width(&top.text),
            WIDTH,
            "the row has to fill the column exactly or the one under it shifts: {:?}",
            top.text
        );
        // Under braille the logo is drawn in dots, so the plain word is not on this row.
        assert!(top.text.contains(LOGO), "{:?}", top.text);
        assert!(
            top.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "the menu and the fold mark close the row: {:?}",
            top.text
        );
        assert_eq!(*hit, Some(Hit::Foot));
        // The letters wear the text colour; the row carries no accent field at all.
        assert_eq!(top.accent, None);
        assert_eq!(top.runs, vec![(1, 8, crate::theme::Role::Text)], "{top:?}");
        // The version is under the logo, on a row of its own that answers no click.
        let (bottom, hit) = &rows[1];
        assert_eq!(
            crate::width::width(&bottom.text),
            WIDTH,
            "{:?}",
            bottom.text
        );
        assert!(
            bottom.text.contains(VERSION),
            "a person reporting a bug has to be able to read the version off the screen: {:?}",
            bottom.text
        );
        assert_eq!(*hit, None);
        // The q's tail opens the version row under the q's right column, alone in its
        // cell, the only dot in ember.
        assert!(
            bottom.text.starts_with(&format!("  {LOGO_TAIL} ")),
            "{bottom:?}"
        );
        assert_eq!(bottom.runs, vec![(2, 1, crate::theme::Role::Brand)]);

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

        // Folded to a strip there is no room for the logo, and the fold mark is the row.
        let (strip, hit) = collapse_row();
        assert_eq!(crate::width::width(&strip.text), COLLAPSED);
        assert!(!strip.text.contains(BRAND));
        assert_eq!(hit, Some(Hit::Foot));
        assert_eq!(act(Hit::Foot, 2, &c, true), Some(Act::Collapse));
    }

    /// A picked avatar signs the version row with its name, when one is picked; the logo
    /// row above carries no trace of it, since the pet no longer draws dots there.
    #[test]
    fn a_picked_avatar_names_the_version_row() {
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 33,
        };
        assert_eq!(pet.name(), "Mohawk Pink");
        let rows = foot_rows(WIDTH, Some(&pet), false, None);
        let (top, _) = &rows[0];
        assert!(!top.text.contains("Mohawk"), "{top:?}");
        let (bottom, hit) = &rows[1];
        assert_eq!(crate::width::width(&bottom.text), WIDTH, "{bottom:?}");
        assert!(bottom.text.contains(VERSION), "{bottom:?}");
        assert!(bottom.text.contains("Mohawk Pink"), "{bottom:?}");
        assert_eq!(*hit, None);

        // Without a pet the version row carries the version alone.
        let (bare, _) = &foot_rows(WIDTH, None, false, None)[1];
        assert!(bare.text.contains(VERSION), "{bare:?}");
        assert!(!bare.text.contains("Mohawk"), "{bare:?}");
    }

    /// A pet name the version row cannot hold whole is left off, not cut: a stub reads as
    /// a smudge. The row stays exactly the column's width either way.
    #[test]
    fn a_long_pet_name_is_left_off_rather_than_cut() {
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "thin4".into(),
            eyes: "big".into(),
            hair_ink: 208,
            body_ink: 33,
        };
        assert_eq!(pet.name(), "Thin 4 Blocked");
        let (wide, _) = &foot_rows(SIDEBAR_MAX, Some(&pet), false, None)[1];
        assert_eq!(crate::width::width(&wide.text), SIDEBAR_MAX);
        assert!(wide.text.contains("Thin 4 Blocked"), "{wide:?}");

        let (narrow, _) = &foot_rows(SIDEBAR_MIN, Some(&pet), false, None)[1];
        assert_eq!(crate::width::width(&narrow.text), SIDEBAR_MIN);
        assert!(narrow.text.contains(VERSION), "{narrow:?}");
        assert!(!narrow.text.contains('\u{2026}'), "{narrow:?}");
        assert!(!narrow.text.contains("Thin"), "{narrow:?}");
    }

    /// Beside the pet, a version or a name that does not fit whole is not shown: a notice
    /// cut to `0.4.0>0.` reads as another version.
    #[test]
    fn beside_the_pet_the_foot_never_cuts_a_version_or_a_name() {
        let pet = art_pet();
        let narrow = foot_rows(PET_COLS + LOGO_COLS + 3, Some(&pet), true, Some("0.10.0"));
        assert_eq!(
            narrow[1].0.text.trim_end(),
            format!("  {LOGO_TAIL} {VERSION}")
        );
        assert_eq!(narrow[2].0.text.trim_end(), "");
        let wide = foot_rows(SIDEBAR_MAX, Some(&pet), true, Some("0.10.0"));
        assert_eq!(
            wide[1].0.text.trim_end(),
            format!("  {LOGO_TAIL} {VERSION}>0.10.0")
        );
        assert_eq!(wide[2].0.text.trim_end(), format!(" {}", pet.name()));
    }

    /// Without an avatar the logo row is byte for byte what it always was.
    #[test]
    fn without_an_avatar_the_foot_is_unchanged() {
        let (line, _) = &foot_rows(WIDTH, None, false, None)[0];
        assert_eq!(line.accent, None);
        assert_eq!(
            line.runs,
            vec![(1, crate::width::width(LOGO), crate::theme::Role::Text)]
        );
        assert_eq!(line.indicator, None);
        assert!(line.text.contains(LOGO));
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
        assert!(avatar_fits(10, 19, true));
        assert!(!avatar_fits(30, 30, false));
        assert!(!avatar_fits(30, 18, true));
        assert!(!avatar_fits(9, 30, true));
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

    /// With a pet and a list short enough to stand above it, the foot takes its four
    /// rows (the logo, the version and the name beside where the pet stamps, then the
    /// menu on its own row) and the column keeps its exact height. A list that needs
    /// those rows keeps them: the pet gives way, and no agent is pushed off the column
    /// for a picture.
    #[test]
    fn a_pet_takes_four_rows_only_when_the_list_still_fits() {
        let pet = art_pet();
        let at = |(spaces, agents): (Vec<WorkspaceRow>, Vec<AgentRow>)| {
            rows12(
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
            )
        };
        let rows = at(sample());
        assert_eq!(rows.len(), 30);
        // The four rows of the foot never carry an agent: the pet's reservation, not
        // the list's, whatever the overlay stamps over their trailing columns.
        for (line, hit) in &rows[26..30] {
            assert!(!matches!(hit, Some(Hit::Terminal(_))), "{line:?}");
        }
        let (menu_row, hit) = &rows[29];
        assert!(
            menu_row.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "{menu_row:?}"
        );
        assert_eq!(*hit, Some(Hit::Foot));
        // A crowd needs every row: the pet gives way, and the two rows it would have
        // reserved carry agents instead.
        let crowded = at(crowd());
        assert_eq!(crowded.len(), 30);
        for row in &crowded[26..28] {
            assert!(carries_agents(row), "{row:?}");
        }
    }

    /// A row of the list proper: an agent's, or the thin (now blank) line between two
    /// agents. A crowded list never reserves padding, so a blank row here can only be
    /// that gap, never wasted space.
    fn carries_agents((line, hit): &Row) -> bool {
        matches!(hit, Some(Hit::Terminal(_))) || line.text.chars().all(|c| c == ' ')
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
            plain[26..28].iter().all(carries_agents),
            "thirty agents must reach the rows a pet would have reserved: {:?}",
            &plain[26..28]
        );
    }

    /// Overlay rows sit directly above the foot's menu row, top first.
    #[test]
    fn overlay_rows_sit_directly_above_the_foot() {
        assert_eq!(avatar_overlay_rows(1, 30), vec![27, 28, 29]);
    }

    /// The overlay paints the pet in its own inks: three braille rows cut to its lit cells,
    /// each reset at the end, empty without colour.
    #[test]
    fn the_overlay_paints_the_pet_in_its_inks() {
        let (cols, rows) = avatar_overlay(&art_pet(), true);
        assert_eq!(rows.len(), 3);
        assert!(cols > 0 && cols <= PET_COLS, "{cols}");
        // Every escape spends a foreground ink; strip them and what is left is braille
        // dots alone, one cell a column, the row's own width.
        let strip_escapes = |row: &str| -> String {
            let mut out = String::new();
            let mut chars = row.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        for row in &rows {
            assert!(row.ends_with("\x1b[0m"), "{row:?}");
            let dots = strip_escapes(row);
            assert_eq!(crate::width::width(&dots), cols, "{row:?}");
            assert!(
                dots.chars().all(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
                "unaccounted cells: {row:?}"
            );
        }
        assert!(rows[0].contains("38;5;212"), "hair on top: {:?}", rows[0]);
        assert_eq!(avatar_overlay(&art_pet(), false), (0, Vec::new()));
    }

    /// A seen update signs the foot of the column with where this build stands.
    #[test]
    fn a_seen_update_names_itself_beside_the_version() {
        // Said directly rather than through the notice file: every foot painted by any
        // test in this process reads that file, so writing it here changed their rows
        // and failed them at random.
        let rows = foot_rows(WIDTH, None, false, Some("9.9.9"));
        let (top, bottom) = (&rows[0].0, &rows[1].0);
        assert!(
            bottom.text.contains("9.9.9"),
            "the waiting release has to be readable off the screen: {:?}",
            bottom.text
        );
        assert_eq!(
            crate::width::width(&bottom.text),
            WIDTH,
            "the longer row still fills the column exactly: {:?}",
            bottom.text
        );
        // The notice rides the version row; the menu is on the logo row above it and
        // never moves for it.
        assert!(
            top.text.contains(MENU_LABEL),
            "the notice never pushes the menu off: {:?}",
            top.text
        );
        let (plain, _) = &foot_rows(WIDTH, None, false, None)[1];
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
    fn priority_view_sorts_by_urgency_without_headers() {
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
        for header in ["needs you", "running", "finished"] {
            assert!(
                !texts.iter().any(|t| t.trim_start().starts_with(header)),
                "a header leaked into the priority view: {texts:?}"
            );
        }
        // The title sits under the next row, where it sits in the tree.
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert!(texts[1].ends_with("priority +"), "{texts:?}");
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
        // Every agent draws one name row, with its second row under it, and the next row
        // names one of them.
        assert_eq!(
            texts
                .iter()
                .filter(|t| !t.trim_start().starts_with('\u{2514}'))
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            10,
            "{texts:?}"
        );
    }

    /// Frame zero is the still column, every mark its dot. Past it only a working agent
    /// moves: its dot turns the spinner, in the working colour, while every calm row is
    /// byte for byte what it was. No state word is ever drawn.
    #[test]
    fn only_a_working_agent_moves_and_frame_zero_is_still() {
        let (spaces, agents) = many_agents();
        let at = |tick: u64| {
            rows12(
                &spaces,
                &agents,
                2,
                1,
                48,
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
        // Two ticks two frames apart: a frame lasts two ticks, so comparing across a
        // single frame boundary would still catch nothing turned.
        let (two, six) = (at(2), at(6));
        let mut moved = 0;
        for agent in agents.iter().filter(|a| a.agent) {
            let still = tree_row(&zero, agent.terminal).clone();
            let role = crate::theme::of(&agent.state);
            let still_mark = cell_mark(role, agent.state == "working", 0);
            assert!(
                still.text.contains(&still_mark),
                "frame zero moved: {still:?}"
            );
            let (a, b) = (
                tree_row(&two, agent.terminal),
                tree_row(&six, agent.terminal),
            );
            if agent.state == "working" {
                moved += 1;
                assert_ne!(
                    a.text, still.text,
                    "the working row never left frame zero: {a:?}"
                );
                assert_ne!(
                    a.text, b.text,
                    "the working row's mark never turned: {a:?} {b:?}"
                );
                assert_eq!(
                    b.indicator,
                    Some((BOX_LEAD, 2, crate::theme::of("working")))
                );
            } else {
                assert_eq!(a, b, "a calm row moved: {a:?}");
                assert_eq!(*a, still, "a calm row differs from the still column: {a:?}");
            }
        }
        assert!(moved > 0, "the fixture has no working agent to move");
        // An unread done keeps the done square on its mark, steady.
        assert_eq!(
            tree_row(&six, 5).indicator,
            Some((BOX_LEAD, 2, crate::theme::of("done"))),
            "no steady unread mark"
        );
        // And no state word on the right of any row.
        for (line, _) in &six {
            for word in ["working", "blocked", "failed", "done", "idle"] {
                assert!(
                    !line.text.trim_end().ends_with(word),
                    "a state word leaked: {:?}",
                    line.text
                );
            }
        }
    }

    /// Across a full breath (and the next one, which starts one arm further round), a
    /// working frame never paints another state's fixed picture: the centre alone
    /// (idle), the middle row (asleep), the full ring (waiting), the full square (done)
    /// or the diamond cross (failed). Only the shapes differ; colour is not what tells
    /// them apart here.
    #[test]
    fn no_working_frame_is_ever_another_states_picture() {
        use crate::theme::Role;
        let others = [
            cell_mark(Role::Idle, false, 0),
            cell_mark(Role::Sleeping, false, 0),
            cell_mark(Role::Blocked, false, 0),
            cell_mark(Role::Done, false, 0),
            cell_mark(Role::Failed, false, 0),
        ];
        for tick in 1..=64u64 {
            let mark = cell_mark(Role::Working, true, tick);
            assert!(
                !others.contains(&mark),
                "tick {tick} painted another state's picture: {mark:?}"
            );
        }
    }

    /// A working ring never closes into the shape blocked wears: the two must read apart
    /// by shape, not only by colour, at every frame the breath turns through.
    #[test]
    fn a_working_mark_never_closes_into_the_waiting_ring() {
        use crate::theme::Role;
        let blocked = cell_mark(Role::Blocked, false, 0);
        for tick in 1..=24u64 {
            assert_ne!(
                cell_mark(Role::Working, true, tick),
                blocked,
                "tick {tick} closed the ring"
            );
        }
    }

    /// The sweep crosses a working agent's activity once every two seconds, brighter
    /// greys in a band and the row's own colour outside it; frame zero has no band.
    #[test]
    fn the_sweep_crosses_a_working_activity_and_frame_zero_has_none() {
        assert!(
            sweep("editing sidebar.rs", 7, 0).len() <= 3,
            "a band parked at the edge"
        );
        let mid = sweep("editing sidebar.rs", 7, SWEEP_TICKS / 2);
        assert!(!mid.is_empty(), "no band mid sweep");
        for (at, len, role) in &mid {
            assert!(*at >= 7 && *len == 1, "{mid:?}");
            assert!(
                matches!(role, crate::theme::Role::Shimmer(1..=11)),
                "{mid:?}"
            );
        }
        assert_eq!(
            sweep("editing sidebar.rs", 7, 3),
            sweep("editing sidebar.rs", 7, 3 + SWEEP_TICKS),
            "the sweep does not come back round"
        );
        let spaces = vec![space(1, "app")];
        let mut working = agent(1, 1, "claude", "working");
        working.activity = Some("Edit src/sidebar.rs".into());
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let still = column(
            &spaces,
            std::slice::from_ref(&working),
            1,
            9,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        let row = second_row(&still, &working, BOX_LEAD + 2, None);
        assert!(
            !row.0
                .runs
                .iter()
                .any(|(_, _, r)| matches!(r, crate::theme::Role::Shimmer(_))),
            "frame zero drew a band: {:?}",
            row.0
        );
        // Working with no activity reported, mid sweep: the worktree after the tab is a
        // place, and a place never shimmers.
        working.activity = None;
        let moving = column(
            &spaces,
            std::slice::from_ref(&working),
            1,
            9,
            SWEEP_TICKS / 2,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        let row = second_row(&moving, &working, BOX_LEAD + 2, Some("qoro".into()));
        assert_eq!(row.0.text.trim_end(), "   \u{2514} claude \u{b7} qoro");
        assert!(
            !row.0
                .runs
                .iter()
                .any(|(_, _, r)| matches!(r, crate::theme::Role::Shimmer(_))),
            "the worktree shimmered: {:?}",
            row.0
        );
    }

    /// The next row names who to see to next and lines up the rest after it: the next
    /// agent's mark, name and wait, then one mark per queued agent in its kind's ink. It
    /// is one row whatever waits, so the tree under it stays where it was, and it never
    /// names the agent the keyboard is already on.
    #[test]
    fn the_next_row_names_the_first_waiting_agent_and_queues_the_rest() {
        use crate::theme::Role;
        let spaces = vec![space(1, "default"), space(2, "test space")];
        let mut blocked = agent(1, 1, "claude", "blocked");
        blocked.agent_title = Some("Claude Code TUI tasar\u{131}m ve bes".into());
        blocked.last_active_at_unix = Some(NOW - 12);
        let mut done = agent(2, 2, "claude", "idle");
        done.agent_session = Some("0d140105-aaaa".into());
        done.last_active_at_unix = Some(NOW - 150);
        let mut other = agent(3, 1, "codex", "idle");
        other.last_active_at_unix = Some(NOW - 30);
        let mut unseen = Unseen::new();
        unseen.insert((2, gen_of(&done)));
        unseen.insert((3, gen_of(&other)));
        let agents = vec![blocked, done, other];
        let (seen, shut) = (Seen::new(), Shut::new());
        let at = |width: usize, focused: u32, unseen: &Unseen| {
            let c = column(
                &spaces,
                &agents,
                1,
                focused,
                0,
                View::Tree,
                &seen,
                unseen,
                NOW,
                width,
                &shut,
            );
            rows_folded(&c, 40, None)
        };
        let rows = at(WIDTH, 9, &unseen);
        assert_eq!(
            rows[0].0.text,
            " \u{282f}\u{2807}Claude Code TUI \u{2026} 12s \u{283f}\u{2807}\u{283f}\u{2807} "
        );
        assert_eq!(rows[0].1, Some(Hit::Next(Some(1))));
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert_eq!(
            rows[0].0.runs,
            vec![
                (1, 2, Role::Blocked),
                (21, 3, Role::Faint),
                (25, 2, crate::theme::of("done")),
                (27, 2, crate::theme::of("done")),
            ]
        );
        // Standing on the blocked agent, the next one is the first finish after it.
        assert_eq!(next_of(&at(WIDTH, 1, &unseen)), Some(2));
        // The narrowest column keeps the name and the wait, exactly its width: the two-cell
        // marks no longer fit beside them there, so the marks give way to their count first.
        let narrow = at(SIDEBAR_MIN, 9, &unseen);
        assert_eq!(narrow[0].0.text, " \u{282f}\u{2807}Claude\u{2026} 12s +2 ");
        assert_eq!(crate::width::width(&narrow[0].0.text), SIDEBAR_MIN);
        // Nobody waiting: the same one row says so and reaches nothing, and the tree under
        // it starts on the same row it did while three agents waited.
        let none = Unseen::new();
        let one = at(WIDTH, 9, &none);
        let mut calm_agents = agents.clone();
        calm_agents[0].state = "working".into();
        let c = column(
            &spaces,
            &calm_agents,
            1,
            9,
            0,
            View::Tree,
            &seen,
            &none,
            NOW,
            WIDTH,
            &shut,
        );
        let calm_rows = rows_folded(&c, 40, None);
        assert_eq!(calm_rows[0].0.text.trim_end(), " nothing needs you");
        assert_eq!(calm_rows[0].1, Some(Hit::Next(None)));
        assert_eq!(calm_rows[0].0.colour, Some(Role::Faint));
        let first_space = |rows: &[Row]| {
            rows.iter()
                .position(|(_, h)| matches!(h, Some(Hit::Space(_))))
        };
        assert_eq!(first_space(&calm_rows), first_space(&rows));
        assert_eq!(first_space(&one), first_space(&rows));
        // Past four marks the rest are counted.
        let many: Vec<AgentRow> = (1..=7).map(|t| agent(t, 1, "claude", "done")).collect();
        let c = column(
            &spaces,
            &many,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &none,
            NOW,
            WIDTH,
            &shut,
        );
        let rows = rows_folded(&c, 40, None);
        assert!(
            rows[0]
                .0
                .text
                .ends_with("\u{283f}\u{2807}\u{283f}\u{2807}\u{283f}\u{2807}\u{283f}\u{2807}+2 "),
            "{:?}",
            rows[0].0.text
        );
        assert_eq!(crate::width::width(&rows[0].0.text), WIDTH);
        // The rank view keeps the next row too, so the title never moves when it flips.
        let c = column(
            &spaces,
            &agents,
            1,
            9,
            0,
            View::Priority,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        assert_eq!(rows_folded(&c, 40, None)[0].1, Some(Hit::Next(Some(1))));
    }

    /// Shape says failed: the folded strip still wears the diamond for failed and the
    /// circle for the rest (see the strip's own test below), but the tree and flat rows
    /// wear a picture per state instead, from `cell_mark`, so the magenta alarm never
    /// stands on colour alone beside the warm pair. Failed is never news (`is_new` only
    /// fires for done/idle), so its X is unconditional.
    #[test]
    fn failed_wears_the_diamond_and_every_other_state_the_circle() {
        for state in ["working", "starting", "blocked", "done", "idle", "unknown"] {
            assert_eq!(dot_glyph(crate::theme::of(state)), DOT, "{state}");
        }
        assert_eq!(dot_glyph(crate::theme::of("failed")), FAILED_DOT);
        // A failed agent renders its X under its rule in the tree, steady, in the
        // failed colour and reaching the failed terminal.
        let spaces = vec![space(2, "webshop")];
        let agents = vec![agent(2, 2, "claude", "failed")];
        let lines = render(&spaces, &agents, 2, 99, 8);
        let mark = cell_mark(crate::theme::of("failed"), false, 0);
        // The space's own line sits in column zero, ahead of the mark: this agent is
        // its space's only row, so it is not the last and wears `LINE`, not `LINE_FOOT`.
        let prefix = format!("\u{2847}{mark}claude");
        // The next row at the top wears the same mark on the same agent, so the tree's
        // own row is the last one that opens with it, not the first.
        let row = lines
            .iter()
            .rposition(|l| l.text.starts_with(&prefix))
            .unwrap_or_else(|| panic!("no failed row {prefix:?}: {:?}", texts(&lines)));
        assert_eq!(
            lines[row].indicator,
            Some((BOX_LEAD, 2, crate::theme::of("failed")))
        );
        assert_eq!(hit(&spaces, &agents, 2, 99, 8, row), Some(Hit::Terminal(2)));
        // An unseen failed finish keeps its X: failed is never news.
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
                .skip_while(|l| !l.text.starts_with(&format!(" {SPACES_TITLE}")))
                .any(|l| l.text.starts_with(&format!("\u{2847}{mark}"))),
            "unseen failed lost its mark: {:?}",
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
    /// away wears the done square, never the failed X, because failed is never news.
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
        // Its dotted row in the tree wears the done circle.
        let mine: Vec<&Line> = rows
            .iter()
            .filter(|(l, _)| l.indicator.is_some())
            .filter(|(_, h)| matches!(h, Some(Hit::Terminal(6))))
            .map(|(l, _)| l)
            .collect();
        assert_eq!(mine.len(), 1);
        for row in mine {
            let (at, width, role) = row.indicator.expect("a mark");
            assert_eq!(width, 2);
            let got: String = row.text.chars().skip(at).take(width).collect();
            assert_eq!(
                got,
                cell_mark(role, false, 0),
                "fresh idle is not the done square: {:?}",
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

                let foot = rows
                    .iter()
                    .find(|(_, h)| *h == Some(Hit::Foot))
                    .map(|(l, _)| l.text.clone())
                    .expect("a foot");
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

                // A top edge folds at its corner and arrow, in the tree only, and only when
                // there is something under it to fold. Its two controls answer in their own
                // columns, which are the same whatever the name and the count are.
                for col in 1..=width {
                    let want = if view == View::Tree && col <= NAME_AT {
                        Act::Fold(1)
                    } else if col == width - 6 + 1 {
                        Act::New(1)
                    } else if col == width - 4 + 1 {
                        Act::Sleep(1)
                    } else if col == width - 2 + 1 {
                        Act::Close(1)
                    } else {
                        Act::Space(1)
                    };
                    assert_eq!(act(Hit::Space(1), col, &c, false), Some(want), "space 1");
                    // The empty space draws no sleep toggle, so every column but the `+`
                    // and the close is the space itself.
                    let want = if col == width - 6 + 1 {
                        Act::New(3)
                    } else if col == width - 2 + 1 {
                        Act::Close(3)
                    } else {
                        Act::Space(3)
                    };
                    assert_eq!(act(Hit::Space(3), col, &c, false), Some(want), "space 3");
                }
                assert_eq!(act(Hit::Space(1), 1, &c, true), Some(Act::Space(1)));
                assert_eq!(act(Hit::Foot, 1, &c, true), Some(Act::Collapse));
                assert_eq!(act(Hit::Terminal(3), 1, &c, false), Some(Act::Terminal(3)));
            }
        }
        // The fold cell is exactly the line's top: the one column before the name.
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        let head: String = rule_row(&rows, 1).text.chars().take(NAME_AT).collect();
        assert_eq!(head, "\u{2846}");
    }

    /// No breath between the title and the tree any more: the first rule is itself a
    /// separator, so the row under the title is already the first space.
    #[test]
    fn the_tree_opens_right_under_the_title() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "idle")];
        let rows = tree_rows(&spaces, &agents, 1, 10, &Seen::new(), &Shut::new());
        assert_eq!(rows[0].1, Some(Hit::Next(None)));
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert_eq!(rows[2].1, Some(Hit::Space(1)));
        assert_eq!(rows[3].1, Some(Hit::Terminal(1)));
    }

    /// A blocked agent is named in the next row at the top, where a person looks first,
    /// and still draws its row under its space: the next row points, it does not move.
    #[test]
    fn a_blocked_agent_is_next_and_still_drawn_under_its_space() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "blocked")];
        let rows = tree_rows(&spaces, &agents, 9, 10, &Seen::new(), &Shut::new());
        let texts = texts(&rows.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>());
        assert!(texts[0].starts_with(" \u{282f}\u{2807}codex"), "{texts:?}");
        assert_eq!(rows[0].1, Some(Hit::Next(Some(1))));
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert_eq!(order(&rows), vec![1]);
        assert!(!texts.iter().any(|t| t.contains("idle")), "{texts:?}");
    }

    /// The queue lists who needs a person by what they need: blocked first, then
    /// failed, then a finish nobody has read. Working and calm agents are the tree's
    /// business alone, and the terminal the keyboard is on is never next.
    #[test]
    fn the_queue_runs_blocked_then_failed_then_unread_done() {
        let agents = vec![
            agent(1, 1, "claude", "done"),
            agent(2, 1, "codex", "failed"),
            agent(3, 2, "claude", "working"),
            agent(4, 2, "codex", "blocked"),
        ];
        let terminals = |focused: u32, seen: &Seen| -> Vec<u32> {
            queue(&agents, focused, seen, &Unseen::new())
                .iter()
                .map(|a| a.terminal)
                .collect()
        };
        assert_eq!(terminals(9, &Seen::new()), vec![4, 2, 1]);
        // Standing on the blocked one: it is in front of the person, so failed is next.
        assert_eq!(terminals(4, &Seen::new()), vec![2, 1]);
        // The walk goes on from where the keyboard is and wraps, so pressing on and on
        // reaches the finish too instead of trading between the two still waiting.
        assert_eq!(terminals(2, &Seen::new()), vec![1, 4]);
        assert_eq!(terminals(1, &Seen::new()), vec![4, 2]);
        // Standing anywhere that is not waiting starts over from the top.
        assert_eq!(terminals(3, &Seen::new()), vec![4, 2, 1]);
        // A finish already read is not news.
        assert_eq!(terminals(9, &Seen::from([(1, 0)])), vec![4, 2]);
        // The row shows the first, then the others' marks in the queue's order.
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let rows = tree_rows(&spaces, &agents, 9, 40, &Seen::new(), &Shut::new());
        assert_eq!(next_of(&rows), Some(4));
        assert!(
            rows[0]
                .0
                .text
                .trim_end()
                .ends_with("\u{2815}\u{2805}\u{283f}\u{2807}"),
            "{:?}",
            rows[0].0
        );
        // Calm the fleet down and the row says nobody is waiting.
        let mut calm = agents.clone();
        calm[1].state = "idle".into();
        calm[3].state = "working".into();
        let rows = tree_rows(&spaces, &calm, 9, 20, &Seen::from([(1, 0)]), &Shut::new());
        assert_eq!(next_of(&rows), None);
        assert_eq!(rows[0].0.text.trim_end(), " nothing needs you");
        assert_eq!(rows[1].1, Some(Hit::Title));
    }

    /// Every height from nothing to a tall window, every pulled width, with and without a
    /// pet, a waiting agent and a three digit count: always exactly the rows asked for, each exactly
    /// the width, open or folded to the strip. A short row leaves the last frame's pixels
    /// behind and a long one spills into the terminal beside the column.
    #[test]
    fn every_height_and_width_paints_exact_rows_with_pet_next_row_and_big_counts() {
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
        // Finished while nobody looked, so the done box is swept too, with a wide name.
        let mut unseen = Unseen::new();
        for t in 600..604 {
            let mut done = agent(t, 2, "claude", "idle");
            done.agent_title = Some("\u{4e2d}\u{6587}\u{4e2d}\u{6587} finished a long job".into());
            done.last_active_at_unix = Some(NOW - 90_000);
            unseen.insert((t, gen_of(&done)));
            agents.push(done);
        }
        let pet = qoro_runtime::avatar::Avatar {
            body: qoro_runtime::avatar::BODY.into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 33,
        };
        let seen = Seen::new();
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
            (
                "Task Review second-row diff",
                "subagent Review second-row diff",
            ),
            // Claude and Gemini hooks send a command's description instead of the command.
            (
                "Bash Resubmit prompt, type a draft, capture frames",
                "Resubmit prompt, type a draft, capture frames",
            ),
            // Without one, the set-up in front of a command is not what it runs.
            ("Bash cd /Users/me/qoro && cargo test", "cargo test"),
            (
                "Bash S=/private/tmp/x; tmux send-keys -t spin",
                "tmux send-keys -t spin",
            ),
            ("Bash cd /a && RUST_LOG=debug FOO=1 cargo run", "cargo run"),
            ("Bash cd /a; ls", "ls"),
            ("Bash cd /a", "cd /a"),
            ("Bash FOO=1", "FOO=1"),
            ("Bash git diff a==b", "git diff a==b"),
            ("shell bash -lc 'cd /w && bun test'", "bun test"),
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
    /// one says what it wants in the blocked colour.
    /// A calm agent keeps one row, whatever its last tool was.
    #[test]
    fn activity_rides_a_sub_row_in_the_tree() {
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
        // The tab, then what it does; inside the box at thirty columns the activity is
        // the one cut.
        assert_eq!(
            unbox(&one[1].text),
            "\u{2847}  \u{2514} claude \u{b7} editing sideba\u{2026}"
        );
        assert_eq!(one[1].colour, Some(crate::theme::Role::Faint));
        let two = under(2);
        assert_eq!(
            unbox(&two[1].text),
            "\u{2847}  \u{2514} codex \u{b7} wants: bun test"
        );
        // Quiet grey, with what it wants in the blocked colour.
        assert_eq!(two[1].colour, Some(crate::theme::Role::Faint));
        assert!(
            two[1].runs.contains(&(13, 15, crate::theme::Role::Blocked)),
            "{:?}",
            two[1].runs
        );
        // A calm agent keeps its tab row, without the stale activity.
        let three = under(3);
        assert_eq!(three.len(), 2, "{texts:?}");
        assert_eq!(unbox(&three[1].text), "\u{28c7}  \u{2514} gemini");
        // The next row names the waiting agent; what it wants is its row's job.
        assert!(texts[0].starts_with(" \u{282f}\u{2807}codex"), "{texts:?}");
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{line:?}");
        }
    }

    /// A waiting agent whose newest word is `thinking` has not said what it wants: no
    /// `wants: thinking` row in the tree.
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

    /// The column moves only for a working agent whose row it draws: a folded space
    /// hides its agents and arms nothing, the keyboard's own agent always draws, and
    /// the priority list folds nothing.
    #[test]
    fn only_a_drawn_working_agent_moves_the_column() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let agents = vec![
            agent(1, 1, "claude", "idle"),
            agent(2, 2, "codex", "working"),
        ];
        let (seen, unseen) = (Seen::new(), Unseen::new());
        let open = Shut::new();
        let folded = Shut::from([2]);
        let c = |shut, focused, view| {
            column(
                &spaces, &agents, 1, focused, 0, view, &seen, &unseen, NOW, WIDTH, shut,
            )
        };
        assert!(
            moves(&c(&open, 1, View::Tree)),
            "an open working agent moves"
        );
        assert!(
            !moves(&c(&folded, 1, View::Tree)),
            "a folded one moves nothing"
        );
        assert!(
            moves(&c(&folded, 2, View::Tree)),
            "the keyboard's own always draws"
        );
        assert!(
            moves(&c(&folded, 1, View::Priority)),
            "priority folds nothing"
        );
        let calm = vec![agent(1, 1, "claude", "idle")];
        let still = column(
            &spaces,
            &calm,
            1,
            1,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &open,
        );
        assert!(!moves(&still), "nothing working, nothing moving");
    }

    /// Past frame zero a working agent's activity carries the band and a waiting one's
    /// never does, and the strip turns the working agent's mark too.
    #[test]
    fn the_band_rides_working_rows_only_and_the_strip_spins() {
        let spaces = vec![space(1, "app")];
        let mut working = agent(1, 1, "claude", "working");
        working.activity = Some("Edit src/sidebar.rs".into());
        let mut blocked = agent(2, 1, "codex", "blocked");
        blocked.activity = Some("Bash bun test".into());
        let agents = vec![working.clone(), blocked.clone()];
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let moving = column(
            &spaces,
            &agents,
            1,
            9,
            SWEEP_TICKS / 2,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        let band = |a: &AgentRow| {
            second_row(&moving, a, BOX_LEAD + 2, None)
                .0
                .runs
                .iter()
                .filter(|(_, _, r)| matches!(r, crate::theme::Role::Shimmer(_)))
                .count()
        };
        assert!(band(&working) > 0, "the working activity has no band");
        assert_eq!(band(&blocked), 0, "a waiting agent swept");
        let strip = rows_collapsed(&moving, 10);
        assert!(
            strip
                .iter()
                .any(|(l, _)| l.text.contains(SPIN[(SWEEP_TICKS / 2 % 10) as usize])),
            "the strip did not spin: {strip:?}"
        );
    }

    /// Costs sit flush right on an agent's row and total in the title, only when asked for
    /// and only where the column is wide enough; the name gives way to them first.
    #[test]
    fn costs_ride_the_row_and_the_title_only_when_shown_and_wide_enough() {
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "idle"),
        ];
        let mut usage = Usage::new();
        usage.insert(
            1,
            Cost {
                cpu_percent: Some(4),
                rss: 212 << 20,
            },
        );
        usage.insert(
            2,
            Cost {
                cpu_percent: None,
                rss: 3 << 30,
            },
        );
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let at = |width| {
            let mut c = column(
                &spaces,
                &agents,
                1,
                9,
                0,
                View::Tree,
                &seen,
                &unseen,
                NOW,
                width,
                &shut,
            );
            c.usage = Some(&usage);
            rows_folded(&c, 12, None)
        };
        let wide = at(30);
        let row = tree_row(&wide, 1);
        assert!(row.text.ends_with(" 4% 212M  "), "{:?}", row.text);
        assert!(
            row.runs
                .iter()
                .any(|(_, _, r)| *r == crate::theme::Role::Faint)
        );
        assert!(
            tree_row(&wide, 2).text.ends_with(" 3.0G  "),
            "{:?}",
            tree_row(&wide, 2).text
        );
        let title = &wide.iter().find(|(_, h)| *h == Some(Hit::Title)).unwrap().0;
        assert!(title.text.ends_with("4% 3.2G tree +"), "{:?}", title.text);
        for (line, _) in &wide {
            assert_eq!(crate::width::width(&line.text), 30, "{line:?}");
        }
        let narrow = at(24);
        assert!(
            !tree_row(&narrow, 1).text.contains('%'),
            "costs on a narrow column"
        );
        assert_eq!(mem_text(1023 << 20), "1023M");
        assert_eq!(mem_text(1536 << 20), "1.5G");
    }

    /// A total too long for the title gives way: the title still says ` agents` whole,
    /// ends in the order word and the `+`, and is exactly the width.
    #[test]
    fn a_long_total_gives_way_to_the_title() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "claude", "working")];
        let mut usage = Usage::new();
        usage.insert(
            1,
            Cost {
                cpu_percent: Some(1234),
                rss: 1023 << 20,
            },
        );
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        for view in [View::Tree, View::Priority] {
            for width in USAGE_MIN_WIDTH..=SIDEBAR_MAX {
                let mut c = column(
                    &spaces, &agents, 1, 1, 0, view, &seen, &unseen, NOW, width, &shut,
                );
                c.usage = Some(&usage);
                let title = title_line(&c);
                assert!(
                    title.text.starts_with(" agents "),
                    "{width}: {:?}",
                    title.text
                );
                assert!(title.text.ends_with(" +"), "{width}: {:?}", title.text);
                assert_eq!(crate::width::width(&title.text), width);
            }
        }
    }

    /// The next row is one row at every height it fits, however many wait, so the tree
    /// under it never moves; past four marks it counts the rest. Too short for the title
    /// and a row of the list under it, the column drops it before anything else.
    #[test]
    fn the_next_row_is_one_row_at_every_height_and_counts_past_four() {
        let spaces = vec![space(1, "app")];
        let eight: Vec<AgentRow> = (1..=8).map(|t| agent(t, 1, "claude", "blocked")).collect();
        let rows = tree_rows(&spaces, &eight, 9, 40, &Seen::new(), &Shut::new());
        assert_eq!(next_of(&rows), Some(1));
        assert!(
            rows[0].0.text.trim_end().ends_with(concat!(
                "\u{282f}\u{2807}",
                "\u{282f}\u{2807}",
                "\u{282f}\u{2807}",
                "\u{282f}\u{2807}",
                "+3"
            )),
            "{:?}",
            rows[0].0
        );
        assert_eq!(order(&rows), (1..=8).collect::<Vec<u32>>());
        for h in 1..40 {
            let rows = tree_rows(&spaces, &eight, 9, h, &Seen::new(), &Shut::new());
            let above = rows
                .iter()
                .take_while(|(_, hit)| *hit != Some(Hit::Title))
                .count();
            // The foot takes its two rows; the next row needs three more above them.
            let want = usize::from(h > 4);
            if rows.iter().any(|(_, hit)| *hit == Some(Hit::Title)) {
                assert_eq!(above, want, "height {h}: {rows:?}");
            }
        }
    }

    /// The rank view keeps the one next row over its urgency-ordered list, on the same row
    /// as in the tree, so flipping the view moves nothing a person was about to click.
    #[test]
    fn the_next_row_stays_in_the_priority_view() {
        let (spaces, agents) = sample_waiting();
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
        assert_eq!(rows[0].1, Some(Hit::Next(Some(3))));
        assert_eq!(rows[1].1, Some(Hit::Title));
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
        assert!(app.text.starts_with("\u{2846}app (3)"), "{:?}", app.text);
        // The count sits beside the name it counts, and the block keeps its columns.
        assert!(app.text.contains("(3)"), "the count keeps every agent");
        assert_eq!(column_of(&app.text, 'z'), Some(WIDTH - 4), "{:?}", app.text);
        assert_eq!(column_of(&app.text, 'x'), Some(WIDTH - 2), "{:?}", app.text);

        // Off the space, the fold hides all three.
        let rows = tree_rows(&spaces, &agents, 4, 14, &Seen::new(), &shut);
        assert_eq!(order(&rows), vec![4]);
        // And the open space next to it is untouched.
        assert!(
            rule_row(&rows, 2).text.starts_with("\u{2846}dub"),
            "{:?}",
            rule_row(&rows, 2).text
        );
    }

    /// A fold never hides a blocked agent silently: a folded rule's line top and count
    /// take the colour of its most urgent child that needs a person, and stay quiet
    /// while every child is calm. Open, the line keeps the space's colour: the marks are
    /// right there.
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
            // The slot is the count in parens now, so the run is the parens and the digits
            // rather than the digits alone.
            let (_, _, count) = rule
                .runs
                .iter()
                .find(|(at, len, _)| {
                    let text = run_text(rule, *at, *len);
                    text.contains('(') && text.chars().any(|c| c.is_ascii_digit())
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
        // Nothing under it wants a person, so the line stays the space's own ink
        // whether the rule is folded or open; only the count keeps the quiet grey.
        assert_eq!(
            marks(&paint(&["idle", "working"], &shut)),
            (crate::theme::space_of(1), Role::Faint)
        );
        assert_eq!(
            marks(&paint(&["idle", "blocked"], &Shut::new())),
            (crate::theme::space_of(1), Role::Faint),
            "an open rule wears its own ink"
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
        assert_eq!(
            unbox(&tree_row(&rows, 1).text),
            "\u{2847}\u{2810}\u{2800}sh"
        );
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
            Some((BOX_LEAD, 2, crate::theme::of("done")))
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
            Some((DOT_AT, 2, crate::theme::Role::Faint))
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
            Some((BOX_LEAD, 2, crate::theme::Role::Faint))
        );
        assert_eq!(tree_row(&rows, 1).colour, Some(crate::theme::Role::Faint));
        // Focused, the same stale row is read, and reads at full strength.
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        assert_eq!(tree_row(&rows, 1).colour, None);
        assert_eq!(
            tree_row(&rows, 1).indicator,
            Some((BOX_LEAD, 2, crate::theme::of("idle")))
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
                .any(|l| l.indicator == Some((DOT_AT, 2, crate::theme::of("done")))),
            "a new row dimmed instead of keeping its sage: {:?}",
            texts(&lines)
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, 2, crate::theme::Role::Faint))),
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
                .all(|l| l.indicator != Some((DOT_AT, 2, crate::theme::Role::Faint))),
            "plain row already dim"
        );
        let after = paint(NOW + 2);
        assert!(
            after
                .iter()
                .any(|l| l.indicator == Some((DOT_AT, 2, crate::theme::Role::Faint))),
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
        assert_eq!(unbox(&rows[0]), "\u{2847}\u{2818}\u{2802}claude");
        // Cut sessions: the mark and the ellipsis frame the text.
        assert!(unbox(&rows[1]).ends_with('…'), "{:?}", rows[1]);
        assert!(
            unbox(&rows[1]).starts_with("\u{2847}\u{2818}\u{2802}Implement"),
            "{:?}",
            rows[1]
        );
        assert!(unbox(&rows[2]).ends_with('…'), "{:?}", rows[2]);
        // A session that already ends in an ellipsis is cut once, not twice.
        let mut dotty = agent(4, 1, "claude", "working");
        dotty.agent_session = Some("abcdefghijklmnop…".into());
        let rows = tree_rows(&spaces, &[dotty], 9, 20, &Seen::new(), &Shut::new());
        let row = tree_row(&rows, 4);
        assert_eq!(row.text.chars().filter(|c| *c == '…').count(), 1);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    /// The second row a calm agent with this cwd draws under a space of this name.
    fn worktree_row(space_name: &str, cwd: Option<&str>, program: &str) -> Line {
        let spaces = vec![space(1, space_name)];
        let mut row = agent(1, 1, program, "idle");
        row.cwd = cwd.map(str::to_string);
        second_of(
            &tree_rows(&spaces, &[row], 9, 10, &Seen::new(), &Shut::new()),
            1,
        )
        .clone()
    }

    /// A calm agent's second row shows the worktree basename after its tab when the
    /// space name does not already say it, and the edge cases keep the bare tab.
    #[test]
    fn the_row_shows_the_worktree_basename_when_different() {
        let row = |cwd| unbox(&worktree_row("app", cwd, "claude").text);
        // Different: the tab, then the worktree. The row is its space's only one, so
        // it is also the last and wears the line's foot.
        assert_eq!(
            row(Some("/home/u/app-feat-x")),
            "\u{28c7}  \u{2514} claude \u{b7} app-feat-x"
        );
        // Trailing slash is not a name.
        assert_eq!(
            row(Some("/home/u/app-feat-x/")),
            "\u{28c7}  \u{2514} claude \u{b7} app-feat-x"
        );
        // Same name in any case: the bare tab.
        assert_eq!(row(Some("/home/u/app")), "\u{28c7}  \u{2514} claude");
        assert_eq!(row(Some("/home/u/app")), "\u{28c7}  \u{2514} claude");
        // Root, empty, blank: no basename, the bare tab.
        assert_eq!(row(Some("/")), "\u{28c7}  \u{2514} claude");
        assert_eq!(row(Some("")), "\u{28c7}  \u{2514} claude");
        assert_eq!(row(Some("   ")), "\u{28c7}  \u{2514} claude");
        // No cwd at all: the bare tab.
        assert_eq!(row(None), "\u{28c7}  \u{2514} claude");
        // A tilde and dots are names and pass through untouched.
        assert_eq!(row(Some("~")), "\u{28c7}  \u{2514} claude \u{b7} ~");
        assert_eq!(row(Some("/home/u/.")), "\u{28c7}  \u{2514} claude \u{b7} .");
        // The row is quiet grey throughout; only the elbow and the dot wear the rule,
        // and the space's own ink frames the line beside it rather than a border that
        // is not drawn any more.
        let line = worktree_row("app", Some("/home/u/app-feat-x"), "claude");
        assert_eq!(line.colour, Some(crate::theme::Role::Faint));
        let rule = crate::theme::Role::Rule;
        let ink = crate::theme::space_of(1);
        assert_eq!(line.runs, vec![(3, 1, rule), (12, 1, rule), (0, 1, ink)]);
        assert_eq!(run_text(&line, 14, 10), "app-feat-x");
        assert_eq!(
            worktree_row("app", None, "claude").runs,
            vec![(3, 1, rule), (0, 1, ink)]
        );
    }

    /// The same worktree name under two spaces stays ambiguous on purpose: each agent
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
        let texts = unboxed(&lines);
        assert_eq!(texts[2], rule_row(&rows, 1).text.trim_end());
        assert_eq!(texts[3], "\u{2847}\u{2810}\u{2800}claude");
        assert_eq!(texts[4], "\u{28c7}  \u{2514} claude \u{b7} x");
        assert!(texts[5].is_empty(), "{texts:?}");
        assert_eq!(texts[6], rule_row(&rows, 2).text.trim_end());
        assert_eq!(texts[7], "\u{2847}\u{2810}\u{2800}codex");
        assert_eq!(texts[8], "\u{28c7}  \u{2514} codex \u{b7} x");
    }

    /// A long worktree shortens with an ellipsis while the tab keeps every letter it
    /// had; a tab too long to leave room shortens too, down to a floor, and the row
    /// still measures the column.
    #[test]
    fn a_long_worktree_shortens_and_the_tab_keeps_its_floor() {
        let line = worktree_row(
            "app",
            Some("/home/u/a-very-long-worktree-name-that-keeps-going"),
            "claude",
        );
        assert!(
            unbox(&line.text).starts_with("\u{28c7}  \u{2514} claude \u{b7} a-very"),
            "{:?}",
            line.text
        );
        assert!(line.text.contains('\u{2026}'), "{:?}", line.text);
        assert_eq!(crate::width::width(&line.text), WIDTH);

        // A tab so long it would push the worktree off: both shorten, neither vanishes.
        let line = worktree_row("app", Some("/home/u/somewhere"), &"c".repeat(40));
        assert!(line.text.contains("cccc"), "{:?}", line.text);
        assert!(line.text.contains(" \u{b7} some"), "{:?}", line.text);
        assert_eq!(crate::width::width(&line.text), WIDTH);
    }

    /// An overlong wide worktree shortens by columns, never splitting a wide
    /// character, and the row stays thirty columns.
    #[test]
    fn a_wide_worktree_shortens_by_columns() {
        let line = worktree_row("app", Some("/home/u/程程程程程程程程程程程程"), "claude");
        assert!(
            unbox(&line.text).starts_with("\u{28c7}  \u{2514} claude \u{b7} 程"),
            "{:?}",
            line.text
        );
        assert!(line.text.contains('\u{2026}'), "{:?}", line.text);
        assert_eq!(crate::width::width(&line.text), WIDTH);
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
        assert!(rule.text.contains("(1)"), "{:?}", rule.text);
        assert!(rule.text.contains('…'), "{:?}", rule.text);
        // The name gave way, not the block: the two controls keep their columns.
        assert_eq!(
            column_of(&rule.text, 'z'),
            Some(WIDTH - 4),
            "{:?}",
            rule.text
        );
        assert_eq!(
            column_of(&rule.text, 'x'),
            Some(WIDTH - 2),
            "{:?}",
            rule.text
        );

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
            unbox(&tree_row(&rows, 2).text).starts_with("\u{2847}\u{2818}\u{2802}behind docker"),
            "{:?}",
            tree_row(&rows, 2).text
        );
        assert!(
            unbox(&tree_row(&rows, 3).text).starts_with("\u{2847}\u{2818}\u{2802}Muse"),
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
    /// somebody waiting and nobody, in both orders: a short row leaves pixels behind and a
    /// long one spills into the terminal on the right.
    #[test]
    fn every_row_is_exactly_the_pulled_width_with_and_without_a_wait() {
        for (spaces, agents) in [sample(), sample_waiting()] {
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
                    assert!(
                        lines[0].text.starts_with(" nothing needs you")
                            || lines[0]
                                .text
                                .strip_prefix(' ')
                                .and_then(|t| t.chars().next())
                                .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
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
                lines.iter().any(|l| l.text.contains("\u{2846}app")),
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
        // In the tree it wears the mauve mark, and is never next.
        let rows = tree_rows(&[space(1, "app")], &[slept], 1, 10, &seen, &Shut::new());
        assert_eq!(
            tree_row(&rows, 7).indicator,
            Some((BOX_LEAD, 2, crate::theme::Role::Sleeping))
        );
        assert_eq!(next_of(&rows), None);
        // The sleeping mark is its own picture, never mistaken for done, blocked,
        // failed or idle at a glance.
        use crate::theme::Role;
        let sleeping = cell_mark(Role::Sleeping, false, 0);
        for role in [Role::Done, Role::Blocked, Role::Failed, Role::Idle] {
            assert_ne!(sleeping, cell_mark(role, false, 0), "{role:?}");
        }
    }

    /// A sleeping space is a box like any other with a `z` after its name and the count of
    /// what a wake brings back, agents only. Folded it draws no tab; unfolded its agent
    /// tabs are faint rows wearing the sleeping mark, each reaching its tab, and its arrow
    /// folds it like a live space's. A sleeping shell tab draws no row at all, the same as
    /// a live one.
    #[test]
    fn a_sleeping_space_draws_folded_with_its_mark_and_dim_tabs() {
        let mut asleep = space(2, "api");
        asleep.asleep = true;
        asleep.sleeping = vec![
            SleepingTab {
                terminal: 7,
                session: None,
                title: None,
                agent: Some("claude".into()),
                cwd: "/src/api".into(),
            },
            SleepingTab {
                terminal: 8,
                session: None,
                title: Some("logs".into()),
                agent: None,
                cwd: "/src/wt".into(),
            },
        ];
        let spaces = vec![space(1, "app"), asleep];
        let agents = vec![agent(1, 1, "claude", "idle")];
        let folded = tree_rows(&spaces, &agents, 1, 20, &Seen::new(), &Shut::from([2]));
        let head = rule_row(&folded, 2);
        // Folded, asleep, and saying so in the slot rather than in a mark after the name.
        assert!(
            head.text.contains("\u{2846}api sleeping"),
            "{:?}",
            head.text
        );
        assert!(!head.text.contains("api z"), "{:?}", head.text);

        assert_eq!(crate::width::width(&head.text), WIDTH);
        // The name and the rule wear the sleeping colour, and the toggle keeps its column.
        assert_eq!(
            head.accent.expect("the name's run").2,
            crate::theme::Role::Sleeping,
            "{:?}",
            head.runs
        );
        assert!(
            head.runs
                .contains(&(WIDTH - 4, 1, crate::theme::Role::Sleeping)),
            "{:?}",
            head.runs
        );
        assert!(!order(&folded).contains(&7), "folded, no tab draws");

        let open = tree_rows(&spaces, &agents, 1, 20, &Seen::new(), &Shut::new());
        // Terminal 8, the shell tab, never draws: the tree lists agents only.
        assert_eq!(order(&open), vec![1, 7]);
        let line = |t| {
            &open
                .iter()
                .find(|(_, h)| *h == Some(Hit::Terminal(t)))
                .expect("the tab draws")
                .0
        };
        let dim = line(7);
        assert_eq!(dim.colour, Some(crate::theme::Role::Faint));
        assert_eq!(
            dim.indicator,
            Some((BOX_LEAD, 2, crate::theme::Role::Sleeping)),
            "the sleeping mark, not a state dot"
        );
        assert_eq!(unbox(&dim.text), "\u{28c7}\u{2812}\u{2802}claude");

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
        assert_eq!(act(Hit::Space(2), NAME_AT, &c, false), Some(Act::Fold(2)));
        assert_eq!(act(Hit::Terminal(7), 9, &c, false), Some(Act::Terminal(7)));
    }

    /// A sleeping agent tab keeps the name its conversation had: a person's title wins,
    /// then the agent's own session name, and only then the bare agent.
    #[test]
    fn a_sleeping_tab_keeps_its_session_name() {
        let named = |title: Option<&str>, session: Option<&str>| {
            let mut asleep = space(2, "api");
            asleep.asleep = true;
            asleep.sleeping = vec![SleepingTab {
                terminal: 7,
                title: title.map(Into::into),
                session: session.map(Into::into),
                agent: Some("claude".into()),
                cwd: "/src/api".into(),
            }];
            let spaces = vec![space(1, "app"), asleep];
            let rows = tree_rows(&spaces, &[], 1, 20, &Seen::new(), &Shut::new());
            let line = rows
                .iter()
                .find(|(_, h)| *h == Some(Hit::Terminal(7)))
                .expect("the tab draws")
                .0
                .clone();
            unbox(&line.text)
        };
        assert!(named(None, Some("fix login")).contains("fix login"));
        assert!(named(Some("mine"), Some("fix login")).contains("mine"));
        assert!(named(None, None).contains("claude"));
    }

    /// The rule as the column paints it, at a width, for a space in a known state.
    fn rule_at(width: usize, name: &str, held: usize, asleep: bool, closable: bool) -> String {
        rule_asked_at(width, name, held, asleep, closable, false)
    }

    fn rule_asked_at(
        width: usize,
        name: &str,
        held: usize,
        asleep: bool,
        closable: bool,
        asking: bool,
    ) -> String {
        let space = WorkspaceRow {
            id: 1,
            name: name.to_string(),
            terminals: held,
            asleep,
            sleeping: if asleep {
                (0..held as u32)
                    .map(|terminal| SleepingTab {
                        terminal,
                        session: None,
                        title: None,
                        agent: Some("claude".into()),
                        cwd: "/tmp".into(),
                    })
                    .collect()
            } else {
                Vec::new()
            },
            closable,
        };
        let spaces = vec![space];
        let agents: Vec<AgentRow> = (1..=held as u32)
            .map(|n| agent(n, 1, "claude", "idle"))
            .collect();
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = column_armed(
            &spaces,
            &agents,
            1,
            1,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            width,
            &shut,
            asking.then_some(1),
        );
        // A height no list of test-sized tabs can overflow, so the rule is on screen
        // rather than scrolled: this is about how a rule is drawn, not about the window.
        rows_folded(&c, 4096, current_avatar().as_ref())
            .into_iter()
            .find(|(l, _)| l.text.starts_with('\u{2846}'))
            .map(|(l, _)| l.text)
            .unwrap_or_default()
    }

    /// `cols` columns of `text` from the zero-based column `at`, counting columns and not
    /// characters, so a full width name cannot shift what is read.
    #[allow(dead_code)]
    fn columns_from(text: &str, at: usize, cols: usize) -> String {
        let mut col = 0;
        let mut out = String::new();
        for c in text.chars() {
            let w = crate::width::columns(c);
            if col >= at && col < at + cols {
                out.push(c);
            }
            col += w;
        }
        out
    }

    /// The column of a character in a row, counting columns and not bytes, or `None` when
    /// the row does not carry it.
    fn column_of(text: &str, at: char) -> Option<usize> {
        let mut col = 0;
        for c in text.chars() {
            if c == at {
                return Some(col);
            }
            col += crate::width::columns(c);
        }
        None
    }

    /// Every rule this column can draw, at every width it can be drawn at, with a name
    /// and a count that stress the arithmetic: the block keeps its columns, the row is
    /// exactly the column, and the name gives way before the controls do.
    ///
    /// No room for the name is a column with no name in it, not a name that runs one
    /// column over. A narrow rule whose count has taken the whole budget would otherwise
    /// draw the ellipsis past it and push every control a column right of where `act`
    /// looks for it, so the painted `z` would be one column from the click that means it.
    #[test]
    fn a_name_with_no_room_is_left_out_rather_than_over_running() {
        assert_eq!(
            shorten_middle("qoro", 0),
            "",
            "an ellipsis is a column of its own"
        );
        assert_eq!(crate::width::width(&shorten_middle("qoro", 0)), 0);
        assert_eq!(shorten_middle("qoro", 1), "\u{2026}");
        assert_eq!(shorten_middle("qoro", 4), "qoro");
        assert_eq!(shorten_middle("qoro", 3), "qo\u{2026}");
        // What a name never gets on the narrowest column the interface draws: the fold
        // cells, the block and the blank last column. A count wide enough to take the
        // rest is what makes a rule sit on the budget, and `shorten_middle` is the shared
        // helper that has to be right about the case for its other callers too.
        assert_eq!(SIDEBAR_MIN.saturating_sub(BLOCK_TAIL), 11);
        assert_eq!(crate::width::width(&format!(" ({})", 999_999_usize)), 9);
    }

    /// Every rule this column can draw, at every width it can be drawn at, with a name
    /// and a count that stress the arithmetic: the block keeps its columns, the row is
    /// exactly the column, and the name gives way before the controls do.
    ///
    /// The sweep is the test. Three widths would only prove three of them.
    #[test]
    fn the_rule_holds_its_columns_at_every_width_and_count() {
        let names = [
            "q",
            "qoro",
            "dub",
            "shop-api",
            "shop-core",
            "shop-core-and-then-some-more-yes",
            // A name this column counts in columns and not characters, which is the
            // case a byte offset would get wrong.
            "\u{4e2d}\u{6587}",
            "\u{4e2d}\u{6587}\u{5b57}",
        ];
        let counts = [0usize, 1, 9, 10, 99, 1234];
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            for name in names {
                for held in counts {
                    for asleep in [false, true] {
                        for closable in [false, true] {
                            let row = rule_at(width, name, held, asleep, closable);
                            let label = format!("{width} {name} {held} {asleep} {closable}");
                            assert_eq!(
                                crate::width::width(&row),
                                width,
                                "not the column's width: {label} {row:?}"
                            );
                            // The sleep toggle, when the space can take one.
                            let sleep = if asleep || held > 0 {
                                Some(width - 4)
                            } else {
                                None
                            };
                            assert_eq!(
                                column_of(&row, 'z'),
                                sleep,
                                "the toggle moved: {label} {row:?}"
                            );
                            assert_eq!(
                                column_of(&row, 'x'),
                                closable.then_some(width - 2),
                                "the close moved: {label} {row:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The count sits beside the name it counts, in the space's own colour, and a sleeping
    /// space says so in words rather than in a mark after the name.
    #[test]
    fn a_rule_says_what_it_holds_beside_its_name() {
        let row = rule_at(WIDTH, "qoro", 4, false, true);
        assert!(row.contains("qoro (4)"), "{row:?}");
        // Asleep, the word replaces the count: they are the same fact, and the count of a
        // sleeping space is in the notice row anyway.
        let row = rule_at(WIDTH, "qoro", 3, true, true);
        assert!(row.contains("qoro sleeping"), "{row:?}");
        assert!(!row.contains("(3)"), "{row:?}");
        // Narrower than the interface ever draws, where the word no longer fits, the
        // count stays, so the name never starves for a state.
        let row = rule_at(16, "qoro", 3, true, true);
        assert!(row.contains("qoro (3)"), "{row:?}");
    }

    /// The session's own default carries no close, because the daemon refuses to close it
    /// and a control that cannot work is worse than no control.
    #[test]
    fn a_space_that_cannot_be_closed_says_so_by_drawing_no_close() {
        let row = rule_at(WIDTH, "default", 2, false, false);
        assert!(!row.contains('x'), "{row:?}");
        let row = rule_at(WIDTH, "dub", 0, false, true);
        assert!(row.contains('x'), "{row:?}");
    }

    /// The two controls answer in the columns the rule painted them in, at every width,
    /// which is what stops a click landing beside a control that moved.
    #[test]
    fn a_rule_answers_its_controls_where_it_painted_them() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            let c = clicks(&spaces, &agents, View::Tree, width, &none);
            let row = rows_folded(&c, 20, None)
                .into_iter()
                .find(|(_, h)| *h == Some(Hit::Space(1)))
                .map(|(l, _)| l.text)
                .expect("the rule");
            // Read the columns off the painted text rather than off the geometry, so this
            // fails if the paint and the hit test ever disagree.
            assert_eq!(
                column_of(&row, 'z'),
                Some(width - 4),
                "width {width}: {row:?}"
            );
            assert_eq!(
                column_of(&row, 'x'),
                Some(width - 2),
                "width {width}: {row:?}"
            );
            assert_eq!(
                act(Hit::Space(1), width - 4 + 1, &c, false),
                Some(Act::Sleep(1)),
                "width {width}"
            );
            assert_eq!(
                act(Hit::Space(1), width - 2 + 1, &c, false),
                Some(Act::Close(1)),
                "width {width}"
            );
            // One column either side of a control is the space, never the control.
            for at in [width - 5, width - 3, width - 1] {
                if (1..=width).contains(&(at + 1)) && at > NAME_AT {
                    assert_eq!(
                        act(Hit::Space(1), at + 1, &c, false),
                        Some(Act::Space(1)),
                        "column {at} of {width}"
                    );
                }
            }
        }
    }

    /// A space with nothing running in it draws no toggle, so there is no column that
    /// answers with a sleep the daemon would refuse.
    #[test]
    fn a_space_that_cannot_sleep_does_not_answer_with_a_sleep() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        for col in 1..=WIDTH {
            assert_ne!(
                act(Hit::Space(3), col, &c, false),
                Some(Act::Sleep(3)),
                "the empty space offered a sleep at column {col}"
            );
        }
    }

    /// An armed rule asks its question in the two columns its controls were, and answers
    /// nothing else: a row mid question does not fold, navigate or sleep.
    #[test]
    fn an_armed_rule_answers_yes_and_no_and_nothing_else() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.armed = Some(1);
        let row = rows_folded(&c, 20, None)
            .into_iter()
            .find(|(_, h)| *h == Some(Hit::Space(1)))
            .map(|(l, _)| l.text)
            .expect("the armed rule");
        assert!(row.contains("close?"), "{row:?}");
        // `y` in the cell `x` was in, because a second click where the question was
        // asked is the click a hand makes, and `n` in the cell `z` was in.
        assert_eq!(column_of(&row, 'n'), Some(WIDTH - 4), "{row:?}");
        assert_eq!(column_of(&row, 'y'), Some(WIDTH - 2), "{row:?}");
        assert_eq!(
            act(Hit::Space(1), WIDTH - 2 + 1, &c, false),
            Some(Act::ConfirmClose(1))
        );
        assert_eq!(
            act(Hit::Space(1), WIDTH - 4 + 1, &c, false),
            Some(Act::CancelClose)
        );
        // The line's top and the body of the row are not answers, they take the question
        // down: a row mid question neither folds nor navigates nor sleeps.
        assert_eq!(act(Hit::Space(1), 1, &c, false), Some(Act::CancelClose));
        assert_eq!(
            act(Hit::Space(1), NAME_AT, &c, false),
            Some(Act::CancelClose)
        );
        assert_eq!(act(Hit::Space(1), 8, &c, false), Some(Act::CancelClose));
        // And the close on another space is its own act, in the cell the close moved to.
        // Another space's own row is its own act, and the run loop is what takes this
        // question down for it: `act` answers about the row it was asked about.
        assert_eq!(
            act(Hit::Space(3), WIDTH - 2 + 1, &c, false),
            Some(Act::Close(3))
        );
    }

    /// The sleeping state is a colour as well as a word, so a narrow column that drops the
    /// word still says it.
    #[test]
    fn a_sleeping_space_keeps_the_only_way_back() {
        let row = rule_at(WIDTH, "qoro", 3, true, true);
        assert!(row.contains('z'), "{row:?}");
        // An awake space with nothing in it draws none, and the daemon would refuse.
        let row = rule_at(WIDTH, "dub", 0, false, true);
        assert!(!row.contains('z'), "{row:?}");
    }

    /// The sleeping state is a colour as well as a word, so the rule says it twice and a
    /// narrow column that has to drop one of them still says it.
    #[test]
    fn a_sleeping_rule_wears_the_sleeping_colour() {
        let spaces = vec![WorkspaceRow {
            id: 1,
            name: "qoro".into(),
            terminals: 3,
            asleep: true,
            sleeping: (0..3)
                .map(|terminal| SleepingTab {
                    terminal,
                    session: None,
                    title: None,
                    agent: Some("claude".into()),
                    cwd: "/tmp".into(),
                })
                .collect(),
            closable: true,
        }];
        let agents: Vec<AgentRow> = (1..=3).map(|n| agent(n, 1, "claude", "idle")).collect();
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
        let rule = rows_folded(&c, 40, current_avatar().as_ref())
            .into_iter()
            .find(|(l, _)| l.text.starts_with('\u{2846}'))
            .expect("the rule");
        let accent = rule.1.and(rule.0.accent).expect("the name's run");
        assert_eq!(accent.2, crate::theme::Role::Sleeping, "{:?}", rule.0);
    }
}

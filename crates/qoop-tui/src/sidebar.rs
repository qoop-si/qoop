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
//! marks selection. At frame zero, which is what a test, a pipe and `QOOP_MOTION=0` get,
//! the rows are the same static text every time.

use qoop_runtime::control::{AgentRow, SleepingTab, WorkspaceRow};
use qoop_runtime::paths::HubLink;

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
    /// row, in the colour of the state it speaks for. Selection is the raised background the painter adds, never a mark.
    pub indicator: Option<(usize, usize, crate::theme::Role)>,
    /// Any further runs: a space's count and controls, a quiet worktree name, a
    /// sweeping activity. The painter merges them with the two above by column; runs
    /// never overlap by construction. Still few per row: a row saying five things in
    /// five colours is a row a person parses rather than reads, so every run here is
    /// quieter than the row's own text or the same state colour the dot already wears.
    pub runs: Vec<(usize, usize, crate::theme::Role)>,
    /// A run filled with its role's ink rather than written in it: the inbox's chosen tab,
    /// the one word on the column that is a block of colour.
    pub fill: Option<(usize, usize, crate::theme::Role)>,
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
/// Thirty eight, dragged anywhere between [`SIDEBAR_MIN`] and half the window. Twenty
/// six is too narrow: this column carries a space name, a
/// separator, a mark, a name and a state word flush against the right edge, and at twenty
/// two the state had nothing left. Thirty left a real project name room to be read rather
/// than recognised from its first eight characters.
///
/// A pulled width reflows the pane beside it, which is the price the person
/// pulling asked to pay: explicit drags reflow, arrivals never do.
pub const WIDTH: usize = 38;

/// The narrowest the open column drags to: a waiting row and its count still read, and a
/// space rule keeps its controls and a name that is still a name.
pub const SIDEBAR_MIN: usize = 24;
const _: () = assert!(SIDEBAR_MIN >= BLOCK_TAIL_MAX + MIN_NAME_COLS);

/// The widest the open column drags to: half the window, so long names and places fit
/// when a person wants them to, and the pane always keeps the other half.
pub fn sidebar_max(cols: u16) -> usize {
    (cols as usize / 2).max(SIDEBAR_MIN)
}

/// The width the open column starts at in a window `cols` wide: [`WIDTH`], but never past
/// [`sidebar_max`], the same rule a resize and a drag keep.
pub fn opening_width(cols: u16) -> usize {
    WIDTH.min(sidebar_max(cols))
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
/// The columns before what a space holds: one blank, so an agent's mark sits under the
/// space's name and every row of the column starts in the same column.
const BOX_LEAD: usize = 1;

/// The column a space's name starts in, and the column every agent's mark starts in, so the
/// name lines up with the marks under it.
pub const NAME_AT: usize = 1;

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
/// The background dot: a hollow circle in the working ink, the same Geometric Shapes block
/// and the same font coverage as the filled one it sits beside.
const BACKGROUND_DOT: char = '\u{25cb}';
/// The column the state dot sits at: one leading space.
const DOT_AT: usize = 1;

/// Which mark a state dot wears: the diamond for failed, the circle for the rest.
///
/// Colour still rides the indicator beside the text; shape rides the text itself,
/// so the pair survives colour blindness and light terminals alike.
fn dot_glyph(role: crate::theme::Role) -> char {
    match role {
        crate::theme::Role::Failed => FAILED_DOT,
        crate::theme::Role::Background => BACKGROUND_DOT,
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
/// idle, a task still running in the background
///          one dot going round the ring, a step a frame, clockwise from the top left
/// ```
///
/// The dots that run while an agent works settle into its next state's picture, so the
/// mark says one thing in one language wherever it stands. The grid takes the braille
/// cell's top three rows and three of its four columns: the fourth column stays empty and
/// is the gap before the name that a single-cell dot needed a space for, so a row is as
/// wide as it was. Without braille (the kernel console, a dumb terminal) it is a plain
/// character and a space. Where the font draws braille the grid would break across its
/// two cells, so there the mark is [`one_cell_mark`].
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
            Role::Background => 'o',
            _ => '.',
        };
        return format!("{plain} ");
    }
    if !crate::colorlevel::braille_grid() {
        return one_cell_mark(role, working, tick);
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
        // A braille cell takes one ink, so the centre cannot stay grey beside a sky dot: the
        // dot goes round alone, which no other state is (the centre is never missing from
        // working's frames, and idle's dot is the centre).
        Role::Background => vec![RING[if working { (tick / 2 % 8) as usize } else { 0 }]],
        _ => vec![(1, 1)],
    };
    braille_pair(&dots)
}

/// The state mark where a picture cannot span two cells (iTerm2, Terminal.app and the
/// other terminals that draw braille from the font): a two by two grid on the middle dot
/// rows of one braille cell, then a space, the same two columns as the grid.
///
/// ```text
/// working  three dots, the gap running round clockwise
/// wants    a diagonal /       failed   a diagonal \
/// done     the full square    idle     the lower left dot
/// asleep   the lower line
/// idle, a task still running in the background
///          one dot going round the four, a step a frame, clockwise from the upper left
/// ```
///
/// One glyph, so the font spaces every dot of it. No working frame is another state's
/// picture.
fn one_cell_mark(role: crate::theme::Role, working: bool, tick: u64) -> String {
    use crate::theme::Role;
    const TL: u32 = 0x02;
    const TR: u32 = 0x10;
    const BL: u32 = 0x04;
    const BR: u32 = 0x20;
    // One step every two ticks, as the grid's breath.
    const TURN: [u32; 4] = [TL | TR | BL, TL | TR | BR, TR | BR | BL, TL | BR | BL];
    // The background dot's round, at the same pace. Its frame at the lower left is idle's
    // picture, which only the ink tells apart: sky against grey.
    const ORBIT: [u32; 4] = [TL, TR, BR, BL];
    let dots = match role {
        Role::Working if working => TURN[(tick / 2 % 4) as usize],
        Role::Working => TURN[0],
        Role::Blocked => TR | BL,
        Role::Failed => TL | BR,
        Role::Done => TL | TR | BL | BR,
        Role::Sleeping => BL | BR,
        Role::Background if working => ORBIT[(tick / 2 % 4) as usize],
        Role::Background => ORBIT[0],
        _ => BL,
    };
    format!("{} ", char::from_u32(0x2800 + dots).unwrap_or(' '))
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
/// column a click landed on decides which, through [`act`]: a space rule answers its
/// controls in their chips and folds anywhere else, the title flips the order at its word and
/// makes a space at its `+`, the foot's menu row opens the menu at its word and folds the
/// column anywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Space(i64),
    Terminal(u32),
    /// A row of the inbox's list: the agent it names, which a click reaches like its row in
    /// the tree, or nobody on a row with nobody on it. Never raised, so the keyboard's row is
    /// only ever drawn in one place.
    Next(Option<u32>),
    /// The inbox's tab row, or the one row it folds to: the column decides which tab.
    Tabs,
    /// The `spaces` title, with the order word and the `+` at its right.
    Title,
    /// The foot of the column: the brand, `menu`, and the mark that folds the column.
    Foot,
}

/// What a click on the column asks for, once the column it landed on is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// The version on the foot: open the site in the browser.
    Site,
    Space(i64),
    /// Fold a space's agents away under its rule, or open them again.
    Fold(i64),
    /// Put a space to sleep: the `sleep` on its rule, which reads the space from the row
    /// it was on rather than from whatever is on screen.
    Sleep(i64),
    /// Wake every sleeping tab of a space: the `wake` on its rule.
    Wake(i64),
    /// The `⋯` on a space's rule: that space's settings.
    Settings(i64),
    /// The cup on a space's rule: keep the space awake, or let idle sleep take it again.
    KeepAwake(i64),
    /// The toggle at the end of a space's rule: open its drawer of controls, or close it.
    Drawer(i64),
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
    /// The foot's `settings`: every setting on one page, as the menu's settings row opens it.
    SettingsPage,
    /// Fold the whole column away to its strip, and back.
    Collapse,
    /// A tab of the inbox: its list, from the top, and the inbox open again if it was folded.
    InboxTab(InboxTab),
    /// The inbox's caret: fold it to one row, or open it again.
    InboxFold,
}

/// Which list the inbox at the top of the column shows.
///
/// All is everything waiting on a person, the same agents the status line's `next:` and
/// `ctrl-a tab` walk; the next three split it by kind, and the last two are the rest of the
/// fleet, so every agent is one click from the top whatever it is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InboxTab {
    #[default]
    All,
    Wants,
    Failed,
    Done,
    Working,
    Asleep,
}

impl InboxTab {
    /// In the order the tab row draws them after the chosen one.
    pub const ORDER: [InboxTab; 6] = [
        InboxTab::All,
        InboxTab::Wants,
        InboxTab::Failed,
        InboxTab::Done,
        InboxTab::Working,
        InboxTab::Asleep,
    ];

    /// The word the chosen tab is spelled with.
    fn word(self) -> &'static str {
        match self {
            InboxTab::All => "inbox",
            InboxTab::Wants => "wants",
            InboxTab::Failed => "failed",
            InboxTab::Done => "done",
            InboxTab::Working => "working",
            InboxTab::Asleep => "asleep",
        }
    }

    /// The ink a tab wears: its state's, and the focused ink for all, which is no one state.
    fn role(self) -> crate::theme::Role {
        use crate::theme::Role;
        match self {
            InboxTab::All => Role::Focused,
            InboxTab::Wants => Role::Blocked,
            InboxTab::Failed => Role::Failed,
            InboxTab::Done => crate::theme::of("done"),
            InboxTab::Working => Role::Working,
            InboxTab::Asleep => Role::Sleeping,
        }
    }

    /// The tab before or after this one, wrapping: the wheel over the tab row walks them,
    /// which reaches a tab a narrow column had no room to draw.
    pub fn step(self, back: bool) -> InboxTab {
        let at = InboxTab::ORDER.iter().position(|t| *t == self).unwrap_or(0);
        let n = InboxTab::ORDER.len();
        InboxTab::ORDER[if back { (at + n - 1) % n } else { (at + 1) % n }]
    }

    /// Whether a row on this list says how long it has waited: only the lists of agents
    /// waiting on a person, where the wait is the news.
    fn timed(self) -> bool {
        matches!(
            self,
            InboxTab::All | InboxTab::Wants | InboxTab::Failed | InboxTab::Done
        )
    }
}

/// The inbox's list rows before anyone drags its edge.
pub const INBOX_ROWS: usize = 3;

/// The rows above the title of an open inbox at its first height: the tab row, its list
/// and the edge under it.
pub const TITLE_ROW: usize = 1 + INBOX_ROWS + 1;

/// The inbox at the top of the column, as this client left it.
///
/// Client-local like the column's own fold: the daemon never knows, and a second client
/// keeps its own. Its height changes only when the person drags or folds it, never because
/// news came or went, so the tree under it stays on the rows it was on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inbox {
    pub tab: InboxTab,
    /// List rows under the tab row, as dragged; the paint gives fewer when the column is short.
    pub rows: usize,
    pub folded: bool,
    /// The first list row the wheel moved to.
    pub scroll: usize,
}

impl Inbox {
    /// The list a click on `tab` shows: that one, or, clicked again where it is already
    /// open, the inbox, everything that waits.
    pub fn picked(&self, tab: InboxTab) -> InboxTab {
        if !self.folded && self.tab == tab {
            InboxTab::All
        } else {
            tab
        }
    }
}

impl Default for Inbox {
    fn default() -> Self {
        Inbox {
            tab: InboxTab::All,
            rows: INBOX_ROWS,
            folded: false,
            scroll: 0,
        }
    }
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

/// Costs by terminal. Empty unless `QOOP_USAGE=1`, and never holding another machine's
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
/// stay qoop's own; only the distinction is shared.
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
///
/// An idle agent with work still running in the background has not finished, so it is not
/// news yet: not in the done list, not in the queue, no sage. Its unseen entry stays, and
/// the moment the job ends the row is plain idle and becomes the news it is.
pub fn is_new(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> bool {
    if agent.terminal == focused || role_of(agent) == crate::theme::Role::Background {
        return false;
    }
    if is_unread(agent, focused, seen) {
        return true;
    }
    matches!(agent.state.as_str(), "idle" | "done")
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
    /// The first list row the wheel moved to, `None` while the list follows the keyboard.
    pub scroll: Option<usize>,
    /// The inbox at the top: which list, how tall, folded or not.
    pub inbox: Inbox,
    /// The space whose drawer of controls is out, and how many of its places: it slides out
    /// a place a frame. Every other rule's drawer is closed.
    pub drawer: Option<(i64, usize)>,
    /// Draw the icons of [`crate::icons`] rather than the plain glyphs. Set from
    /// [`crate::icons::on`] by the painter and the click mapper alike, so both lay a row out
    /// the same; tests say it outright.
    pub icons: bool,
}

/// One row of the column, and what a click on it means.
type Row = (Line, Option<Hit>);

/// Whether a row wants a person: blocked, failed, or finished while nobody looked.
///
/// One answer for every place that asks: the queue behind the next row, the colour of a
/// folded space's count, and the status line's triage.
pub fn needs_person(agent: &AgentRow, focused: u32, seen: &Seen, unseen: &Unseen) -> bool {
    matches!(agent.state.as_str(), "blocked" | "failed") || is_new(agent, focused, seen, unseen)
}

/// The most urgent row among `rows` that wants a person, the first one on a tie: blocked,
/// then failed, then finished while nobody looked. One answer for a folded space's count
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
    avatar: Option<&qoop_runtime::avatar::Avatar>,
) -> Vec<Row> {
    rows_folded_with(c, height, avatar, None)
}

/// [`rows_folded`] with a dragged row's landing rule opened before column row `rule.0`, in
/// the colour `rule.1`. The rule goes in after the window is cut, and the window's last row
/// makes room for it, so the list never scrolls under a held button and the foot stays put.
/// The rows a click maps onto are always [`rows_folded`]'s: the rule is paint only, so the
/// pointer can never chase its own rule.
pub(crate) fn rows_folded_with(
    c: &Column,
    height: usize,
    avatar: Option<&qoop_runtime::avatar::Avatar>,
    rule: Option<(usize, crate::theme::Role)>,
) -> Vec<Row> {
    if height == 0 {
        return Vec::new();
    }
    // The foot, with the pet in it, is taken first, so the list above is laid out in the
    // rows that are really its own and a scroll never keeps the focused row under the pet.
    let (head, feet, body) = parts(c, height, avatar);
    let mut list = window(content(c), body, c, c.width);
    if let Some((at, role)) = rule.filter(|(at, _)| *at >= head.len() && !list.is_empty()) {
        list.pop();
        let at = (at - head.len()).min(list.len());
        list.insert(at, (drop_rule(c.width, role), None));
    }
    let mut out = head;
    out.extend(list);
    out.extend(feet);
    out
}

/// The one-row rule a dragged row would land on: a thin line across the column.
fn drop_rule(width: usize, role: crate::theme::Role) -> Line {
    let text = format!(
        " \u{2576}{}\u{2574}",
        "\u{2500}".repeat(width.saturating_sub(4))
    );
    Line {
        text: crate::width::fit(&text, width),
        colour: Some(role),
        ..Line::default()
    }
}

/// What a dragged agent row reads as beside the pointer: its still mark, the role the mark
/// wears, and its session name, with how many panes ride along when it carries a split.
pub(crate) fn drag_chip(
    c: &Column,
    agent: &AgentRow,
    more: usize,
) -> (String, crate::theme::Role, String) {
    let (role, _) = dot_of(c, agent);
    let name = chip_name(&session_name(agent));
    let name = if more > 0 {
        format!("{name} (+{more})")
    } else {
        name
    };
    (cell_mark(role, false, 0), role, name)
}

/// A dragged row's name as its chip shows it, shortened in the middle.
pub(crate) fn chip_name(name: &str) -> String {
    shorten_middle(name, DRAG_CHIP_NAME)
}

/// The most columns a dragged row's name takes in its chip: enough to tell agents apart,
/// short enough that the chip does not cover the pane it is being dragged over.
const DRAG_CHIP_NAME: usize = 24;

/// The rows above the list, the foot rows kept, and how many rows the list gets between.
fn parts(
    c: &Column,
    height: usize,
    avatar: Option<&qoop_runtime::avatar::Avatar>,
) -> (Vec<Row>, Vec<Row>, usize) {
    // The foot, with the pet in it, is taken first, so the list above is laid out in the
    // rows that are really its own and a scroll never keeps the focused row under the pet.
    let stage = pet_shown(c, height, avatar.is_some()).then(|| inbox_items(c, InboxTab::All).len());
    // A column shorter than the foot keeps the foot's last rows: the menu row is the one
    // that must survive, and it is always the column's last row.
    let feet = foot_rows(c.width, stage, update_available().as_deref(), hub_link());
    let foot = feet.len().min(height);
    let room = height - foot;
    let mut head = inbox_rows(c, inbox_height(c, room));
    // The open inbox's edge, then the title: the edge first when both fit, so the inbox
    // reads as its own tray above the tree. Folded to its one row, it needs none.
    if head.len() > 1 && head.len() + 1 < room {
        head.push((tray_line(c.width), None));
    }
    if head.len() < room {
        head.push((title_line(c), Some(Hit::Title)));
    }
    let body = room - head.len();
    let skip = feet.len() - foot;
    (head, feet.into_iter().skip(skip).collect(), body)
}

/// The first list row after one turn of the wheel over the open column, `step` rows up or
/// down from where the list sits now, never past either end.
pub(crate) fn wheeled(
    c: &Column,
    height: usize,
    avatar: Option<&qoop_runtime::avatar::Avatar>,
    up: bool,
    step: usize,
) -> usize {
    if height == 0 {
        return 0;
    }
    let (_, _, body) = parts(c, height, avatar);
    let content = content(c);
    let at = first_row(&content, body, c);
    if up {
        at.saturating_sub(step)
    } else {
        (at + step).min(content.len().saturating_sub(body))
    }
}

/// Whether the pet stands in the foot, with the inbox count box above it: it fits the
/// column, and the whole list still fits above it. The pet is decoration and the list is
/// not: when the agents need the rows, the pet gives them up rather than push anyone off the
/// column.
/// The column and the painter ask this one question, so the art never lands on a row the
/// list was given.
pub fn pet_shown(c: &Column, height: usize, present: bool) -> bool {
    if !pet_drawn(present) || !avatar_fits(height, c.width, present) {
        return false;
    }
    let room = height - STAGE_FOOT_ROWS;
    inbox_height(c, room) + 2 + content(c).len() <= room
}

/// Whether a picked pet can be drawn at all: it is half blocks in its own inks, so without
/// colour there is nothing to draw it in.
fn pet_drawn(present: bool) -> bool {
    present && crate::colorlevel::level() != crate::colorlevel::Level::Mono
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

/// Everyone waiting on a person, in the order they are best seen to: everyone who wants
/// something, then everyone who failed, then everyone who finished unread, each kind in
/// daemon order. The agent the keyboard is on is left out: it is already in front of the
/// person, so a blocked agent being answered is not also waiting. The inbox's all list.
fn waiting<'a>(
    rows: &'a [AgentRow],
    focused: u32,
    seen: &Seen,
    unseen: &Unseen,
) -> Vec<(usize, &'a AgentRow)> {
    let mut out: Vec<(usize, &AgentRow)> = rows
        .iter()
        .enumerate()
        .filter(|(_, a)| a.agent && a.terminal != focused && needs_person(a, focused, seen, unseen))
        .collect();
    // The daemon's order breaks every tie, so nobody trades places.
    out.sort_by_key(|&(at, a)| (priority_of(a, focused, seen, unseen), at));
    out
}

/// Who needs a person, in the order they are best seen to: [`waiting`], walked on from the
/// keyboard. While the keyboard stands on a waiting agent the queue starts at the one after
/// it and wraps, so `ctrl-a tab` pressed again and again reaches every waiting agent instead
/// of trading between two that are still waiting. One answer for the key and the status
/// line's triage.
pub fn queue<'a>(
    rows: &'a [AgentRow],
    focused: u32,
    seen: &Seen,
    unseen: &Unseen,
) -> Vec<&'a AgentRow> {
    let key = |at: usize, a: &AgentRow| (priority_of(a, focused, seen, unseen), at);
    let mut queue = waiting(rows, focused, seen, unseen);
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

/// A waiting agent's state, as the inbox paints it. Only waiting agents reach here, so
/// anything not blocked or failed finished.
fn wait_role(agent: &AgentRow) -> crate::theme::Role {
    use crate::theme::Role;
    match agent.state.as_str() {
        "blocked" => Role::Blocked,
        "failed" => Role::Failed,
        _ => crate::theme::of("done"),
    }
}

/// An agent's state as a role. An idle agent with work still running in the background is
/// its own state here, derived from the row's count: the daemon's word stays `idle`, which
/// scripts wait on.
fn role_of(agent: &AgentRow) -> crate::theme::Role {
    if agent.state == "idle" && agent.background > 0 {
        crate::theme::Role::Background
    } else {
        crate::theme::of(&agent.state)
    }
}

/// The fewest columns an inbox row's name keeps before the row gives up the space name,
/// then the waited time.
const NAME_FLOOR: usize = 6;

/// What the all list says when nothing waits on the person.
const NOTHING: &str = "nothing needs you";

/// One row of an inbox list: who, in which state, where, and how long it has waited.
struct Item {
    terminal: u32,
    role: crate::theme::Role,
    name: String,
    space: i64,
    waited: Option<String>,
    /// What a calm agent still has running in the background, for the tail of its row.
    background: u32,
}

/// The rows of one of the inbox's lists, in the order it shows them.
fn inbox_items(c: &Column, tab: InboxTab) -> Vec<Item> {
    let timed = tab.timed();
    let agent_item = |a: &AgentRow, role| Item {
        terminal: a.terminal,
        role,
        name: session_name(a),
        space: a.workspace,
        waited: if timed { waited(c, a) } else { None },
        background: if matches!(a.state.as_str(), "idle" | "done") {
            a.background
        } else {
            0
        },
    };
    match tab {
        InboxTab::Working => c
            .agents
            .iter()
            .filter(|a| a.agent && matches!(a.state.as_str(), "working" | "starting"))
            .map(|a| agent_item(a, crate::theme::Role::Working))
            .collect(),
        // Sleeping tabs are the space's, not the daemon's agents: the ones with an agent in
        // them, as the tree lists them under their rule.
        InboxTab::Asleep => c
            .spaces
            .iter()
            .flat_map(|s| s.sleeping.iter().map(move |t| (s, t)))
            .filter(|(_, t)| t.agent.is_some())
            .map(|(s, t)| Item {
                terminal: t.terminal,
                role: crate::theme::Role::Sleeping,
                name: t.label().to_string(),
                space: s.id,
                waited: None,
                background: 0,
            })
            .collect(),
        _ => waiting(c.agents, c.focused_terminal, c.seen, c.unseen)
            .into_iter()
            .map(|(_, a)| a)
            .filter(|a| match tab {
                InboxTab::Wants => a.state == "blocked",
                InboxTab::Failed => a.state == "failed",
                InboxTab::Done => !matches!(a.state.as_str(), "blocked" | "failed"),
                _ => true,
            })
            .map(|a| agent_item(a, wait_role(a)))
            .collect(),
    }
}

/// A tab's mark in the tab row: the state mark, standing still, without its gap.
fn tab_mark(tab: InboxTab) -> String {
    cell_mark(tab.role(), false, 0).trim_end().to_string()
}

/// Where each piece of the tab row sits: the tab it picks (`None` for the caret), the
/// column it starts at, its width, and its text. One layout for the paint and for [`act`],
/// so a click lands on the tab a person aimed at by construction.
///
/// Open: the caret, the chosen tab spelled out and filled, `all` when it is not the chosen
/// one, then every other tab as its mark and count. Folded: the caret, `inbox`, and every
/// state as its mark and count. Too narrow for all of them, a zero count goes first, then
/// the last ones.
fn tab_cells(c: &Column, counts: &[usize; 6]) -> Vec<(Option<InboxTab>, usize, String)> {
    let count = |tab: InboxTab| counts[tab as usize];
    let chosen = c.inbox.tab;
    // The caret and the gap after it are one target, two columns wide.
    let caret = if c.inbox.folded {
        "\u{25b8} "
    } else {
        "\u{25be} "
    };
    let mut head: Vec<(Option<InboxTab>, String)> = vec![(None, caret.to_string())];
    let mut rest: Vec<(Option<InboxTab>, String, usize)> = Vec::new();
    if c.inbox.folded {
        head.push((None, "inbox".to_string()));
        for tab in &InboxTab::ORDER[1..] {
            let n = count(*tab);
            rest.push((Some(*tab), format!(" {}{n}", tab_mark(*tab)), n));
        }
    } else {
        // The chosen list in the fill: `inbox` for everything, or a state's mark and count,
        // the same cell it is when not chosen, so choosing one moves nothing but the fill.
        let cell = |tab: InboxTab| match tab {
            InboxTab::All => format!(" {} ", tab.word()),
            _ => format!(" {}{} ", tab_mark(tab), count(tab)),
        };
        head.push((Some(chosen), cell(chosen)));
        for tab in InboxTab::ORDER.iter().filter(|t| **t != chosen) {
            let n = count(*tab);
            let text = if *tab == InboxTab::All {
                cell(*tab)
            } else {
                format!(" {}{n}", tab_mark(*tab))
            };
            rest.push((Some(*tab), text, n));
        }
    }
    let used = |rest: &[(Option<InboxTab>, String, usize)]| -> usize {
        head.iter()
            .map(|(_, t)| crate::width::width(t))
            .chain(rest.iter().map(|(_, t, _)| crate::width::width(t)))
            .sum()
    };
    while used(&rest) > c.width && !rest.is_empty() {
        let at = rest
            .iter()
            .rposition(|(_, _, n)| *n == 0)
            .unwrap_or(rest.len() - 1);
        rest.remove(at);
    }
    let mut at = 0;
    head.into_iter()
        .chain(rest.into_iter().map(|(tab, text, _)| (tab, text)))
        .map(|(tab, text)| {
            let w = crate::width::width(&text);
            at += w;
            (tab, at - w, text)
        })
        .collect()
}

/// How many each of the inbox's lists holds, in [`InboxTab::ORDER`].
fn tab_counts(c: &Column) -> [usize; 6] {
    InboxTab::ORDER.map(|tab| inbox_items(c, tab).len())
}

/// The inbox's tab row, or the one row it folds to.
fn tab_row(c: &Column) -> Row {
    use crate::theme::Role;
    let counts = tab_counts(c);
    let cells = tab_cells(c, &counts);
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut fill = None;
    let any = counts[InboxTab::All as usize] > 0;
    for (i, (tab, at, cell)) in cells.iter().enumerate() {
        let w = crate::width::width(cell);
        text.push_str(cell);
        match tab {
            // The chosen tab, filled, the cell after the caret.
            Some(t) if !c.inbox.folded && i == 1 => fill = Some((*at, w, t.role())),
            // Every list keeps its state's ink, empty or not, chosen one or another: a row
            // of grey marks would say nothing about which is which.
            Some(t) => {
                runs.push((*at, w, t.role()));
            }
            // `inbox` on the folded row says whether anything waits under it.
            None if c.inbox.folded && i == 1 => {
                runs.push((*at, w, if any { Role::Blocked } else { Role::Title }));
            }
            None => runs.push((*at, w, Role::Faint)),
        }
    }
    let line = Line {
        text: fit(&text, c.width),
        colour: Some(Role::Faint),
        runs,
        fill,
        ..Line::default()
    };
    (line, Some(Hit::Tabs))
}

/// One list row: the mark, the name in its space's ink, and on the right the space and the
/// wait in the faint ink. The last column is left for the scrollbar.
fn item_row(c: &Column, item: &Item) -> Line {
    use crate::theme::Role;
    let space = c
        .spaces
        .iter()
        .find(|s| s.id == item.space)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let mark = cell_mark(item.role, false, 0);
    let lead = 1 + crate::width::width(&mark);
    // The right side gives way before the name: the space goes, then the wait.
    let mut right: Vec<String> = [
        Some(space),
        item.waited.clone(),
        (item.background > 0).then(|| format!("bg:{}", item.background)),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    let right_w = |right: &[String]| -> usize {
        let w: usize = right.iter().map(|s| crate::width::width(s)).sum();
        w + right.len().saturating_sub(1) + usize::from(!right.is_empty())
    };
    while !right.is_empty() && lead + NAME_FLOOR + right_w(&right) + 1 > c.width {
        right.remove(0);
    }
    let room = c.width.saturating_sub(lead + right_w(&right) + 1);
    let name = shorten_middle(&item.name, room);
    let mut text = format!(" {mark}{name}");
    let mut runs = vec![(1, crate::width::width(&mark), item.role)];
    runs.push((
        lead,
        crate::width::width(&name),
        crate::theme::space_in(c.spaces, item.space),
    ));
    let tail = right.join(" ");
    let gap = c
        .width
        .saturating_sub(crate::width::width(&text) + crate::width::width(&tail) + 1);
    text.push_str(&" ".repeat(gap));
    runs.push((
        crate::width::width(&text),
        crate::width::width(&tail),
        Role::Faint,
    ));
    text.push_str(&tail);
    Line {
        text: fit(&text, c.width),
        colour: Some(Role::Faint),
        runs,
        ..Line::default()
    }
}

/// How many rows the inbox takes at the top of a column with `room` rows above the foot:
/// none when the title and a row of the tree would not fit under its tab row, one when
/// folded, else the tab row and as many list rows as were dragged that leave the title and
/// one tree row. Never more because news came, so the tree stays where it was.
fn inbox_height(c: &Column, room: usize) -> usize {
    if room < 3 {
        return 0;
    }
    if c.inbox.folded {
        return 1;
    }
    // What is left once its edge, the title and one row of the tree are paid for.
    1 + c.inbox.rows.max(1).min(room.saturating_sub(4).max(1))
}

/// The edge under the inbox: the column's own ember corners, turning up at each end, and its
/// rule between them, one column in from each side. Not the panel rule the column's line
/// joins: the corners close it on their own.
fn tray_line(width: usize) -> Line {
    use crate::theme::Role;
    let inner = width.saturating_sub(4);
    Line {
        text: fit(
            &format!(" \u{2570}{}\u{256f} ", "\u{2500}".repeat(inner)),
            width,
        ),
        colour: None,
        runs: vec![
            (1, 1, Role::Brand),
            (2, inner, Role::Rule),
            (2 + inner, 1, Role::Brand),
        ],
        ..Line::default()
    }
}

/// The rows above the foot in a column of `height` rows: the inbox, the title and the list.
fn room_of(c: &Column, height: usize, avatar: Option<&qoop_runtime::avatar::Avatar>) -> usize {
    let stage = pet_shown(c, height, avatar.is_some()).then_some(0);
    height
        .saturating_sub(foot_rows(c.width, stage, update_available().as_deref(), hub_link()).len())
}

/// The most list rows a drag may give the inbox in a column of `height` rows: what leaves
/// its edge, the title and one row of the tree. Zero when the inbox has no list rows to drag.
pub(crate) fn inbox_room(
    c: &Column,
    height: usize,
    avatar: Option<&qoop_runtime::avatar::Avatar>,
) -> usize {
    room_of(c, height, avatar).saturating_sub(4)
}

/// The inbox's first list row after one notch of the wheel over its list, never past
/// either end.
pub(crate) fn inbox_wheeled(
    c: &Column,
    height: usize,
    avatar: Option<&qoop_runtime::avatar::Avatar>,
    up: bool,
) -> usize {
    let rows = inbox_height(c, room_of(c, height, avatar)).saturating_sub(1);
    let last = inbox_items(c, c.inbox.tab).len().saturating_sub(rows);
    let at = c.inbox.scroll.min(last);
    if up {
        at.saturating_sub(1)
    } else {
        (at + 1).min(last)
    }
}

/// The inbox's rows: the tab row, then `height - 1` list rows.
fn inbox_rows(c: &Column, height: usize) -> Vec<Row> {
    use crate::theme::Role;
    if height == 0 {
        return Vec::new();
    }
    let mut out = vec![tab_row(c)];
    let rows = height - 1;
    if rows == 0 {
        return out;
    }
    let items = inbox_items(c, c.inbox.tab);
    let at = c.inbox.scroll.min(items.len().saturating_sub(rows));
    if items.is_empty() {
        // Nothing on this list. On all, nothing waits on the person at all, which is worth
        // saying in words; any other list just has nobody on it.
        let indent = 1;
        let said = if c.inbox.tab == InboxTab::All {
            NOTHING
        } else {
            "none"
        };
        let mut first = Line {
            text: fit(&format!("{}{said}", " ".repeat(indent)), c.width),
            colour: Some(Role::Faint),
            ..Line::default()
        };
        if c.inbox.tab == InboxTab::All {
            first.runs.push((indent, said.len(), Role::Focused));
        }
        out.push((first, Some(Hit::Next(None))));
    }
    for item in items.iter().skip(at).take(rows) {
        out.push((item_row(c, item), Some(Hit::Next(Some(item.terminal)))));
    }
    while out.len() < height {
        out.push((blank(c.width).0, Some(Hit::Next(None))));
    }
    // A thumb on a track in the last column, only when the list is longer than its rows.
    if items.len() > rows {
        // Shorter than the track, so the track always shows there is more; and at the far
        // end when the list is scrolled to its end.
        let size = (rows * rows / items.len()).clamp(1, rows.saturating_sub(1).max(1));
        let last = items.len() - rows;
        let from = (at * (rows - size)).div_ceil(last).min(rows - size);
        let thumb = c.inbox.tab.role();
        for (i, (line, _)) in out.iter_mut().skip(1).enumerate() {
            let on = (from..from + size).contains(&i);
            let mut cells: Vec<char> = line.text.chars().collect();
            let last = c.width.saturating_sub(1);
            if crate::width::width(&line.text) == c.width && cells.last() == Some(&' ') {
                cells.pop();
                line.text = cells.into_iter().collect();
                line.text.push(if on { '\u{2503}' } else { '\u{2502}' });
                line.runs
                    .push((last, 1, if on { thumb } else { Role::Rule }));
            }
        }
    }
    out
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
                // The rows are laid out for the space's inside, one column in from each edge.
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

/// A space's own row: whether it folds, its name and what it holds beside the name, and
/// four controls in fixed columns at the right, bare words one blank apart.
///
/// ```text
/// ▾qoop (4)                + sleep ⋯ x
/// ▸wend (1)               + wake  ⋯ x
/// ```
///
/// No line or box marks the space out: the name wears the space's colour and so do the titles
/// of the agents under it (see [`tree_line`]), so a space reads as one block by hue alone.
/// The name starts in the column the agents' marks start in. The column before it says
/// whether the space is open or folded, in the faint ink so it stays quiet beside the
/// coloured name, and only on a space that has something to fold; a click anywhere on the
/// rule off its controls folds, as does `ctrl-a f`.
///
/// The name and the count beside it share one slot: the count while the space is awake,
/// the word `sleeping` while it is not, and the word gives way before the name does,
/// because a name is identity and a state is not. A sleeping space keeps its own colour,
/// one step quieter, the way a calm agent does: the word `sleeping` and the `wake` control
/// carry the state, and the space is still told apart from the others asleep beside it.
///
/// A space of another machine is named `host:name`, as the interface lists it; its link is
/// in the settings box.
///
/// The controls are [`Block`], in fixed columns at the right, always in the fold's faint
/// grey. Folded, the count takes the colour of the most urgent agent under it when that
/// agent wants a person, so a fold never hides a blocked agent silently. Open, the count
/// stays grey: the agents are right there, wearing their own marks. Only agents colour it:
/// a shell never wants a person.
fn rule(c: &Column, space: &WorkspaceRow, kids: &[&AgentRow], open: bool) -> Line {
    use crate::theme::Role;
    let width = c.width;
    let ink = if space.asleep {
        crate::theme::space_calm_in(c.spaces, space.id)
    } else {
        crate::theme::space_in(c.spaces, space.id)
    };
    let agents: Vec<&AgentRow> = kids.iter().copied().filter(|a| a.agent).collect();
    let hot = if open { None } else { urgent_ink(c, &agents) };
    let mark = hot.unwrap_or(Role::Faint);
    // One per tab, however many panes it slept with.
    let dozing = space
        .sleeping
        .iter()
        .filter(|t| t.agent.is_some() && t.lead.is_none_or(|lead| lead == t.terminal))
        .count();
    let held = if space.asleep { dozing } else { kids.len() };
    let armed = c.armed == Some(space.id);
    // Armed, the row is a question, and the controls become its two answers in the places
    // they were in: a question is asked where it was asked from (see `layout`).
    let block = block(c, space);
    // The left part ends a blank before the block, so nothing on it can push a control out
    // of its columns, and what is painted on it is cut where it is.
    let left_w = block.start(width).saturating_sub(1);
    // One slot after the name, and only one: the count. A sleeping space says it sleeps
    // through its `wake` control and its colour, so the slot carries no word for it. The
    // slot carries its own leading space, so the name and the slot never touch. Each holds
    // only while the name keeps its columns, longest first: the count with the sleeping
    // part, then the bare count, then nothing.
    let slots: Vec<String> = if armed {
        // The question is in the block, beside its `y` (see `layout`): the name keeps
        // whatever room is left and the count steps aside while it asks.
        vec![String::new()]
    } else {
        let mut slots = Vec::new();
        if held > 0 {
            if !space.asleep && dozing > 0 {
                // The zzz an asleep tab wears, or a `z` where icons are not drawn.
                let zzz = if c.icons { crate::icons::ASLEEP } else { 'z' };
                slots.push(format!(" ({held}, {dozing}{zzz})"));
            }
            slots.push(format!(" ({held})"));
        }
        slots.push(String::new());
        slots
    };
    // A space of another machine wears a dot before its name, in the colour of its link
    // (green connected, dim parked, warm when it does not answer), and the remote mark
    // before the dot where icons are drawn; not while it asks, when the question needs the
    // room.
    let remote = (!armed)
        .then_some(space.link)
        .flatten()
        .map(|link| match link {
            qoop_runtime::control::LinkState::Up => Role::Done,
            qoop_runtime::control::LinkState::Parked => Role::Faint,
            _ => Role::Blocked,
        });
    let glyph = if c.icons { 2 } else { 0 };
    let lead = if remote.is_some() { glyph + 2 } else { 0 };
    // The columns a name has once the fold, the mark and the slot are paid for.
    let room = |slot: &str| {
        left_w
            .saturating_sub(NAME_AT + lead)
            .saturating_sub(crate::width::width(slot))
    };
    let slot = slots
        .iter()
        .find(|slot| room(slot) >= MIN_NAME_COLS)
        .or(slots.last())
        .cloned()
        .unwrap_or_default();
    let slot_w = crate::width::width(&slot);
    let name = shorten_middle(&space.name, room(&slot));
    let name_w = crate::width::width(&name);
    let fold = if folds(c, space.id) {
        if open { "\u{25be}" } else { "\u{25b8}" }
    } else {
        " "
    };
    let mut runs = Vec::new();
    if fold != " " {
        runs.push((0, 1, Role::Faint));
    }
    if let Some(role) = remote {
        if c.icons {
            runs.push((NAME_AT, 1, role));
        }
        runs.push((NAME_AT + glyph, 1, role));
    }
    if !slot.is_empty() {
        runs.push((
            NAME_AT + lead + name_w,
            slot_w,
            if space.asleep { Role::Sleeping } else { mark },
        ));
    }
    let clip = |(at, w, role): (usize, usize, Role)| {
        (at < left_w && w > 0).then(|| (at, w.min(left_w - at), role))
    };
    runs = runs.into_iter().filter_map(clip).collect();
    let accent = clip((NAME_AT + lead, name_w, ink));
    let prefix = match (remote.is_some(), c.icons) {
        (true, true) => format!("{} {ONLINE} ", crate::icons::REMOTE),
        (true, false) => format!("{ONLINE} "),
        (false, _) => String::new(),
    };
    let left = fit(&format!("{fold}{prefix}{name}{slot}"), left_w);
    // The cells of the block, all in the fold's faint grey: controls are furniture, the
    // name and its slot carry the colour. The cup is the one that says something: full, in
    // the brand ink, while the space is kept awake. The two answers of a question are
    // deliberately not the same weight: `y` is the word, `no` is quieter.
    for (ctl, at, w) in &block.cells {
        let role = match ctl {
            Ctl::Yes | Ctl::Ask => Role::Text,
            Ctl::Cup if block.kept => Role::Brand,
            _ => Role::Faint,
        };
        runs.push((*at, *w, role));
    }
    let text = format!("{left} {}", block.text(width));
    Line {
        fill: None,
        text: fit(&text, width),
        colour: None,
        accent,
        indicator: None,
        runs,
    }
}

/// A control's cells: `word`, padded after to `inner` columns. Text only, nothing drawn
/// round it.
fn chip(word: &str, inner: usize) -> String {
    let pad = inner.saturating_sub(crate::width::width(word));
    format!("{word}{}", " ".repeat(pad))
}

/// The machine a remote space's name says, and the name without it.
///
/// The machine is the `host:` the interface put before the name. A machine that never
/// answered has a row named after the machine itself, with no prefix, so the whole name is
/// the machine. A prefix holding a space is not a machine but a reason in brackets that
/// happens to hold a colon.
pub(crate) fn machine_of(name: &str) -> (&str, &str) {
    name.split_once(':')
        .filter(|(host, _)| !host.is_empty() && !host.contains(' '))
        .unwrap_or((name, name))
}

/// Whether a space has anything to fold away under its rule: an agent, or a sleeping
/// agent. A sleeping shell draws no row, so it is nothing to fold.
fn folds(c: &Column, id: i64) -> bool {
    c.agents.iter().any(|a| a.agent && a.workspace == id)
        || c.spaces
            .iter()
            .any(|s| s.id == id && s.sleeping.iter().any(|t| t.agent.is_some()))
}

/// Whether anything in a space runs: any tab, a shell as much as an agent, which is what
/// the daemon's `SleepWorkspace` puts to sleep. One answer for the paint and the click, so
/// a space never sleeps from a cell that drew nothing.
fn busy(c: &Column, space: &WorkspaceRow) -> bool {
    !space.asleep
        && c.agents
            .iter()
            .any(|a| a.workspace == space.id && a.state != "sleeping")
}

/// What an armed rule says instead of its name's count: the question, and nothing else.
/// The space it is about is on the row, and the notice row carries the count and the
/// reason, so the rule itself only has to ask.
const QUESTION: &str = " close?";

/// What a name must keep to stay a name. A space whose row cannot hold this many columns
/// for the name is a row that has said its name is not the most important thing on it.
const MIN_NAME_COLS: usize = 3;

/// The columns of the moon without icons: `sleep`, and `wake` in the same width.
const WORD_W: usize = 5;

/// One control on a space's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctl {
    /// A new tab or agent in the space.
    New,
    /// Put the space to sleep: the moon, empty.
    Sleep,
    /// Wake its sleeping tabs: the moon, full.
    Wake,
    /// Keep it out of idle sleep: the cup, full while it is kept.
    Cup,
    /// Its settings.
    Settings,
    /// Close it, which only asks first.
    Close,
    /// Open or close the drawer the controls above live in.
    Toggle,
    /// The answers of an armed rule.
    Yes,
    No,
    /// The question of an armed rule, beside its `y`: words, not a control.
    Ask,
}

/// How many of the drawer's places there are, nearest the toggle first: the moon, the cup,
/// settings, a blank that sets the close apart, and the close.
pub const DRAWER: usize = 5;

/// The controls at the right of a space's rule, each with its zero-based column and width.
///
/// Laid out from the right edge in, the last column left blank: the toggle, then `+`, so
/// the two a hand reaches for most never move. Closed, that is the whole of it, with the
/// cup and the full moon peeking left of `+` while the space is kept awake or asleep, so a
/// state is never hidden in the drawer. Open, the drawer slides out left of `+` a place at a
/// time, nearest the pointer first by how often a hand needs it: the moon, the cup,
/// settings, then a blank and the close, as far from the pointer as the row allows. The
/// moon is there only when it can work: something runs to put to sleep, or something sleeps
/// to wake; otherwise its place stays blank, so nothing after it moves.
///
/// With icons each control is one column; without, the moon is a word as wide as `sleep`
/// and settings is `set`. An armed rule is laid out open and answers in it: `y` in the
/// close's place, where the click that asked was, and `no` over `+` and the toggle.
struct Block {
    cells: Vec<(Ctl, usize, usize)>,
    icons: bool,
    /// The space is kept awake: the cup full.
    kept: bool,
    /// The drawer is open, or opening: the toggle says how to put it back.
    open: bool,
}

impl Block {
    /// The first column of the block: the row's left part ends a blank before it.
    fn start(&self, width: usize) -> usize {
        self.cells
            .iter()
            .map(|(_, at, _)| *at)
            .min()
            .unwrap_or(width.saturating_sub(1))
    }

    /// The control at a zero-based column, if one is drawn there.
    fn at(&self, col: usize) -> Option<Ctl> {
        self.cells
            .iter()
            .find(|(_, at, w)| (*at..at + w).contains(&col))
            .map(|(ctl, _, _)| *ctl)
    }

    /// What a control says.
    fn word(&self, ctl: Ctl) -> String {
        let icon = |glyph: char, plain: &str| {
            if self.icons {
                glyph.to_string()
            } else {
                plain.to_string()
            }
        };
        match ctl {
            Ctl::New => "+".into(),
            Ctl::Sleep => icon(crate::icons::SLEEP, "sleep"),
            Ctl::Wake => icon(crate::icons::WAKE, "wake"),
            Ctl::Cup => icon(crate::icons::CUP, "c"),
            Ctl::Settings => icon(crate::icons::SETTINGS, "set"),
            Ctl::Close => "x".into(),
            Ctl::Toggle if self.open => icon(crate::icons::LESS, "\u{203a}"),
            Ctl::Toggle => icon(crate::icons::MORE, "\u{22ef}"),
            Ctl::Yes => "y".into(),
            Ctl::No => "no".into(),
            Ctl::Ask => QUESTION.trim_start().into(),
        }
    }

    /// The columns from [`Block::start`] to the row's end, the last one blank.
    fn text(&self, width: usize) -> String {
        let start = self.start(width);
        let mut out = String::new();
        let mut col = start;
        while col < width {
            match self.cells.iter().find(|(_, at, _)| *at == col) {
                Some((ctl, _, w)) => {
                    out.push_str(&chip(&self.word(*ctl), *w));
                    col += w;
                }
                None => {
                    out.push(' ');
                    col += 1;
                }
            }
        }
        out
    }
}

/// The block of a space's rule `width` wide, with `shown` places of the drawer out.
fn layout(
    width: usize,
    icons: bool,
    space: &WorkspaceRow,
    busy: bool,
    shown: usize,
    armed: bool,
) -> Block {
    let wakes = space.asleep && !space.sleeping.is_empty();
    let moon_w = if icons { 1 } else { WORD_W };
    let mut end = width.saturating_sub(1);
    let mut cells = Vec::new();
    // Each control pays for the blank after it.
    let mut place = |ctl: Option<Ctl>, w: usize, cells: &mut Vec<(Ctl, usize, usize)>| {
        end = end.saturating_sub(w);
        let at = end;
        end = end.saturating_sub(1);
        if let Some(ctl) = ctl {
            cells.push((ctl, at, w));
        }
        at
    };
    let toggle = place(Some(Ctl::Toggle), 1, &mut cells);
    let new = place(Some(Ctl::New), 1, &mut cells);
    let shown = if armed { DRAWER } else { shown.min(DRAWER) };
    if shown > 0 {
        let moon = if wakes {
            Some(Ctl::Wake)
        } else {
            busy.then_some(Ctl::Sleep)
        };
        let settings_w = if icons { 1 } else { 3 };
        let drawer = [
            (moon, moon_w),
            (Some(Ctl::Cup), 1),
            (Some(Ctl::Settings), settings_w),
            (None, 0),
            (space.closable.then_some(Ctl::Close), 1),
        ];
        // The blank before the close is a place of no width: its own blank is the gap.
        for (ctl, w) in drawer.into_iter().take(shown) {
            place(ctl, w, &mut cells);
        }
    } else {
        if wakes {
            place(Some(Ctl::Wake), moon_w, &mut cells);
        }
        if space.keep_awake {
            place(Some(Ctl::Cup), 1, &mut cells);
        }
    }
    if armed {
        // The question in the open layout: `y` where the close is, `no` where `+` and the
        // toggle are, and nothing else, so nothing but the answer is under the pointer.
        let yes = cells
            .iter()
            .find(|(ctl, _, _)| *ctl == Ctl::Close)
            .map(|(_, at, _)| *at);
        cells = yes.map(|at| (Ctl::Yes, at, 1)).into_iter().collect();
        // The question right after `y`, in the room the drawer leaves between the close and
        // `+`, so a column too narrow for the name still asks it whole.
        if let Some(at) = yes {
            let ask = QUESTION.trim_start().len();
            if at + 2 + ask < new {
                cells.push((Ctl::Ask, at + 2, ask));
            }
        }
        cells.push((Ctl::No, new, toggle + 1 - new));
    }
    Block {
        cells,
        icons,
        kept: space.keep_awake,
        open: shown > 0,
    }
}

/// The block of a space's rule as the column has it now.
fn block(c: &Column, space: &WorkspaceRow) -> Block {
    let shown = c
        .drawer
        .filter(|(id, _)| *id == space.id)
        .map_or(0, |(_, shown)| shown);
    layout(
        c.width,
        c.icons,
        space,
        busy(c, space),
        shown,
        c.armed == Some(space.id),
    )
}

/// The widest a closed rule's block is: plain words, a space asleep and kept awake, with the
/// blank before it and the one after.
const CLOSED_BLOCK_MAX: usize = 1 + (WORD_W + 1) + (1 + 1) + (1 + 1) + (1 + 1) + 1;

/// What a closed rule pays for besides its name: the fold column and that block.
const BLOCK_TAIL_MAX: usize = NAME_AT + CLOSED_BLOCK_MAX;

/// A row of a space: `line`, laid out for the space's inside, one column in from each
/// edge, so its mark lines up under the space's name.
fn boxed(line: Line, width: usize) -> Line {
    let shift = |(at, w, role)| (at + BOX_LEAD, w, role);
    let runs = line.runs.into_iter().map(shift).collect();
    let inside = fit(&line.text, box_inside(width));
    Line {
        fill: None,
        text: fit(&format!(" {inside} "), width),
        colour: line.colour,
        accent: line.accent.map(shift),
        indicator: line.indicator.map(shift),
        runs,
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

/// An agent's second row: what it is doing now or, while it rests, which tab it is and
/// where it runs.
///
/// Every agent has one, so a row never grows or shrinks under the eye as an agent starts
/// and stops. While the daemon reports an activity (working, starting, waiting, failed)
/// the row is that activity alone, in a person's words ([`humanize`]): quietly while it
/// works, what it wants in the blocked colour, and the failure in the failed colour. The
/// tab and the place are what a person reads to find a resting agent, and beside a live
/// activity they were two words of noise cutting it short. At rest the tab a person
/// named (or the program, when nobody did) comes back, followed by the directory it runs
/// in when that says something the space name does not. The row reaches the same
/// terminal as the name above it.
fn second_row(c: &Column, agent: &AgentRow, at: usize, place: Option<String>) -> Row {
    use crate::theme::Role;
    let lead = format!("{}\u{2514} ", " ".repeat(at));
    let lead_w = crate::width::width(&lead);
    let room = c.width.saturating_sub(lead_w);
    let mut runs = vec![(at, 1, Role::Rule)];
    let text = match said(agent) {
        Some((what, ink)) => {
            let what = shorten_middle(&what, room);
            if agent.state == "working" && c.tick > 0 {
                // While it works and the column moves, a band of brighter greys crosses
                // what it is doing, left to right: the row says "alive" without a word
                // for it.
                runs.extend(sweep(&what, lead_w, c.tick));
            } else if ink != Role::Faint {
                runs.push((lead_w, crate::width::width(&what), ink));
            }
            format!("{lead}{what}")
        }
        None => {
            let tail_w = |tail: &str| crate::width::width(tail) + 3;
            let tab_room = place
                .as_deref()
                .map_or(room, |p| room.saturating_sub(tail_w(p)).max(room.min(12)));
            let tab = shorten_middle(&label(agent), tab_room);
            let mut text = format!("{lead}{tab}");
            let rest = room.saturating_sub(crate::width::width(&tab) + 3);
            // A place is a place and never moves: no band, only the quiet grey.
            if let Some(place) = place.filter(|_| rest >= 2) {
                runs.push((crate::width::width(&text) + 1, 1, Role::Rule));
                text.push_str(" \u{b7} ");
                text.push_str(&shorten_middle(&place, rest));
            }
            text
        }
    };
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
/// An idle agent with work running in the background says how much of it. A waiting agent whose newest word is `thinking` has said nothing about what it wants
/// (a label-less prompt), so it says nothing rather than `wants: thinking`.
fn said(agent: &AgentRow) -> Option<(String, crate::theme::Role)> {
    use crate::theme::Role;
    // Its turn is over and it reads idle, but its footer says something still runs: said
    // quietly, so a person does not take a busy agent for a free one.
    if agent.state == "idle" && agent.background > 0 {
        let n = agent.background;
        let s = if n == 1 { "" } else { "s" };
        return Some((format!("{n} background task{s}"), Role::Faint));
    }
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

/// What moves in the column, and so how fast the run loop has to step it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// Only the background dots going round: one frame is two of the working clock's ticks,
    /// so the loop steps every other tick and every wake changes a cell.
    Orbit,
    /// A working agent's spinner, breath and sweep, at the working clock.
    Spin,
}

/// Whether anything the column draws is moving, and what.
///
/// A working agent whose row is on it spins. An idle agent with background work orbits, however
/// long it has been idle (a row with work running is never stale), but only where the orbit is
/// drawn: not in the folded strip (`strip`), which shows a still circle. A working agent
/// folded away under its space's rule draws no row, so it moves nothing and arms no timer;
/// the keyboard's own agent always draws. The run loop asks this before it arms the next
/// frame.
/// ponytail: an agent scrolled out of the window still counts, a scroll-aware check
/// would need the window's arithmetic twice.
pub fn moves(c: &Column, strip: bool) -> Option<Motion> {
    let drawn = |a: &AgentRow| {
        c.view == View::Priority
            || !c.shut.contains(&a.workspace)
            || a.terminal == c.focused_terminal
    };
    let motion = |a: &AgentRow| {
        if !a.agent || !drawn(a) {
            return None;
        }
        if a.state == "working" {
            return Some(Motion::Spin);
        }
        (role_of(a) == crate::theme::Role::Background && !strip).then_some(Motion::Orbit)
    };
    let mut found = None;
    for m in c.agents.iter().filter_map(motion) {
        if m == Motion::Spin {
            return Some(m);
        }
        found = Some(m);
    }
    found
}

/// The mark an agent's row opens with: its state dot, or, while it works and the
/// column moves, the spinner's frame in the working colour.
///
/// Frame zero is the still column: hit testing, `QOOP_MOTION=0` and a column with
/// nothing working all paint at zero, where every mark is its dot.
fn mark_of(c: &Column, agent: &AgentRow, role: crate::theme::Role) -> String {
    if c.icons && role == crate::theme::Role::Sleeping {
        return asleep_mark();
    }
    let moving = agent.state == "working" || role == crate::theme::Role::Background;
    cell_mark(role, moving, c.tick)
}

/// An asleep tab's mark where icons are drawn: the zzz and the blank the two-cell mark
/// leaves before the name.
fn asleep_mark() -> String {
    format!("{} ", crate::icons::ASLEEP)
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
/// is reading it, and news never shows faded. A row with work running in the background is
/// never stale: it stays lit however long it has been idle, so the work is seen to go on.
fn dot_of(c: &Column, agent: &AgentRow) -> (crate::theme::Role, bool) {
    let fresh = is_new(agent, c.focused_terminal, c.seen, c.unseen);
    let stale = !fresh
        && agent.terminal != c.focused_terminal
        && role_of(agent) != crate::theme::Role::Background
        && tier_of(&agent.state, agent.last_active_at_unix, c.now_unix) == Some(Tier::Stale);
    let role = if stale {
        crate::theme::Role::Faint
    } else if fresh {
        crate::theme::of("done")
    } else {
        role_of(agent)
    };
    (role, stale)
}

/// An agent inside its space's box, `c` being the box's inside: the dot, the session,
/// and what it costs on the right when costs are shown.
///
/// The session wears its space's colour, which is what tells one space's rows from the
/// next now that no line runs beside them. A calm agent (idle, done and read, asleep, not
/// yet seen) wears the quieter step of it, so the rows that want a person or are working
/// stand out of their own space without a second hue. The keyboard's own row always wears
/// the full step: standing on it is reading it. A stale row dims as a whole, as before.
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
    let room = c.width.saturating_sub(lead + tail + LIVE_TAIL);
    let name = shorten_middle(&session_name(agent), room);
    let mut text = format!("{}{name}", mark_of(c, agent, role));
    let focused = agent.terminal == c.focused_terminal;
    let calm = !focused
        && !matches!(agent.state.as_str(), "working" | "starting")
        && !needs_person(agent, c.focused_terminal, c.seen, c.unseen);
    let ink = if calm {
        crate::theme::space_calm_in(c.spaces, agent.workspace)
    } else {
        crate::theme::space_in(c.spaces, agent.workspace)
    };
    let mut runs = Vec::new();
    if !stale {
        runs.push((lead, crate::width::width(&name), ink));
    }
    if let Some(cost) = cost {
        let at = c.width - crate::width::width(&cost) - LIVE_TAIL;
        text = format!("{}{cost}", fit_to(&text, at));
        runs.push((at, crate::width::width(&cost), crate::theme::Role::Faint));
    }
    // The cup: full in the brand ink when this pane is kept awake, a step quieter when its
    // whole space is, so a click on it is seen to be about the pane alone.
    let held = c
        .spaces
        .iter()
        .any(|s| s.id == agent.workspace && s.keep_awake);
    let at = c.width.saturating_sub(LIVE_TAIL);
    let cup = if c.icons {
        crate::icons::CUP.to_string()
    } else {
        "c".to_string()
    };
    let text = format!("{} {cup}", fit_to(&text, at));
    runs.push((
        at + 1,
        1,
        if agent.keep_awake {
            crate::theme::Role::Brand
        } else if held {
            crate::theme::Role::BrandCalm
        } else {
            crate::theme::Role::Faint
        },
    ));
    let (text, close) = with_close(&text, c.width);
    runs.push(close);
    Line {
        fill: None,
        text,
        colour: stale.then_some(crate::theme::Role::Faint),
        accent: None,
        indicator: Some((0, 2, role)),
        runs,
    }
}

/// The columns the close at the end of an agent's row takes: a blank and the `x`.
const CLOSE_W: usize = 2;

/// The columns a live agent's row ends in: a blank and the keep awake cup, then the close.
const LIVE_TAIL: usize = 2 + CLOSE_W;

/// An agent's first row, laid out for a space's inside, with its close `x` in the last
/// column: once boxed, the column the space's own `x` is in. In the controls' grey.
fn with_close(text: &str, width: usize) -> (String, (usize, usize, crate::theme::Role)) {
    let at = width.saturating_sub(1);
    (
        format!("{}x", fit_to(text, at)),
        (at, 1, crate::theme::Role::Faint),
    )
}

/// Whether a click at `col` (counted from one) on the first row of `terminal` lands on the
/// agent's close. Only the tree draws it, and only under a space it knows.
pub fn on_agent_close(col: usize, terminal: u32, c: &Column) -> bool {
    c.view == View::Tree
        && col + 1 == c.width
        && (c.agents.iter().any(|a| {
            a.terminal == terminal && a.agent && c.spaces.iter().any(|s| s.id == a.workspace)
        }) || c.spaces.iter().any(|s| {
            s.sleeping
                .iter()
                .any(|t| t.terminal == terminal && t.agent.is_some())
        }))
}

/// Whether a click at `col` (counted from one) on the first row of `terminal` lands on a live
/// agent's keep awake cup, two columns left of its close. A sleeping tab has none: idle
/// sleep has nothing to take from it.
pub fn on_agent_cup(col: usize, terminal: u32, c: &Column) -> bool {
    c.view == View::Tree
        && col + 3 == c.width
        && c.agents.iter().any(|a| {
            a.terminal == terminal && a.agent && c.spaces.iter().any(|s| s.id == a.workspace)
        })
}

/// A tab of a sleeping space: its space's quieter colour, like a calm agent's title, with
/// the sleeping mark and no other dot, since nothing runs in it. What a person called it,
/// else the agent that comes back in it, else a shell (a sleep refuses any other program),
/// and where.
fn sleeping_line(c: &Column, tab: &SleepingTab, space: &WorkspaceRow) -> Line {
    let name = tab.label().to_string();
    let text = match cwd_basename(Some(&tab.cwd)).filter(|b| !b.eq_ignore_ascii_case(&space.name)) {
        Some(place) => format!("{name} \u{b7} {place}"),
        None => name,
    };
    let lead = if c.icons {
        asleep_mark()
    } else {
        cell_mark(crate::theme::Role::Sleeping, false, 0)
    };
    let room = c.width.saturating_sub(crate::width::width(&lead) + CLOSE_W);
    let (text, close) = with_close(&format!("{lead}{}", shorten_middle(&text, room)), c.width);
    Line {
        text,
        colour: Some(crate::theme::space_calm_in(c.spaces, space.id)),
        indicator: Some((0, 2, crate::theme::Role::Sleeping)),
        runs: vec![close],
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
            Some((
                at,
                space_w,
                crate::theme::space_in(c.spaces, agent.workspace),
            )),
        )
    };
    Line {
        fill: None,
        text,
        colour: stale.then_some(crate::theme::Role::Faint),
        accent,
        indicator: Some((DOT_AT, 2, role)),
        runs: Vec::new(),
    }
}

/// The first of `content` the window shows in `body` rows: where the wheel put it, or else
/// where it keeps the keyboard's agent on screen.
fn first_row(content: &[Row], body: usize, c: &Column) -> usize {
    let len = content.len();
    if len <= body {
        return 0;
    }
    if let Some(at) = c.scroll {
        return at.min(len - body);
    }
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
}

/// The rows under the title, scrolled into `body` rows and padded out to them.
///
/// The window is where the wheel put it, or else whatever keeps the keyboard's agent on
/// screen, its space's rule too when both fit, and the top of the list otherwise. A list
/// that fits never scrolls, so a calm column is the same rows in the same places on every
/// paint.
fn window(content: Vec<Row>, body: usize, c: &Column, width: usize) -> Vec<Row> {
    let first = first_row(&content, body, c);
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
        Hit::Tabs => {
            let at = col.saturating_sub(1);
            let cell = tab_cells(c, &tab_counts(c))
                .into_iter()
                .find(|(_, from, text)| (*from..from + crate::width::width(text)).contains(&at));
            match cell {
                Some((Some(tab), _, _)) => Some(Act::InboxTab(tab)),
                // The caret folds; anywhere on the folded row opens it again.
                Some((None, 0, _)) => Some(Act::InboxFold),
                _ => c.inbox.folded.then_some(Act::InboxFold),
            }
        }
        Hit::Space(id) => {
            // The folded strip is four columns and carries no controls, so a click there
            // cannot reach one however the columns would fall.
            let space = (!collapsed)
                .then(|| c.spaces.iter().find(|s| s.id == id))
                .flatten();
            if let Some(space) = space {
                // A control answers on every cell of it. An armed rule answers `y` and `no`,
                // and nothing else: a row that is mid question does not fold, does not
                // navigate and does not sleep, because a person aiming at one of those would
                // be answering a question they did not read.
                match block(c, space).at(col.saturating_sub(1)) {
                    Some(Ctl::Yes) => return Some(Act::ConfirmClose(id)),
                    _ if c.armed == Some(id) => return Some(Act::CancelClose),
                    Some(Ctl::Toggle) => return Some(Act::Drawer(id)),
                    Some(Ctl::New) => return Some(Act::New(id)),
                    Some(Ctl::Sleep) => return Some(Act::Sleep(id)),
                    Some(Ctl::Wake) => return Some(Act::Wake(id)),
                    Some(Ctl::Cup) => return Some(Act::KeepAwake(id)),
                    Some(Ctl::Settings) => return Some(Act::Settings(id)),
                    Some(Ctl::Close) => return Some(Act::Close(id)),
                    Some(Ctl::No | Ctl::Ask) | None => {}
                }
            }
            // A rule mid question folds nothing and moves nowhere: a person aiming at the
            // fold is aiming at a question, and the only answers are the two chips.
            if c.armed == Some(id) {
                return Some(Act::CancelClose);
            }
            // In the tree the whole rule, name included, folds its space and opens it again;
            // its tabs are what move the screen there. A rule with nothing under it has
            // nothing to fold, so every cell of it moves to the space.
            let folds = !collapsed && c.view == View::Tree && folds(c, id);
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
            // `menu « ` closes the row: the word, a space, the mark and a space. Where the
            // column has room, `settings` and two spaces come before it.
            let end = c.width.saturating_sub(FOOT_TAIL - MENU_LABEL.len());
            let start = end.saturating_sub(MENU_LABEL.len());
            let settings_start = c.width.saturating_sub(FOOT_TAIL + 2 + SETTINGS_LABEL.len());
            let settings_end = settings_start + SETTINGS_LABEL.len();
            // The version's own cells, when the row shows it: the site, not the fold.
            let version =
                version_link_at(c.width).filter(|at| col > *at && col <= at + VERSION.len());
            Some(if col > start && col <= end {
                Act::Menu
            } else if version.is_some() {
                Act::Site
            } else if c.width >= SETTINGS_FROM && col > settings_start && col <= settings_end {
                Act::SettingsPage
            } else {
                Act::Collapse
            })
        }
    }
}

/// The columns `menu « ` takes at the right of the foot.
const FOOT_TAIL: usize = 7;

const MENU_LABEL: &str = "menu";

const SETTINGS_LABEL: &str = "settings";

/// The narrowest column the foot names `settings` in: the version still fits beside it.
const SETTINGS_FROM: usize = 24;

/// What closes the foot's last row: `settings` when there is room, then `menu « `.
fn foot_tail(width: usize) -> String {
    if width >= SETTINGS_FROM {
        format!("{SETTINGS_LABEL}  {MENU_LABEL} \u{ab} ")
    } else {
        format!("{MENU_LABEL} \u{ab} ")
    }
}

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
        let colour = colour_for(&agent.state)
            .or((role_of(agent) == crate::theme::Role::Background)
                .then_some(crate::theme::Role::Background));
        let role = colour.unwrap_or(crate::theme::Role::Idle);
        (
            Line {
                text: strip_agent(strip_mark(c, agent, role)),
                colour,
                // The space mark sits on the first pad cell, never on the glyph: the glyph
                // column keeps the state colour, so the strip carries state and space side
                // by side instead of painting one over the other.
                accent: Some((2, 1, crate::theme::space_in(c.spaces, agent.workspace))),
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
                accent: Some((1, number.len(), crate::theme::space_in(c.spaces, space.id))),
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
pub(crate) fn session_name(agent: &AgentRow) -> String {
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

/// What [`FILES`] holds: the picked pet, a newer version when one was seen, and whether the
/// phone bridge says it reaches the hub.
type Files = (
    Option<qoop_runtime::avatar::Avatar>,
    Option<String>,
    qoop_runtime::paths::HubLink,
);

/// The pet and the update notice as last read off disk.
///
/// Held, not read per paint: two file reads and a JSON parse on every frame was
/// the render thread's cost while an agent streamed. Both files are written by
/// processes outside the interface (`qoop avatar pick`, `qoop update --check`),
/// so the run loop re-reads them on the acknowledgement tick it already has,
/// and a new pet or notice shows up to one tick late.
// ponytail: process-wide, the interface has one render loop; a State field would
// thread two more arguments through rows_folded and every hit test.
static FILES: std::sync::RwLock<Files> =
    std::sync::RwLock::new((None, None, qoop_runtime::paths::HubLink::Absent));

/// Re-read the pet and the update notice. True when either changed, so the
/// caller repaints. Never under test, so the thirty-odd paints the suite asserts
/// never depend on the developer's own home directory.
pub(crate) fn refresh_files() -> bool {
    if cfg!(test) {
        return false;
    }
    hold(
        &FILES,
        (
            qoop_runtime::avatar::load(),
            notice_from_disk(),
            qoop_runtime::paths::read_hub_link(),
        ),
    )
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

/// Whether the bridge says the hub is reached, as [`refresh_files`] last read it.
fn hub_link() -> HubLink {
    FILES.read().unwrap_or_else(|e| e.into_inner()).2
}

/// The picked avatar, when one is picked, as [`refresh_files`] last read it.
pub(crate) fn current_avatar() -> Option<qoop_runtime::avatar::Avatar> {
    FILES.read().unwrap_or_else(|e| e.into_inner()).0.clone()
}

/// Whether the pet itself fits in the foot: open column, wide enough for the thick q and the
/// widest pet beside it ([`Q_PET_WIDTH`]), tall enough that the foot with its count box
/// still leaves six rows of list.
pub(crate) fn avatar_fits(height: usize, width: usize, present: bool) -> bool {
    present && width >= Q_PET_WIDTH && height >= STAGE_FOOT_ROWS + 6
}

/// The widest pet once trimmed to the columns it lights, in cells.
pub(crate) const PET_COLS: usize = 8;
/// The foot with the pet in it: the pet's five half-block rows standing on the menu row.
/// The same for every avatar, so the words sit in the same rows whatever the pet is.
pub(crate) const PET_FOOT_ROWS: usize = qoop_runtime::avatar::ART_ROWS + 1;
/// The rows of the box that shows the inbox count: a border, the number, a border.
const COUNT_BOX_ROWS: usize = 3;
/// The foot with the pet standing: the count box over the pet's rows and the menu row.
pub(crate) const STAGE_FOOT_ROWS: usize = COUNT_BOX_ROWS + PET_FOOT_ROWS;

/// The pet as itself: its five half-block rows (a cell is two pixels, drawn as
/// [`qoop_runtime::avatar::paint`] says), trimmed to the columns any row lights, the
/// same trim for every row so the picture is never skewed. Pre-escaped for the overlay to
/// stamp at the right of the foot, with its width in cells. Rows it does not light are
/// blank, so the pet stands on the menu row whatever its hair. Empty without colour,
/// because half blocks with no ink are only a smudge.
pub(crate) fn avatar_overlay(
    avatar: &qoop_runtime::avatar::Avatar,
    colour: bool,
) -> (usize, Vec<String>) {
    if !colour {
        return (0, Vec::new());
    }
    use qoop_runtime::avatar::{ART_COLS, Paint, art_cells, bg, fg, paint};
    let cells = art_cells(avatar);
    let lit = |x: usize| cells.iter().any(|row| row[x] != (None, None));
    let Some(lo) = (0..ART_COLS).find(|x| lit(*x)) else {
        return (0, Vec::new());
    };
    let hi = (0..ART_COLS).rfind(|x| lit(*x)).unwrap_or(lo);
    let fore = |n: u8| fg(crate::theme::pet(n));
    let back =
        |n: Option<u8>| n.map_or_else(|| "\x1b[49m".to_string(), |n| bg(crate::theme::pet(n)));
    let ground = crate::theme::ground_fg();
    let rows = cells
        .iter()
        .map(|row| {
            // Reset first: whatever the column last painted must not tint the blanks.
            let mut line = String::from("\x1b[0m");
            let (mut fg_now, mut bg_now) = (String::new(), back(None));
            for &cell in &row[lo..=hi] {
                // An empty half is always the terminal's own background, or its exact colour.
                let (glyph, fore, back) = match (paint(cell), &ground) {
                    (Paint::Blank, _) => (' ', fg_now.clone(), back(None)),
                    (Paint::Fill(n), _) => (' ', fg_now.clone(), back(Some(n))),
                    (Paint::Lower { ink, over }, _) => ('\u{2584}', fore(ink), back(over)),
                    (Paint::Upper(n), Some(ground)) => ('\u{2584}', ground.clone(), back(Some(n))),
                    (Paint::Upper(n), None) => ('\u{2580}', fore(n), back(None)),
                };
                if fore != fg_now {
                    line.push_str(&fore);
                    fg_now = fore;
                }
                if back != bg_now {
                    line.push_str(&back);
                    bg_now = back;
                }
                line.push(glyph);
            }
            line.push_str("\x1b[0m");
            line
        })
        .collect();
    (hi - lo + 1, rows)
}

/// Screen rows the overlay stamps, top first, for a column of `height` rows
/// starting at `top`: the foot's first five, so the pet's feet rest on the menu row.
/// Pure so the paint and the tests agree on arithmetic.
pub(crate) fn avatar_overlay_rows(top: usize, height: usize) -> Vec<usize> {
    (0..qoop_runtime::avatar::ART_ROWS)
        .map(|i| top + height - PET_FOOT_ROWS + i)
        .collect()
}

/// The q of `docs/brand/make.py` in thick dots: the three by three ring drawn one `●` a cell
/// with a blank cell between dots, so the gaps across and down read equal on a cell about
/// half as wide as it is tall, and its tail hanging under the ring's right column.
const Q_LOGO: [&str; 4] = [
    "\u{25cf} \u{25cf} \u{25cf}",
    "\u{25cf}   \u{25cf}",
    "\u{25cf} \u{25cf} \u{25cf}",
    "    \u{25cf}",
];
/// The tail, as (row, cell) of [`Q_LOGO`]: the one dot `make.py` draws in ember. Every other
/// dot is cream. A dot is a cell of its own, so it is exact.
const Q_TAIL: (usize, usize) = (3, 4);
/// The q's cells across.
const Q_COLS: usize = 5;
/// The rows the foot takes with no pet: the q's four, then the menu.
const Q_FOOT_ROWS: usize = Q_LOGO.len() + 1;
/// The narrowest column that holds the q and the widest pet side by side, with a cell
/// between them and a margin at each edge. Under [`SIDEBAR_MIN`], so an open column always
/// has room for the pet.
const Q_PET_WIDTH: usize = 1 + Q_COLS + 1 + PET_COLS + 1;

/// The foot of the open column, its last row always the menu row: the version and the online
/// dot at the left and `menu « ` at the right.
///
/// The logo is the q alone ([`Q_LOGO`]) on the four rows above the menu, cream with its tail
/// in ember. While the pet stands (`stage` is the inbox count) one blank row goes on top and
/// the pet stands at the right of the foot's first five rows (stamped by the overlay), its
/// feet on the row above the menu. Over the pet sits a small box with the count alone.
///
/// ```text
///                  ╭───╮
///                  │ 2 │
///                  ╰───╯
///               ▀▀▀▀▀▀
///  ● ● ●        ▀▀▀▀▀▀
///  ●   ●        ▀▀▀▀▀▀
///  ● ● ●        ▀▀▀▀▀▀
///      ●        ▀▀▀▀▀▀
///  0.5.0 ●  settings  menu «
/// ```
///
/// A dot is a plain `●`, which every terminal draws. Whatever the pet, the words sit in the
/// same rows counted from the bottom: nothing a person reads ever moves. The menu row answers
/// a click the way the foot always has (the menu at its word, the fold elsewhere); the other
/// rows are text and answer nothing.
fn foot_rows(width: usize, stage: Option<usize>, update: Option<&str>, hub: HubLink) -> Vec<Row> {
    use crate::theme::Role;
    let tail = foot_tail(width);
    // The menu closes the last row: `menu « ` flush right, `settings` and the version before it.
    let version = version_text(width, crate::width::width(&tail), update);
    // The dot is the interface's daemon, which it is connected to whenever it is drawn (it
    // ends with the stream), and the hub the phone bridge says it reaches: green when both,
    // warm when the bridge is there and the hub is not, quiet when there is no bridge.
    let dotted = format!("{version} {ONLINE}");
    let online = (!version.is_empty()
        && crate::width::width(&dotted) + 1 + crate::width::width(&tail) <= width)
        .then(|| crate::width::width(&version) + 1);
    let lead = if online.is_some() { dotted } else { version };
    let gap = width.saturating_sub(crate::width::width(&lead) + crate::width::width(&tail));
    let menu_row = Line {
        text: fit(&format!("{lead}{}{tail}", " ".repeat(gap)), width),
        colour: Some(Role::Faint),
        runs: online
            .map(|at| (at, 1, online_role(hub)))
            .into_iter()
            .collect(),
        ..Line::default()
    };
    let menu_row = (menu_row, Some(Hit::Foot));
    let mut out: Vec<Row> = Vec::with_capacity(STAGE_FOOT_ROWS);
    if let Some(count) = stage {
        out.extend(count_box(width, count));
        out.extend((0..PET_FOOT_ROWS - Q_FOOT_ROWS).map(|_| blank(width)));
    }
    out.extend(Q_LOGO.iter().enumerate().map(|(row, dots)| {
        let (at, cell) = Q_TAIL;
        let line = Line {
            text: fit(&format!(" {dots}"), width),
            colour: Some(Role::Text),
            runs: if row == at {
                vec![(1 + cell, 1, Role::Brand)]
            } else {
                Vec::new()
            },
            ..Line::default()
        };
        (line, None)
    }));
    out.push(menu_row);
    out
}

/// The `●` that says the interface is connected to its daemon, and to the hub if there is one.
const ONLINE: char = '\u{25cf}';

/// The online dot's ink for what the bridge says about the hub.
fn online_role(hub: HubLink) -> crate::theme::Role {
    use crate::theme::Role;
    match hub {
        HubLink::Up => Role::Done,
        HubLink::Down => Role::Blocked,
        HubLink::Absent => Role::Faint,
    }
}

/// The box over the pet with the inbox count alone, flush right with the pet's right edge:
/// one blank cell of margin before the column's edge, as the pet keeps. The number is warm
/// while anything waits and quiet at zero.
fn count_box(width: usize, count: usize) -> Vec<Row> {
    use crate::theme::Role;
    let inner = format!(" {count} ");
    let across = crate::width::width(&inner);
    let at = width.saturating_sub(across + 3);
    let row = |text: String, runs: Vec<(usize, usize, Role)>| {
        let line = Line {
            text: fit(&format!("{}{text}", " ".repeat(at)), width),
            colour: Some(Role::Faint),
            runs,
            ..Line::default()
        };
        (line, None)
    };
    let edge = |l: char, r: char| format!("{l}{}{r}", "\u{2500}".repeat(across));
    let number = if count > 0 {
        Role::Blocked
    } else {
        Role::Faint
    };
    vec![
        row(
            edge('\u{256d}', '\u{256e}'),
            vec![(at, across + 2, Role::Rule)],
        ),
        row(
            format!("\u{2502}{inner}\u{2502}"),
            vec![
                (at, 1, Role::Rule),
                (at + 1, across, number),
                (at + 1 + across, 1, Role::Rule),
            ],
        ),
        row(
            edge('\u{2570}', '\u{256f}'),
            vec![(at, across + 2, Role::Rule)],
        ),
    ]
}

/// Where the version starts on the menu row, when the row shows it: the column its link
/// covers. The word is [`VERSION`]; a newer version after it is not part of the link.
pub(crate) fn version_link_at(width: usize) -> Option<usize> {
    version_link_in(width, update_available().as_deref())
}

/// [`version_link_at`] for a given update notice.
fn version_link_in(width: usize, update: Option<&str>) -> Option<usize> {
    let tail = foot_tail(width);
    let shown = version_text(width, crate::width::width(&tail), update);
    (!shown.is_empty()).then_some(1)
}

/// Where this program lives on the web, which the version links to.
pub(crate) const SITE: &str = "https://qoop.si";

/// The version as the menu row opens with it: ` 0.5.0`, or ` 0.5.0>0.6.0` when a newer
/// one was seen, beside a tail of `tail` columns with at least a space between. A version
/// cut short reads as another version, so each form shows whole or not at all: the notice
/// goes first, then the version itself.
fn version_text(width: usize, tail: usize, update: Option<&str>) -> String {
    let fits = |text: &String| crate::width::width(text) + 1 + tail <= width;
    update
        .map(|new| format!(" {VERSION}>{new}"))
        .filter(fits)
        .or_else(|| Some(format!(" {VERSION}")).filter(fits))
        .unwrap_or_default()
}

/// Which build this is, read from the crate rather than written down twice.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A newer version `qoop update --check` saw, when it saw one, as [`refresh_files`]
/// last read it.
fn update_available() -> Option<String> {
    FILES.read().unwrap_or_else(|e| e.into_inner()).1.clone()
}

/// The notice file, read now. A missing file, garbage in it, or nothing newer is no
/// notice, and the row is byte for byte what it always was.
fn notice_from_disk() -> Option<String> {
    let seen = qoop_runtime::paths::read_update_notice()?;
    qoop_runtime::paths::is_newer_version(&seen, VERSION).then_some(seen)
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
            fill: None,
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
    use qoop_runtime::control::LinkState;
    use qoop_runtime::paths::HubLink;

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
        let held = std::sync::RwLock::new((None, None, HubLink::Absent));
        assert!(
            !hold(&held, (None, None, HubLink::Absent)),
            "nothing changed"
        );
        assert!(
            hold(&held, (None, Some("9.9.9".to_string()), HubLink::Absent)),
            "a new notice"
        );
        assert!(
            !hold(&held, (None, Some("9.9.9".to_string()), HubLink::Absent)),
            "the same notice"
        );
        assert_eq!(held.read().unwrap().1.as_deref(), Some("9.9.9"));
        assert!(
            hold(&held, (None, None, HubLink::Absent)),
            "the notice went away"
        );
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
            // Folded: one row at the top, so the tree tests read the tree at the rows they
            // always did. The inbox's own tests open it.
            inbox: Inbox {
                folded: true,
                ..Inbox::default()
            },
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
            scroll: None,
            icons: false,
            drawer: None,
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

    /// A landing rule opens inside the list and costs its last row: the column keeps its
    /// height, the rows above the rule and the foot do not move, and the rows under it move
    /// down by one.
    #[test]
    fn a_drop_rule_opens_inside_the_list_and_keeps_the_foot() {
        let spaces = [space(1, "qoop"), space(2, "app")];
        let agents = [
            agent(1, 1, "claude", "working"),
            agent(2, 1, "codex", "idle"),
            agent(3, 2, "claude", "blocked"),
        ];
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
            0,
            WIDTH,
            &shut,
        );
        let plain = rows_folded(&c, 30, None);
        let at = plain
            .iter()
            .position(|(_, hit)| *hit == Some(Hit::Terminal(2)))
            .expect("drawn");
        let role = crate::theme::space_in(&spaces, 1);
        let ruled = rows_folded_with(&c, 30, None, Some((at, role)));
        assert_eq!(ruled.len(), plain.len());
        assert_eq!(ruled[..at], plain[..at]);
        assert_eq!(
            ruled[at].1, None,
            "the rule is paint, never a place to click"
        );
        assert_eq!(ruled[at].0.colour, Some(role));
        assert!(
            ruled[at].0.text.contains('\u{2576}'),
            "{:?}",
            ruled[at].0.text
        );
        assert_eq!(crate::width::width(&ruled[at].0.text), WIDTH);
        assert_eq!(ruled[at + 1], plain[at], "the rows under it move down one");
        let foot = |rows: &[Row]| rows[rows.len() - 3..].to_vec();
        assert_eq!(foot(&ruled), foot(&plain), "the foot stays put");
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
        avatar: Option<&qoop_runtime::avatar::Avatar>,
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
        avatar: Option<&qoop_runtime::avatar::Avatar>,
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
            keep_awake: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
            task: None,
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
            keep_awake: false,
            background: 0,
            task: None,
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
        let text = without_close(text);
        match text
            .strip_prefix(" \u{2502} ")
            .and_then(|t| t.strip_suffix('\u{2502}'))
        {
            Some(inside) => format!("   {}", inside.trim_end()),
            None => text.to_string(),
        }
    }

    /// An agent's first row without the close `x` at its end: a row whose mark is a braille
    /// dot. A space's rule keeps its own `x`.
    fn without_close(text: &str) -> &str {
        let text = text.trim_end();
        let agent = text
            .chars()
            .nth(1)
            .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c));
        match text.strip_suffix(" x") {
            // A live agent's cup before it too.
            Some(rest) if agent => rest.strip_suffix(" c").unwrap_or(rest).trim_end(),
            _ => text,
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

    /// The same column with its inbox open at the default height.
    fn opened(c: Column) -> Column {
        Column {
            inbox: Inbox::default(),
            ..c
        }
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
            texts.iter().any(|t| t == " \u{2810}\u{2800}codex"),
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
            .position(|t| t == " \u{2818}\u{2802}claude")
            .unwrap_or_else(|| panic!("{texts:?}"));
        assert!(texts[head - 1].starts_with("\u{25be}app"), "{texts:?}");
        assert_eq!(texts[head + 1], "   \u{2514} claude", "{texts:?}");
        // The tree lists agents only, so the space's plain shell tab draws no row: the
        // bottom of a space's box, an empty row before the next space starts, follows
        // straight after the agent's own two rows.
        assert!(texts[head + 2].is_empty(), "{texts:?}");
        assert!(texts[head + 3].starts_with("\u{25be}personal"), "{texts:?}");
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
            .find(|l| l.text.starts_with("\u{25be}app"))
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
        // The title wears its space's colour, right after the two cells of the mark.
        assert!(
            rows[0]
                .runs
                .contains(&(BOX_LEAD + 2, 6, crate::theme::space_of(1))),
            "{:?}",
            rows[0]
        );
        // A rule has no state to have a colour about: it wears the space's own colour
        // on the name and opens with its fold mark, no line.
        let personal = lines
            .iter()
            .find(|l| l.text.contains("personal"))
            .expect("the personal rule");
        assert_eq!(personal.colour, None);
        assert_eq!(
            personal.accent,
            Some((NAME_AT, 8, crate::theme::space_of(2)))
        );
        assert_eq!(personal.indicator, None);
        assert!(
            personal.text.starts_with("\u{25be}personal"),
            "{personal:?}"
        );
    }

    /// A title wears its space's colour; a calm agent (idle, a done row already read,
    /// asleep, not yet seen) wears the quieter step of it, while one working or wanting a
    /// person keeps the full ink. The keyboard's row keeps the full ink whatever its
    /// state, and a stale row dims as a whole with no title colour at all.
    #[test]
    fn a_calm_agent_wears_the_quieter_step_of_its_space() {
        use crate::theme::{space_calm_of, space_of};
        let spaces = vec![space(1, "app")];
        let agents = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "claude", "blocked"),
            agent(3, 1, "claude", "idle"),
            agent(4, 1, "claude", "done"),
            agent(5, 1, "claude", "unobserved"),
            agent(6, 1, "claude", "idle"),
            agent_at(7, 1, "claude", "idle", ACTIVITY_STALE_SECS + 60),
            agent(8, 1, "claude", "done"),
        ];
        // Terminal 4's finish has been read; terminal 8's has not, so it is news.
        let seen = Seen::from([(4, 0)]);
        let rows = tree_rows(&spaces, &agents, 6, 40, &seen, &Shut::new());
        let title = |terminal: u32| {
            let line = tree_row(&rows, terminal);
            line.runs
                .iter()
                .find(|(at, _, _)| *at == BOX_LEAD + 2)
                .map(|run| run.2)
        };
        assert_eq!(title(1), Some(space_of(1)), "working");
        assert_eq!(title(2), Some(space_of(1)), "blocked");
        assert_eq!(title(3), Some(space_calm_of(1)), "idle");
        assert_eq!(title(4), Some(space_calm_of(1)), "a read finish");
        assert_eq!(title(5), Some(space_calm_of(1)), "unobserved");
        assert_eq!(title(6), Some(space_of(1)), "the keyboard's row");
        assert_eq!(title(7), None, "stale dims as a whole");
        assert_eq!(tree_row(&rows, 7).colour, Some(crate::theme::Role::Faint));
        assert_eq!(title(8), Some(space_of(1)), "an unread finish is news");
    }

    /// A colour picked for a space reaches its rule and its agents' titles: the column
    /// reads it from its own rows, so nothing else has to be told.
    #[test]
    fn a_picked_colour_reaches_the_rule_and_the_titles() {
        use crate::theme::Role;
        let mut spaces = vec![space(1, "app")];
        spaces[0].ink = Some(4);
        let agents = vec![
            agent(1, 1, "claude", "working"),
            agent(2, 1, "claude", "idle"),
        ];
        let rows = tree_rows(&spaces, &agents, 1, 40, &Seen::new(), &Shut::new());
        assert_eq!(
            rule_row(&rows, 1).accent,
            Some((NAME_AT, 3, Role::Space(4)))
        );
        let title = |terminal: u32| {
            tree_row(&rows, terminal)
                .runs
                .iter()
                .find(|(at, _, _)| *at == BOX_LEAD + 2)
                .map(|run| run.2)
        };
        assert_eq!(title(1), Some(Role::Space(4)));
        assert_eq!(title(2), Some(Role::SpaceCalm(4)));
    }

    /// Every space is one rule: the fold mark, the name, what it holds beside the name, and
    /// at the right `+` and the drawer's toggle, the rest of the controls in the drawer.
    #[test]
    fn every_space_draws_one_rule_with_its_arrow_count_and_block() {
        use crate::theme::Role;
        let (spaces, agents) = sample();
        let rows = tree_rows(&spaces, &agents, 1, 20, &Seen::new(), &Shut::new());
        let app = rule_row(&rows, 1);
        assert_eq!(
            app.text,
            format!("\u{25be}app (1){}+ \u{22ef} ", " ".repeat(WIDTH - 12))
        );
        assert_eq!(crate::width::width(&app.text), WIDTH);
        // The fold mark, the count, the toggle and `+`, all in the fold's grey. Every run
        // inside the row, none of them overlapping, and no line: the rule has no indicator.
        assert_eq!(
            app.runs,
            [
                (0, 1, Role::Faint),
                (4, 4, Role::Faint),
                (WIDTH - 2, 1, Role::Faint),
                (WIDTH - 4, 1, Role::Faint),
            ]
        );
        // An empty space has nothing to fold and nothing to count.
        let dub = rule_row(&rows, 3);
        assert_eq!(
            dub.text,
            format!(" dub{}+ \u{22ef} ", " ".repeat(WIDTH - 8))
        );

        // Folded, the mark turns, the name stays put and the count stays.
        let shut = Shut::from([1]);
        let rows = tree_rows(&spaces, &agents, 9, 20, &Seen::new(), &shut);
        assert!(
            rule_row(&rows, 1).text.starts_with("\u{25b8}app (1)"),
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

    /// The wheel moves a long list past the keyboard's agent, all the way to the last row
    /// and back to the first, and never past either end.
    #[test]
    fn the_wheel_scrolls_a_long_list_to_its_end_and_back() {
        let spaces: Vec<WorkspaceRow> = (1..=10).map(|n| space(n, &format!("s{n}"))).collect();
        let agents: Vec<AgentRow> = (1..=30u32)
            .map(|n| agent(n, i64::from((n - 1) / 3 + 1), "claude", "idle"))
            .collect();
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = column(
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
        let drawn = |c: &Column| -> Vec<Hit> {
            rows_folded(c, 15, None)
                .into_iter()
                .filter_map(|(_, hit)| hit)
                .collect()
        };
        assert!(!drawn(&c).contains(&Hit::Terminal(30)), "the list fits?");
        // Turned down far enough, the last agent shows and the wheel stops there.
        let mut last = None;
        for _ in 0..100 {
            c.scroll = Some(wheeled(&c, 15, None, false, 3));
            if c.scroll == last {
                break;
            }
            last = c.scroll;
        }
        assert!(drawn(&c).contains(&Hit::Terminal(30)), "{:?}", drawn(&c));
        assert!(
            !drawn(&c).contains(&Hit::Terminal(1)),
            "the focused row pinned it"
        );
        assert_eq!(rows_folded(&c, 15, None).len(), 15);
        // And back up, to the first space's rule under the title.
        for _ in 0..100 {
            c.scroll = Some(wheeled(&c, 15, None, true, 3));
        }
        assert_eq!(c.scroll, Some(0));
        assert!(drawn(&c).contains(&Hit::Space(1)));
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
                    // Never the foot: at a small height there is no title above it yet to
                    // set `in_tree`, and its logo (the word, or the q alone) also opens with
                    // a space.
                    let logo = Q_LOGO.iter().any(|row| text == format!(" {row}"));
                    let next = !text.ends_with('\u{ab}')
                        && !logo
                        && (text
                            .strip_prefix(' ')
                            .and_then(|t| t.chars().next())
                            .is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
                            || text == " nothing needs you");
                    let braille = |c: char| ('\u{2800}'..='\u{28ff}').contains(&c);
                    let tabs = !in_tree && text.starts_with(['\u{25b8}', '\u{25be}']);
                    let want = if tabs {
                        // The inbox's tab row, folded here: the column picks the tab.
                        Some(Hit::Tabs)
                    } else if !in_tree && next {
                        // The next row reaches the first of the queue, or nothing while
                        // nobody waits.
                        let queue = queue(&agents, 3, &Seen::new(), &Unseen::new());
                        Some(Hit::Next(queue.first().map(|a| a.terminal)))
                    } else if text.trim_start().starts_with('\u{2514}') {
                        second
                    } else if text.is_empty() {
                        // A space's bottom edge and the gap between two agents are
                        // blank rows: nothing to click, so nothing they reach.
                        None
                    } else if text.starts_with(&format!(" {SPACES_TITLE}")) && text.ends_with('+') {
                        Some(Hit::Title)
                    } else if text.ends_with('\u{ab}') {
                        // The menu row, the foot's last: the version and `menu «`.
                        Some(Hit::Foot)
                    } else if logo {
                        // The logo and the q's tail are text and answer nothing.
                        None
                    } else if let Some(name) = text
                        .strip_prefix([' ', '\u{25be}', '\u{25b8}'])
                        .filter(|t| {
                            in_tree && t.chars().next().is_some_and(|c| c != ' ' && !braille(c))
                        })
                    {
                        // A space's rule moves to the space it names: the name opens the
                        // row after its fold column, with nothing but a word boundary (a
                        // space, or the row's own end) after it.
                        spaces
                            .iter()
                            .find(|s| {
                                name.starts_with(&s.name)
                                    && name[s.name.len()..]
                                        .chars()
                                        .next()
                                        .is_none_or(|c| !c.is_alphanumeric())
                            })
                            .map(|s| Hit::Space(s.id))
                    } else {
                        // Everything left is an agent's row, its mark at column one, in a
                        // space or flat: it reaches the agent its name names.
                        agents
                            .iter()
                            .find(|a| {
                                text.strip_prefix(' ')
                                    .map(|rest| {
                                        rest.trim_start_matches(braille)
                                            .trim_end_matches('\u{2026}')
                                            .split("  ")
                                            .next()
                                            .is_some_and(|named| {
                                                !named.is_empty()
                                                    && session_name(a).starts_with(
                                                        named.trim_end_matches('\u{2026}'),
                                                    )
                                            })
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
        let lines = render(&[], &agents, 1, 4, 10);
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
            keep_awake: false,
            background: 0,
            task: None,
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
            assert_eq!(unbox(&rows[at].0.text), " \u{2810}\u{2800}opencode");
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
                .filter(|t| *t == " \u{2810}\u{2800}release")
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
            texts.iter().any(|t| t == " \u{2810}\u{2800}abc123"),
            "{texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("opencode")),
            "the program leaked onto a reported session row: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "   \u{2514} release"),
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
            texts.iter().any(|t| t == " \u{2810}\u{2800}codex"),
            "{texts:?}"
        );
    }

    #[test]
    fn a_wrapper_is_shown_as_standing_in_the_way_and_never_as_the_program() {
        let agents = vec![AgentRow {
            keep_awake: false,
            background: 0,
            task: None,
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
            keep_awake: false,
            background: 0,
            task: None,
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
        let rows = tree_rows(&spaces, &agents, 1, 10, &Seen::new(), &Shut::new());
        // The space's own rule: the space after its name is one this module wrote, and
        // the grid's rule would have eaten it.
        let rule = rule_row(&rows, 1);
        assert!(
            rule.text.starts_with("\u{25be}\u{4e2d}\u{6587} (1)"),
            "{:?}",
            rule.text
        );
        assert_eq!(crate::width::width(&rule.text), WIDTH);
        // The agent's row opens with a blank, then the state mark and the name.
        let row = tree_row(&rows, 1);
        assert!(row.text.starts_with(" \u{2810}\u{2800}"), "{:?}", row.text);
        assert_eq!(crate::width::width(&row.text), WIDTH);
    }

    #[test]
    fn an_absurdly_long_state_never_eats_the_session() {
        let agents = vec![agent(1, 1, "claude", &"z".repeat(80))];
        let rows = tree_rows(
            &[space(1, "app")],
            &agents,
            1,
            10,
            &Seen::new(),
            &Shut::new(),
        );
        // No state word is drawn at all: the row is the dot and the session,
        // however long the daemon's state string runs.
        let row = tree_row(&rows, 1);
        assert_eq!(crate::width::width(&row.text), WIDTH);
        assert_eq!(
            unbox(&row.text),
            " \u{2810}\u{2800}claude",
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
        // it needs. Urgency is the inbox's job.
        assert_eq!(order(&rows), vec![1, 3, 5, 6, 9, 2, 4, 7, 8]);
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
        // (Agent programs here never contain these words, so any hit is chrome.) The
        // inbox's own row, folded in these tests, is the one `▸` that belongs.
        for header in ["running", "finished", "idle", "\u{25b8}"] {
            assert!(
                !texts.iter().skip(1).any(|t| t.contains(header)),
                "a group header leaked: {header}: {texts:?}"
            );
        }
        // Nine tree rows.
        assert_eq!(
            texts
                .iter()
                .filter(|t| !t.trim_start().starts_with(['\u{2514}', '\u{2570}']))
                .filter(|t| !t.contains('\u{2514}'))
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            9,
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
            texts.iter().any(|t| t == " \u{2818}\u{2802}claude"),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == " \u{2810}\u{2800}codex"),
            "{texts:?}"
        );
    }

    /// The column opens at its default width only where half the window holds it, and
    /// never under the narrowest a drag allows.
    #[test]
    fn the_column_opens_within_half_the_window() {
        assert_eq!(opening_width(200), WIDTH);
        assert_eq!(opening_width(2 * WIDTH as u16), WIDTH);
        assert_eq!(opening_width(2 * WIDTH as u16 - 2), WIDTH - 1);
        assert_eq!(opening_width(60), 30);
        assert_eq!(opening_width(20), SIDEBAR_MIN);
        for cols in 0..=200u16 {
            assert!(opening_width(cols) <= sidebar_max(cols), "{cols}");
        }
    }

    #[test]
    fn the_foot_of_the_column_carries_the_mark_the_version_and_the_menu() {
        // The q alone, its tail in ember, then the menu.
        let rows = foot_rows(WIDTH, None, None, HubLink::Up);
        assert_eq!(rows.len(), Q_FOOT_ROWS, "the q's four rows and the menu");
        for (line, _) in &rows {
            assert_eq!(
                crate::width::width(&line.text),
                WIDTH,
                "a row has to fill the column exactly or the one under it shifts: {line:?}"
            );
        }
        let q = Q_LOGO;
        let brand = crate::theme::Role::Brand;
        let embers = [vec![], vec![], vec![], vec![(5, 1, brand)]];
        for (i, (line, hit)) in rows[..4].iter().enumerate() {
            assert_eq!(line.text.trim_end(), format!(" {}", q[i]), "row {i}");
            assert_eq!(line.colour, Some(crate::theme::Role::Text));
            assert_eq!(line.runs, embers[i], "row {i}");
            assert_eq!(*hit, None);
        }
        // The menu row: the version at the left, a person reporting a bug reads it off the
        // screen, and `menu « ` closing the row.
        let (menu, hit) = &rows[4];
        assert!(menu.text.starts_with(&format!(" {VERSION} ")), "{menu:?}");
        assert!(
            menu.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "{menu:?}"
        );
        assert_eq!(menu.colour, Some(crate::theme::Role::Faint));
        assert_eq!(*hit, Some(Hit::Foot));

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
        assert_eq!(act(Hit::Foot, 2, &c, false), Some(Act::Site));
        assert_eq!(act(Hit::Foot, 1, &c, false), Some(Act::Collapse));

        // Folded to a strip there is no room for the logo, and the fold mark is the row.
        let (strip, hit) = collapse_row();
        assert_eq!(crate::width::width(&strip.text), COLLAPSED);
        assert!(!strip.text.contains('\u{25cf}'));
        assert_eq!(hit, Some(Hit::Foot));
        assert_eq!(act(Hit::Foot, 2, &c, true), Some(Act::Collapse));
    }

    /// A standing pet only adds rows above the words (the count box and a blank one), so the
    /// words sit in the same rows counted from the bottom whether the pet stands or gives its
    /// rows to the list.
    #[test]
    fn the_pet_adds_rows_above_the_words_and_moves_none_of_them() {
        for width in [SIDEBAR_MIN, WIDTH, SIDEBAR_MAX] {
            let gave_way = foot_rows(width, None, None, HubLink::Up);
            let stands = foot_rows(width, Some(0), None, HubLink::Up);
            assert_eq!(stands.len(), STAGE_FOOT_ROWS);
            let added = STAGE_FOOT_ROWS - gave_way.len();
            let blank_at = COUNT_BOX_ROWS;
            for (line, hit) in &stands[blank_at..added] {
                assert_eq!(line.text, " ".repeat(width), "{line:?}");
                assert_eq!(*hit, None);
            }
            assert_eq!(&stands[added..], &gave_way[..], "width {width}");
        }
    }

    /// The menu row shows the version and, right after it, the green online dot, while the
    /// tail keeps its place at the right; the link starts where the version does.
    #[test]
    fn the_menu_row_carries_the_version_then_the_online_dot() {
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            let rows = foot_rows(width, None, None, HubLink::Up);
            let (menu, hit) = rows.last().expect("the menu row");
            assert_eq!(*hit, Some(Hit::Foot));
            assert_eq!(crate::width::width(&menu.text), width);
            assert!(
                menu.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
                "{menu:?}"
            );
            let version = format!(" {VERSION}");
            assert!(menu.text.starts_with(&version), "{menu:?}");
            assert_eq!(version_link_at(width), Some(1));
            let dot_at = crate::width::width(&version) + 1;
            // The dot goes first when the row is tight: the version and the tail are kept.
            let room = dot_at + 1 + 1 + crate::width::width(&foot_tail(width)) <= width;
            if room {
                assert_eq!(menu.text.chars().nth(dot_at), Some(ONLINE), "{menu:?}");
                assert_eq!(menu.runs, vec![(dot_at, 1, crate::theme::Role::Done)]);
            } else {
                assert!(
                    !menu.text.contains(ONLINE) && menu.runs.is_empty(),
                    "{menu:?}"
                );
            }
            assert!(room || width < WIDTH, "the dot shows at the default width");
        }
        // No room for the version, no link and no dot hanging in the air.
        let tight = foot_rows(8, None, None, HubLink::Up);
        assert!(!tight.last().unwrap().0.text.contains(ONLINE));
        assert_eq!(version_link_at(8), None);
    }

    /// The dot says what the bridge says about the hub: green when reached, warm when a
    /// bridge is there and the hub is not, quiet when there is no bridge. The dot is drawn
    /// in all three, in the same cell.
    #[test]
    fn the_online_dot_follows_the_hub_state() {
        use crate::theme::Role;
        let dot = |hub| {
            let rows = foot_rows(WIDTH, None, None, hub);
            let menu = &rows.last().unwrap().0;
            assert!(menu.text.contains(ONLINE), "{menu:?}");
            menu.runs.clone()
        };
        let at = crate::width::width(&format!(" {VERSION}")) + 1;
        assert_eq!(dot(HubLink::Up), vec![(at, 1, Role::Done)]);
        assert_eq!(dot(HubLink::Down), vec![(at, 1, Role::Blocked)]);
        assert_eq!(dot(HubLink::Absent), vec![(at, 1, Role::Faint)]);
    }

    /// The link and the click cover the version word and nothing else: with an update notice
    /// after it (`0.8.0>0.9.0`) the notice is plain text, and a click anywhere else on the
    /// foot still folds the column.
    #[test]
    fn the_version_link_covers_only_the_version() {
        let rows = foot_rows(WIDTH, None, Some("9.9.9"), HubLink::Up);
        let text: Vec<char> = rows.last().unwrap().0.text.chars().collect();
        let at = version_link_in(WIDTH, Some("9.9.9")).expect("shown");
        let word: String = text[at..at + VERSION.len()].iter().collect();
        assert_eq!(word, VERSION);
        assert_eq!(text[at + VERSION.len()], '>', "the notice follows the link");
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        // 1-based columns: the version is columns 2 to 1 + its length.
        for col in 2..=1 + VERSION.len() {
            assert_eq!(act(Hit::Foot, col, &c, false), Some(Act::Site), "col {col}");
        }
        assert_eq!(act(Hit::Foot, 1, &c, false), Some(Act::Collapse));
        assert_eq!(
            act(Hit::Foot, 2 + VERSION.len(), &c, false),
            Some(Act::Collapse)
        );
        assert_eq!(
            act(Hit::Foot, 2, &c, true),
            Some(Act::Collapse),
            "folded strip"
        );
        // Where the row has no room for the version, nothing is a link.
        assert_eq!(version_link_in(8, None), None);
    }

    /// The logo is the q of `docs/brand/make.py` alone at every width: one dot a cell with a
    /// blank between, cream but for its ember tail, on the four rows right above the menu,
    /// one column in. The pet beside it fits every open column.
    #[test]
    fn the_foot_draws_the_q_alone_at_every_width() {
        for row in Q_LOGO {
            assert!(crate::width::width(row) <= Q_COLS, "{row:?}");
            assert!(row.chars().all(|c| c == ' ' || c == '\u{25cf}'), "{row:?}");
        }
        // The dots, row by row, as make.py places them: the ring, and the tail under its
        // right column.
        let dots = |row: &str| -> Vec<usize> {
            row.chars()
                .enumerate()
                .filter(|(_, c)| *c == '\u{25cf}')
                .map(|(at, _)| at)
                .collect()
        };
        assert_eq!(dots(Q_LOGO[0]), [0, 2, 4]);
        assert_eq!(dots(Q_LOGO[1]), [0, 4]);
        assert_eq!(dots(Q_LOGO[2]), [0, 2, 4]);
        assert_eq!(dots(Q_LOGO[3]), [4]);
        assert!(dots(Q_LOGO[Q_TAIL.0]).contains(&Q_TAIL.1));

        let brand = crate::theme::Role::Brand;
        let embers = [vec![], vec![], vec![], vec![(5, 1, brand)]];
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            for pet in [None, Some(0)] {
                let rows = foot_rows(width, pet, None, HubLink::Up);
                let logo = &rows[rows.len() - Q_FOOT_ROWS..rows.len() - 1];
                for (i, (line, hit)) in logo.iter().enumerate() {
                    assert_eq!(
                        line.text.trim_end(),
                        format!(" {}", Q_LOGO[i]),
                        "width {width}"
                    );
                    assert_eq!(crate::width::width(&line.text), width);
                    assert_eq!(line.colour, Some(crate::theme::Role::Text));
                    assert_eq!(line.runs, embers[i], "row {i}");
                    assert_eq!(*hit, None);
                }
            }
            assert!(avatar_fits(30, width, true), "width {width}");
        }
        const { assert!(Q_PET_WIDTH <= SIDEBAR_MIN) };
    }

    /// With a pet standing, every width from the narrowest to the widest keeps the column's
    /// promises: exactly `height` rows of exactly `width` columns, the menu row last, and
    /// the cells the pet is stamped over blank, so the pet never lands on a word.
    #[test]
    fn the_foot_keeps_its_shape_and_the_pet_its_room_at_every_width() {
        let pet = art_pet();
        let (cols, _) = avatar_overlay(&pet, true);
        let (spaces, agents) = sample();
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            for height in 0..=40 {
                let rows = rows12(
                    &spaces,
                    &agents,
                    1,
                    1,
                    height,
                    0,
                    View::Tree,
                    &seen,
                    &unseen,
                    NOW,
                    width,
                    Some(&pet),
                );
                assert_eq!(rows.len(), height, "width {width} height {height}");
                for (line, _) in &rows {
                    assert_eq!(crate::width::width(&line.text), width, "{line:?}");
                }
                if let Some((_, hit)) = rows.last() {
                    assert_eq!(*hit, Some(Hit::Foot), "width {width} height {height}");
                }
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
                    width,
                    &shut,
                );
                if !pet_shown(&c, height, true) {
                    continue;
                }
                // The overlay's cells: one based column `width - cols`, the foot's first
                // five rows.
                let from = width - cols - 1;
                for (line, _) in &rows[height - PET_FOOT_ROWS..height - 1] {
                    let under: String = line.text.chars().skip(from).take(cols).collect();
                    assert!(
                        under.chars().all(|c| c == ' '),
                        "width {width} height {height}: {line:?}"
                    );
                }
            }
        }
    }

    /// The words of the foot are the same rows whatever the pet: a Bare pet and a Mohawk
    /// paint the whole column identically, the pet's name is nowhere on it, and both pets
    /// stand on the menu row, five rows tall.
    #[test]
    fn the_words_rows_are_identical_for_two_different_pets() {
        let bare = qoop_runtime::avatar::Avatar {
            hair: "none".into(),
            ..art_pet()
        };
        let mohawk = art_pet();
        assert_ne!(bare.name(), mohawk.name());
        let (spaces, agents) = sample();
        let paint = |pet: &qoop_runtime::avatar::Avatar| {
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
                Some(pet),
            )
        };
        let (a, b) = (paint(&bare), paint(&mohawk));
        assert_eq!(a, b);
        for (line, _) in &a {
            assert!(!line.text.contains("Bare"), "{line:?}");
            assert!(!line.text.contains("Mohawk"), "{line:?}");
        }
        for pet in [&bare, &mohawk] {
            let (_, art) = avatar_overlay(pet, true);
            assert_eq!(art.len(), qoop_runtime::avatar::ART_ROWS);
            // The feet are on the pet's last row, the one right above the menu.
            assert!(art[art.len() - 1].contains('\u{2580}'), "{art:?}");
        }
    }

    /// The menu row is the column's last row at every height, the pet or not, down to a
    /// single row: a column too short for the foot keeps its last rows.
    #[test]
    fn the_menu_row_is_the_last_row_at_every_height() {
        let pet = art_pet();
        let (spaces, agents) = sample();
        for avatar in [None, Some(&pet)] {
            for height in 1..=40 {
                let rows = rows12(
                    &spaces,
                    &agents,
                    1,
                    1,
                    height,
                    0,
                    View::Tree,
                    &Seen::new(),
                    &Unseen::new(),
                    NOW,
                    WIDTH,
                    avatar,
                );
                let (last, hit) = rows.last().expect("a row");
                assert_eq!(*hit, Some(Hit::Foot), "height {height}: {last:?}");
                assert!(
                    last.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
                    "height {height}: {last:?}"
                );
            }
        }
    }

    /// A version or a notice that does not fit whole beside `menu « ` is not shown: a
    /// notice cut to `0.4.0>0.` reads as another version. The menu keeps its place.
    #[test]
    fn the_menu_row_never_cuts_a_version_or_a_notice() {
        let menu = |width: usize, update: Option<&str>| {
            foot_rows(width, None, update, HubLink::Up)
                .last()
                .map(|(line, _)| line.text.clone())
                .unwrap_or_default()
        };
        let tail = format!("{MENU_LABEL} \u{ab} ");
        let wide = menu(SIDEBAR_MAX, Some("0.10.0"));
        assert!(wide.starts_with(&format!(" {VERSION}>0.10.0 ")), "{wide:?}");
        // Exactly room for the version, a space and the tail: the notice gives way.
        let snug = crate::width::width(&format!(" {VERSION}")) + 1 + FOOT_TAIL;
        let fits = menu(snug, Some("0.10.0"));
        assert_eq!(fits, format!(" {VERSION} {tail}"));
        // One column less and the version goes too, never cut.
        let tight = menu(snug - 1, Some("0.10.0"));
        assert_eq!(tight.trim_start(), tail, "{tight:?}");
        assert_eq!(crate::width::width(&tight), snug - 1);
        let plain = menu(WIDTH, None);
        assert!(!plain.contains('>'), "{plain:?}");
    }

    fn art_pet() -> qoop_runtime::avatar::Avatar {
        qoop_runtime::avatar::Avatar {
            body: qoop_runtime::avatar::BODY.into(),
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
        assert!(avatar_fits(STAGE_FOOT_ROWS + 6, Q_PET_WIDTH, true));
        assert!(!avatar_fits(30, 30, false));
        assert!(!avatar_fits(30, Q_PET_WIDTH - 1, true));
        assert!(!avatar_fits(STAGE_FOOT_ROWS + 5, 30, true));
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

    /// With a pet and a list short enough to stand above it, the foot takes its nine rows
    /// (the count box, a blank one, the logo and the tail beside where the pet stamps, then
    /// the menu) and
    /// the column keeps its exact height. A list that needs those rows keeps them: the
    /// pet gives way, and no agent is pushed off the column for a picture.
    #[test]
    fn a_pet_takes_its_rows_only_when_the_list_still_fits() {
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
        // The rows of the foot never carry an agent: the pet's reservation, not
        // the list's, whatever the overlay stamps over their trailing columns.
        for (line, hit) in &rows[30 - STAGE_FOOT_ROWS..30] {
            assert!(!matches!(hit, Some(Hit::Terminal(_))), "{line:?}");
        }
        let (menu_row, hit) = &rows[29];
        assert!(
            menu_row.text.ends_with(&format!("{MENU_LABEL} \u{ab} ")),
            "{menu_row:?}"
        );
        assert_eq!(*hit, Some(Hit::Foot));
        // A crowd needs every row: the pet gives way, and the rows it would have reserved
        // over the foot the column keeps without it carry agents instead.
        let crowded = at(crowd());
        assert_eq!(crowded.len(), 30);
        let kept = foot_rows(WIDTH, None, None, HubLink::Up).len();
        for row in &crowded[30 - STAGE_FOOT_ROWS..30 - kept] {
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
        let freed = 30 - STAGE_FOOT_ROWS..30 - foot_rows(WIDTH, None, None, HubLink::Up).len();
        assert!(
            plain[freed.clone()].iter().all(carries_agents),
            "thirty agents must reach the rows a pet would have reserved: {:?}",
            &plain[freed]
        );
    }

    /// Overlay rows are the foot's first five, top first, so the pet stands on the menu.
    #[test]
    fn overlay_rows_stand_on_the_menu_row() {
        assert_eq!(avatar_overlay_rows(1, 30), vec![25, 26, 27, 28, 29]);
    }

    /// The overlay paints the pet in its own inks: five half-block rows cut to the columns
    /// any row lights, every row the same width so nothing skews, each reset at both ends,
    /// empty without colour.
    #[test]
    fn the_overlay_paints_the_pet_in_its_inks() {
        let (cols, rows) = avatar_overlay(&art_pet(), true);
        assert_eq!(rows.len(), qoop_runtime::avatar::ART_ROWS);
        assert!(cols > 0 && cols <= PET_COLS, "{cols}");
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
            assert!(row.starts_with("\x1b[0m"), "{row:?}");
            assert!(row.ends_with("\x1b[0m"), "{row:?}");
            let cells = strip_escapes(row);
            assert_eq!(crate::width::width(&cells), cols, "{row:?}");
            assert!(
                cells
                    .chars()
                    .all(|c| matches!(c, '\u{2580}' | '\u{2584}' | ' ')),
                "unaccounted cells: {row:?}"
            );
        }
        assert!(rows[0].contains(";5;212m"), "hair on top: {:?}", rows[0]);
        assert!(rows.iter().any(|r| r.contains("48;5;33")), "body: {rows:?}");
        assert_eq!(avatar_overlay(&art_pet(), false), (0, Vec::new()));
        // The widest pet there is still fits the room the foot keeps for it.
        for hair in qoop_runtime::avatar::HAIR_STYLES {
            let pet = qoop_runtime::avatar::Avatar {
                hair: hair.into(),
                ..art_pet()
            };
            let (cols, _) = avatar_overlay(&pet, true);
            assert!(cols <= PET_COLS, "{hair} is {cols} wide");
        }
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
        // Every agent draws one name row, with its second row under it.
        assert_eq!(
            texts
                .iter()
                .filter(|t| !t.trim_start().starts_with('\u{2514}'))
                .filter(|t| t.contains("claude") || t.contains("codex"))
                .count(),
            9,
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
            cell_mark(Role::Background, false, 0),
        ];
        for tick in 1..=64u64 {
            let mark = cell_mark(Role::Working, true, tick);
            assert!(
                !others.contains(&mark),
                "tick {tick} painted another state's picture: {mark:?}"
            );
        }
    }

    /// The one-cell mark keeps the grid's promises: two columns, one braille glyph and a
    /// space, a picture per state, and no working frame that is another state's picture.
    #[test]
    fn the_one_cell_mark_is_one_glyph_and_never_another_states_picture() {
        use crate::theme::Role;
        let calm: Vec<String> = [
            Role::Idle,
            Role::Sleeping,
            Role::Blocked,
            Role::Done,
            Role::Failed,
            Role::Background,
        ]
        .iter()
        .map(|&role| one_cell_mark(role, false, 0))
        .collect();
        let unique: std::collections::BTreeSet<_> = calm.iter().collect();
        assert_eq!(
            unique.len(),
            calm.len(),
            "two states share a picture: {calm:?}"
        );
        for tick in 0..=16u64 {
            let mark = one_cell_mark(Role::Working, true, tick);
            assert!(!calm.contains(&mark), "tick {tick}: {mark:?}");
            assert_eq!(crate::width::width(&mark), 2, "{mark:?}");
            assert!(
                mark.ends_with(' ')
                    && ('\u{2800}'..='\u{28ff}').contains(&mark.chars().next().unwrap())
            );
        }
        assert_ne!(
            one_cell_mark(Role::Working, true, 0),
            one_cell_mark(Role::Working, true, 2),
            "the working mark never turned"
        );
    }

    /// The background dot goes once round the ring in eight frames (four on one cell), a
    /// frame every two ticks as working's breath, and no frame of it is another state's
    /// picture: one dot, never the centre's (idle), never working's plus, never blocked's
    /// ring. On one cell its lower left frame is idle's picture, and only the ink (sky
    /// against grey) tells them apart there.
    #[test]
    fn the_background_dot_orbits_and_is_never_another_states_picture() {
        use crate::theme::Role;
        let grid: Vec<String> = (0..16u64)
            .map(|t| cell_mark(Role::Background, true, t))
            .collect();
        let cells: std::collections::BTreeSet<&String> = grid.iter().collect();
        assert_eq!(cells.len(), 8, "{grid:?}");
        assert_eq!(grid[0], grid[1], "a frame lasts two ticks");
        assert_ne!(grid[1], grid[2]);
        assert_eq!(
            cell_mark(Role::Background, true, 16),
            grid[0],
            "eight frames round"
        );
        assert_eq!(
            cell_mark(Role::Background, false, 9),
            grid[0],
            "still it rests on the first"
        );
        let others: Vec<String> = [
            Role::Idle,
            Role::Sleeping,
            Role::Blocked,
            Role::Done,
            Role::Failed,
        ]
        .iter()
        .map(|&r| cell_mark(r, false, 0))
        .collect();
        for tick in 0..=64u64 {
            let orbit = cell_mark(Role::Background, true, tick);
            assert!(!others.contains(&orbit), "tick {tick}: {orbit:?}");
            for t in 0..=64u64 {
                assert_ne!(
                    orbit,
                    cell_mark(Role::Working, true, t),
                    "tick {tick} against working {t}"
                );
            }
        }

        let one: Vec<String> = (0..8u64)
            .map(|t| one_cell_mark(Role::Background, true, t))
            .collect();
        let cells: std::collections::BTreeSet<&String> = one.iter().collect();
        assert_eq!(cells.len(), 4, "{one:?}");
        let idle = one_cell_mark(Role::Idle, false, 0);
        assert_eq!(
            one.iter().filter(|m| **m == idle).count(),
            2,
            "the lower left frame only"
        );
        let others: Vec<String> = [Role::Sleeping, Role::Blocked, Role::Done, Role::Failed]
            .iter()
            .map(|&r| one_cell_mark(r, false, 0))
            .collect();
        for tick in 0..=16u64 {
            let orbit = one_cell_mark(Role::Background, true, tick);
            assert!(!others.contains(&orbit), "tick {tick}: {orbit:?}");
            for t in 0..=16u64 {
                assert_ne!(orbit, one_cell_mark(Role::Working, true, t));
            }
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
        let row = second_row(&moving, &working, BOX_LEAD + 2, Some("qoop".into()));
        assert_eq!(row.0.text.trim_end(), "   \u{2514} claude \u{b7} qoop");
        assert!(
            !row.0
                .runs
                .iter()
                .any(|(_, _, r)| matches!(r, crate::theme::Role::Shimmer(_))),
            "the worktree shimmered: {:?}",
            row.0
        );
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
        let lines = render(&spaces, &agents, 2, 99, 10);
        let mark = cell_mark(crate::theme::of("failed"), false, 0);
        // Column zero is a blank, ahead of the mark.
        let prefix = format!(" {mark}claude");
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
        assert_eq!(
            hit(&spaces, &agents, 2, 99, 10, row),
            Some(Hit::Terminal(2))
        );
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
                .any(|l| l.text.starts_with(&format!(" {mark}"))),
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
    /// the `+` is the last column, and a rule folds anywhere off its chips.
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
                let settings: String = (1..=width)
                    .filter(|col| act(Hit::Foot, *col, &c, false) == Some(Act::SettingsPage))
                    .filter_map(|col| foot.chars().nth(col - 1))
                    .collect();
                let named = if width >= SETTINGS_FROM {
                    SETTINGS_LABEL
                } else {
                    ""
                };
                assert_eq!(settings, named, "width {width}: {foot:?}");
                for col in [1, width - 1, width] {
                    assert_eq!(
                        act(Hit::Foot, col, &c, false),
                        Some(Act::Collapse),
                        "width {width} col {col}"
                    );
                }

                // A rule folds anywhere off its controls, in the tree only, and only when
                // there is something under it to fold. Its controls answer on every cell
                // of their chips, which are the same whatever the name and the count are.
                // Closed, the controls are `+` and the toggle, the last two cells before the
                // blank last column, whatever the name and the count are.
                for col in 1..=width {
                    let want = |id| {
                        if col + 3 == width {
                            Act::New(id)
                        } else if col + 1 == width {
                            Act::Drawer(id)
                        } else if id == 1 && view == View::Tree {
                            Act::Fold(id)
                        } else {
                            Act::Space(id)
                        }
                    };
                    assert_eq!(act(Hit::Space(1), col, &c, false), Some(want(1)), "space 1");
                    assert_eq!(act(Hit::Space(3), col, &c, false), Some(want(3)), "space 3");
                }
                assert_eq!(act(Hit::Space(1), 1, &c, true), Some(Act::Space(1)));
                assert_eq!(act(Hit::Foot, 1, &c, true), Some(Act::Collapse));
                assert_eq!(act(Hit::Terminal(3), 1, &c, false), Some(Act::Terminal(3)));
            }
        }
        // The fold mark before the name and the name itself both fold.
        let rows = tree_rows(&spaces, &agents, 1, 14, &Seen::new(), &Shut::new());
        let head: String = rule_row(&rows, 1).text.chars().take(NAME_AT + 1).collect();
        assert_eq!(head, "\u{25be}c");
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
        assert_eq!(act(Hit::Space(1), NAME_AT, &c, false), Some(Act::Fold(1)));
        assert_eq!(
            act(Hit::Space(1), NAME_AT + 1, &c, false),
            Some(Act::Fold(1))
        );
    }

    /// No breath between the title and the tree any more: the first rule is itself a
    /// separator, so the row under the title is already the first space.
    #[test]
    fn the_tree_opens_right_under_the_title() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "idle")];
        let rows = tree_rows(&spaces, &agents, 1, 10, &Seen::new(), &Shut::new());
        assert_eq!(rows[0].1, Some(Hit::Tabs));
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert_eq!(rows[2].1, Some(Hit::Space(1)));
        assert_eq!(rows[3].1, Some(Hit::Terminal(1)));
    }

    /// A blocked agent is listed in the inbox at the top, where a person looks first,
    /// and still draws its row under its space: the inbox points, it does not move.
    #[test]
    fn a_blocked_agent_is_in_the_inbox_and_still_drawn_under_its_space() {
        let spaces = vec![space(1, "app")];
        let agents = vec![agent(1, 1, "codex", "blocked")];
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = opened(column(
            &spaces,
            &agents,
            1,
            9,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let rows = rows_folded(&c, 16, None);
        let texts = texts(&rows.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>());
        assert!(texts[1].starts_with(" \u{282f}\u{2807}codex"), "{texts:?}");
        assert_eq!(rows[1].1, Some(Hit::Next(Some(1))));
        assert_eq!(rows[TITLE_ROW].1, Some(Hit::Title));
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
        // The inbox's all list is the same order, unwalked: blocked, failed, the finish.
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = opened(column(
            &spaces,
            &agents,
            1,
            9,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let listed: Vec<Option<Hit>> = rows_folded(&c, 40, None)[1..4]
            .iter()
            .map(|(_, h)| *h)
            .collect();
        assert_eq!(
            listed,
            vec![
                Some(Hit::Next(Some(4))),
                Some(Hit::Next(Some(2))),
                Some(Hit::Next(Some(1)))
            ]
        );
        // Calm the fleet down and the list says nobody is waiting, reaching nobody.
        let mut calm = agents.clone();
        calm[1].state = "idle".into();
        calm[3].state = "working".into();
        let seen = Seen::from([(1, 0)]);
        let c = opened(column(
            &spaces,
            &calm,
            1,
            9,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let rows = rows_folded(&c, 20, None);
        assert_eq!(rows[1].0.text.trim_end(), " nothing needs you");
        assert_eq!(rows[1].1, Some(Hit::Next(None)));
        assert_eq!(rows[TITLE_ROW].1, Some(Hit::Title));
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
        let pet = qoop_runtime::avatar::Avatar {
            body: qoop_runtime::avatar::BODY.into(),
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
                "Edit /Users/me/qoop/crates/qoop-tui/src/sidebar.rs",
                "editing sidebar.rs",
            ),
            ("Write src/new.rs", "editing new.rs"),
            ("apply_patch src/a.rs", "editing a.rs"),
            ("Read /x/y/README.md", "reading README.md"),
            ("read_file /x/src/main.rs", "reading main.rs"),
            ("Bash cargo test -p qoop-tui", "cargo test -p qoop-tui"),
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
            ("Bash cd /Users/me/qoop && cargo test", "cargo test"),
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

    /// A working agent says what it is doing on a row of its own under its name, and only
    /// that; a waiting one says what it wants in the blocked colour. A calm agent keeps its
    /// tab on that row, whatever its last tool was.
    #[test]
    fn activity_rides_a_sub_row_in_the_tree() {
        let spaces = vec![space(1, "app")];
        let mut working = agent(1, 1, "claude", "working");
        working.activity = Some("Edit src/sidebar_rows_and_the_foot.rs".into());
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
        // What it does and nothing else: the tab gives the whole row to it.
        assert_eq!(
            unbox(&one[1].text),
            "   \u{2514} editing sidebar_rows_and_the_fo\u{2026}"
        );
        assert_eq!(one[1].colour, Some(crate::theme::Role::Faint));
        let two = under(2);
        assert_eq!(unbox(&two[1].text), "   \u{2514} wants: bun test");
        // Quiet grey, with what it wants in the blocked colour.
        assert_eq!(two[1].colour, Some(crate::theme::Role::Faint));
        assert!(
            two[1].runs.contains(&(5, 15, crate::theme::Role::Blocked)),
            "{:?}",
            two[1].runs
        );
        // A calm agent keeps its tab row, without the stale activity.
        let three = under(3);
        assert_eq!(three.len(), 2, "{texts:?}");
        assert_eq!(unbox(&three[1].text), "   \u{2514} gemini");
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{line:?}");
        }
    }

    /// An agent whose turn is over reads idle while its footer says shells, monitors or
    /// subagents still run: its row says so, quietly, and the state word stays `idle`. One
    /// with nothing running keeps its tab row, and a busy one is not taken for a free one in
    /// the inbox either.
    #[test]
    fn an_idle_agent_with_background_work_says_so() {
        let spaces = vec![space(1, "app")];
        let mut busy = agent(1, 1, "claude", "idle");
        busy.background = 3;
        let mut one = agent(2, 1, "claude", "idle");
        one.background = 1;
        let free = agent(3, 1, "claude", "idle");
        let agents = vec![busy.clone(), one, free];
        let rows = tree_rows(&spaces, &agents, 9, 30, &Seen::new(), &Shut::new());
        let under = |terminal: u32| -> Vec<&Line> {
            rows.iter()
                .skip_while(|(_, h)| *h != Some(Hit::Title))
                .filter(|(_, h)| *h == Some(Hit::Terminal(terminal)))
                .map(|(l, _)| l)
                .collect()
        };
        assert_eq!(unbox(&under(1)[1].text), "   \u{2514} 3 background tasks");
        assert_eq!(unbox(&under(2)[1].text), "   \u{2514} 1 background task");
        assert_eq!(unbox(&under(3)[1].text), "   \u{2514} claude");
        assert_eq!(busy.state, "idle", "the state is not rewritten");

        // The same agent finished while the person was away: the inbox row carries the count.
        let item = |background| Item {
            terminal: 1,
            role: crate::theme::Role::Done,
            name: "claude".into(),
            space: 1,
            waited: None,
            background,
        };
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let column = column_armed(
            &spaces,
            &agents,
            1,
            0,
            0,
            View::Tree,
            &seen,
            &unseen,
            0,
            WIDTH,
            &shut,
            None,
        );
        assert!(item_row(&column, &item(3)).text.contains("bg:3"));
        assert!(!item_row(&column, &item(0)).text.contains("bg:"));
    }
    /// Idle with background work is its own state, derived from the count: the mark and the
    /// ink are the background's in the tree and in the strip, and one finished unread is not news
    /// while its job runs: not in the done list or the queue, no sage. The word stays idle.
    #[test]
    fn idle_with_background_work_is_a_state_of_its_own_everywhere() {
        use crate::theme::Role;
        let mut busy = agent(7, 1, "claude", "idle");
        busy.background = 2;
        busy.generation = Some(3);
        let free = agent(8, 1, "claude", "idle");
        assert_eq!(role_of(&busy), Role::Background);
        assert_eq!(role_of(&free), Role::Idle);
        let mut working = busy.clone();
        working.state = "working".into();
        assert_eq!(role_of(&working), Role::Working);

        let spaces = vec![space(1, "app")];
        let (seen, shut) = (Seen::new(), Shut::new());
        let mut unseen = Unseen::new();
        unseen.insert((7, 3));
        // Finished unread, which would be the done sage for a free agent.
        let agents = vec![busy.clone(), free];
        let rows = tree_rows(&spaces, &agents, 1, 10, &seen, &shut);
        assert_eq!(
            tree_row(&rows, 7).indicator,
            Some((BOX_LEAD, 2, Role::Background))
        );
        let rows = tree_rows(&spaces, &agents[1..], 1, 10, &seen, &shut);
        assert_eq!(tree_row(&rows, 8).indicator.map(|i| i.2), Some(Role::Idle));

        let column = column(
            &spaces,
            &agents,
            1,
            0,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        // Not news while the job runs: not in the done list nor the queue, no sage line.
        assert!(inbox_items(&column, InboxTab::Done).is_empty());
        assert!(inbox_items(&column, InboxTab::All).is_empty());
        assert!(!is_new(&busy, 0, &seen, &unseen));
        assert!(queue(&agents, 0, &seen, &unseen).is_empty());

        let strip = rows_collapsed(&column, 12);
        let mark = |terminal: u32| {
            strip
                .iter()
                .find(|(_, h)| *h == Some(Hit::Terminal(terminal)))
                .map(|(l, _)| (l.text.clone(), l.colour))
                .unwrap()
        };
        assert_eq!(mark(7).1, Some(Role::Background));
        assert!(mark(7).0.contains(BACKGROUND_DOT), "{:?}", mark(7).0);
        assert_eq!(mark(8).1, None);
        assert!(!mark(8).0.contains(BACKGROUND_DOT));

        // The job ends: plain idle with its unseen entry still set, the finish it is.
        let mut ended = agents.clone();
        ended[0].background = 0;
        assert!(is_new(&ended[0], 0, &seen, &unseen));
        let column = self::column(
            &spaces,
            &ended,
            1,
            0,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        );
        let roles: Vec<Role> = inbox_items(&column, InboxTab::Done)
            .iter()
            .map(|i| i.role)
            .collect();
        assert_eq!(roles, [Role::Done]);
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
            moves(&c(&open, 1, View::Tree), false) == Some(Motion::Spin),
            "an open working agent moves"
        );
        assert!(
            moves(&c(&folded, 1, View::Tree), false).is_none(),
            "a folded one moves nothing"
        );
        assert!(
            moves(&c(&folded, 2, View::Tree), false) == Some(Motion::Spin),
            "the keyboard's own always draws"
        );
        assert!(
            moves(&c(&folded, 1, View::Priority), false) == Some(Motion::Spin),
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
        assert!(
            moves(&still, false).is_none(),
            "nothing working, nothing moving"
        );
    }

    /// The orbit asks for the loop only where it is drawn and seen: an idle agent with a job
    /// running in an open space does, however long it has been idle (work running is never
    /// stale), a folded space and the folded strip do not, the keyboard's own row does, and a
    /// working agent beside it still takes the fast clock.
    #[test]
    fn a_background_orbit_moves_the_column_only_where_it_is_seen() {
        let spaces = vec![space(1, "app"), space(2, "dub")];
        let busy = |terminal, workspace, age| {
            let mut a = agent_at(terminal, workspace, "claude", "idle", age);
            a.background = 1;
            a
        };
        let (seen, unseen) = (Seen::new(), Unseen::new());
        let (open, folded) = (Shut::new(), Shut::from([2]));
        let motion = |agents: &[AgentRow], shut, focused, strip| {
            let mut c = column(
                &spaces,
                agents,
                1,
                focused,
                0,
                View::Tree,
                &seen,
                &unseen,
                NOW,
                WIDTH,
                shut,
            );
            c.now_unix = NOW;
            moves(&c, strip)
        };
        let live = vec![busy(1, 1, 60)];
        assert_eq!(motion(&live, &open, 9, false), Some(Motion::Orbit));
        assert_eq!(
            motion(&live, &open, 9, true),
            None,
            "the strip shows a still circle"
        );
        let hidden = vec![busy(2, 2, 60)];
        assert_eq!(
            motion(&hidden, &folded, 9, false),
            None,
            "a folded space draws no row"
        );
        assert_eq!(
            motion(&hidden, &folded, 2, false),
            Some(Motion::Orbit),
            "the keyboard's row draws"
        );
        let stale = vec![busy(1, 1, ACTIVITY_STALE_SECS + 60)];
        assert_eq!(
            motion(&stale, &open, 9, false),
            Some(Motion::Orbit),
            "work running is never stale, so the row keeps going round"
        );
        // And it stays lit: a stale idle row without work dims, this one does not.
        let mut old_free = agent_at(2, 1, "claude", "idle", ACTIVITY_STALE_SECS + 60);
        old_free.background = 0;
        let both_old = vec![stale[0].clone(), old_free];
        let rows = tree_rows(&spaces, &both_old, 9, 40, &Seen::new(), &Shut::new());
        assert_eq!(
            tree_row(&rows, 1).indicator.map(|i| i.2),
            Some(crate::theme::Role::Background)
        );
        assert_ne!(tree_row(&rows, 1).colour, Some(crate::theme::Role::Faint));
        assert_eq!(tree_row(&rows, 2).colour, Some(crate::theme::Role::Faint));
        let mut both = live.clone();
        both.push(agent(3, 1, "claude", "working"));
        assert_eq!(motion(&both, &open, 9, false), Some(Motion::Spin));
        let free = vec![agent(1, 1, "claude", "idle")];
        assert_eq!(motion(&free, &open, 9, false), None);
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
        assert!(row.text.ends_with(" 4% 212M c x "), "{:?}", row.text);
        assert!(
            row.runs
                .iter()
                .any(|(_, _, r)| *r == crate::theme::Role::Faint)
        );
        assert!(
            tree_row(&wide, 2).text.ends_with(" 3.0G c x "),
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

    /// The fleet the inbox tests share: one of each state, the keyboard on none of them.
    fn inbox_fleet() -> (Vec<WorkspaceRow>, Vec<AgentRow>, Unseen) {
        let mut spaces = vec![space(1, "app"), space(2, "dub")];
        spaces[1].sleeping = vec![SleepingTab {
            terminal: 20,
            lead: None,
            title: None,
            agent: Some("claude".into()),
            session: Some("old review".into()),
            cwd: "/tmp".into(),
        }];
        let mut blocked = agent(1, 1, "claude", "blocked");
        blocked.agent_title = Some("sidebar redesign".into());
        blocked.last_active_at_unix = Some(NOW - 12);
        let failed = agent(2, 1, "codex", "failed");
        let done = agent(3, 2, "claude", "idle");
        let working = agent(4, 2, "codex", "working");
        let mut unseen = Unseen::new();
        unseen.insert((3, gen_of(&done)));
        (spaces, vec![blocked, failed, done, working], unseen)
    }

    /// The inbox lists everyone waiting under its tab row, the chosen tab filled, and its
    /// height never depends on how many wait: nobody, one or six, the title and the tree
    /// under it stay on the same rows.
    #[test]
    fn the_inbox_lists_who_waits_and_never_moves_the_tree() {
        use crate::theme::Role;
        let (spaces, agents, unseen) = inbox_fleet();
        let (seen, shut) = (Seen::new(), Shut::new());
        let at = |agents: &[AgentRow], unseen: &Unseen| -> Vec<Row> {
            let c = opened(column(
                &spaces,
                agents,
                1,
                99,
                0,
                View::Tree,
                &seen,
                unseen,
                NOW,
                WIDTH,
                &shut,
            ));
            rows_folded(&c, 40, None)
        };
        let rows = at(&agents, &unseen);
        assert_eq!(rows[0].1, Some(Hit::Tabs));
        assert!(
            rows[0].0.text.starts_with("\u{25be}  inbox "),
            "{:?}",
            rows[0].0
        );
        assert_eq!(rows[0].0.fill, Some((2, 7, Role::Focused)));
        // Blocked, failed, the unread finish: the queue's order.
        let listed: Vec<Option<Hit>> = rows[1..4].iter().map(|(_, h)| *h).collect();
        assert_eq!(
            listed,
            vec![
                Some(Hit::Next(Some(1))),
                Some(Hit::Next(Some(2))),
                Some(Hit::Next(Some(3)))
            ]
        );
        let first = &rows[1].0;
        assert!(
            first.text.starts_with(" \u{282f}\u{2807}sidebar redesign"),
            "{first:?}"
        );
        assert!(first.text.trim_end().ends_with("app 12s"), "{first:?}");
        assert!(
            first.runs.contains(&(1, 2, Role::Blocked)),
            "{:?}",
            first.runs
        );
        for (line, _) in &rows {
            assert_eq!(crate::width::width(&line.text), WIDTH, "{line:?}");
        }
        let title = |rows: &[Row]| rows.iter().position(|(_, h)| *h == Some(Hit::Title));
        assert_eq!(title(&rows), Some(TITLE_ROW));
        // Nobody waiting.
        let mut calm = agents.clone();
        for a in &mut calm {
            a.state = "working".into();
        }
        let none = at(&calm, &Unseen::new());
        assert_eq!(none[1].0.text.trim_end(), " nothing needs you");
        assert_eq!(none[1].1, Some(Hit::Next(None)));
        assert_eq!(title(&none), title(&rows));
        // Six waiting: still four rows, and a thumb on a track says there are more.
        let six: Vec<AgentRow> = (1..=6).map(|t| agent(t, 1, "claude", "blocked")).collect();
        let many = at(&six, &Unseen::new());
        assert_eq!(title(&many), title(&rows));
        let bar: String = many[1..=INBOX_ROWS]
            .iter()
            .map(|(l, _)| l.text.chars().last().unwrap_or(' '))
            .collect();
        assert!(
            bar.contains('\u{2503}') && bar.contains('\u{2502}'),
            "{bar:?}"
        );
        assert!(
            !rows[1..=INBOX_ROWS]
                .iter()
                .any(|(l, _)| l.text.ends_with('\u{2503}'))
        );
    }

    /// Every tab cell picks its tab and the caret folds; each tab lists its own agents, the
    /// working and the sleeping ones without a wait; folded, the inbox is one row of counts.
    #[test]
    fn the_tabs_pick_their_lists_and_fold_to_one_row_of_counts() {
        let (spaces, agents, unseen) = inbox_fleet();
        let (seen, shut) = (Seen::new(), Shut::new());
        let c = opened(column(
            &spaces,
            &agents,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let cells = tab_cells(&c, &tab_counts(&c));
        assert_eq!(cells.iter().filter(|(t, _, _)| t.is_some()).count(), 6);
        for (tab, from, _) in &cells {
            let want = match tab {
                Some(tab) => Some(Act::InboxTab(*tab)),
                None if *from == 0 => Some(Act::InboxFold),
                None => None,
            };
            assert_eq!(
                act(Hit::Tabs, from + 1, &c, false),
                want,
                "{tab:?} at {from}"
            );
        }
        let list = |tab: InboxTab| -> Vec<Row> {
            let c = Column {
                inbox: Inbox { tab, ..c.inbox },
                ..c
            };
            rows_folded(&c, 40, None)
        };
        let working = list(InboxTab::Working);
        assert!(
            working[0]
                .0
                .text
                .starts_with(&format!("\u{25be}  {}1 ", tab_mark(InboxTab::Working))),
            "{:?}",
            working[0].0
        );
        assert_eq!(working[1].1, Some(Hit::Next(Some(4))));
        assert!(
            working[1].0.text.trim_end().ends_with("dub"),
            "{:?}",
            working[1].0
        );
        let asleep = list(InboxTab::Asleep);
        assert_eq!(asleep[1].1, Some(Hit::Next(Some(20))));
        assert!(asleep[1].0.text.contains("old review"), "{:?}", asleep[1].0);
        let wants = list(InboxTab::Wants);
        assert_eq!(wants[1].1, Some(Hit::Next(Some(1))));
        assert_eq!(wants[2].1, Some(Hit::Next(None)));
        // Folded: one row, then the title.
        let folded = Column {
            inbox: Inbox {
                folded: true,
                ..c.inbox
            },
            ..c
        };
        let rows = rows_folded(&folded, 40, None);
        assert!(
            rows[0].0.text.starts_with("\u{25b8} inbox"),
            "{:?}",
            rows[0].0
        );
        assert_eq!(rows[1].1, Some(Hit::Title));
        assert_eq!(act(Hit::Tabs, 4, &folded, false), Some(Act::InboxFold));
        let (tab, from, _) = tab_cells(&folded, &tab_counts(&folded))
            .into_iter()
            .find(|(t, _, _)| t.is_some())
            .expect("a count");
        assert_eq!(
            act(Hit::Tabs, from + 1, &folded, false),
            Some(Act::InboxTab(tab.unwrap()))
        );
    }

    /// At every height the inbox takes its tab row and the list rows that still leave the
    /// title and a row of the tree, none below that; the priority view keeps it too.
    #[test]
    fn the_inbox_gives_way_in_a_short_column_and_stays_in_the_priority_view() {
        let (spaces, agents, unseen) = inbox_fleet();
        let (seen, shut) = (Seen::new(), Shut::new());
        let c = opened(column(
            &spaces,
            &agents,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let foot = foot_rows(WIDTH, None, None, HubLink::Up).len();
        for h in 1..40 {
            let rows = rows_folded(&c, h, None);
            assert_eq!(rows.len(), h);
            let above = rows
                .iter()
                .take_while(|(_, hit)| *hit != Some(Hit::Title))
                .count();
            let room = h.saturating_sub(foot);
            let inbox = if room < 3 {
                0
            } else {
                1 + INBOX_ROWS.min(room.saturating_sub(4).max(1))
            };
            // The open inbox's edge, when it and the title both fit.
            let edge = usize::from(inbox > 1 && inbox + 1 < room);
            let want = inbox + edge;
            if rows.iter().any(|(_, hit)| *hit == Some(Hit::Title)) {
                assert_eq!(above, want, "height {h}");
            }
        }
        let flat = Column {
            view: View::Priority,
            ..c
        };
        let rows = rows_folded(&flat, 40, None);
        assert_eq!(rows[0].1, Some(Hit::Tabs));
        assert_eq!(rows[1].1, Some(Hit::Next(Some(1))));
        assert_eq!(rows[TITLE_ROW].1, Some(Hit::Title));
    }

    /// The wheel over the list stops at both ends; the tab walk wraps; a drag never asks
    /// for fewer than one row or more than the column can give.
    #[test]
    fn the_inbox_scrolls_within_its_list_and_the_drag_is_bounded() {
        let spaces = vec![space(1, "app")];
        let six: Vec<AgentRow> = (1..=6).map(|t| agent(t, 1, "claude", "blocked")).collect();
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = opened(column(
            &spaces,
            &six,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        assert_eq!(inbox_wheeled(&c, 40, None, true), 0);
        c.inbox.scroll = inbox_wheeled(&c, 40, None, false);
        assert_eq!(c.inbox.scroll, 1);
        c.inbox.scroll = 9;
        assert_eq!(inbox_wheeled(&c, 40, None, false), 6 - INBOX_ROWS);
        let rows = rows_folded(&c, 40, None);
        assert_eq!(rows[INBOX_ROWS].1, Some(Hit::Next(Some(6))));
        // Scrolled to the end, the thumb sits at the end of the track.
        assert!(
            rows[INBOX_ROWS].0.text.ends_with('\u{2503}'),
            "{:?}",
            rows[INBOX_ROWS].0
        );
        assert!(rows[1].0.text.ends_with('\u{2502}'), "{:?}", rows[1].0);
        assert_eq!(InboxTab::All.step(true), InboxTab::Asleep);
        assert_eq!(InboxTab::Asleep.step(false), InboxTab::All);
        let foot = foot_rows(WIDTH, None, None, HubLink::Up).len();
        assert_eq!(inbox_room(&c, 40, None), 40 - foot - 4);
        assert_eq!(inbox_room(&c, 3, None), 0);
    }

    /// A tab clicked again where it is already open puts the inbox back; folded, a click on
    /// a tab opens that tab.
    #[test]
    fn the_open_tab_clicked_again_goes_back_to_the_inbox() {
        let open = Inbox {
            tab: InboxTab::Working,
            ..Inbox::default()
        };
        assert_eq!(open.picked(InboxTab::Working), InboxTab::All);
        assert_eq!(open.picked(InboxTab::Wants), InboxTab::Wants);
        assert_eq!(Inbox::default().picked(InboxTab::All), InboxTab::All);
        let folded = Inbox {
            folded: true,
            ..open
        };
        assert_eq!(folded.picked(InboxTab::Working), InboxTab::Working);
    }

    /// Under an open inbox, its edge: ember corners turning up at both ends and the rule
    /// between, one column in from each side, then the title.
    #[test]
    fn an_open_inbox_ends_in_its_edge_before_the_title() {
        use crate::theme::Role;
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.inbox = Inbox::default();
        let rows = rows_folded(&c, 40, None);
        let title = rows
            .iter()
            .position(|(_, h)| *h == Some(Hit::Title))
            .expect("the title");
        assert!(title > 1, "an open inbox above it");
        let edge = &rows[title - 1];
        assert_eq!(edge.1, None, "the edge answers nothing");
        let text: Vec<char> = edge.0.text.chars().collect();
        assert_eq!(
            (text[1], text[WIDTH - 2]),
            ('\u{2570}', '\u{256f}'),
            "{:?}",
            edge.0
        );
        assert!(
            text[2..WIDTH - 2].iter().all(|c| *c == '\u{2500}'),
            "{:?}",
            edge.0
        );
        assert!(edge.0.runs.contains(&(1, 1, Role::Brand)));
        assert!(edge.0.runs.contains(&(WIDTH - 2, 1, Role::Brand)));
        assert_ne!(
            edge.0.colour,
            Some(Role::Rule),
            "not the panel rule the column joins"
        );
        assert_eq!(rows[TITLE_ROW].1, Some(Hit::Title));
    }

    /// Every tab wears its state's ink, empty or not, and the chosen one says its mark and
    /// count, not a word.
    #[test]
    fn the_tabs_keep_their_inks_and_the_chosen_one_has_no_word() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.inbox = Inbox {
            tab: InboxTab::Failed,
            ..Inbox::default()
        };
        let (row, _) = tab_row(&c);
        assert!(!row.text.contains("failed"), "{row:?}");
        assert!(row.text.contains(" inbox "), "{row:?}");
        for tab in &InboxTab::ORDER[1..] {
            if *tab == InboxTab::Failed {
                continue;
            }
            assert!(
                row.runs.iter().any(|(_, _, r)| *r == tab.role()),
                "{tab:?} keeps its ink: {row:?}"
            );
        }
    }

    /// The pet never stands in the inbox, empty or not: the inbox keeps its words at the left
    /// and is only as tall as it was dragged, and the pet is in the foot over the menu row.
    #[test]
    fn an_empty_inbox_has_no_pet_and_the_foot_holds_it() {
        let spaces = vec![space(1, "app")];
        let calm = vec![agent(1, 1, "claude", "working")];
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let c = opened(column(
            &spaces,
            &calm,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        let pet = qoop_runtime::avatar::Avatar {
            body: qoop_runtime::avatar::BODY.into(),
            hair: "flat".into(),
            eyes: "squint".into(),
            hair_ink: 155,
            body_ink: 33,
        };
        let rows = rows_folded(&c, 40, Some(&pet));
        assert_eq!(rows[1].0.text.trim_end(), " nothing needs you");
        assert_eq!(rows.len(), 40);
        // The inbox is the height it is dragged to, not the pet's: the title sits right
        // under the default three list rows and the edge.
        assert_eq!(rows[TITLE_ROW].1, Some(Hit::Title));
        assert_eq!(TITLE_ROW, 1 + INBOX_ROWS + 1);
        assert!(pet_shown(&c, 40, true));
    }

    /// The box over the pet shows the inbox count and nothing else, in the rows right above
    /// the pet's, flush with the pet's right edge; warm while something waits.
    #[test]
    fn the_count_box_stands_over_the_pet_and_shows_only_the_count() {
        use crate::theme::Role;
        let spaces = vec![space(1, "app")];
        let (seen, unseen, shut) = (Seen::new(), Unseen::new(), Shut::new());
        let box_of = |agents: &[AgentRow]| {
            let c = opened(column(
                &spaces,
                agents,
                1,
                99,
                0,
                View::Tree,
                &seen,
                &unseen,
                NOW,
                WIDTH,
                &shut,
            ));
            assert!(pet_shown(&c, 40, true));
            rows_folded(&c, 40, Some(&art_pet()))
        };
        let rows = box_of(&[agent(1, 1, "claude", "working")]);
        let top = 40 - STAGE_FOOT_ROWS;
        let text = |at: usize| rows[at].0.text.trim_end().to_string();
        let across = 3;
        let pad = " ".repeat(WIDTH - across - 3);
        assert_eq!(
            text(top),
            format!("{pad}\u{256d}\u{2500}\u{2500}\u{2500}\u{256e}")
        );
        assert_eq!(text(top + 1), format!("{pad}\u{2502} 0 \u{2502}"));
        assert_eq!(
            text(top + 2),
            format!("{pad}\u{2570}\u{2500}\u{2500}\u{2500}\u{256f}")
        );
        assert!(rows[top + 1].0.runs.contains(&(WIDTH - 5, 3, Role::Faint)));
        // The box ends on the row the pet's rows start on.
        assert_eq!(top + COUNT_BOX_ROWS, 40 - PET_FOOT_ROWS);
        assert!(rows[top..top + 3].iter().all(|(_, hit)| hit.is_none()));
        // Three agents waiting is a three, in the warm ink.
        let waiting: Vec<AgentRow> = (1..=3).map(|t| agent(t, 1, "claude", "blocked")).collect();
        let rows = box_of(&waiting);
        assert_eq!(
            rows[top + 1].0.text.trim_end(),
            format!("{pad}\u{2502} 3 \u{2502}")
        );
        assert!(
            rows[top + 1]
                .0
                .runs
                .contains(&(WIDTH - 5, 3, Role::Blocked))
        );
        // Where the pet gives the rows up, the box goes with it.
        let long: Vec<AgentRow> = (1..=20).map(|t| agent(t, 1, "claude", "working")).collect();
        let c = opened(column(
            &spaces,
            &long,
            1,
            99,
            0,
            View::Tree,
            &seen,
            &unseen,
            NOW,
            WIDTH,
            &shut,
        ));
        assert!(!pet_shown(&c, 40, true));
        assert!(
            rows_folded(&c, 40, Some(&art_pet()))
                .iter()
                .all(|(line, _)| !line.text.contains('\u{256d}'))
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
        assert!(app.text.starts_with("\u{25b8}app (3)"), "{:?}", app.text);
        // The count sits beside the name it counts, and the block keeps its columns.
        assert!(app.text.contains("(3)"), "the count keeps every agent");
        assert_eq!(columns_from(&app.text, WIDTH - 4, 1), "+", "{:?}", app.text);
        assert_eq!(
            columns_from(&app.text, WIDTH - 2, 1),
            "\u{22ef}",
            "{:?}",
            app.text
        );

        // Off the space, the fold hides all three.
        let rows = tree_rows(&spaces, &agents, 4, 14, &Seen::new(), &shut);
        assert_eq!(order(&rows), vec![4]);
        // And the open space next to it is untouched.
        assert!(
            rule_row(&rows, 2).text.starts_with("\u{25be}dub"),
            "{:?}",
            rule_row(&rows, 2).text
        );
    }

    /// A fold never hides a blocked agent silently: a folded rule's count takes the colour
    /// of its most urgent child that needs a person, and stays quiet while every child is
    /// calm. The name keeps the space's colour either way, and no line or indicator is
    /// drawn: the count is the one place the signal lives.
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
        // The colour of the name and of the count, read off the painted runs.
        let marks = |rule: &Line| -> (Role, Role) {
            assert_eq!(rule.indicator, None, "{rule:?}");
            let name = rule.accent.expect("the name").2;
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
            (name, count)
        };
        let shut = Shut::from([1]);
        let ink = crate::theme::space_of(1);
        assert_eq!(
            marks(&paint(&["idle", "failed", "blocked"], &shut)),
            (ink, Role::Blocked)
        );
        assert_eq!(
            marks(&paint(&["idle", "failed"], &shut)),
            (ink, Role::Failed)
        );
        assert_eq!(
            marks(&paint(&["idle", "done"], &shut)),
            (ink, Role::Done),
            "an unread finish is news"
        );
        // Nothing under it wants a person, so the count keeps the quiet grey whether the
        // rule is folded or open.
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
        assert_eq!(unbox(&tree_row(&rows, 1).text), " \u{2810}\u{2800}sh");
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
            !rows
                .iter()
                .any(|(l, h)| *h != Some(Hit::Tabs) && l.text.contains('\u{25b8}')),
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
        wide.agent_title = Some("程".repeat(20));
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
        assert_eq!(unbox(&rows[0]), " \u{2818}\u{2802}claude");
        // Cut sessions: the mark and the ellipsis frame the text.
        assert!(unbox(&rows[1]).ends_with('…'), "{:?}", rows[1]);
        assert!(
            unbox(&rows[1]).starts_with(" \u{2818}\u{2802}Implement"),
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
        // Different: the tab, then the worktree.
        assert_eq!(
            row(Some("/home/u/app-feat-x")),
            "   \u{2514} claude \u{b7} app-feat-x"
        );
        // Trailing slash is not a name.
        assert_eq!(
            row(Some("/home/u/app-feat-x/")),
            "   \u{2514} claude \u{b7} app-feat-x"
        );
        // Same name in any case: the bare tab.
        assert_eq!(row(Some("/home/u/app")), "   \u{2514} claude");
        assert_eq!(row(Some("/home/u/app")), "   \u{2514} claude");
        // Root, empty, blank: no basename, the bare tab.
        assert_eq!(row(Some("/")), "   \u{2514} claude");
        assert_eq!(row(Some("")), "   \u{2514} claude");
        assert_eq!(row(Some("   ")), "   \u{2514} claude");
        // No cwd at all: the bare tab.
        assert_eq!(row(None), "   \u{2514} claude");
        // A tilde and dots are names and pass through untouched.
        assert_eq!(row(Some("~")), "   \u{2514} claude \u{b7} ~");
        assert_eq!(row(Some("/home/u/.")), "   \u{2514} claude \u{b7} .");
        // The row is quiet grey throughout; only the elbow and the dot wear the rule, and
        // nothing is drawn beside it: the space is told by the colour of the title above.
        let line = worktree_row("app", Some("/home/u/app-feat-x"), "claude");
        assert_eq!(line.colour, Some(crate::theme::Role::Faint));
        let rule = crate::theme::Role::Rule;
        assert_eq!(line.runs, vec![(3, 1, rule), (12, 1, rule)]);
        assert_eq!(run_text(&line, 14, 10), "app-feat-x");
        assert_eq!(worktree_row("app", None, "claude").runs, vec![(3, 1, rule)]);
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
        assert_eq!(texts[3], " \u{2810}\u{2800}claude");
        assert_eq!(texts[4], "   \u{2514} claude \u{b7} x");
        assert!(texts[5].is_empty(), "{texts:?}");
        assert_eq!(texts[6], rule_row(&rows, 2).text.trim_end());
        assert_eq!(texts[7], " \u{2810}\u{2800}codex");
        assert_eq!(texts[8], "   \u{2514} codex \u{b7} x");
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
            unbox(&line.text).starts_with("   \u{2514} claude \u{b7} a-very"),
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
            unbox(&line.text).starts_with("   \u{2514} claude \u{b7} 程"),
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
    /// the name before it gives up the fold mark or the count, the accent stays on the
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
        // The name gave way, not the block: the controls keep their columns.
        assert_eq!(
            columns_from(&rule.text, WIDTH - 4, 1),
            "+",
            "{:?}",
            rule.text
        );
        assert_eq!(
            columns_from(&rule.text, WIDTH - 2, 1),
            "\u{22ef}",
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
            unbox(&tree_row(&rows, 2).text).starts_with(" \u{2818}\u{2802}behind docker"),
            "{:?}",
            tree_row(&rows, 2).text
        );
        assert!(
            unbox(&tree_row(&rows, 3).text).starts_with(" \u{2818}\u{2802}Muse"),
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
        // The least that holds the title, one row of list and the foot, then more.
        let least = foot_rows(WIDTH, None, None, HubLink::Up).len() + 2;
        for height in [least, least + 1, 10, 14, 30] {
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
                        lines[0].text.starts_with(['\u{25b8}', '\u{25be}']),
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
        for width in [SIDEBAR_MIN, 30, WIDTH, SIDEBAR_MAX] {
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
                lines.iter().any(|l| l.text.contains(" app")),
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
    /// even with an unseen entry sitting there.
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

    /// A sleeping space is a box like any other, saying so after its name, with a `wake`
    /// that brings its tabs back. Folded it draws no tab; unfolded its agent tabs are faint
    /// rows wearing the sleeping mark, each reaching its tab, and its rule folds it like a
    /// live space's. A sleeping shell tab draws no row at all, the same as
    /// a live one.
    #[test]
    fn a_sleeping_space_draws_folded_with_its_mark_and_dim_tabs() {
        let mut asleep = space(2, "api");
        asleep.asleep = true;
        asleep.sleeping = vec![
            SleepingTab {
                lead: None,
                terminal: 7,
                session: None,
                title: None,
                agent: Some("claude".into()),
                cwd: "/src/api".into(),
            },
            SleepingTab {
                lead: None,
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
        // Folded and asleep: no word for it, the wake control and the colour say so.
        assert!(head.text.starts_with("\u{25b8}api"), "{:?}", head.text);
        assert!(!head.text.contains("sleeping"), "{:?}", head.text);
        assert!(!head.text.contains("api z"), "{:?}", head.text);

        assert_eq!(crate::width::width(&head.text), WIDTH);
        // The name keeps its space's colour one step quieter, and `wake` peeks left of `+`
        // with the drawer closed, in the controls' grey, a blank between them.
        assert_eq!(
            head.accent.expect("the name's run").2,
            crate::theme::space_calm_of(2),
            "{:?}",
            head.runs
        );
        let wake = (WIDTH - 10, WORD_W);
        assert_eq!(
            columns_from(&head.text, wake.0, wake.1),
            flat("wake"),
            "{:?}",
            head.text
        );
        assert!(
            head.runs
                .contains(&(wake.0, wake.1, crate::theme::Role::Faint)),
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
        assert_eq!(dim.colour, Some(crate::theme::space_calm_of(2)));
        assert_eq!(
            dim.indicator,
            Some((BOX_LEAD, 2, crate::theme::Role::Sleeping)),
            "the sleeping mark, not a state dot"
        );
        assert_eq!(unbox(&dim.text), " \u{2812}\u{2802}claude");

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
                lead: None,
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
        // The conversation's name first, as the row had it awake.
        assert!(named(Some("mine"), Some("fix login")).contains("fix login"));
        assert!(named(Some("mine"), None).contains("mine"));
        assert!(named(None, None).contains("claude"));
    }

    /// One space in a known state: `held` idle agents running in it, or asleep with
    /// `held` agent tabs to bring back.
    fn space_in_state(
        name: &str,
        held: usize,
        asleep: bool,
        closable: bool,
    ) -> (Vec<WorkspaceRow>, Vec<AgentRow>) {
        let space = WorkspaceRow {
            keep_awake: false,
            id: 1,
            name: name.to_string(),
            terminals: held,
            asleep,
            sleeping: if asleep {
                (0..held as u32)
                    .map(|terminal| SleepingTab {
                        lead: None,
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
            ink: None,
            link: None,
        };
        let agents = (1..=held as u32)
            .filter(|_| !asleep)
            .map(|n| agent(n, 1, "claude", "idle"))
            .collect();
        (vec![space], agents)
    }

    /// The first rule a column paints.
    fn first_rule(c: &Column) -> Line {
        // A height no list of test-sized tabs can overflow, so the rule is on screen
        // rather than scrolled: this is about how a rule is drawn, not about the window.
        rows_folded(c, 4096, current_avatar().as_ref())
            .into_iter()
            .find(|(_, h)| matches!(h, Some(Hit::Space(_))))
            .map(|(l, _)| l)
            .unwrap_or_default()
    }

    /// The rule as the column paints it, at a width, for a space in a known state.
    fn rule_at(width: usize, name: &str, held: usize, asleep: bool, closable: bool) -> String {
        let (spaces, agents) = space_in_state(name, held, asleep, closable);
        let none = (Seen::new(), Unseen::new(), Shut::new());
        first_rule(&clicks(&spaces, &agents, View::Tree, width, &none)).text
    }

    /// A rule of `c`, the way the painter gets it.
    fn rule_styled(c: &Column, id: i64) -> Line {
        let space = c.spaces.iter().find(|s| s.id == id).expect("the space");
        let kids: Vec<&AgentRow> = c
            .agents
            .iter()
            .filter(|a| a.workspace == id && a.agent)
            .collect();
        rule(c, space, &kids, !c.shut.contains(&id))
    }

    /// A control as the column draws it: around one glyph or in the sleep control's width.
    fn flat(word: &str) -> String {
        chip(word, if word.chars().count() > 1 { WORD_W } else { 1 })
    }

    /// Where a space's rule draws a control, as the painter and the click mapper lay it out.
    fn cell_of(c: &Column, id: i64, ctl: Ctl) -> Option<(usize, usize)> {
        let space = c.spaces.iter().find(|s| s.id == id).expect("the space");
        block(c, space)
            .cells
            .into_iter()
            .find(|(k, _, _)| *k == ctl)
            .map(|(_, at, w)| (at, w))
    }

    /// `cols` columns of `text` from the zero-based column `at`, counting columns and not
    /// characters, so a full width name cannot shift what is read.
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

    /// No room for the name is a column with no name in it, not a name that runs one
    /// column over. A narrow rule whose count has taken the whole budget would otherwise
    /// draw the ellipsis past it and push every control a column right of where `act`
    /// looks for it, so the painted chip would be one column from the click that means it.
    #[test]
    fn a_name_with_no_room_is_left_out_rather_than_over_running() {
        assert_eq!(
            shorten_middle("qoop", 0),
            "",
            "an ellipsis is a column of its own"
        );
        assert_eq!(crate::width::width(&shorten_middle("qoop", 0)), 0);
        assert_eq!(shorten_middle("qoop", 1), "\u{2026}");
        assert_eq!(shorten_middle("qoop", 4), "qoop");
        assert_eq!(shorten_middle("qoop", 3), "qo\u{2026}");
        // The narrowest column the interface draws pays for the fold column, the block and
        // the blank last column, and still leaves a name the columns it needs to stay one.
        assert!(SIDEBAR_MIN.saturating_sub(BLOCK_TAIL_MAX) >= MIN_NAME_COLS);
        assert_eq!(SIDEBAR_MIN, 24);
    }

    /// Every rule this column can draw, at every width it can be drawn at, with a name
    /// and a count that stress the arithmetic: the block keeps its columns, the row is
    /// exactly the column, and the name gives way before the controls do. Busy, idle and
    /// asleep draw their chips in the same places, so no control moves when a space
    /// changes state.
    ///
    /// The sweep is the test. Three widths would only prove three of them.
    #[test]
    fn the_rule_holds_its_columns_at_every_width_and_count() {
        let names = [
            "q",
            "qoop",
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
                            // Closed: `+` and the toggle in the last two cells, and an
                            // asleep space's wake peeking a blank before `+`.
                            assert_eq!(columns_from(&row, width - 4, 1), "+", "{label} {row:?}");
                            assert_eq!(
                                columns_from(&row, width - 2, 1),
                                "\u{22ef}",
                                "{label} {row:?}"
                            );
                            if asleep && held > 0 {
                                assert_eq!(
                                    columns_from(&row, width - 10, WORD_W),
                                    flat("wake"),
                                    "{label} {row:?}"
                                );
                            }
                            assert!(row.ends_with(' '), "{label} {row:?}");
                        }
                    }
                }
            }
        }
    }

    /// The count sits beside the name it counts, asleep or awake; a sleeping space wears
    /// no word for it, since its wake control already says so.
    #[test]
    fn a_rule_says_what_it_holds_beside_its_name() {
        let row = rule_at(WIDTH, "qoop", 4, false, true);
        assert!(row.contains("qoop (4)"), "{row:?}");
        // Asleep, the same count and no word.
        let row = rule_at(WIDTH, "qoop", 3, true, true);
        assert!(row.contains("qoop (3)"), "{row:?}");
        assert!(!row.contains("sleeping"), "{row:?}");
        // Narrower, where the word no longer fits, the count stays, so the name never
        // starves for a state; at the narrowest column the name is cut and the count stays.
        let row = rule_at(30, "qoop", 3, true, true);
        assert!(row.contains("qoop (3)"), "{row:?}");
        let row = rule_at(SIDEBAR_MIN, "qoop-mobile", 3, true, true);
        assert!(row.contains("\u{2026} (3)"), "{row:?}");
    }

    /// A space with tabs asleep beside awake ones says both while there is room, then the
    /// count alone, then only its name: each part of the slot holds only while the name
    /// keeps its columns.
    #[test]
    fn a_partly_sleeping_space_gives_up_its_slot_a_part_at_a_time() {
        let (mut spaces, agents) = space_in_state("qoop-mobile", 3, false, true);
        spaces[0].sleeping = space_in_state("qoop-mobile", 2, true, true).0[0]
            .sleeping
            .clone();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let at = |width| first_rule(&clicks(&spaces, &agents, View::Tree, width, &none)).text;
        assert!(
            at(SIDEBAR_MAX).contains("qoop-mobile (3, 2z)"),
            "{:?}",
            at(60)
        );
        // With the drawer closed the narrowest column still holds the whole slot, the
        // name cut short for it; with it open, the slot gives up its sleeping part first.
        let mut drawn = clicks(&spaces, &agents, View::Tree, SIDEBAR_MAX, &none);
        drawn.icons = true;
        let zzz = format!("(3, 2{})", crate::icons::ASLEEP);
        assert!(first_rule(&drawn).text.contains(&zzz), "{zzz:?}");
        let narrow = at(SIDEBAR_MIN);
        assert!(narrow.contains("\u{2026} (3, 2z)"), "{narrow:?}");
        let mut c = clicks(&spaces, &agents, View::Tree, 30, &none);
        c.drawer = Some((1, DRAWER));
        let open = first_rule(&c).text;
        assert!(open.contains(" (3) ") && !open.contains("2z"), "{open:?}");
    }

    /// The session's own default carries no close, because the daemon refuses to close it
    /// and a control that cannot work is worse than no control.
    #[test]
    fn a_space_that_cannot_be_closed_says_so_by_drawing_no_close() {
        let open = |name: &str, closable| {
            let (spaces, agents) = space_in_state(name, 2, false, closable);
            let none = (Seen::new(), Unseen::new(), Shut::new());
            let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
            c.drawer = Some((1, DRAWER));
            cell_of(&c, 1, Ctl::Close).map(|(at, _)| columns_from(&first_rule(&c).text, at, 1))
        };
        assert_eq!(open("default", false), None);
        assert_eq!(open("dub", true).as_deref(), Some("x"));
    }

    /// The controls answer on every cell of the chips the rule painted them in, at every
    /// width, which is what stops a click landing beside a control that moved.
    #[test]
    fn a_rule_answers_its_controls_where_it_painted_them() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for width in SIDEBAR_MIN..=SIDEBAR_MAX {
            for shown in 0..=DRAWER {
                let mut c = clicks(&spaces, &agents, View::Tree, width, &none);
                c.drawer = Some((1, shown));
                let row = first_rule(&c).text;
                assert_eq!(crate::width::width(&row), width, "{width} {shown}: {row:?}");
                // Read off the painted text, so this fails if paint and hit test disagree.
                let drawn = block(&c, &spaces[0]);
                for (ctl, at, w) in &drawn.cells {
                    assert_eq!(
                        columns_from(&row, *at, *w),
                        chip(&drawn.word(*ctl), *w),
                        "{ctl:?} at {width} {shown}: {row:?}"
                    );
                    let want = match ctl {
                        Ctl::Toggle => Act::Drawer(1),
                        Ctl::New => Act::New(1),
                        Ctl::Sleep => Act::Sleep(1),
                        Ctl::Wake => Act::Wake(1),
                        Ctl::Cup => Act::KeepAwake(1),
                        Ctl::Settings => Act::Settings(1),
                        Ctl::Close => Act::Close(1),
                        Ctl::Yes | Ctl::No | Ctl::Ask => unreachable!("not armed"),
                    };
                    for col in *at..at + w {
                        assert_eq!(act(Hit::Space(1), col + 1, &c, false), Some(want));
                    }
                }
                // `+` and the toggle never move, open or closed.
                assert_eq!(cell_of(&c, 1, Ctl::Toggle), Some((width - 2, 1)));
                assert_eq!(cell_of(&c, 1, Ctl::New), Some((width - 4, 1)));
            }
        }
        // Open, the drawer reads from the toggle out by how often a hand needs it: the moon,
        // the cup, settings, then the close apart from them.
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.drawer = Some((1, DRAWER));
        let mut cells = block(&c, &spaces[0]).cells;
        cells.sort_by_key(|(_, at, _)| std::cmp::Reverse(*at));
        let order: Vec<Ctl> = cells.iter().map(|(ctl, _, _)| *ctl).collect();
        assert_eq!(
            order,
            [
                Ctl::Toggle,
                Ctl::New,
                Ctl::Sleep,
                Ctl::Cup,
                Ctl::Settings,
                Ctl::Close
            ]
        );
        let (settings, _) = cell_of(&c, 1, Ctl::Settings).expect("settings");
        let (close, _) = cell_of(&c, 1, Ctl::Close).expect("close");
        assert_eq!(settings - close, 3, "a blank more before the close");
    }

    /// A space with nothing running in it draws no sleep chip, so there is no column that
    /// answers with a sleep the daemon would refuse, nor with a wake.
    #[test]
    fn a_space_that_cannot_sleep_does_not_answer_with_a_sleep() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        for col in 1..=WIDTH {
            let got = act(Hit::Space(3), col, &c, false);
            assert!(
                !matches!(got, Some(Act::Sleep(_) | Act::Wake(_))),
                "the empty space offered {got:?} at column {col}"
            );
        }
    }

    /// A space running only shells draws `sleep` as one running agents does, and the chip
    /// sleeps it: the daemon puts any running tab to sleep, so the paint and the click ask
    /// the same question of every row of the space.
    #[test]
    fn a_shell_only_space_draws_and_answers_sleep() {
        let spaces = vec![space(1, "app")];
        let agents = vec![shell(2, 1)];
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.drawer = Some((1, DRAWER));
        let row = first_rule(&c).text;
        let (at, w) = cell_of(&c, 1, Ctl::Sleep).expect("a sleep");
        assert_eq!(columns_from(&row, at, w), flat("sleep"), "{row:?}");
        assert_eq!(act(Hit::Space(1), at + 1, &c, false), Some(Act::Sleep(1)));
        // Nothing to fold: shells have no rows in the column.
        assert!(row.starts_with(" app"), "{row:?}");
    }

    /// An armed rule asks its question in the chips its sleep and its close were, and
    /// answers nothing else: a row mid question does not fold, navigate or sleep.
    #[test]
    fn an_armed_rule_answers_yes_and_no_and_nothing_else() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for width in [SIDEBAR_MIN, WIDTH, SIDEBAR_MAX] {
            let mut c = clicks(&spaces, &agents, View::Tree, width, &none);
            c.armed = Some(1);
            let row = first_rule(&c).text;
            // `y` where the close was, because a second click where the question was asked
            // is the click a hand makes, and `no` over `+` and the toggle.
            let yes = cell_of(&c, 1, Ctl::Yes).expect("y");
            let mut open = clicks(&spaces, &agents, View::Tree, width, &none);
            open.drawer = Some((1, DRAWER));
            assert_eq!(cell_of(&open, 1, Ctl::Close), Some(yes), "{row:?}");
            assert_eq!(columns_from(&row, yes.0, 1), "y", "{row:?}");
            let (no, _) = cell_of(&c, 1, Ctl::No).expect("no");
            assert_eq!(columns_from(&row, no, 2), "no", "{row:?}");
            for col in 1..=width {
                let want = if col == yes.0 + 1 {
                    Act::ConfirmClose(1)
                } else {
                    Act::CancelClose
                };
                assert_eq!(
                    act(Hit::Space(1), col, &c, false),
                    Some(want),
                    "column {col} of {width}"
                );
            }
        }
        // The question is never dropped, not even at the narrowest column: it is beside `y`.
        for width in [SIDEBAR_MIN, WIDTH] {
            let mut c = clicks(&spaces, &agents, View::Tree, width, &none);
            c.armed = Some(1);
            let row = first_rule(&c).text;
            assert!(row.contains("y close?") && row.contains(" no "), "{row:?}");
            assert!(row.starts_with("\u{25be}"), "{row:?}");
        }
    }

    /// With icons the rule draws the moon, the cup and the cog in one column each, and a click
    /// on each lands on what it says, read off the paint.
    #[test]
    fn with_icons_the_controls_are_one_column_each_and_answer_where_painted() {
        use crate::icons::{CUP, LESS, MORE, SETTINGS, SLEEP, WAKE};
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        for width in [SIDEBAR_MIN, WIDTH, SIDEBAR_MAX] {
            let mut c = clicks(&spaces, &agents, View::Tree, width, &none);
            c.icons = true;
            // Closed: `+` and the dots.
            let row = first_rule(&c).text;
            assert_eq!(
                columns_from(&row, width - 2, 1),
                MORE.to_string(),
                "{row:?}"
            );
            // Open: each control one column, the dots turned into the arrow that closes.
            c.drawer = Some((1, DRAWER));
            let row = first_rule(&c).text;
            assert_eq!(crate::width::width(&row), width, "{row:?}");
            for (ctl, glyph, want) in [
                (Ctl::Toggle, LESS, Act::Drawer(1)),
                (Ctl::Sleep, SLEEP, Act::Sleep(1)),
                (Ctl::Cup, CUP, Act::KeepAwake(1)),
                (Ctl::Settings, SETTINGS, Act::Settings(1)),
            ] {
                let (at, w) = cell_of(&c, 1, ctl).expect("drawn");
                assert_eq!(
                    (columns_from(&row, at, w), w),
                    (glyph.to_string(), 1),
                    "{row:?}"
                );
                assert_eq!(act(Hit::Space(1), at + 1, &c, false), Some(want), "{width}");
            }
        }
        // A sleeping space's full moon peeks while the drawer is closed, and wakes it.
        let (spaces, agents) = space_in_state("qoop", 2, true, true);
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.icons = true;
        let row = first_rule(&c).text;
        let (at, _) = cell_of(&c, 1, Ctl::Wake).expect("wake peeks");
        assert_eq!(row.chars().nth(at), Some(WAKE), "{row:?}");
        assert_eq!(act(Hit::Space(1), at + 1, &c, false), Some(Act::Wake(1)));
    }

    /// The cup is grey until a person keeps the space or the pane awake, then the brand ink;
    /// an agent in a kept space shows its cup a step quieter: held by the space, not by it.
    #[test]
    fn a_kept_cup_is_full() {
        let (mut spaces, mut agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let cup_role = |spaces: &[WorkspaceRow], agents: &[AgentRow]| {
            let mut c = clicks(spaces, agents, View::Tree, WIDTH, &none);
            c.drawer = Some((1, DRAWER));
            let rule = first_rule(&c);
            let (at, _) = cell_of(&c, 1, Ctl::Cup).expect("the cup");
            let space = rule.runs.iter().find(|(a, _, _)| *a == at).map(|r| r.2);
            let rows = rows_folded(&c, 20, None);
            let row = tree_row(&rows, 1);
            let pane = row
                .runs
                .iter()
                .find(|(a, _, _)| *a == WIDTH - 4)
                .map(|r| r.2);
            (space, pane)
        };
        use crate::theme::Role::{Brand, BrandCalm, Faint};
        assert_eq!(cup_role(&spaces, &agents), (Some(Faint), Some(Faint)));
        agents[0].keep_awake = true;
        assert_eq!(cup_role(&spaces, &agents), (Some(Faint), Some(Brand)));
        agents[0].keep_awake = false;
        spaces[0].keep_awake = true;
        assert_eq!(cup_role(&spaces, &agents), (Some(Brand), Some(BrandCalm)));
        // Kept, the cup peeks with the drawer closed, full.
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        let (at, _) = cell_of(&c, 1, Ctl::Cup).expect("the cup peeks");
        assert!(first_rule(&c).runs.contains(&(at, 1, Brand)));
    }

    /// With icons an asleep tab wears the zzz, and a space of another machine the remote mark
    /// and its link's dot in its link's colour, its name pushed over by exactly those and a
    /// blank each.
    #[test]
    fn with_icons_asleep_tabs_and_remote_spaces_wear_their_marks() {
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let (spaces, agents) = space_in_state("qoop", 1, true, true);
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.icons = true;
        let asleep = rows_folded(&c, 20, None)
            .into_iter()
            .find(|(_, h)| matches!(h, Some(Hit::Terminal(_))))
            .map(|(l, _)| l.text)
            .expect("the asleep tab");
        assert_eq!(
            asleep.chars().nth(1),
            Some(crate::icons::ASLEEP),
            "{asleep:?}"
        );
        let (mut spaces, agents) = space_in_state("mars:qoop", 1, false, true);
        spaces[0].link = Some(qoop_runtime::control::LinkState::Up);
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.icons = true;
        let rule = first_rule(&c);
        assert!(
            rule.text.starts_with(&format!(
                "\u{25be}{} {ONLINE} mars:qoop",
                crate::icons::REMOTE
            )),
            "{:?}",
            rule.text
        );
        assert!(rule.runs.contains(&(NAME_AT, 1, crate::theme::Role::Done)));
        assert!(
            rule.runs
                .contains(&(NAME_AT + 2, 1, crate::theme::Role::Done))
        );
        assert_eq!(rule.accent.map(|a| a.0), Some(NAME_AT + 4));
        c.icons = false;
        assert!(
            first_rule(&c)
                .text
                .starts_with(&format!("\u{25be}{ONLINE} mars:qoop")),
            "the dot without the icon"
        );
    }

    /// An agent's first row ends in a grey `x`, in the column the space's own `x` is in, and
    /// a click there, and only there, is that agent's close.
    #[test]
    fn an_agent_row_carries_its_close_under_the_space_close() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        let rows = rows_folded(&c, 20, None);
        let row = tree_row(&rows, 1);
        let close = WIDTH - 2;
        assert_eq!(row.text.chars().nth(close), Some('x'), "{:?}", row.text);
        // The cup two columns before it, the click there the pane's keep awake.
        assert_eq!(row.text.chars().nth(close - 2), Some('c'), "{:?}", row.text);
        assert!(on_agent_cup(close - 1, 1, &c));
        assert!(!on_agent_cup(close, 1, &c));
        assert!(
            row.runs.contains(&(close, 1, crate::theme::Role::Faint)),
            "{:?}",
            row.runs
        );
        assert!(on_agent_close(close + 1, 1, &c));
        assert!(!on_agent_close(close, 1, &c));
        let flat = clicks(&spaces, &agents, View::Priority, WIDTH, &none);
        assert!(
            !on_agent_close(close + 1, 1, &flat),
            "the priority view draws none"
        );
    }

    /// A sleeping space keeps a way back on its rule: `wake`, in the chip `sleep` is in on
    /// an awake space, waking every tab of it.
    #[test]
    fn a_sleeping_space_keeps_the_only_way_back() {
        let (spaces, agents) = space_in_state("qoop", 3, true, true);
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        let rule = first_rule(&c);
        let (at, w) = cell_of(&c, 1, Ctl::Wake).expect("wake, closed");
        assert_eq!(columns_from(&rule.text, at, w), flat("wake"), "{rule:?}");
        assert!(rule.runs.contains(&(at, w, crate::theme::Role::Faint)));
        assert_eq!(act(Hit::Space(1), at + 1, &c, false), Some(Act::Wake(1)));
        // An awake space with nothing in it has no moon even with the drawer out: the
        // daemon would refuse.
        let (spaces, agents) = space_in_state("dub", 0, false, true);
        let mut c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        c.drawer = Some((1, DRAWER));
        assert_eq!(cell_of(&c, 1, Ctl::Sleep), None);
        assert_eq!(cell_of(&c, 1, Ctl::Wake), None);
    }

    /// The column before the name says open or folded, quietly, and only on a space that
    /// has something to fold: agents, or sleeping tabs.
    #[test]
    fn the_fold_mark_shows_only_where_a_space_folds() {
        use crate::theme::Role;
        let mut asleep = space_in_state("api", 1, true, true).0.remove(0);
        asleep.id = 2;
        let spaces = vec![space(1, "app"), asleep, space(3, "dub"), space(4, "sh")];
        let agents = vec![agent(1, 1, "claude", "idle"), shell(5, 4)];
        let mark = |shut: Shut| {
            let none = (Seen::new(), Unseen::new(), shut);
            let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
            [1, 2, 3, 4].map(|id| {
                let rule = rule_styled(&c, id);
                let first = rule.text.chars().next().unwrap_or_default();
                let faint = rule.runs.first() == Some(&(0, 1, Role::Faint));
                assert_eq!(faint, first != ' ', "{rule:?}");
                first
            })
        };
        assert_eq!(mark(Shut::new()), ['\u{25be}', '\u{25be}', ' ', ' ']);
        assert_eq!(mark(Shut::from([1, 2])), ['\u{25b8}', '\u{25b8}', ' ', ' ']);
    }

    /// A space of another machine is drawn with its machine before its name and no chip,
    /// whatever its link, behind a dot in the colour of that link: green while connected,
    /// dim while parked, warm when it does not answer. The dot needs no icon font.
    #[test]
    fn a_remote_space_is_named_with_its_machine_and_no_chip() {
        use crate::theme::Role;
        for (link, name, ink) in [
            (LinkState::Up, "ubox:webshop", Role::Done),
            (LinkState::Parked, "ubox:webshop", Role::Faint),
            (LinkState::Down, "ubox:webshop (connecting)", Role::Blocked),
        ] {
            let mut row = space(1, name);
            row.link = Some(link);
            let spaces = vec![row];
            let none = (Seen::new(), Unseen::new(), Shut::new());
            let c = clicks(&spaces, &[], View::Tree, SIDEBAR_MAX, &none);
            let rule = rule_styled(&c, 1);
            assert!(
                rule.text.starts_with(&format!(" {ONLINE} {name}")),
                "{rule:?}"
            );
            assert!(rule.runs.contains(&(NAME_AT, 1, ink)), "{rule:?}");
        }
        // A space of this machine has no link, so no dot.
        let spaces = vec![space(1, "ubox:webshop")];
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &[], View::Tree, SIDEBAR_MAX, &none);
        assert!(rule_styled(&c, 1).text.starts_with(" ubox:webshop"));
    }

    /// A control is its word alone, in every terminal: no cap glyph a
    /// terminal might not draw, and nothing drawn round it.
    #[test]
    fn a_rule_paints_its_controls_as_bare_words() {
        let (spaces, agents) = sample();
        let none = (Seen::new(), Unseen::new(), Shut::new());
        let c = clicks(&spaces, &agents, View::Tree, WIDTH, &none);
        let line = rule_styled(&c, 1);
        assert!(
            !line
                .text
                .contains(['\u{e0b6}', '\u{e0b4}', '\u{2590}', '\u{258c}']),
            "{line:?}"
        );
        assert!(line.text.ends_with("+ \u{22ef} "), "{line:?}");
    }

    /// A sleeping space keeps its own colour one step quieter, so spaces asleep side by side
    /// are still told apart; the word and the `wake` chip say the state.
    #[test]
    fn a_sleeping_rule_keeps_its_space_colour_one_step_quieter() {
        let spaces = vec![WorkspaceRow {
            keep_awake: false,
            id: 1,
            name: "qoop".into(),
            terminals: 3,
            asleep: true,
            sleeping: (0..3)
                .map(|terminal| SleepingTab {
                    lead: None,
                    terminal,
                    session: None,
                    title: None,
                    agent: Some("claude".into()),
                    cwd: "/tmp".into(),
                })
                .collect(),
            closable: true,
            ink: None,
            link: None,
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
            .find(|(_, h)| matches!(h, Some(Hit::Space(_))))
            .expect("the rule");
        let accent = rule.1.and(rule.0.accent).expect("the name's run");
        assert_eq!(accent.2, crate::theme::space_calm_of(1), "{:?}", rule.0);
    }
}

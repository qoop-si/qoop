//! The qass screen: what the pane shows while the keyboard is in qass's own tab.
//!
//! A `qass` header (the pet as qass's face in the middle over a state lamp; how qass stands,
//! what it does now and what it did last on its left; the three counters on its right) over a
//! framed `terminal` box that holds qass's real terminal. The header's first rows stand where
//! the tab strip would: qass's screen goes without the tabs. Everything here is a pure function of the rows the daemon sent, so a test
//! reads the screen instead of looking at one. Nothing is made up: every word and number
//! comes from a row, a sleeping tab or the watcher's `last-wake` file.
//!
//! The header band and the terminal box are separate rectangles ([`Plan`]). When qass's tab
//! is a split (the board above qass, decision 12), the header spans the whole pane and the
//! split's panes share what is under it ([`Plan::band`]); a zoom gives the zoomed pane all of
//! it.

use qoop_runtime::avatar::Avatar;
use qoop_runtime::control::{AgentRow, WorkspaceRow};

use crate::layout::{Dir, Rect};
use crate::theme::{self, Role};
use crate::{sidebar, width};

/// The narrowest pane and the shortest one that still hold the screen; under either the pane
/// stays plain.
const MIN_COLS: usize = 24;
const MIN_ROWS: usize = 6;
/// The header: the pet's five rows, then a rule with the state lamp under the pet. Its first rows are painted in
/// the tab strip's rows ([`crate::tabs::ROWS`]), which qass's screen does without, so the pane
/// gives it only the rest ([`Header::rows`]).
const HEAD_ROWS: usize = qoop_runtime::avatar::ART_ROWS + 1;
/// The fewest rows qass's terminal keeps inside its box while the header is a box.
const MIN_INSIDE: usize = 6;
/// The columns qass's own line needs left of the pet (`qass` and its state) before the pet is
/// worth showing: under it the header goes without the pet.
const LEFT_MIN: usize = 16;
/// Blank cells either side of the pet (the lamp's overhang), and between the pet's block and the lines beside it.
const PAD: usize = 1;
const GAP: usize = 3;
/// The narrowest pane that holds the inbox at all, and the one that holds it beside qass's
/// terminal rather than in its place.
pub const INBOX_MIN_COLS: usize = 60;
pub const INBOX_BESIDE_COLS: usize = 100;
/// The shortest pane that holds the inbox.
pub const INBOX_MIN_ROWS: usize = 14;
/// The inbox beside the terminal takes about this share of the pane, kept between two widths.
/// The rows the inbox keeps under the header box, so a card shows its options and its text box.
const INBOX_BOX_ROWS: usize = 14;
const INBOX_SHARE: usize = 43;
const INBOX_WIDTH: (usize, usize) = (48, 56);
/// The "on the clock" panel beside qass's terminal: its width keeps between these two, about
/// this share of the pane.
const CLOCK_WIDTH: (usize, usize) = (38, 46);
const CLOCK_SHARE: usize = 30;
/// The columns qass's terminal keeps beside the panel: with fewer the panel goes under it.
const CLOCK_BESIDE_LEAVES: usize = 56;
/// The narrowest the board page draws in (`qoop task board --page` asks for this much).
pub const BOARD_MIN_COLS: usize = 60;
/// The narrowest qass's terminal gets beside the board.
pub const QASS_MIN_COLS: usize = 40;
/// The board's share of qass's tab when it is shown the other way from how it was saved.
const BOARD_RATIO_V: f32 = 0.70;
const BOARD_RATIO_H: f32 = 0.50;
/// The questions column between the board and qass: the edge column the board's frame keeps,
/// then the inbox at the width it is built for.
pub const QUESTIONS_GAP: usize = 1 + INBOX_WIDTH.0;
/// The most the questions column grows to when the board has room to spare, so a card reads whole.
const QUESTIONS_WIDE: usize = 60;

/// The edge column and the questions at their default, given the `room` over the board's minimum
/// (at least `QUESTIONS_GAP`): the default width, and half of the room beyond it up to `QUESTIONS_WIDE`.
fn default_gap(room: usize) -> usize {
    (QUESTIONS_GAP + (room - QUESTIONS_GAP) / 2).min(QUESTIONS_WIDE + 1)
}
/// The narrowest the questions column is dragged to, so a card still reads.
pub const QUESTIONS_MIN: usize = 24;
/// The columns the keys widen or narrow it by.
pub const QUESTIONS_STEP: usize = 4;

/// The most columns the rail's tabs may have in a pane `cols` wide: what is left once the board keeps
/// its minimum (the rail comes off the board; a pane too narrow for both side by side is stacked).
pub fn rail_room(cols: usize) -> usize {
    cols.saturating_sub(BOARD_MIN_COLS + 3 + crate::rail::EDGE)
}

/// The ratio of qass's tab (its first pane's share) the way the tab is shown when it was saved
/// the other way: the board's own share when the board is first, what is left when qass is.
pub fn board_default(dir: Dir, board_first: bool) -> f32 {
    let board = match dir {
        Dir::V => BOARD_RATIO_V,
        Dir::H => BOARD_RATIO_H,
    };
    if board_first { board } else { 1.0 - board }
}

/// How qass's tab with the board is laid out in a pane `cols` wide: side by side while the
/// board and qass both keep their minimum, else stacked; the saved ratio (the first pane's
/// share, whichever pane that is) when the saved direction is the one shown, else that
/// direction's own. `rail` is the columns the rail may take off the first pane (the board when
/// it is first), always charged here, shown or not, so a card arriving never turns the tab over.
/// The columns cut from the board for the questions: none while they are folded (their tab is on
/// the rail); with `questions` the open column while the board keeps its minimum after it: the
/// default width, or the one the owner dragged them to (`width`), which gives way to what the
/// board needs before the column falls back to the rail.
pub fn board_shape(
    cols: usize,
    saved: (Dir, f32),
    questions: bool,
    width: Option<usize>,
    board_first: bool,
    rail: usize,
) -> (Dir, f32, usize) {
    let ratio = |dir: Dir| match saved.0 == dir {
        true => saved.1,
        false => board_default(dir, board_first),
    };
    // The split's frame takes a cell on each side and its divider one more.
    let total = cols.saturating_sub(3);
    // The first pane's cells are measured the way the split measures them, so the board's and
    // qass's are exact whichever of them is first.
    let first = (total as f32 * ratio(Dir::V).clamp(0.1, 0.9)).floor() as usize;
    let board = if board_first { first } else { total - first };
    // The rail comes off the board, on whichever side it is.
    if board < BOARD_MIN_COLS + rail || total - board < QASS_MIN_COLS {
        return (Dir::H, ratio(Dir::H), 0);
    }
    // Beside the board the questions leave no rail (it shows only while a pane is folded, and
    // `fold_gap` weighs that case), so the board's room for them is what is over its minimum.
    let room = board - BOARD_MIN_COLS;
    let gap = match (questions, width) {
        (false, _) => 0,
        (true, None) if room >= QUESTIONS_GAP => default_gap(room),
        (true, None) => 0,
        (true, Some(width)) => {
            let gap = (width + 1).min(room);
            if gap > QUESTIONS_MIN { gap } else { 0 }
        }
    };
    (Dir::V, ratio(Dir::V), gap)
}

/// The questions width (without their edge column) a drag or a key asks for, `gap` being the edge
/// column and the questions, in a board pane `pane_cols` wide before the cut. `None` when the
/// questions would be under their minimum or the board under its own.
pub fn questions_width(gap: usize, pane_cols: usize) -> Option<usize> {
    let width = gap.checked_sub(1)?;
    (width >= QUESTIONS_MIN && pane_cols.checked_sub(gap)? >= BOARD_MIN_COLS).then_some(width)
}

/// The handle on the questions' edge, a thin line over the middle half of it.
pub fn edge_handle(x: usize, y: usize, rows: usize) -> Option<Handle> {
    let len = rows / 2;
    (len >= 2).then_some(Handle {
        x,
        y: y + (rows - len) / 2,
        len,
        vertical: true,
    })
}

/// The handle on the divider between the board and qass: a thin line over the middle half of
/// it, in pane-local cells. `vertical` is a column of `len` rows from (`x`, `y`), else a row of
/// `len` columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub x: usize,
    pub y: usize,
    pub len: usize,
    pub vertical: bool,
}

/// Where the handle stands, from qass's tile: the divider is the cell on the board's side of
/// it (the column or row just before the tile when the board is first, just after when qass
/// is). `None` when the tile is too short for a line of two cells.
pub fn handle_of(qass: Rect, dir: Dir, board_first: bool) -> Option<Handle> {
    let (along, across) = match dir {
        Dir::V => (qass.rows, qass.cols),
        Dir::H => (qass.cols, qass.rows),
    };
    let len = along / 2;
    if len < 2 || across == 0 {
        return None;
    }
    let start = (along - len) / 2;
    Some(match (dir, board_first) {
        (Dir::V, true) => Handle {
            x: qass.x.checked_sub(1)?,
            y: qass.y + start,
            len,
            vertical: true,
        },
        (Dir::V, false) => Handle {
            x: qass.x + qass.cols,
            y: qass.y + start,
            len,
            vertical: true,
        },
        (Dir::H, true) => Handle {
            x: qass.x + start,
            y: qass.y.checked_sub(1)?,
            len,
            vertical: false,
        },
        (Dir::H, false) => Handle {
            x: qass.x + start,
            y: qass.y + qass.rows,
            len,
            vertical: false,
        },
    })
}

/// A pane of qass's split tab folded to its tab on the rail (never closed): the title or `ctrl-a T`
/// folds it, its tab or the keyboard going in opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    Board,
    Qass,
    /// Both at once: the questions take the whole tab.
    Both,
}

/// The questions' gap on qass's tab with a pane folded, the open pane `open_cols` wide (less the
/// rail): the open column while that pane keeps its own minimum (the board's or qass's) after it,
/// else none, and the questions lay over the open pane when wanted.
pub fn fold_gap(open_cols: usize, board_open: bool, questions: bool) -> usize {
    let min = if board_open {
        BOARD_MIN_COLS
    } else {
        QASS_MIN_COLS
    };
    if questions && open_cols >= min + QUESTIONS_GAP {
        default_gap(open_cols - min)
    } else {
        0
    }
}

/// The file the watcher touches each time it tells qass an event.
const LAST_WAKE: &str = "last-wake";

/// What this client remembers about the screen: the zoom, and the two clocks its lines age on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ui {
    /// The header is put away and the terminal box takes the whole pane. The inbox goes with it.
    pub zoomed: bool,
    /// The question inbox is open beside qass's terminal (or in its place, in a narrow pane).
    pub inbox: bool,
    /// The board or qass's terminal folded to its tab on the rail on qass's split tab: one at a time.
    pub fold: Option<Fold>,
    /// The width the owner dragged the rail's tabs to, for this session; `None` for the default.
    pub rail: Option<usize>,
    /// The space on screen when qass's page was last settled: entering qass's space from any
    /// other folds the board onto the rail.
    pub seen: Option<i64>,
    /// When the watcher last told qass something, unix seconds, as the file's age says.
    pub wake_at: Option<i64>,
    /// The quarter minute the ages were last drawn in, so a line that says `2 min ago` is
    /// repainted soon after it stops being true.
    pub bucket: i64,
    /// The rows the "on the clock" panel wants, none while it is not shown. Not remembered:
    /// the client fills it in each frame from the schedules it holds, so the painter and the
    /// client lay the pane out from the same number.
    pub clock: usize,
}

impl Ui {
    /// Read the clocks again. True when a line of the header would read differently.
    pub fn refresh(&mut self, now: i64) -> bool {
        // Never under test, so the suite never depends on the developer's own home.
        let wake_at = if cfg!(test) { None } else { read_wake() };
        let bucket = now / 15;
        let moved = wake_at != self.wake_at || bucket != self.bucket;
        self.wake_at = wake_at;
        self.bucket = bucket;
        moved
    }
}

/// When qass's watcher last said an event line: its `last-wake` file's age.
fn read_wake() -> Option<i64> {
    let file = qoop_runtime::paths::qass_data_dir()?.join(LAST_WAKE);
    let time = std::fs::metadata(file).ok()?.modified().ok()?;
    let secs = time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

/// What the band above the terminal box is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    /// Put away: the zoomed screen.
    None,
    /// One line, for a short pane.
    Line,
    /// The framed header with the pet.
    Box,
}

impl Header {
    /// The pane rows it takes.
    pub fn rows(self) -> usize {
        match self {
            Header::None => 0,
            Header::Line => 1,
            Header::Box => HEAD_ROWS - crate::tabs::ROWS,
        }
    }
}

/// The inbox's rectangle in the pane, and whether it takes the terminal's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
    pub rect: Rect,
    /// A narrow pane: the inbox stands where the terminal box would, which keeps its size.
    pub replaces: bool,
}

/// Where everything goes in a pane of `cols x rows` cells, pane-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub header: Header,
    pub cols: usize,
    pub rows: usize,
    /// The terminal box with its border.
    pub frame: Rect,
    /// qass's terminal inside it: the size its pty is given.
    pub inner: Rect,
    /// The question inbox, when it is open and the pane is big enough for it.
    pub side: Option<Side>,
    /// The "on the clock" panel, when schedules are shown and the pane has room for it.
    pub clock: Option<Clock>,
}

/// The "on the clock" panel's rectangle in the pane, and where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub rect: Rect,
    /// Under the terminal, the pane's whole width: the pane is too narrow for it beside.
    pub under: bool,
}

impl Plan {
    /// `None` when the pane is too small to frame anything: it stays plain then.
    pub fn new(cols: usize, rows: usize, zoomed: bool, inbox: bool) -> Option<Plan> {
        if cols < MIN_COLS || rows < MIN_ROWS {
            return None;
        }
        // The inbox needs rows of its own: under them the header gives up its box for a line.
        let wants_inbox = inbox && !zoomed && Plan::holds_inbox(cols, rows);
        let box_rows = if wants_inbox {
            Header::Box.rows() + INBOX_BOX_ROWS
        } else {
            Header::Box.rows() + 2 + MIN_INSIDE
        };
        let header = if zoomed {
            Header::None
        } else if rows >= box_rows {
            Header::Box
        } else {
            Header::Line
        };
        let top = header.rows();
        let mut plan = Plan {
            header,
            cols,
            rows,
            frame: Rect {
                x: 0,
                y: top,
                cols,
                rows: rows - top,
            },
            inner: Rect {
                x: 1,
                y: top + 1,
                cols: cols - 2,
                rows: rows - top - 2,
            },
            side: None,
            clock: None,
        };
        if inbox && !zoomed && cols >= INBOX_MIN_COLS && rows >= INBOX_MIN_ROWS {
            let below = Rect {
                x: 0,
                y: top,
                cols,
                rows: rows - top,
            };
            if cols >= INBOX_BESIDE_COLS {
                let w = (cols * INBOX_SHARE / 100).clamp(INBOX_WIDTH.0, INBOX_WIDTH.1);
                plan.side = Some(Side {
                    rect: Rect { cols: w, ..below },
                    replaces: false,
                });
                plan.frame = Rect {
                    x: w,
                    cols: cols - w,
                    ..below
                };
                plan.inner = Rect {
                    x: w + 1,
                    y: top + 1,
                    cols: cols - w - 2,
                    rows: rows - top - 2,
                };
            } else {
                // The pty keeps the size it has: nothing repaints when the inbox comes and goes.
                plan.side = Some(Side {
                    rect: below,
                    replaces: true,
                });
            }
        }
        Some(plan)
    }

    /// The plan with the "on the clock" panel `rows` tall wanted (none: no panel). Beside the
    /// terminal while the terminal keeps [`CLOCK_BESIDE_LEAVES`] columns, else under it, the
    /// pane's whole width, while the terminal keeps its [`MIN_INSIDE`] rows; with room for
    /// neither the panel is not drawn. Never over the inbox standing in the terminal's place,
    /// and never on the zoomed screen. On qass's split tab, whose panes lay themselves out in
    /// `frame`, only under.
    pub fn with_clock(mut self, rows: usize) -> Plan {
        if rows == 0 || self.header == Header::None || self.side.is_some_and(|s| s.replaces) {
            return self;
        }
        let split = self.frame == self.inner;
        let w = (self.cols * CLOCK_SHARE / 100).clamp(CLOCK_WIDTH.0, CLOCK_WIDTH.1);
        if !split && self.frame.cols >= w + CLOCK_BESIDE_LEAVES {
            let rect = Rect {
                x: self.frame.x + self.frame.cols - w,
                y: self.frame.y,
                cols: w,
                rows: self.frame.rows,
            };
            self.frame.cols -= w;
            self.inner.cols -= w;
            self.clock = Some(Clock { rect, under: false });
            return self;
        }
        // The terminal inside its box needs its rows and the box's two borders.
        let room = self.frame.rows.saturating_sub(MIN_INSIDE + 2);
        let n = rows.min(room);
        if n < crate::schedules::LIST_MIN_ROWS {
            return self;
        }
        let rect = Rect {
            x: 0,
            y: self.rows - n,
            cols: self.cols,
            rows: n,
        };
        self.frame.rows -= n;
        self.inner.rows -= n;
        if let Some(side) = &mut self.side {
            side.rect.rows -= n;
        }
        self.clock = Some(Clock { rect, under: true });
        self
    }

    /// Whether a pane of this size can hold the inbox at all.
    pub fn holds_inbox(cols: usize, rows: usize) -> bool {
        cols >= INBOX_MIN_COLS && rows >= INBOX_MIN_ROWS
    }
}

impl Plan {
    /// The plan of qass's split tab: the header over the whole pane, and under it, in `frame`
    /// and `inner` alike, the area the split's panes are laid out in (they draw their own
    /// edges). The box header only when each of two stacked panes still keeps
    /// [`MIN_INSIDE`] rows, which also leaves the questions the rows they need; else one line;
    /// `None` when the pane is too small for even that. Where the questions go is the
    /// tree's to say (the board's tile): `side` is left for the caller.
    pub fn band(cols: usize, rows: usize) -> Option<Plan> {
        if cols < MIN_COLS || rows < 2 * MIN_ROWS {
            return None;
        }
        let room = (3 + 2 * MIN_INSIDE).max(INBOX_BOX_ROWS);
        let header = if rows >= Header::Box.rows() + room {
            Header::Box
        } else {
            Header::Line
        };
        let top = header.rows();
        let panes = Rect {
            x: 0,
            y: top,
            cols,
            rows: rows - top,
        };
        Some(Plan {
            header,
            cols,
            rows,
            frame: panes,
            inner: panes,
            side: None,
            clock: None,
        })
    }
}

/// What the header says, read from the rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// qass's terminal.
    pub terminal: u32,
    /// Its state word as the daemon says it, `sleeping` for a tab that is asleep.
    pub state: String,
    /// What its hook last said it was doing, while it works.
    pub activity: Option<String>,
    /// When it last did something, unix seconds.
    pub active_at: Option<i64>,
    /// The owner's agents, and of them the ones that want him, work, and ask him.
    pub agents: usize,
    pub waiting: usize,
    pub working: usize,
    pub asks: usize,
    /// Whether the owner's switch lets qass watch: off, it wakes only when he writes.
    pub watching: bool,
}

impl Facts {
    fn asleep(&self) -> bool {
        self.state == "sleeping"
    }
}

/// qass's facts, or `None` when qass has no tab here to speak for.
///
/// The counters are the owner's agents only: qass's own tab is never one of them, the way it
/// is on no list of his.
pub fn facts(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    focused: u32,
    cards: Option<usize>,
) -> Option<Facts> {
    let qass = sidebar::qass_space(spaces)?;
    // The agent the keyboard is in, else the first one: a second agent opened in qass's space
    // is not who the header speaks for.
    let mine_in_qass = |r: &&AgentRow| r.workspace == qass && r.agent;
    let own_row = rows
        .iter()
        .filter(mine_in_qass)
        .find(|r| r.terminal == focused)
        .or_else(|| rows.iter().find(mine_in_qass));
    let (own, activity, active_at) = match own_row {
        Some(r) => (
            (r.terminal, r.state.clone()),
            r.activity.clone(),
            r.last_active_at_unix.or(r.slept_at_unix),
        ),
        // A sleeping tab is not a row, only a line of its space.
        None => {
            let tab = spaces
                .iter()
                .find(|w| w.id == qass)?
                .sleeping
                .iter()
                .find(|t| t.agent.is_some())?;
            ((tab.terminal, "sleeping".to_string()), None, None)
        }
    };
    let (_, owner) = sidebar::shown(spaces, rows);
    let mine = || owner.iter().filter(|r| r.agent);
    Some(Facts {
        terminal: own.0,
        state: own.1,
        activity,
        active_at,
        agents: mine().count(),
        // What waits on him, the way a card says "waits on you": blocked or failed, or its
        // task waiting on him. A finish nobody read is the sidebar's to show, not a wait.
        waiting: mine()
            .filter(|r| {
                matches!(r.state.as_str(), "blocked" | "failed")
                    || r.task
                        .as_ref()
                        .is_some_and(|t| t.stage == "waiting-human" && !t.claimed)
            })
            .count(),
        working: mine().filter(|r| r.state == "working").count(),
        // The cards waiting in his inbox once it can be read; the blocked agents before.
        asks: cards.unwrap_or_else(|| mine().filter(|r| r.state == "blocked").count()),
        watching: sidebar::watching().unwrap_or(true),
    })
}

/// What the state means in words: its colour and word, the line under it, and what comes next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Say {
    pub role: Role,
    pub word: String,
    pub sub: String,
    /// The `now` line while it works, else the `next` one.
    pub line: (&'static str, String),
}

fn agents_of(n: usize) -> String {
    if n == 1 {
        "1 agent".to_string()
    } else {
        format!("{n} agents")
    }
}

/// The words for `facts`.
pub fn say(f: &Facts) -> Say {
    let calm = if f.watching {
        ("next", "the watcher's next event".to_string())
    } else {
        ("next", "nothing until you write to it".to_string())
    };
    // His switch off: qass wakes only when he writes, and the words say so.
    let watch = |on: String| {
        if f.watching {
            on
        } else {
            "not watching: wakes when you write".to_string()
        }
    };
    let (role, word, sub, line) = match f.state.as_str() {
        "sleeping" => (
            Role::Sleeping,
            "asleep".to_string(),
            crate::keys::named("sleeps when idle, enter or ctrl-a Z wakes it"),
            ("next", "nothing until you wake it".to_string()),
        ),
        "working" | "starting" => (
            Role::Working,
            f.state.clone(),
            watch(format!("keeping watch over {}", agents_of(f.agents))),
            (
                "now",
                match f.activity.as_deref().map(str::trim) {
                    Some(raw) if !raw.is_empty() => sidebar::humanize(raw),
                    _ => f.state.clone(),
                },
            ),
        ),
        "idle" => (
            Role::Idle,
            "idle".to_string(),
            watch(format!("listening for changes in {}", agents_of(f.agents))),
            calm,
        ),
        "blocked" => (
            Role::Blocked,
            "blocked".to_string(),
            "waiting for your answer".to_string(),
            ("next", "your answer in the terminal below".to_string()),
        ),
        "failed" => (
            Role::Failed,
            "failed".to_string(),
            "stopped with an error".to_string(),
            ("next", "see the terminal below".to_string()),
        ),
        "done" => (
            Role::Done,
            "done".to_string(),
            "finished its turn".to_string(),
            calm,
        ),
        other => (
            Role::Unobserved,
            other.to_string(),
            "its state has not been read yet".to_string(),
            calm,
        ),
    };
    Say {
        role,
        word,
        sub,
        line,
    }
}

/// `3 min ago`, from a unix time.
fn ago(then: i64, now: i64) -> String {
    match (now - then).max(0) {
        0..=59 => "just now".to_string(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86_399 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

/// The `last` line: when qass last did something and when its watcher last woke it.
fn last_line(active: Option<i64>, event: Option<i64>, now: i64) -> String {
    let parts: Vec<String> = [("active", active), ("event", event)]
        .into_iter()
        .filter_map(|(what, at)| at.map(|at| format!("{what} {}", ago(at, now))))
        .collect();
    if parts.is_empty() {
        "nothing yet".to_string()
    } else {
        parts.join(", ")
    }
}

/// The pet as qass wears it: asleep, its eyes close to a squint.
pub fn pet_for(pet: &Avatar, asleep: bool) -> Avatar {
    let mut pet = pet.clone();
    if asleep {
        pet.eyes = "squint".to_string();
    }
    pet
}

/// A piece of a line: its words and the ink they wear.
pub(crate) struct Seg {
    pub(crate) role: Role,
    pub(crate) bold: bool,
    pub(crate) text: String,
}

pub(crate) fn seg(role: Role, text: impl Into<String>) -> Seg {
    Seg {
        role,
        bold: false,
        text: text.into(),
    }
}

pub(crate) fn bold(role: Role, text: impl Into<String>) -> Seg {
    Seg {
        role,
        bold: true,
        text: text.into(),
    }
}

/// `text` cut to at most `cols` columns, on a character.
pub(crate) fn cut(text: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| width::drawn(*c)) {
        let w = width::columns(c);
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The ink of `role`, faded to one grey while a box is open over the screen.
pub(crate) fn ink(role: Role, fade: bool) -> String {
    theme::fg(if fade { Role::Unobserved } else { role })
}

/// `segs` painted into exactly `cols` columns. Words cut at the edge end the line, in an
/// ellipsis.
pub(crate) fn line(segs: &[Seg], cols: usize, fade: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    for s in segs {
        if used >= cols {
            break;
        }
        let all = cut(&s.text, usize::MAX);
        let cut_off = width::width(&all) > cols - used;
        let text = match (cut_off, all.trim().is_empty()) {
            (false, _) => all,
            (true, true) => cut(&all, cols - used),
            (true, false) => crate::asks::cut_dots(&all, cols - used),
        };
        used += width::width(&text);
        out.push_str(&ink(s.role, fade));
        if s.bold && !fade {
            out.push_str("\x1b[1m");
        }
        out.push_str(&text);
        // Nothing after a cut: the ellipsis is the line's end.
        if cut_off {
            break;
        }
    }
    out.push_str(theme::RESET);
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

/// What a count of questions is called, on the chip and under the rail alike.
pub(crate) fn questions(n: usize) -> &'static str {
    if n == 1 { "question" } else { "questions" }
}

/// The three counters, each as `[ N word ]`: the agents that need the owner, those working,
/// and qass's questions.
fn chips(f: &Facts) -> Vec<Vec<Seg>> {
    let one = |n: usize, word: &str, lit: Role| {
        vec![
            seg(Role::Rule, "[ "),
            bold(if n > 0 { lit } else { Role::Faint }, n.to_string()),
            seg(Role::Faint, format!(" {word}")),
            seg(Role::Rule, " ]"),
        ]
    };
    vec![
        one(f.waiting, "need you", Role::Focused),
        one(f.working, "working", Role::Working),
        one(f.asks, questions(f.asks), Role::Blocked),
    ]
}

/// The counters that fit in `room` columns, each whole, a cell apart.
fn fit_chips(f: &Facts, room: usize, first_gap: usize) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut used = 0;
    for (i, chip) in chips(f).into_iter().enumerate() {
        let w: usize = chip.iter().map(|s| width::width(&s.text)).sum();
        let gap = if i == 0 { 0 } else { first_gap };
        if used + gap + w > room {
            break;
        }
        used += gap + w;
        if i > 0 {
            out.push(seg(Role::Text, " ".repeat(first_gap)));
        }
        out.extend(chip);
    }
    out
}

/// A box edge: ember corners, the rule between them, a title after the first corner and a
/// tag before the last, each dropped when the edge is too short to hold it.
pub(crate) fn edge(
    cols: usize,
    (left, right): (char, char),
    title: Option<&str>,
    tag: Option<&str>,
    fade: bool,
) -> String {
    let title_w = title.map_or(0, |t| 3 + width::width(t));
    let tag_w = tag.map_or(0, |t| 3 + width::width(t));
    let (title, tag) = if 2 + title_w + tag_w <= cols {
        (title, tag)
    } else if 2 + title_w <= cols {
        (title, None)
    } else {
        (None, None)
    };
    let used =
        2 + title.map_or(0, |t| 3 + width::width(t)) + tag.map_or(0, |t| 3 + width::width(t));
    let rule = ink(Role::Rule, fade);
    let corner = ink(Role::Brand, fade);
    let mut out = format!("{corner}{left}");
    if let Some(title) = title {
        out.push_str(&format!(
            "{rule}\u{2500} {}{title} ",
            ink(Role::Title, fade)
        ));
    }
    out.push_str(&rule);
    out.push_str(&"\u{2500}".repeat(cols - used));
    if let Some(tag) = tag {
        out.push_str(&format!(" {}{tag} {rule}\u{2500}", ink(Role::Faint, fade)));
    }
    out.push_str(&format!("{corner}{right}{}", theme::RESET));
    out
}

/// What painting the screen needs besides the rows.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub pet: Option<&'a Avatar>,
    /// Whether the outer terminal has colour for the pet's half blocks.
    pub colour: bool,
    pub icons: bool,
    pub tick: u64,
    pub now: i64,
    /// A box is open over the screen, which fades everything behind it.
    pub fade: bool,
}

/// The screen, painted once per frame and read a pane row at a time.
pub struct Screen {
    plan: Plan,
    header: Vec<String>,
    top: String,
    bottom: String,
    side: String,
    /// The inbox's rows, each as wide as its rectangle, while it is open.
    inbox: Vec<String>,
    /// Where the `[ N questions ]` chip is, pane-local: its row and its columns.
    chip: Option<Chip>,
    /// The "on the clock" panel's rows, each as wide as its rectangle, while it is shown.
    clock: Vec<String>,
}

/// The `[ N questions ]` chip's cells in the pane: a click on it opens and closes the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chip {
    /// A header row: with the full header counted from the screen's first row (its first rows
    /// stand in the tab strip's), with the one line header a pane row.
    pub row: usize,
    pub x0: usize,
    pub x1: usize,
}

impl Screen {
    pub fn new(plan: Plan, f: &Facts, wake_at: Option<i64>, ctx: Ctx) -> Screen {
        let say = say(f);
        let moving = f.state == "working";
        let mark = sidebar::state_mark(say.role, moving, ctx.tick, ctx.icons);
        let state = vec![
            bold(Role::Text, "qass"),
            seg(Role::Text, "  "),
            seg(say.role, format!("{mark}{}", say.word)),
        ];
        let (header, chip) = match plan.header {
            Header::None => (Vec::new(), None),
            Header::Line => {
                let (line, x) = one_line(state, f, plan.cols, ctx.fade);
                (vec![line], x.map(|(x0, x1)| Chip { row: 0, x0, x1 }))
            }
            Header::Box => {
                let (rows, x) = header_rows(plan.cols, state, f, &say, wake_at, ctx);
                (rows, x.map(|(x0, x1)| Chip { row: 1, x0, x1 }))
            }
        };
        Screen {
            plan,
            header,
            inbox: Vec::new(),
            clock: Vec::new(),
            chip,
            top: edge(
                plan.frame.cols,
                ('\u{256d}', '\u{256e}'),
                Some("terminal"),
                Some(&format!("T{}", f.terminal)),
                ctx.fade,
            ),
            bottom: edge(
                plan.frame.cols,
                ('\u{2570}', '\u{256f}'),
                None,
                None,
                ctx.fade,
            ),
            side: format!("{}\u{2502}{}", ink(Role::Rule, ctx.fade), theme::RESET),
        }
    }

    /// The inbox's painted rows, one per row of its rectangle.
    pub fn with_inbox(mut self, rows: Vec<String>) -> Screen {
        self.inbox = rows;
        self
    }

    /// The "on the clock" panel's painted rows, one per row of its rectangle.
    pub fn with_clock(mut self, rows: Vec<String>) -> Screen {
        self.clock = rows;
        self
    }

    /// One row of the panel in a split tab, whose panes paint themselves: the row of the pane
    /// `pane_row` falls on, while the panel is shown.
    pub fn clock_row(&self, pane_row: usize) -> Option<String> {
        let clock = self.plan.clock?;
        self.clock.get(pane_row.checked_sub(clock.rect.y)?).cloned()
    }

    /// One row of the inbox in a split tab, whose panes paint themselves: the row of the pane
    /// `pane_row` falls on, while the inbox is open.
    pub fn inbox_row(&self, pane_row: usize) -> Option<String> {
        let side = self.plan.side?;
        self.inbox.get(pane_row.checked_sub(side.rect.y)?).cloned()
    }

    /// The chip that opens the inbox, when the header shows it whole.
    pub fn chip(&self) -> Option<Chip> {
        self.chip
    }

    /// How many of the header's rows stand in the tab strip's rows: those of the full header,
    /// none of the one line header's.
    fn above_rows(&self) -> usize {
        match self.plan.header {
            Header::Box => crate::tabs::ROWS,
            Header::Line | Header::None => 0,
        }
    }

    /// Header row `i` of those painted in the tab strip's rows, when the full header is up:
    /// the screen paints it there instead of the tabs.
    pub fn above(&self, i: usize) -> Option<&str> {
        (i < self.above_rows())
            .then(|| self.header.get(i).map(String::as_str))
            .flatten()
    }

    /// One row of the pane, exactly `plan.cols` columns. `terminal` paints qass's own row
    /// `n` (counted inside the box) in exactly `plan.inner.cols` columns.
    pub fn row(&self, pane_row: usize, terminal: impl FnOnce(usize) -> String) -> String {
        let p = &self.plan;
        if pane_row < p.header.rows()
            && let Some(header) = self.header.get(pane_row + self.above_rows())
        {
            return header.clone();
        }
        let blank = || " ".repeat(p.cols);
        // The panel under the terminal stands in rows the box no longer has.
        if p.clock.is_some_and(|c| c.under)
            && let Some(row) = self.clock_row(pane_row)
        {
            return row;
        }
        if pane_row < p.frame.y || pane_row >= p.frame.y + p.frame.rows {
            return blank();
        }
        // The panel beside the terminal ends every row of the box's rows.
        let beside = p
            .clock
            .filter(|c| !c.under)
            .map(|c| {
                self.clock_row(pane_row)
                    .unwrap_or_else(|| " ".repeat(c.rect.cols))
            })
            .unwrap_or_default();
        let inbox = |at: usize| self.inbox.get(at).cloned().unwrap_or_else(blank);
        // A narrow pane: the inbox is where the terminal box would be.
        if let Some(side) = p.side.filter(|s| s.replaces) {
            return inbox(pane_row - side.rect.y);
        }
        let left = p
            .side
            .map(|s| inbox(pane_row - s.rect.y))
            .unwrap_or_default();
        if pane_row == p.frame.y {
            return format!("{left}{}{beside}", self.top);
        }
        if pane_row + 1 == p.frame.y + p.frame.rows {
            return format!("{left}{}{beside}", self.bottom);
        }
        format!(
            "{left}{0}{1}{0}{beside}",
            self.side,
            terminal(pane_row - p.inner.y)
        )
    }
}

/// The header as one line: the state, then the counters that fit.
fn one_line(
    mut state: Vec<Seg>,
    f: &Facts,
    cols: usize,
    fade: bool,
) -> (String, Option<(usize, usize)>) {
    state.insert(0, seg(Role::Text, " "));
    let used: usize = state.iter().map(|s| width::width(&s.text)).sum();
    // The counters after two blank cells, as many as fit whole.
    let room = cols.saturating_sub(used + 2);
    let chips = fit_chips(f, room, 2);
    let span = last_chip(f, &chips).map(|(from, to)| (used + 2 + from, used + 2 + to));
    if !chips.is_empty() {
        state.push(seg(Role::Text, "  "));
        state.extend(chips);
    }
    (line(&state, cols, fade), span)
}

/// Where the last counter, `[ N questions ]`, stands in the run `fit_chips` made: its first and
/// one past its last column, when all three counters fit whole (four pieces each, and a gap
/// between every two).
fn last_chip(f: &Facts, run: &[Seg]) -> Option<(usize, usize)> {
    if run.len() != 3 * 4 + 2 {
        return None;
    }
    let total: usize = run.iter().map(|s| width::width(&s.text)).sum();
    let own: usize = chips(f).last()?.iter().map(|s| width::width(&s.text)).sum();
    Some((total - own, total))
}

/// The header: the pet in the middle over its lamp, qass's own lines on its left
/// (right-aligned to the pet's block), the counters and the sub line on its right, a rule under.
/// The pet stands centred while the counters fit whole on its right, else as far left as they
/// need; with no room for it (or no colour, or a fade) the lines meet at the centre instead.
/// The counters' cells come back for the chip, in header columns.
fn header_rows(
    cols: usize,
    state: Vec<Seg>,
    f: &Facts,
    say: &Say,
    wake_at: Option<i64>,
    ctx: Ctx,
) -> (Vec<String>, Option<(usize, usize)>) {
    let w = |segs: &[Seg]| segs.iter().map(|s| width::width(&s.text)).sum::<usize>();
    let label = |what: &str, text: &str| {
        vec![
            seg(Role::Faint, format!("{what:<6}")),
            seg(Role::Detail, text.to_string()),
        ]
    };
    let left = [
        state,
        label(say.line.0, &say.line.1),
        label("last", &last_line(f.active_at, wake_at, ctx.now)),
    ];
    let chips_w = w(&fit_chips(f, usize::MAX, 1));
    let art = ctx
        .pet
        .filter(|_| ctx.colour && !ctx.fade)
        .map(|pet| sidebar::avatar_overlay(&pet_for(pet, f.asleep()), true))
        .filter(|(pw, rows)| *pw > 0 && !rows.is_empty());
    // Where the middle stands: the pet's block (the pet two cells in), or with no
    // pet the first of a four cell gap the lines keep apart.
    let fit = |mid_w: usize| {
        let centred = cols.saturating_sub(mid_w) / 2;
        let most = cols.saturating_sub(mid_w + GAP + chips_w + 1);
        let at = centred.min(most);
        (at >= 1 + LEFT_MIN + GAP).then_some(at)
    };
    let pet = art.and_then(|(pw, rows)| {
        let mid_w = PAD + 1 + pw + 1 + PAD;
        fit(mid_w).map(|at| (at, pw, rows))
    });
    let (mid_x, mid_w) = match &pet {
        Some((at, pw, _)) => (*at, PAD + 1 + pw + 1 + PAD),
        None => {
            let gap = 2;
            (fit(gap).unwrap_or(cols.saturating_sub(gap) / 2), gap)
        }
    };
    let left_end = mid_x.saturating_sub(GAP);
    let right_x = (mid_x + mid_w + GAP).min(cols);
    let right_w = cols.saturating_sub(right_x + 1);
    let chips = fit_chips(f, right_w, 1);
    let span = last_chip(f, &chips).map(|(from, to)| (right_x + from, right_x + to));
    let right = [chips, vec![seg(Role::Faint, say.sub.clone())]];
    let mut out = Vec::with_capacity(HEAD_ROWS);
    for r in 0..HEAD_ROWS - 1 {
        // Left of the middle: a line right-aligned against it, cut at the pane's edge.
        let text = r.checked_sub(1).and_then(|i| left.get(i));
        let room = left_end.saturating_sub(1);
        let shown = text.map_or(0, |segs| w(segs).min(room));
        let mut row = " ".repeat(left_end.saturating_sub(shown));
        if let Some(segs) = text {
            row.push_str(&line(segs, shown, ctx.fade));
        }
        row.push_str(&" ".repeat(mid_x - left_end));
        // The middle: the pet with a cell of air either side, or the gap.
        match &pet {
            Some((_, pw, art)) => {
                row.push_str(&" ".repeat(PAD + 1));
                match art.get(r) {
                    Some(cells) => row.push_str(&format!("{cells}{}", theme::RESET)),
                    None => row.push_str(&" ".repeat(*pw)),
                }
                row.push_str(&" ".repeat(PAD + 1));
            }
            None => row.push_str(&" ".repeat(mid_w)),
        }
        // Right of the middle: the counters, then the sub line.
        row.push_str(&" ".repeat(right_x.saturating_sub(mid_x + mid_w)));
        let text = r.checked_sub(1).and_then(|i| right.get(i));
        row.push_str(&line(
            text.map_or(&[][..], |s| &s[..]),
            cols - right_x,
            ctx.fade,
        ));
        out.push(row);
    }
    let lamp = pet.as_ref().map(|(at, pw, _)| (at + 1, pw + 2));
    out.push(rule_row(cols, lamp, lamp_role(f, say), ctx.fade));
    (out, span)
}

/// The lamp's ink: ember while something waits for the owner (an agent that wants him or an
/// ask), else qass's own state (blue working, grey idle).
fn lamp_role(f: &Facts, say: &Say) -> Role {
    if f.waiting + f.asks > 0 {
        Role::Blocked
    } else {
        say.role
    }
}

/// The rule under the header, with the state lamp (a short heavy stretch, `lamp` = its first
/// column and width) under the pet.
fn rule_row(cols: usize, lamp: Option<(usize, usize)>, role: Role, fade: bool) -> String {
    let rule = |n: usize| {
        format!(
            "{}{}{}",
            ink(Role::Rule, fade),
            "\u{2500}".repeat(n),
            theme::RESET
        )
    };
    let Some((x, w)) = lamp.filter(|(x, _)| *x < cols) else {
        return rule(cols);
    };
    let w = w.min(cols - x);
    let lit = format!(
        "{}\x1b[1m{}{}",
        ink(role, fade),
        "\u{2501}".repeat(w),
        theme::RESET
    );
    format!("{}{lit}{}", rule(x), rule(cols - x - w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn facts_of(state: &str) -> Facts {
        Facts {
            terminal: 7,
            state: state.to_string(),
            activity: None,
            active_at: Some(1_000),
            agents: 4,
            waiting: 3,
            working: 1,
            asks: 1,
            watching: true,
        }
    }

    fn ctx(pet: Option<&Avatar>) -> Ctx<'_> {
        Ctx {
            pet,
            colour: true,
            icons: false,
            tick: 0,
            now: 1_000 + 125,
            fade: false,
        }
    }

    fn pet() -> Avatar {
        Avatar {
            body: "darko".into(),
            hair: "mohawk".into(),
            eyes: "big".into(),
            hair_ink: 212,
            body_ink: 231,
        }
    }

    #[test]
    fn the_questions_column_grows_with_the_room_and_never_loses_its_place() {
        let stacked = (Dir::V, 0.70);
        assert_eq!(
            board_shape(161, stacked, true, None, true, 0).2,
            QUESTIONS_GAP
        );
        // 180 columns: the board has 63 over its minimum, the column takes half of the 14 beyond its default.
        assert_eq!(
            board_shape(180, stacked, true, None, true, 0).2,
            QUESTIONS_GAP + 7
        );
        assert_eq!(
            board_shape(200, stacked, true, None, true, 0).2,
            QUESTIONS_WIDE + 1
        );
        assert_eq!(
            board_shape(400, stacked, true, None, true, 0).2,
            QUESTIONS_WIDE + 1
        );
        for cols in 80..=400 {
            let (dir, _, gap) = board_shape(cols, stacked, true, None, true, 0);
            assert!(
                dir != Dir::V || gap == 0 || (QUESTIONS_GAP..=QUESTIONS_WIDE + 1).contains(&gap)
            );
        }
        assert_eq!(fold_gap(60 + QUESTIONS_GAP, true, true), QUESTIONS_GAP);
        assert_eq!(fold_gap(60 + QUESTIONS_GAP - 1, true, true), 0);
        assert_eq!(fold_gap(60 + 100, true, true), QUESTIONS_WIDE + 1);
    }

    #[test]
    fn the_plan_gives_the_terminal_the_inside_of_its_box() {
        // A tall pane: the full header takes 4 pane rows (its first 2 stand in the tab bar's),
        // then the terminal box and its two borders.
        let p = Plan::new(160, 46, false, false).unwrap();
        assert_eq!(p.header, Header::Box);
        assert_eq!((p.frame.y, p.frame.rows, p.frame.cols), (4, 42, 160));
        assert_eq!(
            p.inner,
            Rect {
                x: 1,
                y: 5,
                cols: 158,
                rows: 40
            }
        );
        // The shortest pane that still has the full header leaves six rows inside it.
        let p = Plan::new(80, 12, false, false).unwrap();
        assert_eq!((p.header, p.inner.rows), (Header::Box, 6));
        // One row less and the header is one line.
        let p = Plan::new(80, 11, false, false).unwrap();
        assert_eq!(
            (p.header, p.frame.y, p.inner.y, p.inner.rows),
            (Header::Line, 1, 2, 8)
        );
        let p = Plan::new(24, 6, false, false).unwrap();
        assert_eq!(
            (p.header, p.inner.rows, p.inner.cols),
            (Header::Line, 3, 22)
        );
        // Zoomed: no header, the box is the pane.
        let p = Plan::new(160, 46, true, false).unwrap();
        assert_eq!((p.header, p.frame.y, p.inner.rows), (Header::None, 0, 44));
        // Too small for any frame: the pane stays plain.
        assert_eq!(Plan::new(23, 46, false, false), None);
        assert_eq!(Plan::new(160, 5, false, false), None);
    }

    #[test]
    fn every_size_leaves_the_terminal_a_real_inside() {
        for cols in MIN_COLS..130 {
            for rows in MIN_ROWS..60 {
                for zoomed in [false, true] {
                    let p = Plan::new(cols, rows, zoomed, false).unwrap();
                    assert!(p.inner.cols >= 22 && p.inner.rows >= 3, "{cols}x{rows}");
                    assert_eq!(p.frame.y + p.frame.rows, rows);
                    assert_eq!(p.inner.y + p.inner.rows + 1, rows);
                }
            }
        }
    }

    #[test]
    fn the_words_come_from_the_state() {
        let asleep = say(&facts_of("sleeping"));
        assert_eq!(
            (asleep.role, asleep.word.as_str()),
            (Role::Sleeping, "asleep")
        );
        assert!(asleep.sub.contains("sleeps when idle"), "{asleep:?}");
        assert_eq!(asleep.line.0, "next");
        let mut working = facts_of("working");
        working.activity = Some("Edit src/a.rs".to_string());
        let said = say(&working);
        assert_eq!(said.sub, "keeping watch over 4 agents");
        assert_eq!(said.line, ("now", "editing a.rs".to_string()));
        working.agents = 1;
        assert_eq!(say(&working).sub, "keeping watch over 1 agent");
        working.activity = None;
        assert_eq!(say(&working).line, ("now", "working".to_string()));
        assert_eq!(say(&facts_of("idle")).role, Role::Idle);
        let off = say(&Facts {
            watching: false,
            ..facts_of("idle")
        });
        assert_eq!(off.sub, "not watching: wakes when you write");
        assert_eq!(
            off.line,
            ("next", "nothing until you write to it".to_string())
        );
        assert_eq!(say(&facts_of("blocked")).role, Role::Blocked);
        assert_eq!(say(&facts_of("weird")).word, "weird");
    }

    #[test]
    fn the_last_line_ages_what_exists_and_says_nothing_else() {
        assert_eq!(
            last_line(Some(0), Some(60), 180),
            "active 3 min ago, event 2 min ago"
        );
        assert_eq!(last_line(None, Some(0), 7_300), "event 2 h ago");
        assert_eq!(last_line(Some(0), None, 10), "active just now");
        assert_eq!(last_line(None, None, 10), "nothing yet");
    }

    #[test]
    fn the_header_is_painted_from_the_facts() {
        let plan = Plan::new(100, 40, false, false).unwrap();
        let mut f = facts_of("working");
        f.activity = Some("Bash cargo test".to_string());
        let screen = Screen::new(plan, &f, Some(1_000 + 5), ctx(None));
        let above: Vec<String> = (0..crate::tabs::ROWS)
            .map(|i| {
                strip(
                    screen
                        .above(i)
                        .expect("the full header stands in the bar's rows"),
                )
            })
            .collect();
        let rows: Vec<String> = (0..plan.rows)
            .map(|r| strip(&screen.row(r, |_| " ".repeat(plan.inner.cols))))
            .collect();
        assert!(
            rows.iter().chain(&above).all(|r| width::width(r) == 100),
            "every row is as wide as the pane"
        );
        let text = above
            .iter()
            .chain(&rows)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        for want in [
            "qass",
            "working",
            "keeping watch over 4 agents",
            "last  active 2 min ago, event 2 min ago",
            "now   cargo test",
            "[ 3 need you ] [ 1 working ] [ 1 question ]",
            "terminal",
            "T7",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        // No box around the header: a rule under it, then the terminal's box.
        let rule = plan.header.rows() - 1;
        assert!(
            rows[rule].chars().all(|c| c == '\u{2500}'),
            "{:?}",
            rows[rule]
        );
        assert!(rows[rule + 1].starts_with('\u{256d}') && rows[39].starts_with('\u{2570}'));
        // The terminal sits between the box's sides, one column in.
        assert!(rows[rule + 2].starts_with('\u{2502}') && rows[rule + 2].ends_with('\u{2502}'));
    }

    #[test]
    fn the_terminal_rows_are_asked_for_in_the_boxs_own_numbering() {
        let plan = Plan::new(60, 30, false, false).unwrap();
        let screen = Screen::new(plan, &facts_of("idle"), None, ctx(None));
        let asked = std::cell::RefCell::new(Vec::new());
        for r in 0..plan.rows {
            screen.row(r, |n| {
                asked.borrow_mut().push(n);
                " ".repeat(plan.inner.cols)
            });
        }
        let asked = asked.into_inner();
        assert_eq!(asked.first(), Some(&0));
        assert_eq!(asked.last(), Some(&(plan.inner.rows - 1)));
        assert_eq!(asked.len(), plan.inner.rows);
    }

    /// A line cut at its edge says so: `active 1 m…`, never a bare `active 1`, another fact.
    #[test]
    fn a_cut_line_ends_in_an_ellipsis() {
        let segs = [
            seg(Role::Faint, "last  "),
            seg(Role::Detail, "active 1 min ago"),
        ];
        assert_eq!(strip(&line(&segs, 17, false)), "last  active 1 m\u{2026}");
        assert_eq!(strip(&line(&segs, 22, false)), "last  active 1 min ago");
        // Nothing follows the ellipsis.
        let two = [seg(Role::Detail, "hello world"), seg(Role::Detail, "next")];
        assert_eq!(strip(&line(&two, 9, false)), "hello wo\u{2026}");
    }

    #[test]
    fn a_short_pane_gets_one_line_with_the_counters_that_fit() {
        let plan = Plan::new(60, 10, false, false).unwrap();
        assert_eq!(plan.header, Header::Line);
        let screen = Screen::new(plan, &facts_of("idle"), None, ctx(None));
        let first = strip(&screen.row(0, |_| String::new()));
        assert_eq!(width::width(&first), 60);
        assert!(
            first.contains("qass") && first.contains("idle") && first.contains("[ 3 need you ]"),
            "{first:?}"
        );
        let narrow = Plan::new(30, 10, false, false).unwrap();
        let first = strip(
            &Screen::new(narrow, &facts_of("idle"), None, ctx(None)).row(0, |_| String::new()),
        );
        assert_eq!(width::width(&first), 30);
        assert!(!first.contains("question"), "{first:?}");
    }

    #[test]
    fn the_pet_shows_when_there_is_room_and_squints_asleep() {
        let pet = pet();
        let plan = Plan::new(100, 40, false, false).unwrap();
        let with = Screen::new(plan, &facts_of("working"), None, ctx(Some(&pet)));
        let without = Screen::new(plan, &facts_of("working"), None, ctx(None));
        assert_ne!(with.header, without.header, "the pet is drawn");
        assert!(
            with.header[1].contains('\u{2584}')
                || with.header[1].contains('\u{2580}')
                || with.header[1].contains("\x1b[48;5;")
        );
        // Asleep it wears the squint eyes, and awake the picked ones.
        assert_eq!(pet_for(&pet, true).eyes, "squint");
        assert_eq!(pet_for(&pet, false).eyes, "big");
        let asleep = Screen::new(plan, &facts_of("sleeping"), None, ctx(Some(&pet)));
        let awake = Screen::new(plan, &facts_of("working"), None, ctx(Some(&pet)));
        let art = |eyes: &str| {
            let mut p = pet.clone();
            p.eyes = eyes.to_string();
            sidebar::avatar_overlay(&p, true).1
        };
        let (squint, big) = (art("squint"), art("big"));
        assert_ne!(squint, big, "the eye styles differ in the art");
        for i in 0..squint.len() {
            assert!(asleep.header[i].contains(&squint[i]), "row {i} asleep");
            assert!(awake.header[i].contains(&big[i]), "row {i} awake");
        }
        // Too narrow for the pet beside the text: text alone, and still as wide as the pane.
        let narrow = Plan::new(50, 40, false, false).unwrap();
        let screen = Screen::new(narrow, &facts_of("working"), None, ctx(Some(&pet)));
        let bare = Screen::new(narrow, &facts_of("working"), None, ctx(None));
        assert_eq!(screen.header, bare.header);
        // A box open over the screen fades it, pet included.
        let mut faded = ctx(Some(&pet));
        faded.fade = true;
        let screen = Screen::new(plan, &facts_of("working"), None, faded);
        assert!(!screen.header[1].contains("\x1b[48;5;"));
    }

    /// The full header: the pet in the middle over its lamp, centred while the counters
    /// fit whole on its right, moved left only as far as they need (a 120 column window gives
    /// an 81 column pane), with qass's own line still on its left. The one line header has no
    /// rows in the tab bar's.
    #[test]
    fn the_pet_stands_in_the_middle_and_moves_over_only_for_the_counters() {
        let pet = pet();
        let (pw, _) = sidebar::avatar_overlay(&pet, true);
        for (cols, centred) in [(161, true), (81, false)] {
            let plan = Plan::new(cols, 40, false, false).unwrap();
            let screen = Screen::new(plan, &facts_of("working"), None, ctx(Some(&pet)));
            let rule: Vec<char> = strip(&screen.header[HEAD_ROWS - 1]).chars().collect();
            let lb = rule
                .iter()
                .position(|c| *c == '\u{2501}')
                .expect("lamp start");
            let rb = rule
                .iter()
                .rposition(|c| *c == '\u{2501}')
                .expect("lamp end");
            assert_eq!(rb - lb, pw + 1, "{cols}: the pet and a cell either side");
            let middle = (lb + rb) / 2;
            if centred {
                assert!(middle.abs_diff(cols / 2) <= 1, "{cols}: {middle}");
            } else {
                assert!(middle + 5 < cols / 2, "{cols}: moved left, {middle}");
            }
            let first = strip(&screen.header[1]);
            let (left, right) = first.split_at(first.char_indices().nth(lb).unwrap().0);
            assert!(
                left.contains("qass") && left.contains("working"),
                "{first:?}"
            );
            assert!(
                right.contains("[ 3 need you ] [ 1 working ] [ 1 question ]"),
                "{first:?}"
            );
            assert!(screen.chip().is_some(), "{cols}");
        }
        // No pet (no colour here): no lamp, the counters still whole on the right.
        let plan = Plan::new(161, 40, false, false).unwrap();
        let bare = Screen::new(plan, &facts_of("working"), None, ctx(None));
        assert!(!bare.header.iter().any(|r| r.contains('\u{2501}')));
        assert!(strip(&bare.header[1]).contains("[ 1 question ]"));
        // The one line header stays in the pane: nothing for the tab bar's rows.
        let short = Plan::new(60, 10, false, false).unwrap();
        assert_eq!(
            Screen::new(short, &facts_of("idle"), None, ctx(None)).above(0),
            None
        );
    }

    /// The lamp under the pet: ember while something waits for the owner, else qass's own
    /// state (blue working, grey idle), heavy, and as wide as the pet and a cell either side.
    #[test]
    fn the_lamp_under_the_pet_takes_its_colour_from_the_state() {
        let pet = pet();
        let plan = Plan::new(161, 40, false, false).unwrap();
        for (state, waiting, asks, role) in [
            ("idle", 0, 0, Role::Idle),
            ("working", 0, 2, Role::Blocked),
            ("working", 0, 0, Role::Working),
            ("idle", 1, 0, Role::Blocked),
            ("idle", 0, 2, Role::Blocked),
        ] {
            let facts = Facts {
                waiting,
                asks,
                ..facts_of(state)
            };
            let screen = Screen::new(plan, &facts, None, ctx(Some(&pet)));
            let rule = &screen.header[HEAD_ROWS - 1];
            let lit = format!("{}\x1b[1m", theme::fg(role));
            assert!(
                rule.contains(&format!("{lit}\u{2501}")),
                "{state} {waiting} {asks}: {rule:?}"
            );
        }
    }

    fn row(terminal: u32, workspace: i64, state: &str) -> AgentRow {
        AgentRow {
            parked: false,
            keep_awake: false,
            qass_skip: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
            terminal,
            name: None,
            workspace,
            agent: true,
            program: Some("claude".to_string()),
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

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            goal: None,
        }
    }

    #[test]
    fn the_counters_are_the_owners_agents_and_never_qass() {
        let spaces = vec![space(1, "app"), space(9, "qass")];
        let mut qass = row(5, 9, "working");
        qass.activity = Some("Read a.rs".to_string());
        qass.last_active_at_unix = Some(42);
        let rows = vec![
            row(1, 1, "blocked"),
            row(2, 1, "failed"),
            row(3, 1, "working"),
            row(4, 1, "idle"),
            row(6, 1, "sleeping"),
            qass,
        ];
        let f = facts(&rows, &spaces, 5, None).unwrap();
        assert_eq!((f.terminal, f.state.as_str()), (5, "working"));
        assert_eq!(f.activity.as_deref(), Some("Read a.rs"));
        assert_eq!(f.active_at, Some(42));
        // blocked and failed want him; qass's own working tab is in no count.
        assert_eq!((f.agents, f.waiting, f.working, f.asks), (5, 2, 1, 1));
        // An unread finish is not a wait; a task waiting on him is, once per agent.
        let mut done = rows.clone();
        done.push(row(7, 1, "done"));
        sidebar::qass_last(&mut spaces.clone(), &mut done);
        let f = facts(&done, &spaces, 5, None).unwrap();
        assert_eq!(
            (f.agents, f.waiting),
            (6, 2),
            "a finish nobody read is no wait"
        );
        let mut asks = row(8, 1, "idle");
        asks.task = Some(qoop_runtime::control::TaskRef {
            id: 3,
            goal: "ship".to_string(),
            stage: "waiting-human".to_string(),
            claimed: false,
        });
        done.push(asks);
        sidebar::qass_last(&mut spaces.clone(), &mut done);
        let f = facts(&done, &spaces, 5, None).unwrap();
        assert_eq!((f.agents, f.waiting), (7, 3));
        // No qass tab, no screen.
        let owner_only: Vec<AgentRow> = rows[..5].to_vec();
        assert_eq!(facts(&owner_only, &spaces, 0, None), None);
    }

    #[test]
    fn a_sleeping_tab_speaks_for_qass_with_what_it_knows() {
        let mut spaces = vec![space(1, "app"), space(9, "qass")];
        spaces[1].sleeping = vec![qoop_runtime::control::SleepingTab {
            terminal: 5,
            lead: None,
            title: None,
            agent: Some("claude".to_string()),
            session: None,
            cwd: "/q".to_string(),
        }];
        let rows = vec![row(1, 1, "idle")];
        let f = facts(&rows, &spaces, 0, None).unwrap();
        assert_eq!(
            (f.terminal, f.state.as_str(), f.active_at),
            (5, "sleeping", None)
        );
        assert!(f.asleep());
    }

    /// The inbox beside qass's terminal takes a share of the pane and leaves the terminal the
    /// rest; in a narrow pane it takes the terminal's place and the pty keeps its size.
    #[test]
    fn the_inbox_is_beside_the_terminal_when_wide_and_in_its_place_when_narrow() {
        let closed = Plan::new(122, 40, false, false).unwrap();
        let open = Plan::new(122, 40, false, true).unwrap();
        let side = open.side.expect("an inbox at 122 columns");
        assert!(
            !side.replaces && (48..=56).contains(&side.rect.cols),
            "{side:?}"
        );
        assert_eq!((side.rect.x, side.rect.y, side.rect.rows), (0, 4, 36));
        assert_eq!(open.frame.x, side.rect.cols);
        assert_eq!(open.frame.cols + side.rect.cols, 122);
        // The terminal loses columns and no rows, and still has a real inside.
        assert_eq!(open.inner.rows, closed.inner.rows);
        assert_eq!(open.inner.x, side.rect.cols + 1);
        assert_eq!(open.inner.cols, open.frame.cols - 2);
        assert_eq!(closed.side, None);
        // A wide window keeps the share inside its two widths.
        for cols in 100..220 {
            let p = Plan::new(cols, 40, false, true).unwrap();
            let w = p.side.unwrap().rect.cols;
            assert!((48..=56).contains(&w) && p.inner.cols >= 22, "{cols}: {w}");
        }
        // Narrow: in the terminal's place, the pty exactly as it was.
        let narrow = Plan::new(80, 40, false, true).unwrap();
        let shut = Plan::new(80, 40, false, false).unwrap();
        assert!(narrow.side.unwrap().replaces);
        assert_eq!((narrow.frame, narrow.inner), (shut.frame, shut.inner));
        assert_eq!(narrow.side.unwrap().rect, narrow.frame);
        // Too narrow or too short: no inbox at all. Zoom puts it away with the header.
        assert_eq!(Plan::new(59, 40, false, true).unwrap().side, None);
        assert_eq!(Plan::new(100, 13, false, true).unwrap().side, None);
        assert_eq!(Plan::new(122, 40, true, true).unwrap().side, None);
        assert!(
            Plan::holds_inbox(60, 14) && !Plan::holds_inbox(59, 14) && !Plan::holds_inbox(60, 13)
        );
    }

    #[test]
    fn a_row_with_the_inbox_is_the_inbox_then_the_terminal_box_and_as_wide_as_the_pane() {
        for (cols, rows) in [(122, 40), (100, 34), (80, 40)] {
            let plan = Plan::new(cols, rows, false, true).unwrap();
            let side = plan.side.unwrap();
            let inbox: Vec<String> = (0..side.rect.rows)
                .map(|i| format!("{:<w$}", format!("I{i}"), w = side.rect.cols))
                .collect();
            let screen = Screen::new(plan, &facts_of("idle"), None, ctx(None)).with_inbox(inbox);
            for r in 0..plan.rows {
                let row = strip(&screen.row(r, |_| "T".repeat(plan.inner.cols)));
                assert_eq!(width::width(&row), cols, "{cols}x{rows} row {r}: {row:?}");
                if r >= plan.frame.y {
                    assert!(row.starts_with('I'), "{cols}x{rows} row {r}: {row:?}");
                }
            }
            if !side.replaces {
                // Beside: the terminal's frame follows the inbox and its first row is the edge.
                let top = strip(&screen.row(plan.frame.y, |_| String::new()));
                assert!(
                    top[top.find('\u{256d}').unwrap()..].contains("terminal"),
                    "{top}"
                );
                let mid = strip(&screen.row(plan.frame.y + 2, |_| "T".repeat(plan.inner.cols)));
                assert!(
                    mid.contains(&format!("\u{2502}{}\u{2502}", "T".repeat(plan.inner.cols))),
                    "{mid}"
                );
            }
        }
    }

    #[test]
    fn the_clock_panel_stands_beside_a_wide_terminal_and_under_a_narrow_one() {
        let rows = 14;
        // Wide: it takes its width off the right of the box and spans the box's rows.
        let p = Plan::new(140, 40, false, false).unwrap().with_clock(rows);
        let c = p.clock.unwrap();
        assert!(!c.under);
        assert_eq!((c.rect.rows, c.rect.y), (p.frame.rows, p.frame.y));
        assert_eq!(c.rect.x, p.frame.cols);
        assert_eq!(p.frame.cols + c.rect.cols, 140);
        assert_eq!(p.inner.cols, p.frame.cols - 2);
        // Narrow: under the terminal, the pane's whole width, which loses those rows.
        let whole = Plan::new(90, 40, false, false).unwrap();
        let p = whole.with_clock(rows);
        let c = p.clock.unwrap();
        assert!(c.under);
        assert_eq!((c.rect.x, c.rect.cols, c.rect.rows), (0, 90, rows));
        assert_eq!(c.rect.y, 40 - rows);
        assert_eq!(p.frame.rows, whole.frame.rows - rows);
        assert_eq!(p.inner.rows, whole.inner.rows - rows);
        // No panel wanted, none made.
        assert_eq!(
            Plan::new(140, 40, false, false)
                .unwrap()
                .with_clock(0)
                .clock,
            None
        );
    }

    #[test]
    fn the_clock_panel_never_overlaps_the_terminal_or_leaves_it_too_small() {
        for cols in MIN_COLS..200 {
            for rows in MIN_ROWS..60 {
                for inbox in [false, true] {
                    let Some(base) = Plan::new(cols, rows, false, inbox) else {
                        continue;
                    };
                    let p = base.with_clock(UNDER_ROWS_FOR_TEST);
                    let Some(c) = p.clock else { continue };
                    // The box and the panel share no cell.
                    let (f, r) = (p.frame, c.rect);
                    let apart = f.x + f.cols <= r.x
                        || r.x + r.cols <= f.x
                        || f.y + f.rows <= r.y
                        || r.y + r.rows <= f.y;
                    assert!(apart, "{cols}x{rows} inbox {inbox}: {f:?} vs {r:?}");
                    assert!(r.x + r.cols <= cols && r.y + r.rows <= rows);
                    assert!(p.inner.cols >= 22, "{cols}x{rows}");
                    // Under it, the panel keeps rows to read and the terminal its own; beside,
                    // the terminal keeps its columns and every row it had.
                    if c.under {
                        assert!(r.rows >= crate::schedules::LIST_MIN_ROWS);
                        assert!(p.inner.rows >= MIN_INSIDE, "{cols}x{rows}");
                    } else {
                        assert!(p.frame.cols >= CLOCK_BESIDE_LEAVES);
                        assert_eq!(p.inner.rows, base.inner.rows);
                    }
                    // The inbox standing in the terminal's place keeps the whole pane.
                    assert!(!p.side.is_some_and(|s| s.replaces));
                }
            }
        }
        // Zoomed, there is no header and no panel.
        let zoomed = Plan::new(140, 40, true, false)
            .unwrap()
            .with_clock(UNDER_ROWS_FOR_TEST);
        assert_eq!(zoomed.clock, None);
    }

    /// More rows than any pane here can give, so the plan takes what it can.
    const UNDER_ROWS_FOR_TEST: usize = 12;

    #[test]
    fn a_row_with_the_clock_panel_is_the_terminal_box_then_the_panel_and_as_wide_as_the_pane() {
        for (cols, rows) in [(140, 40), (90, 40)] {
            let plan = Plan::new(cols, rows, false, false).unwrap().with_clock(10);
            let c = plan.clock.unwrap();
            let panel: Vec<String> = (0..c.rect.rows)
                .map(|i| format!("{:<w$}", format!("C{i}"), w = c.rect.cols))
                .collect();
            let screen = Screen::new(plan, &facts_of("idle"), None, ctx(None)).with_clock(panel);
            for r in 0..plan.rows {
                let row = strip(&screen.row(r, |_| "T".repeat(plan.inner.cols)));
                assert_eq!(width::width(&row), cols, "{cols}x{rows} row {r}: {row:?}");
                let in_panel = r >= c.rect.y && r < c.rect.y + c.rect.rows;
                assert_eq!(
                    row.contains('C'),
                    in_panel,
                    "{cols}x{rows} row {r}: {row:?}"
                );
            }
            // Beside, the panel ends the box's rows, edges included; under, it starts a row.
            let at = if c.under { c.rect.y } else { plan.frame.y + 2 };
            let row = strip(&screen.row(at, |_| "T".repeat(plan.inner.cols)));
            assert_eq!(row.starts_with('C'), c.under, "{row:?}");
            assert_eq!(row.contains("\u{2502}C"), !c.under, "{row:?}");
        }
    }

    #[test]
    fn the_chip_is_found_where_it_is_painted() {
        for (cols, rows, pet) in [
            (100, 40, false),
            (100, 40, true),
            (60, 10, false),
            (122, 40, true),
        ] {
            let plan = Plan::new(cols, rows, false, false).unwrap();
            let pet_art = pet.then(self::tests::pet);
            let screen = Screen::new(plan, &facts_of("idle"), None, ctx(pet_art.as_ref()));
            let Some(chip) = screen.chip() else {
                panic!("a chip at {cols}x{rows}");
            };
            let row = match plan.header {
                Header::Box => strip(&screen.header[chip.row]),
                _ => strip(&screen.row(chip.row, |_| String::new())),
            };
            let cells: Vec<char> = row.chars().collect();
            let under: String = cells[chip.x0..chip.x1].iter().collect();
            assert_eq!(under, "[ 1 question ]", "{cols}x{rows} pet={pet}: {row:?}");
        }
        // Too narrow to hold all three counters: no chip to press.
        let narrow = Plan::new(30, 10, false, false).unwrap();
        assert_eq!(
            Screen::new(narrow, &facts_of("idle"), None, ctx(None)).chip(),
            None
        );
    }

    #[test]
    fn the_chip_counts_the_cards_once_they_can_be_read() {
        let spaces = vec![space(1, "app"), space(9, "qass")];
        let rows = vec![row(1, 1, "blocked"), row(5, 9, "idle")];
        let blocked = facts(&rows, &spaces, 5, None).unwrap();
        assert_eq!(
            blocked.asks, 1,
            "a blocked agent before the inbox is readable"
        );
        let cards = facts(&rows, &spaces, 5, Some(3)).unwrap();
        assert_eq!(cards.asks, 3);
        assert_eq!(facts(&rows, &spaces, 5, Some(0)).unwrap().asks, 0);
    }

    /// With the inbox open the header gives up its box for a line when the pane is too short
    /// for both, so the inbox always has room for a card's options and its text box.
    #[test]
    fn the_inbox_keeps_its_rows_by_shrinking_the_header_when_the_pane_is_short() {
        for rows in 14..18 {
            let open = Plan::new(122, rows, false, true).unwrap();
            let shut = Plan::new(122, rows, false, false).unwrap();
            assert_eq!(shut.header, Header::Box, "{rows}");
            assert_eq!(open.header, Header::Line, "{rows}");
            assert!(open.side.unwrap().rect.rows >= INBOX_MIN_ROWS - 1, "{rows}");
        }
        let tall = Plan::new(122, 18, false, true).unwrap();
        assert_eq!(tall.header, Header::Box);
        assert!(tall.side.unwrap().rect.rows >= INBOX_BOX_ROWS);
    }

    /// The split tab's header is the same with the questions shown or not: under it the panes
    /// have the whole width, and the questions are cut from the board's tile, not from here.
    #[test]
    fn the_split_tabs_header_leaves_the_panes_the_whole_width() {
        let plan = Plan::band(122, 40).unwrap();
        assert_eq!(plan.header, Header::Box);
        assert_eq!((plan.frame.x, plan.frame.cols), (0, 122));
        assert_eq!(plan.frame, plan.inner);
        assert_eq!(plan.side, None);
        assert_eq!(Plan::band(122, 19).unwrap().header, Header::Box);
        assert_eq!(Plan::band(122, 18).unwrap().header, Header::Line);
    }

    /// qass's tab with the board goes side by side once the board keeps 60 columns and qass
    /// 40, whatever direction was saved; the saved ratio only for the direction it was saved
    /// with; the questions column only while the board keeps 60 after it.
    #[test]
    fn the_board_goes_beside_qass_when_both_keep_their_minimum() {
        let stacked = (Dir::H, 0.5);
        assert_eq!(
            board_shape(161, stacked, false, None, true, 0),
            (Dir::V, 0.70, 0)
        );
        assert_eq!(
            board_shape(134, stacked, false, None, true, 0),
            (Dir::V, 0.70, 0)
        );
        assert_eq!(
            board_shape(133, stacked, false, None, true, 0),
            (Dir::H, 0.5, 0)
        );
        assert_eq!(
            board_shape(121, (Dir::V, 0.75), false, None, true, 0),
            (Dir::H, 0.5, 0)
        );
        assert_eq!(
            board_shape(200, (Dir::V, 0.6), false, None, true, 0),
            (Dir::V, 0.6, 0)
        );
        assert_eq!(
            board_shape(161, stacked, true, None, true, 0),
            (Dir::V, 0.70, QUESTIONS_GAP)
        );
        assert_eq!(
            board_shape(159, stacked, true, None, true, 0),
            (Dir::V, 0.70, QUESTIONS_GAP)
        );
        assert_eq!(
            board_shape(158, stacked, true, None, true, 0),
            (Dir::V, 0.70, 0)
        );
        // Qass first: the same cuts, the first pane's share is the mirror of the board's, and
        // the saved ratio is read as qass's share, never the board's.
        assert_eq!(board_default(Dir::V, true), 0.70);
        assert_eq!(board_default(Dir::V, false), 1.0 - 0.70);
        assert_eq!(
            board_shape(161, stacked, true, None, false, 0),
            // The board has 51 over its minimum here: one column beyond the default is taken.
            (Dir::V, 1.0 - 0.70, QUESTIONS_GAP + 1)
        );
        assert_eq!(
            board_shape(133, stacked, false, None, false, 0),
            (Dir::H, 0.5, 0)
        );
        // 0.3 of 158 is qass's 47 columns and the board's 111: a column of questions fits; the
        // same number read as the board's share would not even fit the board.
        assert_eq!(
            board_shape(161, (Dir::V, 0.3), true, None, false, 0).2,
            QUESTIONS_GAP + 1
        );
        assert_eq!(
            board_shape(161, (Dir::V, 0.3), false, None, true, 0),
            (Dir::H, 0.5, 0)
        );
        for cols in 80..=260 {
            let (dir, ratio, gap) = board_shape(cols, stacked, true, None, false, 0);
            if dir == Dir::V {
                let total = cols - 3;
                let qass = (total as f32 * ratio).floor() as usize;
                assert!(
                    total - qass - gap >= BOARD_MIN_COLS && qass >= QASS_MIN_COLS,
                    "{cols}"
                );
            }
        }
        for cols in 80..=260 {
            let (dir, ratio, gap) = board_shape(cols, stacked, true, None, true, 0);
            if dir == Dir::V {
                let total = cols - 3;
                let board = (total as f32 * ratio).floor() as usize;
                assert!(
                    board - gap >= BOARD_MIN_COLS && total - board >= QASS_MIN_COLS,
                    "{cols}"
                );
            }
        }
        // The rail's columns are charged before the board's minimum, shown or not: a board that
        // keeps its minimum only without them goes stacked, never side by side without room.
        assert_eq!(
            board_shape(102, (Dir::V, 0.6), false, None, true, 0),
            (Dir::H, 0.5, 0)
        );
        assert_eq!(
            board_shape(103, (Dir::V, 0.6), false, None, true, 0),
            (Dir::V, 0.6, 0)
        );
        assert_eq!(
            board_shape(103, (Dir::V, 0.6), false, None, true, 16),
            (Dir::H, 0.5, 0),
            "the board is first: the rail comes off it"
        );
        assert_eq!(
            board_shape(103, (Dir::V, 0.4), false, None, false, 16),
            (Dir::H, 0.5, 0),
            "swapped: it comes off the board too"
        );
        assert_eq!(
            board_shape(103, (Dir::V, 0.4), false, None, false, 0).0,
            Dir::V
        );
    }

    /// A dragged questions width is kept, gives way to the board's minimum, and is dropped under
    /// the questions' own; a drag or a key is refused when it would break either minimum.
    #[test]
    fn a_dragged_questions_width_gives_way_to_the_board_before_it_is_dropped() {
        let wide = (Dir::V, 0.70);
        assert_eq!(board_shape(200, wide, true, Some(30), true, 0).2, 31);
        assert_eq!(board_shape(200, wide, true, Some(70), true, 0).2, 71);
        // Only 77 columns are left over the board's 60 at 200 columns, and 49 at 160.
        assert_eq!(board_shape(200, wide, true, Some(90), true, 0).2, 77);
        assert_eq!(board_shape(160, wide, true, Some(90), true, 0).2, 49);
        // Less than the questions' minimum is left: they leave the board's side.
        assert_eq!(
            board_shape(200, (Dir::V, 0.45), true, Some(70), true, 0).2,
            28
        );
        assert_eq!(
            board_shape(200, (Dir::V, 0.4), true, Some(70), true, 0).2,
            0
        );
        // Qass first: the board is the second pane.
        assert_eq!(
            board_shape(200, (Dir::V, 0.3), true, Some(70), false, 0).2,
            71
        );
        // Without a width, or without questions, nothing moves.
        assert_eq!(
            board_shape(200, wide, true, None, true, 0).2,
            QUESTIONS_WIDE + 1
        );
        assert_eq!(board_shape(200, wide, false, Some(70), true, 0).2, 0);
    }

    #[test]
    fn the_questions_keep_their_minimum_and_so_does_the_board() {
        assert_eq!(questions_width(25, 100), Some(24));
        assert_eq!(questions_width(24, 100), None);
        assert_eq!(questions_width(40, 100), Some(39));
        assert_eq!(questions_width(41, 100), None);
        assert_eq!(questions_width(0, 100), None);
        assert_eq!(
            edge_handle(5, 2, 20),
            Some(Handle {
                x: 5,
                y: 7,
                len: 10,
                vertical: true
            })
        );
        assert_eq!(edge_handle(5, 2, 3), None);
    }
}

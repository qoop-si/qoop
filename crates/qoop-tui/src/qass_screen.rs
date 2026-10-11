//! The qass screen: what the pane shows while the keyboard is in qass's own tab.
//!
//! A framed `qass` header (the pet as qass's face, how qass stands, what it did last and
//! what it does now, three counters) over a framed `terminal` box that holds qass's real
//! terminal. Everything here is a pure function of the rows the daemon sent, so a test
//! reads the screen instead of looking at one. Nothing is made up: every word and number
//! comes from a row, a sleeping tab or the watcher's `last-wake` file.
//!
//! The header band and the terminal box are separate rectangles ([`Plan`]). When qass's tab
//! is a split (the board above qass, decision 12), the header spans the whole pane and the
//! split's panes share what is under it ([`Plan::band`]); a zoom gives the zoomed pane all of
//! it.

use qoop_runtime::avatar::Avatar;
use qoop_runtime::control::{AgentRow, WorkspaceRow};

use crate::layout::Rect;
use crate::theme::{self, Role};
use crate::{sidebar, width};

/// The narrowest pane and the shortest one that still hold the screen; under either the pane
/// stays plain.
const MIN_COLS: usize = 24;
const MIN_ROWS: usize = 6;
/// The header box: a border, the pet's five rows, a border.
const BOX_ROWS: usize = qoop_runtime::avatar::ART_ROWS + 2;
/// The fewest rows qass's terminal keeps inside its box while the header is a box.
const MIN_INSIDE: usize = 6;
/// The columns the text needs beside the pet, before the pet is worth showing: the widest
/// line is the three counters.
const TEXT_MIN: usize = 44;
/// Blank columns before the pet or the text, and between the pet and the text.
const LEAD: usize = 2;
const GAP: usize = 2;
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
/// The file the watcher touches each time it tells qass an event.
const LAST_WAKE: &str = "last-wake";

/// What this client remembers about the screen: the zoom, and the two clocks its lines age on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ui {
    /// The header is put away and the terminal box takes the whole pane. The inbox goes with it.
    pub zoomed: bool,
    /// The question inbox is open beside qass's terminal (or in its place, in a narrow pane).
    pub inbox: bool,
    /// When the watcher last told qass something, unix seconds, as the file's age says.
    pub wake_at: Option<i64>,
    /// The quarter minute the ages were last drawn in, so a line that says `2 min ago` is
    /// repainted soon after it stops being true.
    pub bucket: i64,
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
            Header::Box => BOX_ROWS,
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
            BOX_ROWS + INBOX_BOX_ROWS
        } else {
            BOX_ROWS + 2 + MIN_INSIDE
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

    /// Whether a pane of this size can hold the inbox at all.
    pub fn holds_inbox(cols: usize, rows: usize) -> bool {
        cols >= INBOX_MIN_COLS && rows >= INBOX_MIN_ROWS
    }
}

impl Plan {
    /// The plan of qass's split tab: the header over the whole pane, and under it, in `frame`
    /// and `inner` alike, the area the split's panes are laid out in (they draw their own
    /// edges). The box header only when each of two panes still keeps [`MIN_INSIDE`] rows,
    /// else one line; `None` when the pane is too small for even that.
    pub fn band(cols: usize, rows: usize, inbox: bool) -> Option<Plan> {
        if cols < MIN_COLS || rows < 2 * MIN_ROWS {
            return None;
        }
        // With the inbox open the box header also leaves it rows of its own.
        let wants_inbox = inbox && Plan::holds_inbox(cols, rows);
        let room = if wants_inbox {
            (3 + 2 * MIN_INSIDE).max(INBOX_BOX_ROWS)
        } else {
            3 + 2 * MIN_INSIDE
        };
        let header = if rows >= BOX_ROWS + room {
            Header::Box
        } else {
            Header::Line
        };
        let top = header.rows();
        let under = Rect {
            x: 0,
            y: top,
            cols,
            rows: rows - top,
        };
        let (side, panes) = if !wants_inbox {
            (None, under)
        } else if cols >= INBOX_BESIDE_COLS {
            let w = (cols * INBOX_SHARE / 100).clamp(INBOX_WIDTH.0, INBOX_WIDTH.1);
            (
                Some(Side {
                    rect: Rect { cols: w, ..under },
                    replaces: false,
                }),
                Rect {
                    x: w,
                    cols: cols - w,
                    ..under
                },
            )
        } else {
            // The panes keep their size under the inbox: nothing repaints.
            (
                Some(Side {
                    rect: under,
                    replaces: true,
                }),
                under,
            )
        };
        Some(Plan {
            header,
            cols,
            rows,
            frame: panes,
            inner: panes,
            side,
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
    seen: &sidebar::Seen,
    unseen: &sidebar::Unseen,
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
        waiting: mine()
            .filter(|r| sidebar::needs_person(r, focused, seen, unseen))
            .count(),
        working: mine().filter(|r| r.state == "working").count(),
        // The cards waiting in his inbox once it can be read; the blocked agents before.
        asks: cards.unwrap_or_else(|| mine().filter(|r| r.state == "blocked").count()),
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
    let calm = ("next", "the watcher's next event".to_string());
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
            format!("keeping watch over {}", agents_of(f.agents)),
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
            format!("listening for changes in {}", agents_of(f.agents)),
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

/// `segs` painted into exactly `cols` columns.
pub(crate) fn line(segs: &[Seg], cols: usize, fade: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    for s in segs {
        if used >= cols {
            break;
        }
        let text = cut(&s.text, cols - used);
        used += width::width(&text);
        out.push_str(&ink(s.role, fade));
        if s.bold && !fade {
            out.push_str("\x1b[1m");
        }
        out.push_str(&text);
    }
    out.push_str(theme::RESET);
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

/// The three counters, each as `[ N word ]`.
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
        one(f.waiting, "waiting", Role::Focused),
        one(f.working, "working", Role::Working),
        one(f.asks, "asks you", Role::Blocked),
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
    /// Where the `[ N asks you ]` chip is, pane-local: its row and its columns.
    chip: Option<Chip>,
}

/// The `[ N asks you ]` chip's cells in the pane: a click on it opens and closes the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chip {
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
                let (rows, x) = box_rows(plan.cols, state, f, &say, wake_at, ctx);
                (rows, x.map(|(x0, x1)| Chip { row: 5, x0, x1 }))
            }
        };
        Screen {
            plan,
            header,
            inbox: Vec::new(),
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

    /// One row of the pane, exactly `plan.cols` columns. `terminal` paints qass's own row
    /// `n` (counted inside the box) in exactly `plan.inner.cols` columns.
    pub fn row(&self, pane_row: usize, terminal: impl FnOnce(usize) -> String) -> String {
        let p = &self.plan;
        if let Some(header) = self.header.get(pane_row) {
            return header.clone();
        }
        let blank = || " ".repeat(p.cols);
        if pane_row < p.frame.y || pane_row >= p.frame.y + p.frame.rows {
            return blank();
        }
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
            return format!("{left}{}", self.top);
        }
        if pane_row + 1 == p.frame.y + p.frame.rows {
            return format!("{left}{}", self.bottom);
        }
        format!("{left}{0}{1}{0}", self.side, terminal(pane_row - p.inner.y))
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

/// Where the last counter, `[ N asks you ]`, stands in the run `fit_chips` made: its first and
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

/// The framed header: the pet on the left, the five lines on the right.
fn box_rows(
    cols: usize,
    state: Vec<Seg>,
    f: &Facts,
    say: &Say,
    wake_at: Option<i64>,
    ctx: Ctx,
) -> (Vec<String>, Option<(usize, usize)>) {
    let inside = cols - 2;
    let art = ctx
        .pet
        .filter(|_| ctx.colour && !ctx.fade)
        .map(|pet| sidebar::avatar_overlay(&pet_for(pet, f.asleep()), true))
        .filter(|(w, rows)| *w > 0 && !rows.is_empty() && inside >= LEAD + w + GAP + TEXT_MIN);
    let (pet_w, rows) = art.unwrap_or((0, Vec::new()));
    let lead = if pet_w > 0 { LEAD + pet_w + GAP } else { LEAD };
    let text_w = inside.saturating_sub(lead + 1);
    let chip_line = fit_chips(f, text_w, 1);
    // The border, then the pet's columns, then the counters.
    let span = last_chip(f, &chip_line).map(|(from, to)| (1 + lead + from, 1 + lead + to));
    let label = |what: &str, text: &str| {
        vec![
            seg(Role::Faint, format!("{what:<6}")),
            seg(Role::Detail, text.to_string()),
        ]
    };
    let texts = [
        state,
        vec![seg(Role::Faint, say.sub.clone())],
        label("last", &last_line(f.active_at, wake_at, ctx.now)),
        label(say.line.0, &say.line.1),
        chip_line,
    ];
    let rule = ink(Role::Rule, ctx.fade);
    let mut out = vec![edge(
        cols,
        ('\u{256d}', '\u{256e}'),
        Some("qass"),
        None,
        ctx.fade,
    )];
    for (i, segs) in texts.iter().enumerate() {
        let pet = match rows.get(i) {
            Some(art) => format!(
                "{}{}{art}{}",
                " ".repeat(LEAD),
                theme::RESET,
                " ".repeat(GAP)
            ),
            None => " ".repeat(lead),
        };
        out.push(format!(
            "{rule}\u{2502}{}{pet}{}{rule} \u{2502}{}",
            theme::RESET,
            line(segs, text_w, ctx.fade),
            theme::RESET,
        ));
    }
    out.push(edge(cols, ('\u{2570}', '\u{256f}'), None, None, ctx.fade));
    (out, span)
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
    fn the_plan_gives_the_terminal_the_inside_of_its_box() {
        // A tall pane: a 7 row header box, then the terminal box and its two borders.
        let p = Plan::new(160, 46, false, false).unwrap();
        assert_eq!(p.header, Header::Box);
        assert_eq!((p.frame.y, p.frame.rows, p.frame.cols), (7, 39, 160));
        assert_eq!(
            p.inner,
            Rect {
                x: 1,
                y: 8,
                cols: 158,
                rows: 37
            }
        );
        // The shortest pane that still has a header box leaves six rows inside it.
        let p = Plan::new(80, 15, false, false).unwrap();
        assert_eq!((p.header, p.inner.rows), (Header::Box, 6));
        // One row less and the header is one line.
        let p = Plan::new(80, 14, false, false).unwrap();
        assert_eq!(
            (p.header, p.frame.y, p.inner.y, p.inner.rows),
            (Header::Line, 1, 2, 11)
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
        let rows: Vec<String> = (0..plan.rows)
            .map(|r| strip(&screen.row(r, |_| " ".repeat(plan.inner.cols))))
            .collect();
        assert!(
            rows.iter().all(|r| width::width(r) == 100),
            "every row is as wide as the pane"
        );
        let text = rows.join("\n");
        for want in [
            "qass",
            "working",
            "keeping watch over 4 agents",
            "last  active 2 min ago, event 2 min ago",
            "now   cargo test",
            "[ 3 waiting ] [ 1 working ] [ 1 asks you ]",
            "terminal",
            "T7",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        assert!(rows[0].starts_with('\u{256d}') && rows[BOX_ROWS - 1].starts_with('\u{2570}'));
        assert!(rows[BOX_ROWS].starts_with('\u{256d}') && rows[39].starts_with('\u{2570}'));
        // The terminal sits between the box's sides, one column in.
        assert!(
            rows[BOX_ROWS + 1].starts_with('\u{2502}') && rows[BOX_ROWS + 1].ends_with('\u{2502}')
        );
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

    #[test]
    fn a_short_pane_gets_one_line_with_the_counters_that_fit() {
        let plan = Plan::new(60, 10, false, false).unwrap();
        assert_eq!(plan.header, Header::Line);
        let screen = Screen::new(plan, &facts_of("idle"), None, ctx(None));
        let first = strip(&screen.row(0, |_| String::new()));
        assert_eq!(width::width(&first), 60);
        assert!(
            first.contains("qass") && first.contains("idle") && first.contains("[ 3 waiting ]"),
            "{first:?}"
        );
        let narrow = Plan::new(30, 10, false, false).unwrap();
        let first = strip(
            &Screen::new(narrow, &facts_of("idle"), None, ctx(None)).row(0, |_| String::new()),
        );
        assert_eq!(width::width(&first), 30);
        assert!(!first.contains("asks you"), "{first:?}");
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
            assert!(asleep.header[1 + i].contains(&squint[i]), "row {i} asleep");
            assert!(awake.header[1 + i].contains(&big[i]), "row {i} awake");
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

    fn row(terminal: u32, workspace: i64, state: &str) -> AgentRow {
        AgentRow {
            keep_awake: false,
            background: 0,
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
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let f = facts(&rows, &spaces, 5, &seen, &unseen, None).unwrap();
        assert_eq!((f.terminal, f.state.as_str()), (5, "working"));
        assert_eq!(f.activity.as_deref(), Some("Read a.rs"));
        assert_eq!(f.active_at, Some(42));
        // blocked and failed want him; qass's own working tab is in no count.
        assert_eq!((f.agents, f.waiting, f.working, f.asks), (5, 2, 1, 1));
        // An unread finish is waiting too.
        let mut done = rows.clone();
        done.push(row(7, 1, "done"));
        sidebar::qass_last(&mut spaces.clone(), &mut done);
        let f = facts(&done, &spaces, 5, &seen, &unseen, None).unwrap();
        assert_eq!((f.agents, f.waiting), (6, 3));
        // No qass tab, no screen.
        let owner_only: Vec<AgentRow> = rows[..5].to_vec();
        assert_eq!(facts(&owner_only, &spaces, 0, &seen, &unseen, None), None);
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
        let f = facts(
            &rows,
            &spaces,
            0,
            &sidebar::Seen::new(),
            &sidebar::Unseen::new(),
            None,
        )
        .unwrap();
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
        assert_eq!((side.rect.x, side.rect.y, side.rect.rows), (0, 7, 33));
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
            let row = strip(&screen.row(chip.row, |_| String::new()));
            let cells: Vec<char> = row.chars().collect();
            let under: String = cells[chip.x0..chip.x1].iter().collect();
            assert_eq!(under, "[ 1 asks you ]", "{cols}x{rows} pet={pet}: {row:?}");
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
        let (seen, unseen) = (sidebar::Seen::new(), sidebar::Unseen::new());
        let blocked = facts(&rows, &spaces, 5, &seen, &unseen, None).unwrap();
        assert_eq!(
            blocked.asks, 1,
            "a blocked agent before the inbox is readable"
        );
        let cards = facts(&rows, &spaces, 5, &seen, &unseen, Some(3)).unwrap();
        assert_eq!(cards.asks, 3);
        assert_eq!(
            facts(&rows, &spaces, 5, &seen, &unseen, Some(0))
                .unwrap()
                .asks,
            0
        );
    }

    /// With the inbox open the header gives up its box for a line when the pane is too short
    /// for both, so the inbox always has room for a card's options and its text box.
    #[test]
    fn the_inbox_keeps_its_rows_by_shrinking_the_header_when_the_pane_is_short() {
        for rows in 15..21 {
            let open = Plan::new(122, rows, false, true).unwrap();
            let shut = Plan::new(122, rows, false, false).unwrap();
            assert_eq!(shut.header, Header::Box, "{rows}");
            assert_eq!(open.header, Header::Line, "{rows}");
            assert!(open.side.unwrap().rect.rows >= INBOX_MIN_ROWS - 1, "{rows}");
        }
        let tall = Plan::new(122, 21, false, true).unwrap();
        assert_eq!(tall.header, Header::Box);
        assert!(tall.side.unwrap().rect.rows >= INBOX_BOX_ROWS);
    }

    /// The split tab's panes sit beside the inbox when it is open, and keep their place under
    /// it when the window is narrow.
    #[test]
    fn the_split_tabs_panes_leave_the_inbox_a_column_or_keep_their_place_under_it() {
        let shut = Plan::band(122, 40, false).unwrap();
        let open = Plan::band(122, 40, true).unwrap();
        let side = open.side.expect("an inbox beside the panes");
        assert!(!side.replaces && (48..=56).contains(&side.rect.cols));
        assert_eq!(open.header, shut.header);
        assert_eq!(
            (open.frame.x, open.frame.cols),
            (side.rect.cols, 122 - side.rect.cols)
        );
        assert_eq!(
            (open.frame.y, open.frame.rows),
            (shut.frame.y, shut.frame.rows)
        );
        assert_eq!(open.frame, open.inner);
        let narrow = Plan::band(80, 40, true).unwrap();
        assert!(narrow.side.unwrap().replaces);
        assert_eq!(narrow.frame, Plan::band(80, 40, false).unwrap().frame);
        // Short: the header gives up its box for a line to leave the inbox its rows.
        assert_eq!(Plan::band(122, 20, true).unwrap().header, Header::Line);
        assert_eq!(Plan::band(59, 40, true).unwrap().side, None);
    }
}

//! `qoop task board --page`: the board on one page, as a list or as a Jira board.
//!
//! The list (the first view, `b` turns it) groups the items by what they want: Needs you, In
//! progress, Not started, then Done folded to one row; a row is the item's mark, key, title and
//! its plan, questions, phase, steps and agent word. Three lanes, Todo, Doing and Done, of issue cards (the space on the card's top edge, the
//! title, the key, the steps ticked of all, the agent on it and its stage). `h j k l` and the
//! arrows move, enter opens the issue (Description, Checklist, Activity on the left, Details
//! on the right; `x` ticks a step), `c` adds an issue to Todo, `H` and `L` move one a lane
//! (decision 6: Todo to Doing asks qass to start it, Doing to Done closes it verified, Doing
//! to Todo takes its agents off it, Done to Todo reopens it), `/` filters, `t` tells qass a
//! line. Read and written through the daemon, as `qoop task` is; read again each time the
//! daemon says a task changed, and every half minute besides.
//!
//! An agent's word on a card is the daemon's status token (`waits`, `checks`, `asleep`...) with
//! its age. Open work that no item holds is one folded row at the foot of Doing, `Not on the
//! board · N`, its count line always in view; `N` opens it to a line per task, `j` and `k`
//! step down into it past the last card, enter (or a click) on a line shows that task's tab.

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qoop_runtime::control::{
    ControlRequest, ControlResponse, EventMessage, ItemCardInfo, ItemInfo, TaskInfo, TaskOp,
    TaskOutcome, WorkspaceRow,
};
use qoop_runtime::{client, paths};
use qoop_tui::theme::{self, Role};
use qoop_tui::width::{self, columns};

use crate::page::{self, Key, Screen, Session, bold, ink, raised, raised_bold};
use crate::qass::{self, Life};
use crate::qass_page::ago;

/// The smallest window the page draws its three lanes in, a card each; under it one lane at a
/// time ([`Page::narrow`]), and the mouse rests.
const NEED: (usize, usize) = (
    qoop_tui::qass_screen::BOARD_MIN_COLS,
    qoop_tui::qass_screen::BOARD_MIN_ROWS,
);
/// The smallest it draws in at all: one lane as a list of keys and titles. The board beside the
/// open column of a 60 column window has about 22 of them.
const FLOOR: (usize, usize) = (20, 8);
/// Under this width the one lane is a list, a row a card, with no boxes.
const LIST_W: usize = 30;
/// A card's rows: full (A1) is its title, then key, ages and its one word; quiet (A2) is its
/// title alone, a word only when someone must act.
const FULL_H: usize = 4;
const QUIET_H: usize = 3;
/// The words a card says when someone must act: the owner, or qass checking a claim.
const WAITS: &str = "waits on you";
const CHECKS: &str = "qass checks";
/// What a card says while no agent is on it: in Doing, and in Todo or a reopened item.
const STARTS: &str = "qass will start it";
const NO_AGENT: &str = "no agent yet";
/// `[ full · quiet  v ]`, the card switch at the board's top right; `[ list · board  b ]`, the
/// view switch right of it, is as wide.
const SWITCH_W: usize = 19;
/// The list's groups, in order, and the ink of each one's name.
const GROUPS: [(&str, Role); 4] = [
    ("Needs you", Role::Blocked),
    ("In progress", Role::WorkingText),
    ("Not started", Role::Text),
    ("Done", Role::Done),
];
/// The Done row's place among the list's stops: no item has id 0 (they count from 1).
// ponytail: a sentinel id, an enum of stops if the list gains another row to stand on.
const FOLD: i64 = 0;
/// A row's title keeps at least this many cells before its parts give way.
const TITLE_MIN: usize = 12;
/// What the list says with nothing on the board.
const EMPTY: &str = "Nothing on the board yet. c adds one, t tells qass.";
/// Read again after this long with no news, for what changes with time alone (a start that
/// expired, "3 min ago").
const POLL: Duration = Duration::from_secs(30);
/// An activity entry shows this many rows until it is opened.
const ENTRY_ROWS: usize = 3;
/// What an entry cut to those rows ends with.
const MORE: &str = " … more";
/// The most activity events the store sends; a full card may have more behind it.
const ACTIVITY_SENT: usize = 200;
/// The statuses of work no item holds, in the order the folded row lists them: what needs the
/// owner first. Any other status counts as `other`, after them.
const NEED_ORDER: [&str; 6] = ["waits", "checks", "error", "asleep", "idle", "working"];
/// The folded row's label, its count after it.
const LOOSE: &str = "Not on the board";
/// What a line of the opened row adds after its word when its task has no tab to go to.
const NO_TAB: &str = "no tab";
/// The lanes, in order, as the daemon names the states.
const LANES: [&str; 3] = ["todo", "doing", "done"];

/// The board page's two views: the list, grouped by what the items want, and the lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    List,
    Board,
}

/// A row of the list view.
enum Line<'a> {
    /// A group's rule: which, and how many it holds.
    Group(usize, usize),
    Item(&'a ItemInfo),
    /// Done's rule, which folds its items: how many.
    Fold(usize),
}

/// What `H` or `L` does to an item, by the lane it is in (decision 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
    /// Todo to Doing: record the start and tell qass.
    Start,
    /// Doing to Done: closed verified.
    Close,
    /// Doing to Todo: every agent off it.
    Unlink,
    /// Done to Todo: open again.
    Reopen,
}

/// The move from an item's state one lane left or right, if there is one.
fn move_of(state: &str, right: bool) -> Option<Move> {
    match (state, right) {
        ("todo", true) => Some(Move::Start),
        ("doing", true) => Some(Move::Close),
        ("doing", false) => Some(Move::Unlink),
        ("done", false) => Some(Move::Reopen),
        _ => None,
    }
}

/// What a key asks of the page's caller: the page never talks to the daemon itself.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    Stay,
    Leave,
    /// Read this item's card and show it.
    Open(i64),
    /// Write on item `0`, then say `1`.
    Write(i64, TaskOp, String),
    /// Record the start of the item with this key, and tell qass "start <KEY>".
    Start(i64, String),
    Add {
        workspace: i64,
        goal: String,
    },
    /// Type a line into qass's terminal.
    Tell(String),
    /// Keep the card view picked: quiet or full.
    Cards(bool),
    /// Keep the view picked: the list or the lanes.
    View(View),
    /// Show this terminal's tab, as `qoop focus` does.
    Goto(u32),
}

/// What a click on the folded row's foot lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    /// Its label or count line: open or close it.
    Toggle,
    /// A line of it, by place in the list.
    Line(usize),
}

/// An activity entry that stays itself when newer ones arrive: the event, and which of its
/// identical twins it is, counted from the oldest.
type EntryKey = (i64, i64, String, String, usize);

/// What a dialog row is part of, to hold the view in place over a reload.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Block {
    Plain,
    Title,
    Description,
    Step(i64),
    Entry(usize),
}

/// One row of the dialog's body: pieces at an offset from the column's left, and the stop
/// of the keyboard it belongs to (a step, then an activity entry).
struct Row {
    parts: Vec<(usize, String, Role, bool)>,
    stop: Option<usize>,
    block: Block,
}

impl Row {
    fn new(block: Block, stop: Option<usize>) -> Row {
        Row {
            parts: Vec::new(),
            stop,
            block,
        }
    }

    fn with(mut self, x: usize, text: &str, role: Role, strong: bool) -> Row {
        self.parts.push((x, text.to_string(), role, strong));
        self
    }
}

/// `text` wrapped to `w` cells, a row each.
fn text_rows(text: &str, w: usize, x: usize, role: Role, strong: bool, block: Block) -> Vec<Row> {
    wrap_cells(text, w, None)
        .iter()
        .map(|line| Row::new(block, None).with(x, line, role, strong))
        .collect()
}

/// Where the view was in the body before a reload.
struct Anchor {
    block: Block,
    entry: Option<EntryKey>,
    offset: usize,
}

/// The dialog's columns and body, from its box.
struct Pane {
    /// A page too small for the two columns: the details go under the rest, in one column.
    one: bool,
    lx: usize,
    lw: usize,
    split: usize,
    dx: usize,
    dw: usize,
    top: usize,
    last: usize,
}

impl Pane {
    fn of((x, y): (usize, usize), (w, h): (usize, usize)) -> Pane {
        let lx = x + 3;
        let one = w + 2 < NEED.0;
        let split = if one { x + w - 1 } else { x + w * 62 / 100 };
        let dx = split + 3;
        Pane {
            one,
            lx,
            lw: split.saturating_sub(lx + 2).max(1),
            split,
            dx,
            dw: (x + w).saturating_sub(dx + 2).max(1),
            top: y + 3,
            last: y + h - 2,
        }
    }

    /// The rows of the body in view.
    fn view(&self) -> usize {
        self.last + 1 - self.top
    }
}

/// The activity entries' keys, in the card's order.
fn entry_keys(card: &ItemCardInfo) -> Vec<EntryKey> {
    let mut seen: HashMap<(i64, i64, &str, &str), usize> = HashMap::new();
    let mut keys: Vec<EntryKey> = card
        .activity
        .iter()
        .rev()
        .map(|e| {
            let n = seen.entry((e.task, e.at, &e.kind, &e.body)).or_insert(0);
            *n += 1;
            (e.task, e.at, e.kind.clone(), e.body.clone(), *n - 1)
        })
        .collect();
    keys.reverse();
    keys
}

/// Rows of a column painted from `scroll` into `view` rows from `top`, at `x` and `w` wide;
/// the row of `lit` wears the raised bar.
fn paint(
    s: &mut Screen,
    rows: &[Row],
    (scroll, top, view): (usize, usize, usize),
    (x, w): (usize, usize),
    lit: Option<usize>,
) {
    for r in 0..view {
        let Some(row) = rows.get(scroll + r) else {
            break;
        };
        let y = top + r;
        let on = lit.is_some() && row.stop == lit;
        if on {
            s.put(x - 1, y, &" ".repeat(w + 1), &raised(Role::Detail));
            if r == 0 || rows[scroll + r - 1].stop != row.stop {
                s.put(x - 2, y, "▸", &ink(Role::Brand));
            }
        }
        for (at, text, role, strong) in &row.parts {
            let esc = match (on, strong) {
                (true, true) => raised_bold(*role),
                (true, false) => raised(*role),
                (false, true) => bold(*role),
                (false, false) => ink(*role),
            };
            s.put(x + at, y, &clip(text, w.saturating_sub(*at)), &esc);
        }
    }
}

/// What the keyboard is in.
#[derive(Clone, Debug, PartialEq)]
enum Mode {
    Board,
    /// The issue dialog, the keyboard on step `0` of its checklist.
    Card(usize),
    /// Typing the filter.
    Filter,
    /// Typing a new issue's title, for the space at this place in the page's list.
    Create {
        title: String,
        space: usize,
    },
    /// Typing a line for qass.
    Tell(String),
}

struct Page {
    /// `None` when the daemon is older than the board.
    items: Option<Vec<ItemInfo>>,
    spaces: Vec<WorkspaceRow>,
    /// qass's terminal and how it stands, as words.
    qass: (Option<u64>, String, bool),
    lane: usize,
    /// The item with the keyboard.
    sel: Option<i64>,
    filter: String,
    mode: Mode,
    card: Option<ItemCardInfo>,
    said: Option<String>,
    now: i64,
    /// The card under a held mouse button: let go over qass's pane (the interface says so),
    /// it is handed to qass.
    held: Option<i64>,
    /// The size the page was last drawn at, which the mouse is read against.
    size: (usize, usize),
    /// Where the held card was drawn when it was pressed (left, top, width): what its release
    /// is checked against, since selecting it may scroll its lane.
    held_at: Option<(usize, usize, usize)>,
    /// The held card was carried off itself: its release is no click.
    wandered: bool,
    /// The Done cards put away are shown (dimmed), to bring one back.
    reveal: bool,
    /// The quiet cards (A2): one line, a word only when someone must act. `v` or the switch at
    /// the top right turns it; `board.cards` in config.toml keeps it.
    quiet: bool,
    /// The list or the lanes: `b` or the switch at the top right turns it; `board.view` in
    /// config.toml keeps it.
    view: View,
    /// The list's first row in view, and whether its Done group is opened.
    list_top: usize,
    done_open: bool,
    /// The list's selected row as last settled: its place among the stops and its group.
    list_was: (usize, usize),
    /// Each lane's first card in view: the wheel moves it, the keyboard and a click bring the
    /// selected card back into view.
    top: [usize; 3],
    /// The dialog's first body row in view.
    scroll: usize,
    /// Every activity entry is shown whole (`E`); `flipped` ones are the other way round.
    all: bool,
    flipped: HashSet<EntryKey>,
    /// The daemon had more activity than it sent.
    older: bool,
    /// The page runs in qass's tab, whose pane title already says `board`: the heading
    /// keeps only its sub line.
    in_qass_tab: bool,
    /// Open work no item holds, as the daemon sent it, and how many there are in all; `None`
    /// from a daemon that does not say, which has no folded row.
    tasks: Vec<TaskInfo>,
    loose: Option<usize>,
    /// The folded row at Doing's foot is opened (`N`), the line the keyboard is on in it, and
    /// the first line in view.
    loose_open: bool,
    loose_at: Option<usize>,
    loose_top: usize,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// How long since `then`, short: `now`, `12 min`, `3 h`, `2 d`; nothing before an older daemon
/// said when.
fn span(then: i64, now: i64) -> String {
    match ago(Some(then), now).as_str() {
        _ if then == 0 => String::new(),
        "just now" => "now".to_string(),
        long => long.trim_end_matches(" ago").to_string(),
    }
}

/// A card's word as a narrow card says it: `waits`, `checks`, `starts`, `no agent`, any
/// other as it is, an age after it kept.
fn short(word: &str) -> String {
    match word.split_once(" · ") {
        Some((_, age)) => format!("{} · {age}", bare(word)),
        None => bare(word),
    }
}

/// [`short`] with the age left off, for a card with no room for one: the age whole or not at
/// all, since a cut one reads as another age.
/// An issue key in at most `room` columns: whole, else cut in the middle keeping the number
/// (`QOOPP…-1`), and never shorter than its number (`-1`), which tells two keys apart.
fn cut_key(key: &str, room: usize) -> String {
    let n = key.chars().count();
    if n <= room {
        return key.to_string();
    }
    let Some(dash) = key.rfind('-') else {
        return fit(key, room.max(1));
    };
    let number = &key[dash..];
    if room <= number.chars().count() {
        return number.to_string();
    }
    let keep = room.saturating_sub(number.chars().count() + 1);
    let lead: String = key[..dash].chars().take(keep).collect();
    format!("{lead}\u{2026}{number}")
}

fn bare(word: &str) -> String {
    let base = word.split_once(" · ").map_or(word, |(base, _)| base);
    match base {
        WAITS => "waits",
        CHECKS => "checks",
        STARTS => "starts",
        NO_AGENT => "no agent",
        other => other,
    }
    .to_string()
}

/// `text` cut to `w` cells, the cut said with an ellipsis, never after a space.
fn fit(text: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if width::width(text) <= w {
        return text.to_string();
    }
    let mut out = clip(text, w.saturating_sub(1)).trim_end().to_string();
    out.push('…');
    out
}

/// `text` cut to `w` cells.
fn clip(text: &str, w: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        used += columns(c);
        if used > w {
            break;
        }
        out.push(c);
    }
    out
}

/// `text` in rows of at most `w` cells, broken at spaces; a word longer than a row is broken
/// where it fills one. Stops after `limit` rows. Control characters and bidi marks are not
/// drawn; a zero-width mark stays with its letter.
fn wrap_cells(text: &str, w: usize, limit: Option<usize>) -> Vec<String> {
    let w = w.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let word: Vec<char> = word.chars().filter(|c| width::drawn(*c)).collect();
        let cells: usize = word.iter().map(|c| columns(*c)).sum();
        if cells == 0 {
            continue;
        }
        if let Some(row) = rows.last_mut().filter(|_| used + 1 + cells <= w) {
            row.push(' ');
            row.extend(word);
            used += 1 + cells;
            continue;
        }
        if limit.is_some_and(|l| rows.len() >= l) {
            break;
        }
        rows.push(String::new());
        used = 0;
        for c in word {
            let cw = columns(c);
            if used > 0 && used + cw > w {
                if limit.is_some_and(|l| rows.len() >= l) {
                    return rows;
                }
                rows.push(String::new());
                used = 0;
            }
            if let Some(row) = rows.last_mut() {
                row.push(c);
            }
            used += cw;
        }
    }
    rows
}

/// A space's name by id, or its number when the list does not have it.
fn space_name(spaces: &[WorkspaceRow], id: Option<i64>) -> String {
    id.and_then(|id| spaces.iter().find(|w| w.id == id))
        .map_or_else(|| "no space".to_string(), |w| w.name.clone())
}

/// Whether an item is shown under `filter`: in its key, title or space, any case.
fn matches(item: &ItemInfo, space: &str, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || [item.key.as_str(), item.title.as_str(), space]
            .iter()
            .any(|s| s.to_lowercase().contains(&filter))
}

/// The items in their lanes, newest first, those `filter` hides left out.
fn lanes<'a>(
    items: &'a [ItemInfo],
    spaces: &[WorkspaceRow],
    filter: &str,
    reveal: bool,
) -> [Vec<&'a ItemInfo>; 3] {
    let mut out: [Vec<&ItemInfo>; 3] = Default::default();
    // A card the owner put away is off the board until he asks to see those.
    for item in items.iter().filter(|i| reveal || !i.hidden) {
        let Some(lane) = LANES.iter().position(|l| *l == item.state) else {
            continue;
        };
        if matches(item, &space_name(spaces, item.workspace), filter) {
            out[lane].push(item);
        }
    }
    // Newest first in every lane: Todo and Doing by the item's and its agents' newest change
    // (`updated_at`), Done by when it closed, since a closed item's `updated_at` can still
    // move. The daemon sends a tie oldest first, so a lane is reversed before the stable sort:
    // a tie is newest first too, as on the phone.
    for lane in &mut out {
        lane.reverse();
        lane.sort_by_key(|i| std::cmp::Reverse(lane_stamp(i)));
    }
    out
}

/// What a lane is ordered by: a Done item's close time (`closed_at`, else `since`, which an
/// older daemon leaves 0), anything else's last change.
fn lane_stamp(item: &ItemInfo) -> i64 {
    if item.state != "done" {
        return item.updated_at;
    }
    item.closed_at
        .or((item.since > 0).then_some(item.since))
        .unwrap_or(item.updated_at)
}

/// Where a status stands in the order of [`NEED_ORDER`]; any other after them all.
fn need_rank(status: &str) -> usize {
    NEED_ORDER
        .iter()
        .position(|s| *s == status)
        .unwrap_or(NEED_ORDER.len())
}

/// What a status token says and the ink it wears. `None` for `unknown`, `remote` and an older
/// daemon's empty token: the stage speaks for those.
pub(crate) fn status_word(token: &str) -> Option<(&'static str, Role)> {
    Some(match token {
        "waits" => (WAITS, Role::Blocked),
        "checks" => (CHECKS, Role::Done),
        // Done, one step quieter than qass still checking: the work is over.
        "finished" => ("finished", Role::Detail),
        "replaced" => ("replaced", Role::FaintText),
        "closed" => ("tab closed", Role::FaintText),
        "asleep" => ("asleep", Role::FaintText),
        "working" => ("working", Role::WorkingText),
        "error" => ("error", Role::FailedText),
        "idle" => ("idle", Role::FaintText),
        _ => return None,
    })
}

/// `word · 3 d`: the age comes from the moment the word began, in the page's own format
/// ([`span`]); none when it is not known.
fn aged(word: &str, at: i64, now: i64) -> String {
    match span(at, now) {
        age if age.is_empty() => word.to_string(),
        age => format!("{word} · {age}"),
    }
}

/// A status as it reads: its word with its age, except `working` under an hour, which has been
/// working just now however long ago it began. `None` where the token has no word of its own.
pub(crate) fn status_said(token: &str, at: i64, now: i64) -> Option<(String, Role)> {
    let (word, role) = status_word(token)?;
    let fresh = token == "working" && now - at < 3600;
    let said = if fresh {
        word.to_string()
    } else {
        aged(word, at, now)
    };
    Some((said, role))
}

/// What an agent on a card is doing, in words and ink, and whether someone must act on it: the
/// owner, or qass checking a claim. An older daemon sends no token: the stage says it.
fn agent_word(a: &qoop_runtime::control::ItemAgentInfo, now: i64) -> (String, Role, bool) {
    if let Some((word, role)) = status_said(&a.status, a.status_at, now) {
        return (word, role, matches!(a.status.as_str(), "waits" | "checks"));
    }
    if a.closed {
        ("finished".into(), Role::Done, false)
    } else if a.claimed {
        (CHECKS.into(), Role::Done, true)
    } else if a.stage == "waiting-human" {
        (WAITS.into(), Role::Blocked, true)
    } else {
        (a.stage.clone(), Role::WorkingText, false)
    }
}

/// Who a card says works on it: the first agent still on it, else the last one, else what
/// its lane means. A closed card says only how it ended, with when: an agent's stage there is
/// stale. The last is whether the word asks someone to act.
fn assignee(item: &ItemInfo, now: i64) -> (String, String, Role, bool) {
    let who = item
        .agents
        .iter()
        .find(|a| !a.closed)
        // An ended agent says who finished, but not on a reopened item back in Todo.
        .or(item.agents.last().filter(|_| item.state != "todo"));
    if item.state == "done" {
        let name = who.map(|a| a.name.clone()).unwrap_or_default();
        let at = who
            .map(|a| a.status_at)
            .filter(|at| *at > 0)
            .unwrap_or_else(|| lane_stamp(item));
        // Only how its agent ended: one still live on a card closed from the board says nothing.
        let ended = who
            .filter(|a| matches!(a.status.as_str(), "finished" | "replaced" | "closed"))
            .and_then(|a| status_said(&a.status, a.status_at, now));
        return match (item.outcome.as_deref(), ended) {
            (Some("failed"), _) => (name, aged("failed", at, now), Role::FailedText, false),
            (_, Some((word, role))) => (name, word, role, false),
            _ => (name, "done".into(), Role::Done, false),
        };
    }
    match who {
        Some(a) => {
            let (word, role, acts) = agent_word(a, now);
            (a.name.clone(), word, role, acts)
        }
        None => match (item.state.as_str(), item.outcome.as_deref()) {
            ("doing", _) => (String::new(), STARTS.into(), Role::BrandText, false),
            _ => (String::new(), NO_AGENT.into(), Role::FaintText, false),
        },
    }
}

/// Whether an open item waits on the owner: an agent of it waits, a question on it is open, or
/// its plan waited too long for qass.
fn needs_you(item: &ItemInfo) -> bool {
    item.state != "done"
        && (item.open_asks > 0
            || matches!(item.plan.as_str(), "stale" | "asked")
            || item.agents.iter().any(|a| !a.closed && a.status == "waits"))
}

/// The lanes regrouped for the list: what waits on the owner, the rest of Doing, the rest of Todo
/// (a plan or not: the owner's pick), then Done. Each group keeps the lanes' newest first.
fn groups<'a>(lanes: &[Vec<&'a ItemInfo>; 3]) -> [Vec<&'a ItemInfo>; 4] {
    let mut open: Vec<&ItemInfo> = lanes[0].iter().chain(&lanes[1]).copied().collect();
    open.sort_by_key(|i| std::cmp::Reverse(lane_stamp(i)));
    let mut out: [Vec<&ItemInfo>; 4] = Default::default();
    for item in open {
        let group = match (needs_you(item), item.state == "doing") {
            (true, _) => 0,
            (false, true) => 1,
            (false, false) => 2,
        };
        out[group].push(item);
    }
    out[3] = lanes[2].clone();
    out
}

/// An item's mark, the sidebar's own picture, and its ink: the ring when it waits on the owner,
/// full once closed or at its last phase done, else its first live agent's state (breath
/// working, the middle row asleep, the centre dot idle); the ring in the quiet ink in Todo.
fn item_mark(item: &ItemInfo) -> (String, Role) {
    let mark = |role| qoop_tui::sidebar::state_mark(role, false, 0, false);
    if needs_you(item) {
        return (mark(Role::Blocked), Role::Blocked);
    }
    let last_done = item.phase.as_ref().is_some_and(|p| p.n == p.of)
        && item.steps_total > 0
        && item.steps_done == item.steps_total;
    if item.state == "done" || last_done {
        return (mark(Role::Done), Role::Done);
    }
    if let Some(a) = item.agents.iter().find(|a| !a.closed) {
        let role = match a.status.as_str() {
            "working" => Role::Working,
            "asleep" => Role::Sleeping,
            "error" => Role::Failed,
            _ => Role::Idle,
        };
        return (mark(role), role);
    }
    if item.state == "todo" {
        let colour = qoop_tui::colorlevel::level() != qoop_tui::colorlevel::Level::Mono;
        return (
            todo_ring(qoop_tui::colorlevel::braille(), colour),
            Role::FaintText,
        );
    }
    (mark(Role::Idle), Role::Idle)
}

/// Todo's mark: waiting's picture, told apart by its quiet ink. Without braille its `?` would
/// say it waits, and without colour it would be the needs-you ring: a plain `o` then.
fn todo_ring(braille: bool, colour: bool) -> String {
    if braille && colour {
        qoop_tui::sidebar::state_mark(Role::Blocked, false, 0, false)
    } else {
        "o ".to_string()
    }
}

/// What a list row says right of its title, in the order it is drawn: the plan, the questions
/// open, the phase, the steps, the agent's word.
fn row_parts(item: &ItemInfo, now: i64) -> Vec<(String, Role)> {
    let mut parts = Vec::new();
    if let Some(plan) = qoop_tui::asks::plan_word(&item.plan, item.plan_by.as_deref()) {
        parts.push(plan);
    }
    match item.open_asks {
        0 => {}
        1 => parts.push(("1 question".to_string(), Role::Blocked)),
        n => parts.push((format!("{n} questions"), Role::Blocked)),
    }
    if let Some(p) = &item.phase {
        parts.push((
            format!("{} {}/{}", fit(&p.name, 16), p.n, p.of),
            Role::Detail,
        ));
    }
    if item.steps_total > 0 {
        let role = if item.steps_done == item.steps_total {
            Role::Done
        } else {
            Role::Detail
        };
        parts.push((format!("{}/{}", item.steps_done, item.steps_total), role));
    }
    let (_, word, role, _) = assignee(item, now);
    if !word.is_empty() {
        parts.push((word, role));
    }
    parts
}

/// The cells `parts` take flush right, two apart, with the two before the first.
fn parts_w(parts: &[(String, Role)]) -> usize {
    parts.iter().map(|(t, _)| width::width(t) + 2).sum()
}

/// `parts` cut to `room` cells, the rightmost first, a word that wants the owner last.
fn drop_parts(mut parts: Vec<(String, Role)>, room: usize) -> Vec<(String, Role)> {
    while parts_w(&parts) > room {
        let urgent = |r: &Role| matches!(r, Role::Blocked | Role::Failed | Role::FailedText);
        let at = parts.iter().rposition(|(_, r)| !urgent(r));
        parts.remove(at.unwrap_or(parts.len() - 1));
    }
    parts
}

impl Page {
    fn new() -> Page {
        Page {
            items: Some(Vec::new()),
            spaces: Vec::new(),
            qass: (None, "not started".into(), false),
            lane: 0,
            sel: None,
            filter: String::new(),
            mode: Mode::Board,
            card: None,
            said: None,
            now: now_unix(),
            held: None,
            size: NEED,
            held_at: None,
            wandered: false,
            reveal: false,
            quiet: false,
            view: View::List,
            list_top: 0,
            done_open: false,
            list_was: (0, 0),
            top: [0; 3],
            scroll: 0,
            all: false,
            flipped: HashSet::new(),
            older: false,
            in_qass_tab: false,
            tasks: Vec::new(),
            loose: None,
            loose_open: false,
            loose_at: None,
            loose_top: 0,
        }
    }

    /// A card's rows in the view picked.
    fn card_h(&self) -> usize {
        if self.quiet { QUIET_H } else { FULL_H }
    }

    /// Where the search box starts on the top row at width `w`.
    fn search_at(&self, w: usize) -> usize {
        w.saturating_sub(self.search().chars().count() + 6)
    }

    /// Where the view switch starts on the top row at width `w`: left of the search box.
    fn view_at(&self, w: usize) -> usize {
        self.search_at(w).saturating_sub(2 + SWITCH_W)
    }

    /// Where the card switch starts on the top row at width `w`: left of the view switch, or in
    /// its place when that leaves no room (qass's split; the footer still says `b`). Only the
    /// lanes have cards.
    fn switch_at(&self, w: usize) -> usize {
        let left = self.view_at(w).saturating_sub(2 + SWITCH_W);
        // " board" ends at column 6.
        if left > 6 { left } else { self.view_at(w) }
    }

    /// The view switch is drawn and clicked: always in the list, on the lanes only while the
    /// card switch has room beside it.
    fn view_shown(&self, w: usize) -> bool {
        self.view == View::List || self.switch_at(w) != self.view_at(w)
    }

    /// The search box's text.
    fn search(&self) -> String {
        let search = match (&self.mode, self.filter.is_empty()) {
            (Mode::Filter, _) => format!("/ {}▌", self.filter),
            (_, true) => "/ search".to_string(),
            (_, false) => format!("/ {}", self.filter),
        };
        fit(&search, 24)
    }

    fn turn_cards(&mut self) -> Step {
        self.quiet = !self.quiet;
        self.follow();
        Step::Cards(self.quiet)
    }

    /// The list to the lanes or back, on the same item: a Done one opens Done in the list.
    fn turn_view(&mut self) -> Step {
        self.view = match self.view {
            View::List => View::Board,
            View::Board => View::List,
        };
        self.loose_at = None;
        if self.selected().is_some_and(|i| i.state == "done") {
            self.done_open = true;
        }
        self.settle();
        self.follow();
        Step::View(self.view)
    }

    /// The list's rows: each group with items its rule and its items, then Done's rule, and
    /// its items while it is opened.
    fn list_lines<'a>(&self, lanes: &[Vec<&'a ItemInfo>; 3]) -> Vec<Line<'a>> {
        let groups = groups(lanes);
        let mut out = Vec::new();
        for (n, group) in groups[..3].iter().enumerate() {
            if !group.is_empty() {
                out.push(Line::Group(n, group.len()));
                out.extend(group.iter().map(|i| Line::Item(i)));
            }
        }
        if !groups[3].is_empty() {
            out.push(Line::Fold(groups[3].len()));
            if self.done_open {
                out.extend(groups[3].iter().map(|i| Line::Item(i)));
            }
        }
        out
    }

    /// What the list's keyboard stands on, in order: its items and the Done row ([`FOLD`]).
    fn stops(&self) -> Vec<i64> {
        self.list_lines(&self.lanes())
            .iter()
            .filter_map(|l| match l {
                Line::Item(i) => Some(i.id),
                Line::Fold(_) => Some(FOLD),
                Line::Group(..) => None,
            })
            .collect()
    }

    /// The list's first row on the page: under the new item's form while one is typed.
    fn list_y(&self) -> usize {
        if matches!(self.mode, Mode::Create { .. }) {
            4
        } else {
            2
        }
    }

    /// The rows the list has at the size last drawn: above the folded row and the foot.
    fn list_room(&self) -> usize {
        let h = self.size.1.max(FLOOR.1);
        (h - 2)
            .saturating_sub(self.list_y() + self.sect_rows(1, h - 4))
            .max(1)
    }

    /// Scroll the list the least that brings the selected row into view, its group's rule with
    /// it when it is the group's first.
    fn list_follow(&mut self) {
        let lanes = self.lanes();
        let lines = self.list_lines(&lanes);
        let Some(at) = lines.iter().position(|l| match l {
            Line::Item(i) => Some(i.id) == self.sel,
            Line::Fold(_) => self.sel == Some(FOLD),
            Line::Group(..) => false,
        }) else {
            return;
        };
        let head = at.saturating_sub(usize::from(
            at > 0 && matches!(lines[at - 1], Line::Group(..)),
        ));
        let room = self.list_room();
        if head < self.list_top {
            self.list_top = head;
        } else if at >= self.list_top + room {
            self.list_top = at + 1 - room;
        }
    }

    fn lanes(&self) -> [Vec<&ItemInfo>; 3] {
        lanes(
            self.items.as_deref().unwrap_or_default(),
            &self.spaces,
            &self.filter,
            self.reveal,
        )
    }

    /// The Done cards put away (within Done's 24 hours: older ones are off the board anyway).
    fn hidden(&self) -> usize {
        self.items
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|i| i.hidden)
            .count()
    }

    /// Put a closed card away, or bring a put away one back: `x` on it, or a click on its
    /// cross. Open cards have none.
    fn dismissed(&self, item: &ItemInfo) -> Step {
        if item.state != "done" {
            return Step::Stay;
        }
        let undo = item.hidden;
        let said = if undo {
            format!("{} is back on the board", item.key)
        } else {
            format!(
                "{} is off the board: hidden ({}) shows it",
                item.key,
                self.hidden() + 1
            )
        };
        Step::Write(item.id, TaskOp::Dismiss { undo }, said)
    }

    /// The spaces an issue can be added to: every one but qass's.
    fn targets(&self) -> Vec<&WorkspaceRow> {
        self.spaces
            .iter()
            .filter(|w| w.name != qass::SPACE)
            .collect()
    }

    /// The item with the keyboard, when it is still shown.
    fn selected(&self) -> Option<&ItemInfo> {
        let sel = self.sel?;
        self.items.as_ref()?.iter().find(|i| i.id == sel)
    }

    /// Keep the keyboard on its item wherever the item went (a move takes it to another
    /// lane); an item gone or filtered out leaves it on the first card of its lane.
    fn settle(&mut self) {
        if self.view == View::List {
            let stops = self.stops();
            // An item gone from the stops (`L` folds it into Done) leaves the keyboard on the
            // row now at its place; one that changed group on a poll is brought into view.
            let (was, group) = self.list_was;
            let kept = self.sel.and_then(|s| stops.iter().position(|n| *n == s));
            let at = kept.unwrap_or(was.min(stops.len().saturating_sub(1)));
            self.sel = stops.get(at).copied();
            let now = match self.sel {
                Some(FOLD) => 3,
                sel => groups(&self.lanes())
                    .iter()
                    .position(|g| g.iter().any(|i| Some(i.id) == sel))
                    .unwrap_or(0),
            };
            if kept.is_none() || now != group {
                self.list_follow();
            }
            self.list_was = (at, now);
            let lanes = self.lanes();
            // The lane follows the item, so the lanes open on it and a move knows its way.
            self.lane = match self.sel {
                Some(FOLD) => 2,
                sel => lanes
                    .iter()
                    .position(|l| l.iter().any(|i| Some(i.id) == sel))
                    .unwrap_or(self.lane),
            };
            return;
        }
        let lanes = self.lanes();
        if let Some(lane) = self
            .sel
            .and_then(|sel| lanes.iter().position(|l| l.iter().any(|i| i.id == sel)))
        {
            // Only a card that went to another lane is brought into view: settle also runs on
            // every poll, which must not undo the wheel.
            let moved = lane != self.lane;
            self.lane = lane;
            if moved {
                self.follow();
            }
            return;
        }
        self.sel = lanes[self.lane].first().map(|i| i.id);
        self.follow();
    }

    /// Move the keyboard `by` cards in its lane.
    fn step(&mut self, by: isize) {
        if self.step_in_loose(by) {
            return;
        }
        if self.view == View::List {
            let stops = self.stops();
            let at = self
                .sel
                .and_then(|sel| stops.iter().position(|s| *s == sel))
                .unwrap_or(0);
            let to = (at as isize + by).clamp(0, stops.len().saturating_sub(1) as isize) as usize;
            self.sel = stops.get(to).copied();
            self.list_follow();
            return;
        }
        let lanes = self.lanes();
        let lane = &lanes[self.lane];
        let at = self
            .sel
            .and_then(|sel| lane.iter().position(|i| i.id == sel))
            .unwrap_or(0);
        let to = (at as isize + by).clamp(0, lane.len().saturating_sub(1) as isize) as usize;
        self.sel = lane.get(to).map(|i| i.id);
        self.follow();
    }

    /// Move the keyboard to the lane `by` over, onto the card at the same height.
    fn side(&mut self, by: isize) {
        self.loose_at = None;
        let lanes = self.lanes();
        let at = self
            .sel
            .and_then(|sel| lanes[self.lane].iter().position(|i| i.id == sel))
            .unwrap_or(0);
        let to = (self.lane as isize + by).clamp(0, 2) as usize;
        let lane = &lanes[to];
        let sel = lane.get(at.min(lane.len().saturating_sub(1))).map(|i| i.id);
        (self.lane, self.sel) = (to, sel);
        self.follow();
    }

    /// A narrow page draws its lane as a list, a row a card: under [`LIST_W`] columns, or when
    /// the lane's box would hold no card at all.
    fn listed(&self, (w, h): (usize, usize), n: usize) -> bool {
        (w < NEED.0 || h < NEED.1) && (w < LIST_W || self.shown(n, 0, h.saturating_sub(4)).1 == 0)
    }

    /// How many cards fit in lane `n`'s rows at the size last drawn; none while the page is too
    /// small to draw lanes at all.
    fn room(&self, n: usize) -> usize {
        let (w, h) = self.size;
        if w < FLOOR.0 || h < FLOOR.1 {
            return 0;
        }
        if self.listed((w, h), n) {
            return h - 4;
        }
        self.shown(n, 0, h - 4).1
    }

    /// Scroll the keyboard's lane the least that brings the selected card into view.
    fn follow(&mut self) {
        if self.view == View::List {
            return self.list_follow();
        }
        let room = self.room(self.lane);
        let lanes = self.lanes();
        let Some(at) = self
            .sel
            .and_then(|sel| lanes[self.lane].iter().position(|i| i.id == sel))
        else {
            return;
        };
        let top = &mut self.top[self.lane];
        if at < *top {
            *top = at;
        } else if room > 0 && at >= *top + room {
            *top = at + 1 - room;
        }
    }

    /// The work no item holds: what needs the owner first, then newest first (the daemon sends
    /// it so; a stable sort keeps it so whatever a daemon does).
    fn loose_lines(&self) -> Vec<&TaskInfo> {
        let mut lines: Vec<&TaskInfo> = self.tasks.iter().collect();
        lines.sort_by_key(|t| need_rank(&t.status));
        lines
    }

    /// Whether the folded row is there: the daemon counted some, and no filter is typed (a
    /// filter reads items, and these have none).
    fn loose_shown(&self) -> bool {
        self.items.is_some() && self.loose.is_some_and(|n| n > 0) && self.filter.trim().is_empty()
    }

    /// The rows the folded row takes from lane `n` of `lane_h` rows: its label and count line,
    /// and while it is open a line per task, as many as leave Doing one card's height.
    fn sect_rows(&self, n: usize, lane_h: usize) -> usize {
        if n != 1 || !self.loose_shown() {
            return 0;
        }
        if !self.loose_open {
            return 2;
        }
        let room = lane_h.saturating_sub(3 + 2 + self.card_h()).max(1);
        2 + self.loose_lines().len().min(room)
    }

    /// The first line of the opened row in view, with `shown` lines in view: the scroll, never
    /// past the last line.
    fn loose_first(&self, shown: usize) -> usize {
        self.loose_top
            .min(self.loose_lines().len().saturating_sub(shown))
    }

    /// Whether the folded row is drawn where the keyboard is: at the list's foot, or in the
    /// lanes' Doing box (a lane too narrow for boxes has none).
    fn loose_here(&self) -> bool {
        self.view == View::List || (self.lane == 1 && !self.listed(self.size, 1))
    }

    /// The line of the opened row with the keyboard, while it is there to have it.
    fn loose_cursor(&self) -> Option<usize> {
        self.loose_at
            .filter(|_| self.loose_shown() && self.loose_open && self.loose_here())
    }

    /// The opened row is Doing's last stop: `j` past its last card steps into it and `k` from
    /// its first line steps back out. Whether it took the key.
    fn step_in_loose(&mut self, by: isize) -> bool {
        if !self.loose_here() || !self.loose_shown() || !self.loose_open {
            return false;
        }
        let lines = self.loose_lines().len();
        match self.loose_cursor() {
            Some(i) => {
                self.loose_at = (by > 0 || i > 0)
                    .then(|| (i as isize + by).clamp(0, lines as isize - 1) as usize);
            }
            None => {
                // In the list it is the last stop's next; in the lanes, Doing's last card's.
                let last = if self.view == View::List {
                    let stops = self.stops();
                    stops.is_empty() || self.sel == stops.last().copied()
                } else {
                    let lanes = self.lanes();
                    let at = self
                        .sel
                        .and_then(|sel| lanes[1].iter().position(|i| i.id == sel))
                        .unwrap_or(0);
                    at + 1 >= lanes[1].len()
                };
                if by < 0 || lines == 0 || !last {
                    return false;
                }
                self.loose_at = Some(0);
            }
        }
        self.loose_follow();
        true
    }

    /// Scroll the opened row the least that brings the keyboard's line into view.
    fn loose_follow(&mut self) {
        let Some(i) = self.loose_cursor() else {
            return;
        };
        let shown = self
            .sect_rows(1, self.size.1.saturating_sub(4))
            .saturating_sub(2);
        if i < self.loose_top {
            self.loose_top = i;
        } else if shown > 0 && i >= self.loose_top + shown {
            self.loose_top = i + 1 - shown;
        }
    }

    /// Open or close the folded row (`N`).
    fn turn_loose(&mut self) {
        // In the lanes the row is drawn in Doing's box: a lane too narrow for boxes has none.
        if self.loose_shown() && (self.view == View::List || !self.listed(self.size, 1)) {
            self.loose_open = !self.loose_open;
            self.loose_at = None;
            self.follow();
        }
    }

    /// Show the tab of the line at `i`; a task with no terminal, or one that runs on another
    /// machine (its number is that machine's), says so.
    fn goto(&mut self, i: usize) -> Step {
        let Some((terminal, id, remote)) = self
            .loose_lines()
            .get(i)
            .map(|t| (t.terminal, t.id, t.status == "remote"))
        else {
            return Step::Stay;
        };
        match terminal.and_then(|t| u32::try_from(t).ok()) {
            Some(terminal) if !remote => Step::Goto(terminal),
            Some(_) => {
                self.said = Some(format!("task #{id} runs on another machine"));
                Step::Stay
            }
            None => {
                self.said = Some(format!("task #{id} has {NO_TAB} to show"));
                Step::Stay
            }
        }
    }

    /// Where the folded row stands in a lane of `h` rows from row `y`: its first row, and how
    /// many rows it has (its label, its count line, then a line per task in view). `None` while
    /// it is not there. The one place its rows are placed, for drawing and for the mouse.
    fn loose_place(&self, y: usize, h: usize) -> Option<(usize, usize)> {
        let rows = self.sect_rows(1, h);
        (rows > 0).then(|| (y + h - 2 - rows, rows))
    }

    /// What a cell of the page is on the folded row, as [`Page::draw`] places it.
    fn loose_hit(&self, col: usize, row: usize) -> Option<Hit> {
        let (w, h) = self.size;
        if w < NEED.0 || h < NEED.1 {
            return None;
        }
        let (top, rows) = self.loose_place(2, h - 4)?;
        let lw = (w - 2 - 4) / 3;
        let x = 1 + (lw + 2);
        if col <= x || col >= x + lw - 1 {
            return None;
        }
        match row.checked_sub(top)? {
            0 | 1 => Some(Hit::Toggle),
            n if n < rows => Some(Hit::Line(self.loose_first(rows - 2) + n - 2)),
            _ => None,
        }
    }

    /// The lane under a page column, as drawn.
    fn lane_at(&self, col: usize) -> Option<usize> {
        let lw = (self.size.0 - 2 - 4) / 3;
        (0..3).find(|n| {
            let x = 1 + n * (lw + 2) + 1;
            col >= x && col < x + lw - 2
        })
    }

    /// One notch of the wheel over a lane: that lane moves by a card, never past its last.
    fn wheel(&mut self, up: bool, col: usize, row: usize) {
        if matches!(self.mode, Mode::Card(_)) {
            let max = self.max_scroll();
            let scroll = self.scroll.min(max);
            self.scroll = if up {
                scroll.saturating_sub(3)
            } else {
                (scroll + 3).min(max)
            };
            return;
        }
        // The list moves a row a notch, never past its last.
        if self.view == View::List {
            if self.mode == Mode::Board {
                let max = self
                    .list_lines(&self.lanes())
                    .len()
                    .saturating_sub(self.list_room());
                let top = self.list_top.min(max);
                self.list_top = if up {
                    top.saturating_sub(1)
                } else {
                    (top + 1).min(max)
                };
            }
            return;
        }
        let (w, h) = self.size;
        if self.mode != Mode::Board || w < NEED.0 || h < NEED.1 || row < 2 || row >= h - 2 {
            return;
        }
        let Some(n) = self.lane_at(col) else {
            return;
        };
        let last = self.lanes()[n].len().saturating_sub(self.room(n));
        let top = self.top[n].min(last);
        self.top[n] = if up {
            top.saturating_sub(1)
        } else {
            (top + 1).min(last)
        };
    }

    /// What `H` or `L` on `item` asks for, and the line said after it.
    fn moved(&self, item: &ItemInfo, right: bool) -> Step {
        let Some(m) = move_of(&item.state, right) else {
            return Step::Stay;
        };
        let key = item.key.clone();
        match m {
            Move::Start => Step::Start(item.id, key),
            Move::Close => Step::Write(
                item.id,
                TaskOp::Close {
                    outcome: TaskOutcome::Verified,
                    reason: "moved to Done on the board".into(),
                    actor: None,
                },
                format!("{key} to done: closed verified"),
            ),
            Move::Unlink => Step::Write(
                item.id,
                TaskOp::Unlink,
                format!("{key} to todo: its agents are off it"),
            ),
            Move::Reopen => Step::Write(
                item.id,
                TaskOp::Reopen,
                format!("{key} back to todo: reopened"),
            ),
        }
    }

    /// The left button on the board: a press on a card selects it and holds it; let go in
    /// the page it is just selected; let go over qass's pane (the interface says so, the page
    /// cannot see past its edge) a Todo card is handed to qass as `L` hands it, any other
    /// says why not. Only on the board itself, not over a dialog or while typing.
    fn mouse(&mut self, key: Key) -> Step {
        match key {
            Key::Mouse {
                down: true,
                held: false,
                col,
                row,
            } if matches!(self.mode, Mode::Card(_)) => {
                return self.click(col as usize, row as usize);
            }
            Key::Mouse {
                down: true,
                held: false,
                col,
                row,
            } if self.mode == Mode::Board => {
                let (col, row) = (col as usize, row as usize);
                self.wandered = false;
                let view = self.view_at(self.size.0);
                if row == 0
                    && view > 6
                    && self.view_shown(self.size.0)
                    && (view..view + SWITCH_W).contains(&col)
                {
                    self.held = None;
                    return self.turn_view();
                }
                if self.view == View::List {
                    self.held = None;
                    return self.list_click(row);
                }
                // A Done card's cross puts it away (or back): never also a press that holds it.
                if let Some(id) = self.cross_at(col, row) {
                    self.held = None;
                    let item = self
                        .items
                        .as_ref()
                        .and_then(|v| v.iter().find(|i| i.id == id));
                    return item.map_or(Step::Stay, |item| self.dismissed(item));
                }
                let at = self.switch_at(self.size.0);
                // " board" ends at column 6; a switch with no room is not drawn and not clicked.
                if row == 0 && at > 6 && (at..at + SWITCH_W).contains(&col) {
                    self.held = None;
                    return self.turn_cards();
                }
                if self.on_hidden_line(col, row) {
                    self.held = None;
                    self.reveal = !self.reveal;
                    self.settle();
                    return Step::Stay;
                }
                if let Some(hit) = self.loose_hit(col, row) {
                    self.held = None;
                    return match hit {
                        Hit::Toggle => {
                            self.turn_loose();
                            Step::Stay
                        }
                        // Selects, as a click on a card does: enter goes to its tab.
                        Hit::Line(i) => {
                            (self.lane, self.loose_at) = (1, Some(i));
                            self.loose_follow();
                            Step::Stay
                        }
                    };
                }
                self.loose_at = None;
                let placed = self.placed(col, row);
                self.held_at = placed.map(|(_, _, at)| at);
                self.held = placed.map(|(lane, id, _)| {
                    (self.lane, self.sel) = (lane, Some(id));
                    self.follow();
                    id
                });
                if let Some(item) = self.selected().filter(|_| self.held.is_some()) {
                    self.said = (item.state == "todo")
                        .then(|| format!("drag onto qass to start {}", item.key));
                }
            }
            // Carried off the card it was pressed on: what comes up later is no click.
            Key::Mouse {
                down: true,
                held: true,
                col,
                row,
            } => {
                if !self.on_held(col as usize, row as usize) {
                    self.wandered = true;
                }
            }
            // Pressed and let go on the same card, never carried off it: a click, which opens it
            // as `enter` does.
            Key::Mouse {
                down: false,
                col,
                row,
                ..
            } => {
                let on = self.on_held(col as usize, row as usize);
                let held = self.held.take();
                self.held_at = None;
                if self.mode == Mode::Board
                    && !self.wandered
                    && on
                    && let Some(id) = held
                {
                    return Step::Open(id);
                }
            }
            Key::Dropped(on_qass) => {
                // Only on the board itself: a dialog or a field typed in has the keys.
                let held = self.held.take().filter(|_| self.mode == Mode::Board);
                let item = held.and_then(|id| self.items.as_ref()?.iter().find(|i| i.id == id));
                if let (true, Some(item)) = (on_qass, item) {
                    return match item.state.as_str() {
                        "todo" => self.moved(item, true),
                        "doing" => {
                            self.said = Some(format!("{} is already in doing", item.key));
                            Step::Stay
                        }
                        _ => {
                            self.said = Some(format!("reopen {} first", item.key));
                            Step::Stay
                        }
                    };
                }
            }
            _ => {}
        }
        Step::Stay
    }

    /// A press on the list: a row opens its item, the Done row opens or folds, the folded row
    /// at the foot opens, or takes the keyboard to a line of it. A row spans the page's width.
    fn list_click(&mut self, row: usize) -> Step {
        let h = self.size.1.max(FLOOR.1);
        let loose = self.sect_rows(1, h - 4);
        if loose > 0 && row + 2 + loose >= h && row + 2 < h {
            match row + 2 + loose - h {
                0 | 1 => self.turn_loose(),
                n => {
                    self.loose_at = Some(self.loose_first(loose - 2) + n - 2);
                    self.loose_follow();
                }
            }
            return Step::Stay;
        }
        let room = self.list_room();
        let hit = {
            let lanes = self.lanes();
            let lines = self.list_lines(&lanes);
            let first = self.list_top.min(lines.len().saturating_sub(room));
            row.checked_sub(self.list_y())
                .filter(|r| *r < room)
                .and_then(|r| lines.get(first + r))
                .and_then(|l| match l {
                    Line::Item(i) => Some(i.id),
                    Line::Fold(_) => Some(FOLD),
                    Line::Group(..) => None,
                })
        };
        self.loose_at = None;
        match hit {
            Some(FOLD) => {
                self.sel = Some(FOLD);
                self.done_open = !self.done_open;
                self.settle();
                Step::Stay
            }
            Some(id) => {
                self.sel = Some(id);
                self.settle();
                Step::Open(id)
            }
            None => Step::Stay,
        }
    }

    /// The card drawn at a cell of the page, with its lane: the lanes and cards as
    /// [`Page::draw`] places them at the size it last drew.
    #[cfg(test)]
    fn card_at(&self, col: usize, row: usize) -> Option<(usize, i64)> {
        self.placed(col, row).map(|(n, id, _)| (n, id))
    }

    /// The card under a cell, with its lane and its top left cell and width, as drawn.
    fn placed(&self, col: usize, row: usize) -> Option<(usize, i64, (usize, usize, usize))> {
        let (w, h) = self.size;
        // Too small, the page shows only how big to make it: no card is under the pointer.
        if w < NEED.0 || h < NEED.1 {
            return None;
        }
        let lw = (w - 2 - 4) / 3;
        let (top, bottom) = (2, h - 2);
        let lanes = self.lanes();
        let n = self.lane_at(col)?;
        let (first, room) = self.shown(n, lanes[n].len(), bottom - top);
        let at = row.checked_sub(top + 1)? / self.card_h();
        (at < room)
            .then(|| lanes[n].get(first + at))
            .flatten()
            .map(|item| {
                let place = (1 + n * (lw + 2) + 1, top + 1 + at * self.card_h(), lw - 2);
                (n, item.id, place)
            })
    }

    /// Whether a cell is on the card held, where it was drawn when pressed.
    fn on_held(&self, col: usize, row: usize) -> bool {
        self.held.is_some()
            && self.held_at.is_some_and(|(x, y, w)| {
                col >= x && col < x + w && row >= y && row < y + self.card_h()
            })
    }

    /// Whether a cell is a Done card's cross: its top edge, one in from the right corner.
    fn cross_at(&self, col: usize, row: usize) -> Option<i64> {
        let (_, id, (x, y, w)) = self.placed(col, row)?;
        let item = self.items.as_ref()?.iter().find(|i| i.id == id)?;
        (item.state == "done" && row == y && col == x + w - 2).then_some(id)
    }

    /// Whether a cell is on the Done lane's foot, where `hidden (N)` is.
    fn on_hidden_line(&self, col: usize, row: usize) -> bool {
        let (w, h) = self.size;
        if w < NEED.0 || h < NEED.1 || self.hidden() == 0 {
            return false;
        }
        let lw = (w - 2 - 4) / 3;
        let x = 1 + 2 * (lw + 2);
        row == h - 4 && col > x && col < x + lw - 1
    }

    /// The first card a lane of `len` cards shows and how many fit in its `h` rows: the lane's
    /// own scroll, never past its last card.
    fn shown(&self, n: usize, len: usize, h: usize) -> (usize, usize) {
        let room = (h - 3).saturating_sub(self.sect_rows(n, h)) / self.card_h();
        (self.top[n].min(len.saturating_sub(room)), room)
    }

    /// One key.
    fn key(&mut self, key: Key) -> Step {
        if key == Key::Stop {
            return Step::Leave;
        }
        if let Key::Wheel { up, col, row } = key {
            self.wheel(up, col as usize, row as usize);
            return Step::Stay;
        }
        if matches!(key, Key::Mouse { .. } | Key::Dropped(_)) {
            return self.mouse(key);
        }
        let step = self.typed(key);
        // A key that takes the page off the board lets go of a card held there.
        if self.mode != Mode::Board {
            self.held = None;
        }
        step
    }

    /// A key, not the mouse.
    fn typed(&mut self, key: Key) -> Step {
        match self.mode.clone() {
            Mode::Board => self.board_key(key),
            Mode::Card(at) => self.card_key(key, at),
            Mode::Filter => {
                match key {
                    Key::Enter => self.mode = Mode::Board,
                    Key::Esc => {
                        self.filter.clear();
                        self.mode = Mode::Board;
                    }
                    other => edit(&mut self.filter, other),
                }
                self.settle();
                self.follow();
                Step::Stay
            }
            Mode::Create { mut title, space } => {
                let n = self.targets().len().max(1);
                match key {
                    Key::Esc => self.mode = Mode::Board,
                    Key::Tab => {
                        self.mode = Mode::Create {
                            title,
                            space: (space + 1) % n,
                        }
                    }
                    Key::BackTab => {
                        self.mode = Mode::Create {
                            title,
                            space: (space + n - 1) % n,
                        }
                    }
                    Key::Enter => {
                        self.mode = Mode::Board;
                        let goal = title.trim().to_string();
                        if let (false, Some(w)) = (goal.is_empty(), self.targets().get(space)) {
                            return Step::Add {
                                workspace: w.id,
                                goal,
                            };
                        }
                    }
                    other => {
                        edit(&mut title, other);
                        self.mode = Mode::Create { title, space };
                    }
                }
                Step::Stay
            }
            Mode::Tell(mut text) => {
                match key {
                    Key::Esc => self.mode = Mode::Board,
                    Key::Enter => {
                        self.mode = Mode::Board;
                        if !text.trim().is_empty() {
                            return Step::Tell(text);
                        }
                    }
                    other => {
                        edit(&mut text, other);
                        self.mode = Mode::Tell(text);
                    }
                }
                Step::Stay
            }
        }
    }

    fn board_key(&mut self, key: Key) -> Step {
        match key {
            Key::Char('q') => return Step::Leave,
            Key::Esc => self.filter.clear(),
            // The list is one column: only the lanes go sideways.
            Key::Left | Key::Char('h') if self.view == View::Board => self.side(-1),
            Key::Right | Key::Char('l') if self.view == View::Board => self.side(1),
            Key::Up | Key::Char('k') => self.step(-1),
            Key::Down | Key::Char('j') => self.step(1),
            Key::Enter => {
                if let Some(i) = self.loose_cursor() {
                    return self.goto(i);
                }
                if self.view == View::List && self.sel == Some(FOLD) {
                    self.done_open = !self.done_open;
                } else if let Some(item) = self.selected() {
                    return Step::Open(item.id);
                }
            }
            Key::Char(c @ ('H' | 'L')) => {
                if let Some(item) = self.selected().filter(|_| self.loose_cursor().is_none()) {
                    return self.moved(item, c == 'L');
                }
            }
            Key::Char('x') => {
                if let Some(item) = self.selected().filter(|_| self.loose_cursor().is_none()) {
                    return self.dismissed(item);
                }
            }
            Key::Char('X') => self.reveal = !self.reveal,
            Key::Char('N') => self.turn_loose(),
            Key::Char('/') => self.mode = Mode::Filter,
            Key::Char('v') => return self.turn_cards(),
            Key::Char('b') => return self.turn_view(),
            Key::Char('t') => self.mode = Mode::Tell(String::new()),
            Key::Char('c') => {
                let here = self.selected().and_then(|i| i.workspace);
                let space = self
                    .targets()
                    .iter()
                    .position(|w| Some(w.id) == here)
                    .unwrap_or(0);
                self.mode = Mode::Create {
                    title: String::new(),
                    space,
                };
            }
            _ => {}
        }
        self.settle();
        Step::Stay
    }

    fn card_key(&mut self, key: Key, at: usize) -> Step {
        let Some(card) = &self.card else {
            self.mode = Mode::Board;
            return Step::Stay;
        };
        let steps = card.steps.len();
        let item = card.item.clone();
        let step = card.steps.get(at).cloned();
        self.scroll = self.scroll.min(self.max_scroll());
        let page = self.pane().view().saturating_sub(1).max(1);
        match key {
            Key::Esc | Key::Char('q') => {
                self.mode = Mode::Board;
                self.card = None;
            }
            Key::Up | Key::Char('k') => self.go(at, false),
            Key::Down | Key::Char('j') => self.go(at, true),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(page),
            Key::PageDown => self.scroll = (self.scroll + page).min(self.max_scroll()),
            Key::Home => self.scroll = 0,
            Key::End => self.scroll = self.max_scroll(),
            Key::Enter => {
                if let Some(entry) = at.checked_sub(steps) {
                    self.toggle(entry);
                }
            }
            Key::Char('E') => {
                self.all = !self.all;
                self.flipped.clear();
            }
            Key::Char('x') | Key::Char(' ') => {
                // A phase's heading is never a box to tick.
                if let Some(step) = step.filter(|s| !qoop_store::item::is_phase(&s.text)) {
                    let done = step.done_at.is_none();
                    let said = format!(
                        "{} step {} {}",
                        item.key,
                        step.n,
                        if done { "ticked" } else { "unticked" }
                    );
                    return Step::Write(item.id, TaskOp::Check { n: step.n, done }, said);
                }
            }
            Key::Char(c @ ('H' | 'L')) => return self.moved(&item, c == 'L'),
            Key::Char('t') => self.mode = Mode::Tell(String::new()),
            _ => {}
        }
        Step::Stay
    }

    fn draw(&self, w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        let x = if self.in_qass_tab {
            0
        } else {
            s.put(1, 0, "board", &bold(Role::Title)) + 1
        };
        let list = self.view == View::List;
        // The heading's words end before the switches: the card switch only on the lanes.
        let end = if list {
            self.view_at(w)
        } else {
            self.switch_at(w)
        };
        let sub = "your tasks, run by qass";
        // How many wait on him, as the list's first group counts them under the filter: before
        // the sub line when only one fits.
        let need = groups(&self.lanes())[0].len();
        let said = if list && need > 0 {
            format!("{need} need you")
        } else {
            String::new()
        };
        let mut x = x;
        let sub_w = sub.chars().count() + if said.is_empty() { 0 } else { 2 };
        if x + 1 + sub_w + said.len() + 2 <= end {
            x = s.put(x + 1, 0, sub, &ink(Role::FaintText)) + 1;
        }
        if !said.is_empty() && x + 1 + said.len() + 2 <= end {
            s.put(x + 1, 0, &said, &bold(Role::Blocked));
        }
        let search = self.search();
        let sx = self.search_at(w);
        // A switch: the view picked in brand ink, the other faint, its key after them.
        let pick = |on: bool| {
            if on {
                bold(Role::BrandText)
            } else {
                ink(Role::FaintText)
            }
        };
        let switch = |s: &mut Screen, at: usize, (a, b): (&str, &str), first: bool, key: &str| {
            s.seg(
                if at > 6 { at } else { w },
                0,
                &[
                    ("[ ", ink(Role::Brand)),
                    (a, pick(first)),
                    (" · ", ink(Role::FaintText)),
                    (b, pick(!first)),
                    (key, ink(Role::Detail)),
                    (" ]", ink(Role::Brand)),
                ],
            );
        };
        if !list {
            switch(
                &mut s,
                self.switch_at(w),
                ("full", "quiet"),
                !self.quiet,
                "  v",
            );
        }
        if self.view_shown(w) {
            switch(&mut s, self.view_at(w), ("list", "board"), list, "  b");
        }
        s.seg(
            sx,
            0,
            &[
                ("[ ", ink(Role::Brand)),
                (&search, ink(Role::Detail)),
                (" ]", ink(Role::Brand)),
            ],
        );
        s.put(1, 1, &"─".repeat(w - 2), &ink(Role::Rule));
        let Some(_) = &self.items else {
            s.put(
                1,
                3,
                "the daemon is older than the board: it needs a restart to show it",
                &ink(Role::Blocked),
            );
            self.foot(&mut s);
            return s;
        };
        let lanes = self.lanes();
        // The list at every width the page draws; the lanes one at a time under their size.
        if list {
            self.list(&mut s, &lanes);
        } else if w < NEED.0 || h < NEED.1 {
            self.narrow(&mut s, &lanes);
        } else {
            // Three lanes, two columns apart.
            let lw = (w - 2 - 4) / 3;
            let (top, bottom) = (2, h - 2);
            for (n, lane) in lanes.iter().enumerate() {
                let lx = 1 + n * (lw + 2);
                self.lane_box(&mut s, n, lane, lx, top, lw, bottom - top);
            }
        }
        // The issue over the page, edge on edge with the lanes: one box, no lane border beside
        // it; over the one lane or the list of a narrow page at its size.
        if let Mode::Card(at) = self.mode
            && let Some(card) = &self.card
        {
            let (corner, size) = Self::dialog_box((w, h));
            self.dialog(&mut s, card, at, corner, size);
        }
        self.foot(&mut s);
        s
    }

    /// A page too small for three lanes: the keyboard's lane alone, `h` and `l` to the next,
    /// named with the others on the rule's row (the others as arrows when they do not fit). A
    /// box with its cards while there is room for a card, else a list of keys and titles. A new
    /// card is made on Todo, so its form shows Todo.
    fn narrow(&self, s: &mut Screen, lanes: &[Vec<&ItemInfo>; 3]) {
        let (w, h) = (s.w, s.h);
        let creating = match &self.mode {
            Mode::Create { title, space } => Some((title, *space)),
            _ => None,
        };
        let shown = if creating.is_some() { 0 } else { self.lane };
        s.put(1, 1, &" ".repeat(w - 2), &ink(Role::Text));
        let names: Vec<String> = (0..3)
            .map(|n| format!("{} {}", LANES[n], lanes[n].len()))
            .collect();
        let all = names.iter().map(|n| n.len() + 2).sum::<usize>() + 3;
        let mut x = 1;
        if all <= w - 2 {
            for (n, name) in names.iter().enumerate() {
                let esc = if n == shown {
                    bold(Role::BrandText)
                } else {
                    ink(Role::FaintText)
                };
                x = s.put(x, 1, name, &esc) + 2;
            }
            s.put(x, 1, "h l", &ink(Role::FaintText));
        } else {
            let before = if shown > 0 { "\u{2039} " } else { "  " };
            x = s.put(x, 1, before, &ink(Role::FaintText));
            x = s.put(x, 1, &names[shown], &bold(Role::BrandText));
            if shown < 2 {
                s.put(x, 1, " \u{203a}", &ink(Role::FaintText));
            }
        }
        let lane = &lanes[shown];
        if !self.listed((w, h), shown) {
            self.lane_box(s, shown, lane, 1, 2, w - 2, h - 4);
            // The rule's row above names the lane: its box keeps a plain edge.
            s.put(1, 2, &format!("┌{}┐", "─".repeat(w - 4)), &ink(Role::Rule));
            return;
        }
        let mut top = 2;
        if let Some((title, space)) = creating {
            self.form(s, top, title, space);
            top += 2;
        }
        let rows = h - 2 - top;
        let first = self.top[shown].min(lane.len().saturating_sub(rows));
        for (row, item) in lane.iter().skip(first).take(rows).enumerate() {
            let on = Some(item.id) == self.sel;
            let (lead, esc) = if on {
                ("\u{25b8} ", bold(Role::Focused))
            } else {
                ("  ", ink(Role::Text))
            };
            let line = fit(&format!("{lead}{} {}", item.key, item.title), w - 2);
            s.put(1, top + row, &line, &esc);
        }
        if lane.is_empty() && creating.is_none() {
            s.put(3, top, "nothing here", &ink(Role::FaintText));
        }
    }

    /// The new item's form on a list's first two rows from `top`: the title typed, and the space
    /// it goes to.
    fn form(&self, s: &mut Screen, top: usize, title: &str, space: usize) {
        let room = s.w - 4;
        let end: String = {
            let n = title.chars().count();
            title.chars().skip(n.saturating_sub(room)).collect()
        };
        let cx = s.put(1, top, "+ ", &ink(Role::Brand));
        let cx = s.put(cx, top, &end, &bold(Role::Focused));
        s.put(cx, top, "\u{258c}", &ink(Role::Brand));
        let to = self
            .targets()
            .get(space)
            .map_or("no space to add to".to_string(), |t| {
                format!("to {}, tab space", t.name)
            });
        s.put(3, top + 1, &fit(&to, s.w - 4), &ink(Role::FaintText));
    }

    /// The list view: a rule with its count over each group's rows, Done folded to its rule
    /// until enter opens it, and the work no item holds folded at the foot. Rows out of view
    /// are counted on the rule under the heading.
    fn list(&self, s: &mut Screen, lanes: &[Vec<&ItemInfo>; 3]) {
        let w = s.w;
        let top = self.list_y();
        if let Mode::Create { title, space } = &self.mode {
            self.form(s, 2, title, *space);
        }
        let lines = self.list_lines(lanes);
        if lines.is_empty() && top == 2 {
            let say = if self.filter.trim().is_empty() {
                EMPTY
            } else {
                "Nothing matches the filter. esc clears it."
            };
            s.put(3, top, &fit(say, w - 4), &ink(Role::FaintText));
        }
        let room = (s.h - 2)
            .saturating_sub(top + self.sect_rows(1, s.h - 4))
            .max(1);
        let first = self.list_top.min(lines.len().saturating_sub(room));
        let key_w = lines
            .iter()
            .filter_map(|l| match l {
                Line::Item(i) => Some(i.key.chars().count()),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .min(12)
            .min(w.saturating_sub(9) / 3);
        let quiet = self.mode == Mode::Filter || self.loose_cursor().is_some();
        for (r, line) in lines.iter().skip(first).take(room).enumerate() {
            let y = top + r;
            match line {
                Line::Group(n, count) => self.group_rule(s, y, *n, *count, ""),
                Line::Fold(count) => {
                    let mut hint = if self.done_open {
                        "enter folds".to_string()
                    } else {
                        "enter opens".to_string()
                    };
                    match (self.hidden(), self.reveal) {
                        (0, _) => {}
                        (n, false) => hint = format!("hidden ({n})  X  {hint}"),
                        (n, true) => hint = format!("hidden ({n}) shown  X  {hint}"),
                    }
                    if self.sel == Some(FOLD) && !quiet {
                        s.put(1, y, "\u{25b8}", &ink(Role::Brand));
                    }
                    self.group_rule(s, y, 3, *count, &hint);
                }
                Line::Item(item) => {
                    let on = Some(item.id) == self.sel && !quiet;
                    self.list_row(s, item, y, key_w, on);
                }
            }
        }
        let below = lines.len().saturating_sub(first + room);
        let marks = [(first, "above"), (below, "below")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, word)| format!(" {n} {word} "))
            .collect::<String>();
        if !marks.is_empty() {
            s.put(
                w.saturating_sub(2 + width::width(&marks)),
                1,
                &marks,
                &ink(Role::FaintText),
            );
        }
        // The work no item holds, at the foot: its label and count line, its lines opened.
        let rows = self.sect_rows(1, s.h - 4);
        if rows > 0 {
            self.loose_row(s, 0, w, (s.h - 2 - rows, rows));
        }
    }

    /// A group's rule on row `y`: its name in its ink, its count, the rule to the edge with
    /// `hint` near its end.
    fn group_rule(&self, s: &mut Screen, y: usize, n: usize, count: usize, hint: &str) {
        let (name, role) = GROUPS[n];
        let x = s.put(3, y, name, &bold(role));
        let x = s.put(x, y, &format!(" {count} "), &ink(Role::FaintText));
        let end = s.w - 1;
        s.put(
            x,
            y,
            &"\u{2500}".repeat(end.saturating_sub(x)),
            &ink(Role::Rule),
        );
        let hint = format!(" {hint} ");
        if hint.len() > 2 && x + 2 + width::width(&hint) + 2 <= end {
            s.put(
                end - 2 - width::width(&hint),
                y,
                &hint,
                &ink(Role::FaintText),
            );
        }
    }

    /// One item's row: the keyboard's arrow, its mark, key and title, then right of it its
    /// parts as fit, the rightmost given up first and a word that wants the owner last. The
    /// mark, the key and the title always stay; a title is cut by cells.
    fn list_row(&self, s: &mut Screen, item: &ItemInfo, y: usize, key_w: usize, on: bool) {
        let end = s.w - 1;
        if on {
            s.put(1, y, "\u{25b8}", &ink(Role::Brand));
        }
        let (mark, role) = item_mark(item);
        s.put(3, y, &mark, &ink(role));
        s.put(6, y, &cut_key(&item.key, key_w), &ink(Role::FaintText));
        let tx = 6 + key_w + 2;
        let room = end.saturating_sub(tx);
        let keep = width::width(&item.title).min(TITLE_MIN);
        let parts = drop_parts(row_parts(item, self.now), room.saturating_sub(keep));
        let title = match (on, item.state == "done") {
            (true, _) => bold(Role::Focused),
            (false, true) => ink(Role::Detail),
            (false, false) => ink(Role::Text),
        };
        s.put(tx, y, &fit(&item.title, room - parts_w(&parts)), &title);
        let mut x = end - parts_w(&parts) + 2;
        for (text, role) in &parts {
            x = s.put(x, y, text, &ink(*role)) + 2;
        }
    }

    /// One lane: its box, its name and count, the cards that fit, and its foot.
    #[allow(clippy::too_many_arguments)]
    fn lane_box(
        &self,
        s: &mut Screen,
        n: usize,
        lane: &[&ItemInfo],
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) {
        let rule = ink(Role::Rule);
        let name = LANES[n];
        let role = [Role::Text, Role::WorkingText, Role::Done][n];
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &rule);
        s.seg(
            x + 2,
            y,
            &[
                (" ", rule.clone()),
                (name, bold(role)),
                (&format!(" {} ", lane.len()), ink(Role::FaintText)),
            ],
        );
        for row in y + 1..y + h - 1 {
            s.put(x, row, "│", &rule);
            s.put(x + w - 1, row, "│", &rule);
        }
        s.put(x, y + h - 1, &format!("└{}┘", "─".repeat(w - 2)), &rule);
        // The cards: as many as fit above the lane's foot, scrolled to keep the keyboard's.
        let (first, room) = self.shown(n, lane.len(), h);
        let mut cy = y + 1;
        for item in lane.iter().skip(first).take(room) {
            let on = n == self.lane
                && Some(item.id) == self.sel
                && self.mode != Mode::Filter
                && self.loose_cursor().is_none();
            self.card(s, item, x + 1, cy, w - 2, on);
            cy += self.card_h();
        }
        if let Mode::Create { title, space } = &self.mode
            && n == 0
        {
            let cy = (y + 1 + lane.len().min(room) * self.card_h())
                .min(y + 1 + (room.max(1) - 1) * self.card_h());
            self.new_card(s, title, *space, x + 1, cy, w - 2);
        }
        if n == 1
            && let Some(place) = self.loose_place(y, h)
        {
            self.loose_row(s, x, w, place);
        }
        let below = lane.len().saturating_sub(first + room);
        let foot = match n {
            0 => "+ create issue  c".to_string(),
            2 => match (self.hidden(), self.reveal) {
                (0, _) => "closed in 24 h".to_string(),
                (n, false) => format!("hidden ({n})  X"),
                (n, true) => format!("hidden ({n}) shown  X"),
            },
            _ => String::new(),
        };
        // How many cards are out of view each way, ahead of the lane's own hint.
        let marks = [(first, "above"), (below, "below")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, word)| format!("{n} {word}  "))
            .collect::<String>();
        let foot = format!("{marks}{foot}");
        s.put(x + 2, y + h - 2, &fit(&foot, w - 4), &ink(Role::FaintText));
    }

    /// How many of the folded row's work wait on the owner, need, break, sleep and so on, in the
    /// order of [`NEED_ORDER`] (`3 wait on you · 15 qass checks · 1 error`), with the ink the
    /// line wears: Blocked when anything waits, Failed when anything broke, else faint.
    fn loose_counts(&self) -> (String, Role) {
        let count =
            |keep: &dyn Fn(&str) -> bool| self.tasks.iter().filter(|t| keep(&t.status)).count();
        let mut chunks = Vec::new();
        for status in NEED_ORDER {
            let n = count(&|s| s == status);
            let word = if status == "waits" {
                "wait on you"
            } else {
                status_word(status).map_or(status, |(word, _)| word)
            };
            if n > 0 {
                chunks.push(format!("{n} {word}"));
            }
        }
        let other = count(&|s| !NEED_ORDER.contains(&s));
        if other > 0 {
            chunks.push(format!("{other} other"));
        }
        // A board that outgrew the frame sent the quietest ones' lines short: say so.
        let unlisted = self.loose.unwrap_or(0).saturating_sub(self.tasks.len());
        if unlisted > 0 {
            chunks.push(format!("{unlisted} not listed"));
        }
        let role = if count(&|s| s == "waits") > 0 {
            Role::Blocked
        } else if count(&|s| s == "error") > 0 {
            Role::FailedText
        } else {
            Role::FaintText
        };
        (chunks.join(" · "), role)
    }

    /// One line of the opened row, left of its goal: `T<terminal> <space> <word> · <age>`, its
    /// ink, and whether it is dimmed. A task with no terminal keeps its word and adds
    /// ` · no tab`, the whole line dimmed. A status with no word of its own (`unknown`,
    /// `remote`) says the task's stage, in plain ink.
    fn loose_head(&self, t: &TaskInfo) -> (String, Role, bool) {
        let terminal = t.terminal.map(|n| format!("T{n} ")).unwrap_or_default();
        let space = space_name(&self.spaces, t.workspace);
        let (word, role) = status_said(&t.status, t.status_at, self.now)
            .unwrap_or_else(|| (aged(&t.stage, t.status_at, self.now), Role::FaintText));
        if t.terminal.is_some() {
            return (format!("{terminal}{space} {word}"), role, false);
        }
        // Its word keeps its ink: a task that waits on him reads so with no tab too.
        (format!("{space} {word} · {NO_TAB}"), role, true)
    }

    /// The folded row in a box at `x`, `w` wide, at its first row and with its rows (at the
    /// foot of Doing, or of the list): its label with the count of all of it, its count line,
    /// and while it is open a line per task.
    fn loose_row(&self, s: &mut Screen, x: usize, w: usize, (top, rows): (usize, usize)) {
        let inner = w - 4;
        let label = format!("{LOOSE} · {}", self.loose.unwrap_or(0));
        let at = s.put(
            x + 2,
            top,
            &fit(&label, inner.saturating_sub(3)),
            &ink(Role::Detail),
        );
        if at + 3 <= x + 2 + inner {
            s.put(at + 2, top, "N", &ink(Role::FaintText));
        }
        let (counts, role) = self.loose_counts();
        s.put(x + 2, top + 1, &fit(&counts, inner), &ink(role));
        let lines = self.loose_lines();
        let first = self.loose_first(rows - 2);
        for (n, t) in lines.iter().enumerate().skip(first).take(rows - 2) {
            let row = top + 2 + n - first;
            let on = self.loose_cursor() == Some(n) && self.mode != Mode::Filter;
            if on {
                s.put(x + 1, row, "›", &ink(Role::Brand));
            }
            let (head, role, dim) = self.loose_head(t);
            let head = fit(&head, inner);
            let end = s.put(x + 2, row, &head, &ink(role));
            let goal_w = (x + 2 + inner).saturating_sub(end + 2);
            if goal_w >= 4 {
                let goal = match (dim, on) {
                    (true, _) => ink(Role::FaintText),
                    (false, true) => bold(Role::Focused),
                    (false, false) => ink(Role::Text),
                };
                s.put(
                    end + 2,
                    row,
                    &fit(&qoop_store::task::one_line(&t.goal), goal_w),
                    &goal,
                );
            }
        }
    }

    /// One card: the space on its top edge, two lines of title, then key, steps, assignee.
    fn card(&self, s: &mut Screen, item: &ItemInfo, x: usize, y: usize, w: usize, on: bool) {
        let edge = if on {
            ink(Role::Brand)
        } else {
            ink(Role::Rule)
        };
        let space = space_name(&self.spaces, item.workspace);
        let space_ink = item.workspace.map_or(ink(Role::FaintText), |id| {
            ink(theme::space_in(&self.spaces, id))
        });
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &edge);
        s.put(x + 2, y, &fit(&format!(" {space} "), w - 6), &space_ink);
        // A Done card's cross puts it away; one put away (shown dimmed) brings it back.
        if item.state == "done" {
            if item.hidden {
                s.put(x + w - 8, y, " show ", &ink(Role::Detail));
            }
            let cross = if item.hidden {
                Role::Detail
            } else {
                Role::FaintText
            };
            s.put(x + w - 2, y, "x", &ink(cross));
        }
        for row in y + 1..y + self.card_h() - 1 {
            s.put(x, row, "│", &edge);
            s.put(x + w - 1, row, "│", &edge);
        }
        s.put(
            x,
            y + self.card_h() - 1,
            &format!("└{}┘", "─".repeat(w - 2)),
            &edge,
        );
        let inner = w - 4;
        let title = if on {
            bold(Role::Focused)
        } else {
            ink(Role::Text)
        };
        // One word, decided once for both views: how a closed card ended, else its agent.
        let (who, word, role, acts) = assignee(item, self.now);
        // How long it has been in its lane, and its agent at work: measured, never estimated.
        let lane_age = span(item.since, self.now);
        let agent_age = item
            .agents
            .iter()
            .find(|a| !a.closed)
            // Its age first: a narrow card cuts the name, never the number.
            .map(|a| format!("{} {}", span(a.since, self.now), a.name));
        if self.quiet {
            // The ages ride on the top edge, before a Done card's cross ("show" is there when
            // it is put away).
            let ages = match &agent_age {
                Some(agent) if item.state != "done" => format!(" {lane_age} · {agent} "),
                _ => format!(" {lane_age} "),
            };
            // By cells: a wide name takes two a character.
            let room = w.saturating_sub(width::width(&space) + 12);
            // Cut, it keeps one space before the rule.
            let ages = if width::width(&ages) > room {
                format!("{} ", fit(&ages, room.saturating_sub(1)))
            } else {
                ages
            };
            if !item.hidden && !ages.trim().is_empty() {
                s.put(
                    x + w - 4 - width::width(&ages),
                    y,
                    &ages,
                    &ink(Role::FaintText),
                );
            }
            // A narrow card keeps room for its title: the short form of the word, and the age
            // only where there is room for both.
            let shown = match (acts, inner) {
                (false, _) => String::new(),
                (true, 30..) => word.clone(),
                (true, 20..) => short(&word),
                (true, _) => bare(&word),
            };
            let done = item.state == "done";
            let lead = if done { "✓ " } else { "" };
            let tw = inner.saturating_sub(shown.chars().count() + 1 + lead.chars().count());
            let tx = s.put(x + 2, y + 1, lead, &ink(Role::Done));
            let text = if done { ink(Role::Detail) } else { title };
            s.put(tx, y + 1, &fit(&item.title, tw), &text);
            s.put(x + w - 2 - shown.chars().count(), y + 1, &shown, &ink(role));
            return;
        }
        // The title keeps its whole row: it is what tells one card from another. The foot: the
        // key, its age in the lane, then its agent and how long it has worked while there is
        // room, and the word flush right in the room the whole key leaves: its longest form
        // that fits, down to its bare one, else none (the card's ink still says the state). A
        // word that wants the owner (waits, blocked, failed) always shows, and the key gives
        // way. A key is cut in the middle, never past its number (`QOOPP…-1`).
        s.put(x + 2, y + 1, &fit(&item.title, inner), &title);
        let (fy, end) = (y + 2, x + w - 2);
        let urgent = acts || matches!(role, Role::Blocked | Role::Failed | Role::FailedText);
        let fits = |key: usize| {
            [word.clone(), short(&word), bare(&word)]
                .into_iter()
                .find(|w| w.chars().count() + key <= inner)
        };
        let key_min = cut_key(&item.key, 0).chars().count() + 1;
        let word = fits(item.key.chars().count() + 1)
            .or_else(|| urgent.then(|| fits(key_min).unwrap_or_else(|| bare(&word))))
            .unwrap_or_default();
        let wx = end - word.chars().count();
        s.put(wx, fy, &word, &ink(role));
        let kx = s.put(
            x + 2,
            fy,
            &cut_key(
                &item.key,
                match word.is_empty() {
                    true => inner,
                    false => wx.saturating_sub(x + 3),
                },
            ),
            &ink(Role::FaintText),
        );
        // A Done card says when it closed: `just now`, `3 min ago`.
        let lane_age = if item.state == "done" && item.since != 0 {
            ago(Some(item.since), self.now)
        } else {
            lane_age
        };
        // The age whole or not at all: a cut one reads as another age.
        let ax = if lane_age.chars().count() <= wx.saturating_sub(kx + 3) {
            s.put(kx + 2, fy, &lane_age, &ink(Role::Detail))
        } else {
            kx
        };
        let steps =
            (item.steps_total > 0).then(|| format!("{}/{}", item.steps_done, item.steps_total));
        let mut cx = ax;
        if let Some(steps) = steps.filter(|t| t.chars().count() + 3 <= wx.saturating_sub(cx)) {
            let role = if item.steps_done == item.steps_total {
                Role::Done
            } else {
                Role::Detail
            };
            cx = s.put(cx + 2, fy, &steps, &ink(role));
        }
        let agent = match agent_age {
            Some(agent) if item.state != "done" => agent,
            _ => who,
        };
        let agent = fit(&agent, wx.saturating_sub(cx + 3));
        // By cells: a wide name takes two a character.
        let agent_w = width::width(&agent);
        if agent_w >= 4 {
            s.put(wx - 1 - agent_w, fy, &agent, &ink(Role::Text));
        }
    }

    /// The card being typed: its title with the cursor, then the space it goes to.
    fn new_card(&self, s: &mut Screen, title: &str, space: usize, x: usize, y: usize, w: usize) {
        let edge = ink(Role::Brand);
        for row in y..y + self.card_h() {
            s.put(x, row, &" ".repeat(w), theme::RESET);
        }
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &edge);
        for row in y + 1..y + self.card_h() - 1 {
            s.put(x, row, "│", &edge);
            s.put(x + w - 1, row, "│", &edge);
        }
        s.put(
            x,
            y + self.card_h() - 1,
            &format!("└{}┘", "─".repeat(w - 2)),
            &edge,
        );
        // The end of the title shows, the way a one-line field scrolls.
        let inner = w - 5;
        let shown: String = {
            let n = title.chars().count();
            title.chars().skip(n.saturating_sub(inner)).collect()
        };
        let cx = s.put(x + 2, y + 1, &shown, &bold(Role::Focused));
        s.put(cx, y + 1, "▌", &ink(Role::Brand));
        // The space it goes to: on its own row in a full card, on the top edge in a quiet one.
        let (sx, sy) = if self.quiet {
            (x + 2, y)
        } else {
            (x + 2, y + 2)
        };
        match self.targets().get(space) {
            Some(target) => {
                let nx = s.put(
                    sx,
                    sy,
                    &fit(&format!(" {} ", target.name), w / 2),
                    &ink(theme::space_in(&self.spaces, target.id)),
                );
                s.put(nx + 1, sy, " tab space ", &ink(Role::FaintText));
            }
            None => {
                s.put(sx, sy, " no space to add to ", &ink(Role::Blocked));
            }
        }
    }

    /// The issue: Description, Checklist and Activity on the left, Details on the right, in
    /// the box at `(x, y)` of `(w, h)`. Both columns are rows wrapped to their width and
    /// scrolled together: nothing in them is cut.
    fn dialog(
        &self,
        s: &mut Screen,
        card: &ItemCardInfo,
        at: usize,
        (x, y): (usize, usize),
        (w, h): (usize, usize),
    ) {
        let edge = ink(Role::Brand);
        for row in y..y + h {
            s.put(x, row, &" ".repeat(w), theme::RESET);
            s.put(x, row, "│", &edge);
            s.put(x + w - 1, row, "│", &edge);
        }
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &edge);
        s.put(x, y + h - 1, &format!("└{}┘", "─".repeat(w - 2)), &edge);
        let item = &card.item;
        let pane = Pane::of((x, y), (w, h));
        let space = space_name(&self.spaces, item.workspace);
        let space_ink = item.workspace.map_or(ink(Role::FaintText), |id| {
            ink(theme::space_in(&self.spaces, id))
        });
        let key = format!(" / {}", item.key);
        // One column: the key alone on the head, the way out stays on the foot.
        if pane.one {
            s.put(
                pane.lx,
                y + 1,
                &fit(&item.key, pane.lw),
                &ink(Role::FaintText),
            );
        } else {
            let space = fit(
                &space,
                (x + w - 16).saturating_sub(pane.lx + width::width(&key)),
            );
            s.seg(
                pane.lx,
                y + 1,
                &[(&space, space_ink), (&key, ink(Role::FaintText))],
            );
            s.seg(
                x + w - 15,
                y + 1,
                &[
                    ("[ ", ink(Role::Brand)),
                    ("esc", bold(Role::Text)),
                    (" close ]", ink(Role::Detail)),
                ],
            );
        }
        let (left, right) = (self.left_rows(card, &pane), self.right_rows(card, &pane));
        let view = pane.view();
        let total = left.len().max(right.len());
        let scroll = self.scroll.min(total.saturating_sub(view));
        for row in pane.top..=pane.last {
            s.put(pane.split, row, "│", &ink(Role::Rule));
        }
        if scroll > 0 {
            s.put(pane.split, pane.top, "▲", &ink(Role::Brand));
        }
        if scroll + view < total {
            s.put(pane.split, pane.last, "▼", &ink(Role::Brand));
        }
        paint(
            s,
            &left,
            (scroll, pane.top, view),
            (pane.lx, pane.lw),
            Some(at),
        );
        // Details stay in view while they fit; a taller column scrolls to its own end.
        let beside = scroll.min(right.len().saturating_sub(view));
        paint(
            s,
            &right,
            (beside, pane.top, view),
            (pane.dx, pane.dw),
            None,
        );
    }

    /// The dialog's box, as [`Page::draw`] places it over the lanes, or over the one lane of a
    /// narrow page at its real size.
    fn dialog_box(size: (usize, usize)) -> ((usize, usize), (usize, usize)) {
        let (w, h) = (size.0.max(FLOOR.0), size.1.max(FLOOR.1));
        if w < NEED.0 || h < NEED.1 {
            return ((1, 2), (w - 2, h - 4));
        }
        ((1, 2), (3 * ((w - 2 - 4) / 3) + 4, h - 4))
    }

    /// The dialog's first column, with the details under it when the pane has one column.
    fn left_rows(&self, card: &ItemCardInfo, pane: &Pane) -> Vec<Row> {
        let mut rows = self.rows(card, pane.lw);
        if pane.one {
            rows.push(Row::new(Block::Plain, None));
            rows.extend(self.details_rows(card, pane.lw));
        }
        rows
    }

    /// The details column, none when they stand under the rest.
    fn right_rows(&self, card: &ItemCardInfo, pane: &Pane) -> Vec<Row> {
        if pane.one {
            return Vec::new();
        }
        self.details_rows(card, pane.dw)
    }

    fn pane(&self) -> Pane {
        let (at, size) = Self::dialog_box(self.size);
        Pane::of(at, size)
    }

    /// The left column: title, description, checklist and activity, wrapped to `lw`.
    fn rows(&self, card: &ItemCardInfo, lw: usize) -> Vec<Row> {
        let item = &card.item;
        let plain =
            |text: &str, role, strong| Row::new(Block::Plain, None).with(0, text, role, strong);
        let blank = || Row::new(Block::Plain, None);
        let mut out = text_rows(&item.title, lw, 0, Role::Focused, true, Block::Title);
        out.push(blank());
        out.push(plain("Description", Role::FaintText, true));
        if card.description.trim().is_empty() {
            out.push(Row::new(Block::Description, None).with(
                0,
                "none yet",
                Role::FaintText,
                false,
            ));
        }
        for para in card.description.lines() {
            out.extend(text_rows(
                para,
                lw,
                0,
                Role::Text,
                false,
                Block::Description,
            ));
        }
        out.push(blank());
        // The plan: where it stands, who approved it and when, its file, its summary.
        if let Some(plan) = &card.plan_file {
            let (word, role) = qoop_tui::asks::plan_word(&item.plan, item.plan_by.as_deref())
                .unwrap_or_else(|| ("plan".to_string(), Role::Text));
            let word = word.trim_start_matches("plan ").to_string();
            let when = format!(" \u{b7} {}", ago(item.plan_at.or(Some(plan.at)), self.now));
            out.push(
                Row::new(Block::Plain, None)
                    .with(0, "Plan", Role::FaintText, true)
                    .with(6, &word, role, false)
                    .with(6 + width::width(&word), &when, Role::FaintText, false),
            );
            let file = std::path::Path::new(&plan.path)
                .file_name()
                .map_or(plan.path.clone(), |f| f.to_string_lossy().into_owned());
            out.extend(text_rows(&file, lw, 0, Role::Detail, false, Block::Plain));
            if !plan.summary.trim().is_empty() {
                out.extend(text_rows(
                    &plan.summary,
                    lw,
                    0,
                    Role::Text,
                    false,
                    Block::Plain,
                ));
            }
            out.push(blank());
        }
        out.push(
            Row::new(Block::Plain, None)
                .with(0, "Checklist", Role::FaintText, true)
                .with(
                    9,
                    &format!("  {}/{}", item.steps_done, item.steps_total),
                    Role::FaintText,
                    false,
                ),
        );
        if card.steps.is_empty() {
            out.push(plain("no steps yet", Role::FaintText, false));
        }
        let going = card.steps.iter().position(|st| st.done_at.is_none());
        let phases = card
            .steps
            .iter()
            .filter(|st| qoop_store::item::is_phase(&st.text))
            .count();
        // Steps above the first heading are the first phase's, as `phase_of` counts them: its
        // rule goes over them.
        let mut order: Vec<usize> = (0..card.steps.len()).collect();
        if let Some(h) = card
            .steps
            .iter()
            .position(|st| qoop_store::item::is_phase(&st.text))
        {
            order.remove(h);
            order.insert(0, h);
        }
        let mut phase = 0;
        for n in order {
            let step = &card.steps[n];
            // A phase's heading is a rule over its steps, never a stop the keyboard ticks.
            if let Some(name) = step.text.strip_prefix("## ") {
                phase += 1;
                let head = fit(&format!("Phase {phase} of {phases}: {name}"), lw);
                let rule = "\u{2500}".repeat(lw.saturating_sub(width::width(&head) + 1));
                out.push(
                    Row::new(Block::Step(step.n), None)
                        .with(0, &head, Role::Detail, true)
                        .with(width::width(&head) + 1, &rule, Role::Rule, false),
                );
                continue;
            }
            let done = step.done_at.is_some();
            let room = lw.saturating_sub(4 + if going == Some(n) { 13 } else { 0 });
            let mut lines = wrap_cells(&step.text, room, None);
            lines.resize(lines.len().max(1), String::new());
            for (i, line) in lines.iter().enumerate() {
                let mut row = Row::new(Block::Step(step.n), Some(n));
                if i == 0 {
                    let mark = if done { "[x] " } else { "[ ] " };
                    row = row.with(0, mark, Role::Detail, false);
                }
                let role = if done { Role::FaintText } else { Role::Text };
                row = row.with(4, line, role, false);
                if i == 0 && going == Some(n) {
                    row = row.with(
                        4 + width::width(line) + 2,
                        "in progress",
                        Role::WorkingText,
                        false,
                    );
                }
                out.push(row);
            }
        }
        out.push(blank());
        out.push(plain("Activity", Role::FaintText, true));
        let keys = if self.flipped.is_empty() {
            Vec::new()
        } else {
            entry_keys(card)
        };
        let tw = lw.saturating_sub(12);
        for (i, e) in card.activity.iter().enumerate() {
            let whole = self.all != keys.get(i).is_some_and(|k| self.flipped.contains(k));
            let what = activity_words(&e.kind, &e.body);
            // One row past the collapsed height is enough to know the entry was cut.
            let mut lines = wrap_cells(&what, tw, (!whole).then_some(ENTRY_ROWS + 1));
            let cut = lines.len() > ENTRY_ROWS && !whole;
            if cut {
                lines.truncate(ENTRY_ROWS);
            }
            lines.resize(lines.len().max(1), String::new());
            let last = lines.len() - 1;
            for (n, line) in lines.iter().enumerate() {
                let mut row = Row::new(Block::Entry(i), Some(card.steps.len() + i));
                if n == 0 {
                    row = row.with(
                        0,
                        &fit(&ago(Some(e.at), self.now), 11),
                        Role::FaintText,
                        false,
                    );
                }
                if cut && n == last {
                    let line = clip(line, tw.saturating_sub(MORE.chars().count()));
                    let end = 12 + width::width(&line);
                    row =
                        row.with(12, &line, Role::Text, false)
                            .with(end, MORE, Role::Detail, false);
                } else {
                    row = row.with(12, line, Role::Text, false);
                }
                out.push(row);
            }
        }
        if self.older || card.activity.len() >= ACTIVITY_SENT {
            out.push(blank());
            let note = format!(
                "older activity is not shown here: qoop task show {}",
                item.key
            );
            out.extend(text_rows(
                &note,
                lw,
                0,
                Role::FaintText,
                false,
                Block::Plain,
            ));
        }
        out
    }

    /// The right column: the facts of the issue, values wrapped to `dw`.
    fn details_rows(&self, card: &ItemCardInfo, dw: usize) -> Vec<Row> {
        let item = &card.item;
        let vw = dw.saturating_sub(11);
        let mut out = vec![
            Row::new(Block::Plain, None).with(0, "Details", Role::FaintText, true),
            Row::new(Block::Plain, None),
        ];
        let kv = |out: &mut Vec<Row>, label: &str, value: &str, role: Role| {
            let mut lines = wrap_cells(value, vw, None);
            lines.resize(lines.len().max(1), String::new());
            for (i, line) in lines.iter().enumerate() {
                let mut row = Row::new(Block::Plain, None);
                if i == 0 && !label.is_empty() {
                    row = row.with(0, label, Role::FaintText, false);
                }
                out.push(row.with(11, line, role, false));
            }
        };
        let state_role = match item.state.as_str() {
            "doing" => Role::WorkingText,
            "done" => Role::Done,
            _ => Role::Text,
        };
        let state = match &item.outcome {
            Some(o) if item.state == "done" => format!("DONE, {o}"),
            _ => item.state.to_uppercase(),
        };
        kv(&mut out, "Status", &state, state_role);
        out.push(Row::new(Block::Plain, None));
        if item.agents.is_empty() {
            kv(&mut out, "Assignees", "none yet", Role::FaintText);
        }
        for (n, a) in item.agents.iter().enumerate() {
            kv(
                &mut out,
                if n == 0 { "Assignees" } else { "" },
                &a.name,
                Role::Text,
            );
            let (word, role, _) = agent_word(a, self.now);
            let place = match a.terminal {
                Some(t) => format!("{}  T{t}", a.machine),
                None => a.machine.clone(),
            };
            if width::width(&place) + 2 + width::width(&word) <= vw {
                let end = 11 + width::width(&place) + 2;
                out.push(
                    Row::new(Block::Plain, None)
                        .with(11, &place, Role::FaintText, false)
                        .with(end, &word, role, false),
                );
            } else {
                kv(&mut out, "", &place, Role::FaintText);
                kv(&mut out, "", &word, role);
            }
        }
        out.push(Row::new(Block::Plain, None));
        let space = space_name(&self.spaces, item.workspace);
        let space_role = item
            .workspace
            .map_or(Role::FaintText, |id| theme::space_in(&self.spaces, id));
        kv(&mut out, "Space", &space, space_role);
        kv(&mut out, "Key", &item.key, Role::Text);
        kv(
            &mut out,
            "Updated",
            &ago(Some(item.updated_at), self.now),
            Role::Text,
        );
        let run_by = match self.qass.0 {
            Some(t) => format!("qass  T{t}"),
            None => "qass".to_string(),
        };
        kv(&mut out, "Run by", &run_by, Role::Text);
        out.push(Row::new(Block::Plain, None));
        let legend = if dw >= 21 {
            "change status ]"
        } else {
            "move ]"
        };
        out.push(
            Row::new(Block::Plain, None)
                .with(0, "[ ", Role::Brand, false)
                .with(2, "H L", Role::Text, true)
                .with(5, &format!(" {legend}"), Role::Detail, false),
        );
        out
    }

    /// The most the body scrolls: its rows less the window.
    fn max_scroll(&self) -> usize {
        let Some(card) = &self.card else {
            return 0;
        };
        let pane = self.pane();
        let rows = self.left_rows(card, &pane).len();
        rows.max(self.right_rows(card, &pane).len())
            .saturating_sub(pane.view())
    }

    /// An activity entry opened if it is shut, shut if it is open.
    fn toggle(&mut self, entry: usize) {
        let Some(key) = self
            .card
            .as_ref()
            .and_then(|c| entry_keys(c).get(entry).cloned())
        else {
            return;
        };
        if !self.flipped.remove(&key) {
            self.flipped.insert(key);
        }
    }

    /// The issue opened: at its top, every entry as it comes.
    fn open_card(&mut self, card: ItemCardInfo, older: bool) {
        // On its first step, past a phase's heading.
        let first = card
            .steps
            .iter()
            .position(|s| !qoop_store::item::is_phase(&s.text))
            .unwrap_or(card.steps.len());
        self.mode = Mode::Card(first);
        self.card = Some(card);
        self.older = older;
        self.scroll = 0;
        self.all = false;
        self.flipped.clear();
    }

    /// The open issue read again: the stop, the opened entries and the place in the body
    /// stay where they were, whatever arrived above them.
    fn keep_card(&mut self, card: ItemCardInfo, older: bool) {
        let anchor = self.anchor();
        let was = match (&self.mode, &self.card) {
            (Mode::Card(at), Some(old)) => at
                .checked_sub(old.steps.len())
                .and_then(|i| entry_keys(old).get(i).cloned()),
            _ => None,
        };
        let keys = entry_keys(&card);
        if let Mode::Card(at) = self.mode {
            let found = was.and_then(|k| keys.iter().position(|n| *n == k));
            let mut at = found.map_or(at, |i| card.steps.len() + i);
            // Steps rewritten with a heading where it stood: on to the next step, as
            // `open_card` does.
            while card
                .steps
                .get(at)
                .is_some_and(|s| qoop_store::item::is_phase(&s.text))
            {
                at += 1;
            }
            let stops = card.steps.len() + card.activity.len();
            self.mode = Mode::Card(at.min(stops.saturating_sub(1)));
        }
        self.flipped.retain(|k| keys.contains(k));
        self.card = Some(card);
        self.older = older;
        if let Some(a) = anchor {
            let block = match &a.entry {
                Some(k) => keys.iter().position(|n| n == k).map(Block::Entry),
                None => Some(a.block),
            };
            let pane = self.pane();
            let first = self
                .card
                .as_ref()
                .zip(block)
                .and_then(|(c, b)| self.rows(c, pane.lw).iter().position(|r| r.block == b));
            self.scroll = first.map_or(usize::MAX, |f| f + a.offset);
        }
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// The block of the first row in view and how far into it the view starts.
    fn anchor(&self) -> Option<Anchor> {
        let card = self.card.as_ref()?;
        let rows = self.left_rows(card, &self.pane());
        let row = rows.get(self.scroll)?;
        if row.block == Block::Plain {
            return None;
        }
        let first = rows.iter().position(|r| r.block == row.block)?;
        let entry = match row.block {
            Block::Entry(i) => entry_keys(card).get(i).cloned(),
            _ => None,
        };
        Some(Anchor {
            block: row.block,
            entry,
            offset: self.scroll - first,
        })
    }

    /// `j` or `k` in the dialog: to the next stop, or, in one taller than the window, down
    /// or up through it first. At either end of the body it scrolls a row, so the title and
    /// the last line can be reached.
    fn go(&mut self, at: usize, down: bool) {
        let Some(card) = &self.card else {
            return;
        };
        let stops = card.steps.len() + card.activity.len();
        let pane = self.pane();
        let view = pane.view();
        let rows = self.left_rows(card, &pane);
        let max = rows
            .len()
            .max(self.right_rows(card, &pane).len())
            .saturating_sub(view);
        let scroll = self.scroll.min(max);
        let range = |stop: usize| {
            let first = rows.iter().position(|r| r.stop == Some(stop))?;
            let last = rows.iter().rposition(|r| r.stop == Some(stop))?;
            Some((first, last))
        };
        // Where the view goes to show a stop: its top, or just enough to show its bottom.
        let show = |(first, last): (usize, usize)| {
            if first < scroll || last + 1 - first > view {
                first
            } else if last >= scroll + view {
                last + 1 - view
            } else {
                scroll
            }
        };
        // A phase's heading is no stop: the keyboard steps over it.
        let next = (at + 1..stops).find(|s| range(*s).is_some());
        let prev = (0..at).rev().find(|s| range(*s).is_some());
        let edge = if down { next.is_none() } else { prev.is_none() };
        let here = range(at);
        let (to, shown) = match here {
            Some((first, last)) if last >= scroll && first < scroll + view => {
                let tall = last + 1 - first > view;
                if tall && down && last >= scroll + view {
                    (at, scroll + 1)
                } else if tall && !down && first < scroll {
                    (at, scroll - 1)
                } else if let (true, Some(next)) = (down, next) {
                    (next, range(next).map_or(scroll, show))
                } else if let (false, Some(prev)) = (down, prev) {
                    (prev, range(prev).map_or(scroll, show))
                } else if down {
                    (at, scroll + 1)
                } else {
                    (at, scroll.saturating_sub(1))
                }
            }
            Some(r) if !edge => (at, show(r)),
            _ if down => (at, scroll + 1),
            _ => (at, scroll.saturating_sub(1)),
        };
        self.mode = Mode::Card(to);
        self.scroll = shown.min(max);
    }

    /// A press in the dialog: on a step or an entry the keyboard goes there, and an entry
    /// opens or shuts.
    fn click(&mut self, col: usize, row: usize) -> Step {
        let pane = self.pane();
        let Some(card) = &self.card else {
            return Step::Stay;
        };
        if row < pane.top || row > pane.last || col + 2 < pane.lx || col >= pane.split {
            return Step::Stay;
        }
        let steps = card.steps.len();
        let scroll = self.scroll.min(self.max_scroll());
        let stop = self
            .left_rows(card, &pane)
            .get(scroll + row - pane.top)
            .and_then(|r| r.stop);
        if let Some(stop) = stop {
            self.mode = Mode::Card(stop);
            if let Some(entry) = stop.checked_sub(steps) {
                self.toggle(entry);
            }
        }
        Step::Stay
    }

    /// The two rows under the lanes: qass and what the page last said, then the keys.
    fn foot(&self, s: &mut Screen) {
        let y = s.h - 2;
        let mut x = s.put(1, y, "qass", &bold(Role::Text));
        if let Some(t) = self.qass.0 {
            x = s.put(x + 1, y, &format!("T{t}"), &ink(Role::FaintText));
        }
        x = s.put(x + 1, y, &self.qass.1, &ink(Role::Detail)) + 3;
        let room = s.w.saturating_sub(x + 1);
        match &self.mode {
            Mode::Tell(text) => {
                let shown: String = {
                    let n = text.chars().count();
                    text.chars()
                        .skip(n.saturating_sub(room.saturating_sub(12)))
                        .collect()
                };
                let tx = s.put(x, y, "tell qass: ", &ink(Role::BrandText));
                let cx = s.put(tx, y, &shown, &bold(Role::Focused));
                s.put(cx, y, "▌", &ink(Role::Brand));
            }
            _ => {
                if let Some(said) = &self.said {
                    s.put(x, y, &fit(said, room), &ink(Role::Text));
                }
            }
        }
        // The other view's key early: the room a narrow page has keeps it before the rest.
        let list = self.view == View::List;
        let other = if list { ("b", "board") } else { ("b", "list") };
        let mut board = vec![
            (if list { "j k" } else { "h j k l" }, "select"),
            other,
            ("enter", "open"),
            ("c", "create"),
            ("/", "filter"),
            ("H L", "move"),
            ("x", "dismiss done"),
            ("v", "cards"),
            ("t", "tell qass"),
            ("q", "close"),
        ];
        if self.loose_shown() {
            board.insert(7, ("N", "not on the board"));
        }
        if list {
            board.retain(|(key, _)| *key != "v");
        }
        // One lane at a time: the keys that move in it first, in the room there is.
        if s.w < NEED.0 || s.h < NEED.1 {
            board = vec![("j k", "move"), ("enter", "open")];
            if !list {
                board.push(("h l", "lane"));
            }
            board.extend([other, ("c", "create"), ("q", "close")]);
        }
        let keys: &[(&str, &str)] = match &self.mode {
            Mode::Board => &board,
            Mode::Card(_) => &[
                ("j k", "step"),
                ("enter", "more"),
                ("E", "all"),
                ("x", "tick"),
                ("H L", "move"),
                ("t", "tell qass"),
                ("esc", "close"),
            ],
            Mode::Filter => &[("enter", "keep"), ("esc", "clear")],
            Mode::Create { .. } => &[
                ("enter", "add to todo"),
                ("tab", "space"),
                ("esc", "cancel"),
            ],
            Mode::Tell(_) => &[("enter", "send to qass"), ("esc", "cancel")],
        };
        chips(s, keys);
    }
}

/// A line of an item's activity in words: what happened, then what it was about.
pub(crate) fn activity_words(kind: &str, body: &str) -> String {
    let body = qoop_store::task::one_line(body);
    let what = match kind {
        "opened" => "added",
        "goal" => "title",
        "description" if body.is_empty() => return "description cleared".to_string(),
        "steps" => return format!("steps set: {}", body.trim_end_matches(" steps")),
        // A start nobody took up says so in its body.
        "start" if body.is_empty() => return "started by the owner".to_string(),
        "evidence" => "check",
        "done" => "claims done",
        "hidden" => return "hidden from the board".to_string(),
        "unhidden" => return "back on the board".to_string(),
        // A plan's events come worded by the store ("plan approved by qass"): said as they are.
        "planfile" | "approved" | "show-me" | "show-me-off" | "stale-card" => return body,
        other => other,
    };
    if body.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {body}")
    }
}

/// The bottom row: the page's name filled in the brand ink, then each key and what it does
/// between ember brackets. Only whole chips, and the last one (the way out) always: the
/// others in order while they fit before it.
fn chips(s: &mut Screen, keys: &[(&str, &str)]) {
    let y = s.h - 1;
    let chip_w =
        |(key, what): &(&str, &str)| 2 + key.chars().count() + what.chars().count() + 3 + 2;
    let Some((last, rest)) = keys.split_last() else {
        return;
    };
    // A page narrower than its lanes names its keys plainly, as the questions beside it do:
    // brackets take the room a key needs. The way out always, the others while they fit.
    if s.w < NEED.0 {
        let word_w = |(key, what): &(&str, &str)| key.chars().count() + what.chars().count() + 3;
        let mut room = s.w.saturating_sub(1 + word_w(last) - 2);
        let mut x = 1;
        let shown = rest.iter().take_while(|chip| {
            let fits = word_w(chip) <= room;
            room = room.saturating_sub(word_w(chip));
            fits
        });
        for (key, what) in shown.chain([last]) {
            let what = format!(" {what}");
            x = s.seg(
                x,
                y,
                &[(key, bold(Role::Text)), (&what, ink(Role::FaintText))],
            ) + 2;
        }
        return;
    }
    // The way out is drawn whole: on a narrow page the page's name gives way to it.
    const NAME: &str = " board ";
    let mut x = if s.w >= NEED.0 && 1 + NAME.len() + 2 + chip_w(last) - 2 <= s.w {
        s.put(1, y, NAME, &theme::filled(Role::Brand)) + 2
    } else {
        1
    };
    let mut room = s.w.saturating_sub(x + chip_w(last));
    let mut shown: Vec<&(&str, &str)> = rest
        .iter()
        .take_while(|chip| {
            let fits = chip_w(chip) <= room;
            room = room.saturating_sub(chip_w(chip));
            fits
        })
        .collect();
    shown.push(last);
    for (key, what) in shown {
        x = s.seg(
            x,
            y,
            &[
                ("[ ", ink(Role::Brand)),
                (key, bold(Role::Text)),
                (&format!(" {what} "), ink(Role::Detail)),
                ("]", ink(Role::Brand)),
            ],
        ) + 2;
    }
}

/// A one-line field takes a key: a character goes on its end, backspace takes one off.
fn edit(text: &mut String, key: Key) {
    match key {
        Key::Char(c) => text.push(c),
        Key::Backspace => {
            text.pop();
        }
        _ => {}
    }
}

/// What a board read brings: the items (`None` from a daemon older than the board), and the
/// open work no item holds, with how many there are in all (`None` from a daemon that does not
/// say, whose `tasks` is every open task and is left unread).
struct Read {
    items: Option<Vec<ItemInfo>>,
    tasks: Vec<TaskInfo>,
    loose: Option<usize>,
}

fn read_board() -> Result<Read, String> {
    let request = ControlRequest::TaskRead {
        id: crate::request_id("board-page"),
        board: true,
        task: None,
        terminal: None,
        token: None,
        before: None,
    };
    match crate::task::ask(request, Duration::from_secs(10)) {
        Ok(ControlResponse::Tasks {
            items,
            tasks,
            loose,
            ..
        }) => Ok(Read {
            items,
            tasks: if loose.is_some() { tasks } else { Vec::new() },
            loose,
        }),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// One item's card.
/// The issue, and whether the daemon had more activity than it sent.
fn read_card(item: i64) -> Result<(ItemCardInfo, bool), String> {
    let request = ControlRequest::TaskRead {
        id: crate::request_id("board-page"),
        board: false,
        task: Some(item),
        terminal: None,
        token: None,
        before: None,
    };
    match crate::task::ask(request, Duration::from_secs(10)) {
        Ok(ControlResponse::Tasks {
            item: Some(card),
            more,
            ..
        }) => Ok((*card, more)),
        Ok(ControlResponse::Tasks { .. }) => Err(format!("#{item} is not on the board")),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

fn read_spaces() -> Vec<WorkspaceRow> {
    let request = ControlRequest::ListWorkspaces {
        id: crate::request_id("board-page"),
    };
    match crate::task::ask(request, Duration::from_secs(10)) {
        Ok(ControlResponse::Workspaces { workspaces, .. }) => workspaces,
        _ => Vec::new(),
    }
}

/// qass's terminal, how it stands in words, and whether it runs (only then is it told).
fn read_qass() -> (Option<u64>, String, bool) {
    let facts = qass::look();
    let word = match facts.life {
        Life::Running => facts.state.unwrap_or_else(|| "running".into()),
        Life::Asleep => "asleep".into(),
        Life::NotStarted => "not started".into(),
    };
    (facts.terminal, word, facts.life == Life::Running)
}

/// qass's terminal, when it can be told a line now: running, and not waiting on the owner.
fn ear(qass: &(Option<u64>, String, bool)) -> Result<u64, String> {
    let (Some(terminal), word, true) = qass else {
        return Err(format!("qass is {}: `qoop qass` brings it", qass.1));
    };
    qass::hearing(Some(word))?;
    Ok(*terminal)
}

/// Type `text` into qass's terminal and press enter.
fn tell_qass(qass: &(Option<u64>, String, bool), text: &str) -> Result<(), String> {
    qass::send(ear(qass)?, text).map_err(|e| e.to_string())
}

/// A `TaskWrite` on an item, the reason of a refusal as the page says it.
fn write(item: Option<i64>, op: TaskOp) -> Result<(i64, Option<String>), String> {
    crate::task::written(item, op).map_err(|e| e.to_string())
}

/// Wake the page each time the daemon says a task, an item or a space goal changed, and
/// once each time it is reached again. On a connection that only watches, unsubscribed
/// from every screen: the page draws none.
fn watch(news: mpsc::Sender<()>) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        loop {
            if let Err(Gone) = rt.block_on(follow(&news)) {
                return;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// The page has gone: nothing reads the news any more.
struct Gone;

async fn follow(news: &mpsc::Sender<()>) -> Result<(), Gone> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Ok(dir) = paths::runtime_dir() else {
        return Ok(());
    };
    let Ok((stream, mut inbuf)) =
        client::connect_observer(&paths::socket_path(&dir), "qoop-board-page").await
    else {
        return Ok(());
    };
    let (mut rd, mut wr) = stream.into_split();
    let mut connection = client::Connection::new();
    let mut seq = 0u32;
    let mut ask = async |request: ControlRequest| {
        crate::control_roundtrip(
            &mut rd,
            &mut wr,
            &mut inbuf,
            &mut connection,
            &mut seq,
            &request,
            Duration::from_secs(10),
        )
        .await
        .map(|(answer, _)| answer)
    };
    let Ok(ControlResponse::Terminals { terminals, .. }) = ask(ControlRequest::ListTerminals {
        id: crate::request_id("board-page"),
    })
    .await
    else {
        return Ok(());
    };
    if ask(ControlRequest::Unsubscribe {
        id: crate::request_id("board-page"),
        terminals,
    })
    .await
    .is_err()
    {
        return Ok(());
    }
    news.send(()).map_err(|_| Gone)?;
    let mut read = [0u8; 64 * 1024];
    loop {
        // A screen already on its way is still acknowledged, or the daemon drops a reader
        // that holds one too long.
        if let Some(ack) = connection.renders().due_ack(Instant::now())
            && wr.write_all(&ack).await.is_err()
        {
            return Ok(());
        }
        let n = match tokio::time::timeout(Duration::from_secs(1), rd.read(&mut read)).await {
            // Quiet: round again, for an acknowledgement come due. The thread ends with the
            // page's process.
            Err(_) => continue,
            Ok(Ok(0) | Err(_)) => return Ok(()),
            Ok(Ok(n)) => n,
        };
        inbuf.extend_from_slice(&read[..n]);
        let Ok(incoming) = connection.drain(&mut inbuf) else {
            return Ok(());
        };
        if incoming.iter().any(|i| {
            matches!(
                i,
                client::Incoming::Event(EventMessage::TasksChanged | EventMessage::Resync)
            )
        }) {
            news.send(()).map_err(|_| Gone)?;
        }
    }
}

/// `qoop task board --page`
pub(crate) fn run() -> std::io::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err(std::io::Error::other("the board page needs a terminal"));
    }
    let config_stamp = || {
        std::fs::metadata(qoop_tui::config::path())
            .and_then(|m| m.modified())
            .ok()
    };
    let mut stamp = config_stamp();
    crate::settings::apply_theme();
    let mut page = Page::new();
    let config = qoop_tui::config::read().config;
    page.quiet = config.quiet_cards;
    page.view = if config.board_list {
        View::List
    } else {
        View::Board
    };
    // qass's tab opens the board in qass's folder (`qass::show_board`), and a wake brings it
    // back there.
    let canonical = |p: std::path::PathBuf| p.canonicalize().ok();
    let here = std::env::current_dir().ok().and_then(canonical);
    page.in_qass_tab =
        here.is_some() && here == qoop_runtime::paths::qass_data_dir().and_then(canonical);
    let (tx, news) = mpsc::channel();
    watch(tx);
    let session = Session::start()?;
    let _mouse = MouseOn::start()?;
    let mut polled: Option<Instant> = None;
    let mut last = String::new();
    'run: loop {
        // A theme changed while the page is up (`qoop config set theme`, settings in another
        // tab) is drawn at once, as the interface around it draws it.
        if config_stamp() != stamp {
            stamp = config_stamp();
            crate::settings::apply_theme();
        }
        let mut fresh = false;
        while news.try_recv().is_ok() {
            fresh = true;
        }
        // A full look (qass's state too) only on the slow round: it asks the daemon for
        // every agent.
        if polled.is_none_or(|at| at.elapsed() >= POLL) {
            page.qass = read_qass();
            polled = Some(Instant::now());
            fresh = true;
        }
        if fresh {
            reload(&mut page);
        }
        let (w, h) = page::size();
        page.size = (w, h);
        if w < NEED.0 || h < NEED.1 {
            page.held = None;
        }
        let screen = page.draw(w.max(FLOOR.0), h.max(FLOOR.1));
        // Written only when it changed: a page that repaints every tick is never idle to the
        // daemon, and its tab would never sleep. Ages are compared to the hour only: "3 min"
        // turning "4 min" on a card is no news, and repainting for it alone would keep the tab
        // awake; a card's "2 h" still turns "3 h" when the hour does.
        let now = page.now;
        page.now -= page.now.rem_euclid(3600);
        let ageless = page.draw(w.max(FLOOR.0), h.max(FLOOR.1)).frame();
        page.now = now;
        let frame = format!("{w}x{h}{ageless}");
        if frame != last {
            // Too narrow in a qoop pane: the key that folds the column, which may give it room.
            // Too short, or outside qoop: how big it must be.
            if w < FLOOR.0 && h >= FLOOR.1 && qoop_runtime::legacy::var("QOOP_TERMINAL").is_ok() {
                let key = qoop_tui::keys::name_of(qoop_tui::config::read().config.prefix);
                let say =
                    format!("too narrow for the board: if the column is open, {key} b folds it");
                page::show_said(&screen, FLOOR, &say)?;
            } else {
                page::show(&screen, FLOOR, "q")?;
            }
            last = frame;
        }
        for key in page::next_keys(&session.typed) {
            match page.key(key) {
                Step::Stay => {}
                Step::Leave => break 'run,
                Step::Goto(terminal) => {
                    page.said = Some(match crate::supervise::run_focus(terminal) {
                        Ok(_) => format!("T{terminal} shown"),
                        Err(why) => why.to_string(),
                    });
                }
                Step::Cards(quiet) => {
                    let view = if quiet { "quiet" } else { "full" };
                    let path = qoop_tui::config::path();
                    page.said = Some(match qoop_tui::config::set(&path, "board.cards", view) {
                        Ok(()) => format!("cards: {view}"),
                        Err(why) => why,
                    });
                }
                Step::View(view) => {
                    let view = match view {
                        View::List => "list",
                        View::Board => "board",
                    };
                    let path = qoop_tui::config::path();
                    page.said = Some(match qoop_tui::config::set(&path, "board.view", view) {
                        Ok(()) => format!("view: {view}"),
                        Err(why) => why,
                    });
                }
                Step::Open(item) => match read_card(item) {
                    Ok((card, older)) => page.open_card(card, older),
                    Err(why) => page.said = Some(why),
                },
                Step::Write(item, op, said) => {
                    page.said = Some(match write(Some(item), op) {
                        Ok(_) => said,
                        Err(why) => why,
                    });
                    reload(&mut page);
                }
                Step::Start(item, key) => {
                    page.qass = read_qass();
                    // Waiting on the owner, qass is not asked: the enter would answer its prompt.
                    let deaf = qass::hearing(Some(&page.qass.1)).err();
                    page.said = Some(
                        match deaf.map_or_else(|| write(Some(item), TaskOp::Start), Err) {
                            Ok(_) => match tell_qass(&page.qass, &format!("start {key}")) {
                                Ok(()) => format!("{key} to doing: qass will start it"),
                                Err(why) => format!("{key} to doing, but {why}"),
                            },
                            Err(why) => why,
                        },
                    );
                    reload(&mut page);
                }
                Step::Add { workspace, goal } => {
                    page.said = Some(
                        match write(
                            None,
                            TaskOp::Add {
                                workspace,
                                goal,
                                description: None,
                            },
                        ) {
                            Ok((id, key)) => {
                                page.sel = Some(id);
                                format!("{} added to todo", key.unwrap_or_default())
                            }
                            Err(why) => why,
                        },
                    );
                    reload(&mut page);
                }
                Step::Tell(text) => {
                    page.qass = read_qass();
                    page.said = Some(match tell_qass(&page.qass, &text) {
                        Ok(()) => "told qass".to_string(),
                        Err(why) => why,
                    });
                }
            }
        }
    }
    drop(session);
    Ok(())
}

/// The page asks for the left button while it runs (motion while held, SGR reports), so a
/// card can be carried onto qass; dropped, the terminal stops sending them.
struct MouseOn;

impl MouseOn {
    fn start() -> std::io::Result<MouseOn> {
        print!("\x1b[?1002h\x1b[?1006h");
        std::io::stdout().flush()?;
        Ok(MouseOn)
    }
}

impl Drop for MouseOn {
    fn drop(&mut self) {
        print!("\x1b[?1006l\x1b[?1002l");
        let _ = std::io::stdout().flush();
    }
}

/// Read the board again, and the open card with it.
fn reload(page: &mut Page) {
    page.now = now_unix();
    page.spaces = read_spaces();
    match read_board() {
        Ok(read) => {
            page.items = read.items;
            page.tasks = read.tasks;
            page.loose = read.loose;
        }
        Err(why) => page.said = Some(why),
    }
    // The open card, while its dialog is up.
    if let (Mode::Card(_), Some(id)) = (&page.mode, page.card.as_ref().map(|c| c.item.id)) {
        match read_card(id) {
            Ok((card, older)) => page.keep_card(card, older),
            Err(why) => page.said = Some(why),
        }
    }
    page.settle();
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::control::{ItemAgentInfo, StepInfo, TaskInfo};

    fn item(id: i64, state: &str, title: &str, workspace: i64) -> ItemInfo {
        ItemInfo {
            plan: String::new(),
            plan_by: None,
            plan_at: None,
            show_me: false,
            open_asks: 0,
            phase: None,
            id,
            key: format!("QOOP-{id}"),
            workspace: Some(workspace),
            title: title.into(),
            state: state.into(),
            outcome: (state == "done").then(|| "verified".into()),
            steps_done: 0,
            steps_total: 0,
            agents: Vec::new(),
            // Earlier ids are newer, so a lane reads in id order unless a test says otherwise.
            updated_at: 1_000 - id,
            since: 0,
            closed_at: None,
            hidden: false,
        }
    }

    fn space(id: i64, name: &str) -> WorkspaceRow {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "terminals": 0
        }))
        .unwrap()
    }

    /// The lanes, which most tests here read; the list's tests turn to it.
    fn page() -> Page {
        let mut p = Page::new();
        p.view = View::Board;
        p.spaces = vec![space(1, "qoop"), space(2, "app"), space(3, qass::SPACE)];
        p.items = Some(vec![
            item(1, "todo", "never names a task after harness text", 1),
            item(2, "todo", "return a task to todo", 1),
            item(3, "doing", "board everywhere", 1),
            item(4, "doing", "ad set", 2),
            item(5, "doing", "contacts", 2),
            item(6, "done", "space goals", 1),
        ]);
        p.settle();
        p
    }

    /// Beside the open column of a narrow window the board draws one lane at a time: at 80
    /// columns (34 for the board) the keyboard's lane in its box with its cards, at 60 (22) a list
    /// of keys and titles; `l` goes to the next lane, and the rule's row names all three.
    #[test]
    fn a_narrow_board_shows_one_lane_at_a_time() {
        let mut p = page();
        for (w, h) in [(34, 14), (22, 14)] {
            p.size = (w, h);
            (p.lane, p.sel) = (0, None);
            p.settle();
            let text = drawn(&p, w, h);
            let named = if w < 30 {
                vec!["todo 2 \u{203a}"]
            } else {
                vec!["todo 2", "doing 3", "done 1"]
            };
            for lane in named {
                assert!(text.contains(lane), "{lane} at {w}: {text}");
            }
            assert!(
                text.contains("QOOP-1"),
                "the keyboard's lane at {w}: {text}"
            );
            assert!(!text.contains("QOOP-3"), "one lane at {w}: {text}");
            p.key(Key::Char('l'));
            let text = drawn(&p, w, h);
            assert!(
                text.contains("QOOP-3") && !text.contains("QOOP-1"),
                "{text}"
            );
        }
        // A list at 22, and at 40 when too short for a card in a box: a row a card, the
        // keyboard's marked, no box.
        for (w, h) in [(22, 14), (40, 9)] {
            p.size = (w, h);
            let text = drawn(&p, w, h);
            assert!(text.contains("\u{25b8} QOOP-3 board"), "{w}x{h}: {text}");
            assert!(!text.contains('\u{250c}'), "{w}x{h}: {text}");
        }
        // In a box the lane is named once, on the rule's row, and the foot names the keys that
        // move in one lane and open a card, plainly (qass's tab at 80 x 24 gives the board 34).
        let text = drawn(&p, 34, 14);
        assert!(
            text.contains("j k move  enter open  q close") && !text.contains("[ q"),
            "{text}"
        );
        assert_eq!(text.matches("doing 3").count(), 1, "{text}");
        // Doing gives rows to the work no item holds: at a height where Todo still holds a card
        // in its box, Doing holds none, so Doing is a list and its picked card is on screen.
        with_loose(&mut p);
        p.size = (40, 11);
        (p.lane, p.sel) = (1, None);
        p.settle();
        let text = drawn(&p, 40, 11);
        assert!(text.contains("\u{25b8} QOOP-3"), "{text}");
        assert!(!text.contains('\u{250c}'), "{text}");
    }

    /// A narrow page draws every mode its keys act in: the card over the one lane at the page's
    /// size, its title and details in one column, and esc closes it; the new card form on Todo,
    /// whatever lane was shown, in the box or as the list's first rows.
    #[test]
    fn a_narrow_board_draws_the_card_and_the_form_its_keys_act_in() {
        for (w, h) in [(34, 17), (22, 17)] {
            let mut p = open_with((w, h), Vec::new(), Vec::new());
            p.card.as_mut().unwrap().item.title = "board everywhere".into();
            let text = drawn(&p, w, h);
            assert!(
                text.contains("everywhere") && text.contains("esc close"),
                "{w}: {text}"
            );
            assert!(
                all_seen(&mut p, &["Checklist".into(), "Details".into()]),
                "the details under it at {w}"
            );
            p.key(Key::Esc);
            assert_eq!(p.mode, Mode::Board, "{w}");
            let mut p = page();
            p.size = (w, h);
            p.key(Key::Char('l'));
            p.key(Key::Char('c'));
            for c in "nightly".chars() {
                p.key(Key::Char(c));
            }
            let text = drawn(&p, w, h);
            assert!(text.contains("nightly\u{258c}"), "the form at {w}: {text}");
            assert!(
                text.contains("esc cancel"),
                "the way out whole at {w}: {text}"
            );
            assert!(text.contains("todo 2"), "on Todo at {w}: {text}");
        }
    }

    /// A card pressed on the board is held; let go over qass (the interface says so) a Todo
    /// card is started as `L` starts it, Doing and Done say why not; let go anywhere else, or
    /// with nothing held, nothing happens.
    #[test]
    fn a_card_carried_onto_qass_is_handed_to_it() {
        let mut p = page();
        p.size = (90, 30);
        let press = |col, row| Key::Mouse {
            down: true,
            held: false,
            col,
            row,
        };
        let release = Key::Mouse {
            down: false,
            held: false,
            col: 0,
            row: 0,
        };
        // Lanes of 28 columns from column 1, the first card on rows 3 to 7.
        assert_eq!(p.card_at(5, 4), Some((0, 1)));
        assert_eq!(p.card_at(35, 4), Some((1, 3)));
        assert_eq!(p.card_at(5, 1), None, "the head");
        assert_eq!(p.key(press(5, 4)), Step::Stay);
        assert_eq!(p.said.as_deref(), Some("drag onto qass to start QOOP-1"));
        assert_eq!(p.key(Key::Dropped(true)), Step::Start(1, "QOOP-1".into()));
        assert_eq!(
            p.key(Key::Dropped(true)),
            Step::Stay,
            "nothing held any more"
        );
        p.key(press(35, 4));
        assert_eq!(p.sel, Some(3));
        assert_eq!(p.key(Key::Dropped(true)), Step::Stay);
        assert_eq!(p.said.as_deref(), Some("QOOP-3 is already in doing"));
        p.key(press(64, 4));
        p.key(Key::Dropped(true));
        assert_eq!(p.said.as_deref(), Some("reopen QOOP-6 first"));
        p.key(press(5, 4));
        assert_eq!(p.key(Key::Dropped(false)), Step::Stay);
        p.key(press(5, 4));
        assert_eq!(p.key(release), Step::Stay);
        assert_eq!(p.key(Key::Dropped(true)), Step::Stay, "let go in the page"); // A window too small draws no board: nothing is under the pointer.
        p.size = (50, 30);
        assert_eq!(p.card_at(5, 4), None);
        p.size = (90, 10);
        assert_eq!(p.card_at(5, 4), None);
    }

    /// A click on a card opens it as `enter` does: pressed and let go on the same card, never
    /// carried off it; a press let go elsewhere, or carried off and back, only selects.
    #[test]
    fn a_click_on_a_card_opens_it_and_a_drag_does_not() {
        let mut p = page();
        p.size = (90, 30);
        let at = |down, held, col, row| Key::Mouse {
            down,
            held,
            col,
            row,
        };
        assert_eq!(p.key(at(true, false, 5, 4)), Step::Stay);
        assert_eq!(
            p.key(at(false, false, 6, 5)),
            Step::Open(1),
            "the same card"
        );
        p.key(at(true, false, 35, 4));
        assert_eq!(
            p.key(at(false, false, 5, 4)),
            Step::Stay,
            "let go on another card"
        );
        assert_eq!(p.sel, Some(3));
        p.key(at(true, false, 5, 4));
        p.key(at(true, true, 35, 4));
        p.key(at(true, true, 5, 4));
        assert_eq!(
            p.key(at(false, false, 5, 4)),
            Step::Stay,
            "carried off and back"
        );
        assert_eq!(
            p.key(at(false, false, 5, 4)),
            Step::Stay,
            "a release with no press"
        );
        p.key(at(true, false, 5, 1));
        assert_eq!(p.key(at(false, false, 5, 1)), Step::Stay, "the head");
    }

    /// A Done card has a cross at its top right that puts it away (nothing deleted: a write
    /// the daemon keeps), as `x` does; an open card has none. Put away cards leave the lane, its
    /// foot counts them, and a click there (or `X`) shows them, their cross bringing one back.
    #[test]
    fn a_done_card_is_put_away_by_its_cross_and_shown_again() {
        let mut p = page();
        p.size = (90, 30);
        let press = |col, row| Key::Mouse {
            down: true,
            held: false,
            col,
            row,
        };
        // The Done lane starts at column 61, its card at 62, 26 wide: the cross at 86, row 3.
        let cell = |p: &Page, col: usize, row: usize| drawn(p, 90, 30).chars().nth(row * 90 + col);
        let line = |p: &Page, row: usize| -> String {
            drawn(p, 90, 30).chars().skip(row * 90).take(90).collect()
        };
        assert_eq!(cell(&p, 86, 3), Some('x'));
        assert_eq!(cell(&p, 26, 3), Some('─'), "no cross on Todo");
        match p.key(press(86, 3)) {
            Step::Write(6, TaskOp::Dismiss { undo: false }, said) => {
                assert!(said.starts_with("QOOP-6 is off the board"), "{said}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(p.held, None, "the cross holds nothing");
        assert_eq!(p.key(press(5, 3)), Step::Stay);
        assert_eq!(
            p.key(Key::Char('x')),
            Step::Stay,
            "an open card has no cross"
        );
        // The daemon says it is hidden: off the lane, counted at its foot.
        for item in p.items.as_mut().unwrap() {
            item.hidden = item.id == 6;
        }
        assert!(p.lanes()[2].is_empty());
        let foot = line(&p, 26);
        assert!(foot.contains("hidden (1)"), "{foot}");
        assert_eq!(p.key(press(66, 26)), Step::Stay);
        assert!(p.reveal && p.lanes()[2].len() == 1);
        assert!(line(&p, 3).contains("show"));
        assert!(matches!(
            p.key(press(86, 3)),
            Step::Write(6, TaskOp::Dismiss { undo: true }, _)
        ));
        p.key(Key::Char('X'));
        assert!(!p.reveal);
        // Shown, a hidden card selected, then put away by a click on the foot: the keyboard
        // leaves it, so enter, x and H cannot act on a card nobody sees.
        p.key(Key::Char('X'));
        p.key(press(70, 4));
        assert_eq!(p.sel, Some(6));
        p.key(Key::Mouse {
            down: false,
            held: false,
            col: 0,
            row: 0,
        });
        p.key(press(66, 26));
        assert!(!p.reveal);
        assert_ne!(p.sel, Some(6));
    }

    /// A card held while the page leaves the board (a field to type in, the dialog) is let
    /// go: a drop then starts nothing.
    #[test]
    fn a_drop_starts_nothing_once_the_page_left_the_board() {
        let mut p = page();
        p.size = (90, 30);
        let press = Key::Mouse {
            down: true,
            held: false,
            col: 5,
            row: 4,
        };
        p.key(press);
        p.key(Key::Char('/'));
        assert_eq!(p.key(Key::Dropped(true)), Step::Stay);
        p.key(Key::Esc);
        p.key(press);
        assert!(matches!(p.key(Key::Enter), Step::Open(1)));
        p.mode = Mode::Card(0);
        assert_eq!(p.key(Key::Dropped(true)), Step::Stay);
    }

    fn drawn(p: &Page, w: usize, h: usize) -> String {
        let mut text = String::new();
        let mut in_esc = false;
        for ch in p.draw(w, h).frame().chars() {
            match (in_esc, ch) {
                (false, '\x1b') => in_esc = true,
                (false, ch) => text.push(ch),
                (true, 'm' | 'H') => in_esc = false,
                (true, _) => {}
            }
        }
        text
    }

    /// A lane of 13 cards in a window that fits 6: the keyboard on the last one has it drawn
    /// (`j` follows), the wheel moves the lane under the pointer by a card and stops at both
    /// ends, another lane scrolls on its own, and neither a poll nor a key elsewhere undoes it.
    #[test]
    fn a_lane_scrolls_with_the_wheel_and_the_keyboard_follows_its_card() {
        let mut p = page();
        p.items = Some(
            (1..=13)
                .map(|n| item(n, "doing", &format!("card number {n}"), 1))
                .chain((20..=29).map(|n| item(n, "done", &format!("closed {n}"), 1)))
                .collect(),
        );
        p.lane = 1;
        p.settle();
        p.size = (120, 4 + 3 + 6 * FULL_H);
        let (w, h) = p.size;
        let shown = |p: &Page| drawn(p, w, h);
        assert!(shown(&p).contains("QOOP-1 "), "the first card is in view");
        assert!(!shown(&p).contains("QOOP-13"));
        assert!(shown(&p).contains("7 below"));
        for _ in 0..12 {
            p.key(Key::Char('j'));
        }
        assert!(shown(&p).contains("QOOP-13"), "the last card is drawn");
        assert!(shown(&p).contains("7 above"));
        // A wheel over DOING (the middle lane) scrolls it; at the top it stops.
        let (col, row) = (60, 8);
        for _ in 0..20 {
            p.key(Key::Wheel { up: true, col, row });
        }
        assert_eq!(p.top[1], 0);
        assert!(shown(&p).contains("QOOP-1 "));
        p.key(Key::Wheel {
            up: false,
            col,
            row,
        });
        assert_eq!(p.top[1], 1);
        for _ in 0..20 {
            p.key(Key::Wheel {
                up: false,
                col,
                row,
            });
        }
        assert_eq!(p.top[1], 7, "never past the last card");
        // A poll leaves the wheel where it is.
        p.top[1] = 3;
        p.settle();
        assert_eq!(p.top[1], 3);
        // DONE scrolls on its own while the keyboard stays in DOING.
        p.key(Key::Wheel {
            up: false,
            col: 100,
            row,
        });
        assert_eq!((p.top[2], p.top[1]), (1, 3));
        // The keyboard on a card out of view brings it back.
        p.key(Key::Char('k'));
        assert!(
            p.top[1]
                <= p.lanes()[1]
                    .iter()
                    .position(|i| Some(i.id) == p.sel)
                    .unwrap()
        );
        // The wheel over no lane, or while typing, does nothing.
        let before = p.top;
        p.key(Key::Wheel {
            up: false,
            col: 0,
            row,
        });
        p.mode = Mode::Filter;
        p.key(Key::Wheel {
            up: false,
            col,
            row,
        });
        assert_eq!(p.top, before);
        assert!(p.filter.is_empty());
    }

    /// A click on a card still finds it after the lane scrolled.
    #[test]
    fn a_card_is_found_where_it_is_drawn_after_a_scroll() {
        let mut p = page();
        p.items = Some(
            (1..=13)
                .map(|n| item(n, "doing", &format!("card number {n}"), 1))
                .collect(),
        );
        p.size = (120, 4 + 3 + 6 * FULL_H);
        p.key(Key::Wheel {
            up: false,
            col: 60,
            row: 8,
        });
        p.key(Key::Wheel {
            up: false,
            col: 60,
            row: 8,
        });
        assert_eq!(
            p.card_at(60, 4),
            Some((1, 3)),
            "the third card is now first"
        );
    }

    #[test]
    fn items_fall_in_their_lanes_and_the_filter_reads_key_title_and_space() {
        let p = page();
        let ids = |lanes: &[Vec<&ItemInfo>; 3]| -> Vec<Vec<i64>> {
            lanes
                .iter()
                .map(|l| l.iter().map(|i| i.id).collect())
                .collect()
        };
        let items = p.items.clone().unwrap();
        assert_eq!(
            ids(&lanes(&items, &p.spaces, "", false)),
            [vec![1, 2], vec![3, 4, 5], vec![6]]
        );
        assert_eq!(
            ids(&lanes(&items, &p.spaces, "APP", false)),
            [vec![], vec![4, 5], vec![]]
        );
        assert_eq!(
            ids(&lanes(&items, &p.spaces, "qoop-6", false)),
            [vec![], vec![], vec![6]]
        );
        assert_eq!(
            ids(&lanes(&items, &p.spaces, " harness ", false)),
            [vec![1], vec![], vec![]]
        );
    }

    #[test]
    fn the_keyboard_moves_in_and_across_lanes_and_stays_on_the_edges() {
        let mut p = page();
        assert_eq!((p.lane, p.sel), (0, Some(1)));
        p.key(Key::Char('j'));
        assert_eq!(p.sel, Some(2));
        p.key(Key::Down);
        assert_eq!(p.sel, Some(2), "the last card stays");
        // Across, at the same height.
        p.key(Key::Char('l'));
        assert_eq!((p.lane, p.sel), (1, Some(4)));
        p.key(Key::Right);
        assert_eq!(
            (p.lane, p.sel),
            (2, Some(6)),
            "a shorter lane: its last card"
        );
        p.key(Key::Right);
        assert_eq!(p.lane, 2);
        p.key(Key::Char('h'));
        p.key(Key::Up);
        p.key(Key::Char('k'));
        assert_eq!((p.lane, p.sel), (1, Some(3)));
        p.key(Key::Left);
        p.key(Key::Left);
        assert_eq!(p.lane, 0);
    }

    #[test]
    fn the_keyboard_follows_its_item_to_another_lane_and_falls_back_when_it_goes() {
        let mut p = page();
        p.sel = Some(2);
        p.items.as_mut().unwrap()[1].state = "doing".into();
        p.settle();
        assert_eq!((p.lane, p.sel), (1, Some(2)));
        p.items.as_mut().unwrap().retain(|i| i.id != 2);
        p.settle();
        assert_eq!((p.lane, p.sel), (1, Some(3)));
    }

    #[test]
    fn a_move_is_decision_six() {
        assert_eq!(move_of("todo", true), Some(Move::Start));
        assert_eq!(move_of("todo", false), None);
        assert_eq!(move_of("doing", true), Some(Move::Close));
        assert_eq!(move_of("doing", false), Some(Move::Unlink));
        assert_eq!(move_of("done", false), Some(Move::Reopen));
        assert_eq!(move_of("done", true), None);
        let mut p = page();
        assert_eq!(p.key(Key::Char('L')), Step::Start(1, "QOOP-1".into()));
        assert_eq!(p.key(Key::Char('H')), Step::Stay);
        p.key(Key::Char('l'));
        assert!(matches!(
            p.key(Key::Char('L')),
            Step::Write(
                3,
                TaskOp::Close {
                    outcome: TaskOutcome::Verified,
                    ..
                },
                _
            )
        ));
        assert!(matches!(
            p.key(Key::Char('H')),
            Step::Write(3, TaskOp::Unlink, _)
        ));
        p.key(Key::Char('l'));
        assert!(matches!(
            p.key(Key::Char('H')),
            Step::Write(6, TaskOp::Reopen, _)
        ));
        assert_eq!(p.key(Key::Char('L')), Step::Stay);
    }

    #[test]
    fn the_filter_is_typed_live_and_esc_clears_it() {
        let mut p = page();
        p.key(Key::Char('/'));
        for c in "app".chars() {
            p.key(Key::Char(c));
        }
        assert_eq!(p.filter, "app");
        assert_eq!((p.lane, p.sel), (0, None), "todo has nothing of app");
        p.key(Key::Enter);
        assert_eq!(p.mode, Mode::Board);
        p.key(Key::Char('l'));
        assert_eq!(p.sel, Some(4));
        p.key(Key::Esc);
        assert_eq!(p.filter, "");
        assert_eq!(p.sel, Some(4), "clearing keeps the card");
    }

    #[test]
    fn create_adds_to_the_selected_cards_space_and_tab_steps_through_the_others() {
        let mut p = page();
        p.key(Key::Char('l'));
        p.key(Key::Char('j')); // QOOP-4, in app
        p.key(Key::Char('c'));
        for c in "fix it".chars() {
            p.key(Key::Char(c));
        }
        p.key(Key::Backspace);
        p.key(Key::Char('t'));
        assert_eq!(
            p.key(Key::Enter),
            Step::Add {
                workspace: 2,
                goal: "fix it".into()
            }
        );
        // tab goes to the next space, never qass's.
        p.key(Key::Char('c'));
        p.key(Key::Char('x'));
        p.key(Key::Tab);
        p.key(Key::Tab);
        assert_eq!(
            p.key(Key::Enter),
            Step::Add {
                workspace: 2,
                goal: "x".into()
            }
        );
        // An empty title adds nothing.
        p.key(Key::Char('c'));
        assert_eq!(p.key(Key::Enter), Step::Stay);
        p.key(Key::Char('c'));
        p.key(Key::Char('y'));
        assert_eq!(p.key(Key::Esc), Step::Stay);
        assert_eq!(p.mode, Mode::Board);
    }

    #[test]
    fn the_dialog_ticks_the_step_the_keyboard_is_on_and_tell_sends_a_line() {
        let mut p = page();
        assert_eq!(p.key(Key::Enter), Step::Open(1));
        p.card = Some(ItemCardInfo {
            plan_file: None,
            item: item(1, "todo", "t", 1),
            description: "why".into(),
            steps: vec![
                StepInfo {
                    n: 1,
                    text: "a".into(),
                    done_at: Some(5),
                },
                StepInfo {
                    n: 2,
                    text: "b".into(),
                    done_at: None,
                },
            ],
            activity: Vec::new(),
        });
        p.mode = Mode::Card(0);
        assert!(matches!(
            p.key(Key::Char('x')),
            Step::Write(1, TaskOp::Check { n: 1, done: false }, _)
        ));
        p.key(Key::Char('j'));
        p.key(Key::Char('j'));
        assert!(matches!(
            p.key(Key::Char('x')),
            Step::Write(1, TaskOp::Check { n: 2, done: true }, _)
        ));
        assert_eq!(p.key(Key::Char('L')), Step::Start(1, "QOOP-1".into()));
        p.key(Key::Esc);
        assert_eq!(p.mode, Mode::Board);
        p.key(Key::Char('t'));
        for c in "hi".chars() {
            p.key(Key::Char(c));
        }
        assert_eq!(p.key(Key::Enter), Step::Tell("hi".into()));
        assert_eq!(p.key(Key::Char('q')), Step::Leave);
    }

    /// "start KEY" and `t` reach qass only while it runs and does not wait on the owner: an
    /// enter into its prompt would answer it in the owner's place.
    #[test]
    fn qass_is_told_nothing_while_it_waits_on_the_owner() {
        let qass = |word: &str, running: bool| (Some(5), word.to_string(), running);
        assert_eq!(ear(&qass("idle", true)), Ok(5));
        assert_eq!(ear(&qass("working", true)), Ok(5));
        let blocked = ear(&qass("blocked", true)).unwrap_err();
        assert!(blocked.contains("qass waits on you"), "{blocked}");
        assert!(tell_qass(&qass("blocked", true), "start QOOP-1").is_err());
        let asleep = ear(&qass("asleep", false)).unwrap_err();
        assert!(asleep.contains("`qoop qass` brings it"), "{asleep}");
    }

    /// The issue is one box over the lanes, edge on edge with them, at every width: no lane
    /// or card border beside or inside its frame. Its activity says each kind in words.
    #[test]
    fn the_dialog_is_one_clean_box_and_its_activity_reads_in_words() {
        use qoop_runtime::control::ItemEventInfo;
        let mut p = page();
        let event = |kind: &str, body: &str| ItemEventInfo {
            task: 1,
            at: 0,
            kind: kind.into(),
            body: body.into(),
        };
        p.card = Some(ItemCardInfo {
            plan_file: None,
            item: item(1, "doing", "t", 1),
            description: String::new(),
            steps: Vec::new(),
            activity: vec![
                event("start", ""),
                event("steps", "4 steps"),
                event("reopened", ""),
                event("description", ""),
            ],
        });
        p.mode = Mode::Card(0);
        for (w, h) in [NEED, (101, 30), (200, 50)] {
            let text: Vec<char> = drawn(&p, w, h).chars().collect();
            let at = |x: usize, y: usize| text[y * w + x];
            let right = 3 * ((w - 6) / 3) + 4;
            let (top, bottom) = (2, h - 3);
            assert_eq!((at(1, top), at(right, top)), ('┌', '┐'), "{w}x{h}");
            assert_eq!((at(1, bottom), at(right, bottom)), ('└', '┘'), "{w}x{h}");
            for y in top + 1..bottom {
                assert_eq!((at(1, y), at(right, y)), ('│', '│'), "row {y} at {w}x{h}");
                assert_ne!(
                    at(2, y),
                    '│',
                    "a lane or card edge inside, row {y} at {w}x{h}"
                );
            }
            for x in 2..right {
                assert_eq!(at(x, top), '─', "col {x} of the top at {w}x{h}");
            }
        }
        let text = drawn(&p, 200, 50);
        for want in [
            "started by the owner",
            "steps set: 4",
            "reopened",
            "description cleared",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(!text.contains("start:"), "{text}");
    }

    #[test]
    fn a_card_says_who_works_on_it_and_what_its_lane_means_without_one() {
        let mut i = item(1, "doing", "t", 1);
        assert_eq!(assignee(&i, 0).1, "qass will start it");
        i.agents.push(ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: "worker".into(),
            machine: "this Mac".into(),
            stage: "waiting-human".into(),
            claimed: false,
            closed: false,
            since: 0,
            ..Default::default()
        });
        assert_eq!(
            assignee(&i, 0),
            ("worker".into(), "waits on you".into(), Role::Blocked, true)
        );
        assert_eq!(assignee(&item(2, "todo", "t", 1), 0).1, "no agent yet");
        // Its agent ended: in Doing it says finished; reopened back in Todo, nobody yet.
        i.agents[0].closed = true;
        assert_eq!(assignee(&i, 0).1, agent_word(&i.agents[0], 0).0);
        i.state = "todo".into();
        assert_eq!(assignee(&i, 0).1, "no agent yet");
    }

    /// A closed card never shows an agent's stage, open or ended: only how it ended.
    #[test]
    fn a_closed_card_says_done_whatever_its_agent_last_said() {
        let mut i = item(1, "done", "t", 1);
        assert_eq!(assignee(&i, 0).1, "done");
        i.agents.push(ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: "worker".into(),
            machine: "this Mac".into(),
            stage: "waiting-human".into(),
            claimed: true,
            closed: false,
            since: 0,
            ..Default::default()
        });
        assert_eq!(
            assignee(&i, 0),
            ("worker".into(), "done".into(), Role::Done, false)
        );
        i.agents[0].closed = true;
        assert_eq!(assignee(&i, 0).1, "done");
        // Failed says when, from the moment the card closed when its agent has no time of its own.
        i.outcome = Some("failed".into());
        assert_eq!(
            assignee(&i, 999 + 2 * DAY),
            (
                "worker".into(),
                "failed · 2 d".into(),
                Role::FailedText,
                false
            )
        );
    }

    const DAY: i64 = 86_400;

    fn agent(status: &str, at: i64) -> ItemAgentInfo {
        ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: "worker".into(),
            machine: "this Mac".into(),
            stage: "implementing".into(),
            status: status.into(),
            status_at: at,
            ..Default::default()
        }
    }

    /// A card's word is the daemon's status token with its age, `working` under an hour apart.
    #[test]
    fn an_asleep_agents_card_reads_asleep_with_its_age() {
        let now = 100 * DAY;
        let mut p = page();
        p.now = now;
        p.items.as_mut().unwrap()[2]
            .agents
            .push(agent("asleep", now - 13 * DAY));
        let i = &p.items.as_ref().unwrap()[2];
        assert_eq!(
            assignee(i, now),
            (
                "worker".into(),
                "asleep · 13 d".into(),
                Role::FaintText,
                false
            )
        );
        assert!(drawn(&p, 200, 50).contains("asleep · 13 d"));
        for (token, ago, said) in [
            ("checks", 8 * DAY, "qass checks · 8 d"),
            ("waits", 3 * 3600, "waits on you · 3 h"),
            ("idle", 120, "idle · 2 min"),
            ("error", 5 * DAY, "error · 5 d"),
            ("working", 600, "working"),
            ("working", 3 * DAY, "working · 3 d"),
        ] {
            assert_eq!(agent_word(&agent(token, now - ago), now).0, said, "{token}");
        }
    }

    /// An older daemon sends no token, and a token with no word of its own (`unknown`, `remote`)
    /// leaves the stage to speak, by the rule a card always had.
    #[test]
    fn an_empty_token_keeps_the_stage_word() {
        for token in ["", "unknown", "remote"] {
            assert_eq!(
                agent_word(&agent(token, 995), 1_000),
                ("implementing".into(), Role::WorkingText, false),
                "{token:?}"
            );
        }
        let mut a = agent("", 0);
        a.claimed = true;
        assert_eq!(agent_word(&a, 0), (CHECKS.into(), Role::Done, true));
        (a.claimed, a.stage) = (false, "waiting-human".into());
        assert_eq!(agent_word(&a, 0), (WAITS.into(), Role::Blocked, true));
    }

    /// One word and ink for each token, and finished work wears a quieter ink than work qass
    /// still checks.
    #[test]
    fn every_status_has_its_word_and_ink_and_finished_is_quieter_than_checks() {
        for (token, word, role) in [
            ("waits", "waits on you", Role::Blocked),
            ("checks", "qass checks", Role::Done),
            ("finished", "finished", Role::Detail),
            ("replaced", "replaced", Role::FaintText),
            ("closed", "tab closed", Role::FaintText),
            ("asleep", "asleep", Role::FaintText),
            ("working", "working", Role::WorkingText),
            ("error", "error", Role::FailedText),
            ("idle", "idle", Role::FaintText),
        ] {
            assert_eq!(status_word(token), Some((word, role)), "{token}");
        }
        assert!(
            ["", "unknown", "remote"]
                .iter()
                .all(|t| status_word(t).is_none())
        );
        let (finished, checks) = (status_word("finished"), status_word("checks"));
        assert_ne!(ink(finished.unwrap().1), ink(checks.unwrap().1));
    }

    /// A Done card says how its agent ended and when, not a bare done; a live agent on a card
    /// closed from the board says nothing of itself.
    #[test]
    fn a_done_card_says_how_its_agent_ended_with_when() {
        let now = 50 * DAY;
        let mut i = item(1, "done", "t", 1);
        i.agents.push(agent("finished", now - 2 * DAY));
        i.agents[0].closed = true;
        assert_eq!(
            assignee(&i, now),
            (
                "worker".into(),
                "finished · 2 d".into(),
                Role::Detail,
                false
            )
        );
        i.agents[0].status = "closed".into();
        assert_eq!(assignee(&i, now).1, "tab closed · 2 d");
        i.outcome = Some("failed".into());
        assert_eq!(
            assignee(&i, now),
            (
                "worker".into(),
                "failed · 2 d".into(),
                Role::FailedText,
                false
            )
        );
        (i.outcome, i.agents[0].closed) = (Some("verified".into()), false);
        i.agents[0].status = "idle".into();
        assert_eq!(assignee(&i, now).1, "done");
    }

    /// The quiet view keeps the cards someone must act on, found by the token: its word now
    /// carries an age, so it no longer equals the bare phrase.
    #[test]
    fn the_quiet_view_keeps_cards_that_wait_by_their_token() {
        let now = 10 * DAY;
        let mut p = page();
        p.now = now;
        p.quiet = true;
        {
            let items = p.items.as_mut().unwrap();
            items[2].agents.push(agent("waits", now - 3 * 3600));
            items[3].agents.push(agent("checks", now - 2 * DAY));
            items[4].agents.push(agent("idle", now - 2 * DAY));
        }
        let text = drawn(&p, 200, 50);
        for want in ["waits on you · 3 h", "qass checks · 2 d"] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(
            !text.contains("idle"),
            "a settled card says no word: {text}"
        );
    }

    fn task(id: i64, terminal: Option<i64>, status: &str, at: i64, goal: &str) -> TaskInfo {
        TaskInfo {
            id,
            workspace: Some(1),
            terminal,
            goal: goal.into(),
            stage: "implementing".into(),
            opened_at: 0,
            updated_at: at,
            claimed_at: None,
            closed_at: None,
            outcome: None,
            folder: None,
            status: status.into(),
            status_at: at,
        }
    }

    /// Twelve open tasks no item holds, the way a daemon sends them (need first, newest first
    /// inside a need), a page of the size the geometry tests click in.
    fn with_loose(p: &mut Page) {
        let now = 50 * DAY;
        p.now = now;
        p.size = (200, 50);
        let at = |days: i64| now - days * DAY;
        p.tasks = vec![
            task(3, Some(13), "waits", at(1), "third wait"),
            task(2, Some(12), "waits", at(2), "second wait"),
            task(1, Some(11), "waits", at(3), "first wait"),
            task(5, Some(15), "checks", at(1), "claimed five"),
            task(4, Some(14), "checks", at(8), "claimed four"),
            task(6, Some(16), "error", at(1), "broke"),
            task(7, Some(17), "asleep", at(13), "slept"),
            task(8, Some(18), "idle", at(1), "settled"),
            task(9, Some(19), "working", at(1), "busy"),
            task(10, None, "unknown", at(2), "lost its tab"),
            task(11, Some(20), "unknown", at(2), "unclear"),
            task(12, Some(21), "remote", at(2), "elsewhere"),
        ];
        p.loose = Some(12);
    }

    const COUNTS: &str =
        "3 wait on you · 2 qass checks · 1 error · 1 asleep · 1 idle · 1 working · 3 other";

    /// The folded row's label counts all of it, its count line lists what each need holds in
    /// need order, and wears Blocked when anything waits, else Failed when anything broke.
    #[test]
    fn the_folded_row_counts_work_in_need_order_in_the_ink_of_what_needs_him() {
        let mut p = page();
        with_loose(&mut p);
        let text = drawn(&p, 280, 50);
        assert!(text.contains("Not on the board · 12"), "{text}");
        assert!(text.contains(COUNTS), "{text}");
        // Doing's own count leaves them out.
        assert!(text.contains("doing 3 "), "{text}");
        let said = |p: &Page, ink: Role| {
            let frame = p.draw(280, 50).frame();
            frame.contains(&format!("{}{}", super::ink(ink), p.loose_counts().0))
        };
        assert!(said(&p, Role::Blocked));
        p.tasks.retain(|t| t.status != "waits");
        assert!(said(&p, Role::FailedText));
        p.tasks.retain(|t| t.status != "error");
        assert!(said(&p, Role::FaintText));
        // One with a count cut off says how many it did not list.
        p.loose = Some(p.tasks.len() + 4);
        assert!(p.loose_counts().0.ends_with("· 4 not listed"));
    }

    /// Hidden while the filter has text, absent from a daemon that sends no count (or a count
    /// of none), and there with no items on the board at all.
    #[test]
    fn the_folded_row_is_hidden_by_a_filter_absent_without_a_count_and_there_with_no_items() {
        let mut p = page();
        with_loose(&mut p);
        let said = |p: &Page| drawn(p, 280, 50).contains(LOOSE);
        assert!(said(&p));
        p.filter = "qoop".into();
        assert!(!said(&p));
        p.filter.clear();
        p.loose = None;
        assert!(!said(&p), "an older daemon sends every open task: no row");
        p.loose = Some(0);
        assert!(!said(&p));
        p.loose = Some(12);
        p.items = Some(Vec::new());
        p.settle();
        assert!(said(&p));
    }

    /// `N` opens it to a line per task, `T<terminal> <space> <word> · <age>  <goal>`, need first
    /// whatever order they arrive in; `j` past the last card steps into it, enter shows the
    /// line's tab, and a task with no tab is dimmed, says so, and goes nowhere.
    #[test]
    fn the_folded_row_opens_to_a_line_per_task_and_enter_goes_to_its_tab() {
        let mut p = page();
        with_loose(&mut p);
        let folded = drawn(&p, 280, 50);
        assert!(!folded.contains("first wait"), "{folded}");
        // Out of need order between the needs, as an older daemon might send them.
        p.tasks.rotate_left(5);
        assert_eq!(p.key(Key::Char('N')), Step::Stay);
        let text = drawn(&p, 280, 50);
        let mut at = 0;
        for line in [
            "T13 qoop waits on you · 1 d  third wait",
            "T12 qoop waits on you · 2 d  second wait",
            "T11 qoop waits on you · 3 d  first wait",
            "T15 qoop qass checks · 1 d  claimed five",
            "T14 qoop qass checks · 8 d  claimed four",
            "T16 qoop error · 1 d  broke",
            "T17 qoop asleep · 13 d  slept",
            "T18 qoop idle · 1 d  settled",
            "T19 qoop working · 1 d  busy",
            "qoop implementing · 2 d · no tab  lost its tab",
            "T20 qoop implementing · 2 d  unclear",
            "T21 qoop implementing · 2 d  elsewhere",
        ] {
            let found = text[at..].find(line);
            assert!(found.is_some(), "{line} after {at}: {text}");
            at += found.unwrap();
        }
        // A task with no terminal keeps its word and its ink and says no tab after it; one with
        // no word of its own says its stage, quiet.
        let frame = p.draw(280, 50).frame();
        let dim = format!("{}qoop implementing · 2 d · no tab", ink(Role::FaintText));
        assert!(frame.contains(&dim));
        let quiet = format!("{}T20 qoop implementing · 2 d", ink(Role::FaintText));
        assert!(frame.contains(&quiet));
        if let Some(t) = p.tasks.iter_mut().find(|t| t.goal == "third wait") {
            t.terminal = None;
        }
        let frame = p.draw(280, 50).frame();
        let waits = format!("{}qoop waits on you · 1 d · no tab", ink(Role::Blocked));
        assert!(
            frame.contains(&waits),
            "a wait with no tab still reads as one"
        );
        if let Some(t) = p.tasks.iter_mut().find(|t| t.goal == "third wait") {
            t.terminal = Some(13);
        }
        // Into it past Doing's last card, and back out from its first line.
        p.key(Key::Char('l'));
        for _ in 0..2 {
            p.key(Key::Char('j'));
        }
        assert_eq!(p.loose_cursor(), None);
        p.key(Key::Char('j'));
        assert_eq!(p.loose_cursor(), Some(0));
        assert_eq!(p.key(Key::Enter), Step::Goto(13));
        p.key(Key::Char('k'));
        assert_eq!(p.loose_cursor(), None);
        assert_eq!(p.selected().map(|i| i.id), Some(5));
        p.key(Key::Char('j'));
        for _ in 0..20 {
            p.key(Key::Char('j'));
        }
        assert_eq!(p.loose_cursor(), Some(11));
        // A task that runs elsewhere has no tab here, and one with no terminal has none at all.
        assert_eq!(p.key(Key::Enter), Step::Stay);
        assert!(
            p.said
                .as_deref()
                .is_some_and(|s| s.contains("another machine"))
        );
        p.key(Key::Char('k'));
        p.key(Key::Char('k'));
        assert_eq!(p.key(Key::Enter), Step::Stay);
        assert!(p.said.as_deref().is_some_and(|s| s.contains(NO_TAB)));
        assert_eq!(p.key(Key::Char('j')), Step::Stay);
        assert_eq!(p.key(Key::Enter), Step::Goto(20));
        assert_eq!(p.key(Key::Char('N')), Step::Stay);
        assert_eq!(p.loose_cursor(), None, "closed again");
    }

    /// The text of one row of a drawn page, escapes left out.
    fn row_text(p: &Page, w: usize, h: usize, row: usize) -> String {
        let frame = p.draw(w, h).frame();
        let from = frame.find(&format!("\x1b[{};1H", row + 1)).unwrap();
        let rest = &frame[from + 1..];
        let to = rest.find("\x1b[").map_or(rest.len(), |_| {
            rest.find(&format!("\x1b[{};1H", row + 2))
                .unwrap_or(rest.len())
        });
        let mut text = String::new();
        let mut in_esc = false;
        for ch in rest[..to].chars() {
            match (in_esc, ch) {
                (false, '\x1b') => in_esc = true,
                (false, ch) => text.push(ch),
                (true, 'm' | 'H') => in_esc = false,
                (true, _) => {}
            }
        }
        text
    }

    /// A click on the label folds and opens it, and one on a line selects it as a click on a
    /// card does: enter then goes to its tab. The rows clicked are the rows drawn.
    #[test]
    fn a_click_on_the_folded_row_opens_it_and_a_click_on_a_line_selects_it() {
        let mut p = page();
        with_loose(&mut p);
        let click = |row: usize| Key::Mouse {
            down: true,
            held: false,
            col: 70,
            row: row as u16,
        };
        // Folded: its two rows sit just above Doing's foot (a lane of 46 rows from row 2).
        assert!(row_text(&p, 200, 50, 44).contains(LOOSE));
        assert_eq!(p.key(click(44)), Step::Stay);
        assert!(p.loose_open);
        let rows = p.sect_rows(1, 46);
        assert_eq!(rows, 14);
        let top = 2 + 46 - 2 - rows;
        assert!(row_text(&p, 200, 50, top).contains(LOOSE));
        assert!(row_text(&p, 200, 50, top + 2).contains("T13 qoop waits on you"));
        assert_eq!(p.key(click(top + 3)), Step::Stay);
        assert_eq!(p.loose_cursor(), Some(1));
        assert_eq!(p.key(Key::Enter), Step::Goto(12));
        assert_eq!(p.key(click(top)), Step::Stay);
        assert!(!p.loose_open);
    }

    /// The smallest window still has one card in Doing above the folded row's two lines, and
    /// opened, one line of it.
    #[test]
    fn the_smallest_window_fits_a_card_and_the_folded_row() {
        let mut p = page();
        with_loose(&mut p);
        p.size = NEED;
        let lane_h = NEED.1 - 4;
        for (open, rows) in [(false, 2), (true, 3)] {
            p.loose_open = open;
            assert_eq!(p.sect_rows(1, lane_h), rows);
            assert_eq!(p.room(1), 1, "open: {open}");
            let (top, rows) = p.loose_place(2, lane_h).unwrap();
            let text = drawn(&p, NEED.0, NEED.1);
            // The card, its title on its own row and its key whole (no room left for its
            // word), and the label on the row the place says.
            assert!(
                text.contains("board every…") && text.contains("│ QOOP-3 "),
                "{text}"
            );
            assert!(row_text(&p, NEED.0, NEED.1, top).contains("Not on the"));
            // The rows end above Doing's foot and its bottom edge.
            assert_eq!(top + rows, 2 + lane_h - 2);
        }
    }

    /// Every lane puts what moved last on top; a tie keeps the daemon's order.
    #[test]
    fn a_lane_reads_newest_activity_first() {
        let mut p = page();
        for (n, at) in [(3, 10), (4, 30), (5, 20)] {
            p.items.as_mut().unwrap()[n - 1].updated_at = at;
        }
        let keys: Vec<&str> = p.lanes()[1].iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["QOOP-4", "QOOP-5", "QOOP-3"]);
    }

    /// A tie is newest first: the daemon sends cards oldest first within one second, and a lane
    /// reads as the phone's does, on every lane, the Done lane included.
    #[test]
    fn a_tie_in_a_lane_is_newest_first() {
        let mut p = page();
        {
            let items = p.items.as_mut().unwrap();
            items.push(item(7, "done", "closed with 6", 1));
            items.push(item(8, "todo", "made with 1 and 2", 1));
            for i in items.iter_mut() {
                (i.updated_at, i.closed_at, i.since) = (50, (i.state == "done").then_some(50), 0);
            }
        }
        let keys =
            |n: usize| -> Vec<String> { p.lanes()[n].iter().map(|i| i.key.clone()).collect() };
        assert_eq!(keys(0), ["QOOP-8", "QOOP-2", "QOOP-1"]);
        assert_eq!(keys(1), ["QOOP-5", "QOOP-4", "QOOP-3"]);
        assert_eq!(keys(2), ["QOOP-7", "QOOP-6"]);
    }

    /// The Done lane follows when items closed, not their last change (an ended agent can write
    /// after the close); an older daemon sends no close time and falls back to the last change.
    #[test]
    fn the_done_lane_reads_close_time_first() {
        let mut p = page();
        {
            let items = p.items.as_mut().unwrap();
            items.push(item(7, "done", "closed first, changed last", 1));
            items.push(item(8, "done", "closed last", 1));
            items.push(item(9, "done", "older daemon", 1));
            for (n, closed, since, updated) in [
                (6, Some(20), 20, 25),
                (7, Some(10), 10, 900),
                (8, Some(50), 50, 60),
            ] {
                let i = &mut items[n - 1];
                (i.closed_at, i.since, i.updated_at) = (closed, since, updated);
            }
            items[8].updated_at = 30;
        }
        let keys: Vec<&str> = p.lanes()[2].iter().map(|i| i.key.as_str()).collect();
        assert_eq!(keys, ["QOOP-8", "QOOP-9", "QOOP-6", "QOOP-7"]);
    }

    /// On a narrow full card a calm card's key stays whole and its word takes only the room it
    /// leaves; a word that wants the owner (waits, failed) stays whole and the key gives way,
    /// keeping its number. The key bar shows whole chips and always the way out.
    #[test]
    fn a_narrow_board_keeps_the_word_and_the_way_out() {
        let mut p = page();
        {
            let items = p.items.as_mut().unwrap();
            items[3].key = "QOOPPRIVAT-2".into();
            items[3].updated_at = 2_000;
            items[3].agents.push(ItemAgentInfo {
                task: 9,
                terminal: Some(4),
                name: "gtm".into(),
                machine: "this Mac".into(),
                stage: "waiting-human".into(),
                claimed: false,
                closed: false,
                since: 0,
                ..Default::default()
            });
            items[5].key = "QOOPPRIVAT-6".into();
            items[5].outcome = Some("failed".into());
        }
        let narrow = drawn(&p, NEED.0, NEED.1);
        assert!(
            narrow.contains("\u{2026}-2 waits │") && narrow.contains("\u{2026}-6 failed │"),
            "{narrow}"
        );
        assert!(
            !narrow.contains("wai…") && !narrow.contains("fai…"),
            "{narrow}"
        );
        assert!(
            narrow.contains("│ QOOP-1 "),
            "a calm card's key whole: {narrow}"
        );
        let roomy = drawn(&p, 100, NEED.1);
        assert!(
            roomy.contains(" waits") && roomy.contains(" failed"),
            "{roomy}"
        );
        for w in [NEED.0, 120, 200] {
            let bar = drawn(&p, w, NEED.1);
            let foot = &bar[bar.rfind(" board ").unwrap()..];
            assert!(foot.contains("[ q close ]"), "{w}: {foot}");
            assert_eq!(
                foot.matches("[ ").count(),
                foot.matches(" ]").count(),
                "{w}: {foot}"
            );
        }
    }

    /// A card's foot never cuts its lane age (a cut one reads as another age), says the lane
    /// words short where the long ones do not fit beside the key (`starts`, `no agent`), and
    /// places a wide agent name by its cells, never over the word or the card's edge.
    #[test]
    fn a_card_foot_cuts_no_fact_into_another() {
        let mut p = page();
        let now = p.now;
        {
            let items = p.items.as_mut().unwrap();
            for i in items.iter_mut() {
                i.since = now - 7200;
                i.key = format!("QOOPPRIVAT-{}", i.id);
            }
            items[2].agents.push(ItemAgentInfo {
                task: 3,
                terminal: Some(7),
                name: "日本語のエージェント".into(),
                machine: "this Mac".into(),
                stage: "verifying".into(),
                claimed: false,
                closed: false,
                since: now - 60,
                ..Default::default()
            });
        }
        for w in NEED.0..=120 {
            let text = drawn(&p, w, NEED.1);
            assert!(
                !text.contains("2 …") && !text.contains("2 h…") && !text.contains(" … "),
                "{w}: {text}"
            );
        }
        // Lanes of (63 - 6) / 3: cards of 13 cells inside (qass's tab at 120 x 40). The key
        // whole, the word gone where it has no room.
        let tight = drawn(&p, 63, NEED.1);
        assert!(
            tight.contains("│ QOOPPRIVAT-1  │") && !tight.contains("-1 no agent"),
            "{tight}"
        );
        let wide = drawn(&p, 200, 30);
        assert!(
            wide.contains("日本") && wide.contains(" verifying │"),
            "{wide}"
        );
        // With the room beside the whole key, the word in the longest form that fits.
        let mid = drawn(&p, 100, 30);
        assert!(
            mid.contains(" no agent yet │") && mid.contains(" starts │"),
            "{mid}"
        );
    }

    /// Where the board shares qass's tab with the questions (cards of 12 to 14 cells, as at
    /// 120 x 40 and 200 x 60), every card's title keeps its whole row and every key keeps its
    /// number: the key is cut in the middle, never the title to a few letters.
    #[test]
    fn a_narrow_card_keeps_its_title_and_its_key_number() {
        let mut p = page();
        for i in p.items.as_mut().unwrap().iter_mut() {
            i.key = format!("QOOPPRIVAT-{}", i.id);
        }
        for w in 60..=66 {
            let inner = (w - 6) / 3 - 6;
            let text = drawn(&p, w, 30);
            // Each card drawn: its title on its whole row, its key whole.
            for i in p.items.as_ref().unwrap() {
                if text.contains(&format!("QOOPPRIVAT-{} ", i.id)) {
                    assert!(
                        text.contains(&fit(&i.title, inner)),
                        "{w}: {}\n{text}",
                        i.title
                    );
                }
            }
            assert!(
                text.contains("│ QOOPPRIVAT-1 ") && text.contains("│ QOOPPRIVAT-3 "),
                "{w}: {text}"
            );
            assert!(
                !text.contains("\u{2026}-") && !text.contains("boar\u{2026}"),
                "{w}: {text}"
            );
        }
        // Only a key wider than the card is cut, in the middle, keeping its number.
        p.items.as_mut().unwrap()[0].key = "QOOPPRIVATEXTRA-1".into();
        let text = drawn(&p, 60, 30);
        assert!(text.contains("│ QOOPPRIVA\u{2026}-1 │"), "{text}");
    }

    /// A quiet card's top edge places its ages and a wide agent name by cells: no corner is
    /// written over, at any width. A Done card closed a moment ago says `just now`.
    #[test]
    fn a_quiet_card_s_edge_keeps_its_corners_and_a_done_card_its_words() {
        let mut p = page();
        let now = p.now;
        let with_agent = |p: &mut Page, name: &str| {
            let items = p.items.as_mut().unwrap();
            items[2].agents = vec![ItemAgentInfo {
                task: 3,
                terminal: Some(7),
                name: name.into(),
                machine: "this Mac".into(),
                stage: "implementing".into(),
                claimed: false,
                closed: false,
                since: now - 60,
                ..Default::default()
            }];
            items[2].since = now - 60;
        };
        p.quiet = true;
        for w in [144, 176, 200, 240] {
            with_agent(&mut p, "日本語のエージェント名前テスト");
            let wide = drawn(&p, w, 30);
            with_agent(&mut p, "release-build-and-notarize");
            let plain = drawn(&p, w, 30);
            assert_eq!(
                wide.matches('┐').count(),
                plain.matches('┐').count(),
                "{w}: {wide}"
            );
            assert!(!wide.contains("…─") && !plain.contains("…─"), "{w}: {wide}");
        }
        p.quiet = false;
        p.items.as_mut().unwrap()[5].since = now - 10;
        let done = drawn(&p, 120, 30);
        assert!(
            done.contains("just now") && !done.contains("now ago"),
            "{done}"
        );
    }

    /// In qass's tab the pane's title already says `board`: the heading keeps its sub line.
    #[test]
    fn in_qass_s_tab_the_heading_does_not_say_board_twice() {
        let mut p = page();
        let alone = drawn(&p, 120, 30);
        assert!(alone.starts_with(" board  your tasks"), "{alone}");
        p.in_qass_tab = true;
        let in_tab = drawn(&p, 120, 30);
        assert!(in_tab.starts_with(" your tasks, run by qass"), "{in_tab}");
    }

    /// The full card says its key, how long it has been in its lane, its agent and how long
    /// that agent has worked, and one word; the quiet card says a word only when someone must
    /// act. `v` turns the view and asks for it to be kept; so does a click on the switch.
    #[test]
    fn full_and_quiet_cards_show_one_word_decided_once() {
        let mut p = page();
        p.now = 10_000;
        let agent = |name: &str, stage: &str, claimed: bool| ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: name.into(),
            machine: "this Mac".into(),
            stage: stage.into(),
            claimed,
            closed: false,
            since: 10_000 - 2 * 3600,
            ..Default::default()
        };
        {
            let items = p.items.as_mut().unwrap();
            items[2].since = 10_000 - 12 * 60;
            items[2].agents.push(agent("onyx", "implementing", false));
            items[3].agents.push(agent("gtm", "waiting-human", false));
            items[4].agents.push(agent("app", "verifying", true));
        }
        let full = drawn(&p, 200, 50);
        for want in [
            "QOOP-3",
            "12 min",
            "2 h onyx",
            "implementing",
            WAITS,
            CHECKS,
        ] {
            assert!(full.contains(want), "{want}: {full}");
        }
        assert_eq!(p.key(Key::Char('v')), Step::Cards(true));
        let quiet = drawn(&p, 200, 50);
        assert!(quiet.contains(WAITS) && quiet.contains(CHECKS), "{quiet}");
        assert!(
            !quiet.contains("implementing"),
            "a working card says no word: {quiet}"
        );
        assert!(quiet.contains("✓ space goals"), "{quiet}");
        assert_eq!(p.card_h(), QUIET_H);
        let narrow = drawn(&p, NEED.0, NEED.1);
        assert!(
            narrow.contains("waits") && !narrow.contains(WAITS),
            "a narrow quiet card keeps room for its title: {narrow}"
        );
        p.size = (200, 50);
        let x = p.switch_at(200) + 3;
        let click = Key::Mouse {
            down: true,
            held: false,
            col: x as u16,
            row: 0,
        };
        assert_eq!(p.key(click), Step::Cards(false));
        assert_eq!(p.card_h(), FULL_H);
    }

    #[test]
    fn the_board_draws_its_lanes_cards_and_keys_at_the_smallest_window_and_a_wide_one() {
        let p = page();
        for (w, h) in [NEED, (200, 50)] {
            let text = drawn(&p, w, h);
            for want in ["todo 2", "doing 3", "done 1", "-1 ", "qass", "select"] {
                assert!(text.contains(want), "{want} at {w}x{h}: {text}");
            }
        }
        assert!(drawn(&p, 200, 50).contains("QOOP-1"));
        let text = drawn(&p, 200, 50);
        for want in [
            "board everywhere",
            "no agent yet",
            "qass will start it",
            "closed",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        // A card's foot stays inside its card in the narrowest lane: the key whole, the word
        // left out where it has no room, the title on its own row.
        let mut narrow = page();
        narrow.items.as_mut().unwrap()[0].key = "DEFAULT-12".into();
        let (w, h) = NEED;
        let text: Vec<char> = drawn(&narrow, w, h).chars().collect();
        let lw = (w - 6) / 3;
        let (right, foot) = (2 + lw - 2 - 1, 2 + 1 + 2);
        assert_eq!(text[foot * w + right], '│');
        let row: String = text[foot * w..foot * w + right].iter().collect();
        assert!(
            row.contains("DEFAULT-12") && !row.contains("no agent"),
            "{row}"
        );
        let mut old = page();
        old.items = None;
        assert!(drawn(&old, 80, 20).contains("older than the board"));
    }

    /// Numbered words, so a wrapped or cut text can be checked word by word.
    fn words(prefix: &str, n: usize) -> String {
        (0..n)
            .map(|i| format!("{prefix}{i:03}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn event(kind: &str, body: &str, at: i64) -> qoop_runtime::control::ItemEventInfo {
        qoop_runtime::control::ItemEventInfo {
            task: 1,
            at,
            kind: kind.into(),
            body: body.into(),
        }
    }

    fn open_with(
        size: (usize, usize),
        steps: Vec<StepInfo>,
        activity: Vec<qoop_runtime::control::ItemEventInfo>,
    ) -> Page {
        let mut p = page();
        p.size = size;
        p.card = Some(ItemCardInfo {
            plan_file: None,
            item: item(1, "doing", "t", 1),
            description: String::new(),
            steps,
            activity,
        });
        p.mode = Mode::Card(0);
        p
    }

    /// Walk the whole body a page at a time: every word must be drawn at some point.
    fn all_seen(p: &mut Page, words: &[String]) -> bool {
        let mut left: Vec<&String> = words.iter().collect();
        p.key(Key::Home);
        for _ in 0..400 {
            let text = drawn(p, p.size.0, p.size.1);
            left.retain(|w| !text.contains(w.as_str()));
            if left.is_empty() {
                return true;
            }
            let before = p.scroll;
            p.key(Key::PageDown);
            if p.scroll == before {
                break;
            }
        }
        left.is_empty()
    }

    #[test]
    fn a_long_evidence_line_is_cut_to_three_rows_and_opens_whole() {
        let body = words("w", 100);
        let mut p = open_with((120, 40), Vec::new(), vec![event("evidence", &body, 5)]);
        let text = drawn(&p, 120, 40);
        assert!(text.contains("… more"), "{text}");
        assert!(text.contains("w000") && !text.contains("w099"), "{text}");
        // Enter opens it in place, enter again shuts it.
        p.key(Key::Enter);
        let text = drawn(&p, 120, 40);
        assert!(text.contains("w099") && !text.contains("… more"), "{text}");
        p.key(Key::Enter);
        assert!(drawn(&p, 120, 40).contains("… more"));
        // `E` opens every entry, and shuts them again.
        p.key(Key::Char('E'));
        assert!(drawn(&p, 120, 40).contains("w099"));
        p.key(Key::Char('E'));
        assert!(!drawn(&p, 120, 40).contains("w099"));
    }

    #[test]
    fn every_word_of_an_open_entry_is_reachable_in_the_smallest_window() {
        let list: Vec<String> = (0..100).map(|i| format!("w{i:03}")).collect();
        let mut p = open_with(
            NEED,
            Vec::new(),
            vec![event("evidence", &list.join(" "), 5)],
        );
        p.key(Key::Enter);
        assert!(all_seen(&mut p, &list));
    }

    #[test]
    fn the_title_description_steps_and_details_are_whole() {
        let title = words("t", 50);
        let mut p = page();
        p.size = (120, 40);
        let mut it = item(1, "doing", &title, 1);
        it.agents.push(ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: words("n", 12),
            machine: "this Mac".into(),
            stage: "working".into(),
            claimed: false,
            closed: false,
            since: 0,
            ..Default::default()
        });
        p.card = Some(ItemCardInfo {
            plan_file: None,
            item: it,
            description: words("d", 400),
            steps: vec![StepInfo {
                n: 1,
                text: words("s", 30),
                done_at: None,
            }],
            activity: Vec::new(),
        });
        p.mode = Mode::Card(0);
        let mut list: Vec<String> = Vec::new();
        for (prefix, n) in [("t", 50), ("d", 400), ("s", 30), ("n", 12)] {
            list.extend((0..n).map(|i| format!("{prefix}{i:03}")));
        }
        assert!(all_seen(&mut p, &list));
    }

    #[test]
    fn the_body_scrolls_by_key_page_wheel_and_says_so() {
        let activity: Vec<_> = (0..60)
            .map(|i| event("evidence", &format!("entry{i:03}"), 100 - i))
            .collect();
        let mut p = open_with(NEED, Vec::new(), activity);
        let text = drawn(&p, NEED.0, NEED.1);
        assert!(
            text.contains("Description") && !text.contains("entry059"),
            "{text}"
        );
        assert!(text.contains('▼') && !text.contains('▲'), "{text}");
        p.key(Key::End);
        let text = drawn(&p, NEED.0, NEED.1);
        assert!(text.contains("entry059") && text.contains('▲') && !text.contains('▼'));
        p.key(Key::PageUp);
        assert!(!drawn(&p, NEED.0, NEED.1).contains("entry059"));
        p.key(Key::Home);
        assert_eq!(p.scroll, 0);
        p.key(Key::Wheel {
            up: false,
            col: 5,
            row: 6,
        });
        assert_eq!(p.scroll, 3);
        p.key(Key::Wheel {
            up: true,
            col: 5,
            row: 6,
        });
        assert_eq!(p.scroll, 0);
        // `j` walks the entries and the view follows to the last one.
        for _ in 0..70 {
            p.key(Key::Char('j'));
        }
        assert!(drawn(&p, NEED.0, NEED.1).contains("entry059"));
        for _ in 0..200 {
            p.key(Key::Char('k'));
        }
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn a_short_body_does_not_scroll_and_a_tiny_window_does_not_panic() {
        let mut p = open_with((200, 50), Vec::new(), vec![event("goal", "short", 1)]);
        for key in [
            Key::PageDown,
            Key::End,
            Key::Char('j'),
            Key::Char('k'),
            Key::Enter,
        ] {
            p.key(key);
        }
        assert_eq!(p.scroll, 0);
        p.size = (4, 3);
        for key in [
            Key::PageDown,
            Key::End,
            Key::Char('j'),
            Key::Char('k'),
            Key::Enter,
            Key::Char('E'),
            Key::Wheel {
                up: false,
                col: 1,
                row: 1,
            },
            Key::Mouse {
                down: true,
                held: false,
                col: 3,
                row: 2,
            },
        ] {
            p.key(key);
        }
        p.size = (0, 0);
        p.key(Key::PageDown);
        let _ = drawn(&p, NEED.0, NEED.1);
    }

    #[test]
    fn a_click_on_an_entry_opens_it_and_a_click_on_the_details_does_not() {
        let body = words("w", 100);
        let mut p = open_with(
            (120, 40),
            Vec::new(),
            vec![event("evidence", &body, 5), event("goal", "other", 4)],
        );
        let pane = p.pane();
        let press = |col: usize, row: usize| Key::Mouse {
            down: true,
            held: false,
            col: col as u16,
            row: row as u16,
        };
        // The Details column shares the rows and toggles nothing.
        p.key(press(pane.dx + 2, pane.top + 1));
        assert!(!drawn(&p, 120, 40).contains("w099"));
        let rows = p.rows(p.card.as_ref().unwrap(), pane.lw);
        let first = rows.iter().position(|r| r.stop == Some(0)).unwrap();
        p.key(press(pane.lx + 14, pane.top + first));
        assert!(drawn(&p, 120, 40).contains("w099"));
        assert_eq!(p.mode, Mode::Card(0));
        let rows = p.rows(p.card.as_ref().unwrap(), pane.lw);
        let second = rows.iter().position(|r| r.stop == Some(1)).unwrap();
        p.key(press(pane.lx + 14, pane.top + second));
        assert_eq!(p.mode, Mode::Card(1));
    }

    #[test]
    fn a_reload_keeps_the_stop_the_opened_entry_and_the_place_in_the_body() {
        let old: Vec<_> = (0..40)
            .map(|i| {
                event(
                    "evidence",
                    &format!("entry{i:03} {}", words("x", 30)),
                    1000 - i,
                )
            })
            .collect();
        let mut p = open_with(NEED, Vec::new(), old.clone());
        for _ in 0..13 {
            p.key(Key::Char('j'));
        }
        p.key(Key::Enter);
        assert_eq!(p.mode, Mode::Card(12));
        let seen = |p: &Page| drawn(p, NEED.0, NEED.1).contains("entry012");
        assert!(seen(&p));
        // Two newer events arrive.
        let mut next = vec![
            event("evidence", "fresh", 2000),
            event("evidence", "fresh2", 1999),
        ];
        next.extend(old);
        let card = ItemCardInfo {
            activity: next,
            ..p.card.clone().unwrap()
        };
        p.keep_card(card, false);
        assert_eq!(p.mode, Mode::Card(14), "the stop followed its entry");
        assert!(seen(&p), "the view stayed on it");
        assert_eq!(p.flipped.len(), 1);
        let open = |p: &Page| {
            let rows = p.rows(p.card.as_ref().unwrap(), p.pane().lw);
            rows.iter().filter(|r| r.stop == Some(14)).count()
        };
        assert!(open(&p) > ENTRY_ROWS, "still open");
        // The entry gone: the stop clamps and nothing panics.
        let card = ItemCardInfo {
            activity: vec![event("goal", "only", 1)],
            ..p.card.clone().unwrap()
        };
        p.keep_card(card, true);
        assert_eq!(p.mode, Mode::Card(0));
        assert_eq!(
            p.scroll,
            p.max_scroll(),
            "the view it was on is gone: the end"
        );
        assert!(drawn(&p, NEED.0, NEED.1).contains("older activity is not shown"));
    }

    #[test]
    fn identical_events_are_told_apart_from_the_oldest() {
        let card = ItemCardInfo {
            plan_file: None,
            item: item(1, "doing", "t", 1),
            description: String::new(),
            steps: Vec::new(),
            activity: vec![
                event("done", "x", 5),
                event("done", "x", 5),
                event("done", "x", 5),
            ],
        };
        let keys = entry_keys(&card);
        assert_eq!([keys[0].4, keys[1].4, keys[2].4], [2, 1, 0]);
        let mut longer = card.clone();
        longer.activity.insert(0, event("done", "x", 5));
        assert_eq!(entry_keys(&longer)[1..], keys[..]);
    }

    #[test]
    fn text_wraps_on_cells_and_keeps_every_letter() {
        let wide = wrap_cells("中文中文中文 ab", 5, None);
        assert!(wide.iter().all(|r| width::width(r) <= 5), "{wide:?}");
        assert_eq!(wide.concat().replace(' ', ""), "中文中文中文ab");
        let turkish = wrap_cells("ğüşiöçİı ğüşiöçİı ğüşiöçİı", 9, None);
        assert_eq!(turkish, ["ğüşiöçİı", "ğüşiöçİı", "ğüşiöçİı"]);
        // A decomposed letter is one cell and stays whole.
        let nfd = wrap_cells("Tu\u{0308}rkc\u{0327}e", 4, None);
        assert!(nfd.iter().all(|r| width::width(r) <= 4), "{nfd:?}");
        assert_eq!(nfd.concat(), "Tu\u{0308}rkc\u{0327}e");
        // A word longer than a row breaks where the row is full; a tab is a space.
        assert_eq!(wrap_cells("abcdefgh", 3, None), ["abc", "def", "gh"]);
        assert_eq!(wrap_cells("a\tb\x1b[31m", 9, None), ["a b[31m"]);
        assert_eq!(wrap_cells("a b c d e f", 1, Some(2)), ["a", "b"]);
        assert!(wrap_cells("", 5, None).is_empty());
        assert_eq!(fit("中文中文", 5), "中文…");
        assert_eq!(fit("qass will start it", 6), "qass…");
    }

    /// The list's board: an item in every group. QOOP-1 is in Todo with a plan that waited too
    /// long, QOOP-4 in Doing has an agent that waits on him: both need him. QOOP-3 works to an
    /// approved plan in its Build phase; QOOP-2 (Todo) and QOOP-5 (Doing, no plan, no phase)
    /// have no plan yet; QOOP-6 is done.
    fn list_page() -> Page {
        let mut p = page();
        p.view = View::List;
        p.now = 10_000;
        let agent = |status: &str| ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: "onyx".into(),
            machine: "this Mac".into(),
            stage: "implementing".into(),
            status: status.into(),
            status_at: 10_000 - 60,
            since: 10_000 - 3600,
            ..Default::default()
        };
        {
            let items = p.items.as_mut().unwrap();
            items[0].plan = "stale".into();
            items[2].agents.push(agent("working"));
            items[2].plan = "approved".into();
            items[2].plan_by = Some("qass".into());
            items[2].phase = Some(qoop_runtime::control::PhaseInfo {
                n: 2,
                of: 3,
                name: "Build".into(),
            });
            (items[2].steps_done, items[2].steps_total) = (2, 5);
            items[3].agents.push(agent("waits"));
        }
        p.settle();
        p
    }

    /// The text of the list's rows from its first, one a line, the escapes left out.
    fn list_rows(p: &Page, w: usize, h: usize) -> Vec<String> {
        (2..h - 2)
            .map(|row| {
                let text = row_text(p, w, h, row);
                text.split_once('H')
                    .map_or(text.clone(), |(_, r)| r.to_string())
            })
            .collect()
    }

    /// The row of the list that names `want`, trimmed.
    fn row_of(rows: &[String], want: &str) -> String {
        rows.iter()
            .find(|r| r.contains(want))
            .unwrap_or_else(|| panic!("{want} in {rows:#?}"))
            .trim()
            .to_string()
    }

    /// The list is the page's first view. Its groups come in the order of what they want of
    /// him, each a rule with its count, an empty group not drawn, Done folded to its rule; the
    /// heading counts what needs him. At 120, 80 and 60 columns alike.
    #[test]
    fn the_list_groups_the_items_by_what_they_want_at_every_width() {
        assert_eq!(Page::new().view, View::List, "the list is the first view");
        let mut p = list_page();
        for (w, h) in [(120, 40), (80, 24), (60, 24)] {
            p.size = (w, h);
            let rows = list_rows(&p, w, h);
            let at = |want: &str| rows.iter().position(|r| r.contains(want)).unwrap();
            let order = [
                at("Needs you 2"),
                at("QOOP-1"),
                at("QOOP-4"),
                at("In progress 2"),
                at("QOOP-3"),
                at("QOOP-5"),
                at("Not started 1"),
                at("QOOP-2"),
                at("Done 1"),
            ];
            assert!(order.is_sorted(), "{w}: {rows:#?}");
            assert!(
                !rows.iter().any(|r| r.contains("QOOP-6")),
                "folded: {rows:#?}"
            );
            let head = drawn(&p, w, h).chars().take(w).collect::<String>();
            if w >= 80 {
                assert!(head.contains("2 need you"), "{head}");
                assert!(head.contains("[ list · board  b ]"), "{head}");
            }
            assert!(
                !head.contains("full · quiet"),
                "no cards in the list: {head}"
            );
        }
        // A group with nothing in it is not drawn.
        p.items.as_mut().unwrap().retain(|i| i.state != "done");
        let text = drawn(&p, 120, 40);
        assert!(!text.contains("Done "), "{text}");
    }

    /// One flat list: `j` and `k` walk it across the groups onto the Done row, enter opens and
    /// folds Done, `h` and `l` do nothing; the lane follows the item, so `H`, `L` and `x` act on
    /// it as on the board.
    #[test]
    fn the_keyboard_walks_the_list_and_acts_on_its_item() {
        let mut p = list_page();
        p.size = (120, 40);
        assert_eq!(p.sel, Some(1), "the first row has it");
        let mut walked = vec![p.sel.unwrap()];
        for _ in 0..6 {
            p.key(Key::Char('j'));
            walked.push(p.sel.unwrap());
        }
        assert_eq!(walked, [1, 4, 3, 5, 2, FOLD, FOLD]);
        assert_eq!(p.key(Key::Enter), Step::Stay);
        assert!(drawn(&p, 120, 40).contains("QOOP-6"), "opened");
        p.key(Key::Char('j'));
        assert_eq!((p.sel, p.lane), (Some(6), 2));
        assert!(matches!(
            p.key(Key::Char('x')),
            Step::Write(6, TaskOp::Dismiss { undo: false }, _)
        ));
        p.key(Key::Char('k'));
        p.key(Key::Char('k'));
        assert_eq!((p.sel, p.lane), (Some(2), 0));
        p.key(Key::Char('l'));
        p.key(Key::Char('h'));
        assert_eq!((p.sel, p.lane), (Some(2), 0), "no sideways in a list");
        assert_eq!(p.key(Key::Char('L')), Step::Start(2, "QOOP-2".into()));
        assert_eq!(p.key(Key::Enter), Step::Open(2));
        p.key(Key::Char('k'));
        assert_eq!((p.sel, p.lane), (Some(5), 1));
        assert!(matches!(
            p.key(Key::Char('L')),
            Step::Write(5, TaskOp::Close { .. }, _)
        ));
        // Enter on the Done row folds it again.
        while p.sel != Some(FOLD) {
            p.key(Key::Char('j'));
        }
        p.key(Key::Enter);
        assert!(!drawn(&p, 120, 40).contains("QOOP-6"), "folded again");
    }

    /// An item that leaves the list's stops on a poll (closed into the folded Done) leaves the
    /// keyboard on the row now at its place, not the first; one that changes group is brought
    /// into view.
    #[test]
    fn the_list_keeps_its_place_when_the_selected_item_moves() {
        let mut p = list_page();
        p.size = (60, 10);
        for _ in 0..2 {
            p.key(Key::Char('j'));
        }
        assert_eq!(p.sel, Some(3));
        p.items.as_mut().unwrap()[2] = item(3, "done", "board everywhere", 1);
        p.settle();
        assert_eq!(p.sel, Some(5), "the row now at its place");
        // A poll that moves it to another group out of view brings it back into view.
        let mut p = list_page();
        p.size = (60, FLOOR.1);
        p.settle();
        assert_eq!((p.sel, p.list_top), (Some(1), 0));
        p.items.as_mut().unwrap()[0].plan.clear();
        p.settle();
        assert_eq!(p.sel, Some(1));
        let rows = list_rows(&p, 60, FLOOR.1);
        assert!(rows.iter().any(|r| r.contains("QOOP-1")), "{rows:#?}");
    }

    /// The mark is qoop's own picture for the item: the ring when it needs him, full once done
    /// or at its last phase done, its live agent's state, the ring in the quiet ink in Todo,
    /// the centre dot with no agent in Doing.
    #[test]
    fn a_row_wears_the_mark_of_its_items_state() {
        let mark = |role| qoop_tui::sidebar::state_mark(role, false, 0, false);
        let p = list_page();
        let items = p.items.as_ref().unwrap();
        let of = |id: i64| item_mark(items.iter().find(|i| i.id == id).unwrap());
        assert_eq!(of(1), (mark(Role::Blocked), Role::Blocked), "a stale plan");
        assert_eq!(
            of(4),
            (mark(Role::Blocked), Role::Blocked),
            "an agent waits"
        );
        assert_eq!(of(3), (mark(Role::Working), Role::Working));
        assert_eq!(
            of(2),
            (mark(Role::Blocked), Role::FaintText),
            "Todo's quiet ring"
        );
        assert_eq!(of(5), (mark(Role::Idle), Role::Idle), "Doing, no agent");
        // Under NO_COLOR (or without braille) the ink is gone: Todo's mark is not the ring.
        for (braille, colour) in [(true, false), (false, true), (false, false)] {
            assert_eq!(todo_ring(braille, colour), "o ");
            assert_ne!(todo_ring(braille, colour), of(1).0);
        }
        assert_eq!(of(6), (mark(Role::Done), Role::Done));
        let mut it = items[2].clone();
        it.open_asks = 1;
        assert_eq!(item_mark(&it).1, Role::Blocked, "a question open");
        it.open_asks = 0;
        it.agents[0].status = "asleep".into();
        assert_eq!(item_mark(&it), (mark(Role::Sleeping), Role::Sleeping));
        it.agents[0].status = "idle".into();
        assert_eq!(item_mark(&it), (mark(Role::Idle), Role::Idle));
        it.phase = Some(qoop_runtime::control::PhaseInfo {
            n: 3,
            of: 3,
            name: "Verify".into(),
        });
        it.steps_done = 5;
        assert_eq!(
            item_mark(&it),
            (mark(Role::Done), Role::Done),
            "its last phase done"
        );
        // Every picture is drawn: the row carries the mark before its key.
        let rows = list_rows(&p, 120, 40);
        assert!(
            row_of(&rows, "QOOP-3").starts_with(&mark(Role::Working)),
            "{rows:#?}"
        );
    }

    /// A row says its plan, questions, phase, steps and agent word right of its title; as the
    /// page narrows they give way from the right, the word that wants him last, while the
    /// mark, key and title stay. A wide title is cut by cells. Each state is a word, not only
    /// an ink, so the list reads the same under NO_COLOR.
    #[test]
    fn a_row_gives_up_its_parts_from_the_right_and_keeps_what_needs_him() {
        let mut p = list_page();
        p.items.as_mut().unwrap()[2].open_asks = 1;
        let rows = list_rows(&p, 120, 40);
        let row = row_of(&rows, "QOOP-3");
        for want in [
            "board everywhere",
            "plan ok · qass",
            "1 question",
            "Build 2/3",
            "2/5",
            "working",
        ] {
            assert!(row.contains(want), "{want}: {row}");
        }
        assert!(
            row_of(&rows, "QOOP-1").contains("plan needs you"),
            "{rows:#?}"
        );
        // A plan card open: the store counts it as the plan's ask, not a question too.
        p.items.as_mut().unwrap()[0].plan = "asked".into();
        let asked = row_of(&list_rows(&p, 120, 40), "QOOP-1");
        assert!(
            asked.contains("plan needs you") && !asked.contains("question"),
            "{asked}"
        );
        assert!(
            row_of(&rows, "QOOP-4").contains("waits on you"),
            "{rows:#?}"
        );
        assert!(
            row_of(&rows, "QOOP-2").contains("no agent yet"),
            "{rows:#?}"
        );
        // At 60 columns the parts that do not need him go first.
        let rows = list_rows(&p, 60, 24);
        let row = row_of(&rows, "QOOP-3");
        assert!(
            row.contains("board everywhere") && row.contains("1 question"),
            "{row}"
        );
        assert!(!row.contains("working") && !row.contains("2/5"), "{row}");
        assert!(
            row_of(&rows, "QOOP-4").contains("waits on you"),
            "{rows:#?}"
        );
        // At the narrowest the mark, key and title stay.
        let rows = list_rows(&p, FLOOR.0 + 20, 24);
        let row = row_of(&rows, "QOOP-3");
        assert!(row.contains("board ev"), "{row}");
        // A title of wide characters is cut by cells, never past the row.
        p.items.as_mut().unwrap()[1].title =
            "看板の一覧を作る看板の一覧を作る看板の一覧を作る看板".into();
        for w in [60, 80] {
            let row = row_of(&list_rows(&p, w, 24), "QOOP-2");
            assert!(
                row.contains('…') && width::width(&row) <= w - 2,
                "{w}: {row}"
            );
        }
    }

    /// `b` turns the list into the lanes and back on the same item, and asks for the view to be
    /// kept; so does a click on the switch. An item in Done opens Done when the list comes
    /// back; the lanes keep their card switch.
    #[test]
    fn b_and_the_switch_turn_the_view_on_the_same_item() {
        let mut p = list_page();
        p.size = (120, 40);
        p.key(Key::Char('j'));
        p.key(Key::Char('j'));
        assert_eq!(p.sel, Some(3));
        assert_eq!(p.key(Key::Char('b')), Step::View(View::Board));
        assert_eq!((p.view, p.sel, p.lane), (View::Board, Some(3), 1));
        let text = drawn(&p, 120, 40);
        assert!(
            text.contains("todo 2") && text.contains("[ full · quiet  v ]"),
            "{text}"
        );
        // A Done card in the lanes, then the list: Done is open on it.
        p.key(Key::Char('l'));
        assert_eq!(p.sel, Some(6));
        let x = p.view_at(120) + 3;
        let click = Key::Mouse {
            down: true,
            held: false,
            col: x as u16,
            row: 0,
        };
        assert_eq!(p.key(click), Step::View(View::List));
        assert_eq!((p.view, p.sel), (View::List, Some(6)));
        assert!(drawn(&p, 120, 40).contains("QOOP-6"), "Done opened on it");
        // A click on a row opens its item; on the Done row it folds.
        let rows = list_rows(&p, 120, 40);
        let row = rows.iter().position(|r| r.contains("QOOP-4")).unwrap() + 2;
        let press = |row: usize| Key::Mouse {
            down: true,
            held: false,
            col: 30,
            row: row as u16,
        };
        assert_eq!(p.key(press(row)), Step::Open(4));
        assert_eq!(p.sel, Some(4));
        let fold = rows.iter().position(|r| r.contains("Done 1")).unwrap() + 2;
        assert_eq!(p.key(press(fold)), Step::Stay);
        assert!(!drawn(&p, 120, 40).contains("QOOP-6"), "folded");
        // The footer names `b` at 60 columns, the other keys giving way first.
        let foot = drawn(&p, 60, 24).chars().skip(23 * 60).collect::<String>();
        assert!(
            foot.contains("b board") && foot.contains("q close"),
            "{foot}"
        );
    }

    /// In qass's split (60 columns) the lanes keep their card switch: it takes the view
    /// switch's place, and the footer still says `b`.
    #[test]
    fn the_lanes_keep_the_card_switch_at_60_columns() {
        let mut p = list_page();
        p.size = (60, 24);
        assert_eq!(p.key(Key::Char('b')), Step::View(View::Board));
        let text = drawn(&p, 60, 24);
        let head = text.chars().take(60).collect::<String>();
        assert!(
            head.contains("[ full · quiet  v ]") && !head.contains("list · board"),
            "{head}"
        );
        let foot = text.chars().skip(23 * 60).collect::<String>();
        assert!(foot.contains("b list"), "{foot}");
        let click = |x: usize| Key::Mouse {
            down: true,
            held: false,
            col: x as u16,
            row: 0,
        };
        assert_eq!(p.switch_at(60), p.view_at(60));
        assert_eq!(p.key(click(p.switch_at(60) + 3)), Step::Cards(true));
        // The list keeps its view switch at the same width.
        assert_eq!(p.key(Key::Char('b')), Step::View(View::List));
        assert_eq!(p.key(click(p.view_at(60) + 3)), Step::View(View::Board));
    }

    /// With nothing on the board the list says how to put something there; a filter that hides
    /// everything says how to clear it.
    #[test]
    fn an_empty_list_says_how_to_fill_it() {
        let mut p = list_page();
        p.items = Some(Vec::new());
        p.settle();
        assert!(drawn(&p, 80, 24).contains(EMPTY));
        let mut p = list_page();
        p.filter = "nothing like it".into();
        p.settle();
        let text = drawn(&p, 80, 24);
        assert!(text.contains("Nothing matches the filter"), "{text}");
        assert!(!text.contains("Needs you"), "{text}");
        assert!(
            !text.contains("need you"),
            "the heading counts the group: {text}"
        );
    }

    /// The work no item holds sits folded at the list's foot as on the board: `N` opens it, `j`
    /// past the last row steps into it, enter goes to its tab.
    #[test]
    fn the_folded_row_sits_at_the_foot_of_the_list() {
        let mut p = list_page();
        with_loose(&mut p);
        p.size = (120, 40);
        assert!(row_text(&p, 120, 40, 36).contains("Not on the board · 12"));
        assert!(row_text(&p, 120, 40, 37).contains(COUNTS));
        p.key(Key::Char('N'));
        // Seven rows and the Done row stand before it: a few more `j` than that must reach it.
        for _ in 0..10 {
            if p.loose_cursor().is_some() {
                break;
            }
            p.key(Key::Char('j'));
        }
        assert_eq!(p.loose_cursor(), Some(0));
        assert_eq!(p.key(Key::Enter), Step::Goto(13));
        assert!(drawn(&p, 120, 40).contains("third wait"), "opened");
    }

    /// The dialog: the plan above the checklist (where it stands, by whom and when, its file,
    /// its summary); a phase's heading is a rule over its steps, never a stop or a tick.
    #[test]
    fn the_dialog_shows_the_plan_and_the_steps_under_their_phases() {
        let step = |n: i64, text: &str, done: bool| StepInfo {
            n,
            text: text.into(),
            done_at: done.then_some(5),
        };
        let mut p = open_with(
            (120, 40),
            vec![
                step(1, "## Plan", true),
                step(2, "write it", true),
                step(3, "## Build", true),
                step(4, "code it", false),
            ],
            Vec::new(),
        );
        p.now = 10_000;
        let card = p.card.take().unwrap();
        let mut card = card;
        card.item.plan = "approved".into();
        card.item.plan_by = Some("owner".into());
        card.item.plan_at = Some(10_000 - 7200);
        card.plan_file = Some(qoop_runtime::control::PlanFileInfo {
            path: "/w/plans/2026-11-01-ship.md".into(),
            hash: "abc".into(),
            terminal: 4,
            summary: "Ship the list view.".into(),
            at: 1,
        });
        p.open_card(card, false);
        assert_eq!(p.mode, Mode::Card(1), "on the first step, past the heading");
        let text = drawn(&p, 120, 40);
        for want in [
            "Plan  ok · you · 2 h ago",
            "2026-11-01-ship.md",
            "Ship the list view.",
            "Phase 1 of 2: Plan",
            "Phase 2 of 2: Build",
            "[ ] code it",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(!text.contains("## "), "{text}");
        assert!(text.find("Plan  ok").unwrap() < text.find("Checklist").unwrap());
        // `j` steps over the heading; back up, `k` stays on the first step.
        p.key(Key::Char('j'));
        assert_eq!(p.mode, Mode::Card(3));
        assert!(matches!(
            p.key(Key::Char('x')),
            Step::Write(1, TaskOp::Check { n: 4, done: true }, _)
        ));
        p.key(Key::Char('k'));
        assert_eq!(p.mode, Mode::Card(1));
        // A heading is never ticked, even with the keyboard put on it.
        p.mode = Mode::Card(0);
        assert_eq!(p.key(Key::Char('x')), Step::Stay);
        // Steps rewritten with a heading where the keyboard stood: on to the next step.
        p.mode = Mode::Card(1);
        let mut card = p.card.clone().unwrap();
        card.steps.insert(1, step(9, "## Draft", false));
        p.keep_card(card, false);
        assert_eq!(p.mode, Mode::Card(2));
    }

    /// A step written above the first heading is the first phase's, in the dialog as in
    /// `phase_of`: the first rule goes over it.
    #[test]
    fn a_step_above_the_first_heading_sits_under_phase_one() {
        let step = |n: i64, text: &str| StepInfo {
            n,
            text: text.into(),
            done_at: None,
        };
        let p = open_with(
            (120, 40),
            vec![
                step(1, "loose first"),
                step(2, "## Plan"),
                step(3, "write it"),
            ],
            Vec::new(),
        );
        assert_eq!(p.mode, Mode::Card(0));
        let text = drawn(&p, 120, 40);
        let rule = text.find("Phase 1 of 1: Plan").expect("the rule");
        assert!(rule < text.find("loose first").unwrap(), "{text}");
        assert!(text.find("loose first").unwrap() < text.find("write it").unwrap());
    }
}

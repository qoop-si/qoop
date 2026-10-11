//! `qoop task board --page`: the board as a Jira board, on one page.
//!
//! Three lanes, Todo, Doing and Done, of issue cards (the space on the card's top edge, the
//! title, the key, the steps ticked of all, the agent on it and its stage). `h j k l` and the
//! arrows move, enter opens the issue (Description, Checklist, Activity on the left, Details
//! on the right; `x` ticks a step), `c` adds an issue to Todo, `H` and `L` move one a lane
//! (decision 6: Todo to Doing asks qass to start it, Doing to Done closes it verified, Doing
//! to Todo takes its agents off it, Done to Todo reopens it), `/` filters, `t` tells qass a
//! line. Read and written through the daemon, as `qoop task` is; read again each time the
//! daemon says a task changed, and every half minute besides.

use std::io::{IsTerminal, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qoop_runtime::control::{
    ControlRequest, ControlResponse, EventMessage, ItemCardInfo, ItemInfo, TaskOp, TaskOutcome,
    WorkspaceRow,
};
use qoop_runtime::{client, paths};
use qoop_tui::theme::{self, Role};

use crate::page::{self, Key, Screen, Session, bold, ink, raised, raised_bold};
use crate::qass::{self, Life};
use crate::qass_page::{ago, wrap};

/// The smallest window the page draws in: three lanes of a card each.
const NEED: (usize, usize) = (qoop_tui::qass_screen::BOARD_MIN_COLS, 14);
/// A card's rows: its top edge with the space, two of title, the foot, its bottom edge.
const CARD_H: usize = 5;
/// Read again after this long with no news, for what changes with time alone (a start that
/// expired, "3 min ago").
const POLL: Duration = Duration::from_secs(30);
/// The lanes, in order, as the daemon names the states.
const LANES: [&str; 3] = ["todo", "doing", "done"];

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
    /// Each lane's first card in view: the wheel moves it, the keyboard and a click bring the
    /// selected card back into view.
    top: [usize; 3],
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// `text` cut to `w` characters, the cut said with an ellipsis.
fn fit(text: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if text.chars().count() <= w {
        return text.to_string();
    }
    let mut out: String = text.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
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

/// The items in their lanes, in the daemon's order, those `filter` hides left out.
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
    out
}

/// What an agent on a card is doing, in words and ink.
fn agent_word(a: &qoop_runtime::control::ItemAgentInfo) -> (String, Role) {
    if a.closed {
        ("finished".into(), Role::Done)
    } else if a.claimed {
        ("claimed done".into(), Role::Done)
    } else if a.stage == "waiting-human" {
        ("waits on you".into(), Role::Blocked)
    } else {
        (a.stage.clone(), Role::Working)
    }
}

/// Who a card says works on it: the first agent still on it, else the last one, else what
/// its lane means.
fn assignee(item: &ItemInfo) -> (String, String, Role) {
    match item
        .agents
        .iter()
        .find(|a| !a.closed)
        // An ended agent says who finished, but not on a reopened item back in Todo.
        .or(item.agents.last().filter(|_| item.state != "todo"))
    {
        Some(a) => {
            let (word, role) = agent_word(a);
            (a.name.clone(), word, role)
        }
        None => match (item.state.as_str(), item.outcome.as_deref()) {
            ("doing", _) => (String::new(), "qass will start it".into(), Role::Brand),
            ("done", Some("failed")) => (String::new(), "failed".into(), Role::Failed),
            ("done", _) => (String::new(), "closed".into(), Role::Done),
            _ => (String::new(), "no agent yet".into(), Role::Faint),
        },
    }
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
            top: [0; 3],
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

    /// How many cards fit in a lane's rows at the size last drawn; none while the page is too
    /// small to draw lanes at all.
    fn room(&self) -> usize {
        let (w, h) = self.size;
        if w < NEED.0 || h < NEED.1 {
            return 0;
        }
        (h - 4 - 3) / CARD_H
    }

    /// Scroll the keyboard's lane the least that brings the selected card into view.
    fn follow(&mut self) {
        let room = self.room();
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
        let (w, h) = self.size;
        if self.mode != Mode::Board || w < NEED.0 || h < NEED.1 || row < 2 || row >= h - 2 {
            return;
        }
        let Some(n) = self.lane_at(col) else {
            return;
        };
        let last = self.lanes()[n].len().saturating_sub(self.room());
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
            } if self.mode == Mode::Board => {
                let (col, row) = (col as usize, row as usize);
                self.wandered = false;
                // A Done card's cross puts it away (or back): never also a press that holds it.
                if let Some(id) = self.cross_at(col, row) {
                    self.held = None;
                    let item = self
                        .items
                        .as_ref()
                        .and_then(|v| v.iter().find(|i| i.id == id));
                    return item.map_or(Step::Stay, |item| self.dismissed(item));
                }
                if self.on_hidden_line(col, row) {
                    self.held = None;
                    self.reveal = !self.reveal;
                    self.settle();
                    return Step::Stay;
                }
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
        let at = row.checked_sub(top + 1)? / CARD_H;
        (at < room)
            .then(|| lanes[n].get(first + at))
            .flatten()
            .map(|item| {
                let place = (1 + n * (lw + 2) + 1, top + 1 + at * CARD_H, lw - 2);
                (n, item.id, place)
            })
    }

    /// Whether a cell is on the card held, where it was drawn when pressed.
    fn on_held(&self, col: usize, row: usize) -> bool {
        self.held.is_some()
            && self
                .held_at
                .is_some_and(|(x, y, w)| col >= x && col < x + w && row >= y && row < y + CARD_H)
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
        let room = (h - 3) / CARD_H;
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
            Key::Left | Key::Char('h') => self.side(-1),
            Key::Right | Key::Char('l') => self.side(1),
            Key::Up | Key::Char('k') => self.step(-1),
            Key::Down | Key::Char('j') => self.step(1),
            Key::Enter => {
                if let Some(item) = self.selected() {
                    return Step::Open(item.id);
                }
            }
            Key::Char(c @ ('H' | 'L')) => {
                if let Some(item) = self.selected() {
                    return self.moved(item, c == 'L');
                }
            }
            Key::Char('x') => {
                if let Some(item) = self.selected() {
                    return self.dismissed(item);
                }
            }
            Key::Char('X') => self.reveal = !self.reveal,
            Key::Char('/') => self.mode = Mode::Filter,
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
        match key {
            Key::Esc | Key::Char('q') => {
                self.mode = Mode::Board;
                self.card = None;
            }
            Key::Up | Key::Char('k') => self.mode = Mode::Card(at.saturating_sub(1)),
            Key::Down | Key::Char('j') => {
                self.mode = Mode::Card((at + 1).min(steps.saturating_sub(1)))
            }
            Key::Char('x') | Key::Char(' ') => {
                if let Some(step) = card.steps.get(at) {
                    let done = step.done_at.is_none();
                    let said = format!(
                        "{} step {} {}",
                        card.item.key,
                        step.n,
                        if done { "ticked" } else { "unticked" }
                    );
                    return Step::Write(card.item.id, TaskOp::Check { n: step.n, done }, said);
                }
            }
            Key::Char(c @ ('H' | 'L')) => return self.moved(&card.item.clone(), c == 'L'),
            Key::Char('t') => self.mode = Mode::Tell(String::new()),
            _ => {}
        }
        Step::Stay
    }

    fn draw(&self, w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        let x = s.put(1, 0, "board", &bold(Role::Title));
        s.put(x + 2, 0, "your tasks, run by qass", &ink(Role::Faint));
        let search = match (&self.mode, self.filter.is_empty()) {
            (Mode::Filter, _) => format!("/ {}▌", self.filter),
            (_, true) => "/ search".to_string(),
            (_, false) => format!("/ {}", self.filter),
        };
        let search = fit(&search, 24);
        let sx = w.saturating_sub(search.chars().count() + 6);
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
        // Three lanes, two columns apart.
        let lw = (w - 2 - 4) / 3;
        let (top, bottom) = (2, h - 2);
        for (n, lane) in lanes.iter().enumerate() {
            let lx = 1 + n * (lw + 2);
            self.lane_box(&mut s, n, lane, lx, top, lw, bottom - top);
        }
        // The issue over the lanes, edge on edge with them: one box, no lane border beside it.
        if let Mode::Card(at) = self.mode
            && let Some(card) = &self.card
        {
            self.dialog(&mut s, card, at, (1, top), (3 * lw + 4, bottom - top));
        }
        self.foot(&mut s);
        s
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
        let (name, role) = [
            ("TODO", Role::Text),
            ("DOING", Role::Working),
            ("DONE", Role::Done),
        ][n];
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &rule);
        s.seg(
            x + 2,
            y,
            &[
                (" ", rule.clone()),
                (name, bold(role)),
                (&format!(" {} ", lane.len()), ink(Role::Faint)),
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
            let on = n == self.lane && Some(item.id) == self.sel && self.mode != Mode::Filter;
            self.card(s, item, x + 1, cy, w - 2, on);
            cy += CARD_H;
        }
        if let Mode::Create { title, space } = &self.mode
            && n == 0
        {
            let cy =
                (y + 1 + lane.len().min(room) * CARD_H).min(y + 1 + (room.max(1) - 1) * CARD_H);
            self.new_card(s, title, *space, x + 1, cy, w - 2);
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
        s.put(x + 2, y + h - 2, &fit(&foot, w - 4), &ink(Role::Faint));
    }

    /// One card: the space on its top edge, two lines of title, then key, steps, assignee.
    fn card(&self, s: &mut Screen, item: &ItemInfo, x: usize, y: usize, w: usize, on: bool) {
        let edge = if on {
            ink(Role::Brand)
        } else {
            ink(Role::Rule)
        };
        let space = space_name(&self.spaces, item.workspace);
        let space_ink = item.workspace.map_or(ink(Role::Faint), |id| {
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
                Role::Faint
            };
            s.put(x + w - 2, y, "x", &ink(cross));
        }
        for row in y + 1..y + CARD_H - 1 {
            s.put(x, row, "│", &edge);
            s.put(x + w - 1, row, "│", &edge);
        }
        s.put(
            x,
            y + CARD_H - 1,
            &format!("└{}┘", "─".repeat(w - 2)),
            &edge,
        );
        let inner = w - 4;
        let title = if on {
            bold(Role::Focused)
        } else {
            ink(Role::Text)
        };
        let lines = wrap(&item.title, inner);
        for (n, line) in lines.iter().take(2).enumerate() {
            let line = if n == 1 && lines.len() > 2 {
                fit(&format!("{line} {}", lines[2]), inner)
            } else {
                fit(line, inner)
            };
            s.put(x + 2, y + 1 + n, &line, &title);
        }
        // The foot: the key, the assignee's state flush right, the steps between them, and
        // the assignee's name before its state while there is room; each cut to what is left.
        let (fy, end) = (y + 3, x + w - 2);
        // The state gets the room the key leaves, and at least a third of the row: it is what a
        // person reads.
        let (who, word, role) = assignee(item);
        let inner = end - x - 2;
        let word = fit(
            &word,
            inner
                .saturating_sub(item.key.chars().count() + 2)
                .max(inner / 3),
        );
        let wx = end - word.chars().count();
        s.put(wx, fy, &word, &ink(role));
        let kx = s.put(
            x + 2,
            fy,
            &fit(&item.key, wx.saturating_sub(x + 3)),
            &ink(Role::Faint),
        );
        let (steps, steps_role) = match (item.steps_total, item.steps_done) {
            (0, _) => ("no steps yet".to_string(), Role::Faint),
            (all, done) if done == all => (format!("{done}/{all}"), Role::Done),
            (all, done) => (format!("{done}/{all}"), Role::Detail),
        };
        let steps = fit(&steps, wx.saturating_sub(kx + 3));
        let sx = s.put(kx + 2, fy, &steps, &ink(steps_role));
        let who = fit(&who, wx.saturating_sub(sx + 3));
        if who.chars().count() >= 4 {
            s.put(wx - 1 - who.chars().count(), fy, &who, &ink(Role::Text));
        }
    }

    /// The card being typed: its title with the cursor, then the space it goes to.
    fn new_card(&self, s: &mut Screen, title: &str, space: usize, x: usize, y: usize, w: usize) {
        let edge = ink(Role::Brand);
        for row in y..y + CARD_H {
            s.put(x, row, &" ".repeat(w), theme::RESET);
        }
        s.put(x, y, &format!("┌{}┐", "─".repeat(w - 2)), &edge);
        for row in y + 1..y + CARD_H - 1 {
            s.put(x, row, "│", &edge);
            s.put(x + w - 1, row, "│", &edge);
        }
        s.put(
            x,
            y + CARD_H - 1,
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
        match self.targets().get(space) {
            Some(target) => {
                let nx = s.put(
                    x + 2,
                    y + 3,
                    &fit(&target.name, w / 2),
                    &ink(theme::space_in(&self.spaces, target.id)),
                );
                s.put(nx + 2, y + 3, "tab space", &ink(Role::Faint));
            }
            None => {
                s.put(x + 2, y + 3, "no space to add to", &ink(Role::Blocked));
            }
        }
    }

    /// The issue: Description, Checklist and Activity on the left, Details on the right, in
    /// the box at `(x, y)` of `(w, h)`.
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
        let space = space_name(&self.spaces, item.workspace);
        let space_ink = item.workspace.map_or(ink(Role::Faint), |id| {
            ink(theme::space_in(&self.spaces, id))
        });
        let lx = x + 3;
        let split = x + w * 62 / 100;
        let lw = split - lx - 2;
        let last = y + h - 2;
        s.seg(
            lx,
            y + 1,
            &[
                (&space, space_ink.clone()),
                (" / ", ink(Role::Faint)),
                (&item.key, ink(Role::Faint)),
            ],
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
        let mut ly = y + 3;
        let line = |s: &mut Screen, ly: &mut usize, text: &str, esc: &str| {
            if *ly <= last {
                s.put(lx, *ly, &fit(text, lw), esc);
            }
            *ly += 1;
        };
        for t in wrap(&item.title, lw) {
            line(s, &mut ly, &t, &bold(Role::Focused));
        }
        ly += 1;
        line(s, &mut ly, "Description", &bold(Role::Faint));
        if card.description.trim().is_empty() {
            line(s, &mut ly, "none yet", &ink(Role::Faint));
        }
        for para in card.description.lines() {
            for t in wrap(para, lw) {
                line(s, &mut ly, &t, &ink(Role::Text));
            }
        }
        ly += 1;
        if ly <= last {
            s.seg(
                lx,
                ly,
                &[
                    ("Checklist", bold(Role::Faint)),
                    (
                        &format!("  {}/{}", item.steps_done, item.steps_total),
                        ink(Role::Faint),
                    ),
                ],
            );
        }
        ly += 1;
        if card.steps.is_empty() {
            line(s, &mut ly, "no steps yet", &ink(Role::Faint));
        }
        let going = card.steps.iter().position(|st| st.done_at.is_none());
        for (n, step) in card.steps.iter().enumerate() {
            if ly > last {
                break;
            }
            let on = n == at;
            if on {
                s.put(lx - 1, ly, &" ".repeat(lw + 1), &raised(Role::Detail));
                s.put(lx - 2, ly, "▸", &ink(Role::Brand));
            }
            let paint = |role: Role| if on { raised_bold(role) } else { ink(role) };
            let mark = if step.done_at.is_some() {
                "[x] "
            } else {
                "[ ] "
            };
            let mut sx = s.put(lx, ly, mark, &paint(Role::Detail));
            let room = lw.saturating_sub(4 + if going == Some(n) { 13 } else { 0 });
            let role = if step.done_at.is_some() {
                Role::Faint
            } else {
                Role::Text
            };
            sx = s.put(sx, ly, &fit(&step.text, room), &paint(role));
            if going == Some(n) {
                s.put(sx + 2, ly, "in progress", &paint(Role::Working));
            }
            ly += 1;
        }
        ly += 1;
        line(s, &mut ly, "Activity", &bold(Role::Faint));
        for e in &card.activity {
            if ly > last {
                break;
            }
            let when = fit(&ago(Some(e.at), self.now), 11);
            s.put(lx, ly, &when, &ink(Role::Faint));
            let what = activity_words(&e.kind, &e.body);
            s.put(lx + 12, ly, &fit(&what, lw - 12), &ink(Role::Text));
            ly += 1;
        }
        // Details.
        for row in y + 3..=last {
            s.put(split, row, "│", &ink(Role::Rule));
        }
        let dx = split + 3;
        let dw = x + w - 2 - dx;
        s.put(dx, y + 3, "Details", &bold(Role::Faint));
        let state_role = match item.state.as_str() {
            "doing" => Role::Working,
            "done" => Role::Done,
            _ => Role::Text,
        };
        let mut dy = y + 5;
        let row = |s: &mut Screen, dy: &mut usize, label: &str, value: &str, esc: String| {
            if *dy <= last {
                s.put(dx, *dy, label, &ink(Role::Faint));
                s.put(dx + 11, *dy, &fit(value, dw.saturating_sub(11)), &esc);
            }
            *dy += 1;
        };
        let state = match &item.outcome {
            Some(o) if item.state == "done" => format!("DONE, {o}"),
            _ => item.state.to_uppercase(),
        };
        row(s, &mut dy, "Status", &state, ink(state_role));
        dy += 1;
        if item.agents.is_empty() {
            row(s, &mut dy, "Assignees", "none yet", ink(Role::Faint));
        }
        for (n, a) in item.agents.iter().enumerate() {
            let label = if n == 0 { "Assignees" } else { "" };
            row(s, &mut dy, label, &a.name, ink(Role::Text));
            let (word, role) = agent_word(a);
            let place = match a.terminal {
                Some(t) => format!("{}  T{t}", a.machine),
                None => a.machine.clone(),
            };
            if dy <= last {
                let px = s.put(dx + 11, dy, &fit(&place, dw / 2), &ink(Role::Faint));
                s.put(
                    px + 2,
                    dy,
                    &fit(&word, dw.saturating_sub(px + 2 - dx)),
                    &ink(role),
                );
            }
            dy += 1;
        }
        dy += 1;
        row(s, &mut dy, "Space", &space, space_ink);
        row(s, &mut dy, "Key", &item.key, ink(Role::Text));
        row(
            s,
            &mut dy,
            "Updated",
            &ago(Some(item.updated_at), self.now),
            ink(Role::Text),
        );
        let run_by = match self.qass.0 {
            Some(t) => format!("qass  T{t}"),
            None => "qass".to_string(),
        };
        row(s, &mut dy, "Run by", &run_by, ink(Role::Text));
        dy += 1;
        if dy <= last {
            s.seg(
                dx,
                dy,
                &[
                    ("[ ", ink(Role::Brand)),
                    ("H L", bold(Role::Text)),
                    (" change status ]", ink(Role::Detail)),
                ],
            );
        }
    }

    /// The two rows under the lanes: qass and what the page last said, then the keys.
    fn foot(&self, s: &mut Screen) {
        let y = s.h - 2;
        let mut x = s.put(1, y, "qass", &bold(Role::Text));
        if let Some(t) = self.qass.0 {
            x = s.put(x + 1, y, &format!("T{t}"), &ink(Role::Faint));
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
                let tx = s.put(x, y, "tell qass: ", &ink(Role::Brand));
                let cx = s.put(tx, y, &shown, &bold(Role::Focused));
                s.put(cx, y, "▌", &ink(Role::Brand));
            }
            _ => {
                if let Some(said) = &self.said {
                    s.put(x, y, &fit(said, room), &ink(Role::Text));
                }
            }
        }
        let keys: &[(&str, &str)] = match &self.mode {
            Mode::Board => &[
                ("h j k l", "select"),
                ("enter", "open"),
                ("c", "create"),
                ("/", "filter"),
                ("H L", "move"),
                ("x", "dismiss done"),
                ("t", "tell qass"),
                ("q", "close"),
            ],
            Mode::Card(_) => &[
                ("j k", "step"),
                ("x", "tick step"),
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
        other => other,
    };
    if body.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {body}")
    }
}

/// The bottom row: the page's name filled in the brand ink, then each key and what it does
/// between ember brackets.
fn chips(s: &mut Screen, keys: &[(&str, &str)]) {
    let y = s.h - 1;
    let mut x = s.put(1, y, " board ", &theme::filled(Role::Brand)) + 2;
    for (key, what) in keys {
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

/// The board, or `None` from a daemon older than it.
fn read_board() -> Result<Option<Vec<ItemInfo>>, String> {
    let request = ControlRequest::TaskRead {
        id: crate::request_id("board-page"),
        board: true,
        task: None,
        terminal: None,
        token: None,
        before: None,
    };
    match crate::task::ask(request, Duration::from_secs(10)) {
        Ok(ControlResponse::Tasks { items, .. }) => Ok(items),
        Ok(ControlResponse::Failed { reason, .. }) => Err(reason),
        Ok(other) => Err(format!("unexpected answer: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// One item's card.
fn read_card(item: i64) -> Result<ItemCardInfo, String> {
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
            item: Some(card), ..
        }) => Ok(*card),
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
    crate::settings::apply_theme();
    let mut page = Page::new();
    let (tx, news) = mpsc::channel();
    watch(tx);
    let session = Session::start()?;
    let _mouse = MouseOn::start()?;
    let mut polled: Option<Instant> = None;
    let mut last = String::new();
    'run: loop {
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
        let screen = page.draw(w.max(NEED.0), h.max(NEED.1));
        // Written only when it changed: a page that repaints every tick is never idle to the
        // daemon, and its tab would never sleep. Ages are left out of the comparison (drawn
        // as of the epoch, every one "just now"): "3 min ago" turning "4 min ago" on an open
        // card is no news, and repainting for it alone would keep the tab awake.
        let now = std::mem::replace(&mut page.now, 0);
        let ageless = page.draw(w.max(NEED.0), h.max(NEED.1)).frame();
        page.now = now;
        let frame = format!("{w}x{h}{ageless}");
        if frame != last {
            page::show(&screen, NEED)?;
            last = frame;
        }
        for key in page::next_keys(&session.typed) {
            match page.key(key) {
                Step::Stay => {}
                Step::Leave => break 'run,
                Step::Open(item) => match read_card(item) {
                    Ok(card) => {
                        page.card = Some(card);
                        page.mode = Mode::Card(0);
                    }
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
        Ok(items) => page.items = items,
        Err(why) => page.said = Some(why),
    }
    // The open card, while its dialog is up.
    if let (Mode::Card(at), Some(card)) = (page.mode.clone(), &page.card) {
        match read_card(card.item.id) {
            Ok(card) => {
                page.mode = Mode::Card(at.min(card.steps.len().saturating_sub(1)));
                page.card = Some(card);
            }
            Err(why) => page.said = Some(why),
        }
    }
    page.settle();
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::control::{ItemAgentInfo, StepInfo};

    fn item(id: i64, state: &str, title: &str, workspace: i64) -> ItemInfo {
        ItemInfo {
            id,
            key: format!("QOOP-{id}"),
            workspace: Some(workspace),
            title: title.into(),
            state: state.into(),
            outcome: (state == "done").then(|| "verified".into()),
            steps_done: 0,
            steps_total: 0,
            agents: Vec::new(),
            updated_at: 0,
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

    fn page() -> Page {
        let mut p = Page::new();
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
        p.size = (120, 4 + 3 + 6 * CARD_H);
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
        p.size = (120, 4 + 3 + 6 * CARD_H);
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
            ids(&lanes(&items, &p.spaces, "app", false)),
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
        assert_eq!(assignee(&i).1, "qass will start it");
        i.agents.push(ItemAgentInfo {
            task: 9,
            terminal: Some(4),
            name: "worker".into(),
            machine: "this Mac".into(),
            stage: "waiting-human".into(),
            claimed: false,
            closed: false,
        });
        assert_eq!(
            assignee(&i),
            ("worker".into(), "waits on you".into(), Role::Blocked)
        );
        assert_eq!(assignee(&item(2, "todo", "t", 1)).1, "no agent yet");
        // Its agent ended: in Doing it says finished; reopened back in Todo, nobody yet.
        i.agents[0].closed = true;
        assert_eq!(assignee(&i).1, agent_word(&i.agents[0]).0);
        i.state = "todo".into();
        assert_eq!(assignee(&i).1, "no agent yet");
    }

    #[test]
    fn the_board_draws_its_lanes_cards_and_keys_at_the_smallest_window_and_a_wide_one() {
        let p = page();
        for (w, h) in [NEED, (200, 50)] {
            let text = drawn(&p, w, h);
            for want in ["TODO", "DOING", "DONE", "QOOP-1", "qass", "select"] {
                assert!(text.contains(want), "{want} at {w}x{h}: {text}");
            }
        }
        let text = drawn(&p, 200, 50);
        for want in [
            "board everywhere",
            "no agent yet",
            "qass will start it",
            "closed",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        // A card's foot stays inside its card in the narrowest lane: the key and the state,
        // the rest cut.
        let mut narrow = page();
        narrow.items.as_mut().unwrap()[0].key = "DEFAULT-12".into();
        let (w, h) = NEED;
        let text: Vec<char> = drawn(&narrow, w, h).chars().collect();
        let lw = (w - 6) / 3;
        let (right, foot) = (2 + lw - 2 - 1, 2 + 1 + 3);
        assert_eq!(text[foot * w + right], '│');
        let row: String = text[foot * w..foot * w + right].iter().collect();
        assert!(row.contains("DEFAUL") && row.contains(" no"), "{row}");
        let mut old = page();
        old.items = None;
        assert!(drawn(&old, 80, 20).contains("older than the board"));
    }
}

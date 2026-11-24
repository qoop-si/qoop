//! The Board, the Inbox's second view: what is on the board of This Mac or of another computer,
//! in four columns (two when the page, the window less the sidebar, is under [`WIDE`] wide): Waiting on you (the same records as Needs you),
//! Doing, Todo and Done. A search over the items, Add item, and a line to tell qass what to work
//! on next.
//!
//! The bands and the words under each item are the shared ones (`qoop_tui::board`), so the
//! TUI's Board and this one list the same items in the same order. A board is read through the
//! mux: another computer's by `Via`, its answer in the interface's numbers, and a computer
//! whose qoop is from before the board says so in words.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, Role, Window, div, prelude::*, px,
};
use qoop_runtime::control::{ControlRequest, ControlResponse, ItemInfo, TaskOp};
use qoop_tui::board::{Band, Board, Read, bands_of, too_old, why};
use qoop_tui::inbox::Inbox;

use crate::app::Desk;
use crate::field::Field;
use crate::link;
use crate::state;
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

/// The page's width (the window less the sidebar, as dragged) from which the board has four
/// columns; under it, two. The plan's 1200 wide window less the sidebar's default 280.
pub const WIDE: f32 = 920.;

/// A write nobody answered is given up after this many seconds.
const WRITE_WAIT: i64 = 30;

/// A board read again this often while it shows: another computer's board has no event.
const REREAD: i64 = 30;

/// The columns a page `width` wide gives the board.
pub fn columns(width: f32) -> usize {
    if width >= WIDE { 4 } else { 2 }
}

/// What the board shows of `items` for the words in `query` (every word is somewhere in an
/// item's key or title, in any case); every item for no words.
pub fn found(items: &[ItemInfo], query: &str) -> Vec<ItemInfo> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    items
        .iter()
        .filter(|i| {
            let text = format!("{} {}", i.key, i.title).to_lowercase();
            words.iter().all(|w| text.contains(w))
        })
        .cloned()
        .collect()
}

/// A band's title as the page says it.
fn title(band: Band) -> &'static str {
    match band {
        Band::Wait => "Waiting on you",
        Band::Doing => "Doing",
        Band::Todo => "Todo",
        Band::Done => "Done",
    }
}

pub use qoop_tui::board::read_id;

/// The computer a read id names.
fn machine_of_read(id: &str) -> Option<usize> {
    if id == qoop_tui::board::READ_ID {
        return Some(0);
    }
    id.strip_prefix(qoop_tui::board::READ_ID)?
        .strip_prefix('-')?
        .parse()
        .ok()
}

/// The item being added.
pub struct AddItem {
    pub field: Entity<Field>,
    /// The space it goes to, on the computer shown.
    pub space: Option<i64>,
}

/// The Board page's own state.
pub struct BoardPage {
    /// The Inbox shows the Board instead of Needs you.
    pub open: bool,
    /// Each computer's board, as last read.
    pub boards: BTreeMap<usize, Board>,
    /// The computer shown.
    pub machine: usize,
    pub find: Entity<Field>,
    pub tell: Entity<Field>,
    pub add: Option<AddItem>,
    /// One focus handle per item that leads somewhere, kept from frame to frame.
    pub(crate) item_focus: std::cell::RefCell<HashMap<(usize, i64), FocusHandle>>,
    /// What an add or the tell line said last.
    pub said: Option<String>,
    /// Computers whose last read failed while an earlier board is kept: why.
    pub kept: BTreeMap<usize, String>,
    /// Writes out: the id, and the computer it went to.
    writes: HashMap<String, Write>,
    serial: u64,
    /// When a read was last sent, for the clock's re-read.
    pub read_at: i64,
}

impl BoardPage {
    pub fn new(cx: &mut Context<Desk>) -> Self {
        let find = cx.new(|cx| {
            let mut f = Field::new("Find an item", cx);
            f.label = Some("Find an item".into());
            f
        });
        let tell = cx.new(|cx| {
            let mut f = Field::new("Tell qass what to work on next", cx);
            f.label = Some("Tell qass".into());
            f
        });
        cx.observe(&find, |_, _, cx| cx.notify()).detach();
        cx.observe(&tell, |_, _, cx| cx.notify()).detach();
        BoardPage {
            open: false,
            boards: BTreeMap::new(),
            machine: 0,
            find,
            tell,
            add: None,
            item_focus: Default::default(),
            said: None,
            kept: BTreeMap::new(),
            writes: HashMap::new(),
            serial: 0,
            read_at: 0,
        }
    }

    /// Which board this is, as read: the computer's, or unread.
    pub fn read_of(&self, machine: usize) -> Read {
        self.boards
            .get(&machine)
            .map_or(Read::Unread, |b| b.read.clone())
    }

    /// An answer that is a board read or a board write's. True when it was.
    pub fn answered(&mut self, answer: &ControlResponse) -> Option<Landed> {
        let id = answer.request_id().to_string();
        if let Some(Write {
            machine, serial, ..
        }) = self.writes.remove(&id)
        {
            // The write's other id (the `Via` it travelled in, or the one inside it) is done too.
            self.writes.retain(|_, w| w.serial != serial);
            return Some(match answer {
                ControlResponse::TaskWritten { key, .. } => Landed::Written(machine, key.clone()),
                ControlResponse::Failed { reason, .. } => Landed::Refused(machine, reason.clone()),
                _ => Landed::Written(machine, None),
            });
        }
        let machine = machine_of_read(&id)?;
        let board = self.boards.entry(machine).or_default();
        let before = board.read.clone();
        if !board.answered_to(&id, answer) {
            return None;
        }
        // A read that failed keeps the board it had: what shows is as of the last read.
        if let (Read::Failed(why), Read::Items(_)) = (&board.read, &before) {
            self.kept.insert(machine, why.clone());
            board.read = before;
        } else {
            self.kept.remove(&machine);
        }
        Some(Landed::Read)
    }
}

/// Where an item of the board leads: its agent's page, or the Inbox card about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    Agent(u32),
    Card(String),
}

/// What opening `item` does: its agent that waits on the person (else its first live agent) when
/// that agent is listed (`listed`: its terminal, renumbered by the mux for another computer);
/// else the open card about it, which only this Mac's cards can name (`local`).
pub fn item_target(
    item: &ItemInfo,
    asks: &[qoop_runtime::control::AskInfo],
    local: bool,
    listed: &dyn Fn(u32) -> bool,
) -> Option<Open> {
    let live = || item.agents.iter().filter(|a| !a.closed);
    let agent = live()
        .find(|a| a.status == "waits")
        .or_else(|| live().next())
        .and_then(|a| a.terminal)
        .and_then(|t| u32::try_from(t).ok())
        .filter(|t| listed(*t));
    if let Some(t) = agent {
        return Some(Open::Agent(t));
    }
    local
        .then(|| {
            asks.iter()
                .find(|a| a.status == "open" && a.item.as_ref().is_some_and(|x| x.id == item.id))
                .map(|a| Open::Card(a.card.clone()))
        })
        .flatten()
}

/// A write out: the computer it went to, the number that ties its ids together, and when.
#[derive(Clone, Copy)]
struct Write {
    machine: usize,
    serial: u64,
    at: i64,
}

impl BoardPage {
    /// Note a write under each id its answer may come under: its own, and the `Via` it travels
    /// in (the mux refuses a write under that one).
    fn wrote(&mut self, ids: &[&str], machine: usize, at: i64) {
        self.serial += 1;
        for id in ids {
            self.writes.insert(
                id.to_string(),
                Write {
                    machine,
                    serial: self.serial,
                    at,
                },
            );
        }
    }

    /// Drop writes nobody answered for [`WRITE_WAIT`] seconds, and say so where "Adding." stood.
    /// True when one was dropped.
    pub fn expire(&mut self, now: i64) -> bool {
        let before = self.writes.len();
        self.writes.retain(|_, w| now - w.at < WRITE_WAIT);
        let dropped = self.writes.len() != before;
        if dropped && self.writes.is_empty() && self.said.as_deref() == Some("Adding.") {
            self.said = Some("The item was not confirmed: look at the board.".into());
        }
        dropped
    }
}

/// What an answer was to.
pub enum Landed {
    Read,
    Written(usize, Option<String>),
    Refused(usize, String),
}

impl Desk {
    /// Show the Board, or Needs you again.
    pub(crate) fn show_board(&mut self, on: bool, cx: &mut Context<Self>) {
        self.board.open = on;
        if on {
            self.ask_board(true);
        }
        cx.notify();
    }

    /// Read the board of the computer shown: at once (`now`), or when one is owed.
    pub(crate) fn ask_board(&mut self, now: bool) {
        self.ask_board_of(self.board.machine, now);
    }

    /// The board of computer `m`, read now or when one is owed.
    fn ask_board_of(&mut self, m: usize, now: bool) {
        let via = state::machines(&self.model.spaces)
            .into_iter()
            .find(|(n, _, _)| *n == m)
            .and_then(|(_, _, via)| via);
        if m != 0 && via.is_none() {
            return;
        }
        let board = self.board.boards.entry(m).or_default();
        if now {
            board.stale = true;
            // A read that never came back (its link dropped) is asked again.
            if board.read == Read::Reading {
                board.read = Read::Unread;
            }
        }
        if let Some(request) = board.request_via(&read_id(m), via) {
            self.board.read_at = crate::app::unix_now();
            self.ask(request);
        }
    }

    /// The window is connected again: reads that were out went with the old link.
    pub(crate) fn board_reconnected(&mut self) {
        for board in self.board.boards.values_mut() {
            if board.read == Read::Reading {
                board.read = Read::Unread;
            }
        }
        if self.board.open {
            self.ask_board(true);
        }
    }

    /// Every 30 s while the Board shows: another computer's board has no event, and a read that
    /// was lost is asked for again.
    pub(crate) fn board_tick(&mut self, now: i64) -> bool {
        if self.board.open && now - self.board.read_at >= REREAD {
            self.ask_board(true);
        }
        self.board.expire(now)
    }

    /// The board changed on This Mac: read again when it shows.
    pub(crate) fn board_changed(&mut self) {
        if let Some(board) = self.board.boards.get_mut(&0) {
            board.stale = true;
        }
        if self.board.open && self.board.machine == 0 {
            self.ask_board(false);
        }
    }

    /// Whether an answer was the Board's: a read, or a write's (an item added, or refused).
    pub(crate) fn board_answered(
        &mut self,
        answer: &ControlResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(landed) = self.board.answered(answer) else {
            return false;
        };
        match landed {
            Landed::Read => {}
            Landed::Written(machine, key) => {
                self.board.said = Some(match key {
                    Some(key) => format!("Added {key}."),
                    None => "Done.".to_string(),
                });
                // The computer shown stays the one the person is on; the one written to is read
                // again so the item shows when they go there.
                self.ask_board_of(machine, true);
            }
            Landed::Refused(machine, reason) => {
                let name = self.machine_label(machine);
                self.board.said = Some(
                    if qoop_runtime::control::too_old_for(&reason, None)
                        || qoop_tui::mux::refused_as_old(&reason)
                    {
                        format!("qoop on {name} is too old to add items: update it.")
                    } else {
                        reason
                    },
                );
            }
        }
        cx.notify();
        true
    }

    /// A computer as a person names it.
    fn machine_label(&self, m: usize) -> String {
        state::machines(&self.model.spaces)
            .into_iter()
            .find(|(n, _, _)| *n == m)
            .map_or_else(|| "this computer".to_string(), |(_, name, _)| name)
    }

    /// The spaces a person works in on the computer shown.
    fn board_spaces(&self) -> Vec<&qoop_runtime::control::WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .filter(|w| {
                w.name != state::QASS_SPACE && state::machine_of(w.id) == self.board.machine
            })
            .collect()
    }

    /// Open the row that adds an item, in the space of the page on screen when it is on this
    /// computer, else the computer's first.
    pub(crate) fn begin_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(add) = &self.board.add {
            let focus = add.field.read(cx).focus.clone();
            window.focus(&focus, cx);
            return;
        }
        let spaces = self.board_spaces();
        let current = match self.back {
            crate::app::Page::Session(t) => self.model.row(t).map(|r| r.workspace),
            _ => None,
        };
        let space = current
            .filter(|id| spaces.iter().any(|w| w.id == *id))
            .or_else(|| spaces.first().map(|w| w.id));
        let field = cx.new(|cx| {
            let mut f = Field::new("What is the item?", cx);
            f.label = Some("New item".into());
            f
        });
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        let focus = field.read(cx).focus.clone();
        window.focus(&focus, cx);
        self.board.add = Some(AddItem { field, space });
        self.board.said = None;
        cx.notify();
    }

    /// The item typed, written to the board of the computer shown: no terminal speaks, so it is
    /// the person's own add, carried by `Via` to another computer.
    pub(crate) fn submit_add(&mut self, cx: &mut Context<Self>) {
        let Some(add) = self.board.add.as_ref() else {
            return;
        };
        let goal = add.field.read(cx).text.trim().to_string();
        let Some(space) = add.space else {
            self.board.said = Some("Pick a space.".into());
            cx.notify();
            return;
        };
        if goal.is_empty() {
            return;
        }
        let m = self.board.machine;
        let id = link::fresh_id("item");
        let write = ControlRequest::TaskWrite {
            id: id.clone(),
            terminal: None,
            token: None,
            task: None,
            action: TaskOp::Add {
                workspace: Some(space),
                goal,
                description: None,
            },
        };
        let request = match m {
            0 => write,
            _ => ControlRequest::Via {
                id: link::fresh_id("via"),
                workspace: space,
                request: Box::new(write),
            },
        };
        let outer = request.request_id().to_string();
        self.board
            .wrote(&[id.as_str(), outer.as_str()], m, crate::app::unix_now());
        self.ask(request);
        self.board.add = None;
        self.board.said = Some("Adding.".into());
        cx.notify();
    }

    /// Enter and Escape on the Board's fields, before the page's own keys.
    pub(crate) fn board_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.board.open || self.page != crate::app::Page::Inbox {
            return false;
        }
        let on = |f: &Entity<Field>, window: &Window, cx: &Context<Self>| {
            f.read(cx).focus.is_focused(window)
        };
        let adding = self
            .board
            .add
            .as_ref()
            .is_some_and(|a| on(&a.field, window, cx));
        match key {
            "enter" if adding => self.submit_add(cx),
            "escape" if adding => {
                self.board.add = None;
                window.focus(&self.focus, cx);
                cx.notify();
            }
            "enter" if on(&self.board.tell.clone(), window, cx) => self.send_tell(window, cx),
            _ => return false,
        }
        true
    }

    /// The line for qass, said: its field is emptied once qass has it, kept when it has not.
    fn send_tell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.board.tell.read(cx).text.trim().to_string();
        if !text.is_empty() && self.tell_qass(text, window, cx) {
            self.board.tell.update(cx, |f, _| f.set_text(""));
        }
    }

    /// An item of the board opened: its agent's page, or the Inbox on the card about it.
    pub(crate) fn open_item(&mut self, open: &Open, window: &mut Window, cx: &mut Context<Self>) {
        match open {
            Open::Agent(t) => self.open(crate::app::Page::Session(*t), window, cx),
            Open::Card(id) => {
                self.board.open = false;
                self.inbox_filter = crate::inbox::Filter::All;
                self.set_cursor(state::Slot::Card(id.clone()), 0);
                self.open(crate::app::Page::Inbox, window, cx);
            }
        }
    }

    /// The Board page: the toolbar, the computer, the four columns, and the line for qass.
    pub fn board_page(&self, list: &Inbox, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let m = self.board.machine;
        let name = self.machine_label(m);
        let read = self.board.read_of(m);
        let handed: BTreeSet<u32> = list.with_qass.iter().copied().collect();
        let query = self.board.find.read(cx).text.clone();
        let state_m = self.machine_state(m);
        let lost = m != 0 && state::unreached(state_m) || m == 0 && self.lost_at.is_some();
        let kept = self.board.kept.get(&m).cloned();
        let cols = columns(self.win_w - self.side_px());
        let body: AnyElement = match &read {
            Read::Unread | Read::Reading => div()
                .flex()
                .flex_col()
                .children((0..4).map(|n| crate::inbox::skeleton(n, t)))
                .into_any_element(),
            Read::Old => note(&too_old(&name), t.t2, "board-old"),
            Read::Failed(why) => note(
                &format!("The board of {name} could not be read: {why}"),
                t.fail_text,
                "board-failed",
            ),
            Read::Items(items) if items.iter().all(|i| i.hidden) => {
                self.board_empty(&name, t, cx).into_any_element()
            }
            Read::Items(items) => {
                let shown = found(items, &query);
                let bands = bands_of(&shown, &handed);
                let band = |b: Band, cx: &mut Context<Self>| self.band(b, bands.of(b), t, cx);
                let rows: Vec<Vec<Band>> = Band::ALL.chunks(cols).map(<[Band]>::to_vec).collect();
                div()
                    .id("board-columns")
                    .debug_selector(move || format!("board-columns-{cols}"))
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .children(rows.into_iter().map(|row| {
                        div()
                            .flex()
                            .items_start()
                            .gap(px(18.))
                            .children(row.into_iter().map(|b| band(b, cx).into_any_element()))
                    }))
                    .when(shown.is_empty() && !query.trim().is_empty(), |d| {
                        d.child(note("No item matches.", t.t2, "board-no-match"))
                    })
                    .into_any_element()
            }
        };
        div()
            .id("board-page")
            .debug_selector(|| "board-page".to_string())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.board_toolbar(&name, t, cx))
            .when_some(self.board.add.as_ref(), |d, add| {
                d.child(self.add_row(add, t, cx))
            })
            .when(lost || kept.is_some(), |d| {
                let words = if lost {
                    state::reach_words(state_m, self.zone)
                } else {
                    "its last read failed".to_string()
                };
                d.child(
                    div()
                        .id("board-lost")
                        .debug_selector(|| "board-lost".to_string())
                        .role(Role::Status)
                        .px(px(26.))
                        .pb(px(8.))
                        .text_size(px(12.5))
                        .text_color(c(t.needs_text))
                        .whitespace_normal()
                        .child(format!(
                            "The board of {name} is not read now ({words}); what shows is as of the last read."
                        )),
                )
            })
            .when_some(self.board.said.clone(), |d, said| {
                d.child(
                    div()
                        .id("board-said")
                        .role(Role::Status)
                        .aria_label(said.clone())
                        .px(px(26.))
                        .pb(px(8.))
                        .text_size(px(12.5))
                        .text_color(c(t.t1))
                        .whitespace_normal()
                        .child(said),
                )
            })
            .child(ui::scroll(
                div()
                    .id("board-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().px(px(26.)).pb(px(14.)).child(body)),
            ))
            .child(self.tell_line(t, cx))
            .into_any_element()
    }

    /// The computer switcher, the search over the items, and Add item.
    fn board_toolbar(&self, name: &str, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let machines = state::machines(&self.model.spaces);
        div()
            .id("board-toolbar")
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(26.))
            .pb(px(10.))
            .child(
                div()
                    .id("board-machines")
                    .role(Role::RadioGroup)
                    .aria_label("Computer")
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .children(machines.into_iter().map(|(m, label, _)| {
                        let on = m == self.board.machine;
                        let said = if self.machine_state(m) == state::MachineState::Live {
                            label.clone()
                        } else {
                            format!(
                                "{label}, {}",
                                state::reach_words(self.machine_state(m), self.zone)
                            )
                        };
                        ui::chip(("board-machine", m as u64), &label, None, None, on, t)
                            .aria_label(said)
                            .debug_selector(move || format!("board-machine-{m}"))
                            .child(ui::icon(Icon::Server, 12., c(t.t2)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.board.machine = m;
                                this.board.add = None;
                                this.ask_board(true);
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .id("board-find")
                    .debug_selector(|| "board-find".to_string())
                    .flex_1()
                    .min_w(px(180.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::icon(Icon::Search, 14., c(t.t2)))
                    .child(div().flex_1().min_w_0().child(self.board.find.clone())),
            )
            .child(
                ui::pill("board-add", &format!("Add item to {name}"), t)
                    .debug_selector(|| "board-add".to_string())
                    .child(ui::icon(Icon::Plus, 14., c(t.t1)))
                    .child("Add item")
                    .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx))),
            )
            .into_any_element()
    }

    /// The row that adds an item: its words, its space, Add and Cancel.
    fn add_row(&self, add: &AddItem, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let spaces = self.board_spaces();
        let space = add.space.and_then(|id| self.model.space(id));
        let many = spaces.len() > 1;
        let next = {
            let ids: Vec<i64> = spaces.iter().map(|w| w.id).collect();
            let at = ids
                .iter()
                .position(|id| Some(*id) == add.space)
                .unwrap_or(0);
            ids.get((at + 1) % ids.len().max(1)).copied()
        };
        div()
            .id("board-add-row")
            .debug_selector(|| "board-add-row".to_string())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(26.))
            .pb(px(10.))
            .child(div().flex_1().min_w(px(220.)).child(add.field.clone()))
            .child(match space {
                Some(w) => ui::pill(
                    "board-add-space",
                    &format!("Space, {}", state::short_space(w)),
                    t,
                )
                .child(
                    div()
                        .text_color(t.space(state::space_slot(w)))
                        .child(state::short_space(w)),
                )
                .when(many, |d| d.child(ui::icon(Icon::Chevron, 11., c(t.t3))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let (Some(add), true) = (this.board.add.as_mut(), many) {
                        add.space = next;
                    }
                    cx.notify();
                }))
                .into_any_element(),
                None => div()
                    .text_size(px(12.5))
                    .text_color(c(t.t2))
                    .child("This computer has no space yet.")
                    .into_any_element(),
            })
            .child(
                ui::pill("board-add-go", "Add", t)
                    .debug_selector(|| "board-add-go".to_string())
                    .child("Add")
                    .on_click(cx.listener(|this, _, _, cx| this.submit_add(cx))),
            )
            .child(
                ui::pill("board-add-cancel", "Cancel", t)
                    .child("Cancel")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.board.add = None;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// One column: its title and count, what the Needs you list holds for the first, and its
    /// items.
    fn band(
        &self,
        band: Band,
        items: &[ItemInfo],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let waits = band == Band::Wait;
        let id = match band {
            Band::Wait => "band-wait",
            Band::Doing => "band-doing",
            Band::Todo => "band-todo",
            Band::Done => "band-done",
        };
        div()
            .id(id)
            .role(Role::Group)
            .aria_label(format!("{}, {}", title(band), items.len()))
            .debug_selector(move || id.to_string())
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title(band)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child(items.len().to_string()),
                    ),
            )
            .when(waits, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .whitespace_normal()
                        .child(qoop_tui::board::WAIT_NOTE),
                )
            })
            .when(items.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(c(t.t3))
                        .child("Nothing here"),
                )
            })
            .children(items.iter().map(|i| {
                let key = i.key.clone();
                let target = item_target(i, &self.asks, self.board.machine == 0, &|t| {
                    self.model.row(t).is_some()
                });
                match target {
                    // Leads somewhere: a control, reached by Tab and opened with Enter or Space.
                    Some(open) => {
                        let handle = self
                            .board
                            .item_focus
                            .borrow_mut()
                            .entry((self.board.machine, i.id))
                            .or_insert_with(|| cx.focus_handle().tab_stop(true))
                            .clone();
                        item_inside(
                            ui::control(
                                ("item", i.id as u64),
                                Role::Button,
                                format!("{}, {}, open it", i.key, i.title),
                                t,
                            )
                            .track_focus(&handle)
                            .hover(|s| ui::paint(s, t.hover()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_item(&open, window, cx)
                            }))
                            .debug_selector(move || format!("item-{key}")),
                            i,
                            waits,
                            band,
                            t,
                        )
                        .into_any_element()
                    }
                    None => item_inside(
                        div()
                            .id(("item", i.id as u64))
                            .debug_selector(move || format!("item-{key}"))
                            .role(Role::ListItem)
                            .aria_label(format!("{}, {}", i.key, i.title)),
                        i,
                        waits,
                        band,
                        t,
                    )
                    .into_any_element(),
                }
            }))
    }

    /// No items on the computer: said, with the step.
    fn board_empty(&self, name: &str, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("board-empty")
            .debug_selector(|| "board-empty".to_string())
            .role(Role::Group)
            .aria_label(format!("No items on {name}"))
            .py(px(20.))
            .flex()
            .flex_col()
            .items_start()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("No items on {name}")),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(c(t.t2))
                    .child("Add one, or tell qass what to work on."),
            )
            .child(
                ui::pill("board-empty-add", "Add item", t)
                    .child(ui::icon(Icon::Plus, 14., c(t.t1)))
                    .child("Add item")
                    .on_click(cx.listener(|this, _, window, cx| this.begin_add(window, cx))),
            )
    }

    /// The line at the foot: what to work on next, said to qass.
    fn tell_line(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let off = self.model.qass_row().is_none();
        div()
            .id("board-tell")
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(26.))
            .py(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(240.))
                    .child(self.board.tell.clone()),
            )
            .child(
                ui::pill("board-tell-send", "Tell qass", t)
                    .debug_selector(|| "board-tell-send".to_string())
                    .child("Tell qass")
                    .on_click(cx.listener(|this, _, window, cx| this.send_tell(window, cx))),
            )
            .when_some(
                self.ask_said.clone().or_else(|| {
                    off.then(|| "qass is not running: start it from its page.".to_string())
                }),
                |d, said| {
                    d.child(
                        div()
                            .id("tell-said")
                            .role(Role::Status)
                            .aria_label(said.clone())
                            .w_full()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .whitespace_normal()
                            .child(said),
                    )
                },
            )
            .into_any_element()
    }
}

/// An item's three lines in `d`, a plain row or a control: its key, its title whole, and why it
/// waits or where it stands.
fn item_inside<D: gpui::Styled + gpui::ParentElement>(
    d: D,
    i: &ItemInfo,
    waits: bool,
    band: Band,
    t: &Theme,
) -> D {
    let d = d
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(8.))
        .py(px(7.))
        .rounded(px(10.));
    let d = if waits { d.bg(ca(t.needs_bg)) } else { d };
    d.child(
        div()
            .font_family(MONO_FONT)
            .text_size(px(11.5))
            .text_color(c(t.t3))
            .child(i.key.clone()),
    )
    .child(
        div()
            .font_weight(FontWeight::MEDIUM)
            .whitespace_normal()
            .child(i.title.clone()),
    )
    .child(
        div()
            .text_size(px(12.))
            .text_color(c(if waits { t.needs_text } else { t.t2 }))
            .whitespace_normal()
            .child(why(i, band)),
    )
}

/// A line of words in the body's place.
fn note(text: &str, ink: u32, id: &'static str) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .role(Role::Status)
        .aria_label(text.to_string())
        .py(px(16.))
        .text_size(px(13.))
        .text_color(c(ink))
        .whitespace_normal()
        .child(text.to_string())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, title: &str) -> ItemInfo {
        serde_json::from_value(serde_json::json!({
            "id": 1, "key": key, "workspace": null, "title": title,
            "state": "todo", "steps_done": 0, "steps_total": 0, "agents": [],
            "updated_at": 1,
        }))
        .expect("item")
    }

    /// Four columns from a page 920 wide, two below it.
    #[test]
    fn the_board_has_four_columns_from_920_and_two_below() {
        assert_eq!(columns(1320.), 4);
        assert_eq!(columns(920.), 4);
        assert_eq!(columns(919.), 2);
        assert_eq!(columns(744.), 2);
    }

    /// A board is read under an id that names its computer, and the answer is told apart by it.
    #[test]
    fn a_read_id_names_its_computer() {
        assert_eq!(read_id(0), "board-read");
        assert_eq!(machine_of_read(&read_id(0)), Some(0));
        assert_eq!(machine_of_read(&read_id(3)), Some(3));
        assert_eq!(machine_of_read("board-read-x"), None);
        assert_eq!(machine_of_read("asks-1"), None);
    }

    /// The search takes every word, in any case, over the key and the title.
    #[test]
    fn the_search_takes_every_word_of_the_key_or_the_title() {
        let items = vec![
            item("QOOP-71", "qoop UI redesign and desktop input"),
            item("QOOP-72", "Phone: the agents list back as the home"),
        ];
        assert_eq!(found(&items, "").len(), 2);
        assert_eq!(found(&items, "DESKTOP ui").len(), 1);
        assert_eq!(found(&items, "qoop-72 phone")[0].key, "QOOP-72");
        assert!(found(&items, "desktop phone").is_empty());
    }
}

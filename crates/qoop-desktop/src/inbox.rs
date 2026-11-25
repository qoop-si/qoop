//! The Inbox page, as mail: on the left, everything that needs the owner as one list, one row
//! per open loop, oldest first, from the shared rules (`qoop_tui::inbox`), then "Not waiting on
//! you" and the quiet sections (With qass, Finished, Answered today); on the right, the picked
//! item's card whole. Digits answer the card in the reader, `u` undoes, a pick waits its 3 s in
//! the hold bar at the foot of the page. The list holds still under the pointer or the keys.
//!
//! The page is a page like any other: the session open before it keeps its terminal alive
//! behind it, and `ctrl-a i` or Escape goes back to it.

use std::collections::HashMap;

use gpui::{AnyElement, Context, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{AgentRow, AskInfo, ControlRequest, TerminalStream};
use qoop_tui::inbox::{Context as Around, Inbox, Item, ItemKind};

use crate::app::{Desk, Page};
use crate::link;
use crate::state::{self, Kind, Pick, Slot};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

/// The most lines of Finished and Answered today drawn.
const QUIET_MOST: usize = 20;

/// The chips over the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    FromQass,
    Failed,
}

impl Filter {
    fn holds(self, item: &Item) -> bool {
        match self {
            Filter::All => true,
            Filter::FromQass => item.kind.from_qass(),
            Filter::Failed => item.kind == ItemKind::Failed,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::FromQass => "From qass",
            Filter::Failed => "Failed",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Filter::All => "chip-all",
            Filter::FromQass => "chip-qass",
            Filter::Failed => "chip-failed",
        }
    }
}

/// What the Inbox list draws: its items (the chip's), then "Not waiting on you". Worked out
/// from the live lists, or kept whole while the list is held (as the column's rows are).
/// ponytail: what is drawn while held can be up to one hover old; a dimmed row does nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListDrawn {
    pub items: Vec<Slot>,
    pub others: Vec<u32>,
    /// Each row's agent as drawn ("space title"): a row that left keeps it, dimmed.
    pub names: HashMap<Slot, String>,
    pub titles: HashMap<u32, String>,
}

impl ListDrawn {
    /// Every agent the list draws, an item's or a "Not waiting on you" row's.
    fn terminals(&self) -> Vec<u32> {
        self.items
            .iter()
            .filter_map(|s| match s {
                Slot::Agent(t) => Some(*t),
                Slot::Card(_) => None,
            })
            .chain(self.others.iter().copied())
            .collect()
    }
}

impl Desk {
    /// The agents "Not waiting on you" lists: every agent by the Priority order, less the
    /// ones an item names and the ones a quiet section lists (With qass, Finished), so no
    /// agent is on the page twice.
    pub(crate) fn not_waiting(&self, list: &Inbox) -> Vec<u32> {
        let named: Vec<u32> = list
            .items
            .iter()
            .filter_map(|i| i.terminal)
            .chain(list.with_qass.iter().copied())
            .chain(list.finished.iter().copied())
            .collect();
        self.model
            .priority()
            .iter()
            .map(|e| e.terminal())
            .filter(|t| !named.contains(t))
            .collect()
    }

    /// What the list draws now from the live lists.
    pub(crate) fn live_list(&self, list: &Inbox) -> ListDrawn {
        let items = self.shown(list);
        let others = self.not_waiting(list);
        let named = |t: u32| {
            self.model
                .row(t)
                .map_or_else(|| format!("tab {t}"), |r| self.who_text(r))
        };
        let names = items
            .iter()
            .map(|i| (i.slot.clone(), self.item_who(i)))
            .collect();
        let agents = self.model.agents();
        let titles = others
            .iter()
            .map(|t| {
                let title = agents
                    .iter()
                    .find(|e| e.terminal() == *t)
                    .map_or_else(|| named(*t), |e| self.entry_name(*e));
                (*t, title)
            })
            .collect();
        ListDrawn {
            items: items.iter().map(|i| i.slot.clone()).collect(),
            others,
            names,
            titles,
        }
    }

    /// Whose an item is, as a row names it: its agent's space and title, or qass.
    pub(crate) fn item_who(&self, item: &Item) -> String {
        item.terminal.map_or_else(
            || "qass".to_string(),
            |t| {
                self.model
                    .row(t)
                    .map_or_else(|| format!("tab {t}"), |r| self.who_text(r))
            },
        )
    }

    /// An agent as a row of the page names it: its space and its title.
    fn entry_name(&self, entry: state::Entry<'_>) -> String {
        match self.model.space(entry.workspace()) {
            Some(w) => format!("{} {}", state::short_space(w), entry.title()),
            None => entry.title(),
        }
    }

    /// A held item row that left: its agent as the list drew it, and the word it says now (the
    /// agent's state, "Closed" once it is gone; a card about no agent, "Answered").
    pub(crate) fn left_item(&self, drawn: &ListDrawn, slot: &Slot) -> (String, String) {
        let name = drawn
            .names
            .get(slot)
            .cloned()
            .unwrap_or_else(|| slot_name(slot));
        let word = match slot {
            Slot::Agent(t) => {
                let agents = self.model.agents();
                self.left_word(agents.iter().find(|e| e.terminal() == *t).copied())
            }
            Slot::Card(_) => "Answered".to_string(),
        };
        (name, word)
    }

    /// What the list draws: its held rows while held, else the live ones.
    pub(crate) fn drawn_list(&self, list: &Inbox) -> ListDrawn {
        self.inbox_held
            .clone()
            .unwrap_or_else(|| self.live_list(list))
    }

    /// Before each frame on the Inbox: hold the list while the pointer is on it or the keys
    /// drive inside it (as the column holds), let it go otherwise.
    pub(crate) fn update_inbox_hold(&mut self, list: &Inbox, window: &Window, cx: &gpui::App) {
        if self.page != Page::Inbox {
            // Left with the pointer on the list (a click on a row that opens an agent): its
            // hover ends with the page, which says nothing when it is drawn again.
            self.inbox_hovered = false;
            self.inbox_held = None;
            return;
        }
        let keys =
            window.last_input_was_keyboard() && self.inbox_list_focus.contains_focused(window, cx);
        if !self.listed || !(self.inbox_hovered || keys) {
            self.inbox_held = None;
            return;
        }
        if self.inbox_held.is_none() {
            self.inbox_held = Some(self.live_list(list));
        }
    }

    /// Before each frame: when the item the reader showed has left (answered here after its
    /// hold, or elsewhere), the next one by the order the list drew (the first later row still
    /// waiting, else the one before) takes the reader and the keys; none left, the list takes
    /// the keys. The keys move only when they were on the card that left, in the list (a row
    /// clicked), or fell to the window.
    pub(crate) fn follow_reader(
        &mut self,
        list: &Inbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Page::Inbox {
            self.reader_shown = None;
            self.reader_order.clear();
            return;
        }
        let items = self.shown(list);
        // The pick's place in the live list, kept in step as items above it leave.
        if let Some(at) = self
            .inbox_cursor
            .as_ref()
            .and_then(|s| items.iter().position(|i| &i.slot == s))
        {
            self.cursor_at = at;
        }
        if let Some(was) = self.reader_shown.clone()
            && !items.iter().any(|i| i.slot == was)
        {
            let order = &self.reader_order;
            let next = order.iter().position(|s| *s == was).and_then(|p| {
                order[p + 1..]
                    .iter()
                    .chain(order[..p].iter().rev())
                    .find_map(|s| {
                        items
                            .iter()
                            .position(|i| &i.slot == s)
                            .map(|at| (s.clone(), at))
                    })
            });
            match next {
                Some((slot, at)) => self.set_cursor(slot, at),
                None => self.inbox_cursor = None,
            }
            let on_it = self
                .card_focus
                .borrow()
                .get(&was)
                .is_some_and(|h| h.contains_focused(window, cx))
                || self.inbox_list_focus.contains_focused(window, cx);
            if on_it || self.focus.is_focused(window) {
                let to = match self.cursor_slot(&items) {
                    Some(slot) => self.card_handle(&slot, cx),
                    None => self.inbox_list_focus.clone(),
                };
                window.focus(&to, cx);
            }
        }
        self.reader_shown = self.cursor_slot(&items);
        self.reader_order = self.drawn_list(list).items;
    }

    /// A card's focus handle, made before it is drawn: the keys can go to a card the reader
    /// is about to show.
    fn card_handle(&self, slot: &Slot, cx: &mut Context<Self>) -> gpui::FocusHandle {
        self.card_focus
            .borrow_mut()
            .entry(slot.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Up and Down on the list: the pick moves to the drawn row above or below, over the rows
    /// that left (dimmed while held); the reader follows. The keys stay on the list.
    fn move_pick(&mut self, down: bool, cx: &mut Context<Self>) {
        let list = self.inbox();
        let items = self.shown(&list);
        let drawn = self.drawn_list(&list).items;
        let live = |slot: &Slot| items.iter().position(|i| &i.slot == slot);
        let here = self
            .cursor_slot(&items)
            .and_then(|s| drawn.iter().position(|d| *d == s));
        let order: Vec<usize> = if down {
            (here.map_or(0, |h| h + 1)..drawn.len()).collect()
        } else {
            (0..here.unwrap_or(0)).rev().collect()
        };
        if let Some((slot, at)) = order
            .into_iter()
            .find_map(|n| live(&drawn[n]).map(|at| (drawn[n].clone(), at)))
        {
            self.set_cursor(slot, at);
            cx.notify();
        }
    }

    /// Everything the Inbox lists now: one list, one count, from the rows and every card read.
    pub(crate) fn inbox(&self) -> Inbox {
        Inbox::build(
            &self.model.rows,
            &self.cards(),
            &self.model.hand,
            &self.inbox_around(),
            &self.own_answers,
        )
    }

    /// What the Inbox needs around the rows: when the day began, the machines not reached (by
    /// their link, or by not answering for a while), the machines whose qoop sends no menu.
    fn inbox_around(&self) -> Around {
        let mut around = qoop_tui::inbox_pane::context(
            &self.model.spaces,
            self.day_start,
            qoop_tui::mux::machines_without_menus(),
        );
        for (m, _, _) in state::machines(&self.model.spaces) {
            if m != 0 && state::unreached(self.machine_state(m)) {
                around.lost.insert(m);
            }
        }
        around
    }

    /// Whether the chip shows `item`.
    pub(crate) fn inbox_filter_holds(&self, item: &Item) -> bool {
        self.inbox_filter.holds(item)
    }

    /// The slot of the card with the ring.
    pub(crate) fn cursor_slot(&self, items: &[&Item]) -> Option<Slot> {
        self.cursor_in(items).map(|at| items[at].slot.clone())
    }

    /// The items the chip shows.
    fn shown<'a>(&self, list: &'a Inbox) -> Vec<&'a Item> {
        list.items
            .iter()
            .filter(|i| self.inbox_filter.holds(i))
            .collect()
    }

    /// The ring follows the keyboard: when focus is on a card or inside one (Tab into its
    /// options), that card has the ring, so a digit answers the card the person is on.
    pub(crate) fn follow_card_focus(&mut self, list: &Inbox, window: &Window, cx: &gpui::App) {
        if self.page != Page::Inbox {
            return;
        }
        let on = self
            .card_focus
            .borrow()
            .iter()
            .find(|(_, handle)| handle.contains_focused(window, cx))
            .map(|(slot, _)| slot.clone());
        let Some(slot) = on.filter(|slot| self.inbox_cursor.as_ref() != Some(slot)) else {
            return;
        };
        let at = self.shown(list).iter().position(|i| i.slot == slot);
        if let Some(at) = at {
            self.set_cursor(slot, at);
        }
    }

    /// The position of the card with the focus ring: the one picked, else the same place in the
    /// list when it left, never past the end. While the list is held, never an item it has not
    /// drawn: the reader never shows a card whose row is missing.
    fn cursor_in(&self, items: &[&Item]) -> Option<usize> {
        let by_slot = self
            .inbox_cursor
            .as_ref()
            .and_then(|slot| items.iter().position(|i| &i.slot == slot));
        let at =
            by_slot.or_else(|| (!items.is_empty()).then(|| self.cursor_at.min(items.len() - 1)))?;
        let undrawn = self
            .inbox_held
            .as_ref()
            .is_some_and(|held| !held.items.contains(&items[at].slot));
        (!undrawn).then_some(at)
    }

    pub(crate) fn set_cursor(&mut self, slot: Slot, at: usize) {
        self.inbox_cursor = Some(slot);
        self.cursor_at = at;
    }

    /// `ctrl-a tab` and Cmd-J: on the Inbox the ring moves to the next card; anywhere else the
    /// agent that has waited longest opens (the Inbox, for a card that names none).
    pub(crate) fn next_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.inbox();
        if self.page == Page::Inbox {
            let items = self.shown(&list);
            let next = match self.cursor_in(&items) {
                Some(at) => Some((at + 1) % items.len()),
                None => (!items.is_empty()).then_some(0),
            };
            if let Some(at) = next {
                let slot = items[at].slot.clone();
                // He asked for it: an item the held list has not drawn ends the hold, so the
                // reader never shows a card whose row is missing.
                if self
                    .inbox_held
                    .as_ref()
                    .is_some_and(|held| !held.items.contains(&slot))
                {
                    self.inbox_held = None;
                }
                // The card takes the keys too. It is not drawn yet (only the reader's card
                // is): its handle is made now, so the focus is never left on the card before,
                // which would take the pick back on the next frame.
                let handle = self.card_handle(&slot, cx);
                self.set_cursor(slot, at);
                window.focus(&handle, cx);
                cx.notify();
            }
            return;
        }
        let here = match self.page {
            Page::Session(terminal) => terminal,
            _ => 0,
        };
        match list.next_after(here) {
            // The one rule the terminal interface goes by too (`Item::opens_inbox`).
            Some(item) => match item.terminal.filter(|_| !item.opens_inbox()) {
                Some(terminal) => self.open(Page::Session(terminal), window, cx),
                None => {
                    let (slot, at) = (item.slot.clone(), 0);
                    self.open(Page::Inbox, window, cx);
                    self.set_cursor(slot, at);
                }
            },
            None => self.open(Page::Inbox, window, cx),
        }
    }

    /// Mark read: the finish of `terminal` counts as seen, for its stop.
    pub(crate) fn mark_seen(&self, terminal: TerminalStream) {
        if let Some(stop) = self.model.row(terminal).and_then(|r| r.stop.clone()) {
            self.ask(ControlRequest::Seen {
                id: link::fresh_id("seen"),
                terminal,
                stop: Some(stop),
            });
        }
    }

    /// A digit on the Inbox: the answer of that number on the card with the ring, once its menu
    /// is armed. Whether the key was taken.
    pub(crate) fn inbox_digit(
        &mut self,
        digit: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let list = self.inbox();
        let items = self.shown(&list);
        let Some(item) = self.cursor_in(&items).map(|at| items[at].clone()) else {
            return false;
        };
        match (&item.slot, item.kind) {
            (Slot::Agent(terminal), ItemKind::Menu) if !item.lost => {
                let Some(row) = self.model.row(*terminal) else {
                    return false;
                };
                let Some(menu) = row.menu.as_ref() else {
                    return false;
                };
                // A menu that has only just appeared takes no digit: a key meant for the
                // screen before it. Taken, and dropped.
                if !self.holds.armed(
                    *terminal,
                    menu.seal,
                    row.menu_seal,
                    std::time::Instant::now(),
                ) {
                    return true;
                }
                if let Some(offer) = state::offers(menu)
                    .into_iter()
                    .find(|o| o.digit == Some(digit))
                    .filter(|o| o.pick != Pick::Qass || item.hand_to_qass)
                {
                    self.pick(*terminal, &offer, window, cx);
                }
                true
            }
            (Slot::Card(id), ItemKind::Agent | ItemKind::Qass | ItemKind::Plan) if !item.lost => {
                let card = self.asks.iter().find(|a| &a.card == id).cloned();
                if let Some(option) = card
                    .as_ref()
                    .and_then(|c| c.body.options.get(usize::from(digit) - 1).map(|o| (c, o)))
                {
                    let (card, option) = option;
                    let key = option.key.clone();
                    self.pick_card(card, &key, window, cx);
                }
                true
            }
            _ => false,
        }
    }

    /// The agent or card a pick is for, as the hold bar names it.
    pub(crate) fn who_of(&self, slot: &Slot) -> String {
        match slot {
            Slot::Agent(terminal) => self
                .model
                .row(*terminal)
                .map_or_else(|| format!("tab {terminal}"), |r| self.model.agent_name(r)),
            Slot::Card(id) => self
                .asks
                .iter()
                .find(|a| &a.card == id)
                .and_then(|a| {
                    a.terminal
                        .and_then(|t| self.model.row(t))
                        .map(|r| self.model.agent_name(r))
                        .or_else(|| a.agent.clone())
                })
                .unwrap_or_else(|| "qass".to_string()),
        }
    }

    /// What an Inbox item asks, as one line: the agent's question on its menu, the card's ask,
    /// else the agent's title (an agent that waits for words asks nothing in particular).
    pub(crate) fn item_question(&self, item: &Item) -> String {
        let row = item.terminal.and_then(|t| self.model.row(t));
        let asked = match &item.slot {
            Slot::Agent(t) => self
                .model
                .row(*t)
                .and_then(|r| r.menu.as_ref())
                .map(|m| m.question.trim().to_string()),
            Slot::Card(id) => self
                .asks
                .iter()
                .find(|a| &a.card == id)
                .map(|a| a.body.asks.trim().to_string()),
        };
        asked
            .filter(|q| !q.is_empty())
            .or_else(|| row.map(state::title_of))
            .unwrap_or_else(|| "qass asks".to_string())
    }

    /// An agent as the cards say it: its space and its title, as words.
    fn who_text(&self, row: &AgentRow) -> String {
        match self.model.space(row.workspace) {
            Some(w) => format!("{} {}", state::short_space(w), state::title_of(row)),
            None => state::title_of(row),
        }
    }

    /// One agent's name beside its mark in a line of the page: its space, its title, and the
    /// machine when it is not this Mac's.
    fn who_line(&self, row: &AgentRow, t: &Theme) -> AnyElement {
        let space = self.model.space(row.workspace);
        let machine = space
            .filter(|w| state::machine_of(w.id) != 0)
            .map(state::machine_name);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(px(6.))
            .when_some(space, |d, w| {
                d.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.space(state::space_slot(w)))
                        .child(state::short_space(w)),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .child(state::title_of(row)),
            )
            .when_some(machine, |d, machine| {
                d.child(div().text_color(c(t.t2)).child(format!("on {machine}")))
            })
            .into_any_element()
    }

    /// The chips: All, From qass, Failed, with a clear control on the one that filters, and the
    /// keys that answer. The title is the band's.
    fn inbox_chips(&self, list: &Inbox, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let count = |f: Filter| list.items.iter().filter(|i| f.holds(i)).count();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .px(px(26.))
            .pt(px(4.))
            .pb(px(8.))
            .children([Filter::All, Filter::FromQass, Filter::Failed].map(|f| {
                let on = self.inbox_filter == f;
                let clear = on && f != Filter::All;
                ui::chip(f.id(), f.label(), Some(count(f)), None, on, t)
                    .debug_selector(move || f.id().to_string())
                    .aria_label(if clear {
                        format!("Clear filter: {}, {}", f.label(), count(f))
                    } else {
                        format!("{}, {}", f.label(), count(f))
                    })
                    .when(clear, |d| d.child(ui::icon(Icon::Close, 11., c(t.t2))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // The chip that filters is the clear control: pressed again, All.
                        this.inbox_filter = if on { Filter::All } else { f };
                        // A new list: drawn again from the live one.
                        this.inbox_held = None;
                        cx.notify();
                    }))
            }))
            .child(div().flex_1())
            .when(list.count > 0, |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .id("inbox-keys")
                        .aria_label("Command J next, 1 to 9 answer, u undo")
                        .child("\u{2318}J next, 1-9 answer, u undo"),
                )
            })
    }

    /// The Inbox page as mail: on the left the list (the items oldest first, then "Not waiting
    /// on you" and the quiet sections), on the right the picked item's card whole with what
    /// `⌘J` goes to next. The list widens by its divider; a main column too narrow for both
    /// puts the reader under the item rows, above the rest. The hold bar stays at the foot.
    pub fn inbox_page(&self, list: &Inbox, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let items = self.shown(list);
        // Only the reader's card is drawn; every listed item keeps its focus handle, so `⌘J`
        // and a leaving item can give the keys to one not drawn yet.
        let cursor = self.cursor_in(&items);
        let reading = cursor.map(|at| items[at].slot.clone());
        self.card_focus
            .borrow_mut()
            .retain(|slot, _| items.iter().any(|i| &i.slot == slot));
        let first_run = self.listed && self.model.agents().is_empty();
        let stacked = self.inbox_stacked();
        let (rows, rest) = self.inbox_list(list, &items, reading.as_ref(), first_run, t, cx);
        let reader = (!first_run).then(|| self.reader(&items, cursor, t, cx));
        let content = if stacked {
            // One column scrolled as one: the item rows, the reader right under them, then the
            // rest, so a pick is always in sight.
            ui::scroll(
                div()
                    .id("inbox-body")
                    .debug_selector(|| "inbox-body".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(14.))
                            .pb(px(20.))
                            .child(self.list_part("inbox-list", cx).child(rows))
                            .children(reader)
                            .child(self.list_part("inbox-list-rest", cx).child(rest)),
                    ),
            )
            .into_any_element()
        } else {
            div()
                .id("inbox-body")
                .debug_selector(|| "inbox-body".to_string())
                .flex_1()
                .min_h_0()
                .flex()
                .child(ui::scroll(
                    div()
                        .id("inbox-list-scroll")
                        .debug_selector(|| "inbox-list-scroll".to_string())
                        .flex_none()
                        .w(px(self.list_px()))
                        .h_full()
                        .overflow_y_scroll()
                        .child(self.list_part("inbox-list", cx).child(rows).child(rest)),
                ))
                .child(self.divider(crate::shell::Edge::InboxList, "Inbox list width", t, cx))
                .children(reader.map(|reader| {
                    ui::scroll(
                        div()
                            .id("inbox-reader-scroll")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .child(reader),
                    )
                }))
                .into_any_element()
        };
        div()
            .id("inbox-page")
            .debug_selector(|| "inbox-page".to_string())
            .size_full()
            .flex()
            .flex_col()
            .when(list.count > 0 || self.inbox_filter != Filter::All, |d| {
                d.child(self.inbox_chips(list, t, cx))
            })
            .when_some(self.qass_stopped(t, cx), |d, line| d.child(line))
            .child(content)
            .when(!self.holds.held.is_empty(), |d| {
                d.child(self.hold_bar(t, cx))
            })
            .into_any_element()
    }

    /// A part of the list (all of it side by side; stacked, the item rows or the rest): the
    /// pointer on it holds the list.
    fn list_part(&self, id: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.inbox_hovered != *hovered {
                    this.inbox_hovered = *hovered;
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .px(px(14.))
    }

    /// The list in two parts. First "N waiting, oldest first" and the item rows, one list box
    /// whose picked row is its active descendant; then "Not waiting on you" with the other
    /// agents and the quiet sections. Held, the rows stay where they are: a row that left
    /// stays dimmed with its new state word, not a link, and the quiet sections leave out an
    /// agent the held rows draw, so none is on the page twice. New ones are counted.
    fn inbox_list(
        &self,
        list: &Inbox,
        items: &[&Item],
        reading: Option<&Slot>,
        first_run: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let drawn = self.drawn_list(list);
        let live = self.live_list(list);
        let agents = self.model.agents();
        let entry = |terminal: u32| agents.iter().find(|e| e.terminal() == terminal).copied();
        // New is what neither held part draws: an item that left moves its agent to the
        // others, and is not new.
        let fresh = self.inbox_held.as_ref().map_or(0, |held| {
            let shown = held.terminals();
            live.items
                .iter()
                .filter(|s| match s {
                    Slot::Agent(t) => !shown.contains(t),
                    Slot::Card(_) => !held.items.contains(s),
                })
                .count()
                + live.others.iter().filter(|t| !shown.contains(t)).count()
        });
        let quiet = match &self.inbox_held {
            Some(held) => {
                let shown = held.terminals();
                let mut quiet = list.clone();
                quiet.with_qass.retain(|t| !shown.contains(t));
                quiet.finished.retain(|t| !shown.contains(t));
                std::borrow::Cow::Owned(quiet)
            }
            None => std::borrow::Cow::Borrowed(list),
        };
        // No count before the Inbox is read, nor on the first run.
        let count =
            (self.listed && !first_run).then(|| format!("{} waiting, oldest first", items.len()));
        let listbox = div()
            .id("inbox-listbox")
            .debug_selector(|| "inbox-listbox".to_string())
            .role(Role::ListBox)
            .aria_label(
                count
                    .as_ref()
                    .map_or_else(|| "Inbox".to_string(), |n| format!("Inbox, {n}")),
            )
            .track_focus(&self.inbox_list_focus)
            // Up and Down move the pick while the list has the keys; the reader follows.
            .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "up" => this.move_pick(false, cx),
                    "down" => this.move_pick(true, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(drawn.items.iter().map(|slot| {
                if let Some(at) = items.iter().position(|i| &i.slot == slot) {
                    return self.list_row(items[at], at, reading == Some(slot), t, cx);
                }
                let (name, word) = self.left_item(&drawn, slot);
                crate::column::left_row(format!("inbox-row-{}", slot_name(slot)), name, word, t)
            }));
        let rows = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(count.map(|n| {
                div()
                    .id("inbox-count")
                    .debug_selector(|| "inbox-count".to_string())
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(px(12.5))
                    .text_color(c(t.t2))
                    .child(n)
            }))
            .when(!self.listed, |d| d.children((0..3).map(|n| skeleton(n, t))))
            .when_some(
                state::loading_line(&self.model.spaces, &self.model.rows),
                |d, line| {
                    d.child(
                        div()
                            .id("loading-line")
                            .role(Role::Status)
                            .aria_label(line.clone())
                            .px(px(8.))
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child(line),
                    )
                },
            )
            .when(self.listed && items.is_empty(), |d| {
                if first_run || self.inbox_filter != Filter::All {
                    d.child(self.inbox_empty(first_run, t, cx))
                } else {
                    d.child(
                        div()
                            .id("inbox-none")
                            .debug_selector(|| "inbox-none".to_string())
                            .role(Role::Status)
                            .px(px(8.))
                            .py(px(6.))
                            .text_size(px(13.))
                            .text_color(c(t.t2))
                            .child("Nothing waits on you."),
                    )
                }
            })
            .child(listbox)
            .when(fresh > 0, |d| {
                d.child(
                    div()
                        .id("inbox-new")
                        .debug_selector(|| "inbox-new".to_string())
                        .role(Role::Status)
                        .px(px(8.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(format!("{fresh} new, shown when you move away")),
                )
            });
        let others = drawn.others.iter().map(|&o| {
            if live.others.contains(&o) {
                return self.other_row(o, t, cx);
            }
            let title = drawn
                .titles
                .get(&o)
                .cloned()
                .unwrap_or_else(|| format!("tab {o}"));
            crate::column::left_row(
                format!("inbox-other-{o}"),
                title,
                self.left_word(entry(o)),
                t,
            )
        });
        let others: Vec<AnyElement> = others.collect();
        let rest = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pb(px(20.))
            .when(!others.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(8.))
                        .pt(px(14.))
                        .pb(px(4.))
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Not waiting on you"),
                )
                .child(
                    div()
                        .id("inbox-others")
                        .role(Role::List)
                        .aria_label("Not waiting on you")
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(others),
                )
            })
            // The quiet lists belong to the whole Inbox: a filter hides them.
            .when(self.inbox_filter == Filter::All && !first_run, |d| {
                d.child(div().px(px(8.)).child(self.quiet_sections(&quiet, t, cx)))
            });
        (rows.into_any_element(), rest.into_any_element())
    }

    /// One item of the list: its agent's logo and mark, the question, its space and title,
    /// whose it is ("Needs you", "From qass", "Failed") and how long it has waited. A click
    /// puts its card in the reader.
    fn list_row(
        &self,
        item: &Item,
        at: usize,
        picked: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = item.terminal.and_then(|t| self.model.row(t));
        let question = self.item_question(item);
        let space = row.and_then(|r| self.model.space(r.workspace));
        let title = row.map_or_else(|| "qass".to_string(), state::title_of);
        let (whose, ink, kind) = match item.kind {
            ItemKind::Failed => (
                "Failed",
                t.fail_text,
                Kind::Status(qoop_domain::status::Status::Failed),
            ),
            kind if kind.from_qass() => (
                "From qass",
                t.needs_text,
                Kind::Status(qoop_domain::status::Status::Blocked),
            ),
            _ => (
                "Needs you",
                t.needs_text,
                Kind::Status(qoop_domain::status::Status::Blocked),
            ),
        };
        let ago = item.since.map(|at| self.now.saturating_sub(at).max(0));
        let who = match space {
            Some(w) => format!("{} {title}", state::short_space(w)),
            None => title.clone(),
        };
        let label = match ago {
            Some(secs) => format!(
                "{who} asks: {question}, {}, {}",
                whose.to_lowercase(),
                state::spoken_span(secs)
            ),
            None => format!("{who} asks: {question}, {}", whose.to_lowercase()),
        };
        let slot = item.slot.clone();
        let key = slot_name(&slot);
        let selector = format!("inbox-row-{key}");
        ui::control(
            gpui::ElementId::from(selector.clone()),
            Role::ListBoxOption,
            label,
            t,
        )
        .debug_selector(move || selector)
        // One stop for the list; Up and Down walk its rows.
        .tab_stop(false)
        .aria_selected(picked)
        // The list box has the keys, its picked row is what a screen reader says.
        .when(picked, |d| d.aria_active_descendant())
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(8.))
        .py(px(8.))
        .rounded(px(10.))
        .when(picked, |d| ui::current(d, t))
        .hover(|s| ui::paint(s, t.hover()))
        .child(div().pt(px(2.)).child(ui::avatar(
            row.and_then(|r| r.program.as_deref()).or(Some("qass")),
            kind,
            24.,
            false,
            t,
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(t.ink))
                        .child(question),
                )
                .child({
                    let text = match space {
                        Some(w) => {
                            let tag = state::short_space(w);
                            gpui::StyledText::new(format!("{tag} {title}")).with_highlights([(
                                0..tag.len(),
                                gpui::HighlightStyle {
                                    color: Some(t.space(state::space_slot(w))),
                                    font_weight: Some(FontWeight::SEMIBOLD),
                                    ..Default::default()
                                },
                            )])
                        }
                        None => gpui::StyledText::new(title),
                    };
                    div()
                        .whitespace_normal()
                        .text_size(px(12.5))
                        .text_color(c(t.t1))
                        .child(text)
                })
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .child(div().text_color(c(ink)).child(whose))
                        .children(
                            ago.map(|secs| {
                                div().text_color(c(t.t2)).child(state::short_span(secs))
                            }),
                        ),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_cursor(slot.clone(), at);
            cx.notify();
        }))
        .into_any_element()
    }

    /// An agent under "Not waiting on you": its logo and mark, its space and title, its state;
    /// it opens the agent's page. One that left while held stays, dimmed.
    fn other_row(&self, terminal: u32, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let agents = self.model.agents();
        let Some(entry) = agents.iter().find(|e| e.terminal() == terminal).copied() else {
            return div()
                .debug_selector(move || format!("inbox-other-{terminal}"))
                .px(px(8.))
                .py(px(6.))
                .text_size(px(13.))
                .text_color(c(t.t2))
                .child("Closed")
                .into_any_element();
        };
        let kind = self.shown_kind(entry);
        let space = self.model.space(entry.workspace());
        let words = match entry {
            state::Entry::Live(r) => self.shown_word(r),
            state::Entry::Asleep(..) => kind.word().to_string(),
        };
        ui::control(
            ("inbox-other", u64::from(terminal)),
            Role::Link,
            self.model.row_label(entry, self.now),
            t,
        )
        .debug_selector(move || format!("inbox-other-{terminal}"))
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(10.))
        .hover(|s| ui::paint(s, t.hover()))
        .child(ui::avatar(entry.program(), kind, 24., false, t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(13.))
                        .child(match space {
                            Some(w) => format!("{} {}", state::short_space(w), entry.title()),
                            None => entry.title(),
                        }),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(ui::word_ink(kind, t)))
                        .child(words),
                ),
        )
        .on_click(
            cx.listener(move |this, _, window, cx| this.open(Page::Session(terminal), window, cx)),
        )
        .into_any_element()
    }

    /// The reader: the picked item's card whole, and what `⌘J` goes to next; while loading,
    /// a line saying so; with nothing to answer, that.
    fn reader(
        &self,
        items: &[&Item],
        cursor: Option<usize>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let quiet = |id: &'static str, text: &str| {
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .role(Role::Status)
                .aria_label(text.to_string())
                .py(px(10.))
                .text_size(px(13.5))
                .text_color(c(t.t2))
                .child(text.to_string())
                .into_any_element()
        };
        let body = match cursor {
            _ if !self.listed => quiet("inbox-reader-loading", "Loading"),
            None => quiet("inbox-reader-empty", "Nothing to answer."),
            Some(at) => {
                let next = (items.len() > 1).then(|| items[(at + 1) % items.len()]);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(self.item_card(items[at], at, true, t, cx))
                    .when_some(next, |d, next| {
                        let who = next
                            .terminal
                            .and_then(|t| self.model.row(t))
                            .map_or_else(|| "qass".to_string(), |r| self.who_text(r));
                        d.child(
                            div()
                                .id("inbox-next")
                                .debug_selector(|| "inbox-next".to_string())
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(12.5))
                                .text_color(c(t.t2))
                                .child(ui::kbd("\u{2318}J", t))
                                .child(format!("next: {who}")),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .id("inbox-reader")
            .debug_selector(|| "inbox-reader".to_string())
            .px(px(14.))
            .pb(px(20.))
            .max_w(px(900.))
            .child(body)
            .into_any_element()
    }

    /// qass cannot take a question now (not running, or not watching): said on the page with the
    /// step, since "Hand to qass" is then left out of every card.
    fn qass_stopped(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let why = self.model.qass_off().filter(|_| self.listed)?;
        let running = self.model.qass_row().is_some();
        let asleep = self
            .model
            .spaces
            .iter()
            .any(|w| w.name == state::QASS_SPACE && w.asleep);
        let (button, label) = if running {
            ("qass-watch", "Start watching")
        } else {
            ("qass-start", "Start qass")
        };
        let busy = !running && self.qass_starting();
        Some(
            div()
                .id("qass-stopped")
                .debug_selector(|| "qass-stopped".to_string())
                .role(Role::Status)
                .aria_label(format!("{why}, so Hand to qass is not offered"))
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(10.))
                .mx(px(26.))
                .mb(px(8.))
                .max_w(px(888.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(200.))
                        .text_size(px(13.))
                        .text_color(c(t.t1))
                        .whitespace_normal()
                        .child(format!("{why}, so Hand to qass is not offered.")),
                )
                .child(if busy {
                    ui::pill_off(button, label, t)
                        .child(label)
                        .into_any_element()
                } else {
                    ui::pill(button, label, t)
                        .debug_selector(move || button.to_string())
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if running {
                                this.set_watch(true, cx)
                            } else if asleep {
                                this.wake_qass(cx)
                            } else {
                                this.start_qass(cx)
                            }
                        }))
                        .into_any_element()
                })
                .into_any_element(),
        )
    }

    /// Nothing waits (or nothing has started yet): said, with the step.
    fn inbox_empty(&self, first_run: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (title, line) = if first_run {
            ("No agents yet", "\u{2318}N starts one.")
        } else if self.inbox_filter != Filter::All {
            (
                "Nothing here",
                "Nothing in this filter needs you. All shows the rest.",
            )
        } else {
            (
                "Nothing needs you",
                "Questions from your agents and from qass land here: one list, one count.",
            )
        };
        div()
            .id("inbox-empty")
            .role(Role::Group)
            .aria_label(title)
            .py(px(24.))
            .flex()
            .flex_col()
            .items_start()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_size(px(13.)).text_color(c(t.t2)).child(line))
            .child(
                ui::pill("empty-new", "New agent", t)
                    .debug_selector(|| "empty-new".to_string())
                    .child(ui::icon(Icon::Plus, 14., c(t.t1)))
                    .child("New agent")
                    .on_click(cx.listener(|this, _, window, cx| this.new_work(None, window, cx))),
            )
            .into_any_element()
    }

    /// The pick that waits its 3 s, named by its target, with Send now and Undo: pinned under
    /// the list so it is in sight wherever the card is.
    fn hold_bar(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(hold) = self.holds.held.last() else {
            return div().into_any_element();
        };
        let left = hold
            .until
            .saturating_duration_since(std::time::Instant::now())
            .as_secs_f32()
            .ceil()
            .max(1.);
        let who = self.who_of(&hold.slot);
        let others = self.holds.held.len() - 1;
        let slot = hold.slot.clone();
        let again = slot.clone();
        div()
            .id("hold-bar")
            .debug_selector(|| "hold-bar".to_string())
            .role(Role::Status)
            .aria_label(format!(
                "Sending {} to {who} in {}",
                hold.label,
                state::count(left as usize, "second")
            ))
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .mx(px(26.))
            .mb(px(14.))
            .p(px(12.))
            .max_w(px(888.))
            .rounded(px(12.))
            .bg(c(t.pop))
            .border_1()
            .border_color(ca(t.line))
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .whitespace_normal()
                            .child(format!("Sending \"{}\" in {left} s", hold.label)),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .whitespace_normal()
                            .child(match others {
                                0 => format!("to {who}"),
                                n => format!("to {who}, and {n} more waiting"),
                            }),
                    ),
            )
            .child(
                ui::pill("send-now", "Send now", t)
                    .debug_selector(|| "send-now".to_string())
                    .h(px(30.))
                    .child("Send now")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_now(Some(&slot), cx);
                    })),
            )
            .child(
                ui::pill("undo-bar", "Undo", t)
                    .debug_selector(|| "undo-bar".to_string())
                    .h(px(30.))
                    .child("Undo")
                    .child(ui::kbd("u", t))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.undo(Some(&again), cx);
                    })),
            )
            .into_any_element()
    }

    /// One card: who it is from and how long it has waited, what it asks, and its answers. The
    /// card with the ring takes the digits.
    fn item_card(
        &self,
        item: &Item,
        index: usize,
        focused: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = item.terminal.and_then(|terminal| self.model.row(terminal));
        let card = match &item.slot {
            Slot::Card(id) => self.asks.iter().find(|a| &a.card == id),
            Slot::Agent(_) => None,
        };
        let slot = item.slot.clone();
        let handle = self
            .card_focus
            .borrow_mut()
            .entry(item.slot.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        ui::control(
            gpui::SharedString::from(format!("inbox-card-{}", slot_name(&slot))),
            Role::Group,
            self.card_label(item, row, card),
            t,
        )
        .track_focus(&handle)
        .tab_stop(false)
        .cursor_default()
        .aria_selected(focused)
        .debug_selector(move || format!("inbox-card-{}", slot_name(&slot)))
        .relative()
        // A pick just held on this card: the hold bar takes the list's bottom, and the list
        // scrolls the card back into view above it.
        .when(self.reveal_once(&item.slot), |d| {
            d.child(ui::bring_into_view())
        })
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(12.))
        .rounded(px(14.))
        .bg(c(t.card))
        .border_2()
        .border_color(if focused { c(t.focus) } else { ca(t.hair) })
        .on_mouse_down(gpui::MouseButton::Left, {
            let slot = item.slot.clone();
            cx.listener(move |this, _, _, cx| {
                this.set_cursor(slot.clone(), index);
                cx.notify();
            })
        })
        .child(self.source_line(item, row, card, t, cx))
        .child(if item.lost {
            self.lost_line(row, t)
        } else {
            self.card_answers(item, row, card, index, t, cx)
        })
        .into_any_element()
    }

    /// What a screen reader says of a card: its kind and who it is from.
    fn card_label(&self, item: &Item, row: Option<&AgentRow>, card: Option<&AskInfo>) -> String {
        let who = row.map_or_else(
            || {
                card.and_then(|c| c.agent.clone())
                    .unwrap_or_else(|| "qass".into())
            },
            |r| self.model.agent_name(r),
        );
        let said = match item.kind {
            ItemKind::Qass | ItemKind::Plan => "Question from qass",
            ItemKind::Failed => "Failed",
            ItemKind::Reply => "Waiting for your words",
            _ => "Question",
        };
        format!("{said}, {who}")
    }

    /// Who the card is from: the mark, the agent (a link to it), "via qass", and its age.
    fn source_line(
        &self,
        item: &Item,
        row: Option<&AgentRow>,
        card: Option<&AskInfo>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let from_qass = item.kind.from_qass();
        let kind = match row {
            Some(_) if item.lost => Kind::Status(qoop_domain::status::Status::Unknown),
            Some(r) => Kind::of(r),
            None => Kind::Status(qoop_domain::status::Status::Blocked),
        };
        let age = match (item.lost, row, item.since) {
            (true, Some(r), _) => self.as_of(r.workspace),
            (_, _, Some(at)) => Some(state::short_span(self.now.saturating_sub(at).max(0))),
            _ => None,
        };
        let terminal = item.terminal;
        div()
            .flex()
            .items_start()
            .gap(px(8.))
            .text_size(px(13.))
            .child(div().pt(px(3.)).child(if from_qass {
                ui::icon(Icon::Eye, 13., c(t.t1)).into_any_element()
            } else {
                ui::mark(kind, t, 12.).into_any_element()
            }))
            .child(match (row, terminal) {
                (Some(r), Some(terminal)) => ui::control(
                    ("inbox-open", u64::from(terminal)),
                    Role::Link,
                    format!("Open {}", self.model.agent_name(r)),
                    t,
                )
                .debug_selector(move || format!("inbox-open-{terminal}"))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap_x(px(6.))
                .px(px(2.))
                .rounded(px(5.))
                .hover(|s| ui::paint(s, t.hover()))
                .child(self.who_line(r, t))
                .when(from_qass, |d| {
                    d.child(div().text_color(c(t.t2)).child("via qass"))
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open(Page::Session(terminal), window, cx)
                }))
                .into_any_element(),
                _ => div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(
                        card.and_then(|c| c.agent.clone())
                            .unwrap_or_else(|| "qass".to_string()),
                    )
                    .child(div().text_color(c(t.t2)).child(" via qass"))
                    .into_any_element(),
            })
            .when_some(age, |d, age| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(age),
                )
            })
            .into_any_element()
    }

    /// A card whose machine is not reached: it is as of its last answer, and waits for it.
    fn lost_line(&self, row: Option<&AgentRow>, t: &Theme) -> AnyElement {
        let machine = row
            .and_then(|r| self.model.space(r.workspace))
            .map_or_else(|| "its computer".to_string(), state::machine_name);
        div()
            .text_size(px(13.))
            .text_color(c(t.t2))
            .whitespace_normal()
            .child(format!(
                "Waits on {machine}; answering opens again when it is back."
            ))
            .into_any_element()
    }

    /// What the card asks and the answers it offers, by its kind.
    pub(crate) fn card_answers(
        &self,
        item: &Item,
        row: Option<&AgentRow>,
        card: Option<&AskInfo>,
        index: usize,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let said = |slot: &Slot, cx: &mut Context<Self>| self.said_line_in(slot, false, t, cx);
        match (item.kind, row, card) {
            (ItemKind::Menu, Some(row), _) => self.menu_answers(item, row, t, cx),
            (ItemKind::Reply, Some(row), _) => {
                let terminal = row.terminal;
                let own_open = self
                    .own
                    .as_ref()
                    .filter(|o| o.terminal == terminal && o.card.is_none());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        self.note_line(
                            "It ended its turn and waits for your words.".to_string(),
                            t,
                        ),
                    )
                    .child(match own_open {
                        Some(own) => div().id("own-answer").child(own.field.clone()),
                        None => div().id("own-answer").child(
                            ui::pill(("reply-open", u64::from(terminal)), "Reply", t)
                                .self_start()
                                .debug_selector(move || format!("reply-open-{terminal}"))
                                .child(if self.drafts.contains_key(&item.slot) {
                                    "Reply in your own words (your draft is kept)"
                                } else {
                                    "Reply in your own words"
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_own(terminal, None, window, cx)
                                })),
                        ),
                    })
                    .when_some(said(&item.slot, cx), |d, line| d.child(line))
                    .into_any_element()
            }
            (ItemKind::Dialog, Some(row), _) => self.open_only(
                row,
                "It shows a dialog that qoop cannot read here. Open the agent to answer it.",
                t,
                cx,
            ),
            (ItemKind::Open, Some(row), _) => {
                let machine = self
                    .model
                    .space(row.workspace)
                    .map_or_else(|| "that computer".to_string(), state::machine_name);
                self.open_only(
                    row,
                    &format!(
                        "It asks something on its screen. qoop on {machine} cannot show it \
                         here, so open the agent to answer it."
                    ),
                    t,
                    cx,
                )
            }
            (ItemKind::Failed, Some(row), _) => {
                self.open_only(row, "It failed. Open the agent to see why.", t, cx)
            }
            (
                ItemKind::Agent | ItemKind::Qass | ItemKind::Plan | ItemKind::Dialog,
                _,
                Some(card),
            ) => {
                let hand = item.hand_to_qass && self.model.qass_off().is_none();
                self.card_body(index, card, true, hand, t, cx)
            }
            _ => self
                .note_line("This one has gone from the list.".into(), t)
                .into_any_element(),
        }
    }

    fn note_line(&self, text: String, t: &Theme) -> gpui::Div {
        div()
            .text_size(px(13.))
            .text_color(c(t.t1))
            .whitespace_normal()
            .child(text)
    }

    /// A card with only Open the agent (a dialog, an old machine's question, a failure).
    fn open_only(
        &self,
        row: &AgentRow,
        words: &str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal = row.terminal;
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(8.))
            .child(self.note_line(words.to_string(), t))
            .child(
                ui::pill(("open-agent", u64::from(terminal)), "Open the agent", t)
                    .debug_selector(move || format!("open-agent-{terminal}"))
                    .child("Open the agent")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(Page::Session(terminal), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// An agent's own menu: its question and what it is about, its answers with their digits
    /// (Hand to qass only where the daemon takes it), the field for words of one's own.
    fn menu_answers(
        &self,
        item: &Item,
        row: &AgentRow,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let terminal = row.terminal;
        let Some(menu) = row.menu.as_ref() else {
            return div().into_any_element();
        };
        let offers: Vec<state::Offer> = state::offers(menu)
            .into_iter()
            .filter(|o| o.pick != Pick::Qass || item.hand_to_qass)
            .collect();
        let words = state::text_key(menu).is_some() && self.takes_words(row);
        let own_open = self
            .own
            .as_ref()
            .filter(|o| o.terminal == terminal && o.card.is_none());
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(!menu.question.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .whitespace_normal()
                        .child(menu.question.clone()),
                )
            })
            .when(!menu.context.is_empty(), |d| {
                d.child(
                    div()
                        .p(px(8.))
                        .rounded(px(8.))
                        .bg(c(crate::theme::TERMINAL_BG))
                        .font_family(MONO_FONT)
                        .text_size(px(11.5))
                        .text_color(c(crate::theme::DARK.t1))
                        .children(
                            menu.context
                                .iter()
                                .map(|l| div().whitespace_normal().min_h(px(14.)).child(l.clone())),
                        ),
                )
            })
            .when(offers.is_empty(), |d| {
                d.child(self.note_line(
                    "Several boxes to tick: open the agent and answer it there.".into(),
                    t,
                ))
                .child(
                    ui::pill(("open-agent", u64::from(terminal)), "Open the agent", t)
                        .child("Open the agent")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open(Page::Session(terminal), window, cx)
                        })),
                )
            })
            .children(
                offers
                    .iter()
                    .enumerate()
                    .map(|(n, o)| self.offer_button(terminal, o, ("inbox", n), t, cx)),
            )
            .when(words && !offers.is_empty(), |d| match own_open {
                Some(own) => d.child(
                    div()
                        .id("own-answer")
                        .debug_selector(|| "own-field".to_string())
                        .child(own.field.clone()),
                ),
                None => d.child(
                    ui::link_button("own-open", "Something else? Type your own answer", t)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_own(terminal, None, window, cx)
                        })),
                ),
            })
            .when_some(self.said_line_in(&item.slot, false, t, cx), |d, line| {
                d.child(line)
            })
            .into_any_element()
    }

    /// The quiet lists under the cards: questions qass holds, finished work nobody has read,
    /// and what was answered today.
    fn quiet_sections(&self, list: &Inbox, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let head = |title: &str, n: usize| {
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .pt(px(14.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .child(n.to_string()),
                )
                .child(div().flex_1())
        };
        let line = |terminal: u32,
                    words: String,
                    extra: Option<AnyElement>,
                    cx: &mut Context<Self>| {
            let row = self.model.row(terminal);
            let kind = row.map_or(Kind::Status(qoop_domain::status::Status::Unknown), |r| {
                self.shown_kind(state::Entry::Live(r))
            });
            div()
                .flex()
                .items_start()
                .gap(px(4.))
                .child(
                    ui::control(
                        ("quiet-row", u64::from(terminal)),
                        Role::Link,
                        row.map_or_else(
                            || format!("tab {terminal}"),
                            |r| self.model.row_label(state::Entry::Live(r), self.now),
                        ),
                        t,
                    )
                    .debug_selector(move || format!("quiet-row-{terminal}"))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(5.))
                    .rounded(px(10.))
                    .hover(|s| ui::paint(s, t.hover()))
                    .child(div().pt(px(3.)).child(ui::mark(kind, t, 12.)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .whitespace_normal()
                                    .child(row.map_or_else(String::new, |r| self.who_text(r))),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .whitespace_normal()
                                    .child(words),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open(Page::Session(terminal), window, cx)
                    })),
                )
                .when_some(extra, |d, extra| d.child(extra))
                .into_any_element()
        };
        let handed: Vec<AnyElement> = list
            .with_qass
            .iter()
            .map(|&terminal| {
                let words = self
                    .model
                    .row(terminal)
                    .and_then(|r| self.model.qass_words(r))
                    .unwrap_or_else(|| "With qass".into());
                line(terminal, words, None, cx)
            })
            .collect();
        // Every row is listed, or the rest are one "N more" away: never dropped silently.
        let cap = if self.quiet_all {
            usize::MAX
        } else {
            QUIET_MOST
        };
        let hidden =
            list.finished.len().saturating_sub(cap) + list.answered.len().saturating_sub(cap);
        let long = list.finished.len().max(list.answered.len()) > QUIET_MOST;
        let finished: Vec<AnyElement> = list
            .finished
            .iter()
            .take(cap)
            .map(|&terminal| {
                let age = self
                    .model
                    .row(terminal)
                    .and_then(|r| state::age_of(r, self.now))
                    .map_or_else(|| "Done".to_string(), |age| format!("Done {age} ago"));
                let read = self
                    .model
                    .row(terminal)
                    .is_some_and(|r| r.stop.is_some())
                    .then(|| {
                        ui::link_button(("mark-read", u64::from(terminal)), "Mark read", t)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mark_seen(terminal);
                                cx.notify();
                            }))
                            .into_any_element()
                    });
                line(terminal, age, read, cx)
            })
            .collect();
        let unread = list.finished.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .when(!handed.is_empty(), |d| {
                d.child(head("With qass", handed.len())).children(handed)
            })
            .when(!finished.is_empty(), |d| {
                d.child(head("Finished", unread.len()).child(
                    ui::link_button("mark-all-read", "Mark all read", t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            for &terminal in &unread {
                                this.mark_seen(terminal);
                            }
                            cx.notify();
                        },
                    )),
                ))
                .children(finished)
            })
            .when(!list.answered.is_empty(), |d| {
                d.child(head("Answered today", list.answered.len()))
                    .children(list.answered.iter().take(cap).enumerate().map(|(i, a)| {
                        div()
                            .id(("answered", i as u64))
                            .debug_selector(move || format!("answered-{i}"))
                            .flex()
                            .items_start()
                            .gap(px(10.))
                            .px(px(6.))
                            .py(px(4.))
                            .text_size(px(13.))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(MONO_FONT)
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .child(state::clock(a.at, self.zone)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(a.label.clone()),
                                    )
                                    .child(div().text_color(c(t.t2)).child(format!("to {}", a.to))),
                            )
                    }))
            })
            .when(long, |d| {
                d.child(
                    ui::link_button(
                        "quiet-more",
                        &if self.quiet_all {
                            "Show fewer".to_string()
                        } else {
                            format!("{hidden} more")
                        },
                        t,
                    )
                    .debug_selector(|| "quiet-more".to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.quiet_all = !this.quiet_all;
                        cx.notify();
                    })),
                )
            })
            .into_any_element()
    }

    /// A card qass or an agent wrote: what the agent does, why it asks, the options with their
    /// reasons and which qass would pick, as text: nothing is picked for the owner. `own`: the
    /// field for words of one's own; `hand`: Hand to qass beside the options.
    pub(crate) fn card_body(
        &self,
        index: usize,
        card: &AskInfo,
        own: bool,
        hand: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = card.card.clone();
        let body = &card.body;
        let pick = body.pick.as_ref().map(|p| {
            let n = body
                .options
                .iter()
                .position(|o| o.key == p.key)
                .map_or_else(|| p.key.clone(), |i| (i + 1).to_string());
            format!("qass would pick {n}: {}", p.reason)
        });
        let own_field = self
            .own
            .as_ref()
            .filter(|o| o.card.as_deref() == Some(id.as_str()))
            .map(|o| o.field.clone());
        let terminal = card.terminal.unwrap_or_default();
        let put_away = id.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_size(px(13.5))
                    .whitespace_normal()
                    .child(body.asks.clone()),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(c(t.t1))
                    .whitespace_normal()
                    .child(format!("Doing: {}", body.doing)),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(c(t.t1))
                    .whitespace_normal()
                    .child(format!("Why: {}", body.why)),
            )
            .children(body.options.iter().enumerate().map(|(n, o)| {
                let info = card.clone();
                let key = o.key.clone();
                ui::control(
                    ("card-option", ((index as u64) << 8) | n as u64),
                    Role::Button,
                    format!("{}, {}", n + 1, o.label),
                    t,
                )
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(8.))
                .map(|d| ui::paint(d, t.rest()))
                .hover(|s| ui::paint(s, t.pressed()))
                .text_size(px(12.5))
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .text_color(c(t.t2))
                        .child((n + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().whitespace_normal().child(o.label.clone()))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(c(t.t2))
                                .whitespace_normal()
                                .child(o.reason.clone()),
                        ),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick_card(&info, &key, window, cx)),
                )
            }))
            .when(own, |d| {
                let words = id.clone();
                d.child(match own_field {
                    Some(field) => div().id(("card-own", index as u64)).child(field),
                    None => div().id(("card-own", index as u64)).child(
                        ui::link_button(
                            ("card-own-open", index as u64),
                            "Something else? Type your own answer",
                            t,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_own(terminal, Some(words.clone()), window, cx)
                            },
                        )),
                    ),
                })
            })
            // The one qass rule: a card an agent asked goes to This Mac's qass only while it
            // runs and watches. A card qass asked has no such button.
            .when(hand, |d| {
                let qass = id.clone();
                d.child(
                    ui::pill(("card-qass", index as u64), "Hand to qass", t)
                        .self_start()
                        .debug_selector(move || format!("card-qass-{qass}"))
                        .child(ui::icon(Icon::Eye, 14., c(t.t1)))
                        .child("Hand to qass")
                        .on_click({
                            let card = id.clone();
                            cx.listener(move |this, _, window, cx| {
                                this.card_to_qass(&card, window, cx)
                            })
                        }),
                )
            })
            .when_some(
                self.said_line_in(&Slot::Card(id.clone()), false, t, cx),
                |d, line| d.child(line),
            )
            .when_some(pick, |d, p| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .whitespace_normal()
                        .child(p),
                )
            })
            .child(
                ui::link_button(("card-dismiss", index as u64), "Put away", t).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.put_away(&put_away, window, cx);
                    }),
                ),
            )
            .into_any_element()
    }

    /// A card on an agent's own page (`in_tab`), framed and titled by its id, with words of
    /// one's own and Hand to qass beside the options.
    pub(crate) fn card_block(
        &self,
        index: usize,
        card: &AskInfo,
        hand: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = card.card.clone();
        let by = state::short_span(self.now.saturating_sub(card.opened_at).max(0));
        div()
            .debug_selector({
                let id = id.clone();
                move || format!("card-{id}")
            })
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(10.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.needs_line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .child(div().text_color(c(t.needs_text)).child(id))
                    .child(div().text_color(c(t.t2)).child(by)),
            )
            .child(self.card_body(index, card, true, hand, t, cx))
            .into_any_element()
    }
}

/// A card's name for its debug selector.
fn slot_name(slot: &Slot) -> String {
    match slot {
        Slot::Agent(terminal) => format!("t{terminal}"),
        Slot::Card(card) => card.clone(),
    }
}

/// A row's shape while the list is still to come: no words, no spinner.
pub(crate) fn skeleton(n: usize, t: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("skeleton", n as u64))
        .role(Role::Status)
        .aria_label("Loading agents")
        .flex()
        .items_center()
        .gap(px(10.))
        .h(px(46.))
        .px(px(8.))
        .child(div().size(px(12.)).rounded_full().bg(ca(t.fill2)))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().h(px(10.)).w(px(220.)).rounded(px(5.)).bg(ca(t.fill2)))
                .child(div().h(px(8.)).w(px(140.)).rounded(px(4.)).bg(ca(t.fill))),
        )
}

/// Put a card away unanswered: held like an answer, so it can be undone.
impl Desk {
    pub(crate) fn put_away(&mut self, card: &str, window: &mut Window, cx: &mut Context<Self>) {
        let pick = Pick::PutAway {
            card: card.to_string(),
        };
        self.hold_card(card, pick, "Put away".into(), window, cx);
    }
}

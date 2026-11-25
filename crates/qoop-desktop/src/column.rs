//! The column's lists under its head: the small Inbox that folds under the Inbox row, the
//! Agents strip (Spaces or Priority, and a new space), the state filter, and the spaces with
//! their agents, newest first.
//!
//! The hold: while the pointer is over the column, or the keyboard drives inside it, the rows
//! drawn stay where they are. Only the person's own changes (a fold, a rename, a new space, an
//! agent they started) go into the held rows at once; everything else waits for the hold to
//! end, and new rows are counted at the foot instead of drawn.
//!
//! A space's heading folds it, renames it in place (a click on its name), starts an agent in
//! it (`+`) and opens its menu (`...`). New space opens a field under the strip; an empty name
//! takes the numbered name `ctrl-a w` would give (`qoop_tui::run::space_name_after`).

use std::collections::HashMap;

use gpui::{
    AnyElement, Context, ElementId, Entity, FocusHandle, FontWeight, Role, Transformation, Window,
    deferred, div, percentage, prelude::*, px,
};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_tui::inbox::Inbox;

use crate::app::{Desk, Page};
use crate::field::Field;
use crate::link;
use crate::state::{self, Entry, Kind, SideView, Slot, StateTab};
use crate::theme::{Theme, c, ca};
use crate::ui::{self, Icon};

/// A write to the spaces nobody answered is given up after this many seconds (the Board's).
const SPACE_WAIT: i64 = 30;

/// One block of the agent list: a space and its agents (Spaces), or every agent (Priority).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub space: Option<i64>,
    /// The space's name as its heading draws it.
    pub name: String,
    pub open: bool,
    /// Its agents on the filter's list, in the order drawn.
    pub rows: Vec<u32>,
}

/// What the column's lists draw: worked out from the live lists, or kept whole while held.
/// ponytail: what is drawn while held can be up to one hover old, and a click on a dimmed row
/// does nothing; a row's own state (its mark, age and word) is always the live one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Drawn {
    /// The small Inbox's rows.
    pub inbox: Vec<Slot>,
    pub blocks: Vec<Block>,
    /// Each agent row's title as drawn: one renamed elsewhere keeps it while held.
    pub titles: HashMap<u32, String>,
}

impl Drawn {
    fn has_row(&self, terminal: u32) -> bool {
        self.blocks.iter().any(|b| b.rows.contains(&terminal))
    }
}

/// What a space field writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditFor {
    Rename(i64),
    New,
}

/// The rename field in a space's heading, or the new-space field under the strip.
pub struct SpaceEdit {
    pub target: EditFor,
    pub field: Entity<Field>,
    /// "Renaming", a refusal's reason, no answer: one line under the field.
    pub said: Option<String>,
    /// A write of it was answered: it closes once the spaces are read again.
    pub done: bool,
}

/// A space heading's menu, open, and the item the keys are on.
pub struct SpaceMenu {
    pub space: i64,
    pub at: usize,
}

/// The menu's items, in order.
const MENU: [&str; 4] = ["New agent in", "Rename", "New space", "Fold"];

impl Desk {
    /// The new spaces asked for and not yet listed: the writes out, and the ones made whose
    /// space the list does not show yet (between `WorkspaceCreated` and the spaces' answer).
    pub(crate) fn creating(&self) -> usize {
        let out = self
            .space_writes
            .values()
            .filter(|(target, _)| *target == EditFor::New)
            .count();
        let unlisted = self
            .made
            .iter()
            .filter(|id| !self.model.spaces.iter().any(|w| w.id == **id))
            .count();
        out + unlisted
    }

    /// The numbered name an empty new space takes: what `ctrl-a w` makes, the new spaces
    /// already asked for counted.
    pub(crate) fn next_space_name(&self) -> String {
        qoop_tui::run::space_name_after(&self.model.spaces, None, self.creating())
    }

    /// The kind an agent of the list sorts by in the Priority view.
    fn rank_of(&self, terminal: u32) -> u8 {
        self.model
            .agents()
            .into_iter()
            .find(|e| e.terminal() == terminal)
            .map_or(Kind::Sleeping.rank(), |e| e.kind().rank())
    }

    /// `terminal` at the top of `block`: of its space, or of its state's group in Priority.
    fn place_own(&self, block: &mut Block, terminal: u32) {
        block.rows.retain(|t| *t != terminal);
        let at = match block.space {
            Some(_) => 0,
            None => {
                let rank = self.rank_of(terminal);
                block
                    .rows
                    .iter()
                    .position(|t| self.rank_of(*t) >= rank)
                    .unwrap_or(block.rows.len())
            }
        };
        block.rows.insert(at, terminal);
    }

    /// What the column draws now from the live lists.
    pub(crate) fn live_column(&self, inbox: &Inbox) -> Drawn {
        let on_screen = match self.page {
            Page::Session(t) => Some(t),
            _ => None,
        };
        let waiting = inbox.terminals();
        let tab = self.side_filter;
        let mut titles = HashMap::new();
        let mut keep = |rows: Vec<Entry<'_>>| -> Vec<u32> {
            rows.into_iter()
                .filter(|e| state::in_filter(e, tab))
                .map(|e| {
                    titles.insert(e.terminal(), e.title());
                    e.terminal()
                })
                .collect()
        };
        let mut blocks: Vec<Block> = match self.side_view {
            SideView::Spaces => self
                .model
                .groups()
                .into_iter()
                .map(|g| {
                    let id = g.space.id;
                    let default = match tab {
                        StateTab::All => state::opens_by_default(&g, &waiting, on_screen),
                        _ => true,
                    };
                    let name = space_name(g.space);
                    let rows = keep(g.rows);
                    Block {
                        space: Some(id),
                        name,
                        // A space with nothing on the list is folded, whatever was asked.
                        open: !rows.is_empty() && self.folds.get(&id).copied().unwrap_or(default),
                        rows,
                    }
                })
                .collect(),
            SideView::Priority => vec![Block {
                space: None,
                name: String::new(),
                open: true,
                rows: keep(self.model.priority()),
            }],
        };
        // The agent the person just started is the newest, before the daemon stamps a time.
        if let Some(own) = self.own_new {
            let fresh = self
                .model
                .agents()
                .into_iter()
                .find(|e| e.terminal() == own)
                .is_some_and(|e| state::active_at(e).is_none());
            if fresh {
                for block in blocks.iter_mut().filter(|b| b.rows.contains(&own)) {
                    self.place_own(block, own);
                }
            }
        }
        Drawn {
            inbox: if self.inbox_open {
                inbox
                    .items
                    .iter()
                    .take(qoop_tui::sidebar::INBOX_ROWS)
                    .map(|i| i.slot.clone())
                    .collect()
            } else {
                Vec::new()
            },
            blocks,
            titles,
        }
    }

    /// What the column draws: the held rows while held, else the live ones.
    pub(crate) fn drawn_column(&self, inbox: &Inbox) -> Drawn {
        self.held.clone().unwrap_or_else(|| self.live_column(inbox))
    }

    /// Before each frame: hold the column while the pointer is on it or the keyboard drives in
    /// it (a focus left there by a click holds nothing once the pointer leaves), let it go
    /// otherwise, and put the agent the person started into the held rows.
    pub(crate) fn update_hold(&mut self, inbox: &Inbox, window: &Window, cx: &gpui::App) {
        // The menu without the keys (⌘J, Tab, a click elsewhere took them) is closed.
        if self.space_menu.is_some() && !self.menu_has_keys(window) {
            self.drop_menu();
        }
        let keys = window.last_input_was_keyboard() && self.side_focus.contains_focused(window, cx);
        // The menu covers the column and blocks its hover: the pointer on it is on the column.
        let pointer = self.side_hovered || self.menu_hovered;
        // Nothing listed yet (the skeletons): nothing to hold.
        if !(pointer || keys) || !self.listed {
            self.held = None;
            return;
        }
        let mut held = self.held.take().unwrap_or_else(|| self.live_column(inbox));
        if let Some(own) = self.own_new.filter(|t| !held.has_row(*t))
            && let Some(entry) = self
                .model
                .agents()
                .into_iter()
                .find(|e| e.terminal() == own)
            && state::in_filter(&entry, self.side_filter)
        {
            let space = entry.workspace();
            let title = entry.title();
            if let Some(block) = held
                .blocks
                .iter_mut()
                .find(|b| b.space.is_none_or(|s| s == space))
            {
                self.place_own(block, own);
                block.open = true;
                held.titles.insert(own, title);
            }
        }
        self.held = Some(held);
    }

    /// The person's fold of a space: at once, in the held rows too. A space with nothing on
    /// the list (empty, or filtered out) has nothing to open: no choice is kept for it.
    pub(crate) fn toggle_fold(&mut self, space: i64, cx: &mut Context<Self>) {
        let inbox = self.inbox();
        let live = self.live_column(&inbox);
        let drawn = self.held.as_ref().unwrap_or(&live);
        let Some(block) = drawn.blocks.iter().find(|b| b.space == Some(space)) else {
            return;
        };
        let has_rows = |d: &Drawn| {
            d.blocks
                .iter()
                .any(|b| b.space == Some(space) && !b.rows.is_empty())
        };
        if !has_rows(drawn) && !has_rows(&live) {
            return;
        }
        let open = block.open;
        self.folds.insert(space, !open);
        if let Some(held) = self.held.as_mut()
            && let Some(block) = held.blocks.iter_mut().find(|b| b.space == Some(space))
        {
            block.open = !open;
            if block.open
                && let Some(now) = live.blocks.iter().find(|b| b.space == Some(space))
            {
                block.rows = now.rows.clone();
                for t in &now.rows {
                    if let Some(title) = live.titles.get(t) {
                        held.titles.insert(*t, title.clone());
                    }
                }
            }
        }
        cx.notify();
    }

    /// The person opens or folds the small Inbox: at once, in the held rows too.
    pub(crate) fn toggle_small_inbox(&mut self, cx: &mut Context<Self>) {
        self.inbox_open = !self.inbox_open;
        if self.held.is_some() {
            let inbox = self.inbox();
            let now = self.live_column(&inbox).inbox;
            if let Some(held) = self.held.as_mut() {
                held.inbox = now;
            }
        }
        cx.notify();
    }

    /// A filter tab or the order switched: the whole list is drawn again from the live lists.
    fn set_list(&mut self, view: SideView, tab: StateTab, cx: &mut Context<Self>) {
        self.side_view = view;
        self.side_filter = tab;
        // The frame takes a fresh hold of what it now shows.
        self.held = None;
        cx.notify();
    }

    /// The spaces came again: the person's renames and new spaces go into the held rows, and a
    /// field whose write was answered closes now that the list shows it.
    pub(crate) fn spaces_came(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut held) = self.held.take() {
            let inbox = self.inbox();
            let live = self.live_column(&inbox);
            for block in &mut held.blocks {
                if let Some(id) = block.space.filter(|id| self.renamed.contains(id))
                    && let Some(now) = live.blocks.iter().find(|b| b.space == Some(id))
                {
                    block.name = now.name.clone();
                }
            }
            for id in &self.made {
                let shown = held.blocks.iter().any(|b| b.space == Some(*id));
                if let (false, Some(now)) =
                    (shown, live.blocks.iter().find(|b| b.space == Some(*id)))
                {
                    // An empty space has no activity: last, folded.
                    held.blocks.push(Block {
                        open: false,
                        ..now.clone()
                    });
                }
            }
            self.held = Some(held);
        }
        self.renamed.clear();
        // A space made counts toward the next number until the list has it.
        let spaces = &self.model.spaces;
        self.made.retain(|id| !spaces.iter().any(|w| w.id == *id));
        let finished = self.space_edit.as_ref().is_some_and(|e| {
            e.done
                && !self
                    .space_writes
                    .values()
                    .any(|(target, _)| *target == e.target)
        });
        if finished {
            self.close_space_edit(window, cx);
        }
        self.refresh_placeholder(cx);
    }

    /// An answer to a space write. True when it was one.
    pub(crate) fn space_answered(
        &mut self,
        answer: &ControlResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((target, _)) = self.space_writes.remove(answer.request_id()) else {
            return false;
        };
        let ours = self.space_edit.as_ref().is_some_and(|e| e.target == target);
        match answer {
            ControlResponse::Failed { reason, .. } => {
                if ours && let Some(edit) = self.space_edit.as_mut() {
                    edit.said = Some(reason.clone());
                }
            }
            other => {
                match (other, target) {
                    (ControlResponse::WorkspaceCreated { workspace, .. }, _) => {
                        self.made.push(*workspace)
                    }
                    (_, EditFor::Rename(space)) => {
                        self.renamed.insert(space);
                    }
                    _ => {}
                }
                if ours && let Some(edit) = self.space_edit.as_mut() {
                    edit.done = true;
                }
                self.ask(ControlRequest::ListWorkspaces {
                    id: link::fresh_id("spaces"),
                });
            }
        }
        self.refresh_placeholder(cx);
        cx.notify();
        true
    }

    /// Space writes nobody answered for [`SPACE_WAIT`] seconds are given up, and the field
    /// says so; `now` `None` is the link gone, which takes every write with it. True when the
    /// column changed.
    pub(crate) fn space_writes_lost(&mut self, now: Option<i64>, cx: &mut Context<Self>) -> bool {
        let before = self.space_writes.len();
        let mut lost = Vec::new();
        self.space_writes.retain(|_, (target, at)| {
            let keep = now.is_some_and(|now| now - *at < SPACE_WAIT);
            if !keep {
                lost.push(*target);
            }
            keep
        });
        if let Some(edit) = self.space_edit.as_mut()
            && lost.contains(&edit.target)
        {
            let machine = match edit.target {
                EditFor::Rename(id) => self
                    .model
                    .space(id)
                    .map_or_else(|| "This Mac".to_string(), state::machine_name),
                EditFor::New => "This Mac".to_string(),
            };
            edit.said = Some(format!("No answer from {machine}. Try again."));
        }
        let changed = self.space_writes.len() != before;
        if changed {
            self.refresh_placeholder(cx);
        }
        changed
    }

    /// The new-space field shows the numbered name an empty Enter makes.
    fn refresh_placeholder(&self, cx: &mut Context<Self>) {
        if let Some(edit) = self
            .space_edit
            .as_ref()
            .filter(|e| e.target == EditFor::New)
        {
            let name = self.next_space_name();
            edit.field.update(cx, |f, cx| {
                f.placeholder = name.into();
                cx.notify();
            });
        }
    }

    /// Open the rename field in `space`'s heading, its name selected.
    pub(crate) fn open_rename(&mut self, space: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.model.space(space) else {
            return;
        };
        let name = state::short_space(row);
        let field = cx.new(|cx| {
            let mut f = Field::new(&name, cx);
            f.label = Some(format!("New name for {name}").into());
            f.set_text(&name);
            f.select_all();
            f
        });
        self.open_space_edit(EditFor::Rename(space), field, window, cx);
    }

    /// Open the new-space field under the strip, its placeholder the name an empty Enter makes.
    pub(crate) fn open_new_space(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.next_space_name();
        let field = cx.new(|cx| {
            let mut f = Field::new(&name, cx);
            f.label = Some("Name of the new space".into());
            f
        });
        self.open_space_edit(EditFor::New, field, window, cx);
    }

    fn open_space_edit(
        &mut self,
        target: EditFor,
        field: Entity<Field>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        let focus = field.read(cx).focus.clone();
        self.space_menu = None;
        self.space_edit = Some(SpaceEdit {
            target,
            field,
            said: None,
            done: false,
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// A space's name control's focus handle, kept from frame to frame.
    pub(crate) fn name_handle(&self, space: i64, cx: &mut Context<Self>) -> FocusHandle {
        self.name_focus
            .borrow_mut()
            .entry(space)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// Close the space field, the keys on the space's name (a rename) or the strip's `+` (a
    /// new space), drawn again in place of the field from the next frame.
    pub(crate) fn close_space_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.space_edit.take() {
            let to = match edit.target {
                EditFor::Rename(space) => self.name_handle(space, cx),
                EditFor::New => self.new_space_focus.clone(),
            };
            window.focus(&to, cx);
        }
        cx.notify();
    }

    /// Enter in a space field: the name trimmed, written. An empty rename is not sent; an
    /// empty new space takes the numbered name the field shows.
    fn submit_space_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.space_edit.as_ref() else {
            return;
        };
        let target = edit.target;
        let typed = edit.field.read(cx).text.trim().to_string();
        let out = self.space_writes.values().any(|(t, _)| *t == target);
        let (request, said) = match target {
            EditFor::Rename(space) => {
                if typed.is_empty() {
                    self.space_said("A space needs a name.");
                    cx.notify();
                    return;
                }
                if out {
                    return;
                }
                (
                    ControlRequest::RenameWorkspace {
                        id: link::fresh_id("rename"),
                        workspace: space,
                        name: typed,
                    },
                    "Renaming",
                )
            }
            EditFor::New => {
                // A typed name goes once; empty Enters each make the next number.
                if out && !typed.is_empty() {
                    return;
                }
                let name = if typed.is_empty() {
                    self.next_space_name()
                } else {
                    typed
                };
                (
                    ControlRequest::CreateWorkspace {
                        id: link::fresh_id("space"),
                        name,
                    },
                    "Making the space",
                )
            }
        };
        self.space_writes.insert(
            request.request_id().to_string(),
            (target, crate::app::unix_now()),
        );
        self.ask(request);
        self.space_said(said);
        self.refresh_placeholder(cx);
        cx.notify();
    }

    fn space_said(&mut self, said: &str) {
        if let Some(edit) = self.space_edit.as_mut() {
            edit.said = Some(said.to_string());
        }
    }

    /// Open `space`'s menu, the keys on its first item.
    fn open_menu(&mut self, space: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.space_menu = Some(SpaceMenu { space, at: 0 });
        window.focus(&self.menu_items[0], cx);
        cx.notify();
    }

    /// Whether the open menu has the keys: one of its items is focused. Anywhere else (the
    /// terminal after ⌘J, the next control after Tab) the keys are not the menu's.
    pub(crate) fn menu_has_keys(&self, window: &Window) -> bool {
        self.space_menu.is_some() && self.menu_items.iter().any(|h| h.is_focused(window))
    }

    /// The menu went out of the person's hands: closed with nothing more said.
    fn drop_menu(&mut self) {
        self.space_menu = None;
        // The pointer on the menu was on the column: it still is until it moves.
        if std::mem::take(&mut self.menu_hovered) {
            self.side_hovered = true;
        }
    }

    /// Close the menu, the keys back on its `...`.
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.space_menu.as_ref() {
            let back = self.more_focus.borrow().get(&menu.space).cloned();
            self.drop_menu();
            if let Some(back) = back {
                window.focus(&back, cx);
            }
        }
        cx.notify();
    }

    /// Run the menu's item `at` for its space.
    fn run_menu(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.space_menu.as_ref().map(|m| m.space) else {
            return;
        };
        self.drop_menu();
        match at {
            0 => self.new_work(Some(space), window, cx),
            1 => self.open_rename(space, window, cx),
            2 => self.open_new_space(window, cx),
            _ => {
                self.toggle_fold(space, cx);
                let back = self.more_focus.borrow().get(&space).cloned();
                if let Some(back) = back {
                    window.focus(&back, cx);
                }
            }
        }
        cx.notify();
    }

    /// Enter, Escape and the arrows in a space field and the open menu, before the window's
    /// own keys (an Escape there leaves the field, never the Inbox; a digit is typed, never an
    /// answer). True when taken.
    pub(crate) fn column_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.menu_has_keys(window)
            && let Some(menu) = self.space_menu.as_mut()
        {
            let at = menu.at;
            match key {
                // The item takes the focus, so a screen reader says it.
                "up" | "down" => {
                    let n = MENU.len();
                    menu.at = if key == "up" {
                        (at + n - 1) % n
                    } else {
                        (at + 1) % n
                    };
                    let to = self.menu_items[menu.at].clone();
                    window.focus(&to, cx);
                }
                "enter" | "space" => self.run_menu(at, window, cx),
                "escape" => self.close_menu(window, cx),
                _ => return false,
            }
            cx.notify();
            return true;
        }
        let in_field = self
            .space_edit
            .as_ref()
            .is_some_and(|e| e.field.read(cx).focus.is_focused(window));
        if !in_field {
            return false;
        }
        match key {
            "enter" => self.submit_space_edit(cx),
            "escape" => self.close_space_edit(window, cx),
            _ => return false,
        }
        true
    }

    /// The small Inbox under the Inbox row while it is open: its first rows (the question,
    /// then the space and title, then the age), "N more in the Inbox", or that nothing waits.
    pub(crate) fn small_inbox(
        &self,
        drawn: &Drawn,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.inbox_open {
            return None;
        }
        let quiet = |text: String| {
            div()
                .px(px(10.))
                .py(px(4.))
                .text_size(px(12.5))
                .text_color(c(t.t2))
                .child(text)
        };
        let body: AnyElement = if !self.listed {
            div().child(crate::inbox::skeleton(0, t)).into_any_element()
        } else if drawn.inbox.is_empty() && inbox.items.is_empty() {
            quiet("Nothing waits on you.".into())
                .id("side-inbox-none")
                .debug_selector(|| "side-inbox-none".to_string())
                .into_any_element()
        } else {
            let more = inbox
                .items
                .len()
                .saturating_sub(qoop_tui::sidebar::INBOX_ROWS);
            div()
                .flex()
                .flex_col()
                .children(
                    drawn
                        .inbox
                        .iter()
                        .enumerate()
                        .map(|(n, slot)| self.small_item(n, slot, inbox, t, cx)),
                )
                .when(more > 0, |d| {
                    d.child(
                        ui::link_button("side-inbox-more", &format!("{more} more in the Inbox"), t)
                            .debug_selector(|| "side-inbox-more".to_string())
                            .ml(px(10.))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(Page::Inbox, window, cx)
                            })),
                    )
                })
                .into_any_element()
        };
        Some(
            div()
                .id("side-inbox")
                .debug_selector(|| "side-inbox".to_string())
                .role(Role::List)
                .aria_label("Waiting on you")
                .pl(px(18.))
                .pb(px(4.))
                .child(body)
                .into_any_element(),
        )
    }

    /// One row of the small Inbox: a click opens its card in the Inbox.
    fn small_item(
        &self,
        n: usize,
        slot: &Slot,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = match slot {
            Slot::Agent(t) => format!("agent-{t}"),
            Slot::Card(id) => format!("card-{id}"),
        };
        let selector = format!("side-item-{key}");
        let Some(item) = inbox.items.iter().find(|i| &i.slot == slot) else {
            // Answered while held: in place, dimmed, not a link.
            return div()
                .debug_selector(move || selector)
                .px(px(10.))
                .py(px(3.))
                .text_size(px(12.5))
                .text_color(c(t.t2))
                .child("Answered")
                .into_any_element();
        };
        let question = self.item_question(item);
        let row = item.terminal.and_then(|t| self.model.row(t));
        let who = row.map_or_else(
            || "qass".to_string(),
            |r| match self.model.space(r.workspace) {
                Some(w) => format!("{} {}", state::short_space(w), state::title_of(r)),
                None => state::title_of(r),
            },
        );
        let ago = item.since.map(|at| self.now.saturating_sub(at).max(0));
        let label = match ago {
            Some(secs) => format!("{who} asks: {question}, {}", state::spoken_span(secs)),
            None => format!("{who} asks: {question}"),
        };
        let at = n;
        let open_slot = slot.clone();
        ui::control(ElementId::from(selector.clone()), Role::Link, label, t)
            .debug_selector(move || selector)
            .group(ui::AVATAR_ROW)
            .flex()
            .items_start()
            .gap(px(8.))
            .px(px(8.))
            .py(px(3.))
            .rounded(px(8.))
            .text_size(px(12.5))
            .hover(|s| ui::paint(s, t.hover()))
            .child(div().pt(px(2.)).child(ui::avatar(
                row.and_then(|r| r.program.as_deref()).or(Some("qass")),
                Kind::Status(qoop_domain::status::Status::Blocked),
                18.,
                true,
                t,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .whitespace_normal()
                            .text_color(c(t.ink))
                            .child(question),
                    )
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(px(11.5))
                            .text_color(c(t.t2))
                            .child(who),
                    ),
            )
            .when_some(ago, |d, secs| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(c(t.t2))
                        .child(state::short_span(secs)),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.inbox_filter = crate::inbox::Filter::All;
                this.set_cursor(open_slot.clone(), at);
                this.open(Page::Inbox, window, cx);
            }))
            .into_any_element()
    }

    /// The Agents strip, the filter, the new-space field, the list and its foot.
    pub(crate) fn agent_list(
        &self,
        drawn: &Drawn,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agents = self.model.agents();
        let by_terminal: HashMap<u32, Entry<'_>> =
            agents.iter().map(|e| (e.terminal(), *e)).collect();
        let tab = self.side_filter;
        let shown = agents.iter().filter(|e| state::in_filter(e, tab)).count();
        // New rows the hold keeps back: counted, not drawn.
        // Only what will show once let go: not the rows of a space that stays folded.
        let fresh = if self.held.is_some() {
            let live = self.live_column(inbox);
            live.blocks
                .iter()
                .filter(|b| b.open)
                .flat_map(|b| b.rows.iter())
                .filter(|t| !drawn.has_row(**t))
                .count()
                + live
                    .inbox
                    .iter()
                    .filter(|s| !drawn.inbox.contains(s))
                    .count()
        } else {
            0
        };
        let nothing = self.listed && agents.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(self.strip(t, cx))
            .child(self.filter_row(&agents, t, cx))
            .when_some(
                self.space_edit
                    .as_ref()
                    .filter(|e| e.target == EditFor::New),
                |d, edit| d.child(self.edit_box(edit, t)),
            )
            .when(!self.listed, |d| {
                d.children((0..4).map(|n| crate::inbox::skeleton(n, t)))
            })
            .when(nothing, |d| {
                d.child(
                    div()
                        .id("side-empty")
                        .px(px(10.))
                        .py(px(8.))
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .child("No agents yet. \u{2318}N starts one."),
                )
            })
            .children(
                drawn
                    .blocks
                    .iter()
                    .map(|b| self.block(b, drawn, &by_terminal, inbox, t, cx)),
            )
            .when(self.listed && !nothing && tab != StateTab::All, |d| {
                d.child(self.filter_note(tab, shown, agents.len() - shown, t, cx))
            })
            .when(fresh > 0, |d| {
                d.child(
                    div()
                        .id("side-new")
                        .debug_selector(|| "side-new".to_string())
                        .role(Role::Status)
                        .px(px(10.))
                        .py(px(6.))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(format!("{fresh} new, shown when you move away")),
                )
            })
            .into_any_element()
    }

    /// "Agents", the Spaces or Priority switch, and `+` for a new space.
    fn strip(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.side_view;
        let option =
            |id: &'static str, label: &'static str, value: SideView, cx: &mut Context<Self>| {
                let on = view == value;
                let other = match value {
                    SideView::Spaces => SideView::Priority,
                    SideView::Priority => SideView::Spaces,
                };
                ui::control(id, Role::RadioButton, label, t)
                    .debug_selector(move || id.to_string())
                    .aria_toggled(ui::toggled(on))
                    .px(px(8.))
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .text_color(c(t.t1))
                    .when(on, |d| ui::picked(d, t))
                    .hover(|s| ui::paint(s, t.hover()))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let tab = this.side_filter;
                        this.set_list(value, tab, cx);
                    }))
                    // Left and Right move between the two, as a radio group's keys do.
                    .on_key_down(
                        cx.listener(move |this, e: &gpui::KeyDownEvent, window, cx| {
                            if matches!(e.keystroke.key.as_str(), "left" | "right") {
                                let tab = this.side_filter;
                                this.set_list(other, tab, cx);
                                match other {
                                    SideView::Spaces => window.focus_prev(cx),
                                    SideView::Priority => window.focus_next(cx),
                                }
                                cx.stop_propagation();
                            }
                        }),
                    )
            };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .pt(px(10.))
            .pb(px(4.))
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Agents"),
            )
            .child(
                div()
                    .id("side-view")
                    .role(Role::RadioGroup)
                    .aria_label("Order the agents")
                    .flex()
                    .gap(px(2.))
                    .child(option("view-spaces", "Spaces", SideView::Spaces, cx))
                    .child(option("view-priority", "Priority", SideView::Priority, cx)),
            )
            .child(div().flex_1())
            .child(
                icon_button("side-new-space", "New space", Icon::Plus, t)
                    .track_focus(&self.new_space_focus)
                    .debug_selector(|| "side-new-space".to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.open_new_space(window, cx))),
            )
    }

    /// The state tabs: each its mark and count, the chosen one spelled out in its ink; the
    /// chosen one again goes back to all.
    fn filter_row(
        &self,
        agents: &[Entry<'_>],
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chosen = self.side_filter;
        div()
            .id("side-filter")
            .role(Role::RadioGroup)
            .aria_label("Show agents")
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(1.))
            .px(px(8.))
            .pb(px(4.))
            .children(StateTab::ORDER.map(|tab| {
                let n = agents.iter().filter(|e| state::in_filter(e, tab)).count();
                let on = tab == chosen;
                let ink = match (tab, tab.kind()) {
                    (StateTab::All, _) => t.ink,
                    (StateTab::Saved, _) => t.saved_text,
                    (_, Some(kind)) => ui::word_ink(kind, t),
                    (_, None) => t.t1,
                };
                let mark = match (tab, tab.kind()) {
                    (StateTab::Saved, _) => Some(ui::dots_mark(&state::SAVED_DOTS, t.saved, 10.)),
                    (_, Some(kind)) => Some(ui::dots_mark(
                        &state::mark_dots(kind, None),
                        ui::mark_ink(kind, t),
                        10.,
                    )),
                    _ => None,
                };
                let id = tab.word();
                ui::control(
                    ("side-tab", tab as u64),
                    Role::RadioButton,
                    format!("{}, {}", tab.word(), state::count(n, "agent")),
                    t,
                )
                .debug_selector(move || format!("side-tab-{id}"))
                .aria_toggled(ui::toggled(on))
                .flex()
                .items_center()
                .gap(px(4.))
                .h(px(24.))
                .px(px(4.))
                .rounded(px(7.))
                .text_size(px(12.))
                .text_color(c(t.t1))
                .when(on, |d| ui::picked(d, t))
                .hover(|s| ui::paint(s, t.hover()))
                .when_some(mark, |d, m| d.child(m))
                .when(tab == StateTab::All || on, |d| {
                    d.child(div().text_color(c(ink)).child(tab.word()))
                })
                .child(
                    div()
                        .text_color(c(if n == 0 { t.t3 } else { t.t2 }))
                        .child(n.to_string()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = if on { StateTab::All } else { tab };
                    let view = this.side_view;
                    this.set_list(view, next, cx);
                }))
                .on_key_down(cx.listener(
                    |_, e: &gpui::KeyDownEvent, window, cx| {
                        match e.keystroke.key.as_str() {
                            "left" => window.focus_prev(cx),
                            "right" => window.focus_next(cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    },
                ))
            }))
    }

    /// Under a filtered list: what it shows and how many all shows besides.
    fn filter_note(
        &self,
        tab: StateTab,
        shown: usize,
        others: usize,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (head, tail) = filter_words(tab, shown, others);
        div()
            .id("side-filter-note")
            .debug_selector(|| "side-filter-note".to_string())
            .role(Role::Status)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(4.))
            .px(px(10.))
            .pt(px(8.))
            .text_size(px(12.))
            .text_color(c(t.t2))
            .child(head)
            .child(
                ui::link_button("side-filter-all", "all", t)
                    .debug_selector(|| "side-filter-all".to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        let view = this.side_view;
                        this.set_list(view, StateTab::All, cx);
                    })),
            )
            .child(tail)
    }

    /// One block: a space's heading and, open, its agents; or the Priority list.
    fn block(
        &self,
        block: &Block,
        drawn: &Drawn,
        by_terminal: &HashMap<u32, Entry<'_>>,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = div().flex().flex_col().gap(px(1.)).when(block.open, |d| {
            d.children(block.rows.iter().map(|terminal| {
                let title = drawn.titles.get(terminal).cloned();
                match by_terminal
                    .get(terminal)
                    .filter(|e| state::in_filter(e, self.side_filter))
                {
                    Some(entry) => {
                        self.agent_row(*entry, title, block.space.is_none(), inbox, t, cx)
                    }
                    None => self.left_row(*terminal, title, by_terminal.get(terminal).copied(), t),
                }
            }))
        });
        let Some(space) = block.space else {
            return rows.into_any_element();
        };
        div()
            .id(("side-group", space as u64))
            .flex()
            .flex_col()
            .child(self.heading(space, block, by_terminal, t, cx))
            .child(rows)
            .into_any_element()
    }

    /// A space's heading: its fold, its name (renamed in place), its count, `+` and `...`.
    fn heading(
        &self,
        space: i64,
        block: &Block,
        by_terminal: &HashMap<u32, Entry<'_>>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = self.model.space(space);
        let total = by_terminal
            .values()
            .filter(|e| e.workspace() == space)
            .count();
        let matches = by_terminal
            .values()
            .filter(|e| e.workspace() == space && state::in_filter(e, self.side_filter))
            .count();
        let none = self.side_filter != StateTab::All && matches == 0;
        let counted = self.heading_count(space, block.open);
        let name = block.name.clone();
        let slot = row.map_or(0, state::space_slot);
        let open = block.open;
        let editing = self
            .space_edit
            .as_ref()
            .filter(|e| e.target == EditFor::Rename(space));
        let menu_open = self.space_menu.as_ref().is_some_and(|m| m.space == space);
        let more_focus = self
            .more_focus
            .borrow_mut()
            .entry(space)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        let name_focus = self.name_handle(space, cx);
        div()
            .id(("side-space", space as u64))
            .debug_selector(move || format!("side-space-{space}"))
            .relative()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(4.))
            .pt(px(8.))
            .pb(px(2.))
            .pl(px(2.))
            .child(
                ui::control(
                    ("side-fold", space as u64),
                    Role::Button,
                    if open {
                        format!("Fold {name}")
                    } else {
                        format!("Open {name}")
                    },
                    t,
                )
                .debug_selector(move || format!("side-fold-{space}"))
                .aria_expanded(open)
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .hover(|s| ui::paint(s, t.hover()))
                .child({
                    let chevron = ui::icon(Icon::Chevron, 12., c(t.t3));
                    if open {
                        chevron.with_transformation(Transformation::rotate(percentage(0.25)))
                    } else {
                        chevron
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(space, cx))),
            )
            .map(|d| match editing {
                Some(edit) => d.child(div().flex_1().min_w(px(80.)).child(self.edit_box(edit, t))),
                None => d
                    .child(
                        ui::control(
                            ("side-name", space as u64),
                            Role::Button,
                            format!("Rename {name}, {}", state::count(total, "agent")),
                            t,
                        )
                        .track_focus(&name_focus)
                        .debug_selector(move || format!("side-name-{space}"))
                        .px(px(3.))
                        .rounded(px(5.))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.space(slot))
                        .hover(|s| s.bg(c(t.card2)))
                        .child(name.clone())
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.open_rename(space, window, cx),
                        )),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("side-count-{space}"))
                            .text_size(px(12.))
                            .text_color(c(if none { t.t3 } else { t.t2 }))
                            .child(counted),
                    ),
            })
            .child(div().flex_1())
            // `+` and `...` stay together at the right of the last line.
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(2.))
                    .child(
                        icon_button(
                            ("side-plus", space as u64),
                            &format!("New agent in {name}"),
                            Icon::Plus,
                            t,
                        )
                        .debug_selector(move || format!("side-plus-{space}"))
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.new_work(Some(space), window, cx),
                        )),
                    )
                    .child(
                        icon_button(
                            ("side-more", space as u64),
                            &format!("{name} actions"),
                            Icon::More,
                            t,
                        )
                        .track_focus(&more_focus)
                        .debug_selector(move || format!("side-more-{space}"))
                        .aria_expanded(menu_open)
                        .when(menu_open, |d| ui::current(d, t))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                // The press closed this menu already (`Desk::menu`).
                                if this.menu_closed_by_press.take() == Some(space) {
                                    cx.notify();
                                } else if this.space_menu.as_ref().is_some_and(|m| m.space == space)
                                {
                                    this.close_menu(window, cx);
                                } else {
                                    this.open_menu(space, window, cx);
                                }
                            },
                        )),
                    ),
            )
            .when(menu_open, |d| d.child(self.menu(space, &name, open, t, cx)))
    }

    /// The heading's menu, over the column: New agent in it, Rename, New space, Fold.
    fn menu(
        &self,
        space: i64,
        name: &str,
        open: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let at = self.space_menu.as_ref().map_or(0, |m| m.at);
        let labels = [
            format!("{} {name}", MENU[0]),
            MENU[1].to_string(),
            MENU[2].to_string(),
            if open { MENU[3] } else { "Open" }.to_string(),
        ];
        // Over the heading's agents, inside the column's width, drawn last. It takes every
        // press on it (`occlude`): a click on an item never also clicks the row under it. Its
        // own hover stands for the column's, which it blocks, so the hold stays.
        deferred(
            div()
                .id("side-menu")
                .debug_selector(|| "side-menu".to_string())
                .role(Role::Menu)
                .aria_label(format!("{name} actions"))
                .occlude()
                .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if this.menu_hovered != *hovered {
                        this.menu_hovered = *hovered;
                        cx.notify();
                    }
                }))
                .absolute()
                .top(px(30.))
                .left(px(24.))
                .right_0()
                .p(px(4.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .rounded(px(10.))
                .bg(c(t.pop))
                .border_1()
                .border_color(ca(t.line))
                .on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                    if this.space_menu.as_ref().is_some_and(|m| m.space == space) {
                        // A press on this menu's own `...` closes it and nothing more.
                        this.menu_closed_by_press = Some(space);
                        this.drop_menu();
                        window.refresh();
                        cx.notify();
                    }
                }))
                .children(labels.into_iter().enumerate().map(|(n, label)| {
                    ui::control(
                        ("side-menu-item", n as u64),
                        Role::MenuItem,
                        label.clone(),
                        t,
                    )
                    .track_focus(&self.menu_items[n])
                    .debug_selector(move || format!("side-menu-{n}"))
                    .aria_selected(n == at)
                    .px(px(10.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .text_color(c(t.t1))
                    .when(n == at, |d| ui::current(d, t))
                    .hover(|s| ui::paint(s, t.hover()))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| this.run_menu(n, window, cx)))
                })),
        )
        .with_priority(1)
    }

    /// What a space's heading says beside its name, `open` or folded as drawn: its count (of
    /// the filter's list when filtered), and the machine for another computer's space.
    pub(crate) fn heading_count(&self, space: i64, open: bool) -> String {
        let agents = self.model.agents();
        let of_space = || agents.iter().filter(|e| e.workspace() == space);
        let total = of_space().count();
        let matches = of_space()
            .filter(|e| state::in_filter(e, self.side_filter))
            .count();
        let counted = count_words(self.side_filter, matches, total, open);
        match self
            .model
            .space(space)
            .filter(|w| state::machine_of(w.id) != 0)
        {
            Some(w) => format!("{counted}, {}", state::machine_name(w)),
            None => counted,
        }
    }

    /// A space field and the line under it.
    fn edit_box(&self, edit: &SpaceEdit, t: &Theme) -> impl IntoElement {
        div()
            .id("side-edit")
            .debug_selector(|| "side-edit".to_string())
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(4.))
            .py(px(2.))
            .child(edit.field.clone())
            .when_some(edit.said.clone(), |d, said| {
                d.child(
                    div()
                        .id("side-edit-said")
                        .debug_selector(|| "side-edit-said".to_string())
                        .role(Role::Status)
                        .aria_label(said.clone())
                        .whitespace_normal()
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(said),
                )
            })
    }

    /// One agent of the list: its avatar with its mark, its title (wrapped whole), qass's eye
    /// with its card count, then how long it has waited (Needs you) or its state word.
    fn agent_row(
        &self,
        entry: Entry<'_>,
        title: Option<String>,
        tagged: bool,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = self.shown_kind(entry);
        let terminal = entry.terminal();
        let on = self.page == Page::Session(terminal);
        let handed = matches!(entry, Entry::Live(r) if self.model.with_qass(r));
        let cards = self.cards_of(terminal, entry.workspace()).len();
        let right: AnyElement = if kind.needs_you() {
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(c(t.t2))
                .children(self.side_age(entry, inbox))
                .into_any_element()
        } else {
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(c(ui::word_ink(kind, t)))
                .child(kind.word())
                .into_any_element()
        };
        let space = self.model.space(entry.workspace());
        let label = self.row_name(entry, inbox);
        ui::control(("tree-row", u64::from(terminal)), Role::Link, label, t)
            .debug_selector(move || format!("tree-row-{terminal}"))
            .group(ui::AVATAR_ROW)
            .flex()
            .items_start()
            .gap(px(8.))
            .min_h(px(28.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(10.))
            .text_size(px(13.))
            .when(on, |d| ui::current(d, t))
            .hover(|s| ui::paint(s, t.hover()))
            .child(
                div()
                    .pt(px(1.))
                    .child(ui::avatar(entry.program(), kind, 20., !on, t)),
            )
            .child({
                let title = title.unwrap_or_else(|| entry.title());
                // In Priority each row says its space, in its ink, before its title.
                let text = match space.filter(|_| tagged) {
                    Some(w) => {
                        let tag = state::short_space(w);
                        let highlight = gpui::HighlightStyle {
                            color: Some(t.space(state::space_slot(w))),
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..Default::default()
                        };
                        gpui::StyledText::new(format!("{tag} {title}"))
                            .with_highlights([(0..tag.len(), highlight)])
                    }
                    None => gpui::StyledText::new(title),
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(c(if kind.needs_you() { t.ink } else { t.t1 }))
                    .child(text)
            })
            .when(handed || cards > 0, |d| {
                d.child(
                    div()
                        .debug_selector(move || format!("tree-cards-{terminal}"))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .px(px(5.))
                        .rounded(px(8.))
                        .bg(ca(t.needs_bg))
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(t.needs_text))
                        .child(ui::icon(Icon::Eye, 11., c(t.t2)))
                        .when(cards > 0, |d| d.child(cards.to_string())),
                )
            })
            .child(right)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open(Page::Session(terminal), window, cx)
            }))
            .into_any_element()
    }

    /// A held row that left (answered or slept elsewhere, closed, off the filter): in place,
    /// dimmed, with its new state word, not a link.
    fn left_row(
        &self,
        terminal: u32,
        title: Option<String>,
        now: Option<Entry<'_>>,
        t: &Theme,
    ) -> AnyElement {
        let word = now.map_or("closed", |e| self.shown_kind(e).word());
        div()
            .debug_selector(move || format!("tree-row-{terminal}"))
            .flex()
            .items_start()
            .gap(px(8.))
            .min_h(px(28.))
            .px(px(8.))
            .py(px(4.))
            .text_size(px(13.))
            .text_color(c(t.t2))
            .child(div().w(px(20.)).flex_none())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .child(title.unwrap_or_else(|| format!("tab {terminal}"))),
            )
            .child(div().flex_none().text_size(px(11.5)).child(word))
            .into_any_element()
    }
}

/// The line under a filtered list: what it shows (or that nothing is), then the words after
/// the "all" link.
pub(crate) fn filter_words(tab: StateTab, shown: usize, others: usize) -> (String, String) {
    let head = match (shown, tab) {
        (0, StateTab::Wants) => "No agent wants you.".to_string(),
        (0, tab) => format!("No {} agents.", tab.word()),
        (_, StateTab::Wants) => "Showing agents that want you.".to_string(),
        (_, tab) => format!("Showing {} agents.", tab.word()),
    };
    (head, format!("shows the other {others}."))
}

/// A space heading's count: "2" open, "2 agents" folded; filtered, "1 of 2", or "none
/// working" when none of its agents is on the list.
pub(crate) fn count_words(tab: StateTab, matches: usize, total: usize, open: bool) -> String {
    match (tab, matches, open) {
        (StateTab::All, _, true) => total.to_string(),
        (StateTab::All, _, false) => state::count(total, "agent"),
        (tab, 0, _) => format!("none {}", tab.word()),
        _ => format!("{matches} of {total}"),
    }
}

/// A space's name as its heading draws it: This Mac's whole, another machine's without its
/// machine (said beside it).
fn space_name(space: &qoop_runtime::control::WorkspaceRow) -> String {
    if state::machine_of(space.id) == 0 {
        space.name.clone()
    } else {
        state::short_space(space)
    }
}

/// A small square button with an icon: `+`, `...`.
fn icon_button(id: impl Into<ElementId>, label: &str, icon: Icon, t: &Theme) -> ui::Control {
    ui::control(id, Role::Button, label.to_string(), t)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .hover(|s| ui::paint(s, t.hover()))
        .child(ui::icon(icon, 14., c(t.t1)))
}

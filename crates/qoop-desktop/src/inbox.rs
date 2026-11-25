//! The Inbox page: everything that needs the owner as one list, one card per open loop, oldest
//! first, from the shared rules (`qoop_tui::inbox`). Digits answer the card with the focus ring,
//! `u` undoes, a pick waits its 3 s in the hold bar at the foot of the page, and the quiet
//! sections (With qass, Finished, Answered today) sit below the list.
//!
//! The page is a page like any other: the session open before it keeps its terminal alive
//! behind it, and `ctrl-a i` or Escape goes back to it.

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

impl Desk {
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
    /// list when it left, never past the end.
    fn cursor_in(&self, items: &[&Item]) -> Option<usize> {
        let by_slot = self
            .inbox_cursor
            .as_ref()
            .and_then(|slot| items.iter().position(|i| &i.slot == slot));
        by_slot.or_else(|| (!items.is_empty()).then(|| self.cursor_at.min(items.len() - 1)))
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
            if let Some(at) = self.cursor_in(&items).map(|at| (at + 1) % items.len()) {
                let slot = items[at].slot.clone();
                // The card takes the keyboard focus too, so the list scrolls it into view.
                let handle = self.card_focus.borrow().get(&slot).cloned();
                self.set_cursor(slot, at);
                if let Some(handle) = handle {
                    window.focus(&handle, cx);
                }
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

    /// The Inbox page.
    pub fn inbox_page(&self, list: &Inbox, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let items = self.shown(list);
        self.card_focus
            .borrow_mut()
            .retain(|slot, _| items.iter().any(|i| &i.slot == slot));
        let cursor = self.cursor_in(&items);
        let first_run = self.listed && self.model.agents().is_empty();
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
            .child(ui::scroll(
                div()
                    .id("inbox-body")
                    .debug_selector(|| "inbox-body".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px(px(26.))
                            .pb(px(20.))
                            .max_w(px(940.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .when(!self.listed, |d| d.children((0..3).map(|n| skeleton(n, t))))
                            .when_some(
                                state::loading_line(&self.model.spaces, &self.model.rows),
                                |d, line| {
                                    d.child(
                                        div()
                                            .id("loading-line")
                                            .role(Role::Status)
                                            .aria_label(line.clone())
                                            .text_size(px(12.5))
                                            .text_color(c(t.t2))
                                            .child(line),
                                    )
                                },
                            )
                            .when(self.listed && items.is_empty(), |d| {
                                d.child(self.inbox_empty(first_run, t, cx))
                            })
                            .children(
                                items.iter().enumerate().map(|(i, item)| {
                                    self.item_card(item, i, cursor == Some(i), t, cx)
                                }),
                            )
                            // The quiet lists belong to the whole Inbox: a filter hides them.
                            .when(self.inbox_filter == Filter::All, |d| {
                                d.child(self.quiet_sections(list, t, cx))
                            }),
                    ),
            ))
            .when(!self.holds.held.is_empty(), |d| {
                d.child(self.hold_bar(t, cx))
            })
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
    fn card_answers(
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
                Some(own) => d.child(div().id("own-answer").child(own.field.clone())),
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

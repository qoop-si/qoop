//! Answering: the question card docked under an agent that needs the owner, the panel beside
//! it, the 3 s hold with Undo, typed answers and the hand-off to qass. The card, the corner
//! note, the sidebar line and the Inbox all answer through `pick`, and a qass card's option
//! through `pick_card`: one hold, one Undo, one way of saying what became of it.
//!
//! A pick is never sent at once: it waits `state::HOLD` ("Sends in 3 s, Undo `u`"), then goes
//! as `AnswerMenu`, `AnswerMenuText` or `HandToQass`, and the card says "Sending" until the
//! daemon says it typed it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use gpui::{AnyElement, Context, Entity, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{
    AgentRow, AskInfo, AskOp, ControlRequest, ControlResponse, Hand, TerminalStream,
};

use crate::app::{Desk, Page};
use crate::field::Field;
use crate::link;
use crate::state::{self, Hold, Kind, Offer, Pick, Slot};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

/// The label of a hand qass took: its card then reads "With qass, handed 17:59".
const HANDED: &str = "Handed to qass";

/// What became of a pick after its hold, as the card says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Said {
    /// Asked under `id`; `busy` is asked again once.
    Sending {
        id: String,
        hold: Hold,
        retried: bool,
    },
    Sent {
        label: String,
        at: i64,
    },
    Refused(String),
}

/// The owner's own words for an agent's answer field, while its card is open: for the agent's
/// menu, or for one of its question cards (`card`).
pub struct OwnAnswer {
    pub terminal: TerminalStream,
    pub card: Option<String>,
    pub field: Entity<Field>,
}

impl Desk {
    /// The machine a row runs on, as its tile names it.
    fn machine_of_row(&self, row: &AgentRow) -> String {
        self.model
            .space(row.workspace)
            .map_or_else(|| "This Mac".to_string(), state::machine_name)
    }

    /// Why `row`'s answers are closed while its machine is not reached, by the cause: this Mac's
    /// daemon when it is lost (every machine is reached through it), else the link to the
    /// machine the row runs on.
    pub(crate) fn stale_reason(&self, row: &AgentRow) -> String {
        if self.lost_at.is_some() || state::machine_of(row.workspace) == 0 {
            state::stale_reason(None)
        } else {
            state::stale_reason(Some(&self.machine_of_row(row)))
        }
    }

    /// Why "Let qass handle it" cannot be pressed for `row`, or `None` when it can: the one rule
    /// (`Model::qass_reason`), This Mac's qass taking remote agents' questions too.
    pub fn qass_reason(&self, row: &AgentRow) -> Option<String> {
        self.model.qass_reason(row)
    }

    /// This Mac's hand id, made the first time a remote agent's question is handed.
    fn hand_id(&mut self) -> Option<String> {
        if self.model.hand_id.is_none() {
            self.model.hand_id = qoop_runtime::paths::hand_id().ok();
        }
        self.model.hand_id.clone()
    }

    /// Whether `row` takes typed answers: its machine has not refused them as too old.
    fn takes_words(&self, row: &AgentRow) -> bool {
        !self.too_old.contains(&state::machine_of(row.workspace))
    }

    /// Pick `offer` for `terminal`'s menu: held for 3 s, then sent.
    pub(crate) fn pick(
        &mut self,
        terminal: TerminalStream,
        offer: &Offer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.model.row(terminal) else {
            return;
        };
        let Some(menu) = row.menu.as_ref() else {
            return;
        };
        if self.as_of(row.workspace).is_some() {
            return;
        }
        if offer.pick == Pick::Qass && self.qass_reason(row).is_some() {
            return;
        }
        if matches!(offer.pick, Pick::Text { .. }) && !self.takes_words(row) {
            return;
        }
        let hold = Hold {
            slot: Slot::Agent(terminal),
            generation: row.generation,
            seal: menu.seal,
            pick: offer.pick.clone(),
            label: offer.label.clone(),
            until: Instant::now() + state::HOLD,
        };
        self.hold(hold, window, cx);
        // Picked on the session's card: the keys go back to its terminal, as after a typed
        // answer, so the next question's digit works and `u` still undoes.
        if !self.inbox && self.term_drawn() {
            window.focus(&self.term_focus, cx);
        }
    }

    /// Pick option `option` of qass's card: held for 3 s like any answer, then written.
    pub(crate) fn pick_card(
        &mut self,
        card: &AskInfo,
        option: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // qass's cards live on this Mac's daemon: nothing to answer while it is gone.
        if self.lost_at.is_some() {
            return;
        }
        let label = card
            .body
            .options
            .iter()
            .find(|o| o.key == option)
            .map_or_else(|| option.to_string(), |o| o.label.clone());
        let pick = Pick::Card {
            card: card.card.clone(),
            option: option.to_string(),
        };
        self.hold_card(&card.card, pick, label, window, cx);
    }

    /// Hand card `card` to qass to decide: held for 3 s like any answer, then written.
    pub(crate) fn card_to_qass(&mut self, card: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.lost_at.is_some() {
            return;
        }
        let pick = Pick::CardQass {
            card: card.to_string(),
        };
        self.hold_card(card, pick, "Hand to qass".into(), window, cx);
    }

    /// A pick on card `card`, held under the card.
    fn hold_card(
        &mut self,
        card: &str,
        pick: Pick,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hold = Hold {
            slot: Slot::Card(card.to_string()),
            generation: None,
            seal: 0,
            pick,
            label,
            until: Instant::now() + state::HOLD,
        };
        self.hold(hold, window, cx);
    }

    /// The open question cards of the agent in `terminal`, oldest first. None for a remote
    /// agent: a card names this Mac's terminal, and the mux numbers a remote's terminals its
    /// own way, so a number that matches is not the same agent.
    pub(crate) fn cards_of(&self, terminal: TerminalStream, workspace: i64) -> Vec<&AskInfo> {
        if state::machine_of(workspace) != 0 {
            return Vec::new();
        }
        qoop_tui::tabq::open_cards(&self.asks, terminal)
    }

    /// How many question cards `entry`'s agent waits on, beside its name; nothing at zero.
    pub(crate) fn card_count(
        &self,
        entry: state::Entry<'_>,
        place: &'static str,
        t: &Theme,
    ) -> Option<AnyElement> {
        let terminal = entry.terminal();
        let n = self.cards_of(terminal, entry.workspace()).len();
        (n > 0).then(|| {
            div()
                .debug_selector(move || format!("{place}-cards-{terminal}"))
                .flex_none()
                .px(px(6.))
                .rounded(px(8.))
                .bg(ca(t.needs_bg))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(t.needs_text))
                .child(n.to_string())
                .into_any_element()
        })
    }

    fn hold(&mut self, hold: Hold, _: &mut Window, cx: &mut Context<Self>) {
        self.said.remove(&hold.slot);
        self.holds.start(hold);
        self.after_hold(cx);
        cx.notify();
    }

    /// Hand every agent that needs the owner to qass, under one shared Undo.
    pub(crate) fn hand_all(
        &mut self,
        terminals: &[TerminalStream],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let until = Instant::now() + state::HOLD;
        for &terminal in terminals {
            let Some(row) = self.model.row(terminal) else {
                continue;
            };
            if self.qass_reason(row).is_some() {
                continue;
            }
            let hold = Hold {
                slot: Slot::Agent(terminal),
                generation: row.generation,
                seal: row.menu.as_ref().map_or(0, |m| m.seal),
                pick: Pick::Qass,
                label: "Let qass handle it".into(),
                until,
            };
            self.said.remove(&hold.slot);
            self.holds.start(hold);
        }
        let _ = window;
        self.after_hold(cx);
        cx.notify();
    }

    /// Wake once a second while anything is on hold: its count goes down, and what is due is
    /// sent. One timer for every hold, which keeps waking until nothing is held, so a timer that
    /// runs a moment early only delays a send by a tick.
    fn after_hold(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        self.tick_holds(cx);
    }

    fn tick_holds(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(1))
                .await;
            let _ = this.update(cx, |d, cx| {
                d.fire();
                if d.holds.held.is_empty() {
                    d.ticking = false;
                } else {
                    d.tick_holds(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Send the picks whose hold is up.
    pub(crate) fn fire(&mut self) {
        let due = self.holds.due(Instant::now());
        self.send(due);
    }

    /// Send what is on hold now, Undo or not: the window is closing.
    pub fn flush_holds(&mut self) {
        let all = self.holds.flush();
        self.send(all);
    }

    /// The hand-off line's Undo: the agents held for qass, and no other pick.
    pub(crate) fn undo_hand_off(&mut self, cx: &mut Context<Self>) {
        if self.holds.undo_hand_off() {
            cx.notify();
        }
    }

    /// Undo `slot`'s pick, or the newest one (with every pick held together with it).
    pub(crate) fn undo(&mut self, slot: Option<&Slot>, cx: &mut Context<Self>) -> bool {
        let undone = self.holds.undo(slot);
        if undone {
            cx.notify();
        }
        undone
    }

    fn send(&mut self, holds: Vec<Hold>) {
        // Hand-offs go one request per machine, naming every agent there.
        let mut hands: BTreeMap<usize, Vec<Hold>> = BTreeMap::new();
        for hold in holds {
            match &hold.pick {
                Pick::Qass => {
                    let machine = hold
                        .terminal()
                        .and_then(|t| self.model.row(t))
                        .map_or(0, |r| state::machine_of(r.workspace));
                    hands.entry(machine).or_default().push(hold);
                }
                _ => self.send_one(hold, false),
            }
        }
        for (machine, held) in hands {
            let id = link::fresh_id("hand");
            // Another machine's agent: for This Mac's qass, named by this Mac's hand id. With no
            // id (no home folder) it goes as before and that machine answers for itself.
            let away = if machine == 0 { None } else { self.hand_id() };
            self.ask(ControlRequest::HandToQass {
                id: id.clone(),
                hands: held
                    .iter()
                    .filter_map(|h| {
                        Some(Hand {
                            terminal: h.terminal()?,
                            generation: h.generation,
                            seal: (h.seal != 0).then_some(h.seal),
                            away: away.clone(),
                        })
                    })
                    .collect(),
            });
            for hold in held {
                let slot = hold.slot.clone();
                let sending = Said::Sending {
                    id: id.clone(),
                    hold,
                    retried: true,
                };
                self.said.insert(slot, sending);
            }
        }
    }

    fn send_one(&mut self, hold: Hold, retried: bool) {
        let id = link::fresh_id("answer");
        let terminal = hold.terminal().unwrap_or_default();
        let request = match &hold.pick {
            Pick::Key(key) => ControlRequest::AnswerMenu {
                id: id.clone(),
                terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
            },
            Pick::Text { key, text } => ControlRequest::AnswerMenuText {
                id: id.clone(),
                terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
                text: text.clone(),
            },
            // The owner's answer to qass's card; qass types it to the agent.
            Pick::Card { card, option } => ControlRequest::AskWrite {
                id: id.clone(),
                terminal: None,
                token: None,
                action: AskOp::Answer {
                    card: card.clone(),
                    option: Some(option.clone()),
                    text: None,
                    seen: None,
                    to_qass: false,
                },
            },
            Pick::CardText { card, text } => ControlRequest::AskWrite {
                id: id.clone(),
                terminal: None,
                token: None,
                action: AskOp::Answer {
                    card: card.clone(),
                    option: None,
                    text: Some(text.clone()),
                    seen: None,
                    to_qass: false,
                },
            },
            Pick::CardQass { card } => ControlRequest::AskWrite {
                id: id.clone(),
                terminal: None,
                token: None,
                action: AskOp::Answer {
                    card: card.clone(),
                    option: None,
                    text: None,
                    seen: None,
                    to_qass: true,
                },
            },
            Pick::Qass => return,
        };
        self.ask(request);
        let slot = hold.slot.clone();
        self.said.insert(slot, Said::Sending { id, hold, retried });
    }

    /// The picks a sending answer was asked under `id`, with their holds.
    fn sending(&self, id: &str) -> Vec<(Hold, bool)> {
        self.said
            .values()
            .filter_map(|s| match s {
                Said::Sending {
                    id: i,
                    hold,
                    retried,
                } if i == id => Some((hold.clone(), *retried)),
                _ => None,
            })
            .collect()
    }

    /// An answer to an answer: whether it was one of ours.
    pub(crate) fn answer_reply(&mut self, reply: &ControlResponse, cx: &mut Context<Self>) -> bool {
        let id = reply.request_id();
        let mine = self.sending(id);
        if mine.is_empty() {
            return false;
        }
        let now = crate::app::unix_now();
        for (hold, retried) in mine {
            let terminal = hold.terminal();
            // The agent put up its next question before this answer came back: what happened
            // to the old one is not said under the new card.
            let moved_on = hold.seal != 0
                && terminal
                    .and_then(|t| self.model.row(t))
                    .and_then(|r| r.menu.as_ref())
                    .is_some_and(|m| m.seal != hold.seal);
            if moved_on {
                self.said.remove(&hold.slot);
                continue;
            }
            let machine = terminal
                .and_then(|t| self.model.row(t))
                .map_or_else(|| "This Mac".to_string(), |r| self.machine_of_row(r));
            let said = match reply {
                ControlResponse::AskWritten { .. }
                    if matches!(hold.pick, Pick::CardQass { .. }) =>
                {
                    Said::Sent {
                        label: "Handed to qass".into(),
                        at: now,
                    }
                }
                ControlResponse::MenuAnswered { .. } | ControlResponse::AskWritten { .. } => {
                    Said::Sent {
                        label: hold.label.clone(),
                        at: now,
                    }
                }
                ControlResponse::Handed { taken, .. }
                    if terminal.is_some_and(|t| taken.contains(&t)) =>
                {
                    Said::Sent {
                        label: HANDED.into(),
                        at: now,
                    }
                }
                ControlResponse::Handed { .. } => {
                    Said::Refused("qass did not take this one: the question had moved on.".into())
                }
                ControlResponse::Failed { reason, .. } if reason == "busy" && !retried => {
                    // Keys still queued for the terminal: asked once more, a moment later.
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(300))
                            .await;
                        let _ = this.update(cx, |d, cx| {
                            d.send_one(hold, true);
                            cx.notify();
                        });
                    })
                    .detach();
                    continue;
                }
                ControlResponse::Failed { reason, .. } => {
                    let m = terminal
                        .and_then(|t| self.model.row(t))
                        .map_or(0, |r| state::machine_of(r.workspace));
                    if reason.contains("unknown variant") && matches!(hold.pick, Pick::Text { .. })
                    {
                        self.too_old.insert(m);
                    }
                    // A hand for This Mac's qass that the other machine answered as if it were
                    // for its own: its qoop predates such hands. Not offered there again.
                    let older = hold.pick == Pick::Qass
                        && m != 0
                        // Only a hand that went with this Mac's id (`away`) says the far qoop is older.
                        && self.model.hand_id.is_some()
                        && (reason == "qass_not_running"
                            || reason == "not_watching"
                            || reason.contains("unknown variant"));
                    if older {
                        self.model.no_hands.insert(m);
                        Said::Refused(state::refusal_words(
                            "unknown variant",
                            &machine,
                            &hold.pick,
                        ))
                    } else {
                        Said::Refused(state::refusal_words(reason, &machine, &hold.pick))
                    }
                }
                _ => continue,
            };
            self.said.insert(hold.slot.clone(), said);
        }
        cx.notify();
        true
    }

    /// After a new agent list: a pick whose menu changed or went is dropped, and the card says
    /// who answered; the digits of a new menu wait out their arming.
    pub(crate) fn menus_changed(&mut self) {
        let dropped = self.holds.menus(&self.model.rows, Instant::now());
        for hold in dropped {
            let by = hold
                .terminal()
                .and_then(|t| self.model.row(t))
                .and_then(|r| r.answered.as_ref())
                .map(|a| format!("Answered on the {}: {}.", by_word(&a.by), a.label));
            let said = by.unwrap_or_else(|| {
                state::refusal_words("already_answered", "This Mac", &hold.pick)
            });
            self.said.insert(hold.slot.clone(), Said::Refused(said));
        }
        // An answer field for an agent that no longer asks closes; for a card, once the card
        // is no longer open.
        let gone = self.own.as_ref().is_some_and(|own| match &own.card {
            Some(card) => !self
                .asks
                .iter()
                .any(|a| a.card == *card && a.status == "open"),
            None => !self
                .model
                .row(own.terminal)
                .is_some_and(|r| r.menu.is_some()),
        });
        if gone {
            self.own = None;
        }
    }

    /// A digit typed while a question card is on screen: that answer, once the menu is armed.
    pub(crate) fn digit(&mut self, digit: u8, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((terminal, offers, seal)) = self.card_offers() else {
            return false;
        };
        if !self.holds.armed(terminal, seal, Instant::now()) {
            // Taken, and dropped: a key meant for the screen before this menu.
            return true;
        }
        if let Some(offer) = offers.iter().find(|o| o.digit == Some(digit)) {
            let offer = offer.clone();
            self.pick(terminal, &offer, window, cx);
        }
        true
    }

    /// The question card's agent, its answers and its menu, when one is on screen.
    pub(crate) fn card_offers(&self) -> Option<(TerminalStream, Vec<Offer>, u64)> {
        let Page::Session(terminal) = self.page else {
            return None;
        };
        let row = self.model.row(terminal)?;
        let menu = row.menu.as_ref().filter(|_| Kind::of(row).needs_you())?;
        Some((terminal, state::offers(menu), menu.seal))
    }

    /// Open the field for the owner's own answer to `terminal`'s menu, or to its card `card`.
    pub(crate) fn open_own(
        &mut self,
        terminal: TerminalStream,
        card: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| Field::new("Type your own answer", cx));
        let focus = field.read(cx).focus.clone();
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        window.focus(&focus, cx);
        self.own = Some(OwnAnswer {
            terminal,
            card,
            field,
        });
        cx.notify();
    }

    /// Enter in the own-answer field: the words, held like any pick.
    pub(crate) fn own_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(own) = self.own.as_ref() else {
            return false;
        };
        if !own.field.read(cx).focus.is_focused(window) {
            return false;
        }
        let terminal = own.terminal;
        let text = own.field.read(cx).text.trim().to_string();
        // A card's own words: held under the card; the daemon says when they are too long.
        if let Some(card) = own.card.clone() {
            if !text.is_empty() {
                let label = format!("\"{}\"", short(&text, 40));
                self.own = None;
                self.hold_card(
                    &card,
                    Pick::CardText {
                        card: card.clone(),
                        text,
                    },
                    label,
                    window,
                    cx,
                );
                window.focus(&self.term_focus, cx);
            }
            return true;
        }
        let key = self
            .model
            .row(terminal)
            .and_then(|r| r.menu.as_ref())
            .and_then(state::text_key);
        let (Some(key), false) = (key, text.is_empty()) else {
            return true;
        };
        if text.chars().count() > qoop_runtime::menu::TEXT_MAX {
            let pick = Pick::Text { key, text };
            let said = state::refusal_words("too_long", "This Mac", &pick);
            self.said.insert(Slot::Agent(terminal), Said::Refused(said));
            cx.notify();
            return true;
        }
        let offer = Offer {
            digit: None,
            label: format!("\"{}\"", short(&text, 40)),
            detail: None,
            pick: Pick::Text { key, text },
        };
        self.own = None;
        self.pick(terminal, &offer, window, cx);
        window.focus(&self.term_focus, cx);
        true
    }

    /// One answer as a button: its digit, its words, and what it does when pressed.
    pub(crate) fn offer_button(
        &self,
        terminal: TerminalStream,
        offer: &Offer,
        (place, n): (&'static str, usize),
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.model.row(terminal);
        let stale = row
            .filter(|r| self.as_of(r.workspace).is_some())
            .map(|r| self.stale_reason(r));
        let reason = stale.or_else(|| match (&offer.pick, row) {
            (Pick::Qass, Some(r)) => self.qass_reason(r),
            (Pick::Text { .. }, Some(r)) if !self.takes_words(r) => Some(state::refusal_words(
                "unknown variant",
                &self.machine_of_row(r),
                &offer.pick,
            )),
            _ => None,
        });
        let label = match offer.digit {
            Some(d) => format!("{d}, {}", offer.label),
            None => offer.label.clone(),
        };
        let picked = offer.clone();
        ui::control(
            (place, (u64::from(terminal) << 8) | n as u64),
            Role::Button,
            match &reason {
                Some(why) => format!("{label}, unavailable: {why}"),
                None => label.clone(),
            },
            t,
        )
        .debug_selector(move || format!("{place}-{terminal}-{}", picked.digit.unwrap_or(0)))
        .flex()
        .items_start()
        .gap(px(8.))
        .min_h(px(28.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(8.))
        .map(|d| ui::paint(d, t.rest()))
        .when(reason.is_none(), |d| d.hover(|s| ui::paint(s, t.pressed())))
        .when(reason.is_some(), |d| ui::paint(d, t.off()))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .font_family(MONO_FONT)
                .text_color(c(t.t2))
                .child(offer.digit.map(|d| d.to_string()).unwrap_or_default()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(offer.label.clone())
                .when_some(offer.detail.clone(), |d, detail| {
                    d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(detail))
                })
                .when_some(reason, |d, why| {
                    d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(why))
                }),
        )
        .on_click({
            let offer = offer.clone();
            cx.listener(move |this, _, window, cx| this.pick(terminal, &offer, window, cx))
        })
        .into_any_element()
    }

    /// The hold, the sending or the outcome of `terminal`'s pick, as one line with Undo.
    pub(crate) fn said_line(
        &self,
        slot: &Slot,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let number = slot_number(slot);
        if let Some(hold) = self.holds.of(slot) {
            let left = hold
                .until
                .saturating_duration_since(Instant::now())
                .as_secs_f32()
                .ceil()
                .max(1.);
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .child(
                        div()
                            .id(("held", number))
                            .role(Role::Status)
                            .aria_label(format!(
                                "{}: sends in {}",
                                hold.label,
                                state::count(left as usize, "second")
                            ))
                            .flex_1()
                            .text_color(c(t.t1))
                            .child(format!("{}: sends in {left} s", hold.label)),
                    )
                    .child({
                        let undo = slot.clone();
                        ui::pill(("undo", number), "Undo", t)
                            .h(px(26.))
                            .child("Undo")
                            .child(ui::kbd("u", t))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.undo(Some(&undo), cx);
                            }))
                    })
                    .into_any_element(),
            );
        }
        let row = match slot {
            Slot::Agent(t) => self.model.row(*t),
            Slot::Card(_) => None,
        };
        // One name for a handed question, as the sidebar row says it.
        let qass = row.and_then(|r| self.model.qass_words(r));
        let text = match (self.said.get(slot), qass) {
            (Some(Said::Sending { .. }), _) => "Sending".to_string(),
            (Some(Said::Sent { label, at }), Some(qass)) if label == HANDED => {
                format!("{qass}, handed {}", state::clock(*at, self.zone))
            }
            // The hand is over (qass answered, left it, or stopped): no "Sent: Handed" left
            // standing over a question that waits on the owner again. A moment after the send
            // the row may not say it is with qass yet.
            (Some(Said::Sent { label, at }), None)
                if label == HANDED && !row.is_some_and(|r| r.with_qass) && self.now - *at > 5 =>
            {
                if row.is_some_and(|r| r.qass_missed) {
                    "qass did not take it".into()
                } else {
                    return None;
                }
            }
            (Some(Said::Sent { label, at }), _) => {
                format!("Sent: {label}, {}", state::clock(*at, self.zone))
            }
            (Some(Said::Refused(why)), _) => why.clone(),
            (None, Some(qass)) => qass,
            (None, None) if row.is_some_and(|r| r.qass_missed) => "qass did not take it".into(),
            (None, None) => return None,
        };
        Some(
            div()
                .id(("said", number))
                .role(Role::Status)
                .aria_label(text.clone())
                .text_size(px(12.5))
                .text_color(c(t.t1))
                .child(text)
                .into_any_element(),
        )
    }

    /// d-question: docked under the agent's screen while it needs the owner.
    /// A question another client answered: its options stay, closed and greyed, under who
    /// answered, with Dismiss.
    pub(crate) fn closed_card_view(
        &self,
        menu: &qoop_runtime::control::AgentMenu,
        line: &str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("closed-card")
            .debug_selector(|| "closed-card".to_string())
            .role(Role::Group)
            .aria_label(format!("Question: {}. {line}", menu.question))
            .flex_none()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .when(!menu.question.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(t.t2))
                        .child(menu.question.clone()),
                )
            })
            .children(state::offers(menu).into_iter().map(|o| {
                div()
                    .flex()
                    .gap(px(8.))
                    .min_h(px(28.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(8.))
                    .map(|d| ui::paint(d, t.off()))
                    .child(
                        div()
                            .w(px(14.))
                            .flex_none()
                            .font_family(MONO_FONT)
                            .child(o.digit.map(|d| d.to_string()).unwrap_or_default()),
                    )
                    .child(o.label)
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .child(ui::icon(Icon::Phone, 14., c(t.t2)))
                    .child(div().flex_1().child(line.to_string()))
                    .child(
                        ui::pill("closed-dismiss", "Dismiss", t)
                            .h(px(26.))
                            .child("Dismiss")
                            .child(ui::kbd(&format!("{} x", self.prefix_name()), t))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.closed_card = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn question_card(
        &self,
        row: &AgentRow,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(menu) = row.menu.as_ref() else {
            return div().into_any_element();
        };
        let terminal = row.terminal;
        let offers = state::offers(menu);
        let words = state::text_key(menu).is_some() && self.takes_words(row);
        // Its machine is not reached: the question is the last one seen, neutral, not answerable.
        let as_of = self.as_of(row.workspace);
        let stale = as_of.is_some();
        let own_open = self
            .own
            .as_ref()
            .filter(|o| o.terminal == terminal && o.card.is_none() && !stale);
        let kind = if stale {
            Kind::Status(qoop_domain::status::Status::Unknown)
        } else {
            Kind::of(row)
        };
        div()
            .id("question-card")
            .debug_selector(|| "question-card".to_string())
            .role(Role::Group)
            .aria_label(match &as_of {
                Some(when) => format!("Question, {when}: {}", menu.question),
                None => format!("Question: {}", menu.question),
            })
            .flex_none()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(if stale { t.line } else { t.needs_line }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(12.5))
                    .child(ui::mark(kind, t, 11.))
                    .child(
                        div()
                            .text_color(c(if stale { t.t2 } else { t.needs_text }))
                            .child("Needs you"),
                    )
                    .when_some(as_of.or_else(|| state::age_of(row, self.now)), |d, age| {
                        d.child(
                            div()
                                .debug_selector(|| "question-card-when".to_string())
                                .text_color(c(t.t2))
                                .child(age),
                        )
                    }),
            )
            .when(!menu.question.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(menu.question.clone()),
                )
            })
            .when(offers.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .child("Several boxes to tick: answer it in the terminal."),
                )
            })
            .children(
                offers
                    .iter()
                    .enumerate()
                    .map(|(n, o)| self.offer_button(terminal, o, ("card", n), t, cx)),
            )
            .when(words && !offers.is_empty(), |d| match own_open {
                Some(own) => d.child(div().id("own-answer").child(own.field.clone())),
                None if stale => d.child(
                    ui::control(
                        "own-open",
                        Role::Link,
                        format!(
                            "Something else? Type your own answer, unavailable: {}",
                            self.stale_reason(row)
                        ),
                        t,
                    )
                    .debug_selector(|| "own-open-off".to_string())
                    .tab_stop(false)
                    .cursor_default()
                    .min_h(px(24.))
                    .px(px(4.))
                    .text_size(px(12.))
                    .text_color(c(t.t3))
                    .child("Something else? Type your own answer"),
                ),
                None => d.child(
                    ui::link_button("own-open", "Something else? Type your own answer", t)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_own(terminal, None, window, cx)
                        })),
                ),
            })
            .when_some(self.said_line(&Slot::Agent(terminal), t, cx), |d, line| {
                d.child(line)
            })
            .into_any_element()
    }

    /// The panel beside the card: what is asked, qass on this one, after a pick, elsewhere.
    pub(crate) fn question_side(&self, row: &AgentRow, t: &Theme) -> AnyElement {
        let menu = row.menu.as_ref();
        let card = self
            .asks
            .iter()
            .find(|a| a.terminal == Some(row.terminal) && a.status == "open");
        let file = menu.and_then(|m| self.named_file(row, &m.context));
        let section = |title: &str| {
            div()
                .pt(px(6.))
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(t.t2))
                .child(title.to_string())
        };
        let text = |s: String| div().text_size(px(12.5)).text_color(c(t.t1)).child(s);
        div()
            .id("question-side")
            .role(Role::Complementary)
            .aria_label("About this question")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .child(section("What is asked"))
            .child(
                div()
                    .p(px(8.))
                    .rounded(px(8.))
                    .bg(c(crate::theme::TERMINAL_BG))
                    .font_family(MONO_FONT)
                    .text_size(px(11.5))
                    .text_color(c(crate::theme::DARK.t1))
                    .children(
                        menu.map(|m| m.context.clone())
                            .unwrap_or_default()
                            .into_iter()
                            .map(|l| div().whitespace_nowrap().overflow_hidden().child(l)),
                    ),
            )
            .when_some(file, |d, name| {
                d.child(text(format!("{name}: Open in your editor from Changes.")))
            })
            .child(section("qass on this one"))
            .child(match card {
                Some(card) => {
                    let pick = card.body.pick.as_ref().map(|p| {
                        let n = card
                            .body
                            .options
                            .iter()
                            .position(|o| o.key == p.key)
                            .map_or_else(|| p.key.clone(), |i| (i + 1).to_string());
                        format!("qass would pick {n}: {}", p.reason)
                    });
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(text(card.body.why.clone()))
                        .when_some(pick, |d, p| d.child(text(p)))
                        .into_any_element()
                }
                None => text(
                    "qass has not looked at this one. Let qass handle it: it answers if its rules \
                     allow, otherwise it comes back here."
                        .into(),
                )
                .into_any_element(),
            })
            .child(section("After a pick"))
            .child(text("It sends in 3 s; Undo u.".into()))
            .when(row.clients.iter().any(|c| c == "phone"), |d| {
                d.child(section("Elsewhere")).child(text(
                    "Also on your iPhone. The first answer wins; the other closes as Answered \
                     on the Mac."
                        .into(),
                ))
            })
            .into_any_element()
    }

    /// The first file the question names that is in the agent's checkout on this Mac.
    fn named_file(&self, row: &AgentRow, context: &[String]) -> Option<String> {
        let cwd = row
            .cwd
            .as_ref()
            .filter(|_| state::machine_of(row.workspace) == 0)?;
        context
            .iter()
            .flat_map(|l| l.split_whitespace())
            .map(|w| w.trim_matches(|c: char| !(c.is_alphanumeric() || "./_-".contains(c))))
            .filter(|w| w.contains('.') && !w.starts_with('.') && !w.contains(".."))
            .find(|w| Path::new(cwd).join(w).is_file())
            .map(str::to_string)
    }
}

/// A number for a pick's controls: the terminal for an agent, a hash of the card's id (above
/// every terminal number) for a card.
fn slot_number(slot: &Slot) -> u64 {
    use std::hash::{Hash, Hasher};
    match slot {
        Slot::Agent(t) => u64::from(*t),
        Slot::Card(card) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            card.hash(&mut h);
            (1 << 40) | (h.finish() & 0xffff_ffff)
        }
    }
}

/// "phone" as the card says where an answer came from.
fn by_word(kind: &str) -> &str {
    match kind {
        "computer" => "Mac",
        "phone" => "iPhone",
        "browser" => "browser",
        "attach" => "terminal",
        other => other,
    }
}

/// At most `most` characters of `text`, with an ellipsis when cut.
fn short(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    format!("{cut}\u{2026}")
}

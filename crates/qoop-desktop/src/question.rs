//! Answering: the question card docked under an agent that needs the owner, the panel beside
//! it, the 3 s hold with Undo, typed answers and the hand-off to qass. The card, the corner
//! note, the sidebar line and the Inbox all answer through `pick`.
//!
//! A pick is never sent at once: it waits `state::HOLD` ("Sends in 3 s, Undo `u`"), then goes
//! as `AnswerMenu`, `AnswerMenuText` or `HandToQass`, and the card says "Sending" until the
//! daemon says it typed it.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use gpui::{AnyElement, Context, Entity, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse, Hand, TerminalStream};

use crate::app::{Desk, Page};
use crate::field::{Field, FieldColors};
use crate::link;
use crate::state::{self, Hold, Kind, Offer, Pick};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui;

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

/// The owner's own words for an agent's answer field, while its card is open.
pub struct OwnAnswer {
    pub terminal: TerminalStream,
    pub field: Entity<Field>,
}

impl Desk {
    /// The machine a row runs on, as its tile names it.
    fn machine_of_row(&self, row: &AgentRow) -> String {
        self.model
            .space(row.workspace)
            .map_or_else(|| "This Mac".to_string(), state::machine_name)
    }

    /// Why "Let qass handle it" cannot be pressed for `row`, or `None` when it can. A remote
    /// agent's qass is asked; its refusal says so.
    pub fn qass_reason(&self, row: &AgentRow) -> Option<String> {
        if row.qass_skip {
            return Some("qass leaves this agent alone".into());
        }
        if state::machine_of(row.workspace) != 0 {
            return None;
        }
        match self.model.qass_line().as_str() {
            "not running" => Some("qass is not running".into()),
            "not watching" => Some("qass is not watching".into()),
            _ => None,
        }
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
        if offer.pick == Pick::Qass && self.qass_reason(row).is_some() {
            return;
        }
        if matches!(offer.pick, Pick::Text { .. }) && !self.takes_words(row) {
            return;
        }
        let hold = Hold {
            terminal,
            generation: row.generation,
            seal: menu.seal,
            pick: offer.pick.clone(),
            label: offer.label.clone(),
            until: Instant::now() + state::HOLD,
        };
        self.holds.start(hold);
        self.said.remove(&terminal);
        self.after_hold(window, cx);
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
                terminal,
                generation: row.generation,
                seal: row.menu.as_ref().map_or(0, |m| m.seal),
                pick: Pick::Qass,
                label: "Let qass handle it".into(),
                until,
            };
            self.holds.start(hold);
            self.said.remove(&terminal);
        }
        self.after_hold(window, cx);
        cx.notify();
    }

    /// Wake when the hold is up, and once a second meanwhile for its count.
    fn after_hold(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        for wait in [1, 2, 3] {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(wait))
                    .await;
                let _ = this.update(cx, |d, cx| {
                    d.fire();
                    cx.notify();
                });
            })
            .detach();
        }
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

    /// Undo `terminal`'s pick, or every pick on hold.
    pub(crate) fn undo(
        &mut self,
        terminal: Option<TerminalStream>,
        cx: &mut Context<Self>,
    ) -> bool {
        let undone = self.holds.undo(terminal);
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
                    let machine = self
                        .model
                        .row(hold.terminal)
                        .map_or(0, |r| state::machine_of(r.workspace));
                    hands.entry(machine).or_default().push(hold);
                }
                _ => self.send_one(hold, false),
            }
        }
        for (_, held) in hands {
            let id = link::fresh_id("hand");
            self.ask(ControlRequest::HandToQass {
                id: id.clone(),
                hands: held
                    .iter()
                    .map(|h| Hand {
                        terminal: h.terminal,
                        generation: h.generation,
                        seal: (h.seal != 0).then_some(h.seal),
                    })
                    .collect(),
            });
            for hold in held {
                let terminal = hold.terminal;
                let sending = Said::Sending {
                    id: id.clone(),
                    hold,
                    retried: true,
                };
                self.said.insert(terminal, sending);
            }
        }
    }

    fn send_one(&mut self, hold: Hold, retried: bool) {
        let id = link::fresh_id("answer");
        let request = match &hold.pick {
            Pick::Key(key) => ControlRequest::AnswerMenu {
                id: id.clone(),
                terminal: hold.terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
            },
            Pick::Text { key, text } => ControlRequest::AnswerMenuText {
                id: id.clone(),
                terminal: hold.terminal,
                generation: hold.generation,
                seal: hold.seal,
                key: key.clone(),
                text: text.clone(),
            },
            Pick::Qass => return,
        };
        self.ask(request);
        let terminal = hold.terminal;
        self.said
            .insert(terminal, Said::Sending { id, hold, retried });
    }

    /// The terminal a sending answer was asked under `id`, with its hold.
    fn sending(&self, id: &str) -> Vec<(TerminalStream, Hold, bool)> {
        self.said
            .iter()
            .filter_map(|(&t, s)| match s {
                Said::Sending {
                    id: i,
                    hold,
                    retried,
                } if i == id => Some((t, hold.clone(), *retried)),
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
        for (terminal, hold, retried) in mine {
            let machine = self
                .model
                .row(terminal)
                .map_or_else(|| "This Mac".to_string(), |r| self.machine_of_row(r));
            let said = match reply {
                ControlResponse::MenuAnswered { .. } => Said::Sent {
                    label: hold.label.clone(),
                    at: now,
                },
                ControlResponse::Handed { taken, .. } if taken.contains(&terminal) => Said::Sent {
                    label: "Handed to qass".into(),
                    at: now,
                },
                ControlResponse::Handed { .. } => Said::Refused("qass did not take it".into()),
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
                    if reason.contains("unknown variant") && matches!(hold.pick, Pick::Text { .. })
                    {
                        if let Some(row) = self.model.row(terminal) {
                            self.too_old.insert(state::machine_of(row.workspace));
                        }
                    }
                    Said::Refused(state::refusal_words(reason, &machine))
                }
                _ => continue,
            };
            self.said.insert(terminal, said);
        }
        cx.notify();
        true
    }

    /// After a new agent list: a pick whose menu changed or went is dropped, and the card says
    /// who answered; the digits of a new menu wait out their arming.
    pub(crate) fn menus_changed(&mut self) {
        let dropped = self.holds.menus(&self.model.rows, Instant::now());
        for hold in dropped {
            let by = self
                .model
                .row(hold.terminal)
                .and_then(|r| r.answered.as_ref())
                .map(|a| format!("Answered on the {}: {}", by_word(&a.by), a.label));
            self.said.insert(
                hold.terminal,
                Said::Refused(by.unwrap_or_else(|| "Already answered".into())),
            );
        }
        // An answer field for an agent that no longer asks closes.
        let gone = self.own.as_ref().is_some_and(|own| {
            !self
                .model
                .row(own.terminal)
                .is_some_and(|r| r.menu.is_some())
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

    /// Open the field for the owner's own answer to `terminal`'s menu.
    fn open_own(&mut self, terminal: TerminalStream, window: &mut Window, cx: &mut Context<Self>) {
        let t = self.theme();
        let colors = FieldColors {
            text: c(t.ink).into(),
            placeholder: c(t.t2).into(),
            caret: c(t.ink).into(),
            selection: ca(t.fill2).into(),
        };
        let field = cx.new(|cx| Field::new("Type your own answer", colors, cx));
        let focus = field.read(cx).focus.clone();
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        window.focus(&focus, cx);
        self.own = Some(OwnAnswer { terminal, field });
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
        let key = self
            .model
            .row(terminal)
            .and_then(|r| r.menu.as_ref())
            .and_then(state::text_key);
        let (Some(key), false) = (key, text.is_empty()) else {
            return true;
        };
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
        let reason = match (&offer.pick, row) {
            (Pick::Qass, Some(r)) => self.qass_reason(r),
            (Pick::Text { .. }, Some(r)) if !self.takes_words(r) => Some(state::refusal_words(
                "unknown variant",
                &self.machine_of_row(r),
            )),
            _ => None,
        };
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
        .bg(ca(t.fill))
        .when(reason.is_none(), |d| d.hover(|s| s.bg(ca(t.fill2))))
        .when(reason.is_some(), |d| d.text_color(c(t.t3)))
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
        terminal: TerminalStream,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(hold) = self.holds.of(terminal) {
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
                            .flex_1()
                            .text_color(c(t.t1))
                            .child(format!("{}: sends in {left} s", hold.label)),
                    )
                    .child(
                        ui::pill(("undo", u64::from(terminal)), "Undo", t)
                            .h(px(26.))
                            .child("Undo")
                            .child(ui::kbd("u", t))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.undo(Some(terminal), cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        let row = self.model.row(terminal);
        let text = match self.said.get(&terminal) {
            Some(Said::Sending { .. }) => "Sending".to_string(),
            Some(Said::Sent { label, at }) => {
                format!("Sent: {label}, {}", state::clock(*at, self.zone))
            }
            Some(Said::Refused(why)) => why.clone(),
            None if row.is_some_and(|r| r.with_qass) => "With qass".into(),
            None if row.is_some_and(|r| r.qass_missed) => "qass did not take it".into(),
            None => return None,
        };
        Some(
            div()
                .id(("said", u64::from(terminal)))
                .role(Role::Status)
                .aria_label(text.clone())
                .text_size(px(12.5))
                .text_color(c(t.t1))
                .child(text)
                .into_any_element(),
        )
    }

    /// d-question: docked under the agent's screen while it needs the owner.
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
        let own_open = self.own.as_ref().filter(|o| o.terminal == terminal);
        div()
            .id("question-card")
            .debug_selector(|| "question-card".to_string())
            .role(Role::Group)
            .aria_label(format!("Question: {}", menu.question))
            .flex_none()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.needs_line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_size(px(12.5))
                    .child(ui::mark(Kind::of(row), t, 11.))
                    .child(div().text_color(c(t.needs_text)).child("Needs you"))
                    .when_some(state::age_of(row, self.now), |d, age| {
                        d.child(div().text_color(c(t.t2)).child(age))
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
                Some(own) => d.child(
                    div()
                        .id("own-answer")
                        .p(px(6.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ca(t.line))
                        .child(own.field.clone()),
                ),
                None => d.child(
                    ui::link_button("own-open", "Something else? Type your own answer", t)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_own(terminal, window, cx)
                        })),
                ),
            })
            .when_some(self.said_line(terminal, t, cx), |d, line| d.child(line))
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
                    "qass has not looked at this one. 5 hands it over: it answers if its rules \
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

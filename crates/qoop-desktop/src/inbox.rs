//! The Inbox as the right panel (`ctrl-a i`): agents that need the owner with their answers,
//! qass's cards, the ones that failed or finished unread, and what qass handled today.

use gpui::{AnyElement, Context, FontWeight, Role, div, prelude::*, px};
use qoop_domain::Status;
use qoop_runtime::control::{AgentRow, AskInfo, AskOp, ControlRequest, TerminalStream};

use crate::app::{Desk, Page};
use crate::link;
use crate::state::{self, Kind, Pick};
use crate::theme::{Theme, c, ca};
use crate::ui::{self, Icon};

impl Desk {
    /// The agents the Inbox lists: Needs you, then Failed, then Done, as home ranks them.
    fn inbox_rows(&self) -> Vec<&AgentRow> {
        let mut rows: Vec<&AgentRow> = self
            .model
            .live()
            .filter(|r| {
                matches!(
                    Kind::of(r),
                    Kind::Status(Status::Blocked | Status::Failed | Status::Done)
                )
            })
            .collect();
        rows.sort_by_key(|r| Kind::of(r).rank());
        rows
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

    fn card_write(&self, action: AskOp) {
        self.ask(ControlRequest::AskWrite {
            id: link::fresh_id("card"),
            terminal: None,
            token: None,
            action,
        });
    }

    pub fn inbox_panel(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.inbox_rows();
        let needs: Vec<&AgentRow> = rows
            .iter()
            .copied()
            .filter(|r| Kind::of(r).needs_you())
            .collect();
        let unread: Vec<TerminalStream> = rows
            .iter()
            .filter(|r| !Kind::of(r).needs_you() && r.stop.is_some())
            .map(|r| r.terminal)
            .collect();
        let handable: Vec<TerminalStream> = needs
            .iter()
            .filter(|r| r.menu.is_some() && !r.with_qass && self.qass_reason(r).is_none())
            .map(|r| r.terminal)
            .collect();
        let handing = self
            .holds
            .held
            .iter()
            .filter(|h| h.pick == Pick::Qass)
            .count();
        let cards: Vec<&AskInfo> = self.asks.iter().filter(|a| a.status == "open").collect();
        let empty = rows.is_empty() && cards.is_empty();
        div()
            .id("inbox-panel")
            .role(Role::Complementary)
            .aria_label("Inbox")
            .absolute()
            .right_0()
            .top(px(56.))
            .bottom_0()
            .w(px(360.))
            .px(px(14.))
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .bg(c(t.panel))
            .border_l_1()
            .border_color(ca(t.line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Inbox"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(13.))
                            .text_color(c(t.t2))
                            .child(format!(
                                "{} needs you, {} unread",
                                needs.len() + cards.len(),
                                unread.len()
                            )),
                    )
                    .when(!unread.is_empty(), |d| {
                        let all = unread.clone();
                        d.child(
                            ui::link_button("mark-all-read", "Mark all read", t).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    for &terminal in &all {
                                        this.mark_seen(terminal);
                                    }
                                    cx.notify();
                                }),
                            ),
                        )
                    })
                    .child(ui::kbd("esc", t))
                    .child(
                        ui::control("inbox-close", Role::Button, "Close the Inbox", t)
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .child(ui::icon(Icon::Close, 14., c(t.t2)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inbox = false;
                                cx.notify();
                            })),
                    ),
            )
            .when(handable.len() >= 2 && handing == 0, |d| {
                let all = handable.clone();
                d.child(
                    ui::pill(
                        "hand-all",
                        &format!("Let qass take these {}", handable.len()),
                        t,
                    )
                    .child(ui::icon(Icon::Eye, 14., c(t.t1)))
                    .child(format!("Let qass take these {}", handable.len()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.hand_all(&all, window, cx)),
                    ),
                )
            })
            .when(handing >= 2, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.5))
                        .child(
                            div()
                                .flex_1()
                                .child(format!("Handing {handing} to qass in 3 s")),
                        )
                        .child(
                            ui::pill("undo-all", "Undo", t)
                                .h(px(26.))
                                .child("Undo")
                                .child(ui::kbd("u", t))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.undo(None, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .id("inbox-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .when(empty, |d| {
                        d.child(
                            div()
                                .pt(px(8.))
                                .text_size(px(13.))
                                .text_color(c(t.t2))
                                .child("Nothing needs you."),
                        )
                    })
                    .children(needs.iter().map(|r| self.needs_block(r, t, cx)))
                    .children(
                        cards
                            .iter()
                            .enumerate()
                            .map(|(i, a)| self.card_block(i, a, t, cx)),
                    )
                    .children(
                        rows.iter()
                            .filter(|r| !Kind::of(r).needs_you())
                            .map(|r| self.row_line(r, t, cx)),
                    )
                    .when(!self.acts.is_empty(), |d| {
                        d.child(
                            div()
                                .pt(px(10.))
                                .text_size(px(11.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c(t.t2))
                                .child("qass handled today"),
                        )
                        .children(self.acts.iter().take(20).map(|a| {
                            div()
                                .flex()
                                .gap(px(8.))
                                .text_size(px(12.))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(c(t.t3))
                                        .child(state::clock(a.at, self.zone)),
                                )
                                .child(div().flex_1().text_color(c(t.t1)).child(a.text.clone()))
                        }))
                    }),
            )
    }

    /// One agent's row: it opens the agent.
    fn row_head(&self, row: &AgentRow, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = Kind::of(row);
        let terminal = row.terminal;
        let space = self.model.space(row.workspace);
        ui::control(
            ("inbox-row", u64::from(terminal)),
            Role::Link,
            self.model.row_label(state::Entry::Live(row), self.now),
            t,
        )
        .debug_selector(move || format!("inbox-row-{terminal}"))
        .flex()
        .gap(px(8.))
        .px(px(4.))
        .py(px(5.))
        .rounded(px(10.))
        .hover(|s| s.bg(ca(t.fill)))
        .child(div().pt(px(3.)).child(ui::mark(kind, t, 12.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(13.5)).child(state::title_of(row)))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .child(
                            div()
                                .text_color(c(ui::word_ink(kind, t)))
                                .child(kind.word()),
                        )
                        .when_some(space, |d, w| {
                            d.child(
                                div()
                                    .text_color(t.space(state::space_slot(w)))
                                    .child(w.name.clone()),
                            )
                        }),
                ),
        )
        .when_some(state::age_of(row, self.now), |d, age| {
            d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(age))
        })
        .on_click(
            cx.listener(move |this, _, window, cx| this.open(Page::Session(terminal), window, cx)),
        )
    }

    /// An agent that needs the owner: its question and its answers, answered from here.
    fn needs_block(&self, row: &AgentRow, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let terminal = row.terminal;
        let menu = row.menu.as_ref();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(6.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.needs_line))
            .child(self.row_head(row, t, cx))
            .when_some(menu.filter(|m| !m.question.is_empty()), |d, m| {
                d.child(
                    div()
                        .px(px(4.))
                        .text_size(px(13.))
                        .text_color(c(t.t1))
                        .child(m.question.clone()),
                )
            })
            .when_some(menu, |d, m| {
                d.children(
                    state::offers(m)
                        .iter()
                        .enumerate()
                        .map(|(n, o)| self.offer_button(terminal, o, ("inbox", n), t, cx)),
                )
            })
            .when_some(self.said_line(terminal, t, cx), |d, line| d.child(line))
            .into_any_element()
    }

    /// A card qass wrote: what the agent does, why it asks, the options with their reasons, and
    /// which qass would pick, as text: nothing is picked for the owner.
    fn card_block(
        &self,
        index: usize,
        card: &AskInfo,
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
        let dismiss = id.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(10.))
            .rounded(px(12.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.5))
                    .child(div().text_color(c(t.needs_text)).child("qass asks"))
                    .when_some(card.agent.clone(), |d, a| {
                        d.child(div().text_color(c(t.t2)).child(a))
                    }),
            )
            .child(div().text_size(px(13.5)).child(body.asks.clone()))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(c(t.t1))
                    .child(format!("Doing: {}", body.doing)),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(c(t.t1))
                    .child(format!("Why: {}", body.why)),
            )
            .children(body.options.iter().enumerate().map(|(n, o)| {
                let card_id = id.clone();
                let key = o.key.clone();
                ui::control(
                    ("card-option", ((index as u64) << 8) | n as u64),
                    Role::Button,
                    format!("{}, {}", n + 1, o.label),
                    t,
                )
                .flex()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(8.))
                .bg(ca(t.fill))
                .hover(|s| s.bg(ca(t.fill2)))
                .text_size(px(12.5))
                .child(
                    div()
                        .w(px(14.))
                        .text_color(c(t.t2))
                        .child((n + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(o.label.clone())
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(c(t.t2))
                                .child(o.reason.clone()),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.card_write(AskOp::Answer {
                        card: card_id.clone(),
                        option: Some(key.clone()),
                        text: None,
                        seen: None,
                    });
                    cx.notify();
                }))
            }))
            .when_some(pick, |d, p| {
                d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(p))
            })
            .child(
                ui::link_button(("card-dismiss", index as u64), "Dismiss", t).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.card_write(AskOp::Dismiss {
                            card: dismiss.clone(),
                        });
                        cx.notify();
                    }),
                ),
            )
            .into_any_element()
    }

    /// A failed or finished agent: it opens, and Mark read says its finish was seen.
    fn row_line(&self, row: &AgentRow, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let terminal = row.terminal;
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(div().flex_1().min_w_0().child(self.row_head(row, t, cx)))
            .when(row.stop.is_some(), |d| {
                d.child(
                    ui::link_button(("mark-read", u64::from(terminal)), "Mark read", t).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.mark_seen(terminal);
                            cx.notify();
                        }),
                    ),
                )
            })
            .into_any_element()
    }
}

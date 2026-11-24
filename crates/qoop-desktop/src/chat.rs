//! The chat drawer on the qass page: what qass's session says, as messages (`Messages`, read
//! again when `MessagesChanged` names qass's terminal), with a Terminal tab for the terminal
//! itself, and the ask bar. It opens over the page from the right and closes with Escape (on the
//! Chat tab, since an Escape typed into the Terminal tab belongs to the agent).
//!
//! The chat is read from qass's session file by the daemon that owns the disk; a daemon from
//! before `Messages`, a session it does not read and a qass that has not said which session it
//! is in each say so in words, and the Terminal tab is always there.

use std::cell::Cell;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, MouseButton, Pixels, Role, ScrollHandle,
    Window, div, point, prelude::*, px,
};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_tui::chat_view::{Bubble, Who, bubbles};

use crate::app::{Desk, Page};
use crate::field::Field;
use crate::link;
use crate::theme::{Theme, c, ca};
use crate::ui;

/// The newest items asked of qass's session: tool calls fill a session, so a page holds far more
/// of them than messages.
const ITEMS: u32 = 200;

/// A chat read again this often while the drawer shows: the daemon sends news only for a terminal
/// asked about in the last 30 seconds.
const REREAD: i64 = 20;

/// The drawer's two tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatTab {
    #[default]
    Chat,
    Terminal,
}

/// The drawer's state.
pub struct ChatDrawer {
    pub open: bool,
    pub tab: ChatTab,
    /// The messages as last read; `None` until the first answer.
    pub bubbles: Option<Vec<Bubble>>,
    /// Why the chat cannot be shown, in words.
    pub note: Option<String>,
    /// The read out, by id.
    asked: Option<String>,
    /// When a read was last sent.
    read_at: i64,
    pub ask: Entity<Field>,
    pub(crate) scroll: ScrollHandle,
    // The list stays at its newest message while the person is at the bottom of it; once they
    // scroll up it holds where it is, and it follows again when they come back to the bottom
    // (judged from where it was last drawn: `last_max`). Opened, it starts at the newest.
    /// How far the list could scroll when it was last drawn: "at the bottom" is judged against
    /// it, so a new message does not read as the person having scrolled away.
    last_max: Cell<Pixels>,
    /// How many messages were drawn last.
    drawn: Cell<usize>,
    /// News came while a read was out: one more read follows its answer.
    dirty: bool,
    /// The page's Chat button, which takes the focus back when the drawer closes.
    pub button: FocusHandle,
}

impl ChatDrawer {
    pub fn new(cx: &mut Context<Desk>) -> Self {
        let ask = cx.new(|cx| {
            let mut f = Field::new("Ask qass, or tell it what to do", cx);
            f.label = Some("Ask qass".into());
            f
        });
        cx.observe(&ask, |_, _, cx| cx.notify()).detach();
        ChatDrawer {
            open: false,
            tab: ChatTab::Chat,
            bubbles: None,
            note: None,
            asked: None,
            read_at: 0,
            ask,
            scroll: ScrollHandle::new(),
            last_max: Cell::new(px(0.)),
            drawn: Cell::new(0),
            dirty: false,
            button: cx.focus_handle(),
        }
    }

    /// Whether qass's terminal is the drawer's body.
    pub fn terminal(&self) -> bool {
        self.open && self.tab == ChatTab::Terminal
    }
}

/// What a refusal of `Messages` means for the person.
pub fn refusal_words(reason: &str) -> String {
    if qoop_runtime::control::too_old_for(reason, Some("Messages")) {
        return "qoop on This Mac is too old to show the chat: the Terminal tab has it.".into();
    }
    match reason {
        "no_session" => {
            "qass has not said which session it is in yet: the Terminal tab has it.".into()
        }
        "not_agent" => "qass is not an agent right now: the Terminal tab has it.".into(),
        "too_large" => "This session is too large for the chat: the Terminal tab has it.".into(),
        "unavailable" => "The chat could not be read just now.".into(),
        other => format!("The chat could not be read: {other}"),
    }
}

impl Desk {
    /// Open the drawer on `tab`: the chat is read, or qass's terminal comes on.
    pub(crate) fn open_chat(&mut self, tab: ChatTab, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.open = true;
        self.chat.tab = tab;
        // Opened at the newest message.
        self.chat.last_max.set(px(0.));
        self.chat.scroll.set_offset(point(px(0.), px(0.)));
        // The terminal comes on (or goes) with the tab.
        self.open(Page::Qass, window, cx);
        match tab {
            ChatTab::Chat => {
                self.ask_chat(true);
                let focus = self.chat.ask.read(cx).focus.clone();
                window.focus(&focus, cx);
            }
            ChatTab::Terminal => {
                if self.term_drawn() {
                    window.focus(&self.term_focus, cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn close_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat.open = false;
        self.open(Page::Qass, window, cx);
        // Back on the button that opened it (WEB-06), else the window.
        let button = self.chat.button.clone();
        if self.qass_terminal().is_some() {
            window.focus(&button, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Read qass's chat: now, or only if it is overdue. One read is out at a time: news while it
    /// is out is kept, and read again once its answer comes (a read never answered is given up
    /// after a while).
    pub(crate) fn ask_chat(&mut self, now: bool) {
        let Some(terminal) = self.qass_terminal() else {
            self.chat.bubbles = None;
            self.chat.asked = None;
            return;
        };
        let at = crate::app::unix_now();
        let recent = at - self.chat.read_at < REREAD;
        if self.chat.asked.is_some() && recent {
            self.chat.dirty |= now;
            return;
        }
        if !now && recent {
            return;
        }
        let id = link::fresh_id("chat");
        self.chat.asked = Some(id.clone());
        self.chat.read_at = at;
        self.chat.dirty = false;
        self.ask(ControlRequest::Messages {
            id,
            terminal,
            skip: 0,
            limit: ITEMS,
            revision: None,
        });
    }

    /// An answer to the chat read: whether it was. News that came while it was out is read now.
    pub(crate) fn chat_came(&mut self, answer: &ControlResponse, cx: &mut Context<Self>) -> bool {
        if self.chat.asked.as_deref() != Some(answer.request_id()) {
            return false;
        }
        self.chat.asked = None;
        match answer {
            ControlResponse::Messages { items, .. } => {
                self.chat.bubbles = Some(bubbles(items));
                self.chat.note = None;
            }
            ControlResponse::Failed { reason, .. } => {
                self.chat.note = Some(refusal_words(reason));
            }
            _ => return false,
        }
        if std::mem::take(&mut self.chat.dirty) {
            self.ask_chat(true);
        }
        cx.notify();
        true
    }

    /// The chat of `terminal` has news: read again when it is qass's and the drawer shows it.
    pub(crate) fn chat_changed(&mut self, terminal: u32) {
        if self.chat.open
            && self.chat.tab == ChatTab::Chat
            && self.qass_terminal() == Some(terminal)
        {
            self.ask_chat(true);
        }
    }

    /// Once a second: the open chat is read again when it is overdue.
    pub(crate) fn chat_tick(&mut self) {
        if self.chat.open && self.chat.tab == ChatTab::Chat && self.page == Page::Qass {
            self.ask_chat(false);
        }
    }

    /// Enter in the drawer's ask field, and Escape: it closes the drawer unless the terminal has
    /// the keys, where an Escape belongs to the agent.
    pub(crate) fn chat_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.chat.open || self.page != Page::Qass {
            return false;
        }
        let on_ask = self.chat.ask.read(cx).focus.is_focused(window);
        match key {
            "enter" if on_ask => {
                let text = self.chat.ask.read(cx).text.trim().to_string();
                if !text.is_empty() && self.tell_qass(text, window, cx) {
                    self.chat.ask.update(cx, |f, _| f.set_text(""));
                }
                true
            }
            "escape"
                if !(self.chat.tab == ChatTab::Terminal && self.term_focus.is_focused(window)) =>
            {
                self.close_chat(window, cx);
                true
            }
            _ => false,
        }
    }

    /// The drawer over the page: a scrim that closes it, and the panel on the right.
    pub(crate) fn chat_drawer(
        &self,
        t: &Theme,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = |id: &'static str,
                   label: &'static str,
                   which: ChatTab,
                   cx: &mut Context<Self>| {
            ui::chip(id, label, None, None, self.chat.tab == which, t)
                .debug_selector(move || id.to_string())
                .on_click(cx.listener(move |this, _, window, cx| this.open_chat(which, window, cx)))
        };
        let head = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(ui::qass_mark(16., t))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("qass"),
            )
            .child(
                div()
                    .id("chat-tabs")
                    .role(Role::RadioGroup)
                    .aria_label("Chat or terminal")
                    .flex()
                    .gap(px(4.))
                    .child(tab("chat-tab", "Chat", ChatTab::Chat, cx))
                    .child(tab("terminal-tab", "Terminal", ChatTab::Terminal, cx)),
            )
            .child(
                ui::pill("chat-close", "Close the chat", t)
                    .debug_selector(|| "chat-close".to_string())
                    .child("Close")
                    // Escape closes it, except while the terminal has the keys.
                    .when(!(self.chat.tab == ChatTab::Terminal && focused), |d| {
                        d.child(ui::kbd("esc", t).debug_selector(|| "chat-close-esc".to_string()))
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.close_chat(window, cx))),
            );
        let body: AnyElement = match self.chat.tab {
            ChatTab::Terminal => div()
                .flex_1()
                .min_h_0()
                .child(self.terminal_area(focused, cx))
                .into_any_element(),
            ChatTab::Chat => self.chat_messages(t, cx),
        };
        div()
            .id("chat-scrim")
            .absolute()
            .inset_0()
            .flex()
            .justify_end()
            .bg(ca(if t.dark { 0x00000073 } else { 0x26262640 }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_chat(window, cx)),
            )
            .child(
                div()
                    .id("chat-drawer")
                    .debug_selector(|| "chat-drawer".to_string())
                    .role(Role::Dialog)
                    .aria_label("qass chat")
                    // A click inside the panel is not a click on the scrim.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // The chat is a narrow column; qass's terminal gets the page's width, as it
                    // had before, so it is not squeezed to a few dozen columns.
                    .when(self.chat.tab == ChatTab::Chat, |d| d.w(px(400.)))
                    .when(self.chat.tab == ChatTab::Terminal, |d| d.w_full())
                    .max_w_full()
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(18.))
                    .bg(c(t.pop))
                    .border_l_1()
                    .border_color(ca(t.line))
                    .child(head)
                    .child(body)
                    .when(self.chat.tab == ChatTab::Chat, |d| {
                        d.child(self.ask_bar(true, t, cx))
                    }),
            )
            .into_any_element()
    }

    /// The messages, oldest first, newest at the foot.
    fn chat_messages(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let said = |text: &str, ink: u32| {
            div()
                .id("chat-note")
                .role(Role::Status)
                .aria_label(text.to_string())
                .text_size(px(13.))
                .text_color(c(ink))
                .whitespace_normal()
                .child(text.to_string())
        };
        let list = match (&self.chat.bubbles, &self.chat.note, self.qass_terminal()) {
            (_, _, None) => {
                return said(
                    "qass is not running. Start it from the top of the page.",
                    t.t2,
                )
                .into_any_element();
            }
            (_, Some(note), _) => return said(note, t.t2).into_any_element(),
            (None, None, _) => return said("Reading the chat.", t.t2).into_any_element(),
            (Some(list), None, _) => list,
        };
        // Stay at the newest message while the person is at the bottom of the list; scrolled
        // up, it holds still, and it follows again once they are back at the bottom.
        let max = self.chat.scroll.max_offset().y;
        let at_bottom = self.chat.scroll.offset().y <= -self.chat.last_max.get() + px(4.);
        if at_bottom {
            self.chat.scroll.set_offset(point(px(0.), -max));
        }
        // The list's size is known only once it is laid out, so a list that grew (or a window
        // that changed) is drawn once more to settle on the foot.
        if max != self.chat.last_max.get() || list.len() != self.chat.drawn.get() {
            self.chat.drawn.set(list.len());
            let desk = cx.entity();
            cx.defer(move |cx| desk.update(cx, |_, cx| cx.notify()));
        }
        self.chat.last_max.set(max);
        div()
            .id("chat-list")
            .debug_selector(|| "chat-list".to_string())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.chat.scroll)
            .flex()
            .flex_col()
            .gap(px(8.))
            .when(list.is_empty(), |d| {
                d.child(said("Nothing has been said in this session yet.", t.t2))
            })
            .children(list.iter().enumerate().map(|(i, b)| {
                match b {
                    Bubble::Said { by, text } => {
                        let you = *by == Who::You;
                        div()
                            .id(("chat-bubble", i as u64))
                            .debug_selector(move || format!("chat-bubble-{i}"))
                            .max_w(gpui::relative(0.9))
                            .flex_none()
                            .px(px(11.))
                            .py(px(8.))
                            .rounded(px(12.))
                            .text_size(px(13.))
                            .whitespace_normal()
                            .when(you, |d| d.self_end().bg(c(t.ink)).text_color(c(t.bg)))
                            .when(!you, |d| d.self_start().bg(c(t.card2)).text_color(c(t.t1)))
                            .flex()
                            .flex_col()
                            .children(
                                text.lines()
                                    .map(|l| div().min_h(px(16.)).child(l.to_string())),
                            )
                            .into_any_element()
                    }
                    Bubble::Note(text) => div()
                        .id(("chat-bubble", i as u64))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .whitespace_normal()
                        .child(text.clone())
                        .into_any_element(),
                    Bubble::Failed(text) => div()
                        .id(("chat-bubble", i as u64))
                        .text_size(px(12.5))
                        .text_color(c(t.fail_text))
                        .whitespace_normal()
                        .child(text.clone())
                        .into_any_element(),
                }
            }))
            .into_any_element()
    }
}

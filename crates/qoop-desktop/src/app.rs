//! The window: routing between pages, the sidebar, the top bar, the status line, the right
//! panel, the prefix keys, and what the link brings in.
//!
//! Pages are Home, a Session (one terminal), qass (its own terminal), Schedules and Settings;
//! the Inbox opens as the right panel on any of them. The prefix from `config.toml` (default
//! `ctrl-a`) belongs to the app: `ctrl-a c` New work, `ctrl-a i` Inbox, the prefix twice
//! sends it to the terminal on screen.

use std::borrow::Cow;
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    App, Bounds, Context, Entity, EntityInputHandler, FocusHandle, Focusable, FontWeight,
    KeyDownEvent, MouseButton, Pixels, Render, Role, TitlebarOptions, UTF16Selection, Window,
    WindowAppearance, WindowBounds, WindowOptions, div, linear_color_stop, linear_gradient, point,
    prelude::*, px, size,
};
use qoop_domain::Viewport;
use qoop_runtime::avatar::Avatar;
use qoop_runtime::control::{ControlRequest, ControlResponse, EventMessage, TerminalStream};
use qoop_runtime::daemon::SnapshotWire;
use qoop_runtime::schedule::Zone;
use qoop_tui::seen;
use tokio::sync::mpsc::UnboundedSender;

use crate::element::{BACKGROUND, Cells, FOREGROUND, TerminalElement, cells};
use crate::keys::{self, Press};
use crate::link::{self, Down, Latest, Up};
use crate::newwork::NewWork;
use crate::state::{self, Chip, Kind, Model};
use crate::theme::{MONO_FONT, TERMINAL_BG, Theme, UI_FONT, c, ca};
use crate::ui::{self, Icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Session(TerminalStream),
    Qass,
    Schedules,
    Settings,
}

/// The terminal on screen, with what its pane remembers between frames.
pub struct Term {
    pub terminal: TerminalStream,
    pub screen: Option<Rc<SnapshotWire>>,
    sent: Rc<Cell<(u16, u16)>>,
    viewport: Rc<Cell<Option<Viewport>>>,
}

pub struct Desk {
    pub model: Model,
    pub page: Page,
    pub inbox: bool,
    pub chip: Chip,
    pub sheet: Option<NewWork>,
    pub term: Option<Term>,
    /// The person's `config.toml`: the prefix byte and the theme (auto follows the system).
    prefix: u8,
    theme_pref: qoop_tui::config::Theme,
    pub dark: bool,
    prefix_pending: bool,
    pub up: UnboundedSender<Up>,
    latest: Latest,
    pub connected: bool,
    pub focus: FocusHandle,
    pub term_focus: FocusHandle,
    /// Text the input method composes for the terminal; what it commits is typed.
    marked: Option<String>,
    /// The person's last key or click here, for seen.
    last_input: Option<Instant>,
    look: Option<seen::Look>,
    pub now: i64,
    pub zone: Zone,
    pub my_pet: Option<Avatar>,
    cells: Cells,
    /// The agents New work offers: (name, command).
    pub offers: Vec<(String, String)>,
    /// New work asked for: the create's id, the line its terminal gets, and the space. Kept
    /// here, not on the sheet, so a sheet closed while it starts still gets its line.
    pub pending: Option<(String, String, Option<i64>)>,
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Desk {
    pub fn new(
        up: UnboundedSender<Up>,
        latest: Latest,
        config: &qoop_tui::config::Read,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut offered: Vec<(String, String)> = ["claude", "codex", "opencode"]
            .map(|a| (a.to_string(), a.to_string()))
            .to_vec();
        for l in &config.agents {
            if !offered.iter().any(|(name, _)| *name == l.name) {
                offered.push((l.name.clone(), l.command.clone()));
            }
        }
        let theme_pref = config.config.theme;
        Desk {
            model: Model::default(),
            page: Page::Home,
            inbox: false,
            chip: Chip::All,
            sheet: None,
            term: None,
            prefix: config.config.prefix,
            theme_pref,
            dark: dark_for(theme_pref, window.appearance()),
            prefix_pending: false,
            up,
            latest,
            connected: false,
            focus,
            term_focus: cx.focus_handle(),
            marked: None,
            last_input: None,
            look: None,
            now: unix_now(),
            zone: qoop_runtime::schedule::system_zone(),
            my_pet: qoop_runtime::avatar::load(),
            cells: cells(cx, 12.),
            offers: state::worktree_agents(&offered),
            pending: None,
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::pick(self.dark)
    }

    pub fn ask(&self, request: ControlRequest) {
        let _ = self.up.send(Up::Request(Box::new(request)));
    }

    /// Take in what the link said.
    pub fn apply(&mut self, down: Down, window: &mut Window, cx: &mut Context<Self>) {
        match down {
            Down::Connected => {
                self.connected = true;
                // A New work asked for before the connection dropped: asked again, under its id.
                self.ask_pending();
                // A new connection, maybe a new daemon: say the size again.
                if let Some(term) = &self.term {
                    term.sent.set((0, 0));
                }
            }
            Down::Disconnected(why) => {
                if self.connected {
                    eprintln!("qoop-desktop: {why}; connecting again");
                }
                self.connected = false;
            }
            Down::Screens => {
                let screens =
                    std::mem::take(&mut *self.latest.lock().unwrap_or_else(|e| e.into_inner()));
                if let Some(term) = &mut self.term
                    && let Some((_, screen)) =
                        screens.into_iter().find(|(t, _)| *t == term.terminal)
                {
                    term.screen = Some(Rc::new(screen));
                }
            }
            Down::Answer(answer) => self.answer(*answer, window, cx),
            Down::Event(EventMessage::Focus { terminal }) => {
                self.open(Page::Session(terminal), window, cx);
                window.activate_window();
            }
            Down::Event(_) => {}
        }
    }

    fn answer(&mut self, answer: ControlResponse, window: &mut Window, cx: &mut Context<Self>) {
        match answer {
            ControlResponse::Agents { agents, .. } => self.model.rows = agents,
            ControlResponse::Workspaces { workspaces, .. } => self.model.spaces = workspaces,
            ControlResponse::Schedules { schedules, .. } => self.model.schedules = schedules,
            ControlResponse::QassWatch { on, .. } => self.model.qass_watch = Some(on),
            ControlResponse::TerminalCreated { id, terminal } => {
                self.started(&id, terminal, window, cx)
            }
            ControlResponse::Failed { id, reason } => self.refused(&id, reason),
            _ => {}
        }
    }

    /// Go to a page, showing its terminal's screens and dropping the last one's.
    pub fn open(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = match page {
            Page::Session(t) => Some(t),
            Page::Qass => self.qass_terminal(),
            _ => None,
        };
        if self.term.as_ref().map(|t| t.terminal) != terminal {
            if let Some(old) = self.term.take() {
                let _ = self.up.send(Up::Hide(vec![old.terminal]));
            }
            self.marked = None;
            if let Some(terminal) = terminal {
                self.term = Some(Term {
                    terminal,
                    screen: None,
                    sent: Rc::new(Cell::new((0, 0))),
                    viewport: Rc::new(Cell::new(None)),
                });
                let _ = self.up.send(Up::Show(vec![terminal]));
            }
        }
        self.page = page;
        if self.term.is_some() {
            window.focus(&self.term_focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn qass_terminal(&self) -> Option<TerminalStream> {
        let space = self
            .model
            .spaces
            .iter()
            .find(|w| w.name == state::QASS_SPACE)?;
        self.model
            .rows
            .iter()
            .find(|r| r.workspace == space.id && r.agent)
            .map(|r| r.terminal)
    }

    /// The prefix as `config.toml` names it, for the key hints: `ctrl-b`.
    pub fn prefix_name(&self) -> String {
        qoop_tui::keys::name_of(self.prefix)
    }

    fn is_prefix(&self, k: &gpui::Keystroke) -> bool {
        let letter = match self.prefix {
            p @ 1..=26 => (b'a' + p - 1) as char,
            _ => return false,
        };
        k.modifiers.control
            && !k.modifiers.platform
            && !k.modifiers.alt
            && k.key.len() == 1
            && k.key.starts_with(letter)
    }

    /// Before anything else hears a key: the prefix and what follows it, the sheet's focus
    /// trap, and the person's last key for seen.
    fn capture_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.last_input = Some(Instant::now());
        let k = &event.keystroke;
        if self.prefix_pending {
            self.prefix_pending = false;
            cx.stop_propagation();
            if self.is_prefix(k) {
                if let Some(term) = &self.term
                    && self.page != Page::Home
                    && self.connected
                {
                    let _ = self.up.send(Up::Input(term.terminal, vec![self.prefix]));
                }
                return;
            }
            match k.key.as_str() {
                "c" => self.new_work(None, window, cx),
                "i" => {
                    self.inbox = !self.inbox;
                    cx.notify();
                }
                _ => {}
            }
            return;
        }
        if self.is_prefix(k) {
            self.prefix_pending = true;
            cx.stop_propagation();
            return;
        }
        if self.sheet.is_some() {
            self.sheet_key(event, window, cx);
        }
    }

    /// Keys nobody else took: Tab moves focus, Escape closes the right panel.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        match k.key.as_str() {
            "tab" if k.modifiers.shift => window.focus_prev(cx),
            "tab" => window.focus_next(cx),
            "escape" if self.inbox => {
                self.inbox = false;
                cx.notify();
            }
            _ => {}
        }
    }

    /// A key for the terminal on screen.
    fn term_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.modifiers.platform && k.key == "q" {
            cx.quit();
            return;
        }
        // Nothing typed while the daemon is away: it would land, late, in whatever runs then.
        let Some(term) = self.term.as_ref().filter(|_| self.connected) else {
            return;
        };
        let app_cursor = term
            .screen
            .as_ref()
            .is_some_and(|s| s.modes.application_cursor);
        let press = Press {
            key: &k.key,
            key_char: k.key_char.as_deref(),
            ctrl: k.modifiers.control,
            alt: k.modifiers.alt,
            shift: k.modifiers.shift,
            cmd: k.modifiers.platform,
            option_is_meta: false,
        };
        if let Some(bytes) = keys::encode(press, app_cursor) {
            let _ = self.up.send(Up::Input(term.terminal, bytes));
            cx.stop_propagation();
        }
    }

    /// Once a second: the clock for ages and Next runs, and seen. Draws again only when
    /// something shown can have changed: every second while an age is under a minute, else
    /// every half minute.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = unix_now();
        let fresh = self.model.live().any(|r| {
            r.last_active_at_unix
                .is_some_and(|at| now.saturating_sub(at) < 60)
        });
        let redraw = now != self.now && (fresh || now / 30 != self.now / 30);
        self.now = now;
        let finish = match self.page {
            Page::Session(t) => self.model.finish(t),
            _ => None,
        };
        let present = finish.is_some()
            && seen::present(
                Some(window.is_window_active()),
                self.last_input,
                Instant::now(),
                seen::locked(),
            );
        let finish = finish.map(|(t, stop)| (t, stop.to_string()));
        if seen::step(
            &mut self.look,
            finish.as_ref().map(|(t, s)| (*t, s.as_str())),
            present,
            Instant::now(),
        ) && let Some((terminal, stop)) = finish
        {
            self.ask(ControlRequest::Seen {
                id: link::fresh_id("seen"),
                terminal,
                stop: Some(stop),
            });
        }
        if redraw {
            cx.notify();
        }
    }

    fn sidebar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = |id: &'static str,
                   icon: Icon,
                   label: &'static str,
                   page: Page,
                   cx: &mut Context<Self>| {
            let on = self.page == page;
            ui::control(id, Role::Link, label, t)
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(32.))
                .px(px(10.))
                .rounded(px(10.))
                .text_size(px(13.5))
                .text_color(c(if on { t.ink } else { t.t1 }))
                .when(on, |d| d.bg(ca(t.fill)))
                .hover(|s| s.bg(ca(t.fill)))
                .child(ui::icon(icon, 16., c(t.t2)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.open(page, window, cx)))
        };
        let next = state::next_runs(&self.model.schedules, &self.model.rows, self.zone, 1)
            .into_iter()
            .find(|r| r.running.is_none())
            .map(|r| format!("next {}", state::clock_at(r.at, self.now, self.zone)));
        div()
            .id("sidebar")
            .role(Role::Navigation)
            .aria_label("qoop")
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .w(px(248.))
            .pt(px(40.))
            .px(px(10.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .bg(c(t.sidebar))
            .border_r_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(10.))
                    .pt(px(4.))
                    .pb(px(10.))
                    .child(ui::logo(24.))
                    .child(
                        div()
                            .text_size(px(21.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("qoop"),
                    ),
            )
            .child(nav("nav-home", Icon::Home, "Home", Page::Home, cx))
            .child(
                nav(
                    "nav-schedules",
                    Icon::Clock,
                    "Schedules",
                    Page::Schedules,
                    cx,
                )
                .when_some(next, |d, next| {
                    d.child(
                        div()
                            .ml_auto()
                            .text_size(px(11.5))
                            .text_color(c(t.t2))
                            .child(next),
                    )
                }),
            )
            .child(nav(
                "nav-settings",
                Icon::Gear,
                "Settings",
                Page::Settings,
                cx,
            ))
            .child(
                div()
                    .id("tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .mt(px(8.))
                    .when(self.page != Page::Home, |d| d.child(self.tree(t, cx))),
            )
            .child(self.tiles(t))
    }

    /// The spaces with their agents, on every page but Home.
    fn tree(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.term.as_ref().map(|term| term.terminal);
        div()
            .flex()
            .flex_col()
            .gap(px(1.))
            .children(self.model.groups(Chip::All).into_iter().map(|g| {
                let slot = state::space_slot(g.space);
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(10.))
                            .pb(px(3.))
                            .px(px(10.))
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.space(slot))
                            .child(g.space.name.clone()),
                    )
                    .children(g.rows.into_iter().map(|entry| {
                        let kind = entry.kind();
                        let terminal = entry.terminal();
                        let on = shown == Some(terminal);
                        ui::control(
                            ("tree-row", terminal as u64),
                            Role::Link,
                            self.model.row_label(entry, self.now),
                            t,
                        )
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .min_h(px(28.))
                        .px(px(10.))
                        .rounded(px(10.))
                        .text_size(px(13.))
                        .when(on, |d| d.bg(ca(t.fill)))
                        .hover(|s| s.bg(ca(t.fill)))
                        .child(ui::mark(kind, t, 11.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(entry.title()),
                        )
                        .when_some(entry.age(self.now), |d, age| {
                            d.child(div().text_size(px(11.5)).text_color(c(t.t2)).child(age))
                        })
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open(Page::Session(terminal), window, cx)
                            },
                        ))
                    }))
            }))
    }

    /// One tile per space at the bottom left, naming its machine and its connection, and the
    /// phone line while a phone has an agent open.
    fn tiles(&self, t: &Theme) -> impl IntoElement {
        let spaces: Vec<_> = self
            .model
            .spaces
            .iter()
            .filter(|w| w.name != state::QASS_SPACE)
            .collect();
        div()
            .flex()
            .flex_col()
            .pt(px(10.))
            .border_t_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(spaces.into_iter().map(|w| {
                        let up = state::link_word(w) == "connected" && self.connected;
                        let word = if self.connected {
                            state::link_word(w)
                        } else {
                            "retrying"
                        };
                        let machine = state::machine_name(w);
                        div()
                            .id(("tile", w.id as u64))
                            .role(Role::Group)
                            .aria_label(format!("{}, {word}, {machine}", w.name))
                            .w(px(72.))
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .px(px(6.))
                            .py(px(5.))
                            .rounded(px(9.))
                            .bg(ca(t.fill))
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(t.space(state::space_slot(w)))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(
                                        w.name.rsplit(':').next().unwrap_or(&w.name).to_string(),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .text_size(px(11.))
                                    .text_color(c(t.t1))
                                    .child(
                                        div()
                                            .size(px(7.))
                                            .rounded_full()
                                            .when(up, |d| d.bg(c(t.done)))
                                            .when(!up, |d| d.border_1().border_color(c(t.t3))),
                                    )
                                    .child(word),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .text_size(px(10.5))
                                    .text_color(c(t.t2))
                                    .whitespace_nowrap()
                                    .when_some(
                                        self.my_pet
                                            .as_ref()
                                            .filter(|_| state::machine_of(w.id) == 0),
                                        |d, pet| d.child(ui::pet(pet, 0.8)),
                                    )
                                    .child(machine),
                            )
                    })),
            )
            .when(self.model.phone_here(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .pt(px(8.))
                        .px(px(6.))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(ui::icon(Icon::Phone, 14., c(t.t2)))
                        .child("iPhone, seen now"),
                )
            })
    }

    fn title(&self) -> (String, Option<String>) {
        match self.page {
            Page::Home => ("All agents".into(), Some(self.model.summary())),
            Page::Session(t) => {
                let row = self.model.row(t);
                let space = row
                    .and_then(|r| self.model.space(r.workspace))
                    .map(|w| w.name.clone());
                (
                    row.map_or_else(|| format!("tab {t}"), state::title_of),
                    space,
                )
            }
            Page::Qass => ("qass".into(), None),
            Page::Schedules => {
                let on = self.model.schedules.iter().filter(|s| !s.paused).count();
                let n = self.model.schedules.len();
                ("Schedules".into(), Some(format!("{n} schedules, {on} on")))
            }
            Page::Settings => (
                "Settings".into(),
                Some(state::tilde(
                    &qoop_tui::config::path().display().to_string(),
                )),
            ),
        }
    }

    fn top_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, sub) = self.title();
        let badge = self.model.badge();
        div()
            .id("top")
            .role(Role::Banner)
            .absolute()
            .left(px(248.))
            .right_0()
            .top_0()
            .h(px(56.))
            .flex()
            .items_center()
            .px(px(22.))
            .border_b_1()
            .border_color(ca(t.line))
            .child(
                div()
                    .id("title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(19.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .when_some(sub, |d, sub| {
                        d.child(
                            div()
                                .text_size(px(13.))
                                .text_color(c(t.t2))
                                .whitespace_nowrap()
                                .child(sub),
                        )
                    }),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        ui::control(
                            "qass-pill",
                            Role::Button,
                            format!("qass, {}", self.model.qass_line()),
                            t,
                        )
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(34.))
                        .px(px(14.))
                        .rounded_full()
                        .bg(ca(t.fill))
                        .text_size(px(13.))
                        .child(ui::icon(Icon::Eye, 15., c(t.t1)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("qass"))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(c(if self.model.qass_line().starts_with("watching") {
                                    t.work_text
                                } else {
                                    t.t2
                                }))
                                .child(self.model.qass_line()),
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open(Page::Qass, window, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        ui::pill("new-work", "New work", t)
                            .child(ui::icon(Icon::Plus, 15., c(t.t1)))
                            .child("New work")
                            .child(ui::kbd(&format!("{} c", self.prefix_name()), t))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_work(None, window, cx)),
                            ),
                    )
                    .child(
                        ui::pill(
                            "inbox",
                            &match badge {
                                0 => "Inbox".to_string(),
                                n => format!("Inbox, {n} waiting"),
                            },
                            t,
                        )
                        .aria_expanded(self.inbox)
                        .when(self.inbox, |d| d.bg(ca(t.fill2)))
                        .child(ui::icon(Icon::Inbox, 15., c(t.t1)))
                        .child("Inbox")
                        .when(badge > 0, |d| {
                            d.child(
                                div()
                                    .min_w(px(18.))
                                    .h(px(18.))
                                    .px(px(5.))
                                    .rounded_full()
                                    .bg(c(t.needs))
                                    .text_color(c(t.badge_fg))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .flex()
                                    .justify_center()
                                    .child(badge.to_string()),
                            )
                        })
                        .child(ui::kbd(&format!("{} i", self.prefix_name()), t))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.inbox = !this.inbox;
                            cx.notify();
                        })),
                    ),
            )
    }

    fn status_line(&self, t: &Theme) -> impl IntoElement {
        let right = if self.inbox { 360. } else { 0. };
        div()
            .absolute()
            .left(px(248.))
            .right(px(right))
            .bottom(px(4.))
            .h(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .font_family(MONO_FONT)
            .text_size(px(11.5))
            .text_color(c(t.t2))
            .whitespace_nowrap()
            .when(!self.connected, |d| d.child("connecting to the daemon"))
            .when_some(
                self.model.status_line().filter(|_| self.connected),
                |d, line| d.child(div().text_color(c(t.needs_text)).child(line)),
            )
            .child(format!("{}: [c new work] [i inbox]", self.prefix_name()))
    }

    /// The right panel: what waits on the person, newest state first. Answering comes later;
    /// a row opens its agent.
    fn inbox_panel(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<_> = self
            .model
            .live()
            .filter(|r| {
                matches!(
                    Kind::of(r),
                    Kind::Status(
                        qoop_domain::Status::Blocked
                            | qoop_domain::Status::Failed
                            | qoop_domain::Status::Done
                    )
                )
            })
            .collect();
        rows.sort_by_key(|r| Kind::of(r).rank());
        let needs = rows.iter().filter(|r| Kind::of(r).needs_you()).count();
        let unread = rows.len() - needs;
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
            .gap(px(4.))
            .bg(c(t.panel))
            .border_l_1()
            .border_color(ca(t.line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pb(px(6.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Inbox"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(14.))
                            .text_color(c(t.t2))
                            .child(format!("{needs} needs you, {unread} unread")),
                    )
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
            .when(rows.is_empty(), |d| {
                d.child(
                    div()
                        .pt(px(8.))
                        .text_size(px(13.))
                        .text_color(c(t.t2))
                        .child("Nothing needs you."),
                )
            })
            .children(rows.into_iter().map(|row| {
                let kind = Kind::of(row);
                let terminal = row.terminal;
                let space = self.model.space(row.workspace);
                ui::control(
                    ("inbox-row", terminal as u64),
                    Role::Link,
                    self.model.row_label(state::Entry::Live(row), self.now),
                    t,
                )
                .debug_selector(move || format!("inbox-row-{terminal}"))
                .flex()
                .gap(px(8.))
                .px(px(4.))
                .py(px(7.))
                .rounded(px(10.))
                .when(kind.needs_you(), |d| {
                    d.border_color(ca(t.needs_line)).bg(c(t.card))
                })
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
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open(Page::Session(terminal), window, cx)
                }))
            }))
    }

    fn terminal_area(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let term = self.term.as_ref();
        let up = self.up.clone();
        div()
            .id("terminal")
            .role(Role::Terminal)
            .aria_label("Terminal")
            .key_context("Terminal")
            .track_focus(&self.term_focus)
            .on_key_down(cx.listener(Self::term_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.term_focus, cx);
                    this.marked = None;
                }),
            )
            .size_full()
            .rounded(px(12.))
            .overflow_hidden()
            .bg(c(TERMINAL_BG))
            .p(px(8.))
            .child(TerminalElement {
                pane: 0,
                terminal: term.map(|t| t.terminal),
                screen: term.and_then(|t| t.screen.clone()),
                cells: self.cells.clone(),
                focused,
                stale: !self.connected,
                sent: term.map_or_else(|| Rc::new(Cell::new((0, 0))), |t| t.sent.clone()),
                viewport: term.map_or_else(|| Rc::new(Cell::new(None)), |t| t.viewport.clone()),
                resize: Rc::new(move |t, cols, rows| {
                    let _ = up.send(Up::Resize(t, cols, rows));
                }),
                input: Some((self.term_focus.clone(), cx.entity())),
            })
    }

    fn centre(&self, t: &Theme, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let right = if self.inbox { 360. } else { 0. };
        let body = div()
            .id("centre")
            .role(Role::Main)
            .absolute()
            .left(px(248.))
            .top(px(56.))
            .right(px(right))
            .bottom(px(26.));
        match self.page {
            Page::Home => body.overflow_y_scroll().child(self.home(t, cx)),
            Page::Session(_) => body.p(px(12.)).child(self.terminal_area(focused, cx)),
            Page::Qass if self.term.is_some() => {
                body.p(px(12.)).child(self.terminal_area(focused, cx))
            }
            Page::Qass => body.p(px(24.)).child(
                div()
                    .text_size(px(13.5))
                    .text_color(c(t.t2))
                    .child("qass is not running."),
            ),
            Page::Schedules => body
                .overflow_y_scroll()
                .p(px(24.))
                .child(self.schedule_list(t)),
            Page::Settings => body.p(px(24.)).child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child(qoop_tui::config::path().display().to_string()),
            ),
        }
    }

    fn schedule_list(&self, t: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .when(self.model.schedules.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(13.5))
                        .text_color(c(t.t2))
                        .child("No schedules yet."),
                )
            })
            .children(self.model.schedules.iter().map(|s| {
                let next = match (s.paused, s.next_at) {
                    (true, _) => "paused".to_string(),
                    (false, Some(at)) => state::until(at, self.now),
                    (false, None) => String::new(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .min_h(px(34.))
                    .border_b_1()
                    .border_color(ca(t.hair))
                    .text_size(px(12.5))
                    .child(
                        div()
                            .w(px(160.))
                            .font_family(MONO_FONT)
                            .child(s.name.clone()),
                    )
                    .child(div().w(px(80.)).text_color(c(t.t1)).child(s.agent.clone()))
                    .child(div().flex_1().text_color(c(t.t1)).child(s.when.clone()))
                    .child(div().text_color(c(t.t2)).child(next))
            }))
    }
}

/// Whether to draw dark: the file's theme, or the system's when it says auto.
pub fn dark_for(pref: qoop_tui::config::Theme, appearance: WindowAppearance) -> bool {
    match pref {
        qoop_tui::config::Theme::Dark => true,
        qoop_tui::config::Theme::Light => false,
        qoop_tui::config::Theme::Auto => {
            matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    }
}

/// Text from the input method for the terminal on screen: a dead key's accented letter, an
/// IME's committed word. Ordinary keys never come here; `term_key` sends them.
impl EntityInputHandler for Desk {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.marked.as_ref().map_or(0, |m| m.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..m.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.marked = None;
        if let Some(term) = &self.term
            && self.connected
            && !text.is_empty()
        {
            let _ = self
                .up
                .send(Up::Input(term.terminal, text.as_bytes().to_vec()));
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_string());
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let term = self.term.as_ref()?;
        let screen = term.screen.as_ref()?;
        let (top, left) = term.viewport.get().map_or((0, 0), |v| (v.row, v.col));
        let row = screen.cursor.0.saturating_sub(usize::from(top));
        let col = screen.cursor.1.saturating_sub(usize::from(left));
        Some(Bounds::new(
            point(
                element.origin.x + self.cells.width * col as f32,
                element.origin.y + self.cells.height * row as f32,
            ),
            size(self.cells.width, self.cells.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Focusable for Desk {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Desk {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme();
        let focused = self.term_focus.is_focused(window);
        let mut root = div()
            .id("desk")
            .role(Role::Application)
            .aria_label("qoop")
            .key_context("Desk")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::capture_key))
            .on_key_down(cx.listener(Self::key))
            .capture_any_mouse_down(
                cx.listener(|this, _, _, _| this.last_input = Some(Instant::now())),
            )
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(c(t.bg))
            .font_family(UI_FONT)
            .text_size(px(13.5))
            .text_color(c(t.ink))
            .when(t.dark, |d| {
                d.child(div().absolute().left_0().right_0().top_0().h(px(300.)).bg(
                    linear_gradient(
                        180.,
                        linear_color_stop(ca(0x965f374d), 0.),
                        linear_color_stop(ca(0x965f3700), 1.),
                    ),
                ))
            })
            .child(self.sidebar(&t, cx))
            .child(self.top_bar(&t, cx))
            .child(self.centre(&t, focused, cx))
            .child(self.status_line(&t));
        if self.inbox {
            root = root.child(self.inbox_panel(&t, cx));
        }
        if self.sheet.is_some() {
            root = root.child(self.sheet_view(&t, cx));
        }
        root
    }
}

/// Open the window on the isolated or real stack the environment names, and run until it
/// closes.
pub fn main() {
    let socket = match qoop_runtime::runtime_dir() {
        Ok(dir) => qoop_runtime::socket_path(&dir),
        Err(e) => {
            eprintln!("qoop-desktop: {e}");
            std::process::exit(1);
        }
    };
    let config = qoop_tui::config::read();
    let (up, mut down, latest) = link::start(socket, (FOREGROUND, BACKGROUND));
    gpui_platform::application()
        .with_assets(ui::Assets)
        .run(move |cx: &mut App| {
            let mut fonts: Vec<Cow<'static, [u8]>> = crate::theme::FONTS
                .iter()
                .map(|f| Cow::Borrowed(*f))
                .collect();
            fonts.push(Cow::Borrowed(qoop_tui::icons::FONT));
            if let Err(e) = cx.text_system().add_fonts(fonts) {
                eprintln!("qoop-desktop: fonts not loaded: {e}");
            }
            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(1024.), px(680.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("qoop".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(18.))),
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let desk =
                        cx.new(|cx| Desk::new(up.clone(), latest.clone(), &config, window, cx));
                    wire(&desk, &mut down, window, cx);
                    desk
                },
            );
            if opened.is_err() {
                eprintln!("qoop-desktop: could not open a window");
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}

/// Feed the link's news to the window, follow the system's appearance, and tick the clock.
fn wire(
    desk: &Entity<Desk>,
    down: &mut tokio::sync::mpsc::UnboundedReceiver<Down>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut down = std::mem::replace(down, tokio::sync::mpsc::unbounded_channel().1);
    let weak = desk.downgrade();
    window
        .spawn(cx, async move |cx| {
            while let Some(first) = down.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = down.try_recv() {
                    batch.push(more);
                }
                let applied = weak.update_in(cx, |this, window, cx| {
                    for d in batch {
                        this.apply(d, window, cx);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        })
        .detach();
    let weak = desk.downgrade();
    window
        .spawn(cx, async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    desk.update(cx, |_, cx| {
        cx.observe_window_appearance(window, |this: &mut Desk, window, cx| {
            this.dark = dark_for(this.theme_pref, window.appearance());
            cx.notify();
        })
        .detach();
    });
}

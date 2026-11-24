//! The shell around every page: the sidebar (Inbox, qass, the spaces folded to what waits, the
//! computers), the drag dividers that size it, the banner strip for what is wrong with the
//! connection, a page's header, and the computer page.
//!
//! Nothing here has a fixed width but the dividers' hit area: the sidebar and a drawer are
//! sized by dragging (or the arrow keys on a focused divider), within the clamps below, and
//! text wraps whole inside whatever width it has.

use gpui::{
    AnyElement, Context, FontWeight, KeyDownEvent, MouseButton, Role, Window, div, prelude::*, px,
};
use qoop_tui::inbox::Inbox;

use crate::app::{Desk, Page};
use crate::state::{self, Entry, Group, MachineState};
use crate::theme::{Theme, UI_FONT, c, ca};
use crate::ui::{self, Icon};

/// The sidebar's width at first, and the least and most it can be dragged to.
pub const SIDE_DEFAULT: f32 = 280.;
pub const SIDE_MIN: f32 = 200.;
pub const SIDE_MAX: f32 = 420.;
/// A drawer (the task strip, the review) beside the terminal: its width at first and its bounds.
pub const DRAWER_DEFAULT: f32 = 300.;
pub const REVIEW_DEFAULT: f32 = 560.;
pub const DRAWER_MIN: f32 = 240.;
pub const DRAWER_MAX: f32 = 760.;
/// What the main column keeps beside the sidebar, and the terminal beside a drawer: a divider
/// never drags past these, so no panel can squeeze the work out of the window.
pub const MAIN_MIN: f32 = 560.;
pub const TERM_MIN: f32 = 360.;
/// What the page spends around the terminal beside a drawer: the dividers and the padding.
pub const CHROME: f32 = 38.;
/// One arrow key on a focused divider, and with shift.
const NUDGE: f32 = 16.;
const NUDGE_FAR: f32 = 64.;

/// The sidebar width `want` asks for in a window `window` wide.
pub fn clamp_side(want: f32, window: f32) -> f32 {
    want.clamp(SIDE_MIN, SIDE_MAX.min(window - MAIN_MIN).max(SIDE_MIN))
}

/// The drawer width `want` asks for in a window `window` wide with a sidebar `side` wide.
pub fn clamp_drawer(want: f32, window: f32, side: f32) -> f32 {
    want.clamp(
        DRAWER_MIN,
        DRAWER_MAX
            .min(window - side - TERM_MIN - CHROME)
            .max(DRAWER_MIN),
    )
}

/// The divider being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The sidebar's right edge.
    Side,
    /// The left edge of the drawer beside the terminal.
    Drawer,
}

impl Desk {
    /// The sidebar's width in the window as it is now.
    pub(crate) fn side_px(&self) -> f32 {
        clamp_side(self.side_w, self.win_w)
    }

    /// The drawer's width in the window as it is now: the review's while it is open.
    pub(crate) fn drawer_px(&self) -> f32 {
        let want = if self.review.is_some() {
            self.review_w
        } else {
            self.drawer_w
        };
        clamp_drawer(want, self.win_w, self.side_px())
    }

    /// The divider `edge` is dragged to the window's `x`.
    pub(crate) fn drag_to(&mut self, edge: Edge, x: f32) {
        match edge {
            Edge::Side => self.side_w = clamp_side(x, self.win_w),
            Edge::Drawer => {
                // The drawer's right edge is the window's, less the page's padding.
                let want = clamp_drawer(self.win_w - x - 12., self.win_w, self.side_px());
                if self.review.is_some() {
                    self.review_w = want;
                } else {
                    self.drawer_w = want;
                }
            }
        }
    }

    /// An arrow key on a focused divider moves it, as a drag would.
    fn nudge(&mut self, edge: Edge, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let step = if k.modifiers.shift { NUDGE_FAR } else { NUDGE };
        let by = match k.key.as_str() {
            "left" => -step,
            "right" => step,
            _ => return,
        };
        // The sidebar grows to the right; a drawer, on the right of the page, to the left.
        let x = match edge {
            Edge::Side => self.side_px() + by,
            Edge::Drawer => self.win_w - self.drawer_px() - 12. + by,
        };
        self.drag_to(edge, x);
        cx.stop_propagation();
        cx.notify();
    }

    /// A drag handle between two panels: a hairline with a wider grip, resized by the mouse
    /// and by the arrow keys.
    pub(crate) fn divider(
        &self,
        edge: Edge,
        label: &'static str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let on = self.drag == Some(edge);
        ui::control(
            match edge {
                Edge::Side => "divider-side",
                Edge::Drawer => "divider-drawer",
            },
            Role::Splitter,
            label,
            t,
        )
        .aria_orientation(gpui::Orientation::Vertical)
        .aria_numeric_value(f64::from(match edge {
            Edge::Side => self.side_px(),
            Edge::Drawer => self.drawer_px(),
        }))
        .aria_min_numeric_value(f64::from(match edge {
            Edge::Side => SIDE_MIN,
            Edge::Drawer => DRAWER_MIN,
        }))
        .aria_max_numeric_value(f64::from(match edge {
            Edge::Side => SIDE_MAX.min(self.win_w - MAIN_MIN).max(SIDE_MIN),
            Edge::Drawer => DRAWER_MAX
                .min(self.win_w - self.side_px() - TERM_MIN - CHROME)
                .max(DRAWER_MIN),
        }))
        .debug_selector(move || match edge {
            Edge::Side => "divider-side".to_string(),
            Edge::Drawer => "divider-drawer".to_string(),
        })
        .flex_none()
        .w(px(7.))
        .h_full()
        .flex()
        .justify_center()
        .cursor_ew_resize()
        .child(div().w(px(if on { 2. } else { 1. })).h_full().bg(if on {
            c(t.focus)
        } else {
            ca(t.hair)
        }))
        .hover(|s| s.bg(ca(t.fill)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.drag = Some(edge);
                cx.notify();
            }),
        )
        .on_key_down(cx.listener(move |this, event, _, cx| this.nudge(edge, event, cx)))
    }

    /// What is wrong with the connection, above the page and wrapped whole: each machine not
    /// reached, This Mac's daemon down with Start qoop, or the connection still opening.
    pub(crate) fn banner_strip(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.banner();
        let lost = self.lost_at.is_some();
        let connecting = !self.connected && !lost;
        if banner.is_none() && !lost && !connecting {
            return None;
        }
        Some(
            div()
                .id("banner")
                .role(Role::Status)
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .px(px(22.))
                .py(px(8.))
                .border_b_1()
                .border_color(ca(t.line))
                .bg(ca(t.fill))
                .text_size(px(12.5))
                .text_color(c(t.t2))
                .when_some(banner, |d, banner| {
                    d.child(
                        div()
                            .id("reach-banner")
                            .role(Role::Status)
                            .aria_label(banner.clone())
                            .min_w_0()
                            .whitespace_normal()
                            .text_color(c(t.t1))
                            .child(banner),
                    )
                })
                // This Mac's daemon down: the step, where the state is said (FLOW-20).
                .when(lost, |d| {
                    d.child(
                        ui::link_button("start-qoop", "Start qoop", t)
                            .debug_selector(|| "start-qoop".into())
                            .flex_none()
                            .font_family(UI_FONT)
                            .on_click(cx.listener(|this, _, _, cx| this.start_qoop(cx))),
                    )
                    .when_some(self.qoop_said.clone(), |d, said| {
                        d.child(
                            div()
                                .id("qoop-said")
                                .role(Role::Status)
                                .aria_label(said.clone())
                                .min_w_0()
                                .whitespace_normal()
                                .child(said),
                        )
                    })
                })
                // No qoop here at all: nothing will ever answer, so say how to get it.
                .when(connecting, |d| {
                    d.child(div().min_w_0().whitespace_normal().child(if self.qoop_missing {
                        format!(
                            "qoop is not installed. Install it with the one-line install: {}",
                            state::INSTALL_LINE
                        )
                    } else {
                        "connecting to the daemon".to_string()
                    }))
                })
                .into_any_element(),
        )
    }

    /// A page's own header: its title and what it is about, wrapped whole.
    pub(crate) fn page_header(&self, t: &Theme) -> impl IntoElement {
        let (title, sub) = self.title();
        div()
            .id("title")
            .role(Role::Heading)
            .aria_level(1)
            .aria_label(title.clone())
            .flex_none()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(px(12.))
            .px(px(24.))
            .pt(px(18.))
            .pb(px(10.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when_some(sub, |d, sub| {
                d.child(div().text_size(px(13.)).text_color(c(t.t2)).child(sub))
            })
    }

    /// The sidebar: where qoop opens (Inbox, qass, settings, a new agent), the spaces folded to
    /// what waits and the agent on screen, and the computers pinned under the list.
    pub(crate) fn sidebar(
        &self,
        width: f32,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let nav = |id: &'static str,
                   icon: Icon,
                   label: &'static str,
                   page: Page,
                   badge: usize,
                   note: Option<String>,
                   cx: &mut Context<Self>| {
            let on = self.page == page;
            let said = match badge {
                0 => label.to_string(),
                n => format!("{label}, {n} waiting"),
            };
            ui::control(id, Role::Link, said, t)
                .flex()
                .items_center()
                .gap(px(10.))
                .min_h(px(32.))
                .px(px(10.))
                .rounded(px(10.))
                .text_size(px(13.5))
                .text_color(c(t.t1))
                .when(on, |d| ui::current(d, t))
                .hover(|s| ui::paint(s, t.hover()))
                .child(ui::icon(icon, 16., c(t.t2)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(label)
                        .when_some(note, |d, note| {
                            d.child(div().text_size(px(11.5)).text_color(c(t.t2)).child(note))
                        }),
                )
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
                .on_click(cx.listener(move |this, _, window, cx| this.open(page, window, cx)))
        };
        let qass_note = match self.model.qass_line().as_str() {
            line if line.starts_with("watching") => None,
            line => Some(line.to_string()),
        };
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".to_string())
            .role(Role::Navigation)
            .aria_label("qoop")
            .flex_none()
            .w(px(width))
            .h_full()
            .pt(px(40.))
            .px(px(10.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .bg(c(t.sidebar))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(6.))
                    .gap_y(px(4.))
                    .px(px(10.))
                    .pt(px(4.))
                    .pb(px(10.))
                    .child(ui::logo(22.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("qoop"),
                    )
                    .child(
                        ui::control("nav-settings", Role::Link, "Settings", t)
                            .size(px(30.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .when(self.page == Page::Settings, |d| ui::current(d, t))
                            .hover(|s| ui::paint(s, t.hover()))
                            .child(ui::icon(Icon::Gear, 16., c(t.t2)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(Page::Settings, window, cx)
                            })),
                    )
                    .child(
                        ui::pill("new-work", "New agent", t)
                            .h(px(30.))
                            .px(px(8.))
                            .child(ui::icon(Icon::Plus, 14., c(t.t1)))
                            .child(
                                ui::kbd("\u{2318}N", t)
                                    .id("kbd-new-agent")
                                    .aria_label("Command N"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_work(None, window, cx)),
                            ),
                    ),
            )
            .child(nav(
                "nav-inbox",
                Icon::Inbox,
                "Inbox",
                Page::Inbox,
                inbox.count,
                None,
                cx,
            ))
            .child(nav(
                "nav-qass",
                Icon::Eye,
                "qass",
                Page::Qass,
                0,
                qass_note,
                cx,
            ))
            // The spaces scroll as one list, and a Tab stop below the fold scrolls into view;
            // the computers stay under it.
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .pt(px(8.))
                    .child(ui::scroll(
                        div()
                            .id("sidebar-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(self.tree(inbox, t, cx)),
                    )),
            )
            .child(self.computers(t, cx))
    }

    /// The spaces with the agents that wait and the one on screen, the rest behind "N more",
    /// and a space where nothing waits as one line.
    fn tree(&self, inbox: &Inbox, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = match self.page {
            Page::Session(terminal) => Some(terminal),
            _ => None,
        };
        let waiting = inbox.terminals();
        let groups = self.model.groups();
        let nothing = self.listed && groups.is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(1.))
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
                groups
                    .into_iter()
                    .map(|g| self.space_block(g, inbox, &waiting, open, t, cx)),
            )
    }

    /// One space: its header, the agents it lists and its fold; or one line when quiet.
    fn space_block(
        &self,
        g: Group<'_>,
        inbox: &Inbox,
        waiting: &[u32],
        open: Option<u32>,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = g.space.id;
        let m = state::machine_of(id);
        let expanded = self.expanded.contains(&id);
        let fold = state::fold(&g, waiting, open, expanded);
        let slot = state::space_slot(g.space);
        let name = if m == 0 {
            g.space.name.clone()
        } else {
            state::short_space(g.space)
        };
        let machine = (m != 0).then(|| state::machine_name(g.space));
        let label = match &machine {
            Some(machine) => format!("{name}, on {machine}"),
            None => name.clone(),
        };
        let toggle = move |this: &mut Desk, _: &mut Window, cx: &mut Context<Desk>| {
            if !this.expanded.remove(&id) {
                this.expanded.insert(id);
            }
            cx.notify();
        };
        if fold.quiet {
            let agents = state::count(g.count, "agent");
            return ui::control(
                ("side-space", id as u64),
                Role::Button,
                format!("{label}, {agents}, show them"),
                t,
            )
            .debug_selector(move || format!("side-space-{id}"))
            .aria_expanded(false)
            .flex()
            .items_baseline()
            .flex_wrap()
            .gap_x(px(6.))
            .mt(px(6.))
            .px(px(10.))
            .py(px(3.))
            .rounded(px(8.))
            .hover(|s| ui::paint(s, t.hover()))
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.space(slot))
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child(match &machine {
                        Some(machine) => format!("{agents}, {machine}"),
                        None => agents,
                    }),
            )
            .child(ui::icon(Icon::Chevron, 12., c(t.t3)))
            .on_click(cx.listener(move |this, _, window, cx| toggle(this, window, cx)))
            .into_any_element();
        }
        let more = fold.more;
        div()
            .id(("side-group", id as u64))
            .flex()
            .flex_col()
            .child(
                div()
                    .id(("side-space", id as u64))
                    .debug_selector(move || format!("side-space-{id}"))
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(label)
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .gap_x(px(6.))
                    .pt(px(10.))
                    .pb(px(3.))
                    .px(px(10.))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.space(slot))
                            .child(name),
                    )
                    .when_some(machine, |d, machine| {
                        d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(machine))
                    }),
            )
            .children(
                fold.shown
                    .iter()
                    .map(|entry| self.tree_row(*entry, inbox, t, cx)),
            )
            .when(more > 0, |d| {
                d.child(
                    ui::link_button(("side-more", id as u64), &format!("{more} more"), t)
                        .debug_selector(move || format!("side-more-{id}"))
                        .ml(px(28.))
                        .on_click(cx.listener(move |this, _, window, cx| toggle(this, window, cx))),
                )
            })
            .when(expanded, |d| {
                d.child(
                    ui::link_button(("side-fewer", id as u64), "Show fewer", t)
                        .ml(px(28.))
                        .on_click(cx.listener(move |this, _, window, cx| toggle(this, window, cx))),
                )
            })
            .into_any_element()
    }

    /// The age a sidebar row shows. How long a waiting agent has waited is the Inbox's own wait
    /// (the card says the same), not when the agent was last active; the others keep their last
    /// activity.
    pub(crate) fn side_age(&self, entry: Entry<'_>, inbox: &Inbox) -> Option<String> {
        inbox
            .since_of(entry.terminal())
            .map(|at| state::short_span(self.now.saturating_sub(at).max(0)))
            .or_else(|| entry.age(self.now))
    }

    /// One agent of the sidebar: its mark and its title (wrapped whole), how many question
    /// cards it waits on, and its age. A handed agent wears qass's eye.
    fn tree_row(
        &self,
        entry: Entry<'_>,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = self.shown_kind(entry);
        let terminal = entry.terminal();
        let on = self.page == Page::Session(terminal);
        let handed = matches!(entry, Entry::Live(r) if self.model.with_qass(r));
        let age = self.side_age(entry, inbox);
        ui::control(
            ("tree-row", u64::from(terminal)),
            Role::Link,
            self.model.row_label(entry, self.now),
            t,
        )
        .debug_selector(move || format!("tree-row-{terminal}"))
        .flex()
        .items_start()
        .gap(px(8.))
        .min_h(px(28.))
        .px(px(10.))
        .py(px(4.))
        .rounded(px(10.))
        .text_size(px(13.))
        .when(on, |d| ui::current(d, t))
        .hover(|s| ui::paint(s, t.hover()))
        .child(div().pt(px(4.)).child(if handed {
            ui::icon(Icon::Eye, 11., c(t.t2)).into_any_element()
        } else {
            ui::mark(kind, t, 11.).into_any_element()
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .child(entry.title()),
        )
        .when_some(self.card_count(entry, "tree", t), |d, n| d.child(n))
        .when_some(age, |d, age| {
            d.child(
                div()
                    .flex_none()
                    .pt(px(1.))
                    .text_size(px(11.5))
                    .text_color(c(t.t2))
                    .child(age),
            )
        })
        .on_click(
            cx.listener(move |this, _, window, cx| this.open(Page::Session(terminal), window, cx)),
        )
        .into_any_element()
    }

    /// The computers, pinned under the spaces: This Mac and every other machine with how it is
    /// reached, its agent count, and Retry now where it is not.
    fn computers(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let machines = state::machines(&self.model.spaces);
        div()
            .id("computers")
            .role(Role::Group)
            .aria_label("Computers")
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(1.))
            .pt(px(4.))
            .border_t_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(6.))
                    .pb(px(2.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Computers"),
            )
            .children(
                machines
                    .into_iter()
                    .map(|(m, name, _)| self.computer_row(m, name, t, cx)),
            )
            .when(self.model.phone_here(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .px(px(10.))
                        .pt(px(6.))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .child(ui::icon(Icon::Phone, 14., c(t.t2)))
                        .child("iPhone, seen now"),
                )
            })
    }

    /// One computer: its name opens its page; its reach and Retry now sit beside it.
    fn computer_row(
        &self,
        m: usize,
        name: String,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let agents = self
            .model
            .agents()
            .iter()
            .filter(|e| state::machine_of(e.workspace()) == m)
            .count();
        let tail = format!(", {}", state::count(agents, "agent"));
        if m == 0 {
            return self.this_mac_line(name, tail, t, cx);
        }
        let on = self.page == Page::Computer(m);
        self.reach_line(m, name, tail, on, t, cx).into_any_element()
    }

    /// This Mac's own line: its daemon, answering or not.
    fn this_mac_line(
        &self,
        name: String,
        tail: String,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let up = self.lost_at.is_none() && self.connected;
        let on = self.page == Page::Computer(0);
        let (dot, ink) = if up {
            (t.done, t.done_text)
        } else {
            (t.needs, t.needs_text)
        };
        let word = if up { "online" } else { "not running" };
        ui::control("computer-0", Role::Link, format!("{name}, {word}{tail}"), t)
            .debug_selector(|| "computer-0".to_string())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(6.))
            .min_h(px(26.))
            .px(px(10.))
            .rounded(px(8.))
            .text_size(px(12.))
            .when(on, |d| ui::current(d, t))
            .hover(|s| ui::paint(s, t.hover()))
            .child(div().size(px(7.)).flex_none().rounded_full().bg(c(dot)))
            .when_some(self.pet_of(0), |d, pet| d.child(pet))
            .child(div().text_color(c(t.t1)).child(name))
            .child(
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(c(ink))
                    .child(format!("{word}{tail}")),
            )
            .on_click(cx.listener(|this, _, window, cx| this.open(Page::Computer(0), window, cx)))
            .into_any_element()
    }

    /// Machine `m` and how it is reached, in the words the banner uses, as a dot and its word:
    /// green online, grey parked or retrying, amber not reached with Retry now beside it. Its
    /// name opens the computer page. Why its link is down (the ssh error) is in the tooltip.
    /// `tail` follows the word.
    pub(crate) fn reach_line(
        &self,
        m: usize,
        name: String,
        tail: String,
        current: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let reach = self.machine_state(m);
        let up = reach == MachineState::Live;
        let away = state::unreached(reach);
        let retrying = reach == MachineState::Retrying;
        // While This Mac's daemon is down a retry of another machine goes nowhere: none offered.
        let behind = matches!(reach, MachineState::Behind { .. });
        let (dot, ink) = if up {
            (t.done, t.done_text)
        } else if away {
            (t.needs, t.needs_text)
        } else {
            (t.t3, t.t2)
        };
        let word = state::reach_words(reach, self.zone);
        let reason = self.link_reasons.get(&m).cloned();
        div()
            .id(("reach", m as u64))
            .debug_selector(move || format!("machine-{m}"))
            .when_some(reason.filter(|_| !up), |d, why| {
                d.tooltip(ui::tip_text(why, *t))
            })
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(6.))
            .min_h(px(26.))
            .px(px(10.))
            .rounded(px(8.))
            .text_size(px(12.))
            .when(current, |d| ui::current(d, t))
            .child(
                div()
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .when(up || away, |d| d.bg(c(dot)))
                    .when(!up && !away, |d| d.border_1().border_color(c(dot))),
            )
            .when_some(self.pet_of(m), |d, pet| d.child(pet))
            .child(
                ui::control(
                    ("computer-open", m as u64),
                    Role::Link,
                    format!("{name}, {word}{tail}"),
                    t,
                )
                .text_color(c(t.t1))
                .px(px(2.))
                .rounded(px(5.))
                .hover(|s| ui::paint(s, t.hover()))
                .child(name.clone())
                .on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.open(Page::Computer(m), window, cx)
                    }),
                ),
            )
            // The longest word ("not reached while This Mac is down, as of 22:36") wraps in
            // the sidebar's width rather than run out of it uncut.
            .child(
                div()
                    .debug_selector(move || format!("reach-word-{m}"))
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(c(ink))
                    .child(format!("{word}{tail}")),
            )
            .when(away || retrying, |d| {
                d.when(!behind, |d| d.child(self.retry_button(m, &name, t, cx)))
            })
    }

    /// Retry now: try machine `m` at once.
    fn retry_button(
        &self,
        m: usize,
        name: &str,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        ui::control(
            ("retry", m as u64),
            Role::Button,
            format!("Retry now, {name}"),
            t,
        )
        .debug_selector(move || format!("retry-{m}"))
        .flex_none()
        .whitespace_nowrap()
        .px(px(6.))
        .h(px(22.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_color(c(t.t1))
        .hover(|s| ui::paint(s, t.pressed()))
        .child("Retry now")
        .on_click(cx.listener(move |this, _, _, cx| {
            this.retry_machine(m);
            cx.notify();
        }))
    }

    /// The computer page (`dMachine`): how the machine is reached, Retry now, and its agents as
    /// they were last read.
    pub(crate) fn computer_page(&self, m: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = state::machines(&self.model.spaces)
            .into_iter()
            .find(|(n, _, _)| *n == m)
            .map_or_else(|| "Computer".to_string(), |(_, name, _)| name);
        let reach = self.machine_state(m);
        let away = state::unreached(reach);
        let as_of = self.model.spaces.iter().find_map(|w| {
            (state::machine_of(w.id) == m)
                .then(|| self.as_of(w.id))
                .flatten()
        });
        let (word, ink) = match (m, reach) {
            (0, _) if self.lost_at.is_some() => ("not running".to_string(), t.needs_text),
            (0, _) => ("online".to_string(), t.done_text),
            (_, state) => (
                state::reach_words(state, self.zone),
                if away {
                    t.needs_text
                } else if state == MachineState::Live {
                    t.done_text
                } else {
                    t.t2
                },
            ),
        };
        let why = match (m, reach) {
            (0, _) if self.lost_at.is_some() => {
                "qoop is not running on this Mac. Its agents keep the state they had; start qoop \
                 to answer them again."
                    .to_string()
            }
            (0, _) => "qoop on this Mac is answering.".to_string(),
            (_, MachineState::Live) => format!("qoop on {name} is answering."),
            (_, MachineState::Parked) => format!(
                "Nothing runs on {name} now, so qoop let its link go. Retry now reconnects it."
            ),
            (_, MachineState::Behind { .. }) => format!(
                "{name} is reached through qoop on This Mac, which is not running. Start qoop \
                 there first."
            ),
            (_, _) => format!(
                "It may be asleep or off the network, or qoop on it stopped. Its agents keep the \
                 state they had{}; answering and typing open again when it is back.",
                as_of
                    .as_ref()
                    .map_or(String::new(), |when| format!(" ({when})"))
            ),
        };
        let groups = self.model.groups();
        let rows: Vec<Entry<'_>> = groups
            .iter()
            .filter(|g| state::machine_of(g.space.id) == m)
            .flat_map(|g| g.rows.iter().copied())
            .collect();
        let retry = m != 0
            && (away || reach == MachineState::Retrying || reach == MachineState::Parked)
            && !matches!(reach, MachineState::Behind { .. });
        div()
            .id("computer-page")
            .debug_selector(|| "computer-page".to_string())
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(24.))
            .pt(px(18.))
            .pb(px(18.))
            .max_w(px(820.))
            .child(
                div()
                    .id("title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(name.clone())
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .child(ui::icon(Icon::Server, 20., c(ink)))
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .id("computer-state")
                            .text_size(px(13.))
                            .text_color(c(ink))
                            .child(word),
                    )
                    .when(retry, |d| {
                        d.child(
                            ui::pill(("retry", m as u64), "Retry now", t)
                                .debug_selector(move || format!("retry-page-{m}"))
                                .child("Retry now")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.retry_machine(m);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(m == 0 && self.lost_at.is_some(), |d| {
                        d.child(
                            ui::pill("start-qoop-page", "Start qoop", t)
                                .child("Start qoop")
                                .on_click(cx.listener(|this, _, _, cx| this.start_qoop(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(13.5))
                    .text_color(c(t.t2))
                    .whitespace_normal()
                    .child(why),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .pt(px(6.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(match &as_of {
                                Some(when) => format!("Its agents, {when}"),
                                None => "Its agents".to_string(),
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child(rows.len().to_string()),
                    ),
            )
            .when(rows.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(13.))
                        .text_color(c(t.t2))
                        .child("No agents there."),
                )
            })
            .children(rows.into_iter().map(|entry| {
                let terminal = entry.terminal();
                let kind = self.shown_kind(entry);
                ui::control(
                    ("computer-agent", u64::from(terminal)),
                    Role::Link,
                    self.model.row_label(entry, self.now),
                    t,
                )
                .debug_selector(move || format!("computer-agent-{terminal}"))
                .flex()
                .items_start()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(10.))
                .hover(|s| ui::paint(s, t.hover()))
                .child(div().pt(px(4.)).child(ui::mark(kind, t, 12.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.5)).whitespace_normal().child(
                            self.model.space(entry.workspace()).map_or_else(
                                || entry.title(),
                                |space| format!("{} {}", state::short_space(space), entry.title()),
                            ),
                        ))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(c(ui::word_ink(kind, t)))
                                .child(match entry {
                                    Entry::Live(row) => self.shown_word(row),
                                    Entry::Asleep(..) => kind.word().to_string(),
                                }),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open(Page::Session(terminal), window, cx)
                }))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag never leaves the sidebar or a drawer narrower or wider than its bounds, nor takes
    /// the room the main column or the terminal keeps; a window too small for the bounds gives
    /// the minimum, never a panic.
    #[test]
    fn a_divider_stays_inside_its_clamp() {
        assert_eq!(clamp_side(0., 1440.), SIDE_MIN);
        assert_eq!(clamp_side(280., 1440.), 280.);
        assert_eq!(clamp_side(900., 1440.), SIDE_MAX);
        // At the smallest window the sidebar leaves the main column its share first.
        assert_eq!(clamp_side(420., 1024.), 420.);
        assert_eq!(
            clamp_side(900., 900.),
            900. - MAIN_MIN,
            "the main column keeps its share"
        );
        assert_eq!(clamp_side(300., 100.), SIDE_MIN, "no room: the minimum");

        assert_eq!(clamp_drawer(0., 1440., 280.), DRAWER_MIN);
        assert_eq!(clamp_drawer(560., 1440., 280.), 560.);
        assert_eq!(clamp_drawer(2000., 1440., 280.), DRAWER_MAX);
        // The terminal keeps its width beside the dividers and the padding: the most a drawer
        // takes at 1024 is what is left after 280, 360 and the 38 of chrome.
        assert_eq!(clamp_drawer(700., 1024., 280.), 346.);
        assert_eq!(
            clamp_drawer(500., 600., 280.),
            DRAWER_MIN,
            "no room: the minimum"
        );
    }
}

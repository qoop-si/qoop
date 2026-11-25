//! The shell around every page: the sidebar (Inbox, qass, the spaces folded to what waits, the
//! computers), the drag dividers that size it, the banner strip for what is wrong with the
//! connection, a page's header, and the computer page.
//!
//! Nothing here has a fixed width but the dividers' hit area: the sidebar and a drawer are
//! sized by dragging (or the arrow keys on a focused divider), within the clamps below, and
//! text wraps whole inside whatever width it has.

use std::time::Duration;

use gpui::{
    AnyElement, App, Context, FontWeight, HighlightStyle, KeyDownEvent, MouseButton,
    MouseDownEvent, Role, StyledText, Window, div, prelude::*, px,
};
use qoop_domain::status::Status;
use qoop_runtime::avatar::Avatar;
use qoop_tui::inbox::Inbox;

use crate::app::{Desk, Page};
use crate::state::{self, Entry, Kind, MachineState};
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
/// The top band's least height: the column's first row and the page's. Its first line is
/// centred on the window buttons' centre (`traffic_light_position` 18, 18: 25 px down); a
/// title that wraps grows it downward.
pub const TOP_BAND: f32 = 50.;
/// Where the band's first line starts and how tall it is: its centre is the buttons'.
const BAND_TOP: f32 = 12.;
const BAND_LINE: f32 = 26.;
/// The band's right end (Next waiting, the gear, the pet), one width whatever the count:
/// that of its widest state, "Next waiting 99+" with its key. So the Inbox's count can never
/// move where the title wraps, and with it the terminal's size.
pub const BAND_RIGHT: f32 = 266.;
/// The room the window buttons take at the left of the band, before the fold toggle.
const BUTTONS: f32 = 78.;
/// One arrow key on a focused divider, and with shift.
const NUDGE: f32 = 16.;
const NUDGE_FAR: f32 = 64.;

/// One frame of the working mark's breath: two of the TUI's 80 ms ticks, so a breath takes
/// about 1.3 seconds there and here.
pub const FRAME: Duration = Duration::from_millis(160);

/// The marks' clock: the frame the working breath is at, and whether the timer that steps it
/// runs. The timer runs only while a mark moves.
#[derive(Debug, Default)]
pub struct Beat {
    pub frame: u32,
    running: bool,
}

impl Beat {
    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running
    }
}

/// Whether a state's mark moves: a turn going, or work running in the background.
pub fn moves(kind: Kind) -> bool {
    matches!(kind, Kind::Status(Status::Working | Status::Background))
}

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

/// A press on the band: one moves the window, two zoom it as the system's title bar setting
/// says (on macOS a drag area of GPUI's does nothing at this commit, so the window is moved
/// by hand). The controls in the band take their own presses first ([`no_drag`]).
fn drag_window(event: &MouseDownEvent, window: &mut Window, _: &mut App) {
    match event.click_count {
        1 => window.start_window_move(),
        2 => window.titlebar_double_click(),
        _ => {}
    }
}

/// A control in the band: a press on it is its own, never a drag of the window.
fn no_drag(control: ui::Control) -> ui::Control {
    control.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
    /// Whether any agent's mark moves now, and motion is on.
    pub(crate) fn animating(&self) -> bool {
        ui::motion_on()
            && self
                .model
                .live()
                .any(|r| moves(self.shown_kind(Entry::Live(r))))
    }

    /// Start the marks' clock when a mark moves and it is not running. Asked every second by
    /// the window's tick; the timer ends by itself on the first frame where nothing moves,
    /// so an idle window draws nothing extra.
    pub(crate) fn keep_beating(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.beat.running || !self.animating() {
            return;
        }
        self.beat.running = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                match this.update_in(cx, |this, _, cx| this.step_beat(cx)) {
                    Ok(true) => {}
                    _ => return,
                }
            }
        })
        .detach();
    }

    /// One frame on, and the window drawn again; false, with the clock stopped, when no mark
    /// moves any more.
    fn step_beat(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.animating() {
            self.beat.running = false;
            return false;
        }
        self.beat.frame = self.beat.frame.wrapping_add(1);
        ui::set_beat(self.beat.frame);
        cx.notify();
        true
    }

    /// The sidebar's width in the window as it is now: none while it is folded.
    pub(crate) fn side_px(&self) -> f32 {
        if !self.side_open {
            return 0.;
        }
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

    /// The top band, every page's first row: the page's title on the left, its first line on
    /// the window buttons' centre and wrapping downward; Next waiting, the gear and the pet at
    /// a right end of one fixed width. A press on it moves the window, two zoom it. With the
    /// column folded, the buttons' room and the fold toggle open it.
    pub(crate) fn top_band(&self, inbox: &Inbox, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (label, lead) = self.band_title(inbox, t);
        div()
            .id("top-band")
            .debug_selector(|| "top-band".to_string())
            .flex_none()
            .min_h(px(TOP_BAND))
            .flex()
            .items_start()
            .gap(px(16.))
            .pl(px(if self.side_open { 24. } else { 0. }))
            .pr(px(16.))
            .on_mouse_down(MouseButton::Left, drag_window)
            .when(!self.side_open, |d| {
                d.child(div().flex_none().w(px(BUTTONS))).child(
                    div()
                        .flex_none()
                        .mt(px(BAND_TOP + BAND_LINE / 2. - 15.))
                        .child(self.side_toggle(t, cx)),
                )
            })
            .child(
                div()
                    .id("title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(label)
                    .flex_1()
                    .min_w_0()
                    .py(px(BAND_TOP))
                    .flex()
                    .flex_wrap()
                    // Every piece's first line is one `BAND_LINE` tall from the top: the
                    // band's first line stays on the buttons' centre however the rest wraps.
                    .items_start()
                    .gap_x(px(12.))
                    .line_height(px(BAND_LINE))
                    .children(lead),
            )
            .child(self.band_right(inbox, t, cx))
            .into_any_element()
    }

    /// The page's title as the band draws it, and what a screen reader says for it. The first
    /// piece is `band-lead`, whose centre is the band's first line.
    fn band_title(&self, inbox: &Inbox, t: &Theme) -> (String, Vec<AnyElement>) {
        let big = |text: String| {
            div()
                .text_size(px(20.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let quiet = |text: String| div().text_size(px(13.5)).text_color(c(t.t2)).child(text);
        // A mark or an icon on the first line, centred in it like the text beside it.
        let on_line = |glyph: AnyElement| {
            div()
                .debug_selector(|| "band-lead".to_string())
                .flex_none()
                .h(px(BAND_LINE))
                .flex()
                .items_center()
                .child(glyph)
                .into_any_element()
        };
        match self.page {
            Page::Session(terminal) => {
                let row = self.model.row(terminal);
                let space = row.and_then(|r| self.model.space(r.workspace));
                let (title, program, machine) = match row {
                    Some(r) => (
                        state::title_of(r),
                        r.program.clone(),
                        space.map_or_else(|| "This Mac".to_string(), state::machine_name),
                    ),
                    None => (format!("tab {terminal}"), None, "This Mac".to_string()),
                };
                let about = match program {
                    Some(program) => format!("{program} on {machine}"),
                    None => format!("on {machine}"),
                };
                let kind = row.map_or(Kind::Status(Status::Unknown), |r| {
                    self.shown_kind(Entry::Live(r))
                });
                let label = match space {
                    Some(w) => format!("{}, {title}, {about}", state::short_space(w)),
                    None => format!("{title}, {about}"),
                };
                // The space, the title and where it runs as one run of text, wrapped as text
                // wraps: a long title starts on the first line beside the mark and grows the
                // band only by its own extra lines. As separate pieces of the wrapping row, a
                // title wider than what is left of the line would drop whole to the second.
                let name = space.map(state::short_space).unwrap_or_default();
                let lead_in = if name.is_empty() {
                    String::new()
                } else {
                    format!("{name} ")
                };
                let text = format!("{lead_in}{title} {about}");
                let (title_at, about_at) = (lead_in.len(), lead_in.len() + title.len() + 1);
                let mut runs = vec![
                    (
                        title_at..title_at + title.len(),
                        HighlightStyle {
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..Default::default()
                        },
                    ),
                    (
                        about_at..text.len(),
                        HighlightStyle {
                            color: Some(c(t.t2).into()),
                            ..Default::default()
                        },
                    ),
                ];
                if let Some(w) = space {
                    runs.push((
                        0..name.len(),
                        HighlightStyle {
                            color: Some(t.space(state::space_slot(w))),
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..Default::default()
                        },
                    ));
                }
                runs.sort_by_key(|(range, _)| range.start);
                let lead = vec![
                    on_line(ui::mark(kind, t, 14.).into_any_element()),
                    div()
                        .debug_selector(|| "band-title".to_string())
                        // A basis of nothing: it always fits beside the mark, then takes the
                        // rest of the line.
                        .flex_1()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(px(15.))
                        .child(StyledText::new(text).with_highlights(runs))
                        .into_any_element(),
                ];
                (label, lead)
            }
            Page::Computer(m) => {
                let (name, word, ink) = self.computer_reach(m, t);
                let lead = vec![
                    on_line(ui::icon(Icon::Server, 20., c(ink)).into_any_element()),
                    big(name.clone()).into_any_element(),
                    div()
                        .id("computer-state")
                        .text_size(px(13.5))
                        .text_color(c(ink))
                        .child(word.clone())
                        .into_any_element(),
                ];
                (format!("{name}, {word}"), lead)
            }
            _ => {
                let (title, mut sub) = self.title();
                if self.page == Page::Inbox && inbox.count > 0 {
                    sub = Some(format!("{} waiting", inbox.count));
                }
                let label = match &sub {
                    Some(sub) => format!("{title}, {sub}"),
                    None => title.clone(),
                };
                let mut lead = vec![
                    big(title)
                        .debug_selector(|| "band-lead".to_string())
                        .into_any_element(),
                ];
                lead.extend(sub.map(|sub| quiet(sub).into_any_element()));
                (label, lead)
            }
        }
    }

    /// The band's right end, one width on every page: Next waiting with how many other things
    /// wait and its key (or why it cannot go), the gear for Settings, and the person's pet.
    fn band_right(&self, inbox: &Inbox, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        // On an agent's page, what waits other than this agent.
        let count = match self.page {
            Page::Session(here) => inbox
                .items
                .iter()
                .filter(|i| i.terminal != Some(here))
                .count(),
            _ => inbox.count,
        };
        let next = if count > 0 {
            let shown = if count > 99 {
                "99+".to_string()
            } else {
                count.to_string()
            };
            ui::pill(
                "next-waiting",
                &format!("Next waiting, {}, Command J", state::count(count, "item")),
                t,
            )
            .child("Next waiting")
            .child(
                div()
                    .debug_selector(|| "next-waiting-count".to_string())
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
                    .child(shown),
            )
            .child(ui::kbd("\u{2318}J", t))
            .pr(px(6.))
            .on_click(cx.listener(|this, _, window, cx| this.next_waiting(window, cx)))
        } else {
            ui::pill_off("next-waiting", "Nothing else waits", t).child("Nothing else waits")
        };
        let pet = self.my_pet.clone().unwrap_or_else(Avatar::first);
        div()
            .id("band-right")
            .debug_selector(|| "band-right".to_string())
            .flex_none()
            .w(px(BAND_RIGHT))
            .h(px(30.))
            .mt(px(BAND_TOP + BAND_LINE / 2. - 15.))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(12.))
            .child(
                no_drag(next)
                    .debug_selector(|| "next-waiting".to_string())
                    .h(px(28.)),
            )
            .child(
                no_drag(ui::control("band-settings", Role::Button, "Settings", t))
                    .debug_selector(|| "band-settings".to_string())
                    .size(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .when(self.page == Page::Settings, |d| ui::current(d, t))
                    .hover(|s| ui::paint(s, t.hover()))
                    .child(ui::icon(Icon::Gear, 17., c(t.t1)))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open(Page::Settings, window, cx)),
                    ),
            )
            // Decorative: the person, not a control; a screen reader skips it.
            .child(
                div()
                    .debug_selector(|| "band-pet".to_string())
                    .size(px(26.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .bg(c(t.pop))
                    .child(ui::pet_for(&pet, 1.4, t)),
            )
    }

    /// The column's fold toggle: in the column's corner while it shows, in the band's while it
    /// is folded. One focus handle for both places, so the keys stay on it across a fold.
    fn side_toggle(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.side_open;
        no_drag(
            ui::control(
                "side-toggle",
                Role::Button,
                if open {
                    "Hide the column"
                } else {
                    "Show the column"
                },
                t,
            )
            .track_focus(&self.side_toggle),
        )
        .debug_selector(|| "side-toggle".to_string())
        .aria_expanded(open)
        .size(px(30.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .hover(|s| ui::paint(s, t.hover()))
        .child(ui::icon(Icon::Panel, 17., c(t.t1)))
        .on_click(cx.listener(|this, _, window, cx| this.toggle_side(window, cx)))
    }

    /// Fold the column or bring it back. Nothing else moves but the main column's width; the
    /// keys go to the toggle unless the terminal has them.
    pub(crate) fn toggle_side(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_open = !self.side_open;
        self.drag = None;
        if !self.term_focus.is_focused(window) {
            window.focus(&self.side_toggle, cx);
        }
        cx.notify();
    }

    /// qass's line in the column, drawn and spoken: its state, how long, and the agent it runs
    /// while it runs and watches ("idle 23m, claude", said "idle 23 minutes, claude"); else why
    /// not ("not running", "not watching").
    pub(crate) fn qass_note(&self) -> (String, String) {
        let line = self.model.qass_line();
        let Some(row) = self
            .model
            .qass_row()
            .filter(|_| line.starts_with("watching"))
        else {
            return (line.clone(), line);
        };
        let word = self.shown_kind(Entry::Live(row)).word();
        let since = row.slept_at_unix.or(row.last_active_at_unix);
        let ago = since.map(|at| self.now.saturating_sub(at).max(0));
        let (mut drawn, mut spoken) = (word.to_string(), word.to_string());
        if let Some(secs) = ago {
            drawn = format!("{drawn} {}", state::short_span(secs));
            spoken = format!("{spoken} {}", state::spoken_span(secs));
        }
        if let Some(program) = &row.program {
            drawn = format!("{drawn}, {program}");
            spoken = format!("{spoken}, {program}");
        }
        (drawn, spoken)
    }

    /// The agent qass's avatar wears: the one it runs, or while it does not run the one it
    /// runs as (qass's own pick comes first), so its face is never a bare "?".
    pub(crate) fn qass_program(&self) -> Option<&str> {
        self.model
            .qass_row()
            .and_then(|r| r.program.as_deref())
            .or_else(|| self.agent_names.first().map(String::as_str))
    }

    /// Whether the sidebar's link to `page` is the page on screen: qass's is on its agent page too.
    pub(crate) fn nav_on(&self, page: Page) -> bool {
        self.page == page
            || (page == Page::Qass
                && self
                    .qass_terminal()
                    .is_some_and(|q| self.page == Page::Session(q)))
    }

    /// The sidebar: the corner (the window buttons' room and the fold toggle), then qass, New
    /// agent, Inbox, Board and Schedules, the spaces folded to what waits and the agent on
    /// screen, and the computers pinned under the list.
    pub(crate) fn sidebar(
        &self,
        width: f32,
        inbox: &Inbox,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let nav = |id: &'static str,
                   lead: AnyElement,
                   label: &'static str,
                   page: Page,
                   badge: usize,
                   note: Option<String>,
                   spoken: Option<String>,
                   cx: &mut Context<Self>| {
            let on = self.nav_on(page);
            let mut said = match badge {
                0 => label.to_string(),
                n => format!("{label}, {n} waiting"),
            };
            // The note as a screen reader says it, where its drawn words are short.
            if let Some(note) = spoken.or_else(|| note.clone()) {
                said = format!("{said}, {note}");
            }
            ui::control(id, Role::Link, said, t)
                .debug_selector(move || id.to_string())
                .group(ui::AVATAR_ROW)
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
                .child(lead)
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
        let icon = |which: Icon| ui::icon(which, 16., c(t.t2)).into_any_element();
        let qass = self.model.qass_row();
        let qass_kind = qass.map_or(Kind::Status(Status::Unknown), |r| {
            self.shown_kind(Entry::Live(r))
        });
        let (qass_note, qass_spoken) = self.qass_note();
        let qass_ring = !self.nav_on(Page::Qass);
        let qass_face = div()
            .when(qass_ring, |d| d.debug_selector(|| "qass-ring".to_string()))
            .child(ui::avatar(
                self.qass_program(),
                qass_kind,
                20.,
                qass_ring,
                t,
            ))
            .into_any_element();
        let drawn = self.drawn_column(inbox);
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".to_string())
            .role(Role::Navigation)
            .aria_label("qoop")
            // The hold's region: the pointer over it, or the keys driving inside it.
            .track_focus(&self.side_focus)
            // Where the pointer is, typing or not: a name typed in a space field leaves it
            // over the column.
            .hover_listener_mode(gpui::HoverListenerMode::InputModalityIndependent)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.side_hovered != *hovered {
                    this.side_hovered = *hovered;
                    cx.notify();
                }
            }))
            .flex_none()
            .w(px(width))
            .h_full()
            .px(px(10.))
            .pb(px(10.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .bg(c(t.sidebar))
            // The corner: the window buttons' room, then the toggle on the band's first line.
            .child(
                div()
                    .id("side-corner")
                    .flex_none()
                    .h(px(TOP_BAND))
                    .pt(px(BAND_TOP + BAND_LINE / 2. - 15.))
                    .flex()
                    .justify_end()
                    .on_mouse_down(MouseButton::Left, drag_window)
                    .child(self.side_toggle(t, cx)),
            )
            .child(nav(
                "nav-qass",
                qass_face,
                "qass",
                Page::Qass,
                0,
                Some(qass_note),
                Some(qass_spoken),
                cx,
            ))
            .child(
                ui::control("new-work", Role::Button, "New agent, Command N", t)
                    .debug_selector(|| "new-work".to_string())
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(32.))
                    .px(px(10.))
                    .rounded(px(10.))
                    .text_size(px(13.5))
                    .text_color(c(t.t1))
                    .hover(|s| ui::paint(s, t.hover()))
                    .child(ui::icon(Icon::Plus, 16., c(t.ink)))
                    .child(div().flex_1().min_w_0().child("New agent"))
                    .child(ui::kbd("\u{2318}N", t))
                    .on_click(cx.listener(|this, _, window, cx| this.new_work(None, window, cx))),
            )
            // The Inbox, and its first rows folded under it by its chevron.
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        nav(
                            "nav-inbox",
                            icon(Icon::Inbox),
                            "Inbox",
                            Page::Inbox,
                            inbox.count,
                            None,
                            None,
                            cx,
                        )
                        .flex_1()
                        .min_w_0(),
                    )
                    .child(
                        ui::control(
                            "side-inbox-fold",
                            Role::Button,
                            "Show the first waiting items",
                            t,
                        )
                        .debug_selector(|| "side-inbox-fold".to_string())
                        .aria_expanded(self.inbox_open)
                        .size(px(26.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .hover(|s| ui::paint(s, t.hover()))
                        .child({
                            let chevron = ui::icon(Icon::Chevron, 12., c(t.t3));
                            if self.inbox_open {
                                chevron.with_transformation(gpui::Transformation::rotate(
                                    gpui::percentage(0.25),
                                ))
                            } else {
                                chevron
                            }
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_small_inbox(cx))),
                    ),
            )
            .when_some(self.small_inbox(&drawn, inbox, t, cx), |d, small| {
                d.child(small)
            })
            .child(nav(
                "nav-board",
                icon(Icon::Board),
                "Board",
                Page::Board,
                0,
                self.board_note(inbox),
                None,
                cx,
            ))
            .child(nav(
                "nav-schedules",
                icon(Icon::Clock),
                "Schedules",
                Page::Schedules,
                0,
                Some(state::schedules_line(&self.model.schedules)),
                None,
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
                    .pt(px(4.))
                    .child(ui::scroll(
                        div()
                            .id("sidebar-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(self.agent_list(&drawn, inbox, t, cx)),
                    )),
            )
            .child(self.computers(t, cx))
    }

    /// The age a sidebar row shows. How long a waiting agent has waited is the Inbox's own wait
    /// (the card says the same), not when the agent was last active; the others keep their last
    /// activity.
    pub(crate) fn side_age(&self, entry: Entry<'_>, inbox: &Inbox) -> Option<String> {
        self.side_secs(entry, inbox).map(state::short_span)
    }

    /// The seconds [`Self::side_age`] draws: the Inbox's wait, else the last activity.
    fn side_secs(&self, entry: Entry<'_>, inbox: &Inbox) -> Option<i64> {
        inbox
            .since_of(entry.terminal())
            .or_else(|| state::active_at(entry))
            .map(|at| self.now.saturating_sub(at).max(0))
    }

    /// An agent row's name to a screen reader: its title, agent, space and state in words, the
    /// age the row draws said as words ("22 minutes", never "22m"), and qass's cards on it.
    pub(crate) fn row_name(&self, entry: Entry<'_>, inbox: &Inbox) -> String {
        let spoken = self.side_secs(entry, inbox).map(state::spoken_span);
        let label = self.model.row_label_aged(entry, spoken);
        let handed = matches!(entry, Entry::Live(r) if self.model.with_qass(r));
        let cards = self.cards_of(entry.terminal(), entry.workspace()).len();
        if handed || cards > 0 {
            format!("{label}, qass holds {}", state::count(cards, "card"))
        } else {
            label
        }
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
    /// A computer's name, and how it is reached in words and in its ink: the band's title on
    /// its page.
    fn computer_reach(&self, m: usize, t: &Theme) -> (String, String, u32) {
        let name = state::machines(&self.model.spaces)
            .into_iter()
            .find(|(n, _, _)| *n == m)
            .map_or_else(|| "Computer".to_string(), |(_, name, _)| name);
        let reach = self.machine_state(m);
        let (word, ink) = match (m, reach) {
            (0, _) if self.lost_at.is_some() => ("not running".to_string(), t.needs_text),
            (0, _) => ("online".to_string(), t.done_text),
            (_, state) => (
                state::reach_words(state, self.zone),
                if state::unreached(state) {
                    t.needs_text
                } else if state == MachineState::Live {
                    t.done_text
                } else {
                    t.t2
                },
            ),
        };
        (name, word, ink)
    }

    pub(crate) fn computer_page(&self, m: usize, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (name, _, _) = self.computer_reach(m, t);
        let reach = self.machine_state(m);
        let away = state::unreached(reach);
        let as_of = self.model.spaces.iter().find_map(|w| {
            (state::machine_of(w.id) == m)
                .then(|| self.as_of(w.id))
                .flatten()
        });
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
        let mut rows: Vec<Entry<'_>> = groups
            .iter()
            .filter(|g| state::machine_of(g.space.id) == m)
            .flat_map(|g| g.rows.iter().copied())
            .collect();
        // What asks for the person first (the column's spaces sort by activity; this list is
        // about what needs doing on that machine), newest first within each state.
        rows.sort_by_key(|e| e.kind().rank());
        let retry = m != 0
            && (away || reach == MachineState::Retrying || reach == MachineState::Parked)
            && !matches!(reach, MachineState::Behind { .. });
        let start = m == 0 && self.lost_at.is_some();
        div()
            .id("computer-page")
            .debug_selector(|| "computer-page".to_string())
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(24.))
            .pt(px(6.))
            .pb(px(18.))
            .max_w(px(820.))
            // The name and its reach are the band's title; the steps that bring it back here.
            .when(retry || start, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(12.))
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
                        .when(start, |d| {
                            d.child(
                                ui::pill("start-qoop-page", "Start qoop", t)
                                    .child("Start qoop")
                                    .on_click(cx.listener(|this, _, _, cx| this.start_qoop(cx))),
                            )
                        }),
                )
            })
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

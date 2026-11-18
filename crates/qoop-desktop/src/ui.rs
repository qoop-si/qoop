//! The widgets the window draws, only the ones it uses: icons, the state mark, the pets, the
//! logo, key caps, buttons, chips, rows and the sheet frame. Every control has a role, a label
//! a screen reader says, a visible focus ring for the keyboard, and a 24 px target at least.

use std::borrow::Cow;

use gpui::{
    A11ySubtreeBuilder, AnyElement, AnyView, App, AssetSource, Bounds, BoxShadow, Context, Div,
    DivFrameState, ElementId, FocusHandle, FontWeight, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, Interactivity, LayoutId, Pixels, Render, Role, ScrollHandle, SharedString,
    Stateful, StyleRefinement, Svg, Toggled, Window, div, point, prelude::*, px, svg,
};
use qoop_domain::status::Status;
use qoop_runtime::avatar::{self, Avatar};

use crate::state::{Kind, mark_dots};
use crate::theme::{MONO_FONT, Pair, Theme, c, ca};

/// The outline icons of the design (24 x 24, 1.6 stroke), drawn as masks in the ink asked for.
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Home,
    Clock,
    Gear,
    Plus,
    Inbox,
    Eye,
    Chevron,
    Close,
    Phone,
}

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Icon::Home => "icons/home.svg",
            Icon::Clock => "icons/clock.svg",
            Icon::Gear => "icons/gear.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::Inbox => "icons/inbox.svg",
            Icon::Eye => "icons/eye.svg",
            Icon::Chevron => "icons/chevron.svg",
            Icon::Close => "icons/close.svg",
            Icon::Phone => "icons/phone.svg",
        }
    }

    fn body(path: &str) -> Option<&'static str> {
        Some(match path {
            "icons/home.svg" => {
                r#"<path d="M4 11l8-7 8 7v8a1 1 0 0 1-1 1h-4.5v-6h-5v6H5a1 1 0 0 1-1-1z"/>"#
            }
            "icons/clock.svg" => r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#,
            "icons/gear.svg" => {
                r#"<circle cx="12" cy="12" r="3"/><path d="M12 3.5v2.2M12 18.3v2.2M3.5 12h2.2M18.3 12h2.2M6 6l1.6 1.6M16.4 16.4L18 18M6 18l1.6-1.6M16.4 7.6L18 6"/>"#
            }
            "icons/plus.svg" => r#"<path d="M12 5v14M5 12h14"/>"#,
            "icons/inbox.svg" => {
                r#"<path d="M4 13l2.4-7.2a1 1 0 0 1 .9-.8h9.4a1 1 0 0 1 .9.8L20 13v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1z"/><path d="M4 13h4.5l1 2h5l1-2H20"/>"#
            }
            "icons/eye.svg" => {
                r#"<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>"#
            }
            "icons/chevron.svg" => r#"<path d="M9.5 6l6 6-6 6"/>"#,
            "icons/close.svg" => r#"<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>"#,
            "icons/phone.svg" => {
                r#"<rect x="7" y="3" width="10" height="18" rx="2.5"/><path d="M11 18h2"/>"#
            }
            _ => return None,
        })
    }
}

/// The icons, served to GPUI's `svg()` from memory.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::body(path).map(|body| {
            Cow::Owned(
                format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub fn icon(which: Icon, size: f32, ink: impl Into<Hsla>) -> Svg {
    svg()
        .path(which.path())
        .size(px(size))
        .flex_none()
        .text_color(ink.into())
}

/// A state's mark ink.
pub fn mark_ink(kind: Kind, t: &Theme) -> u32 {
    match kind {
        Kind::Status(Status::Blocked) => t.needs,
        Kind::Status(Status::Failed) => t.fail,
        Kind::Status(Status::Working | Status::Starting | Status::Background) => t.work,
        Kind::Status(Status::Done) => t.done,
        Kind::Sleeping => t.sleep,
        Kind::Status(Status::Parked | Status::Idle) => t.idle,
        Kind::Status(Status::Unknown) => t.t3,
    }
}

/// A state's word ink: the state's text step, the quiet ones in the quiet text ink.
pub fn word_ink(kind: Kind, t: &Theme) -> u32 {
    match kind {
        Kind::Status(Status::Blocked) => t.needs_text,
        Kind::Status(Status::Failed) => t.fail_text,
        Kind::Status(Status::Working | Status::Starting | Status::Background) => t.work_text,
        Kind::Status(Status::Done) => t.done_text,
        Kind::Sleeping => t.sleep_text,
        _ => t.t2,
    }
}

/// The state mark: real dots on the 3x3 grid, `size` px square. Always drawn beside the
/// word, so it is hidden from a screen reader, which reads the word.
pub fn mark(kind: Kind, t: &Theme, size: f32) -> Div {
    let dot = (size / 5.0).max(2.0).round();
    let step = (size - dot) / 2.0;
    let ink = c(mark_ink(kind, t));
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .children(mark_dots(kind, None).into_iter().map(|(x, y)| {
            div()
                .absolute()
                .left(px(f32::from(x) * step))
                .top(px(f32::from(y) * step))
                .size(px(dot))
                .rounded_full()
                .bg(ink)
        }))
}

/// A pet, one `cell` px square per pixel of its 16 x 12 grid.
pub fn pet(pet: &Avatar, cell: f32) -> Div {
    pet_rects(&avatar::rects(pet), cell)
}

/// A pet from its rects: this Mac's drawn here, another machine's as its daemon sent it.
pub fn pet_rects(rects: &[crate::state::PetRect], cell: f32) -> Div {
    let w = rects.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
    let h = rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
    div()
        .relative()
        .w(px(f32::from(w) * cell))
        .h(px(f32::from(h) * cell))
        .flex_none()
        .children(rects.iter().map(|(x, y, w, h, hex)| {
            let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
            div()
                .absolute()
                .left(px(f32::from(*x) * cell))
                .top(px(f32::from(*y) * cell))
                .w(px(f32::from(*w) * cell))
                .h(px(f32::from(*h) * cell))
                .bg(c(rgb))
        }))
}

/// The qoop logo as IDENTITY.md draws it: eight cream dots and the orange tail on the dark
/// rounded square, in the 1024 unit box of the SVG.
pub fn logo(size: f32) -> Div {
    let k = size / 1024.0;
    let r = 64.0 * k;
    let dots: [(f32, f32, u32); 9] = [
        (331.8, 331.8, 0xfcf7f1),
        (512.0, 331.8, 0xfcf7f1),
        (692.2, 331.8, 0xfcf7f1),
        (331.8, 512.0, 0xfcf7f1),
        (692.2, 512.0, 0xfcf7f1),
        (331.8, 692.2, 0xfcf7f1),
        (512.0, 692.2, 0xfcf7f1),
        (692.2, 692.2, 0xfcf7f1),
        (692.2, 872.4, 0xfd4b03),
    ];
    div()
        .relative()
        .size(px(size))
        .flex_none()
        .rounded(px(224.0 * k))
        .bg(c(0x0f0f11))
        .children(dots.into_iter().map(move |(x, y, ink)| {
            div()
                .absolute()
                .left(px(x * k - r))
                .top(px(y * k - r))
                .size(px(2.0 * r))
                .rounded_full()
                .bg(c(ink))
        }))
}

/// A radio button's state as AccessKit reads it.
pub fn toggled(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}

/// A control state painted: its fill and the ink of the label on it, never one without the
/// other. Takes a control or the style of its hover, so every state goes through here.
pub fn paint<S: Styled>(s: S, pair: Pair) -> S {
    s.bg(ca(pair.fill)).text_color(c(pair.text))
}

/// A picked chip, radio or day: the picked pair and its outline, so the pick never rests on a
/// tint alone. The element carries a 1 px border at rest (a `control` does), so nothing moves.
pub fn picked<S: Styled>(s: S, t: &Theme) -> S {
    paint(s, t.picked()).border_color(c(t.picked_edge()))
}

/// The page, tab, row or run you are on: the current pair with the same outline as a pick,
/// so where you are never rests on a tint alone either. On a `control`'s 1 px border.
pub fn current<S: Styled>(s: S, t: &Theme) -> S {
    paint(s, t.current()).border_color(c(t.picked_edge()))
}

/// A key cap: `ctrl-a c`.
pub fn kbd(keys: &str, t: &Theme) -> Div {
    kbd_in(keys, t.key())
}

/// A key cap in its own pair: `Theme::key_on` inside a control that is on (Start).
pub fn kbd_in(keys: &str, pair: Pair) -> Div {
    paint(div(), pair)
        .flex_none()
        .px(px(6.))
        .py(px(3.))
        .rounded(px(6.))
        .font_family(MONO_FONT)
        .text_size(px(10.5))
        .whitespace_nowrap()
        .child(keys.to_string())
}

/// A focusable control: its role, its label, a tab stop, and a ring when the keyboard is on
/// it. The caller adds the look and `on_click`.
pub fn control(
    id: impl Into<ElementId>,
    role: Role,
    label: impl Into<SharedString>,
    t: &Theme,
) -> Control {
    let ring = *t;
    Control::new(
        div()
            .id(id)
            .role(role)
            .aria_label(label)
            .cursor_pointer()
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |s| focus_ring(s, &ring)),
    )
}

/// A focusable element that knows when the keyboard brings the focus to it and asks the
/// scrolling list or page around it ([`scroll`]) to bring it into view, so a Tab stop below
/// the fold is never focused out of sight. It focuses by its own handle, kept from frame to
/// frame, or by the one given with [`Control::track_focus`].
pub struct Control {
    div: Option<Stateful<Div>>,
    focus: Option<FocusHandle>,
    tab_stop: bool,
}

/// What a control keeps between frames: its own focus handle, and whether it had the focus.
#[derive(Default)]
struct Kept {
    own: Option<FocusHandle>,
    focused: bool,
}

impl Control {
    /// `div` as a control: a Tab stop with a handle of its own.
    pub fn new(div: Stateful<Div>) -> Self {
        Control {
            div: Some(div.focusable()),
            focus: None,
            tab_stop: true,
        }
    }

    fn div(&mut self) -> &mut Stateful<Div> {
        self.div
            .as_mut()
            .expect("a control holds its div until it is drawn")
    }

    /// Focus by `focus` (a sheet's stop, a field's) instead of a handle of its own; its Tab
    /// stop is then the handle's.
    pub fn track_focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    /// Whether Tab stops here (its own handle only).
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }
}

impl Styled for Control {
    fn style(&mut self) -> &mut StyleRefinement {
        Styled::style(self.div())
    }
}

impl InteractiveElement for Control {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div().interactivity()
    }
}

impl StatefulInteractiveElement for Control {}

impl ParentElement for Control {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div().extend(elements)
    }
}

impl IntoElement for Control {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Control {
    type RequestLayoutState = DivFrameState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.div.as_ref().and_then(Element::id)
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.div.as_ref().and_then(|d| Element::source_location(d))
    }

    fn a11y_role(&self) -> Option<Role> {
        self.div.as_ref().and_then(Element::a11y_role)
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        if let Some(d) = self.div.as_ref() {
            Element::write_a11y_info(d, node);
        }
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Option<Hitbox>,
        builder: &mut A11ySubtreeBuilder,
    ) {
        Element::a11y_synthetic_children(self.div(), prepaint, builder);
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, DivFrameState) {
        let focus = match (self.focus.clone(), id) {
            (Some(given), _) => Some(given),
            (None, Some(id)) => {
                let own = window.with_element_state(id, |kept: Option<Kept>, _| {
                    let mut kept = kept.unwrap_or_default();
                    let own = kept.own.get_or_insert_with(|| cx.focus_handle()).clone();
                    (own, kept)
                });
                Some(own.tab_stop(self.tab_stop))
            }
            (None, None) => None,
        };
        if let Some(focus) = focus {
            let div = self.div.take().expect("a control's div");
            self.div = Some(div.track_focus(&focus));
            self.focus = Some(focus);
        }
        Element::request_layout(self.div(), id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut DivFrameState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        let hitbox = Element::prepaint(self.div(), id, inspector_id, bounds, state, window, cx);
        if let (Some(id), Some(focus)) = (id, self.focus.as_ref()) {
            let now = focus.is_focused(window);
            let gained = window.with_element_state(id, |kept: Option<Kept>, _| {
                let mut kept = kept.unwrap_or_default();
                let gained = now && !kept.focused;
                kept.focused = now;
                (gained, kept)
            });
            // By the keyboard only: a mouse down focuses too, and a list moving under the
            // pointer before the button comes up would lose the click.
            if gained && window.last_input_was_keyboard() {
                window.request_autoscroll(bounds);
            }
        }
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut DivFrameState,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        Element::paint(
            self.div(),
            id,
            inspector_id,
            bounds,
            state,
            hitbox,
            window,
            cx,
        );
    }
}

/// `div` (scrolling on its own) as a list or page that scrolls a control gaining the focus
/// inside it into view: the innermost one around the control that can, by the least.
pub fn scroll(div: Stateful<Div>) -> Scroll {
    Scroll {
        div: Some(div),
        handle: None,
    }
}

pub struct Scroll {
    div: Option<Stateful<Div>>,
    handle: Option<ScrollHandle>,
}

impl Scroll {
    fn div(&mut self) -> &mut Stateful<Div> {
        self.div
            .as_mut()
            .expect("a scroll holds its div until it is drawn")
    }
}

impl IntoElement for Scroll {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scroll {
    type RequestLayoutState = DivFrameState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        self.div.as_ref().and_then(Element::id)
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.div.as_ref().and_then(|d| Element::source_location(d))
    }

    fn a11y_role(&self) -> Option<Role> {
        self.div.as_ref().and_then(Element::a11y_role)
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        if let Some(d) = self.div.as_ref() {
            Element::write_a11y_info(d, node);
        }
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Option<Hitbox>,
        builder: &mut A11ySubtreeBuilder,
    ) {
        Element::a11y_synthetic_children(self.div(), prepaint, builder);
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, DivFrameState) {
        if let Some(id) = id {
            let handle = window.with_element_state(id, |kept: Option<ScrollHandle>, _| {
                let handle = kept.unwrap_or_default();
                (handle.clone(), handle)
            });
            let div = self.div.take().expect("a scroll's div");
            self.div = Some(div.track_scroll(&handle));
            self.handle = Some(handle);
        }
        Element::request_layout(self.div(), id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut DivFrameState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        // A request made before this list (a control beside it) is the page's to answer.
        let before = window.take_autoscroll();
        let hitbox = Element::prepaint(self.div(), id, inspector_id, bounds, state, window, cx);
        let inside = window.take_autoscroll();
        let up = match (inside, self.handle.as_ref()) {
            (Some(want), Some(handle)) => match reveal(handle, bounds, want) {
                Reveal::Shown => None,
                Reveal::Moved => {
                    // The children are placed for this frame already: draw the next.
                    window.defer(cx, |window, _| window.refresh());
                    None
                }
                Reveal::Cannot => Some(want),
            },
            (inside, _) => inside,
        };
        if let Some(want) = up.or(before) {
            window.request_autoscroll(want);
        }
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut DivFrameState,
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        Element::paint(
            self.div(),
            id,
            inspector_id,
            bounds,
            state,
            hitbox,
            window,
            cx,
        );
    }
}

#[derive(Debug, PartialEq)]
enum Reveal {
    /// In view already.
    Shown,
    /// Scrolled to show it.
    Moved,
    /// This list cannot move it closer: the one around it may.
    Cannot,
}

/// Scroll `handle`'s list (on screen at `view`) by the least that shows `want`, with a little
/// room so the focus ring is not cut; its top first when it is taller than the list.
fn reveal(handle: &ScrollHandle, view: Bounds<Pixels>, want: Bounds<Pixels>) -> Reveal {
    let room = px(8.);
    let to_top = view.top() + room - want.top();
    let dy = if want.top() < view.top() {
        to_top
    } else if want.bottom() > view.bottom() {
        (view.bottom() - room - want.bottom()).max(to_top)
    } else {
        return Reveal::Shown;
    };
    let offset = handle.offset();
    let y = (offset.y + dy).min(px(0.)).max(-handle.max_offset().y);
    if y == offset.y {
        return Reveal::Cannot;
    }
    handle.set_offset(point(offset.x, y));
    Reveal::Moved
}

/// The keyboard's ring: a 2 px band in the focus ink just inside the control's edge and a
/// 1 px band of `bg` inside that, over its fill and under its label, so the fill and the
/// text stay as they are and the ring reads on any fill (an on switch's included). Inset
/// shadows, in paint order, because GPUI draws an outer one under the whole control: a
/// control with no fill of its own (a link, a chip) would be filled with it and its label
/// lost.
pub fn focus_ring(s: StyleRefinement, t: &Theme) -> StyleRefinement {
    let ring: Hsla = c(t.focus).into();
    let band = |color: Hsla, spread: f32| BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: true,
    };
    s.border_color(ring)
        .shadow(vec![band(c(t.bg).into(), 3.), band(ring, 2.)])
}

/// A pill button with an outline: `+ New work ctrl-a c`.
pub fn pill(id: impl Into<ElementId>, label: &str, t: &Theme) -> Control {
    let pressed = t.pressed();
    pill_frame(id, label, t, t.rest()).hover(move |s| paint(s, pressed))
}

/// A pill that cannot be pressed now: its reason in the label, out of the tab order, and
/// its text still readable (no opacity over it).
pub fn pill_off(id: impl Into<ElementId>, label: &str, t: &Theme) -> Control {
    pill_frame(id, label, t, t.off())
        .tab_stop(false)
        .cursor_default()
}

fn pill_frame(id: impl Into<ElementId>, label: &str, t: &Theme, pair: Pair) -> Control {
    paint(control(id, Role::Button, label.to_string(), t), pair)
        .flex()
        .items_center()
        .gap(px(7.))
        .h(px(34.))
        .px(px(12.))
        .rounded_full()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
}

/// A small outlined text button: `Schedules >`, `Mark all read`.
pub fn link_button(id: impl Into<ElementId>, label: &str, t: &Theme) -> Control {
    control(id, Role::Link, label.to_string(), t)
        .flex()
        .items_center()
        .gap(px(4.))
        .min_h(px(24.))
        .px(px(4.))
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(c(t.t1))
        .whitespace_nowrap()
        .child(label.to_string())
}

/// One chip of a segmented control: its mark, its label and its count.
pub fn chip(
    id: impl Into<ElementId>,
    label: &str,
    count: Option<usize>,
    kind: Option<Kind>,
    on: bool,
    t: &Theme,
) -> Control {
    let said = match count {
        Some(n) => format!("{label}, {n}"),
        None => label.to_string(),
    };
    control(id, Role::RadioButton, said, t)
        .aria_toggled(toggled(on))
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .px(px(9.))
        .rounded_full()
        .text_size(px(12.5))
        .text_color(c(t.t2))
        .when(on, |d| picked(d, t))
        .when_some(kind, |d, kind| d.child(mark(kind, t, 9.)))
        .child(label.to_string())
        .when_some(count, |d, n| {
            d.child(
                div()
                    .text_size(px(11.))
                    .text_color(c(t.t2))
                    .child(n.to_string()),
            )
        })
}

/// An on and off switch, 36 by 20, its state said to a screen reader. The caller adds
/// `on_click`.
pub fn switch(id: impl Into<ElementId>, label: &str, on: bool, t: &Theme) -> Control {
    switch_frame(id, label, on, t, if on { t.on() } else { t.pressed() })
}

/// A switch that cannot be turned now: the off pair on either side, its knob still showing
/// which, out of the tab order.
pub fn switch_off(id: impl Into<ElementId>, label: &str, on: bool, t: &Theme) -> Control {
    switch_frame(id, label, on, t, t.off())
        .tab_stop(false)
        .cursor_default()
}

/// The knob is the text drawn on the track.
fn switch_frame(id: impl Into<ElementId>, label: &str, on: bool, t: &Theme, pair: Pair) -> Control {
    paint(control(id, Role::Switch, label.to_string(), t), pair)
        .aria_toggled(toggled(on))
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .items_center()
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(c(pair.text)))
}

/// A small tag with an outline: `local`, `remote`.
pub fn tag(text: &str, t: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(7.))
        .rounded_full()
        .border_1()
        .border_color(ca(t.line))
        .text_size(px(10.5))
        .text_color(c(t.t2))
        .child(text.to_string())
}

/// A card: the raised ground of a group.
pub fn card(t: &Theme) -> Div {
    div()
        .bg(c(t.card))
        .rounded(px(14.))
        .border_1()
        .border_color(ca(t.hair))
}

/// A tooltip: a line of text on the popover ground, shown beside the control it explains.
pub struct Tip {
    text: SharedString,
    t: Theme,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(280.))
            .px(px(9.))
            .py(px(6.))
            .rounded(px(8.))
            .bg(c(self.t.pop))
            .border_1()
            .border_color(ca(self.t.line))
            .text_size(px(12.))
            .text_color(c(self.t.t1))
            .child(self.text.clone())
    }
}

/// What `.tooltip()` takes to show a text made at run time (a link's reason).
pub fn tip_text(text: String, t: Theme) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| {
        cx.new(|_| Tip {
            text: text.clone(),
            t,
        })
        .into()
    }
}

/// What `.tooltip()` takes to show `text`.
pub fn tip(text: &'static str, t: Theme) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| {
        cx.new(|_| Tip {
            text: text.into(),
            t,
        })
        .into()
    }
}

/// Tab (`back`: Shift-Tab) inside a sheet whose frame tracks `frame`: the window's own order,
/// gone round until the focus is in the frame again, so a stop behind the scrim is never
/// reached and the last stop wraps to the first.
/// ponytail: walks the window's whole order, a few dozen stops; bounded so it always ends.
pub fn trap_tab(frame: &FocusHandle, back: bool, window: &mut Window, cx: &mut App) {
    for _ in 0..512 {
        if back {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        if frame.contains_focused(window, cx) {
            return;
        }
    }
}

/// The sheet's frame: a scrim over the window and a card in its middle, holding `body`.
pub fn sheet(id: &'static str, label: &str, t: &Theme, body: AnyElement) -> Stateful<Div> {
    div()
        .id("scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(if t.dark { 0x000000a0 } else { 0x26262640 }))
        .child(
            div()
                .id(id)
                .role(Role::Dialog)
                .aria_label(label.to_string())
                .w(px(560.))
                .p(px(20.))
                .rounded(px(18.))
                .bg(c(t.pop))
                .border_1()
                .border_color(ca(t.line))
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{DARK, LIGHT};

    /// The ring is the focus ink inside the edge with a `bg` band inside it: it never sets
    /// the fill or the text, and no shadow of it is drawn outside, where GPUI would paint it
    /// under the label. The focus band is painted last, over the `bg` one.
    #[test]
    fn the_focus_ring_keeps_the_fill_and_the_label() {
        for t in [DARK, LIGHT] {
            let s = focus_ring(StyleRefinement::default(), &t);
            assert!(s.background.is_none(), "the fill stays the control's own");
            assert!(s.text.color.is_none(), "the label stays the state's own");
            let ring: Hsla = c(t.focus).into();
            assert_eq!(s.border_color, Some(ring));
            let shadows = s.box_shadow.unwrap_or_default();
            let bands: Vec<_> = shadows
                .iter()
                .map(|b| (b.inset, b.color, b.spread_radius))
                .collect();
            let bg: Hsla = c(t.bg).into();
            assert_eq!(bands, [(true, bg, px(3.)), (true, ring, px(2.))]);
        }
    }

    /// A painted state sets its fill and its text together, on a control and on a hover.
    #[test]
    fn a_state_paints_its_fill_and_its_text() {
        for t in [DARK, LIGHT] {
            for pair in [t.rest(), t.pressed(), t.on(), t.off()] {
                let s = paint(StyleRefinement::default(), pair);
                assert_eq!(s.background, Some(ca(pair.fill).into()));
                assert_eq!(s.text.color, Some(c(pair.text).into()));
            }
        }
    }
}

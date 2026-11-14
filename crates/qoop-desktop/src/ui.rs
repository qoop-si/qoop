//! The widgets the window draws, only the ones it uses: icons, the state mark, the pets, the
//! logo, key caps, buttons, chips, rows and the sheet frame. Every control has a role, a label
//! a screen reader says, a visible focus ring for the keyboard, and a 24 px target at least.

use std::borrow::Cow;

use gpui::{
    AnyElement, AssetSource, Div, ElementId, FontWeight, Hsla, Role, SharedString, Stateful, Svg,
    Toggled, div, prelude::*, px, svg,
};
use qoop_domain::status::Status;
use qoop_runtime::avatar::{self, Avatar};

use crate::state::{Kind, mark_dots};
use crate::theme::{MONO_FONT, Theme, c, ca};

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
    let rects = avatar::rects(pet);
    let w = rects.iter().map(|r| r.0 + r.2).max().unwrap_or(0);
    let h = rects.iter().map(|r| r.1 + r.3).max().unwrap_or(0);
    div()
        .relative()
        .w(px(f32::from(w) * cell))
        .h(px(f32::from(h) * cell))
        .flex_none()
        .children(rects.into_iter().map(|(x, y, w, h, hex)| {
            let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0);
            div()
                .absolute()
                .left(px(f32::from(x) * cell))
                .top(px(f32::from(y) * cell))
                .w(px(f32::from(w) * cell))
                .h(px(f32::from(h) * cell))
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

/// A key cap: `ctrl-a c`.
pub fn kbd(keys: &str, t: &Theme) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(ca(t.fill))
        .font_family(MONO_FONT)
        .text_size(px(10.5))
        .text_color(c(t.t2))
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
) -> Stateful<Div> {
    let ring = c(t.ink);
    div()
        .id(id)
        .role(role)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .cursor_pointer()
        .border_1()
        .border_color(gpui::transparent_black())
        .focus_visible(move |s| s.border_color(ring))
}

/// A pill button with an outline: `+ New work ctrl-a c`.
pub fn pill(id: impl Into<ElementId>, label: &str, t: &Theme) -> Stateful<Div> {
    control(id, Role::Button, label.to_string(), t)
        .flex()
        .items_center()
        .gap(px(7.))
        .h(px(34.))
        .px(px(12.))
        .rounded_full()
        .bg(ca(t.fill))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(c(t.ink))
        .whitespace_nowrap()
        .hover(|s| s.bg(ca(t.fill2)))
}

/// A small outlined text button: `Schedules >`, `Mark all read`.
pub fn link_button(id: impl Into<ElementId>, label: &str, t: &Theme) -> Stateful<Div> {
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
) -> Stateful<Div> {
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
        .text_color(c(if on { t.ink } else { t.t2 }))
        .when(on, |d| d.bg(c(t.card2)))
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

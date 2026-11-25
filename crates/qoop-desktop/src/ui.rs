//! The widgets the window draws, only the ones it uses: icons, the state mark, the pets, the
//! logo, key caps, buttons, chips, rows and the sheet frame. Every control has a role, a label
//! a screen reader says, a visible focus ring for the keyboard, and a 24 px target at least.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::OnceLock;

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
    Server,
    Search,
    /// The column's fold toggle: a window with its side pane.
    Panel,
    /// The Board's entry: three columns.
    Board,
}

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Icon::Panel => "icons/panel.svg",
            Icon::Board => "icons/board.svg",
            Icon::Home => "icons/home.svg",
            Icon::Clock => "icons/clock.svg",
            Icon::Gear => "icons/gear.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::Inbox => "icons/inbox.svg",
            Icon::Eye => "icons/eye.svg",
            Icon::Chevron => "icons/chevron.svg",
            Icon::Close => "icons/close.svg",
            Icon::Phone => "icons/phone.svg",
            Icon::Server => "icons/server.svg",
            Icon::Search => "icons/search.svg",
        }
    }

    fn body(path: &str) -> Option<&'static str> {
        Some(match path {
            "icons/home.svg" => {
                r#"<path d="M4 11l8-7 8 7v8a1 1 0 0 1-1 1h-4.5v-6h-5v6H5a1 1 0 0 1-1-1z"/>"#
            }
            "icons/clock.svg" => r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#,
            // The cog of Feather's "settings" (MIT): one toothed outline round a hub, pulled in a
            // little so it sits as large as its neighbours. Rays round a circle read as a sun.
            "icons/gear.svg" => {
                r#"<g transform="translate(12 12) scale(.88) translate(-12 -12)"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/></g>"#
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
            "icons/server.svg" => {
                r#"<rect x="4" y="4" width="16" height="6.5" rx="1.5"/><rect x="4" y="13.5" width="16" height="6.5" rx="1.5"/><path d="M7.5 7.25h.01M7.5 16.75h.01"/>"#
            }
            "icons/search.svg" => r#"<circle cx="11" cy="11" r="6.5"/><path d="M16 16l4.5 4.5"/>"#,
            "icons/panel.svg" => {
                r#"<rect x="3.5" y="5" width="17" height="14" rx="2"/><path d="M14.5 5v14"/>"#
            }
            "icons/board.svg" => {
                r#"<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M9.2 4.5v15M14.8 4.5v15"/>"#
            }
            _ => return None,
        })
    }
}

/// The agents with a logo, as their program is named.
const LOGOS: [&str; 4] = ["claude", "codex", "opencode", "gemini"];

/// An agent's logo as one filled path in a 24 x 24 box: Simple Icons (CC0), the phone's own
/// `agentPaths.ts`. Drawn in one ink, never a brand colour.
fn logo_body(path: &str) -> Option<&'static str> {
    Some(match path {
        "logos/claude.svg" => {
            "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z"
        }
        "logos/codex.svg" => {
            "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z"
        }
        "logos/opencode.svg" => "M22 24H2V0h20zM17 4.8H7v14.4h10z",
        "logos/gemini.svg" => {
            "M11.04 19.32Q12 21.51 12 24q0-2.49.93-4.68.96-2.19 2.58-3.81t3.81-2.55Q21.51 12 24 12q-2.49 0-4.68-.93a12.3 12.3 0 0 1-3.81-2.58 12.3 12.3 0 0 1-2.58-3.81Q12 2.49 12 0q0 2.49-.96 4.68-.93 2.19-2.55 3.81a12.3 12.3 0 0 1-3.81 2.58Q2.49 12 0 12q2.49 0 4.68.96 2.19.93 3.81 2.55t2.55 3.81"
        }
        _ => return None,
    })
}

/// The icons and the logos, served to GPUI's `svg()` from memory: an icon is outlines, a
/// logo one filled path.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(d) = logo_body(path) {
            return Ok(Some(Cow::Owned(
                format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" d="{d}"/></svg>"##
                )
                .into_bytes(),
            )));
        }
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

thread_local! {
    /// The frame of the working mark's breath, set by the window's clock (`Desk::step_beat`),
    /// and macOS's Reduce Motion as the window last read it ([`read_reduce_motion`]).
    /// ponytail: per thread, one value for every window: the window steps, reads and draws on
    /// the main thread only (one window today), and each test keeps its own.
    static BEAT: Cell<u32> = const { Cell::new(0) };
    static REDUCED: Cell<bool> = const { Cell::new(false) };
}

pub fn set_beat(frame: u32) {
    BEAT.set(frame);
}

/// Reduce Motion on or off for the marks: what [`read_reduce_motion`] read, or a test's.
pub fn set_reduce_motion(on: bool) {
    REDUCED.set(on);
}

/// Whether marks may move: not with `QOOP_MOTION=0` set, as in the TUI, nor with macOS's
/// Reduce Motion on. Every mark goes through here.
pub fn motion_on() -> bool {
    static STILL: OnceLock<bool> = OnceLock::new();
    let still = *STILL
        .get_or_init(|| qoop_runtime::legacy::var("QOOP_MOTION").is_ok_and(|v| v.trim() == "0"));
    motion(still, REDUCED.get())
}

/// The dots a mark draws now: the working breath at the clock's frame while motion is on, its
/// still picture while it is not.
pub fn mark_dots_now(kind: Kind) -> Vec<(u8, u8)> {
    mark_dots(kind, motion_on().then(|| BEAT.get()))
}

/// [`motion_on`]'s rule: either one asking for stillness wins.
fn motion(env_still: bool, system_reduced: bool) -> bool {
    !env_still && !system_reduced
}

/// Read macOS's Reduce Motion again: at launch and whenever the window is activated.
/// ponytail: a change in System Settings shows when the window is next activated; a live
/// notification needs an Objective-C observer (`objc2-app-kit` as a direct dependency, which
/// is the owner's call).
pub fn read_reduce_motion() {
    set_reduce_motion(reduce_from(mac::preference(), mac::workspace));
}

/// The preference when it could be read; else AppKit's documented answer (the preference's
/// domain may be refused to an app as `open` launches it).
fn reduce_from(preference: Option<bool>, workspace: impl FnOnce() -> bool) -> bool {
    preference.unwrap_or_else(workspace)
}

mod mac {
    use std::ffi::{c_char, c_void};

    #[link(name = "objc")]
    // SAFETY: the signatures are the Objective-C runtime's own (`objc/runtime.h`,
    // `objc/message.h`); `objc_msgSend` is declared bare and cast to each call's own signature
    // before it is called, as the runtime requires.
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    // AppKit, where `NSWorkspace` lives (GPUI links it too).
    #[link(name = "AppKit", kind = "framework")]
    // SAFETY: nothing is declared from it; the block only links the framework.
    unsafe extern "C" {}

    /// `[[NSWorkspace sharedWorkspace] accessibilityDisplayShouldReduceMotion]`: the documented
    /// answer, false when AppKit is not there to ask.
    pub fn workspace() -> bool {
        // SAFETY: fn pointer types only; nothing is called through them here.
        type Bare = unsafe extern "C" fn();
        // SAFETY: as above.
        type SendObject = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
        // SAFETY: as above.
        type SendBool = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u8;
        // SAFETY: the class and the selectors are looked up by C string literals, and a null
        // class or object is checked before any message; each message is sent through
        // `objc_msgSend` cast to the signature of its method (an object, then a BOOL), and the
        // shared workspace is not owned here, so nothing is released.
        unsafe {
            let class = objc_getClass(c"NSWorkspace".as_ptr());
            if class.is_null() {
                return false;
            }
            let send = std::mem::transmute::<Bare, SendObject>(objc_msgSend);
            let shared = send(class, sel_registerName(c"sharedWorkspace".as_ptr()));
            if shared.is_null() {
                return false;
            }
            let send_bool = std::mem::transmute::<Bare, SendBool>(objc_msgSend);
            send_bool(
                shared,
                sel_registerName(c"accessibilityDisplayShouldReduceMotion".as_ptr()),
            ) != 0
        }
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    // SAFETY: the signatures are CoreFoundation's own (`CFPreferences.h`, `CFString.h`,
    // `CFBase.h`).
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFPreferencesAppSynchronize(app: *const c_void) -> u8;
        fn CFPreferencesGetAppBooleanValue(
            key: *const c_void,
            app: *const c_void,
            valid: *mut u8,
        ) -> u8;
        fn CFRelease(object: *const c_void);
    }

    const UTF8: u32 = 0x0800_0100;

    /// `com.apple.universalaccess`'s `reduceMotion`, synchronised first (CoreFoundation keeps
    /// a domain it read once and would answer the launch value for ever); `None` when it
    /// could not be read.
    pub fn preference() -> Option<bool> {
        // SAFETY: the two strings made here are released once, after their last use; a null
        // from either is checked before use; the boolean read borrows nothing.
        unsafe {
            let key = CFStringCreateWithCString(std::ptr::null(), c"reduceMotion".as_ptr(), UTF8);
            let app = CFStringCreateWithCString(
                std::ptr::null(),
                c"com.apple.universalaccess".as_ptr(),
                UTF8,
            );
            let read = (!key.is_null() && !app.is_null())
                .then(|| {
                    CFPreferencesAppSynchronize(app);
                    let mut valid = 0u8;
                    let on = CFPreferencesGetAppBooleanValue(key, app, &mut valid) != 0;
                    (valid != 0).then_some(on)
                })
                .flatten();
            for object in [key, app] {
                if !object.is_null() {
                    CFRelease(object);
                }
            }
            read
        }
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
        .children(mark_dots_now(kind).into_iter().map(|(x, y)| {
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

/// The person's pet in the window's theme: on light, the hair and body inks one step darker
/// in their own family, as the TUI and the phone draw it (`qoop_tui::theme::light_pet`).
pub fn pet_for(pet: &Avatar, cell: f32, t: &Theme) -> Div {
    if t.dark {
        return self::pet(pet, cell);
    }
    let light = Avatar {
        hair_ink: qoop_tui::theme::light_pet(pet.hair_ink),
        body_ink: qoop_tui::theme::light_pet(pet.body_ink),
        ..pet.clone()
    };
    self::pet(&light, cell)
}

/// The group a row carrying an [`avatar`] joins: under the pointer, the mark's ring lets the
/// row's own fill show.
pub const AVATAR_ROW: &str = "avatar-row";

/// An agent's avatar: its program's logo in a disc (the first letter of any other program),
/// with its state mark stacked at the bottom right. `ring`: the row sits on the column's own
/// ground, so the mark wears a ring of it over the disc; on a row painted otherwise (the one
/// on screen, or one under the pointer) the ring is left out rather than drawn as a patch of
/// another ground. Hidden from a screen reader, like the mark: the row's name says the agent
/// and its state.
pub fn avatar(program: Option<&str>, kind: Kind, size: f32, ring: bool, t: &Theme) -> Div {
    let name = program.unwrap_or_default().trim().to_lowercase();
    let face = if LOGOS.contains(&name.as_str()) {
        svg()
            .path(format!("logos/{name}.svg"))
            .size(px(size * 0.55))
            .flex_none()
            .text_color(c(t.ink))
            .into_any_element()
    } else {
        div()
            .text_size(px(size * 0.45))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(t.ink))
            .child(name.chars().next().unwrap_or('?').to_string())
            .into_any_element()
    };
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(
            div()
                .size_full()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(c(t.disc()))
                .when(!t.dark, |d| d.border_1().border_color(ca(t.line)))
                .child(face),
        )
        .child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .p(px(2.))
                .rounded(px(4.))
                .when(ring, |d| {
                    d.debug_selector(|| "avatar-ring".to_string())
                        .bg(c(t.sidebar))
                        .group_hover(AVATAR_ROW, |s| s.bg(gpui::transparent_black()))
                })
                .child(mark(kind, t, if size >= 22. { 10. } else { 9. })),
        )
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

/// A layer over its parent that asks the scrolling list around it ([`scroll`]) to bring the
/// parent into view in the frame it is drawn: for what just grew or moved out of sight.
pub fn bring_into_view() -> impl IntoElement {
    gpui::canvas(
        |bounds, window, _| window.request_autoscroll(bounds),
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
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
        .border_color(ca(t.line))
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

/// The sheet's frame: a scrim over the window and a card in its middle, holding `body` and,
/// under it, `footer` (its Cancel and Save or Start).
///
/// A sheet taller than the room it opens in never runs off the window: the body scrolls
/// inside the card, its top always in view, and the footer stays under it, so the way out
/// and the way on are always on screen (a remote's schedule sheet carries a line more).
pub fn sheet(
    id: &'static str,
    label: &str,
    t: &Theme,
    body: AnyElement,
    footer: AnyElement,
) -> Stateful<Div> {
    div()
        .id("scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(ca(if t.dark { 0x000000a0 } else { 0x26262640 }))
        .p(px(12.))
        .child(
            div()
                .id(id)
                .debug_selector(move || id.to_string())
                .role(Role::Dialog)
                .aria_label(label.to_string())
                .w(px(560.))
                .max_h_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(20.))
                .rounded(px(18.))
                .bg(c(t.pop))
                .border_1()
                .border_color(ca(t.line))
                .child(scroll(
                    div()
                        .id((id, 1u64))
                        .flex_shrink(1.)
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                ))
                .child(div().flex_none().child(footer)),
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

    /// Marks move only with neither `QOOP_MOTION=0` nor macOS's Reduce Motion asking for
    /// stillness; with it on, a working mark draws its still picture whatever the clock's
    /// frame. (The window's clock stopping is `reduce_motion_stills_the_marks_and_their_clock`.)
    #[test]
    fn reduce_motion_or_qoop_motion_0_keeps_every_mark_still() {
        assert!(motion(false, false));
        assert!(!motion(true, false), "QOOP_MOTION=0 wins");
        assert!(!motion(false, true), "the system's Reduce Motion stops it");
        assert!(!motion(true, true));
        let working = Kind::Status(Status::Working);
        set_beat(5);
        set_reduce_motion(true);
        assert_eq!(
            mark_dots_now(working),
            crate::state::mark_dots(working, None)
        );
        set_reduce_motion(false);
        assert_eq!(
            mark_dots_now(working),
            crate::state::mark_dots(working, Some(5)),
            "moving again: the breath at the clock's frame"
        );
        assert_ne!(
            mark_dots_now(working),
            crate::state::mark_dots(working, None)
        );
    }

    /// The preference read wins when it could be read; refused (as it may be for the app as
    /// `open` launches it), AppKit's own answer is asked.
    #[test]
    fn reduce_motion_falls_back_to_appkit_when_the_preference_is_refused() {
        assert!(reduce_from(Some(true), || panic!("not asked")));
        assert!(!reduce_from(Some(false), || panic!("not asked")));
        assert!(reduce_from(None, || true), "refused: AppKit says on");
        assert!(!reduce_from(None, || false));
        // Both reads run in this process and answer, whatever this Mac's setting is.
        let _ = (mac::preference(), mac::workspace());
    }

    /// A logo is one filled path, never outlined like an icon; a program with none has no
    /// asset (the avatar draws its letter).
    #[test]
    fn a_logo_is_filled_and_an_icon_is_outlined() {
        for name in LOGOS {
            let svg = Assets
                .load(&format!("logos/{name}.svg"))
                .expect("loads")
                .expect("a logo");
            let svg = String::from_utf8(svg.into_owned()).expect("utf8");
            assert!(
                svg.contains("fill=\"#000\"") && !svg.contains("stroke"),
                "{name}"
            );
        }
        assert!(Assets.load("logos/aider.svg").expect("loads").is_none());
        let panel = Assets
            .load("icons/panel.svg")
            .expect("loads")
            .expect("panel");
        assert!(String::from_utf8_lossy(&panel).contains("stroke=\"#000\""));
    }

    /// The Settings button is a cog: one toothed outline round a hub. Rays round a circle (eight
    /// separate strokes) read as a sun.
    #[test]
    fn the_settings_icon_is_a_cog_not_a_sun() {
        let svg = Assets
            .load("icons/gear.svg")
            .expect("loads")
            .expect("a gear");
        let svg = String::from_utf8(svg.into_owned()).expect("utf8");
        let outline = svg.split("<path d=\"").nth(1).expect("an outline");
        let outline = outline.split('"').next().unwrap_or_default();
        assert_eq!(
            outline.matches('M').count(),
            1,
            "one closed outline, not separate rays"
        );
        assert!(
            outline.ends_with('z') && outline.contains('a'),
            "toothed and closed"
        );
        assert!(svg.contains("<circle"), "a hub");
    }
}

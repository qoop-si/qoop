//! How much colour the outer terminal gets, decided once per session.
//!
//! The chrome never spends palette entries 0 to 15 because those belong to the
//! person's theme. What remains is a choice per session: no colour at all,
//! the 256 palette, or truecolour. This module makes that choice from the
//! environment, maps truecolour pane content down to the 256 palette when the
//! terminal cannot take it, and wraps queries for tmux passthrough.
//!
//! Detection order, most certain first: an explicit `QORO_COLOR` override, the
//! safety floor (`dumb`, the `linux` console, legacy conhost), `NO_COLOR`,
//! `COLORTERM`, a conservative Apple Terminal default, `TERM` suffix hints,
//! then the 256 default. The floor comes before `COLORTERM` on purpose: a
//! leaked `COLORTERM=truecolor` over ssh, sudo, tmux or WSL must never cause
//! truecolour bytes on a terminal that cannot draw them.
//!
//! Two deliberate limits, recorded so they read as decisions and not oversights.
//! A terminfo database lookup (`colors` capability, `RGB` flag) is future work:
//! no terminfo crate is vendored, and the environment heuristics below cover the
//! cases that break real sessions. Apple Terminal stays on the 256 path until a
//! device probe records pre Tahoe against Tahoe behaviour (presence of
//! `COLORTERM`, OSC 10 and 11 reply form, scheme query support); the downgrade
//! keeps Tahoe readable in the meantime.

use std::sync::atomic::{AtomicU8, Ordering};

/// No colour: attributes only. `NO_COLOR`, `dumb`, the `linux` console.
const MONO: u8 = 0;
/// The 256 palette. Every modern terminal, and the chrome's only language.
const PALETTE: u8 = 1;
/// Truecolour in, truecolour out. Only with a positive signal.
const TRUE: u8 = 2;

/// The colour level the outer terminal gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Attributes only: bold, inverse, shapes, labels. Never an SGR colour.
    Mono,
    /// 256 palette. RGB pane content is mapped to entries 16 to 255.
    Palette,
    /// Truecolour passes through untouched.
    True,
}

impl Level {
    fn stored(self) -> u8 {
        match self {
            Level::Mono => MONO,
            Level::Palette => PALETTE,
            Level::True => TRUE,
        }
    }

    fn loaded(value: u8) -> Level {
        match value {
            MONO => Level::Mono,
            TRUE => Level::True,
            _ => Level::Palette,
        }
    }
}

static LEVEL: AtomicU8 = AtomicU8::new(TRUE);

/// The level in effect. True until [`apply`] runs at startup, which preserves
/// the historical passthrough for anything that paints before detection.
pub fn level() -> Level {
    Level::loaded(LEVEL.load(Ordering::Relaxed))
}

fn set(level: Level) {
    LEVEL.store(level.stored(), Ordering::Relaxed);
}

/// Decide from the real environment and put it in effect. Called once at
/// startup, before the first frame is painted.
pub fn apply() {
    set(detect());
}

/// The environment that matters, as plain values so tests never touch the
/// process environment.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// Explicit override. `none`, `mono` or `0` mean mono, `256` or `palette`
    /// mean the palette, `true`, `truecolor` or `24bit` mean truecolour.
    /// Anything else is ignored. Beats `NO_COLOR`, per the no-color.org FAQ.
    pub qoro_color: Option<String>,
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub term_program: Option<String>,
    pub wt_session: bool,
    pub windows: bool,
    pub no_color: bool,
    pub tmux: bool,
}

/// Read the real environment into an [`Env`].
pub fn current_env() -> Env {
    let var = |name: &str| std::env::var(name).ok();
    Env {
        qoro_color: var("QORO_COLOR"),
        term: var("TERM"),
        colorterm: var("COLORTERM"),
        term_program: var("TERM_PROGRAM"),
        wt_session: std::env::var_os("WT_SESSION").is_some(),
        windows: std::env::var("OS").is_ok_and(|os| os == "Windows_NT"),
        no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        tmux: std::env::var_os("TMUX").is_some(),
    }
}

/// Decide the level from the environment. Pure, so the precedence is tested.
pub fn detect_from(env: &Env) -> Level {
    if let Some(level) = parse_override(env.qoro_color.as_deref()) {
        return level;
    }
    if floor_is_mono(env) {
        return Level::Mono;
    }
    if env.no_color {
        return Level::Mono;
    }
    if is_true_hint(env.colorterm.as_deref()) {
        return Level::True;
    }
    if env.term_program.as_deref() == Some("Apple_Terminal") {
        // Conservative until the device probe lands (see module docs).
        // A Tahoe host that sets COLORTERM already returned above.
        return Level::Palette;
    }
    match env.term.as_deref().unwrap_or_default() {
        term if is_true_hint_suffix(term) => Level::True,
        term if term.ends_with("256color") => Level::Palette,
        _ => Level::Palette,
    }
}

/// Decide from the real environment.
pub fn detect() -> Level {
    detect_from(&current_env())
}

fn parse_override(value: Option<&str>) -> Option<Level> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "none" | "mono" | "0" => Some(Level::Mono),
        "256" | "palette" => Some(Level::Palette),
        "true" | "truecolor" | "24bit" => Some(Level::True),
        _ => None,
    }
}

/// Terminals that cannot draw extended colour at all, whatever else claims
/// otherwise. `dumb` is a pipe or an unknown, `linux` is the kernel console,
/// and a Windows host without a modern terminal session is legacy conhost.
fn floor_is_mono(env: &Env) -> bool {
    match env.term.as_deref() {
        Some("dumb") | Some("linux") => return true,
        _ => {}
    }
    env.windows && !env.wt_session && env.term_program.is_none()
}

/// `COLORTERM` values that advertise truecolour. Compared case insensitively:
/// `TrueColor` spellings exist in the wild and the cost of wrongly sending
// truecolour is a garbled screen, while the cost of wrongly downgrading is a
// slightly duller one.
fn is_true_hint(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => {
            let lower = raw.to_ascii_lowercase();
            lower.contains("truecolor") || lower.contains("24bit")
        }
    }
}

/// `TERM` suffixes that advertise truecolour directly.
fn is_true_hint_suffix(term: &str) -> bool {
    term.ends_with("direct") || term.ends_with("truecolor")
}

/// Map an RGB colour to the nearest 256 palette entry, searching entries 16
/// to 255 only. Entries 0 to 15 are theme owned: mapping into them would hand
/// the hue back to the person's theme, which is exactly what the downgrade
/// exists to avoid.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    fn entry_rgb(entry: u8) -> [u16; 3] {
        if entry >= 232 {
            let v = u16::from(8 + 10 * (entry - 232));
            return [v, v, v];
        }
        const STEPS: [u16; 6] = [0, 95, 135, 175, 215, 255];
        let i = usize::from(entry - 16);
        [STEPS[i / 36], STEPS[(i / 6) % 6], STEPS[i % 6]]
    }
    let target = [u16::from(r), u16::from(g), u16::from(b)];
    let distance = |entry: u8| {
        let [er, eg, eb] = entry_rgb(entry);
        let (dr, dg, db) = (
            target[0].abs_diff(er),
            target[1].abs_diff(eg),
            target[2].abs_diff(eb),
        );
        u32::from(dr) * dr as u32 + u32::from(dg) * dg as u32 + u32::from(db) * db as u32
    };
    (16..=255u8)
        .min_by_key(|entry| distance(*entry))
        .expect("the 16 to 255 palette range is never empty")
}

/// Wrap bytes in tmux DCS passthrough when running inside tmux, so queries
/// reach the terminal outside instead of being answered by tmux or swallowed.
/// Payload escapes are doubled, which is what tmux expects.
pub fn tmux_wrapped(input: &[u8], in_tmux: bool) -> Vec<u8> {
    if !in_tmux {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for byte in input {
        out.push(*byte);
        if *byte == 0x1b {
            out.push(0x1b);
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            term: Some("xterm-256color".to_string()),
            ..Env::default()
        }
    }

    #[test]
    fn an_explicit_override_beats_everything_including_no_color() {
        let mut full = env();
        full.no_color = true;
        full.qoro_color = Some("true".to_string());
        assert_eq!(detect_from(&full), Level::True);
        full.qoro_color = Some("256".to_string());
        assert_eq!(detect_from(&full), Level::Palette);
        full.qoro_color = Some("none".to_string());
        assert_eq!(detect_from(&full), Level::Mono);
        full.qoro_color = Some("nonsense".to_string());
        assert_eq!(detect_from(&full), Level::Mono);
    }

    #[test]
    fn the_floor_holds_whatever_colorterm_claims() {
        for term in ["dumb", "linux"] {
            let mut guarded = env();
            guarded.term = Some(term.to_string());
            guarded.colorterm = Some("truecolor".to_string());
            assert_eq!(detect_from(&guarded), Level::Mono, "TERM={term}");
        }
        let mut conhost = env();
        conhost.windows = true;
        conhost.term = None;
        conhost.colorterm = Some("truecolor".to_string());
        assert_eq!(detect_from(&conhost), Level::Mono);
        let mut modern = conhost.clone();
        modern.wt_session = true;
        assert_eq!(detect_from(&modern), Level::True);
    }

    #[test]
    fn no_color_means_mono_unless_overridden() {
        let mut plain = env();
        plain.no_color = true;
        assert_eq!(detect_from(&plain), Level::Mono);
    }

    #[test]
    fn colorterm_advertises_truecolor_and_apple_terminal_stays_conservative() {
        let mut rgb = env();
        rgb.colorterm = Some("truecolor".to_string());
        assert_eq!(detect_from(&rgb), Level::True);
        rgb.colorterm = Some("24bit".to_string());
        assert_eq!(detect_from(&rgb), Level::True);
        let mut apple = env();
        apple.term_program = Some("Apple_Terminal".to_string());
        assert_eq!(detect_from(&apple), Level::Palette);
        apple.colorterm = Some("truecolor".to_string());
        assert_eq!(detect_from(&apple), Level::True);
    }

    #[test]
    fn term_suffix_hints_apply_and_the_default_is_the_palette() {
        let mut direct = env();
        direct.term = Some("xterm-direct".to_string());
        assert_eq!(detect_from(&direct), Level::True);
        let mut indexed = env();
        indexed.term = Some("tmux-256color".to_string());
        assert_eq!(detect_from(&indexed), Level::Palette);
        assert_eq!(detect_from(&env()), Level::Palette);
    }

    #[test]
    fn the_downgrade_lands_on_known_entries_and_never_in_the_theme_range() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(255, 102, 0), 202);
        assert_eq!(nearest_256(92, 156, 245), 75);
        for (r, g, b) in [(10, 200, 30), (200, 10, 200), (128, 128, 128)] {
            let entry = nearest_256(r, g, b);
            assert!(entry >= 16, "{r},{g},{b} mapped into the theme range");
        }
    }

    #[test]
    fn tmux_wrapping_is_a_noop_outside_tmux_and_doubles_escapes_inside() {
        let query = b"\x1b]11;?\x1b\\";
        assert_eq!(tmux_wrapped(query, false), query);
        let wrapped = tmux_wrapped(query, true);
        assert!(wrapped.starts_with(b"\x1bPtmux;"), "{wrapped:?}");
        assert!(wrapped.ends_with(b"\x1b\\"), "{wrapped:?}");
        let doubled: &[u8] = b"\x1b\x1b]11;?";
        assert!(
            wrapped.windows(doubled.len()).any(|w| w == doubled),
            "{wrapped:?}"
        );
    }
}

//! The colours and fonts of the window, dark and light, from DECISION section 4: the warm
//! brown dark ground, the white light one, the state inks of IDENTITY.md and the phone's
//! `theme.ts`. Orange is Needs you and nothing else. The terminal's ground stays dark in both.

use gpui::{Hsla, Rgba, rgb, rgba};

pub const UI_FONT: &str = "DM Sans";
pub const MONO_FONT: &str = "JetBrains Mono";

/// The fonts the window draws with, built in: DM Sans at three weights (static instances of
/// the full variable font, Turkish letters included) and JetBrains Mono, with their OFL texts
/// beside them in `assets/fonts`.
pub const FONTS: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/DMSans-Regular.ttf"),
    include_bytes!("../assets/fonts/DMSans-Medium.ttf"),
    include_bytes!("../assets/fonts/DMSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    // Grounds.
    pub bg: u32,
    pub sidebar: u32,
    pub card: u32,
    pub card2: u32,
    pub panel: u32,
    pub pop: u32,
    // Text, loudest to quietest.
    pub ink: u32,
    pub t1: u32,
    pub t2: u32,
    pub t3: u32,
    // Lines and fills: a tint of the ink at an alpha, as `0xRRGGBBAA`.
    pub line: u32,
    pub hair: u32,
    pub fill: u32,
    pub fill2: u32,
    /// The pressed and selected ground (New work's key cap, a switch that is on) and the ink on it.
    pub sel: u32,
    pub on_sel: u32,
    /// The keyboard's focus ring: 3:1 against every ground and every state fill outside it,
    /// and against `bg`, the 1 px band drawn inside it, so it reads on any control's own fill
    /// (a switch that is on included).
    pub focus: u32,
    // States: the mark's ink, and the word's where it differs.
    pub needs: u32,
    pub needs_text: u32,
    pub needs_bg: u32,
    pub needs_line: u32,
    pub work: u32,
    pub work_text: u32,
    pub done: u32,
    pub done_text: u32,
    pub fail: u32,
    pub fail_text: u32,
    pub sleep: u32,
    pub sleep_text: u32,
    pub idle: u32,
    pub badge_fg: u32,
    /// The six space inks, as names wear them.
    pub spaces: [u32; 6],
}

pub const DARK: Theme = Theme {
    dark: true,
    bg: 0x14110f,
    sidebar: 0x0e0c0b,
    card: 0x1d1916,
    card2: 0x24201c,
    panel: 0x191512,
    pop: 0x26211d,
    ink: 0xf3f1ed,
    t1: 0xe6e1da,
    // The design's #b0aaa2 and #938d85, lifted: they read under 4.5:1 on a hovered row or a
    // pressed chip in the top glow. The state words below are lifted for the same grounds.
    t2: 0xc0bcb5,
    t3: 0xa7a29b,
    line: 0xfff0e11f,
    hair: 0xfff0e112,
    fill: 0xfff0e111,
    fill2: 0xfff0e129,
    sel: 0xf3f1ed,
    on_sel: 0x14110f,
    focus: 0x70abff,
    needs: 0xff8700,
    needs_text: 0xff8700,
    needs_bg: 0xff870017,
    needs_line: 0xff87008c,
    work: 0x00afff,
    work_text: 0x52c9ff,
    done: 0x00af5f,
    done_text: 0x21b974,
    fail: 0xff00ff,
    fail_text: 0xff5fff,
    sleep: 0xaf87d7,
    sleep_text: 0xb794db,
    idle: 0x6c6c6c,
    badge_fg: 0x14110f,
    spaces: [0xd787ff, 0xd7ff5f, 0x5fffff, 0xffafd7, 0x87d7ff, 0xffff87],
};

pub const LIGHT: Theme = Theme {
    dark: false,
    bg: 0xffffff,
    sidebar: 0xfdfcfa,
    card: 0xfdfcfa,
    card2: 0xf4f2ee,
    panel: 0xfdfcfa,
    pop: 0xffffff,
    ink: 0x262626,
    t1: 0x333333,
    t2: 0x585858,
    t3: 0x626262,
    line: 0x261e1424,
    hair: 0x261e1417,
    fill: 0x261e140d,
    fill2: 0x261e141f,
    sel: 0x262626,
    on_sel: 0xffffff,
    focus: 0x1f5fc4,
    needs: 0xaf5f00,
    needs_text: 0x875f00,
    needs_bg: 0xaf5f0012,
    needs_line: 0xaf5f0099,
    work: 0x0087d7,
    work_text: 0x005faf,
    done: 0x008700,
    done_text: 0x005f00,
    fail: 0xd700d7,
    fail_text: 0xaf00af,
    sleep: 0x875faf,
    sleep_text: 0x7a52a3,
    idle: 0x767676,
    badge_fg: 0xffffff,
    spaces: [0x8700d7, 0x5f5f00, 0x005f5f, 0xaf005f, 0x005f87, 0x875f00],
};

/// The theme the window draws in, set by its view on every frame it changes, so a view of
/// its own (a text field) made before a switch draws in the new theme too.
impl gpui::Global for Theme {}

/// The theme the window draws in now; dark before the window set one.
pub fn current(cx: &gpui::App) -> Theme {
    cx.try_global::<Theme>().copied().unwrap_or(DARK)
}

/// The terminal's own ground and ink, the same in both themes. Whatever is drawn on it takes
/// `DARK`'s tokens, a control included.
pub const TERMINAL_BG: u32 = 0x0f0f11;

/// The dark theme's warm glow at the window's top (`0xRRGGBBAA`), fading out down 300 px:
/// the top bar and the top of a page sit on it over `bg`.
pub const TOP_GLOW: u32 = 0x965f374d;

/// An opaque token as a colour.
pub fn c(hex: u32) -> Rgba {
    rgb(hex)
}

/// A token with alpha (`0xRRGGBBAA`) as a colour.
pub fn ca(hex: u32) -> Rgba {
    rgba(hex)
}

/// A control state's fill (`0xRRGGBBAA`, over the ground it sits on) and the ink of the label
/// drawn on it: a state fill never ships without the text that reads on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    pub fill: u32,
    pub text: u32,
}

/// An opaque token as `0xRRGGBBAA`.
const fn opaque(hex: u32) -> u32 {
    (hex << 8) | 0xff
}

impl Theme {
    pub fn pick(dark: bool) -> Theme {
        if dark { DARK } else { LIGHT }
    }

    /// A control with a fill of its own at rest (a pill, a qass chip, a segmented group).
    pub fn rest(&self) -> Pair {
        Pair {
            fill: self.fill,
            text: self.ink,
        }
    }

    /// A row, link or quiet button under the pointer. Its quieter lines keep their own inks,
    /// which `every_text_token_reads_on_every_ground` checks on this fill over every ground.
    pub fn hover(&self) -> Pair {
        Pair {
            fill: self.fill,
            text: self.ink,
        }
    }

    /// The page, tab, row or run you are on.
    pub fn current(&self) -> Pair {
        Pair {
            fill: self.fill,
            text: self.ink,
        }
    }

    /// A filled control under the pointer, or open (the Inbox while its panel shows), and a
    /// switch that is off: its knob is the text.
    pub fn pressed(&self) -> Pair {
        Pair {
            fill: self.fill2,
            text: self.ink,
        }
    }

    /// A picked chip, radio or day.
    pub fn picked(&self) -> Pair {
        Pair {
            fill: opaque(self.card2),
            text: self.ink,
        }
    }

    /// The outline a picked chip, radio or day, the current page, tab, row or run, and a text
    /// field wear (DECISION's `.tq.on`): a tint alone is about 1.1:1 against an unpicked
    /// neighbour, the outline 3:1 at least, and a field with no edge shows only its caret.
    pub fn picked_edge(&self) -> u32 {
        self.t3
    }

    /// A switch that is on, the sheet's Start: the selected ground and the ink on it.
    pub fn on(&self) -> Pair {
        Pair {
            fill: opaque(self.sel),
            text: self.on_sel,
        }
    }

    /// A control that cannot be used now (Start with nothing to start, Run now while a run
    /// works). Its label still reads at 4.5:1: no opacity over it. No fill, so it differs from
    /// a live control by more than its text tone; a pill draws a hairline edge instead.
    pub fn off(&self) -> Pair {
        Pair {
            fill: 0,
            text: self.t2,
        }
    }

    /// A key cap (`ctrl-a c`), drawn inside a control in any of the states above but on.
    pub fn key(&self) -> Pair {
        Pair {
            fill: self.fill,
            text: self.t1,
        }
    }

    /// A key cap inside a control that is on (Start's `enter`): the on ground's own ink, on a
    /// tint of it.
    pub fn key_on(&self) -> Pair {
        Pair {
            fill: (self.on_sel << 8) | 0x1f,
            text: self.on_sel,
        }
    }

    /// Every state pair, named, for the contrast test. Focused is not a pair: the ring is
    /// drawn over the state's fill and leaves its text as it is.
    #[cfg(test)]
    pub fn states(&self) -> [(&'static str, Pair); 8] {
        [
            ("rest", self.rest()),
            ("hover", self.hover()),
            ("current", self.current()),
            ("pressed", self.pressed()),
            ("picked", self.picked()),
            ("on", self.on()),
            ("off", self.off()),
            ("key", self.key()),
        ]
    }

    pub fn space(&self, slot: usize) -> Hsla {
        c(self.spaces[slot % 6]).into()
    }
}

/// WCAG relative luminance of an opaque `0xRRGGBB`.
#[cfg(test)]
fn luminance(hex: u32) -> f64 {
    let lin = |v: u32| {
        let s = f64::from(v & 0xff) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(hex >> 16) + 0.7152 * lin(hex >> 8) + 0.0722 * lin(hex)
}

#[cfg(test)]
fn contrast(a: u32, b: u32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `top` (`0xRRGGBBAA`) laid over the opaque `under`.
#[cfg(test)]
fn over(top: u32, under: u32) -> u32 {
    let alpha = f64::from(top & 0xff) / 255.0;
    let mix = |shift: u32| {
        let t = f64::from((top >> (shift + 8)) & 0xff);
        let u = f64::from((under >> shift) & 0xff);
        ((t * alpha + u * (1.0 - alpha)).round() as u32) << shift
    };
    mix(16) | mix(8) | mix(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page grounds a theme draws on. In dark, also the top glow over `bg` (at its
    /// strongest) and the terminal's ground, which only ever carries `DARK`'s tokens.
    fn page_grounds(t: &Theme) -> Vec<u32> {
        let mut pages = vec![t.bg, t.sidebar, t.card, t.card2, t.panel, t.pop];
        if t.dark {
            pages.extend([over(TOP_GLOW, t.bg), TERMINAL_BG]);
        }
        pages
    }

    /// Every text ink reads at 4.5:1 on every ground it is drawn on, in both themes: the
    /// grounds, a hovered row (fill over any of them), a Needs you row (its tint over the
    /// card), and the space names in their inks. A filled control's quieter lines (its icon,
    /// the qass chip's state line) also read on its pressed fill.
    #[test]
    fn every_text_token_reads_on_every_ground() {
        for t in [DARK, LIGHT] {
            let pages = page_grounds(&t);
            let mut grounds = pages.clone();
            grounds.extend(pages.iter().map(|&g| over(t.fill, g)));
            grounds.push(over(t.needs_bg, t.card));
            let mut texts = vec![
                ("ink", t.ink),
                ("t1", t.t1),
                ("t2", t.t2),
                ("t3", t.t3),
                ("needs", t.needs_text),
                ("work", t.work_text),
                ("done", t.done_text),
                ("fail", t.fail_text),
                ("sleep", t.sleep_text),
            ];
            texts.extend(t.spaces.iter().map(|s| ("space", *s)));
            for (name, text) in texts {
                for &ground in &grounds {
                    let ratio = contrast(text, ground);
                    assert!(
                        ratio >= 4.5,
                        "{} {name} #{text:06x} on #{ground:06x}: {ratio:.2}",
                        if t.dark { "dark" } else { "light" }
                    );
                }
            }
            for (name, text) in [("t1", t.t1), ("t2", t.t2), ("work", t.work_text)] {
                for &page in &pages {
                    let ground = over(t.fill2, page);
                    let ratio = contrast(text, ground);
                    assert!(
                        ratio >= 4.5,
                        "{} line {name} #{text:06x} on pressed #{ground:06x}: {ratio:.2}",
                        if t.dark { "dark" } else { "light" }
                    );
                }
            }
            // The badge's figure on Needs you: 11 px semibold is not large text, so 4.5:1.
            let badge = contrast(t.badge_fg, t.needs);
            assert!(badge >= 4.5, "the badge's figure: {badge:.2}");
        }
    }

    /// Every state pair, on every ground it can sit on, in both themes: the label reads at
    /// 4.5:1 on its own fill (the owner's focused qass chip was ink on ink). A ground is a
    /// page ground (the dark top glow and the terminal's ground included) or a filled group
    /// over one (a segmented control). The focus ring reads at 3:1 (WCAG
    /// 1.4.11) against what is outside it (a ground, or a state fill another control sits on)
    /// and against `bg`, its inner band, whatever the control's own fill. A key cap reads
    /// inside a control in every state, in `key_on` when the control is on.
    #[test]
    fn every_state_label_reads_on_its_fill_and_the_ring_on_its_neighbours() {
        for t in [DARK, LIGHT] {
            let theme = if t.dark { "dark" } else { "light" };
            let pages = page_grounds(&t);
            let mut grounds = pages.clone();
            grounds.extend(pages.iter().map(|&g| over(t.fill, g)));
            for (name, pair) in t.states() {
                for &ground in &grounds {
                    let fill = over(pair.fill, ground);
                    let ratio = contrast(pair.text, fill);
                    assert!(
                        ratio >= 4.5,
                        "{theme} {name}: #{:06x} on #{fill:06x}: {ratio:.2}",
                        pair.text
                    );
                    // `on` is the selected ground of one control, never under another; a key
                    // cap in it takes its own pair.
                    if name == "on" {
                        let key = t.key_on();
                        let cap = over(key.fill, fill);
                        let ratio = contrast(key.text, cap);
                        assert!(ratio >= 4.5, "{theme} key cap in on #{cap:06x}: {ratio:.2}");
                        continue;
                    }
                    let ring = contrast(t.focus, fill);
                    assert!(
                        ring >= 3.0,
                        "{theme} ring next to {name} #{fill:06x}: {ring:.2}"
                    );
                    let key = t.key();
                    let cap = over(key.fill, fill);
                    let ratio = contrast(key.text, cap);
                    assert!(
                        ratio >= 4.5,
                        "{theme} key cap in {name} #{cap:06x}: {ratio:.2}"
                    );
                }
            }
            for &ground in &grounds {
                let ring = contrast(t.focus, ground);
                assert!(ring >= 3.0, "{theme} ring on #{ground:06x}: {ring:.2}");
                // A pick, and the page, tab, row or run you are on, are told from their
                // neighbours by the outline, never the tint alone: 3:1 against the ground
                // beside them (an unpicked neighbour's, hovered or not) and their own fill.
                let edge = t.picked_edge();
                for (name, pair) in [("picked", t.picked()), ("current", t.current())] {
                    let own = over(pair.fill, ground);
                    let hovered = over(t.hover().fill, ground);
                    for (what, next) in
                        [("ground", ground), ("hovered", hovered), ("own fill", own)]
                    {
                        let ratio = contrast(edge, next);
                        assert!(
                            ratio >= 3.0,
                            "{theme} {name} edge on its {what} #{next:06x}: {ratio:.2}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn contrast_is_the_wcag_ratio() {
        assert!((contrast(0x000000, 0xffffff) - 21.0).abs() < 0.01);
        assert_eq!(over(0x00000080, 0xffffff), 0x7f7f7f);
    }
}

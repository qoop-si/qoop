//! The few icons the interface draws, the font that carries them, and whether this terminal
//! gets them.
//!
//! A terminal draws characters from fonts, never pictures, so an icon is a glyph: twelve from
//! the Nerd Fonts symbol sets in `assets/QoopSymbols.ttf`, built by
//! `scripts/build-icon-font.py`, which fits every one to one box and one centre line (the
//! sets were drawn apart and stood at different heights) and puts them at our own codepoints
//! in the supplementary private use plane. iTerm2, Terminal, Ghostty and kitty draw a glyph
//! their font lacks from any installed font that has it, which is why qoop installs its own
//! into the person's font folder: `install.sh` and `qoop update` through `qoop icons install`,
//! and the interface when it finds it missing. WezTerm and Alacritty never look past their own
//! fonts, and a terminal never says it failed to draw a glyph, so icons are drawn only where
//! they were seen to draw; everywhere else each one keeps the glyph or word the interface drew
//! before.
//!
//! Measured on 2026-10-02 (macOS): with the font installed and nothing else changed, a plane
//! 16 glyph drew in iTerm2 3.7.3, Terminal, Ghostty 1.3.1 and kitty 0.49.2, and boxes in
//! WezTerm 20240203 and Alacritty. Terminal drew boxes for the Nerd codepoints in the basic
//! plane's private use area, which is why ours are in plane 16. XTVERSION (`CSI > 0 q`) named
//! iTerm2, ghostty, kitty and WezTerm; Terminal and Alacritty did not answer, and kitty and
//! Alacritty inherited `TERM_PROGRAM=iTerm.app` from the shell that started them, which is
//! why the answer beats the environment.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::colorlevel::Env;

/// The font, embedded so every install path (the script, `qoop update`, a copied binary)
/// carries it.
pub const FONT: &[u8] = include_bytes!("../assets/QoopSymbols.ttf");

/// The file name this build's font is installed under: the name and a hash of the bytes. A
/// changed font is a new file, which macOS hands to running terminals at once; the same name
/// written over would leave them drawing from the copy they already loaded.
fn font_file() -> String {
    // FNV-1a, enough to tell two builds of a 3 KB file apart.
    let hash = FONT.iter().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    });
    format!("QoopSymbols-{hash:08x}.ttf")
}

/// An asleep tab's mark (Material Design `sleep`).
pub const ASLEEP: char = '\u{10c0a0}';
/// Keep awake, empty or full by colour (Codicons `coffee`).
pub const CUP: char = '\u{10c0a1}';
/// A space of another machine (Codicons `remote`).
pub const REMOTE: char = '\u{10c0a2}';
/// A space's settings (Material Design `cog-outline`).
pub const SETTINGS: char = '\u{10c0a3}';
/// Sleep, on an awake space: an empty moon (Font Awesome `moon-o`).
pub const SLEEP: char = '\u{10c0a4}';
/// Wake, on a sleeping space: the same moon filled (Material Design `power-sleep`).
pub const WAKE: char = '\u{10c0a5}';
/// The closed drawer of a space's controls (Material Design `dots-horizontal`).
pub const MORE: char = '\u{10c0a6}';
/// The open drawer, a click putting it back (Material Design `chevron-right`).
pub const LESS: char = '\u{10c0a7}';
/// qass watches the agents: an open eye beside the pet (Material Design `eye`).
pub const EYE: char = '\u{10c0a8}';
/// qass does not watch: the eye shut, a lid and three lashes drawn by the font script in the
/// place of Material Design `eye-off`.
pub const EYE_SHUT: char = '\u{10c0a9}';
/// Notifications on, on the foot beside the cog (Material Design `bell-outline`).
pub const BELL: char = '\u{10c0aa}';
/// Notifications muted, here or everywhere by colour (Material Design `bell-off-outline`).
pub const BELL_OFF: char = '\u{10c0ab}';

/// The glyphs by the Nerd names `scripts/build-icon-font.py` prints, in its order.
pub const GLYPHS: [(&str, char); 12] = [
    ("md-sleep", ASLEEP),
    ("cod-coffee", CUP),
    ("cod-remote", REMOTE),
    ("md-cog_outline", SETTINGS),
    ("fa-moon_o", SLEEP),
    ("md-power_sleep", WAKE),
    ("md-dots_horizontal", MORE),
    ("md-chevron_right", LESS),
    ("md-eye", EYE),
    ("md-eye_off", EYE_SHUT),
    ("md-bell_outline", BELL),
    ("md-bell_off_outline", BELL_OFF),
];

static ON: AtomicBool = AtomicBool::new(false);

/// Whether the painters draw icons this frame.
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Set what [`on`] answers. Returns whether it changed, so the caller repaints.
pub fn set(on: bool) -> bool {
    ON.swap(on, Ordering::Relaxed) != on
}

/// The `icons` line of the config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Setting {
    /// Where they were seen to draw.
    #[default]
    Auto,
    On,
    Off,
}

impl Setting {
    pub fn name(self) -> &'static str {
        match self {
            Setting::Auto => "auto",
            Setting::On => "on",
            Setting::Off => "off",
        }
    }
}

/// Who answered XTVERSION, by what that means for the glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// iTerm2, Ghostty, kitty: they draw our glyphs from the installed font.
    FromFont,
    /// Anything else: WezTerm, which never looks past its own fonts, and tmux, which
    /// answers for itself.
    Other,
}

/// What a terminal's XTVERSION name (`ghostty 1.3.1`, `kitty(0.49.2)`, `iTerm2 3.7.3`,
/// `WezTerm 20240203-...`, `tmux 3.5a`) means here.
pub fn answer_of(name: &str) -> Answer {
    let name = name.trim().to_ascii_lowercase();
    if ["ghostty", "kitty", "iterm2"]
        .iter()
        .any(|t| name.starts_with(t))
    {
        Answer::FromFont
    } else {
        Answer::Other
    }
}

/// What the terminal said to XTVERSION.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Asked, nothing yet: the first frames.
    Waiting,
    /// No answer in time, or never asked.
    Silent,
    Named(Answer),
}

/// Whether the environment names a terminal that draws from installed fonts: iTerm2, Terminal,
/// Ghostty or kitty, by what each sets itself. Alacritty and WezTerm never do.
fn draws_from_fonts(env: &Env) -> bool {
    !env.alacritty
        && !env.wezterm
        && (env.ghostty
            || env.kitty
            || matches!(
                env.term_program.as_deref(),
                Some("iTerm.app" | "Apple_Terminal" | "ghostty")
            ))
}

/// Whether to draw icons. Pure, so every row of the measured table is a test.
///
/// `font` is whether this machine has the glyphs in its font folder; the interface on
/// another machine than the terminal cannot see that folder, so over ssh it never counts.
pub fn decide(setting: Setting, env: &Env, reply: &Reply, font: bool, mono: bool) -> bool {
    match setting {
        Setting::On => return true,
        Setting::Off => return false,
        Setting::Auto => {}
    }
    if mono || env.tmux || matches!(env.term.as_deref(), Some("dumb" | "linux")) {
        return false;
    }
    let local_font = font && !env.ssh;
    match reply {
        Reply::Named(Answer::FromFont) => local_font,
        Reply::Named(Answer::Other) => false,
        // Terminal never answers XTVERSION; neither does Alacritty, whose own variable says
        // so even when it inherited Terminal's.
        Reply::Silent => {
            env.term_program.as_deref() == Some("Apple_Terminal") && !env.alacritty && local_font
        }
        Reply::Waiting => draws_from_fonts(env) && local_font,
    }
}

/// Where iTerm2 and Terminal find a person's fonts. Only macOS: everywhere else the icons
/// are drawn only by terminals that carry the glyphs themselves.
fn font_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Fonts"))
}

/// Whether the installed font is this build's.
pub fn installed() -> bool {
    let Some(path) = font_dir().map(|dir| dir.join(font_file())) else {
        return false;
    };
    let here = std::fs::read(&path).is_ok_and(|b| b == FONT);
    if here {
        // Seen in use today: another build's cleanup counts a week from here.
        let _ = std::fs::File::options()
            .append(true)
            .open(&path)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    }
    here
}

/// Put the font where the terminal's font fallback finds it, when the interface runs in one
/// that draws from installed fonts, on this machine, and it is not there yet.
///
/// The interface draws icons only from a font that was there when it started: a terminal
/// that is already running looks for a glyph before a font written that moment is
/// registered, and iTerm2 never listens for a font arriving, so the first frames would be
/// boxes that stay. The install and update put the font in place long before that.
pub fn install(env: &Env) {
    if env.ssh || env.tmux || !draws_from_fonts(env) || installed() {
        return;
    }
    if let Err(e) = install_font() {
        eprintln!("qoop: the icon font was not installed: {e}");
    }
}

/// Put this build's font into the font folder, whatever terminal this runs in: the install
/// and the update, for the terminals the person opens later. Written beside itself and renamed
/// in, so a reader never sees half a font. Where it is, or `None` where there is no folder
/// for it (anything but macOS).
pub fn install_font() -> std::io::Result<Option<PathBuf>> {
    let Some(dir) = font_dir() else {
        return Ok(None);
    };
    if !installed() {
        write_font(&dir)
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", dir.display())))?;
    }
    Ok(Some(dir.join(font_file())))
}

/// Take every font of qoop's out of the font folder, this build's and older ones. What it
/// removed.
pub fn uninstall_font() -> std::io::Result<Vec<PathBuf>> {
    let Some(dir) = font_dir() else {
        return Ok(Vec::new());
    };
    remove_fonts(&dir)
}

fn remove_fonts(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries?,
    };
    let mut removed = Vec::new();
    for entry in entries.flatten() {
        if is_ours(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
            removed.push(entry.path());
        }
    }
    Ok(removed)
}

/// A font file qoop wrote: this name, hashed or not, or the one it had before the rename.
fn is_ours(file: &str) -> bool {
    let Some(stem) = file.strip_suffix(".ttf") else {
        return false;
    };
    ["QoopSymbols", "QoroSymbols"].iter().any(|name| {
        stem.strip_prefix(name)
            .is_some_and(|hash| hash.is_empty() || hash.starts_with('-'))
    })
}

fn write_font(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let name = font_file();
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    // A write cut short (a full disk) leaves nothing behind either.
    let written = std::fs::write(&tmp, FONT).and_then(|()| std::fs::rename(&tmp, dir.join(&name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return written;
    }
    // Older fonts go: the unhashed name of the first builds, and the font of a build no
    // interface has started for a week. A newer build's font is left alone, since another
    // qoop, a release beside a build from source, may be drawing from it right now.
    let week = std::time::Duration::from_secs(7 * 24 * 60 * 60);
    for entry in std::fs::read_dir(dir)?.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > week);
        let hashed = file.starts_with("QoopSymbols-") && is_ours(&file);
        if file != name && (file == "QoopSymbols.ttf" || (hashed && stale)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(term_program: Option<&str>) -> Env {
        Env {
            term: Some("xterm-256color".to_string()),
            term_program: term_program.map(String::from),
            ..Env::default()
        }
    }

    fn named(name: &str) -> Reply {
        Reply::Named(answer_of(name))
    }

    /// The font file holds exactly the glyphs the painters draw, at the same codepoints:
    /// a rebuild that moved one would draw the wrong picture with no error anywhere.
    #[test]
    fn the_font_holds_every_glyph_the_table_names() {
        let cmap = font_cmap(FONT);
        for (name, glyph) in GLYPHS {
            assert!(
                cmap.contains(&(glyph as u32)),
                "{name} {:x} not in the font",
                glyph as u32
            );
        }
        assert_eq!(cmap.len(), GLYPHS.len(), "nothing else in the font");
        // Ours, in plane 16: Terminal draws boxes for the basic plane's private use area.
        assert!(GLYPHS.iter().all(|(_, g)| *g as u32 >= 0x10_0000));
    }

    /// The six terminals measured on 2026-10-02, each as it answers.
    #[test]
    fn icons_draw_only_where_they_were_seen_to_draw() {
        let auto = Setting::Auto;
        // iTerm2, Ghostty and kitty draw ours from the installed font, and only with it;
        // kitty inherits TERM_PROGRAM from the shell that started it.
        for (program, name) in [
            (Some("iTerm.app"), "iTerm2 3.7.3"),
            (Some("ghostty"), "ghostty 1.3.1"),
            (Some("iTerm.app"), "kitty(0.49.2)"),
        ] {
            assert!(
                decide(auto, &probe(program), &named(name), true, false),
                "{name}"
            );
            assert!(
                !decide(auto, &probe(program), &named(name), false, false),
                "{name}"
            );
        }
        // Terminal never answers, and draws from the font too.
        assert!(decide(
            auto,
            &probe(Some("Apple_Terminal")),
            &Reply::Silent,
            true,
            false
        ));
        assert!(!decide(
            auto,
            &probe(Some("Apple_Terminal")),
            &Reply::Silent,
            false,
            false
        ));
        // WezTerm and Alacritty never look past their own fonts.
        let wezterm = Env {
            wezterm: true,
            ..probe(Some("WezTerm"))
        };
        assert!(!decide(
            auto,
            &wezterm,
            &named("WezTerm 20240203-110809-5046fc22"),
            true,
            false
        ));
        assert!(!decide(auto, &wezterm, &Reply::Waiting, true, false));
        let alacritty = Env {
            alacritty: true,
            ..probe(Some("iTerm.app"))
        };
        assert!(!decide(auto, &alacritty, &Reply::Silent, true, false));
        assert!(!decide(auto, &alacritty, &Reply::Waiting, true, false));
        let from_terminal = Env {
            alacritty: true,
            ..probe(Some("Apple_Terminal"))
        };
        assert!(!decide(auto, &from_terminal, &Reply::Silent, true, false));
    }

    /// A terminal that answers is believed over the environment: a silence from what the
    /// environment called iTerm2 is some other terminal wearing an inherited variable.
    #[test]
    fn the_answer_beats_the_environment() {
        let auto = Setting::Auto;
        let iterm = probe(Some("iTerm.app"));
        assert!(
            decide(auto, &iterm, &Reply::Waiting, true, false),
            "first frames guess"
        );
        assert!(
            !decide(auto, &iterm, &Reply::Silent, true, false),
            "iTerm2 always answers"
        );
        assert!(!decide(auto, &iterm, &named("xterm(390)"), true, false));
    }

    /// Where the glyph could not be checked: tmux answers for itself, the interface on a
    /// remote cannot see the local fonts, and plain or colourless terminals get plain text.
    #[test]
    fn tmux_ssh_and_plain_terminals_keep_todays_glyphs() {
        let auto = Setting::Auto;
        let tmux = Env {
            tmux: true,
            ..probe(Some("iTerm.app"))
        };
        assert!(!decide(auto, &tmux, &named("tmux 3.5a"), true, false));
        assert!(!decide(
            auto,
            &probe(None),
            &named("tmux 3.5a"),
            true,
            false
        ));
        let ssh = Env {
            ssh: true,
            ..probe(Some("iTerm.app"))
        };
        assert!(!decide(auto, &ssh, &named("iTerm2 3.7.3"), true, false));
        assert!(!decide(auto, &ssh, &named("ghostty 1.3.1"), true, false));
        let dumb = Env {
            term: Some("dumb".to_string()),
            ..probe(None)
        };
        assert!(!decide(auto, &dumb, &named("ghostty 1.3.1"), true, false));
        assert!(!decide(
            auto,
            &probe(Some("ghostty")),
            &named("ghostty 1.3.1"),
            true,
            true
        ));
        assert!(
            !decide(auto, &probe(None), &Reply::Silent, true, false),
            "unknown and silent"
        );
    }

    #[test]
    fn the_config_line_forces_either_way() {
        let alacritty = Env {
            alacritty: true,
            ..probe(None)
        };
        assert!(decide(
            Setting::On,
            &alacritty,
            &Reply::Silent,
            false,
            false
        ));
        assert!(!decide(
            Setting::Off,
            &probe(Some("ghostty")),
            &named("ghostty 1.3.1"),
            true,
            false
        ));
    }

    /// Installing writes the embedded font once, in the right place, and leaves an equal
    /// file alone.
    #[test]
    fn the_font_installs_once_into_the_font_folder() {
        let dir = std::env::temp_dir().join(format!("qoop-font-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_font(&dir).expect("write");
        // The first builds' unhashed font goes; a newer build's, in use, stays.
        std::fs::write(dir.join("QoopSymbols.ttf"), b"old").expect("old");
        std::fs::write(dir.join("QoopSymbols-0000beef.ttf"), b"other").expect("other");
        write_font(&dir).expect("again");
        assert!(
            dir.join("QoopSymbols-0000beef.ttf").exists(),
            "another build's font stays"
        );
        std::fs::remove_file(dir.join("QoopSymbols-0000beef.ttf")).expect("tidy");
        assert_eq!(std::fs::read(dir.join(font_file())).expect("read"), FONT);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy() != font_file())
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temporary file and no older font left: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Uninstalling takes every font qoop wrote, under either name, and nothing beside them.
    #[test]
    fn uninstall_takes_only_qoops_fonts() {
        let dir = std::env::temp_dir().join(format!("qoop-font-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let ours = [
            "QoopSymbols-aad02378.ttf",
            "QoopSymbols.ttf",
            "QoroSymbols-b64d777f.ttf",
        ];
        let others = ["Menlo.ttf", "QoopSymbols-1.otf", "QoopSymbolsExtra.ttf"];
        for f in ours.iter().chain(&others) {
            std::fs::write(dir.join(f), b"x").expect("write");
        }
        let mut removed: Vec<_> = remove_fonts(&dir)
            .expect("remove")
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(removed, ours);
        for f in others {
            assert!(dir.join(f).exists(), "{f} stays");
        }
        assert!(
            remove_fonts(&dir.join("none"))
                .expect("no folder")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The codepoints of a TrueType font's format 4 and 12 cmap subtables, read by hand so
    /// the test needs no font crate.
    fn font_cmap(font: &[u8]) -> std::collections::BTreeSet<u32> {
        let u16_at = |at: usize| u16::from_be_bytes([font[at], font[at + 1]]) as usize;
        let u32_at = |at: usize| {
            u32::from_be_bytes([font[at], font[at + 1], font[at + 2], font[at + 3]]) as usize
        };
        let tables = u16_at(4);
        let cmap = (0..tables)
            .map(|i| 12 + 16 * i)
            .find(|&rec| &font[rec..rec + 4] == b"cmap")
            .map(|rec| u32_at(rec + 8))
            .expect("a cmap table");
        let mut out = std::collections::BTreeSet::new();
        for i in 0..u16_at(cmap + 2) {
            let sub = cmap + u32_at(cmap + 4 + 8 * i + 4);
            match u16_at(sub) {
                4 => {
                    let segs = u16_at(sub + 6) / 2;
                    for s in 0..segs {
                        let end = u16_at(sub + 14 + 2 * s);
                        let start = u16_at(sub + 16 + 2 * segs + 2 * s);
                        if start != 0xffff {
                            out.extend(start as u32..=end as u32);
                        }
                    }
                }
                12 => {
                    let groups = u32_at(sub + 12);
                    for gi in 0..groups {
                        let at = sub + 16 + 12 * gi;
                        out.extend(u32_at(at) as u32..=u32_at(at + 4) as u32);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

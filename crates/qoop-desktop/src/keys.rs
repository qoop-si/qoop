//! A key press as the bytes a terminal program reads.
//!
//! xterm's legacy encoding: arrows as `CSI A` (or `SS3 A` when the program asked for
//! application cursor keys), modified keys as `CSI 1;m A`, Ctrl folding a letter to its
//! control byte. Option composes characters, as in Terminal.app: on German, Turkish and
//! most European Mac layouts it is how `@ [ ] { } | ~` are typed. Only when a person asks
//! for it is Option Meta, an ESC prefix. Option and Cmd with the arrows and Backspace are the
//! macOS word and line keys (`ESC b`, `ESC f`, `ESC DEL`, `^A`, `^E`, `^U`), as Ghostty and
//! iTerm2 send them. A new line in an agent's prompt is how the three terminals send it: with
//! the kitty keyboard protocol on (the program pushed `CSI > 1 u`, the snapshot carries the
//! flags) a modified Enter is `CSI 13;m u`, Shift+Enter being `CSI 13;2u`; Option+Enter is
//! `ESC CR`; Ctrl+J is a line feed. With the protocol off Shift+Enter is a plain return, as
//! in any terminal.

/// The key as GPUI reports it, without GPUI's types, so the encoder tests run anywhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct Press<'a> {
    pub key: &'a str,
    pub key_char: Option<&'a str>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
    /// Option is Meta (an ESC prefix) rather than a compose key. Off unless asked for.
    pub option_is_meta: bool,
    /// The kitty keyboard flags the program pushed (`ModesWire::kitty_keyboard`), 0 when none.
    pub kitty: u8,
}

/// The kitty protocol's first flag: keys the legacy bytes cannot tell apart get `CSI u` forms.
const KITTY_DISAMBIGUATE: u8 = 1;

/// What a keystroke types, for the window's own one-letter keys: the ASCII character the
/// layout produced (`/` is shift-7 on a Turkish Q keyboard), else the key's name (`c` on a
/// Russian layout that types a Cyrillic letter there, `tab`, `escape`).
pub fn typed(k: &gpui::Keystroke) -> &str {
    k.key_char
        .as_deref()
        .filter(|c| c.len() == 1 && c.bytes().all(|b| b.is_ascii_graphic()))
        .unwrap_or(&k.key)
}

/// Whether the press asks for a new line rather than to send: Shift+Enter, Option+Enter or
/// Ctrl+J. Text fields that send on Enter and the terminal agree on it.
pub fn newline(k: &gpui::Keystroke) -> bool {
    let m = &k.modifiers;
    let enter = k.key == "enter" && (m.shift || m.alt);
    let line_feed = k.key == "j" && m.control;
    !m.platform && (enter || line_feed)
}

/// The bytes for one press, or `None` for a press the terminal does not get (Cmd chords
/// belong to the app).
pub fn encode(p: Press<'_>, app_cursor: bool) -> Option<Vec<u8>> {
    // The macOS word and line keys, as Ghostty's defaults and iTerm2's Natural Text Editing send
    // them: readline, zsh, Claude Code and Codex all read these, and shells do not read the
    // modified CSI (`CSI 1;3D`) xterm would send. Option composes nothing on these keys.
    let plain = !p.ctrl && !p.shift;
    if p.cmd && plain && !p.alt {
        return match p.key {
            "left" => Some(vec![0x01]),
            "right" => Some(vec![0x05]),
            "backspace" => Some(vec![0x15]),
            _ => None,
        };
    }
    if p.cmd {
        return None;
    }
    if p.alt && plain && !p.option_is_meta {
        match p.key {
            "left" => return Some(b"\x1bb".to_vec()),
            "right" => return Some(b"\x1bf".to_vec()),
            "backspace" => return Some(b"\x1b\x7f".to_vec()),
            _ => {}
        }
    }
    let modifier = 1 + u8::from(p.shift) + 2 * u8::from(p.alt) + 4 * u8::from(p.ctrl);
    // Legacy bytes cannot tell Shift+Enter from Enter: a program that asked for the kitty
    // protocol reads the `CSI u` form (Claude Code and Codex insert a new line on it).
    if p.key == "enter" && modifier > 1 && p.kitty & KITTY_DISAMBIGUATE != 0 {
        return Some(format!("\x1b[13;{modifier}u").into_bytes());
    }
    let cursor = |end: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{end}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{end}").into_bytes()
        } else {
            format!("\x1b[{end}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let meta = p.alt && p.option_is_meta;
    let alt = |bytes: Vec<u8>| -> Vec<u8> {
        if meta {
            let mut out = vec![0x1b];
            out.extend(bytes);
            out
        } else {
            bytes
        }
    };
    let bytes = match p.key {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        // Enter composes nothing, so Option+Enter is `ESC CR` whether or not Option is Meta:
        // what Claude Code reads as a new line when the kitty protocol is off.
        "enter" if p.alt => b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "escape" => alt(b"\x1b".to_vec()),
        "backspace" if p.ctrl => alt(vec![0x08]),
        "backspace" => alt(vec![0x7f]),
        "tab" if p.shift => b"\x1b[Z".to_vec(),
        "tab" => alt(b"\t".to_vec()),
        "space" if p.ctrl => alt(vec![0]),
        key if p.ctrl => alt(vec![control(key)?]),
        // Meta sends ESC and the key itself, not the character macOS composes for it
        // (Option+s is "ß"): the convention shells and agents read.
        key if meta && key.chars().count() == 1 => {
            let base = if p.shift {
                key.to_uppercase()
            } else {
                key.to_string()
            };
            alt(base.into_bytes())
        }
        "space" if meta => alt(b" ".to_vec()),
        "space" => b" ".to_vec(),
        _ => p.key_char?.as_bytes().to_vec(),
    };
    Some(bytes)
}

/// Ctrl plus a key, as the control byte it stands for.
fn control(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' => Some(0x1f),
        '/' => Some(0x1f),
        '8' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str) -> Press<'_> {
        Press {
            key,
            key_char: Some(key),
            ..Press::default()
        }
    }

    #[test]
    fn plain_text_is_its_character() {
        let p = Press {
            key: "a",
            key_char: Some("A"),
            shift: true,
            ..Press::default()
        };
        assert_eq!(encode(p, false), Some(b"A".to_vec()));
        let p = Press {
            key: "u",
            key_char: Some("ü"),
            ..Press::default()
        };
        assert_eq!(encode(p, false), Some("ü".as_bytes().to_vec()));
    }

    #[test]
    fn arrows_follow_the_cursor_mode() {
        assert_eq!(encode(key("up"), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(key("up"), true), Some(b"\x1bOA".to_vec()));
        let shifted = Press {
            shift: true,
            ..key("left")
        };
        assert_eq!(encode(shifted, true), Some(b"\x1b[1;2D".to_vec()));
    }

    #[test]
    fn ctrl_folds_and_alt_prefixes() {
        let ctrl_c = Press {
            ctrl: true,
            ..key("c")
        };
        assert_eq!(encode(ctrl_c, false), Some(vec![3]));
        let meta_b = Press {
            alt: true,
            option_is_meta: true,
            key: "b",
            key_char: Some("∫"),
            ..Press::default()
        };
        assert_eq!(encode(meta_b, false), Some(b"\x1bb".to_vec()));
        let meta_shift_s = Press {
            alt: true,
            shift: true,
            option_is_meta: true,
            key: "s",
            key_char: Some("Í"),
            ..Press::default()
        };
        assert_eq!(encode(meta_shift_s, false), Some(b"\x1bS".to_vec()));
        assert_eq!(encode(key("enter"), false), Some(b"\r".to_vec()));
        assert_eq!(encode(key("backspace"), false), Some(vec![0x7f]));
    }

    /// German layout: Option+L is `@`, Option+5 is `[`. Without Meta they type that.
    #[test]
    fn option_composes_by_default() {
        for (key, composed) in [("l", "@"), ("5", "["), ("8", "{"), ("7", "|"), ("n", "~")] {
            let p = Press {
                alt: true,
                key,
                key_char: Some(composed),
                ..Press::default()
            };
            assert_eq!(
                encode(p, false),
                Some(composed.as_bytes().to_vec()),
                "{key}"
            );
        }
    }

    /// A new line in an agent's prompt, as Ghostty, iTerm2 and kitty send it.
    #[test]
    fn new_line_keys_reach_the_agent() {
        let with = |key_name, shift, alt, ctrl, kitty| Press {
            shift,
            alt,
            ctrl,
            kitty,
            ..key(key_name)
        };
        // The kitty protocol on (flag 1 pushed, other flags beside it): the `CSI u` forms.
        for kitty in [1, 3, 5, 0b11111] {
            let enc = |s, a, c| encode(with("enter", s, a, c, kitty), false);
            assert_eq!(enc(true, false, false), Some(b"\x1b[13;2u".to_vec()));
            assert_eq!(enc(false, true, false), Some(b"\x1b[13;3u".to_vec()));
            assert_eq!(enc(false, false, true), Some(b"\x1b[13;5u".to_vec()));
            assert_eq!(
                enc(false, false, false),
                Some(b"\r".to_vec()),
                "plain Enter"
            );
        }
        // Off, or only flags that leave Enter alone: Shift+Enter is a return, as in any terminal.
        for kitty in [0, 2, 4] {
            let shift = with("enter", true, false, false, kitty);
            assert_eq!(encode(shift, false), Some(b"\r".to_vec()));
        }
        // Option+Enter is ESC CR with Option composing text or as Meta, and in either protocol state.
        let alt = with("enter", false, true, false, 0);
        assert_eq!(encode(alt, false), Some(b"\x1b\r".to_vec()));
        let meta = Press {
            option_is_meta: true,
            ..alt
        };
        assert_eq!(encode(meta, false), Some(b"\x1b\r".to_vec()));
        // Ctrl+J is a line feed.
        let ctrl_j = with("j", false, false, true, 0);
        assert_eq!(encode(ctrl_j, false), Some(b"\n".to_vec()));
        // Cmd+Enter stays with the app.
        let cmd = Press {
            cmd: true,
            kitty: 1,
            ..key("enter")
        };
        assert_eq!(encode(cmd, false), None);
    }

    /// The other modified keys, as xterm sends them: what agents' key readers expect.
    #[test]
    fn modified_navigation_keys() {
        let with = |key_name, shift, alt, ctrl| Press {
            shift,
            alt,
            ctrl,
            ..key(key_name)
        };
        let enc = |p| encode(p, false);
        assert_eq!(
            enc(with("tab", true, false, false)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(enc(with("tab", false, false, false)), Some(b"\t".to_vec()));
        assert_eq!(
            enc(with("up", false, true, false)),
            Some(b"\x1b[1;3A".to_vec())
        );
        assert_eq!(
            enc(with("right", false, false, true)),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            enc(with("left", true, false, true)),
            Some(b"\x1b[1;6D".to_vec())
        );
        assert_eq!(
            enc(with("home", false, false, false)),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            enc(with("end", false, false, false)),
            Some(b"\x1b[F".to_vec())
        );
        assert_eq!(encode(key("home"), true), Some(b"\x1bOH".to_vec()));
        assert_eq!(
            enc(with("pageup", false, false, false)),
            Some(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            enc(with("pagedown", false, false, false)),
            Some(b"\x1b[6~".to_vec())
        );
        assert_eq!(
            enc(with("pageup", true, false, false)),
            Some(b"\x1b[5;2~".to_vec())
        );
        assert_eq!(
            enc(with("delete", false, false, true)),
            Some(b"\x1b[3;5~".to_vec())
        );
        // Option as Meta: an ESC prefix on a letter.
        let meta = Press {
            alt: true,
            option_is_meta: true,
            key: "f",
            key_char: Some("\u{192}"),
            ..Press::default()
        };
        assert_eq!(enc(meta), Some(b"\x1bf".to_vec()));
    }

    /// The window's own Enter and Ctrl+J: a new line only when a field takes it.
    #[test]
    fn newline_names_the_three_keys() {
        let stroke = |text: &'static str| gpui::Keystroke::parse(text).expect("a keystroke");
        for yes in ["shift-enter", "alt-enter", "ctrl-j"] {
            assert!(newline(&stroke(yes)), "{yes}");
        }
        for no in ["enter", "cmd-enter", "j", "ctrl-k", "shift-j", "tab"] {
            assert!(!newline(&stroke(no)), "{no}");
        }
    }

    /// The window's keys by what the layout typed when it is ASCII, else by the key: `/` on a
    /// Turkish Q shift-7, `c` on a Russian layout's `c` key.
    #[test]
    fn typed_takes_ascii_and_falls_back_to_the_key() {
        let stroke = |key: &str, key_char: Option<&str>| gpui::Keystroke {
            key: key.into(),
            key_char: key_char.map(Into::into),
            ..gpui::Keystroke::default()
        };
        assert_eq!(typed(&stroke("7", Some("/"))), "/");
        assert_eq!(typed(&stroke("c", Some("\u{0441}"))), "c");
        assert_eq!(typed(&stroke("x", Some("\u{03c7}"))), "x");
        assert_eq!(typed(&stroke("tab", Some("\t"))), "tab");
        assert_eq!(typed(&stroke("escape", None)), "escape");
    }

    /// Option and Cmd with the arrows and Backspace, as Ghostty's macOS defaults and iTerm2's
    /// Natural Text Editing send them: shells read these, not a modified CSI.
    #[test]
    fn mac_word_and_line_keys() {
        let alt = |k| Press {
            alt: true,
            ..key(k)
        };
        let cmd = |k| Press {
            cmd: true,
            ..key(k)
        };
        assert_eq!(encode(alt("left"), false), Some(b"\x1bb".to_vec()));
        assert_eq!(encode(alt("right"), true), Some(b"\x1bf".to_vec()));
        assert_eq!(encode(alt("backspace"), false), Some(b"\x1b\x7f".to_vec()));
        assert_eq!(encode(cmd("left"), false), Some(vec![0x01]));
        assert_eq!(encode(cmd("right"), false), Some(vec![0x05]));
        assert_eq!(encode(cmd("backspace"), false), Some(vec![0x15]));
        // Shift and Ctrl keep xterm's modified keys; other Cmd chords stay with the app.
        let alt_shift = Press {
            shift: true,
            ..alt("left")
        };
        assert_eq!(encode(alt_shift, false), Some(b"\x1b[1;4D".to_vec()));
        let cmd_up = cmd("up");
        assert_eq!(encode(cmd_up, false), None);
    }

    #[test]
    fn cmd_chords_stay_with_the_app() {
        let cmd_q = Press {
            cmd: true,
            ..key("q")
        };
        assert_eq!(encode(cmd_q, false), None);
    }
}

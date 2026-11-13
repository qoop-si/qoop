//! A key press as the bytes a terminal program reads.
//!
//! xterm's legacy encoding: arrows as `CSI A` (or `SS3 A` when the program asked for
//! application cursor keys), modified keys as `CSI 1;m A`, Ctrl folding a letter to its
//! control byte. Option composes characters, as in Terminal.app: on German, Turkish and
//! most European Mac layouts it is how `@ [ ] { } | ~` are typed. Only when a person asks
//! for it is Option Meta, an ESC prefix. The kitty keyboard protocol needs a flag the
//! snapshot does not carry yet, so Shift+Enter is a plain carriage return here.

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
}

/// The bytes for one press, or `None` for a press the terminal does not get (Cmd chords
/// belong to the app).
pub fn encode(p: Press<'_>, app_cursor: bool) -> Option<Vec<u8>> {
    if p.cmd {
        return None;
    }
    let modifier = 1 + u8::from(p.shift) + 2 * u8::from(p.alt) + 4 * u8::from(p.ctrl);
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
        "enter" => alt(b"\r".to_vec()),
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
        let enter = Press {
            alt: true,
            ..key("enter")
        };
        assert_eq!(encode(enter, false), Some(b"\r".to_vec()));
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

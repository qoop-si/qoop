//! Decoding the bytes a terminal emulator sends for a key press.
//!
//! This is rules, not a lookup table, and the corpus is what keeps the rules honest. A
//! table would have to be complete to be correct, and no table is complete: the modifier
//! encodings alone are a product of key, modifier set and mode. The rules are the same
//! ones the encodings were generated from, so the corpus can grow without the decoder
//! changing, which is exactly the property a table does not have.
//!
//! Two byte values carry two meanings each and no terminal can separate them: `0x1b` is
//! Escape and control with `[`, and `0x08` is backspace and control with `H`. The decoder
//! picks the named key for `0x1b` and the control letter for `0x08`, and the corpus
//! carries only the side that is true on the wire. Guessing differently per application is
//! a job for a keymap, not for a decoder.

/// A key, independent of how any particular terminal spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Tab,
    Backspace,
    Enter,
    Escape,
    KeypadEnter,
    F(u8),
}

/// Shift, alt and control. Higher bits exist in the kitty protocol (super, hyper, meta and
/// the lock keys) and are deliberately not modelled: nothing in this milestone acts on
/// them, and a field nobody reads is a field nobody maintains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const CTRL: Self = Self(4);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The xterm modifier parameter: one plus shift 1, alt 2, control 4. Bits above those
    /// belong to the kitty protocol and are dropped rather than rejected, because a
    /// terminal sending super with an arrow is sending a key we still want to see.
    pub fn from_param(param: u32) -> Self {
        Self(param.saturating_sub(1) as u8 & 0b111)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: Modifiers,
    pub kind: KeyKind,
}

impl KeyEvent {
    fn press(code: KeyCode, mods: Modifiers) -> Self {
        Self {
            code,
            mods,
            kind: KeyKind::Press,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// A key, and how many bytes it consumed.
    Key(KeyEvent, usize),
    /// A prefix of something longer. The caller must read more, or call again with
    /// `at_idle` once the read has settled.
    Incomplete,
    /// Not a sequence this decoder understands. The count is what to skip to resynchronise.
    Unknown(usize),
}

/// Decode the next key from the front of `buf`.
///
/// `at_idle` says that no more bytes are coming for now, which is the only thing that can
/// tell a lone `ESC` from the start of a sequence. A terminal cannot answer that question
/// from the bytes alone, and neither can this function, so the caller that owns the read
/// loop answers it.
pub fn decode(buf: &[u8], at_idle: bool) -> Decoded {
    decode_with_depth(buf, at_idle, 0)
}

/// The meta prefix stacks, and a stack whose depth the sender chooses is a stack overflow.
///
/// Measured: 20 000 leading `ESC` bytes, which is one paste, aborted the whole process on a
/// 2 MiB stack, taking every terminal in it. Two meta modifiers is already more than any
/// real terminal sends, so anything past this limit is not a key.
pub const MAX_META_DEPTH: usize = 4;

fn decode_with_depth(buf: &[u8], at_idle: bool, depth: usize) -> Decoded {
    let Some(&first) = buf.first() else {
        return Decoded::Incomplete;
    };

    if first == 0x1b {
        if depth >= MAX_META_DEPTH {
            // Resynchronise on the whole run rather than one byte at a time, so a caller
            // skipping `Unknown(n)` does not walk a megabyte of escapes in single steps.
            let run = buf.iter().take_while(|b| **b == 0x1b).count();
            return Decoded::Unknown(run);
        }
        return decode_escape(buf, at_idle, depth);
    }
    if first == 0x7f {
        return Decoded::Key(KeyEvent::press(KeyCode::Backspace, Modifiers::NONE), 1);
    }
    if first == b'\t' {
        return Decoded::Key(KeyEvent::press(KeyCode::Tab, Modifiers::NONE), 1);
    }
    if first == b'\r' {
        return Decoded::Key(KeyEvent::press(KeyCode::Enter, Modifiers::NONE), 1);
    }
    if first < 0x20 {
        // ANSI X3.4: a control code is the letter with bits 6 and 7 cleared. 0x00 is
        // control with at-sign, which every terminal produces for control with space.
        let ch = match first {
            0x00 => ' ',
            0x1c => '\\',
            0x1d => ']',
            0x1e => '^',
            0x1f => '_',
            other => (other + 0x60) as char,
        };
        return Decoded::Key(KeyEvent::press(KeyCode::Char(ch), Modifiers::CTRL), 1);
    }
    decode_utf8(buf)
}

fn decode_utf8(buf: &[u8]) -> Decoded {
    let len = utf8_len(buf[0]);
    if len == 0 {
        return Decoded::Unknown(1);
    }
    if buf.len() < len {
        return Decoded::Incomplete;
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => {
            let ch = s.chars().next().expect("a decoded scalar value");
            Decoded::Key(KeyEvent::press(KeyCode::Char(ch), Modifiers::NONE), len)
        }
        Err(_) => Decoded::Unknown(1),
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 0,
    }
}

fn decode_escape(buf: &[u8], at_idle: bool, depth: usize) -> Decoded {
    if buf.len() == 1 {
        return if at_idle {
            Decoded::Key(KeyEvent::press(KeyCode::Escape, Modifiers::NONE), 1)
        } else {
            Decoded::Incomplete
        };
    }
    match buf[1] {
        b'[' => decode_csi(buf),
        b'O' => decode_ss3(buf),
        _ => {
            // ESC as a meta prefix. Decoding the remainder rather than assuming a plain
            // character is what makes alt with control with A come out as both modifiers.
            match decode_with_depth(&buf[1..], at_idle, depth + 1) {
                Decoded::Key(event, used) => Decoded::Key(
                    KeyEvent {
                        mods: event.mods.union(Modifiers::ALT),
                        ..event
                    },
                    used + 1,
                ),
                Decoded::Incomplete => Decoded::Incomplete,
                Decoded::Unknown(n) => Decoded::Unknown(n + 1),
            }
        }
    }
}

fn decode_ss3(buf: &[u8]) -> Decoded {
    if buf.len() < 3 {
        return Decoded::Incomplete;
    }
    let code = match buf[2] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'M' => KeyCode::KeypadEnter,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Decoded::Unknown(3),
    };
    Decoded::Key(KeyEvent::press(code, Modifiers::NONE), 3)
}

/// Parameters of a CSI sequence, as `;` separated groups of `:` separated sub-parameters.
struct Params {
    groups: Vec<Vec<u32>>,
}

impl Params {
    fn group(&self, index: usize) -> Option<&[u32]> {
        self.groups.get(index).map(Vec::as_slice)
    }

    fn value(&self, group: usize, sub: usize) -> Option<u32> {
        self.group(group).and_then(|g| g.get(sub)).copied()
    }
}

fn decode_csi(buf: &[u8]) -> Decoded {
    let mut i = 2;
    // A private marker (`<`, `=`, `>`, `?`) means this is not a key report.
    let private = matches!(buf.get(2), Some(b'<'..=b'?'));
    if private {
        i = 3;
    }
    let param_start = i;
    while i < buf.len() && matches!(buf[i], b'0'..=b'9' | b';' | b':') {
        i += 1;
    }
    // Intermediates, which a key report never has, but skipping them keeps
    // resynchronisation on the sequence boundary rather than inside it.
    while i < buf.len() && matches!(buf[i], 0x20..=0x2f) {
        i += 1;
    }
    let Some(&final_byte) = buf.get(i) else {
        return Decoded::Incomplete;
    };
    if !(0x40..=0x7e).contains(&final_byte) {
        return Decoded::Unknown(i + 1);
    }
    let used = i + 1;
    if private {
        return Decoded::Unknown(used);
    }
    let Some(params) = parse_params(&buf[param_start..i]) else {
        // Discarded with explicit resynchronisation: the sequence is skipped whole, so the
        // caller resumes at the byte after its final byte rather than inside it.
        return Decoded::Unknown(used);
    };

    let (code, mods_group) = match final_byte {
        b'A' => (KeyCode::Up, 1),
        b'B' => (KeyCode::Down, 1),
        b'C' => (KeyCode::Right, 1),
        b'D' => (KeyCode::Left, 1),
        b'H' => (KeyCode::Home, 1),
        b'F' => (KeyCode::End, 1),
        b'P' => (KeyCode::F(1), 1),
        b'Q' => (KeyCode::F(2), 1),
        b'R' => (KeyCode::F(3), 1),
        b'S' => (KeyCode::F(4), 1),
        b'Z' => {
            return Decoded::Key(KeyEvent::press(KeyCode::Tab, Modifiers::SHIFT), used);
        }
        b'~' => {
            let Some(number) = params.value(0, 0) else {
                return Decoded::Unknown(used);
            };
            let Some(code) = tilde_key(number) else {
                return Decoded::Unknown(used);
            };
            (code, 1)
        }
        b'u' => {
            // The kitty protocol: the first parameter is the character itself.
            let Some(point) = params.value(0, 0) else {
                return Decoded::Unknown(used);
            };
            let Some(ch) = char::from_u32(point) else {
                return Decoded::Unknown(used);
            };
            (KeyCode::Char(ch), 1)
        }
        _ => return Decoded::Unknown(used),
    };

    let mods = params
        .value(mods_group, 0)
        .map(Modifiers::from_param)
        .unwrap_or(Modifiers::NONE);
    // The kitty protocol carries the event type as a sub-parameter of the modifier group,
    // which is the only way a release can be told from a press. Without reading it, a
    // terminal in that mode delivers every key twice, which is exactly the duplicate the
    // corpus gate exists to catch.
    let kind = match params.value(mods_group, 1) {
        Some(2) => KeyKind::Repeat,
        Some(3) => KeyKind::Release,
        _ => KeyKind::Press,
    };
    Decoded::Key(KeyEvent { code, mods, kind }, used)
}

/// Section 6.7: a parameter list over this many entries is discarded rather than parsed.
///
/// Without it a single `CSI` with a hundred thousand parameters allocates a hundred
/// thousand vectors and still returns a key, which is neither a crash nor the bound the
/// plan says is there.
pub const MAX_PARAMS: usize = 32;

fn parse_params(bytes: &[u8]) -> Option<Params> {
    let mut groups = Vec::new();
    if bytes.is_empty() {
        return Some(Params { groups });
    }
    if bytes.iter().filter(|b| **b == b';').count() >= MAX_PARAMS {
        return None;
    }
    for group in bytes.split(|&b| b == b';') {
        let subs = group
            .split(|&b| b == b':')
            .map(|s| {
                std::str::from_utf8(s)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0)
            })
            .collect();
        groups.push(subs);
    }
    Some(Params { groups })
}

/// The `CSI number ~` table, from xterm's ctlseqs.
fn tilde_key(number: u32) -> Option<KeyCode> {
    Some(match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F((number - 10) as u8),
        17..=21 => KeyCode::F((number - 11) as u8),
        23..=26 => KeyCode::F((number - 12) as u8),
        28 | 29 => KeyCode::F((number - 13) as u8),
        31..=34 => KeyCode::F((number - 14) as u8),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_escape_is_only_a_key_once_the_read_has_settled() {
        assert_eq!(decode(b"\x1b", false), Decoded::Incomplete);
        assert_eq!(
            decode(b"\x1b", true),
            Decoded::Key(KeyEvent::press(KeyCode::Escape, Modifiers::NONE), 1)
        );
    }

    #[test]
    fn a_sequence_arriving_in_pieces_is_not_decoded_early() {
        assert_eq!(decode(b"\x1b[1;5", false), Decoded::Incomplete);
        assert_eq!(
            decode(b"\x1b[1;5A", false),
            Decoded::Key(KeyEvent::press(KeyCode::Up, Modifiers::CTRL), 6)
        );
    }

    #[test]
    fn meta_composes_with_control() {
        let Decoded::Key(event, used) = decode(b"\x1b\x01", true) else {
            panic!("expected a key");
        };
        assert_eq!(used, 2);
        assert_eq!(event.code, KeyCode::Char('a'));
        assert!(event.mods.contains(Modifiers::CTRL));
        assert!(event.mods.contains(Modifiers::ALT));
    }

    #[test]
    fn a_kitty_release_is_not_a_second_press() {
        let Decoded::Key(event, _) = decode(b"\x1b[97;1:3u", true) else {
            panic!("expected a key");
        };
        assert_eq!(event.code, KeyCode::Char('a'));
        assert_eq!(event.kind, KeyKind::Release);
    }

    #[test]
    fn a_mouse_report_is_skipped_whole_rather_than_read_as_keys() {
        assert_eq!(decode(b"\x1b[<0;10;10M", true), Decoded::Unknown(11));
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    /// 20 000 leading escapes used to abort the process by overflowing the stack.
    #[test]
    fn a_stack_of_meta_prefixes_is_bounded() {
        let buf = vec![0x1bu8; 64 * 1024];
        match decode(&buf, true) {
            Decoded::Unknown(n) => assert!(n > 1, "resynchronisation must skip the run"),
            other => panic!("expected a skip, got {other:?}"),
        }
    }

    /// Section 6.7's other bound: a parameter list past the limit is discarded whole.
    #[test]
    fn an_overlong_parameter_list_is_discarded_not_parsed() {
        let mut buf = b"\x1b[".to_vec();
        for _ in 0..MAX_PARAMS + 10 {
            buf.extend_from_slice(b"1;");
        }
        buf.push(b'A');
        match decode(&buf, true) {
            Decoded::Unknown(n) => assert_eq!(n, buf.len(), "the whole sequence is skipped"),
            other => panic!("expected a discard, got {other:?}"),
        }
    }

    /// And a list inside the limit still decodes, so the bound is a bound and not a ban.
    #[test]
    fn an_ordinary_parameter_list_still_decodes() {
        assert_eq!(
            decode(b"\x1b[1;5A", true),
            Decoded::Key(
                KeyEvent {
                    code: KeyCode::Up,
                    mods: Modifiers::CTRL,
                    kind: KeyKind::Press
                },
                6
            )
        );
    }
}

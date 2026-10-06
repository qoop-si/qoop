//! What the terminal says about itself, taken out of what the person typed.
//!
//! A terminal that has been asked to report focus, its colours or its light and dark
//! scheme answers on the same stream the keyboard arrives on. Those answers are not
//! keystrokes, and every place that reads keystrokes treats an escape as a cancel: the
//! naming box, the menu, the focus list and the palette all read `ESC` as "close", and the
//! read only view closes on any byte at all. So a window switch would close whatever the
//! person had open and type the rest of the report into the program in the pane.
//!
//! This is the one place that sees every read first. It takes the reports out, hands them
//! over as what they are, and leaves everything else exactly as it arrived.
//!
//! A read can end inside a report. The start of one is held for the next read the way a
//! mouse report is, but only once it is unambiguous: a lone `ESC` at the end of a read is
//! the Esc key, which has to close an overlay at once, so it is never held. `ESC [` and
//! `ESC ]` are held, and given back as typed input if nothing follows within
//! [`HOLD`], because Alt with `[` or `]` sends exactly those two bytes and the person is
//! still owed their key. `ESC P`, which starts the terminal's name, is held the same way
//! for Alt with `P`, and anything but `ESC P > |` after it is typed as it came. What remains is a report split right after its escape byte, which
//! terminals do not do in practice: they write a report in one go.

use std::time::{Duration, Instant};

/// How long a held `ESC [` or `ESC ]` waits for the rest of a report before it is typed.
///
/// The same idea as tmux's `escape-time`, at the small end of what that is set to: long
/// enough for the rest of a report written in one go and split by the read, short enough
/// that an Alt key is not felt to lag.
pub const HOLD: Duration = Duration::from_millis(10);

/// How long half a drag report waits for the rest. A chunk of dropped data is up to 4 KB
/// and a read is no bigger, so a long one always arrives in two, and the second is not
/// owed to the Alt key the way the start of `ESC ]` is: once `ESC ] 72 ;` has been read it
/// is a report, and typing it into the pane would be the one wrong answer.
const DRAG_HOLD: Duration = Duration::from_secs(1);

/// The longest report this reads. A colour answer is about thirty bytes; anything much
/// longer that has not ended is not one of these and is handed on as typed.
const MAX_REPORT: usize = 96;

/// The longest drag report: a chunk of base64 is at most 4096 bytes, and its metadata is short.
const MAX_DRAG: usize = 4096 + 128;

/// The most of a terminal's name that is read: `WezTerm 20240203-110809-5046fc22` is the
/// longest seen. The answer is still read to its end, up to [`MAX_REPORT`].
const MAX_NAME: usize = 64;

/// Something the terminal said about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// The window the interface runs in gained (`true`) or lost the focus (`CSI I`, `CSI O`).
    Focus(bool),
    /// An answer to `OSC 10 ?` (foreground) or `OSC 11 ?` (background), as 8 bit channels.
    Color { background: bool, rgb: [u8; 3] },
    /// The terminal's scheme changed or was asked for (`CSI ? 997 ; 1 n` dark, `; 2 n` light).
    Scheme { light: bool },
    /// Its name, the answer to XTVERSION (`CSI > 0 q`): `DCS > | name ST`.
    Terminal(crate::icons::Answer),
    /// Its primary device attributes (`CSI ? ... c`), the answer to `CSI c`. Every terminal
    /// gives one, so one arriving before a name means the terminal has no name to give.
    Attributes,
    /// Something dragged over the window, from the terminal's drag and drop protocol
    /// (`OSC 72`), which a program asks for with `t=a` and which gives a cell to a drag.
    Drag(Drag),
}

/// One thing the terminal said about a drag, as far as this interface asks.
///
/// Cells are zero based, from the top left of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drag {
    /// The pointer moved over the window with something held; `-1, -1` is the drag
    /// leaving it.
    Over {
        col: i32,
        row: i32,
        mimes: Vec<String>,
    },
    /// Released over a cell, with the MIME types on offer in the order they are asked for.
    Dropped {
        col: i32,
        row: i32,
        mimes: Vec<String>,
    },
    /// A chunk of the data asked for, decoded; `end` on the last one.
    Data { bytes: Vec<u8>, end: bool },
    /// The terminal could not give what was asked for.
    Failed,
}

/// The filter. One per client, like the key decoder.
#[derive(Debug, Default)]
pub struct Reports {
    /// The start of a report the last read ended inside.
    held: Vec<u8>,
    /// When the held bytes were held, so they can be given back if nothing follows.
    since: Option<Instant>,
    /// Held bytes whose wait ran out: a key after all, typed on the next strip as they are,
    /// never looked at again (looked at again, `ESC [` would only be held again).
    released: Vec<u8>,
}

/// What one sequence at the front of a buffer is.
enum Found {
    /// A whole report, and how many bytes it took.
    Report(usize, Option<Report>),
    /// Could still become a report: wait for more.
    Partial,
    /// Not a report.
    No,
}

impl Reports {
    /// Take the reports out of one read. The rest comes back in its original order.
    pub fn strip(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<Report>) {
        let mut input = std::mem::take(&mut self.held);
        self.since = None;
        input.extend_from_slice(bytes);

        let mut out = std::mem::take(&mut self.released);
        out.reserve(input.len());
        let mut reports = Vec::new();
        let mut at = 0;
        while at < input.len() {
            if input[at] != 0x1b {
                out.push(input[at]);
                at += 1;
                continue;
            }
            match classify(&input[at..]) {
                Found::Report(used, report) => {
                    reports.extend(report);
                    at += used;
                }
                Found::Partial => {
                    self.held = input[at..].to_vec();
                    self.since = Some(Instant::now());
                    break;
                }
                Found::No => {
                    out.push(input[at]);
                    at += 1;
                }
            }
        }
        (out, reports)
    }

    /// When the held bytes should be given back as typed, if anything is held.
    ///
    /// `None` while nothing is held, so the loop arms no timer at all on a quiet screen.
    pub fn deadline(&self) -> Option<Instant> {
        let wait = if self.held.starts_with(b"\x1b]72;") {
            DRAG_HOLD
        } else {
            HOLD
        };
        self.since.map(|since| since + wait)
    }

    /// The wait ran out: the held bytes were a key after all, and the next [`strip`]
    /// hands them over as typed, ahead of whatever that read brings.
    ///
    /// [`strip`]: Reports::strip
    pub fn expire(&mut self) {
        self.since = None;
        let held = std::mem::take(&mut self.held);
        self.released.extend(held);
    }
}

/// What the escape at the front of `bytes` starts.
fn classify(bytes: &[u8]) -> Found {
    debug_assert_eq!(bytes.first(), Some(&0x1b));
    // A lone escape is the Esc key. Never held: an overlay has to close on it now.
    let Some(&kind) = bytes.get(1) else {
        return Found::No;
    };
    match kind {
        b'[' => csi(bytes),
        b']' => osc(bytes),
        b'P' => dcs(bytes),
        _ => Found::No,
    }
}

/// `CSI I`, `CSI O`, `CSI ? 997 ; n n` and the device attributes `CSI ? ... c`.
fn csi(bytes: &[u8]) -> Found {
    match bytes.get(2) {
        None => return Found::Partial,
        Some(b'I') => return Found::Report(3, Some(Report::Focus(true))),
        Some(b'O') => return Found::Report(3, Some(Report::Focus(false))),
        Some(b'?') => {}
        Some(_) => return Found::No,
    }
    // Digits and semicolons, then the letter that says what it was.
    let Some(len) = bytes[3..]
        .iter()
        .position(|byte| !(byte.is_ascii_digit() || *byte == b';'))
    else {
        return if bytes.len() < MAX_REPORT {
            Found::Partial
        } else {
            Found::No
        };
    };
    let end = 3 + len;
    let report = match (bytes[end], &bytes[3..end]) {
        (b'c', params) if !params.is_empty() => Report::Attributes,
        (b'n', b"997;1") => Report::Scheme { light: false },
        (b'n', b"997;2") => Report::Scheme { light: true },
        _ => return Found::No,
    };
    Found::Report(end + 1, Some(report))
}

/// `DCS > | name ST`, the terminal's name, ended by `ESC \`.
fn dcs(bytes: &[u8]) -> Found {
    const START: &[u8] = b"\x1bP>|";
    let shared = bytes.len().min(START.len());
    if bytes[..shared] != START[..shared] {
        return Found::No;
    }
    if bytes.len() <= START.len() {
        return Found::Partial;
    }
    let body = &bytes[START.len()..];
    let Some(at) = body.windows(2).position(|pair| pair == b"\x1b\\") else {
        return if bytes.len() < MAX_REPORT {
            Found::Partial
        } else {
            Found::No
        };
    };
    let name = String::from_utf8_lossy(&body[..at.min(MAX_NAME)]);
    Found::Report(
        START.len() + at + 2,
        Some(Report::Terminal(crate::icons::answer_of(&name))),
    )
}

/// `OSC 10 ; rgb:... ST` and `OSC 11 ; rgb:... ST`, ended by `BEL` or `ESC \`, and the
/// drag reports `OSC 72 ; metadata ; payload ST`.
fn osc(bytes: &[u8]) -> Found {
    let body = &bytes[2..];
    let background = if body.starts_with(b"72;") {
        return drag(bytes);
    } else if body.starts_with(b"11;") {
        true
    } else if body.starts_with(b"10;") {
        false
    } else if b"10;".starts_with(body) || b"11;".starts_with(body) || b"72;".starts_with(body) {
        return Found::Partial;
    } else {
        return Found::No;
    };
    let payload = &body[3..];
    let Some((at, terminator)) = terminator(payload) else {
        return if bytes.len() < MAX_REPORT {
            Found::Partial
        } else {
            Found::No
        };
    };
    let used = 2 + 3 + at + terminator;
    // An answer this cannot read is still an answer and never a keystroke: it is taken out
    // and dropped rather than typed into the pane.
    let report = parse_rgb(&payload[..at]).map(|rgb| Report::Color { background, rgb });
    Found::Report(used, report)
}

/// Where an OSC ends, and how many bytes its terminator takes: whichever of `BEL` and
/// `ESC \` comes first.
fn terminator(payload: &[u8]) -> Option<(usize, usize)> {
    payload
        .iter()
        .enumerate()
        .find_map(|(at, byte)| match byte {
            0x07 => Some((at, 1)),
            0x1b if payload.get(at + 1) == Some(&b'\\') => Some((at, 2)),
            _ => None,
        })
}

/// `OSC 72 ; t=m:x=3:y=4 ; mimes ST` and the rest of the drag and drop protocol that this
/// interface speaks as the receiving end. Every one is taken out, read or not.
fn drag(bytes: &[u8]) -> Found {
    let payload = &bytes[5..];
    let Some((at, terminator)) = terminator(payload) else {
        return if bytes.len() < MAX_DRAG {
            Found::Partial
        } else {
            Found::No
        };
    };
    let used = 5 + at + terminator;
    let text = String::from_utf8_lossy(&payload[..at]);
    let (meta, data) = text.split_once(';').unwrap_or((&text, ""));
    Found::Report(used, read_drag(meta, data).map(Report::Drag))
}

/// What one `OSC 72` says, from its `key=value:` metadata and its payload.
fn read_drag(meta: &str, data: &str) -> Option<Drag> {
    let mut kind = None;
    let (mut col, mut row, mut more) = (0, 0, None);
    for pair in meta.split(':') {
        match pair.split_once('=')? {
            ("t", value) => kind = value.chars().next(),
            ("x", value) => col = value.parse().ok()?,
            ("y", value) => row = value.parse().ok()?,
            ("m", value) => more = Some(value == "1"),
            _ => {}
        }
    }
    // The chunks after the first may carry nothing but `m`, and are still data.
    match kind.or(more.map(|_| 'r'))? {
        'm' => Some(Drag::Over {
            col,
            row,
            mimes: data.split_whitespace().map(str::to_string).collect(),
        }),
        'M' => Some(Drag::Dropped {
            col,
            row,
            mimes: data.split_whitespace().map(str::to_string).collect(),
        }),
        'r' => Some(Drag::Data {
            bytes: base64(data)?,
            end: more != Some(true),
        }),
        'R' | 'E' => Some(Drag::Failed),
        _ => None,
    }
}

/// Standard base64, padded or not. `None` on anything else.
fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut have) = (0u32, 0);
    for byte in text.trim_end_matches('=').bytes() {
        let six = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(six);
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    Some(out)
}

/// `rgb:R/G/B` with one to four hex digits a channel, scaled to eight bits.
fn parse_rgb(text: &[u8]) -> Option<[u8; 3]> {
    let text = std::str::from_utf8(text).ok()?;
    let rest = text.strip_prefix("rgb:")?;
    let mut channels = rest.split('/');
    let mut rgb = [0u8; 3];
    for slot in &mut rgb {
        let digits = channels.next()?;
        if digits.is_empty() || digits.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1u32 << (4 * digits.len() as u32)) - 1;
        *slot = ((value * 255 + max / 2) / max) as u8;
    }
    channels.next().is_none().then_some(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(bytes: &[u8]) -> (Vec<u8>, Vec<Report>) {
        Reports::default().strip(bytes)
    }

    #[test]
    fn ordinary_typing_passes_through_untouched() {
        let (out, reports) = strip(b"hello \x1b[A\x1b[<0;3;4M\x01d");
        assert_eq!(out, b"hello \x1b[A\x1b[<0;3;4M\x01d");
        assert!(reports.is_empty());
    }

    #[test]
    fn focus_reports_are_taken_out_wherever_they_land() {
        let (out, reports) = strip(b"ab\x1b[Ocd\x1b[I");
        assert_eq!(out, b"abcd");
        assert_eq!(reports, vec![Report::Focus(false), Report::Focus(true)]);
    }

    #[test]
    fn a_lone_escape_is_the_esc_key_and_is_never_held() {
        let mut filter = Reports::default();
        let (out, reports) = filter.strip(b"\x1b");
        assert_eq!(out, b"\x1b", "the Esc key has to close an overlay now");
        assert!(reports.is_empty());
        assert_eq!(filter.deadline(), None);
    }

    #[test]
    fn a_background_answer_in_either_terminator_is_read_as_a_colour() {
        let (out, reports) = strip(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
        assert!(out.is_empty());
        assert_eq!(
            reports,
            vec![Report::Color {
                background: true,
                rgb: [255, 255, 255]
            }]
        );
        let (out, reports) = strip(b"x\x1b]10;rgb:0/8/f\x07y");
        assert_eq!(out, b"xy");
        assert_eq!(
            reports,
            vec![Report::Color {
                background: false,
                rgb: [0, 136, 255]
            }]
        );
    }

    #[test]
    fn channels_of_one_to_four_digits_scale_to_eight_bits() {
        assert_eq!(parse_rgb(b"rgb:f/f/f"), Some([255, 255, 255]));
        assert_eq!(parse_rgb(b"rgb:80/80/80"), Some([128, 128, 128]));
        assert_eq!(parse_rgb(b"rgb:c0c0/c0c0/c0c0"), Some([192, 192, 192]));
        assert_eq!(parse_rgb(b"rgb:123/abc/000"), Some([18, 171, 0]));
        assert_eq!(parse_rgb(b"rgb:12345/0/0"), None);
        assert_eq!(parse_rgb(b"rgb:1/2"), None);
        assert_eq!(parse_rgb(b"rgb:1/2/3/4"), None);
        assert_eq!(parse_rgb(b"cmyk:1/2/3"), None);
    }

    #[test]
    fn an_unreadable_colour_answer_is_dropped_rather_than_typed() {
        let (out, reports) = strip(b"\x1b]11;rgba:1/2/3/4\x07");
        assert!(out.is_empty(), "{out:?}");
        assert!(reports.is_empty());
    }

    #[test]
    fn a_scheme_report_says_dark_or_light() {
        let (out, reports) = strip(b"\x1b[?997;2n\x1b[?997;1n");
        assert!(out.is_empty());
        assert_eq!(
            reports,
            vec![
                Report::Scheme { light: true },
                Report::Scheme { light: false }
            ]
        );
    }

    #[test]
    fn other_private_sequences_are_not_mistaken_for_a_scheme() {
        let (out, reports) = strip(b"\x1b[?1049h\x1b[?997;3n");
        assert_eq!(out, b"\x1b[?1049h\x1b[?997;3n");
        assert!(reports.is_empty());
    }

    /// Every report, cut at every byte after its escape, arrives whole and leaves nothing
    /// typed. The cut right after the escape byte is the one documented exception.
    #[test]
    fn a_report_split_anywhere_after_its_escape_still_arrives_whole() {
        let cases: [(&[u8], Report); 5] = [
            (b"\x1b[I", Report::Focus(true)),
            (b"\x1b[O", Report::Focus(false)),
            (b"\x1b[?997;2n", Report::Scheme { light: true }),
            (
                b"\x1b]11;rgb:0000/0000/0000\x1b\\",
                Report::Color {
                    background: true,
                    rgb: [0, 0, 0],
                },
            ),
            (
                b"\x1b]10;rgb:ff/ff/ff\x07",
                Report::Color {
                    background: false,
                    rgb: [255, 255, 255],
                },
            ),
        ];
        for (whole, expected) in cases {
            for cut in 2..whole.len() {
                let mut filter = Reports::default();
                let (first, early) = filter.strip(&whole[..cut]);
                assert!(first.is_empty(), "cut {cut} of {whole:?} typed {first:?}");
                assert!(early.is_empty());
                assert!(filter.deadline().is_some(), "cut {cut} held nothing");
                let (second, reports) = filter.strip(&whole[cut..]);
                assert!(second.is_empty(), "cut {cut} of {whole:?} typed {second:?}");
                assert_eq!(reports, vec![expected.clone()], "cut {cut} of {whole:?}");
                assert_eq!(filter.deadline(), None);
            }
        }
    }

    #[test]
    fn a_held_alt_bracket_is_given_back_as_typed_when_nothing_follows() {
        let mut filter = Reports::default();
        let (out, _) = filter.strip(b"x\x1b[");
        assert_eq!(out, b"x");
        assert!(filter.deadline().is_some());
        filter.expire();
        assert_eq!(filter.deadline(), None);
        // Typed as they are, even though they still look like the start of a report.
        let (out, _) = filter.strip(b"");
        assert_eq!(out, b"\x1b[");
        let (out, _) = filter.strip(b"\x1b]");
        assert!(out.is_empty());
        filter.expire();
        let (out, _) = filter.strip(b"k");
        assert_eq!(out, b"\x1b]k", "released first, then what the read brought");
    }

    /// The answers to XTVERSION and DA1, as the measured terminals sent them, are taken out;
    /// what they say is the terminal's kind, nothing of them is typed.
    #[test]
    fn a_terminals_name_and_attributes_are_reports_not_keys() {
        use crate::icons::Answer;
        let (out, reports) = strip(b"a\x1bP>|ghostty 1.3.1\x1b\\b");
        assert_eq!(out, b"ab");
        assert_eq!(reports, [Report::Terminal(Answer::FromFont)]);
        let (_, reports) = strip(b"\x1bP>|iTerm2 3.7.3\x1b\\\x1b[?62;4c");
        assert_eq!(
            reports,
            [Report::Terminal(Answer::FromFont), Report::Attributes]
        );
        let (_, reports) = strip(b"\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\");
        assert_eq!(reports, [Report::Terminal(Answer::Other)]);
        let (out, reports) = strip(b"\x1bP>|tmux 3.5a\x1b\\");
        assert!(out.is_empty());
        assert_eq!(reports, [Report::Terminal(Answer::Other)]);
        // Terminal answers DA1 alone.
        let (out, reports) = strip(b"\x1b[?1;2c");
        assert!(out.is_empty());
        assert_eq!(reports, [Report::Attributes]);
        // A name split by the read is put back together.
        let mut filter = Reports::default();
        let (out, reports) = filter.strip(b"\x1bP>|kitty(0.4");
        assert!(out.is_empty() && reports.is_empty());
        let (out, reports) = filter.strip(b"9.2)\x1b\\");
        assert!(out.is_empty());
        assert_eq!(reports, [Report::Terminal(Answer::FromFont)]);
    }

    /// Alt with `P` sends `ESC P`: held for the length of a report, then typed, and any
    /// other byte after it is typed at once.
    #[test]
    fn alt_p_is_still_a_key() {
        let mut filter = Reports::default();
        let (out, _) = filter.strip(b"\x1bP");
        assert!(out.is_empty());
        filter.expire();
        let (out, _) = filter.strip(b"");
        assert_eq!(out, b"\x1bP");
        let (out, reports) = strip(b"\x1bPx");
        assert_eq!(out, b"\x1bPx");
        assert!(reports.is_empty());
        // A name that never ends is not one, and is given back rather than eaten.
        let mut long = b"\x1bP>|".to_vec();
        long.extend(std::iter::repeat_n(b'a', 200));
        let (out, reports) = strip(&long);
        assert_eq!(out, long);
        assert!(reports.is_empty());
    }

    #[test]
    fn a_held_prefix_that_turns_out_to_be_a_key_is_typed_in_order() {
        let mut filter = Reports::default();
        let (out, _) = filter.strip(b"\x1b[");
        assert!(out.is_empty());
        // The arrow key that was split: typed whole, nothing lost, nothing reordered.
        let (out, reports) = filter.strip(b"Ab");
        assert_eq!(out, b"\x1b[Ab");
        assert!(reports.is_empty());
    }

    #[test]
    fn an_endless_osc_is_not_held_forever() {
        let mut long = b"\x1b]11;rgb:".to_vec();
        long.extend(std::iter::repeat_n(b'0', MAX_REPORT));
        let (out, reports) = strip(&long);
        assert_eq!(out, long, "a runaway sequence is typed, not swallowed");
        assert!(reports.is_empty());
    }

    #[test]
    fn a_drag_over_a_cell_and_leaving_are_read() {
        let (out, reports) = strip(b"a\x1b]72;t=m:x=12:y=5:X=99:Y=80:o=3;text/uri-list\x1b\\b");
        assert_eq!(out, b"ab");
        assert_eq!(
            reports,
            vec![Report::Drag(Drag::Over {
                col: 12,
                row: 5,
                mimes: vec!["text/uri-list".into()]
            })]
        );
        let (_, reports) = strip(b"\x1b]72;t=m:x=-1:y=-1\x07");
        assert_eq!(
            reports,
            vec![Report::Drag(Drag::Over {
                col: -1,
                row: -1,
                mimes: vec![]
            })]
        );
    }

    #[test]
    fn a_drop_carries_the_cell_and_the_offered_types_in_order() {
        let (_, reports) = strip(b"\x1b]72;t=M:x=70:y=9:o=1;text/plain text/uri-list\x1b\\");
        assert_eq!(
            reports,
            vec![Report::Drag(Drag::Dropped {
                col: 70,
                row: 9,
                mimes: vec!["text/plain".into(), "text/uri-list".into()]
            })]
        );
    }

    #[test]
    fn dropped_data_is_decoded_chunk_by_chunk() {
        let (_, reports) = strip(b"\x1b]72;t=r:x=1:m=1;ZmlsZTov\x1b\\\x1b]72;t=r:x=1:m=0;L2EgYg==\x1b\\\x1b]72;t=r:x=1:m=0\x1b\\");
        assert_eq!(
            reports,
            vec![
                Report::Drag(Drag::Data {
                    bytes: b"file:/".to_vec(),
                    end: false
                }),
                Report::Drag(Drag::Data {
                    bytes: b"/a b".to_vec(),
                    end: true
                }),
                Report::Drag(Drag::Data {
                    bytes: vec![],
                    end: true
                }),
            ]
        );
        // A later chunk with nothing but its `m` is data too.
        let (_, reports) = strip(b"\x1b]72;m=0\x1b\\");
        assert_eq!(
            reports,
            vec![Report::Drag(Drag::Data {
                bytes: vec![],
                end: true
            })]
        );
        let (_, reports) = strip(b"\x1b]72;t=R:x=1;EPERM\x1b\\");
        assert_eq!(reports, vec![Report::Drag(Drag::Failed)]);
    }

    #[test]
    fn a_drag_report_split_between_reads_is_held_and_never_typed() {
        let mut filter = Reports::default();
        let (out, reports) = filter.strip(b"\x1b]72;t=m:x=3");
        assert!(out.is_empty() && reports.is_empty());
        let (out, reports) = filter.strip(b":y=4\x1b\\");
        assert!(out.is_empty());
        assert_eq!(
            reports,
            vec![Report::Drag(Drag::Over {
                col: 3,
                row: 4,
                mimes: vec![]
            })]
        );
    }

    #[test]
    fn half_a_drag_report_waits_long_and_half_an_escape_waits_a_moment() {
        let mut filter = Reports::default();
        filter.strip(b"\x1b]72;t=r:x=1;QUJD");
        let wait = filter
            .deadline()
            .unwrap()
            .duration_since(filter.since.unwrap());
        assert_eq!(wait, DRAG_HOLD);
        let mut filter = Reports::default();
        filter.strip(b"\x1b]");
        let wait = filter
            .deadline()
            .unwrap()
            .duration_since(filter.since.unwrap());
        assert_eq!(wait, HOLD);
    }

    #[test]
    fn base64_reads_padded_and_unpadded_text_and_refuses_the_rest() {
        assert_eq!(base64("aGk="), Some(b"hi".to_vec()));
        assert_eq!(base64("aGk"), Some(b"hi".to_vec()));
        assert_eq!(base64(""), Some(vec![]));
        assert_eq!(base64("a*k="), None);
    }
}

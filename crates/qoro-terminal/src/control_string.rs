//! A bound on control strings, applied before the parser sees them.
//!
//! `vte`, built with `std`, accumulates an OSC payload into an unbounded `Vec`. A program
//! that opens `ESC ]` and then writes without ever terminating it therefore grows the
//! daemon by exactly as much as it writes: measured at four megabytes fed and four
//! megabytes retained. That is a denial of service one `printf` long, and it is per
//! terminal, so a workspace multiplies it.
//!
//! The master plan's rule (section 6.7) is discard, not truncation: a control string over
//! 4 KiB is dropped whole, with explicit resynchronisation and a counter. Truncating would
//! hand the parser a shortened string that still looks well formed, which is how a title
//! becomes a corrupted title instead of an absent one.
//!
//! Anything that is not a control string passes through: a sequence is staged only from
//! `ESC` until the parser returns to ground, and in the ordinary case that is a handful of
//! bytes released on the same call.

use qoro_domain::EscapeState;

/// The plan's figure. Larger than any legitimate title, hyperlink or status sequence, and
/// small enough that a per-terminal reservation of it is not worth counting.
pub const MAX_CONTROL_STRING: usize = 4 * 1024;

#[derive(Default)]
pub struct ControlStringFilter {
    state: EscapeState,
    /// A sequence in progress. Never larger than `MAX_CONTROL_STRING`, because that is the
    /// point at which it is discarded.
    staged: Vec<u8>,
    /// Dropping bytes until the scanner reaches ground again. The terminator goes too: the
    /// parser never saw the opening byte, so there is nothing for it to terminate.
    discarding: bool,
    discarded_sequences: u64,
    discarded_bytes: u64,
}

impl ControlStringFilter {
    /// Append the bytes the parser should see. Bytes belonging to an incomplete sequence
    /// are held until it completes, so the parser is never handed half of one.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            let next = self.state.step(b);
            self.state = next;

            if self.discarding {
                self.discarded_bytes += 1;
                if next.is_ground() {
                    self.discarding = false;
                }
                continue;
            }

            if self.staged.is_empty() && next.is_ground() {
                out.push(b);
                continue;
            }

            self.staged.push(b);
            if next.is_ground() {
                out.extend_from_slice(&self.staged);
                self.staged.clear();
            } else if self.staged.len() >= MAX_CONTROL_STRING {
                self.discarded_sequences += 1;
                self.discarded_bytes += self.staged.len() as u64;
                self.staged.clear();
                self.discarding = true;
            }
        }
    }

    /// How many sequences were dropped for exceeding the bound, and how many bytes went
    /// with them. Exposed because the plan bans a silent discard as firmly as a silent
    /// truncation: something has to be able to say the screen is missing a sequence.
    pub fn discarded(&self) -> (u64, u64) {
        (self.discarded_sequences, self.discarded_bytes)
    }

    /// Bytes currently held for an incomplete sequence.
    pub fn staged_len(&self) -> usize {
        self.staged.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(filter: &mut ControlStringFilter, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        filter.feed(input, &mut out);
        out
    }

    #[test]
    fn ordinary_text_and_sequences_pass_through_unchanged() {
        let mut f = ControlStringFilter::default();
        let input = b"hi\x1b[1;31mred\x1b]0;title\x07done\x1b(B!";
        assert_eq!(run(&mut f, input), input);
        assert_eq!(f.discarded(), (0, 0));
    }

    #[test]
    fn a_sequence_split_across_reads_is_delivered_whole_and_in_order() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        for part in [&b"a\x1b]0;ti"[..], &b"tle\x07b"[..]] {
            f.feed(part, &mut out);
        }
        assert_eq!(out, b"a\x1b]0;title\x07b");
    }

    #[test]
    fn an_over_long_control_string_is_discarded_whole_not_truncated() {
        let mut f = ControlStringFilter::default();
        let mut input = b"before\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'A', MAX_CONTROL_STRING * 3));
        input.extend_from_slice(b"\x07after");
        let out = run(&mut f, &input);
        assert_eq!(
            out, b"beforeafter",
            "the parser saw part of a discarded string"
        );
        let (sequences, bytes) = f.discarded();
        assert_eq!(sequences, 1);
        assert!(bytes as usize >= MAX_CONTROL_STRING);
    }

    #[test]
    fn an_unterminated_control_string_never_accumulates() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        f.feed(b"\x1b]0;", &mut out);
        for _ in 0..64 {
            f.feed(&vec![b'A'; 64 * 1024], &mut out);
            assert!(
                f.staged_len() <= MAX_CONTROL_STRING,
                "held {} bytes",
                f.staged_len()
            );
        }
        assert!(out.is_empty());
    }

    /// The three byte prefix that used to bypass the bound entirely.
    ///
    /// `ESC [ 1 ESC ]` leaves the parser inside an OSC. A scanner that treated ESC as an
    /// ordinary CSI parameter byte believed it was back at ground on the `]`, passed
    /// everything after it straight through, and reported zero discards while the parser
    /// grew by exactly what the child wrote.
    #[test]
    fn an_escape_inside_a_csi_does_not_open_an_unbounded_hole() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        f.feed(b"\x1b[1\x1b]0;", &mut out);
        for _ in 0..8 {
            f.feed(&vec![b'A'; 64 * 1024], &mut out);
            assert!(
                f.staged_len() <= MAX_CONTROL_STRING,
                "held {} bytes",
                f.staged_len()
            );
        }
        assert!(out.is_empty(), "the parser was handed {} bytes", out.len());
        assert_eq!(f.discarded().0, 1, "the discard was not counted");
    }

    /// A control byte between the ESC and the `]` is ignored by the parser and has to be
    /// ignored here too, or the OSC it opens is never bounded.
    #[test]
    fn a_control_byte_after_escape_does_not_open_an_unbounded_hole() {
        for prefix in [&b"\x1b\x00]0;"[..], b"\x1b\x7f]0;", b"\x1b\x85]0;"] {
            let mut f = ControlStringFilter::default();
            let mut out = Vec::new();
            f.feed(prefix, &mut out);
            for _ in 0..8 {
                f.feed(&vec![b'A'; 64 * 1024], &mut out);
                assert!(
                    f.staged_len() <= MAX_CONTROL_STRING,
                    "{prefix:?} held {} bytes",
                    f.staged_len()
                );
            }
            assert_eq!(
                f.discarded().0,
                1,
                "{prefix:?}: the discard was not counted"
            );
        }
    }

    /// The same missing transition in the other direction: CAN returns the parser to
    /// ground, and a scanner that stayed in `String` would discard ordinary text as if it
    /// were a control string, blanking the terminal from that point on.
    #[test]
    fn a_cancel_inside_a_control_string_resynchronises() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        f.feed(b"\x1b]0;title\x18", &mut out);
        f.feed(b"ordinary text", &mut out);
        // The cancelled sequence is forwarded rather than swallowed: cancelling is the
        // parser's job and it does it correctly. What matters here is that the filter let
        // go of the string state, so the text after it is not eaten as payload.
        assert!(
            String::from_utf8_lossy(&out).ends_with("ordinary text"),
            "text after a cancelled control string did not reach the parser: {:?}",
            String::from_utf8_lossy(&out)
        );
        assert_eq!(f.discarded(), (0, 0));
    }

    #[test]
    fn the_stream_resynchronises_after_a_discard() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        f.feed(b"\x1b]0;", &mut out);
        f.feed(&vec![b'A'; MAX_CONTROL_STRING * 2], &mut out);
        // ST rather than BEL, to prove the terminator is recognised in both forms.
        f.feed(b"\x1b\\next", &mut out);
        assert_eq!(out, b"next");
    }

    /// vte ends a DCS at the 8-bit ST, 0x9C, and goes back to ground. A scanner that stayed
    /// in the string held everything after it, and past 4 KiB dropped a plain text file
    /// printed after the sequence.
    #[test]
    fn a_dcs_ends_at_the_eight_bit_string_terminator() {
        let mut f = ControlStringFilter::default();
        let mut out = Vec::new();
        f.feed(b"\x1bPq#0\x9c", &mut out);
        f.feed(&vec![b'x'; 5000], &mut out);
        assert_eq!(out.iter().filter(|&&b| b == b'x').count(), 5000);
        assert_eq!(f.discarded(), (0, 0));
    }
}

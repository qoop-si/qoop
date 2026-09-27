//! Image envelopes on the output path, caught beside the parser.
//!
//! The grid engine (`alacritty_terminal`) has no graphics events: a Kitty APC or an
//! iTerm2 inline image that reaches it vanishes without a trace, and a real image is
//! always larger than the 4 KiB `ControlStringFilter` bound, so it never even reaches
//! the parser. Either way the user sees nothing where an agent pasted a picture.
//!
//! This sniffer is a tap, not a filter. It watches a copy of the child's bytes for two
//! envelopes and hands completed ones to the caller as `CapturedImage`, while the bytes
//! it consumed never reach the parser at all: an image is a side channel, not grid
//! cells, so the parser must neither draw it nor count it as discarded.
//!
//! The bound this enforces is its own. An unterminated envelope is staged up to
//! `MAX_IMAGE_BYTES` with a deadline, then discarded whole with a counter, which is the
//! same discard-not-truncate rule as `control_string` but with a budget sized for
//! pictures rather than titles. Non-image bytes pass through byte for byte, including
//! every `ESC ]` that is not `1337` and every `ESC _` that is not `G`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How much one image envelope may stage while it is still incomplete.
///
/// Sized for real pictures: a phone screenshot as base64 is a few megabytes, and the
/// envelope travels on as base64 inside JSON, so the budget must leave room for that
/// third on top while staying inside the proto reassembly cap. Sized against denial
/// of service all the same: per terminal, counted on discard, with a deadline below.
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// How long an incomplete envelope may wait for its terminator.
pub const IMAGE_DEADLINE: Duration = Duration::from_secs(10);

/// Which protocol the envelope speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
}

/// One completed image envelope, verbatim, terminator included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedImage {
    /// Monotonic per sniffer, starting at 1. The placement identity the client uses
    /// to delete exactly this image again.
    pub id: u64,
    pub protocol: ImageProtocol,
    /// The whole envelope, opening `ESC` to terminator, byte for byte.
    pub bytes: Vec<u8>,
}

/// A Kitty chunked transmission in progress: the first chunk's control and the
/// payload so far. Emitted whole on `m=0`, never in parts.
struct Assembly {
    control: Vec<u8>,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    SawEsc,
    /// `ESC _` seen, waiting for the `G` that makes it Kitty graphics.
    ApcKind,
    /// Inside a Kitty envelope, after `ESC _ G`.
    ApcBody,
    /// `ESC ]` seen, matching `1337;File=` byte by byte.
    OscKind {
        matched: u8,
    },
    /// Inside an iTerm2 envelope, after `ESC ] 1337;File=`.
    OscBody,
    /// Over budget or past deadline: swallowing until the terminator.
    Discarding {
        kitty: bool,
    },
}

/// Catches Kitty and iTerm2 image envelopes in a child byte stream.
pub struct ImageSniffer {
    state: State,
    /// An envelope in progress, withheld from the clean stream. Empty at ground.
    staged: Vec<u8>,
    /// `ESC` seen as the last body byte: a possible `ST` terminator.
    body_esc: bool,
    staged_since: Option<Instant>,
    deadline: Duration,
    next_id: u64,
    ready: VecDeque<CapturedImage>,
    assembling: Option<Assembly>,
    discarded_envelopes: u64,
    discarded_bytes: u64,
}

impl Default for ImageSniffer {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageSniffer {
    pub fn new() -> Self {
        Self::with_deadline(IMAGE_DEADLINE)
    }

    pub fn with_deadline(deadline: Duration) -> Self {
        Self {
            state: State::Ground,
            staged: Vec::new(),
            body_esc: false,
            staged_since: None,
            deadline,
            next_id: 1,
            ready: VecDeque::new(),
            assembling: None,
            discarded_envelopes: 0,
            discarded_bytes: 0,
        }
    }

    /// Watch `input`, appending the non-image bytes to `clean`.
    ///
    /// Completed images are not in `clean`; drain them with `take_images`. An
    /// incomplete trailing envelope stays staged until it completes, the bound trips,
    /// or the deadline passes, at which point it is replayed to `clean`: it may have
    /// been ordinary text all along, and the parser owns that decision.
    pub fn feed(&mut self, input: &[u8], clean: &mut Vec<u8>) {
        for &b in input {
            self.step(b, clean);
        }
        self.check_deadline(clean);
    }

    /// Completed envelopes since the last call, oldest first.
    pub fn take_images(&mut self) -> Vec<CapturedImage> {
        self.ready.drain(..).collect()
    }

    /// Envelopes dropped for exceeding the bound or the deadline, and their bytes.
    /// Exposed for the same reason every other discard in this codebase is counted:
    /// a missing picture must be sayable.
    pub fn discarded(&self) -> (u64, u64) {
        (self.discarded_envelopes, self.discarded_bytes)
    }

    fn step(&mut self, b: u8, clean: &mut Vec<u8>) {
        // CAN and SUB cancel whatever is in progress, the parser's way. The staged
        // bytes were withheld from the parser, so they go to clean now: cancelling is
        // the parser's job and it does it correctly. A chain in progress dies with
        // the cancel: its opening is gone, so the rest can never assemble.
        if b == 0x18 || b == 0x1a {
            self.abandon_assembly();
            self.flush_to_clean(clean);
            clean.push(b);
            return;
        }
        match self.state {
            State::Ground => {
                if b == 0x1b {
                    self.begin_stage();
                    self.state = State::SawEsc;
                } else {
                    clean.push(b);
                }
            }
            State::SawEsc => {
                self.staged.push(b);
                match b {
                    b'_' => self.state = State::ApcKind,
                    b']' => self.state = State::OscKind { matched: 0 },
                    0x1b => {
                        // The old ESC was ordinary text; the new one may open.
                        self.staged.clear();
                        self.begin_stage();
                    }
                    _ => self.flush_to_clean(clean),
                }
            }
            State::ApcKind => {
                if b == 0x1b {
                    // `ESC _ ESC`: the first envelope never opened. The staged bytes
                    // are ordinary text; the new ESC may open.
                    self.flush_to_clean(clean);
                    self.begin_stage();
                    self.state = State::SawEsc;
                    return;
                }
                self.staged.push(b);
                if b == b'G' {
                    self.state = State::ApcBody;
                    self.body_esc = false;
                } else {
                    // Not Kitty graphics. Ordinary control string, the filter's
                    // business, not ours.
                    self.flush_to_clean(clean);
                }
            }
            State::ApcBody => self.body_step(b, true, clean),
            State::OscKind { matched } => {
                if b == 0x1b {
                    self.flush_to_clean(clean);
                    self.begin_stage();
                    self.state = State::SawEsc;
                    return;
                }
                self.staged.push(b);
                // The whole address, `File=` included. `1337` alone also carries
                // `ClearScrollback`, `StealFocus`, `Copy=` and the rest of iTerm2's
                // commands, and capturing those as pictures forwarded them to the outer
                // terminal untouched, clipboard writes past the policy gate included.
                // The `;` matters too: `ESC ] 13371;...` is some other OSC.
                const MAGIC: &[u8] = b"1337;File=";
                if (matched as usize) < MAGIC.len() && b == MAGIC[matched as usize] {
                    let matched = matched + 1;
                    self.state = if matched as usize == MAGIC.len() {
                        State::OscBody
                    } else {
                        State::OscKind { matched }
                    };
                    self.body_esc = false;
                } else {
                    // Not an iTerm2 image. Ordinary OSC, the filter's business.
                    self.flush_to_clean(clean);
                }
            }
            State::OscBody => self.body_step(b, false, clean),
            State::Discarding { kitty } => {
                self.discarded_bytes += 1;
                if b == 0x07 && !kitty {
                    self.drop_staged();
                } else if b == 0x1b {
                    // Possible ST; the next byte decides. Count it when seen.
                    self.body_esc = true;
                } else if self.body_esc {
                    self.body_esc = false;
                    if b == b'\\' {
                        self.drop_staged();
                    } else {
                        // Not ST: the ESC ended the string for the parser, so it ends
                        // the discard too, and opens whatever it opens.
                        self.drop_staged();
                        self.reopen(b, clean);
                    }
                }
            }
        }
    }

    /// One body byte of a Kitty (`kitty = true`) or iTerm2 envelope.
    fn body_step(&mut self, b: u8, kitty: bool, clean: &mut Vec<u8>) {
        if self.body_esc {
            self.body_esc = false;
            self.staged.push(b);
            if b == b'\\' {
                self.complete(if kitty {
                    ImageProtocol::Kitty
                } else {
                    ImageProtocol::ITerm2
                });
                return;
            }
            // An ESC that does not open ST ends the envelope, as it ends the string for
            // the parser. Staged on, one lone ESC held back every byte after it, up to
            // the bound, and then the filter threw them away. What was staged is an
            // unfinished picture: counted and dropped.
            self.staged.pop();
            self.discarded_envelopes += 1;
            self.discarded_bytes += self.staged.len() as u64;
            self.abandon_assembly();
            self.drop_staged();
            self.reopen(b, clean);
            return;
        }
        if b == 0x1b {
            self.body_esc = true;
            self.staged.push(b);
            return;
        }
        // BEL terminates OSC (iTerm2) but not APC (Kitty), where it would be payload.
        if b == 0x07 && !kitty {
            self.staged.push(b);
            self.complete(ImageProtocol::ITerm2);
            return;
        }
        self.staged.push(b);
        self.check_bound();
        let _ = clean;
    }

    /// An ESC that ended something, and the byte after it: read as a fresh sequence.
    fn reopen(&mut self, b: u8, clean: &mut Vec<u8>) {
        self.begin_stage();
        self.state = State::SawEsc;
        self.step(b, clean);
    }

    fn begin_stage(&mut self) {
        if self.staged.is_empty() {
            self.staged_since = Some(Instant::now());
        }
        self.staged.push(0x1b);
        self.body_esc = false;
    }

    fn flush_to_clean(&mut self, clean: &mut Vec<u8>) {
        clean.extend_from_slice(&self.staged);
        self.staged.clear();
        self.staged_since = None;
        self.state = State::Ground;
        self.body_esc = false;
    }

    fn drop_staged(&mut self) {
        self.staged.clear();
        self.staged_since = None;
        self.state = State::Ground;
        self.body_esc = false;
    }

    fn complete(&mut self, protocol: ImageProtocol) {
        let bytes = std::mem::take(&mut self.staged);
        self.staged_since = None;
        self.state = State::Ground;
        self.body_esc = false;
        if protocol == ImageProtocol::Kitty && self.assemble_chunk(&bytes) {
            // A middle chunk: held for the rest of the chain, not emitted.
            return;
        }
        self.emit(protocol, bytes);
    }

    /// One finished envelope for the client.
    fn emit(&mut self, protocol: ImageProtocol, bytes: Vec<u8>) {
        let id = self.next_id;
        self.next_id += 1;
        self.ready.push_back(CapturedImage {
            id,
            protocol,
            bytes,
        });
    }

    /// Fold a Kitty envelope into a chunked transmission in progress.
    ///
    /// Large pictures travel as `m=1` chunks with the control data only on the
    /// first, closed by `m=0`. Each chunk alone is a broken tail; only the
    /// assembled whole is emitted, with `m` rewritten to `0` so the outer
    /// terminal does not wait for chunks that never come. Returns true when the
    /// envelope was consumed as a middle chunk.
    ///
    /// One chain at a time: a new chain carrying full control abandons the old
    /// one, counted, because interleaved transmissions from one child are a
    /// malfunction rather than a protocol.
    fn assemble_chunk(&mut self, bytes: &[u8]) -> bool {
        let Some(semi) = bytes.iter().position(|&b| b == b';') else {
            return false;
        };
        let control = &bytes[3..semi];
        let payload = &bytes[semi + 1..bytes.len() - 2];
        let value_of = |key: &[u8]| {
            control.split(|&b| b == b',').find_map(|pair| {
                let mut kv = pair.splitn(2, |&b| b == b'=');
                (kv.next() == Some(key)).then(|| kv.next()).flatten()
            })
        };
        match value_of(b"m") {
            Some(b"1") => {
                let fresh = value_of(b"f").is_some()
                    || value_of(b"s").is_some()
                    || value_of(b"v").is_some();
                if fresh {
                    self.abandon_assembly();
                    self.assembling = Some(Assembly {
                        control: control.to_vec(),
                        payload: payload.to_vec(),
                    });
                } else if let Some(chain) = self.assembling.as_mut() {
                    if chain.payload.len() + payload.len() > MAX_IMAGE_BYTES {
                        self.abandon_assembly();
                    } else {
                        chain.payload.extend_from_slice(payload);
                    }
                } else {
                    // A continuation with no opening: the opening was lost to a
                    // bound or a cancel. Counted, not drawn half.
                    self.discarded_envelopes += 1;
                    self.discarded_bytes += bytes.len() as u64;
                }
                true
            }
            _ => {
                if let Some(chain) = self.assembling.take() {
                    if chain.payload.len() + payload.len() > MAX_IMAGE_BYTES {
                        self.discarded_envelopes += 1;
                        self.discarded_bytes +=
                            (chain.control.len() + chain.payload.len() + bytes.len()) as u64;
                        return true;
                    }
                    let mut control = chain.control;
                    // The chain opened with `m=1`; the assembled whole must not
                    // claim more is coming.
                    let mut at = None;
                    let mut start = 0;
                    for pair in control.split(|&b| b == b',') {
                        if pair == b"m=1" {
                            at = Some(start + 2);
                            break;
                        }
                        start += pair.len() + 1;
                    }
                    if let Some(at) = at
                        && at < control.len()
                    {
                        control[at] = b'0';
                    }
                    let mut whole = Vec::with_capacity(
                        3 + control.len() + 1 + chain.payload.len() + payload.len() + 2,
                    );
                    whole.extend_from_slice(b"\x1b_G");
                    whole.extend_from_slice(&control);
                    whole.push(b';');
                    whole.extend_from_slice(&chain.payload);
                    whole.extend_from_slice(payload);
                    whole.extend_from_slice(b"\x1b\\");
                    self.emit(ImageProtocol::Kitty, whole);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Drop a chain in progress, counted. A half picture is never emitted.
    fn abandon_assembly(&mut self) {
        if let Some(chain) = self.assembling.take() {
            self.discarded_envelopes += 1;
            self.discarded_bytes += (chain.control.len() + chain.payload.len()) as u64;
        }
    }

    fn check_bound(&mut self) {
        if self.staged.len() > MAX_IMAGE_BYTES {
            self.discarded_envelopes += 1;
            self.discarded_bytes += self.staged.len() as u64;
            let kitty = self.state == State::ApcBody;
            self.staged.clear();
            self.staged_since = None;
            self.body_esc = false;
            // A chain missing a middle chunk can never assemble: what came
            // before no longer has a whole to belong to.
            self.abandon_assembly();
            self.state = State::Discarding { kitty };
        }
    }

    fn check_deadline(&mut self, clean: &mut Vec<u8>) {
        if self.staged.is_empty() {
            return;
        }
        if let Some(since) = self.staged_since
            && since.elapsed() > self.deadline
        {
            self.discarded_envelopes += 1;
            self.discarded_bytes += self.staged.len() as u64;
            // Past the deadline the opening is abandoned, but the bytes were
            // only ever withheld, never judged: they go back to the stream,
            // where the bound in front of the parser still applies. A picture
            // that trickles slower than the cap per deadline arrives in the
            // filter's discard count rather than on screen.
            self.abandon_assembly();
            self.flush_to_clean(clean);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(parts: &[&[u8]]) -> (Vec<u8>, Vec<CapturedImage>) {
        let mut s = ImageSniffer::new();
        let mut clean = Vec::new();
        for p in parts {
            s.feed(p, &mut clean);
        }
        (clean, s.take_images())
    }

    #[test]
    fn ordinary_text_and_sequences_pass_through_byte_for_byte() {
        let input = b"hi\x1b[1;31mred\x1b]0;title\x07done\x1b(B!";
        let (clean, images) = run(&[input]);
        assert_eq!(clean, input);
        assert!(images.is_empty());
    }

    #[test]
    fn a_kitty_envelope_is_captured_and_hidden_from_the_parser() {
        let img = b"\x1b_Gf=100,a=T;Zm9vYmFy\x1b\\";
        let (clean, images) = run(&[b"before", img, b"after"]);
        assert_eq!(clean, b"beforeafter");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].protocol, ImageProtocol::Kitty);
        assert_eq!(images[0].bytes, img);
        assert_eq!(images[0].id, 1);
    }

    #[test]
    fn an_iterm2_envelope_is_captured_with_bel_and_with_st() {
        let bel = b"\x1b]1337;File=inline=1:Zm9v\x07";
        let st = b"\x1b]1337;File=inline=1:Zm9v\x1b\\";
        let (clean, images) = run(&[bel, st]);
        assert!(clean.is_empty());
        assert_eq!(images.len(), 2);
        assert!(images.iter().all(|i| i.protocol == ImageProtocol::ITerm2));
        assert_eq!(images[0].id, 1);
        assert_eq!(images[1].id, 2);
    }

    /// iTerm2's other commands share the `1337` address and are not pictures. Captured,
    /// they reached the outer terminal verbatim: a clipboard write past the policy gate.
    #[test]
    fn an_iterm2_command_that_is_not_a_file_is_not_a_picture() {
        for command in [
            &b"\x1b]1337;ClearScrollback\x07"[..],
            b"\x1b]1337;StealFocus\x07",
            b"\x1b]1337;Copy=:aGk=\x07",
            b"\x1b]1337;SetMark\x1b\\",
        ] {
            let (clean, images) = run(&[command]);
            assert!(images.is_empty(), "{command:?} was captured");
            assert_eq!(clean, command, "{command:?} left the stream");
        }
    }

    /// A lone ESC in a body ends the envelope and what follows is output again: held
    /// back, the rest of the stream vanished into a picture that never finished.
    #[test]
    fn a_stray_escape_ends_an_envelope_and_the_output_after_it_survives() {
        let (clean, images) = run(&[b"\x1b]1337;File=inline=1:Zm9v\x1b[1mbold text"]);
        assert!(images.is_empty());
        assert_eq!(clean, b"\x1b[1mbold text");
        let (clean, images) = run(&[b"\x1b_Gf=100;Zm9v\x1bxplain"]);
        assert!(images.is_empty());
        assert_eq!(clean, b"\x1bxplain");
    }

    #[test]
    fn an_envelope_split_across_reads_completes_whole() {
        let (clean, images) = run(&[b"\x1b_Gf=100,a", b"=T;Zm9v", b"YmFy\x1b", b"\\after"]);
        assert_eq!(clean, b"after");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].bytes, b"\x1b_Gf=100,a=T;Zm9vYmFy\x1b\\");
    }

    #[test]
    fn a_non_image_apc_and_osc_pass_through() {
        // `ESC _ X` is not Kitty graphics; `ESC ] 0;` is a title, not iTerm2.
        let input = b"\x1b_Xhello\x1b\\\x1b]0;title\x07";
        let (clean, images) = run(&[input]);
        assert_eq!(clean, input);
        assert!(images.is_empty());
    }

    #[test]
    fn neighbouring_text_survives_on_both_sides() {
        let (clean, images) = run(&[b"before\r\n", b"\x1b_Gf=100,a=T;iVBOR\x1b\\", b"after\r\n"]);
        assert_eq!(clean, b"before\r\nafter\r\n");
        assert_eq!(images.len(), 1);
    }

    #[test]
    fn an_over_budget_envelope_is_discarded_whole_and_counted() {
        let mut s = ImageSniffer::new();
        let mut clean = Vec::new();
        s.feed(b"\x1b_G", &mut clean);
        s.feed(&vec![b'A'; MAX_IMAGE_BYTES], &mut clean);
        // Terminator after the bound: swallowed, and the stream resynchronises.
        s.feed(b"\x1b\\next", &mut clean);
        assert_eq!(clean, b"next");
        assert!(s.take_images().is_empty());
        let (seqs, bytes) = s.discarded();
        assert_eq!(seqs, 1);
        assert!(bytes as usize >= MAX_IMAGE_BYTES);
    }

    #[test]
    fn a_past_deadline_envelope_is_discarded_and_counted() {
        let mut s = ImageSniffer::with_deadline(Duration::from_millis(1));
        let mut clean = Vec::new();
        s.feed(b"\x1b_Gf=100;", &mut clean);
        std::thread::sleep(Duration::from_millis(5));
        s.feed(b"more", &mut clean);
        assert!(s.take_images().is_empty());
        assert_eq!(s.discarded().0, 1);
        // And the sniffer still works afterwards.
        let (clean2, images) = run(&[b"\x1b_Ga=T;eA==\x1b\\"]);
        let _ = (clean2, images);
        let mut clean3 = Vec::new();
        s.feed(b"\x1b_Ga=T;eA==\x1b\\", &mut clean3);
        assert_eq!(s.take_images().len(), 1);
    }

    #[test]
    fn a_cancel_flushes_the_staged_bytes_to_clean() {
        let mut s = ImageSniffer::new();
        let mut clean = Vec::new();
        s.feed(b"\x1b_Gf=100\x18", &mut clean);
        s.feed(b"text", &mut clean);
        assert_eq!(clean, b"\x1b_Gf=100\x18text");
        assert!(s.take_images().is_empty());
    }

    #[test]
    fn a_chunked_kitty_transmission_assembles_to_one_image() {
        // Three chunks: control only on the first, `m=0` closing the last.
        let (clean, images) = run(&[
            b"\x1b_Gf=100,s=4,v=4,m=1;QUJD".as_slice(),
            b"REVG\x1b\\".as_slice(),
            b"\x1b_Gm=1;R0hJ\x1b\\".as_slice(),
            b"\x1b_Gm=0;SktM\x1b\\after".as_slice(),
        ]);
        assert_eq!(clean, b"after");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].protocol, ImageProtocol::Kitty);
        assert_eq!(
            images[0].bytes,
            b"\x1b_Gf=100,s=4,v=4,m=0;QUJDREVGR0hJSktM\x1b\\"
        );
    }

    #[test]
    fn a_chunked_transmission_over_budget_is_dropped_whole() {
        // Two middle chunks whose payloads sum past the cap: the chain is
        // abandoned, counted, and never half drawn. Each chunk alone stays
        // inside the per-envelope bound, so this is the assembly bound firing,
        // not the staging one.
        let mut s = ImageSniffer::new();
        let mut clean = Vec::new();
        s.feed(b"\x1b_Gf=100,m=1;", &mut clean);
        s.feed(&vec![b'A'; MAX_IMAGE_BYTES - 1024], &mut clean);
        s.feed(b"\x1b\\", &mut clean);
        s.feed(b"\x1b_Gm=1;", &mut clean);
        s.feed(&vec![b'B'; 2048], &mut clean);
        s.feed(b"\x1b\\", &mut clean);
        assert!(s.take_images().is_empty());
        assert!(s.discarded().0 >= 1);
        // And the sniffer still works afterwards.
        s.feed(b"\x1b_Ga=T;eA==\x1b\\", &mut clean);
        assert_eq!(s.take_images().len(), 1);
    }

    #[test]
    fn an_osc_that_merely_starts_with_1337_is_not_an_image() {
        let input = b"\x1b]13371;not an image\x07";
        let (clean, images) = run(&[input]);
        assert_eq!(clean, input);
        assert!(images.is_empty());
    }

    #[test]
    fn a_stalled_envelope_is_replayed_not_lost() {
        // The opening may have been ordinary text all along: past the deadline
        // it goes back to the stream instead of vanishing.
        let mut s = ImageSniffer::with_deadline(Duration::from_millis(1));
        let mut clean = Vec::new();
        s.feed(b"\x1b_Gf=100;", &mut clean);
        std::thread::sleep(Duration::from_millis(5));
        s.feed(b"more", &mut clean);
        assert_eq!(clean, b"\x1b_Gf=100;more");
        assert_eq!(s.discarded().0, 1);
    }
}

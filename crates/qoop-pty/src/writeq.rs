//! The PTY write queue.
//!
//! Three measured facts shape this, and each of them broke an earlier design:
//!
//! 1. A write large enough to fill the PTY buffer partially succeeds and then returns
//!    `WouldBlock`. Without a retained offset the tail is silently lost. The spike's
//!    `write_all` worked only because its writes were small.
//! 2. The effective tty input queue is on the order of one kilobyte: delivering 1 MiB took
//!    over a thousand `EAGAIN` retries. Chunks are sized to that, not to a round number.
//! 3. The queue is bounded by **bytes**, not by message count. Bounding by count lets one
//!    arbitrarily large paste occupy one slot, which is how a multi-megabyte paste becomes
//!    an unbounded allocation.

use bytes::Bytes;
use qoop_domain::EscapeState;
use std::collections::VecDeque;

/// Sized to the measured tty input queue rather than to a round number, so a chunk is
/// roughly what one drain can absorb.
pub const CHUNK: usize = 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WriteQueueError {
    #[error("write queue is full: {queued} bytes queued, {cap} byte capacity")]
    Full { queued: usize, cap: usize },
    #[error("terminal is closing and no longer accepts writes")]
    Closed,
}

/// Priority of a queued write.
///
/// Terminal replies are ordered ahead of user input and have reserved capacity, because an
/// application that emitted a Device Status Report is blocked until it gets an answer, and
/// a saturated input queue must not be able to deadlock it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WritePriority {
    UserInput,
    TerminalReply,
}

struct Pending {
    bytes: Bytes,
    offset: usize,
}

pub struct WriteQueue {
    replies: VecDeque<Pending>,
    input: VecDeque<Pending>,
    queued: usize,
    /// Tracked separately so each class is bounded by its own budget.
    reply_bytes: usize,
    cap: usize,
    reserved_for_replies: usize,
    closed: bool,
}

impl WriteQueue {
    pub fn new(cap: usize, reserved_for_replies: usize) -> Self {
        assert!(
            reserved_for_replies < cap,
            "reservation must fit in the cap"
        );
        Self {
            replies: VecDeque::new(),
            input: VecDeque::new(),
            queued: 0,
            reply_bytes: 0,
            cap,
            reserved_for_replies,
            closed: false,
        }
    }

    pub fn queued_bytes(&self) -> usize {
        self.queued
    }

    pub fn is_empty(&self) -> bool {
        self.replies.is_empty() && self.input.is_empty()
    }

    /// Close the write gate. Anything already accepted still drains, which is what lets a
    /// terminal finish delivering the bytes it promised before it tears down.
    pub fn close(&mut self) {
        self.closed = true;
    }

    pub fn push(&mut self, bytes: Bytes, priority: WritePriority) -> Result<(), WriteQueueError> {
        if self.closed {
            return Err(WriteQueueError::Closed);
        }
        // Each class has its own budget, so neither can starve the other. Giving replies
        // the whole cap would let a chatty application's replies fill it and leave no room
        // for the user to type, which is the same failure in the other direction.
        let (used, budget) = match priority {
            WritePriority::TerminalReply => (self.reply_bytes, self.reserved_for_replies),
            WritePriority::UserInput => (
                self.queued - self.reply_bytes,
                self.cap - self.reserved_for_replies,
            ),
        };
        if used + bytes.len() > budget {
            return Err(WriteQueueError::Full {
                queued: used,
                cap: budget,
            });
        }
        self.queued += bytes.len();
        let p = Pending { bytes, offset: 0 };
        match priority {
            WritePriority::TerminalReply => {
                self.reply_bytes += p.bytes.len();
                self.replies.push_back(p)
            }
            WritePriority::UserInput => self.input.push_back(p),
        }
        Ok(())
    }

    /// Everything still waiting, replies first: what a daemon that is handing its terminals
    /// over carries to the next one.
    pub fn remaining(&self) -> Vec<u8> {
        self.replies
            .iter()
            .chain(self.input.iter())
            .flat_map(|p| p.bytes[p.offset..].iter().copied())
            .collect()
    }

    /// The next slice to hand to `write`, at most `CHUNK` bytes. Replies first.
    pub fn peek(&self) -> Option<&[u8]> {
        let p = self.replies.front().or_else(|| self.input.front())?;
        let end = (p.offset + CHUNK).min(p.bytes.len());
        Some(&p.bytes[p.offset..end])
    }

    /// Record that `n` bytes were accepted by the kernel. A partial write is the normal
    /// case, not an error.
    pub fn advance(&mut self, n: usize) {
        let from_replies = !self.replies.is_empty();
        let q = if from_replies {
            &mut self.replies
        } else {
            &mut self.input
        };
        let Some(front) = q.front_mut() else { return };
        // Clamp: a caller that reports more than the front's remaining length would
        // otherwise underflow `queued`, and an underflowed total silently removes the
        // bound this whole type exists to provide.
        let n = n.min(front.bytes.len() - front.offset);
        front.offset += n;
        self.queued -= n;
        if from_replies {
            self.reply_bytes -= n;
        }
        if front.offset >= front.bytes.len() {
            q.pop_front();
        }
    }
}

/// Split a paste into chunks whose boundaries never fall inside an escape sequence.
///
/// A boundary is taken at the first ground state at or after the target size, so a chunk
/// may be slightly larger than `CHUNK` but never splits a sequence. A pathological input
/// that never returns to ground, for example an unterminated OSC, would otherwise produce
/// one unbounded chunk, so there is a hard ceiling after which the chunk is emitted anyway:
/// at that point the input is already malformed and holding it is worse than splitting it.
pub const MAX_CHUNK: usize = 8 * CHUNK;

pub fn chunk_paste(input: &[u8]) -> Vec<Bytes> {
    let mut out = Vec::new();
    let mut state = EscapeState::default();
    let mut start = 0usize;
    for (i, b) in input.iter().enumerate() {
        state = state.step(*b);
        let len = i + 1 - start;
        let at_boundary = state.is_ground() && len >= CHUNK;
        if at_boundary || len >= MAX_CHUNK {
            out.push(Bytes::copy_from_slice(&input[start..=i]));
            start = i + 1;
        }
    }
    if start < input.len() {
        out.push(Bytes::copy_from_slice(&input[start..]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_write_keeps_its_offset() {
        let mut q = WriteQueue::new(4096, 512);
        q.push(Bytes::from_static(b"abcdef"), WritePriority::UserInput)
            .unwrap();
        assert_eq!(q.peek(), Some(&b"abcdef"[..]));
        q.advance(2);
        assert_eq!(q.peek(), Some(&b"cdef"[..]));
        q.advance(4);
        assert!(q.is_empty());
        assert_eq!(q.queued_bytes(), 0);
    }

    #[test]
    fn what_is_left_comes_out_replies_first_from_the_offset() {
        let mut q = WriteQueue::new(4096, 512);
        q.push(Bytes::from_static(b"typed"), WritePriority::UserInput)
            .unwrap();
        q.push(Bytes::from_static(b"reply"), WritePriority::TerminalReply)
            .unwrap();
        q.advance(2);
        assert_eq!(q.remaining(), b"plytyped");
    }

    #[test]
    fn the_queue_is_bounded_by_bytes_not_by_message_count() {
        let mut q = WriteQueue::new(1000, 100);
        // One large message is refused even though the queue holds no messages at all,
        // which a count-bounded queue would have accepted.
        let err = q
            .push(Bytes::from(vec![0u8; 2000]), WritePriority::UserInput)
            .unwrap_err();
        assert!(matches!(err, WriteQueueError::Full { .. }));
    }

    #[test]
    fn a_saturated_input_queue_cannot_deadlock_a_terminal_reply() {
        // The application is blocked waiting for a cursor position report. If input could
        // consume the whole budget, the reply would never be queued and it would hang.
        let mut q = WriteQueue::new(1000, 200);
        q.push(Bytes::from(vec![0u8; 800]), WritePriority::UserInput)
            .unwrap();
        assert!(
            q.push(Bytes::from_static(b"x"), WritePriority::UserInput)
                .is_err()
        );
        q.push(
            Bytes::from_static(b"\x1b[10;5R"),
            WritePriority::TerminalReply,
        )
        .expect("the reservation exists for exactly this");
    }

    #[test]
    fn replies_are_written_before_queued_input() {
        let mut q = WriteQueue::new(4096, 512);
        q.push(Bytes::from_static(b"input"), WritePriority::UserInput)
            .unwrap();
        q.push(Bytes::from_static(b"reply"), WritePriority::TerminalReply)
            .unwrap();
        assert_eq!(q.peek(), Some(&b"reply"[..]));
    }

    #[test]
    fn a_closed_queue_refuses_new_writes_but_drains_what_it_accepted() {
        let mut q = WriteQueue::new(4096, 512);
        q.push(Bytes::from_static(b"already"), WritePriority::UserInput)
            .unwrap();
        q.close();
        assert_eq!(
            q.push(Bytes::from_static(b"new"), WritePriority::UserInput),
            Err(WriteQueueError::Closed)
        );
        assert_eq!(q.peek(), Some(&b"already"[..]));
    }

    #[test]
    fn advance_past_the_front_cannot_underflow_the_total() {
        let mut q = WriteQueue::new(4096, 512);
        q.push(Bytes::from_static(b"abc"), WritePriority::UserInput)
            .unwrap();
        // A caller that over-reports would otherwise wrap `queued` and silently remove the
        // bound this type exists to provide.
        q.advance(99);
        assert_eq!(q.queued_bytes(), 0);
        assert!(q.is_empty());
    }

    #[test]
    fn replies_cannot_starve_user_input_either() {
        // The reservation exists so replies always fit. Giving replies the whole cap would
        // let a chatty application fill it and leave no room for the user to type, which is
        // the same failure in the other direction.
        let mut q = WriteQueue::new(1000, 200);
        for _ in 0..40 {
            let _ = q.push(Bytes::from(vec![0u8; 8]), WritePriority::TerminalReply);
        }
        q.push(Bytes::from(vec![0u8; 700]), WritePriority::UserInput)
            .expect("user input must keep its own budget");
    }

    #[test]
    fn chunking_preserves_the_input_exactly() {
        let input: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let joined: Vec<u8> = chunk_paste(&input).concat();
        assert_eq!(joined, input);
    }

    /// Run the scanner over a chunk and report where it ends up. A chunk is safe exactly
    /// when a fresh scanner is back at ground after consuming it.
    fn ends_in_ground(chunk: &[u8]) -> bool {
        let mut st = EscapeState::default();
        for b in chunk {
            st = st.step(*b);
        }
        st.is_ground()
    }

    /// An oracle that does not use the scanner under test: a sequence is intact if it
    /// appears contiguously inside exactly one chunk. Asserting with the scanner would
    /// only prove the scanner agrees with itself.
    fn appears_whole_in_one_chunk(chunks: &[Bytes], needle: &[u8]) -> bool {
        chunks
            .iter()
            .filter(|c| c.windows(needle.len()).any(|w| w == needle))
            .count()
            == 1
    }

    #[test]
    fn known_sequences_appear_whole_in_exactly_one_chunk() {
        // Independent of the scanner: if a boundary split any of these, the byte string
        // would appear in no chunk at all, because it would be spread across two.
        let needles: [&[u8]; 5] = [
            b"\x1b[38;5;123m",
            b"\x1b[200~",
            b"\x1b[201~",
            b"\x1b]0;a title\x07",
            b"\x1bP1;2q payload \x1b\\",
        ];
        for (i, needle) in needles.iter().enumerate() {
            // Place the sequence so a naive split at CHUNK would land inside it.
            let offset = CHUNK - 3 - i;
            let mut input = vec![b'a'; offset];
            input.extend_from_slice(needle);
            input.extend_from_slice(&[b'b'; 3000]);
            let chunks = chunk_paste(&input);
            assert_eq!(chunks.concat(), input, "chunking lost bytes for {needle:?}");
            assert!(
                appears_whole_in_one_chunk(&chunks, needle),
                "{needle:?} was split across chunks"
            );
        }
    }

    #[test]
    fn a_chunk_boundary_never_falls_inside_an_escape_sequence() {
        // A payload where a naive split at CHUNK lands inside a CSI sequence.
        let mut input = vec![b'a'; CHUNK - 3];
        input.extend_from_slice(b"\x1b[38;5;123m");
        input.extend_from_slice(&[b'b'; 4000]);
        for c in chunk_paste(&input) {
            assert!(ends_in_ground(&c), "a chunk ended mid-sequence");
        }
    }

    #[test]
    fn string_sequences_of_every_kind_stay_whole() {
        // OSC, DCS, APC, PM and SOS are all string sequences. The first version only knew
        // about OSC, so a DCS straddling a boundary would have been split.
        for intro in [&b"\x1b]0;"[..], b"\x1bP", b"\x1b_G", b"\x1b^", b"\x1bX"] {
            let mut input = vec![b'x'; CHUNK - 1];
            input.extend_from_slice(intro);
            input.extend_from_slice(&[b'p'; 200]);
            input.extend_from_slice(b"\x1b\\");
            input.extend_from_slice(&[b'y'; 100]);
            let chunks = chunk_paste(&input);
            assert_eq!(chunks.concat(), input);
            for c in &chunks {
                assert!(ends_in_ground(c), "split inside {intro:?}");
            }
        }
    }

    #[test]
    fn bracketed_paste_markers_stay_whole() {
        let mut input = vec![b'x'; CHUNK - 2];
        input.extend_from_slice(b"\x1b[200~");
        input.extend_from_slice(&[b'y'; 100]);
        input.extend_from_slice(b"\x1b[201~");
        let chunks = chunk_paste(&input);
        let joined: Vec<u8> = chunks.concat();
        assert_eq!(joined, input);
        for c in &chunks {
            assert!(ends_in_ground(c));
        }
    }

    #[test]
    fn an_unterminated_sequence_is_split_at_a_hard_ceiling_rather_than_held_forever() {
        // At this point the input is already malformed, and holding an unbounded chunk
        // waiting for a terminator that will never come is worse than splitting it.
        let mut input = vec![0x1b, b']'];
        input.extend_from_slice(&[b'a'; 100_000]);
        let chunks = chunk_paste(&input);
        assert_eq!(chunks.concat(), input);
        assert!(
            chunks.len() > 1,
            "an unterminated sequence must still be split"
        );
        for c in &chunks {
            assert!(c.len() <= MAX_CHUNK);
        }
    }
}

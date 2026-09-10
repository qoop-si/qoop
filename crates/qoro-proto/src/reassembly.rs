//! Inbound chunk reassembly, with the bounds that make it safe.
//!
//! A body larger than one frame is chunked across frames on the same stream, with END on
//! the last. Without bounds, any process that can complete a handshake streams individually
//! valid chunks with END never set until the daemon is out of memory, and one daemon serves
//! every workspace on the machine.
//!
//! The bounds are deliberately **receiver-side only**. Applying the deadline to our own
//! sending would turn a client on a slow link into a disconnect loop, which is the mobile
//! case; outbound pacing is flow control and drain-rate eviction, not a timer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};

use crate::frame::{Channel, Flags, Frame};

/// Maximum reassembled size for one inbound stream, on the control, event and terminal
/// channels. The blob channel exists precisely so a large artefact does not have to fit
/// inside this, and it carries its own per-transfer limit.
pub const MAX_REASSEMBLED: usize = 8 * 1024 * 1024;

/// Maximum time from the first chunk of a sequence to its END.
pub const SEQUENCE_DEADLINE: Duration = Duration::from_secs(10);

/// Maximum reassembled bytes across **all** inbound streams on one connection.
///
/// The per-stream cap alone bounds nothing useful: a peer opens many streams and holds one
/// unfinished chunk on each, and the total is streams times the per-stream cap. Measured by
/// review at 125 MiB from 2000 frames, and the real ceiling without this is link bandwidth
/// multiplied by the sequence deadline.
pub const MAX_CONNECTION_REASSEMBLY: usize = 16 * 1024 * 1024;

/// Maximum number of unfinished chunk sequences on one connection.
///
/// Bytes alone are not enough: many tiny partial sequences cost map entries and expiry work
/// rather than payload bytes.
pub const MAX_CONCURRENT_SEQUENCES: usize = 256;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReassemblyError {
    #[error("stream {stream} reassembly exceeded {MAX_REASSEMBLED} bytes")]
    TooLarge { stream: u32 },
    #[error("stream {stream} did not complete within the sequence deadline")]
    Expired { stream: u32 },
    #[error("stream {stream} interleaved a new message inside an unfinished chunk sequence")]
    Interleaved { stream: u32 },
    #[error("stream {stream} carried an ACK on a chunk other than the first")]
    AckOnContinuation { stream: u32 },
    #[error("connection reassembly exceeded {MAX_CONNECTION_REASSEMBLY} bytes across all streams")]
    ConnectionTooLarge,
    #[error("more than {MAX_CONCURRENT_SEQUENCES} unfinished chunk sequences on one connection")]
    TooManySequences,
}

struct Partial {
    started: Instant,
    buf: BytesMut,
    ack: Option<u64>,
    last_seq: u32,
}

/// A complete logical message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub channel: Channel,
    pub stream: u32,
    pub ack: Option<u64>,
    pub body: Bytes,
}

/// Per-connection inbound reassembly state.
#[derive(Default)]
pub struct Reassembler {
    partial: HashMap<(u8, u32), Partial>,
    /// Maintained incrementally. Summing the map per frame would make an attack that opens
    /// many streams quadratic in the number of streams, which is its own denial of service.
    pending: usize,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one decoded frame. Returns a message when the sequence completes.
    ///
    /// `now` is a parameter rather than a call to `Instant::now` so the deadline is
    /// testable without sleeping, which is the difference between a test that runs in
    /// microseconds and one nobody runs.
    pub fn push(&mut self, frame: Frame, now: Instant) -> Result<Option<Message>, ReassemblyError> {
        let key = (frame.channel as u8, frame.stream);
        let is_end = frame.flags.contains(Flags::END);

        // A standalone acknowledgement is not part of any message: END set, no body, an ack
        // value. It has to be allowed through while a chunk sequence is open on the same
        // stream, because ADR-013 requires a client to acknowledge within two seconds and
        // ADR-004 requires a large paste to be chunked, and a client obeying both emits one
        // between two chunks. Treating it as a continuation disconnected a conforming
        // client in the middle of a paste.
        if is_end && frame.body.is_empty() && frame.ack.is_some() {
            return Ok(Some(Message {
                channel: frame.channel,
                stream: frame.stream,
                ack: frame.ack,
                body: frame.body,
            }));
        }

        // The fast path: a single-frame message with nothing pending on this stream.
        if is_end && !self.partial.contains_key(&key) {
            return Ok(Some(Message {
                channel: frame.channel,
                stream: frame.stream,
                ack: frame.ack,
                body: frame.body,
            }));
        }

        match self.partial.get_mut(&key) {
            None => {
                // First chunk of a new sequence.
                if frame.body.len() > MAX_REASSEMBLED {
                    return Err(ReassemblyError::TooLarge {
                        stream: frame.stream,
                    });
                }
                if self.partial.len() >= MAX_CONCURRENT_SEQUENCES {
                    return Err(ReassemblyError::TooManySequences);
                }
                if self.pending_bytes() + frame.body.len() > MAX_CONNECTION_REASSEMBLY {
                    return Err(ReassemblyError::ConnectionTooLarge);
                }
                let mut buf = BytesMut::with_capacity(frame.body.len());
                buf.extend_from_slice(&frame.body);
                self.pending += buf.len();
                self.partial.insert(
                    key,
                    Partial {
                        started: now,
                        buf,
                        ack: frame.ack,
                        last_seq: frame.seq,
                    },
                );
                Ok(None)
            }
            Some(p) => {
                // Inclusive. Reaching the deadline is the deadline passing, and a strict
                // comparison left a sequence alive at exactly the boundary, which a property
                // test generating a zero jitter finds and a hand written one does not.
                if now.duration_since(p.started) >= SEQUENCE_DEADLINE {
                    self.drop_partial(&key);
                    return Err(ReassemblyError::Expired {
                        stream: frame.stream,
                    });
                }
                // ACK may ride only on the first frame of a sequence. Allowing it anywhere
                // would make "which ack is authoritative" a question with two answers.
                if frame.ack.is_some() {
                    self.drop_partial(&key);
                    return Err(ReassemblyError::AckOnContinuation {
                        stream: frame.stream,
                    });
                }
                // A chunk sequence is atomic once started: no other logical message may be
                // interleaved on that stream until END. There is no message id in the
                // envelope, so a receiver could not otherwise detect a splice, and it would
                // reassemble a body that is neither of the two messages.
                if frame.seq != p.last_seq.wrapping_add(1) {
                    self.drop_partial(&key);
                    return Err(ReassemblyError::Interleaved {
                        stream: frame.stream,
                    });
                }
                if p.buf.len() + frame.body.len() > MAX_REASSEMBLED {
                    self.drop_partial(&key);
                    return Err(ReassemblyError::TooLarge {
                        stream: frame.stream,
                    });
                }
                if self.pending + frame.body.len() > MAX_CONNECTION_REASSEMBLY {
                    self.drop_partial(&key);
                    return Err(ReassemblyError::ConnectionTooLarge);
                }
                let p = self.partial.get_mut(&key).expect("just checked");
                p.buf.extend_from_slice(&frame.body);
                self.pending += frame.body.len();
                p.last_seq = frame.seq;
                if is_end {
                    let p = self.partial.remove(&key).expect("just checked");
                    self.pending -= p.buf.len();
                    Ok(Some(Message {
                        channel: frame.channel,
                        stream: frame.stream,
                        ack: p.ack,
                        body: p.buf.freeze(),
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Drop sequences that have exceeded the deadline without a further frame.
    ///
    /// Without this, a peer that sends one chunk and then goes silent holds its buffer
    /// until the connection closes, because the deadline above is only evaluated when a
    /// frame arrives.
    pub fn expire(&mut self, now: Instant) -> Vec<u32> {
        let expired: Vec<_> = self
            .partial
            .iter()
            .filter(|(_, p)| now.duration_since(p.started) >= SEQUENCE_DEADLINE)
            .map(|(k, _)| *k)
            .collect();
        for k in &expired {
            self.drop_partial(k);
        }
        expired.into_iter().map(|(_, stream)| stream).collect()
    }

    /// Remove a partial sequence and release its bytes from the connection total. Every
    /// removal goes through here, so `pending` cannot drift.
    fn drop_partial(&mut self, key: &(u8, u32)) {
        if let Some(p) = self.partial.remove(key) {
            self.pending -= p.buf.len();
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending
    }

    /// Number of unfinished chunk sequences.
    pub fn pending_sequences(&self) -> usize {
        self.partial.len()
    }
}

/// Split a body into frames for one stream, setting END on the last.
///
/// `first_seq` is the sequence number the writer assigns at hand-off. An ACK, if any,
/// rides on the first frame only.
pub fn chunk(
    channel: Channel,
    stream: u32,
    first_seq: u32,
    ack: Option<u64>,
    body: Bytes,
) -> Vec<Frame> {
    let budget = crate::frame::MAX_PAYLOAD - ack.map_or(0, |_| crate::frame::ACK_PREFIX_LEN);
    if body.len() <= budget {
        let mut f = Frame::new(channel, stream, body);
        f.seq = first_seq;
        f.ack = ack;
        return vec![f];
    }
    let mut frames = Vec::new();
    let mut offset = 0usize;
    let mut seq = first_seq;
    while offset < body.len() {
        let room = if frames.is_empty() {
            budget
        } else {
            crate::frame::MAX_PAYLOAD
        };
        let end = (offset + room).min(body.len());
        let is_last = end == body.len();
        let mut f = Frame {
            channel,
            flags: if is_last {
                Flags::END
            } else {
                Flags::default()
            },
            stream,
            seq,
            ack: if frames.is_empty() { ack } else { None },
            body: body.slice(offset..end),
        };
        if !is_last {
            f.flags = Flags::default();
        }
        frames.push(f);
        offset = end;
        seq = seq.wrapping_add(1);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conflict that disconnected a conforming client: ADR-013 requires an
    /// acknowledgement within two seconds, ADR-004 requires a large message to be chunked,
    /// and both apply to the same stream at the same time.
    #[test]
    fn a_standalone_acknowledgement_may_arrive_inside_a_chunk_sequence() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let body = Bytes::from(vec![7u8; crate::frame::MAX_PAYLOAD * 3]);
        let frames = chunk(Channel::Terminal, 1, 0, None, body.clone());
        assert!(frames.len() > 2, "the fixture must actually be chunked");

        assert_eq!(r.push(frames[0].clone(), now).unwrap(), None);

        let mut ack = Frame::new(Channel::Terminal, 1, Bytes::new());
        ack.ack = Some(42);
        let message = r
            .push(ack, now)
            .expect("a standalone ack is not a continuation")
            .expect("it is a complete message on its own");
        assert_eq!(message.ack, Some(42));
        assert!(message.body.is_empty());

        // And the sequence it interrupted still completes.
        assert_eq!(r.push(frames[1].clone(), now).unwrap(), None);
        let done = r.push(frames[2].clone(), now).unwrap().expect("completed");
        assert_eq!(done.body, body);
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_single_frame_message_passes_straight_through() {
        let mut r = Reassembler::new();
        let f = Frame::new(Channel::Control, 1, Bytes::from_static(b"hi"));
        let m = r.push(f, t0()).unwrap().unwrap();
        assert_eq!(m.body, Bytes::from_static(b"hi"));
        assert_eq!(r.pending_bytes(), 0);
    }

    #[test]
    fn a_chunked_body_reassembles_to_the_original() {
        let body = Bytes::from(vec![7u8; crate::frame::MAX_PAYLOAD * 3 + 11]);
        let frames = chunk(Channel::Terminal, 4, 0, None, body.clone());
        assert!(frames.len() > 3);
        assert!(frames.last().unwrap().flags.contains(Flags::END));
        assert!(
            frames[..frames.len() - 1]
                .iter()
                .all(|f| !f.flags.contains(Flags::END))
        );

        let mut r = Reassembler::new();
        let now = t0();
        let mut out = None;
        for f in frames {
            if let Some(m) = r.push(f, now).unwrap() {
                out = Some(m);
            }
        }
        assert_eq!(out.unwrap().body, body);
        assert_eq!(r.pending_bytes(), 0);
    }

    #[test]
    fn an_ack_rides_only_on_the_first_chunk() {
        let body = Bytes::from(vec![1u8; crate::frame::MAX_PAYLOAD + 1]);
        let frames = chunk(Channel::Terminal, 4, 0, Some(42), body);
        assert_eq!(frames[0].ack, Some(42));
        assert!(frames[1..].iter().all(|f| f.ack.is_none()));

        let mut r = Reassembler::new();
        let now = t0();
        let mut out = None;
        for f in frames {
            if let Some(m) = r.push(f, now).unwrap() {
                out = Some(m);
            }
        }
        assert_eq!(out.unwrap().ack, Some(42));
    }

    #[test]
    fn an_ack_on_a_continuation_is_rejected() {
        let mut r = Reassembler::new();
        let now = t0();
        let mut first = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"a"));
        first.flags = Flags::default();
        first.seq = 0;
        assert!(r.push(first, now).unwrap().is_none());
        let mut second = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"b"));
        second.seq = 1;
        second.ack = Some(1);
        assert_eq!(
            r.push(second, now),
            Err(ReassemblyError::AckOnContinuation { stream: 1 })
        );
    }

    #[test]
    fn a_sequence_that_never_ends_is_bounded_by_size() {
        // The unbounded-inbound-buffer attack: individually valid chunks, END never set.
        let mut r = Reassembler::new();
        let now = t0();
        let mut seq = 0u32;
        let chunk_body = Bytes::from(vec![0u8; crate::frame::MAX_PAYLOAD]);
        loop {
            let mut f = Frame::new(Channel::Control, 1, chunk_body.clone());
            f.flags = Flags::default();
            f.seq = seq;
            seq += 1;
            match r.push(f, now) {
                Ok(_) => {
                    assert!(
                        r.pending_bytes() <= MAX_REASSEMBLED,
                        "buffer grew past the cap"
                    );
                }
                Err(e) => {
                    assert_eq!(e, ReassemblyError::TooLarge { stream: 1 });
                    assert_eq!(r.pending_bytes(), 0, "the partial buffer must be released");
                    break;
                }
            }
            assert!(seq < 1000, "the cap must be reached long before this");
        }
    }

    #[test]
    fn a_sequence_that_stalls_expires_and_releases_its_buffer() {
        let mut r = Reassembler::new();
        let now = t0();
        let mut f = Frame::new(Channel::Control, 3, Bytes::from(vec![0u8; 1024]));
        f.flags = Flags::default();
        assert!(r.push(f, now).unwrap().is_none());
        assert_eq!(r.pending_bytes(), 1024);
        assert_eq!(r.expire(now + SEQUENCE_DEADLINE / 2), Vec::<u32>::new());
        assert_eq!(r.expire(now + SEQUENCE_DEADLINE * 2), vec![3]);
        assert_eq!(r.pending_bytes(), 0);
    }

    #[test]
    fn interleaving_a_new_message_inside_a_sequence_is_rejected() {
        // Without this, a lane replacement partway through a chunked snapshot would splice
        // two bodies into one that is neither, and no receiver could detect it.
        let mut r = Reassembler::new();
        let now = t0();
        let mut first = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"aaaa"));
        first.flags = Flags::default();
        first.seq = 10;
        assert!(r.push(first, now).unwrap().is_none());

        let mut other = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"bbbb"));
        other.seq = 99; // not last_seq + 1
        assert_eq!(
            r.push(other, now),
            Err(ReassemblyError::Interleaved { stream: 1 })
        );
    }

    #[test]
    fn many_streams_cannot_exceed_the_connection_budget() {
        // The per-stream cap alone bounds nothing: a peer opens many streams and holds one
        // unfinished chunk on each. Measured by review at 125 MiB from 2000 frames.
        let mut r = Reassembler::new();
        let now = t0();
        let body = Bytes::from(vec![0u8; 64 * 1024]);
        let mut stream = 0u32;
        loop {
            let mut f = Frame::new(Channel::Control, stream, body.clone());
            f.flags = Flags::default();
            f.seq = 0;
            stream += 1;
            match r.push(f, now) {
                Ok(_) => {
                    assert!(
                        r.pending_bytes() <= MAX_CONNECTION_REASSEMBLY,
                        "connection total grew past the cap"
                    );
                    assert!(r.pending_sequences() <= MAX_CONCURRENT_SEQUENCES);
                }
                Err(e) => {
                    assert!(
                        matches!(
                            e,
                            ReassemblyError::ConnectionTooLarge | ReassemblyError::TooManySequences
                        ),
                        "unexpected error {e:?}"
                    );
                    break;
                }
            }
            assert!(stream < 100_000, "a cap must be reached long before this");
        }
    }

    #[test]
    fn many_tiny_sequences_are_bounded_by_count_not_only_by_bytes() {
        let mut r = Reassembler::new();
        let now = t0();
        for stream in 0..(MAX_CONCURRENT_SEQUENCES as u32) {
            let mut f = Frame::new(Channel::Event, stream, Bytes::from_static(b"x"));
            f.flags = Flags::default();
            f.seq = 0;
            r.push(f, now).expect("within the count budget");
        }
        let mut f = Frame::new(Channel::Event, 9999, Bytes::from_static(b"x"));
        f.flags = Flags::default();
        assert_eq!(r.push(f, now), Err(ReassemblyError::TooManySequences));
        // Byte pressure is negligible here, so a bytes-only bound would have let this run.
        assert!(r.pending_bytes() < 4096);
    }

    #[test]
    fn the_pending_total_returns_to_zero_on_every_exit_path() {
        // `pending` is maintained incrementally, so every removal has to go through one
        // place or it drifts, and a drifted total silently changes the cap.
        let now = t0();
        for finish in [true, false] {
            let mut r = Reassembler::new();
            let mut a = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"aaaa"));
            a.flags = Flags::default();
            a.seq = 0;
            r.push(a, now).unwrap();
            assert_eq!(r.pending_bytes(), 4);
            if finish {
                let mut b = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"bb"));
                b.seq = 1;
                r.push(b, now).unwrap().unwrap();
            } else {
                let mut bad = Frame::new(Channel::Terminal, 1, Bytes::from_static(b"bb"));
                bad.seq = 77;
                assert!(r.push(bad, now).is_err());
            }
            assert_eq!(r.pending_bytes(), 0, "finish={finish}");
            assert_eq!(r.pending_sequences(), 0, "finish={finish}");
        }
    }

    #[test]
    fn streams_reassemble_independently() {
        let mut r = Reassembler::new();
        let now = t0();
        for stream in [1u32, 2] {
            let mut f = Frame::new(Channel::Terminal, stream, Bytes::from_static(b"x"));
            f.flags = Flags::default();
            f.seq = 0;
            assert!(r.push(f, now).unwrap().is_none());
        }
        for stream in [1u32, 2] {
            let mut f = Frame::new(Channel::Terminal, stream, Bytes::from_static(b"y"));
            f.seq = 1;
            let m = r.push(f, now).unwrap().unwrap();
            assert_eq!(m.stream, stream);
            assert_eq!(m.body, Bytes::from_static(b"xy"));
        }
    }
}

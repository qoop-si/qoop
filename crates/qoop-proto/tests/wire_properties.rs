//! Gate G1.5: property tests over the wire format.
//!
//! Golden fixtures only catch inputs someone already thought to capture. These are the
//! parts of the format where a hand-written test would have to guess which case matters.

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use proptest::prelude::*;
use qoop_proto::frame::{ACK_PREFIX_LEN, HEADER_LEN};
use qoop_proto::reassembly::{Reassembler, SEQUENCE_DEADLINE, chunk};
use qoop_proto::{
    Channel, DecodeError, Flags, Frame, MAX_FRAME, MAX_PAYLOAD, MAX_REASSEMBLED, ReassemblyError,
};

fn any_channel() -> impl Strategy<Value = Channel> {
    prop_oneof![
        Just(Channel::Handshake),
        Just(Channel::Control),
        Just(Channel::Event),
        Just(Channel::Terminal),
        Just(Channel::Blob),
    ]
}

fn any_frame() -> impl Strategy<Value = Frame> {
    (
        any_channel(),
        any::<u32>(),
        any::<u32>(),
        proptest::option::of(any::<u64>()),
        prop::collection::vec(any::<u8>(), 0..2048),
        any::<bool>(),
    )
        .prop_map(|(channel, stream, seq, ack, body, end)| Frame {
            channel,
            flags: if end { Flags::END } else { Flags::default() },
            // The handshake channel requires stream zero, and encode enforces it, so the
            // generator produces frames that are legal to encode. Illegal combinations are
            // covered by the dedicated rejection properties below.
            stream: if channel == Channel::Handshake {
                0
            } else {
                stream
            },
            seq,
            ack,
            body: Bytes::from(body),
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// A frame whose ACK flag disagrees with its ACK value never reaches the wire.
    #[test]
    fn a_set_ack_flag_without_a_value_never_encodes(
        channel in any_channel(),
        seq in any::<u32>(),
    ) {
        let mut f = Frame::new(channel, 0, Bytes::from_static(b"x"));
        f.seq = seq;
        f.flags = f.flags.with(Flags::ACK);
        f.ack = None;
        let mut buf = BytesMut::new();
        prop_assert!(f.encode(&mut buf).is_err());
        prop_assert!(buf.is_empty(), "a refused encode must write nothing");
    }

    /// Encode then decode is the identity, for every field, including the flags.
    #[test]
    fn encode_decode_roundtrip(f in any_frame()) {
        let mut buf = BytesMut::new();
        f.encode(&mut buf).unwrap();
        prop_assert_eq!(buf.len(), f.wire_len());
        let out = Frame::decode(&mut buf).unwrap().unwrap();
        prop_assert!(buf.is_empty());
        prop_assert_eq!(out.channel, f.channel);
        prop_assert_eq!(out.stream, f.stream);
        prop_assert_eq!(out.seq, f.seq);
        prop_assert_eq!(out.ack, f.ack);
        prop_assert_eq!(&out.body, &f.body);
        // Full flag equality, not only END: the ACK flag is derived from `ack` on encode,
        // so a disagreement between the two would otherwise pass this property.
        let mut expected = f.flags;
        if f.ack.is_some() {
            expected = expected.with(Flags::ACK);
        }
        prop_assert_eq!(out.flags, expected);
    }

    /// A stream split at any byte boundary decodes to the same frames as an unsplit feed.
    /// This is the split-boundary property that a silent
    /// 64 byte truncation cap would fail.
    #[test]
    fn split_at_any_boundary_decodes_identically(
        frames in prop::collection::vec(any_frame(), 1..6),
        cut in 0usize..4096,
    ) {
        let mut whole = BytesMut::new();
        for f in &frames {
            f.encode(&mut whole).unwrap();
        }
        let cut = cut.min(whole.len());

        let mut incremental = BytesMut::new();
        let mut got = Vec::new();
        incremental.extend_from_slice(&whole[..cut]);
        while let Some(f) = Frame::decode(&mut incremental).unwrap() {
            got.push(f);
        }
        incremental.extend_from_slice(&whole[cut..]);
        while let Some(f) = Frame::decode(&mut incremental).unwrap() {
            got.push(f);
        }

        prop_assert_eq!(got.len(), frames.len());
        for (a, b) in got.iter().zip(frames.iter()) {
            prop_assert_eq!(&a.body, &b.body);
            prop_assert_eq!(a.stream, b.stream);
        }
    }

    /// A declared length over the cap is rejected from the prefix alone, without waiting
    /// for or reserving the bytes it claims.
    #[test]
    fn an_oversized_prefix_is_rejected_without_the_payload(extra in 1usize..1_000_000) {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&((MAX_FRAME + extra) as u32).to_le_bytes());
        prop_assert_eq!(
            Frame::decode(&mut buf),
            Err(DecodeError::Oversized(MAX_FRAME + extra))
        );
    }

    /// Any reserved flag bit is rejected. A receiver that ignored them could not later be
    /// given a meaning without silently changing behaviour on old peers.
    #[test]
    fn reserved_flag_bits_are_always_rejected(bits in 0b0000_0100u8..=0b1111_1111) {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        buf.extend_from_slice(&[Channel::Control as u8, bits]);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        prop_assert!(matches!(
            Frame::decode(&mut buf),
            Err(DecodeError::ReservedFlags(_))
        ));
    }

    /// Any channel outside the defined set closes the connection rather than being ignored.
    #[test]
    fn unknown_channels_are_always_rejected(c in 5u8..=255) {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&(HEADER_LEN as u32).to_le_bytes());
        buf.extend_from_slice(&[c, 0]);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        prop_assert_eq!(Frame::decode(&mut buf), Err(DecodeError::UnknownChannel(c)));
    }

    /// A body of any size chunks and reassembles to exactly itself, with END on the last
    /// frame and on no other, and the ack on the first frame and on no other.
    #[test]
    fn chunking_reassembles_to_the_original(
        len in 0usize..(MAX_PAYLOAD * 3 + 17),
        ack in proptest::option::of(any::<u64>()),
        first_seq in any::<u32>(),
    ) {
        let body = Bytes::from(vec![0x5Au8; len]);
        let frames = chunk(Channel::Terminal, 3, first_seq, ack, body.clone());
        prop_assert!(!frames.is_empty());
        prop_assert!(frames.last().unwrap().flags.contains(Flags::END));
        for f in &frames[..frames.len() - 1] {
            prop_assert!(!f.flags.contains(Flags::END));
        }
        prop_assert_eq!(frames[0].ack, ack);
        for f in &frames[1..] {
            prop_assert_eq!(f.ack, None);
        }
        for f in &frames {
            let payload = f.body.len() + f.ack.map_or(0, |_| ACK_PREFIX_LEN);
            prop_assert!(payload <= MAX_PAYLOAD, "chunk exceeds the frame budget");
        }

        let now = std::time::Instant::now();
        let mut r = Reassembler::new();
        let mut out = None;
        for f in frames {
            if let Some(m) = r.push(f, now).unwrap() {
                prop_assert!(out.is_none(), "only the last frame completes a message");
                out = Some(m);
            }
        }
        let m = out.expect("a complete sequence must produce a message");
        prop_assert_eq!(m.body, body);
        prop_assert_eq!(m.ack, ack);
        prop_assert_eq!(r.pending_bytes(), 0);
    }

    /// Chunks survive a full encode and decode pass, not only the in-memory path.
    #[test]
    fn chunks_survive_the_wire(len in 0usize..(MAX_PAYLOAD * 2 + 3)) {
        let body = Bytes::from(vec![0xC3u8; len]);
        let mut buf = BytesMut::new();
        for f in chunk(Channel::Blob, 9, 0, None, body.clone()) {
            f.encode(&mut buf).unwrap();
        }
        let now = std::time::Instant::now();
        let mut r = Reassembler::new();
        let mut out = None;
        while let Some(f) = Frame::decode(&mut buf).unwrap() {
            if let Some(m) = r.push(f, now).unwrap() {
                out = Some(m);
            }
        }
        prop_assert_eq!(out.unwrap().body, body);
    }

    /// A frame cut anywhere before its declared length has fully arrived decodes to nothing
    /// and consumes nothing, whether the cut lands inside the four byte length prefix,
    /// inside the header, or inside the body. `frame.rs` has a unit test that does this
    /// exhaustively for one fixed frame; this covers every field and flag combination the
    /// generator produces.
    #[test]
    fn a_truncated_frame_never_decodes_or_consumes(f in any_frame(), cut in 0usize..MAX_FRAME) {
        let mut whole = BytesMut::new();
        f.encode(&mut whole).unwrap();
        let cut = cut.min(whole.len() - 1);

        let mut partial = BytesMut::from(&whole[..cut]);
        let before = partial.clone();
        prop_assert_eq!(Frame::decode(&mut partial), Ok(None));
        prop_assert_eq!(partial, before, "a partial decode must not consume anything");
    }

    /// A chunk sequence that never sets END is bounded by size rather than growing without
    /// limit. Every frame here is individually valid, legal channel, legal flags, correct
    /// sequence order, nothing truncated, only END is missing forever, which is exactly the
    /// shape the reassembler's size cap exists for (see `reassembly.rs`).
    #[test]
    fn an_unending_sequence_is_bounded_by_size(chunk_len in (MAX_PAYLOAD / 4)..=MAX_PAYLOAD) {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let body = Bytes::from(vec![0x11u8; chunk_len]);
        let mut seq = 0u32;
        loop {
            let mut f = Frame::new(Channel::Control, 7, body.clone());
            f.flags = Flags::default(); // never END
            f.seq = seq;
            seq += 1;
            match r.push(f, now) {
                Ok(_) => prop_assert!(r.pending_bytes() <= MAX_REASSEMBLED),
                Err(e) => {
                    prop_assert!(
                        matches!(
                            e,
                            ReassemblyError::TooLarge { .. } | ReassemblyError::ConnectionTooLarge
                        ),
                        "unexpected error {e:?}"
                    );
                    prop_assert_eq!(r.pending_bytes(), 0, "the partial buffer must be released");
                    break;
                }
            }
            // chunk_len is at least a quarter of MAX_PAYLOAD, so the cap is reached in a
            // bounded, small number of frames; this is a generous upper bound on that, not
            // the expected count.
            prop_assert!(seq < 10_000, "the cap must be reached long before this");
        }
    }

    /// A chunk sequence that never sets END is also bounded by time, for the case that
    /// never grows large enough to hit the size cap: one small chunk, then silence past the
    /// deadline.
    #[test]
    fn an_unending_sequence_is_bounded_by_deadline(
        body_len in 0usize..4096,
        jitter_micros in 0u64..1_000_000,
    ) {
        let mut r = Reassembler::new();
        let start = Instant::now();
        let mut first = Frame::new(Channel::Event, 3, Bytes::from(vec![0x22u8; body_len]));
        first.flags = Flags::default(); // never END
        prop_assert_eq!(r.push(first, start).unwrap(), None);

        let past_deadline = start + SEQUENCE_DEADLINE + Duration::from_micros(jitter_micros);
        let mut next = Frame::new(Channel::Event, 3, Bytes::from_static(b"x"));
        next.seq = 1;
        prop_assert_eq!(
            r.push(next, past_deadline),
            Err(ReassemblyError::Expired { stream: 3 })
        );
        prop_assert_eq!(r.pending_bytes(), 0, "the expired buffer must be released");
    }
}

//! The wire frame: encode, decode, and the bounds that make a decoder safe against a
//! hostile peer.
//!
//! This module is the normative artifact for ADR-004. The ADR is the rationale; each pass
//! over it found another decision an implementer would have had to invent while reading it,
//! which is evidence about prose as a medium. Where this code and the ADR disagree, this
//! code and its tests are right and the ADR is a bug.
//!
//! ```text
//! frame  := len:u32le  header  payload
//! header := channel:u8  flags:u8  stream:u32le  seq:u32le
//! len    := byte count of header + payload, at most MAX_FRAME
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};

/// Header size in bytes: channel, flags, stream, seq.
pub const HEADER_LEN: usize = 1 + 1 + 4 + 4;

/// Maximum length prefix value, covering header plus payload.
///
/// Deliberately small. This is what prevents head-of-line blocking on a single socket: a
/// large transfer is many small frames, so control and terminal frames interleave between
/// them. A single socket with per-lane queues gives isolation only if no one frame can
/// monopolise the write.
pub const MAX_FRAME: usize = 65536;

/// Maximum payload in one frame.
pub const MAX_PAYLOAD: usize = MAX_FRAME - HEADER_LEN;

/// Wire length of the acknowledgement prefix, when the ACK flag is set.
pub const ACK_PREFIX_LEN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Channel {
    /// Fixed for all time, never negotiated, JSON payload. Something has to be fixed for
    /// negotiation to be expressible.
    Handshake = 0,
    Control = 1,
    Event = 2,
    /// Bidirectional. Daemon to client carries render frames; client to daemon carries
    /// input, resize requests and standalone acknowledgements.
    Terminal = 3,
    Blob = 4,
}

impl Channel {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Channel::Handshake,
            1 => Channel::Control,
            2 => Channel::Event,
            3 => Channel::Terminal,
            4 => Channel::Blob,
            _ => return None,
        })
    }

    /// What `stream` means on this channel. Defined for every channel, including the ones
    /// that do not use it, because "undefined" is how two implementations stop
    /// interoperating.
    pub fn stream_meaning(self) -> StreamMeaning {
        match self {
            Channel::Handshake => StreamMeaning::AlwaysZero,
            Channel::Control => StreamMeaning::RequestId,
            Channel::Event => StreamMeaning::SubscriptionId,
            Channel::Terminal => StreamMeaning::TerminalId,
            Channel::Blob => StreamMeaning::TransferId,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMeaning {
    AlwaysZero,
    RequestId,
    SubscriptionId,
    TerminalId,
    TransferId,
}

/// Frame flags. Bits 2 through 7 are reserved and must be zero: a receiver that ignored
/// them could not later be given a meaning without silently changing behaviour on old
/// peers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Flags(u8);

impl Flags {
    pub const END: Flags = Flags(0b0000_0001);
    pub const ACK: Flags = Flags(0b0000_0010);
    const RESERVED_MASK: u8 = 0b1111_1100;

    pub fn bits(self) -> u8 {
        self.0
    }
    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn with(self, other: Flags) -> Self {
        Flags(self.0 | other.0)
    }
    fn from_bits(v: u8) -> Result<Self, DecodeError> {
        if v & Self::RESERVED_MASK != 0 {
            return Err(DecodeError::ReservedFlags(v));
        }
        Ok(Flags(v))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub channel: Channel,
    pub flags: Flags,
    pub stream: u32,
    /// Per stream, assigned at the moment the frame is handed to the writer, after any
    /// render-lane replacement. Assigned any earlier, a replaced frame would consume a
    /// number that is never transmitted and every burst would look like a gap.
    pub seq: u32,
    /// Acknowledged revision, present only when the ACK flag is set. It may ride only on
    /// the first frame of a chunk sequence.
    pub ack: Option<u64>,
    pub body: Bytes,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("declared length {0} exceeds MAX_FRAME {MAX_FRAME}")]
    Oversized(usize),
    #[error("declared length {0} is smaller than the {HEADER_LEN} byte header")]
    Undersized(usize),
    #[error("unknown channel {0}")]
    UnknownChannel(u8),
    #[error("reserved flag bits set: {0:#010b}")]
    ReservedFlags(u8),
    #[error("stream must be zero on the handshake channel, got {0}")]
    NonZeroHandshakeStream(u32),
    #[error("ACK flag set but the payload is shorter than the {ACK_PREFIX_LEN} byte prefix")]
    TruncatedAckPrefix,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("payload of {0} bytes exceeds MAX_PAYLOAD {MAX_PAYLOAD}")]
    PayloadTooLarge(usize),
    #[error("the ACK flag is set but no acknowledged revision is present")]
    AckFlagWithoutValue,
    #[error("stream must be zero on the handshake channel, got {0}")]
    NonZeroHandshakeStream(u32),
}

impl Frame {
    pub fn new(channel: Channel, stream: u32, body: impl Into<Bytes>) -> Self {
        Frame {
            channel,
            // A single-frame message sets END. A sender that omitted it and a receiver
            // that treated a missing END as "keep accumulating" would hang on the first
            // message, and both readings were consistent with the prose.
            flags: Flags::END,
            stream,
            seq: 0,
            ack: None,
            body: body.into(),
        }
    }

    /// Total wire size including the length prefix.
    pub fn wire_len(&self) -> usize {
        4 + HEADER_LEN + self.ack.map_or(0, |_| ACK_PREFIX_LEN) + self.body.len()
    }

    pub fn encode(&self, out: &mut BytesMut) -> Result<(), EncodeError> {
        // The flag and the value must agree. Encoding a set flag with no value would put a
        // frame on the wire whose payload the peer reads eight bytes into the body.
        if self.flags.contains(Flags::ACK) && self.ack.is_none() {
            return Err(EncodeError::AckFlagWithoutValue);
        }
        if self.channel == Channel::Handshake && self.stream != 0 {
            return Err(EncodeError::NonZeroHandshakeStream(self.stream));
        }
        let payload_len = self.ack.map_or(0, |_| ACK_PREFIX_LEN) + self.body.len();
        if payload_len > MAX_PAYLOAD {
            return Err(EncodeError::PayloadTooLarge(payload_len));
        }
        let mut flags = self.flags;
        if self.ack.is_some() {
            flags = flags.with(Flags::ACK);
        }
        out.reserve(4 + HEADER_LEN + payload_len);
        out.put_u32_le((HEADER_LEN + payload_len) as u32);
        out.put_u8(self.channel as u8);
        out.put_u8(flags.bits());
        out.put_u32_le(self.stream);
        out.put_u32_le(self.seq);
        if let Some(ack) = self.ack {
            out.put_u64_le(ack);
        }
        out.put_slice(&self.body);
        Ok(())
    }

    /// Decode one frame from the front of `buf`.
    ///
    /// Returns `Ok(None)` when more bytes are needed, without consuming anything. The
    /// length is validated against `MAX_FRAME` **before** any allocation, so a forged
    /// prefix cannot make us reserve gigabytes.
    pub fn decode(buf: &mut BytesMut) -> Result<Option<Frame>, DecodeError> {
        if buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > MAX_FRAME {
            return Err(DecodeError::Oversized(len));
        }
        if len < HEADER_LEN {
            return Err(DecodeError::Undersized(len));
        }
        if buf.len() < 4 + len {
            return Ok(None);
        }
        buf.advance(4);
        let mut frame = buf.split_to(len);

        let channel_raw = frame.get_u8();
        let channel =
            Channel::from_u8(channel_raw).ok_or(DecodeError::UnknownChannel(channel_raw))?;
        let flags = Flags::from_bits(frame.get_u8())?;
        let stream = frame.get_u32_le();
        let seq = frame.get_u32_le();

        if channel == Channel::Handshake && stream != 0 {
            return Err(DecodeError::NonZeroHandshakeStream(stream));
        }

        let ack = if flags.contains(Flags::ACK) {
            if frame.len() < ACK_PREFIX_LEN {
                return Err(DecodeError::TruncatedAckPrefix);
            }
            Some(frame.get_u64_le())
        } else {
            None
        };

        Ok(Some(Frame {
            channel,
            flags,
            stream,
            seq,
            ack,
            body: frame.freeze(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(f: &Frame) -> Frame {
        let mut buf = BytesMut::new();
        f.encode(&mut buf).unwrap();
        assert_eq!(buf.len(), f.wire_len());
        let out = Frame::decode(&mut buf).unwrap().unwrap();
        assert!(buf.is_empty(), "decode must consume exactly one frame");
        out
    }

    #[test]
    fn encode_decode_is_the_identity() {
        let f = Frame::new(Channel::Terminal, 7, Bytes::from_static(b"hello"));
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn an_ack_prefix_survives_a_round_trip() {
        let mut f = Frame::new(Channel::Terminal, 7, Bytes::new());
        f.ack = Some(u64::MAX);
        let out = roundtrip(&f);
        assert_eq!(out.ack, Some(u64::MAX));
        assert!(out.flags.contains(Flags::ACK));
    }

    #[test]
    fn a_single_frame_message_sets_end() {
        assert!(
            Frame::new(Channel::Control, 1, Bytes::new())
                .flags
                .contains(Flags::END)
        );
    }

    #[test]
    fn a_partial_frame_consumes_nothing() {
        let f = Frame::new(Channel::Control, 1, Bytes::from_static(b"abcdef"));
        let mut buf = BytesMut::new();
        f.encode(&mut buf).unwrap();
        let full = buf.len();
        for cut in 0..full {
            let mut partial = buf.clone();
            partial.truncate(cut);
            let before = partial.clone();
            assert_eq!(Frame::decode(&mut partial), Ok(None), "cut at {cut}");
            assert_eq!(
                partial, before,
                "a partial decode must not consume, cut {cut}"
            );
        }
    }

    #[test]
    fn an_oversized_length_is_rejected_before_allocating() {
        let mut buf = BytesMut::new();
        buf.put_u32_le((MAX_FRAME + 1) as u32);
        // Deliberately no payload: the point is that the decision is made from the prefix
        // alone, so a forged prefix cannot make us wait for or reserve the bytes.
        assert_eq!(
            Frame::decode(&mut buf),
            Err(DecodeError::Oversized(MAX_FRAME + 1))
        );
    }

    #[test]
    fn a_length_shorter_than_the_header_is_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(3);
        buf.put_slice(&[0, 0, 0]);
        assert_eq!(Frame::decode(&mut buf), Err(DecodeError::Undersized(3)));
    }

    #[test]
    fn an_unknown_channel_is_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(HEADER_LEN as u32);
        buf.put_u8(9);
        buf.put_u8(0);
        buf.put_u32_le(0);
        buf.put_u32_le(0);
        assert_eq!(Frame::decode(&mut buf), Err(DecodeError::UnknownChannel(9)));
    }

    #[test]
    fn reserved_flag_bits_are_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(HEADER_LEN as u32);
        buf.put_u8(Channel::Control as u8);
        buf.put_u8(0b0000_0100);
        buf.put_u32_le(0);
        buf.put_u32_le(0);
        assert_eq!(
            Frame::decode(&mut buf),
            Err(DecodeError::ReservedFlags(0b0000_0100))
        );
    }

    #[test]
    fn a_nonzero_stream_on_the_handshake_channel_is_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u32_le(HEADER_LEN as u32);
        buf.put_u8(Channel::Handshake as u8);
        buf.put_u8(Flags::END.bits());
        buf.put_u32_le(1);
        buf.put_u32_le(0);
        assert_eq!(
            Frame::decode(&mut buf),
            Err(DecodeError::NonZeroHandshakeStream(1))
        );
    }

    #[test]
    fn an_ack_flag_with_a_truncated_prefix_is_rejected() {
        let mut buf = BytesMut::new();
        buf.put_u32_le((HEADER_LEN + 3) as u32);
        buf.put_u8(Channel::Terminal as u8);
        buf.put_u8(Flags::ACK.bits());
        buf.put_u32_le(1);
        buf.put_u32_le(0);
        buf.put_slice(&[1, 2, 3]);
        assert_eq!(
            Frame::decode(&mut buf),
            Err(DecodeError::TruncatedAckPrefix)
        );
    }

    #[test]
    fn a_maximum_size_payload_encodes_and_decodes() {
        let f = Frame::new(Channel::Blob, 1, Bytes::from(vec![0xABu8; MAX_PAYLOAD]));
        assert_eq!(f.wire_len(), 4 + MAX_FRAME);
        assert_eq!(roundtrip(&f).body.len(), MAX_PAYLOAD);
    }

    #[test]
    fn one_byte_over_the_maximum_payload_is_refused_at_encode() {
        let f = Frame::new(Channel::Blob, 1, Bytes::from(vec![0u8; MAX_PAYLOAD + 1]));
        let mut buf = BytesMut::new();
        assert_eq!(
            f.encode(&mut buf),
            Err(EncodeError::PayloadTooLarge(MAX_PAYLOAD + 1))
        );
    }

    #[test]
    fn an_ack_prefix_counts_against_the_payload_budget() {
        let mut f = Frame::new(Channel::Terminal, 1, Bytes::from(vec![0u8; MAX_PAYLOAD]));
        f.ack = Some(0);
        let mut buf = BytesMut::new();
        assert!(matches!(
            f.encode(&mut buf),
            Err(EncodeError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn two_frames_decode_independently_from_one_buffer() {
        let a = Frame::new(Channel::Control, 1, Bytes::from_static(b"a"));
        let b = Frame::new(Channel::Event, 2, Bytes::from_static(b"bb"));
        let mut buf = BytesMut::new();
        a.encode(&mut buf).unwrap();
        b.encode(&mut buf).unwrap();
        assert_eq!(Frame::decode(&mut buf).unwrap().unwrap(), a);
        assert_eq!(Frame::decode(&mut buf).unwrap().unwrap(), b);
        assert_eq!(Frame::decode(&mut buf), Ok(None));
    }

    #[test]
    fn the_ack_flag_and_the_ack_value_must_agree() {
        let mut f = Frame::new(Channel::Terminal, 1, Bytes::new());
        f.flags = f.flags.with(Flags::ACK);
        f.ack = None;
        let mut buf = BytesMut::new();
        assert_eq!(f.encode(&mut buf), Err(EncodeError::AckFlagWithoutValue));
    }

    #[test]
    fn encode_refuses_a_nonzero_handshake_stream_as_well_as_decode() {
        // Rejecting it only on decode means we can emit a frame our own decoder refuses.
        let f = Frame::new(Channel::Handshake, 7, Bytes::new());
        let mut buf = BytesMut::new();
        assert_eq!(
            f.encode(&mut buf),
            Err(EncodeError::NonZeroHandshakeStream(7))
        );
    }

    #[test]
    fn every_channel_has_a_defined_stream_meaning() {
        for c in [
            Channel::Handshake,
            Channel::Control,
            Channel::Event,
            Channel::Terminal,
            Channel::Blob,
        ] {
            // The point is that this is exhaustive and total: an undefined stream meaning
            // is how two implementations stop interoperating.
            let _ = c.stream_meaning();
        }
        assert_eq!(
            Channel::Handshake.stream_meaning(),
            StreamMeaning::AlwaysZero
        );
    }
}

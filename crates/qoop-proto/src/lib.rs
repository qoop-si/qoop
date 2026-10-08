//! The qoop wire protocol.
//!
//! This crate, not ADR-004, is the normative definition. The ADR carries the reasoning and
//! the rejected alternatives; the frozen fixtures are generated from here.

#![forbid(unsafe_code)]

pub mod frame;
pub mod handshake;
pub mod reassembly;

pub use frame::{Channel, DecodeError, EncodeError, Flags, Frame, MAX_FRAME, MAX_PAYLOAD};
pub use handshake::{
    CODEC_GENERATION, ClientHello, CodecOffers, HandshakeOutcome, INTERFACE_ID, RefuseReason,
    SelectedCodecs, ServerCapabilities, ServerRefuse, ServerWelcome, negotiate,
};
pub use reassembly::{MAX_REASSEMBLED, Message, Reassembler, ReassemblyError, chunk};

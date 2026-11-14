//! Domain entities and the invariants that are enforced by types rather than by review.
//!
//! No I/O, no async runtime, no terminal library. That is checked by this crate's
//! dependency list, which is deliberately three crates long.
//!
//! Where a type exists to make an invariant unrepresentable rather than merely documented,
//! the invariant number is named in its doc comment, because an invariant with no enforcing
//! mechanism is a wish.

#![forbid(unsafe_code)]

pub mod escape;
pub mod identity;
pub mod ids;
pub mod launch;
pub mod observation;
pub mod program;
pub mod report;
pub mod shell;
pub mod sizing;
pub mod status;
pub mod terminal;
pub mod viewport;

pub use escape::EscapeState;
pub use identity::{AgentIdentity, Boundary, CapabilityToken, ClaimRefusal, TerminalCapability};
pub use ids::*;
pub use launch::{EnvPlan, EnvRef, LaunchIntent, NativeSessionRef, SecretRef};
pub use observation::{
    AgentBelief, AgentObservation, AgentRuntimeState, Confidence, ObservationSource,
    ObservationVerdict,
};
pub use sizing::{OwnershipEvent, PtySize, SizePolicy, SizeRequest, SizeVerdict};
pub use status::{Status, StatusFacts};
pub use terminal::{TerminalPaths, TerminalPhase};
pub use viewport::Viewport;

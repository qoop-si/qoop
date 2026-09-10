//! The durable log, and the rule that a projection is a fold over it.
//!
//! ADR-017. Three kinds of event, because the kind decides the durability rule: a command
//! is durable before it is acknowledged, an outcome is durable and paired with its command,
//! and an observation is lossy under pressure with the loss recorded as a gap.
//!
//! Terminal output is not in here. It is megabytes a second, it has its own segmented
//! store, and a log that contained it would be mostly noise and would take as long to
//! replay as the session took to happen. What the log records is that a terminal produced
//! output, which is what tells a projection to go and read the history.

#![forbid(unsafe_code)]

pub mod projection;
pub mod store;

use serde::{Deserialize, Serialize};

pub use projection::{Projection, WorkspaceView};
pub use store::{EventStore, StoreError};

/// Which ordered sequence an event belongs to.
///
/// There is no global order. Producing one means a single writer, and a single writer is
/// what makes a busy terminal slow down a quiet one. A reader that needs to interleave two
/// streams orders by timestamp and accepts that the result is approximate across them,
/// which is stated here so that no projection is written depending on an order it will not
/// get.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Stream {
    Workspace(i64),
    Terminal(u32),
    /// One command's own lifetime: the command and its outcome.
    Command(String),
}

impl Stream {
    pub fn key(&self) -> String {
        match self {
            Stream::Workspace(id) => format!("w:{id}"),
            Stream::Terminal(id) => format!("t:{id}"),
            Stream::Command(id) => format!("c:{id}"),
        }
    }
}

/// What was asked for, what happened, what we noticed, and what we lost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Body {
    /// Durable before the command is acknowledged (ADR-005 rule 2).
    Command {
        /// The client's key. The same key applied twice is applied once.
        idempotency_key: String,
        operation: String,
        detail: serde_json::Value,
    },
    /// Always paired with its command by the same key, so a restart can find an intent
    /// with no outcome and reconcile it.
    Outcome {
        idempotency_key: String,
        ok: bool,
        detail: serde_json::Value,
    },
    /// Something we noticed. Carries what it was derived from, not only the conclusion: a
    /// wrong conclusion has to be diagnosable and not merely visible.
    Observation {
        terminal: u32,
        state: String,
        source: String,
        confidence: String,
        sampled_generation: u64,
        /// The text or signal the conclusion was drawn from, already subject to the secret
        /// rules. A redaction is recorded as having happened rather than omitted.
        evidence: String,
        redacted: bool,
    },
    /// What was lost, and when. A reader that sees one knows its folded state may be
    /// behind the world and that the fix is to re-read rather than to wait.
    Gap {
        dropped: u64,
        first_unix: i64,
        last_unix: i64,
        reason: String,
    },
    /// A terminal produced output between two history ordinals. Not the output.
    Output {
        terminal: u32,
        from_ordinal: u64,
        to_ordinal: u64,
    },
}

/// One entry in the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Position within its own stream. Dense and starting at one, so a reader can tell a
    /// gap in delivery from the end of the stream.
    pub sequence: u64,
    pub stream: Stream,
    pub at_unix: i64,
    pub body: Body,
}

impl Event {
    pub fn idempotency_key(&self) -> Option<&str> {
        match &self.body {
            Body::Command {
                idempotency_key, ..
            }
            | Body::Outcome {
                idempotency_key, ..
            } => Some(idempotency_key),
            _ => None,
        }
    }

    /// Whether this event is one a projection may lose without noticing.
    pub fn is_lossy(&self) -> bool {
        matches!(self.body, Body::Observation { .. })
    }
}

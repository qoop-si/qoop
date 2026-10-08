//! One transcript message: who said it, what it says, when.
//!
//! `ts` is epoch millis when the product stores a timestamp, else `None`.
//! Model-internal reasoning (Claude thinking blocks, opencode reasoning
//! parts, Gemini thoughts) is excluded from `text` on purpose: it is not
//! conversation, and `text` stays the
//! conversation itself. What `--json` adds over text output is uncapped fidelity of
//! these messages, not a wider message set.

use serde::{Deserialize, Serialize};

/// Which side of the conversation a message belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// The agent product that owns a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Product {
    Claude,
    Codex,
    Opencode,
    Gemini,
}

impl Product {
    /// The stable name used in output and errors.
    pub fn as_str(self) -> &'static str {
        match self {
            Product::Claude => "claude",
            Product::Codex => "codex",
            Product::Opencode => "opencode",
            Product::Gemini => "gemini",
        }
    }
}

/// One message of a transcript window, oldest first in every API here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub ts: Option<i64>,
}

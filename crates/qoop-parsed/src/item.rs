//! The plain types a chat view reads. No raw records and no `extra` ever travel in them.

use serde::{Deserialize, Serialize};

/// What the chat view shows of one session, at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest {
    /// Changes whenever the session file (opencode: the database or its `-wal`) does: a hash
    /// of length and modification time. A page cursor (`Options::skip`) is only good for the
    /// revision it was taken at.
    pub revision: u64,
    /// The `seq` of the first item returned (0 when there are none), in this response's window.
    pub first_seq: u32,
    /// Older items exist: cut by `limit`, by the byte budget, or beyond the widest window read.
    pub has_older: bool,
    pub items: Vec<Item>,
    /// Records the parser could not read or could not match (a torn last line, a result whose
    /// call lies before the tail window).
    pub warnings: u32,
}

/// One row of the chat. `seq` is the item's position in the window that served the response,
/// counted from the window start (the first item the parser produced from the tail, or from
/// the whole session when it is read whole). It is not an entry id and not a line number:
/// those can be absent, synthetic (`ord:n`) or cut in two at a tail start. Use it to order
/// and to dedupe rows within one response. It is not a cursor: a wider window renumbers it,
/// so paging goes by `Options::skip` and the `Digest::revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    User {
        seq: u32,
        ts: Option<i64>,
        text: String,
        attachments: Vec<Chip>,
    },
    Agent {
        seq: u32,
        ts: Option<i64>,
        text: String,
        model: Option<String>,
    },
    /// The agent reasoned. `text` is the reasoning when the file keeps it (redacted, clamped
    /// like any text), `None` when the agent records none or only an encrypted blob. `ms`
    /// stays `None` where no file records how long it took.
    Thinking {
        seq: u32,
        ms: Option<u64>,
        text: Option<String>,
    },
    Tool {
        seq: u32,
        id: String,
        name: String,
        /// One line: the command, path, pattern, url or query.
        summary: String,
        status: Status,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
        output_bytes: u64,
        /// [`detail`](crate::detail) has input or output text to show.
        has_detail: bool,
    },
    Subagent {
        seq: u32,
        summary: String,
        steps: u32,
    },
    Compaction {
        seq: u32,
    },
    Error {
        seq: u32,
        text: String,
    },
    /// A record with no row of its own: its kind name and a short text, never its JSON.
    Notice {
        seq: u32,
        kind: String,
        text: String,
    },
}

/// A file or image the user attached: its name and size, never its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub name: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// No result yet.
    Running,
    Ok,
    Failed,
    /// No result, and the run went on or ended without one.
    Interrupted,
    /// Denied by the user or a permission rule.
    Rejected,
}

/// The full text of one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detail {
    /// What was asked: the command, the patch, the file written, else the call's arguments.
    pub input: String,
    /// What came back, as the agent saw it.
    pub output: String,
    /// `input` or `output` was cut at the byte limit (each field has the limit alone).
    pub truncated: bool,
}

impl Item {
    pub fn seq(&self) -> u32 {
        match self {
            Item::User { seq, .. }
            | Item::Agent { seq, .. }
            | Item::Thinking { seq, .. }
            | Item::Tool { seq, .. }
            | Item::Subagent { seq, .. }
            | Item::Compaction { seq }
            | Item::Error { seq, .. }
            | Item::Notice { seq, .. } => *seq,
        }
    }

    /// A tool result whose call the parser did not see.
    pub(crate) fn is_orphan(&self) -> bool {
        matches!(self, Item::Tool { name, .. } if name == crate::project::ORPHAN)
    }

    pub(crate) fn set_seq(&mut self, value: u32) {
        match self {
            Item::User { seq, .. }
            | Item::Agent { seq, .. }
            | Item::Thinking { seq, .. }
            | Item::Tool { seq, .. }
            | Item::Subagent { seq, .. }
            | Item::Compaction { seq }
            | Item::Error { seq, .. }
            | Item::Notice { seq, .. } => *seq = value,
        }
    }

    /// Bytes the item adds to a response, near enough for a budget.
    pub(crate) fn weight(&self) -> usize {
        const BASE: usize = 48;
        BASE + match self {
            Item::User {
                text, attachments, ..
            } => text.len() + attachments.iter().map(|c| c.name.len() + 16).sum::<usize>(),
            Item::Agent { text, model, .. } => text.len() + model.as_ref().map_or(0, String::len),
            Item::Tool {
                id, name, summary, ..
            } => id.len() + name.len() + summary.len(),
            Item::Subagent { summary, .. } => summary.len(),
            Item::Error { text, .. } => text.len(),
            Item::Notice { kind, text, .. } => kind.len() + text.len(),
            Item::Thinking { text, .. } => text.as_ref().map_or(0, String::len),
            Item::Compaction { .. } => 0,
        }
    }
}

//! The plain types a chat view reads. No raw records and no `extra` ever travel in them.

use serde::{Deserialize, Serialize};

/// What the chat view shows of one session, at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest {
    /// Changes whenever the session file (opencode: the database or its `-wal`) does: a hash
    /// of length and modification time, below 2^53 so a JavaScript number carries it exactly.
    /// A page cursor (`Options::skip`) is only good for the revision it was taken at.
    pub revision: u64,
    /// The `seq` of the first (oldest) item returned, 0 when there are none.
    pub first_seq: u32,
    /// Older items exist: cut by `limit`, by the byte budget, or beyond the widest window read.
    pub has_older: bool,
    pub items: Vec<Item>,
    /// Records the parser could not read or could not match (a torn last line, a result whose
    /// call lies before the tail window).
    pub warnings: u32,
}

/// One row of the chat. A kind this build does not know (a newer daemon's) reads as
/// [`Item::Unknown`], so one new variant never makes a client drop the whole page: draw it as
/// one dim line. `seq` is the item's distance from the end of what the parser read: the newest
/// item of a revision is 0 and older ones count up, so the items of a page, oldest first, have
/// falling `seq`. It is the same number in every page of one `Digest::revision`, whatever
/// window served it. It is not an entry id and not a line number: those can be absent,
/// synthetic (`ord:n`) or cut in two at a tail start. It is not a cursor either: paging goes
/// by `Options::skip` and the revision. A new revision (the file grew) shifts every `seq` by
/// the items it added, so a client keys rows by what they are, never by `seq`.
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
        /// When the call started, in ms since the epoch, where the file records it (a client
        /// reads the wall clock span of a run of calls from it; calls can overlap).
        #[serde(default)]
        ts: Option<i64>,
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
    /// A row of a kind this build does not know. Never produced by the parser: it is what a
    /// client reads from a newer daemon. It carries nothing, not even a `seq`.
    #[serde(other)]
    Unknown,
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
    /// A status this build does not know (a newer daemon's); draw it as no status at all.
    #[serde(other)]
    Unknown,
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
            Item::Unknown => 0,
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
            Item::Unknown => {}
        }
    }

    /// Bytes the item adds to a response: its JSON, which is what travels. Counting the text
    /// alone undercounts a page of quotes and newlines by a fifth or more.
    pub(crate) fn weight(&self) -> usize {
        // One more for the comma between items.
        serde_json::to_vec(self).map_or(usize::MAX, |json| json.len() + 1)
    }
}

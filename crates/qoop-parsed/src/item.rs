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
        /// The agent has not taken this input yet: it was typed while the agent worked and
        /// waits in its queue. Absent from the wire when false; an older daemon never sends it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued: bool,
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
        /// Images and files the result carried (a screenshot, a read image): names and sizes,
        /// never bytes. Absent from the wire when there are none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Chip>,
        /// What the call is, whatever its name: the parser's tool kind (`shell`, `edit`,
        /// `question`, `plan`, `todo`, ...). Absent on an earlier result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// The agent's own words for the row where it gave some: a command's description (the
        /// `summary` stays the command), a question's answer or `Declined`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Lines the call added and removed, where its result records them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adds: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dels: Option<u64>,
    },
    Subagent {
        seq: u32,
        summary: String,
        /// Tool calls the child session made, read from its own session where it can be found
        /// (a spawn row only; 0 when it cannot, for a message to a child, and past the newest
        /// 32 spawns of a page).
        steps: u32,
        /// How the subagent call stands: `running` while its work goes on (a background agent
        /// included), `failed`, `interrupted`. An older daemon sends none, which reads as `ok`.
        #[serde(default)]
        status: Status,
        /// What the call did to the subagent: `spawn`, `message` or `wait`; absent when the
        /// tool's name does not say (the summary still opens with the verb, for older clients).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        act: Option<String>,
        /// The agent's own name (a named Claude agent, the one a message goes to, a Codex agent
        /// path's last part), else its type when that is not the generic one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The subagent's session id, for `Messages { child }`: the spawned child, or the child a
        /// message or a wait names. Only where the daemon can open it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<String>,
        /// The call's id, for [`detail`](crate::detail): the prompt and the report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// [`detail`](crate::detail) has a prompt or a report to show.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        has_detail: bool,
        /// Why the call could not start, on one line (an agent type that does not exist): only on a
        /// failed call that named no child and no run, since one that did started and failed later.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// A workflow's run id. A workflow row has no `act` and no `child`: its lanes do.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<String>,
        /// A workflow's agents, in the order they started.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lanes: Vec<Lane>,
    },
    Compaction {
        seq: u32,
        /// The first line of the summary the agent wrote, redacted and clamped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
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
        /// Who sent an `agent message`. `text` still opens with `<sender>: ` for older clients.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// A row of a kind this build does not know. Never produced by the parser: it is what a
    /// client reads from a newer daemon. It carries nothing, not even a `seq`.
    #[serde(other)]
    Unknown,
}

/// A file or image the user attached: its name and size, never its bytes. A local image also
/// carries the path the record names, so a client can fetch the picture itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chip {
    pub name: String,
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// No result yet.
    Running,
    #[default]
    Ok,
    Failed,
    /// No result, and the run went on or ended without one.
    Interrupted,
    /// Denied by the user or a permission rule.
    Rejected,
    /// No result yet, and the call waits: a Claude teammate that went idle until its next message
    /// (agentparse status detail `idle`). Not running. An older client reads it as `unknown`.
    Idle,
    /// A status this build does not know (a newer daemon's); draw it as no status at all.
    #[serde(other)]
    Unknown,
}

/// One agent of a workflow run. `child` opens its own chat (`Messages { child }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub label: String,
    pub status: LaneStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneStatus {
    Running,
    Done,
    /// The run ended before this agent reported.
    Stopped,
    /// The run recorded this agent as failed.
    Failed,
    /// A status this build does not know (a newer daemon's).
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
            | Item::Compaction { seq, .. }
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
            | Item::Compaction { seq, .. }
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

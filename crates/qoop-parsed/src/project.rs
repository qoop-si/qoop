//! Turning parsed entries into chat items.

use agentparse::{
    Agent, Entry, EntryKind, ErrorInfo, Media, Message, Origin, Part, ProcessAction, Role, Session,
    Tool, ToolCall, ToolResult, ToolStatus,
};

use std::collections::HashMap;

use crate::item::{Chip, Item, Status};
use crate::subagent::Spawn;
use crate::text;

/// Records the agents write for their own bookkeeping, not for the reader (seen in real
/// Claude and Codex sessions), and the `attachment.*` records Claude injects. They get no row.
/// Any other kind with no row of its own becomes a [`Item::Notice`] naming the kind, so a new
/// record type shows up instead of vanishing.
const BOOKKEEPING: &[&str] = &[
    "agent",
    "ai-title",
    "api-request",
    "api-request-blob",
    "api-request-shape",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "atis-latch",
    "bridge-session",
    "cost-state",
    "custom-title",
    "encrypted_content",
    "file-history-delta",
    "file-history-snapshot",
    "frame-link",
    "inter_agent_communication_metadata",
    "isolation-latch",
    "last-prompt",
    "mode",
    "pr-link",
    "queue-operation",
    "relocated",
    "step-finish",
    "step-start",
    "SubAgentActivity",
    "task_started",
    "todos",
    "token_count",
    "world_state",
    "worktree-state",
];

fn is_bookkeeping(kind: &str) -> bool {
    BOOKKEEPING.contains(&kind) || kind.starts_with("attachment.")
}

/// The name of a result whose call lies outside the window.
pub const ORPHAN: &str = "earlier result";

/// The name of a Codex code cell that recorded no calls of its own.
const CODE: &str = "code";

/// The most attachment chips one item carries; more are counted in a trailing `+N` chip.
const MAX_CHIPS: usize = 8;

/// The id a row carries and `detail` looks up: the call's own id, redacted and clamped like
/// every other text that leaves here.
pub fn item_id(raw: &str) -> String {
    text::summary(raw)
}

/// What the walk over a session carries besides the rows: the agent (compaction summaries
/// read differently) and the spawn rows, by index, with where their child session is.
struct Cx {
    agent: Agent,
    spawns: Vec<(usize, Spawn)>,
}

/// Every item of the session, oldest first, each numbered by its distance from the end of
/// what was parsed: the newest is 0. A wider tail window only adds older items in front, so a
/// number does not change from one page to the next. The map holds the spawn rows by `seq`;
/// their `steps` are 0 until [`crate::subagent::annotate`] fills the ones a page shows.
pub(crate) fn project(session: &Session) -> (Vec<Item>, HashMap<u32, Spawn>) {
    let mut cx = Cx {
        agent: session.agent,
        spawns: Vec::new(),
    };
    let mut out = Vec::new();
    for entry in &session.entries {
        entry_items(entry, &mut cx, &mut out);
    }
    let last = out.len().saturating_sub(1);
    for (i, item) in out.iter_mut().enumerate() {
        item.set_seq(seq_of(last, i));
    }
    let spawns = cx
        .spawns
        .into_iter()
        .map(|(i, spawn)| (seq_of(last, i), spawn))
        .collect();
    (out, spawns)
}

fn seq_of(last: usize, index: usize) -> u32 {
    u32::try_from(last - index).unwrap_or(u32::MAX)
}

fn entry_items(entry: &Entry, cx: &mut Cx, out: &mut Vec<Item>) {
    let ts = entry.ts;
    match &entry.kind {
        EntryKind::Message(m) => message_items(m, ts, cx, out),
        EntryKind::Compaction(c) => out.push(Item::Compaction {
            seq: 0,
            text: c
                .summary
                .as_deref()
                .and_then(|s| compaction_line(cx.agent, s)),
        }),
        EntryKind::Error(e) => out.push(Item::Error {
            seq: 0,
            text: text::text(&error_text(e)),
        }),
        EntryKind::Turn(t) => turn_item(t.status.as_deref(), t.error.as_deref(), out),
        EntryKind::Notice(n) => out.push(notice(n.kind.as_deref().unwrap_or("notice"), &n.text)),
        EntryKind::Rewind(_) => out.push(notice("rewind", "History was rolled back")),
        // Claude's pointer to the session it went on in: a row only when it names one.
        EntryKind::Other(o) if o.kind == "continued-in" => {
            let next = o.data.get("continuedInSessionId").and_then(|v| v.as_str());
            if next.is_some_and(|id| !id.is_empty()) {
                out.push(notice("continued-in", "Continued in a new session"));
            }
        }
        EntryKind::Other(o) if !is_bookkeeping(&o.kind) => {
            out.push(notice(&o.kind, "Record not shown here"));
        }
        // Settings and hook runs are the harness talking to itself; bookkeeping is above.
        EntryKind::Settings(_) | EntryKind::Hook(_) | EntryKind::Other(_) => {}
    }
}

/// What an error row says: the message, the HTTP status where the message does not already
/// name it (a 2xx is no error), and the retry attempt of a notice that the call is tried again.
fn error_text(e: &ErrorInfo) -> String {
    let mut line = e.message.trim().to_string();
    if line.is_empty() {
        line = "API error".into();
    }
    if let Some(status) = e.status.filter(|s| *s >= 400 && !names_status(&line, *s)) {
        line = format!("{line} (HTTP {status})");
    }
    if let Some(n) = e.retry {
        line = format!("{line}, retrying (attempt {n})");
    }
    line
}

/// The line carries the number as a word of its own (`(HTTP 429)`, `529 Overloaded`), not as
/// part of another number or an identifier.
fn names_status(line: &str, status: u16) -> bool {
    let status = status.to_string();
    line.split(|c: char| !c.is_alphanumeric())
        .any(|word| word == status)
}

fn turn_item(status: Option<&str>, error: Option<&str>, out: &mut Vec<Item>) {
    match (status, error) {
        // OpenCode writes a failed turn's error twice: as the error record and on the turn.
        (_, Some(e)) if repeats_last_error(e, out) => {}
        (_, Some(e)) => out.push(Item::Error {
            seq: 0,
            text: text::text(match e.trim() {
                "" => "Turn failed",
                shown => shown,
            }),
        }),
        (Some("interrupted"), None) => out.push(notice("interrupted", "Interrupted")),
        _ => {}
    }
}

/// The turn's error is the one the row just before it shows, said again: the same message
/// once the status and retry suffixes this file adds are taken off. An empty error never
/// matches, and neither does a row that was a retry notice: the failure after the last retry
/// is the news.
fn repeats_last_error(error: &str, out: &[Item]) -> bool {
    let error = text::text(error.trim());
    let Some(Item::Error { text: last, .. }) = out.last() else {
        return false;
    };
    let (base, retried) = strip_suffixes(last);
    !error.is_empty() && !retried && base == error
}

/// `line` without a `, retrying (attempt N)` tail and then a ` (HTTP N)` tail, and whether the
/// first was there.
fn strip_suffixes(line: &str) -> (&str, bool) {
    match strip_tail(line, ", retrying (attempt ") {
        Some(rest) => (strip_tail(rest, " (HTTP ").unwrap_or(rest), true),
        None => (strip_tail(line, " (HTTP ").unwrap_or(line), false),
    }
}

/// `line` up to the last `open` when the rest is a number and `)`.
fn strip_tail<'a>(line: &'a str, open: &str) -> Option<&'a str> {
    let at = line.rfind(open)?;
    let number = line[at + open.len()..].strip_suffix(')')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then(|| &line[..at])
}

/// The line a compaction row shows: the first one of the summary with words in it, clamped.
/// Claude opens with a fixed sentence ("This session is being continued...") and markers
/// (`Summary:`, `## ...`, a bare tag) that say nothing; OpenCode's summary opens with a
/// `## Goal` heading, which stays, followed by the line under it.
fn compaction_line(agent: Agent, summary: &str) -> Option<String> {
    const CONTINUED: &str = "This session is being continued";
    let mut lines = summary
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().any(char::is_alphabetic));
    let line = if agent == Agent::Opencode {
        let first = lines.next()?;
        match first.strip_prefix('#') {
            Some(heading) => {
                let heading = heading.trim_start_matches('#').trim();
                match lines.next() {
                    Some(next) => format!("{heading}: {next}"),
                    None => heading.to_string(),
                }
            }
            None => first.to_string(),
        }
    } else {
        lines
            .find(|l| {
                !l.starts_with(CONTINUED)
                    && !l.starts_with('#')
                    && !l.trim_end_matches(':').eq_ignore_ascii_case("summary")
                    && !(l.starts_with('<') && l.ends_with('>'))
            })?
            .to_string()
    };
    Some(text::short(&line, COMPACTION_MAX))
}

/// Characters of a compaction row's text.
const COMPACTION_MAX: usize = 160;

/// A record with no text of its own is named by its kind (`model_refusal_no_fallback` reads
/// `model refusal no fallback`).
fn notice(kind: &str, body: &str) -> Item {
    let body = match body.trim() {
        "" => kind.replace(['_', '-'], " "),
        shown => shown.to_string(),
    };
    Item::Notice {
        seq: 0,
        kind: text::summary(kind),
        text: text::text(&body),
    }
}

fn message_items(m: &Message, ts: Option<i64>, cx: &mut Cx, out: &mut Vec<Item>) {
    // Injected context (environment, instructions, reminders, skill bodies) is the harness's,
    // not the conversation's.
    if m.origin == Origin::Context {
        return;
    }
    let mut user_text = Vec::new();
    let mut chips = Vec::new();
    let mut thinking = Thinking::default();
    for part in &m.parts {
        if !matches!(part, Part::Reasoning { .. }) {
            thinking.flush(out);
        }
        match part {
            Part::Text { text } => match m.role {
                Role::Assistant => push_agent(m, ts, text, out),
                _ => user_text.push(text.as_str()),
            },
            Part::Reasoning { text, .. } => thinking.add(text.as_deref()),
            Part::Attachment(media) => chips.push(chip(media)),
            Part::ToolCall(call) => push_call(call, cx, out),
            Part::ToolResult(result) => out.extend(orphan(result)),
            Part::Other { kind, data } if kind == "fallback" => out.push(fallback(data)),
            Part::Other { kind, .. } if !is_bookkeeping(kind) => {
                out.push(notice(kind, "Block not shown here"));
            }
            Part::Other { .. } => {}
        }
    }
    thinking.flush(out);
    if !user_text.is_empty() || !chips.is_empty() {
        push_user_side(m, ts, &user_text.join("\n\n"), chips, out);
    }
}

/// Claude's `{"type":"fallback","from":{"model"},"to":{"model"}}` block: the request moved to
/// another model.
fn fallback(data: &serde_json::Value) -> Item {
    let model = |side: &str| data.pointer(&format!("/{side}/model"))?.as_str();
    let said = match (model("from"), model("to")) {
        (Some(from), Some(to)) => format!(
            "Switched from {} to {}",
            text::summary(from),
            text::summary(to)
        ),
        _ => "Switched model".to_string(),
    };
    notice("model fallback", &said)
}

/// Reasoning parts that follow each other are one row.
#[derive(Default)]
struct Thinking {
    text: String,
}

impl Thinking {
    fn add(&mut self, text: Option<&str>) {
        if let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) {
            if !self.text.is_empty() {
                self.text.push_str("\n\n");
            }
            self.text.push_str(t);
        }
    }

    fn flush(&mut self, out: &mut Vec<Item>) {
        // Reasoning that is encrypted or not kept has nothing to show, and no file records
        // how long it took (`ms` stays `None`): a row with neither is not a row.
        if !self.text.is_empty() {
            out.push(Item::Thinking {
                seq: 0,
                ms: None,
                text: Some(text::text(&self.text)),
            });
        }
        *self = Thinking::default();
    }
}

fn push_agent(m: &Message, ts: Option<i64>, body: &str, out: &mut Vec<Item>) {
    let body = body.trim();
    if !body.is_empty() {
        out.push(Item::Agent {
            seq: 0,
            ts,
            text: text::text(body),
            model: m.model.as_deref().map(text::summary),
        });
    }
}

/// Text on the user's side of the line: what a human typed, a slash command, or what the
/// harness handed the agent as input (command output, notifications, summaries).
fn push_user_side(m: &Message, ts: Option<i64>, body: &str, chips: Vec<Chip>, out: &mut Vec<Item>) {
    let kind = m.kind.as_deref().unwrap_or("");
    let shown = match (m.origin, kind) {
        (Origin::Command, "local-command-stdout") => {
            return push_notice_text("command output", &text::unwrap_leading(body, kind), out);
        }
        (Origin::Command, _) => text::command_line(body),
        (Origin::Human, _) => text::strip_wrappers(body),
        (Origin::Notification, _) => {
            let what = if kind.is_empty() {
                "notification"
            } else {
                kind
            };
            return push_notice_text(what, &text::notification_text(body), out);
        }
        (Origin::Summary, _) => return push_notice_text("summary", body, out),
        (Origin::Agent, _) => return push_agent_message(body, out),
        (_, _) => return push_notice_text("agent message", body, out),
    };
    if !shown.is_empty() || !chips.is_empty() {
        out.push(Item::User {
            seq: 0,
            ts,
            text: text::text(&shown),
            attachments: cap_chips(chips),
        });
    }
}

/// What another agent sent: one line, `<sender>: <first line of the payload>`, and nothing at
/// all for a message with no payload. Text that is not in that shape stays as it came.
fn push_agent_message(body: &str, out: &mut Vec<Item>) {
    let Some((sender, payload)) = text::agent_message(body) else {
        return push_notice_text("agent message", body, out);
    };
    if !payload.is_empty() {
        let line = format!(
            "{}: {}",
            text::short(&sender, 60),
            text::first_line(&payload, 120)
        );
        out.push(notice("agent message", &line));
    }
}

fn push_notice_text(kind: &str, body: &str, out: &mut Vec<Item>) {
    if !body.trim().is_empty() {
        out.push(notice(kind, body.trim()));
    }
}

fn chip(media: &Media) -> Chip {
    let from_url = media
        .url
        .as_deref()
        .filter(|u| plain_name(u))
        .and_then(|u| u.rsplit('/').next());
    let name = [media.name.as_deref(), from_url, media.mime.as_deref()]
        .into_iter()
        .flatten()
        .find(|n| plain_name(n))
        .unwrap_or("attachment");
    Chip {
        name: text::summary(name),
        bytes: media.bytes,
    }
}

/// A name a person gave a file, not a data URL or an encoded body that ended up where a name
/// goes.
fn plain_name(name: &str) -> bool {
    let encoded = name.len() >= 40
        && !name.contains('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b));
    !name.is_empty()
        && name.len() <= 120
        && !name.starts_with("data:")
        && !name.contains(";base64,")
        && !encoded
}

fn chips(media: &[Media]) -> Vec<Chip> {
    cap_chips(media.iter().map(chip).collect())
}

fn cap_chips(mut chips: Vec<Chip>) -> Vec<Chip> {
    if chips.len() > MAX_CHIPS {
        let more = chips.len() - MAX_CHIPS;
        chips.truncate(MAX_CHIPS);
        chips.push(Chip {
            name: format!("+{more}"),
            bytes: None,
        });
    }
    chips
}

fn push_call(call: &ToolCall, cx: &mut Cx, out: &mut Vec<Item>) {
    if let Tool::Task {
        description,
        prompt,
        ..
    } = &call.tool
    {
        let what = description
            .as_deref()
            .or(prompt.as_deref())
            .unwrap_or("subagent");
        if let Some(spawn) = Spawn::of(call) {
            cx.spawns.push((out.len(), spawn));
        }
        out.push(Item::Subagent {
            seq: 0,
            summary: text::summary(&format!("{}{what}", task_verb(&call.name))),
            steps: 0,
        });
        return;
    }
    out.extend(rows(call).iter().map(tool_item));
}

/// A call that starts a subagent (its child session has the steps).
pub(crate) fn is_spawn(call: &ToolCall) -> bool {
    matches!(call.tool, Tool::Task { .. }) && task_verb(&call.name) == SPAWNED
}

const SPAWNED: &str = "spawned: ";

/// What a subagent call did, from the tool's own name: Codex `spawn_agent`, `send_message`,
/// `followup_task`, `wait_agent`, Claude `Task` / `Agent` / `SendMessage`, OpenCode `task`.
/// Empty where the name does not say (`close_agent`, `list_agents`).
fn task_verb(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if name.contains("wait") {
        "waited for: "
    } else if name.contains("send") || name.contains("followup") {
        "sent: "
    } else if name.contains("spawn")
        || ["task", "agent", "subagent", "invoke_agent"].contains(&&*name)
    {
        SPAWNED
    } else {
        ""
    }
}

/// One tool row's source: a call that shows as a row, and the id the row and `detail` share.
pub(crate) struct Row<'a> {
    pub id: String,
    pub call: &'a ToolCall,
    /// A call made inside a Codex code cell.
    child: bool,
}

/// The rows of one call. A Codex code cell that recorded calls is not a row itself: its calls
/// are, in order, each under its own id (a child with none gets `<cell id>#<n>`), so the raw
/// JavaScript never shows. Every other call, a cell with no calls included, is one row.
pub(crate) fn rows(call: &ToolCall) -> Vec<Row<'_>> {
    if !matches!(call.tool, Tool::Code { .. }) || call.children.is_empty() {
        return vec![Row {
            id: item_id(&call.id),
            call,
            child: false,
        }];
    }
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for (n, child) in call.children.iter().enumerate() {
        let mut id = child.id.clone();
        if id.is_empty() {
            id = format!("{}#{n}", call.id);
        }
        if !seen.insert(id.clone()) {
            id = format!("{id}#{n}");
            seen.insert(id.clone());
        }
        rows.push(Row {
            id: item_id(&id),
            call: child,
            child: true,
        });
    }
    rows
}

fn tool_item(row: &Row) -> Item {
    let call = row.call;
    let shell = call.output.as_ref().and_then(|o| o.shell.as_ref());
    let output = output_text(call);
    Item::Tool {
        seq: 0,
        id: row.id.clone(),
        name: tool_name(call, row.child),
        summary: summary(call),
        status: status(call),
        exit_code: shell.and_then(|s| s.exit_code),
        duration_ms: shell.and_then(|s| s.duration_ms).or_else(|| elapsed(call)),
        ts: call.started_at,
        output_bytes: output.len() as u64,
        has_detail: !output.is_empty() || !input_text(call).is_empty(),
        attachments: call
            .output
            .as_ref()
            .map_or_else(Vec::new, |o| chips(&o.attachments)),
    }
}

/// A call made inside a code cell is named by what it is (`shell`, `process`, `patch`); the
/// cell itself, when it shows, is `code`.
fn tool_name(call: &ToolCall, child: bool) -> String {
    let named = match &call.tool {
        Tool::Code { .. } => CODE,
        _ if !child => return text::summary(&call.name),
        Tool::Shell(_) => "shell",
        Tool::Process { .. } => "process",
        Tool::Patch { .. } => "patch",
        Tool::Mcp { .. } => "mcp",
        Tool::Read { .. } => "read",
        Tool::WebSearch { .. } => "web search",
        Tool::WebFetch { .. } => "web fetch",
        _ if call.output.as_ref().is_some_and(|o| !o.files.is_empty()) => "patch",
        _ => return text::summary(&call.name),
    };
    named.into()
}

/// A result whose call lies outside the window: its first line, dim. One with nothing to say
/// gets no row.
fn orphan(result: &ToolResult) -> Option<Item> {
    let output = cell_output(&result.output.text);
    let summary = text::first_line(&output, text::SUMMARY_MAX);
    if summary.is_empty() && output.trim().is_empty() {
        return None;
    }
    Some(Item::Tool {
        seq: 0,
        id: item_id(&result.call_id),
        name: ORPHAN.into(),
        summary,
        status: map_status(result.status, true),
        exit_code: result.output.shell.as_ref().and_then(|s| s.exit_code),
        duration_ms: None,
        ts: None,
        output_bytes: output.len() as u64,
        has_detail: !output.is_empty(),
        attachments: chips(&result.output.attachments),
    })
}

/// What the call returned, as the agent saw it. A process or a command's own output is its
/// stdout and stderr; a code cell's is its text without the `Script completed` header and
/// the JSON envelopes around each chunk.
pub(crate) fn output_text(call: &ToolCall) -> String {
    let Some(o) = &call.output else {
        return String::new();
    };
    match (o.text.is_empty(), o.shell.as_ref()) {
        (true, Some(s)) => [s.stdout.as_deref(), s.stderr.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
        _ if matches!(call.tool, Tool::Code { .. }) => cell_output(&o.text),
        _ => o.text.clone(),
    }
}

/// A code cell's output text: `Script completed\nWall time 0.8 seconds\nOutput:\n` and then
/// one JSON object per chunk, `{"chunk_id","wall_time_seconds","exit_code","output"}`. The
/// header goes, each chunk becomes its `output`, and what is not JSON (a printed string, a cut
/// chunk) stays as it is. Text with no such header comes back unchanged.
pub(crate) fn cell_output(raw: &str) -> String {
    let Some(body) = raw
        .strip_prefix("Script ")
        .and_then(|r| r.split_once("\nOutput:"))
        .map(|(_, body)| body.strip_prefix('\n').unwrap_or(body))
    else {
        return raw.to_string();
    };
    let mut out = String::new();
    let mut chunks = serde_json::Deserializer::from_str(body).into_iter::<serde_json::Value>();
    let mut from = 0;
    loop {
        let next = chunks.next();
        let to = chunks.byte_offset();
        let piece = match next {
            None => break,
            Some(Ok(serde_json::Value::Object(o)))
                if o.contains_key("chunk_id") || o.contains_key("output") =>
            {
                out.push_str(o.get("output").and_then(|v| v.as_str()).unwrap_or(""));
                from = to;
                continue;
            }
            Some(Ok(serde_json::Value::Object(o))) if o.is_empty() => "",
            Some(Ok(_)) => body[from..to].trim(),
            Some(Err(_)) => {
                push_line(&mut out, body[from..].trim());
                break;
            }
        };
        push_line(&mut out, piece);
        from = to;
    }
    out
}

fn push_line(out: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(piece);
}

/// What was asked: the command, the process action, the script, the patch, the file written,
/// an edit's old and new text (each cut short), else the call's arguments.
pub(crate) fn input_text(call: &ToolCall) -> String {
    match &call.tool {
        Tool::Shell(s) => s.command.clone(),
        Tool::Process { input, .. } => match input.as_deref().filter(|i| !i.is_empty()) {
            Some(i) => format!("{}\n{i}", summary_raw(call)),
            None => summary_raw(call),
        },
        Tool::Patch { patch } => patch.clone(),
        Tool::Code { source } => source.clone(),
        Tool::Write { path, content } => format!("{path}\n{}", content.as_deref().unwrap_or("")),
        Tool::Edit { path, edits } => edits.iter().fold(path.clone(), |acc, e| {
            format!(
                "{acc}\n- {}\n+ {}",
                text::preview(&e.old, EDIT_PREVIEW),
                text::preview(&e.new, EDIT_PREVIEW)
            )
        }),
        _ if call.output.as_ref().is_some_and(|o| !o.files.is_empty()) => patched_files(call),
        _ => match &call.input {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        },
    }
}

/// Characters of an edit's old and of its new text that `detail` shows.
const EDIT_PREVIEW: usize = 600;

/// A call with no result is never `Ok`: it is running, or it was cut short.
fn status(call: &ToolCall) -> Status {
    map_status(call.status, call.output.is_some())
}

fn map_status(status: ToolStatus, has_result: bool) -> Status {
    match status {
        ToolStatus::Pending => Status::Running,
        ToolStatus::Ok if !has_result => Status::Running,
        ToolStatus::Ok => Status::Ok,
        ToolStatus::Error => Status::Failed,
        ToolStatus::Rejected => Status::Rejected,
        ToolStatus::Interrupted => Status::Interrupted,
    }
}

fn elapsed(call: &ToolCall) -> Option<u64> {
    let ms = call.ended_at? - call.started_at?;
    u64::try_from(ms).ok()
}

/// The one line that says what the call did.
fn summary(call: &ToolCall) -> String {
    text::summary(&summary_raw(call))
}

fn summary_raw(call: &ToolCall) -> String {
    let path = |p: &Option<String>| p.clone().unwrap_or_default();
    match &call.tool {
        Tool::Shell(s) => s.command.clone(),
        Tool::Process { id, action, .. } => {
            let verb = match action {
                ProcessAction::Read => "read",
                ProcessAction::Write => "write",
                ProcessAction::Kill => "kill",
            };
            format!("{verb} {}", path(id)).trim_end().to_string()
        }
        Tool::Read { path, .. } | Tool::Write { path, .. } | Tool::Edit { path, .. } => {
            path.clone()
        }
        Tool::Patch { .. } => patched_files(call),
        Tool::Search {
            pattern, path: p, ..
        } => join(pattern, p.as_deref()),
        Tool::Glob { pattern, path: p } => join(pattern, p.as_deref()),
        Tool::List { path: p } => path(p),
        Tool::WebFetch { url, .. } => path(url),
        Tool::WebSearch { query } => path(query),
        Tool::Todo { items } => format!("{} items", items.len()),
        Tool::Mcp { server, tool } => format!("{server}.{tool}"),
        // A cell that recorded no calls: the first line of its program, short.
        Tool::Code { source } => text::first_line(source, CODE_SUMMARY),
        Tool::Skill { name } => path(name),
        Tool::Other if call.output.as_ref().is_some_and(|o| !o.files.is_empty()) => {
            patched_files(call)
        }
        Tool::Task { .. } | Tool::Question | Tool::Plan | Tool::Other => String::new(),
    }
}

/// Characters of a code cell's first line that its row shows.
const CODE_SUMMARY: usize = 80;

fn join(pattern: &str, path: Option<&str>) -> String {
    match path {
        Some(p) => format!("{pattern} in {p}"),
        None => pattern.to_string(),
    }
}

fn patched_files(call: &ToolCall) -> String {
    let files = call.output.iter().flat_map(|o| &o.files);
    let paths = files.map(|f| f.path.as_str()).collect::<Vec<_>>();
    if paths.is_empty() {
        "apply patch".to_string()
    } else {
        paths.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_with_no_result_is_never_ok() {
        assert_eq!(map_status(ToolStatus::Ok, false), Status::Running);
        assert_eq!(map_status(ToolStatus::Ok, true), Status::Ok);
        assert_eq!(map_status(ToolStatus::Pending, true), Status::Running);
        assert_eq!(map_status(ToolStatus::Error, true), Status::Failed);
    }

    #[test]
    fn a_result_whose_call_is_before_the_window_is_an_orphan_row_and_a_warning() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let at = text.find("\"tool_use_id\": \"toolu_build\"").unwrap();
        let from_result = text.len() - (text[..at].rfind('\n').unwrap() + 1);
        let session = agentparse::parse_file_tail(&path, from_result as u64).unwrap();
        assert!(!session.warnings.is_empty(), "the parser counts it");
        let rows = project(&session).0;
        let Item::Tool {
            id,
            name,
            status,
            summary,
            ..
        } = &rows[0]
        else {
            panic!("first row")
        };
        assert_eq!(
            (id.as_str(), name.as_str(), *status),
            ("toolu_build", ORPHAN, Status::Failed)
        );
        assert!(summary.starts_with("Exit code 101"));
        assert!(rows[0].is_orphan() && !rows[1].is_orphan());
    }

    #[test]
    fn a_chip_is_named_by_a_file_name_or_the_mime_never_by_an_encoded_body() {
        let media = |name: Option<&str>, url: Option<&str>| Media {
            name: name.map(String::from),
            url: url.map(String::from),
            mime: Some("image/png".into()),
            bytes: Some(7),
        };
        let body = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg";
        assert_eq!(chip(&media(Some("shot.png"), None)).name, "shot.png");
        assert_eq!(
            chip(&media(None, Some("https://x.io/a/b.pdf"))).name,
            "b.pdf"
        );
        assert_eq!(
            chip(&media(Some(&format!("data:image/png;base64,{body}")), None)).name,
            "image/png"
        );
        assert_eq!(
            chip(&media(None, Some(&format!("data:image/png;base64,{body}")))).name,
            "image/png"
        );
        assert_eq!(chip(&media(Some(body), None)).name, "image/png");
        assert_eq!(chip(&media(Some("x;base64,AAAA"), None)).name, "image/png");
        let bare = Media {
            name: Some(body.into()),
            ..Media::default()
        };
        assert_eq!(chip(&bare).name, "attachment");
    }

    #[test]
    fn a_failed_turn_does_not_repeat_the_error_row_before_it() {
        let error = |text: &str| Item::Error {
            seq: 0,
            text: text.into(),
        };
        let mut out = vec![error("Rate limit exceeded. (HTTP 429)")];
        turn_item(Some("failed"), Some("Rate limit exceeded."), &mut out);
        assert_eq!(out.len(), 1, "same error, once");
        turn_item(Some("failed"), Some("Quota is gone."), &mut out);
        assert_eq!(out.len(), 2, "a different error still shows");
        let agent = Item::Agent {
            seq: 0,
            ts: None,
            text: "x".into(),
            model: None,
        };
        let mut out = vec![error("Boom"), agent];
        turn_item(Some("failed"), Some("Boom"), &mut out);
        assert_eq!(out.len(), 3, "an error after other rows is a new one");
    }

    #[test]
    fn the_failure_after_the_last_retry_shows_and_an_empty_error_is_not_a_repeat() {
        let error = |text: &str| Item::Error {
            seq: 0,
            text: text.into(),
        };
        let mut out = vec![error("API error, retrying (attempt 3)")];
        turn_item(Some("failed"), Some("API error"), &mut out);
        assert_eq!(out.len(), 2, "the final failure after a retry row");
        let mut out = vec![error("Boom (HTTP 500)")];
        turn_item(Some("failed"), Some(""), &mut out);
        assert_eq!(
            out.len(),
            2,
            "an empty error does not match, and says something"
        );
        assert!(matches!(&out[1], Item::Error { text, .. } if text == "Turn failed"));
        let mut out = vec![error("Boom (HTTP 500)")];
        turn_item(Some("failed"), Some("Boom"), &mut out);
        turn_item(Some("failed"), Some("Boo"), &mut out);
        assert_eq!(out.len(), 2, "equal, not a prefix");
    }

    #[test]
    fn a_status_is_named_only_as_a_word_of_its_own() {
        assert!(names_status("529 Overloaded", 529));
        assert!(names_status("Overloaded (HTTP 529)", 529));
        assert!(names_status("error: 429.", 429));
        assert!(!names_status("port 15290 refused", 529));
        assert!(!names_status("request req_4291 failed", 429));
        let info = ErrorInfo {
            message: "port 15290 refused".into(),
            status: Some(529),
            ..ErrorInfo::default()
        };
        assert_eq!(error_text(&info), "port 15290 refused (HTTP 529)");
    }

    #[test]
    fn a_compaction_line_by_agent() {
        let claude = |s| compaction_line(Agent::Claude, s);
        assert_eq!(
            claude("Summary:\n<summary>\nDid it."),
            Some("Did it.".into())
        );
        assert_eq!(claude("###\n  \n"), None);
        let opencode = |s| compaction_line(Agent::Opencode, s);
        assert_eq!(
            opencode("## Goal\nFix the build."),
            Some("Goal: Fix the build.".into())
        );
        assert_eq!(opencode("## Goal"), Some("Goal".into()));
        assert_eq!(opencode("Plain start\nmore"), Some("Plain start".into()));
    }

    #[test]
    fn an_error_names_a_status_once_and_never_a_success() {
        let info = |message: &str, status: Option<u16>| ErrorInfo {
            message: message.into(),
            status,
            ..ErrorInfo::default()
        };
        assert_eq!(
            error_text(&info("Overloaded", Some(529))),
            "Overloaded (HTTP 529)"
        );
        assert_eq!(
            error_text(&info("529 Overloaded", Some(529))),
            "529 Overloaded"
        );
        assert_eq!(error_text(&info("dropped", Some(200))), "dropped");
        assert_eq!(error_text(&info("", None)), "API error");
    }

    #[test]
    fn opencode_todo_snapshots_are_bookkeeping() {
        assert!(is_bookkeeping("todos") && is_bookkeeping("api-request-shape"));
        assert!(!is_bookkeeping("continued-in") && !is_bookkeeping("synthetic"));
    }

    #[test]
    fn more_than_eight_chips_end_in_a_count() {
        let chips = |n: usize| {
            (0..n)
                .map(|i| Chip {
                    name: format!("f{i}"),
                    bytes: None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(cap_chips(chips(8)).len(), 8);
        let capped = cap_chips(chips(11));
        assert_eq!(capped.len(), 9);
        assert_eq!(
            capped[8],
            Chip {
                name: "+3".into(),
                bytes: None
            }
        );
    }
}

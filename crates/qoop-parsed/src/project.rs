//! Turning parsed entries into chat items.

use agentparse::{
    Agent, Entry, EntryKind, ErrorInfo, Hook, Media, Message, Origin, Part, ProcessAction, Role,
    Session, TodoItem, Tool, ToolCall, ToolOutput, ToolResult, ToolStatus, Turn,
};

use std::collections::{HashMap, HashSet};
use std::path::Path;

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
    "fork-context-ref",
    "frame-link",
    "inter_agent_communication_metadata",
    "isolation-latch",
    "last-prompt",
    "mode",
    "plan_update",
    "pr-link",
    "queue-operation",
    "relocated",
    "step-finish",
    "step-start",
    "SubAgentActivity",
    "task_started",
    "todos",
    "token_count",
    "turn_diff",
    "web_search_end",
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
/// read differently), the spawn rows, by index, with where their child session is, the ids of
/// the entries a rewind rolled back, and where the newest turn record sits (only it says what
/// still runs in the background).
struct Cx {
    agent: Agent,
    spawns: Vec<(usize, Spawn)>,
    rolled_back: HashSet<String>,
    newest_turn: Option<usize>,
    /// See `Options::alive_since`.
    since: Option<i64>,
}

/// Every item of the session, oldest first, each numbered by its distance from the end of
/// what was parsed: the newest is 0. A wider tail window only adds older items in front, so a
/// number does not change from one page to the next. The map holds the spawn rows by `seq`;
/// their `steps` are 0 until [`crate::subagent::annotate`] fills the ones a page shows.
pub(crate) fn project(
    session: &Session,
    alive_since: Option<i64>,
) -> (Vec<Item>, HashMap<u32, Spawn>) {
    let mut cx = Cx {
        agent: session.agent,
        spawns: Vec::new(),
        rolled_back: session
            .entries
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Rewind(r) => Some(r.dropped.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect(),
        newest_turn: session
            .entries
            .iter()
            .rposition(|e| matches!(e.kind, EntryKind::Turn(_))),
        since: alive_since,
    };
    let mut out = Vec::new();
    for (at, entry) in session.entries.iter().enumerate() {
        entry_items(entry, at, &mut cx, &mut out);
    }
    let last = out.len().saturating_sub(1);
    for (i, item) in out.iter_mut().enumerate() {
        item.set_seq(seq_of(last, i));
        // Capped only now: a later record can still name the files of the chips.
        if let Item::User { attachments, .. } = item {
            *attachments = cap_chips(std::mem::take(attachments));
        }
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

fn entry_items(entry: &Entry, at: usize, cx: &mut Cx, out: &mut Vec<Item>) {
    let ts = entry.ts;
    // What a rewind rolled back is no longer part of the conversation.
    if entry
        .id
        .as_ref()
        .is_some_and(|id| cx.rolled_back.contains(id))
    {
        return;
    }
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
        EntryKind::Turn(t) => {
            turn_item(t.status.as_deref(), t.error.as_deref(), out);
            if cx.newest_turn == Some(at) && !before(ts, cx.since) {
                out.extend(background_notice(t));
            }
        }
        // A record with no text and no kind says nothing (Gemini writes an empty `info` for a
        // compression); the level names the kind where the record has none.
        EntryKind::Notice(n) if n.text.trim().is_empty() && n.kind.is_none() => {}
        EntryKind::Notice(n) => out.push(notice(
            n.kind.as_deref().or(n.level.as_deref()).unwrap_or("notice"),
            &n.text,
        )),
        EntryKind::Hook(h) => out.extend(hook_notice(h, out)),
        EntryKind::Rewind(_) => out.push(notice("rewind", "History was rolled back")),
        // Claude's pointer to the session it went on in: a row only when it names one.
        EntryKind::Other(o) if o.kind == "continued-in" => {
            let next = o.data.get("continuedInSessionId").and_then(|v| v.as_str());
            if next.is_some_and(|id| !id.is_empty()) {
                out.push(notice("continued-in", "Continued in a new session"));
            }
        }
        EntryKind::Other(o) if !is_bookkeeping(&o.kind) => {
            let said = own_text(&o.data).unwrap_or_else(|| "Record not shown here".into());
            out.push(notice(&o.kind, &said));
        }
        // Settings are the harness talking to itself; bookkeeping is above.
        EntryKind::Settings(_) | EntryKind::Other(_) => {}
    }
}

/// What a record of a kind this build has no row for says itself: its first message, text,
/// command or reason, on one line.
fn own_text(data: &serde_json::Value) -> Option<String> {
    ["message", "text", "content", "summary", "command", "reason"]
        .iter()
        .filter_map(|key| data.get(key)?.as_str())
        .map(|s| text::first_line(s, text::SUMMARY_MAX))
        .find(|s| !s.is_empty())
}

/// The one quiet row that says background work is still going: from the newest turn record,
/// and only while it counts any.
fn background_notice(turn: &Turn) -> Option<Item> {
    let count = |n: Option<u32>, what: &str| {
        let n = n.filter(|n| *n > 0)?;
        Some(format!(
            "{n} background {what}{}",
            if n == 1 { "" } else { "s" }
        ))
    };
    let counts: Vec<String> = [
        count(turn.pending_agents, "agent"),
        count(turn.pending_workflows, "workflow"),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!counts.is_empty()).then(|| {
        notice(
            "background",
            &format!("{} still running", counts.join(" and ")),
        )
    })
}

/// A hook run is silent while it went well. One that blocked, exited non-zero or wrote to
/// stderr is a row: the event, what it did, and the first line it said. A block the agent logs
/// twice (the blocking attachment, then the stop summary that carries the same error) is one.
fn hook_notice(h: &Hook, out: &[Item]) -> Option<Item> {
    let failed = h.exit_code.is_some_and(|code| code != 0);
    let line = |s: &Option<String>| {
        s.as_deref()
            .map(|s| text::first_line(s, text::SUMMARY_MAX))
            .filter(|l| !l.is_empty())
    };
    let said = line(&h.stderr);
    if !(h.blocked || failed || said.is_some()) {
        return None;
    }
    let name = h.event.as_deref().or(h.name.as_deref()).unwrap_or("hook");
    // A hook that exited 0 and still wrote to stderr is not in error: it said something.
    let did = match (h.blocked, h.exit_code) {
        (true, _) => "blocked",
        (false, Some(0)) => "said",
        (false, _) => "error",
    };
    let said = said
        .or_else(|| line(&h.stdout))
        .or_else(|| h.exit_code.filter(|c| *c != 0).map(|c| format!("exit {c}")));
    let head = format!("{name} hook {did}");
    let repeats = said.is_some()
        && matches!(out.last(), Some(Item::Notice { kind, text, .. })
            if kind == "hook"
                && text.starts_with(&format!("{name} hook "))
                && text.split_once(": ").map(|(_, last)| last) == said.as_deref());
    let body = match &said {
        Some(said) => format!("{head}: {said}"),
        None => head,
    };
    (!repeats).then(|| notice("hook", &body))
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
/// `## Goal` heading, which stays, followed by the line under it. Claude's sections are
/// numbered (`1. Primary Request and Intent:`), so those headings say nothing either, and
/// Gemini's `<state_snapshot>` opens with the `<overall_goal>` text.
fn compaction_line(agent: Agent, summary: &str) -> Option<String> {
    const CONTINUED: &str = "This session is being continued";
    let goal = text::inner_of(summary, "overall_goal");
    let summary = goal.as_deref().unwrap_or(summary);
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
                    && !is_section_heading(l)
                    && !l.trim_end_matches(':').eq_ignore_ascii_case("summary")
                    && !(l.starts_with('<') && l.ends_with('>'))
            })?
            .to_string()
    };
    Some(text::short(&line, COMPACTION_MAX))
}

/// Characters of a compaction row's text.
const COMPACTION_MAX: usize = 160;

/// `1. Primary Request and Intent:`, `1. **Primary Request and Intent**` or `B) Files:`: a
/// numbered or lettered line that ends in a colon, or is bold all through, opens a section, it
/// is not what the section says.
fn is_section_heading(line: &str) -> bool {
    let Some((mark, rest)) = line.split_once(['.', ')']) else {
        return false;
    };
    let numbered = !mark.is_empty() && mark.bytes().all(|b| b.is_ascii_digit());
    let lettered = mark.len() == 1 && mark.bytes().all(|b| b.is_ascii_alphabetic());
    let rest = rest.trim();
    let bold = rest.len() > 4 && rest.starts_with("**") && rest.ends_with("**");
    (numbered || lettered) && (rest.ends_with(':') || bold)
}

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
    // not the conversation's, and so is Gemini's "Got it" after a compression. The one thing
    // it holds that the chat wants is where an uploaded image lives.
    if m.origin == Origin::Context {
        return attach_sources(m, out);
    }
    if m.kind.as_deref() == Some("compaction_ack") {
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
            Part::ToolCall(call) => push_call(call, ts, cx, out),
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
            attachments: chips,
            queued: m.phase.as_deref() == Some("queued"),
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
        path: local_image(media),
    }
}

/// `[Image: source: <path>]`, the text part Claude Code writes after an uploaded image block.
fn image_source(text: &str) -> Option<String> {
    let path = text
        .trim()
        .strip_prefix("[Image: source: ")?
        .strip_suffix(']')?;
    image_path(path)
}

/// Claude writes where an uploaded image lives as an injected record after the prompt that
/// holds it, one `[Image: source: <path>]` text per image. Each path goes to the next image
/// chip, without a path, of the latest sent user row (the row's chips are not capped yet).
fn attach_sources(m: &Message, out: &mut [Item]) {
    let paths = m.parts.iter().filter_map(|p| match p {
        Part::Text { text } => image_source(text),
        _ => None,
    });
    let Some(Item::User { attachments, .. }) = out
        .iter_mut()
        .rev()
        .find(|i| matches!(i, Item::User { queued: false, .. }))
    else {
        return;
    };
    let open = attachments
        .iter_mut()
        .filter(|c| c.path.is_none() && c.name.starts_with("image/"));
    for (chip, path) in open.zip(paths) {
        chip.path = Some(path);
    }
}

/// The local file of an image: an absolute path or a `file://` URL the record holds, never a
/// remote or `data:` URL.
fn local_image(media: &Media) -> Option<String> {
    let url = media.url.as_deref()?;
    match url.strip_prefix("file://") {
        Some(rest) => image_path(&percent_decode(rest)?),
        None => image_path(url),
    }
}

/// What the daemon's `ShowOpen` can open for the phone: an absolute path with an image
/// extension (`IMAGES` in `qoop-runtime/src/shows.rs`, keep in step). A path is sent as it is
/// recorded, so one over `PATH_MOST` bytes or with a control character is dropped, not cut.
fn image_path(path: &str) -> Option<String> {
    let image = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    let clean = path.len() <= PATH_MOST && !path.chars().any(char::is_control);
    (Path::new(path).is_absolute() && image && clean).then(|| path.to_string())
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "heic"];

/// The longest path kept on a chip (the usual `PATH_MAX` of Linux and macOS).
const PATH_MOST: usize = 4096;

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b != b'%' {
            out.push(b);
            continue;
        }
        let hex = [bytes.next()?, bytes.next()?];
        out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
    }
    String::from_utf8(out).ok()
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
            path: None,
        });
    }
    chips
}

fn push_call(call: &ToolCall, ts: Option<i64>, cx: &mut Cx, out: &mut Vec<Item>) {
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
        if let Some(spawn) = Spawn::of(call, cx.agent) {
            cx.spawns.push((out.len(), spawn));
        }
        out.push(Item::Subagent {
            seq: 0,
            summary: text::summary(&format!("{}{what}", verb_of(call, cx.agent))),
            steps: 0,
            status: live_status(call, ts, cx.since),
        });
        return;
    }
    out.extend(rows(call).iter().map(|row| tool_item(row, ts, cx.since)));
}

/// A call that starts a subagent (its child session has the steps).
pub(crate) fn is_spawn(call: &ToolCall, agent: Agent) -> bool {
    matches!(call.tool, Tool::Task { .. }) && verb_of(call, agent) == SPAWNED
}

/// [`task_verb`] of the call's name; Gemini starts a subagent by calling the agent by its own
/// name (`codebase_investigator`), so a call it linked to a child session that no verb names is
/// a spawn.
fn verb_of(call: &ToolCall, agent: Agent) -> &'static str {
    match task_verb(&call.name) {
        "" if agent == Agent::Gemini && call.child_session.is_some() => SPAWNED,
        verb => verb,
    }
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

fn tool_item(row: &Row, entry_ts: Option<i64>, since: Option<i64>) -> Item {
    let call = row.call;
    let shell = call.output.as_ref().and_then(|o| o.shell.as_ref());
    let output = output_text(call);
    Item::Tool {
        seq: 0,
        id: row.id.clone(),
        name: tool_name(call, row.child),
        summary: summary(call),
        status: live_status(call, entry_ts, since),
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
    let output = orphan_output(result);
    // Output that is only escape codes says nothing.
    if text::preview(&output, text::SUMMARY_MAX).trim().is_empty() {
        return None;
    }
    let command = result.output.data.as_ref().and_then(command_of);
    let summary = match command {
        Some(c) if !c.trim().is_empty() => text::summary(&c),
        _ => text::first_line(&output, text::SUMMARY_MAX),
    };
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

/// The command a result's data keeps: a string, or an argv, where `[sh|bash|zsh, -c|-lc, script]`
/// is the script and any other argv is read as one command line.
fn command_of(data: &serde_json::Value) -> Option<String> {
    match data.get("command")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(argv) => {
            let argv: Vec<&str> = argv.iter().filter_map(|a| a.as_str()).collect();
            let shell = |name: &str| matches!(name.rsplit('/').next(), Some("sh" | "bash" | "zsh"));
            match argv.as_slice() {
                [name, "-c" | "-lc", script] if shell(name) => Some((*script).to_string()),
                [] => None,
                _ => Some(argv.join(" ")),
            }
        }
        _ => None,
    }
}

/// What the call returned, as the agent saw it. A process or a command's own output is its
/// stdout and stderr; a code cell's is its text without the `Script completed` header and
/// the JSON envelopes around each chunk.
pub(crate) fn output_text(call: &ToolCall) -> String {
    let Some(o) = &call.output else {
        return String::new();
    };
    match streams(o) {
        Some(streams) => streams,
        None if matches!(call.tool, Tool::Code { .. }) => cell_output(&o.text),
        None => o.text.clone(),
    }
}

/// What a result whose call lies outside the window says: its text, else the output of the
/// command it came from.
pub(crate) fn orphan_output(result: &ToolResult) -> String {
    streams(&result.output).unwrap_or_else(|| cell_output(&result.output.text))
}

/// A command's own stdout and stderr, for a result that has no text of its own.
fn streams(o: &ToolOutput) -> Option<String> {
    let shell = o.shell.as_ref().filter(|_| o.text.is_empty())?;
    Some(
        [shell.stdout.as_deref(), shell.stderr.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
    )
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
        Tool::Todo { items } => checklist(items),
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

/// `at` is before `since`: both known, and it came first.
fn before(at: Option<i64>, since: Option<i64>) -> bool {
    matches!((at, since), (Some(at), Some(since)) if at < since)
}

/// [`status`], except that a call still running which began before the agent process that now
/// runs the session started was left behind by one that is gone: it is `interrupted`. The call's
/// own start counts, else the time of the message that made it.
fn live_status(call: &ToolCall, entry_ts: Option<i64>, since: Option<i64>) -> Status {
    match status(call) {
        Status::Running if before(call.started_at.or(entry_ts), since) => Status::Interrupted,
        status => status,
    }
}

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
        Tool::Todo { items } => todo_line(items),
        Tool::Mcp { server, tool } => format!("{server}.{tool}"),
        // A cell that recorded no calls: the first line of its program, short.
        Tool::Code { source } => text::first_line(source, CODE_SUMMARY),
        Tool::Skill { name } => path(name),
        Tool::Other if call.output.as_ref().is_some_and(|o| !o.files.is_empty()) => {
            patched_files(call)
        }
        Tool::Question => question_line(call),
        Tool::Plan => plan_line(call),
        Tool::Other => first_field(&call.input, OTHER_FIELDS),
        Tool::Task { .. } => String::new(),
    }
}

/// The fields of a tool this build has no view of that say what it was for, in order.
const OTHER_FIELDS: &[&str] = &[
    "subject",
    "description",
    "reason",
    "query",
    "objective",
    "skill",
];

/// The first of `keys` that holds text in a JSON object input.
fn first_field(input: &serde_json::Value, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| input.get(key)?.as_str())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The first question asked, and the answer the result gives to it (`User has answered your
/// questions: "<question>"="<answer>"`) once there is one.
fn question_line(call: &ToolCall) -> String {
    let asked = call.input.pointer("/questions/0/question");
    let asked = asked.or_else(|| call.input.get("question"));
    let Some(question) = asked.and_then(|q| q.as_str()).map(str::trim) else {
        return String::new();
    };
    let answer = call
        .output
        .as_ref()
        .and_then(|o| o.text.split_once(&format!("\"{question}\"=\"")))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(answer, _)| answer.trim())
        .filter(|a| !a.is_empty());
    match answer {
        Some(answer) => format!("{question} {answer}"),
        None => question.to_string(),
    }
}

/// The title of the plan put up for approval: its first heading, else its first line.
fn plan_line(call: &ToolCall) -> String {
    let plan = call
        .input
        .get("plan")
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let lines = || plan.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines()
        .find(|l| l.starts_with('#'))
        .or_else(|| lines().next());
    line.map_or_else(String::new, |l| {
        l.trim_start_matches('#').trim().to_string()
    })
}

/// `2 of 5 done: <the item in progress>`; the item's own text for a list of one with no state
/// (a created task), the count for a list that keeps no state at all.
fn todo_line(items: &[TodoItem]) -> String {
    fn state(item: &TodoItem) -> &str {
        item.status.as_deref().unwrap_or("")
    }
    if items.iter().all(|i| state(i).is_empty()) {
        return match items {
            [one] => one.content.clone(),
            _ => format!("{} items", items.len()),
        };
    }
    let done = items
        .iter()
        .filter(|i| matches!(state(i), "completed" | "done"))
        .count();
    let head = format!("{done} of {} done", items.len());
    match items.iter().find(|i| state(i) == "in_progress") {
        Some(now) => format!("{head}: {}", now.content),
        None => head,
    }
}

/// The list as a checklist, one line per item.
fn checklist(items: &[TodoItem]) -> String {
    let mark = |item: &TodoItem| match item.status.as_deref() {
        Some("completed" | "done") => "[x]",
        Some("in_progress") => "[~]",
        Some("cancelled") => "[-]",
        _ => "[ ]",
    };
    let lines: Vec<String> = items
        .iter()
        .map(|i| format!("{} {}", mark(i), i.content))
        .collect();
    lines.join("\n")
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

/// A session of no entries, for the tests of this module and of `subagent`.
#[cfg(test)]
pub(crate) fn blank_session(agent: Agent, path: &std::path::Path) -> Session {
    Session {
        agent,
        id: "sid".into(),
        source: agentparse::Source::File { path: path.into() },
        format: "test".into(),
        parent: None,
        title: None,
        cwd: None,
        git: None,
        cli_version: None,
        models: Vec::new(),
        started_at: None,
        updated_at: None,
        usage: Default::default(),
        entries: Vec::new(),
        extra: None,
        warnings: Vec::new(),
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
        let rows = project(&session, None).0;
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

    fn picture(mime: Option<&str>, url: Option<&str>) -> Media {
        Media {
            mime: mime.map(String::from),
            url: url.map(String::from),
            ..Media::default()
        }
    }

    #[test]
    fn a_chip_carries_the_local_path_of_an_image_and_nothing_else() {
        let path = |m: Media| chip(&m).path;
        // Codex `local_image` (a bare path, no type), OpenCode file part (file URL, encoded).
        assert_eq!(
            path(picture(None, Some("/tmp/up/a.PNG"))).as_deref(),
            Some("/tmp/up/a.PNG")
        );
        assert_eq!(
            path(picture(
                Some("image/png"),
                Some("file:///tmp/my%20shot.png")
            ))
            .as_deref(),
            Some("/tmp/my shot.png")
        );
        // Remote, relative, not an image, broken encoding and no URL at all have no path.
        for m in [
            picture(Some("image/png"), Some("https://x.test/a.png")),
            picture(Some("image/png"), Some("shots/a.png")),
            picture(Some("application/pdf"), Some("/tmp/a.pdf")),
            picture(Some("image/png"), Some("file:///tmp/a%2")),
            picture(Some("image/png"), None),
            // The daemon cannot open these as pictures, whatever the type says.
            picture(Some("image/bmp"), Some("/tmp/a.bmp")),
            picture(Some("image/heif"), Some("/tmp/a.heif")),
            picture(Some("image/png"), Some("/tmp/no-extension")),
            // Over 4096 bytes or holding a control character: dropped, not cut.
            picture(None, Some(&format!("/{}.png", "a".repeat(4096)))),
            picture(None, Some("file:///tmp/a%0Ab.png")),
            picture(None, Some("/tmp/a\u{1b}[31m.png")),
        ] {
            assert_eq!(path(m.clone()), None, "{m:?}");
        }
        let longest = format!("/{}.png", "a".repeat(4096 - 5));
        assert_eq!(longest.len(), 4096);
        assert_eq!(path(picture(None, Some(&longest))), Some(longest));
    }

    #[test]
    fn a_claude_image_source_record_gives_its_paths_to_the_prompt_before_it_and_is_no_row() {
        let image = Media {
            mime: Some("image/jpeg".into()),
            bytes: Some(9),
            ..Media::default()
        };
        let mut prompt = human("look [Image #1] [Image #2]");
        prompt
            .parts
            .extend([Part::Attachment(image.clone()), Part::Attachment(image)]);
        let text = |t: &str| Part::Text { text: t.into() };
        let mut source = message(Role::User, Origin::Context, Some("meta"), "");
        source.parts = vec![
            text("[Image: source: /var/up/one.jpg]"),
            text("[Image: source: /var/up/two.jpg]"),
            text("[Image: source: relative.jpg]"),
        ];
        let stray = message(
            Role::User,
            Origin::Context,
            Some("meta"),
            "[Image: source: /x.png]",
        );
        let rows = rows_of(&session(
            Agent::Claude,
            vec![
                EntryKind::Message(stray.clone()),
                EntryKind::Message(prompt),
                EntryKind::Message(source),
                EntryKind::Message(stray),
            ],
        ));
        let [
            Item::User {
                text, attachments, ..
            },
        ] = rows.as_slice()
        else {
            panic!("{rows:?}")
        };
        assert_eq!(text, "look [Image #1] [Image #2]");
        let paths: Vec<_> = attachments.iter().map(|c| c.path.as_deref()).collect();
        assert_eq!(paths, [Some("/var/up/one.jpg"), Some("/var/up/two.jpg")]);
    }

    fn sources(paths: &[&str]) -> EntryKind {
        let mut m = message(Role::User, Origin::Context, Some("meta"), "");
        m.parts = paths
            .iter()
            .map(|p| Part::Text {
                text: format!("[Image: source: {p}]"),
            })
            .collect();
        EntryKind::Message(m)
    }

    fn prompt_with(attachments: Vec<Part>, phase: Option<&str>) -> EntryKind {
        let mut m = human("look");
        m.phase = phase.map(String::from);
        m.parts.extend(attachments);
        EntryKind::Message(m)
    }

    fn image() -> Part {
        Part::Attachment(Media {
            mime: Some("image/png".into()),
            bytes: Some(9),
            ..Media::default()
        })
    }

    fn chip_paths(rows: &[Item]) -> Vec<Vec<Option<&str>>> {
        rows.iter()
            .filter_map(|i| match i {
                Item::User { attachments, .. } => {
                    Some(attachments.iter().map(|c| c.path.as_deref()).collect())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_source_path_goes_to_an_image_chip_never_to_a_document_ahead_of_it() {
        let document = Part::Attachment(Media {
            mime: Some("application/pdf".into()),
            bytes: Some(5),
            ..Media::default()
        });
        let rows = rows_of(&session(
            Agent::Claude,
            vec![
                prompt_with(vec![document, image()], None),
                sources(&["/up/a.png"]),
            ],
        ));
        assert_eq!(chip_paths(&rows), [[None, Some("/up/a.png")]]);
    }

    #[test]
    fn a_source_path_skips_a_queued_row_and_is_dropped_when_not_an_image_path() {
        let rows = rows_of(&session(
            Agent::Claude,
            vec![
                prompt_with(vec![image()], None),
                prompt_with(vec![image()], Some("queued")),
                sources(&["/up/a.png"]),
                sources(&["/up/b.bmp", "/up/c\u{7}.png"]),
            ],
        ));
        assert_eq!(chip_paths(&rows), [[Some("/up/a.png")], [None]]);
    }

    #[test]
    fn more_than_eight_chips_end_in_a_count() {
        let chips = |n: usize| {
            (0..n)
                .map(|i| Chip {
                    name: format!("f{i}"),
                    bytes: None,
                    path: None,
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
                bytes: None,
                path: None
            }
        );
    }

    // Hand-built sessions: what the projection does with each shape, whatever parser made it.

    fn entry(id: &str, kind: EntryKind) -> Entry {
        Entry {
            id: Some(id.into()),
            parent_id: None,
            turn_id: None,
            ts: None,
            line: None,
            extra: None,
            kind,
        }
    }

    fn session(agent: Agent, kinds: Vec<EntryKind>) -> Session {
        let mut session = blank_session(agent, std::path::Path::new("/x/chats/session-a.jsonl"));
        session.entries = kinds
            .into_iter()
            .enumerate()
            .map(|(n, kind)| entry(&format!("e{n}"), kind))
            .collect();
        session
    }

    fn message(role: Role, origin: Origin, kind: Option<&str>, body: &str) -> Message {
        Message {
            role,
            origin,
            kind: kind.map(String::from),
            author: None,
            model: None,
            agent: None,
            phase: None,
            parts: vec![Part::Text { text: body.into() }],
            usage: None,
            stop_reason: None,
        }
    }

    fn human(body: &str) -> Message {
        message(Role::User, Origin::Human, None, body)
    }

    fn call(name: &str, tool: Tool, input: serde_json::Value, status: ToolStatus) -> ToolCall {
        ToolCall {
            id: format!("id-{name}"),
            name: name.into(),
            input,
            tool,
            status,
            status_detail: None,
            output: None,
            started_at: None,
            ended_at: None,
            child_session: None,
            children: Vec::new(),
        }
    }

    fn with_text(mut call: ToolCall, text: &str) -> ToolCall {
        call.output = Some(ToolOutput {
            text: text.into(),
            ..ToolOutput::default()
        });
        call
    }

    fn call_message(call: ToolCall) -> EntryKind {
        let mut m = message(Role::Assistant, Origin::Model, None, "");
        m.parts = vec![Part::ToolCall(call)];
        EntryKind::Message(m)
    }

    fn rows_of(session: &Session) -> Vec<Item> {
        project(session, None).0
    }

    fn notices(items: &[Item]) -> Vec<(&str, &str)> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Notice { kind, text, .. } => Some((kind.as_str(), text.as_str())),
                _ => None,
            })
            .collect()
    }

    fn turn(agents: Option<u32>, workflows: Option<u32>) -> EntryKind {
        EntryKind::Turn(Turn {
            pending_agents: agents,
            pending_workflows: workflows,
            ..Turn::default()
        })
    }

    #[test]
    fn a_subagent_row_carries_how_its_call_stands() {
        let task = |status| {
            let tool = Tool::Task {
                description: Some("survey".into()),
                prompt: None,
                agent_type: None,
            };
            let call = call("Agent", tool, serde_json::Value::Null, status);
            if status == ToolStatus::Pending {
                call
            } else {
                with_text(call, "done")
            }
        };
        let status_of = |status| {
            let items = rows_of(&session(Agent::Claude, vec![call_message(task(status))]));
            match &items[0] {
                Item::Subagent { status, .. } => *status,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(status_of(ToolStatus::Pending), Status::Running);
        assert_eq!(status_of(ToolStatus::Ok), Status::Ok);
        assert_eq!(status_of(ToolStatus::Error), Status::Failed);
        assert_eq!(status_of(ToolStatus::Interrupted), Status::Interrupted);
    }

    #[test]
    fn input_still_in_the_queue_is_marked_and_delivered_input_is_not() {
        let mut waiting = human("do this next");
        waiting.phase = Some("queued".into());
        let items = rows_of(&session(
            Agent::Claude,
            vec![
                EntryKind::Message(human("first")),
                EntryKind::Message(waiting),
            ],
        ));
        let queued: Vec<_> = items
            .iter()
            .filter_map(|i| match i {
                Item::User { text, queued, .. } => Some((text.as_str(), *queued)),
                _ => None,
            })
            .collect();
        assert_eq!(queued, [("first", false), ("do this next", true)]);
    }

    #[test]
    fn only_the_newest_turn_says_what_still_runs_in_the_background() {
        let at = |turns: Vec<EntryKind>| {
            let items = rows_of(&session(Agent::Claude, turns));
            notices(&items)
                .into_iter()
                .map(|(k, t)| (k.to_string(), t.to_string()))
                .collect::<Vec<_>>()
        };
        let said = |text: &str| vec![("background".to_string(), text.to_string())];
        assert_eq!(
            at(vec![turn(Some(2), None)]),
            said("2 background agents still running")
        );
        assert_eq!(
            at(vec![turn(Some(1), None)]),
            said("1 background agent still running")
        );
        assert_eq!(
            at(vec![turn(Some(2), Some(1))]),
            said("2 background agents and 1 background workflow still running")
        );
        assert_eq!(
            at(vec![turn(Some(3), None), turn(Some(0), Some(0))]),
            [],
            "the newest turn counts none"
        );
        assert_eq!(at(vec![turn(None, None)]), []);
        assert_eq!(
            at(vec![turn(Some(3), None), turn(Some(1), None)]),
            said("1 background agent still running"),
            "one row, from the newest"
        );
    }

    #[test]
    fn work_left_running_by_an_agent_that_is_gone_reads_interrupted() {
        let shell = |started: Option<i64>| {
            let mut c = call(
                "Bash",
                Tool::Shell(agentparse::Shell::default()),
                serde_json::Value::Null,
                ToolStatus::Pending,
            );
            c.started_at = started;
            c
        };
        let task = |started: Option<i64>| {
            let tool = Tool::Task {
                description: Some("survey".into()),
                prompt: None,
                agent_type: None,
            };
            let mut c = call("Agent", tool, serde_json::Value::Null, ToolStatus::Pending);
            c.started_at = started;
            c
        };
        let statuses = |since: Option<i64>, calls: Vec<ToolCall>, ts: Option<i64>| {
            let mut s = session(Agent::Claude, calls.into_iter().map(call_message).collect());
            s.entries.iter_mut().for_each(|e| e.ts = ts);
            project(&s, since)
                .0
                .iter()
                .map(|i| match i {
                    Item::Tool { status, .. } | Item::Subagent { status, .. } => *status,
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        let calls = || {
            vec![
                shell(Some(1000)),
                shell(Some(3000)),
                task(Some(1000)),
                task(None),
            ]
        };
        use Status::{Interrupted, Running};
        assert_eq!(
            statuses(Some(2000), calls(), None),
            [Interrupted, Running, Interrupted, Running],
            "one that began before the agent started is left behind"
        );
        assert_eq!(
            statuses(Some(2000), calls(), Some(500)),
            [Interrupted, Running, Interrupted, Interrupted],
            "no start of its own: the message's time"
        );
        assert_eq!(
            statuses(None, calls(), Some(500)),
            [Running; 4],
            "no start known for the agent: nothing changes"
        );
        // A call that ended is not touched.
        let mut done = shell(Some(1000));
        done.status = ToolStatus::Ok;
        done = with_text(done, "ok");
        assert_eq!(statuses(Some(2000), vec![done], None), [Status::Ok]);
        // The count of agents still running belongs to an agent that is there.
        let at = |since: Option<i64>, ts: i64| {
            let mut s = session(Agent::Claude, vec![turn(Some(2), None)]);
            s.entries[0].ts = Some(ts);
            notices(&project(&s, since).0).len()
        };
        assert_eq!(at(Some(2000), 1000), 0, "a turn from before it started");
        assert_eq!(at(Some(2000), 3000), 1);
        assert_eq!(at(None, 1000), 1);
    }

    fn hook(event: &str) -> Hook {
        Hook {
            event: Some(event.into()),
            ..Hook::default()
        }
    }

    #[test]
    fn a_hook_is_a_row_only_when_it_blocked_failed_or_spoke_on_stderr() {
        let said = |hook: Hook| {
            let items = rows_of(&session(Agent::Claude, vec![EntryKind::Hook(hook)]));
            notices(&items)
                .into_iter()
                .map(|(k, t)| format!("{k}: {t}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            said(Hook {
                exit_code: Some(0),
                stdout: Some("ok".into()),
                ..hook("PreToolUse")
            }),
            Vec::<String>::new(),
            "a success is silent"
        );
        assert_eq!(
            said(Hook {
                blocked: true,
                stderr: Some("no force push\nsecond line".into()),
                ..hook("PreToolUse")
            }),
            ["hook: PreToolUse hook blocked: no force push"]
        );
        assert_eq!(
            said(Hook {
                exit_code: Some(2),
                ..hook("Stop")
            }),
            ["hook: Stop hook error: exit 2"]
        );
        assert_eq!(
            said(Hook {
                exit_code: Some(0),
                stderr: Some("bun: command not found".into()),
                ..hook("SessionStart")
            }),
            ["hook: SessionStart hook said: bun: command not found"],
            "exit 0 with a word on stderr is not an error"
        );
        assert_eq!(
            said(Hook {
                stderr: Some("Failed with non-blocking status code".into()),
                ..hook("SessionStart")
            }),
            ["hook: SessionStart hook error: Failed with non-blocking status code"],
            "no exit code recorded, an error is what the agent called it"
        );
        assert_eq!(
            said(Hook {
                blocked: true,
                ..hook("Stop")
            }),
            ["hook: Stop hook blocked"]
        );
        // The blocking attachment and the stop summary that repeats it are one row.
        let twice = rows_of(&session(
            Agent::Claude,
            vec![
                EntryKind::Hook(Hook {
                    blocked: true,
                    stderr: Some("write a card".into()),
                    ..hook("Stop")
                }),
                EntryKind::Hook(Hook {
                    exit_code: Some(2),
                    stderr: Some("write a card".into()),
                    ..hook("Stop")
                }),
                EntryKind::Hook(Hook {
                    blocked: true,
                    stderr: Some("another reason".into()),
                    ..hook("Stop")
                }),
                EntryKind::Hook(Hook {
                    blocked: true,
                    stderr: Some("another reason".into()),
                    ..hook("PreToolUse")
                }),
            ],
        ));
        assert_eq!(
            notices(&twice),
            [
                ("hook", "Stop hook blocked: write a card"),
                ("hook", "Stop hook blocked: another reason"),
                ("hook", "PreToolUse hook blocked: another reason")
            ]
        );
    }

    fn other(kind: &str, data: serde_json::Value) -> EntryKind {
        EntryKind::Other(agentparse::Other {
            kind: kind.into(),
            data,
        })
    }

    #[test]
    fn an_unknown_record_says_its_own_text_and_a_bookkeeping_one_says_nothing() {
        let items = rows_of(&session(
            Agent::Codex,
            vec![
                other(
                    "warning",
                    serde_json::json!({"message": "Heads up\nsecond"}),
                ),
                other(
                    "exec_approval_request",
                    serde_json::json!({"command": "rm -rf x"}),
                ),
                other("mystery", serde_json::json!({"n": 1})),
                other("turn_diff", serde_json::json!({"unified_diff": "x"})),
                other("plan_update", serde_json::json!({"text": "x"})),
                other("web_search_end", serde_json::json!({"query": "x"})),
                other("token_count", serde_json::json!({"n": 1})),
                other("fork-context-ref", serde_json::json!({"n": 1})),
            ],
        ));
        assert_eq!(
            notices(&items),
            [
                ("warning", "Heads up"),
                ("exec_approval_request", "rm -rf x"),
                ("mystery", "Record not shown here"),
            ]
        );
    }

    #[test]
    fn a_notice_without_a_kind_is_named_by_its_level_and_an_empty_one_is_not_a_row() {
        let notice = |level: Option<&str>, kind: Option<&str>, text: &str| {
            EntryKind::Notice(agentparse::Notice {
                level: level.map(String::from),
                kind: kind.map(String::from),
                text: text.into(),
            })
        };
        let items = rows_of(&session(
            Agent::Gemini,
            vec![
                notice(Some("info"), None, "Model changed"),
                notice(Some("warning"), None, "Quota low"),
                notice(Some("info"), None, ""),
                notice(None, None, "  "),
                notice(Some("info"), Some("informational"), "Kept kind"),
                notice(None, Some("model_refusal_no_fallback"), ""),
            ],
        ));
        assert_eq!(
            notices(&items),
            [
                ("info", "Model changed"),
                ("warning", "Quota low"),
                ("informational", "Kept kind"),
                ("model_refusal_no_fallback", "model refusal no fallback"),
            ]
        );
    }

    #[test]
    fn a_compaction_line_skips_numbered_headings_and_reads_a_gemini_goal() {
        let claude = |s| compaction_line(Agent::Claude, s);
        assert_eq!(
            claude(
                "This session is being continued from a previous one.\nSummary:\n1. Primary Request and Intent:\n   The user asked for a chat fix.\n2. Key Concepts:\n"
            ),
            Some("The user asked for a chat fix.".into())
        );
        assert_eq!(claude("B) Files:\nEdited a.rs"), Some("Edited a.rs".into()));
        assert_eq!(
            claude("1. **Primary Request and Intent**\nThe user wants X."),
            Some("The user wants X.".into())
        );
        assert_eq!(
            claude("1. **Fix** the build first"),
            Some("1. **Fix** the build first".into()),
            "bold words in a sentence are not a heading"
        );
        assert_eq!(
            claude("1. Fix the build first"),
            Some("1. Fix the build first".into()),
            "a numbered sentence is not a heading"
        );
        let gemini = |s| compaction_line(Agent::Gemini, s);
        assert_eq!(
            gemini(
                "<state_snapshot><overall_goal>Ship the parser.</overall_goal><key_knowledge>x</key_knowledge></state_snapshot>"
            ),
            Some("Ship the parser.".into())
        );
        assert_eq!(
            gemini(
                "<state_snapshot>\n  <overall_goal>\n    Ship the parser.\n  </overall_goal>\n</state_snapshot>"
            ),
            Some("Ship the parser.".into())
        );
    }

    #[test]
    fn gemini_got_it_after_a_compression_is_not_a_message() {
        let ack = message(
            Role::Assistant,
            Origin::Model,
            Some("compaction_ack"),
            "Got it. Thanks for the additional context!",
        );
        let items = rows_of(&session(
            Agent::Gemini,
            vec![
                EntryKind::Message(ack),
                EntryKind::Message(message(Role::Assistant, Origin::Model, None, "Real reply")),
            ],
        ));
        assert_eq!(items.len(), 1);
        assert!(matches!(&items[0], Item::Agent { text, .. } if text == "Real reply"));
    }

    fn summary_of(call: ToolCall) -> String {
        let items = rows_of(&session(Agent::Claude, vec![call_message(call)]));
        match &items[0] {
            Item::Tool { summary, .. } => summary.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_question_row_says_what_was_asked_and_the_answer_once_there_is_one() {
        let input = serde_json::json!({"questions": [
            {"question": "Which database?", "options": []},
            {"question": "Which cache?"}
        ]});
        let asked = call(
            "AskUserQuestion",
            Tool::Question,
            input,
            ToolStatus::Pending,
        );
        assert_eq!(summary_of(asked.clone()), "Which database?");
        let answered = with_text(
            asked,
            "User has answered your questions: \"Which database?\"=\"Postgres\", \"Which cache?\"=\"None\". You can now continue.",
        );
        assert_eq!(summary_of(answered), "Which database? Postgres");
    }

    #[test]
    fn a_plan_row_is_the_plans_first_heading_else_its_first_line() {
        let plan = |text: &str| {
            let input = serde_json::json!({ "plan": text });
            summary_of(call("ExitPlanMode", Tool::Plan, input, ToolStatus::Ok))
        };
        assert_eq!(
            plan("Intro line\n\n## Fix the chat\n- step"),
            "Fix the chat"
        );
        assert_eq!(plan("\nJust text\nmore"), "Just text");
        assert_eq!(plan(""), "");
    }

    #[test]
    fn a_todo_row_counts_what_is_done_and_names_what_is_in_progress() {
        let item = |content: &str, status: Option<&str>| TodoItem {
            content: content.into(),
            status: status.map(String::from),
            priority: None,
        };
        let items = vec![
            item("write test", Some("completed")),
            item("fix chat", Some("in_progress")),
            item("ship", Some("pending")),
        ];
        assert_eq!(todo_line(&items), "1 of 3 done: fix chat");
        assert_eq!(todo_line(&items[..1]), "1 of 1 done");
        assert_eq!(todo_line(&[item("only one", None)]), "only one");
        assert_eq!(todo_line(&[item("a", None), item("b", None)]), "2 items");
        assert_eq!(checklist(&items), "[x] write test\n[~] fix chat\n[ ] ship");
        let c = call(
            "TodoWrite",
            Tool::Todo { items },
            serde_json::Value::Null,
            ToolStatus::Ok,
        );
        assert_eq!(input_text(&c), "[x] write test\n[~] fix chat\n[ ] ship");
        assert_eq!(summary_of(c), "1 of 3 done: fix chat");
    }

    #[test]
    fn a_tool_this_build_has_no_view_of_reads_what_it_was_for_from_its_input() {
        let other = |input: serde_json::Value| {
            summary_of(call("ScheduleWakeup", Tool::Other, input, ToolStatus::Ok))
        };
        assert_eq!(
            other(serde_json::json!({"reason": "check the build"})),
            "check the build"
        );
        assert_eq!(
            other(serde_json::json!({"subject": "Fix it", "description": "long text"})),
            "Fix it"
        );
        assert_eq!(
            other(serde_json::json!({"query": "select:Read"})),
            "select:Read"
        );
        assert_eq!(
            other(serde_json::json!({"objective": "ship", "n": 1})),
            "ship"
        );
        assert_eq!(other(serde_json::json!({"n": 1})), "");
        assert_eq!(other(serde_json::json!("freeform")), "");
    }

    #[test]
    fn an_orphan_with_no_text_shows_the_commands_output() {
        let result = |text: &str, stdout: Option<&str>, stderr: Option<&str>| ToolResult {
            call_id: "exec-1".into(),
            status: ToolStatus::Ok,
            status_detail: None,
            output: ToolOutput {
                text: text.into(),
                shell: Some(agentparse::ShellResult {
                    exit_code: Some(0),
                    stdout: stdout.map(String::from),
                    stderr: stderr.map(String::from),
                    ..Default::default()
                }),
                ..ToolOutput::default()
            },
        };
        let row = orphan(&result("", Some(" 2 files changed\nmore"), None)).expect("a row");
        assert!(
            matches!(&row, Item::Tool { summary, has_detail: true, .. } if summary == "2 files changed")
        );
        assert_eq!(
            orphan_output(&result("", Some("out"), Some("err"))),
            "out\nerr"
        );
        assert_eq!(orphan_output(&result("given", Some("out"), None)), "given");
        assert!(orphan(&result("", None, None)).is_none());
        let mut named = result("", Some("out"), None);
        named.output.data =
            Some(serde_json::json!({"command": ["/bin/zsh", "-lc", "git diff --stat"]}));
        assert!(
            matches!(orphan(&named), Some(Item::Tool { summary, .. }) if summary == "git diff --stat")
        );
        // A plain argv is one command line, and what it carries is masked: the last word of it
        // alone would be the secret.
        let mut direct = result("", Some("out"), None);
        direct.output.data =
            Some(serde_json::json!({"command": ["mysql", "-u", "root", "--password", "hunter2"]}));
        let Some(Item::Tool { summary, .. }) = orphan(&direct) else {
            panic!("a row")
        };
        assert_eq!(summary, "mysql -u root --password ***");
        direct.output.data = Some(serde_json::json!({"command": ["git", "status"]}));
        assert!(
            matches!(orphan(&direct), Some(Item::Tool { summary, .. }) if summary == "git status")
        );
        // Output that is only escape codes is no row.
        assert!(orphan(&result("\x1b[0m\n", None, None)).is_none());
        assert!(orphan(&result("", Some("\x1b[0m"), None)).is_none());
    }

    #[test]
    fn what_a_rewind_rolled_back_is_not_shown_and_the_notice_stays() {
        let mut s = session(
            Agent::Gemini,
            vec![
                EntryKind::Message(human("keep")),
                EntryKind::Message(human("Wrong prompt, ignore")),
                EntryKind::Message(message(Role::Assistant, Origin::Model, None, "Okay.")),
                EntryKind::Rewind(agentparse::Rewind {
                    dropped: vec!["e1".into(), "e2".into()],
                    turns: None,
                }),
            ],
        );
        let items = rows_of(&s);
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(matches!(&items[0], Item::User { text, .. } if text == "keep"));
        assert!(matches!(&items[1], Item::Notice { kind, .. } if kind == "rewind"));
        s.entries.pop();
        assert_eq!(rows_of(&s).len(), 3, "without the rewind they all show");
    }

    #[test]
    fn gemini_calls_an_agent_by_its_own_name_and_that_is_a_spawn_with_a_child() {
        let tool = Tool::Task {
            description: Some("Map the repo".into()),
            prompt: None,
            agent_type: None,
        };
        let mut c = call(
            "codebase_investigator",
            tool,
            serde_json::Value::Null,
            ToolStatus::Pending,
        );
        c.child_session = Some("agent7".into());
        let items = project(&session(Agent::Gemini, vec![call_message(c.clone())]), None);
        assert!(
            matches!(&items.0[0], Item::Subagent { summary, .. } if summary == "spawned: Map the repo")
        );
        assert_eq!(items.1.len(), 1, "its child is looked up for the steps");
        let claude = project(&session(Agent::Claude, vec![call_message(c)]), None);
        assert!(
            matches!(&claude.0[0], Item::Subagent { summary, .. } if summary == "Map the repo")
        );
        assert!(claude.1.is_empty());
    }
}

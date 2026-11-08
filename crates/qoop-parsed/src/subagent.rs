//! How many tool calls a subagent made. `agentparse` gives the id of the child session and
//! nothing about its length, so the child's own session is read, and only as much as the page
//! needs:
//!
//! - Claude: `subagents/agent-<id>.jsonl` next to the parent file, else the number the
//!   harness wrote into the call's result (`totalToolUseCount`).
//! - Codex: the child rollout under the sessions root, found by the child's own id.
//! - Gemini: `chats/<parent session id>/<agent id>.jsonl` next to the parent file.
//! - OpenCode: a count over the child's rows in the same database, never a parse of it.
//!
//! The steps of a spawn row are the tool calls its child made. Only the newest
//! [`MAX_SPAWNS`] spawn rows of the page that is returned are read. A child file bigger than
//! [`MAX_CHILD_BYTES`] is not read. A count is remembered per child file and its length and
//! modification time, so a load costs a stat per child, and a running child's count moves when
//! its file does. The children of the newest spawn rows are also part of the revision, so a
//! child that progresses is a change of the chat.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use agentparse::{Agent, Session, Source, ToolCall};
use qoop_history::locate::locate_codex;
use qoop_history::{BaseDirs, Located, Product, valid_id};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use crate::item::{Item, Lane, LaneStatus, Status};
use crate::load::{fnv, len_and_mtime};
use crate::project;

/// Spawn rows of a page whose child is read.
pub(crate) const MAX_SPAWNS: usize = 16;

/// Only spawn rows among the newest this many items count for the revision. Every window the
/// parser reads holds at least that many (see `load`), so the same set is seen from every
/// page of one file state.
pub(crate) const WATCH_ITEMS: u32 = 200;

/// A child session bigger than this is not read.
pub(crate) const MAX_CHILD_BYTES: u64 = 8 * 1024 * 1024;

/// Counts remembered, and sessions whose watched children are remembered.
const MAX_COUNTS: usize = 256;
const MAX_WATCHED: usize = 64;

/// How long a child rollout that was not found is not looked for again under the same root.
const MISSING_FOR: Duration = Duration::from_secs(30);

/// A spawn row's child: the id of its session, and the step count the harness reported. Or a
/// workflow row's run: its id in `child`, and lanes read from its journal instead of steps.
#[derive(Debug, Clone)]
pub(crate) struct Spawn {
    child: String,
    reported: Option<u64>,
    run: bool,
}

impl Spawn {
    /// The child session's id, or the workflow's run id.
    pub(crate) fn child(&self) -> &str {
        &self.child
    }

    /// The spawn of a call that starts a subagent whose child id is a plain id.
    pub(crate) fn of(call: &ToolCall, agent: Agent) -> Option<Spawn> {
        let child = call.child_session.as_deref().filter(|c| valid_id(c))?;
        project::is_spawn(call, agent).then(|| Spawn {
            child: child.to_string(),
            // Claude writes the child's tool count into the result of a call that waited.
            reported: call
                .output
                .as_ref()
                .and_then(|o| o.data.as_ref())
                .and_then(|d| d.get("totalToolUseCount"))
                .and_then(serde_json::Value::as_u64),
            run: false,
        })
    }

    /// The run of a call that launched a workflow, when its run id is a plain id.
    pub(crate) fn run(call: &ToolCall) -> Option<Spawn> {
        let data = call.output.as_ref()?.data.as_ref()?;
        let run = data.get("runId")?.as_str().filter(|r| valid_id(r))?;
        Some(Spawn {
            child: run.to_string(),
            reported: None,
            run: true,
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Claude,
    Codex,
    Gemini,
}

#[derive(Default)]
struct Memo {
    /// Steps by child file, its length and modification time.
    counts: Vec<((PathBuf, u64, u64), u32)>,
    /// Where a Codex child was found, by sessions root and child id.
    found: Vec<((PathBuf, String), PathBuf)>,
    /// A Codex child that was not found, and when.
    missing: Vec<((PathBuf, String), Instant)>,
    /// The child files that belong to the revision of a parent file.
    watched: Vec<(PathBuf, Vec<PathBuf>)>,
}

static MEMO: Mutex<Memo> = Mutex::new(Memo {
    counts: Vec::new(),
    found: Vec::new(),
    missing: Vec::new(),
    watched: Vec::new(),
});

fn memo() -> std::sync::MutexGuard<'static, Memo> {
    MEMO.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Put `value` under `key`, dropping the oldest entry at `cap`.
pub(crate) fn remember<K: PartialEq, V>(list: &mut Vec<(K, V)>, cap: usize, key: K, value: V) {
    list.retain(|(k, _)| *k != key);
    if list.len() >= cap {
        list.remove(0);
    }
    list.push((key, value));
}

/// Fill the steps of the page's spawn rows, remember which children the revision now covers,
/// and return the revision: `base` (the parent's stamp) when there are none, else `base`
/// folded with the length and modification time of each.
pub(crate) fn annotate(
    source: &crate::locate::Source,
    session: &Session,
    spawns: &HashMap<u32, Spawn>,
    codex_root: Option<&Path>,
    page: &mut [Item],
    base: u64,
) -> u64 {
    let ctx = Ctx {
        session,
        codex_root,
    };
    let revision = match source {
        crate::locate::Source::File(parent) => {
            let mut near: Vec<_> = spawns
                .iter()
                .filter(|(seq, _)| **seq < WATCH_ITEMS)
                .collect();
            near.sort_by_key(|(seq, _)| **seq);
            // A lane's result in its run's journal is the only write that changes its status, so
            // each workflow's journal is watched too, outside the spawns' budget.
            let (runs, children): (Vec<_>, Vec<_>) = near.into_iter().partition(|(_, s)| s.run);
            let files: Vec<PathBuf> = children
                .into_iter()
                .take(MAX_SPAWNS)
                .chain(runs)
                .filter_map(|(_, spawn)| ctx.file(spawn))
                .collect();
            let revision = combine(base, &stamps(&files));
            remember(&mut memo().watched, MAX_WATCHED, parent.clone(), files);
            revision
        }
        crate::locate::Source::Opencode { .. } => base,
    };
    let mut left = MAX_SPAWNS;
    for item in page.iter_mut().rev() {
        let Item::Subagent {
            seq,
            steps,
            status,
            lanes,
            ..
        } = item
        else {
            continue;
        };
        let Some(spawn) = spawns.get(seq) else {
            continue;
        };
        if spawn.run {
            let journal = ctx.file(spawn);
            *lanes = journal.map_or_else(Vec::new, |j| read_lanes(&j, *status == Status::Running));
        } else if left > 0 {
            left -= 1;
            *steps = ctx.steps(spawn, source);
        }
    }
    revision
}

/// A workflow's lanes from its run's journal: a `started` record is a lane (its agent, label
/// and phase) keyed by its `key`, so a retry under a new agent takes the lane over. A `result`
/// record for the lane's agent makes it done, a `failed` one failed. A lane with no end runs
/// while the workflow does, and was stopped once it ended. No lane's own file is read: its steps show
/// on its own page. A journal that is missing or over [`MAX_CHILD_BYTES`] gives none.
// ponytail: read again on every page that shows the row, no memo; journals are small. A memo by
// length and modification time, as `file_calls` keeps, is the upgrade if a page shows many runs.
fn read_lanes(journal: &Path, running: bool) -> Vec<Lane> {
    #[derive(Deserialize)]
    struct Record {
        #[serde(rename = "type")]
        kind: String,
        key: Option<String>,
        #[serde(rename = "agentId")]
        agent: Option<String>,
        label: Option<String>,
        phase: Option<String>,
    }
    let Ok((len, _)) = len_and_mtime(journal) else {
        return Vec::new();
    };
    let Ok(file) = std::fs::File::open(journal) else {
        return Vec::new();
    };
    if len > MAX_CHILD_BYTES {
        return Vec::new();
    }
    let open = if running {
        LaneStatus::Running
    } else {
        LaneStatus::Stopped
    };
    // (key, agent now on it, lane)
    let mut lanes: Vec<(String, String, Lane)> = Vec::new();
    for line in BufReader::new(file.take(len))
        .split(b'\n')
        .map_while(Result::ok)
    {
        let text = String::from_utf8_lossy(&line);
        if ![
            r#""type":"started""#,
            r#""type":"result""#,
            r#""type":"failed""#,
        ]
        .iter()
        .any(|k| text.contains(k))
        {
            continue;
        }
        let Ok(Record {
            kind,
            key,
            agent: Some(agent),
            label,
            phase,
        }) = serde_json::from_str::<Record>(&text)
        else {
            continue;
        };
        // A lane is its key: a retry starts the same key under a new agent.
        let key = key.unwrap_or_else(|| agent.clone());
        let known = lanes.iter_mut().find(|(k, _, _)| *k == key);
        match (kind.as_str(), known) {
            ("started", known) => {
                let label = label.or(phase).unwrap_or_else(|| "agent".into());
                let lane = Lane {
                    label: crate::text::summary(&label),
                    status: open,
                    child: valid_id(&agent).then(|| agent.clone()),
                };
                match known {
                    Some(known) => *known = (key, agent, lane),
                    None => lanes.push((key, agent, lane)),
                }
            }
            // Only the agent now on the lane ends it, not one a retry replaced.
            (end, Some((_, now, lane))) if *now == agent => {
                lane.status = if end == "result" {
                    LaneStatus::Done
                } else {
                    LaneStatus::Failed
                };
            }
            _ => {}
        }
    }
    lanes.into_iter().map(|(_, _, lane)| lane).collect()
}

/// The revision of `source` as [`annotate`] last left its children: `base` and a stat of each.
pub(crate) fn with_children(source: &crate::locate::Source, base: u64) -> u64 {
    let crate::locate::Source::File(parent) = source else {
        return base;
    };
    let files = memo()
        .watched
        .iter()
        .find(|(p, _)| p == parent)
        .map(|(_, files)| files.clone())
        .unwrap_or_default();
    combine(base, &stamps(&files))
}

fn stamps(files: &[PathBuf]) -> Vec<(u64, u64)> {
    files
        .iter()
        .map(|f| len_and_mtime(f).unwrap_or((0, 0)))
        .collect()
}

fn combine(base: u64, stamps: &[(u64, u64)]) -> u64 {
    if stamps.is_empty() {
        return base;
    }
    let mut parts = vec![base];
    parts.extend(stamps.iter().flat_map(|(len, mtime)| [*len, *mtime]));
    fnv(&parts)
}

struct Ctx<'a> {
    session: &'a Session,
    codex_root: Option<&'a Path>,
}

impl Ctx<'_> {
    /// The child's file, where the format keeps one.
    fn file(&self, spawn: &Spawn) -> Option<PathBuf> {
        let Source::File { path } = &self.session.source else {
            return None;
        };
        match self.session.agent {
            Agent::Claude if spawn.run => Some(
                claude_dir(path)
                    .join("workflows")
                    .join(&spawn.child)
                    .join("journal.jsonl"),
            ),
            _ if spawn.run => None,
            Agent::Claude => Some(claude_file(path, &spawn.child)),
            Agent::Codex => find_codex(&codex_root(path, self.codex_root)?, &spawn.child),
            Agent::Gemini => Some(gemini_file(path, &self.session.id, &spawn.child)),
            _ => None,
        }
    }

    fn steps(&self, spawn: &Spawn, source: &crate::locate::Source) -> u32 {
        let kind = match self.session.agent {
            Agent::Claude => Kind::Claude,
            Agent::Codex => Kind::Codex,
            Agent::Gemini => Kind::Gemini,
            _ => {
                let crate::locate::Source::Opencode { db, .. } = source else {
                    return 0;
                };
                return opencode_calls(db, &spawn.child).map_or(0, clamp);
            }
        };
        // The child's own file wins when it is there; the harness' number is the fallback.
        let counted = self.file(spawn).and_then(|f| file_calls(&f, kind));
        let reported = spawn.reported.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        counted.or(reported).unwrap_or(0)
    }
}

fn clamp(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `<project>/<session>.jsonl` keeps its children in `<project>/<session>/subagents/`, at every
/// depth: a subagent's own children sit beside it. The child id is a plain id (`valid_id`), so
/// it names a file in that directory and no other. A workflow's agents sit one folder down, in
/// `subagents/workflows/<run>/`: that is looked in when the flat file is not there, and only a
/// file [`inside`] the parent's directory is taken.
fn claude_file(parent: &Path, child: &str) -> PathBuf {
    let dir = claude_dir(parent);
    let name = format!("agent-{child}.jsonl");
    let flat = dir.join(&name);
    if flat.is_file() {
        return flat;
    }
    let runs = std::fs::read_dir(dir.join("workflows"))
        .into_iter()
        .flatten();
    runs.flatten()
        .find_map(|run| inside(parent, run.path().join(&name)))
        .unwrap_or(flat)
}

/// The `subagents` directory of a Claude session file: its own, or the one it sits in.
fn claude_dir(parent: &Path) -> PathBuf {
    match parent.parent() {
        Some(dir) if dir.file_name().is_some_and(|n| n == "subagents") => dir.to_path_buf(),
        _ => parent.with_extension("").join("subagents"),
    }
}

/// Where the session of subagent `child` is, for a reader of the session `parent` that `program`
/// wrote (whose own id is `parent_id`). The id is untrusted: it must be a plain id, and the session it names must be a
/// child of this one: a Claude or Gemini file in the parent's own directory for children (a link
/// out of it is not followed), a Codex rollout whose first record names `parent_id` as its parent
/// thread, an OpenCode session whose `parent_id` is `parent_id`. `None` for anything else, and for
/// a child not written yet.
pub fn child_source(
    program: &str,
    parent: &crate::locate::Source,
    parent_id: &str,
    child: &str,
    codex_dir: Option<&Path>,
) -> Option<crate::locate::Source> {
    use crate::locate::Source as S;
    if !valid_id(child) || child == parent_id {
        return None;
    }
    let path = match parent {
        S::Opencode { db, .. } => {
            return opencode_parent(db, child)
                .filter(|p| p == parent_id)
                .map(|_| S::Opencode {
                    db: db.clone(),
                    id: child.to_string(),
                });
        }
        S::File(path) => path,
    };
    let file = match qoop_history::detect_product(program).ok()? {
        Product::Claude => inside(path, claude_file(path, child))?,
        Product::Gemini => inside(path, gemini_file(path, parent_id, child))?,
        Product::Codex => {
            let file = find_codex(&codex_root(path, codex_dir)?, child)?;
            codex_child_of(&file, child, parent_id).then_some(file)?
        }
        Product::Opencode => return None,
    };
    Some(S::File(file))
}

/// `file` when it is a regular file reached from the parent session file's own directory with no
/// link on the way: a linked file, or a linked `subagents` or session directory (one that leads to
/// another session's children), is refused.
fn inside(parent: &Path, file: PathBuf) -> Option<PathBuf> {
    let base = parent.parent()?;
    let below = file.strip_prefix(base).ok()?;
    let real = file.canonicalize().ok()?;
    (real == base.canonicalize().ok()?.join(below) && real.is_file()).then_some(file)
}

/// A Codex rollout's first record (`session_meta`) is thread `child`'s and names `parent_id` as
/// its parent thread.
fn codex_child_of(file: &Path, child: &str, parent_id: &str) -> bool {
    let Ok(Some(v)) = qoop_history::locate::first_line_json(file) else {
        return false;
    };
    let p = v.get("payload");
    let at = |ptr: &str| p.and_then(|p| p.pointer(ptr)).and_then(|s| s.as_str());
    at("/id") == Some(child)
        && [
            "/parent_thread_id",
            "/source/subagent/thread_spawn/parent_thread_id",
        ]
        .iter()
        .any(|ptr| at(ptr) == Some(parent_id))
}

/// The `parent_id` of OpenCode session `id`.
fn opencode_parent(db: &Path, id: &str) -> Option<String> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_millis(200)).ok()?;
    // Either layout may hold the session; a table or column that is not there finds nothing.
    ["session_v2", "session"].iter().find_map(|table| {
        conn.query_row(
            &format!("SELECT parent_id FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    })
}

/// `<project>/chats/session-<..>.jsonl` keeps its children in
/// `<project>/chats/<parent session id>/<agent id>.jsonl`.
fn gemini_file(parent: &Path, session_id: &str, child: &str) -> PathBuf {
    parent
        .with_file_name(session_id)
        .join(format!("{child}.jsonl"))
}

/// The sessions root of a Codex parent file: the one the caller located it under, else the
/// directory above `YYYY/MM/DD` when the file sits in that layout, else none (a file outside
/// it has no tree to look the child up in).
fn codex_root(parent: &Path, given: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = given {
        return Some(root.to_path_buf());
    }
    let day = parent.parent()?;
    let month = day.parent()?;
    let year = month.parent()?;
    let digits = |dir: &Path, len: usize| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.len() == len && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if digits(day, 2) && digits(month, 2) && digits(year, 4) {
        year.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

/// The rollout of a Codex child under `root`. Where it is, and that it is not there, are
/// remembered (the second for [`MISSING_FOR`]), so a missing one does not walk the tree again
/// for every row of every load.
fn find_codex(root: &Path, child: &str) -> Option<PathBuf> {
    let key = (root.to_path_buf(), child.to_string());
    {
        let memo = memo();
        if let Some((_, path)) = memo.found.iter().find(|(k, _)| *k == key)
            && path.is_file()
        {
            return Some(path.clone());
        }
        if memo
            .missing
            .iter()
            .any(|(k, at)| *k == key && at.elapsed() < MISSING_FOR)
        {
            return None;
        }
    }
    let dirs = BaseDirs {
        claude: Vec::new(),
        codex: root.to_path_buf(),
        opencode: None,
        gemini: PathBuf::new(),
    };
    let found = match locate_codex(child, &dirs) {
        Ok(Located::File(path)) => Some(path),
        _ => None,
    };
    let mut memo = memo();
    match &found {
        Some(path) => remember(&mut memo.found, MAX_COUNTS, key, path.clone()),
        None => remember(&mut memo.missing, MAX_COUNTS, key, Instant::now()),
    }
    found
}

/// The tool calls in one child file, from the memo when its length and modification time are
/// the ones counted before. `None` for a file that is missing or too big.
fn file_calls(path: &Path, kind: Kind) -> Option<u32> {
    let (len, mtime) = len_and_mtime(path).ok()?;
    if len > MAX_CHILD_BYTES {
        return None;
    }
    let key = (path.to_path_buf(), len, mtime);
    if let Some((_, n)) = memo().counts.iter().find(|(k, _)| *k == key) {
        return Some(*n);
    }
    // Only what the stat saw is read: a child that grows meanwhile is counted at its next stat.
    let reader = BufReader::new(std::fs::File::open(path).ok()?.take(len));
    let n = clamp(match kind {
        Kind::Claude => claude_calls(reader),
        Kind::Codex => codex_calls(reader),
        Kind::Gemini => gemini_calls(reader),
    });
    remember(&mut memo().counts, MAX_COUNTS, key, n);
    Some(n)
}

/// Tool-use blocks of a Claude child, once per block id. Only a line that has the marker is
/// parsed.
fn claude_calls(reader: impl BufRead) -> usize {
    #[derive(Deserialize)]
    struct Line {
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        #[serde(default, deserialize_with = "tool_use_ids")]
        content: Vec<String>,
    }
    let mut seen = HashSet::new();
    for line in reader.split(b'\n').map_while(Result::ok) {
        let text = String::from_utf8_lossy(&line);
        if !text.contains("\"type\":\"tool_use\"") {
            continue;
        }
        if let Ok(Line {
            message: Some(Message { content }),
        }) = serde_json::from_str::<Line>(&text)
        {
            seen.extend(content);
        }
    }
    seen.len()
}

/// The ids of the `tool_use` blocks of a message `content`, which is a list of blocks or a
/// plain string.
fn tool_use_ids<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    struct Block {
        #[serde(rename = "type")]
        kind: Option<String>,
        id: Option<String>,
    }
    struct Blocks;
    impl<'de> Visitor<'de> for Blocks {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of blocks or a string")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut ids = Vec::new();
            while let Some(block) = seq.next_element::<Block>()? {
                if block.kind.as_deref() == Some("tool_use") {
                    // A block with no id still counts, once.
                    ids.push(block.id.unwrap_or_else(|| format!("#{}", ids.len())));
                }
            }
            Ok(ids)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_any(Blocks)
}

/// Tool calls of a Gemini child, once per call id: a message is written again as it grows, and
/// a later `$patch` record can carry the calls of one too. Only a line that has the marker is
/// parsed.
fn gemini_calls(reader: impl BufRead) -> usize {
    fn ids(record: &serde_json::Value, out: &mut HashSet<String>) {
        let calls = record.get("toolCalls").and_then(|c| c.as_array());
        for (n, call) in calls.into_iter().flatten().enumerate() {
            let id = call.get("id").and_then(|i| i.as_str());
            out.insert(id.map_or_else(|| format!("#{n}"), str::to_string));
        }
    }
    let mut seen = HashSet::new();
    for line in reader.split(b'\n').map_while(Result::ok) {
        let text = String::from_utf8_lossy(&line);
        if !text.contains("\"toolCalls\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        ids(&record, &mut seen);
        let updates = record.pointer("/$patch/updates").and_then(|u| u.as_array());
        updates
            .into_iter()
            .flatten()
            .for_each(|u| ids(u, &mut seen));
    }
    seen.len()
}

/// Tool calls of a Codex child: the `response_item` records whose payload is a call, from the
/// ordinal its own history starts at (before that is the history it inherited).
fn codex_calls(reader: impl BufRead) -> usize {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: Option<String>,
        ordinal: Option<u64>,
        payload: Option<Payload>,
    }
    #[derive(Deserialize)]
    struct Payload {
        #[serde(rename = "type")]
        kind: Option<String>,
        subagent_history_start_ordinal: Option<u64>,
    }
    let mut start = 0;
    let mut first = true;
    let mut n = 0;
    for line in reader.split(b'\n').map_while(Result::ok) {
        let text = String::from_utf8_lossy(&line);
        let head = std::mem::take(&mut first);
        if !head && !text.contains("_call\"") {
            continue;
        }
        let Ok(Line {
            kind,
            ordinal,
            payload: Some(payload),
        }) = serde_json::from_str::<Line>(&text)
        else {
            continue;
        };
        match kind.as_deref() {
            Some("session_meta") if head => {
                start = payload.subagent_history_start_ordinal.unwrap_or(0)
            }
            Some("response_item")
                if ordinal.is_none_or(|o| o >= start)
                    && payload
                        .kind
                        .as_deref()
                        .is_some_and(|k| k.ends_with("_call")) =>
            {
                n += 1;
            }
            _ => {}
        }
    }
    n
}

/// The tool calls of a child session in the same OpenCode database, by a count over its rows:
/// the `tool` parts of the older layout and the `tool` blocks of the assistant rows of the
/// newer one. The database is opened read only.
fn opencode_calls(db: &Path, child: &str) -> Option<usize> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_millis(200)).ok()?;
    let count = |sql: &str| -> Option<i64> { conn.query_row(sql, [child], |r| r.get(0)).ok() };
    let older = count(
        "SELECT count(*) FROM part WHERE session_id = ?1 AND json_extract(data, '$.type') = 'tool'",
    );
    // A session that is in both layouts was migrated: the older rows are the whole history, and
    // the newer rows that repeat one of its messages are not counted a second time.
    let migrated = if older.is_some() {
        "AND m.id NOT IN (SELECT id FROM message WHERE session_id = ?1)"
    } else {
        ""
    };
    let newer = count(&format!(
        "SELECT count(*) FROM session_message m, json_each(m.data, '$.content') b \
         WHERE m.session_id = ?1 AND m.type = 'assistant' {migrated} \
         AND json_extract(b.value, '$.type') = 'tool'"
    ));
    // Neither table answered: the database is not one to count in.
    (older.is_some() || newer.is_some())
        .then(|| usize::try_from(older.unwrap_or(0) + newer.unwrap_or(0)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_child_counts_each_tool_use_block_once_by_id() {
        let line = |blocks: &str| format!("{{\"message\":{{\"content\":{blocks}}}}}\n");
        let text = [
            line(r#"[{"type":"text","text":"x"},{"type":"tool_use","id":"a","name":"Bash"}]"#),
            line(r#"[{"type":"tool_use","id":"a","name":"Bash"}]"#),
            line(r#"[{"type":"tool_use","id":"b"},{"type":"tool_use","id":"c"}]"#),
            line(r#""a plain string with "type":"tool_use" in it""#),
            "not json \"type\":\"tool_use\"\n".to_string(),
        ]
        .concat();
        assert_eq!(claude_calls(text.as_bytes()), 3);
    }

    #[test]
    fn a_codex_child_counts_calls_from_where_its_own_history_starts() {
        let rec = |ordinal: u64, ty: &str, payload: &str| {
            format!("{{\"ordinal\":{ordinal},\"type\":\"{ty}\",\"payload\":{payload}}}\n")
        };
        let text = [
            rec(0, "session_meta", r#"{"subagent_history_start_ordinal":3}"#),
            rec(1, "response_item", r#"{"type":"function_call"}"#),
            rec(3, "response_item", r#"{"type":"function_call"}"#),
            rec(4, "response_item", r#"{"type":"function_call_output"}"#),
            rec(5, "response_item", r#"{"type":"custom_tool_call"}"#),
            rec(6, "event_msg", r#"{"type":"exec_command_call"}"#),
        ]
        .concat();
        assert_eq!(codex_calls(text.as_bytes()), 2);
    }

    #[test]
    fn a_gemini_child_counts_each_call_id_once_across_rewrites_and_patches() {
        let text = [
            r#"{"sessionId":"c","kind":"subagent"}"#,
            r#"{"id":"g1","type":"gemini","content":"x","toolCalls":[{"id":"a"},{"id":"b"}]}"#,
            r#"{"id":"g1","type":"gemini","content":"x","toolCalls":[{"id":"a"},{"id":"b"}]}"#,
            r#"{"$patch":{"updates":[{"id":"g2","toolCalls":[{"id":"c"},{"id":"a"}]}]}}"#,
            r#"{"id":"u","type":"user","content":"the word toolCalls in text"}"#,
            "not json \"toolCalls\"",
        ]
        .join("\n");
        assert_eq!(gemini_calls(text.as_bytes()), 3);
        assert_eq!(
            gemini_file(
                Path::new("/h/tmp/p/chats/session-a-1.jsonl"),
                "uuid-1",
                "agent7"
            ),
            PathBuf::from("/h/tmp/p/chats/uuid-1/agent7.jsonl")
        );
    }

    #[test]
    fn a_gemini_spawn_row_counts_the_calls_in_its_child_file_next_to_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        std::fs::create_dir_all(chats.join("parent-1")).unwrap();
        let parent = chats.join("session-2026-10-12T09-00-parent-1.jsonl");
        std::fs::write(&parent, "").unwrap();
        std::fs::write(
            chats.join("parent-1/agent7.jsonl"),
            "{\"id\":\"g\",\"toolCalls\":[{\"id\":\"a\"},{\"id\":\"b\"}]}\n",
        )
        .unwrap();
        let call = ToolCall {
            id: "t".into(),
            name: "codebase_investigator".into(),
            input: serde_json::Value::Null,
            tool: agentparse::Tool::Task {
                description: Some("map".into()),
                prompt: None,
                agent_type: None,
                name: None,
            },
            status: agentparse::ToolStatus::Pending,
            status_detail: None,
            output: None,
            started_at: None,
            ended_at: None,
            child_session: Some("agent7".into()),
            children: Vec::new(),
        };
        let spawn = Spawn::of(&call, Agent::Gemini).expect("a spawn");
        let mut session = project::blank_session(Agent::Gemini, &parent);
        session.id = "parent-1".into();
        let source = crate::locate::Source::File(parent);
        let ctx = Ctx {
            session: &session,
            codex_root: None,
        };
        assert_eq!(ctx.steps(&spawn, &source), 2);
    }

    #[test]
    fn a_codex_root_is_the_one_given_or_the_directory_above_a_dated_layout() {
        let at = |p: &str| codex_root(Path::new(p), None);
        assert_eq!(
            at("/h/.codex/sessions/2026/10/15/rollout-x.jsonl"),
            Some(PathBuf::from("/h/.codex/sessions"))
        );
        assert_eq!(at("/Users/me/rollout-x.jsonl"), None);
        assert_eq!(at("/h/sessions/flat/rollout-x.jsonl"), None);
        assert_eq!(
            codex_root(Path::new("/x/rollout.jsonl"), Some(Path::new("/r"))),
            Some(PathBuf::from("/r"))
        );
    }

    fn fixture_db(dir: &Path) -> PathBuf {
        let db = dir.join("o.db");
        let sql = include_str!("../tests/fixtures/opencode/v2.sql");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
        db
    }

    #[test]
    fn an_opencode_child_is_counted_by_sql_in_both_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture_db(dir.path());
        // `part` rows (older layout) and `session_message` blocks (newer) of three children.
        assert_eq!(
            opencode_calls(&db, "ses_ed52a75fff9cZv864nVGS5jPGC"),
            Some(2)
        );
        assert_eq!(
            opencode_calls(&db, "ses_ecd6a835ff7fukfa0R96znWxUu"),
            Some(1)
        );
        assert_eq!(
            opencode_calls(&db, "ses_ed52a633ff93Y7Vatlwl1zyPKr"),
            Some(0)
        );
        assert_eq!(opencode_calls(&dir.path().join("none.db"), "x"), None);
        let empty = dir.path().join("e.db");
        rusqlite::Connection::open(&empty).unwrap();
        assert_eq!(opencode_calls(&empty, "x"), None, "no table to count in");
    }

    /// A Claude parent file with `n` async spawns, each with a child file of two calls.
    fn claude_spawns(dir: &Path, n: usize) -> (Session, Vec<PathBuf>) {
        let sid = "7a1c0de0-1111-4222-8333-444455556666";
        let project = dir.join("p");
        std::fs::create_dir_all(project.join(sid).join("subagents")).unwrap();
        let mut lines = vec![format!(
            "{{\"type\":\"user\",\"uuid\":\"u0\",\"sessionId\":\"{sid}\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}}}}\n"
        )];
        let mut children = Vec::new();
        for k in 0..n {
            let (id, agent) = (format!("toolu_{k}"), format!("a{k:03}"));
            lines.push(format!(
                "{{\"type\":\"assistant\",\"uuid\":\"a{k}\",\"sessionId\":\"{sid}\",\"message\":{{\"id\":\"m{k}\",\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"Agent\",\"input\":{{\"description\":\"d{k}\",\"prompt\":\"p\"}}}}]}}}}\n"
            ));
            lines.push(format!(
                "{{\"type\":\"user\",\"uuid\":\"r{k}\",\"sessionId\":\"{sid}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"ok\"}}]}},\"toolUseResult\":{{\"agentId\":\"{agent}\",\"isAsync\":true}}}}\n"
            ));
            let child = project
                .join(sid)
                .join("subagents")
                .join(format!("agent-{agent}.jsonl"));
            let calls = ["x", "y"].map(|t| {
                format!("{{\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"{agent}{t}\"}}]}}}}\n")
            });
            std::fs::write(&child, calls.concat()).unwrap();
            children.push(child);
        }
        let parent = project.join(format!("{sid}.jsonl"));
        std::fs::write(&parent, lines.concat()).unwrap();
        (agentparse::parse_file(&parent).unwrap(), children)
    }

    fn counted(path: &Path) -> bool {
        memo().counts.iter().any(|((p, _, _), _)| p == path)
    }

    #[test]
    fn only_the_spawn_rows_of_the_returned_page_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let (session, children) = claude_spawns(dir.path(), 40);
        let (mut items, spawns) = project::project(&session, None, None);
        let from = items.len() - 3;
        let mut page = items.split_off(from);
        let source = crate::locate::Source::File(dir.path().join("p/x.jsonl"));
        annotate(&source, &session, &spawns, None, &mut page, 7);
        let steps: Vec<_> = page
            .iter()
            .filter_map(|i| match i {
                Item::Subagent { steps, .. } => Some(*steps),
                _ => None,
            })
            .collect();
        assert_eq!(steps, [2, 2, 2], "the page's three spawn rows");
        assert!(children[37..].iter().all(|c| counted(c)));
        assert!(
            !children.iter().take(37).any(|c| counted(c)),
            "no row outside the page was read"
        );
    }

    #[test]
    fn the_memo_drops_its_oldest_entry_at_the_cap() {
        let mut list: Vec<(u32, u32)> = Vec::new();
        for n in 0..5 {
            remember(&mut list, 3, n, n);
        }
        remember(&mut list, 3, 3, 33);
        assert_eq!(list, [(2, 2), (4, 4), (3, 33)]);
    }

    #[test]
    fn a_child_opens_only_as_a_session_of_its_own_parent() {
        use crate::locate::Source as S;
        let dir = tempfile::tempdir().unwrap();
        // The real path: lookups resolve links (`/var` is `/private/var` on macOS).
        let base = dir.path().canonicalize().unwrap();
        // Claude: one `subagents/` directory per top session, children of children beside them.
        let top = base.as_path().join("p/s1.jsonl");
        let subs = base.as_path().join("p/s1/subagents");
        std::fs::create_dir_all(&subs).unwrap();
        std::fs::write(&top, "").unwrap();
        std::fs::write(subs.join("agent-a1.jsonl"), "").unwrap();
        std::fs::write(subs.join("agent-a2.jsonl"), "").unwrap();
        std::fs::write(base.as_path().join("p/secret.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(
            base.as_path().join("p/secret.jsonl"),
            subs.join("agent-out.jsonl"),
        )
        .unwrap();
        let claude = |parent: &Path, child: &str| {
            child_source("claude", &S::File(parent.to_path_buf()), "s1", child, None)
        };
        assert_eq!(
            claude(&top, "a1"),
            Some(S::File(subs.join("agent-a1.jsonl")))
        );
        assert_eq!(
            claude(&subs.join("agent-a1.jsonl"), "a2"),
            Some(S::File(subs.join("agent-a2.jsonl"))),
            "a subagent's own child sits beside it"
        );
        for refused in ["../secret", "a1/../a2", "missing", "out", ""] {
            assert_eq!(claude(&top, refused), None, "{refused}");
        }
        // Another session's `subagents` directory linked in as this one's.
        let other = base.join("p/s2/subagents");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("agent-b1.jsonl"), "").unwrap();
        let linked = base.join("p/s3.jsonl");
        std::fs::write(&linked, "").unwrap();
        std::fs::create_dir_all(base.join("p/s3")).unwrap();
        std::os::unix::fs::symlink(&other, base.join("p/s3/subagents")).unwrap();
        assert_eq!(
            child_source("claude", &S::File(linked), "s3", "b1", None),
            None,
            "a linked subagents directory"
        );

        // Codex: the rollout must name the parent (or the root) as its own.
        let day = base.as_path().join("sessions/2026/10/12");
        std::fs::create_dir_all(&day).unwrap();
        let meta = |id: &str, parent: &str, root: &str| {
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"session_id\":\"{root}\",\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{parent}\"}}}}}}}}}}\n"
            )
        };
        let ids = [
            "019f1a10-0001-7000-8000-000000000001",
            "019f1a10-0002-7000-8000-000000000002",
            "019f1a10-0003-7000-8000-000000000003",
            "019f1a10-0004-7000-8000-000000000004",
        ];
        let rollout = |id: &str| day.join(format!("rollout-2026-10-12T09-00-00-{id}.jsonl"));
        std::fs::write(rollout(ids[0]), meta(ids[0], "", ids[0])).unwrap();
        std::fs::write(rollout(ids[1]), meta(ids[1], ids[0], ids[0])).unwrap();
        std::fs::write(rollout(ids[2]), meta(ids[2], ids[1], ids[0])).unwrap();
        std::fs::write(
            rollout(ids[3]),
            meta(ids[3], "someone-else", "someone-else"),
        )
        .unwrap();
        let codex =
            |child: &str| child_source("codex", &S::File(rollout(ids[0])), ids[0], child, None);
        assert_eq!(codex(ids[1]), Some(S::File(rollout(ids[1]))));
        assert_eq!(
            codex(ids[2]),
            None,
            "a grandchild is its parent's child, not the root's"
        );
        assert_eq!(codex(ids[3]), None, "another session's child");
        assert_eq!(codex(ids[0]), None, "the session itself");

        // OpenCode: the row's `parent_id`.
        let db = fixture_db(base.as_path());
        let oc = |parent: &str, child: &str| {
            child_source(
                "opencode",
                &S::Opencode {
                    db: db.clone(),
                    id: parent.into(),
                },
                parent,
                child,
                None,
            )
        };
        let (root, kid) = (
            "ses_ed52a75fff9cZv864nVGS5jPGC",
            "ses_ed52a633ff93Y7Vatlwl1zyPKr",
        );
        assert_eq!(
            oc(root, kid),
            Some(S::Opencode {
                db: db.clone(),
                id: kid.into()
            })
        );
        assert_eq!(oc("ses_ecd6ad17ff80AEbRxnOg8B21Mn", kid), None);

        // Gemini: `chats/<parent id>/<child>.jsonl`.
        let chats = base.as_path().join("g/chats");
        std::fs::create_dir_all(chats.join("gp")).unwrap();
        std::fs::write(chats.join("gp/agent7.jsonl"), "").unwrap();
        let gemini = child_source(
            "gemini",
            &S::File(chats.join("session-x.jsonl")),
            "gp",
            "agent7",
            None,
        );
        assert_eq!(gemini, Some(S::File(chats.join("gp/agent7.jsonl"))));
    }

    /// A Claude parent file with one Workflow launch (its result text names the run, as about
    /// 40% of launches do with no `toolUseResult`), ended by a notification when `ended`, and the
    /// run's journal: three lanes started, the first two with a result. Returns the parent and
    /// the journal.
    fn claude_workflow(dir: &Path, ended: bool) -> (PathBuf, PathBuf) {
        let sid = "7a1c0de0-1111-4222-8333-444455556666";
        let run = "wf_af368794-257";
        let project = dir.join("p");
        let runs = project.join(sid).join("subagents/workflows").join(run);
        std::fs::create_dir_all(&runs).unwrap();
        let rec = |kind: &str, uuid: &str, ts: &str, message: &str| {
            format!(
                "{{\"type\":\"{kind}\",\"uuid\":\"{uuid}\",\"sessionId\":\"{sid}\",\"timestamp\":\"2026-11-07T14:{ts}.000Z\",\"message\":{message}}}\n"
            )
        };
        let mut lines = vec![
            rec("user", "u0", "34:00", r#"{"role":"user","content":"count"}"#),
            rec(
                "assistant",
                "a1",
                "34:41",
                r#"{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"toolu_wf","name":"Workflow","input":{"script":"x"}}]}"#,
            ),
            rec(
                "user",
                "r1",
                "34:42",
                &serde_json::json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_wf", "content": format!("Workflow launched in background. Task ID: wc5135q8d\nSummary: Two read-only agents\nRun ID: {run}\n\nYou will be notified when it completes.")}]}).to_string(),
            ),
        ];
        if ended {
            lines.push(rec(
                "user",
                "n1",
                "35:10",
                &serde_json::json!({"role": "user", "content": "<task-notification>\n<task-id>wc5135q8d</task-id>\n<tool-use-id>toolu_wf</tool-use-id>\n<status>killed</status>\n<summary>Workflow stopped</summary>\n</task-notification>"}).to_string(),
            ));
        }
        let parent = project.join(format!("{sid}.jsonl"));
        std::fs::write(&parent, lines.concat()).unwrap();
        let journal = runs.join("journal.jsonl");
        std::fs::write(
            &journal,
            [
                r#"{"type":"launched"}"#,
                r#"{"type":"started","key":"k1","agentId":"ac5a2df1d68b53059","label":"readme-words","phase":"Count"}"#,
                r#"{"type":"started","key":"k2","agentId":"a3e50f9194212a07a","label":"calc-functions","phase":"Count"}"#,
                r#"{"type":"started","key":"k3","agentId":"a77e0000000000003","phase":"Count"}"#,
                r#"{"type":"result","key":"k1","agentId":"ac5a2df1d68b53059","result":{"count":8}}"#,
                r#"{"type":"result","key":"k2","agentId":"a3e50f9194212a07a","result":{"count":2}}"#,
            ]
            .map(|l| format!("{l}\n"))
            .concat(),
        )
        .unwrap();
        std::fs::write(runs.join("agent-a3e50f9194212a07a.jsonl"), "").unwrap();
        (parent, journal)
    }

    fn workflow_page(parent: &Path) -> (Vec<Item>, u64) {
        let session = agentparse::parse_file(parent).unwrap();
        let (mut items, spawns) = project::project(&session, None, None);
        let source = crate::locate::Source::File(parent.to_path_buf());
        let revision = annotate(&source, &session, &spawns, None, &mut items, 7);
        (items, revision)
    }

    fn lanes_of(items: &[Item]) -> Vec<(String, crate::item::LaneStatus, Option<String>)> {
        items
            .iter()
            .find_map(|i| match i {
                Item::Subagent {
                    run: Some(_),
                    lanes,
                    ..
                } => Some(
                    lanes
                        .iter()
                        .map(|l| (l.label.clone(), l.status, l.child.clone()))
                        .collect(),
                ),
                _ => None,
            })
            .expect("a workflow row")
    }

    #[test]
    fn a_workflow_row_lists_its_lanes_from_the_runs_journal() {
        use crate::item::LaneStatus::{Done, Running, Stopped};
        let o = |s: &str| Some(s.to_string());
        let dir = tempfile::tempdir().unwrap();
        let (parent, _) = claude_workflow(dir.path(), false);
        assert_eq!(
            lanes_of(&workflow_page(&parent).0),
            [
                ("readme-words".to_string(), Done, o("ac5a2df1d68b53059")),
                ("calc-functions".to_string(), Done, o("a3e50f9194212a07a")),
                ("Count".to_string(), Running, o("a77e0000000000003")),
            ]
        );
        let dir = tempfile::tempdir().unwrap();
        let (parent, _) = claude_workflow(dir.path(), true);
        assert_eq!(
            lanes_of(&workflow_page(&parent).0)[2],
            ("Count".to_string(), Stopped, o("a77e0000000000003")),
            "a lane with no result under an ended run"
        );
    }

    #[test]
    fn a_restarted_lane_is_one_lane_and_a_failed_one_ends() {
        use crate::item::LaneStatus::{Done, Failed};
        let o = |s: &str| Some(s.to_string());
        let dir = tempfile::tempdir().unwrap();
        let (parent, journal) = claude_workflow(dir.path(), false);
        // The real shapes: a key started again under a new agent (a retry), and a top-level
        // `failed` record for a lane that gave up.
        std::fs::write(
            &journal,
            [
                r#"{"type":"started","key":"k1","agentId":"a1000000000000001","label":"first"}"#,
                r#"{"type":"started","key":"k1","agentId":"a1000000000000002","label":"first"}"#,
                r#"{"type":"result","key":"k1","agentId":"a1000000000000002","result":{}}"#,
                r#"{"type":"started","key":"k2","agentId":"a1000000000000003","label":"second"}"#,
                r#"{"type":"failed","key":"k2","agentId":"a1000000000000003"}"#,
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        assert_eq!(
            lanes_of(&workflow_page(&parent).0),
            [
                ("first".to_string(), Done, o("a1000000000000002")),
                ("second".to_string(), Failed, o("a1000000000000003")),
            ]
        );
    }

    #[test]
    fn a_lanes_agent_opens_from_the_runs_folder() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (parent, journal) = claude_workflow(&base, false);
        let lane = journal.with_file_name("agent-a3e50f9194212a07a.jsonl");
        assert_eq!(
            child_source(
                "claude",
                &crate::locate::Source::File(parent),
                "7a1c0de0-1111-4222-8333-444455556666",
                "a3e50f9194212a07a",
                None
            ),
            Some(crate::locate::Source::File(lane))
        );
    }

    #[test]
    fn a_lanes_result_in_the_journal_changes_the_revision() {
        let dir = tempfile::tempdir().unwrap();
        let (parent, journal) = claude_workflow(dir.path(), false);
        let (_, before) = workflow_page(&parent);
        let source = crate::locate::Source::File(parent.clone());
        assert_eq!(with_children(&source, 7), before);
        let mut text = std::fs::read_to_string(&journal).unwrap();
        text.push_str(
            "{\"type\":\"result\",\"key\":\"k3\",\"agentId\":\"a77e0000000000003\",\"result\":{}}\n",
        );
        std::fs::write(&journal, text).unwrap();
        assert_ne!(
            with_children(&source, 7),
            before,
            "only the journal changed: the lane went from running to done"
        );
    }
}

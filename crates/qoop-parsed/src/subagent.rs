//! How many tool calls a subagent made. `agentparse` gives the id of the child session and
//! nothing about its length, so the child's own session is read, and only as much as the page
//! needs:
//!
//! - Claude: `subagents/agent-<id>.jsonl` next to the parent file, else the number the
//!   harness wrote into the call's result (`totalToolUseCount`).
//! - Codex: the child rollout under the sessions root, found by the child's own id.
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
use qoop_history::{BaseDirs, Located, valid_id};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use crate::item::Item;
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

/// A spawn row's child: the id of its session, and the step count the harness reported.
#[derive(Debug, Clone)]
pub(crate) struct Spawn {
    child: String,
    reported: Option<u64>,
}

impl Spawn {
    /// The spawn of a call that starts a subagent whose child id is a plain id.
    pub(crate) fn of(call: &ToolCall) -> Option<Spawn> {
        let child = call.child_session.as_deref().filter(|c| valid_id(c))?;
        project::is_spawn(call).then(|| Spawn {
            child: child.to_string(),
            // Claude writes the child's tool count into the result of a call that waited.
            reported: call
                .output
                .as_ref()
                .and_then(|o| o.data.as_ref())
                .and_then(|d| d.get("totalToolUseCount"))
                .and_then(serde_json::Value::as_u64),
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Claude,
    Codex,
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
fn remember<K: PartialEq, V>(list: &mut Vec<(K, V)>, cap: usize, key: K, value: V) {
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
            let files: Vec<PathBuf> = near
                .into_iter()
                .take(MAX_SPAWNS)
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
        let Item::Subagent { seq, steps, .. } = item else {
            continue;
        };
        let Some(spawn) = spawns.get(seq) else {
            continue;
        };
        if left == 0 {
            break;
        }
        left -= 1;
        *steps = ctx.steps(spawn, source);
    }
    revision
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
            Agent::Claude => Some(claude_file(path, &spawn.child)),
            Agent::Codex => find_codex(&codex_root(path, self.codex_root)?, &spawn.child),
            _ => None,
        }
    }

    fn steps(&self, spawn: &Spawn, source: &crate::locate::Source) -> u32 {
        let kind = match self.session.agent {
            Agent::Claude => Kind::Claude,
            Agent::Codex => Kind::Codex,
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

/// `<project>/<session>.jsonl` keeps its children in `<project>/<session>/subagents/`. The
/// child id is a plain id (`valid_id`), so it names a file in that directory and no other.
fn claude_file(parent: &Path, child: &str) -> PathBuf {
    parent
        .with_extension("")
        .join("subagents")
        .join(format!("agent-{child}.jsonl"))
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
        let (mut items, spawns) = project::project(&session);
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
}

//! What a person typed to their agents in one repository, across every session there.
//!
//! The terminal readers answer "what did this one session say"; this answers "what did
//! the person ask for in this repo": their own messages from every Claude Code, Codex and
//! opencode session whose directory is the repo, inside it, or one of its worktrees, live
//! or deleted. Files are streamed line by line and only user lines are parsed, so a
//! gigabyte of history costs about a second. Gemini's chats are not read: their newer
//! format is not one this crate parses.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::model::{Message, Product, Role};
use crate::{BaseDirs, claude, codex, opencode};

/// The repository the messages belong to, as git sees it. The caller asks git; this
/// module only compares paths.
#[derive(Debug, Clone)]
pub struct Repo {
    /// The main worktree's top directory.
    pub root: PathBuf,
    /// The git common dir every worktree of this repo shares.
    pub common: PathBuf,
    /// Every live worktree, wherever it is (a scratch directory too).
    pub worktrees: Vec<PathBuf>,
}

/// One typed message and where it was typed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Typed {
    /// Epoch millis.
    pub ts: i64,
    pub product: Product,
    /// A short session id: enough to tell sessions apart in a list.
    pub session: String,
    pub text: String,
}

/// Lines a person never typed, though they ride on user lines: slash-command echoes,
/// notifications, reminders, interrupts, the summary a resumed session starts with, a
/// message relayed from another session and a recovered session's transcript.
/// `ponytail:` a prompt an agent typed into a session it started itself still reads as
/// typed; nothing in the stores tells the two apart.
const NOT_TYPED: [&str; 12] = [
    "<command-",
    "<local-command",
    "<task-notification",
    "<system-reminder",
    "<bash-",
    "Caveat:",
    "[Request interrupted",
    "[Your previous response",
    "This session is being continued",
    "<user_instructions>",
    "Another Claude session",
    "Resume from where you left off",
];

/// Typed words, not a lone `/command` or a dropped file path.
fn typed_text(text: &str) -> bool {
    let t = text.trim();
    let lone_slash = t.starts_with('/') && !t.contains(char::is_whitespace);
    !t.is_empty() && !lone_slash && !NOT_TYPED.iter().any(|p| t.starts_with(p))
}

/// What was read: the newest messages, oldest first, and what could not be read (a
/// locked or unfamiliar opencode database), so a gap is said rather than hidden.
#[derive(Debug, Default)]
pub struct TypedIn {
    pub messages: Vec<Typed>,
    pub unread: Vec<String>,
}

/// A source written up to this long before a message it holds still counts: a copied
/// or synced file can carry an older mtime than its own lines.
const MTIME_SLACK_MS: i64 = 86_400_000;

/// The newest `last` messages typed in `repo`, oldest first.
pub fn typed_in(repo: &Repo, dirs: &BaseDirs, last: usize) -> TypedIn {
    let mut member = Membership::new(repo);
    let mut unread = Vec::new();
    let mut sources = claude_files(repo, &dirs.claude);
    sources.extend(codex_files(&dirs.codex));
    let conn = dirs.opencode.as_ref().and_then(|db| {
        let opened = opencode::open(db).and_then(|c| opencode::top_sessions(&c).map(|s| (c, s)));
        opened
            .map_err(|e| unread.push(format!("opencode: {e}")))
            .ok()
    });
    if let Some((_, sessions)) = &conn {
        for (id, directory, updated) in sessions {
            if member.belongs(Path::new(directory)) {
                sources.push(Source {
                    kind: Kind::Opencode(id.clone()),
                    mtime: *updated,
                });
            }
        }
    }
    // Newest first: once `last` messages are held, a source last written well before the
    // oldest of them cannot hold a newer one, and neither can any after it.
    sources.sort_by_key(|s| std::cmp::Reverse(s.mtime));
    let mut held: Vec<Typed> = Vec::new();
    for source in sources {
        if held.len() >= last {
            held.sort_by_key(|m| std::cmp::Reverse(m.ts));
            held.truncate(last);
            if held
                .last()
                .is_some_and(|oldest| source.mtime + MTIME_SLACK_MS < oldest.ts)
            {
                break;
            }
        }
        let found = match &source.kind {
            Kind::Claude(path) => read_claude(path, source.mtime, &mut member),
            Kind::Codex(path) => read_codex(path, source.mtime, &mut member),
            Kind::Opencode(id) => match &conn {
                Some((conn, _)) => opencode::typed(conn, id)
                    .unwrap_or_else(|e| {
                        unread.push(format!("opencode {}: {e}", tail(id, 8)));
                        Vec::new()
                    })
                    .into_iter()
                    .map(|m| Typed {
                        ts: m.ts.unwrap_or(source.mtime),
                        product: Product::Opencode,
                        session: tail(id, 8),
                        text: m.text,
                    })
                    .collect(),
                None => Vec::new(),
            },
        };
        held.extend(found.into_iter().filter(|m| typed_text(&m.text)));
    }
    held.sort_by_key(|m| std::cmp::Reverse(m.ts));
    held.truncate(last);
    held.reverse();
    TypedIn {
        messages: held,
        unread,
    }
}

struct Source {
    kind: Kind,
    /// Epoch millis of the last write.
    mtime: i64,
}

enum Kind {
    Claude(PathBuf),
    Codex(PathBuf),
    Opencode(String),
}

fn mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64)
}

fn head(id: &str, n: usize) -> String {
    id.chars().take(n).collect()
}

/// An opencode id starts with its time, so its end tells sessions apart.
fn tail(id: &str, n: usize) -> String {
    let chars: Vec<char> = id.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}

/// Claude Code names a project folder after the directory, every character that is not
/// a letter or a digit turned into `-`.
fn claude_folder(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|c| {
            let n = if c.is_ascii_alphanumeric() {
                1
            } else {
                c.len_utf16()
            };
            std::iter::repeat_n(if c.is_ascii_alphanumeric() { c } else { '-' }, n)
        })
        .collect()
}

/// The session files of the folders that can hold this repo: the repo's own name as a
/// prefix (the repo, anything inside it, and `<repo>-` siblings) and each live worktree.
/// Which session really belongs is decided from the cwd inside the file.
fn claude_files(repo: &Repo, base: &Path) -> Vec<Source> {
    let mut prefixes = Vec::new();
    for p in [&repo.root].into_iter().chain(&repo.worktrees) {
        prefixes.push(claude_folder(p));
        if let Ok(real) = std::fs::canonicalize(p) {
            prefixes.push(claude_folder(&real));
        }
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            continue;
        }
        let Ok(files) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_some_and(|e| e == "jsonl") && path.is_file() {
                out.push(Source {
                    mtime: mtime(&path),
                    kind: Kind::Claude(path),
                });
            }
        }
    }
    out
}

/// Every Codex rollout file (`YYYY/MM/DD/rollout-*.jsonl`); which belong is read from
/// each file's first line, only when the walk reaches it.
fn codex_files(base: &Path) -> Vec<Source> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                out.push(Source {
                    mtime: mtime(&path),
                    kind: Kind::Codex(path),
                });
            }
        }
    }
    out
}

/// Each line of a file, read into one reused buffer: transcripts run to tens of
/// megabytes, and only a few of their lines are ever parsed.
fn each_line(path: &Path, mut f: impl FnMut(&[u8]) -> bool) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if !f(&buf) {
                    return;
                }
            }
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// A Claude Code session: its cwd from the first line that has one, then the lines a
/// person typed. Headless runs (`sdk-cli`), subagent lines and compaction summaries are
/// not a person typing.
fn read_claude(path: &Path, mtime: i64, member: &mut Membership) -> Vec<Typed> {
    let session = path
        .file_stem()
        .map(|s| head(&s.to_string_lossy(), 8))
        .unwrap_or_default();
    let mut placed = false;
    let mut out = Vec::new();
    each_line(path, |line| {
        if !placed {
            if !contains(line, b"\"cwd\"") {
                return true;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return true;
            };
            let Some(cwd) = v.get("cwd").and_then(Value::as_str) else {
                return true;
            };
            if v.get("entrypoint").and_then(Value::as_str) == Some("sdk-cli")
                || !member.belongs(Path::new(cwd))
            {
                return false;
            }
            placed = true;
        }
        let probe = &line[..line.len().min(512)];
        if !contains(probe, b"\"type\":\"user\"") {
            return true;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return true;
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool) == Some(true);
        let origin = v
            .get("origin")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str);
        if flag("isSidechain") || flag("isCompactSummary") || origin.is_some_and(|k| k != "human") {
            return true;
        }
        if let Some(Message {
            role: Role::User,
            text,
            ts,
        }) = claude::route(&v)
        {
            out.push(Typed {
                ts: ts.unwrap_or(mtime),
                product: Product::Claude,
                session: session.clone(),
                text,
            });
        }
        true
    });
    out
}

/// A Codex session started by a person (`source` `cli` or `vscode`; review runs and
/// subagents start their own): the `UserMessage` items. A file with only `user_message`
/// events, or none, yields nothing; the other side's `response_item` copies carry the
/// injected instructions, so they are never used here.
fn read_codex(path: &Path, mtime: i64, member: &mut Membership) -> Vec<Typed> {
    let mut session = None;
    let mut out = Vec::new();
    each_line(path, |line| {
        let Some(session) = &session else {
            let Ok(v) = serde_json::from_slice::<Value>(line) else {
                return false;
            };
            let p = v.get("payload");
            let get = |k: &str| p.and_then(|p| p.get(k)).and_then(Value::as_str);
            let by_person = matches!(get("source"), Some("cli" | "vscode"));
            match get("cwd") {
                Some(cwd) if by_person && member.belongs(Path::new(cwd)) => {
                    session = Some(head(get("id").unwrap_or(""), 8));
                    return true;
                }
                _ => return false,
            }
        };
        if !contains(line, b"UserMessage") {
            return true;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            return true;
        };
        let mut said = Vec::new();
        codex::route_line(&v, &mut said, &mut Vec::new());
        for m in said.into_iter().filter(|m| m.role == Role::User) {
            // Codex can log one message twice; the copy says nothing new.
            if out
                .last()
                .is_some_and(|l: &Typed| l.text == m.text && Some(l.ts) == m.ts)
            {
                continue;
            }
            out.push(Typed {
                ts: m.ts.unwrap_or(mtime),
                product: Product::Codex,
                session: session.clone(),
                text: m.text,
            });
        }
        true
    });
    out
}

/// Whether a session's directory is this repo's, remembered per directory.
struct Membership<'a> {
    repo: &'a Repo,
    roots: Vec<PathBuf>,
    common: PathBuf,
    seen: HashMap<PathBuf, bool>,
}

impl<'a> Membership<'a> {
    fn new(repo: &'a Repo) -> Self {
        let mut roots = Vec::new();
        for p in [&repo.root].into_iter().chain(&repo.worktrees) {
            roots.push(p.clone());
            roots.push(resolved(p));
        }
        Membership {
            repo,
            roots,
            common: resolved(&repo.common),
            seen: HashMap::new(),
        }
    }

    fn belongs(&mut self, cwd: &Path) -> bool {
        if let Some(&known) = self.seen.get(cwd) {
            return known;
        }
        let answer = self.decide(cwd);
        self.seen.insert(cwd.to_path_buf(), answer);
        answer
    }

    /// In when the directory is the repo, inside it, a live worktree, or a `<repo>-`
    /// sibling; then a directory that still exists must be in this repo's git (not a
    /// nested or sibling repo of its own), and one that is gone counts as a deleted
    /// worktree. `ponytail:` a deleted sibling that was another repo counts too; rare,
    /// and only text is read.
    fn decide(&self, cwd: &Path) -> bool {
        let real = resolved(cwd);
        let near = [cwd, real.as_path()].iter().any(|p| {
            self.roots.iter().any(|r| p.starts_with(r))
                || [&self.repo.root, &resolved(&self.repo.root)]
                    .iter()
                    .any(|r| p.ancestors().any(|a| sibling(a, r)))
        });
        if !near {
            return false;
        }
        if !real.exists() {
            return true;
        }
        git_dir(&real).is_some_and(|g| g.starts_with(&self.common))
    }
}

/// `<repo>-anything` beside the repo: where worktrees are made by default.
fn sibling(path: &Path, root: &Path) -> bool {
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return false;
    };
    path.parent() == Some(parent)
        && path.file_name().is_some_and(|n| {
            n.to_string_lossy()
                .starts_with(&format!("{}-", name.to_string_lossy()))
        })
}

/// A path with its symlinks resolved; for one that is gone, its nearest existing
/// ancestor resolved and the rest kept, so `/tmp/gone` and `/private/tmp/gone` agree.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut rest = Vec::new();
    let mut at = path;
    while let Some(parent) = at.parent() {
        if let Some(name) = at.file_name() {
            rest.push(name.to_os_string());
        }
        if let Ok(real) = std::fs::canonicalize(parent) {
            return rest.iter().rev().fold(real, |acc, n| acc.join(n));
        }
        at = parent;
    }
    path.to_path_buf()
}

/// The git dir a directory works in: the nearest `.git` above it, a directory as is, a
/// worktree's `.git` file followed to its `gitdir:`. Resolved, so it compares with the
/// common dir.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let mut at = Some(dir);
    while let Some(d) = at {
        let dot = d.join(".git");
        if dot.is_dir() {
            return std::fs::canonicalize(dot).ok();
        }
        if dot.is_file() {
            let text = std::fs::read_to_string(&dot).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            return std::fs::canonicalize(d.join(target)).ok();
        }
        at = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn user(cwd: &Path, ts: &str, text: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"user","cwd":{},"timestamp":"{ts}","message":{{"role":"user","content":{}}}{extra}}}"#,
            serde_json::to_string(&cwd.to_string_lossy()).unwrap(),
            serde_json::to_string(text).unwrap()
        ) + "\n"
    }

    fn claude_session(base: &Path, cwd: &Path, id: &str, lines: &[String]) {
        write(
            &base.join(claude_folder(cwd)).join(format!("{id}.jsonl")),
            &lines.concat(),
        );
    }

    fn codex_session(base: &Path, name: &str, cwd: &Path, source: &str, text: &str) {
        let meta = format!(
            r#"{{"type":"session_meta","payload":{{"id":"{name}-0000","cwd":{},"source":"{source}"}}}}"#,
            serde_json::to_string(&cwd.to_string_lossy()).unwrap()
        );
        let said = format!(
            r#"{{"type":"event_msg","timestamp":"2026-10-03T10:00:00Z","payload":{{"type":"item_completed","item":{{"type":"UserMessage","content":[{{"type":"text","text":{}}}]}}}}}}"#,
            serde_json::to_string(text).unwrap()
        );
        write(
            &base.join(format!("2026/10/03/rollout-{name}.jsonl")),
            &format!("{meta}\n{said}\n"),
        );
    }

    fn opencode_db(path: &Path, rows: &[(&str, &Path, Option<&str>, &str)]) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session(id TEXT PRIMARY KEY, directory TEXT, parent_id TEXT, time_updated INTEGER);
             CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, data TEXT);",
        )
        .unwrap();
        for (i, (id, dir, parent, text)) in rows.iter().enumerate() {
            let ts = 1_791_100_000_000i64 + i as i64;
            conn.execute(
                "INSERT INTO session VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, dir.to_string_lossy(), parent, ts],
            )
            .unwrap();
            for (role, j) in [("user", 0), ("assistant", 1)] {
                let mid = format!("{id}-m{j}");
                conn.execute(
                    "INSERT INTO message VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![mid, id, ts + j, format!(r#"{{"role":"{role}"}}"#)],
                )
                .unwrap();
                let part = serde_json::json!({"type": "text", "text": format!("{role}: {text}")});
                conn.execute(
                    "INSERT INTO part VALUES (?1, ?2, ?3)",
                    rusqlite::params![format!("{mid}-p"), mid, part.to_string()],
                )
                .unwrap();
                let synthetic = serde_json::json!({"type": "text", "text": "added by opencode", "synthetic": true});
                conn.execute(
                    "INSERT INTO part VALUES (?1, ?2, ?3)",
                    rusqlite::params![format!("{mid}-q"), mid, synthetic.to_string()],
                )
                .unwrap();
            }
        }
    }

    /// The repo, a file inside it, a live worktree elsewhere, a deleted `<repo>-` worktree:
    /// in. A sibling repo, a nested repo, another project, a review run, a subagent and
    /// every kind of line nobody typed: out.
    #[test]
    fn only_what_a_person_typed_in_this_repo_and_its_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let top = std::fs::canonicalize(tmp.path()).unwrap();
        let root = top.join("code/app");
        let common = root.join(".git");
        std::fs::create_dir_all(common.join("worktrees/scratch")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let scratch = top.join("scratch/app-fix");
        write(
            &scratch.join(".git"),
            &format!("gitdir: {}\n", common.join("worktrees/scratch").display()),
        );
        let gone = top.join("code/app-wt-old");
        let sibling = top.join("code/app-other");
        std::fs::create_dir_all(sibling.join(".git")).unwrap();
        let nested = root.join("ref/lib");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        let elsewhere = top.join("code/web");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let home = top.join("home");
        let claude = home.join(".claude/projects");
        claude_session(
            &claude,
            &root,
            "aaaaaaaa-1",
            &[
                r#"{"type":"attachment"}"#.to_string() + "\n",
                user(&root, "2026-10-01T10:00:00Z", "run it on staging first", ""),
                user(
                    &root,
                    "2026-10-01T10:01:00Z",
                    "<command-name>/qass</command-name>",
                    "",
                ),
                user(
                    &root,
                    "2026-10-01T10:02:00Z",
                    "<task-notification>done</task-notification>",
                    "",
                ),
                user(
                    &root,
                    "2026-10-01T10:03:00Z",
                    "This session is being continued from a previous one",
                    "",
                ),
                user(
                    &root,
                    "2026-10-01T10:04:00Z",
                    "summary",
                    r#","isCompactSummary":true"#,
                ),
                user(
                    &root,
                    "2026-10-01T10:05:00Z",
                    "from a subagent",
                    r#","isSidechain":true"#,
                ),
                user(
                    &root,
                    "2026-10-01T10:06:00Z",
                    "a hook said this",
                    r#","origin":{"kind":"task-notification"}"#,
                ),
                user(&root, "2026-10-01T10:07:00Z", "/compact", ""),
                user(
                    &root,
                    "2026-10-01T10:08:00Z",
                    "Another Claude session sent this",
                    "",
                ),
                user(
                    &root,
                    "2026-10-01T10:09:00Z",
                    "test it in a real browser",
                    r#","origin":{"kind":"human"}"#,
                ),
            ],
        );
        claude_session(
            &claude,
            &root.join("src"),
            "bbbbbbbb-2",
            &[user(
                &root.join("src"),
                "2026-10-02T10:00:00Z",
                "inside the repo counts",
                "",
            )],
        );
        claude_session(
            &claude,
            &scratch,
            "cccccccc-3",
            &[user(
                &scratch,
                "2026-10-02T11:00:00Z",
                "a live worktree elsewhere counts",
                "",
            )],
        );
        claude_session(
            &claude,
            &gone,
            "dddddddd-4",
            &[user(
                &gone,
                "2026-10-02T12:00:00Z",
                "a deleted worktree counts",
                "",
            )],
        );
        claude_session(
            &claude,
            &gone.join("crates/x"),
            "dddddddd-9",
            &[user(
                &gone.join("crates/x"),
                "2026-10-02T12:30:00Z",
                "inside a deleted worktree counts",
                "",
            )],
        );
        claude_session(
            &claude,
            &sibling,
            "eeeeeeee-5",
            &[user(
                &sibling,
                "2026-10-02T13:00:00Z",
                "a sibling repo does not",
                "",
            )],
        );
        claude_session(
            &claude,
            &nested,
            "ffffffff-6",
            &[user(
                &nested,
                "2026-10-02T14:00:00Z",
                "a nested repo does not",
                "",
            )],
        );
        claude_session(
            &claude,
            &root,
            "gggggggg-7",
            &[
                r#"{"type":"user","cwd":"x","entrypoint":"sdk-cli"}"#.replace(
                    "\"x\"",
                    &serde_json::to_string(&root.to_string_lossy()).unwrap(),
                ) + "\n",
                user(&root, "2026-10-02T15:00:00Z", "a headless run does not", ""),
            ],
        );

        let codex = home.join(".codex/sessions");
        codex_session(&codex, "c1", &root, "cli", "codex: ship it to staging");
        codex_session(&codex, "c2", &root, "exec", "You are reviewing a diff");
        codex_session(&codex, "c3", &elsewhere, "cli", "another project");

        let db = home.join("opencode.db");
        opencode_db(
            &db,
            &[
                ("ses_aaaaaaaa1111", &root, None, "check the logs"),
                (
                    "ses_aaaaaaaa2222",
                    &root,
                    Some("ses_aaaaaaaa1111"),
                    "a child session",
                ),
                ("ses_aaaaaaaa3333", &elsewhere, None, "another project"),
            ],
        );

        let repo = Repo {
            root: root.clone(),
            common,
            worktrees: vec![scratch],
        };
        let dirs = BaseDirs {
            claude,
            codex,
            opencode: Some(db),
            gemini: home.join(".gemini/tmp"),
        };
        let got: Vec<String> = typed_in(&repo, &dirs, 50)
            .messages
            .into_iter()
            .map(|m| format!("{} {} {}", m.product.as_str(), m.session, m.text))
            .collect();
        let mut want = vec![
            "claude aaaaaaaa run it on staging first",
            "claude aaaaaaaa test it in a real browser",
            "claude bbbbbbbb inside the repo counts",
            "claude cccccccc a live worktree elsewhere counts",
            "claude dddddddd a deleted worktree counts",
            "claude dddddddd inside a deleted worktree counts",
            "codex c1-0000 codex: ship it to staging",
            "opencode aaaa1111 user: check the logs",
        ];
        let mut sorted = got.clone();
        sorted.sort();
        want.sort();
        assert_eq!(sorted, want, "{got:#?}");

        // The newest two, oldest first: the bound keeps the newest, the order is time.
        let newest: Vec<String> = typed_in(&repo, &dirs, 2)
            .messages
            .into_iter()
            .map(|m| m.text)
            .collect();
        assert_eq!(
            newest,
            ["codex: ship it to staging", "user: check the logs"]
        );
    }

    #[test]
    fn a_day_from_epoch_millis() {
        assert_eq!(crate::date_utc(0), "1970-01-01");
        assert_eq!(crate::date_utc(1_759_752_000_000), "2025-10-06");
        assert_eq!(crate::date_utc(951_782_400_000), "2000-02-29");
    }
}

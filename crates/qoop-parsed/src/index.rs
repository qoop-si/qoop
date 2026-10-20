//! Every agent session on this computer, cut down to what a history list and an activity
//! grid need: where it ran, what it was called, and its tokens and prompts per half hour.
//!
//! A full read of a large history takes about a minute (19k sessions, 53 s on one thread), so
//! each session is summarized once and kept with the size and modification time it had; a
//! refresh parses only what changed since. Nothing here knows of a daemon: the caller decides
//! when to refresh, where the file lives and what goes to a client.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub use agentparse::Roots;
use agentparse::{Agent, EntryKind, Session, SessionRef, Source};
use serde::{Deserialize, Serialize};

/// The width of a bucket: every whole and half-hour time zone has its midnight on an edge.
/// `ponytail:` +5:45 and +12:45 are off by a quarter hour at a day's edge, and the asker sends
/// one offset for the whole range, so days on the other side of a DST change are an hour off;
/// quarter-hour buckets and an offset per day if that matters.
pub const BUCKET_MS: i64 = 1_800_000;
const DAY_MS: i64 = 86_400_000;
/// A title is one line of a list.
const TITLE_MOST: usize = 120;
/// Bumped when `Summary` changes meaning: an older file is then read again from scratch.
const VERSION: u32 = 2;

/// One session as the lists need it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Where it was read from: unique, unlike `id` (a resumed or copied session repeats it).
    pub key: String,
    pub agent: Agent,
    pub id: String,
    /// A subagent or child thread: its tokens count, it is never listed on its own.
    pub child: bool,
    /// Started by a program, not a person (`claude -p`, `codex exec`): its tokens count, it is
    /// not a conversation to come back to, so it is never listed.
    #[serde(default)]
    pub headless: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The product's title, else the first typed message; redacted, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Messages a person typed.
    pub prompts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    /// `Usage::total`: prompt, cache and output tokens.
    pub tokens: u64,
    /// `(epoch ms / BUCKET_MS, tokens, prompts)`, oldest first.
    pub buckets: Vec<(i64, u64, u32)>,
}

/// A session's tokens and typed messages, each in the half hour it happened. Usage no entry
/// carries a time for (a session-level total, an old format) goes to the session's last write.
pub fn summarize(key: String, session: &Session, child: bool) -> Summary {
    let mut at: BTreeMap<i64, (u64, u32)> = BTreeMap::new();
    let mut placed = 0u64;
    let mut prompts = 0u32;
    for entry in &session.entries {
        let (usage, typed) = match &entry.kind {
            EntryKind::Message(m) => (m.usage.as_ref(), m.is_human()),
            EntryKind::Compaction(c) => (c.usage.as_ref(), false),
            _ => continue,
        };
        prompts += u32::from(typed);
        let Some(ts) = entry.ts else { continue };
        let tokens = usage.map_or(0, |u| u.total());
        if tokens == 0 && !typed {
            continue;
        }
        placed = placed.saturating_add(tokens);
        let slot = at.entry(ts.div_euclid(BUCKET_MS)).or_default();
        slot.0 = slot.0.saturating_add(tokens);
        slot.1 += u32::from(typed);
    }
    let tokens = session.usage.total();
    let rest = tokens.saturating_sub(placed);
    if let Some(last) = session.updated_at.or(session.started_at)
        && rest > 0
    {
        at.entry(last.div_euclid(BUCKET_MS)).or_default().0 += rest;
    }
    Summary {
        key,
        agent: session.agent,
        id: session.id.clone(),
        child: child || session.parent.is_some(),
        headless: headless(session),
        cwd: session.cwd.clone(),
        title: session.title.as_deref().and_then(title),
        prompts,
        started_at: session.started_at,
        updated_at: session.updated_at,
        tokens,
        buckets: at.into_iter().map(|(b, (t, p))| (b, t, p)).collect(),
    }
}

/// Whether a program started the session: Claude's `entrypoint` `sdk-cli` (`claude -p`, the
/// SDK), Codex's `source` other than `cli` or `vscode` (`exec`, a review run).
fn headless(session: &Session) -> bool {
    let said = |key: &str| {
        session
            .extra
            .as_ref()
            .and_then(|e| e.get(key))
            .and_then(serde_json::Value::as_str)
    };
    match session.agent {
        Agent::Claude => said("entrypoint") == Some("sdk-cli"),
        Agent::Codex => said("source").is_some_and(|s| !matches!(s, "cli" | "vscode")),
        Agent::Opencode | Agent::Gemini => false,
    }
}

/// A title as a list shows it: secrets masked, on one line, cut at [`TITLE_MOST`].
fn title(raw: &str) -> Option<String> {
    let line = crate::redact(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = line.chars().take(TITLE_MOST).collect();
    (!cut.is_empty()).then_some(cut)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    size: u64,
    mtime: i64,
    /// `None`: the session could not be read; kept so it is not read again until it changes.
    summary: Option<Summary>,
}

/// Every summary, keyed by where it was read from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    version: u32,
    entries: HashMap<String, Cached>,
}

impl Index {
    /// The index saved at `path`; a missing, unreadable or older one is empty.
    pub fn load(path: &Path) -> Index {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Index>(&bytes).ok())
            .filter(|index| index.version == VERSION)
            .unwrap_or_default()
    }

    /// Written whole to a file beside `path`, then renamed over it: a reader never sees half.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&Saved {
            version: VERSION,
            entries: &self.entries,
        })?;
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    /// Finds every session under `roots` and reads the ones that are new or changed, on
    /// `threads` threads. True when anything was added, changed or removed.
    pub fn refresh(&mut self, roots: &Roots, threads: usize) -> bool {
        let (refs, warnings) = agentparse::discover(roots);
        let mut kept = HashMap::with_capacity(refs.len());
        let mut todo = Vec::new();
        for r in refs {
            let key = key_of(&r.source);
            let (size, mtime) = stamp(&r);
            match self.entries.remove(&key) {
                Some(c) if c.size == size && c.mtime == mtime => {
                    kept.insert(key, c);
                }
                _ => todo.push((key, size, mtime, r)),
            }
        }
        // A root that could not be read this time (a warning names a path under it) is not a
        // history that is gone: what it held is kept until a read sees it again. The other
        // roots still forget what left them. A warning under no root keeps everything.
        if !warnings.is_empty() {
            let homes: Vec<String> = [&roots.claude, &roots.codex, &roots.opencode, &roots.gemini]
                .into_iter()
                .flatten()
                .map(|p| p.display().to_string())
                .collect();
            let warned: Vec<&String> = homes
                .iter()
                .filter(|h| warnings.iter().any(|w| w.message.starts_with(h.as_str())))
                .collect();
            let unplaced = warnings
                .iter()
                .any(|w| !homes.iter().any(|h| w.message.starts_with(h.as_str())));
            let held: Vec<String> = self
                .entries
                .keys()
                .filter(|k| unplaced || warned.iter().any(|h| k.starts_with(h.as_str())))
                .cloned()
                .collect();
            for key in held {
                if let Some(c) = self.entries.remove(&key) {
                    kept.insert(key, c);
                }
            }
        }
        let changed = !todo.is_empty() || !self.entries.is_empty();
        kept.extend(read_all(todo, threads));
        self.entries = kept;
        self.version = VERSION;
        changed
    }

    /// The index at `path` brought up to date with this computer's sessions and written back
    /// when anything changed: what `qoop sessions-index` runs, in a process of its own.
    pub fn refresh_file(path: &Path) -> std::io::Result<bool> {
        let mut index = Index::load(path);
        // One thread: a first read at two measured 860 MB at its peak.
        let changed = index.refresh(&Roots::from_env(), 1);
        if changed {
            index.save(path)?;
        }
        Ok(changed)
    }

    /// Every session that could be read.
    pub fn summaries(&self) -> Vec<&Summary> {
        self.entries
            .values()
            .filter_map(|c| c.summary.as_ref())
            .collect()
    }
}

/// `Index` as written, borrowing its entries.
#[derive(Serialize)]
struct Saved<'a> {
    version: u32,
    entries: &'a HashMap<String, Cached>,
}

fn key_of(source: &Source) -> String {
    match source {
        Source::File { path } => path.display().to_string(),
        Source::Sqlite { path, session_id } => format!("{}#{session_id}", path.display()),
    }
}

/// What tells a changed session from a kept one: a file's size and mtime, a database row's
/// own update time.
fn stamp(r: &SessionRef) -> (u64, i64) {
    match &r.source {
        Source::File { path } => {
            let size = std::fs::metadata(path).map_or(0, |m| m.len());
            (size, r.updated_at.unwrap_or(0))
        }
        Source::Sqlite { .. } => (0, r.updated_at.unwrap_or(0)),
    }
}

/// Parses each session on one of `threads` threads. A session that fails, or panics the
/// parser, is kept as unreadable rather than stopping the rest.
fn read_all(todo: Vec<(String, u64, i64, SessionRef)>, threads: usize) -> Vec<(String, Cached)> {
    let next = AtomicUsize::new(0);
    let work = |out: &mut Vec<(String, Cached)>| {
        while let Some((key, size, mtime, r)) = todo.get(next.fetch_add(1, Ordering::Relaxed)) {
            let child = r.parent_id.is_some();
            let summary = catch_unwind(AssertUnwindSafe(|| {
                agentparse::parse(r)
                    .ok()
                    .map(|s| summarize(key.clone(), &s, child))
            }))
            .ok()
            .flatten();
            out.push((
                key.clone(),
                Cached {
                    size: *size,
                    mtime: *mtime,
                    summary,
                },
            ));
        }
    };
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads.max(1))
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    work(&mut out);
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

/// One summary per conversation: a resumed or copied session is a second file with the same
/// `(agent, id)` holding the first one's messages too, so only the newest file of each counts.
pub fn latest<'a>(all: &[&'a Summary]) -> Vec<&'a Summary> {
    let mut newest: HashMap<(Agent, &str), &'a Summary> = HashMap::with_capacity(all.len());
    for s in all {
        let at = |s: &Summary| (s.updated_at.unwrap_or(0), s.key.clone());
        newest
            .entry((s.agent, s.id.as_str()))
            .and_modify(|held| {
                if at(s) > at(held) {
                    *held = s;
                }
            })
            .or_insert(s);
    }
    newest.into_values().collect()
}

/// A conversation a person had and can come back to: typed into, not a subagent, not headless.
fn listed(s: &Summary) -> bool {
    !s.child && !s.headless && s.prompts > 0
}

/// Whether `cwd` is one of `dirs` or inside one. No `dirs`: every directory.
fn under(cwd: Option<&str>, dirs: &[String]) -> bool {
    dirs.is_empty()
        || cwd.is_some_and(|cwd| {
            dirs.iter().any(|d| {
                let d = d.trim_end_matches('/');
                cwd == d
                    || cwd
                        .strip_prefix(d)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
}

/// A listed session and its tokens inside the asked range (all of them without one).
#[derive(Debug, Clone, PartialEq)]
pub struct Listed<'a> {
    pub summary: &'a Summary,
    pub tokens: u64,
    pub prompts: u32,
    /// The newest moment of it inside the range.
    pub last: i64,
}

/// The newest `limit` sessions a person typed into, in `dirs` and active within
/// `[from, to)` epoch ms, newest first. Subagents are never listed. `all` is [`latest`]'s.
pub fn sessions<'a>(
    all: &[&'a Summary],
    dirs: &[String],
    from: Option<i64>,
    to: Option<i64>,
    limit: usize,
) -> Vec<Listed<'a>> {
    let inside = |b: i64| {
        let ms = b * BUCKET_MS;
        from.is_none_or(|f| ms + BUCKET_MS > f) && to.is_none_or(|t| ms < t)
    };
    let mut out: Vec<Listed> = all
        .iter()
        .filter(|s| listed(s) && under(s.cwd.as_deref(), dirs))
        .filter_map(|s| {
            let ranged = from.is_some() || to.is_some();
            let mut listed = Listed {
                summary: s,
                tokens: if ranged { 0 } else { s.tokens },
                prompts: if ranged { 0 } else { s.prompts },
                last: s.updated_at.or(s.started_at).unwrap_or(0),
            };
            if !ranged {
                return Some(listed);
            }
            let hits: Vec<_> = s.buckets.iter().filter(|(b, ..)| inside(*b)).collect();
            let last = hits.last()?;
            listed.tokens = hits.iter().map(|(_, t, _)| t).sum();
            listed.prompts = hits.iter().map(|(.., p)| p).sum();
            listed.last = (last.0 + 1) * BUCKET_MS - 1;
            Some(listed)
        })
        .collect();
    out.sort_by(|a, b| {
        b.last
            .cmp(&a.last)
            .then_with(|| a.summary.key.cmp(&b.summary.key))
    });
    out.truncate(limit);
    out
}

/// One day of work, in the asker's time zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Day {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub tokens: u64,
    pub prompts: u32,
    /// Sessions worked in that day, subagents not counted.
    pub sessions: u32,
    /// The directories with the most tokens that day, at most [`DAY_PROJECTS`].
    pub projects: Vec<DayPart>,
    /// Each agent product that ran that day.
    pub agents: Vec<DayPart>,
}

/// A directory's or an agent's share of a day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayPart {
    pub name: String,
    pub tokens: u64,
    pub sessions: u32,
}

/// How many directories a day names.
pub const DAY_PROJECTS: usize = 5;

#[derive(Default)]
struct Acc {
    tokens: u64,
    prompts: u32,
    sessions: HashSet<usize>,
    projects: HashMap<String, (u64, HashSet<usize>)>,
    agents: HashMap<&'static str, (u64, HashSet<usize>)>,
}

/// The last `days` days up to `now`, in a zone `tz_offset_min` minutes east of UTC, oldest
/// first; a day with nothing is left out. `all` is [`latest`]'s. A day's sessions are the ones
/// [`sessions`] would list for it: typed into, not subagents.
pub fn activity(all: &[&Summary], days: u32, tz_offset_min: i32, now: i64) -> Vec<Day> {
    let offset = i64::from(tz_offset_min) * 60_000;
    let today = (now + offset).div_euclid(DAY_MS);
    let first = today - i64::from(days.max(1)) + 1;
    let mut by_day: BTreeMap<i64, Acc> = BTreeMap::new();
    for (n, s) in all.iter().enumerate() {
        for &(b, tokens, prompts) in &s.buckets {
            let day = (b * BUCKET_MS + offset).div_euclid(DAY_MS);
            if day < first || day > today {
                continue;
            }
            let acc = by_day.entry(day).or_default();
            acc.tokens = acc.tokens.saturating_add(tokens);
            acc.prompts += prompts;
            let listed = listed(s);
            if listed {
                acc.sessions.insert(n);
            }
            let add = |part: &mut (u64, HashSet<usize>)| {
                part.0 = part.0.saturating_add(tokens);
                if listed {
                    part.1.insert(n);
                }
            };
            if let Some(cwd) = &s.cwd {
                add(acc.projects.entry(cwd.clone()).or_default());
            }
            add(acc.agents.entry(s.agent.as_str()).or_default());
        }
    }
    by_day
        .into_iter()
        .map(|(day, acc)| Day {
            day: qoop_history::date_utc(day * DAY_MS),
            tokens: acc.tokens,
            prompts: acc.prompts,
            sessions: acc.sessions.len() as u32,
            projects: ranked(acc.projects, DAY_PROJECTS),
            agents: ranked(acc.agents, usize::MAX),
        })
        .collect()
}

/// The parts with the most tokens first, at most `most`.
fn ranked<K: ToString>(parts: HashMap<K, (u64, HashSet<usize>)>, most: usize) -> Vec<DayPart> {
    let mut out: Vec<DayPart> = parts
        .into_iter()
        .map(|(name, (tokens, sessions))| DayPart {
            name: name.to_string(),
            tokens,
            sessions: sessions.len() as u32,
        })
        .collect();
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.name.cmp(&b.name)));
    out.truncate(most);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    /// 2026-01-20T14:00:00Z.
    const T0: i64 = 1_768_917_600_000;

    fn line(kind: &str, ts: i64, cwd: &str, body: serde_json::Value) -> String {
        let at = format_ts(ts);
        let mut v = serde_json::json!({
            "type": kind, "cwd": cwd, "sessionId": "s1", "uuid": format!("u{ts}"),
            "parentUuid": null, "timestamp": at, "message": body,
        });
        v["isSidechain"] = false.into();
        v.to_string()
    }

    fn format_ts(ms: i64) -> String {
        let day = qoop_history::date_utc(ms);
        let s = ms.rem_euclid(DAY_MS) / 1000;
        format!(
            "{day}T{:02}:{:02}:{:02}.000Z",
            s / 3600,
            s / 60 % 60,
            s % 60
        )
    }

    fn user(ts: i64, cwd: &str, text: &str) -> String {
        line(
            "user",
            ts,
            cwd,
            serde_json::json!({"role": "user", "content": text}),
        )
    }

    fn reply(ts: i64, cwd: &str, n: u64) -> String {
        let usage = serde_json::json!({"input_tokens": n, "output_tokens": n, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
        line(
            "assistant",
            ts,
            cwd,
            serde_json::json!({"id": format!("m{ts}"), "role": "assistant", "model": "m", "content": [{"type": "text", "text": "ok"}], "usage": usage}),
        )
    }

    /// A Claude root with one session per `(slug, id, lines)`.
    fn claude_root(sessions: &[(&str, &str, Vec<String>)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (slug, id, lines) in sessions {
            let dir = root.path().join("projects").join(slug);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
        }
        root
    }

    fn roots(claude: &Path) -> Roots {
        Roots {
            claude: Some(claude.to_path_buf()),
            ..Roots::default()
        }
    }

    #[test]
    fn tokens_and_prompts_land_in_the_half_hour_they_happened() {
        let root = claude_root(&[(
            "-w-a",
            "11111111-1111-4111-8111-111111111111",
            vec![
                user(
                    T0,
                    "/w/a",
                    "fix the test sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789",
                ),
                reply(T0 + 1000, "/w/a", 10),
                user(T0 + 2 * HOUR, "/w/a", "and again"),
                reply(T0 + 2 * HOUR + 1000, "/w/a", 5),
            ],
        )]);
        let mut index = Index::default();
        assert!(index.refresh(&roots(root.path()), 2));
        let all = index.summaries();
        assert_eq!(all.len(), 1);
        let s = &all[0];
        assert_eq!(
            (s.prompts, s.tokens, s.cwd.as_deref()),
            (2, 30, Some("/w/a"))
        );
        let b0 = T0 / BUCKET_MS;
        assert_eq!(s.buckets, vec![(b0, 20, 1), (b0 + 4, 10, 1)]);
        // The key in the first message is never cached or sent.
        let title = s.title.as_deref().unwrap();
        assert!(
            title.starts_with("fix the test") && !title.contains("abcdefghij"),
            "{title}"
        );
    }

    #[test]
    fn a_headless_run_counts_its_tokens_but_is_never_listed() {
        let mut headless = user(T0, "/w/a", "review this diff");
        headless = headless.replace(
            "\"isSidechain\":false",
            "\"isSidechain\":false,\"entrypoint\":\"sdk-cli\"",
        );
        let root = claude_root(&[(
            "-w-a",
            "33333333-3333-4333-8333-333333333333",
            vec![headless, reply(T0 + 1000, "/w/a", 10)],
        )]);
        let mut index = Index::default();
        index.refresh(&roots(root.path()), 1);
        let all = index.summaries();
        assert!(all[0].headless, "{:?}", all[0]);
        assert!(sessions(&all, &[], None, None, 10).is_empty());
        let day = &activity(&all, 7, 0, T0 + DAY_MS)[0];
        assert_eq!((day.tokens, day.sessions), (20, 0));
    }

    #[test]
    fn a_refresh_reads_only_what_changed_and_forgets_what_is_gone() {
        let root = claude_root(&[
            (
                "-w-a",
                "11111111-1111-4111-8111-111111111111",
                vec![user(T0, "/w/a", "one")],
            ),
            (
                "-w-b",
                "22222222-2222-4222-8222-222222222222",
                vec![user(T0, "/w/b", "two")],
            ),
        ]);
        let r = roots(root.path());
        let file = root.path().join("index.json");
        let mut index = Index::default();
        assert!(index.refresh(&r, 1));
        index.save(&file).unwrap();
        let mut again = Index::load(&file);
        assert!(!again.refresh(&r, 1), "nothing changed, nothing read");
        std::fs::remove_file(
            root.path()
                .join("projects/-w-b/22222222-2222-4222-8222-222222222222.jsonl"),
        )
        .unwrap();
        assert!(again.refresh(&r, 1));
        assert_eq!(again.summaries().len(), 1);
        std::fs::write(
            root.path().join("index.json"),
            b"{\"version\":0,\"entries\":{}}",
        )
        .unwrap();
        assert!(
            Index::load(&file).entries.is_empty(),
            "an older file starts over"
        );
    }

    #[test]
    fn a_root_that_cannot_be_read_this_time_keeps_what_it_held() {
        use std::os::unix::fs::PermissionsExt;
        let root = claude_root(&[(
            "-w-a",
            "11111111-1111-4111-8111-111111111111",
            vec![user(T0, "/w/a", "one")],
        )]);
        let r = roots(root.path());
        let mut index = Index::default();
        index.refresh(&r, 1);
        let projects = root.path().join("projects");
        std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o000)).unwrap();
        index.refresh(&r, 1);
        std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            index.summaries().len(),
            1,
            "an unreadable history is not an empty one"
        );
    }

    fn summary(key: &str, cwd: &str, buckets: Vec<(i64, u64, u32)>, child: bool) -> Summary {
        Summary {
            key: key.into(),
            agent: Agent::Claude,
            id: key.into(),
            child,
            headless: false,
            cwd: Some(cwd.into()),
            title: None,
            prompts: buckets.iter().map(|b| b.2).sum(),
            started_at: buckets.first().map(|b| b.0 * BUCKET_MS),
            updated_at: buckets.last().map(|b| b.0 * BUCKET_MS),
            tokens: buckets.iter().map(|b| b.1).sum(),
            buckets,
        }
    }

    #[test]
    fn sessions_match_a_directory_and_its_insides_newest_first_without_subagents() {
        let b = T0 / BUCKET_MS;
        let all = [
            summary("old", "/w/a", vec![(b, 5, 1)], false),
            summary("new", "/w/a/sub", vec![(b + 10, 7, 1)], false),
            summary("next", "/w/ab", vec![(b + 20, 1, 1)], false),
            summary("kid", "/w/a", vec![(b + 30, 9, 0)], true),
        ];
        let refs: Vec<&Summary> = all.iter().collect();
        let keys = |v: Vec<Listed>| v.iter().map(|l| l.summary.key.clone()).collect::<Vec<_>>();
        assert_eq!(
            keys(sessions(&refs, &["/w/a/".into()], None, None, 10)),
            ["new", "old"]
        );
        assert_eq!(keys(sessions(&refs, &[], None, None, 1)), ["next"]);
        let day = sessions(&refs, &[], Some(T0), Some(T0 + BUCKET_MS), 10);
        assert_eq!(
            (keys(day.clone()), day[0].tokens),
            (vec!["old".to_string()], 5)
        );
    }

    #[test]
    fn a_resumed_copy_counts_once_as_its_newest_file() {
        let b = T0 / BUCKET_MS;
        let mut copy = summary("copy", "/w/a", vec![(b, 5, 1), (b + 2, 3, 1)], false);
        copy.id = "same".into();
        let mut first = summary("first", "/w/a", vec![(b, 5, 1)], false);
        first.id = "same".into();
        let all = [first, copy];
        let refs: Vec<&Summary> = all.iter().collect();
        let one = latest(&refs);
        assert_eq!(
            one.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            ["copy"]
        );
        assert_eq!(activity(&one, 7, 0, T0 + DAY_MS)[0].tokens, 8);
    }

    #[test]
    fn days_follow_the_askers_zone_and_name_their_projects_and_agents() {
        // 23:30 UTC is the next day three hours east.
        let late = (T0 - 14 * HOUR + 23 * HOUR + HOUR / 2) / BUCKET_MS;
        let all = [
            summary("a", "/w/a", vec![(late, 100, 1)], false),
            summary("k", "/w/a", vec![(late, 50, 0)], true),
        ];
        let refs: Vec<&Summary> = all.iter().collect();
        let utc = activity(&refs, 7, 0, T0 + DAY_MS);
        assert_eq!(
            utc.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(),
            ["2026-01-20"]
        );
        let east = activity(&refs, 7, 180, T0 + DAY_MS);
        let d = &east[0];
        assert_eq!(
            (d.day.as_str(), d.tokens, d.sessions, d.prompts),
            ("2026-01-21", 150, 1, 1)
        );
        assert_eq!(
            d.projects,
            vec![DayPart {
                name: "/w/a".into(),
                tokens: 150,
                sessions: 1
            }]
        );
        assert_eq!(d.agents[0].name, "claude");
        assert!(
            activity(&refs, 1, 0, T0 + 3 * DAY_MS).is_empty(),
            "outside the range"
        );
    }
}

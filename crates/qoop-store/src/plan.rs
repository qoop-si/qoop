//! An item's plan and qass taking work from Todo by itself.
//!
//! A plan is a file the worker on an item wrote and had reviewed. It is recorded as item
//! events, read newest first like the lane, so nothing is stored that the record could
//! contradict: `planfile` (path, hash, the terminal that linked it, its summary), `approved`
//! (by qass or the owner, for one hash), `show-me` and `show-me-off` (the owner asked to see
//! this item's plan). An approval holds for the hash it names: a rewritten file reads
//! `changed` until it is approved again.
//!
//! Pickup (`next_item`) names the Todo item qass should start, and with `take` writes its
//! start in the same transaction, so two calls never name the same item.

use rusqlite::{Connection, OptionalExtension, params};

use crate::item::{ItemState, is_item};
use crate::task::{TaskError, event, one_line, printable, touch, within};
use crate::{Store, StoreError};

/// The longest plan summary kept, in characters.
pub const SUMMARY_MAX: usize = 600;
/// How long a plan may wait for an approval, with no card open on its item, before the
/// owner is told it waits.
pub const STALE_SECS: i64 = 1_800;
/// How old a Todo item must be before qass takes it: a half-written line gets its edits.
pub const SETTLE_SECS: i64 = 600;
/// Items at work at once before qass takes another.
// ponytail: one cap for the whole machine; a cap per space if one repo gets crowded.
pub const QASS_WORKERS: usize = 3;
/// Starts that never got an agent linked before an item is left for the owner.
pub const DEAD_STARTS: usize = 2;

/// FNV-1a, 64 bits, as hex: tells one version of a plan file from another.
// ponytail: a change detector, not a security hash; stable across Rust releases, which
// `DefaultHasher` is not, and no new crate for it.
pub fn plan_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Who approved a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovedBy {
    Qass,
    Owner,
}

impl ApprovedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qass => "qass",
            Self::Owner => "owner",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "qass" => Some(Self::Qass),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

/// Where an item's plan is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanState {
    /// No plan file linked.
    None,
    /// Linked, not approved.
    Written,
    /// Linked, not approved, no card open on the item, for longer than [`STALE_SECS`].
    Stale,
    /// Not approved, and qass's plan card is open on the item: it waits on the owner.
    Asked,
    /// Approved for the newest file.
    Approved(ApprovedBy),
    /// Approved for an older version of the file.
    Changed,
}

impl PlanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Written => "written",
            Self::Stale => "stale",
            Self::Asked => "asked",
            Self::Approved(_) => "approved",
            Self::Changed => "changed",
        }
    }

    pub fn by(self) -> Option<ApprovedBy> {
        match self {
            Self::Approved(by) => Some(by),
            _ => None,
        }
    }
}

/// The newest plan file of an item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanFile {
    pub path: String,
    pub hash: String,
    /// The terminal that linked it, where qass sends "approved, build"; 0 when qass linked it.
    pub terminal: i64,
    pub summary: String,
    pub at: i64,
}

/// An item's plan as the board reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanInfo {
    pub state: PlanState,
    pub file: Option<PlanFile>,
    /// The owner asked to see this item's plan before it is built.
    pub show_me: bool,
    /// When its plan last changed: the newest link or approval.
    pub at: Option<i64>,
}

/// A plan that waited too long for qass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StalePlan {
    pub item: i64,
    pub key: String,
    /// The terminal that linked it; 0 when qass did.
    pub terminal: i64,
    pub hash: String,
}

/// What `next_item` answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Next {
    /// The item to start: id, key, title.
    pub item: Option<(i64, String, String)>,
    /// Items at work now (an open agent linked, or a start still waiting for one).
    pub at_work: usize,
    /// Keys left in Todo because [`DEAD_STARTS`] starts never got an agent.
    pub skipped: Vec<String>,
}

fn newest_body(
    tx: &Connection,
    item: i64,
    kinds: &[&str],
) -> rusqlite::Result<Option<(i64, i64, String, String)>> {
    let marks = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT id, at, kind, body FROM task_event WHERE task = ? AND kind IN ({marks}) \
         ORDER BY id DESC LIMIT 1"
    );
    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&item];
    for k in kinds {
        args.push(k);
    }
    tx.query_row(&sql, args.as_slice(), |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .optional()
}

fn plan_file(tx: &Connection, item: i64) -> rusqlite::Result<Option<PlanFile>> {
    Ok(
        newest_body(tx, item, &["planfile"])?.and_then(|(_, at, _, body)| {
            let mut parts = body.splitn(4, '\t');
            let path = parts.next()?.to_string();
            let hash = parts.next()?.to_string();
            let terminal = parts.next()?.parse().ok()?;
            let summary = parts.next().unwrap_or("").to_string();
            Some(PlanFile {
                path,
                hash,
                terminal,
                summary,
                at,
            })
        }),
    )
}

/// A card is on an item: its task is linked to it, or its terminal is the terminal of an open
/// agent linked to it (a screen dialog's draft has no task). The item is `?1`.
const ON_ITEM: &str = "(a.task IN (SELECT task FROM task_item WHERE item = ?1) \
    OR a.terminal IN (SELECT t.terminal FROM task_item l JOIN task t ON t.id = l.task \
                      WHERE l.item = ?1 AND t.closed_at IS NULL AND t.terminal IS NOT NULL))";

/// A card's handle; none for a body that is not JSON.
const HANDLE: &str = "CASE WHEN json_valid(a.body) THEN json_extract(a.body, '$.handle') END";

/// Cards on an item, by [`ON_ITEM`]. Open ones are the questions he has not answered; with
/// `in_flight`, answered ones still on their way (queued, passed) count too.
pub(crate) fn open_cards(tx: &Connection, item: i64, in_flight: bool) -> rusqlite::Result<usize> {
    let statuses = if in_flight {
        "('open', 'queued', 'passed')"
    } else {
        "('open')"
    };
    tx.query_row(
        &format!("SELECT COUNT(*) FROM ask a WHERE a.status IN {statuses} AND {ON_ITEM}"),
        params![item],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
}

/// Open questions on an item: its open cards but qass's plan card, which the plan's own state
/// (`asked`) already tells.
pub(crate) fn open_questions(tx: &Connection, item: i64) -> rusqlite::Result<usize> {
    tx.query_row(
        &format!(
            "SELECT COUNT(*) FROM ask a WHERE a.status = 'open' AND {HANDLE} IS NOT 'plan' AND {ON_ITEM}"
        ),
        params![item],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
}

/// qass's plan card (handle `plan`) is open on the item, by [`ON_ITEM`].
fn plan_card_open(tx: &Connection, item: i64) -> rusqlite::Result<bool> {
    tx.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM ask a WHERE a.status = 'open' AND {HANDLE} = 'plan' AND {ON_ITEM})"
        ),
        params![item],
        |r| r.get(0),
    )
}

/// An item's plan, read from its events.
pub(crate) fn plan_info(tx: &Connection, item: i64, now: i64) -> rusqlite::Result<PlanInfo> {
    let show_me = matches!(
        newest_body(tx, item, &["show-me", "show-me-off"])?,
        Some((_, _, kind, _)) if kind == "show-me"
    );
    let Some(file) = plan_file(tx, item)? else {
        return Ok(PlanInfo {
            state: PlanState::None,
            file: None,
            show_me,
            at: None,
        });
    };
    let newest = newest_body(tx, item, &["approved"])?;
    let at = Some(
        newest
            .as_ref()
            .map_or(file.at, |(_, at, _, _)| (*at).max(file.at)),
    );
    let approved = newest.and_then(|(_, _, _, body)| {
        let (by, hash) = body.split_once('\t')?;
        Some((ApprovedBy::parse(by)?, hash.to_string()))
    });
    // The newest version not approved waits on qass the same way, whether an older one was.
    let waited = now - file.at >= STALE_SECS;
    let state = match approved {
        Some((by, hash)) if hash == file.hash => PlanState::Approved(by),
        _ if plan_card_open(tx, item)? => PlanState::Asked,
        _ if waited && open_cards(tx, item, true)? == 0 => PlanState::Stale,
        Some(_) => PlanState::Changed,
        None => PlanState::Written,
    };
    Ok(PlanInfo {
        state,
        file: Some(file),
        show_me,
        at,
    })
}

/// The open item a plan write names, or why not.
fn open_item_row(tx: &Connection, item: i64) -> Result<(), StoreError> {
    if !is_item(tx, item)? {
        return Err(TaskError::NotAnItem(item).into());
    }
    let closed: Option<i64> = tx.query_row(
        "SELECT closed_at FROM task WHERE id = ?1",
        params![item],
        |r| r.get(0),
    )?;
    if closed.is_some() {
        return Err(TaskError::Closed(item).into());
    }
    Ok(())
}

/// The open item a card is about, by the rule `open_cards` counts by: the card's task is linked
/// to it, or the card's terminal is the terminal of an open agent linked to it.
pub(crate) fn item_of_card(
    tx: &Connection,
    task: Option<i64>,
    terminal: Option<i64>,
) -> rusqlite::Result<Option<(i64, String)>> {
    // An ended card goes by its task only: the caller passes no terminal for one, since a
    // terminal number is given again to a new tab.
    let item: Option<i64> = tx
        .query_row(
            "SELECT l.item FROM task_item l JOIN task i ON i.id = l.item JOIN task t ON t.id = l.task \
             WHERE i.closed_at IS NULL AND (l.task = ?1 OR (t.closed_at IS NULL AND t.terminal = ?2)) \
             ORDER BY l.task = ?1 DESC LIMIT 1",
            params![task, terminal],
            |r| r.get(0),
        )
        .optional()?;
    item.map(|i| Ok((i, crate::item::key_of(tx, i)?)))
        .transpose()
}

impl Store {
    /// The open item a card is about: see [`item_of_card`].
    pub fn item_of_card(
        &self,
        task: Option<i64>,
        terminal: Option<i64>,
    ) -> Result<Option<(i64, String)>, StoreError> {
        Ok(item_of_card(&self.conn, task, terminal)?)
    }

    /// Link a plan file to an open item: its path, the hash of what it says now, the terminal
    /// that linked it (0 for qass) and its summary. Answers the hash.
    pub fn set_planfile(
        &self,
        item: i64,
        path: &str,
        bytes: &[u8],
        terminal: i64,
        now: i64,
    ) -> Result<String, StoreError> {
        // The path as it is, spaces and all: `approve` reads the file by it again. A tab or a
        // line break would split the event.
        let path = path.to_string();
        if path.trim().is_empty() {
            return Err(TaskError::Empty("the plan path").into());
        }
        if path.chars().any(char::is_control) {
            return Err(TaskError::PlanPath.into());
        }
        let text = String::from_utf8_lossy(bytes);
        let summary = one_line(&printable(&summary_of(&text)))
            .chars()
            .take(SUMMARY_MAX)
            .collect::<String>();
        within("the plan path", &path, 1_000)?;
        let hash = plan_hash(bytes);
        let tx = self.conn.unchecked_transaction()?;
        open_item_row(&tx, item)?;
        // qass linking it again (an edit after the worker linked it) keeps the worker's terminal:
        // where the build line goes.
        let terminal = match (terminal, plan_file(&tx, item)?) {
            (0, Some(old)) => old.terminal,
            (t, _) => t,
        };
        event(
            &tx,
            item,
            "planfile",
            &format!("{path}\t{hash}\t{terminal}\t{summary}"),
            now,
        )?;
        touch(&tx, item, now)?;
        tx.commit()?;
        Ok(hash)
    }

    /// Approve an open item's newest plan file, for the hash the approver read. Refused when
    /// there is no plan or the hash is not the newest file's. Answers the terminal to tell.
    pub fn approve_plan(
        &self,
        item: i64,
        by: ApprovedBy,
        hash: &str,
        now: i64,
    ) -> Result<i64, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        open_item_row(&tx, item)?;
        let Some(file) = plan_file(&tx, item)? else {
            return Err(TaskError::NoPlan(item).into());
        };
        // The whole hash, or the 8 or more characters of it a card carries.
        let read = hash.trim();
        if read.len() < 8 || !file.hash.starts_with(read) {
            return Err(TaskError::PlanMoved {
                read: read.to_string(),
                newest: file.hash,
            }
            .into());
        }
        event(
            &tx,
            item,
            "approved",
            &format!("{}\t{}", by.as_str(), file.hash),
            now,
        )?;
        touch(&tx, item, now)?;
        tx.commit()?;
        Ok(file.terminal)
    }

    /// Mark (or unmark) an open item as one whose plan the owner wants to see. Answers
    /// whether it changed.
    pub fn set_show_me(&self, item: i64, on: bool, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        open_item_row(&tx, item)?;
        if plan_info(&tx, item, now)?.show_me == on {
            return Ok(false);
        }
        event(
            &tx,
            item,
            if on { "show-me" } else { "show-me-off" },
            "",
            now,
        )?;
        touch(&tx, item, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// An item's plan as the board reads it.
    pub fn plan_info(&self, item: i64, now: i64) -> Result<PlanInfo, StoreError> {
        Ok(plan_info(&self.conn, item, now)?)
    }

    /// Open items whose plan went stale and the owner was not told of this version yet. The
    /// daemon drafts one card for each (`asks.rs`).
    pub fn stale_plans(&self, now: i64) -> Result<Vec<StalePlan>, StoreError> {
        let items = {
            let mut stmt = self.conn.prepare(
                // From the open items, each looked up by its own index: never a scan of
                // every event, once a minute.
                "SELECT b.task FROM board_item b JOIN task t ON t.id = b.task \
                 WHERE t.closed_at IS NULL AND EXISTS (SELECT 1 FROM task_event e \
                 WHERE e.task = b.task AND e.kind = 'planfile')",
            )?;
            stmt.query_map([], |r| r.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut out = Vec::new();
        for item in items {
            let info = plan_info(&self.conn, item, now)?;
            // The card goes on the agent working on it now: a terminal number kept from the
            // link may belong to another tab by now.
            let agent: Option<i64> = self
                .conn
                .query_row(
                    "SELECT t.terminal FROM task_item l JOIN task t ON t.id = l.task \
                     WHERE l.item = ?1 AND t.closed_at IS NULL AND t.terminal IS NOT NULL \
                     ORDER BY t.id DESC LIMIT 1",
                    params![item],
                    |r| r.get(0),
                )
                .optional()?;
            if info.state == PlanState::Stale
                && let Some(file) = info.file
                && let Some(terminal) = agent
                && newest_body(&self.conn, item, &["stale-card"])?
                    .is_none_or(|(_, _, _, hash)| hash != file.hash)
            {
                out.push(StalePlan {
                    item,
                    key: crate::item::key_of(&self.conn, item)?,
                    terminal,
                    hash: file.hash,
                });
            }
        }
        Ok(out)
    }

    /// Mark that the owner was told an item's plan waits, for this version of it: told once.
    pub fn mark_stale_carded(&self, item: i64, hash: &str, now: i64) -> Result<(), StoreError> {
        event(&self.conn, item, "stale-card", hash, now)?;
        Ok(())
    }

    /// The Todo item qass should start now, or none, and with `take` its start written in
    /// the same transaction. Eligible: Todo, its space not left alone by qass, opened after
    /// qass first took work by itself and at least [`SETTLE_SECS`] ago, fewer than
    /// [`DEAD_STARTS`] starts that never got an agent; first in the board's Todo order. None
    /// while [`QASS_WORKERS`] items are at work. The first `take` sets when pickup began, so
    /// the owner's older Todo items stay his.
    pub fn next_item(&self, now: i64, take: bool) -> Result<Next, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let since: Option<i64> = tx
            .query_row("SELECT since FROM pickup WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        let since = match (since, take) {
            (Some(s), _) => Some(s),
            (None, true) => {
                tx.execute(
                    "INSERT INTO pickup (id, since) VALUES (1, ?1)",
                    params![now],
                )?;
                Some(now)
            }
            (None, false) => None,
        };
        let open: Vec<(i64, i64, String, Option<i64>)> = {
            let mut stmt = tx.prepare(
                "SELECT t.id, t.opened_at, t.goal, t.workspace FROM task t \
                 JOIN board_item b ON b.task = t.id WHERE t.closed_at IS NULL ORDER BY t.id",
            )?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut at_work = 0;
        let mut todo = Vec::new();
        for (id, opened, title, workspace) in open {
            let row = crate::item::item_row_of(&tx, id, now)?;
            let open_agent = row.agents.iter().any(|a| !a.closed && a.remote.is_none());
            let fresh_start = crate::item::fresh_start(&tx, id, now)?;
            if open_agent || fresh_start {
                at_work += 1;
            } else if row.state == ItemState::Todo {
                todo.push((row.updated_at, id, opened, title, workspace, row.key));
            }
        }
        let mut next = Next {
            item: None,
            at_work,
            skipped: Vec::new(),
        };
        // Committed on every way out: the first take sets when pickup began, at the cap too.
        if at_work >= QASS_WORKERS {
            tx.commit()?;
            return Ok(next);
        }
        let Some(since) = since else {
            // Pickup never began: nothing is the owner's surprise yet, and a read sets nothing.
            return Ok(next);
        };
        // The board's own Todo order: the newest change on top.
        todo.sort_by_key(|t| std::cmp::Reverse((t.0, t.1)));
        for (_, id, opened, title, workspace, key) in todo {
            if opened < since || now - opened < SETTLE_SECS {
                continue;
            }
            let left_alone: bool = match workspace {
                Some(ws) => tx
                    .query_row(
                        "SELECT 1 FROM qass_skip_workspace WHERE workspace = ?1",
                        params![ws],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some(),
                None => true,
            };
            if left_alone {
                continue;
            }
            if crate::item::dead_starts(&tx, id)? >= DEAD_STARTS {
                next.skipped.push(key);
                continue;
            }
            if take {
                event(&tx, id, "start", "qass took it from Todo", now)?;
                touch(&tx, id, now)?;
            }
            next.item = Some((id, key, title));
            break;
        }
        tx.commit()?;
        Ok(next)
    }
}

/// What a plan event reads as in an item's activity, or `None` for any other kind.
pub(crate) fn said(kind: &str, body: &str) -> Option<String> {
    Some(match kind {
        "planfile" => {
            let path = body.split('\t').next().unwrap_or("");
            let name = path.rsplit('/').next().unwrap_or(path);
            format!("plan {name}")
        }
        "approved" => format!(
            "plan approved by {}",
            body.split('\t').next().unwrap_or("qass")
        ),
        "show-me" => "the owner asked to see the plan".to_string(),
        "show-me-off" => "the owner no longer needs to see the plan".to_string(),
        "stale-card" => "plan waited 30 minutes: the owner was told".to_string(),
        _ => return None,
    })
}

/// The text under a `## Summary` heading, up to the next heading of that level or higher;
/// else the first paragraph after the title.
pub fn summary_of(text: &str) -> String {
    let mut under = false;
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if under {
            if t.starts_with("## ") || t.starts_with("# ") {
                break;
            }
            if !t.is_empty() {
                out.push(t);
            }
        } else if t.eq_ignore_ascii_case("## summary") {
            under = true;
        }
    }
    if !out.is_empty() {
        return out.join(" ");
    }
    text.lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty() || l.starts_with('#'))
        .take_while(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::START_SECS;

    fn setup() -> (Store, i64, i64, i64) {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("app", 1).unwrap();
        let t = s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap();
        let (item, _) = s.add_item(ws, "Ship the thing", "", 10).unwrap();
        (s, ws, t, item)
    }

    #[test]
    fn hash_is_fnv1a_and_tells_versions_apart() {
        // The published FNV-1a 64 vectors.
        assert_eq!(plan_hash(b""), "cbf29ce484222325");
        assert_eq!(plan_hash(b"a"), "af63dc4c8601ec8c");
        assert_eq!(plan_hash(b"foobar"), "85944171f73967e8");
        assert_ne!(plan_hash(b"plan v1"), plan_hash(b"plan v2"));
    }

    #[test]
    fn summary_reads_its_section_or_the_first_paragraph() {
        let plan = "# Title\n\nIntro line.\n\n## Summary\n\n1. one\n2. two\n\n## Next\nno";
        assert_eq!(summary_of(plan), "1. one 2. two");
        assert_eq!(
            summary_of("# T\n\nfirst para\nstill\n\nsecond"),
            "first para still"
        );
    }

    #[test]
    fn plan_states_follow_the_newest_events() {
        let (s, _, t, item) = setup();
        assert_eq!(s.plan_info(item, 20).unwrap().state, PlanState::None);
        let h1 = s.set_planfile(item, "/p/plan.md", b"v1", t, 20).unwrap();
        assert_eq!(s.plan_info(item, 21).unwrap().state, PlanState::Written);
        // Stale after STALE_SECS with no card open on the item.
        assert_eq!(
            s.plan_info(item, 20 + STALE_SECS).unwrap().state,
            PlanState::Stale
        );
        assert_eq!(s.approve_plan(item, ApprovedBy::Qass, &h1, 30).unwrap(), t);
        assert_eq!(
            s.plan_info(item, 31).unwrap().state,
            PlanState::Approved(ApprovedBy::Qass)
        );
        // A rewritten file is a new version: the approval was for the old one.
        let h2 = s.set_planfile(item, "/p/plan.md", b"v2", t, 40).unwrap();
        assert_eq!(s.plan_info(item, 41).unwrap().state, PlanState::Changed);
        // The owner over qass, for the new version.
        s.approve_plan(item, ApprovedBy::Owner, &h2, 50).unwrap();
        assert_eq!(
            s.plan_info(item, 51).unwrap().state,
            PlanState::Approved(ApprovedBy::Owner)
        );
    }

    #[test]
    fn approval_is_refused_for_a_version_nobody_read() {
        let (s, _, t, item) = setup();
        assert!(matches!(
            s.approve_plan(item, ApprovedBy::Qass, "x", 20),
            Err(StoreError::Task(TaskError::NoPlan(_)))
        ));
        let h1 = s.set_planfile(item, "/p/plan.md", b"v1", t, 20).unwrap();
        s.set_planfile(item, "/p/plan.md", b"v2", t, 21).unwrap();
        assert!(matches!(
            s.approve_plan(item, ApprovedBy::Qass, &h1, 22),
            Err(StoreError::Task(TaskError::PlanMoved { .. }))
        ));
    }

    #[test]
    fn show_me_toggles_and_needs_no_plan() {
        let (s, _, _, item) = setup();
        assert!(s.set_show_me(item, true, 20).unwrap());
        assert!(!s.set_show_me(item, true, 21).unwrap());
        assert!(s.plan_info(item, 22).unwrap().show_me);
        assert!(s.set_show_me(item, false, 23).unwrap());
        assert!(!s.plan_info(item, 24).unwrap().show_me);
    }

    #[test]
    fn an_open_card_on_the_item_keeps_a_plan_from_going_stale() {
        let (s, _, t, item) = setup();
        let task = s.begin_task(t, "work", None, 11).unwrap().0;
        s.link_task(task, item, 12).unwrap();
        s.set_planfile(item, "/p/plan.md", b"v1", t, 20).unwrap();
        s.conn
            .execute(
                "INSERT INTO ask (card, terminal, task, status, body, opened_at) \
                 VALUES ('E1', ?1, ?2, 'open', '', 21)",
                params![t, task],
            )
            .unwrap();
        assert_eq!(
            s.plan_info(item, 20 + STALE_SECS).unwrap().state,
            PlanState::Written
        );
        assert!(s.stale_plans(20 + STALE_SECS).unwrap().is_empty());
        // Answered and on its way (queued) still holds it: the owner spoke, qass acts next.
        s.conn
            .execute("UPDATE ask SET status = 'queued'", [])
            .unwrap();
        assert_eq!(
            s.plan_info(item, 20 + STALE_SECS).unwrap().state,
            PlanState::Written
        );
        // Ended: nothing holds it any more.
        s.conn
            .execute("UPDATE ask SET status = 'closed'", [])
            .unwrap();
        assert_eq!(
            s.plan_info(item, 20 + STALE_SECS).unwrap().state,
            PlanState::Stale
        );
        assert_eq!(s.stale_plans(20 + STALE_SECS).unwrap().len(), 1);
        // Told once per version, even after the card is answered and gone.
        let hash = s.plan_info(item, 20).unwrap().file.unwrap().hash;
        s.mark_stale_carded(item, &hash, 20 + STALE_SECS).unwrap();
        assert!(s.stale_plans(30 + STALE_SECS).unwrap().is_empty());
        s.set_planfile(item, "/p/plan.md", b"v2", t, 40 + STALE_SECS)
            .unwrap();
        assert_eq!(s.stale_plans(40 + 2 * STALE_SECS).unwrap().len(), 1);
    }

    #[test]
    fn next_waits_for_pickup_to_begin_and_leaves_older_items_alone() {
        let (s, ws, _, old) = setup();
        let now = 10 + SETTLE_SECS + 1;
        // A read never begins pickup.
        assert_eq!(s.next_item(now, false).unwrap().item, None);
        // The first take begins it now: the item opened at 10 is the owner's.
        assert_eq!(s.next_item(now, true).unwrap().item, None);
        let (new, key) = s.add_item(ws, "New work", "", now + 1).unwrap();
        // Too fresh to take.
        assert_eq!(s.next_item(now + 2, true).unwrap().item, None);
        let at = now + 1 + SETTLE_SECS;
        let got = s.next_item(at, true).unwrap();
        assert_eq!(got.item.as_ref().map(|i| i.0), Some(new));
        assert_eq!(got.item.unwrap().1, key);
        // Taken once: its start holds it, the older item is never offered.
        let again = s.next_item(at + 1, true).unwrap();
        assert_eq!(again.item, None);
        assert_eq!(again.at_work, 1);
        let _ = old;
    }

    #[test]
    fn next_counts_fresh_starts_and_open_agents_toward_the_cap() {
        let (s, ws, t, _) = setup();
        // Pickup begins after the setup item (opened at 10), which stays the owner's.
        s.next_item(50, true).unwrap();
        let mut keys = Vec::new();
        for i in 0..4 {
            keys.push(s.add_item(ws, &format!("Work {i}"), "", 100 + i).unwrap().0);
        }
        let at = 200 + SETTLE_SECS;
        // An open agent (asleep or not) on one item holds a slot.
        let task = s.begin_task(t, "work", None, at).unwrap().0;
        s.link_task(task, keys[0], at).unwrap();
        assert!(s.next_item(at, true).unwrap().item.is_some());
        assert!(s.next_item(at, true).unwrap().item.is_some());
        let full = s.next_item(at, true).unwrap();
        assert_eq!(full.item, None);
        assert_eq!(full.at_work, QASS_WORKERS);
    }

    #[test]
    fn next_skips_left_alone_spaces_and_dead_starts() {
        let (s, ws, _, _) = setup();
        // Pickup begins after the setup item (opened at 10), which stays the owner's.
        s.next_item(50, true).unwrap();
        let other = s.create_workspace("quiet", 3).unwrap();
        s.add_item(other, "Leave me", "", 100).unwrap();
        s.set_qass_skip_workspace(other, true).unwrap();
        let (dead, dead_key) = s.add_item(ws, "Never linked", "", 101).unwrap();
        s.start_item(dead, 102).unwrap();
        s.start_item(dead, 103 + START_SECS).unwrap();
        let at = 104 + 2 * START_SECS + SETTLE_SECS;
        let got = s.next_item(at, false).unwrap();
        assert_eq!(got.item, None);
        assert_eq!(got.skipped, vec![dead_key]);
    }

    #[test]
    fn the_card_reads_words_and_the_row_carries_the_plan() {
        let (s, _, t, item) = setup();
        let hash = s
            .set_planfile(
                item,
                "/home/p/plans/ship.md",
                b"## Summary\nShip it.\n",
                t,
                20,
            )
            .unwrap();
        s.approve_plan(item, ApprovedBy::Qass, &hash, 21).unwrap();
        s.set_show_me(item, true, 22).unwrap();
        let card = s.item_card(item, 23).unwrap().unwrap();
        let words: Vec<&str> = card.activity.iter().map(|e| e.body.as_str()).collect();
        assert!(words.contains(&"plan ship.md"), "{words:?}");
        assert!(words.contains(&"plan approved by qass"), "{words:?}");
        assert!(
            words.contains(&"the owner asked to see the plan"),
            "{words:?}"
        );
        assert!(!words.iter().any(|w| w.contains(&hash)), "{words:?}");
        assert_eq!(card.item.plan, PlanState::Approved(ApprovedBy::Qass));
        assert!(card.item.show_me);
        let file = card.plan.unwrap();
        assert_eq!(file.summary, "Ship it.");
        assert_eq!(file.terminal, t);
    }

    #[test]
    fn a_card_finds_its_item_by_task_or_by_an_open_agents_terminal() {
        let (s, _, t, item) = setup();
        let task = s.begin_task(t, "work", None, 11).unwrap().0;
        s.link_task(task, item, 12).unwrap();
        let key = crate::item::key_of(&s.conn, item).unwrap();
        // By its task.
        assert_eq!(
            s.item_of_card(Some(task), None).unwrap(),
            Some((item, key.clone()))
        );
        // A screen dialog's draft has no task: by the terminal of the open agent.
        assert_eq!(s.item_of_card(None, Some(t)).unwrap(), Some((item, key)));
        // Nothing to go by, or a terminal with no linked agent: none.
        assert_eq!(s.item_of_card(None, None).unwrap(), None);
        assert_eq!(s.item_of_card(None, Some(t + 99)).unwrap(), None);
        // A plan's time is its newest link or approval.
        let h = s.set_planfile(item, "/p/a.md", b"v1", t, 20).unwrap();
        assert_eq!(s.plan_info(item, 21).unwrap().at, Some(20));
        s.approve_plan(item, ApprovedBy::Qass, &h, 30).unwrap();
        assert_eq!(s.plan_info(item, 31).unwrap().at, Some(30));
    }

    #[test]
    fn an_open_plan_card_makes_the_plan_wait_on_the_owner() {
        let (s, _, t, item) = setup();
        let task = s.begin_task(t, "work", None, 11).unwrap().0;
        s.link_task(task, item, 12).unwrap();
        let h = s.set_planfile(item, "/p/plan.md", b"v1", t, 20).unwrap();
        let card = |handle: &str, status: &str| {
            s.conn.execute("DELETE FROM ask", []).unwrap();
            s.conn
                .execute(
                    "INSERT INTO ask (card, terminal, task, status, body, opened_at) \
                     VALUES ('E1', ?1, ?2, ?3, ?4, 21)",
                    params![t, task, status, format!("{{\"handle\":\"{handle}\"}}")],
                )
                .unwrap();
        };
        card("plan", "open");
        assert_eq!(s.plan_info(item, 22).unwrap().state, PlanState::Asked);
        // The plan says it asked: the card is no question on top of that.
        let row = s.item_row(item, 22).unwrap();
        assert_eq!((row.plan, row.open_asks), (PlanState::Asked, 0));
        // It still holds the plan back from going stale while it is open.
        assert_eq!(open_cards(&s.conn, item, true).unwrap(), 1);
        // Another question on the item is no plan card.
        card("domain", "open");
        assert_eq!(s.plan_info(item, 22).unwrap().state, PlanState::Written);
        assert_eq!(s.item_row(item, 22).unwrap().open_asks, 1);
        // Answered: it no longer waits on him.
        card("plan", "queued");
        assert_eq!(s.plan_info(item, 22).unwrap().state, PlanState::Written);
        // Approved wins over a card still open.
        card("plan", "open");
        s.approve_plan(item, ApprovedBy::Owner, &h, 30).unwrap();
        assert_eq!(
            s.plan_info(item, 31).unwrap().state,
            PlanState::Approved(ApprovedBy::Owner)
        );
    }

    #[test]
    fn a_closed_item_waits_on_nobody_with_a_plan_card_left_open() {
        let (s, _, t, item) = setup();
        let task = s.begin_task(t, "work", None, 11).unwrap().0;
        s.link_task(task, item, 12).unwrap();
        s.set_planfile(item, "/p/plan.md", b"v1", t, 20).unwrap();
        s.conn
            .execute(
                "INSERT INTO ask (card, terminal, task, status, body, opened_at) \
                 VALUES ('E1', ?1, ?2, 'open', '{\"handle\":\"plan\"}', 21)",
                params![t, task],
            )
            .unwrap();
        s.close_task(item, crate::Outcome::Verified, "done", 30)
            .unwrap();
        assert_eq!(s.item_row(item, 31).unwrap().plan, PlanState::Written);
    }
}

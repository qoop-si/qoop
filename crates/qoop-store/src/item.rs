//! The owner's board: items, their steps, and the agents working on them.
//!
//! An item is a task row with no terminal, opened by `add` and marked in `board_item`, so
//! the record, the events and the close a task has serve it as they are. Its key is
//! `<PREFIX>-<n>`, fixed when it is added. An agent's task is linked to it (`task_item`),
//! and an agent on another machine is a row qass keeps current (`task_item_remote`).
//!
//! Its column on the board is never stored: it is read from what happened (`state`), so
//! no write can leave an item in a column its record contradicts.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::task::{
    COLUMNS, Outcome, STAGES, TaskError, TaskEventRow, TaskRow, close, event, goal_text, one_line,
    open_row, printable, row, row_of, touch, within,
};
use crate::{Store, StoreError};

/// Budgets, in characters and steps. Past one the write is refused with the limit named.
pub const DESCRIPTION_MAX: usize = 2_500;
pub const STEPS_MAX: usize = 30;
pub const STEP_MAX: usize = 200;
/// How long a start waits for an agent to be linked before the item reads as Todo again.
pub const START_SECS: i64 = 600;
/// How long a Done item stays on the board.
pub const DONE_SECS: i64 = 86_400;
/// The most of a remote agent's name or machine kept, and of its stage.
const NAME_MAX: usize = 100;
const STAGE_MAX: usize = 40;
/// The body of the `unlinked` event that takes every agent off an item at once.
const EVERY_AGENT: &str = "every agent";
/// The longest key prefix.
const PREFIX_MAX: usize = 10;
/// The most activity one card carries; the reader fits it to its frame.
const ACTIVITY_MAX: usize = 200;
/// The agents that ended an item keeps, newest first, beside every open one: an item worked
/// for weeks would carry every agent it ever had into each board read.
const ENDED_KEPT: usize = 5;

/// Where an item is on the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemState {
    Todo,
    Doing,
    /// Closed verified or failed.
    Done,
    /// Closed abandoned: dropped by a person, or its space removed. Never shown.
    Dropped,
}

impl ItemState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }
}

/// An agent working on an item: a task on this machine, or one qass reported from a remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemAgent {
    pub task: i64,
    pub terminal: Option<i64>,
    /// The remote's name, for an agent there; `None` is this machine.
    pub remote: Option<String>,
    /// The name qass reported for a remote agent; for a local one, what it was called when it
    /// was linked (its reader names it afresh while it is open).
    pub name: Option<String>,
    /// A remote agent's stage as qass last reported it, at `updated_at`.
    pub stage: String,
    pub claimed: bool,
    pub closed: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRow {
    /// The item's own task row: its goal is the title.
    pub task: TaskRow,
    pub key: String,
    pub state: ItemState,
    pub steps_done: usize,
    pub steps_total: usize,
    pub agents: Vec<ItemAgent>,
    /// The newest change on it or on an agent working on it.
    pub updated_at: i64,
    /// Put away by the owner (`dismiss`): closed, off the board, nothing of it deleted.
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepRow {
    pub n: i64,
    pub text: String,
    pub done_at: Option<i64>,
}

/// An item with everything a card shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCard {
    pub item: ItemRow,
    pub description: String,
    pub steps: Vec<StepRow>,
    /// Its own events and its agents' stages, evidence and done claims, newest first. `task`
    /// says whose; an id of 0 is a line read from the record, not stored.
    pub activity: Vec<TaskEventRow>,
}

/// The key prefix a space's name gives: its letters and digits, upper case, at most ten
/// (Jira's own limit, so a whole word like `DEFAULT` is not cut into a typo); `ITEM` when it
/// has none.
pub fn prefix_of(name: &str) -> String {
    let prefix: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(PREFIX_MAX)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if prefix.is_empty() {
        "ITEM".to_string()
    } else {
        prefix
    }
}

fn is_item(tx: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM board_item WHERE task = ?1",
            params![id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The open item, or why it cannot be written to.
fn open_item(tx: &Connection, item: i64) -> Result<TaskRow, StoreError> {
    if !is_item(tx, item)? {
        return Err(TaskError::NotAnItem(item).into());
    }
    open_row(tx, item)
}

/// The id of the newest event of a kind on a task, and when.
fn newest(tx: &Connection, task: i64, kind: &str) -> rusqlite::Result<Option<(i64, i64)>> {
    tx.query_row(
        "SELECT id, at FROM task_event WHERE task = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1",
        params![task, kind],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// The start still waiting for an agent: newer than the last unlink, close or reopen (each ends
/// what a start asked for: a reopened item is Todo until it is started again), and it is when.
fn waiting_start(tx: &Connection, item: i64) -> rusqlite::Result<Option<(i64, i64)>> {
    let start = newest(tx, item, "start")?;
    let mut ended = 0;
    for kind in ["unlinked", "closed", "reopened"] {
        ended = ended.max(newest(tx, item, kind)?.map_or(0, |(id, _)| id));
    }
    Ok(start.filter(|(id, _)| *id > ended))
}

/// A task's link moves to the task that replaced it in its terminal, in the transaction
/// that opened the new one, so a new request in a linked terminal stays on the board.
pub(crate) fn carry_link(tx: &Connection, old: i64, new: i64, now: i64) -> rusqlite::Result<()> {
    let item: Option<i64> = tx
        .query_row(
            "SELECT item FROM task_item WHERE task = ?1",
            params![old],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(item) = item {
        tx.execute(
            "UPDATE task_item SET task = ?2 WHERE task = ?1",
            params![old, new],
        )?;
        event(tx, item, "linked", &format!("#{new}"), now)?;
        touch(tx, item, now)?;
    }
    Ok(())
}

/// Close a space's open items as abandoned, inside the transaction that removes it.
pub(crate) fn abandon_open_items(
    tx: &Transaction<'_>,
    workspace: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let open = {
        let mut stmt = tx.prepare(
            "SELECT t.id FROM task t JOIN board_item b ON b.task = t.id \
             WHERE t.workspace = ?1 AND t.closed_at IS NULL",
        )?;
        stmt.query_map(params![workspace], |r| r.get(0))?
            .collect::<Result<Vec<i64>, _>>()?
    };
    for item in open {
        close(tx, item, Outcome::Abandoned, "its space was removed", now)?;
    }
    Ok(())
}

fn steps(tx: &Connection, item: i64) -> rusqlite::Result<Vec<StepRow>> {
    let mut stmt =
        tx.prepare("SELECT n, text, done_at FROM task_step WHERE task = ?1 ORDER BY n")?;
    stmt.query_map(params![item], |r| {
        Ok(StepRow {
            n: r.get(0)?,
            text: r.get(1)?,
            done_at: r.get(2)?,
        })
    })?
    .collect()
}

fn state(
    tx: &Connection,
    t: &TaskRow,
    agents: &[ItemAgent],
    now: i64,
) -> rusqlite::Result<ItemState> {
    match t.outcome.as_deref() {
        Some("verified" | "failed") => return Ok(ItemState::Done),
        Some(_) => return Ok(ItemState::Dropped),
        None => {}
    }
    if agents.iter().any(|a| !a.closed) {
        return Ok(ItemState::Doing);
    }
    // Every agent on it ended, and linked after its last reopen: the work is finished and
    // waits for qass's check, never Todo again (an unlink takes the agents off; a reopen with no
    // agent since is Todo). Event ids, not clocks: a remote's re-sync rewrites its time.
    if !agents.is_empty() {
        let linked = newest(tx, t.id, "linked")?.map_or(0, |(id, _)| id);
        let reopened = newest(tx, t.id, "reopened")?.map_or(0, |(id, _)| id);
        if linked > reopened {
            return Ok(ItemState::Doing);
        }
    }
    Ok(match waiting_start(tx, t.id)? {
        Some((_, at)) if now - at < START_SECS => ItemState::Doing,
        _ => ItemState::Todo,
    })
}

fn agents(tx: &Connection, item: i64) -> rusqlite::Result<Vec<ItemAgent>> {
    let mut stmt = tx.prepare(
        "SELECT t.id, t.terminal, t.stage, t.claimed_at IS NOT NULL, t.closed_at IS NOT NULL, \
         t.updated_at, l.name FROM task_item l JOIN task t ON t.id = l.task WHERE l.item = ?1 \
         ORDER BY t.id",
    )?;
    let mut all = stmt
        .query_map(params![item], |r| {
            Ok(ItemAgent {
                task: r.get(0)?,
                terminal: r.get(1)?,
                remote: None,
                name: r.get(6)?,
                stage: r.get(2)?,
                claimed: r.get(3)?,
                closed: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stmt = tx.prepare(
        "SELECT task, remote, name, stage, updated_at FROM task_item_remote \
         WHERE item = ?1 ORDER BY remote, task",
    )?;
    let remote = stmt.query_map(params![item], |r| {
        let stage: String = r.get(3)?;
        Ok(ItemAgent {
            task: r.get(0)?,
            terminal: None,
            remote: Some(r.get(1)?),
            name: Some(r.get(2)?),
            // A stage that is not one an open task is in is how qass reports its end.
            closed: !STAGES.contains(&stage.as_str()),
            stage,
            claimed: false,
            updated_at: r.get(4)?,
        })
    })?;
    for agent in remote {
        all.push(agent?);
    }
    let mut ended: Vec<usize> = (0..all.len()).filter(|&i| all[i].closed).collect();
    ended.sort_by_key(|&i| std::cmp::Reverse(all[i].updated_at));
    let old: BTreeSet<usize> = ended.into_iter().skip(ENDED_KEPT).collect();
    Ok(all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !old.contains(i))
        .map(|(_, a)| a)
        .collect())
}

/// An item's key, `<PREFIX>-<n>`.
fn key_of(tx: &Connection, item: i64) -> rusqlite::Result<String> {
    tx.query_row(
        "SELECT prefix, n FROM board_item WHERE task = ?1",
        params![item],
        |r| {
            Ok(format!(
                "{}-{}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?
            ))
        },
    )
}

fn item_row(tx: &Connection, task: TaskRow, now: i64) -> rusqlite::Result<ItemRow> {
    let key = key_of(tx, task.id)?;
    let agents = agents(tx, task.id)?;
    let state = state(tx, &task, &agents, now)?;
    let (steps_total, steps_done): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COUNT(done_at) FROM task_step WHERE task = ?1",
        params![task.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let updated_at = agents
        .iter()
        .map(|a| a.updated_at)
        .fold(task.updated_at, i64::max);
    // Put away since its latest close: a mark from before a reopen and a new close (an older
    // binary reopens without clearing it) is stale and ignored.
    let hidden = match task.closed_at {
        Some(closed) => tx
            .query_row(
                "SELECT 1 FROM board_hidden WHERE task = ?1 AND at >= ?2",
                params![task.id, closed],
                |_| Ok(()),
            )
            .optional()?
            .is_some(),
        None => false,
    };
    Ok(ItemRow {
        task,
        key,
        state,
        steps_done: steps_done as usize,
        steps_total: steps_total as usize,
        agents,
        updated_at,
        hidden,
    })
}

fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEventRow> {
    Ok(TaskEventRow {
        id: r.get(0)?,
        task: r.get(1)?,
        at: r.get(2)?,
        kind: r.get(3)?,
        body: r.get(4)?,
    })
}

/// The spans of event ids each local task was linked to an item in: from its `linked #<task>`
/// to the `unlinked` that took it off (`#<task>`, or every agent at once), or to now. Event
/// ids are one sequence across tasks, so an agent's events fall in or out of a span by id.
fn link_windows(tx: &Connection, item: i64) -> rusqlite::Result<Vec<(i64, i64, i64)>> {
    let mut stmt = tx.prepare(
        "SELECT id, kind, body FROM task_event WHERE task = ?1 \
         AND kind IN ('linked', 'unlinked') ORDER BY id",
    )?;
    let mut windows: Vec<(i64, i64, i64)> = Vec::new();
    for e in stmt.query_map(params![item], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (id, kind, body) = e?;
        // A remote agent's link names it, not a task here: it has no events on this machine.
        let task = body.strip_prefix('#').and_then(|n| n.parse::<i64>().ok());
        match (kind.as_str(), task) {
            ("linked", Some(task)) => windows.push((task, id, i64::MAX)),
            ("unlinked", _) if task.is_some() || body == EVERY_AGENT => {
                for w in windows
                    .iter_mut()
                    .filter(|w| w.2 == i64::MAX && task.is_none_or(|t| t == w.0))
                {
                    w.2 = id;
                }
            }
            _ => {}
        }
    }
    Ok(windows)
}

/// What a remote agent's stage reads as in activity.
fn said(stage: &str) -> &str {
    if stage == "waiting-human" {
        "waits on you"
    } else {
        stage
    }
}

impl Store {
    /// Add an item to a space's board: a task with no terminal, keyed by the space's prefix
    /// and the next number for it, counted in this transaction.
    pub fn add_item(
        &self,
        workspace: i64,
        title: &str,
        description: &str,
        now: i64,
    ) -> Result<(i64, String), StoreError> {
        let title = goal_text(title)?;
        let description = printable(description);
        within("the description", &description, DESCRIPTION_MAX)?;
        let tx = self.conn.unchecked_transaction()?;
        let name: Option<String> = tx
            .query_row(
                "SELECT name FROM workspace WHERE id = ?1",
                params![workspace],
                |r| r.get(0),
            )
            .optional()?;
        let Some(name) = name else {
            return Err(StoreError::NoWorkspace(workspace));
        };
        let prefix = prefix_of(&name);
        tx.execute(
            "INSERT INTO task (workspace, terminal, goal, stage, opened_at, updated_at) \
             VALUES (?1, NULL, ?2, ?3, ?4, ?4)",
            params![workspace, title, STAGES[0], now],
        )?;
        let id = tx.last_insert_rowid();
        let n: i64 = tx.query_row(
            "SELECT COALESCE(MAX(n), 0) + 1 FROM board_item WHERE prefix = ?1",
            params![prefix],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO board_item (task, prefix, n, description) VALUES (?1, ?2, ?3, ?4)",
            params![id, prefix, n, description],
        )?;
        event(&tx, id, "opened", &title, now)?;
        if !description.is_empty() {
            event(&tx, id, "description", &description, now)?;
        }
        tx.commit()?;
        Ok((id, format!("{prefix}-{n}")))
    }

    /// The item a key names, `<PREFIX>-<n>` in any case. An id is the writer's to check.
    pub fn item_id(&self, key: &str) -> Result<Option<i64>, StoreError> {
        let Some((prefix, n)) = key.trim().rsplit_once('-') else {
            return Ok(None);
        };
        let Ok(n) = n.parse::<i64>() else {
            return Ok(None);
        };
        Ok(self
            .conn
            .query_row(
                "SELECT task FROM board_item WHERE prefix = ?1 AND n = ?2",
                params![prefix.to_ascii_uppercase(), n],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// An open item's title, or why it cannot be worked on.
    pub fn open_item_goal(&self, item: i64) -> Result<String, StoreError> {
        Ok(open_item(&self.conn, item)?.goal)
    }

    /// [`Store::begin_task`] for `qoop task link`: a task it opens is named after the item and
    /// says it was opened by the link, not by an edit. Refused, with nothing begun, unless the
    /// item is open.
    pub fn begin_linked(
        &self,
        terminal: i64,
        item: i64,
        asked_at: Option<i64>,
        now: i64,
    ) -> Result<(i64, bool), StoreError> {
        let title = open_item(&self.conn, item)?.goal;
        let key = key_of(&self.conn, item)?;
        self.begin(terminal, &title, asked_at, &format!("a link to {key}"), now)
    }

    /// Keep what a linked agent is called, for its card once its terminal is gone.
    pub fn name_agent(&self, task: i64, name: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE task_item SET name = ?2 WHERE task = ?1",
            params![
                task,
                one_line(name).chars().take(NAME_MAX).collect::<String>()
            ],
        )?;
        Ok(())
    }

    /// The item a task is linked to, if any.
    pub fn linked_item(&self, task: i64) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT item FROM task_item WHERE task = ?1",
                params![task],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Rewrite an open item's description; empty clears it. Answers whether it changed.
    pub fn describe_item(&self, item: i64, text: &str, now: i64) -> Result<bool, StoreError> {
        let text = printable(text);
        within("the description", &text, DESCRIPTION_MAX)?;
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        let old: String = tx.query_row(
            "SELECT description FROM board_item WHERE task = ?1",
            params![item],
            |r| r.get(0),
        )?;
        if old == text {
            return Ok(false);
        }
        tx.execute(
            "UPDATE board_item SET description = ?2 WHERE task = ?1",
            params![item, text],
        )?;
        touch(&tx, item, now)?;
        event(&tx, item, "description", &text, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Replace an open item's steps. Blank lines are dropped; a step whose text is unchanged
    /// keeps its tick wherever it moved. Answers whether the list changed.
    pub fn set_steps(&self, item: i64, list: &[String], now: i64) -> Result<bool, StoreError> {
        let list: Vec<String> = list
            .iter()
            .map(|s| one_line(s))
            .filter(|s| !s.is_empty())
            .collect();
        if list.len() > STEPS_MAX {
            return Err(TaskError::TooManySteps {
                len: list.len(),
                max: STEPS_MAX,
            }
            .into());
        }
        for step in &list {
            within("a step", step, STEP_MAX)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        let old = steps(&tx, item)?;
        if old.iter().map(|s| &s.text).eq(list.iter()) {
            return Ok(false);
        }
        let ticks: HashMap<&str, i64> = old
            .iter()
            .filter_map(|s| Some((s.text.as_str(), s.done_at?)))
            .collect();
        tx.execute("DELETE FROM task_step WHERE task = ?1", params![item])?;
        for (i, text) in list.iter().enumerate() {
            tx.execute(
                "INSERT INTO task_step (task, n, text, done_at) VALUES (?1, ?2, ?3, ?4)",
                params![item, i as i64 + 1, text, ticks.get(text.as_str())],
            )?;
        }
        touch(&tx, item, now)?;
        event(&tx, item, "steps", &format!("{} steps", list.len()), now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Tick step `n` (from 1) of an open item, or untick it. Answers whether it changed.
    pub fn check_step(&self, item: i64, n: i64, done: bool, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        let all = steps(&tx, item)?;
        let Some(step) = all.iter().find(|s| s.n == n) else {
            return Err(TaskError::NoStep(n, all.len()).into());
        };
        if step.done_at.is_some() == done {
            return Ok(false);
        }
        tx.execute(
            "UPDATE task_step SET done_at = ?3 WHERE task = ?1 AND n = ?2",
            params![item, n, done.then_some(now)],
        )?;
        touch(&tx, item, now)?;
        let kind = if done { "ticked" } else { "unticked" };
        event(&tx, item, kind, &format!("{n}. {}", step.text), now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Link an open task to an open item, moving it off any other. Answers whether it moved.
    pub fn link_task(&self, task: i64, item: i64, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        if is_item(&tx, task)? {
            return Err(TaskError::ItemAsAgent(task).into());
        }
        open_row(&tx, task)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT item FROM task_item WHERE task = ?1",
                params![task],
                |r| r.get(0),
            )
            .optional()?;
        if current == Some(item) {
            return Ok(false);
        }
        if let Some(old) = current {
            event(&tx, old, "unlinked", &format!("#{task}"), now)?;
            touch(&tx, old, now)?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO task_item (task, item) VALUES (?1, ?2)",
            params![task, item],
        )?;
        event(&tx, item, "linked", &format!("#{task}"), now)?;
        touch(&tx, item, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Take a task off its item; for an item, every agent off it and a waiting start with
    /// them, which is what moves it back to Todo. Answers whether anything changed.
    pub fn unlink(&self, id: i64, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if is_item(&tx, id)? {
            let local = tx.execute("DELETE FROM task_item WHERE item = ?1", params![id])?;
            let remote = tx.execute("DELETE FROM task_item_remote WHERE item = ?1", params![id])?;
            let item = row_of(&tx, id)?;
            let waiting = item.closed_at.is_none() && waiting_start(&tx, id)?.is_some();
            if local + remote == 0 && !waiting {
                return Ok(false);
            }
            event(&tx, id, "unlinked", EVERY_AGENT, now)?;
            touch(&tx, id, now)?;
        } else {
            let item: Option<i64> = tx
                .query_row(
                    "SELECT item FROM task_item WHERE task = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(item) = item else {
                return Err(TaskError::NotOnBoard.into());
            };
            tx.execute("DELETE FROM task_item WHERE task = ?1", params![id])?;
            event(&tx, item, "unlinked", &format!("#{id}"), now)?;
            touch(&tx, item, now)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Record, or bring up to date, an agent on another machine working on an open item, as
    /// qass reads it from that machine's events. Answers whether a reader would see a change.
    pub fn link_remote(
        &self,
        item: i64,
        remote: &str,
        task: i64,
        name: &str,
        stage: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let remote = one_line(remote);
        let name = one_line(name);
        let stage = one_line(stage);
        for (what, text, max) in [
            ("the remote", &remote, NAME_MAX),
            ("the name", &name, NAME_MAX),
            ("the stage", &stage, STAGE_MAX),
        ] {
            if text.is_empty() {
                return Err(TaskError::Empty(what).into());
            }
            within(what, text, max)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        let old: Option<(i64, String, String)> = tx
            .query_row(
                "SELECT item, name, stage FROM task_item_remote WHERE remote = ?1 AND task = ?2",
                params![remote, task],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO task_item_remote (item, remote, task, name, stage, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (remote, task) DO UPDATE SET \
             item = excluded.item, name = excluded.name, stage = excluded.stage, \
             updated_at = excluded.updated_at",
            params![item, remote, task, name, stage, now],
        )?;
        let who = format!("{name} on {remote}");
        let changed = match &old {
            Some((was, n, s)) if *was == item => {
                if *s != stage {
                    event(&tx, item, "stage", &format!("{who}: {}", said(&stage)), now)?;
                }
                *n != name || *s != stage
            }
            _ => {
                if let Some((was, _, _)) = &old {
                    event(&tx, *was, "unlinked", &who, now)?;
                    touch(&tx, *was, now)?;
                }
                event(
                    &tx,
                    item,
                    "linked",
                    &format!("{who}: {}", said(&stage)),
                    now,
                )?;
                true
            }
        };
        if changed {
            touch(&tx, item, now)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Ask for an open item to be started: Doing for `START_SECS`, until an agent is linked
    /// or it is moved back. Answers its key, as stored, for the line qass is told.
    pub fn start_item(&self, item: i64, now: i64) -> Result<String, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        open_item(&tx, item)?;
        event(&tx, item, "start", "", now)?;
        touch(&tx, item, now)?;
        let key = key_of(&tx, item)?;
        tx.commit()?;
        Ok(key)
    }

    /// Bring a closed item back to the board, its steps as they were. Items only: a task
    /// ends with its agent's work. Refused once its space is gone: it would be open on every
    /// board with nowhere to run. Answers whether it was closed.
    pub fn reopen_item(&self, item: i64, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if !is_item(&tx, item)? {
            return Err(TaskError::NotAnItem(item).into());
        }
        let row = row_of(&tx, item)?;
        if row.closed_at.is_none() {
            return Ok(false);
        }
        if row.workspace.is_none() {
            return Err(TaskError::SpaceGone(item).into());
        }
        tx.execute(
            "UPDATE task SET closed_at = NULL, outcome = NULL, updated_at = ?2 WHERE id = ?1",
            params![item, now],
        )?;
        // Open again, so on the board again.
        tx.execute("DELETE FROM board_hidden WHERE task = ?1", params![item])?;
        event(&tx, item, "reopened", "", now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Put a closed item away from the board, or (`undo`) bring it back: it stays closed, with
    /// its outcome, steps, links and history; its activity says so. Refused for an open item.
    /// Answers whether anything changed.
    pub fn dismiss_item(&self, item: i64, undo: bool, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if !is_item(&tx, item)? {
            return Err(TaskError::NotAnItem(item).into());
        }
        // Done (closed verified or failed) is what the Done lane shows; a dropped item is on no
        // board to be put away from.
        let row = row_of(&tx, item)?;
        let Some(closed) = row
            .closed_at
            .filter(|_| matches!(row.outcome.as_deref(), Some("verified" | "failed")))
        else {
            return Err(TaskError::NotDone(item).into());
        };
        let marked: bool = tx
            .query_row(
                "SELECT 1 FROM board_hidden WHERE task = ?1 AND at >= ?2",
                params![item, closed],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let changed = match (undo, marked) {
            (true, true) => {
                tx.execute("DELETE FROM board_hidden WHERE task = ?1", params![item])?;
                true
            }
            (false, false) => {
                tx.execute(
                    "INSERT OR REPLACE INTO board_hidden (task, at) VALUES (?1, ?2)",
                    params![item, now.max(closed)],
                )?;
                true
            }
            _ => false,
        };
        if changed {
            event(&tx, item, if undo { "unhidden" } else { "hidden" }, "", now)?;
            tx.execute(
                "UPDATE task SET updated_at = ?2 WHERE id = ?1",
                params![item, now],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The board: every open item, then those done in the last `DONE_SECS`, without their
    /// texts. Dropped ones are not on it.
    pub fn items(&self, now: i64) -> Result<Vec<ItemRow>, StoreError> {
        let tasks = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM task WHERE id IN (SELECT task FROM board_item) \
                 AND (closed_at IS NULL OR (outcome IN ('verified', 'failed') AND closed_at >= ?1)) \
                 ORDER BY closed_at IS NOT NULL, closed_at DESC, id"
            ))?;
            stmt.query_map(params![now - DONE_SECS], row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut items = Vec::with_capacity(tasks.len());
        for task in tasks {
            items.push(item_row(&self.conn, task, now)?);
        }
        Ok(items)
    }

    /// One item with its description, steps and activity, whatever its state.
    pub fn item_card(&self, item: i64, now: i64) -> Result<Option<ItemCard>, StoreError> {
        let conn = &self.conn;
        if !is_item(conn, item)? {
            return Ok(None);
        }
        let row = item_row(conn, row_of(conn, item)?, now)?;
        let description: String = conn.query_row(
            "SELECT description FROM board_item WHERE task = ?1",
            params![item],
            |r| r.get(0),
        )?;
        let mut activity = {
            let mut stmt = conn.prepare(
                "SELECT id, task, at, kind, body FROM task_event WHERE task = ?1 \
                 ORDER BY id DESC LIMIT ?2",
            )?;
            stmt.query_map(params![item, ACTIVITY_MAX as i64], event_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        // Every task ever linked, not only the ones linked now, an agent moved off keeping
        // what it did here; and each only while it was linked, its work on another item
        // being that item's.
        let mut stmt = conn.prepare(
            "SELECT id, task, at, kind, body FROM task_event WHERE task = ?1 \
             AND kind IN ('stage', 'evidence', 'done') AND id > ?2 AND id < ?3 \
             ORDER BY id DESC LIMIT ?4",
        )?;
        for (task, from, to) in link_windows(conn, item)? {
            for e in stmt.query_map(params![task, from, to, ACTIVITY_MAX as i64], event_row)? {
                let mut e = e?;
                if e.kind == "stage" {
                    e.body = said(&e.body).to_string();
                }
                activity.push(e);
            }
        }
        // A start nobody took up: said once it has run out, unless an agent came after it.
        if row.state == ItemState::Todo
            && let Some((start, at)) = waiting_start(conn, item)?
            && newest(conn, item, "linked")?.is_none_or(|(id, _)| id < start)
        {
            activity.push(TaskEventRow {
                id: 0,
                task: item,
                at: at + START_SECS,
                kind: "start".to_string(),
                body: "qass did not start it".to_string(),
            });
        }
        activity.sort_by_key(|a| std::cmp::Reverse((a.at, a.id)));
        activity.truncate(ACTIVITY_MAX);
        Ok(Some(ItemCard {
            item: row,
            description,
            steps: steps(conn, item)?,
            activity,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_err(e: StoreError) -> TaskError {
        match e {
            StoreError::Task(t) => t,
            other => panic!("expected a task error, got {other}"),
        }
    }

    fn setup() -> (Store, i64, i64) {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("qoop-tui", 1).unwrap();
        let t = s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap();
        (s, ws, t)
    }

    fn state_of(s: &Store, item: i64, now: i64) -> ItemState {
        s.item_card(item, now).unwrap().unwrap().item.state
    }

    /// A closed item put away is off the board and nothing of it is lost: still closed and
    /// verified, its history one line longer; brought back, it is on the board again; reopened,
    /// never hidden; an open item cannot be put away.
    #[test]
    fn a_closed_item_is_dismissed_and_shown_again_and_nothing_is_lost() {
        let (s, ws, _) = setup();
        let (item, _) = s.add_item(ws, "ship it", "", 10).unwrap();
        assert!(matches!(
            task_err(s.dismiss_item(item, false, 15).unwrap_err()),
            TaskError::NotDone(_)
        ));
        s.close_task(item, Outcome::Verified, "moved on the board", 20)
            .unwrap();
        let hidden = |now| {
            s.items(now)
                .unwrap()
                .iter()
                .find(|i| i.task.id == item)
                .map(|i| i.hidden)
        };
        assert_eq!(hidden(21), Some(false));
        assert!(s.dismiss_item(item, false, 30).unwrap());
        assert!(!s.dismiss_item(item, false, 31).unwrap(), "already hidden");
        assert_eq!(hidden(32), Some(true));
        let card = s.item_card(item, 32).unwrap().unwrap();
        assert_eq!(card.item.task.outcome.as_deref(), Some("verified"));
        assert!(card.item.task.closed_at.is_some());
        assert_eq!(card.activity[0].kind, "hidden");
        assert!(s.dismiss_item(item, true, 40).unwrap());
        assert_eq!(hidden(41), Some(false));
        assert_eq!(
            s.item_card(item, 41).unwrap().unwrap().activity[0].kind,
            "unhidden"
        );
        s.dismiss_item(item, false, 50).unwrap();
        assert!(s.reopen_item(item, 60).unwrap());
        assert_eq!(hidden(61), Some(false), "open again, on the board again");
        let marks: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM board_hidden WHERE task = ?1",
                params![item],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marks, 0, "a reopen takes the mark off");
        // A mark an older binary left behind across a reopen and a new close is stale.
        s.conn
            .execute(
                "INSERT OR REPLACE INTO board_hidden (task, at) VALUES (?1, 62)",
                params![item],
            )
            .unwrap();
        s.close_task(item, Outcome::Verified, "again", 70).unwrap();
        assert_eq!(
            hidden(71),
            Some(false),
            "the mark is from before this close"
        );
        // Put away in the very second it closed: hidden (the mark is at its close, not before).
        assert!(s.dismiss_item(item, false, 70).unwrap());
        assert_eq!(hidden(71), Some(true));
        // Put away, reopened and closed again all in one second: the reopen took the mark off,
        // so the new close is on the board.
        assert!(s.reopen_item(item, 70).unwrap());
        s.close_task(item, Outcome::Verified, "same second", 70)
            .unwrap();
        assert_eq!(hidden(70), Some(false));
        // A dropped item is on no board: nothing to put away.
        let (dropped, _) = s.add_item(ws, "never mind", "", 80).unwrap();
        s.close_task(dropped, Outcome::Abandoned, "dropped", 81)
            .unwrap();
        assert!(matches!(
            task_err(s.dismiss_item(dropped, false, 82).unwrap_err()),
            TaskError::NotDone(_)
        ));
        assert!(matches!(
            task_err(s.dismiss_item(999, false, 90).unwrap_err()),
            TaskError::NotAnItem(_)
        ));
    }

    /// A start asked for before a close is spent: the reopened item is Todo, not Doing, until
    /// it is started again.
    #[test]
    fn a_reopened_item_waits_for_a_new_start() {
        let (s, ws, _) = setup();
        let (item, _) = s.add_item(ws, "ship it", "", 10).unwrap();
        s.start_item(item, 20).unwrap();
        assert_eq!(state_of(&s, item, 21), ItemState::Doing);
        s.close_task(item, Outcome::Verified, "moved on the board", 30)
            .unwrap();
        assert!(s.reopen_item(item, 40).unwrap());
        assert_eq!(state_of(&s, item, 41), ItemState::Todo);
        s.start_item(item, 50).unwrap();
        assert_eq!(state_of(&s, item, 51), ItemState::Doing);
    }

    /// Keys take the space's letters and digits, upper case, ten at most, `ITEM` for none;
    /// the number counts per prefix, across spaces that share one; a rename keeps the key;
    /// a key is found in any case and by `#id`.
    #[test]
    fn keys_count_per_prefix_and_never_change() {
        let (s, ws, _) = setup();
        let other = s.create_workspace("QooP-tUI!", 1).unwrap();
        let blank = s.create_workspace("!!", 1).unwrap();
        assert_eq!(prefix_of("qoop-tui"), "QOOPTUI");
        assert_eq!(prefix_of("default"), "DEFAULT");
        assert_eq!(prefix_of("frontend-dashboard"), "FRONTENDDA");
        let (a, ka) = s.add_item(ws, "first", "", 10).unwrap();
        let (_, kb) = s.add_item(ws, "second", "", 11).unwrap();
        let (_, kc) = s.add_item(other, "third", "", 12).unwrap();
        let (_, kd) = s.add_item(blank, "fourth", "", 13).unwrap();
        assert_eq!(
            [ka.as_str(), &kb, &kc, &kd],
            ["QOOPTUI-1", "QOOPTUI-2", "QOOPTUI-3", "ITEM-1"]
        );
        s.rename_workspace(ws, "web").unwrap();
        let (_, ke) = s.add_item(ws, "fifth", "", 14).unwrap();
        assert_eq!(ke, "WEB-1");
        assert_eq!(s.item_card(a, 15).unwrap().unwrap().item.key, "QOOPTUI-1");
        assert_eq!(s.item_id("qooptui-1").unwrap(), Some(a));
        assert_eq!(
            s.item_id(&format!("#{a}")).unwrap(),
            None,
            "an id is the writer's to check"
        );
        assert_eq!(s.item_id("QOOPTUI-9").unwrap(), None);
        assert!(matches!(
            s.add_item(999, "x", "", 16).unwrap_err(),
            StoreError::NoWorkspace(999)
        ));
        assert!(s.add_item(ws, "  ", "", 16).is_err());
        assert!(
            s.add_item(ws, "x", &"d".repeat(DESCRIPTION_MAX + 1), 16)
                .is_err()
        );
    }

    /// Setting the list replaces it; an unchanged text keeps its tick wherever it moved;
    /// the caps are refused, never cut; a missing step is named.
    #[test]
    fn steps_keep_their_ticks_and_their_limits() {
        let (s, ws, _) = setup();
        let (item, _) = s.add_item(ws, "ship", "", 10).unwrap();
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(
            s.set_steps(item, &list(&["read", "fix", "", "test"]), 11)
                .unwrap()
        );
        assert!(s.check_step(item, 2, true, 12).unwrap());
        assert!(!s.check_step(item, 2, true, 13).unwrap(), "already ticked");
        assert!(
            !s.set_steps(item, &list(&["read", "fix", "test"]), 14)
                .unwrap()
        );
        assert!(
            s.set_steps(item, &list(&["fix", "review", "test"]), 15)
                .unwrap()
        );
        let card = s.item_card(item, 16).unwrap().unwrap();
        let ticked: Vec<_> = card
            .steps
            .iter()
            .map(|s| (s.n, s.text.as_str(), s.done_at))
            .collect();
        assert_eq!(
            ticked,
            [(1, "fix", Some(12)), (2, "review", None), (3, "test", None)]
        );
        assert_eq!((card.item.steps_done, card.item.steps_total), (1, 3));
        assert!(matches!(
            task_err(s.check_step(item, 4, true, 17).unwrap_err()),
            TaskError::NoStep(4, 3)
        ));
        let many: Vec<String> = (0..=STEPS_MAX).map(|n| format!("s{n}")).collect();
        assert!(matches!(
            task_err(s.set_steps(item, &many, 18).unwrap_err()),
            TaskError::TooManySteps { .. }
        ));
        assert!(matches!(
            task_err(
                s.set_steps(item, &["x".repeat(STEP_MAX + 1)], 18)
                    .unwrap_err()
            ),
            TaskError::TooLong { .. }
        ));
        assert!(s.check_step(item, 1, false, 19).unwrap());
        assert!(s.describe_item(item, "why and how", 20).unwrap());
        assert!(!s.describe_item(item, "why and how", 21).unwrap());
        assert_eq!(
            s.item_card(item, 22).unwrap().unwrap().description,
            "why and how"
        );
    }

    /// A link goes only to an open item, never from one; it moves off another item; it
    /// follows the task's successor in its terminal, from a begin and from a replace.
    #[test]
    fn links_refuse_what_is_not_an_open_item_and_follow_a_superseded_task() {
        let (mut s, ws, t) = setup();
        let (item, _) = s.add_item(ws, "login", "", 10).unwrap();
        let (other, _) = s.add_item(ws, "signup", "", 10).unwrap();
        let (task, _) = s.begin_task(t, "fix login", Some(9), 11).unwrap();
        let plain = {
            let u = s.create_terminal(6, ws, "/tmp", "/bin/sh", 2).unwrap();
            s.open_task(u, "other work", false, 11).unwrap()
        };
        assert!(matches!(
            task_err(s.link_task(task, plain, 12).unwrap_err()),
            TaskError::NotAnItem(_)
        ));
        assert!(matches!(
            task_err(s.link_task(other, item, 12).unwrap_err()),
            TaskError::ItemAsAgent(_)
        ));
        assert!(s.link_task(task, other, 12).unwrap());
        assert!(s.link_task(task, item, 13).unwrap(), "moved");
        assert!(!s.link_task(task, item, 14).unwrap());
        assert_eq!(s.linked_item(task).unwrap(), Some(item));
        assert_eq!(state_of(&s, other, 15), ItemState::Todo);
        assert_eq!(state_of(&s, item, 15), ItemState::Doing);

        // A done claim and a newer request: the new task carries the link.
        s.claim_task_done(task, "done", 20).unwrap();
        let (next, opened) = s.begin_task(t, "now signup", Some(30), 31).unwrap();
        assert!(opened);
        assert_eq!(s.linked_item(next).unwrap(), Some(item));
        assert_eq!(s.linked_item(task).unwrap(), None);
        // And a replace does the same.
        let third = s.open_task(t, "again", true, 32).unwrap();
        assert_eq!(s.linked_item(third).unwrap(), Some(item));
        assert_eq!(state_of(&s, item, 33), ItemState::Doing);
        // A task closed with its terminal is not carried: nothing replaced it.
        s.close_terminal(t, 34).unwrap();
        assert_eq!(s.linked_item(third).unwrap(), Some(item), "kept as history");
        // Its only agent ended: finished, waiting for qass's check, never Todo again.
        assert_eq!(state_of(&s, item, 35), ItemState::Doing);

        s.close_task(other, Outcome::Verified, "", 36).unwrap();
        let u = s.create_terminal(7, ws, "/tmp", "/bin/sh", 37).unwrap();
        let late = s.open_task(u, "late", false, 37).unwrap();
        assert!(matches!(
            task_err(s.link_task(late, other, 38).unwrap_err()),
            TaskError::Closed(_)
        ));
        assert!(matches!(
            task_err(s.unlink(late, 39).unwrap_err()),
            TaskError::NotOnBoard
        ));
    }

    /// An item whose agents all ended stays in Doing until it is closed; a reopen with no
    /// agent linked since is Todo, and a re-sync of an ended remote agent changes nothing.
    #[test]
    fn a_finished_item_never_falls_back_to_todo() {
        let (s, ws, t) = setup();
        let (item, _) = s.add_item(ws, "logo", "", 10).unwrap();
        let task = s.open_task(t, "logo", false, 11).unwrap();
        s.link_task(task, item, 12).unwrap();
        s.close_terminal(t, 13).unwrap();
        assert_eq!(state_of(&s, item, 14), ItemState::Doing);
        let card = s.item_card(item, 14).unwrap().unwrap();
        assert!(
            card.item.agents.iter().all(|a| a.closed),
            "its agent reads finished"
        );
        s.close_task(item, Outcome::Verified, "checked", 15)
            .unwrap();
        assert_eq!(state_of(&s, item, 16), ItemState::Done);
        assert!(s.reopen_item(item, 17).unwrap());
        assert_eq!(
            state_of(&s, item, 18),
            ItemState::Todo,
            "reopened, nobody on it yet"
        );
        s.link_remote(item, "box", 7, "T3", "verified", 19).unwrap();
        assert_eq!(
            state_of(&s, item, 20),
            ItemState::Doing,
            "a new agent since, finished"
        );
    }

    /// Todo, Doing on a start for ten minutes or on a live agent, Todo again on an unlink
    /// or an expired start (said in the activity), Done on a close, Todo on a reopen with the
    /// steps kept, and a dropped item off the board.
    #[test]
    fn the_state_is_read_from_what_happened() {
        let (s, ws, t) = setup();
        let (item, _) = s.add_item(ws, "search", "", 10).unwrap();
        s.set_steps(item, &["a".to_string(), "b".to_string()], 10)
            .unwrap();
        s.check_step(item, 1, true, 10).unwrap();
        assert_eq!(state_of(&s, item, 11), ItemState::Todo);
        s.start_item(item, 100).unwrap();
        assert_eq!(state_of(&s, item, 100 + START_SECS - 1), ItemState::Doing);
        let late = s.item_card(item, 100 + START_SECS).unwrap().unwrap();
        assert_eq!(late.item.state, ItemState::Todo);
        assert_eq!(late.activity[0].body, "qass did not start it");
        // Moved back before it ran out: Todo at once, and nothing to say.
        s.start_item(item, 1_000).unwrap();
        assert!(s.unlink(item, 1_001).unwrap());
        let back = s.item_card(item, 1_002).unwrap().unwrap();
        assert_eq!(back.item.state, ItemState::Todo);
        assert_eq!(back.activity[0].kind, "unlinked");
        assert!(!s.unlink(item, 1_003).unwrap(), "nothing left to unlink");

        // A remote agent: Doing while its stage is one of an open task.
        assert!(
            s.link_remote(item, "box", 7, "T3", "implementing", 1_100)
                .unwrap()
        );
        assert!(
            !s.link_remote(item, "box", 7, "T3", "implementing", 1_101)
                .unwrap()
        );
        assert!(
            s.link_remote(item, "box", 7, "T3", "waiting-human", 1_102)
                .unwrap()
        );
        let card = s.item_card(item, 1_103).unwrap().unwrap();
        assert_eq!(card.item.state, ItemState::Doing);
        assert_eq!(card.item.agents[0].remote.as_deref(), Some("box"));
        assert_eq!(card.activity[0].body, "T3 on box: waits on you");
        assert!(
            s.link_remote(item, "box", 7, "T3", "verified", 1_104)
                .unwrap()
        );
        // The remote agent finished: the item stays in Doing for qass's check.
        assert_eq!(state_of(&s, item, 1_105), ItemState::Doing);

        // A local agent, its waiting stage read as such in the item's activity.
        let task = s.open_task(t, "the work", false, 1_200).unwrap();
        s.link_task(task, item, 1_201).unwrap();
        s.set_task_stage(task, "waiting-human", 1_202).unwrap();
        s.task_evidence(task, "cargo test: ok", 1_203).unwrap();
        let card = s.item_card(item, 1_204).unwrap().unwrap();
        assert_eq!(card.item.state, ItemState::Doing);
        assert_eq!(
            card.item.updated_at, 1_203,
            "the agent's write is the item's news"
        );
        let theirs: Vec<_> = card
            .activity
            .iter()
            .filter(|e| e.task == task)
            .map(|e| e.body.as_str())
            .collect();
        assert_eq!(theirs, ["cargo test: ok", "waits on you"]);

        s.close_task(item, Outcome::Verified, "checked", 1_300)
            .unwrap();
        assert_eq!(state_of(&s, item, 1_301), ItemState::Done);
        assert_eq!(s.items(1_301).unwrap().len(), 1);
        assert!(
            s.items(1_300 + DONE_SECS + 1).unwrap().is_empty(),
            "a day later"
        );
        assert!(s.reopen_item(item, 1_400).unwrap());
        assert!(!s.reopen_item(item, 1_401).unwrap());
        let card = s.item_card(item, 1_402).unwrap().unwrap();
        assert_eq!(
            card.item.state,
            ItemState::Doing,
            "its agent is still on it"
        );
        assert_eq!(card.item.steps_done, 1, "steps kept");
        assert!(matches!(
            task_err(s.reopen_item(task, 1_403).unwrap_err()),
            TaskError::NotAnItem(_)
        ));
        s.close_task(item, Outcome::Abandoned, "dropped", 1_500)
            .unwrap();
        assert_eq!(state_of(&s, item, 1_501), ItemState::Dropped);
        assert!(
            s.items(1_501).unwrap().is_empty(),
            "a dropped item is not shown"
        );
    }

    /// A card shows an agent's work only from while it was linked to that item: moved to
    /// another, its later work is that one's; taken off with every agent, nothing more.
    #[test]
    fn a_card_shows_an_agents_work_only_while_it_was_linked_there() {
        let (s, ws, t) = setup();
        let (a, _) = s.add_item(ws, "login", "", 10).unwrap();
        let (b, _) = s.add_item(ws, "signup", "", 10).unwrap();
        let task = s.open_task(t, "work", false, 11).unwrap();
        s.task_evidence(task, "before any link", 12).unwrap();
        s.link_task(task, a, 13).unwrap();
        s.task_evidence(task, "on a", 14).unwrap();
        s.link_task(task, b, 15).unwrap();
        s.task_evidence(task, "on b", 16).unwrap();
        s.unlink(b, 17).unwrap();
        s.task_evidence(task, "on nothing", 18).unwrap();
        let theirs = |item| -> Vec<String> {
            s.item_card(item, 20)
                .unwrap()
                .unwrap()
                .activity
                .into_iter()
                .filter(|e| e.task == task)
                .map(|e| e.body)
                .collect()
        };
        assert_eq!(theirs(a), ["on a"]);
        assert_eq!(theirs(b), ["on b"]);
    }

    /// A reopen brings an item back only to a space that is still there.
    #[test]
    fn a_reopen_is_refused_once_the_space_is_gone() {
        let (s, _, _) = setup();
        let gone = s.create_workspace("gone", 1).unwrap();
        let (item, _) = s.add_item(gone, "orphan", "", 10).unwrap();
        s.delete_workspace(gone).unwrap();
        assert!(matches!(
            task_err(s.reopen_item(item, 20).unwrap_err()),
            TaskError::SpaceGone(_)
        ));
        assert!(s.task(item).unwrap().unwrap().closed_at.is_some());
    }

    /// The board and a card carry every open agent and the newest five that ended; a start
    /// answers the key as stored, whatever case it was asked in.
    #[test]
    fn an_item_keeps_its_open_agents_and_the_newest_ended_ones() {
        let (mut s, ws, _) = setup();
        let (item, key) = s.add_item(ws, "long one", "", 10).unwrap();
        let mut ended = Vec::new();
        for n in 0..7 {
            let t = s
                .create_terminal(20 + n, ws, "/tmp", "/bin/sh", 11)
                .unwrap();
            let task = s.open_task(t, "a go at it", false, 12).unwrap();
            s.link_task(task, item, 13).unwrap();
            s.close_task(task, Outcome::Failed, "", 100 + n).unwrap();
            ended.push(task);
        }
        let t = s.create_terminal(40, ws, "/tmp", "/bin/sh", 11).unwrap();
        let open = s.open_task(t, "the live one", false, 50).unwrap();
        s.link_task(open, item, 51).unwrap();
        let card = s.item_card(item, 200).unwrap().unwrap();
        let kept: Vec<i64> = card.item.agents.iter().map(|a| a.task).collect();
        let mut want = ended[2..].to_vec();
        want.push(open);
        assert_eq!(kept, want, "the two oldest that ended are left out");
        assert_eq!(s.items(200).unwrap()[0].agents.len(), 6);
        assert_eq!(s.start_item(item, 201).unwrap(), key);
    }

    /// Items are not tasks anyone works in: no reader of tasks lists them, open or closed.
    /// Removing a space closes its open items as abandoned with it.
    #[test]
    fn readers_skip_items_and_a_removed_space_drops_its_own() {
        let (s, ws, t) = setup();
        let (item, _) = s.add_item(ws, "board work", "", 10).unwrap();
        let (done, _) = s.add_item(ws, "finished", "", 10).unwrap();
        let task = s.open_task(t, "real work", false, 11).unwrap();
        s.close_task(done, Outcome::Verified, "", 12).unwrap();
        let open: Vec<i64> = s.open_tasks().unwrap().iter().map(|t| t.id).collect();
        assert_eq!(open, [task]);
        assert!(s.closed_tasks_since(0, 10).unwrap().is_empty());
        let keep = s.create_workspace("keep", 13).unwrap();
        let (kept, _) = s.add_item(keep, "elsewhere", "", 13).unwrap();
        s.delete_workspace(ws).unwrap();
        let row = s.task(item).unwrap().unwrap();
        assert_eq!(row.outcome.as_deref(), Some("abandoned"));
        assert_eq!(
            s.task(done).unwrap().unwrap().outcome.as_deref(),
            Some("verified")
        );
        assert!(s.task(kept).unwrap().unwrap().closed_at.is_none());
        let shown: Vec<i64> = s.items(14).unwrap().iter().map(|i| i.task.id).collect();
        assert_eq!(shown, [kept, done]);
    }
}

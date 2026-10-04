//! Tasks: the work an agent in a terminal said it is doing, and what it recorded on the way.
//!
//! The agent opens and updates its own task with `qoro task ...`; qoro keeps it here rather
//! than in a file in the repo or in anyone's memory, so a crash, a restart or a compaction
//! loses nothing and every reader (qass, the phone, the notch) sees the same record.
//!
//! A task belongs to a terminal, one open at a time. Terminal ids are reused, so a task
//! never outlives its terminal open: every path that ends a terminal abandons its task in
//! the same transaction (`abandon_open_tasks`). Done from the agent is a claim, recorded and
//! cleared by the next stage write; only a close ends a task, and the outcome says how.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{Store, StoreError};

/// The stages an open task can be in. The end of a task is its outcome, not a stage.
pub const STAGES: [&str; 6] = [
    "investigating",
    "planning",
    "implementing",
    "reviewing",
    "verifying",
    "waiting-human",
];

/// Budgets, in characters. Past one the write is refused with the limit named, never cut:
/// a cut plan reads as a whole one.
pub const GOAL_MAX: usize = 300;
pub const PLAN_MAX: usize = 2_500;
pub const EVIDENCE_MAX: usize = 1_000;
pub const SUMMARY_MAX: usize = 600;
pub const REASON_MAX: usize = 600;

/// How a task ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Checked and accepted.
    Verified,
    /// Checked and given up on.
    Failed,
    /// The same terminal opened another task in its place.
    Superseded,
    /// Its terminal or space went away while it was open.
    Abandoned,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub id: i64,
    pub workspace: Option<i64>,
    pub terminal: Option<i64>,
    pub goal: String,
    pub stage: String,
    pub opened_at: i64,
    pub updated_at: i64,
    /// The last done claim, cleared by a later stage write.
    pub claimed_at: Option<i64>,
    pub closed_at: Option<i64>,
    pub outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskEventRow {
    pub id: i64,
    pub task: i64,
    pub at: i64,
    pub kind: String,
    pub body: String,
}

/// What a task write may be refused for, apart from the database itself failing.
#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("terminal {0} has open task #{1}: {2}; record on it with plan, stage and evidence")]
    AlreadyOpen(i64, i64, String),
    #[error("terminal {0} is not open")]
    TerminalClosed(i64),
    #[error("no task #{0}")]
    NoTask(i64),
    #[error("task #{0} is closed")]
    Closed(i64),
    #[error("unknown stage {0:?}; one of: {stages}", stages = STAGES.join(", "))]
    UnknownStage(String),
    #[error("{what} is {len} characters, over the {max} limit; shorten it")]
    TooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
    #[error("{0} is empty")]
    Empty(&'static str),
}

/// One line, no control characters: a goal is shown in lists and injected into agents.
fn one_line(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control() && !invisible(*c))
        .collect()
}

/// Characters that show nothing and still change what a reader is told: zero widths,
/// direction overrides, tag characters. A goal reaches later sessions' context; what it
/// says must be what anyone sees.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}'
        | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}')
}

/// Lines and tabs kept, every other control character dropped: the text is shown in other
/// terminals (qass reads it), where an escape sequence would draw instead of read.
fn printable(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| (!c.is_control() || *c == '\n' || *c == '\t') && !invisible(*c))
        .collect()
}

fn within(what: &'static str, text: &str, max: usize) -> Result<(), StoreError> {
    let len = text.chars().count();
    if len > max {
        return Err(TaskError::TooLong { what, len, max }.into());
    }
    Ok(())
}

const COLUMNS: &str =
    "id, workspace, terminal, goal, stage, opened_at, updated_at, claimed_at, closed_at, outcome";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        id: r.get(0)?,
        workspace: r.get(1)?,
        terminal: r.get(2)?,
        goal: r.get(3)?,
        stage: r.get(4)?,
        opened_at: r.get(5)?,
        updated_at: r.get(6)?,
        claimed_at: r.get(7)?,
        closed_at: r.get(8)?,
        outcome: r.get(9)?,
    })
}

fn event(tx: &Connection, task: i64, kind: &str, body: &str, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO task_event (task, at, kind, body) VALUES (?1, ?2, ?3, ?4)",
        params![task, now, kind, body],
    )?;
    Ok(())
}

fn close(
    tx: &Connection,
    task: i64,
    outcome: Outcome,
    reason: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE task SET closed_at = ?2, updated_at = ?2, outcome = ?3, terminal = NULL \
         WHERE id = ?1",
        params![task, now, outcome.as_str()],
    )?;
    let body = if reason.is_empty() {
        outcome.as_str().to_string()
    } else {
        format!("{}: {reason}", outcome.as_str())
    };
    event(tx, task, "closed", &body, now)
}

/// Close every open task of these terminals as abandoned. Called inside the transaction that
/// ends the terminals, so a reused terminal id never finds an old task open.
pub(crate) fn abandon_open_tasks(
    tx: &Transaction<'_>,
    terminals: &[i64],
    now: i64,
) -> rusqlite::Result<()> {
    for &terminal in terminals {
        let open: Option<i64> = tx
            .query_row(
                "SELECT id FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(task) = open {
            close(tx, task, Outcome::Abandoned, "its terminal closed", now)?;
        }
    }
    Ok(())
}

/// The open task row, or why it cannot be written to.
fn open_row(tx: &Connection, task: i64) -> Result<TaskRow, StoreError> {
    let found = tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM task WHERE id = ?1"),
            params![task],
            row,
        )
        .optional()?;
    match found {
        None => Err(TaskError::NoTask(task).into()),
        Some(t) if t.closed_at.is_some() => Err(TaskError::Closed(task).into()),
        Some(t) => Ok(t),
    }
}

impl Store {
    /// Open a task for a terminal. Refused when the terminal is not open (checked here, at
    /// the write, because a request admitted a moment before the terminal closed still
    /// arrives), and when it already has an open task unless `replace`, which closes that
    /// one as superseded: a nested agent sharing the terminal must not quietly take over
    /// its parent's task.
    pub fn open_task(
        &self,
        terminal: i64,
        goal: &str,
        replace: bool,
        now: i64,
    ) -> Result<i64, StoreError> {
        let goal = one_line(goal);
        if goal.is_empty() {
            return Err(TaskError::Empty("the goal").into());
        }
        within("the goal", &goal, GOAL_MAX)?;
        let tx = self.conn.unchecked_transaction()?;
        let workspace: Option<i64> = tx
            .query_row(
                "SELECT workspace FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?;
        let Some(workspace) = workspace else {
            return Err(TaskError::TerminalClosed(terminal).into());
        };
        let open: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, goal FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old, old_goal)) = open {
            if !replace {
                return Err(TaskError::AlreadyOpen(terminal, old, old_goal).into());
            }
            close(&tx, old, Outcome::Superseded, "", now)?;
        }
        tx.execute(
            "INSERT INTO task (workspace, terminal, goal, stage, opened_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![workspace, terminal, goal, STAGES[0], now],
        )?;
        let id = tx.last_insert_rowid();
        event(&tx, id, "opened", &goal, now)?;
        tx.commit()?;
        Ok(id)
    }

    /// The terminal's open task, opened here with `goal` when it has none. One transaction,
    /// so a burst of edit attempts opens one task; an open task is left exactly as it is,
    /// unless the agent claimed it done and a request came after the claim (`asked_at`): that
    /// is new work, and the claimed task is closed as superseded, its claim and evidence kept.
    /// Answers the task and whether it was opened now.
    pub fn begin_task(
        &self,
        terminal: i64,
        goal: &str,
        asked_at: Option<i64>,
        now: i64,
    ) -> Result<(i64, bool), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let open: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT id, claimed_at FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match (open, asked_at) {
            (Some((id, Some(claimed))), Some(asked)) if asked > claimed => {
                close(
                    &tx,
                    id,
                    Outcome::Superseded,
                    "a new request after its done claim",
                    now,
                )?;
            }
            (Some((id, _)), _) => return Ok((id, false)),
            (None, _) => {}
        }
        let workspace: Option<i64> = tx
            .query_row(
                "SELECT workspace FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?;
        let Some(workspace) = workspace else {
            return Err(TaskError::TerminalClosed(terminal).into());
        };
        let mut goal: String = one_line(goal).chars().take(GOAL_MAX).collect();
        if goal.is_empty() {
            goal = format!("work in terminal {terminal}");
        }
        tx.execute(
            "INSERT INTO task (workspace, terminal, goal, stage, opened_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![workspace, terminal, goal, STAGES[0], now],
        )?;
        let id = tx.last_insert_rowid();
        event(
            &tx,
            id,
            "opened",
            &format!("opened by qoro at an edit attempt: {goal}"),
            now,
        )?;
        tx.commit()?;
        Ok((id, true))
    }

    /// Move an open task to a stage. Any stage write clears a done claim: the worker is back
    /// at work. Answers whether anything a reader shows changed (stage or claim).
    pub fn set_task_stage(&self, task: i64, stage: &str, now: i64) -> Result<bool, StoreError> {
        if !STAGES.contains(&stage) {
            return Err(TaskError::UnknownStage(stage.to_string()).into());
        }
        let tx = self.conn.unchecked_transaction()?;
        let t = open_row(&tx, task)?;
        tx.execute(
            "UPDATE task SET stage = ?2, claimed_at = NULL, updated_at = ?3 WHERE id = ?1",
            params![task, stage, now],
        )?;
        event(&tx, task, "stage", stage, now)?;
        tx.commit()?;
        Ok(t.stage != stage || t.claimed_at.is_some())
    }

    /// Record the task's contract: goal, acceptance, plan, how it will be verified, risks.
    pub fn task_plan(&self, task: i64, text: &str, now: i64) -> Result<(), StoreError> {
        self.task_text(task, "plan", "the plan", text, PLAN_MAX, now)
    }

    /// Record a check and its result.
    pub fn task_evidence(&self, task: i64, text: &str, now: i64) -> Result<(), StoreError> {
        self.task_text(task, "evidence", "the evidence", text, EVIDENCE_MAX, now)
    }

    fn task_text(
        &self,
        task: i64,
        kind: &str,
        what: &'static str,
        text: &str,
        max: usize,
        now: i64,
    ) -> Result<(), StoreError> {
        let text = printable(text);
        if text.is_empty() {
            return Err(TaskError::Empty(what).into());
        }
        within(what, &text, max)?;
        let tx = self.conn.unchecked_transaction()?;
        open_row(&tx, task)?;
        tx.execute(
            "UPDATE task SET updated_at = ?2 WHERE id = ?1",
            params![task, now],
        )?;
        event(&tx, task, kind, &text, now)?;
        tx.commit()?;
        Ok(())
    }

    /// The agent says it is done. A claim: it is checked outside the agent, and the task
    /// stays open until a close.
    pub fn claim_task_done(&self, task: i64, summary: &str, now: i64) -> Result<(), StoreError> {
        let summary = printable(summary);
        within("the summary", &summary, SUMMARY_MAX)?;
        let tx = self.conn.unchecked_transaction()?;
        open_row(&tx, task)?;
        tx.execute(
            "UPDATE task SET claimed_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![task, now],
        )?;
        event(&tx, task, "done", &summary, now)?;
        tx.commit()?;
        Ok(())
    }

    /// End a task as verified or failed. Answers the terminal it was in, so the caller can
    /// tell that terminal's readers.
    pub fn close_task(
        &self,
        task: i64,
        outcome: Outcome,
        reason: &str,
        now: i64,
    ) -> Result<Option<i64>, StoreError> {
        let reason = printable(reason);
        within("the reason", &reason, REASON_MAX)?;
        let tx = self.conn.unchecked_transaction()?;
        let t = open_row(&tx, task)?;
        close(&tx, task, outcome, &reason, now)?;
        tx.commit()?;
        Ok(t.terminal)
    }

    pub fn task(&self, id: i64) -> Result<Option<TaskRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM task WHERE id = ?1"),
                params![id],
                row,
            )
            .optional()?)
    }

    /// The open task of a terminal, if it has one.
    pub fn open_task_of(&self, terminal: i64) -> Result<Option<TaskRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM task WHERE terminal = ?1 AND closed_at IS NULL"),
                params![terminal],
                row,
            )
            .optional()?)
    }

    /// Every open task, oldest first.
    pub fn open_tasks(&self) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM task WHERE closed_at IS NULL ORDER BY id"
        ))?;
        let rows = stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A task's events, newest first, older than `before` when given.
    pub fn task_events(
        &self,
        task: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TaskEventRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task, at, kind, body FROM task_event \
             WHERE task = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![task, before.unwrap_or(i64::MAX), limit as i64],
                |r| {
                    Ok(TaskEventRow {
                        id: r.get(0)?,
                        task: r.get(1)?,
                        at: r.get(2)?,
                        kind: r.get(3)?,
                        body: r.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_terminal() -> (Store, i64, i64) {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("w", 1).unwrap();
        let t = s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap();
        (s, ws, t)
    }

    fn task_err(e: StoreError) -> TaskError {
        match e {
            StoreError::Task(t) => t,
            other => panic!("expected a task error, got {other}"),
        }
    }

    #[test]
    fn a_task_records_its_life_and_ends_only_on_close() {
        let (s, ws, t) = store_with_terminal();
        let id = s
            .open_task(t, "  fix the\nredirect\u{202E}\u{200B} ", false, 10)
            .unwrap();
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(row.goal, "fix the redirect");
        assert_eq!(row.workspace, Some(ws));
        assert_eq!(row.stage, "investigating");
        s.task_plan(id, "goal, acceptance, plan, verify", 11)
            .unwrap();
        assert!(s.set_task_stage(id, "implementing", 12).unwrap());
        s.task_evidence(id, "cargo test: 12 passed", 13).unwrap();
        s.claim_task_done(id, "redirect fixed", 14).unwrap();
        assert_eq!(s.task(id).unwrap().unwrap().claimed_at, Some(14));
        assert!(
            s.task(id).unwrap().unwrap().closed_at.is_none(),
            "done is a claim"
        );
        assert_eq!(
            s.close_task(id, Outcome::Verified, "checked", 15).unwrap(),
            Some(t)
        );
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(row.outcome.as_deref(), Some("verified"));
        assert_eq!(row.terminal, None);
        let kinds: Vec<_> = s
            .task_events(id, None, 50)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            ["closed", "done", "evidence", "stage", "plan", "opened"]
        );
        assert!(matches!(
            task_err(s.task_evidence(id, "late", 16).unwrap_err()),
            TaskError::Closed(_)
        ));
        assert!(matches!(
            task_err(s.close_task(id, Outcome::Failed, "", 17).unwrap_err()),
            TaskError::Closed(_)
        ));
    }

    #[test]
    fn a_second_task_is_refused_unless_it_replaces_the_first() {
        let (s, _, t) = store_with_terminal();
        let first = s.open_task(t, "one", false, 10).unwrap();
        match task_err(s.open_task(t, "two", false, 11).unwrap_err()) {
            TaskError::AlreadyOpen(term, id, goal) => {
                assert_eq!((term, id, goal.as_str()), (t, first, "one"))
            }
            other => panic!("{other}"),
        }
        let second = s.open_task(t, "two", true, 12).unwrap();
        assert_eq!(
            s.task(first).unwrap().unwrap().outcome.as_deref(),
            Some("superseded")
        );
        assert_eq!(s.open_task_of(t).unwrap().unwrap().id, second);
        assert_eq!(s.open_tasks().unwrap().len(), 1);
    }

    #[test]
    fn a_stage_write_clears_a_claim_and_says_so() {
        let (s, _, t) = store_with_terminal();
        let id = s.open_task(t, "x", false, 10).unwrap();
        assert!(
            !s.set_task_stage(id, "investigating", 11).unwrap(),
            "same stage, no claim"
        );
        s.claim_task_done(id, "", 12).unwrap();
        assert!(
            s.set_task_stage(id, "investigating", 13).unwrap(),
            "the claim cleared"
        );
        assert_eq!(s.task(id).unwrap().unwrap().claimed_at, None);
        assert!(matches!(
            task_err(s.set_task_stage(id, "done", 14).unwrap_err()),
            TaskError::UnknownStage(_)
        ));
    }

    #[test]
    fn every_way_a_terminal_ends_abandons_its_task() {
        let (mut s, ws, t) = store_with_terminal();
        let a = s.open_task(t, "a", false, 10).unwrap();
        s.close_terminal(t, 11).unwrap();
        assert_eq!(
            s.task(a).unwrap().unwrap().outcome.as_deref(),
            Some("abandoned")
        );

        let u = s
            .create_terminal_answering(Some("k"), 6, ws, "/tmp", "/bin/sh", None, None, 12)
            .unwrap();
        let b = s.open_task(u, "b", false, 13).unwrap();
        s.abandon_create("k", u, 14).unwrap();
        assert_eq!(
            s.task(b).unwrap().unwrap().outcome.as_deref(),
            Some("abandoned")
        );

        let v = s.create_terminal(7, ws, "/tmp", "/bin/sh", 15).unwrap();
        let c = s.open_task(v, "c", false, 16).unwrap();
        s.delete_workspace(ws).unwrap();
        let row = s.task(c).unwrap().unwrap();
        assert_eq!(row.outcome.as_deref(), Some("abandoned"));
        assert_eq!(
            row.workspace, None,
            "the space is gone, the task's history is not"
        );
        assert!(s.open_tasks().unwrap().is_empty());
    }

    #[test]
    fn a_reused_terminal_id_starts_with_no_task() {
        let (mut s, ws, t) = store_with_terminal();
        s.open_task(t, "old", false, 10).unwrap();
        s.delete_workspace(ws).unwrap();
        let ws2 = s.create_workspace("w2", 20).unwrap();
        let again = s.create_terminal(t, ws2, "/tmp", "/bin/sh", 21).unwrap();
        assert_eq!(again, t, "the id came back");
        assert!(s.open_task_of(again).unwrap().is_none());
        s.open_task(again, "new", false, 22).unwrap();
    }

    #[test]
    fn a_closed_terminal_cannot_open_a_task() {
        let (s, _, t) = store_with_terminal();
        s.close_terminal(t, 10).unwrap();
        assert!(matches!(
            task_err(s.open_task(t, "late", false, 11).unwrap_err()),
            TaskError::TerminalClosed(_)
        ));
        assert!(matches!(
            task_err(s.open_task(99, "none", false, 11).unwrap_err()),
            TaskError::TerminalClosed(99)
        ));
    }

    #[test]
    fn budgets_are_refused_with_the_limit_never_cut() {
        let (s, _, t) = store_with_terminal();
        let long = "x".repeat(GOAL_MAX + 1);
        match task_err(s.open_task(t, &long, false, 10).unwrap_err()) {
            TaskError::TooLong { max, .. } => assert_eq!(max, GOAL_MAX),
            other => panic!("{other}"),
        }
        let id = s.open_task(t, "ok", false, 10).unwrap();
        assert!(s.task_plan(id, &"p".repeat(PLAN_MAX + 1), 11).is_err());
        assert!(
            s.task_evidence(id, &"e".repeat(EVIDENCE_MAX + 1), 11)
                .is_err()
        );
        assert!(
            s.claim_task_done(id, &"d".repeat(SUMMARY_MAX + 1), 11)
                .is_err()
        );
        assert!(s.task_plan(id, "   ", 11).is_err());
        assert!(s.open_task(t, "\n\t ", true, 11).is_err());
        assert_eq!(
            s.task_events(id, None, 50).unwrap().len(),
            1,
            "only the open landed"
        );
    }

    #[test]
    fn recorded_text_keeps_lines_and_drops_escapes() {
        let (s, _, t) = store_with_terminal();
        let id = s.open_task(t, "x", false, 10).unwrap();
        s.task_evidence(id, "line one\n\tline two\u{1b}[2J\u{1b}]0;title\u{7}", 11)
            .unwrap();
        let body = &s.task_events(id, None, 1).unwrap()[0].body;
        assert_eq!(body, "line one\n\tline two[2J]0;title");
    }

    #[test]
    fn begin_opens_once_and_leaves_an_open_task_alone() {
        let (s, _, t) = store_with_terminal();
        let (id, opened) = s.begin_task(t, "make it shout", Some(9), 10).unwrap();
        assert!(opened);
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(row.goal, "make it shout");
        assert_eq!(
            s.begin_task(t, "something else", Some(9), 11).unwrap(),
            (id, false)
        );
        let row2 = s.task(id).unwrap().unwrap();
        assert_eq!(
            (row2.goal.as_str(), row2.updated_at),
            ("make it shout", 10),
            "untouched"
        );
        assert_eq!(s.task_events(id, None, 10).unwrap().len(), 1);
        s.close_task(id, Outcome::Verified, "", 12).unwrap();
        let (next, opened) = s.begin_task(t, "", None, 13).unwrap();
        assert!(opened && next != id);
        assert_eq!(
            s.task(next).unwrap().unwrap().goal,
            format!("work in terminal {t}")
        );
    }

    #[test]
    fn a_request_after_a_done_claim_begins_new_work() {
        let (s, _, t) = store_with_terminal();
        let (first, _) = s.begin_task(t, "first", Some(9), 10).unwrap();
        s.claim_task_done(first, "done", 20).unwrap();
        // A follow up edit for the same request stays on the claimed task.
        assert_eq!(
            s.begin_task(t, "first", Some(9), 21).unwrap(),
            (first, false)
        );
        // A request after the claim is new work.
        let (second, opened) = s.begin_task(t, "second", Some(30), 31).unwrap();
        assert!(opened && second != first);
        let old = s.task(first).unwrap().unwrap();
        assert_eq!(old.outcome.as_deref(), Some("superseded"));
        assert_eq!(old.claimed_at, Some(20), "the claim stays on record");
    }

    #[test]
    fn events_page_backwards() {
        let (s, _, t) = store_with_terminal();
        let id = s.open_task(t, "x", false, 10).unwrap();
        for n in 0..5 {
            s.task_evidence(id, &format!("check {n}"), 11 + n).unwrap();
        }
        let first = s.task_events(id, None, 3).unwrap();
        assert_eq!(
            first.iter().map(|e| e.body.as_str()).collect::<Vec<_>>(),
            ["check 4", "check 3", "check 2"]
        );
        let rest = s.task_events(id, Some(first[2].id), 10).unwrap();
        assert_eq!(
            rest.iter().map(|e| e.body.as_str()).collect::<Vec<_>>(),
            ["check 1", "check 0", "x"]
        );
    }
}

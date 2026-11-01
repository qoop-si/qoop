//! Tasks: the work an agent in a terminal said it is doing, and what it recorded on the way.
//!
//! The agent opens and updates its own task with `qoop task ...`; qoop keeps it here rather
//! than in a file in the repo or in anyone's memory, so a crash, a restart or a compaction
//! loses nothing and every reader (qass, the phone, the notch) sees the same record.
//!
//! A task belongs to a terminal, one open at a time. Terminal ids are reused, so a task
//! never outlives its terminal tied to it: every path that ends a terminal abandons its task
//! in the same transaction (`abandon_open_tasks`), or unties it when its agent claimed it
//! done, so the claim can still be checked. Done from the agent is a claim, recorded and
//! cleared by the next stage write; only a close ends a task, and the outcome says how.

use std::collections::BTreeMap;

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
/// a cut plan reads as a whole one. A board item's title is the one exception: it is cut to
/// `item::TITLE_MAX` at a word, its whole text kept (`item::short_title`).
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
    /// Where its agent works: its terminal's folder while it has one (asleep too), else the
    /// one kept when a claimed task's tab closed.
    pub folder: Option<String>,
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
    /// A close of an agent's task that already ended, while the item it worked on is open:
    /// the item is what is left to close, the same way.
    #[error(
        "task #{0} is closed; its item {1} is open: qoop task close {1} {2} '<what you checked>'"
    )]
    ClosedOnItem(i64, String, &'static str),
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
    #[error("#{0} is not a board item")]
    NotAnItem(i64),
    #[error("#{0} is a board item itself; an agent's task is what links to one")]
    ItemAsAgent(i64),
    #[error("this task is not on the board: qoop task link <KEY> puts it there")]
    NotOnBoard,
    #[error("no step {0}; the item has {1}")]
    NoStep(i64, usize),
    #[error("{len} steps, over the {max} limit; merge some")]
    TooManySteps { len: usize, max: usize },
    #[error("#{0}'s space was removed; add it again in a space that is there")]
    SpaceGone(i64),
    #[error("only a done item can be dismissed; #{0} is open or was dropped")]
    NotDone(i64),
    #[error("#{0} has no plan file: qoop task planfile <path> links one")]
    NoPlan(i64),
    #[error("the plan file's path has a tab or a line break: rename the file")]
    PlanPath,
    #[error("the plan changed since it was read (read {read}, newest {newest}): read it again")]
    PlanMoved { read: String, newest: String },
}

/// One line, no control characters: a goal is shown in lists and injected into agents.
pub fn one_line(text: &str) -> String {
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
pub(crate) fn printable(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| (!c.is_control() || *c == '\n' || *c == '\t') && !invisible(*c))
        .collect()
}

/// A goal as it is kept: one line, not empty, within `GOAL_MAX`. Checked before anything is
/// written, so a refused goal leaves no task opened on its way.
pub fn goal_text(goal: &str) -> Result<String, StoreError> {
    let goal = one_line(goal);
    if goal.is_empty() {
        return Err(TaskError::Empty("the goal").into());
    }
    within("the goal", &goal, GOAL_MAX)?;
    Ok(goal)
}

pub(crate) fn within(what: &'static str, text: &str, max: usize) -> Result<(), StoreError> {
    let len = text.chars().count();
    if len > max {
        return Err(TaskError::TooLong { what, len, max }.into());
    }
    Ok(())
}

pub(crate) const COLUMNS: &str = "id, workspace, terminal, goal, stage, opened_at, updated_at, claimed_at, closed_at, outcome, \
     COALESCE((SELECT tm.cwd FROM terminal tm WHERE tm.id = task.terminal), \
     (SELECT e.body FROM task_event e WHERE e.task = task.id AND e.kind = 'folder' \
     ORDER BY e.id DESC LIMIT 1))";

/// A subquery for when the task row named `alias` was last put to `waiting-human`: the time
/// of its newest such stage event, `NULL` when it never was.
pub(crate) fn waiting_at(alias: &str) -> String {
    format!(
        "(SELECT MAX(w.at) FROM task_event w WHERE w.task = {alias}.id \
         AND w.kind = 'stage' AND w.body = 'waiting-human')"
    )
}

pub(crate) fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
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
        folder: r.get(10)?,
    })
}

pub(crate) fn event(
    tx: &Connection,
    task: i64,
    kind: &str,
    body: &str,
    now: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO task_event (task, at, kind, body) VALUES (?1, ?2, ?3, ?4)",
        params![task, now, kind, body],
    )?;
    Ok(())
}

/// Keep the terminal's folder on a claimed task before it lets go of the terminal: its check
/// still finds the repo once the tab is gone.
fn keep_folder(tx: &Connection, task: i64, terminal: i64, now: i64) -> rusqlite::Result<()> {
    let cwd: Option<String> = tx
        .query_row(
            "SELECT cwd FROM terminal WHERE id = ?1",
            params![terminal],
            |r| r.get(0),
        )
        .optional()?;
    match cwd {
        Some(cwd) => event(tx, task, "folder", &cwd, now),
        None => Ok(()),
    }
}

pub(crate) fn close(
    tx: &Connection,
    task: i64,
    outcome: Outcome,
    reason: &str,
    now: i64,
) -> rusqlite::Result<()> {
    let terminal: Option<i64> = tx
        .query_row(
            "SELECT terminal FROM task WHERE id = ?1",
            params![task],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(terminal) = terminal {
        crate::ask::close_cards(tx, terminal, "its task ended", true, now)?;
    }
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

/// Close every open task of these terminals as abandoned, except one its agent claimed done:
/// that work is finished and waits for its check, so it stays open and claimed, untied from
/// the terminal, for qass or the owner to close verified or failed. Called inside the
/// transaction that ends the terminals, so a reused terminal id never finds an old task open.
pub(crate) fn abandon_open_tasks(
    tx: &Transaction<'_>,
    terminals: &[i64],
    now: i64,
) -> rusqlite::Result<()> {
    crate::ask::close_for_terminals(tx, terminals, now)?;
    for &terminal in terminals {
        let open: Option<(i64, Option<i64>)> = tx
            .query_row(
                "SELECT id, claimed_at FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match open {
            Some((task, Some(_))) => {
                keep_folder(tx, task, terminal, now)?;
                tx.execute(
                    "UPDATE task SET terminal = NULL, updated_at = ?2 WHERE id = ?1",
                    params![task, now],
                )?;
                event(
                    tx,
                    task,
                    "terminal",
                    "its terminal closed; the claim waits for its check",
                    now,
                )?;
            }
            Some((task, None)) => close(tx, task, Outcome::Abandoned, "its terminal closed", now)?,
            None => {}
        }
    }
    Ok(())
}

/// Close a space's open tasks as abandoned when the space goes: a claimed one left untied by
/// its closed tab would otherwise stay open on no board.
pub(crate) fn abandon_space_tasks(
    tx: &Transaction<'_>,
    space: i64,
    now: i64,
) -> rusqlite::Result<()> {
    let open: Vec<i64> = tx
        .prepare("SELECT id FROM task WHERE workspace = ?1 AND closed_at IS NULL")?
        .query_map(params![space], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for task in open {
        close(tx, task, Outcome::Abandoned, "its space was removed", now)?;
    }
    Ok(())
}

/// A task row by id.
pub(crate) fn row_of(tx: &Connection, task: i64) -> rusqlite::Result<TaskRow> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM task WHERE id = ?1"),
        params![task],
        row,
    )
}

/// Say a task changed now.
pub(crate) fn touch(tx: &Connection, task: i64, now: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE task SET updated_at = ?2 WHERE id = ?1",
        params![task, now],
    )?;
    Ok(())
}

/// The open task row, or why it cannot be written to.
pub(crate) fn open_row(tx: &Connection, task: i64) -> Result<TaskRow, StoreError> {
    match row_of(tx, task).optional()? {
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
        let mut replaced = None;
        if let Some((old, old_goal)) = open {
            if !replace {
                return Err(TaskError::AlreadyOpen(terminal, old, old_goal).into());
            }
            close(&tx, old, Outcome::Superseded, "", now)?;
            replaced = Some(old);
        }
        tx.execute(
            "INSERT INTO task (workspace, terminal, goal, stage, opened_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![workspace, terminal, goal, STAGES[0], now],
        )?;
        let id = tx.last_insert_rowid();
        event(&tx, id, "opened", &goal, now)?;
        if let Some(old) = replaced {
            crate::item::carry_link(&tx, old, id, now)?;
        }
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
        self.begin(
            terminal,
            goal,
            asked_at,
            "qoop when the agent changed files",
            now,
        )
    }

    /// [`Store::begin_task`], its `opened` event saying who opened it (`by`).
    pub(crate) fn begin(
        &self,
        terminal: i64,
        goal: &str,
        asked_at: Option<i64>,
        by: &str,
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
        let replaced = match (open, asked_at) {
            (Some((id, Some(claimed))), Some(asked)) if asked > claimed => {
                close(
                    &tx,
                    id,
                    Outcome::Superseded,
                    "a new request after its done claim",
                    now,
                )?;
                Some(id)
            }
            (Some((id, _)), _) => return Ok((id, false)),
            (None, _) => None,
        };
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
        event(&tx, id, "opened", &format!("opened by {by}: {goal}"), now)?;
        // Here and not in `close`, which abandon shares: only a task that has a successor
        // hands its place on the board to it.
        if let Some(old) = replaced {
            crate::item::carry_link(&tx, old, id, now)?;
        }
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
        // A stage written again is no move: a card waits on.
        if stage != "waiting-human"
            && stage != t.stage
            && let Some(terminal) = t.terminal
        {
            crate::ask::close_cards(&tx, terminal, "no longer waits", false, now)?;
        }
        tx.commit()?;
        Ok(t.stage != stage || t.claimed_at.is_some())
    }

    /// The open task of `terminal` ended a turn asking the owner and he has not answered: its
    /// stage becomes `waiting-human`, as if the agent had set it. Unlike a stage the agent
    /// writes, a claim stays: the question does not undo work it said was done. Answers the
    /// task id, `None` when there is no open task or it already waits.
    pub fn wait_on_owner(&self, terminal: i64, now: i64) -> Result<Option<i64>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let task: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, stage FROM task WHERE terminal = ?1 AND closed_at IS NULL \
                 AND stage != 'waiting-human' ORDER BY id DESC LIMIT 1",
                params![terminal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((task, stage)) = task else {
            return Ok(None);
        };
        // The stage it leaves, on record when it began there with no stage written (a new
        // task): his answer takes it back there ([`Store::answer_waiting`]).
        let recorded: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_event WHERE task = ?1 AND kind = 'stage')",
            params![task],
            |r| r.get(0),
        )?;
        if !recorded {
            event(&tx, task, "stage", &stage, now)?;
        }
        tx.execute(
            "UPDATE task SET stage = 'waiting-human', updated_at = ?2 WHERE id = ?1",
            params![task, now],
        )?;
        event(&tx, task, "stage", "waiting-human", now)?;
        tx.commit()?;
        Ok(Some(task))
    }

    /// The open task of `terminal`, waiting on the owner, took his answer (a prompt reached the
    /// agent after his input in its tab at `input`): its stage goes back to the one before
    /// `waiting-human`, else `implementing`. Input from before the wait began answers nothing.
    /// Only the stage moves: a claim stays. Answers the stage it went back to, `None` when the
    /// task was not waiting or the input came before it did.
    pub fn answer_waiting(
        &self,
        terminal: i64,
        input: i64,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let waiting: Option<i64> = tx
            .query_row(
                &format!(
                    "SELECT id FROM task WHERE terminal = ?1 AND closed_at IS NULL \
                     AND stage = 'waiting-human' AND COALESCE({}, 0) <= ?2 \
                     ORDER BY id DESC LIMIT 1",
                    waiting_at("task")
                ),
                params![terminal, input],
                |r| r.get(0),
            )
            .optional()?;
        let Some(task) = waiting else {
            return Ok(None);
        };
        let before: Option<String> = tx
            .query_row(
                "SELECT body FROM task_event WHERE task = ?1 AND kind = 'stage' \
                 AND body != 'waiting-human' ORDER BY id DESC LIMIT 1",
                params![task],
                |r| r.get(0),
            )
            .optional()?;
        let back = before
            .filter(|b| STAGES.contains(&b.as_str()))
            .unwrap_or_else(|| "implementing".to_string());
        let moved = tx.execute(
            "UPDATE task SET stage = ?2, updated_at = ?3 WHERE id = ?1 AND stage = 'waiting-human'",
            params![task, back, now],
        )?;
        if moved == 0 {
            return Ok(None);
        }
        event(&tx, task, "stage", &back, now)?;
        crate::ask::close_cards(&tx, terminal, "answered in its tab", false, now)?;
        tx.commit()?;
        Ok(Some(back))
    }

    /// Rewrite an open task's goal: what the owner wants, in plain words, with its done bar.
    /// The first goal is the owner's cut prompt; this replaces it as the ask becomes clear or
    /// changes. The old text stays in the events. Answers whether the goal changed.
    pub fn set_task_goal(&self, task: i64, goal: &str, now: i64) -> Result<bool, StoreError> {
        let full = goal_text(goal)?;
        let tx = self.conn.unchecked_transaction()?;
        let t = open_row(&tx, task)?;
        // An item's goal is its title: cut to fit a card, the whole text kept in the event.
        let item: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM board_item WHERE task = ?1)",
            params![task],
            |r| r.get(0),
        )?;
        let goal = if item {
            crate::item::short_title(&full)
        } else {
            full.clone()
        };
        // The same words again change nothing: no event, and the task does not look touched.
        if t.goal == goal && full == goal {
            return Ok(false);
        }
        tx.execute(
            "UPDATE task SET goal = ?2, updated_at = ?3 WHERE id = ?1",
            params![task, goal, now],
        )?;
        event(&tx, task, "goal", &full, now)?;
        tx.commit()?;
        Ok(true)
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
        touch(&tx, task, now)?;
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
        let t = open_row(&tx, task)?;
        tx.execute(
            "UPDATE task SET claimed_at = ?2, updated_at = ?2 WHERE id = ?1",
            params![task, now],
        )?;
        event(&tx, task, "done", &summary, now)?;
        if let Some(terminal) = t.terminal {
            crate::ask::close_cards(&tx, terminal, "its task is done", false, now)?;
        }
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
        // An item its last agent already closed: the same close again is no error.
        if let Some(done) = row_of(&tx, task).optional()?
            && done.outcome.as_deref() == Some(outcome.as_str())
            && crate::item::is_item(&tx, task)?
        {
            return Ok(done.terminal);
        }
        let t = open_row(&tx, task)?;
        close(&tx, task, outcome, &reason, now)?;
        if outcome == Outcome::Verified && t.claimed_at.is_some() {
            crate::item::follow_last_agent(&tx, task, &reason, now)?;
        }
        tx.commit()?;
        Ok(t.terminal)
    }

    pub fn task(&self, id: i64) -> Result<Option<TaskRow>, StoreError> {
        Ok(row_of(&self.conn, id).optional()?)
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

    /// Every open task, oldest first. Board items are not tasks anyone works in: they are
    /// read with `items`.
    pub fn open_tasks(&self) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM task WHERE closed_at IS NULL \
             AND id NOT IN (SELECT task FROM board_item) ORDER BY id"
        ))?;
        let rows = stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The open tasks no open item has: not an item, not linked to one still open, and not in
    /// the space named `except` (qass's own), newest first. What a board shows apart from its
    /// items.
    pub fn loose_tasks(&self, except: &str) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM task WHERE closed_at IS NULL \
             AND id NOT IN (SELECT task FROM board_item) \
             AND id NOT IN (SELECT l.task FROM task_item l JOIN task i ON i.id = l.item \
                            WHERE i.closed_at IS NULL) \
             AND (workspace IS NULL \
                  OR workspace NOT IN (SELECT id FROM workspace WHERE name = ?1)) \
             ORDER BY id DESC"
        ))?;
        let rows = stmt
            .query_map(params![except], row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// When each open task now at `waiting-human` got there: its newest such stage event.
    pub fn waiting_since(&self) -> Result<BTreeMap<i64, i64>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT task.id, {} FROM task \
             WHERE closed_at IS NULL AND stage = 'waiting-human'",
            waiting_at("task")
        ))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
            })?
            .filter_map(|r| match r {
                Ok((id, Some(at))) => Some(Ok((id, at))),
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The terminals that hold an open task.
    pub fn open_task_terminals(&self) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT terminal FROM task WHERE closed_at IS NULL AND terminal IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Tasks checked and closed (verified or failed) at or after `since`, newest first, at
    /// most `limit`. Superseded and abandoned ones ended without a check, so they are not
    /// news; a board item closed is shown as a Done item, not here.
    pub fn closed_tasks_since(&self, since: i64, limit: usize) -> Result<Vec<TaskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM task WHERE closed_at >= ?1 AND outcome IN ('verified', 'failed') \
             AND id NOT IN (SELECT task FROM board_item) \
             ORDER BY closed_at DESC, id DESC LIMIT ?2"
        ))?;
        let rows = stmt
            .query_map(params![since, limit as i64], row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A task's newest events of one kind, newest first: the plan of a task with many
    /// evidence lines is still found.
    pub fn task_events_of(
        &self,
        task: i64,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<TaskEventRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task, at, kind, body FROM task_event \
             WHERE task = ?1 AND kind = ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![task, kind, limit as i64], |r| {
                Ok(TaskEventRow {
                    id: r.get(0)?,
                    task: r.get(1)?,
                    at: r.get(2)?,
                    kind: r.get(3)?,
                    body: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
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

    /// Work its agent claimed done is finished: its tab closing leaves the task open and
    /// claimed, untied from the terminal, so it can still be checked and closed verified.
    #[test]
    fn a_claimed_task_outlives_its_terminal_for_its_check() {
        let (mut s, _, t) = store_with_terminal();
        let a = s.open_task(t, "ship 2.5.1", false, 10).unwrap();
        s.claim_task_done(a, "submitted to App Review", 11).unwrap();
        s.close_terminal(t, 12).unwrap();
        let row = s.task(a).unwrap().unwrap();
        assert_eq!(row.closed_at, None, "still open for its check");
        assert!(row.claimed_at.is_some());
        assert_eq!(row.terminal, None);
        s.close_task(a, Outcome::Verified, "in review, checked", 13)
            .unwrap();
        assert_eq!(
            s.task(a).unwrap().unwrap().outcome.as_deref(),
            Some("verified")
        );
        // A space deleted takes a claimed task with it: no board would show it again.
        let ws = s.create_workspace("gone", 20).unwrap();
        let u = s.create_terminal(6, ws, "/tmp", "/bin/sh", 21).unwrap();
        let b = s.open_task(u, "ship 2.5.2", false, 22).unwrap();
        s.claim_task_done(b, "done", 23).unwrap();
        s.close_terminal(u, 24).unwrap();
        s.delete_workspace(ws).unwrap();
        assert_eq!(
            s.task(b).unwrap().unwrap().outcome.as_deref(),
            Some("abandoned")
        );
    }

    /// Its check finds the repo from the folder: the terminal's while it has one, asleep too,
    /// and the one kept when a claimed task's tab closed.
    #[test]
    fn a_task_keeps_its_folder_past_its_tab() {
        let (s, _, t) = store_with_terminal();
        let a = s.open_task(t, "ship", false, 10).unwrap();
        s.set_cwd(t, "/w/demo-wt-ship").unwrap();
        assert_eq!(
            s.task(a).unwrap().unwrap().folder.as_deref(),
            Some("/w/demo-wt-ship")
        );
        s.claim_task_done(a, "merged", 11).unwrap();
        s.close_terminal(t, 12).unwrap();
        let row = s.task(a).unwrap().unwrap();
        assert_eq!(row.terminal, None);
        assert_eq!(row.folder.as_deref(), Some("/w/demo-wt-ship"));
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
    fn an_answer_takes_a_task_off_waiting_on_the_owner() {
        let (s, _, t) = store_with_terminal();
        let id = s.open_task(t, "x", false, 10).unwrap();
        assert_eq!(
            s.answer_waiting(t, 11, 11).unwrap(),
            None,
            "not waiting: nothing moves"
        );
        s.set_task_stage(id, "planning", 12).unwrap();
        s.set_task_stage(id, "waiting-human", 13).unwrap();
        s.claim_task_done(id, "asks before the merge", 14).unwrap();
        assert_eq!(
            s.answer_waiting(t, 15, 15).unwrap().as_deref(),
            Some("planning")
        );
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(row.stage, "planning", "the stage before it");
        assert_eq!(row.claimed_at, Some(14), "the claim stays");
        assert_eq!(s.answer_waiting(t, 16, 16).unwrap(), None, "said once");
        // Input from before the wait began is no answer to it.
        s.set_task_stage(id, "waiting-human", 20).unwrap();
        assert_eq!(
            s.answer_waiting(t, 19, 19).unwrap(),
            None,
            "typed before the wait"
        );
        assert_eq!(s.task(id).unwrap().unwrap().stage, "waiting-human");
        assert_eq!(
            s.answer_waiting(t, 20, 25).unwrap().as_deref(),
            Some("planning"),
            "typed as it began"
        );
        assert_eq!(
            s.task(id).unwrap().unwrap().updated_at,
            25,
            "moved when the prompt came"
        );
        // Waiting with no stage before it: implementing.
        let (other, _, u) = {
            let (s2, ws, u) = store_with_terminal();
            (s2, ws, u)
        };
        let fresh = other.open_task(u, "y", false, 10).unwrap();
        other.set_task_stage(fresh, "waiting-human", 11).unwrap();
        assert_eq!(
            other.answer_waiting(u, 12, 12).unwrap().as_deref(),
            Some("implementing")
        );
    }

    /// A question left unanswered puts the task on waiting with its claim kept, once, and his
    /// answer takes it back to the stage it had.
    #[test]
    fn an_unanswered_question_waits_on_the_owner_and_keeps_the_claim() {
        let (s, _, t) = store_with_terminal();
        assert_eq!(s.wait_on_owner(t, 9).unwrap(), None, "no task, nothing");
        let id = s.open_task(t, "x", false, 10).unwrap();
        s.set_task_stage(id, "verifying", 11).unwrap();
        s.claim_task_done(id, "merged", 12).unwrap();
        assert_eq!(s.wait_on_owner(t, 13).unwrap(), Some(id));
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(row.stage, "waiting-human");
        assert_eq!(row.claimed_at, Some(12), "the claim stays");
        assert_eq!(s.wait_on_owner(t, 14).unwrap(), None, "already waits");
        assert_eq!(
            s.answer_waiting(t, 15, 15).unwrap().as_deref(),
            Some("verifying")
        );
        // A new task, no stage written: back to the one it opened in.
        let (other, _, u) = store_with_terminal();
        let fresh = other.open_task(u, "y", false, 10).unwrap();
        let opened = other.task(fresh).unwrap().unwrap().stage;
        assert_eq!(other.wait_on_owner(u, 11).unwrap(), Some(fresh));
        assert_eq!(other.answer_waiting(u, 12, 12).unwrap(), Some(opened));
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

    /// The goal is rewritten in place, cleaned like an opened one, kept in the events,
    /// refused when too long or once closed; the space goal follows the same text rules,
    /// clears on empty, and dies with its space so a reused id starts blank.
    #[test]
    fn goals_are_rewritten_cleaned_and_bounded() {
        let (s, ws, t) = store_with_terminal();
        let id = s.open_task(t, "dostum bak su", false, 10).unwrap();
        assert!(
            s.set_task_goal(
                id,
                " wend search\nwithout\u{202E} embeddings\u{E0041}; done when top10 >= 45% ",
                11
            )
            .unwrap()
        );
        let row = s.task(id).unwrap().unwrap();
        assert_eq!(
            row.goal,
            "wend search without embeddings; done when top10 >= 45%"
        );
        assert_eq!(row.updated_at, 11);
        assert!(
            !s.set_task_goal(id, &row.goal, 12).unwrap(),
            "same text is no change"
        );
        let events = s.task_events(id, None, 10).unwrap();
        assert_eq!(events[0].kind, "goal");
        assert_eq!(
            events.last().unwrap().body,
            "dostum bak su",
            "the first words stay"
        );
        assert!(matches!(
            task_err(
                s.set_task_goal(id, &"x".repeat(GOAL_MAX + 1), 13)
                    .unwrap_err()
            ),
            TaskError::TooLong { .. }
        ));
        assert!(matches!(
            task_err(s.set_task_goal(id, " \n ", 13).unwrap_err()),
            TaskError::Empty(_)
        ));
        s.close_task(id, Outcome::Verified, "", 14).unwrap();
        assert!(matches!(
            task_err(s.set_task_goal(id, "late", 15).unwrap_err()),
            TaskError::Closed(_)
        ));
        assert_eq!(s.task_events_of(id, "goal", 5).unwrap().len(), 1);
        assert_eq!(s.closed_tasks_since(14, 5).unwrap().len(), 1);
        assert!(s.closed_tasks_since(15, 5).unwrap().is_empty());

        let goal_of = |s: &Store, ws: i64| {
            s.workspaces()
                .unwrap()
                .into_iter()
                .find(|w| w.id == ws)
                .and_then(|w| w.goal)
        };
        assert!(
            s.set_space_goal(ws, "ship the\nphone\u{200B} app", 20)
                .unwrap()
        );
        assert_eq!(goal_of(&s, ws).as_deref(), Some("ship the phone app"));
        assert!(s.set_space_goal(ws, "  ", 21).unwrap());
        assert_eq!(goal_of(&s, ws), None, "empty clears");
        assert!(
            s.set_space_goal(ws + 100, "x", 22)
                .is_ok_and(|found| !found)
        );
        assert!(s.set_space_goal(ws, &"y".repeat(GOAL_MAX + 1), 23).is_err());
        let other = s.create_workspace("other", 24).unwrap();
        assert!(s.set_space_goal(other, "gone soon", 25).unwrap());
        s.delete_workspace(other).unwrap();
        let again = s.create_workspace("again", 26).unwrap();
        assert_eq!(goal_of(&s, again), None, "a reused id starts with no goal");
    }
}

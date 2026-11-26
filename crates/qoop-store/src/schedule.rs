//! Schedules: an agent the owner asked qoop to start at a time, once or on a repeat.
//!
//! A row is the whole job: when (`cron` in local time, `at`, one moment, or `watch`, a command
//! whose new output starts the run), what to start
//! (`agent`, `prompt`) and where (`cwd`, `space`), and what became of the last try. The
//! daemon's loop reads the rows and decides; this file only keeps them and makes the two
//! moves that have to be exact.
//!
//! The first is `claim`: a slot is fired at most once, however many daemons, restarts or
//! handovers look at the row, because the claim is one compare and set on `last_slot`. The
//! second is `report`: the child that opens the terminal says how it went, and only for the
//! slot the row still holds, so a late report never overwrites a newer run. Ids come from
//! `AUTOINCREMENT`, so the id of a removed schedule is never given again and a run that is
//! still reporting cannot land on another job.
//!
//! Each run also has a row of its own in `schedule_run`, made when it starts and ended when its
//! agent's turn ends or its tab closes. A run row is written by its id, whatever slot the
//! schedule holds by then, so an edit or a newer run never loses an older run's report.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Store, StoreError};

/// The longest name a schedule has, in characters.
pub const NAME_MOST: usize = 40;

/// How many runs of one schedule are kept: the newest.
pub const RUNS_KEPT: i64 = 200;

/// One run of a schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRunRow {
    pub id: i64,
    pub schedule: i64,
    /// The slot the schedule held when the run started.
    pub slot: i64,
    pub started_at: i64,
    /// The tab the run opened, once it said.
    pub terminal: Option<i64>,
    /// The task its agent opened, read when the run ended.
    pub task: Option<i64>,
    /// `claimed` until the run reports, then `started`, `skipped: <why>` or `failed: <why>`.
    pub result: String,
    pub ended_at: Option<i64>,
    /// `done`, `failed`, `gone` or `skipped`; none while it runs.
    pub outcome: Option<String>,
    /// The agent's newest message when its turn ended.
    pub summary: Option<String>,
}

const RUN_COLUMNS: &str =
    "id, schedule, slot, started_at, terminal, task, result, ended_at, outcome, summary";

fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRunRow> {
    Ok(ScheduleRunRow {
        id: r.get(0)?,
        schedule: r.get(1)?,
        slot: r.get(2)?,
        started_at: r.get(3)?,
        terminal: r.get(4)?,
        task: r.get(5)?,
        result: r.get(6)?,
        ended_at: r.get(7)?,
        outcome: r.get(8)?,
        summary: r.get(9)?,
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// When a run that says `result` ended, and how: a claim or a start is still going, a failure
/// ended `failed`, and a skip ended `skipped` (its `result` says why: `skipped: cancelled` is a
/// run the owner cancelled). Clients read `result` before `outcome`, so an older one draws a
/// skip as it always did.
fn ending(result: &str, at: i64) -> (Option<i64>, Option<&'static str>) {
    match result {
        "claimed" | "started" => (None, None),
        _ if result.starts_with("failed: ") => (Some(at), Some("failed")),
        _ if result.starts_with("skipped: ") => (Some(at), Some("skipped")),
        _ => (Some(at), None),
    }
}

/// The run row a slot claimed while a run of its schedule is still open says this.
pub const SKIPPED_OPEN: &str = "skipped: its last run is still open";

/// What a due slot came to when it was claimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fired {
    /// Its run starts, under this row.
    Run(i64),
    /// A run of the schedule was still open: the slot's row says skipped.
    Skipped(i64),
}

/// The open-run rule of Run now and of a due slot, inside the caller's transaction: first the
/// runs that cannot still be going end `gone` (their tab is among `closed`, or they had no tab
/// for [`NO_TAB_FOR`] seconds), then whether any run of schedule `id` is still open (claimed,
/// or started and not ended).
fn open_after_sweep(tx: &Connection, id: i64, now: i64, closed: &[i64]) -> rusqlite::Result<bool> {
    const OPEN: &str = "ended_at IS NULL AND result IN ('claimed', 'started')";
    for terminal in closed {
        tx.execute(
            &format!(
                "UPDATE schedule_run SET ended_at = ?3, outcome = 'gone' \
                 WHERE schedule = ?1 AND terminal = ?2 AND {OPEN}"
            ),
            params![id, terminal, now],
        )?;
    }
    tx.execute(
        &format!(
            "UPDATE schedule_run SET ended_at = ?2, outcome = 'gone' \
             WHERE schedule = ?1 AND terminal IS NULL AND started_at < ?3 AND {OPEN}"
        ),
        params![id, now, now - NO_TAB_FOR],
    )?;
    tx.query_row(
        &format!("SELECT EXISTS (SELECT 1 FROM schedule_run WHERE schedule = ?1 AND {OPEN})"),
        params![id],
        |r| r.get(0),
    )
}

/// Take `slot` of schedule `id` from `expected` with nothing written of a run: a watch's
/// check. False when the schedule moved on or is paused.
fn claim_slot_in(
    tx: &Connection,
    id: i64,
    expected: Option<i64>,
    slot: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        "UPDATE schedule SET last_slot = ?3 \
         WHERE id = ?1 AND last_slot IS ?2 AND paused = 0",
        params![id, expected, slot],
    )? > 0)
}

/// Take `slot` of schedule `id` from `expected` with `result`, and its run's row: none when the
/// schedule moved on or is paused.
fn claim_in(
    tx: &Connection,
    id: i64,
    expected: Option<i64>,
    slot: i64,
    now: i64,
    result: &str,
) -> rusqlite::Result<Option<i64>> {
    let won = tx.execute(
        "UPDATE schedule SET last_slot = ?3, last_at = ?4, last_result = ?5, \
         last_terminal = NULL \
         WHERE id = ?1 AND last_slot IS ?2 AND paused = 0",
        params![id, expected, slot, now, result],
    )? > 0;
    if !won {
        return Ok(None);
    }
    insert_run(tx, id, slot, now, result, None).map(Some)
}

/// Pause schedule `id` for good, with `done_at` its first `at`, inside the caller's transaction.
/// False when there is no such schedule.
fn complete_in(tx: &Connection, id: i64, at: i64) -> rusqlite::Result<bool> {
    let there = tx.execute("UPDATE schedule SET paused = 1 WHERE id = ?1", params![id])? > 0;
    if there {
        tx.execute(
            "INSERT INTO schedule_mark (schedule, done_at) VALUES (?1, ?2) \
             ON CONFLICT(schedule) DO UPDATE SET done_at = COALESCE(done_at, excluded.done_at)",
            params![id, at],
        )?;
    }
    Ok(there)
}

/// What a Run now came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunNow {
    Started(i64),
    /// A run of the schedule is still open: no second one.
    Open,
    NoSchedule,
}

/// How long a run may go without a tab before it counts as one that never started.
pub const NO_TAB_FOR: i64 = 120;

/// Record a run of `schedule` and keep only its newest [`RUNS_KEPT`]. Answers its id.
fn insert_run(
    conn: &Connection,
    schedule: i64,
    slot: i64,
    at: i64,
    result: &str,
    terminal: Option<i64>,
) -> rusqlite::Result<i64> {
    let (ended_at, outcome) = ending(result, at);
    conn.execute(
        "INSERT INTO schedule_run (schedule, slot, started_at, terminal, result, ended_at, outcome) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![schedule, slot, at, terminal, result, ended_at, outcome],
    )?;
    let run = conn.last_insert_rowid();
    conn.execute(
        "DELETE FROM schedule_run WHERE schedule = ?1 AND id NOT IN \
         (SELECT id FROM schedule_run WHERE schedule = ?1 ORDER BY id DESC LIMIT ?2)",
        params![schedule, RUNS_KEPT],
    )?;
    Ok(run)
}

/// Mark run `run` failed with `why`, only while it still reads `claimed`.
fn fail_run_in(conn: &Connection, run: i64, why: &str, at: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE schedule_run SET result = ?2, ended_at = ?3, outcome = 'failed' \
         WHERE id = ?1 AND result = 'claimed'",
        params![run, format!("failed: {why}"), at],
    )? > 0)
}

/// Whether `run` is the newest run of `schedule`: nothing started after it, so how it went is
/// what the schedule's `last_*` say even when an edit moved the slot.
fn newest_run(conn: &Connection, schedule: i64, run: Option<i64>) -> rusqlite::Result<bool> {
    let Some(run) = run else {
        return Ok(false);
    };
    let newest: Option<i64> = conn.query_row(
        "SELECT MAX(id) FROM schedule_run WHERE schedule = ?1",
        params![schedule],
        |r| r.get(0),
    )?;
    Ok(newest == Some(run))
}

/// What a schedule write refused for: the message is the answer to give.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error(
        "a schedule is named with letters, digits, dots, dashes or underscores, at most {NAME_MOST} characters, and not only digits (a number is an id)"
    )]
    BadName,
    #[error("there is already a schedule named {0}")]
    NameTaken(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRow {
    pub id: i64,
    pub name: String,
    /// Five field cron text in local time, or `None` for a one-off.
    pub cron: Option<String>,
    /// The moment of a one-off, in unix seconds.
    pub at: Option<i64>,
    pub agent: String,
    pub cwd: String,
    pub space: Option<String>,
    pub prompt: String,
    pub paused: bool,
    pub created_at: i64,
    /// The last slot claimed or skipped: the key a slot is never fired twice by.
    pub last_slot: Option<i64>,
    pub last_at: Option<i64>,
    /// `claimed`, `started`, `skipped: <why>` or `failed: <why>`.
    pub last_result: Option<String>,
    /// The terminal the last run opened.
    pub last_terminal: Option<i64>,
    /// The shell command of a watch: checked every few minutes, its new lines start the run.
    /// Kept in `schedule_watch`, so an older binary reads the row as one with no timing.
    pub watch: Option<String>,
    /// Every slot of a repeat up to this one is cancelled: the owner skipped the next run, once
    /// or more (a one-off's cancel completes it instead). Kept in `schedule_mark`, as `done_at`
    /// is.
    pub skip_until: Option<i64>,
    /// When the owner marked it complete: paused for good, its runs kept.
    pub done_at: Option<i64>,
}

const COLUMNS: &str = "s.id, s.name, s.cron, s.at, s.agent, s.cwd, s.space, s.prompt, s.paused, \
                       s.created_at, s.last_slot, s.last_at, s.last_result, s.last_terminal, w.command, \
                       m.skip_until, m.done_at";

const FROM: &str = "schedule s LEFT JOIN schedule_watch w ON w.schedule = s.id \
                    LEFT JOIN schedule_mark m ON m.schedule = s.id";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRow> {
    Ok(ScheduleRow {
        id: r.get(0)?,
        name: r.get(1)?,
        cron: r.get(2)?,
        at: r.get(3)?,
        agent: r.get(4)?,
        cwd: r.get(5)?,
        space: r.get(6)?,
        prompt: r.get(7)?,
        paused: r.get::<_, i64>(8)? != 0,
        created_at: r.get(9)?,
        last_slot: r.get(10)?,
        last_at: r.get(11)?,
        last_result: r.get(12)?,
        last_terminal: r.get(13)?,
        watch: r.get(14)?,
        skip_until: r.get(15)?,
        done_at: r.get(16)?,
    })
}

/// Whether `name` is one a schedule may have.
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= NAME_MOST
        && !name.bytes().all(|b| b.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl Store {
    /// Record a schedule and answer its id; `row.id`, the `last_*` columns, `skip_until` and
    /// `done_at` are ignored.
    pub fn add_schedule(&self, row: &ScheduleRow) -> Result<i64, StoreError> {
        if !name_ok(&row.name) {
            return Err(ScheduleError::BadName.into());
        }
        let tx = self.conn.unchecked_transaction()?;
        let taken = tx
            .query_row(
                "SELECT 1 FROM schedule WHERE name = ?1",
                params![row.name],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Err(ScheduleError::NameTaken(row.name.clone()).into());
        }
        tx.execute(
            "INSERT INTO schedule (name, cron, at, agent, cwd, space, prompt, paused, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
            params![
                row.name,
                row.cron,
                row.at,
                row.agent,
                row.cwd,
                row.space,
                row.prompt,
                row.created_at
            ],
        )?;
        let id = tx.last_insert_rowid();
        if let Some(command) = &row.watch {
            tx.execute(
                "INSERT INTO schedule_watch (schedule, command) VALUES (?1, ?2)",
                params![id, command],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Every schedule, oldest first.
    pub fn schedules(&self) -> Result<Vec<ScheduleRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM {FROM} ORDER BY s.id"))?;
        Ok(stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?)
    }

    /// One schedule by its id.
    pub fn schedule(&self, id: i64) -> Result<Option<ScheduleRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM {FROM} WHERE s.id = ?1"),
                params![id],
                row,
            )
            .optional()?)
    }

    /// Stop a schedule firing. Answers whether there was one.
    pub fn pause_schedule(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("UPDATE schedule SET paused = 1 WHERE id = ?1", params![id])?
            > 0)
    }

    /// Let a paused schedule fire again from `now` on: its last slot becomes `now`, so what
    /// came due while it was paused is not caught up. A schedule that is not paused is left
    /// alone, since moving its last slot would drop a catch-up that is owed.
    pub fn resume_schedule(&self, id: i64, now: i64) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE schedule SET paused = 0, last_slot = ?2 WHERE id = ?1 AND paused = 1",
            params![id, now],
        )? > 0)
    }

    /// Cancel every run of schedule `id` up to `slot`, its next one, while it still holds the
    /// last slot the caller decided on (`expected`) and is not paused: a claim in between makes
    /// the caller's next run a stale one. Nothing else of the schedule changes. Answers whether
    /// it was written.
    pub fn skip_schedule(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "INSERT INTO schedule_mark (schedule, skip_until) \
             SELECT id, ?3 FROM schedule WHERE id = ?1 AND last_slot IS ?2 AND paused = 0 \
             ON CONFLICT(schedule) DO UPDATE SET skip_until = excluded.skip_until",
            params![id, expected, slot],
        )? > 0)
    }

    /// Mark a schedule complete at `at`: paused for good, its runs kept. Marked twice, it keeps
    /// the first time. Answers whether there was one.
    pub fn complete_schedule(&self, id: i64, at: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let there = complete_in(&tx, id, at)?;
        tx.commit()?;
        Ok(there)
    }

    /// Cancel the one run of a one-off at `slot`: claimed as `result` (its run row says why) and
    /// complete, in one transaction, so it reads as done at once rather than on with nothing
    /// planned. From `expected` only, as [`Store::skip_schedule`]. Answers whether it was
    /// written.
    pub fn skip_once(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        result: &str,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if claim_in(&tx, id, expected, slot, now, result)?.is_none() {
            return Ok(false);
        }
        complete_in(&tx, id, now)?;
        tx.commit()?;
        Ok(true)
    }

    /// Forget a schedule. Answers whether there was one.
    pub fn remove_schedule(&self, id: i64) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM schedule WHERE id = ?1", params![id])?
            > 0)
    }

    /// Take `slot` for a run: one compare and set on the slot the caller saw last
    /// (`expected`), writing the slot, when, the `result` (`claimed`, or `skipped: ...`) and
    /// no terminal yet. Answers whether this call changed the row, which is whether it won:
    /// a second caller with the same `expected` changes nothing. A paused row is not taken.
    /// No `result` is a watch's check, which moves the slot and leaves the last run as it was,
    /// so the panel keeps showing the last start while checks come and go. A `result` also
    /// makes the slot's run row ([`Store::claim_run`]).
    pub fn claim_schedule(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        result: Option<&str>,
    ) -> Result<bool, StoreError> {
        if let Some(result) = result {
            return Ok(self.claim_run(id, expected, slot, now, result)?.is_some());
        }
        Ok(claim_slot_in(&self.conn, id, expected, slot)?)
    }

    /// Claim a watch's due check by Run now's rule: while one of its runs is still open (the
    /// agent its last check started), the slot is taken and the check is not run, so the lines
    /// it would have seen wait for the next one. One write transaction, as [`Store::claim_fire`].
    /// Answers whether to check, or none when somebody else took the slot first.
    pub fn claim_check(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        closed: &[i64],
    ) -> Result<Option<bool>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let open = open_after_sweep(&tx, id, now, closed)?;
        let won = claim_slot_in(&tx, id, expected, slot)?;
        tx.commit()?;
        Ok(won.then_some(!open))
    }

    /// [`Store::claim_schedule`] with a result, and the run row of the slot made in the same
    /// transaction (a skip's ended as it starts). Answers the run's id when this call won.
    pub fn claim_run(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        result: &str,
    ) -> Result<Option<i64>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let run = claim_in(&tx, id, expected, slot, now, result)?;
        tx.commit()?;
        Ok(run)
    }

    /// Claim a due slot to start its run, by the same rule as Run now: while a run of the
    /// schedule is still open (one that waits on a question, or a Run now just claimed), the
    /// slot is claimed as skipped and no second agent starts. One write transaction: the sweep
    /// of runs that cannot still be going (`closed` tabs, no tab in time), the check and the
    /// claim. None when somebody else took the slot first.
    pub fn claim_fire(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        closed: &[i64],
    ) -> Result<Option<Fired>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let open = open_after_sweep(&tx, id, now, closed)?;
        let result = if open { SKIPPED_OPEN } else { "claimed" };
        let run = claim_in(&tx, id, expected, slot, now, result)?;
        tx.commit()?;
        Ok(run.map(|run| {
            if open {
                Fired::Skipped(run)
            } else {
                Fired::Run(run)
            }
        }))
    }

    /// A run the owner asked for now: a row of its own under the slot the schedule holds, with
    /// nothing claimed. Answers its id, or none when there is no such schedule.
    pub fn start_run(&self, id: i64, slot: i64, now: i64) -> Result<Option<i64>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let there = tx
            .query_row("SELECT 1 FROM schedule WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !there {
            return Ok(None);
        }
        let run = insert_run(&tx, id, slot, now, "claimed", None)?;
        tx.commit()?;
        Ok(Some(run))
    }

    /// A run the owner asked for now, unless a run of the schedule is still open (claimed, or
    /// started and not ended), whichever of its runs that is: a skipped slot written after a
    /// run that still works is newer and already ended, and must not let a second run start.
    /// The check and the new row are one write transaction (`BEGIN IMMEDIATE`), so two asks at
    /// once make one run. Before the check, a run that cannot still be going is ended `gone`:
    /// one whose tab the daemon found closed (`closed`, terminal numbers), and one that has had
    /// no tab for [`NO_TAB_FOR`] seconds.
    pub fn run_now(
        &self,
        id: i64,
        slot: i64,
        now: i64,
        closed: &[i64],
    ) -> Result<RunNow, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let there = tx
            .query_row("SELECT 1 FROM schedule WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !there {
            return Ok(RunNow::NoSchedule);
        }
        if open_after_sweep(&tx, id, now, closed)? {
            tx.commit()?;
            return Ok(RunNow::Open);
        }
        let run = insert_run(&tx, id, slot, now, "claimed", None)?;
        tx.commit()?;
        Ok(RunNow::Started(run))
    }

    /// The open runs of schedule `id` and the tabs they opened: what the daemon checks for
    /// tabs that closed before a Run now.
    pub fn open_run_tabs(&self, id: i64) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT terminal FROM schedule_run WHERE schedule = ?1 AND ended_at IS NULL \
             AND result IN ('claimed', 'started') AND terminal IS NOT NULL",
        )?;
        Ok(stmt
            .query_map(params![id], |r| r.get(0))?
            .collect::<Result<Vec<i64>, _>>()?)
    }

    /// Mark run `run` failed with `why`, only while it still reads `claimed`: its child ended
    /// without reporting.
    pub fn fail_run(&self, run: i64, why: &str) -> Result<bool, StoreError> {
        fail_run_in(&self.conn, run, why, unix_now()).map_err(StoreError::from)
    }

    /// The runs started that have not ended, with the tab each opened.
    pub fn open_runs(&self) -> Result<Vec<ScheduleRunRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_run \
             WHERE result = 'started' AND ended_at IS NULL AND terminal IS NOT NULL"
        ))?;
        Ok(stmt
            .query_map([], run_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// End run `run` at `at` with `outcome` and `summary`, once: the task is the newest its
    /// tab opened since the run started. Answers whether it was still going.
    pub fn end_run(
        &self,
        run: i64,
        at: i64,
        outcome: &str,
        summary: Option<&str>,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE schedule_run SET ended_at = ?2, outcome = ?3, summary = ?4, \
             task = (SELECT t.id FROM task t WHERE t.terminal = schedule_run.terminal \
                     AND t.opened_at >= schedule_run.started_at ORDER BY t.id DESC LIMIT 1) \
             WHERE id = ?1 AND ended_at IS NULL",
            params![run, at, outcome, summary],
        )? > 0)
    }

    /// The runs of `schedule`, newest first, at most `limit`, started before `before` when
    /// given: the next page is asked with the oldest `started_at` of this one.
    pub fn schedule_runs(
        &self,
        schedule: i64,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<ScheduleRunRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_run \
             WHERE schedule = ?1 AND (?2 IS NULL OR started_at < ?2) \
             ORDER BY started_at DESC, id DESC LIMIT ?3"
        ))?;
        Ok(stmt
            .query_map(params![schedule, before, limit], run_row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Write `row` over the schedule of its id, keeping the id, its runs, whether it is paused
    /// and how its last run went. `moved` is when a new timing starts: it becomes the last
    /// slot, so nothing of the old timing is caught up, and the runs cancelled of the old
    /// timing are forgotten with it. A watch whose command changed starts
    /// over with no lines seen. Answers whether there was one.
    pub fn change_schedule(
        &self,
        row: &ScheduleRow,
        moved: Option<i64>,
    ) -> Result<bool, StoreError> {
        if !name_ok(&row.name) {
            return Err(ScheduleError::BadName.into());
        }
        let tx = self.conn.unchecked_transaction()?;
        let taken = tx
            .query_row(
                "SELECT 1 FROM schedule WHERE name = ?1 AND id != ?2",
                params![row.name, row.id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if taken {
            return Err(ScheduleError::NameTaken(row.name.clone()).into());
        }
        let changed = tx.execute(
            "UPDATE schedule SET name = ?2, cron = ?3, at = ?4, agent = ?5, cwd = ?6, space = ?7, \
             prompt = ?8, last_slot = COALESCE(?9, last_slot) WHERE id = ?1",
            params![
                row.id, row.name, row.cron, row.at, row.agent, row.cwd, row.space, row.prompt,
                moved
            ],
        )? > 0;
        if !changed {
            return Ok(false);
        }
        if moved.is_some() {
            tx.execute(
                "UPDATE schedule_mark SET skip_until = NULL WHERE schedule = ?1",
                params![row.id],
            )?;
        }
        match &row.watch {
            Some(command) => tx.execute(
                "INSERT INTO schedule_watch (schedule, command) VALUES (?1, ?2) \
                 ON CONFLICT(schedule) DO UPDATE SET \
                 seen = CASE WHEN command = excluded.command THEN seen END, \
                 command = excluded.command",
                params![row.id, command],
            )?,
            None => tx.execute(
                "DELETE FROM schedule_watch WHERE schedule = ?1",
                params![row.id],
            )?,
        };
        tx.commit()?;
        Ok(true)
    }

    /// The lines a watch's last check saw; `None` when it was never checked.
    pub fn watch_seen(&self, id: i64) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT seen FROM schedule_watch WHERE schedule = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// How the run of `slot` went, applied only while the row still holds that slot. A row
    /// that never fired holds slot 0, which is what a run asked for by hand carries. Answers
    /// whether it was applied. `seen` is a watch's new baseline, written with it; the result
    /// `checked` is a watch's check that started nothing, which writes the baseline only.
    ///
    /// `run` is the run's own row, written whatever slot the schedule holds now; the newest run
    /// also counts as holding it, so an edit that moved the slot during a run keeps its report.
    /// A watch's run that started an agent has no row yet (the check decided it): it is made
    /// here, in the same transaction.
    pub fn report_schedule(
        &self,
        id: i64,
        slot: i64,
        result: &str,
        terminal: Option<i64>,
        seen: Option<&str>,
        run: Option<i64>,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let at = unix_now();
        let ran = match run {
            _ if result == "checked" => false,
            Some(run) => {
                let (ended_at, outcome) = ending(result, at);
                tx.execute(
                    "UPDATE schedule_run SET result = ?3, terminal = COALESCE(?4, terminal), \
                     ended_at = COALESCE(ended_at, ?5), outcome = COALESCE(outcome, ?6) \
                     WHERE id = ?1 AND schedule = ?2",
                    params![run, id, result, terminal, ended_at, outcome],
                )? > 0
            }
            None if result == "started" => {
                let watch = tx
                    .query_row(
                        "SELECT 1 FROM schedule_watch WHERE schedule = ?1",
                        params![id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if watch {
                    insert_run(&tx, id, slot, at, result, terminal)?;
                }
                watch
            }
            None => false,
        };
        let held = tx
            .query_row(
                "SELECT 1 FROM schedule WHERE id = ?1 AND COALESCE(last_slot, 0) = ?2",
                params![id, slot],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
            || newest_run(&tx, id, run)?;
        if !held {
            tx.commit()?;
            return Ok(ran);
        }
        if let Some(seen) = seen {
            tx.execute(
                "UPDATE schedule_watch SET seen = ?2 WHERE schedule = ?1",
                params![id, seen],
            )?;
        }
        if result != "checked" {
            tx.execute(
                // `last_at` too: a run the owner asked for now claimed nothing, so the time it
                // shows would otherwise be the last scheduled one's.
                "UPDATE schedule SET last_result = ?3, last_terminal = ?4, \
                 last_at = CAST(strftime('%s', 'now') AS INTEGER) \
                 WHERE id = ?1",
                params![id, slot, result, terminal],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Mark the run of `slot` failed with `why`, only while it still reads `claimed`: the
    /// child that should have reported ended without doing so. Its run row `run` is marked
    /// too; the newest run counts as holding the slot, as in [`Store::report_schedule`].
    pub fn fail_claimed_schedule(
        &self,
        id: i64,
        slot: i64,
        run: Option<i64>,
        why: &str,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let newest = newest_run(&tx, id, run)?;
        let failed = tx.execute(
            "UPDATE schedule SET last_result = ?3 \
             WHERE id = ?1 AND (COALESCE(last_slot, 0) = ?2 OR ?4) AND last_result = 'claimed'",
            params![id, slot, format!("failed: {why}"), newest],
        )? > 0;
        let marked = match run {
            Some(run) => fail_run_in(&tx, run, why, unix_now())?,
            None => false,
        };
        tx.commit()?;
        Ok(failed || marked)
    }

    /// Runs still `claimed` when a daemon starts belong to a daemon that is gone, so their
    /// child never reported: they become `failed: lost at restart`, their run rows too.
    /// Answers how many schedules.
    pub fn fail_stale_claims(&self) -> Result<usize, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let lost = tx.execute(
            "UPDATE schedule SET last_result = 'failed: lost at restart' \
             WHERE last_result = 'claimed'",
            [],
        )?;
        tx.execute(
            "UPDATE schedule_run SET result = 'failed: lost at restart', outcome = 'failed', \
             ended_at = ?1 WHERE result = 'claimed'",
            params![unix_now()],
        )?;
        tx.commit()?;
        Ok(lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn made(name: &str) -> ScheduleRow {
        ScheduleRow {
            id: 0,
            name: name.into(),
            cron: Some("0 9 * * *".into()),
            at: None,
            agent: "claude".into(),
            cwd: "/tmp".into(),
            space: None,
            prompt: "look at the inbox".into(),
            paused: false,
            created_at: 100,
            last_slot: None,
            last_at: None,
            last_result: None,
            last_terminal: None,
            watch: None,
            skip_until: None,
            done_at: None,
        }
    }

    #[test]
    fn a_name_is_unique_and_never_only_digits() {
        let s = Store::in_memory().expect("open");
        assert_eq!(s.add_schedule(&made("morning")).unwrap(), 1);
        assert!(matches!(
            s.add_schedule(&made("morning")),
            Err(StoreError::Schedule(ScheduleError::NameTaken(_)))
        ));
        for bad in ["", "12", "has space", "a/b"] {
            assert!(
                matches!(
                    s.add_schedule(&made(bad)),
                    Err(StoreError::Schedule(ScheduleError::BadName))
                ),
                "{bad:?}"
            );
        }
        assert!(s.add_schedule(&made("2nd-run")).is_ok());
    }

    /// A skip is written only from the last slot the caller saw and never on a paused row;
    /// a new timing forgets it, and a remove takes the mark with the schedule.
    #[test]
    fn a_skip_holds_from_the_slot_it_saw_and_a_new_timing_forgets_it() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(s.skip_schedule(id, None, 900).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!((held.skip_until, held.last_slot), (Some(900), None));
        // A second skip moves it on; one from a slot the row no longer holds does nothing.
        assert!(s.skip_schedule(id, None, 960).unwrap());
        assert!(!s.skip_schedule(id, Some(500), 1020).unwrap());
        assert!(!s.skip_schedule(id + 9, None, 1020).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().skip_until, Some(960));
        assert!(s.pause_schedule(id).unwrap());
        assert!(!s.skip_schedule(id, None, 1020).unwrap(), "paused");
        // An edit that keeps the timing keeps the skip; a new timing drops it.
        let same = ScheduleRow { id, ..made("a") };
        assert!(s.change_schedule(&same, None).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().skip_until, Some(960));
        assert!(s.change_schedule(&same, Some(700)).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().skip_until, None);
        assert!(s.remove_schedule(id).unwrap());
        let marks: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM schedule_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marks, 0);
    }

    /// Complete pauses for good with the first time it was marked, and keeps the runs.
    #[test]
    fn complete_pauses_keeps_the_runs_and_the_first_time() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        s.claim_run(id, None, 500, 501, "claimed").unwrap().unwrap();
        assert!(s.complete_schedule(id, 800).unwrap());
        assert!(s.complete_schedule(id, 900).unwrap());
        assert!(!s.complete_schedule(id + 9, 900).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!((held.paused, held.done_at), (true, Some(800)));
        assert_eq!(runs(&s, id).len(), 1);
    }

    /// A one-off's cancel writes its run row and completes it together, and only from the slot
    /// the caller saw.
    #[test]
    fn a_cancelled_one_off_is_claimed_and_complete_at_once() {
        let s = Store::in_memory().expect("open");
        let once = ScheduleRow {
            cron: None,
            at: Some(900),
            ..made("once")
        };
        let id = s.add_schedule(&once).unwrap();
        assert!(
            !s.skip_once(id, Some(5), 900, 800, "skipped: cancelled")
                .unwrap()
        );
        assert!(runs(&s, id).is_empty());
        assert!(!s.schedule(id).unwrap().unwrap().paused);
        assert!(
            s.skip_once(id, None, 900, 800, "skipped: cancelled")
                .unwrap()
        );
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(
            (held.last_slot, held.paused, held.done_at),
            (Some(900), true, Some(800))
        );
        let run = &runs(&s, id)[0];
        assert_eq!(
            (run.slot, run.result.as_str(), run.outcome.as_deref()),
            (900, "skipped: cancelled", Some("skipped"))
        );
    }

    /// Two claims of one slot from the same last slot: one changes the row, the other none.
    #[test]
    fn claiming_a_slot_twice_changes_one_row_then_zero() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(
            s.claim_schedule(id, None, 500, 501, Some("claimed"))
                .unwrap()
        );
        assert!(
            !s.claim_schedule(id, None, 500, 502, Some("claimed"))
                .unwrap(),
            "the second caller lost"
        );
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(held.last_slot, Some(500));
        assert_eq!(held.last_at, Some(501));
        assert_eq!(held.last_result.as_deref(), Some("claimed"));
        assert_eq!(held.last_terminal, None);
        // The next slot is claimed from the one just taken.
        assert!(
            s.claim_schedule(id, Some(500), 560, 561, Some("skipped: too old"))
                .unwrap()
        );
        assert!(
            !s.claim_schedule(id, Some(500), 560, 561, Some("claimed"))
                .unwrap()
        );
    }

    #[test]
    fn a_paused_row_is_not_claimed_and_resume_banks_nothing() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(s.pause_schedule(id).unwrap());
        assert!(
            !s.claim_schedule(id, None, 500, 501, Some("claimed"))
                .unwrap()
        );
        assert!(s.resume_schedule(id, 900).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert!(!held.paused);
        assert_eq!(held.last_slot, Some(900));
        // Resuming a row that is not paused moves nothing.
        assert!(!s.resume_schedule(id, 1000).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().last_slot, Some(900));
        assert!(!s.pause_schedule(77).unwrap());
    }

    #[test]
    fn a_report_lands_only_on_the_slot_the_row_still_holds() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(
            s.claim_schedule(id, None, 500, 501, Some("claimed"))
                .unwrap()
        );
        assert!(
            s.report_schedule(id, 500, "started", Some(7), None, None)
                .unwrap()
        );
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(held.last_result.as_deref(), Some("started"));
        assert_eq!(held.last_terminal, Some(7));
        assert!(
            !s.report_schedule(id, 440, "failed: late", None, None, None)
                .unwrap(),
            "a report for an older slot is dropped"
        );
        assert_eq!(
            s.schedule(id).unwrap().unwrap().last_result.as_deref(),
            Some("started")
        );
        // A row that never fired holds slot 0: what a run by hand reports against.
        let fresh = s.add_schedule(&made("b")).unwrap();
        assert!(
            s.report_schedule(fresh, 0, "started", Some(9), None, None)
                .unwrap()
        );
    }

    #[test]
    fn a_claim_left_over_from_a_dead_daemon_becomes_failed() {
        let s = Store::in_memory().expect("open");
        let a = s.add_schedule(&made("a")).unwrap();
        let b = s.add_schedule(&made("b")).unwrap();
        s.claim_schedule(a, None, 500, 501, Some("claimed"))
            .unwrap();
        s.claim_schedule(b, None, 500, 501, Some("claimed"))
            .unwrap();
        s.report_schedule(b, 500, "started", Some(3), None, None)
            .unwrap();
        assert_eq!(s.fail_stale_claims().unwrap(), 1);
        assert_eq!(
            s.schedule(a).unwrap().unwrap().last_result.as_deref(),
            Some("failed: lost at restart")
        );
        assert_eq!(
            s.schedule(b).unwrap().unwrap().last_result.as_deref(),
            Some("started")
        );
        // One run that ended without reporting, named by its slot.
        s.claim_schedule(a, Some(500), 560, 561, Some("claimed"))
            .unwrap();
        assert!(!s.fail_claimed_schedule(a, 500, None, "x").unwrap());
        assert!(
            s.fail_claimed_schedule(a, 560, None, "exited with 1")
                .unwrap()
        );
        assert_eq!(
            s.schedule(a).unwrap().unwrap().last_result.as_deref(),
            Some("failed: exited with 1")
        );
    }

    #[test]
    fn a_removed_id_is_not_given_again() {
        let s = Store::in_memory().expect("open");
        let a = s.add_schedule(&made("a")).unwrap();
        assert!(s.remove_schedule(a).unwrap());
        assert!(!s.remove_schedule(a).unwrap());
        assert_eq!(s.add_schedule(&made("a")).unwrap(), a + 1);
        assert_eq!(s.schedules().unwrap().len(), 1);
    }

    #[test]
    fn a_watch_keeps_its_command_and_its_baseline_and_goes_with_its_schedule() {
        let s = Store::in_memory().expect("open");
        let row = ScheduleRow {
            cron: None,
            watch: Some("gh issue list".into()),
            ..made("issues")
        };
        let id = s.add_schedule(&row).unwrap();
        assert_eq!(
            s.schedule(id).unwrap().unwrap().watch.as_deref(),
            Some("gh issue list")
        );
        assert_eq!(
            s.schedules().unwrap()[0].watch.as_deref(),
            Some("gh issue list")
        );
        assert_eq!(s.watch_seen(id).unwrap(), None, "never checked");
        // A run starts and reports; then a check claims the next slot and keeps the run shown.
        assert!(s.claim_schedule(id, None, 500, 500, None).unwrap());
        assert!(
            !s.claim_schedule(id, None, 500, 500, None).unwrap(),
            "once per slot"
        );
        assert!(
            s.report_schedule(id, 500, "started", Some(4), Some("#1 a"), None)
                .unwrap()
        );
        assert!(s.claim_schedule(id, Some(500), 800, 800, None).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(
            (
                held.last_slot,
                held.last_result.as_deref(),
                held.last_terminal
            ),
            (Some(800), Some("started"), Some(4))
        );
        let shown_at = held.last_at;
        // A check that started nothing writes its baseline and nothing else.
        assert!(
            s.report_schedule(id, 800, "checked", None, Some("#1 a\n#2 b"), None)
                .unwrap()
        );
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(
            (
                held.last_result.as_deref(),
                held.last_terminal,
                held.last_at
            ),
            (Some("started"), Some(4), shown_at)
        );
        assert_eq!(s.watch_seen(id).unwrap().as_deref(), Some("#1 a\n#2 b"));
        // A late report of an older slot writes no baseline.
        assert!(
            !s.report_schedule(id, 500, "checked", None, Some("x"), None)
                .unwrap()
        );
        assert_eq!(s.watch_seen(id).unwrap().as_deref(), Some("#1 a\n#2 b"));
        assert!(s.remove_schedule(id).unwrap());
        let left: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM schedule_watch", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "the watch goes with its schedule");
    }

    fn runs(s: &Store, id: i64) -> Vec<ScheduleRunRow> {
        s.schedule_runs(id, None, 100).unwrap()
    }

    /// Run now is refused while any run of the schedule is open, a newer skipped row on top
    /// of a working run included; a run whose tab closed, or that never got one in time, no
    /// longer holds it back.
    #[test]
    fn run_now_waits_for_every_open_run() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        let first = s.claim_run(id, None, 500, 501, "claimed").unwrap().unwrap();
        assert!(
            s.report_schedule(id, 500, "started", Some(7), None, Some(first))
                .unwrap()
        );
        // The next slot is skipped while the first works: a newer row, already ended.
        let skipped = s
            .claim_run(id, Some(500), 600, 601, "skipped: the last run still works")
            .unwrap();
        assert!(skipped.is_some(), "a skipped row is written");
        assert_eq!(s.run_now(id, 600, 602, &[]).unwrap(), RunNow::Open);
        assert_eq!(s.open_run_tabs(id).unwrap(), [7]);
        // Its tab closed: it ended, and Run now starts one.
        let started = s.run_now(id, 600, 603, &[7]).unwrap();
        assert!(matches!(started, RunNow::Started(_)), "{started:?}");
        assert_eq!(
            s.run_now(id, 600, 604, &[]).unwrap(),
            RunNow::Open,
            "the new one is open"
        );
        // A run with no tab long after it was claimed never started.
        let later = 604 + NO_TAB_FOR + 1;
        assert!(matches!(
            s.run_now(id, 600, later, &[]).unwrap(),
            RunNow::Started(_)
        ));
        assert_eq!(s.run_now(77, 0, later, &[]).unwrap(), RunNow::NoSchedule);
    }

    /// A due slot while a run of the schedule is still open is skipped, whether that run waits
    /// on a question (started, not ended) or is a Run now just claimed; once it ends, the next
    /// due slot starts its run.
    #[test]
    fn a_due_slot_waits_for_an_open_run_as_run_now_does() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        let Some(Fired::Run(first)) = s.claim_fire(id, None, 500, 501, &[]).unwrap() else {
            panic!("the first slot starts");
        };
        // It started and now waits on a question: open, not ended.
        s.report_schedule(id, 500, "started", Some(7), None, Some(first))
            .unwrap();
        let skipped = s.claim_fire(id, Some(500), 600, 601, &[]).unwrap();
        assert!(matches!(skipped, Some(Fired::Skipped(_))), "{skipped:?}");
        let rows = runs(&s, id);
        assert_eq!(rows[0].result, SKIPPED_OPEN);
        // Its tab closed: the next slot starts.
        let next = s.claim_fire(id, Some(600), 700, 701, &[7]).unwrap();
        assert!(matches!(next, Some(Fired::Run(_))), "{next:?}");
        // A Run now claimed (no tab yet) holds the next slot back too.
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("b")).unwrap();
        assert!(matches!(
            s.run_now(id, 0, 1_000, &[]).unwrap(),
            RunNow::Started(_)
        ));
        let held = s.claim_fire(id, None, 1_010, 1_011, &[]).unwrap();
        assert!(matches!(held, Some(Fired::Skipped(_))), "{held:?}");
    }

    /// A watch whose last check started an agent that is still open does not check again
    /// (no second agent beside it) until that tab closes; the slot still moves on.
    #[test]
    fn a_watch_check_waits_for_its_open_run() {
        let s = Store::in_memory().expect("open");
        let row = ScheduleRow {
            cron: None,
            watch: Some("gh issue list".into()),
            ..made("issues")
        };
        let id = s.add_schedule(&row).unwrap();
        assert_eq!(s.claim_check(id, None, 500, 500, &[]).unwrap(), Some(true));
        assert_eq!(
            s.claim_check(id, None, 500, 500, &[]).unwrap(),
            None,
            "once per slot"
        );
        // That check found something and started an agent, still open in tab 4.
        assert!(
            s.report_schedule(id, 500, "started", Some(4), Some("#1 a"), None)
                .unwrap()
        );
        assert_eq!(
            s.claim_check(id, Some(500), 600, 600, &[]).unwrap(),
            Some(false),
            "no check beside the open run"
        );
        assert_eq!(s.schedule(id).unwrap().unwrap().last_slot, Some(600));
        assert_eq!(s.watch_seen(id).unwrap().as_deref(), Some("#1 a"));
        // Its tab closed: the next slot checks.
        assert_eq!(
            s.claim_check(id, Some(600), 700, 700, &[4]).unwrap(),
            Some(true)
        );
    }

    /// Two Run now asks at the same moment, from two connections, make one run.
    #[test]
    fn two_run_nows_at_once_make_one_run() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("qoop.db");
        let id = Store::open(&path)
            .unwrap()
            .add_schedule(&made("a"))
            .unwrap();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(6));
        let asks: Vec<_> = (0..6)
            .map(|_| {
                let (path, gate) = (path.clone(), gate.clone());
                std::thread::spawn(move || {
                    let s = Store::open(&path).expect("open");
                    gate.wait();
                    s.run_now(id, 0, 1_000, &[]).expect("an answer")
                })
            })
            .collect();
        let started = asks
            .into_iter()
            .map(|t| t.join().expect("an ask"))
            .filter(|r| matches!(r, RunNow::Started(_)))
            .count();
        assert_eq!(started, 1);
    }

    #[test]
    fn a_run_now_after_a_scheduled_run_is_a_second_row_and_each_report_lands_on_its_own() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        let first = s.claim_run(id, None, 500, 501, "claimed").unwrap().unwrap();
        assert!(
            s.claim_run(id, None, 500, 502, "claimed")
                .unwrap()
                .is_none(),
            "the second caller lost and made no row"
        );
        assert!(
            s.report_schedule(id, 500, "started", Some(7), None, Some(first))
                .unwrap()
        );
        let now = s.start_run(id, 500, 600).unwrap().unwrap();
        assert!(s.start_run(77, 0, 600).unwrap().is_none());
        assert!(
            s.report_schedule(id, 500, "started", Some(8), None, Some(now))
                .unwrap()
        );
        let listed = runs(&s, id);
        assert_eq!(
            listed
                .iter()
                .map(|r| (r.id, r.started_at, r.result.as_str(), r.terminal))
                .collect::<Vec<_>>(),
            [
                (now, 600, "started", Some(8)),
                (first, 501, "started", Some(7))
            ],
            "newest first"
        );
        assert!(
            listed
                .iter()
                .all(|r| r.ended_at.is_none() && r.outcome.is_none())
        );
        // A skip is a row too, ended `skipped` as it starts.
        assert!(
            s.claim_schedule(id, Some(500), 560, 561, Some("skipped: too old"))
                .unwrap()
        );
        let skip = runs(&s, id)
            .into_iter()
            .find(|r| r.slot == 560)
            .expect("the skip's row");
        assert_eq!(
            (skip.result.as_str(), skip.ended_at, skip.outcome.as_deref()),
            ("skipped: too old", Some(561), Some("skipped"))
        );
        assert_eq!(s.open_runs().unwrap().len(), 2);
    }

    #[test]
    fn an_edit_keeps_the_id_and_the_runs_and_a_run_going_still_reports() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        s.add_schedule(&made("b")).unwrap();
        let run = s.claim_run(id, None, 500, 501, "claimed").unwrap().unwrap();
        let edited = ScheduleRow {
            id,
            cron: Some("0 10 * * *".into()),
            prompt: "a new prompt".into(),
            ..made("a2")
        };
        assert!(s.change_schedule(&edited, Some(700)).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(
            (
                held.name.as_str(),
                held.cron.as_deref(),
                held.prompt.as_str(),
                held.last_slot,
                held.last_result.as_deref()
            ),
            (
                "a2",
                Some("0 10 * * *"),
                "a new prompt",
                Some(700),
                Some("claimed")
            ),
            "a new timing starts from the edit"
        );
        // The run of the old slot reports after the edit: its row, and the schedule's last run,
        // since nothing started after it.
        assert!(
            s.report_schedule(id, 500, "started", Some(7), None, Some(run))
                .unwrap()
        );
        assert_eq!(runs(&s, id).len(), 1);
        assert_eq!(runs(&s, id)[0].terminal, Some(7));
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(
            (held.last_result.as_deref(), held.last_terminal),
            (Some("started"), Some(7))
        );
        // No new timing keeps the slot; a name another schedule has is refused.
        assert!(s.change_schedule(&edited, None).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().last_slot, Some(700));
        assert!(matches!(
            s.change_schedule(&ScheduleRow { id, ..made("b") }, None),
            Err(StoreError::Schedule(ScheduleError::NameTaken(_)))
        ));
        assert!(
            !s.change_schedule(
                &ScheduleRow {
                    id: 77,
                    ..made("c")
                },
                None
            )
            .unwrap()
        );
        // A watch's new command starts over with nothing seen; the same command keeps it.
        let watched = ScheduleRow {
            cron: None,
            watch: Some("gh issue list".into()),
            ..edited.clone()
        };
        assert!(s.change_schedule(&watched, Some(800)).unwrap());
        s.report_schedule(id, 800, "checked", None, Some("#1"), None)
            .unwrap();
        assert!(s.change_schedule(&watched, None).unwrap());
        assert_eq!(s.watch_seen(id).unwrap().as_deref(), Some("#1"));
        let other = ScheduleRow {
            watch: Some("gh pr list".into()),
            ..watched
        };
        assert!(s.change_schedule(&other, None).unwrap());
        assert_eq!(s.watch_seen(id).unwrap(), None);
        assert!(s.change_schedule(&edited, None).unwrap());
        assert_eq!(s.schedule(id).unwrap().unwrap().watch, None);
    }

    #[test]
    fn both_fail_paths_mark_the_run_and_a_started_run_is_left_alone() {
        let s = Store::in_memory().expect("open");
        let a = s.add_schedule(&made("a")).unwrap();
        let b = s.add_schedule(&made("b")).unwrap();
        let run = s.claim_run(a, None, 500, 501, "claimed").unwrap().unwrap();
        assert!(
            s.fail_claimed_schedule(a, 500, Some(run), "exited with 1")
                .unwrap()
        );
        let failed = &runs(&s, a)[0];
        assert_eq!(
            (failed.result.as_str(), failed.outcome.as_deref()),
            ("failed: exited with 1", Some("failed"))
        );
        assert!(failed.ended_at.is_some());
        // A run asked for now, lost when its daemon went.
        let lost = s.start_run(b, 0, 600).unwrap().unwrap();
        assert_eq!(s.fail_stale_claims().unwrap(), 0, "no schedule was claimed");
        let row = &runs(&s, b)[0];
        assert_eq!(
            (row.id, row.result.as_str(), row.outcome.as_deref()),
            (lost, "failed: lost at restart", Some("failed"))
        );
        // A run that reported is not failed after.
        let started = s.start_run(b, 0, 700).unwrap().unwrap();
        s.report_schedule(b, 0, "started", Some(3), None, Some(started))
            .unwrap();
        assert!(!s.fail_run(started, "x").unwrap());
        assert_eq!(runs(&s, b)[0].result, "started");
    }

    #[test]
    fn a_watch_that_started_an_agent_gets_a_row_and_a_check_does_not() {
        let s = Store::in_memory().expect("open");
        let row = ScheduleRow {
            cron: None,
            watch: Some("gh issue list".into()),
            ..made("issues")
        };
        let id = s.add_schedule(&row).unwrap();
        assert!(s.claim_schedule(id, None, 500, 500, None).unwrap());
        s.report_schedule(id, 500, "checked", None, Some("#1"), None)
            .unwrap();
        assert!(runs(&s, id).is_empty(), "a check is no run");
        assert!(s.claim_schedule(id, Some(500), 800, 800, None).unwrap());
        assert!(
            s.report_schedule(id, 800, "started", Some(4), Some("#1\n#2"), None)
                .unwrap()
        );
        let listed = runs(&s, id);
        assert_eq!(listed.len(), 1);
        assert_eq!(
            (
                listed[0].slot,
                listed[0].result.as_str(),
                listed[0].terminal
            ),
            (800, "started", Some(4))
        );
        // A schedule that is not a watch makes no row from a report without one.
        let plain = s.add_schedule(&made("plain")).unwrap();
        s.report_schedule(plain, 0, "started", Some(5), None, None)
            .unwrap();
        assert!(runs(&s, plain).is_empty());
    }

    #[test]
    fn a_run_ends_once_with_the_task_its_tab_opened() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        let run = s.claim_run(id, None, 500, 501, "claimed").unwrap().unwrap();
        assert!(s.open_runs().unwrap().is_empty(), "not started yet");
        s.report_schedule(id, 500, "started", Some(7), None, Some(run))
            .unwrap();
        let task_at = |opened: i64, closed: Option<i64>| {
            s.conn
                .execute(
                    "INSERT INTO task (terminal, goal, stage, opened_at, updated_at, closed_at) \
                     VALUES (7, 'g', 'implementing', ?1, ?1, ?2)",
                    params![opened, closed],
                )
                .unwrap();
            s.conn.last_insert_rowid()
        };
        task_at(400, Some(450));
        let task = task_at(550, None);
        assert_eq!(s.open_runs().unwrap()[0].id, run);
        assert!(s.end_run(run, 900, "done", Some("all green")).unwrap());
        assert!(!s.end_run(run, 950, "gone", None).unwrap(), "once");
        let ended = &runs(&s, id)[0];
        assert_eq!(
            (
                ended.ended_at,
                ended.outcome.as_deref(),
                ended.summary.as_deref(),
                ended.task
            ),
            (Some(900), Some("done"), Some("all green"), Some(task))
        );
        assert!(s.open_runs().unwrap().is_empty());
    }

    #[test]
    fn runs_page_by_their_start_and_go_with_their_schedule() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        for at in [100, 200, 300, 400, 500] {
            s.start_run(id, 0, at).unwrap();
        }
        let page = |before, limit| -> Vec<i64> {
            s.schedule_runs(id, before, limit)
                .unwrap()
                .iter()
                .map(|r| r.started_at)
                .collect()
        };
        assert_eq!(page(None, 2), [500, 400]);
        assert_eq!(page(Some(400), 2), [300, 200]);
        assert_eq!(page(Some(200), 2), [100]);
        assert!(page(Some(100), 2).is_empty());
        // The newest are kept.
        for at in 0..RUNS_KEPT {
            s.start_run(id, 0, 1000 + at).unwrap();
        }
        let kept = s.schedule_runs(id, None, 1000).unwrap();
        assert_eq!(kept.len() as i64, RUNS_KEPT);
        assert_eq!(kept.last().unwrap().started_at, 1000);
        assert!(s.remove_schedule(id).unwrap());
        let left: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM schedule_run", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "the runs go with their schedule");
    }
}

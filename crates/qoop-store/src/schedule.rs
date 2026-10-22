//! Schedules: an agent the owner asked qoop to start at a time, once or on a repeat.
//!
//! A row is the whole job: when (`cron` in local time, or `at`, one moment), what to start
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

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// The longest name a schedule has, in characters.
pub const NAME_MOST: usize = 40;

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
}

const COLUMNS: &str = "id, name, cron, at, agent, cwd, space, prompt, paused, created_at, \
                       last_slot, last_at, last_result, last_terminal";

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
    /// Record a schedule and answer its id; `row.id` and the `last_*` columns are ignored.
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
        tx.commit()?;
        Ok(id)
    }

    /// Every schedule, oldest first.
    pub fn schedules(&self) -> Result<Vec<ScheduleRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM schedule ORDER BY id"))?;
        Ok(stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?)
    }

    /// One schedule by its id.
    pub fn schedule(&self, id: i64) -> Result<Option<ScheduleRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM schedule WHERE id = ?1"),
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
    pub fn claim_schedule(
        &self,
        id: i64,
        expected: Option<i64>,
        slot: i64,
        now: i64,
        result: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE schedule SET last_slot = ?3, last_at = ?4, last_result = ?5, \
             last_terminal = NULL \
             WHERE id = ?1 AND last_slot IS ?2 AND paused = 0",
            params![id, expected, slot, now, result],
        )? > 0)
    }

    /// How the run of `slot` went, applied only while the row still holds that slot. A row
    /// that never fired holds slot 0, which is what a run asked for by hand carries. Answers
    /// whether it was applied.
    pub fn report_schedule(
        &self,
        id: i64,
        slot: i64,
        result: &str,
        terminal: Option<i64>,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            // `last_at` too: a run the owner asked for now claimed nothing, so the time it
            // shows would otherwise be the last scheduled one's.
            "UPDATE schedule SET last_result = ?3, last_terminal = ?4, \
             last_at = CAST(strftime('%s', 'now') AS INTEGER) \
             WHERE id = ?1 AND COALESCE(last_slot, 0) = ?2",
            params![id, slot, result, terminal],
        )? > 0)
    }

    /// Mark the run of `slot` failed with `why`, only while it still reads `claimed`: the
    /// child that should have reported ended without doing so.
    pub fn fail_claimed_schedule(&self, id: i64, slot: i64, why: &str) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE schedule SET last_result = ?3 \
             WHERE id = ?1 AND COALESCE(last_slot, 0) = ?2 AND last_result = 'claimed'",
            params![id, slot, format!("failed: {why}")],
        )? > 0)
    }

    /// Runs still `claimed` when a daemon starts belong to a daemon that is gone, so their
    /// child never reported: they become `failed: lost at restart`. Answers how many.
    pub fn fail_stale_claims(&self) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            "UPDATE schedule SET last_result = 'failed: lost at restart' \
             WHERE last_result = 'claimed'",
            [],
        )?)
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

    /// Two claims of one slot from the same last slot: one changes the row, the other none.
    #[test]
    fn claiming_a_slot_twice_changes_one_row_then_zero() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(s.claim_schedule(id, None, 500, 501, "claimed").unwrap());
        assert!(
            !s.claim_schedule(id, None, 500, 502, "claimed").unwrap(),
            "the second caller lost"
        );
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(held.last_slot, Some(500));
        assert_eq!(held.last_at, Some(501));
        assert_eq!(held.last_result.as_deref(), Some("claimed"));
        assert_eq!(held.last_terminal, None);
        // The next slot is claimed from the one just taken.
        assert!(
            s.claim_schedule(id, Some(500), 560, 561, "skipped: too old")
                .unwrap()
        );
        assert!(
            !s.claim_schedule(id, Some(500), 560, 561, "claimed")
                .unwrap()
        );
    }

    #[test]
    fn a_paused_row_is_not_claimed_and_resume_banks_nothing() {
        let s = Store::in_memory().expect("open");
        let id = s.add_schedule(&made("a")).unwrap();
        assert!(s.pause_schedule(id).unwrap());
        assert!(!s.claim_schedule(id, None, 500, 501, "claimed").unwrap());
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
        assert!(s.claim_schedule(id, None, 500, 501, "claimed").unwrap());
        assert!(s.report_schedule(id, 500, "started", Some(7)).unwrap());
        let held = s.schedule(id).unwrap().unwrap();
        assert_eq!(held.last_result.as_deref(), Some("started"));
        assert_eq!(held.last_terminal, Some(7));
        assert!(
            !s.report_schedule(id, 440, "failed: late", None).unwrap(),
            "a report for an older slot is dropped"
        );
        assert_eq!(
            s.schedule(id).unwrap().unwrap().last_result.as_deref(),
            Some("started")
        );
        // A row that never fired holds slot 0: what a run by hand reports against.
        let fresh = s.add_schedule(&made("b")).unwrap();
        assert!(s.report_schedule(fresh, 0, "started", Some(9)).unwrap());
    }

    #[test]
    fn a_claim_left_over_from_a_dead_daemon_becomes_failed() {
        let s = Store::in_memory().expect("open");
        let a = s.add_schedule(&made("a")).unwrap();
        let b = s.add_schedule(&made("b")).unwrap();
        s.claim_schedule(a, None, 500, 501, "claimed").unwrap();
        s.claim_schedule(b, None, 500, 501, "claimed").unwrap();
        s.report_schedule(b, 500, "started", Some(3)).unwrap();
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
        s.claim_schedule(a, Some(500), 560, 561, "claimed").unwrap();
        assert!(!s.fail_claimed_schedule(a, 500, "x").unwrap());
        assert!(s.fail_claimed_schedule(a, 560, "exited with 1").unwrap());
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
}

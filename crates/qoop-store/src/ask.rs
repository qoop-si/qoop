//! Asks: a question qass puts to the owner, kept until it is answered.
//!
//! A card is written by qass once (`qoop ask add`), numbered `E<n>` by `escalate.sh next`, and
//! kept here so the pane, the phone and the desk all read one record. The card's text is a JSON
//! `body` this crate never looks into: the daemon and the clients own its shape.
//!
//! A card is tied to its agent by the agent's session id, not by the terminal alone: terminal
//! ids are reused, so every path that ends a terminal closes its live cards in the same
//! transaction (`close_for_terminals`, called from `task::abandon_open_tasks`).
//!
//! Status words: `open` (waiting for him), `passed` (his words were typed to qass, which
//! relays them), `answered` (his words went to the worker), `closed`. `closed_at` is when the
//! card last changed status: when it was passed, answered or closed.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{Store, StoreError};

/// How long an ended card is kept.
pub const KEEP_SECS: i64 = 14 * 24 * 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskRow {
    pub id: i64,
    pub card: String,
    pub agent: Option<String>,
    pub terminal: Option<i64>,
    pub session: Option<String>,
    pub task: Option<i64>,
    pub status: String,
    pub note: Option<String>,
    pub body: String,
    pub answer: Option<String>,
    pub option: Option<String>,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
}

/// What an `add` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// A new card.
    New,
    /// The same card again (a retry): nothing changed.
    Same,
    /// An open card with this number was rewritten.
    Replaced,
}

/// What a write to an ask may be refused for.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error(
        "{0} is already {1}: a number is never used twice, take a new one with escalate.sh next E"
    )]
    Ended(String, String),
    #[error("no card {0}")]
    NoCard(String),
    #[error("{0} is already {1}")]
    NotOpen(String, String),
}

const COLUMNS: &str = "id, card, agent, terminal, session, task, status, note, body, answer, \
                       option, opened_at, closed_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AskRow> {
    Ok(AskRow {
        id: r.get(0)?,
        card: r.get(1)?,
        agent: r.get(2)?,
        terminal: r.get(3)?,
        session: r.get(4)?,
        task: r.get(5)?,
        status: r.get(6)?,
        note: r.get(7)?,
        body: r.get(8)?,
        answer: r.get(9)?,
        option: r.get(10)?,
        opened_at: r.get(11)?,
        closed_at: r.get(12)?,
    })
}

fn find(conn: &Connection, card: &str) -> rusqlite::Result<Option<AskRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM ask WHERE card = ?1"),
        params![card],
        row,
    )
    .optional()
}

/// Close every live card of these terminals, as its agent closed. Called inside the
/// transaction that ends the terminals, so a reused terminal id never finds an old card.
pub(crate) fn close_for_terminals(
    tx: &Transaction<'_>,
    terminals: &[i64],
    now: i64,
) -> rusqlite::Result<()> {
    for terminal in terminals {
        tx.execute(
            "UPDATE ask SET status = 'closed', note = 'agent closed', closed_at = ?2 \
             WHERE terminal = ?1 AND status IN ('open', 'passed')",
            params![terminal, now],
        )?;
    }
    Ok(())
}

impl Store {
    /// Write a card. `body` is the card's JSON. An open card with the number is rewritten
    /// (qass fixing a typo), an identical one is left alone (a retry), an ended one is
    /// refused. Every other live card of the same agent is closed as superseded, but only
    /// when the card names an agent.
    pub fn add_ask(
        &self,
        card: &str,
        agent: Option<&str>,
        terminal: Option<i64>,
        session: Option<&str>,
        body: &str,
        now: i64,
    ) -> Result<Added, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if let Some(old) = find(&tx, card)? {
            if old.status != "open" {
                return Err(AskError::Ended(card.into(), old.status).into());
            }
            if old.body == body && old.agent.as_deref() == agent {
                return Ok(Added::Same);
            }
            tx.execute(
                "UPDATE ask SET agent = ?2, terminal = ?3, session = COALESCE(session, ?4), body = ?5 \
                 WHERE id = ?1",
                params![old.id, agent, terminal, session, body],
            )?;
            tx.commit()?;
            return Ok(Added::Replaced);
        }
        if let Some(agent) = agent {
            tx.execute(
                "UPDATE ask SET status = 'closed', note = 'superseded', closed_at = ?2 \
                 WHERE agent = ?1 AND status IN ('open', 'passed')",
                params![agent, now],
            )?;
        }
        let task: Option<i64> = match terminal {
            Some(t) => tx
                .query_row(
                    "SELECT id FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                    params![t],
                    |r| r.get(0),
                )
                .optional()?,
            None => None,
        };
        tx.execute(
            "INSERT INTO ask (card, agent, terminal, session, task, status, body, opened_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
            params![card, agent, terminal, session, task, body, now],
        )?;
        tx.commit()?;
        Ok(Added::New)
    }

    /// A draft the daemon files for an agent that waits on the owner: card `W<n>`, `n` the
    /// next row id, so a number is never reused or refused. None when a live card already names
    /// the terminal: qass's card, or an earlier draft, stands.
    pub fn add_draft(
        &self,
        agent: &str,
        terminal: i64,
        session: Option<&str>,
        body: &str,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE terminal = ?1 AND status IN ('open', 'passed'))",
            params![terminal],
            |r| r.get(0),
        )?;
        if live {
            return Ok(None);
        }
        let Some(task): Option<i64> = tx
            .query_row(
                "SELECT id FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        // One draft a waiting spell: a card opened since it last began waiting (answered,
        // dismissed or closed) stands for this spell, so a stage nobody moved never re-asks.
        let asked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE task = ?1 AND opened_at >= COALESCE( \
             (SELECT MAX(at) FROM task_event WHERE task = ?1 AND kind = 'stage' \
              AND body = 'waiting-human'), 0))",
            params![task],
            |r| r.get(0),
        )?;
        if asked {
            return Ok(None);
        }
        let n: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ask", [], |r| r.get(0))?;
        let card = format!("W{n}");
        tx.execute(
            "INSERT INTO ask (card, agent, terminal, session, task, status, body, opened_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
            params![card, agent, terminal, session, task, body, now],
        )?;
        tx.commit()?;
        Ok(Some(card))
    }

    /// One card by its number.
    pub fn ask(&self, card: &str) -> Result<Option<AskRow>, StoreError> {
        Ok(find(&self.conn, card)?)
    }

    /// Take an open card for an answer: `open` to `to` (`answered` or `passed`), once. The
    /// second caller is told it is no longer open. Answers the card as it was.
    pub fn claim_ask(
        &self,
        card: &str,
        to: &str,
        option: Option<&str>,
        answer: &str,
        note: &str,
        now: i64,
    ) -> Result<AskRow, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let old = find(&tx, card)?.ok_or_else(|| AskError::NoCard(card.into()))?;
        if old.status != "open" {
            return Err(AskError::NotOpen(card.into(), old.status).into());
        }
        tx.execute(
            "UPDATE ask SET status = ?2, answer = ?3, option = ?4, note = ?5, closed_at = ?6 \
             WHERE id = ?1",
            params![old.id, to, answer, option, note, now],
        )?;
        tx.commit()?;
        Ok(old)
    }

    /// Give a card back to `open`, after its answer could not be sent.
    pub fn reopen_ask(&self, card: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE ask SET status = 'open', answer = NULL, option = NULL, note = NULL, \
             closed_at = NULL WHERE card = ?1 AND status IN ('answered', 'passed')",
            params![card],
        )?;
        Ok(())
    }

    /// End a live card (`open` or `passed`) with a note. False when it was already ended,
    /// which is not an error: a late `done` for a card he already dismissed is harmless.
    pub fn close_ask(&self, card: &str, note: &str, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if find(&tx, card)?.is_none() {
            return Err(AskError::NoCard(card.into()).into());
        }
        let n = tx.execute(
            "UPDATE ask SET status = 'closed', note = ?2, closed_at = ?3 \
             WHERE card = ?1 AND status IN ('open', 'passed')",
            params![card, note, now],
        )?;
        tx.commit()?;
        Ok(n == 1)
    }

    /// The cards still waiting (`open`) or being relayed (`passed`), oldest first.
    pub fn live_asks(&self) -> Result<Vec<AskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM ask WHERE status IN ('open', 'passed') ORDER BY opened_at, id"
        ))?;
        Ok(stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?)
    }

    /// The newest ended cards, newest first, at most `limit`.
    pub fn ended_asks(&self, limit: usize) -> Result<Vec<AskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM ask WHERE status IN ('answered', 'closed') \
             ORDER BY closed_at DESC, id DESC LIMIT ?1"
        ))?;
        Ok(stmt
            .query_map(params![limit as i64], row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Whether a terminal has a live draft (`W<n>`), the daemon's card for its agent.
    pub fn has_draft(&self, terminal: i64) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE terminal = ?1 AND card LIKE 'W%' \
             AND status IN ('open', 'passed'))",
            params![terminal],
            |r| r.get(0),
        )?)
    }

    /// At daemon start: cards of a terminal that is no longer open end as `agent closed`,
    /// and ended cards past their keep are dropped.
    pub fn tidy_asks(&self, now: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE ask SET status = 'closed', note = 'agent closed', closed_at = ?1 \
             WHERE status IN ('open', 'passed') AND terminal IS NOT NULL AND terminal NOT IN \
             (SELECT id FROM terminal WHERE closed_at IS NULL)",
            params![now],
        )?;
        self.conn.execute(
            "DELETE FROM ask WHERE status IN ('answered', 'closed') AND closed_at < ?1",
            params![now - KEEP_SECS],
        )?;
        Ok(())
    }
}

/// End a terminal's live drafts: its agent no longer waits on the owner (its stage moved, it
/// took an answer, it claimed or ended its task). qass's own cards are left to qass.
pub(crate) fn close_drafts(
    tx: &Connection,
    terminal: i64,
    note: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE ask SET status = 'closed', note = ?2, closed_at = ?3 \
         WHERE terminal = ?1 AND card LIKE 'W%' AND status IN ('open', 'passed')",
        params![terminal, note, now],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_terminal() -> (Store, i64) {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("w", 1).unwrap();
        let t = s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap();
        (s, t)
    }

    /// A draft is made for an agent waiting on him: never over a live card, once a waiting
    /// spell, a new number for a new spell; it closes when the agent stops waiting (its stage
    /// moved, it took an answer, it claimed or ended its task), and qass's own card stays.
    #[test]
    fn a_draft_is_filed_once_a_wait_and_closes_when_the_wait_ends() {
        let (s, t) = store_with_terminal();
        assert_eq!(
            s.add_draft("T5", t, None, "{d}", 9).unwrap(),
            None,
            "no task, no draft"
        );
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        let first = s.add_draft("T5", t, Some("sess"), "{d}", 12).unwrap();
        let card = first.clone().expect("a draft");
        assert!(card.starts_with('W'));
        assert!(s.has_draft(t).unwrap());
        assert_eq!(
            s.add_draft("T5", t, None, "{d}", 13).unwrap(),
            None,
            "a live card stands"
        );
        // His answer in its tab ends the wait and the draft; the same spell never asks again.
        s.answer_waiting(t, 14).unwrap();
        assert_eq!(
            s.ask(&card).unwrap().unwrap().note.as_deref(),
            Some("answered in its tab")
        );
        s.set_task_stage(task, "waiting-human", 15).unwrap();
        let second = s
            .add_draft("T5", t, None, "{d}", 15)
            .unwrap()
            .expect("a new spell");
        assert_ne!(second, card);
        s.claim_ask(&second, "answered", None, "go", "owner, inbox", 16)
            .unwrap();
        assert_eq!(
            s.add_draft("T5", t, None, "{d}", 17).unwrap(),
            None,
            "answered this spell"
        );
        // Each way out of a wait closes a live draft.
        for (n, end) in ["stage", "done", "close"].into_iter().enumerate() {
            let at = 20 + 10 * n as i64;
            let task = match end {
                "close" => s.open_task(t, "ship", true, at).unwrap(),
                _ => task,
            };
            s.set_task_stage(task, "waiting-human", at).unwrap();
            let card = s
                .add_draft("T5", t, None, "{d}", at + 1)
                .unwrap()
                .expect(end);
            match end {
                "stage" => drop(s.set_task_stage(task, "implementing", at + 2).unwrap()),
                "done" => s.claim_task_done(task, "shipped", at + 2).unwrap(),
                _ => drop(
                    s.close_task(task, crate::Outcome::Verified, "ok", at + 2)
                        .unwrap(),
                ),
            }
            assert_eq!(s.ask(&card).unwrap().unwrap().status, "closed", "{end}");
        }
        // qass's card is never closed as a draft.
        s.add_ask("E9", Some("T5"), Some(t), None, "{e}", 60)
            .unwrap();
        let task = s.open_task(t, "more", true, 61).unwrap();
        s.set_task_stage(task, "implementing", 62).unwrap();
        assert_eq!(s.ask("E9").unwrap().unwrap().status, "open");
    }

    #[test]
    fn a_card_is_added_once_rewritten_when_it_changes_and_never_reused_when_ended() {
        let (s, t) = store_with_terminal();
        assert_eq!(
            s.add_ask("E7", Some("T5"), Some(t), Some("sess"), "{a}", 10)
                .unwrap(),
            Added::New
        );
        assert_eq!(
            s.add_ask("E7", Some("T5"), Some(t), Some("sess"), "{a}", 11)
                .unwrap(),
            Added::Same
        );
        assert_eq!(
            s.add_ask("E7", Some("T5"), Some(t), Some("sess"), "{b}", 12)
                .unwrap(),
            Added::Replaced
        );
        assert_eq!(s.ask("E7").unwrap().unwrap().body, "{b}");
        // A rewrite keeps the conversation the card was first about.
        assert_eq!(
            s.ask("E7").unwrap().unwrap().session.as_deref(),
            Some("sess")
        );
        s.add_ask("E7", Some("T5"), Some(t), Some("other"), "{c}", 13)
            .unwrap();
        assert_eq!(
            s.ask("E7").unwrap().unwrap().session.as_deref(),
            Some("sess")
        );
        assert!(s.close_ask("E7", "chat", 20).unwrap());
        let err = s
            .add_ask("E7", Some("T5"), Some(t), None, "{b}", 21)
            .unwrap_err();
        assert!(err.to_string().contains("never used twice"), "{err}");
    }

    #[test]
    fn a_newer_card_of_the_same_agent_supersedes_and_a_card_about_nothing_never_does() {
        let (s, t) = store_with_terminal();
        s.add_ask("E1", Some("T5"), Some(t), None, "{}", 10)
            .unwrap();
        s.add_ask("E2", None, None, None, "{}", 11).unwrap();
        s.add_ask("E3", None, None, None, "{}", 12).unwrap();
        let live: Vec<String> = s.live_asks().unwrap().into_iter().map(|a| a.card).collect();
        assert_eq!(live, ["E1", "E2", "E3"], "cards about no terminal coexist");
        s.add_ask("E4", Some("T5"), Some(t), None, "{}", 13)
            .unwrap();
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref(), e1.closed_at),
            ("closed", Some("superseded"), Some(13))
        );
        assert_eq!(s.live_asks().unwrap().len(), 3);
    }

    #[test]
    fn an_answer_claims_the_card_once_and_can_be_given_back() {
        let (s, t) = store_with_terminal();
        s.add_ask("E1", Some("T5"), Some(t), None, "{}", 10)
            .unwrap();
        let old = s
            .claim_ask("E1", "answered", Some("b"), "wait", "owner, inbox", 20)
            .unwrap();
        assert_eq!(old.status, "open");
        let again = s
            .claim_ask("E1", "answered", Some("a"), "go", "owner, inbox", 21)
            .unwrap_err();
        assert!(again.to_string().contains("already answered"), "{again}");
        assert!(s.live_asks().unwrap().is_empty());
        s.reopen_ask("E1").unwrap();
        let back = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (back.status.as_str(), back.answer, back.closed_at),
            ("open", None, None)
        );
    }

    #[test]
    fn a_passed_card_stays_live_until_it_is_closed() {
        let (s, t) = store_with_terminal();
        s.add_ask("E1", Some("T5"), Some(t), None, "{}", 10)
            .unwrap();
        s.claim_ask("E1", "passed", None, "go", "passed to qass", 20)
            .unwrap();
        let passed = s.live_asks().unwrap().remove(0);
        assert_eq!(
            (passed.status.as_str(), passed.closed_at),
            ("passed", Some(20))
        );
        assert!(s.ended_asks(5).unwrap().is_empty());
        assert!(s.close_ask("E1", "qass answered", 30).unwrap());
        assert!(
            !s.close_ask("E1", "again", 31).unwrap(),
            "a late done is harmless"
        );
        assert!(
            s.close_ask("E9", "x", 32).is_err(),
            "an unknown card is refused"
        );
        assert_eq!(s.ended_asks(5).unwrap().len(), 1);
    }

    #[test]
    fn ending_a_terminal_closes_its_live_cards_and_tidy_drops_old_ones() {
        let (s, t) = store_with_terminal();
        s.add_ask("E1", Some("T5"), Some(t), None, "{}", 10)
            .unwrap();
        s.add_ask("E2", Some("ubox/T2"), None, None, "{}", 11)
            .unwrap();
        assert!(s.close_terminal(t, 50).unwrap());
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref()),
            ("closed", Some("agent closed"))
        );
        assert_eq!(
            s.ask("E2").unwrap().unwrap().status,
            "open",
            "a remote card stays"
        );
        // A card whose terminal is not open is closed at start, and old ended ones go.
        s.add_ask("E3", Some("T9"), Some(9), None, "{}", 60)
            .unwrap();
        s.tidy_asks(70).unwrap();
        assert_eq!(s.ask("E3").unwrap().unwrap().status, "closed");
        s.tidy_asks(50 + KEEP_SECS + 1).unwrap();
        assert!(s.ask("E1").unwrap().is_none());
        assert!(
            s.ask("E2").unwrap().is_some(),
            "a live card is never pruned"
        );
    }
}

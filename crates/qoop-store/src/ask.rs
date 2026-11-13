//! Asks: a question qass puts to the owner, kept until it is answered.
//!
//! A card is written by qass once (`qoop ask add`), numbered `E<n>` by `escalate.sh new`, and
//! kept here so the pane, the phone and the desk all read one record. The card's text is a JSON
//! `body` this crate never looks into: the daemon and the clients own its shape.
//!
//! A card is tied to its agent by the agent's session id, not by the terminal alone: terminal
//! ids are reused, so every path that ends a terminal unties its live cards in the same
//! transaction (`close_for_terminals`, called from `task::abandon_open_tasks`, and at start).
//!
//! Status words: `open` (waiting for him), `queued` (his answer is kept and will be typed as
//! soon as its target takes input), `passed` (his words were typed to qass, which relays
//! them), `answered` (his words went to the worker), `closed`. `closed_at` is when the card
//! last changed status: when it was queued, passed, answered or closed.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Store, StoreError};

/// The note of a draft that waits for qass on a plan, not on the agent.
pub const PLAN_NOTE: &str = "plan waits for qass";

/// The note of a waiting draft closed because its agent worked again.
const WORKS_AGAIN: &str = "it works again";

/// How long an ended card is kept.
pub const KEEP_SECS: i64 = 14 * 24 * 3600;

/// The note of a card he left unanswered ([`Store::leave_asks`]), or whose tab or agent ended
/// while it waited: its agent's conversation is told of it once, at his next prompt.
pub const LEFT: &str = "left";

/// The note of a left card once its conversation was told of it ([`Store::raise_left`]).
pub const LEFT_RAISED: &str = "left, raised";

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

/// Said in every refusal of a card number that is not free, so a caller can retry with the next.
pub const NOT_FREE: &str = "a number is never used twice";

/// What a write to an ask may be refused for.
#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("{0} is already {1}: {NOT_FREE}, take a new one with escalate.sh new")]
    Ended(String, String),
    #[error(
        "{0} is taken by another question (its handle is {1}): {NOT_FREE}, take a new one with escalate.sh new"
    )]
    Taken(String, String),
    #[error("no card {0}")]
    NoCard(String),
    #[error("{0} is already {1}")]
    NotOpen(String, String),
    #[error(
        "{0} is still live for this agent (open, or his answer is on its way): one question at a time"
    )]
    Waiting(String),
    #[error("no open task here: open one with qoop task new")]
    NoTask,
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

/// The `handle` a card's JSON names: the store never reads more of it.
fn handle_of(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("handle")?.as_str().map(str::to_string)
}

fn find(conn: &Connection, card: &str) -> rusqlite::Result<Option<AskRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM ask WHERE card = ?1"),
        params![card],
        row,
    )
    .optional()
}

/// The note on an answer of his still queued for a tab that ended.
pub const TAB_CLOSED: &str = "its tab closed: qass gets the answer";

/// The start of a `CASE` that notes [`LEFT`] on a card about the agent's own question, still
/// open, that is not a plan's: what a closing tab or a leaving agent leaves for its
/// conversation to raise when it comes back. Ends with the other note and `END`.
pub(crate) fn left_or() -> String {
    format!(
        "CASE WHEN status = 'open' AND task IS NOT NULL AND (card LIKE 'Q%' OR card LIKE 'W%') \
         AND COALESCE(note, '') != '{PLAN_NOTE}' THEN '{LEFT}' ELSE"
    )
}

/// End these terminals' cards. An open card closes, a draft or qass's: the agent it asks about
/// is gone, and a question left in his inbox for it is stale (the agent's own question is
/// noted [`LEFT`], so a resumed conversation is told of it); so does a passed card: his words
/// already reached qass. A queued answer stays live with terminal and agent cleared, so a
/// reused terminal id or agent label never matches it and his answer goes to qass. Called
/// inside the transaction that ends the terminals, and at start for those that ended unseen.
pub(crate) fn close_for_terminals(
    tx: &Connection,
    terminals: &[i64],
    now: i64,
) -> rusqlite::Result<()> {
    for terminal in terminals {
        tx.execute(
            &format!(
                "UPDATE ask SET status = 'closed', note = {} 'agent closed' END, \
                 closed_at = ?2 WHERE terminal = ?1 AND status IN ('open', 'passed')",
                left_or()
            ),
            params![terminal, now],
        )?;
        tx.execute(
            "UPDATE ask SET terminal = NULL, agent = NULL, note = ?2 \
             WHERE terminal = ?1 AND status IN ('open', 'queued')",
            params![terminal, TAB_CLOSED],
        )?;
    }
    Ok(())
}

impl Store {
    /// Write a card. `body` is the card's JSON. An open card with the number is rewritten
    /// (qass fixing a typo), an identical one is left alone (a retry), an ended one or one
    /// about another handle (two questions given one number) is refused. Every other open card
    /// of the same agent is closed as superseded, but only when the card names an agent: an
    /// answer he already gave is never dropped, nor the agent's own question (`Q<n>`).
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
            // A card about nobody stays so: one whose tab closed is never tied to whoever has
            // its terminal id now.
            let (agent, terminal) = if old.agent.is_none() {
                (None, None)
            } else {
                (agent, terminal)
            };
            if old.body == body && old.agent.as_deref() == agent {
                return Ok(Added::Same);
            }
            let (was, now_is) = (handle_of(&old.body), handle_of(body));
            if was != now_is {
                return Err(AskError::Taken(card.into(), was.unwrap_or_default()).into());
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
                 WHERE agent = ?1 AND status = 'open' AND card NOT LIKE 'Q%'",
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
        self.file_draft(agent, terminal, session, body, None, now)
    }

    /// [`Store::add_draft`] for a plan that waits for qass: the card is marked [`PLAN_NOTE`], so
    /// an agent that goes on working does not end it ([`Store::close_waiting_drafts`]); the plan
    /// is what waits, not the agent.
    pub fn add_plan_draft(
        &self,
        agent: &str,
        terminal: i64,
        session: Option<&str>,
        body: &str,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        self.file_draft(agent, terminal, session, body, Some(PLAN_NOTE), now)
    }

    fn file_draft(
        &self,
        agent: &str,
        terminal: i64,
        session: Option<&str>,
        body: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE terminal = ?1 AND status IN ('open', 'queued', 'passed'))",
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
        // One that closed because its agent worked again does not: it asks again when it waits;
        // nor one he left unanswered, which the agent was told of and asks again.
        let asked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE task = ?1 AND COALESCE(note, '') != ?2 \
             AND COALESCE(note, '') NOT LIKE 'left%' AND opened_at >= COALESCE( \
             (SELECT MAX(at) FROM task_event WHERE task = ?1 AND kind = 'stage' \
              AND body = 'waiting-human'), 0))",
            params![task, WORKS_AGAIN],
            |r| r.get(0),
        )?;
        if asked {
            return Ok(None);
        }
        let n: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ask", [], |r| r.get(0))?;
        let card = format!("W{n}");
        tx.execute(
            "INSERT INTO ask (card, agent, terminal, session, task, status, note, body, opened_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8)",
            params![card, agent, terminal, session, task, note, body, now],
        )?;
        tx.commit()?;
        Ok(Some(card))
    }

    /// A draft for a question dialog still up on an agent's screen, the spell that began at
    /// `since`: card `W<n>` like [`Store::add_draft`], with or without an open task. None when
    /// a live card names the terminal, or a card for it was opened since the spell began.
    ///
    /// Its `task` is left empty, task or not: that is what marks a dialog draft, so a tool or a
    /// prompt closes only these ([`Store::close_dialog_drafts`]). The note cannot mark it, since
    /// an answer rewrites the note.
    pub fn add_dialog_draft(
        &self,
        agent: &str,
        terminal: i64,
        session: Option<&str>,
        body: &str,
        since: i64,
        now: i64,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let asked: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE terminal = ?1 \
             AND (status IN ('open', 'queued', 'passed') OR opened_at > ?2))",
            params![terminal, since],
            |r| r.get(0),
        )?;
        if asked {
            return Ok(None);
        }
        let n: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ask", [], |r| r.get(0))?;
        let card = format!("W{n}");
        tx.execute(
            "INSERT INTO ask (card, agent, terminal, session, status, body, opened_at) \
             VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?6)",
            params![card, agent, terminal, session, body, now],
        )?;
        tx.commit()?;
        Ok(Some(card))
    }

    /// A card the daemon files about qass itself (no watcher came when it was told): card
    /// `W<n>` like a draft, with no agent, terminal or task, so no tab's prompt, dialog or end
    /// closes it and his answer goes to qass. None while a live card with the same handle
    /// stands: one is never filed twice.
    pub fn add_notice(&self, body: &str, now: i64) -> Result<Option<String>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let handle = handle_of(body);
        let live: Vec<String> = tx
            .prepare(
                "SELECT body FROM ask WHERE terminal IS NULL AND task IS NULL AND card LIKE 'W%' \
                 AND status IN ('open', 'queued', 'passed')",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if live.iter().any(|b| handle_of(b) == handle) {
            return Ok(None);
        }
        let n: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ask", [], |r| r.get(0))?;
        let card = format!("W{n}");
        tx.execute(
            "INSERT INTO ask (card, status, body, opened_at) VALUES (?1, 'open', ?2, ?3)",
            params![card, body, now],
        )?;
        tx.commit()?;
        Ok(Some(card))
    }

    /// End the open notice with `handle`: what it said is over. An answer he gave to it is
    /// left to go its way. Answers whether one ended.
    pub fn close_notice(&self, handle: &str, note: &str, now: i64) -> Result<bool, StoreError> {
        let open: Vec<(i64, String)> = self
            .conn
            .prepare("SELECT id, body FROM ask WHERE terminal IS NULL AND task IS NULL AND card LIKE 'W%' \
                 AND status = 'open'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut ended = false;
        for (id, body) in open {
            if handle_of(&body).as_deref() == Some(handle) {
                self.conn.execute(
                    "UPDATE ask SET status = 'closed', note = ?2, closed_at = ?3 WHERE id = ?1",
                    params![id, note, now],
                )?;
                ended = true;
            }
        }
        Ok(ended)
    }

    /// End a terminal's open waiting draft (a `W` card with a task) written before `since`: its
    /// agent worked at `since`, so the question it left is stale. A card written at or after
    /// that second is the turn end that followed and stands. A dialog draft has no task and
    /// closes on [`Store::close_dialog_drafts`]; a plan's card, an answer he already gave
    /// (`queued`, `passed`) and qass's cards stay. Answers whether one ended.
    pub fn close_waiting_drafts(&self, terminal: i64, since: i64) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE ask SET status = 'closed', note = ?3, closed_at = ?2 \
             WHERE terminal = ?1 AND card LIKE 'W%' AND task IS NOT NULL AND status = 'open' \
             AND opened_at < ?2 AND COALESCE(note, '') != ?4",
            params![terminal, since, WORKS_AGAIN, PLAN_NOTE],
        )? > 0)
    }

    /// End a terminal's live dialog draft: its agent reported a tool or a prompt, so the
    /// dialog is gone. A waiting draft and qass's cards stay. Answers whether one ended.
    pub fn close_dialog_drafts(&self, terminal: i64, now: i64) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE ask SET status = 'closed', note = 'the dialog ended', closed_at = ?2 \
             WHERE terminal = ?1 AND card LIKE 'W%' AND task IS NULL \
             AND status IN ('open', 'passed')",
            params![terminal, now],
        )? > 0)
    }

    /// A prompt reached the agent in `terminal` after his input in its tab at `input` (his
    /// answer, or new work): it moved past the question, so its open cards end, written before
    /// that input. An answer he gave that waits to be typed or passed on stays. Answers how
    /// many ended.
    pub fn prompt_taken(&self, terminal: i64, input: i64, now: i64) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            "UPDATE ask SET status = 'closed', note = 'it took a prompt in its tab', \
             closed_at = ?3 WHERE terminal = ?1 AND status = 'open' AND opened_at < ?2",
            params![terminal, input, now],
        )?)
    }

    /// A new card `card` replaces the older ones it names (`replaces:` in its block): one
    /// UPDATE ends those that are still `open` E cards, never `card` itself. A question he
    /// answered (`queued`, `passed`), an ended one, a draft and a number the inbox never had
    /// stay as they are. Answers the cards it ended, in the order they were named.
    pub fn supersede_asks(
        &self,
        card: &str,
        replaces: &[String],
        now: i64,
    ) -> Result<Vec<String>, StoreError> {
        if replaces.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; replaces.len()].join(", ");
        let note = format!("superseded by {card}");
        let args = [card, note.as_str()]
            .into_iter()
            .chain(replaces.iter().map(String::as_str));
        let mut stmt = self.conn.prepare(&format!(
            "UPDATE ask SET status = 'closed', note = ?2, closed_at = {now} \
             WHERE card IN ({marks}) AND status = 'open' AND card LIKE 'E%' AND card <> ?1 \
             RETURNING card"
        ))?;
        let ended: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(args), |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut named: Vec<String> = Vec::new();
        for c in replaces.iter().filter(|c| ended.contains(*c)) {
            if !named.contains(c) {
                named.push(c.clone());
            }
        }
        Ok(named)
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

    /// Give a card back to `open`, after its answer could not be sent, with the reason.
    pub fn reopen_ask(&self, card: &str, note: Option<&str>) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE ask SET status = 'open', answer = NULL, option = NULL, note = ?2, \
             closed_at = NULL WHERE card = ?1 AND status IN ('answered', 'queued', 'passed')",
            params![card, note],
        )?;
        Ok(())
    }

    /// A queued answer is about to be typed: `queued` to `to` (`answered` or `passed`), once.
    /// False when it is no longer queued (a chat answer or qass closed it): nothing is typed.
    pub fn deliver_ask(
        &self,
        card: &str,
        to: &str,
        note: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        let n = self.conn.execute(
            "UPDATE ask SET status = ?2, note = ?3, closed_at = ?4 \
             WHERE card = ?1 AND status = 'queued'",
            params![card, to, note, now],
        )?;
        Ok(n == 1)
    }

    /// The answers kept and not yet typed, oldest first.
    pub fn queued_asks(&self) -> Result<Vec<AskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM ask WHERE status = 'queued' ORDER BY closed_at, id"
        ))?;
        Ok(stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?)
    }

    /// End a live card (`open`, `queued` or `passed`) with a note. False when it was already ended,
    /// which is not an error: a late `done` for a card he already dismissed is harmless.
    pub fn close_ask(&self, card: &str, note: &str, now: i64) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        if find(&tx, card)?.is_none() {
            return Err(AskError::NoCard(card.into()).into());
        }
        let n = tx.execute(
            "UPDATE ask SET status = 'closed', note = ?2, closed_at = ?3 \
             WHERE card = ?1 AND status IN ('open', 'queued', 'passed')",
            params![card, note, now],
        )?;
        tx.commit()?;
        Ok(n == 1)
    }

    /// The cards still waiting (`open`), kept for delivery (`queued`) or being relayed
    /// (`passed`), oldest first.
    pub fn live_asks(&self) -> Result<Vec<AskRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM ask WHERE status IN ('open', 'queued', 'passed') ORDER BY opened_at, id"
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

    /// How many open cards each task has, by task id: what a board row of a task says.
    pub fn open_asks_by_task(&self) -> Result<std::collections::BTreeMap<i64, usize>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT task, COUNT(*) FROM ask WHERE status = 'open' AND task IS NOT NULL GROUP BY task",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as usize))
            })?
            .collect::<Result<_, _>>()?)
    }

    /// The terminals with an open card that waits on the owner. A plan draft waits on qass, and
    /// its agent goes on working (`add_plan_draft`): not one.
    pub fn open_ask_terminals(&self) -> Result<std::collections::BTreeSet<i64>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT terminal FROM ask WHERE status = 'open' AND terminal IS NOT NULL \
             AND COALESCE(note, '') != ?1",
        )?;
        Ok(stmt
            .query_map(params![PLAN_NOTE], |r| r.get::<_, i64>(0))?
            .collect::<Result<_, _>>()?)
    }

    /// Whether a terminal has a live draft (`W<n>`), the daemon's card for its agent.
    pub fn has_draft(&self, terminal: i64) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM ask WHERE terminal = ?1 AND card LIKE 'W%' \
             AND status IN ('open', 'queued', 'passed'))",
            params![terminal],
            |r| r.get(0),
        )?)
    }

    /// The agent's own question (`qoop task ask`): card `Q<n>`, `n` the next row id, tied to
    /// the terminal's open task and conversation. Refused with no open task, and while a live
    /// card names the terminal: one question at a time, and a draft or qass's card stands.
    pub fn add_agent_ask(
        &self,
        agent: &str,
        terminal: i64,
        session: Option<&str>,
        body: &str,
        now: i64,
    ) -> Result<String, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let live: Option<String> = tx
            .query_row(
                "SELECT card FROM ask WHERE terminal = ?1 \
                 AND status IN ('open', 'queued', 'passed') ORDER BY id LIMIT 1",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(card) = live {
            return Err(AskError::Waiting(card).into());
        }
        let task: i64 = tx
            .query_row(
                "SELECT id FROM task WHERE terminal = ?1 AND closed_at IS NULL",
                params![terminal],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(AskError::NoTask)?;
        let n: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ask", [], |r| r.get(0))?;
        let card = format!("Q{n}");
        tx.execute(
            "INSERT INTO ask (card, agent, terminal, session, task, status, body, opened_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
            params![card, agent, terminal, session, task, body, now],
        )?;
        tx.commit()?;
        Ok(card)
    }

    /// Set aside what he left unanswered: every `open` card (not a notice, not a plan's) opened
    /// `after` seconds ago or more ends noted [`LEFT`], kept as
    /// every ended card is. A task that waited on him with nothing live left on its terminal
    /// goes back to its stage before the wait ([`crate::task::stage_back`]), and no other card
    /// is touched. Answers how many cards were left.
    pub fn leave_asks(&self, now: i64, after: i64) -> Result<usize, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        // A notice is the daemon's `W` card about qass itself (no tab, no task): it is filed
        // again while what it says holds, so it never leaves. qass's card about nobody does.
        let due = "status = 'open' AND NOT (card LIKE 'W%' AND terminal IS NULL AND task IS NULL) \
                   AND COALESCE(note, '') != ?1 AND opened_at <= ?2";
        let tasks: Vec<i64> = tx
            .prepare(&format!(
                "SELECT DISTINCT task FROM ask WHERE {due} AND task IS NOT NULL"
            ))?
            .query_map(params![PLAN_NOTE, now - after], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let left = tx.execute(
            &format!("UPDATE ask SET status = 'closed', note = ?3, closed_at = ?4 WHERE {due}"),
            params![PLAN_NOTE, now - after, LEFT, now],
        )?;
        for task in tasks {
            let still: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM ask a JOIN task t ON t.id = ?1 \
                 WHERE (a.task = t.id OR a.terminal = t.terminal) \
                 AND a.status IN ('open', 'queued', 'passed'))",
                params![task],
                |r| r.get(0),
            )?;
            if !still {
                crate::task::stage_back(&tx, task, now)?;
            }
        }
        tx.commit()?;
        Ok(left)
    }

    /// The questions a conversation left, to tell it of them once: the agent's own (`W`, `Q`)
    /// noted [`LEFT`], asked in `session`, or, for a card that has no conversation, about
    /// `task`. They are noted [`LEFT_RAISED`] in the same transaction, so a second read finds
    /// none. Oldest first.
    pub fn raise_left(
        &self,
        session: Option<&str>,
        task: Option<i64>,
    ) -> Result<Vec<AskRow>, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let pick = "status = 'closed' AND note = ?3 AND task IS NOT NULL \
                    AND (card LIKE 'W%' OR card LIKE 'Q%') \
                    AND ((?1 IS NOT NULL AND session = ?1) OR (session IS NULL AND task = ?2))";
        let rows: Vec<AskRow> = tx
            .prepare(&format!(
                "SELECT {COLUMNS} FROM ask WHERE {pick} ORDER BY opened_at, id"
            ))?
            .query_map(params![session, task, LEFT], row)?
            .collect::<rusqlite::Result<_>>()?;
        if !rows.is_empty() {
            tx.execute(
                &format!("UPDATE ask SET note = ?4 WHERE {pick}"),
                params![session, task, LEFT, LEFT_RAISED],
            )?;
            tx.commit()?;
        }
        Ok(rows)
    }

    /// At daemon start: cards of a terminal that is no longer open are ended as its tab's end
    /// ends them (`close_for_terminals`), queued answers go back to `open`, and ended cards
    /// past their keep are dropped.
    pub fn tidy_asks(&self, now: i64) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let gone: Vec<i64> = tx
            .prepare(
                "SELECT DISTINCT terminal FROM ask WHERE status IN ('open', 'queued', 'passed') \
                 AND terminal IS NOT NULL AND terminal NOT IN \
                 (SELECT id FROM terminal WHERE closed_at IS NULL)",
            )?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        close_for_terminals(&tx, &gone, now)?;
        tx.commit()?;
        // An answer kept for delivery does not outlive the daemon that was to type it: it is
        // given back to him, with the reason, once the next start has checked its terminal.
        self.conn.execute(
            "UPDATE ask SET status = 'open', answer = NULL, option = NULL, \
             note = 'not delivered: qoop restarted', closed_at = NULL WHERE status = 'queued' \
             AND (terminal IS NULL OR terminal IN (SELECT id FROM terminal WHERE closed_at IS NULL))",
            [],
        )?;
        self.conn.execute(
            "DELETE FROM ask WHERE status IN ('answered', 'closed') AND closed_at < ?1",
            params![now - KEEP_SECS],
        )?;
        Ok(())
    }
}

/// End a terminal's live cards: its agent no longer waits on the owner (its stage moved, it
/// took a prompt, it claimed or ended its task), so a question about it left open is stale.
/// Drafts end whatever their state; qass's own cards only while open and written before now,
/// so one written in the same second as the move stands. With `keep_queued` an answer he
/// already gave stays queued for delivery.
///
/// The agent's own card (`Q<n>`, `qoop task ask`) asks about work it goes on with: its own
/// stage moves (`stage_move`) leave it alone, and an answer of his that waits to be typed to it
/// is never dropped here; anything else ends it while open or passed to qass.
pub(crate) fn close_cards(
    tx: &Connection,
    terminal: i64,
    note: &str,
    keep_queued: bool,
    stage_move: bool,
    now: i64,
) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE ask SET status = 'closed', note = ?2, closed_at = ?3 \
         WHERE terminal = ?1 AND ((card LIKE 'W%' AND status IN ('open', 'queued', 'passed') \
         AND NOT (?4 AND status = 'queued')) \
         OR (card LIKE 'E%' AND status = 'open' AND opened_at < ?3) \
         OR (card LIKE 'Q%' AND NOT ?5 AND status IN ('open', 'passed')))",
        params![terminal, note, now, keep_queued, stage_move],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A notice stands alone: a second one with its handle is not filed while it is live, one
    /// with another handle is, and once he ends it the next is filed again. A tab's prompt or
    /// end never closes it.
    #[test]
    fn a_notice_is_filed_once_while_it_is_live() {
        let (s, t) = store_with_terminal();
        // qass's own card about no tab, under the same handle, is not a notice: it neither
        // stops one nor is ended with it.
        s.add_ask(
            "E1",
            None,
            None,
            None,
            r#"{"handle":"qass","asks":"mine"}"#,
            5,
        )
        .unwrap();
        let body = r#"{"handle":"qass","asks":"qass is not watching since 09:05"}"#;
        let first = s.add_notice(body, 10).unwrap().expect("filed");
        assert_eq!(s.add_notice(body, 11).unwrap(), None, "not twice");
        let other = r#"{"handle":"else"}"#;
        assert!(s.add_notice(other, 12).unwrap().is_some());
        s.prompt_taken(t, 20, 21).unwrap();
        s.close_dialog_drafts(t, 22).unwrap();
        let tx = s.conn.unchecked_transaction().unwrap();
        close_for_terminals(&tx, &[t], 23).unwrap();
        tx.commit().unwrap();
        assert_eq!(s.ask(&first).unwrap().unwrap().status, "open");
        assert!(s.close_ask(&first, "seen", 30).unwrap());
        let second = s
            .add_notice(body, 31)
            .unwrap()
            .expect("a new one once it ended");
        // The daemon ends it when what it says is over; only that handle's.
        assert!(s.close_notice("qass", "qass checks again", 40).unwrap());
        assert!(!s.close_notice("qass", "again", 41).unwrap(), "once");
        assert_eq!(s.ask(&second).unwrap().unwrap().status, "closed");
        assert_eq!(
            s.ask("E1").unwrap().unwrap().status,
            "open",
            "qass's card stays"
        );
        assert_eq!(
            s.live_asks().unwrap().len(),
            2,
            "the other notice and qass's card stay"
        );
    }

    fn store_with_terminal() -> (Store, i64) {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("w", 1).unwrap();
        let t = s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap();
        (s, t)
    }

    /// A draft is made for an agent waiting on him: never over a live card, once a waiting
    /// spell, a new number for a new spell; it closes when the agent stops waiting (its stage
    /// moved, it took an answer, it claimed or ended its task), and qass's own card with it.
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
        s.answer_waiting(t, 14, 14).unwrap();
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
        // qass's card about it ends the same way: its question is stale once it moved on.
        s.add_ask("E9", Some("T5"), Some(t), None, "{e}", 60)
            .unwrap();
        let task = s.open_task(t, "more", true, 61).unwrap();
        s.set_task_stage(task, "implementing", 62).unwrap();
        let e9 = s.ask("E9").unwrap().unwrap();
        assert_eq!(
            (e9.status.as_str(), e9.note.as_deref()),
            ("closed", Some("no longer waits"))
        );
    }

    /// A dialog draft needs no task, never stands over a live card, is filed once a spell (from
    /// the spell's start), and a tool or a prompt closes it even once he answered, while a
    /// waiting draft stays. A task's stage moving closes it too.
    #[test]
    fn a_dialog_draft_is_filed_once_a_spell_and_only_it_closes_on_a_tool() {
        let (s, t) = store_with_terminal();
        let first = s
            .add_dialog_draft("T5", t, Some("sess"), "{d}", 10, 130)
            .unwrap()
            .expect("no task needed");
        assert_eq!(s.ask(&first).unwrap().unwrap().task, None);
        assert_eq!(
            s.add_dialog_draft("T5", t, None, "{d}", 10, 131).unwrap(),
            None,
            "a live card stands"
        );
        // His words went to qass: the note changed, and the tool still closes it.
        s.claim_ask(&first, "passed", None, "yes", "owner, inbox", 132)
            .unwrap();
        assert!(s.close_dialog_drafts(t, 133).unwrap());
        let row = s.ask(&first).unwrap().unwrap();
        assert_eq!(
            (row.status.as_str(), row.note.as_deref()),
            ("closed", Some("the dialog ended"))
        );
        assert_eq!(
            s.add_dialog_draft("T5", t, None, "{d}", 10, 140).unwrap(),
            None,
            "this spell had its draft"
        );
        let second = s
            .add_dialog_draft("T5", t, None, "{d}", 135, 255)
            .unwrap()
            .expect("a new spell");
        assert_ne!(second, first);
        // A waiting draft is not a dialog's: a tool leaves it.
        s.close_ask(&second, "dismissed", 256).unwrap();
        let task = s.open_task(t, "ship", false, 260).unwrap();
        s.set_task_stage(task, "waiting-human", 261).unwrap();
        let waiting = s
            .add_draft("T5", t, None, "{d}", 262)
            .unwrap()
            .expect("a waiting draft");
        assert!(!s.close_dialog_drafts(t, 263).unwrap());
        assert_eq!(s.ask(&waiting).unwrap().unwrap().status, "open");
        // With a task open, its stage moving closes a dialog draft.
        s.set_task_stage(task, "implementing", 264).unwrap();
        let third = s
            .add_dialog_draft("T5", t, None, "{d}", 270, 390)
            .unwrap()
            .expect("a new spell");
        s.set_task_stage(task, "reviewing", 391).unwrap();
        assert_eq!(s.ask(&third).unwrap().unwrap().status, "closed");
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
        assert!(err.to_string().contains(NOT_FREE), "{err}");
    }

    /// Two questions given one number: a rewrite naming another handle is refused, a typo fix
    /// under the same handle is taken.
    #[test]
    fn an_open_card_is_never_rewritten_as_another_question() {
        let (s, t) = store_with_terminal();
        let card = |handle: &str, asks: &str| format!(r#"{{"handle":"{handle}","asks":"{asks}"}}"#);
        s.add_ask("E8", Some("T5"), Some(t), None, &card("korea", "ship?"), 10)
            .unwrap();
        let err = s
            .add_ask("E8", None, None, None, &card("cv", "merge?"), 11)
            .unwrap_err();
        assert!(
            err.to_string().contains("taken") && err.to_string().contains(NOT_FREE),
            "{err}"
        );
        assert_eq!(
            s.add_ask(
                "E8",
                Some("T5"),
                Some(t),
                None,
                &card("korea", "ship it?"),
                12
            )
            .unwrap(),
            Added::Replaced
        );
        assert!(s.ask("E8").unwrap().unwrap().body.contains("ship it?"));
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
        s.reopen_ask("E1", None).unwrap();
        let back = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (back.status.as_str(), back.answer, back.closed_at),
            ("open", None, None)
        );
    }

    #[test]
    fn a_queued_answer_is_live_delivered_once_and_ended_by_a_close() {
        let (s, t) = store_with_terminal();
        for (card, at) in [("E1", 10), ("E2", 11)] {
            s.add_ask(card, None, None, None, "{}", at).unwrap();
            s.claim_ask(card, "queued", None, "go", "queued", at + 10)
                .unwrap();
        }
        assert_eq!(s.live_asks().unwrap().len(), 2, "queued is live");
        let order: Vec<String> = s
            .queued_asks()
            .unwrap()
            .into_iter()
            .map(|a| a.card)
            .collect();
        assert_eq!(order, ["E1", "E2"], "oldest first");
        assert!(s.deliver_ask("E1", "passed", "passed to qass", 30).unwrap());
        assert!(
            !s.deliver_ask("E1", "passed", "passed to qass", 31).unwrap(),
            "once only"
        );
        // A close (the chat answered) ends a queued card, so it is never typed.
        assert!(s.close_ask("E2", "answered in chat", 32).unwrap());
        assert!(!s.deliver_ask("E2", "passed", "passed to qass", 33).unwrap());
        // A queued card gives back with its reason; its agent exiting leaves it to qass.
        s.add_ask("E3", Some("T5"), Some(t), None, "{}", 40)
            .unwrap();
        s.claim_ask("E3", "queued", None, "go", "queued", 41)
            .unwrap();
        s.reopen_ask("E3", Some("not delivered: no tab")).unwrap();
        let back = s.ask("E3").unwrap().unwrap();
        assert_eq!(
            (back.status.as_str(), back.note.as_deref()),
            ("open", Some("not delivered: no tab"))
        );
        s.claim_ask("E3", "queued", None, "go", "queued", 42)
            .unwrap();
        s.close_terminal(t, 50).unwrap();
        let e3 = s.ask("E3").unwrap().unwrap();
        assert_eq!((e3.status.as_str(), e3.terminal), ("queued", None));
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
    fn a_new_card_ends_only_the_open_e_cards_it_names() {
        let (s, t) = store_with_terminal();
        for (n, card) in ["E1", "E2", "E3", "E4", "E6"].into_iter().enumerate() {
            s.add_ask(card, None, None, None, "{}", 10 + n as i64)
                .unwrap();
        }
        s.claim_ask("E2", "queued", Some("a"), "a", "queued", 20)
            .unwrap();
        s.claim_ask("E3", "passed", None, "go", "passed to qass", 20)
            .unwrap();
        s.close_ask("E4", "chat", 20).unwrap();
        let w = draft_on(&s, t, "T5", false, 30);
        let named: Vec<String> = ["E1", "E2", "E3", "E4", "E5", "E6", &w, "E99"]
            .iter()
            .map(|c| c.to_string())
            .collect();
        let closed = s.supersede_asks("E5", &named, 40).unwrap();
        assert_eq!(
            closed,
            ["E1", "E6"],
            "only open E cards, in the order named"
        );
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref(), e1.closed_at),
            ("closed", Some("superseded by E5"), Some(40))
        );
        for (card, status) in [("E2", "queued"), ("E3", "passed"), ("E4", "closed")] {
            assert_eq!(s.ask(card).unwrap().unwrap().status, status, "{card}");
        }
        assert_eq!(s.ask("E4").unwrap().unwrap().note.as_deref(), Some("chat"));
        assert_eq!(s.ask(&w).unwrap().unwrap().status, "open", "a W card stays");
        s.add_ask("E5", None, None, None, "{}", 41).unwrap();
        assert!(
            s.supersede_asks("E5", &["E5".into()], 42)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.ask("E5").unwrap().unwrap().status, "open", "never itself");
        assert!(s.supersede_asks("E5", &[], 43).unwrap().is_empty());
    }

    fn session_of(s: &Store, t: i64) -> Option<String> {
        s.conn
            .query_row(
                "SELECT agent_session FROM terminal WHERE id = ?1",
                params![t],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The agent left a conversation after the grace: the terminal forgets the id and its open
    /// cards of that conversation end. An answer he gave (queued, passed), a card of another
    /// conversation or of none stay.
    #[test]
    fn an_agent_that_left_ends_the_open_cards_of_its_conversation_only() {
        let (s, t) = store_with_terminal();
        s.set_agent_session(t, "sess").unwrap();
        s.add_ask("E1", Some("T5"), Some(t), Some("sess"), "{}", 10)
            .unwrap();
        s.add_ask("E2", None, Some(t), Some("sess"), "{}", 11)
            .unwrap();
        s.add_ask("E3", None, Some(t), Some("sess"), "{}", 12)
            .unwrap();
        s.add_ask("E4", None, Some(t), Some("other"), "{}", 13)
            .unwrap();
        s.add_ask("E5", None, Some(t), None, "{}", 14).unwrap();
        s.claim_ask("E2", "queued", Some("a"), "a", "queued", 20)
            .unwrap();
        s.claim_ask("E3", "passed", None, "go", "passed to qass", 20)
            .unwrap();
        assert_eq!(s.agent_left(t, "sess", 50).unwrap(), 1);
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref(), e1.closed_at),
            ("closed", Some("its agent left"), Some(50))
        );
        for (card, status) in [
            ("E2", "queued"),
            ("E3", "passed"),
            ("E4", "open"),
            ("E5", "open"),
        ] {
            assert_eq!(s.ask(card).unwrap().unwrap().status, status, "{card}");
        }
        assert_eq!(session_of(&s, t), None);
    }

    /// Nothing ends unless the forget changed a row: a sleeping tab keeps its id and its
    /// cards (they still wait on him), and so does a tab whose row holds another id by now.
    #[test]
    fn an_agent_that_left_ends_nothing_when_the_forget_changed_no_row() {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("w", 1).unwrap();
        let (asleep, moved) = (
            s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap(),
            s.create_terminal(6, ws, "/tmp", "/bin/sh", 2).unwrap(),
        );
        for (t, card) in [(asleep, "E1"), (moved, "E2")] {
            s.set_agent_session(t, "sess").unwrap();
            s.add_ask(card, None, Some(t), Some("sess"), "{}", 10)
                .unwrap();
        }
        s.sleep_terminals(
            20,
            &[crate::Checkpoint {
                terminal: asleep,
                agent: Some("claude".into()),
                agent_session: Some("sess".into()),
            }],
        )
        .unwrap();
        assert_eq!(s.agent_left(asleep, "sess", 50).unwrap(), 0);
        assert_eq!(s.ask("E1").unwrap().unwrap().status, "open", "asleep");
        assert_eq!(session_of(&s, asleep).as_deref(), Some("sess"));
        s.set_agent_session(moved, "next").unwrap();
        assert_eq!(s.agent_left(moved, "sess", 50).unwrap(), 0);
        assert_eq!(s.ask("E2").unwrap().unwrap().status, "open", "new id");
        assert_eq!(session_of(&s, moved).as_deref(), Some("next"));
    }

    /// A draft never goes over qass's card for the same terminal, open or already answered,
    /// and never closes it.
    #[test]
    fn a_draft_never_replaces_a_live_card_of_qass() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        s.add_ask("E1", Some("T5"), Some(t), None, "{e}", 12)
            .unwrap();
        assert_eq!(s.add_draft("T5", t, None, "{d}", 13).unwrap(), None, "open");
        s.claim_ask("E1", "answered", None, "go", "owner, inbox", 14)
            .unwrap();
        assert_eq!(
            s.add_draft("T5", t, None, "{d}", 15).unwrap(),
            None,
            "answered"
        );
        assert_eq!(s.ask("E1").unwrap().unwrap().status, "answered");
    }

    /// A waiting draft ends when its agent works again; an answer he gave and qass's own card
    /// are not its to end.
    #[test]
    fn a_working_agent_closes_its_waiting_draft_only() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        let draft = s.add_draft("T5", t, None, "{d}", 12).unwrap().unwrap();
        assert!(s.close_waiting_drafts(t, 13).unwrap());
        let row = s.ask(&draft).unwrap().unwrap();
        assert_eq!(
            (row.status.as_str(), row.note.as_deref()),
            ("closed", Some("it works again"))
        );
        assert!(!s.close_waiting_drafts(t, 14).unwrap(), "once");
        // It waits again with no stage moved: a card closed that way does not stand for the
        // spell. One written in the second of the event is the turn end after it, and stays.
        let next = s
            .add_draft("T5", t, None, "{d}", 30)
            .unwrap()
            .expect("asks again");
        assert!(!s.close_waiting_drafts(t, 30).unwrap(), "same second");
        assert!(s.close_waiting_drafts(t, 31).unwrap());
        assert_eq!(s.ask(&next).unwrap().unwrap().status, "closed");
        s.add_ask("E2", Some("T5"), Some(t), None, "{e}", 23)
            .unwrap();
        assert!(!s.close_waiting_drafts(t, 24).unwrap());
        assert_eq!(s.ask("E2").unwrap().unwrap().status, "open");
    }

    /// A plan's card waits on qass, not on the agent: its work does not end the card.
    #[test]
    fn an_agent_going_on_keeps_the_card_of_its_plan() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        let card = s.add_plan_draft("T5", t, None, "{d}", 12).unwrap().unwrap();
        assert!(!s.close_waiting_drafts(t, 20).unwrap());
        assert_eq!(s.ask(&card).unwrap().unwrap().status, "open");
    }

    /// A card waiting on the owner names its terminal as one that needs him; a plan's card,
    /// which waits on qass while its agent works, does not.
    #[test]
    fn only_a_card_waiting_on_the_owner_marks_its_terminal() {
        let (mut s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        s.add_plan_draft("T5", t, None, "{p}", 12).unwrap().unwrap();
        assert!(
            s.open_ask_terminals().unwrap().is_empty(),
            "a plan waits on qass"
        );
        let ws = s.create_workspace("v", 2).unwrap();
        let other = s.create_terminal(6, ws, "/tmp", "/bin/sh", 3).unwrap();
        let task = s.open_task(other, "ask", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        s.add_draft("T6", other, None, "{d}", 12)
            .unwrap()
            .expect("a draft");
        assert_eq!(
            s.open_ask_terminals()
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [other]
        );
    }

    /// The goal of a task qoop opened when the agent changed files is the owner's cut prompt,
    /// until someone writes a goal.
    #[test]
    fn an_auto_opened_goal_is_the_owners_prompt_until_a_goal_is_written() {
        let (s, t) = store_with_terminal();
        let (auto, _) = s
            .begin_task(t, "fix the rate, you idiot", Some(9), 10)
            .unwrap();
        assert!(s.goal_is_prompt(auto).unwrap());
        s.set_task_goal(auto, "Use the live rate; done when tests pass", 11)
            .unwrap();
        assert!(!s.goal_is_prompt(auto).unwrap());
        let own = s.open_task(t, "ship the login fix", true, 12).unwrap();
        assert!(!s.goal_is_prompt(own).unwrap(), "the agent's own words");
    }

    /// A waiting spell on terminal `t` with a draft for it, his answer queued when `queue`.
    fn draft_on(s: &Store, t: i64, agent: &str, queue: bool, at: i64) -> String {
        let task = s.open_task(t, "ship", true, at).unwrap();
        s.set_task_stage(task, "waiting-human", at).unwrap();
        let card = s
            .add_draft(agent, t, Some("sess"), "{d}", at)
            .unwrap()
            .unwrap();
        if queue {
            s.claim_ask(&card, "queued", Some("a"), "a", "queued", at)
                .unwrap();
        }
        card
    }

    fn untied(s: &Store, card: &str) -> (String, Option<i64>, Option<String>, Option<String>) {
        let a = s.ask(card).unwrap().unwrap();
        (a.status, a.terminal, a.agent, a.note)
    }

    /// A tab's end closes its open draft and a passed card; qass's open card and a queued answer
    /// go on to qass, untied from the terminal and the agent; a remote card is not touched.
    #[test]
    fn a_tabs_end_closes_its_open_draft_and_hands_the_rest_to_qass() {
        let mut s = Store::in_memory().expect("open");
        let ws = s.create_workspace("w", 1).unwrap();
        let (t5, t6) = (
            s.create_terminal(5, ws, "/tmp", "/bin/sh", 2).unwrap(),
            s.create_terminal(6, ws, "/tmp", "/bin/sh", 2).unwrap(),
        );
        let w5 = draft_on(&s, t5, "T5", false, 10);
        let w6 = draft_on(&s, t6, "T6", true, 11);
        s.add_ask("E1", Some("T6"), Some(t6), None, "{}", 12)
            .unwrap();
        s.add_ask("E2", Some("ubox/T2"), None, None, "{}", 13)
            .unwrap();
        s.add_ask("E3", None, Some(t5), None, "{}", 14).unwrap();
        s.claim_ask("E3", "passed", None, "go", "passed to qass", 15)
            .unwrap();
        assert_eq!(
            s.ask(&w6).unwrap().unwrap().status,
            "queued",
            "never superseded"
        );
        assert!(s.close_terminal(t5, 50).unwrap());
        assert!(s.close_terminal(t6, 50).unwrap());
        // The agent's own open question is left for its conversation to raise if resumed.
        let closed = s.ask(&w5).unwrap().unwrap();
        assert_eq!(
            (closed.status.as_str(), closed.note.as_deref()),
            ("closed", Some(LEFT))
        );
        assert_eq!(
            s.ask("E3").unwrap().unwrap().status,
            "closed",
            "a passed card: his words already reached qass"
        );
        // qass's open card about a closed tab is stale: it closes, as a draft does.
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref()),
            ("closed", Some("agent closed"))
        );
        let tab = Some(TAB_CLOSED.to_string());
        assert_eq!(untied(&s, &w6), ("queued".into(), None, None, tab));
        assert_eq!(
            untied(&s, "E2"),
            ("open".into(), None, Some("ubox/T2".into()), None),
            "a remote card stays"
        );
        // A restart gives the queued answer back to him, as any queued answer.
        s.tidy_asks(60).unwrap();
        let w6 = s.ask(&w6).unwrap().unwrap();
        assert_eq!(
            (w6.status.as_str(), w6.terminal, w6.note.as_deref()),
            ("open", None, Some("not delivered: qoop restarted"))
        );
        // Old ended cards go; a live one is never pruned.
        s.tidy_asks(50 + KEEP_SECS + 1).unwrap();
        assert!(s.ask(&w5).unwrap().is_none());
        assert!(s.ask("E2").unwrap().is_some());
    }

    /// The agent a card asks about moves on (it takes a prompt, its task changes stage, it
    /// claims done): every question about it still open ends, qass's too; one written in the
    /// same second as the move, a remote one and his queued answer stand.
    #[test]
    fn a_card_ends_when_its_agent_moves_on() {
        let (s, t) = store_with_terminal();
        s.add_ask("E1", Some("T5"), Some(t), None, "{}", 10)
            .unwrap();
        assert_eq!(s.prompt_taken(t, 20, 20).unwrap(), 1);
        let e1 = s.ask("E1").unwrap().unwrap();
        assert_eq!(
            (e1.status.as_str(), e1.note.as_deref()),
            ("closed", Some("it took a prompt in its tab"))
        );
        s.add_ask("E2", Some("T5"), Some(t), None, "{}", 30)
            .unwrap();
        assert_eq!(s.prompt_taken(t, 30, 30).unwrap(), 0, "written this second");
        let task = s.open_task(t, "ship it", false, 31).unwrap();
        let stage = s.task(task).unwrap().unwrap().stage;
        s.set_task_stage(task, &stage, 36).unwrap();
        assert_eq!(
            s.ask("E2").unwrap().unwrap().status,
            "open",
            "a stage written again"
        );
        s.set_task_stage(task, "reviewing", 40).unwrap();
        assert_eq!(s.ask("E2").unwrap().unwrap().status, "closed");
        s.add_ask("E3", Some("T5"), Some(t), None, "{}", 50)
            .unwrap();
        s.set_task_stage(task, "waiting-human", 60).unwrap();
        assert_eq!(
            s.ask("E3").unwrap().unwrap().status,
            "open",
            "the wait is the question"
        );
        s.claim_task_done(task, "done", 70).unwrap();
        assert_eq!(s.ask("E3").unwrap().unwrap().status, "closed");
        // His answer waiting to be typed survives a prompt; only open cards end.
        let w = draft_on(&s, t, "T5", true, 80);
        assert_eq!(s.prompt_taken(t, 90, 90).unwrap(), 0);
        assert_eq!(s.ask(&w).unwrap().unwrap().status, "queued");
    }

    /// A terminal that ended while no daemon watched is ended the same way at start.
    #[test]
    fn a_tab_that_ended_unseen_is_ended_the_same_way_at_start() {
        let (s, t) = store_with_terminal();
        let open = draft_on(&s, t, "T5", false, 10);
        s.add_ask("E1", Some("T9"), Some(9), None, "{}", 11)
            .unwrap();
        s.claim_ask("E1", "queued", None, "go", "queued", 12)
            .unwrap();
        s.conn
            .execute(
                "UPDATE terminal SET closed_at = 20 WHERE id = ?1",
                params![t],
            )
            .unwrap();
        s.tidy_asks(30).unwrap();
        assert_eq!(s.ask(&open).unwrap().unwrap().status, "closed");
        // Untied first, so its queued answer is given back to him rather than closed.
        assert_eq!(
            untied(&s, "E1"),
            (
                "open".into(),
                None,
                None,
                Some("not delivered: qoop restarted".into())
            )
        );
    }

    /// An answer he already gave outlives its task: closing the task leaves it queued for
    /// delivery, while an open draft closes with it.
    #[test]
    fn an_answer_he_gave_outlives_its_task() {
        let (s, t) = store_with_terminal();
        let card = draft_on(&s, t, "T5", true, 10);
        let task = s.open_task_of(t).unwrap().unwrap().id;
        s.close_task(task, crate::Outcome::Verified, "ok", 20)
            .unwrap();
        let a = s.ask(&card).unwrap().unwrap();
        assert_eq!((a.status.as_str(), a.terminal), ("queued", Some(t)));
    }

    fn status_note(s: &Store, card: &str) -> (String, Option<String>) {
        let a = s.ask(card).unwrap().unwrap();
        (a.status, a.note)
    }

    fn stage(s: &Store, task: i64) -> String {
        s.task(task).unwrap().unwrap().stage
    }

    /// The agent's own question asks about work it goes on with: its stage moves, its tool
    /// calls and qass's card for it leave the card open, one question at a time, and only with
    /// an open task.
    #[test]
    fn an_agent_ask_survives_its_stage_moves_and_work() {
        let (s, t) = store_with_terminal();
        assert!(matches!(
            s.add_agent_ask("T5", t, Some("sess"), "{q}", 9),
            Err(StoreError::Ask(AskError::NoTask))
        ));
        let task = s.open_task(t, "ship", false, 10).unwrap();
        let card = s.add_agent_ask("T5", t, Some("sess"), "{q}", 11).unwrap();
        assert!(card.starts_with('Q'));
        assert!(matches!(
            s.add_agent_ask("T5", t, Some("sess"), "{q2}", 12),
            Err(StoreError::Ask(AskError::Waiting(c))) if c == card
        ));
        s.set_task_stage(task, "reviewing", 13).unwrap();
        s.set_task_stage(task, "verifying", 14).unwrap();
        s.close_waiting_drafts(t, 15).unwrap();
        s.add_ask("E9", Some("T5"), Some(t), None, r#"{"handle":"x"}"#, 16)
            .unwrap();
        assert_eq!(status_note(&s, &card).0, "open");
        assert_eq!(
            s.add_draft("T5", t, None, "{d}", 17).unwrap(),
            None,
            "no turn-end draft over it"
        );
    }

    /// His answer to the agent's question still reaches it when the agent claims its task
    /// done; one qass is relaying ends with the task, as an open one does.
    #[test]
    fn an_answer_to_an_agent_ask_survives_done() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        let card = s.add_agent_ask("T5", t, Some("sess"), "{q}", 11).unwrap();
        s.claim_ask(&card, "queued", Some("a"), "go", "owner, inbox", 12)
            .unwrap();
        s.claim_task_done(task, "shipped", 13).unwrap();
        assert_eq!(status_note(&s, &card).0, "queued");
        s.deliver_ask(&card, "passed", "qass", 14).unwrap();
        s.close_task(task, crate::Outcome::Verified, "ok", 15)
            .unwrap();
        assert_eq!(status_note(&s, &card).0, "closed");
    }

    /// An open card he did not answer in a day is set aside as left, kept; its task stops
    /// waiting on him. A plan's card, a notice and a younger card stay open.
    #[test]
    fn an_open_card_leaves_after_a_day_and_its_task_stops_waiting() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "reviewing", 11).unwrap();
        s.set_task_stage(task, "waiting-human", 12).unwrap();
        let card = s
            .add_draft("T5", t, Some("sess"), "{d}", 100)
            .unwrap()
            .unwrap();
        let notice = s.add_notice(r#"{"handle":"qass"}"#, 100).unwrap().unwrap();
        assert_eq!(s.leave_asks(150, 100).unwrap(), 0, "younger than a day");
        assert_eq!(s.leave_asks(200, 100).unwrap(), 1);
        assert_eq!(status_note(&s, &card), ("closed".into(), Some(LEFT.into())));
        assert_eq!(status_note(&s, &notice).0, "open");
        assert_eq!(stage(&s, task), "reviewing");
        let plan = {
            let (s2, t2) = (&s, t);
            s2.set_task_stage(task, "waiting-human", 300).unwrap();
            s2.add_plan_draft("T5", t2, None, "{p}", 300)
                .unwrap()
                .unwrap()
        };
        s.leave_asks(1000, 100).unwrap();
        assert_eq!(status_note(&s, &plan).0, "open");
    }

    /// Leaving one card touches no other: a younger card on the same tab stays open, and the
    /// task keeps waiting while it does.
    #[test]
    fn leave_touches_no_other_card() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        let old = s.add_draft("T5", t, None, "{d}", 100).unwrap().unwrap();
        s.add_ask("E3", None, Some(t), None, r#"{"handle":"y"}"#, 190)
            .unwrap();
        s.leave_asks(200, 100).unwrap();
        assert_eq!(status_note(&s, &old).1.as_deref(), Some(LEFT));
        assert_eq!(status_note(&s, "E3").0, "open");
        assert_eq!(stage(&s, task), "waiting-human");
    }

    /// A tab closing over the agent's own open question leaves it for a resumed conversation;
    /// qass's card closes as before.
    #[test]
    fn a_closed_tab_leaves_its_question() {
        let (s, t) = store_with_terminal();
        s.open_task(t, "ship", false, 10).unwrap();
        let card = s.add_agent_ask("T5", t, Some("sess"), "{q}", 11).unwrap();
        let tx = s.conn.unchecked_transaction().unwrap();
        close_for_terminals(&tx, &[t], 12).unwrap();
        tx.commit().unwrap();
        assert_eq!(status_note(&s, &card), ("closed".into(), Some(LEFT.into())));
        let (s, t) = store_with_terminal();
        s.add_ask("E4", Some("T5"), Some(t), None, r#"{"handle":"z"}"#, 11)
            .unwrap();
        let tx = s.conn.unchecked_transaction().unwrap();
        close_for_terminals(&tx, &[t], 12).unwrap();
        tx.commit().unwrap();
        assert_eq!(status_note(&s, "E4").1.as_deref(), Some("agent closed"));
    }

    /// A question he left does not count as asked for the wait: the agent's re-ask gets a card.
    #[test]
    fn a_left_card_does_not_block_the_re_ask() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        s.add_draft("T5", t, None, "{d}", 100).unwrap().unwrap();
        s.leave_asks(200, 100).unwrap();
        s.set_task_stage(task, "waiting-human", 201).unwrap();
        assert!(s.add_draft("T5", t, None, "{d}", 202).unwrap().is_some());
    }

    /// A left question is raised once, to its own conversation: another conversation in the
    /// same tab never sees it, and a card with no conversation is matched by its task.
    #[test]
    fn a_left_question_is_raised_once_for_its_session_only() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        let card = s.add_agent_ask("T5", t, Some("sess"), "{q}", 11).unwrap();
        s.leave_asks(200, 100).unwrap();
        assert!(
            s.raise_left(Some("other"), Some(task + 1))
                .unwrap()
                .is_empty()
        );
        let raised = s.raise_left(Some("sess"), Some(task)).unwrap();
        assert_eq!(
            raised.iter().map(|r| r.card.as_str()).collect::<Vec<_>>(),
            [card.as_str()]
        );
        assert!(
            s.raise_left(Some("sess"), Some(task)).unwrap().is_empty(),
            "once"
        );
        assert_eq!(status_note(&s, &card).1.as_deref(), Some(LEFT_RAISED));
        let none = s.add_agent_ask("T5", t, None, "{q}", 300).unwrap();
        s.leave_asks(500, 100).unwrap();
        let raised = s.raise_left(Some("sess"), Some(task)).unwrap();
        assert_eq!(
            raised.iter().map(|r| r.card.as_str()).collect::<Vec<_>>(),
            [none.as_str()]
        );
    }

    /// A closing tab leaves the agent's own question, never a plan's card; an agent leaving
    /// its conversation leaves its question the same way.
    #[test]
    fn a_plans_card_is_never_left_and_an_agent_leaving_leaves_its_question() {
        let (s, t) = store_with_terminal();
        let task = s.open_task(t, "ship", false, 10).unwrap();
        s.set_task_stage(task, "waiting-human", 11).unwrap();
        let plan = s.add_plan_draft("T5", t, None, "{p}", 12).unwrap().unwrap();
        let tx = s.conn.unchecked_transaction().unwrap();
        close_for_terminals(&tx, &[t], 13).unwrap();
        tx.commit().unwrap();
        assert_eq!(status_note(&s, &plan).1.as_deref(), Some("agent closed"));
        let (s, t) = store_with_terminal();
        s.set_agent(t, Some("claude"), Some("sess")).unwrap();
        s.open_task(t, "ship", false, 10).unwrap();
        let card = s.add_agent_ask("T5", t, Some("sess"), "{q}", 11).unwrap();
        assert_eq!(s.agent_left(t, "sess", 12).unwrap(), 1);
        assert_eq!(status_note(&s, &card).1.as_deref(), Some(LEFT));
    }

    /// qass's card about nobody leaves after a day too; a notice about qass itself does not.
    #[test]
    fn a_card_about_nobody_leaves_but_a_notice_stays() {
        let (s, _) = store_with_terminal();
        s.add_ask("E7", None, None, None, r#"{"handle":"elster"}"#, 100)
            .unwrap();
        let notice = s.add_notice(r#"{"handle":"qass"}"#, 100).unwrap().unwrap();
        assert_eq!(s.leave_asks(300, 100).unwrap(), 1);
        assert_eq!(status_note(&s, "E7").1.as_deref(), Some(LEFT));
        assert_eq!(status_note(&s, &notice).0, "open");
    }
}

//! Durable metadata: workspaces, terminals and layout.
//!
//! ADR-005. SQLite with WAL for metadata, segmented files for terminal history, and a hard
//! rule that nothing in a terminal's path ever waits on either. This crate is deliberately
//! synchronous and knows nothing about tokio: the caller decides what runs on the blocking
//! pool, because a store that spawns its own tasks is a store that can be called from the
//! reactor by accident.
//!
//! What is here is metadata, not state. A terminal's pty, engine and write queue die with
//! the process. What survives is what a person asked for: the workspaces they made, the
//! terminals in them, and where those terminals were started.

#![forbid(unsafe_code)]

pub mod history;

use std::path::Path;

pub use history::History;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("no workspace {0}")]
    NoWorkspace(i64),
    #[error(
        "this database was written by a newer qoro (schema {found}, this build understands \
         {understood}); use that build or move the file aside"
    )]
    FromTheFuture { found: i64, understood: i64 },
    /// The caller stopped waiting before the write was committed, so it was rolled back
    /// rather than landing after the caller had already answered.
    #[error("abandoned by its caller before it was committed")]
    Abandoned,
    /// A ledger row that is not the JSON this build writes, so something other than this
    /// build wrote it. A distinct variant rather than a string inside `Sqlite` because the
    /// caller has to be able to tell it apart from "the store is busy", and it is reported
    /// rather than hidden either way. What the daemon does with it is ADR-005's rule 1: it
    /// is on the terminal's path, so it never turns a command into a refusal.
    #[error("a stored command outcome is not readable: {0}")]
    Outcome(#[from] serde_json::Error),
}

/// The ledger's answer to a create that made terminal `id`, as it is stored.
fn created_answer(id: i64) -> Result<String, serde_json::Error> {
    serde_json::to_string(&(true, serde_json::json!({ "terminal": id })))
}

/// The schema version this build writes and expects.
///
/// Checked on open rather than assumed. A daemon that silently runs against a newer
/// schema is how a downgrade eats data.
pub const SCHEMA_VERSION: i64 = 2;

pub struct Store {
    conn: Connection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRow {
    pub id: i64,
    pub workspace: i64,
    pub title: Option<String>,
    pub cwd: String,
    pub argv: String,
    /// The agent last seen in this terminal, if one ever was. What restore reads to
    /// decide whether there is a conversation to carry on, and whose conversation.
    pub agent: Option<String>,
    /// That agent's own id for the conversation, when it told us one. Absent means
    /// restore can only ask the agent for its newest conversation in this directory.
    pub agent_session: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

/// Bring a database up to the schema this build writes.
///
/// A ladder rather than a version check, because the check this replaces refused every
/// database written by an older build and the only way past it was to delete the file.
/// Losing a person's workspaces to a schema bump is not a migration policy. A database
/// from a newer build is still refused: this build cannot know what a column it has never
/// heard of means, and guessing is how a downgrade eats data.
fn migrate(conn: &Connection, found: i64) -> Result<(), StoreError> {
    if found > SCHEMA_VERSION {
        return Err(StoreError::FromTheFuture {
            found,
            understood: SCHEMA_VERSION,
        });
    }
    // Written only when it changes: every open writing it made a read-only `qoro ls` a
    // writer, racing the daemon's own writes for the lock.
    if found >= SCHEMA_VERSION {
        return Ok(());
    }
    // One transaction for the ALTERs and the stamp, which SQLite makes atomic for DDL too.
    // Separate, a kill between two of them left a version 1 database with one column added,
    // and every later open failed on the duplicate column.
    let tx = conn.unchecked_transaction()?;
    // A fresh database has every column already, because the CREATE statements above are
    // the current shape. Only a database that predates a column needs the ALTER.
    if found != 0 && found < 2 {
        for statement in [
            "ALTER TABLE terminal ADD COLUMN agent TEXT",
            "ALTER TABLE terminal ADD COLUMN agent_session TEXT",
        ] {
            tx.execute(statement, [])?;
        }
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

impl Store {
    /// Open, creating and migrating as needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// The same, for a file that must already exist: a reader that finds nothing gets an
    /// error rather than an empty database left behind in its place.
    pub fn open_existing(path: &Path) -> Result<Self, StoreError> {
        let flags = rusqlite::OpenFlags::default() - rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        Self::prepare(Connection::open_with_flags(path, flags)?)
    }

    /// An in-memory store, for tests and for a daemon told not to persist.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        // Wait for a lock the daemon holds rather than failing at once, before anything
        // below can need one: a `qoro ls` landing on a daemon write printed "0 tabs".
        // Five seconds, the runtime's own deadline for a store call.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // WAL so a reader never blocks the writer, and `foreign_keys` because a layout row
        // pointing at a terminal that does not exist is not a state worth representing.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS terminal (
                 id INTEGER PRIMARY KEY,
                 workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                 title TEXT,
                 cwd TEXT NOT NULL,
                 argv TEXT NOT NULL,
                 agent TEXT,
                 agent_session TEXT,
                 created_at INTEGER NOT NULL,
                 closed_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS layout (
                 workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                 terminal INTEGER NOT NULL REFERENCES terminal(id) ON DELETE CASCADE,
                 ordinal INTEGER NOT NULL,
                 PRIMARY KEY (workspace, terminal)
             );
             CREATE INDEX IF NOT EXISTS terminal_by_workspace ON terminal(workspace);
             -- Each tab's split tree, as JSON the client wrote. A table of its own and
             -- created unconditionally rather than a column behind a version bump: a
             -- bump makes every older binary refuse this file, and an older binary that
             -- ignores the table loses nothing but the splits.
             CREATE TABLE IF NOT EXISTS split_tree (tree TEXT NOT NULL);
             -- The workspaces that are asleep: their rows are open and nothing runs. The
             -- same reasoning as split_tree: an older binary ignores the table and brings
             -- a sleeping space back awake, which loses nothing.
             CREATE TABLE IF NOT EXISTS asleep (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE,
                 since INTEGER NOT NULL
             );
             -- What a mutating command with this key already produced, so a retry after an
             -- ambiguous failure returns the original answer instead of doing the work
             -- again. The same reasoning as split_tree and asleep: the table is new and the
             -- version is not, because a version bump makes every older binary refuse the
             -- file and cost the person their workspaces. An older binary that ignores this
             -- table loses only retry safety, which it never had a reader for anyway.
             CREATE TABLE IF NOT EXISTS applied (
                 idempotency_key TEXT PRIMARY KEY,
                 outcome TEXT NOT NULL,
                 at_unix INTEGER NOT NULL
             );
             -- What a person called a conversation here, keyed by the agent's own id
             -- for it, so the name follows the conversation through a sleep and a
             -- restart and is never written into the agent's storage. New table, no
             -- version bump, for the reason the ones above give.
             CREATE TABLE IF NOT EXISTS session_title (
                 session TEXT PRIMARY KEY,
                 title TEXT NOT NULL
             );",
        )?;
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        migrate(&conn, found)?;
        Ok(Self { conn })
    }

    pub fn create_workspace(&self, name: &str, now: i64) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO workspace (name, created_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Give a workspace a different name.
    ///
    /// Answers how many rows it touched, so a caller can tell "renamed" from "there is no
    /// such workspace" without asking a second question.
    pub fn rename_workspace(&self, id: i64, name: &str) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            "UPDATE workspace SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?)
    }

    /// Drop a workspace row.
    ///
    /// Answers how many rows it touched, like the rename above. Terminal and layout rows
    /// go with it through `ON DELETE CASCADE`: the daemon only calls this for a
    /// workspace with no live terminal, so what goes is dead history, never a session
    /// somebody is looking at.
    pub fn delete_workspace(&self, id: i64) -> Result<usize, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM workspace WHERE id = ?1", params![id])?)
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM workspace ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkspaceRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Write down what a command with this key produced.
    ///
    /// `INSERT OR IGNORE`, so the first answer is the one that stands. A command that
    /// answers twice has already done the work twice by the time the second write lands,
    /// and overwriting would replace the answer the first caller received with one that
    /// does not match what happened.
    pub fn record_applied(
        &self,
        key: &str,
        ok: bool,
        detail: &serde_json::Value,
        now: i64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO applied (idempotency_key, outcome, at_unix)
             VALUES (?1, ?2, ?3)",
            params![key, serde_json::to_string(&(ok, detail))?, now],
        )?;
        Ok(())
    }

    /// What a command with this key already produced, if it has been applied.
    pub fn applied(&self, key: &str) -> Result<Option<(bool, serde_json::Value)>, StoreError> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT outcome FROM applied WHERE idempotency_key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        }
    }

    /// Undo a create whose shell never started: close its row and take back its answer, in
    /// one transaction, so a retry tries again instead of being told about a dead tab.
    ///
    /// Only an answer naming this terminal is taken back. A key answered by another
    /// attempt at the same create stands, whatever this one saw.
    pub fn abandon_create(&mut self, key: &str, id: i64, now: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE terminal SET closed_at = ?2 WHERE id = ?1 AND closed_at IS NULL",
            params![id, now],
        )?;
        tx.execute(
            "DELETE FROM applied WHERE idempotency_key = ?1 AND outcome = ?2",
            params![key, created_answer(id)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Forget what is older than `before_unix`.
    ///
    /// A client's retry is seconds behind the answer it missed, so the ledger only has to
    /// outlive that. Without this the table is the one part of this store that would grow
    /// for as long as the machine is switched on.
    pub fn prune_applied(&self, before_unix: i64) -> Result<usize, StoreError> {
        Ok(self.conn.execute(
            "DELETE FROM applied WHERE at_unix < ?1",
            params![before_unix],
        )?)
    }

    /// How many terminals a session was holding when it was last left.
    ///
    /// Read straight from the database rather than from a daemon, so a listing works with
    /// nothing running, which is exactly when a person asks what sessions they have.
    pub fn open_terminal_count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM terminal WHERE closed_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    /// Record a terminal and place it at the end of its workspace's layout.
    ///
    /// One transaction, because a terminal with no layout row is a terminal nobody can see
    /// and a layout row with no terminal is a dangling reference. Either both land or
    /// neither does.
    /// `id` is the daemon's own stream id, not a key the database invents. One number for
    /// one terminal everywhere: the alternative is a mapping table that has to be kept
    /// true, and a restore that reads the wrong row when it is not.
    pub fn create_terminal(
        &mut self,
        id: i64,
        workspace: i64,
        cwd: &str,
        argv: &str,
        now: i64,
    ) -> Result<i64, StoreError> {
        self.create_terminal_answering(None, id, workspace, cwd, argv, None, None, now)
    }

    /// The same, with its name and the answer to the command that asked for it written
    /// down in the same transaction.
    ///
    /// A create answered only after a second write can start a shell and then fail to
    /// record it, which leaves a client told `Failed` with a tab running and a retry that
    /// opens another. Here the row, its place, its name and the answer land together or
    /// not at all, and a key already answered fails the insert, so two attempts at one
    /// create cannot both make a terminal.
    ///
    /// `abandoned` is read under the store's lock just before the commit: a caller whose
    /// deadline passed sets it, and the write rolls back instead of landing after the caller
    /// has already answered. A write already committed is visible to the caller's next read,
    /// which waits for the same lock.
    #[allow(clippy::too_many_arguments)]
    pub fn create_terminal_answering(
        &mut self,
        key: Option<&str>,
        id: i64,
        workspace: i64,
        cwd: &str,
        argv: &str,
        title: Option<&str>,
        abandoned: Option<&std::sync::atomic::AtomicBool>,
        now: i64,
    ) -> Result<i64, StoreError> {
        let tx = self.conn.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT id FROM workspace WHERE id = ?1",
                params![workspace],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StoreError::NoWorkspace(workspace));
        }
        tx.execute(
            "INSERT INTO terminal (id, workspace, cwd, argv, created_at, title) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, workspace, cwd, argv, now, title],
        )?;
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM layout WHERE workspace = ?1",
            params![workspace],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO layout (workspace, terminal, ordinal) VALUES (?1, ?2, ?3)",
            params![workspace, id, next],
        )?;
        if let Some(key) = key {
            tx.execute(
                "INSERT INTO applied (idempotency_key, outcome, at_unix) VALUES (?1, ?2, ?3)",
                params![key, created_answer(id)?, now],
            )?;
        }
        if abandoned.is_some_and(|a| a.load(std::sync::atomic::Ordering::SeqCst)) {
            return Err(StoreError::Abandoned);
        }
        tx.commit()?;
        Ok(id)
    }

    /// Mark a terminal closed. The row stays: what was open and when is history, and
    /// deleting it would make a restart unable to say why a workspace looks different.
    pub fn close_terminal(&self, id: i64, now: i64) -> Result<bool, StoreError> {
        let n = self.conn.execute(
            "UPDATE terminal SET closed_at = ?2 WHERE id = ?1 AND closed_at IS NULL",
            params![id, now],
        )?;
        Ok(n == 1)
    }

    /// Every stored split tree, oldest first, as the JSON it was written as.
    pub fn splits(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT tree FROM split_tree ORDER BY rowid")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Replace every stored split tree. One transaction, so a crash in the middle leaves
    /// the old set rather than half of the new one.
    pub fn set_splits(&mut self, trees: &[String]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM split_tree", [])?;
        for tree in trees {
            tx.execute("INSERT INTO split_tree (tree) VALUES (?1)", params![tree])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every conversation a person named, as `(session, title)`.
    pub fn session_titles(&self) -> Result<Vec<(String, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT session, title FROM session_title")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Name a conversation, or with `None` give it back its agent's own title.
    pub fn set_session_title(&self, session: &str, title: Option<&str>) -> Result<(), StoreError> {
        match title {
            Some(title) => self.conn.execute(
                "INSERT INTO session_title (session, title) VALUES (?1, ?2) \
                 ON CONFLICT(session) DO UPDATE SET title = excluded.title",
                params![session, title],
            )?,
            None => self.conn.execute(
                "DELETE FROM session_title WHERE session = ?1",
                params![session],
            )?,
        };
        Ok(())
    }

    /// Move an open terminal to another workspace, at the end of its tabs. One
    /// transaction for the row and its place, so a crash leaves it where it was.
    /// Answers whether a row moved.
    pub fn move_terminal(&mut self, id: i64, workspace: i64) -> Result<bool, StoreError> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE terminal SET workspace = ?2 WHERE id = ?1 AND closed_at IS NULL",
            params![id, workspace],
        )?;
        if n == 1 {
            tx.execute("DELETE FROM layout WHERE terminal = ?1", params![id])?;
            tx.execute(
                "INSERT INTO layout (workspace, terminal, ordinal) VALUES (?2, ?1, \
                 (SELECT COALESCE(MAX(ordinal), -1) + 1 FROM layout WHERE workspace = ?2))",
                params![id, workspace],
            )?;
        }
        tx.commit()?;
        Ok(n == 1)
    }

    /// The highest terminal id ever written, open or closed, or 0 for none.
    ///
    /// What a restart mints above: a closed row keeps its id as the primary key, so
    /// minting above the open rows alone hands out an id that already exists.
    pub fn max_terminal_id(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM terminal", [], |r| {
                r.get(0)
            })?)
    }

    /// The open terminals of a workspace, in layout order.
    pub fn open_terminals(&self, workspace: i64) -> Result<Vec<TerminalRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.workspace, t.title, t.cwd, t.argv, t.agent, t.agent_session,
                    t.created_at, t.closed_at
             FROM terminal t JOIN layout l ON l.terminal = t.id
             WHERE t.workspace = ?1 AND t.closed_at IS NULL
             ORDER BY l.ordinal",
        )?;
        let rows = stmt
            .query_map(params![workspace], |r| {
                Ok(TerminalRow {
                    id: r.get(0)?,
                    workspace: r.get(1)?,
                    title: r.get(2)?,
                    cwd: r.get(3)?,
                    argv: r.get(4)?,
                    agent: r.get(5)?,
                    agent_session: r.get(6)?,
                    created_at: r.get(7)?,
                    closed_at: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn set_title(&self, id: i64, title: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE terminal SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        Ok(())
    }

    /// Forget the name a person gave a terminal, so a cleared tab comes back
    /// cleared rather than under a name that no longer exists on screen.
    pub fn clear_title(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE terminal SET title = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Record where a terminal actually is.
    ///
    /// Not where it was started. A person opens a tab, moves into a worktree and starts an
    /// agent there, and a restore using the directory the tab was born in would put that
    /// agent back in the wrong project.
    pub fn set_cwd(&self, id: i64, cwd: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE terminal SET cwd = ?2 WHERE id = ?1",
            params![id, cwd],
        )?;
        Ok(())
    }

    /// Record an agent's own id for the conversation in a terminal, without touching which
    /// agent it is.
    ///
    /// Two writers, two questions. What is running is decided by watching the process
    /// table; the id can only come from the agent itself, through its own hook, and an id
    /// arriving must not overwrite a name the other writer knows better.
    pub fn set_agent_session(&self, id: i64, session: &str) -> Result<(), StoreError> {
        // Never into a sleeping space: its rows say what to bring back, and a late write
        // from the teardown that put it to sleep would say something else. Checked here,
        // when the write runs, not when it was asked for, because a write whose caller
        // gave up waiting still runs.
        self.conn.execute(
            "UPDATE terminal SET agent_session = ?2 WHERE id = ?1 \
             AND workspace NOT IN (SELECT workspace FROM asleep)",
            params![id, session],
        )?;
        Ok(())
    }

    /// Record which agent is in a terminal, and its own id for the conversation when it
    /// gave us one.
    ///
    /// `None` for the agent means there is no longer one there, and that is written down:
    /// a person who left their agent left it, and a restart that started it again would be
    /// undoing something they did on purpose.
    ///
    /// The session id is the other way round. It is only ever written, never cleared by a
    /// `None`, because an agent that stops reporting its id has not ended the conversation,
    /// and forgetting the id would turn an exact resume into a guess.
    pub fn set_agent(
        &self,
        id: i64,
        agent: Option<&str>,
        agent_session: Option<&str>,
    ) -> Result<(), StoreError> {
        // Not into a sleeping space, for the reason `set_agent_session` gives: an agent
        // ending under the sleep is announced as gone, and writing that down would bring
        // the tab back as a bare shell.
        self.conn.execute(
            "UPDATE terminal SET agent = ?2, agent_session = COALESCE(?3, agent_session) \
             WHERE id = ?1 AND workspace NOT IN (SELECT workspace FROM asleep)",
            params![id, agent, agent_session],
        )?;
        Ok(())
    }
}

/// What a sleeping terminal's row has to say when its space goes to sleep: which agent
/// was in it and that agent's own id for the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub terminal: i64,
    pub agent: Option<String>,
    pub agent_session: Option<String>,
}

impl Store {
    /// Every workspace that is asleep, and since when, the latest first.
    pub fn asleep(&self) -> Result<Vec<(i64, i64)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT workspace, since FROM asleep ORDER BY since DESC, workspace DESC")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Put a workspace to sleep: its terminals' agents written down, then the mark, in one
    /// transaction. A failure leaves neither, so a space is never marked asleep with rows
    /// that do not say what to bring back.
    pub fn sleep_workspace(
        &mut self,
        workspace: i64,
        since: i64,
        checkpoints: &[Checkpoint],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for c in checkpoints {
            tx.execute(
                "UPDATE terminal SET agent = ?2, agent_session = COALESCE(?3, agent_session) \
                 WHERE id = ?1",
                params![c.terminal, c.agent, c.agent_session],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO asleep (workspace, since) VALUES (?1, ?2)",
            params![workspace, since],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Take the sleep mark off a workspace. Answers whether it had one.
    pub fn wake_workspace(&self, workspace: i64) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "DELETE FROM asleep WHERE workspace = ?1",
            params![workspace],
        )? == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory().expect("open")
    }

    #[test]
    fn a_workspace_and_its_terminals_round_trip() {
        let mut s = store();
        let ws = s.create_workspace("work", 100).unwrap();
        let a = s.create_terminal(1, ws, "/tmp", "/bin/sh", 101).unwrap();
        let b = s.create_terminal(2, ws, "/var", "/bin/sh", 102).unwrap();

        let open = s.open_terminals(ws).unwrap();
        assert_eq!(open.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(open[0].cwd, "/tmp");
    }

    #[test]
    fn closing_a_terminal_hides_it_without_losing_it() {
        let mut s = store();
        let ws = s.create_workspace("work", 100).unwrap();
        let a = s.create_terminal(1, ws, "/tmp", "/bin/sh", 101).unwrap();
        assert!(s.close_terminal(a, 200).unwrap());
        assert!(s.open_terminals(ws).unwrap().is_empty());
        // Closing twice is not a second close, which is what makes a retry safe.
        assert!(!s.close_terminal(a, 300).unwrap());
        // And its id is still taken, which is what a restart has to mint above.
        assert_eq!(s.max_terminal_id().unwrap(), a);
    }

    #[test]
    fn a_terminal_in_a_workspace_that_does_not_exist_is_refused_whole() {
        let mut s = store();
        let before = s.workspaces().unwrap().len();
        assert!(matches!(
            s.create_terminal(1, 999, "/tmp", "/bin/sh", 1),
            Err(StoreError::NoWorkspace(999))
        ));
        // And nothing was left behind by the failed transaction.
        assert_eq!(s.workspaces().unwrap().len(), before);
        let ws = s.create_workspace("w", 1).unwrap();
        assert!(s.open_terminals(ws).unwrap().is_empty());
    }

    #[test]
    fn metadata_survives_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qoro.db");
        let ws;
        let t;
        {
            let mut s = Store::open(&path).unwrap();
            ws = s.create_workspace("persisted", 1).unwrap();
            t = s.create_terminal(7, ws, "/tmp", "/bin/sh", 2).unwrap();
        }
        let s = Store::open(&path).unwrap();
        let open = s.open_terminals(ws).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, t);
    }

    #[test]
    fn layout_order_is_creation_order_and_survives_a_close() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).unwrap();
        let a = s.create_terminal(11, ws, "/a", "/bin/sh", 1).unwrap();
        let b = s.create_terminal(12, ws, "/b", "/bin/sh", 2).unwrap();
        let c = s.create_terminal(13, ws, "/c", "/bin/sh", 3).unwrap();
        s.close_terminal(b, 4).unwrap();
        assert_eq!(
            s.open_terminals(ws)
                .unwrap()
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>(),
            vec![a, c],
            "closing the middle terminal reordered the others"
        );
    }

    #[test]
    fn split_trees_are_replaced_as_a_set_and_survive_a_reopen() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("qoro.db");
        {
            let mut s = Store::open(&path).unwrap();
            s.set_splits(&["a".into(), "b".into()]).unwrap();
            s.set_splits(&["c".into(), "a".into()]).unwrap();
        }
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.splits().unwrap(), vec!["c".to_string(), "a".to_string()]);
        s.set_splits(&[]).unwrap();
        assert!(s.splits().unwrap().is_empty());
    }

    /// Splits and the ledger must not cost an older binary the file: the tables are new, the
    /// version is not. A bump makes every older build refuse the file, and the error it
    /// prints tells the person to move it aside, which is deleting their workspaces.
    #[test]
    fn new_tables_do_not_bump_the_schema_version() {
        assert_eq!(
            SCHEMA_VERSION, 2,
            "a new table must be created unconditionally, never behind a version bump"
        );
    }

    /// The upgrade this change actually ships: a version 2 database with no `applied` table.
    ///
    /// Written by hand rather than by a constructor, because the old constructor is exactly
    /// what changed and using it would test the new code against itself. This is the file
    /// every existing install has.
    #[test]
    fn a_version_two_database_gains_the_ledger_table_on_open() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("qoro.db");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE workspace (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE terminal (
                     id INTEGER PRIMARY KEY,
                     workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                     title TEXT,
                     cwd TEXT NOT NULL,
                     argv TEXT NOT NULL,
                     agent TEXT,
                     agent_session TEXT,
                     created_at INTEGER NOT NULL,
                     closed_at INTEGER
                 );
                 CREATE TABLE layout (
                     workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                     terminal INTEGER NOT NULL REFERENCES terminal(id) ON DELETE CASCADE,
                     ordinal INTEGER NOT NULL,
                     PRIMARY KEY (workspace, terminal)
                 );
                 CREATE TABLE split_tree (tree TEXT NOT NULL);
                 INSERT INTO workspace (id, name, created_at) VALUES (1, 'app', 100);
                 INSERT INTO terminal (id, workspace, cwd, argv, created_at)
                      VALUES (1, 1, '/tmp', '/bin/sh', 100);
                 INSERT INTO layout (workspace, terminal, ordinal) VALUES (1, 1, 0);",
            )
            .expect("a version 2 database, without the ledger");
            conn.pragma_update(None, "user_version", 2i64)
                .expect("stamp");
        }

        let s = Store::open(&path).expect("an existing database must open, not be refused");
        assert_eq!(s.applied("k1").expect("the table exists"), None);
        s.record_applied("k1", true, &serde_json::json!({ "terminal": 1 }), 200)
            .expect("record");
        // The person's own state came through untouched, and the row reads back.
        assert_eq!(s.workspaces().expect("workspaces")[0].name, "app");
        assert_eq!(s.open_terminals(1).expect("terminals").len(), 1);
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 1 })))
        );
    }

    /// A retry returns the answer the first attempt produced.
    #[test]
    fn a_replayed_command_returns_the_original_answer() {
        let s = Store::in_memory().expect("a store");
        assert_eq!(s.applied("k1").expect("lookup"), None);
        s.record_applied("k1", true, &serde_json::json!({ "terminal": 7 }), 100)
            .expect("record");
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 7 })))
        );
    }

    /// A row this build cannot read is an error, not an absence.
    ///
    /// The distinction is the whole reason `Outcome` is its own variant: a caller that
    /// cannot tell a broken row from a missing one cannot reason about which it has. What
    /// the daemon then does with that error is its own decision, and ADR-005's rule 1 makes
    /// it. This test only pins that the two are not the same answer.
    #[test]
    fn a_row_this_build_cannot_read_is_an_error_rather_than_an_absence() {
        let s = Store::in_memory().expect("a store");
        s.conn
            .execute(
                "INSERT INTO applied (idempotency_key, outcome, at_unix) VALUES ('k1', 'not json', 100)",
                [],
            )
            .expect("write a row another build left");

        let answer = s.applied("k1");
        assert!(
            matches!(answer, Err(StoreError::Outcome(_))),
            "a row that cannot be read was answered as: {answer:?}"
        );
    }

    /// A create and its answer land together: the terminal row, its layout place and the
    /// ledger row, in one transaction.
    #[test]
    fn a_create_writes_its_answer_in_the_same_transaction() {
        let mut s = Store::in_memory().expect("a store");
        let ws = s.create_workspace("w", 1).expect("workspace");
        s.create_terminal_answering(
            Some("k1"),
            7,
            ws,
            "/tmp",
            "/bin/sh",
            Some("build"),
            None,
            100,
        )
        .expect("create");
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 7 })))
        );
        let rows = s.open_terminals(ws).expect("terminals");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title.as_deref(), Some("build"));
    }

    /// A second create under an answered key fails whole: no second terminal row, and the
    /// first answer still stands. This is what stops two attempts at one create, racing on
    /// two connections, from both opening a tab.
    #[test]
    fn a_create_under_an_answered_key_makes_nothing() {
        let mut s = Store::in_memory().expect("a store");
        let ws = s.create_workspace("w", 1).expect("workspace");
        s.create_terminal_answering(Some("k1"), 7, ws, "/tmp", "/bin/sh", None, None, 100)
            .expect("create");
        assert!(
            s.create_terminal_answering(Some("k1"), 8, ws, "/tmp", "/bin/sh", None, None, 200)
                .is_err(),
            "a second create under the same key was written"
        );
        assert_eq!(s.open_terminals(ws).expect("terminals").len(), 1);
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 7 })))
        );
    }

    /// A create whose caller stopped waiting before the commit leaves nothing: no row, no
    /// answer. This is what keeps a write stalled past its deadline from landing after the
    /// daemon has told the client it failed.
    #[test]
    fn an_abandoned_write_rolls_back_before_its_commit() {
        let mut s = Store::in_memory().expect("a store");
        let ws = s.create_workspace("w", 1).expect("workspace");
        let gone = std::sync::atomic::AtomicBool::new(true);
        assert!(matches!(
            s.create_terminal_answering(
                Some("k1"),
                7,
                ws,
                "/tmp",
                "/bin/sh",
                None,
                Some(&gone),
                100
            ),
            Err(StoreError::Abandoned)
        ));
        assert_eq!(s.applied("k1").expect("lookup"), None);
        assert!(s.open_terminals(ws).expect("terminals").is_empty());
    }

    /// A create whose shell never started is undone whole: no open row to come back at
    /// the next start, and no answer, so a retry tries again.
    #[test]
    fn an_abandoned_create_leaves_no_tab_and_no_answer() {
        let mut s = Store::in_memory().expect("a store");
        let ws = s.create_workspace("w", 1).expect("workspace");
        s.create_terminal_answering(Some("k1"), 7, ws, "/tmp", "/bin/sh", None, None, 100)
            .expect("create");
        s.abandon_create("k1", 7, 200).expect("abandon");
        assert_eq!(s.applied("k1").expect("lookup"), None);
        assert!(s.open_terminals(ws).expect("terminals").is_empty());
    }

    /// Abandoning takes back only an answer that names this terminal: a key another
    /// attempt answered stands.
    #[test]
    fn abandoning_keeps_an_answer_that_names_another_terminal() {
        let mut s = Store::in_memory().expect("a store");
        let ws = s.create_workspace("w", 1).expect("workspace");
        s.create_terminal_answering(Some("k1"), 7, ws, "/tmp", "/bin/sh", None, None, 100)
            .expect("create");
        s.abandon_create("k1", 8, 200).expect("abandon");
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 7 })))
        );
        assert_eq!(s.open_terminals(ws).expect("terminals").len(), 1);
    }

    /// The first answer is the one that stands.
    ///
    /// A command that answers twice has already done the work twice by the time the second
    /// write lands, so the second answer describes a state the first caller never saw.
    #[test]
    fn a_second_write_to_the_same_key_does_not_overwrite_the_first() {
        let s = Store::in_memory().expect("a store");
        s.record_applied("k1", true, &serde_json::json!({ "terminal": 7 }), 100)
            .expect("record");
        s.record_applied("k1", true, &serde_json::json!({ "terminal": 9 }), 200)
            .expect("record again");
        assert_eq!(
            s.applied("k1").expect("lookup"),
            Some((true, serde_json::json!({ "terminal": 7 }))),
            "the retry replaced the answer the first caller received"
        );
    }

    /// A recorded failure replays as a failure, not as a success.
    ///
    /// Neither call site records one today: both write `true`, so the flag exists for the
    /// first command recorded before it knows how it will end. The alternative is to drop
    /// the flag and answer a retry of a failed command with a success, which is the worse
    /// error. Pinned here so the encoding is known to carry it.
    #[test]
    fn a_recorded_failure_replays_as_a_failure() {
        let s = Store::in_memory().expect("a store");
        s.record_applied(
            "k1",
            false,
            &serde_json::json!({ "reason": "no space" }),
            100,
        )
        .expect("record");
        let (ok, detail) = s.applied("k1").expect("lookup").expect("recorded");
        assert!(!ok, "a failure replayed as a success");
        assert_eq!(detail["reason"], "no space");
    }

    /// The ledger is the one table here that would otherwise grow forever.
    #[test]
    fn pruning_the_ledger_keeps_what_a_retry_can_still_ask_about() {
        let s = Store::in_memory().expect("a store");
        s.record_applied("old", true, &serde_json::json!({}), 100)
            .expect("record");
        s.record_applied("new", true, &serde_json::json!({}), 9_000)
            .expect("record");
        assert_eq!(s.prune_applied(1_000).expect("prune"), 1);
        assert_eq!(s.applied("old").expect("lookup"), None);
        assert!(
            s.applied("new").expect("lookup").is_some(),
            "pruning reached past the cutoff"
        );
    }

    /// The ledger survives a reopen, which is the whole reason it is in this file.
    #[test]
    fn the_ledger_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("qoro.db");
        {
            let s = Store::open(&path).expect("open");
            s.record_applied("k1", true, &serde_json::json!({ "terminal": 7 }), 100)
                .expect("record");
        }
        let s = Store::open(&path).expect("reopen");
        assert!(s.applied("k1").expect("lookup").is_some());
    }

    /// A database written by the build before sessions existed, opened by this build.
    ///
    /// The check this replaces refused it outright, and refusing is not a migration: the
    /// only way past it was to delete the person's workspaces. Written here as the real
    /// version 1 schema rather than by calling an old constructor, because the old
    /// constructor is what changed.
    #[test]
    fn a_database_from_the_previous_schema_is_migrated_rather_than_refused() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("old.db");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE workspace (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     created_at INTEGER NOT NULL
                 );
                 CREATE TABLE terminal (
                     id INTEGER PRIMARY KEY,
                     workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                     title TEXT,
                     cwd TEXT NOT NULL,
                     argv TEXT NOT NULL,
                     created_at INTEGER NOT NULL,
                     closed_at INTEGER
                 );
                 CREATE TABLE layout (
                     workspace INTEGER NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
                     terminal INTEGER NOT NULL REFERENCES terminal(id) ON DELETE CASCADE,
                     ordinal INTEGER NOT NULL,
                     PRIMARY KEY (workspace, terminal)
                 );
                 INSERT INTO workspace (id, name, created_at) VALUES (1, 'app', 10);
                 INSERT INTO terminal (id, workspace, cwd, argv, created_at)
                     VALUES (5, 1, '/tmp', '/bin/sh', 11);
                 INSERT INTO layout (workspace, terminal, ordinal) VALUES (1, 5, 0);",
            )
            .expect("write the old schema");
            conn.pragma_update(None, "user_version", 1i64)
                .expect("stamp");
        }

        let store = Store::open(&path).expect("an old database must open, not be refused");
        assert!(
            store.splits().expect("the split table exists").is_empty(),
            "an old database gains the split table empty"
        );
        let spaces = store.workspaces().expect("workspaces");
        assert_eq!(spaces.len(), 1, "the workspace survived");
        assert_eq!(spaces[0].name, "app");
        let open = store.open_terminals(1).expect("terminals");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].cwd, "/tmp");
        assert_eq!(open[0].agent, None);

        // And it is a version 2 database afterwards, so the next open does no work.
        drop(store);
        Store::open(&path).expect("reopen");
    }

    /// A migration that fails halfway leaves nothing behind: the ALTERs and the version
    /// stamp are one transaction, so a kill between two of them cannot leave a version 1
    /// database with one new column that every later open trips over.
    #[test]
    fn a_migration_that_fails_halfway_changes_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("half.db");
        {
            let conn = Connection::open(&path).expect("open");
            // A version 1 terminal table that already has the second column, so the second
            // ALTER fails after the first one ran.
            conn.execute_batch(
                "CREATE TABLE terminal (
                     id INTEGER PRIMARY KEY,
                     workspace INTEGER NOT NULL,
                     title TEXT,
                     cwd TEXT NOT NULL,
                     argv TEXT NOT NULL,
                     agent_session TEXT,
                     created_at INTEGER NOT NULL,
                     closed_at INTEGER
                 );",
            )
            .expect("write the schema");
            conn.pragma_update(None, "user_version", 1i64)
                .expect("stamp");
        }
        assert!(Store::open(&path).is_err(), "the second ALTER must fail");

        let conn = Connection::open(&path).expect("reopen");
        let has_agent: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('terminal') WHERE name = 'agent'",
                [],
                |r| r.get(0),
            )
            .expect("columns");
        assert_eq!(
            has_agent, 0,
            "the first ALTER outlived the failed migration"
        );
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("future.db");
        {
            let conn = Connection::open(&path).expect("open");
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .expect("stamp");
        }
        let Err(err) = Store::open(&path) else {
            panic!("a newer schema is not something to guess at");
        };
        assert!(
            matches!(err, StoreError::FromTheFuture { .. }),
            "the refusal has to say what it is: {err}"
        );
        assert!(
            err.to_string().contains("newer qoro"),
            "and say it to a person: {err}"
        );
    }

    #[test]
    fn a_listing_reads_what_a_session_was_holding_without_a_daemon() {
        let mut s = store();
        let ws = s.create_workspace("app", 1).expect("space");
        assert_eq!(s.open_terminal_count().expect("count"), 0);
        s.create_terminal(1, ws, "/a", "/bin/sh", 1).expect("t1");
        s.create_terminal(2, ws, "/b", "/bin/sh", 1).expect("t2");
        assert_eq!(s.open_terminal_count().expect("count"), 2);
        s.close_terminal(1, 9).expect("close");
        assert_eq!(
            s.open_terminal_count().expect("count"),
            1,
            "a tab the person closed is not one waiting to come back"
        );
    }

    #[test]
    fn an_agent_and_its_own_session_id_survive_and_the_id_is_never_forgotten() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("space");
        s.create_terminal(4, ws, "/tmp", "/bin/sh", 1).expect("t");

        s.set_agent(4, Some("claude"), None)
            .expect("first sighting");
        assert_eq!(
            s.open_terminals(ws).expect("list")[0].agent.as_deref(),
            Some("claude")
        );
        assert_eq!(s.open_terminals(ws).expect("list")[0].agent_session, None);

        s.set_agent(4, Some("claude"), Some("abc-123"))
            .expect("it told us its id");
        assert_eq!(
            s.open_terminals(ws).expect("list")[0]
                .agent_session
                .as_deref(),
            Some("abc-123")
        );

        // A later sighting with no id in hand must not throw the id away: the conversation
        // did not end, the agent simply did not repeat itself.
        s.set_agent(4, Some("claude"), None)
            .expect("a later sighting");
        assert_eq!(
            s.open_terminals(ws).expect("list")[0]
                .agent_session
                .as_deref(),
            Some("abc-123"),
            "an exact resume was turned into a guess"
        );

        // And leaving the agent is recorded, because a person who quit their agent quit it.
        s.set_agent(4, None, None).expect("it is gone");
        assert_eq!(s.open_terminals(ws).expect("list")[0].agent, None);
        assert_eq!(
            s.open_terminals(ws).expect("list")[0]
                .agent_session
                .as_deref(),
            Some("abc-123"),
            "the conversation it had is still the one to carry on if it comes back"
        );
    }

    #[test]
    fn a_workspace_sleeps_with_its_agents_written_down_and_wakes() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(7, Some("claude"), Some("abc")).expect("agent");
        s.sleep_workspace(
            ws,
            10,
            &[Checkpoint {
                terminal: 7,
                agent: Some("claude".into()),
                agent_session: None,
            }],
        )
        .expect("sleep");
        assert_eq!(s.asleep().expect("asleep"), vec![(ws, 10)]);
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent.as_deref(), Some("claude"));
        // A checkpoint with no id keeps the one the hook reported.
        assert_eq!(row.agent_session.as_deref(), Some("abc"));
        assert!(s.wake_workspace(ws).expect("wake"));
        assert!(!s.wake_workspace(ws).expect("wake again"));
        assert!(s.asleep().expect("asleep").is_empty());
    }

    #[test]
    fn deleting_a_workspace_takes_its_sleep_mark_with_it() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.sleep_workspace(ws, 5, &[]).expect("sleep");
        s.delete_workspace(ws).expect("delete");
        assert!(s.asleep().expect("asleep").is_empty());
    }

    #[test]
    fn a_sleeping_space_refuses_late_agent_writes() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(3, Some("claude"), Some("abc")).expect("agent");
        s.sleep_workspace(ws, 2, &[]).expect("sleep");
        s.set_agent(3, None, None).expect("a late write runs");
        s.set_agent_session(3, "other").expect("a late id runs");
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent.as_deref(), Some("claude"));
        assert_eq!(row.agent_session.as_deref(), Some("abc"));
        s.wake_workspace(ws).expect("wake");
        s.set_agent(3, None, None).expect("an awake write");
        assert_eq!(s.open_terminals(ws).expect("rows")[0].agent, None);
    }
}

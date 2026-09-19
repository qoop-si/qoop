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
    // A fresh database has every column already, because the CREATE statements above are
    // the current shape. Only a database that predates a column needs the ALTER.
    if found != 0 && found < 2 {
        for statement in [
            "ALTER TABLE terminal ADD COLUMN agent TEXT",
            "ALTER TABLE terminal ADD COLUMN agent_session TEXT",
        ] {
            conn.execute(statement, [])?;
        }
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

impl Store {
    /// Open, creating and migrating as needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::prepare(conn)
    }

    /// An in-memory store, for tests and for a daemon told not to persist.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
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
             CREATE INDEX IF NOT EXISTS terminal_by_workspace ON terminal(workspace);",
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
            "INSERT INTO terminal (id, workspace, cwd, argv, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, workspace, cwd, argv, now],
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
        self.conn.execute(
            "UPDATE terminal SET agent_session = ?2 WHERE id = ?1",
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
        self.conn.execute(
            "UPDATE terminal SET agent = ?2, agent_session = COALESCE(?3, agent_session) \
             WHERE id = ?1",
            params![id, agent, agent_session],
        )?;
        Ok(())
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
}

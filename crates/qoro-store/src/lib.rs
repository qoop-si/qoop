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
}

/// The schema version this build writes and expects.
///
/// Checked on open rather than assumed. A daemon that silently runs against a newer
/// schema is how a downgrade eats data.
pub const SCHEMA_VERSION: i64 = 1;

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
    pub created_at: i64,
    pub closed_at: Option<i64>,
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
        if found == 0 {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if found != SCHEMA_VERSION {
            return Err(StoreError::Sqlite(rusqlite::Error::InvalidQuery));
        }
        Ok(Self { conn })
    }

    pub fn create_workspace(&self, name: &str, now: i64) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO workspace (name, created_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(self.conn.last_insert_rowid())
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

    /// Record a terminal and place it at the end of its workspace's layout.
    ///
    /// One transaction, because a terminal with no layout row is a terminal nobody can see
    /// and a layout row with no terminal is a dangling reference. Either both land or
    /// neither does.
    pub fn create_terminal(
        &mut self,
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
            "INSERT INTO terminal (workspace, cwd, argv, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![workspace, cwd, argv, now],
        )?;
        let id = tx.last_insert_rowid();
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
            "SELECT t.id, t.workspace, t.title, t.cwd, t.argv, t.created_at, t.closed_at
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
                    created_at: r.get(5)?,
                    closed_at: r.get(6)?,
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
        let a = s.create_terminal(ws, "/tmp", "/bin/sh", 101).unwrap();
        let b = s.create_terminal(ws, "/var", "/bin/sh", 102).unwrap();

        let open = s.open_terminals(ws).unwrap();
        assert_eq!(open.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(open[0].cwd, "/tmp");
    }

    #[test]
    fn closing_a_terminal_hides_it_without_losing_it() {
        let mut s = store();
        let ws = s.create_workspace("work", 100).unwrap();
        let a = s.create_terminal(ws, "/tmp", "/bin/sh", 101).unwrap();
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
            s.create_terminal(999, "/tmp", "/bin/sh", 1),
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
            t = s.create_terminal(ws, "/tmp", "/bin/sh", 2).unwrap();
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
        let a = s.create_terminal(ws, "/a", "/bin/sh", 1).unwrap();
        let b = s.create_terminal(ws, "/b", "/bin/sh", 2).unwrap();
        let c = s.create_terminal(ws, "/c", "/bin/sh", 3).unwrap();
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
}

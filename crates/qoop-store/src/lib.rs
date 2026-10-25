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

pub mod ask;
pub mod history;
pub mod item;
pub mod schedule;
pub mod show;
pub mod task;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub use ask::{Added, AskError, AskRow};
pub use history::History;
pub use item::{ItemAgent, ItemCard, ItemRow, ItemState, StepRow};
use rusqlite::{Connection, OptionalExtension, params};
pub use schedule::{ScheduleError, ScheduleRow};
pub use show::ShowRow;
pub use task::{Outcome, TaskError, TaskEventRow, TaskRow};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("no workspace {0}")]
    NoWorkspace(i64),
    /// A task write refused for what it asked, not for the database: the message is the
    /// answer to give.
    #[error("{0}")]
    Task(#[from] task::TaskError),
    /// An ask write refused for what it asked: the message is the answer to give.
    #[error("{0}")]
    Ask(#[from] ask::AskError),
    /// A schedule write refused for what it asked: the message is the answer to give.
    #[error("{0}")]
    Schedule(#[from] schedule::ScheduleError),
    #[error(
        "this database was written by a newer qoop (schema {found}, this build understands \
         {understood}); use that build or move the file aside"
    )]
    FromTheFuture { found: i64, understood: i64 },
    /// The caller stopped waiting before the write was committed, so it was rolled back
    /// rather than landing after the caller had already answered.
    #[error("abandoned by its caller before it was committed")]
    Abandoned,
    /// A file a reader was pointed at that holds no qoop database: a reader never makes one.
    #[error("{} holds no qoop database", .0.display())]
    NotADatabase(std::path::PathBuf),
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
    /// The colour a person picked for it, one of the six space inks. `None` is the
    /// colour its id gives it.
    pub ink: Option<u8>,
    /// What the owner wants from the work in this space, one line, when he said.
    pub goal: Option<String>,
    /// Where a new tab in it starts when nobody names a directory (`space_dir`).
    pub dir: Option<String>,
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
    // Written only when it changes: every open writing it made a read-only `qoop ls` a
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

/// Turn each space a former daemon put to sleep whole into marks on its open terminals,
/// once. Read before anything is written, so opening a store with nothing to turn (every
/// store after the first open) never takes the write lock; the rows are
/// read again under it, so two openers at once turn them once.
fn mark_asleep_spaces(conn: &Connection) -> Result<(), StoreError> {
    let pending: bool = conn.query_row("SELECT EXISTS (SELECT 1 FROM asleep)", [], |r| r.get(0))?;
    if !pending {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR IGNORE INTO asleep_terminal (terminal, since) \
         SELECT t.id, a.since FROM asleep a JOIN terminal t ON t.workspace = a.workspace \
         WHERE t.closed_at IS NULL",
        [],
    )?;
    tx.execute("DELETE FROM asleep", [])?;
    tx.commit()?;
    Ok(())
}

impl Store {
    /// Open, creating and migrating as needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        let store = Self::prepare(conn)?;
        // Turned by the opener that writes, the daemon, and never by a reader: a `qoop ls`
        // beside an older daemon that still sleeps whole spaces must not take its marks.
        mark_asleep_spaces(&store.conn)?;
        Ok(store)
    }

    /// The same, for a file that must already exist: a reader that finds nothing gets an
    /// error rather than an empty database left behind in its place. A file with no qoop
    /// schema is nothing too: a reader that built the schema into one would make a stray file
    /// (what a bare `sqlite3 <path>` leaves) look like a real database, which every later
    /// reader and daemon start would then prefer.
    pub fn open_existing(path: &Path) -> Result<Self, StoreError> {
        if !Self::holds_schema(path) {
            return Err(StoreError::NotADatabase(path.to_path_buf()));
        }
        let flags = rusqlite::OpenFlags::default() - rusqlite::OpenFlags::SQLITE_OPEN_CREATE;
        Self::prepare(Connection::open_with_flags(path, flags)?)
    }

    /// Whether the file at `path` is a qoop database: it has the schema a daemon wrote. Read
    /// only, so asking never makes one; a missing, empty or foreign file is not.
    pub fn holds_schema(path: &Path) -> bool {
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| {
                // A lock is a database being written, not a missing one: wait as `prepare`
                // does rather than answer "no store" and send the reader elsewhere.
                c.busy_timeout(std::time::Duration::from_secs(5))?;
                c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'workspace')",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap_or(false)
    }

    /// Whether this build could open the database at `path`, found out by reading and
    /// nothing else: no table is made, no migration run, nothing written. For the dry run a
    /// daemon makes of the binary it is about to become, while it still serves from the same
    /// file.
    pub fn check_openable(path: &Path) -> Result<(), StoreError> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::FromTheFuture {
                found,
                understood: SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// An in-memory store, for tests and for a daemon told not to persist.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        // Wait for a lock the daemon holds rather than failing at once, before anything
        // below can need one: a `qoop ls` landing on a daemon write printed "0 tabs".
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
             -- The terminals that are asleep, which is what sleeps now: a tab, not a whole
             -- space. The rows in `asleep` above are from before, and are turned into
             -- these once (`mark_asleep_spaces`). New table, no version bump, for the
             -- reason the ones above give.
             CREATE TABLE IF NOT EXISTS asleep_terminal (
                 terminal INTEGER PRIMARY KEY REFERENCES terminal(id) ON DELETE CASCADE,
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
             );
             -- The colour a person picked for a space. Only a picked one has a row; the
             -- rest take the colour their id gives them. Cascaded with the space, because
             -- ids are reused and a new space must not inherit an old one's colour. New
             -- table, no version bump, for the reason the ones above give.
             CREATE TABLE IF NOT EXISTS workspace_ink (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE,
                 ink INTEGER NOT NULL
             );
             -- The spaces and the panes a person asked to keep awake: idle sleep passes
             -- them by. Cascaded like the rest, because ids are reused. New tables, no
             -- version bump, for the reason the ones above give: an older binary ignores
             -- them and sleeps an idle tab, which loses nothing.
             CREATE TABLE IF NOT EXISTS awake_workspace (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS awake_terminal (
                 terminal INTEGER PRIMARY KEY REFERENCES terminal(id) ON DELETE CASCADE
             );
             -- The spaces and the panes a person asked qass to leave alone. Cascaded like the
             -- rest, because ids are reused. New tables, no version bump: an older binary
             -- ignores them and qass watches the tab as before.
             CREATE TABLE IF NOT EXISTS qass_skip_workspace (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS qass_skip_terminal (
                 terminal INTEGER PRIMARY KEY REFERENCES terminal(id) ON DELETE CASCADE
             );
             -- What the owner wants from the work in a space, one line. Cascaded like the
             -- rest, because ids are reused. New table, no version bump: an older binary
             -- ignores it.
             -- The agents a person parked to answer later, and when. Cascaded like the
             -- rest, because ids are reused. New table, no version bump: an older binary
             -- ignores it and shows the agent as it is.
             CREATE TABLE IF NOT EXISTS parked_terminal (
                 terminal INTEGER PRIMARY KEY REFERENCES terminal(id) ON DELETE CASCADE,
                 at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS space_goal (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE,
                 goal TEXT NOT NULL,
                 updated_at INTEGER NOT NULL
             );
             -- Where a new tab in a space starts when nobody names a directory: its home,
             -- the space's own folder, never where some tab of it moved to since. Cascaded
             -- like the rest, because ids are reused. New table, no version bump: an older
             -- binary ignores it and opens a tab where it did before.
             CREATE TABLE IF NOT EXISTS space_dir (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE,
                 dir TEXT NOT NULL
             );
             -- Where a person put each space in the list. A space never placed has no row
             -- and comes after the placed ones, by id, so a new one lands at the end.
             -- Cascaded like the rest, because ids are reused. New table, no version bump:
             -- an older binary ignores it and lists the spaces by id.
             CREATE TABLE IF NOT EXISTS workspace_order (
                 workspace INTEGER PRIMARY KEY REFERENCES workspace(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL
             );
             -- The work an agent said it is doing (`task.rs`). `terminal` is not a foreign
             -- key: terminal ids are reused, so every path that ends a terminal closes its
             -- open task instead (one claimed done stays open, untied, for its check), and a
             -- closed task keeps its history with `terminal` NULL.
             -- New tables, no version bump: an older binary ignores them.
             CREATE TABLE IF NOT EXISTS task (
                 id INTEGER PRIMARY KEY,
                 workspace INTEGER REFERENCES workspace(id) ON DELETE SET NULL,
                 terminal INTEGER,
                 goal TEXT NOT NULL,
                 stage TEXT NOT NULL,
                 opened_at INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL,
                 claimed_at INTEGER,
                 closed_at INTEGER,
                 outcome TEXT
             );
             CREATE UNIQUE INDEX IF NOT EXISTS task_one_open_per_terminal
                 ON task(terminal) WHERE closed_at IS NULL;
             CREATE TABLE IF NOT EXISTS task_event (
                 id INTEGER PRIMARY KEY,
                 task INTEGER NOT NULL REFERENCES task(id) ON DELETE CASCADE,
                 at INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 body TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS task_event_by_task ON task_event(task, id);
             -- The owner's board (`item.rs`). An item is a task row opened by `add` that
             -- never has a terminal; this marks it and keeps its key, fixed at creation so
             -- a rename of the space never changes it. Its steps, the agents' tasks linked
             -- to it, and the agents on other machines qass reports, each in a table of
             -- its own. New tables, no version bump: an older binary ignores them.
             CREATE TABLE IF NOT EXISTS board_item (
                 task INTEGER PRIMARY KEY REFERENCES task(id) ON DELETE CASCADE,
                 prefix TEXT NOT NULL,
                 n INTEGER NOT NULL,
                 description TEXT NOT NULL DEFAULT '',
                 UNIQUE (prefix, n)
             );
             -- A Done item the owner put away (`dismiss`): off every board, nothing deleted.
             -- New table, no version bump: an older binary ignores it.
             CREATE TABLE IF NOT EXISTS board_hidden (
                 task INTEGER PRIMARY KEY REFERENCES task(id) ON DELETE CASCADE,
                 at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS task_step (
                 task INTEGER NOT NULL REFERENCES task(id) ON DELETE CASCADE,
                 n INTEGER NOT NULL,
                 text TEXT NOT NULL,
                 done_at INTEGER,
                 PRIMARY KEY (task, n)
             );
             -- `name` is what the agent was called when it was linked, kept for when its
             -- terminal is gone.
             CREATE TABLE IF NOT EXISTS task_item (
                 task INTEGER PRIMARY KEY REFERENCES task(id) ON DELETE CASCADE,
                 item INTEGER NOT NULL REFERENCES task(id) ON DELETE CASCADE,
                 name TEXT
             );
             CREATE INDEX IF NOT EXISTS task_item_by_item ON task_item(item);
             CREATE TABLE IF NOT EXISTS task_item_remote (
                 item INTEGER NOT NULL REFERENCES task(id) ON DELETE CASCADE,
                 remote TEXT NOT NULL,
                 task INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 stage TEXT NOT NULL,
                 updated_at INTEGER NOT NULL,
                 PRIMARY KEY (remote, task)
             );
             -- The questions qass put to the owner (`ask.rs`). `terminal` is not a foreign key,
             -- for the reason `task.terminal` is not: ids are reused, so every path that ends a
             -- terminal closes its live cards. New table, no version bump: an older binary
             -- ignores it.
             CREATE TABLE IF NOT EXISTS ask (
                 id INTEGER PRIMARY KEY,
                 card TEXT NOT NULL UNIQUE,
                 agent TEXT,
                 terminal INTEGER,
                 session TEXT,
                 task INTEGER REFERENCES task(id) ON DELETE SET NULL,
                 status TEXT NOT NULL,
                 note TEXT,
                 body TEXT NOT NULL,
                 answer TEXT,
                 option TEXT,
                 opened_at INTEGER NOT NULL,
                 closed_at INTEGER
             );
             -- What agents put on the owner's phone (`show.rs`). AUTOINCREMENT, so an id a
             -- phone marked seen is never given again after a prune. New table, no version
             -- bump, like `ask`.
             CREATE TABLE IF NOT EXISTS show (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 at INTEGER NOT NULL,
                 boot TEXT NOT NULL,
                 terminal INTEGER NOT NULL,
                 generation INTEGER NOT NULL,
                 space TEXT,
                 title TEXT,
                 kind TEXT NOT NULL,
                 target TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 note TEXT
             );
             -- The shows the phone asked for from a chat, not an agent (`show.rs`): kept out of
             -- the list and pruned on their own. `mtime` is the file's when it was asked, so an
             -- unchanged file asked again is the same row. New table, no version bump: an
             -- older binary lists them as shows.
             CREATE TABLE IF NOT EXISTS show_asked (
                 show INTEGER PRIMARY KEY REFERENCES show(id) ON DELETE CASCADE,
                 mtime INTEGER NOT NULL
             );
             -- Agents the owner asked qoop to start at a time (`schedule.rs`). AUTOINCREMENT,
             -- so a removed schedule's id is never given again to a run still reporting.
             -- New table, no version bump, like `ask` and `show`: an older binary never
             -- reads it.
             CREATE TABLE IF NOT EXISTS schedule (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL UNIQUE,
                 cron TEXT,
                 at INTEGER,
                 agent TEXT NOT NULL,
                 cwd TEXT NOT NULL,
                 space TEXT,
                 prompt TEXT NOT NULL,
                 paused INTEGER NOT NULL DEFAULT 0,
                 created_at INTEGER NOT NULL,
                 last_slot INTEGER,
                 last_at INTEGER,
                 last_result TEXT,
                 last_terminal INTEGER
             );
             -- The command of a schedule that starts on new output (`schedule.rs`), and the
             -- lines its last check saw (NULL: never checked). A table of its own, so an older
             -- binary reads such a schedule as one with no timing and never fires it.
             CREATE TABLE IF NOT EXISTS schedule_watch (
                 schedule INTEGER PRIMARY KEY REFERENCES schedule(id) ON DELETE CASCADE,
                 command TEXT NOT NULL,
                 seen TEXT
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

    /// Put a workspace just before `before` in the list, or last when it is `None` or not a
    /// workspace. Every space gets its place written, in one transaction, so the list is the
    /// same on the next start. `before` naming the workspace itself moves nothing. Answers
    /// false when there is no such workspace.
    pub fn move_workspace(&mut self, id: i64, before: Option<i64>) -> Result<bool, StoreError> {
        let mut order: Vec<i64> = self.workspaces()?.iter().map(|w| w.id).collect();
        let Some(from) = order.iter().position(|w| *w == id) else {
            return Ok(false);
        };
        if before == Some(id) {
            return Ok(true);
        }
        order.remove(from);
        let at = before
            .and_then(|b| order.iter().position(|w| *w == b))
            .unwrap_or(order.len());
        order.insert(at, id);
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM workspace_order", [])?;
        for (position, workspace) in order.iter().enumerate() {
            tx.execute(
                "INSERT INTO workspace_order (workspace, position) VALUES (?1, ?2)",
                params![workspace, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// The daemon's own space: the lowest id, wherever the list has it.
    pub fn first_workspace(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT MIN(id) FROM workspace", [], |r| r.get(0))?)
    }

    /// Pick a workspace's colour, or with `None` give it back the one its id gives it.
    ///
    /// Answers false when there is no such workspace, like a rename that touched nothing.
    pub fn set_workspace_ink(&self, id: i64, ink: Option<u8>) -> Result<bool, StoreError> {
        match ink {
            // Through a select on the space, so a missing one inserts nothing rather than
            // failing on the foreign key.
            Some(ink) => Ok(self.conn.execute(
                "INSERT OR REPLACE INTO workspace_ink (workspace, ink) \
                 SELECT id, ?2 FROM workspace WHERE id = ?1",
                params![id, ink],
            )? > 0),
            None => {
                let exists = self
                    .conn
                    .query_row("SELECT 1 FROM workspace WHERE id = ?1", params![id], |_| {
                        Ok(())
                    })
                    .optional()?
                    .is_some();
                if exists {
                    self.conn.execute(
                        "DELETE FROM workspace_ink WHERE workspace = ?1",
                        params![id],
                    )?;
                }
                Ok(exists)
            }
        }
    }

    /// Set what the owner wants from a space; an empty text clears it.
    /// One line, at most `task::GOAL_MAX` characters (refused, never cut). Answers false
    /// when there is no such space.
    pub fn set_space_goal(&self, id: i64, goal: &str, now: i64) -> Result<bool, StoreError> {
        let goal = task::one_line(goal);
        task::within("the goal", &goal, task::GOAL_MAX)?;
        let exists = self
            .conn
            .query_row("SELECT 1 FROM workspace WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        if goal.is_empty() {
            self.conn
                .execute("DELETE FROM space_goal WHERE workspace = ?1", params![id])?;
        } else {
            self.conn.execute(
                "INSERT OR REPLACE INTO space_goal (workspace, goal, updated_at) VALUES (?1, ?2, ?3)",
                params![id, goal, now],
            )?;
        }
        Ok(true)
    }

    /// Where a new tab in a space starts when nobody names a directory, if it has a home.
    pub fn space_dir(&self, id: i64) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT dir FROM space_dir WHERE workspace = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Give a space its home. Answers false when there is no such space.
    pub fn set_space_dir(&self, id: i64, dir: &str) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "INSERT OR REPLACE INTO space_dir (workspace, dir) \
             SELECT id, ?2 FROM workspace WHERE id = ?1",
            params![id, dir],
        )? > 0)
    }

    /// Make `dir` a space's home unless it already has one. A space that does not exist is
    /// left alone.
    pub fn claim_space_dir(&self, id: i64, dir: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO space_dir (workspace, dir) \
             SELECT id, ?2 FROM workspace WHERE id = ?1",
            params![id, dir],
        )?;
        Ok(())
    }

    /// A home for a space that has had tabs and has none, from where its tabs were: a directory
    /// named like the space first, then one that is not a worktree or a scratch folder, then
    /// the one most of its tabs were in, then the earliest. With it, whether it is a worktree
    /// or a scratch folder (only good enough for one tab, never to keep). `skip` holds
    /// directories that say nothing about a space (the daemon's own, where a tab nobody placed
    /// starts, `/`, the home folder). Read off the database only.
    ///
    /// ponytail: a guess from history, made once per space; `qoop space dir` corrects it.
    pub fn guess_space_dir(
        &self,
        space: i64,
        skip: &[String],
    ) -> Result<Option<(String, bool)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT w.name, t.cwd, COUNT(*), MIN(t.created_at) FROM workspace w \
             JOIN terminal t ON t.workspace = w.id \
             WHERE w.id = ?1 \
             AND NOT EXISTS (SELECT 1 FROM space_dir d WHERE d.workspace = w.id) \
             GROUP BY t.cwd",
        )?;
        let rows = stmt
            .query_map(params![space], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        type Rank = (bool, bool, i64, std::cmp::Reverse<i64>);
        let mut best: Option<(Rank, String)> = None;
        for (name, cwd, count, first) in rows {
            let path = std::path::Path::new(&cwd);
            let trimmed = cwd.trim_end_matches('/');
            if !path.is_absolute() || skip.iter().any(|s| s.trim_end_matches('/') == trimmed) {
                continue;
            }
            let base = path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let named = base == name.trim().to_lowercase();
            // By the folder names worktrees and scratch get, not by asking the disk: a hung
            // mount must not hold the daemon (ADR-011).
            let passing = base.contains("-wt-")
                || ["/.worktrees/", "/.claude/worktrees/"]
                    .iter()
                    .any(|part| cwd.contains(part))
                || [
                    "/tmp/",
                    "/private/tmp/",
                    "/var/folders/",
                    "/private/var/folders/",
                ]
                .iter()
                .any(|prefix| cwd.starts_with(prefix));
            let rank = (named, !passing, count, std::cmp::Reverse(first));
            if best.as_ref().is_none_or(|(had, _)| rank > *had) {
                best = Some((rank, cwd));
            }
        }
        Ok(best.map(|((_, keep, ..), dir)| (dir, !keep)))
    }

    /// Keep a space awake, or let idle sleep take its tabs again. Answers false when there
    /// is no such space.
    pub fn set_keep_awake_workspace(&self, id: i64, on: bool) -> Result<bool, StoreError> {
        let exists = self
            .conn
            .query_row("SELECT 1 FROM workspace WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if exists {
            let sql = if on {
                "INSERT OR IGNORE INTO awake_workspace (workspace) VALUES (?1)"
            } else {
                "DELETE FROM awake_workspace WHERE workspace = ?1"
            };
            self.conn.execute(sql, params![id])?;
        }
        Ok(exists)
    }

    /// The same for one pane. Answers false when there is no such open terminal.
    pub fn set_keep_awake_terminal(&self, id: i64, on: bool) -> Result<bool, StoreError> {
        let open = self
            .conn
            .query_row(
                "SELECT 1 FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if open {
            let sql = if on {
                "INSERT OR IGNORE INTO awake_terminal (terminal) VALUES (?1)"
            } else {
                "DELETE FROM awake_terminal WHERE terminal = ?1"
            };
            self.conn.execute(sql, params![id])?;
        }
        Ok(open)
    }

    /// Have qass leave a space alone, or watch it again. Answers false when there is no
    /// such space.
    pub fn set_qass_skip_workspace(&self, id: i64, on: bool) -> Result<bool, StoreError> {
        let exists = self
            .conn
            .query_row("SELECT 1 FROM workspace WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if exists {
            let sql = if on {
                "INSERT OR IGNORE INTO qass_skip_workspace (workspace) VALUES (?1)"
            } else {
                "DELETE FROM qass_skip_workspace WHERE workspace = ?1"
            };
            self.conn.execute(sql, params![id])?;
        }
        Ok(exists)
    }

    /// The same for one pane. Answers false when there is no such open terminal.
    pub fn set_qass_skip_terminal(&self, id: i64, on: bool) -> Result<bool, StoreError> {
        let open = self
            .conn
            .query_row(
                "SELECT 1 FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if open {
            let sql = if on {
                "INSERT OR IGNORE INTO qass_skip_terminal (terminal) VALUES (?1)"
            } else {
                "DELETE FROM qass_skip_terminal WHERE terminal = ?1"
            };
            self.conn.execute(sql, params![id])?;
        }
        Ok(open)
    }

    /// Park an open terminal's agent at `at`, or take it back. Answers false when there is
    /// no such open terminal.
    pub fn set_parked_terminal(&self, id: i64, on: bool, at: i64) -> Result<bool, StoreError> {
        let open = self
            .conn
            .query_row(
                "SELECT 1 FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if open && on {
            self.conn.execute(
                "INSERT OR REPLACE INTO parked_terminal (terminal, at) VALUES (?1, ?2)",
                params![id, at],
            )?;
        } else if open {
            self.conn.execute(
                "DELETE FROM parked_terminal WHERE terminal = ?1",
                params![id],
            )?;
        }
        Ok(open)
    }

    /// Every parked terminal and when it was parked.
    pub fn parked(&self) -> Result<BTreeMap<i64, i64>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT terminal, at FROM parked_terminal")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<BTreeMap<i64, i64>, _>>()?;
        Ok(rows)
    }

    /// Every space and every pane kept awake.
    pub fn kept_awake(&self) -> Result<(BTreeSet<i64>, BTreeSet<i64>), StoreError> {
        let read = |sql: &str| -> Result<BTreeSet<i64>, StoreError> {
            let mut stmt = self.conn.prepare(sql)?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<BTreeSet<i64>, _>>()?;
            Ok(ids)
        };
        Ok((
            read("SELECT workspace FROM awake_workspace")?,
            read("SELECT terminal FROM awake_terminal")?,
        ))
    }

    /// Every space and every pane qass leaves alone.
    pub fn qass_skips(&self) -> Result<(BTreeSet<i64>, BTreeSet<i64>), StoreError> {
        let read = |sql: &str| -> Result<BTreeSet<i64>, StoreError> {
            let mut stmt = self.conn.prepare(sql)?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<BTreeSet<i64>, _>>()?;
            Ok(ids)
        };
        Ok((
            read("SELECT workspace FROM qass_skip_workspace")?,
            read("SELECT terminal FROM qass_skip_terminal")?,
        ))
    }

    /// Drop a workspace row.
    ///
    /// Answers how many rows it touched, like the rename above. Terminal and layout rows
    /// go with it through `ON DELETE CASCADE`: the daemon only calls this for a
    /// workspace with no live terminal, so what goes is dead history, never a session
    /// somebody is looking at.
    pub fn delete_workspace(&self, id: i64) -> Result<usize, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        // Read before the delete: the cascade takes the terminal rows with it.
        let terminals = {
            let mut stmt = tx.prepare("SELECT id FROM terminal WHERE workspace = ?1")?;
            stmt.query_map(params![id], |r| r.get(0))?
                .collect::<Result<Vec<i64>, _>>()?
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        task::abandon_open_tasks(&tx, &terminals, now)?;
        task::abandon_space_tasks(&tx, id, now)?;
        // Its board items too: an item lives in a space, and one left open with no space
        // would stay on every board with nowhere to run.
        item::abandon_open_items(&tx, id, now)?;
        let n = tx.execute("DELETE FROM workspace WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT w.id, w.name, w.created_at, i.ink, g.goal, d.dir FROM workspace w \
                 LEFT JOIN workspace_ink i ON i.workspace = w.id \
                 LEFT JOIN space_goal g ON g.workspace = w.id \
                 LEFT JOIN space_dir d ON d.workspace = w.id \
                 LEFT JOIN workspace_order o ON o.workspace = w.id \
                 ORDER BY o.position IS NULL, o.position, w.id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(WorkspaceRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created_at: r.get(2)?,
                    // A value no byte can hold is auto rather than a list that fails.
                    ink: r
                        .get::<_, Option<i64>>(3)?
                        .and_then(|ink| u8::try_from(ink).ok()),
                    goal: r.get(4)?,
                    dir: r.get(5)?,
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
            "DELETE FROM asleep_terminal WHERE terminal = ?1",
            params![id],
        )?;
        task::abandon_open_tasks(&tx, &[id], now)?;
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
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE terminal SET closed_at = ?2 WHERE id = ?1 AND closed_at IS NULL",
            params![id, now],
        )?;
        // A closed tab is not asleep: nothing will ever wake it, nor keep it awake.
        tx.execute(
            "DELETE FROM asleep_terminal WHERE terminal = ?1",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM awake_terminal WHERE terminal = ?1",
            params![id],
        )?;
        // Nor skipped by qass: its id may be handed out again.
        tx.execute(
            "DELETE FROM qass_skip_terminal WHERE terminal = ?1",
            params![id],
        )?;
        // Nor parked: nobody will answer it.
        tx.execute(
            "DELETE FROM parked_terminal WHERE terminal = ?1",
            params![id],
        )?;
        // Nor working on anything: its id may be handed out again.
        task::abandon_open_tasks(&tx, &[id], now)?;
        tx.commit()?;
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

    /// Move open terminals into a workspace, together and in the order given: just before
    /// the open tab `before`, or after every open tab there when it is `None` or not one of
    /// them. The workspace may be the one they are in, which is a reorder.
    ///
    /// One transaction for the rows and the whole order, so a crash leaves them where they
    /// were. Answers whether they moved: nothing moves when one of them is not open. A
    /// `before` that is one of them names where they already are when they are already in
    /// this workspace, which moves nothing and answers yes; moving in from another, it names
    /// nothing here, and they go last.
    pub fn move_terminals(
        &mut self,
        ids: &[i64],
        workspace: i64,
        before: Option<i64>,
    ) -> Result<bool, StoreError> {
        if before.is_some_and(|b| ids.contains(&b)) {
            let mut here = true;
            for id in ids {
                let at: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT workspace FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                        params![id],
                        |r| r.get(0),
                    )
                    .optional()?;
                here &= at == Some(workspace);
            }
            if here {
                return Ok(true);
            }
        }
        let before = before.filter(|b| !ids.contains(b));
        let tx = self.conn.transaction()?;
        for id in ids {
            let n = tx.execute(
                "UPDATE terminal SET workspace = ?2 WHERE id = ?1 AND closed_at IS NULL",
                params![id, workspace],
            )?;
            if n != 1 {
                // Dropped unfinished: the transaction rolls back every row it touched.
                return Ok(false);
            }
            // A tab moves awake: the daemon refuses to move a sleeping one, and a mark
            // left here would hold its agent writes back in the new space.
            tx.execute(
                "DELETE FROM asleep_terminal WHERE terminal = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM layout WHERE terminal = ?1", params![id])?;
        }
        // The space's other open tabs in their order, the moved ones spliced in, and every
        // place written again from zero. Closed rows keep theirs; a new tab still goes after
        // them all, at the largest ordinal plus one.
        let mut order: Vec<i64> = tx
            .prepare(
                "SELECT l.terminal FROM layout l JOIN terminal t ON t.id = l.terminal
                 WHERE l.workspace = ?1 AND t.closed_at IS NULL
                 ORDER BY l.ordinal, l.terminal",
            )?
            .query_map(params![workspace], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let at = before
            .and_then(|b| order.iter().position(|t| *t == b))
            .unwrap_or(order.len());
        order.splice(at..at, ids.iter().copied());
        for (ordinal, terminal) in order.iter().enumerate() {
            tx.execute(
                "INSERT INTO layout (workspace, terminal, ordinal) VALUES (?1, ?2, ?3)
                 ON CONFLICT (workspace, terminal) DO UPDATE SET ordinal = excluded.ordinal",
                params![workspace, terminal, ordinal as i64],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Every open terminal's place in its workspace's order, for a listing to sort by.
    pub fn layout_order(&self) -> Result<Vec<(i64, i64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT l.terminal, l.ordinal FROM layout l
             JOIN terminal t ON t.id = l.terminal AND t.workspace = l.workspace
             WHERE t.closed_at IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
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
             ORDER BY l.ordinal, t.id",
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

    /// Every terminal ever given this name, open or closed, newest first: where a
    /// conversation that outlived its tab is found again. A closed tab keeps its last
    /// conversation's id even after its agent left.
    pub fn named_terminals(&self, title: &str) -> Result<Vec<TerminalRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workspace, title, cwd, argv, agent, agent_session, created_at, closed_at
             FROM terminal WHERE title = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map(params![title], |r| {
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
        // Never into a sleeping tab: its row says what to bring back, and a late write
        // from the teardown that put it to sleep would say something else. Checked here,
        // when the write runs, not when it was asked for, because a write whose caller
        // gave up waiting still runs.
        self.conn.execute(
            "UPDATE terminal SET agent_session = ?2 WHERE id = ?1 \
             AND id NOT IN (SELECT terminal FROM asleep_terminal)",
            params![id, session],
        )?;
        Ok(())
    }

    /// Forget the id of a conversation whose agent has left the terminal, if the row still
    /// holds that one: an id the next agent wrote meanwhile is its own, and stays.
    pub fn forget_agent_session(&self, id: i64, session: &str) -> Result<(), StoreError> {
        // Not in a sleeping tab, like every write here: its row says what to bring back.
        self.conn.execute(
            "UPDATE terminal SET agent_session = NULL WHERE id = ?1 AND agent_session = ?2 \
             AND id NOT IN (SELECT terminal FROM asleep_terminal)",
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
    /// The session id is the other way round. It is never cleared by a `None` here, because
    /// an agent that stops reporting its id has not ended the conversation, and forgetting
    /// the id would turn an exact resume into a guess. It goes with the agent that had it,
    /// through `forget_agent_session`.
    pub fn set_agent(
        &self,
        id: i64,
        agent: Option<&str>,
        agent_session: Option<&str>,
    ) -> Result<(), StoreError> {
        // Not into a sleeping tab, for the reason `set_agent_session` gives: an agent
        // ending under the sleep is announced as gone, and writing that down would bring
        // the tab back as a bare shell.
        self.conn.execute(
            "UPDATE terminal SET agent = ?2, agent_session = COALESCE(?3, agent_session) \
             WHERE id = ?1 AND id NOT IN (SELECT terminal FROM asleep_terminal)",
            params![id, agent, agent_session],
        )?;
        Ok(())
    }
}

/// What a sleeping terminal's row has to say when it goes to sleep: which agent was in
/// it and that agent's own id for the conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub terminal: i64,
    pub agent: Option<String>,
    pub agent_session: Option<String>,
}

impl Store {
    /// Every open terminal that is asleep, as `(terminal, workspace, since)`, the latest
    /// first. The terminals of one tab share their `since`.
    pub fn asleep(&self) -> Result<Vec<(i64, i64, i64)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT a.terminal, t.workspace, a.since FROM asleep_terminal a \
             JOIN terminal t ON t.id = a.terminal WHERE t.closed_at IS NULL \
             ORDER BY a.since DESC, a.terminal DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The agent an open, sleeping terminal holds, as `(agent, its conversation id)`: what a
    /// reader needs to find the conversation while no daemon row stands for the tab.
    pub fn sleeping_agent(
        &self,
        terminal: i64,
    ) -> Result<Option<(String, Option<String>)>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT t.agent, t.agent_session FROM asleep_terminal a \
                 JOIN terminal t ON t.id = a.terminal \
                 WHERE a.terminal = ?1 AND t.closed_at IS NULL AND t.agent IS NOT NULL",
                params![terminal],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Put terminals to sleep: their agents written down, then their marks, in one
    /// transaction. A failure leaves neither, so a tab is never marked asleep with a row
    /// that does not say what to bring back. A closed terminal is not marked.
    pub fn sleep_terminals(
        &mut self,
        since: i64,
        checkpoints: &[Checkpoint],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for c in checkpoints {
            write_checkpoint(&tx, c)?;
            tx.execute(
                "INSERT OR REPLACE INTO asleep_terminal (terminal, since) \
                 SELECT id, ?2 FROM terminal WHERE id = ?1 AND closed_at IS NULL",
                params![c.terminal, since],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Take the sleep marks off these terminals. Answers how many had one.
    pub fn unmark(&mut self, terminals: &[i64]) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for t in terminals {
            n += tx.execute(
                "DELETE FROM asleep_terminal WHERE terminal = ?1",
                params![t],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Write the terminals' agents down as a sleep does, in one transaction, without
    /// marking or unmarking any terminal asleep: what a daemon moving onto a new binary
    /// leaves for the one after it, whose restore brings every awake row back from them.
    pub fn checkpoint(&mut self, checkpoints: &[Checkpoint]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for c in checkpoints {
            write_checkpoint(&tx, c)?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// A checkpoint into its terminal's row. A session left unsaid keeps the one written.
fn write_checkpoint(tx: &rusqlite::Transaction<'_>, c: &Checkpoint) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE terminal SET agent = ?2, agent_session = COALESCE(?3, agent_session) \
             WHERE id = ?1",
        params![c.terminal, c.agent, c.agent_session],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    /// Every tab of a name is found again, closed ones too, newest first, with the id its
    /// last conversation left even after its agent went.
    #[test]
    fn a_named_tab_outlives_its_agent() {
        let mut store = Store::in_memory().expect("store");
        let space = store.create_workspace("qass", 1).expect("space");
        for id in [1, 2, 3] {
            store
                .create_terminal(id, space, "/q", "zsh", 1)
                .expect("terminal");
        }
        store.set_title(1, "qass").expect("title");
        store.set_title(3, "qass").expect("title");
        store
            .set_agent(1, Some("claude"), Some("old"))
            .expect("agent");
        store
            .set_agent(3, Some("codex"), Some("new"))
            .expect("agent");
        store.set_agent(3, None, None).expect("agent left");
        store.close_terminal(3, 2).expect("close");
        let found = store.named_terminals("qass").expect("query");
        let seen: Vec<_> = found
            .iter()
            .map(|t| {
                (
                    t.id,
                    t.agent.as_deref(),
                    t.agent_session.as_deref(),
                    t.closed_at,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                (3, None, Some("new"), Some(2)),
                (1, Some("claude"), Some("old"), None)
            ]
        );
    }

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
        let path = dir.path().join("qoop.db");
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
        let path = dir.path().join("qoop.db");
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
        let path = dir.path().join("qoop.db");
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
        let path = dir.path().join("qoop.db");
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
            err.to_string().contains("newer qoop"),
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

    fn bare(terminal: i64) -> Checkpoint {
        Checkpoint {
            terminal,
            agent: None,
            agent_session: None,
        }
    }

    #[test]
    fn a_tab_sleeps_with_its_agent_written_down_and_wakes_alone() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.create_terminal(8, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(7, Some("claude"), Some("abc")).expect("agent");
        s.sleep_terminals(
            10,
            &[Checkpoint {
                terminal: 7,
                agent: Some("claude".into()),
                agent_session: None,
            }],
        )
        .expect("sleep");
        // One tab of the space, not the space.
        assert_eq!(s.asleep().expect("asleep"), vec![(7, ws, 10)]);
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent.as_deref(), Some("claude"));
        // A checkpoint with no id keeps the one the hook reported.
        assert_eq!(row.agent_session.as_deref(), Some("abc"));
        assert_eq!(s.unmark(&[7]).expect("wake"), 1);
        assert_eq!(s.unmark(&[7]).expect("wake again"), 0);
        assert!(s.asleep().expect("asleep").is_empty());
    }

    #[test]
    fn the_tab_that_slept_last_comes_first() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        for t in [1, 2, 3] {
            s.create_terminal(t, ws, "/tmp", "/bin/sh", 1).expect("t");
        }
        s.sleep_terminals(5, &[bare(1), bare(2)])
            .expect("one tab of two panes");
        s.sleep_terminals(9, &[bare(3)]).expect("another tab");
        assert_eq!(
            s.asleep().expect("asleep"),
            vec![(3, ws, 9), (2, ws, 5), (1, ws, 5)]
        );
    }

    /// A sleeping tab still says whose conversation it holds; an awake one, or one with no
    /// agent, says nothing.
    #[test]
    fn a_sleeping_tab_names_its_agent_and_conversation() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.create_terminal(8, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(7, Some("claude"), Some("abc")).expect("agent");
        assert_eq!(s.sleeping_agent(7).expect("query"), None, "awake");
        s.sleep_terminals(
            10,
            &[
                Checkpoint {
                    terminal: 7,
                    agent: Some("claude".into()),
                    agent_session: Some("abc".into()),
                },
                Checkpoint {
                    terminal: 8,
                    agent: None,
                    agent_session: None,
                },
            ],
        )
        .expect("sleep");
        assert_eq!(
            s.sleeping_agent(7).expect("query"),
            Some(("claude".into(), Some("abc".into())))
        );
        assert_eq!(s.sleeping_agent(8).expect("query"), None, "no agent");
        assert_eq!(s.sleeping_agent(9).expect("query"), None, "no such tab");
    }

    #[test]
    fn a_closed_or_moved_tab_is_not_asleep() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        let other = s.create_workspace("o", 1).expect("ws");
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.create_terminal(2, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.sleep_terminals(5, &[bare(1), bare(2)]).expect("sleep");
        s.close_terminal(1, 6).expect("close");
        s.move_terminals(&[2], other, None).expect("move");
        assert!(s.asleep().expect("asleep").is_empty());
        // A closed row is never marked.
        s.sleep_terminals(7, &[bare(1)])
            .expect("sleep a closed one");
        assert!(s.asleep().expect("asleep").is_empty());
    }

    /// A move places tabs exactly: before a tab, at the end, in their own space or another,
    /// a whole split together; naming one of the moved tabs, or a tab that is not open there,
    /// changes nothing or lands at the end; a closed tab is never moved.
    #[test]
    fn a_move_places_tabs_where_it_is_asked() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        let other = s.create_workspace("o", 1).expect("ws");
        for id in 1..=4 {
            s.create_terminal(id, ws, "/tmp", "/bin/sh", 1).expect("t");
        }
        s.create_terminal(9, other, "/tmp", "/bin/sh", 1)
            .expect("t");
        let ids = |s: &Store, ws| -> Vec<i64> {
            s.open_terminals(ws)
                .expect("rows")
                .iter()
                .map(|r| r.id)
                .collect()
        };
        let order_of = |s: &Store| {
            let mut all = s.layout_order().expect("order");
            all.sort();
            all
        };
        // Within its own space: to the front, into the middle, to the end.
        assert!(s.move_terminals(&[3], ws, Some(1)).expect("move"));
        assert_eq!(ids(&s, ws), vec![3, 1, 2, 4]);
        assert!(s.move_terminals(&[3], ws, Some(4)).expect("move"));
        assert_eq!(ids(&s, ws), vec![1, 2, 3, 4]);
        assert!(s.move_terminals(&[1], ws, None).expect("move"));
        assert_eq!(ids(&s, ws), vec![2, 3, 4, 1]);
        // A split moves as one, in the order given.
        assert!(s.move_terminals(&[4, 1], ws, Some(2)).expect("move"));
        assert_eq!(ids(&s, ws), vec![4, 1, 2, 3]);
        // Naming one of the moved tabs is where they already are.
        let before = order_of(&s);
        assert!(s.move_terminals(&[4, 1], ws, Some(1)).expect("no-op"));
        assert_eq!(order_of(&s), before);
        // Into another space naming itself: it names nothing there, and goes last.
        assert!(s.move_terminals(&[3], other, Some(3)).expect("move"));
        assert_eq!(ids(&s, other), vec![9, 3]);
        assert!(s.move_terminals(&[3], ws, None).expect("back"));
        // Into another space, at a place, and at the end when the place is not open there.
        assert!(s.move_terminals(&[2], other, Some(9)).expect("move"));
        assert_eq!(ids(&s, other), vec![2, 9]);
        assert_eq!(ids(&s, ws), vec![4, 1, 3]);
        assert!(s.move_terminals(&[3], other, Some(1)).expect("move"));
        assert_eq!(ids(&s, other), vec![2, 9, 3]);
        // A closed tab is not moved, and takes nothing with it.
        s.close_terminal(4, 5).expect("close");
        assert!(!s.move_terminals(&[1, 4], other, None).expect("refused"));
        assert_eq!(ids(&s, ws), vec![1]);
        assert_eq!(ids(&s, other), vec![2, 9, 3]);
        // A new tab still lands after every other, closed ones included.
        s.create_terminal(5, other, "/tmp", "/bin/sh", 1)
            .expect("t");
        assert_eq!(ids(&s, other), vec![2, 9, 3, 5]);
    }

    /// Spaces keep the order a person gave them: moved before another or to the end, new
    /// ones after, a deleted one's place gone with it, and the lowest id still the first
    /// space wherever it is listed.
    #[test]
    fn spaces_keep_the_order_they_were_put_in() {
        let mut s = store();
        let ids: Vec<i64> = ["a", "b", "c"]
            .iter()
            .map(|n| s.create_workspace(n, 1).expect("ws"))
            .collect();
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        let listed = |s: &Store| -> Vec<i64> {
            s.workspaces().expect("list").iter().map(|w| w.id).collect()
        };
        assert_eq!(listed(&s), vec![a, b, c], "never placed: by id");
        assert!(s.move_workspace(c, Some(a)).expect("move"));
        assert_eq!(listed(&s), vec![c, a, b]);
        assert!(s.move_workspace(c, None).expect("move"));
        assert_eq!(listed(&s), vec![a, b, c]);
        assert!(s.move_workspace(a, Some(c)).expect("move"));
        assert_eq!(listed(&s), vec![b, a, c]);
        assert!(s.move_workspace(a, Some(a)).expect("no-op"));
        assert_eq!(listed(&s), vec![b, a, c]);
        assert!(!s.move_workspace(99, None).expect("no such space"));
        let d = s.create_workspace("d", 1).expect("ws");
        assert_eq!(listed(&s), vec![b, a, c, d], "a new space lands last");
        s.delete_workspace(a).expect("delete");
        assert_eq!(listed(&s), vec![b, c, d]);
        assert_eq!(s.first_workspace().expect("first"), Some(b.min(c)));
    }

    #[test]
    fn deleting_a_workspace_takes_its_sleep_marks_with_it() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.sleep_terminals(5, &[bare(1)]).expect("sleep");
        s.delete_workspace(ws).expect("delete");
        assert!(s.asleep().expect("asleep").is_empty());
    }

    /// A space's home is guessed from where its tabs were: a folder named like the space beats
    /// a worktree more tabs sat in, scratch and worktrees come last and say so, the daemon's own
    /// directory never counts, and a space with a home is not guessed for.
    #[test]
    fn a_space_home_is_guessed_from_its_tabs_and_kept() {
        let mut s = store();
        let qoop = s.create_workspace("qoop", 1).expect("ws");
        let other = s.create_workspace("other", 1).expect("ws");
        let only_home = s.create_workspace("h", 1).expect("ws");
        let fresh = s.create_workspace("fresh", 1).expect("ws");
        let worktrees = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(1, qoop, "/c/qoop-wt-a", "/bin/sh", 1)
            .expect("t");
        s.create_terminal(2, qoop, "/c/qoop-wt-a", "/bin/sh", 2)
            .expect("t");
        s.create_terminal(3, qoop, "/c/qoop/", "/bin/sh", 3)
            .expect("t");
        s.close_terminal(3, 4).expect("closed history counts too");
        s.create_terminal(4, other, "/w/b", "/bin/sh", 2)
            .expect("t");
        s.create_terminal(5, other, "/w/a", "/bin/sh", 1)
            .expect("t");
        s.create_terminal(6, other, "/w/a", "/bin/sh", 3)
            .expect("t");
        s.create_terminal(7, only_home, "/home/me", "/bin/sh", 1)
            .expect("t");
        s.create_terminal(9, fresh, "/private/tmp/x", "/bin/sh", 9)
            .expect("t");
        s.create_terminal(10, fresh, "/c/private/fresh-wt-a", "/bin/sh", 9)
            .expect("t");
        s.create_terminal(11, fresh, "/c/private/app", "/bin/sh", 10)
            .expect("t");
        s.create_terminal(12, worktrees, "/c/r-wt-x", "/bin/sh", 1)
            .expect("t");
        let skip = ["/home/me".to_string(), "/".to_string()];
        let guess = |s: &Store, ws| s.guess_space_dir(ws, &skip).expect("guess");
        assert_eq!(guess(&s, qoop), Some(("/c/qoop/".into(), false)));
        assert_eq!(guess(&s, other), Some(("/w/a".into(), false)));
        assert_eq!(guess(&s, only_home), None);
        assert_eq!(
            guess(&s, fresh),
            Some(("/c/private/app".into(), false)),
            "a folder merely under one named private is no scratch"
        );
        assert_eq!(guess(&s, worktrees), Some(("/c/r-wt-x".into(), true)));
        s.claim_space_dir(qoop, "/c/qoop").expect("claim");
        s.claim_space_dir(qoop, "/elsewhere")
            .expect("a second claim is ignored");
        assert_eq!(s.space_dir(qoop).expect("read").as_deref(), Some("/c/qoop"));
        assert_eq!(
            guess(&s, qoop),
            None,
            "a space with a home is not guessed for"
        );
        assert!(s.set_space_dir(qoop, "/c/qoop2").expect("set"));
        assert!(!s.set_space_dir(99, "/x").expect("no space"));
        let listed = s.workspaces().expect("list");
        assert_eq!(listed[0].dir.as_deref(), Some("/c/qoop2"));
        s.claim_space_dir(99, "/x").expect("no space, no row");
        let last = s.create_workspace("last", 9).expect("ws");
        assert!(s.set_space_dir(last, "/f").expect("set"));
        s.delete_workspace(last).expect("delete");
        let back = s.create_workspace("again", 9).expect("ws");
        assert_eq!(back, last, "ids are reused");
        assert_eq!(
            s.space_dir(back).expect("read"),
            None,
            "went with its space"
        );
    }

    /// Keep awake is kept for a space and a pane, taken back on request, and never outlives
    /// the space or the pane: a leftover row would block the space's delete on its key.
    #[test]
    fn keep_awake_round_trips_and_goes_with_its_space_and_pane() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t1");
        s.create_terminal(2, ws, "/tmp", "/bin/sh", 1).expect("t2");
        assert!(s.set_keep_awake_workspace(ws, true).expect("space"));
        assert!(s.set_keep_awake_workspace(ws, true).expect("twice is fine"));
        assert!(s.set_keep_awake_terminal(1, true).expect("pane"));
        assert!(s.set_keep_awake_terminal(2, true).expect("pane 2"));
        assert!(!s.set_keep_awake_workspace(99, true).expect("no space"));
        assert!(!s.set_keep_awake_terminal(99, true).expect("no pane"));
        let (spaces, panes) = s.kept_awake().expect("read");
        assert_eq!((spaces.len(), panes.len()), (1, 2));
        assert!(s.set_keep_awake_terminal(2, false).expect("off"));
        s.close_terminal(1, 2).expect("close");
        assert!(!s.set_keep_awake_terminal(1, true).expect("closed"));
        assert_eq!(s.kept_awake().expect("read").1, BTreeSet::new());
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t3");
        assert!(s.set_keep_awake_terminal(3, true).expect("pane 3"));
        s.delete_workspace(ws).expect("delete with rows kept awake");
        assert_eq!(
            s.kept_awake().expect("read"),
            (BTreeSet::new(), BTreeSet::new())
        );
    }

    /// A qass skip is kept for a space and a pane, taken back on request, and never
    /// outlives the space or the pane: ids are reused, and a new tab must not inherit it.
    #[test]
    fn qass_skip_round_trips_and_goes_with_its_space_and_pane() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t1");
        s.create_terminal(2, ws, "/tmp", "/bin/sh", 1).expect("t2");
        assert!(s.set_qass_skip_workspace(ws, true).expect("space"));
        assert!(s.set_qass_skip_workspace(ws, true).expect("twice is fine"));
        assert!(s.set_qass_skip_terminal(1, true).expect("pane"));
        assert!(s.set_qass_skip_terminal(2, true).expect("pane 2"));
        assert!(!s.set_qass_skip_workspace(99, true).expect("no space"));
        assert!(!s.set_qass_skip_terminal(99, true).expect("no pane"));
        let (spaces, panes) = s.qass_skips().expect("read");
        assert_eq!((spaces.len(), panes.len()), (1, 2));
        assert!(s.set_qass_skip_terminal(2, false).expect("off"));
        s.close_terminal(1, 2).expect("close");
        assert!(!s.set_qass_skip_terminal(1, true).expect("closed"));
        assert_eq!(s.qass_skips().expect("read").1, BTreeSet::new());
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t3");
        assert!(s.set_qass_skip_terminal(3, true).expect("pane 3"));
        s.delete_workspace(ws).expect("delete with rows skipped");
        assert_eq!(
            s.qass_skips().expect("read"),
            (BTreeSet::new(), BTreeSet::new())
        );
    }

    /// A park is kept with its time, taken back on request, and never outlives its pane.
    #[test]
    fn a_park_round_trips_and_goes_with_its_pane() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t1");
        s.create_terminal(2, ws, "/tmp", "/bin/sh", 1).expect("t2");
        assert!(s.set_parked_terminal(1, true, 100).expect("park"));
        assert!(s.set_parked_terminal(2, true, 200).expect("park 2"));
        assert!(!s.set_parked_terminal(99, true, 1).expect("no pane"));
        assert_eq!(
            s.parked().expect("read"),
            BTreeMap::from([(1, 100), (2, 200)])
        );
        assert!(s.set_parked_terminal(2, false, 300).expect("unpark"));
        s.close_terminal(1, 2).expect("close");
        assert!(!s.set_parked_terminal(1, true, 400).expect("closed"));
        assert_eq!(s.parked().expect("read"), BTreeMap::new());
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t3");
        assert!(s.set_parked_terminal(3, true, 500).expect("park 3"));
        s.delete_workspace(ws).expect("delete with a parked row");
        assert_eq!(s.parked().expect("read"), BTreeMap::new());
    }

    #[test]
    fn a_picked_colour_round_trips_and_goes_with_its_space() {
        let s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        assert_eq!(s.workspaces().expect("list")[0].ink, None);
        // Back to auto on a space that never had a colour is still a yes.
        assert!(s.set_workspace_ink(ws, None).expect("none"));
        assert!(s.set_workspace_ink(ws, Some(3)).expect("set"));
        assert!(s.set_workspace_ink(ws, Some(4)).expect("again"));
        assert_eq!(s.workspaces().expect("list")[0].ink, Some(4));
        assert!(s.set_workspace_ink(ws, None).expect("auto"));
        assert_eq!(s.workspaces().expect("list")[0].ink, None);
        // No such space, either way: nothing written.
        assert!(!s.set_workspace_ink(ws + 1, Some(1)).expect("missing"));
        assert!(!s.set_workspace_ink(ws + 1, None).expect("missing"));

        // Ids are reused, so the next space under this id starts on auto.
        assert!(s.set_workspace_ink(ws, Some(2)).expect("set"));
        s.delete_workspace(ws).expect("delete");
        let again = s.create_workspace("v", 2).expect("ws");
        assert_eq!(again, ws);
        assert_eq!(s.workspaces().expect("list")[0].ink, None);
    }

    #[test]
    fn a_space_slept_whole_by_an_older_daemon_becomes_marks_on_its_tabs_once() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("qoop.db");
        let ws;
        {
            let mut s = Store::open(&path).expect("open");
            ws = s.create_workspace("w", 1).expect("ws");
            let awake = s.create_workspace("a", 1).expect("ws");
            s.create_terminal(1, ws, "/tmp", "/bin/sh", 1).expect("t");
            s.create_terminal(2, ws, "/tmp", "/bin/sh", 1).expect("t");
            s.create_terminal(3, awake, "/tmp", "/bin/sh", 1)
                .expect("t");
            s.close_terminal(2, 2).expect("close");
            // What an older daemon wrote when it put the space to sleep.
            s.conn
                .execute(
                    "INSERT INTO asleep (workspace, since) VALUES (?1, 40)",
                    params![ws],
                )
                .expect("old mark");
        }
        let reader = Store::open_existing(&path).expect("a reader leaves it alone");
        assert!(reader.asleep().expect("asleep").is_empty());
        drop(reader);
        let opened = Store::open(&path).expect("the daemon's open turns it");
        assert_eq!(opened.asleep().expect("asleep"), vec![(1, ws, 40)]);
        drop(opened);
        let again = Store::open(&path).expect("a second open finds nothing to turn");
        assert_eq!(again.asleep().expect("asleep"), vec![(1, ws, 40)]);
        let old: i64 = again
            .conn
            .query_row("SELECT COUNT(*) FROM asleep", [], |r| r.get(0))
            .expect("count");
        assert_eq!(old, 0);
    }

    /// A reader handed an empty file (what a stray `sqlite3 <path>` leaves) refuses it and
    /// leaves it empty: building a schema there made it look like a real database.
    #[test]
    fn a_reader_never_turns_an_empty_file_into_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qoop.db");
        std::fs::write(&path, "").unwrap();
        let refused = Store::open_existing(&path).err().expect("refused");
        assert!(
            refused.to_string().contains("holds no qoop database"),
            "{refused}"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert!(!dir.path().join("qoop.db-wal").exists());
        // A header with no schema (what `sqlite3 <path> "PRAGMA journal_mode=WAL"` leaves).
        let stray = dir.path().join("stray.db");
        rusqlite::Connection::open(&stray)
            .unwrap()
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        assert!(std::fs::metadata(&stray).unwrap().len() > 0);
        assert!(!Store::holds_schema(&stray));
        assert!(Store::open_existing(&stray).is_err());
        let real = dir.path().join("real.db");
        drop(Store::open(&real).unwrap());
        assert!(Store::holds_schema(&real));
    }

    #[test]
    fn a_sleeping_tab_refuses_late_agent_writes() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(3, Some("claude"), Some("abc")).expect("agent");
        let held = Checkpoint {
            terminal: 3,
            agent: Some("claude".into()),
            agent_session: None,
        };
        s.sleep_terminals(2, &[held]).expect("sleep");
        s.set_agent(3, None, None).expect("a late write runs");
        s.set_agent_session(3, "other").expect("a late id runs");
        s.forget_agent_session(3, "abc")
            .expect("a late forget runs");
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent.as_deref(), Some("claude"));
        assert_eq!(row.agent_session.as_deref(), Some("abc"));
        s.unmark(&[3]).expect("wake");
        s.set_agent(3, None, None).expect("an awake write");
        assert_eq!(s.open_terminals(ws).expect("rows")[0].agent, None);
    }

    #[test]
    fn a_forgotten_conversation_is_gone_from_the_row() {
        let mut s = store();
        let ws = s.create_workspace("w", 1).expect("ws");
        s.create_terminal(3, ws, "/tmp", "/bin/sh", 1).expect("t");
        s.set_agent(3, Some("opencode"), Some("ses_a"))
            .expect("agent");
        // The next agent's id landed first: forgetting the old one leaves it.
        s.set_agent_session(3, "ses_b").expect("next id");
        s.forget_agent_session(3, "ses_a")
            .expect("forget a stale one");
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent_session.as_deref(), Some("ses_b"));
        s.forget_agent_session(3, "ses_b").expect("forget");
        let row = &s.open_terminals(ws).expect("rows")[0];
        assert_eq!(row.agent_session, None);
        // Forgotten, an agent coming back with no id cannot bring the old one back.
        s.set_agent(3, Some("opencode"), None).expect("agent again");
        assert_eq!(s.open_terminals(ws).expect("rows")[0].agent_session, None);
    }
}

#[cfg(test)]
mod openable_tests {
    use super::*;

    #[test]
    fn a_dry_run_reads_the_database_and_changes_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("qoop.db");
        assert!(
            Store::check_openable(&path).is_err(),
            "a missing file is not openable"
        );
        assert!(!path.exists(), "the dry run made a database");
        drop(Store::open(&path).expect("a database"));
        let before = std::fs::read(&path).expect("bytes");
        Store::check_openable(&path).expect("a database of this build opens");
        assert_eq!(std::fs::read(&path).expect("bytes"), before);
    }

    #[test]
    fn a_database_from_a_newer_build_is_not_openable() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("qoop.db");
        drop(Store::open(&path).expect("a database"));
        Connection::open(&path)
            .expect("open")
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("newer");
        assert!(matches!(
            Store::check_openable(&path),
            Err(StoreError::FromTheFuture { .. })
        ));
    }
}

//! Moving what has to outlive a reboot out of the runtime directory (ADR-005).
//!
//! A build before this one kept the database and the history beside the socket. The first
//! daemon of this build to start finds them there and moves them to `paths::state_dir`,
//! once, while it holds the startup lock and has bound the socket, which together prove no
//! other daemon has them open.

use std::io;
use std::path::Path;

/// Move the database and the history from `from` to `to`.
///
/// The history goes first and the database last, because the database arriving is what
/// says the move is done: a start interrupted in between finds the database still in
/// `from` and finishes. The database is copied with `VACUUM INTO`, which reads it through
/// SQLite with its write-ahead log applied, so there is never a moment where a database is
/// on disk without the log it needs. Nothing happens when `to` already has a database: it
/// is the one in use, and the one in `from` is left where it is and named.
pub fn migrate(from: &Path, to: &Path) -> io::Result<()> {
    // The name this build gives the database, then the one every build before the rename
    // gave it (see `legacy`): a database in either is moved, and renamed once it is there.
    for db in ["qoop.db", "qoro.db"] {
        migrate_db(from, to, db)?;
    }
    Ok(())
}

fn migrate_db(from: &Path, to: &Path, db: &str) -> io::Result<()> {
    let old = from.join(db);
    if from == to || !old.exists() {
        return Ok(());
    }
    let new = to.join(db);
    if new.exists() {
        if holds_tabs(&new)? {
            eprintln!(
                "qoopd: a database is in both {} and {}; using the second, the first is \
                 left as it is",
                from.display(),
                to.display()
            );
            return Ok(());
        }
        // One that never held a tab was made by a daemon of this build that started
        // somewhere with nothing to move (under another runtime directory, say). Taking it
        // as the real one would leave the real one where the system deletes it.
        for (suffix, aside) in [
            ("", ".empty"),
            ("-wal", ".empty-wal"),
            ("-shm", ".empty-shm"),
        ] {
            let name = format!("{db}{suffix}");
            if to.join(&name).exists() {
                std::fs::rename(to.join(&name), to.join(format!("{db}{aside}")))?;
            }
        }
    }
    move_dir(&from.join("history"), &to.join("history"))?;
    let moving = to.join(format!("{db}.moving"));
    remove_if_there(&moving)?;
    let target = moving
        .to_str()
        .ok_or_else(|| io::Error::other(format!("{} is not UTF-8", moving.display())))?;
    rusqlite::Connection::open(&old)
        .and_then(|db| {
            db.busy_timeout(std::time::Duration::from_secs(5))?;
            db.execute("VACUUM INTO ?1", [target])
        })
        .map_err(|e| {
            io::Error::other(format!(
                "could not move {} to {}: {e}; with qoop stopped, move the old file aside to \
                 start without it",
                old.display(),
                to.display()
            ))
        })?;
    // `VACUUM INTO` does not sync what it writes, and the old copy is deleted next: a power
    // cut between the two must not leave the only copy unwritten.
    std::fs::File::open(&moving)?.sync_all()?;
    std::fs::rename(&moving, &new)?;
    // Best effort: some filesystems refuse to sync a directory, and the data is synced.
    let _ = std::fs::File::open(to).and_then(|d| d.sync_all());
    for suffix in ["", "-wal", "-shm"] {
        remove_if_there(&from.join(format!("{db}{suffix}")))?;
    }
    eprintln!("qoopd: moved the database and history to {}", to.display());
    Ok(())
}

/// Whether a database has ever recorded a tab, open or closed. A file with no schema yet,
/// left by a start that died before creating it, has not.
fn holds_tabs(db: &Path) -> io::Result<bool> {
    rusqlite::Connection::open(db)
        .and_then(|c| {
            c.busy_timeout(std::time::Duration::from_secs(5))?;
            let schema: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'terminal')",
                [],
                |r| r.get(0),
            )?;
            if !schema {
                return Ok(false);
            }
            c.query_row("SELECT EXISTS (SELECT 1 FROM terminal)", [], |r| r.get(0))
        })
        .map_err(|e| {
            io::Error::other(format!(
                "could not read {}: {e}; with qoop stopped, move it aside to start without it",
                db.display()
            ))
        })
}

/// Move a directory, by rename where it can and by copy where it crosses a filesystem (a
/// Linux runtime directory is a tmpfs). A copy lands under a temporary name first, so a
/// half copied directory is never taken for the real one. An existing destination is
/// left alone: an interrupted earlier move already put it there.
fn move_dir(from: &Path, to: &Path) -> io::Result<()> {
    // An empty one is what a daemon that had nothing to move leaves: not in the way.
    if to.is_dir() && std::fs::read_dir(to)?.next().is_none() {
        std::fs::remove_dir(to)?;
    }
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        eprintln!(
            "qoopd: {} is already there, so {} was left where it is",
            to.display(),
            from.display()
        );
        return Ok(());
    }
    match std::fs::rename(from, to) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => copy_then_swap(from, to),
        other => other,
    }
}

/// The cross-filesystem half of `move_dir`: a copy under a temporary name, left over by an
/// interrupted start and thrown away if it is there, then renamed into place, and only
/// then the source removed.
fn copy_then_swap(from: &Path, to: &Path) -> io::Result<()> {
    let moving = to.with_extension("moving");
    if moving.exists() {
        std::fs::remove_dir_all(&moving)?;
    }
    copy_dir(from, &moving)?;
    std::fs::rename(&moving, to)?;
    std::fs::remove_dir_all(from)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database with a row, left the way a running daemon leaves one: in WAL mode, with
    /// the row still in the log, not yet checkpointed into the main file.
    fn old_layout(dir: &Path) -> rusqlite::Connection {
        let db = rusqlite::Connection::open(dir.join("qoop.db")).expect("open");
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE workspace (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO workspace (name) VALUES ('app');
             PRAGMA user_version = 2;",
        )
        .expect("seed");
        std::fs::create_dir_all(dir.join("history/7")).expect("history");
        std::fs::write(dir.join("history/7/0.seg"), b"scrollback").expect("segment");
        db
    }

    #[test]
    fn everything_moves_and_nothing_is_left_behind() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Held open while the move runs, so the row is only in the WAL: a move that copied
        // the main file alone would lose it.
        let _open = old_layout(from.path());
        assert!(from.path().join("qoop.db-wal").exists());

        migrate(from.path(), to.path()).expect("migrate");

        let db = rusqlite::Connection::open(to.path().join("qoop.db")).expect("moved db");
        let name: String = db
            .query_row("SELECT name FROM workspace", [], |r| r.get(0))
            .expect("the row came with it");
        assert_eq!(name, "app");
        let version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("version");
        assert_eq!(version, 2, "the schema version did not survive the copy");
        assert_eq!(
            std::fs::read(to.path().join("history/7/0.seg")).expect("history moved"),
            b"scrollback"
        );
        for gone in ["qoop.db", "qoop.db-wal", "qoop.db-shm", "history"] {
            assert!(!from.path().join(gone).exists(), "{gone} was left behind");
        }
        assert!(!to.path().join("qoop.db.moving").exists());
    }

    /// Run twice, the second start finds nothing to do and changes nothing.
    #[test]
    fn a_second_run_does_nothing() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        migrate(from.path(), to.path()).expect("migrate");
        migrate(from.path(), to.path()).expect("again");
        assert!(to.path().join("qoop.db").exists());
    }

    /// A start interrupted after the history moved finishes the move: the database is still
    /// in the old place, which is what says the move is not done.
    #[test]
    fn an_interrupted_move_is_finished() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        std::fs::rename(from.path().join("history"), to.path().join("history")).unwrap();
        std::fs::write(to.path().join("qoop.db.moving"), b"half a copy").unwrap();
        migrate(from.path(), to.path()).expect("finish");
        let db = rusqlite::Connection::open(to.path().join("qoop.db")).expect("db");
        let n: i64 = db
            .query_row("SELECT COUNT(*) FROM workspace", [], |r| r.get(0))
            .expect("a whole database, not the half copy");
        assert_eq!(n, 1);
        assert!(!from.path().join("qoop.db").exists());
    }

    /// A database already in the new place is the one in use: the old one is not copied
    /// over it, and is left where it is.
    #[test]
    fn a_database_already_moved_is_never_overwritten() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        rusqlite::Connection::open(to.path().join("qoop.db"))
            .unwrap()
            .execute_batch("CREATE TABLE terminal (id INTEGER); INSERT INTO terminal VALUES (5);")
            .unwrap();
        migrate(from.path(), to.path()).expect("migrate");
        let id: i64 = rusqlite::Connection::open(to.path().join("qoop.db"))
            .unwrap()
            .query_row("SELECT id FROM terminal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(id, 5, "the database in use was replaced");
        assert!(from.path().join("qoop.db").exists());
    }

    /// The copy used across filesystems, exercised directly: two temp directories are on
    /// one filesystem, so `rename` would never fall back to it here. A half copy left by
    /// an interrupted start is thrown away, not merged.
    #[test]
    fn the_cross_device_copy_keeps_every_file_and_drops_a_half_copy() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        std::fs::create_dir_all(to.path().join("history.moving/9")).unwrap();
        copy_then_swap(&from.path().join("history"), &to.path().join("history")).expect("copy");
        assert_eq!(
            std::fs::read(to.path().join("history/7/0.seg")).unwrap(),
            b"scrollback"
        );
        assert!(
            !to.path().join("history/9").exists(),
            "the half copy was merged in"
        );
        assert!(!to.path().join("history.moving").exists());
        assert!(!from.path().join("history").exists());
    }

    /// A file with no schema at all in the way, left by a start that died at once, is set
    /// aside too rather than stopping every start after it.
    #[test]
    fn a_file_with_no_schema_in_the_way_is_set_aside() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        std::fs::write(to.path().join("qoop.db"), b"").unwrap();
        migrate(from.path(), to.path()).expect("migrate");
        assert!(to.path().join("qoop.db.empty").exists());
        assert!(!from.path().join("qoop.db").exists());
    }

    /// A database that never held a tab, made by a daemon of this build that had nothing to
    /// move, does not shadow the real one: it is set aside and the real one moves.
    #[test]
    fn an_empty_database_in_the_way_is_set_aside() {
        let (from, to) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(old_layout(from.path()));
        rusqlite::Connection::open(from.path().join("qoop.db"))
            .unwrap()
            .execute_batch("CREATE TABLE terminal (id INTEGER); INSERT INTO terminal VALUES (1);")
            .unwrap();
        rusqlite::Connection::open(to.path().join("qoop.db"))
            .unwrap()
            .execute_batch("CREATE TABLE terminal (id INTEGER);")
            .unwrap();
        std::fs::create_dir(to.path().join("history")).unwrap();
        migrate(from.path(), to.path()).expect("migrate");
        let n: i64 = rusqlite::Connection::open(to.path().join("qoop.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM terminal", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the empty database was kept over the real one");
        assert!(to.path().join("qoop.db.empty").exists());
        assert!(to.path().join("history/7/0.seg").exists());
    }

    /// What a build before the rename left in the runtime directory moves like the rest.
    #[test]
    fn a_database_under_the_old_name_moves_too() {
        let from = tempfile::tempdir().unwrap();
        let to = tempfile::tempdir().unwrap();
        rusqlite::Connection::open(from.path().join("qoro.db"))
            .unwrap()
            .execute_batch("CREATE TABLE t (n); INSERT INTO t VALUES (3);")
            .unwrap();
        migrate(from.path(), to.path()).unwrap();
        assert!(!from.path().join("qoro.db").exists());
        let n: i64 = rusqlite::Connection::open(to.path().join("qoro.db"))
            .unwrap()
            .query_row("SELECT n FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
    }
}

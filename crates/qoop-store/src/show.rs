//! Shows: a result an agent put on the owner's phone with `qoop show`.
//!
//! A row is the record only: the path or url and what the daemon knew when it was shown. The
//! file itself is not copied, so a changed or deleted file reads as changed or gone. Ids come
//! from `AUTOINCREMENT`, so an id is never given twice, not even after the rows are pruned:
//! a phone keeps "seen up to" one.
//!
//! A show the phone asked for from a chat is a row too, marked in `show_asked`: fetched and
//! tunneled by its id like any other, but not listed, and pruned apart, so a chat full of
//! paths never pushes an agent's result out.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// How many shows are kept: the newest, pruned on every write.
pub const KEPT: i64 = 200;

/// How many shows the phone asked for are kept, apart from [`KEPT`].
pub const ASKED_KEPT: i64 = 200;

/// The rows that are not the phone's own.
const AGENTS: &str = "id NOT IN (SELECT show FROM show_asked)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShowRow {
    pub id: i64,
    pub at: i64,
    /// The daemon that recorded it, so a later one knows the terminal may be another's.
    pub boot: String,
    pub terminal: i64,
    /// The terminal's agent generation when it was shown.
    pub generation: i64,
    pub space: Option<String>,
    pub title: Option<String>,
    /// `image`, `video`, `file` or `url`.
    pub kind: String,
    /// The canonical path of a file, or the url.
    pub target: String,
    pub size: i64,
    pub note: Option<String>,
}

const COLUMNS: &str = "id, at, boot, terminal, generation, space, title, kind, target, size, note";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShowRow> {
    Ok(ShowRow {
        id: r.get(0)?,
        at: r.get(1)?,
        boot: r.get(2)?,
        terminal: r.get(3)?,
        generation: r.get(4)?,
        space: r.get(5)?,
        title: r.get(6)?,
        kind: r.get(7)?,
        target: r.get(8)?,
        size: r.get(9)?,
        note: r.get(10)?,
    })
}

fn insert(tx: &rusqlite::Transaction<'_>, row: &ShowRow) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO show (at, boot, terminal, generation, space, title, kind, target, size, note) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.at,
            row.boot,
            row.terminal,
            row.generation,
            row.space,
            row.title,
            row.kind,
            row.target,
            row.size,
            row.note
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

impl Store {
    /// Record a show and drop all but the newest [`KEPT`]. Answers its id; `row.id` is ignored.
    pub fn add_show(&self, row: &ShowRow) -> Result<i64, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let id = insert(&tx, row)?;
        tx.execute(
            &format!(
                "DELETE FROM show WHERE id IN \
                 (SELECT id FROM show WHERE {AGENTS} ORDER BY id DESC LIMIT -1 OFFSET ?1)"
            ),
            params![KEPT],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Record a show the phone asked for, whose file was modified at `mtime`, and drop all but
    /// the newest [`ASKED_KEPT`] of those. One asked before by this daemon from the same
    /// terminal for the same target, size and `mtime` is answered instead of a new row.
    pub fn ask_show(&self, row: &ShowRow, mtime: i64) -> Result<ShowRow, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let before = tx
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM show WHERE boot = ?1 AND target = ?2 AND size = ?3 \
                     AND terminal = ?5 AND id IN (SELECT show FROM show_asked WHERE mtime = ?4) \
                     ORDER BY id DESC LIMIT 1"
                ),
                params![row.boot, row.target, row.size, mtime, row.terminal],
                self::row,
            )
            .optional()?;
        if let Some(before) = before {
            return Ok(before);
        }
        let id = insert(&tx, row)?;
        tx.execute(
            "INSERT INTO show_asked (show, mtime) VALUES (?1, ?2)",
            params![id, mtime],
        )?;
        tx.execute(
            "DELETE FROM show WHERE id IN \
             (SELECT show FROM show_asked ORDER BY show DESC LIMIT -1 OFFSET ?1)",
            params![ASKED_KEPT],
        )?;
        tx.commit()?;
        Ok(ShowRow { id, ..row.clone() })
    }

    /// The newest shows, newest first, at most `limit`, the phone's own left out.
    pub fn shows(&self, limit: usize) -> Result<Vec<ShowRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM show WHERE {AGENTS} ORDER BY id DESC LIMIT ?1"
        ))?;
        Ok(stmt
            .query_map(params![limit as i64], row)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// One show by its id.
    pub fn show(&self, id: i64) -> Result<Option<ShowRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM show WHERE id = ?1"),
                params![id],
                row,
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(n: i64) -> ShowRow {
        ShowRow {
            id: 0,
            at: n,
            boot: "b".into(),
            terminal: 1,
            generation: 1,
            space: None,
            title: None,
            kind: "file".into(),
            target: format!("/tmp/{n}"),
            size: n,
            note: None,
        }
    }

    /// The newest 200 are kept, and an id is never given twice, a pruned one included.
    #[test]
    fn prune_keeps_the_newest_and_ids_keep_rising() {
        let s = Store::in_memory().expect("open");
        for n in 1..=205 {
            assert_eq!(s.add_show(&shown(n)).unwrap(), n);
        }
        let all = s.shows(1000).unwrap();
        assert_eq!(all.len(), KEPT as usize);
        assert_eq!(all[0].id, 205, "newest first");
        assert_eq!(all.last().unwrap().id, 6);
        assert!(s.show(5).unwrap().is_none(), "the oldest went");
        assert_eq!(s.show(205).unwrap().unwrap().target, "/tmp/205");
        s.conn.execute("DELETE FROM show", []).unwrap();
        assert_eq!(
            s.add_show(&shown(1)).unwrap(),
            206,
            "an id is not reused once its rows are gone"
        );
    }

    /// The phone's own shows are not listed, are pruned on their own cap without touching an
    /// agent's, and one asked again unchanged is the same row.
    #[test]
    fn asked_shows_stay_apart() {
        let s = Store::in_memory().expect("open");
        let agent = s.add_show(&shown(1)).unwrap();
        let asked = s.ask_show(&shown(2), 7).unwrap();
        assert_eq!(s.ask_show(&shown(2), 7).unwrap(), asked, "the same row");
        assert_ne!(
            s.ask_show(&shown(2), 8).unwrap().id,
            asked.id,
            "a newer file"
        );
        let other = ShowRow {
            terminal: 2,
            ..shown(2)
        };
        assert_ne!(
            s.ask_show(&other, 7).unwrap().id,
            asked.id,
            "another terminal"
        );
        let ids: Vec<i64> = s.shows(10).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(ids, [agent], "only the agent's is listed");
        assert_eq!(s.show(asked.id).unwrap().unwrap().target, "/tmp/2");
        for n in 0..ASKED_KEPT {
            s.ask_show(&shown(3), n).unwrap();
        }
        assert!(s.show(asked.id).unwrap().is_none(), "the oldest asked went");
        assert!(s.show(agent).unwrap().is_some(), "the agent's stays");
        let asked: i64 = s
            .conn
            .query_row("SELECT count(*) FROM show_asked", [], |r| r.get(0))
            .unwrap();
        assert_eq!(asked, ASKED_KEPT);
        // An agent's prune counts only agents' rows.
        for n in 0..KEPT {
            s.add_show(&shown(n)).unwrap();
        }
        assert_eq!(s.shows(1000).unwrap().len(), KEPT as usize);
        let all: i64 = s
            .conn
            .query_row("SELECT count(*) FROM show", [], |r| r.get(0))
            .unwrap();
        assert_eq!(all, KEPT + ASKED_KEPT, "no asked row went with them");
    }
}

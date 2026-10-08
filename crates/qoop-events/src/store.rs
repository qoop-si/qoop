//! The log itself.
//!
//! SQLite with WAL (ADR-005), one row per event, ordered per stream. Synchronous and
//! unaware of any runtime: the caller decides what runs on the blocking pool, because a
//! store that spawns its own tasks is a store that can be called from the reactor by
//! accident.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Body, Event, Stream};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        Self::prepare(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS event (
                 rowid_alias INTEGER PRIMARY KEY,
                 stream TEXT NOT NULL,
                 sequence INTEGER NOT NULL,
                 at_unix INTEGER NOT NULL,
                 body TEXT NOT NULL,
                 UNIQUE (stream, sequence)
             );
             CREATE INDEX IF NOT EXISTS event_by_stream ON event(stream, sequence);
             -- The idempotency ledger. Separate from the log because it is a lookup, and a
             -- lookup that scans the log grows slower exactly as the log grows.
             CREATE TABLE IF NOT EXISTS applied (
                 idempotency_key TEXT PRIMARY KEY,
                 outcome TEXT NOT NULL,
                 at_unix INTEGER NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// Append events. One transaction, so a batch either lands whole or not at all.
    ///
    /// The sequence is allocated here rather than by the caller: a caller that allocated it
    /// would have to hold a lock across its own work to keep it dense, and a sequence with
    /// holes cannot tell a reader whether it missed something.
    pub fn append(
        &mut self,
        at_unix: i64,
        entries: &[(Stream, Body)],
    ) -> Result<Vec<Event>, StoreError> {
        let tx = self.conn.transaction()?;
        let mut written = Vec::with_capacity(entries.len());
        for (stream, body) in entries {
            let key = stream.key();
            let next: i64 = tx.query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM event WHERE stream = ?1",
                params![key],
                |r| r.get(0),
            )?;
            let encoded = serde_json::to_string(body)?;
            tx.execute(
                "INSERT INTO event (stream, sequence, at_unix, body) VALUES (?1, ?2, ?3, ?4)",
                params![key, next, at_unix, encoded],
            )?;
            if let Body::Outcome {
                idempotency_key,
                ok,
                detail,
            } = body
            {
                // Recorded with the outcome, so a retry after a crash returns the original
                // answer rather than doing the work again.
                tx.execute(
                    "INSERT OR IGNORE INTO applied (idempotency_key, outcome, at_unix)
                     VALUES (?1, ?2, ?3)",
                    params![
                        idempotency_key,
                        serde_json::to_string(&(ok, detail))?,
                        at_unix
                    ],
                )?;
            }
            written.push(Event {
                sequence: next as u64,
                stream: stream.clone(),
                at_unix,
                body: body.clone(),
            });
        }
        tx.commit()?;
        Ok(written)
    }

    /// What a command with this key produced, if it has already been applied.
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

    /// Every event, in a deterministic order.
    ///
    /// Ordered by stream and then by sequence rather than by insertion, because insertion
    /// order is not a promise the log makes and a replay that depended on it would produce
    /// a different answer after a vacuum.
    pub fn replay(&self) -> Result<Vec<Event>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT stream, sequence, at_unix, body FROM event ORDER BY stream, sequence",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let stream: String = r.get(0)?;
                let sequence: i64 = r.get(1)?;
                let at_unix: i64 = r.get(2)?;
                let body: String = r.get(3)?;
                Ok((stream, sequence, at_unix, body))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (stream, sequence, at_unix, body) in rows {
            out.push(Event {
                sequence: sequence as u64,
                stream: parse_stream(&stream),
                at_unix,
                body: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    /// One stream, in order.
    pub fn stream(&self, stream: &Stream) -> Result<Vec<Event>, StoreError> {
        let key = stream.key();
        let mut stmt = self.conn.prepare(
            "SELECT sequence, at_unix, body FROM event WHERE stream = ?1 ORDER BY sequence",
        )?;
        let rows = stmt
            .query_map(params![key], |r| {
                let sequence: i64 = r.get(0)?;
                let at_unix: i64 = r.get(1)?;
                let body: String = r.get(2)?;
                Ok((sequence, at_unix, body))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (sequence, at_unix, body) in rows {
            out.push(Event {
                sequence: sequence as u64,
                stream: stream.clone(),
                at_unix,
                body: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    /// Forget every event and ledger row older than `before_unix`, in one transaction.
    ///
    /// The retention ADR-017 leaves to the caller. Sequences stay dense for what comes next,
    /// because `append` allocates from what remains.
    pub fn prune(&mut self, before_unix: i64) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM event WHERE at_unix < ?1", params![before_unix])?;
        tx.execute(
            "DELETE FROM applied WHERE at_unix < ?1",
            params![before_unix],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Commands with no outcome. What a restart has to reconcile.
    pub fn unreconciled(&self) -> Result<Vec<String>, StoreError> {
        let mut keys = Vec::new();
        for event in self.replay()? {
            if let Body::Command {
                idempotency_key, ..
            } = &event.body
                && self.applied(idempotency_key)?.is_none()
            {
                keys.push(idempotency_key.clone());
            }
        }
        Ok(keys)
    }
}

fn parse_stream(key: &str) -> Stream {
    match key.split_once(':') {
        Some(("w", id)) => Stream::Workspace(id.parse().unwrap_or(0)),
        Some(("t", id)) => Stream::Terminal(id.parse().unwrap_or(0)),
        _ => Stream::Command(key.trim_start_matches("c:").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> EventStore {
        EventStore::in_memory().expect("open")
    }

    fn command(key: &str) -> Body {
        Body::Command {
            idempotency_key: key.into(),
            operation: "CreateTerminal".into(),
            detail: serde_json::json!({ "cwd": "/tmp" }),
        }
    }

    #[test]
    fn sequences_are_dense_and_per_stream() {
        let mut s = store();
        s.append(
            100,
            &[
                (Stream::Terminal(1), command("a")),
                (Stream::Terminal(2), command("b")),
                (Stream::Terminal(1), command("c")),
            ],
        )
        .expect("append");
        let one = s.stream(&Stream::Terminal(1)).expect("stream");
        assert_eq!(
            one.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let two = s.stream(&Stream::Terminal(2)).expect("stream");
        assert_eq!(
            two.iter().map(|e| e.sequence).collect::<Vec<_>>(),
            vec![1],
            "one stream's traffic advanced another's sequence"
        );
    }

    #[test]
    fn an_outcome_records_what_a_retry_should_be_told() {
        let mut s = store();
        s.append(1, &[(Stream::Command("k1".into()), command("k1"))])
            .expect("append");
        assert_eq!(s.applied("k1").expect("lookup"), None, "not applied yet");

        s.append(
            2,
            &[(
                Stream::Command("k1".into()),
                Body::Outcome {
                    idempotency_key: "k1".into(),
                    ok: true,
                    detail: serde_json::json!({ "terminal": 7 }),
                },
            )],
        )
        .expect("append");
        let (ok, detail) = s.applied("k1").expect("lookup").expect("applied");
        assert!(ok);
        assert_eq!(detail["terminal"], 7);
    }

    #[test]
    fn a_command_with_no_outcome_is_what_a_restart_reconciles() {
        let mut s = store();
        s.append(
            1,
            &[
                (Stream::Command("done".into()), command("done")),
                (Stream::Command("lost".into()), command("lost")),
            ],
        )
        .expect("append");
        s.append(
            2,
            &[(
                Stream::Command("done".into()),
                Body::Outcome {
                    idempotency_key: "done".into(),
                    ok: true,
                    detail: serde_json::Value::Null,
                },
            )],
        )
        .expect("append");
        assert_eq!(s.unreconciled().expect("scan"), vec!["lost".to_string()]);
    }

    /// The sequence is allocated inside the transaction, so a row that appeared behind our
    /// back is continued from rather than collided with.
    ///
    /// A caller-allocated sequence would have to hold a lock across its own work to stay
    /// dense, and a sequence with holes cannot tell a reader whether it missed something.
    #[test]
    fn a_sequence_allocated_inside_the_transaction_never_collides() {
        let mut s = store();
        s.append(1, &[(Stream::Terminal(1), command("a"))])
            .expect("append");
        // A row inserted by something else, at the next sequence this store would have
        // chosen.
        let body = serde_json::to_string(&command("outside")).expect("encode");
        s.conn
            .execute(
                "INSERT INTO event (stream, sequence, at_unix, body) VALUES ('t:1', 2, 1, ?1)",
                params![body],
            )
            .expect("direct insert");

        s.append(3, &[(Stream::Terminal(1), command("c"))])
            .expect("the append must continue rather than collide");
        let sequences: Vec<u64> = s
            .stream(&Stream::Terminal(1))
            .expect("stream")
            .iter()
            .map(|e| e.sequence)
            .collect();
        assert_eq!(sequences, vec![1, 2, 3], "the sequence is not dense");
    }

    #[test]
    fn the_log_survives_reopening() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("events.db");
        {
            let mut s = EventStore::open(&path).expect("open");
            s.append(5, &[(Stream::Workspace(1), command("k"))])
                .expect("append");
        }
        let s = EventStore::open(&path).expect("reopen");
        let events = s.replay().expect("replay");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].at_unix, 5);
    }

    /// The log keeps a window, not everything: old events and old ledger rows go, newer
    /// ones stay, and a new event after a prune still gets the next sequence.
    #[test]
    fn a_prune_drops_what_is_older_than_the_cutoff_and_nothing_newer() {
        let mut s = store();
        let outcome = |key: &str| Body::Outcome {
            idempotency_key: key.into(),
            ok: true,
            detail: serde_json::json!({}),
        };
        s.append(10, &[(Stream::Command("old".into()), outcome("old"))])
            .expect("append");
        s.append(20, &[(Stream::Command("new".into()), outcome("new"))])
            .expect("append");
        s.append(20, &[(Stream::Terminal(1), command("t"))])
            .expect("append");

        s.prune(15).expect("prune");
        assert_eq!(s.replay().expect("replay").len(), 2);
        assert_eq!(s.applied("old").expect("lookup"), None);
        assert!(s.applied("new").expect("lookup").is_some());

        s.append(30, &[(Stream::Terminal(1), command("u"))])
            .expect("append");
        let sequences: Vec<u64> = s
            .stream(&Stream::Terminal(1))
            .expect("stream")
            .iter()
            .map(|e| e.sequence)
            .collect();
        assert_eq!(sequences, vec![1, 2]);
    }
}

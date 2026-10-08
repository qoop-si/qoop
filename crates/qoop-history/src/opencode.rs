//! Opencode sessions (`~/.local/share/opencode/opencode.db`).
//!
//! Opencode keeps every session in SQLite: `session` rows carry id,
//! directory and title; `message` rows carry the role; `part` rows carry the
//! content. Read with rusqlite 0.32, the version this workspace uses.
//!
//! The database is opened read-only with a short busy-timeout: a live
//! opencode writer makes us wait briefly, never corrupts, and a still-locked
//! database is an honest error rather than silent stale data.

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

use crate::Error;
use crate::model::{Message, Role};

/// How long a live writer may hold the lock before we give up honestly.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// Messages of one opencode session, oldest first, plus a corrupt-row
/// count. `None` when the id is unknown; corrupt rows inside are skipped
/// and counted, never fatal.
pub fn read_session(
    db_path: &Path,
    session_id: &str,
) -> Result<Option<(Vec<Message>, usize)>, Error> {
    // Raw rusqlite text stays in errors: it names operations on the user's own
    // database in the user's own terminal, where it debugs lock and schema
    // failures. Located transcript paths (the probe-able part) never enter any
    // error; see `locate::checked`.
    let conn = open(db_path)?;
    if !session_exists(&conn, session_id)? {
        return Ok(None);
    }
    Ok(Some(read_messages(&conn, session_id)?))
}

fn session_exists(conn: &Connection, session_id: &str) -> Result<bool, Error> {
    let mut stmt = conn
        .prepare("SELECT 1 FROM session WHERE id=?1")
        .map_err(|e| Error::Store(e.to_string()))?;
    let found: Result<Option<i64>, _> = stmt
        .query_row([session_id], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        });
    found
        .map(|v| v.is_some())
        .map_err(|e| Error::Store(e.to_string()))
}

fn read_messages(conn: &Connection, session_id: &str) -> Result<(Vec<Message>, usize), Error> {
    let mut ids: Vec<(String, String, i64)> = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, data, time_created FROM message
                 WHERE session_id=?1 ORDER BY time_created, id",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([session_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        for row in rows {
            ids.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
    }
    let mut out = Vec::with_capacity(ids.len());
    let mut skipped = 0;
    // One statement for every message's parts, not one per message.
    let mut pstmt = conn
        .prepare(
            "SELECT data FROM part
             WHERE message_id=?1 ORDER BY time_created, id",
        )
        .map_err(|e| Error::Store(e.to_string()))?;
    for (mid, data, ts) in ids {
        let Ok(v) = serde_json::from_str::<Value>(&data) else {
            skipped += 1;
            continue;
        };
        // Only conversation sides become messages; system/developer rows
        // cannot be represented honestly and are skipped, not relabeled.
        let role = match v.get("role").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let created = v
            .get("time")
            .and_then(|t| t.get("created"))
            .and_then(Value::as_i64)
            .unwrap_or(ts);
        let mut texts = Vec::new();
        let prows = pstmt
            .query_map([mid.as_str()], |r| r.get::<_, String>(0))
            .map_err(|e| Error::Store(e.to_string()))?;
        for row in prows {
            let data = row.map_err(|e| Error::Store(e.to_string()))?;
            let Ok(pv) = serde_json::from_str::<Value>(&data) else {
                skipped += 1;
                continue;
            };
            if let Some(t) = map_part(&pv)
                && !t.is_empty()
            {
                texts.push(t);
            }
        }
        let text = texts.join("\n");
        if text.is_empty() {
            continue;
        }
        out.push(Message {
            role,
            text,
            ts: Some(created),
        });
    }
    Ok((out, skipped))
}

/// The database opened the way every read here opens it: read only, a short wait on a writer.
pub(crate) fn open(db_path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::Store(e.to_string()))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| Error::Store(e.to_string()))?;
    Ok(conn)
}

/// Every top-level session: `(id, directory, last update in millis)`. A child session (a
/// subagent's) carries its parent's prompts, not a person's, so it is left out where the
/// schema says which those are.
pub(crate) fn top_sessions(conn: &Connection) -> Result<Vec<(String, String, i64)>, Error> {
    let has_parent = conn
        .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name='parent_id'")
        .and_then(|mut s| s.exists([]))
        .map_err(|e| Error::Store(e.to_string()))?;
    let sql = if has_parent {
        "SELECT id, directory, time_updated FROM session WHERE parent_id IS NULL"
    } else {
        "SELECT id, directory, time_updated FROM session"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| Error::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| Error::Store(e.to_string()))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| Error::Store(e.to_string()))
}

/// What the person typed in one session, oldest first: the text parts of user messages,
/// never the synthetic ones opencode adds itself. The role is read in SQL, so the large
/// assistant parts are never parsed.
pub(crate) fn typed(conn: &Connection, session_id: &str) -> Result<Vec<Message>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.time_created, p.data FROM message m JOIN part p ON p.message_id = m.id
             WHERE m.session_id = ?1 AND json_extract(m.data, '$.role') = 'user'
             ORDER BY m.time_created, m.id, p.id",
        )
        .map_err(|e| Error::Store(e.to_string()))?;
    let rows = stmt
        .query_map([session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| Error::Store(e.to_string()))?;
    let mut out: Vec<Message> = Vec::new();
    let mut last_id = String::new();
    for row in rows {
        let (mid, ts, data) = row.map_err(|e| Error::Store(e.to_string()))?;
        let Ok(part) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if part.get("type").and_then(Value::as_str) != Some("text")
            || part.get("synthetic").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(m) if mid == last_id => {
                m.text.push('\n');
                m.text.push_str(text);
            }
            _ => out.push(Message {
                role: Role::User,
                text: text.to_string(),
                ts: Some(ts),
            }),
        }
        last_id = mid;
    }
    Ok(out)
}

/// One part to display text. Reasoning and file bytes are excluded (see
/// `model.rs`); tool calls keep their name; patches keep paths.
fn map_part(p: &Value) -> Option<String> {
    match p.get("type").and_then(Value::as_str).unwrap_or("") {
        "text" => {
            let text = p.get("text").and_then(Value::as_str).unwrap_or("");
            (!text.is_empty()).then(|| text.to_string())
        }
        "tool" => p.get("tool").and_then(Value::as_str).map(str::to_string),
        "patch" => {
            let files = p
                .get("files")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if files.is_empty() {
                None
            } else {
                Some(format!("edited files:\n{files}"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, title TEXT,
                version TEXT, slug TEXT, time_created INTEGER, time_updated INTEGER, time_compacting INTEGER);
             CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                time_updated INTEGER, data TEXT);
             CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                time_created INTEGER, time_updated INTEGER, data TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session(id, project_id, directory, title, version, slug, time_created, time_updated, time_compacting)
             VALUES ('ses_1','global','/Users/x/proj','My title','v','s',1000,2000,NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message(id, session_id, time_created, time_updated, data)
             VALUES ('m1','ses_1',1000,1001,'{\"role\":\"user\",\"time\":{\"created\":1000}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part(id, message_id, session_id, time_created, time_updated, data)
             VALUES ('p1','m1','ses_1',1000,1001,'{\"type\":\"text\",\"text\":\"do the thing\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message(id, session_id, time_created, time_updated, data)
             VALUES ('m2','ses_1',1002,1003,'{\"role\":\"assistant\",\"time\":{\"created\":1002}}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part(id, message_id, session_id, time_created, time_updated, data)
             VALUES ('p2','m2','ses_1',1002,1003,'{\"type\":\"step-start\"}')",
            [],
        )
        .unwrap();
        drop(conn);
        (dir, db)
    }

    #[test]
    fn reads_one_session_protocol_rows_dropped() {
        let (_dir, db) = scratch_db();
        let (msgs, skipped) = read_session(&db, "ses_1").unwrap().expect("known id");
        assert_eq!(msgs.len(), 1);
        assert_eq!(skipped, 0);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].text, "do the thing");
        assert_eq!(msgs[0].ts, Some(1000));
    }

    #[test]
    fn unknown_id_is_none() {
        let (_dir, db) = scratch_db();
        assert!(read_session(&db, "ses_nope").unwrap().is_none());
    }
}

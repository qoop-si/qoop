//! Usage counts (ADR-023): numbers per UTC day, and the few values the sender keeps.
//!
//! `telemetry_day` holds one number per day and key. A count adds to it, a gauge keeps the
//! larger of the two, so a restart in the middle of a day loses nothing. `telemetry_kv` holds
//! the install id, the next send time, each day's version and platform, and how often the hub
//! refused a day. Keys are `&'static str`: what can become a key is decided in the runtime's
//! fixed table, never by a value that came from somewhere else.

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

impl Store {
    /// Add `by` to a day's key.
    pub fn telemetry_bump(&self, day: &str, key: &'static str, by: f64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO telemetry_day (day, key, n) VALUES (?1, ?2, ?3)
             ON CONFLICT (day, key) DO UPDATE SET n = n + excluded.n",
            params![day, key, by],
        )?;
        Ok(())
    }

    /// Keep the larger of a day's key and `value`.
    pub fn telemetry_peak(
        &self,
        day: &str,
        key: &'static str,
        value: f64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO telemetry_day (day, key, n) VALUES (?1, ?2, ?3)
             ON CONFLICT (day, key) DO UPDATE SET n = max(n, excluded.n)",
            params![day, key, value],
        )?;
        Ok(())
    }

    /// Every key of a day with its number.
    pub fn telemetry_day(&self, day: &str) -> Result<Vec<(String, f64)>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT key, n FROM telemetry_day WHERE day = ?1 ORDER BY key")?;
        let rows = statement.query_map(params![day], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The days that have numbers, oldest first.
    pub fn telemetry_days(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT DISTINCT day FROM telemetry_day ORDER BY day")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forget one day: its numbers and what was kept for it.
    pub fn telemetry_drop_day(&self, day: &str) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM telemetry_day WHERE day = ?1", params![day])?;
        tx.execute(
            "DELETE FROM telemetry_kv WHERE key IN (?1, ?2)",
            params![format!("meta:{day}"), format!("refused:{day}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Forget everything: every day, the id, the send time. What turning it off does.
    pub fn telemetry_purge(&self) -> Result<(), StoreError> {
        self.conn
            .execute_batch("DELETE FROM telemetry_day; DELETE FROM telemetry_kv;")?;
        Ok(())
    }

    pub fn telemetry_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM telemetry_kv WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn telemetry_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO telemetry_kv (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Keep `value` unless the key already has one; answers what the key holds afterwards.
    /// A day's version and platform are the ones of its first count.
    pub fn telemetry_keep(&self, key: &str, value: &str) -> Result<String, StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO telemetry_kv (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(self
            .telemetry_get(key)?
            .unwrap_or_else(|| value.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    #[test]
    fn a_count_adds_and_a_gauge_keeps_the_larger() {
        let s = Store::in_memory().unwrap();
        s.telemetry_bump("2026-10-24", "terms_opened", 2.0).unwrap();
        s.telemetry_bump("2026-10-24", "terms_opened", 3.0).unwrap();
        s.telemetry_peak("2026-10-24", "agents_max", 4.0).unwrap();
        s.telemetry_peak("2026-10-24", "agents_max", 2.0).unwrap();
        s.telemetry_bump("2026-10-25", "terms_opened", 1.0).unwrap();
        assert_eq!(
            s.telemetry_day("2026-10-24").unwrap(),
            vec![("agents_max".into(), 4.0), ("terms_opened".into(), 5.0)]
        );
        assert_eq!(
            s.telemetry_days().unwrap(),
            vec!["2026-10-24", "2026-10-25"]
        );
    }

    #[test]
    fn a_day_meta_is_its_first_and_a_dropped_day_takes_its_meta() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.telemetry_keep("meta:2026-10-24", "a").unwrap(), "a");
        assert_eq!(s.telemetry_keep("meta:2026-10-24", "b").unwrap(), "a");
        s.telemetry_set("refused:2026-10-24", "2").unwrap();
        s.telemetry_set("id", "x").unwrap();
        s.telemetry_bump("2026-10-24", "terms_opened", 1.0).unwrap();
        s.telemetry_drop_day("2026-10-24").unwrap();
        assert!(s.telemetry_days().unwrap().is_empty());
        assert_eq!(s.telemetry_get("meta:2026-10-24").unwrap(), None);
        assert_eq!(s.telemetry_get("refused:2026-10-24").unwrap(), None);
        assert_eq!(s.telemetry_get("id").unwrap().as_deref(), Some("x"));
        s.telemetry_purge().unwrap();
        assert_eq!(s.telemetry_get("id").unwrap(), None);
    }
}

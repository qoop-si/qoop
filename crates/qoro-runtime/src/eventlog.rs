//! The event log, seen from the runtime.
//!
//! Same shape as the metadata store and for the same reason: `qoro-events` is synchronous
//! and knows nothing about tokio, and this is the half that decides what runs where. Every
//! call goes through the daemon's one bounded blocking pool (ADR-011), so the thread count
//! does not grow with the workload.
//!
//! The durability rules are ADR-005's and they differ by kind, which is why they are
//! separate methods rather than one `append`. A command that returned before it was durable
//! would make a retry unsafe, and an observation that waited for the disk would put storage
//! on the path of a terminal.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use qoro_events::{Body, Event, EventStore, Projection, Stream, WorkspaceView};

use crate::blocking::BlockingError;

/// How long a durable append may take before the caller stops waiting.
pub const APPEND_DEADLINE: Duration = Duration::from_secs(5);

/// Observations waiting to be written, and what was lost while waiting.
///
/// The same shape as the history spill and for the same reason: nothing on a terminal's
/// path may wait for a disk, so the handover is a bounded buffer with a drop counter.
pub const OBSERVATION_CAPACITY: usize = 4096;

/// How long observations gather before the daemon writes them, as one batch.
pub const OBSERVATION_BATCH: Duration = Duration::from_secs(1);

/// How long the log keeps an event. The idempotency ledger only has to outlive a client's
/// retry, which is seconds, and forensics on a reap or a crash look back days, not months.
pub const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("the log did not answer within {APPEND_DEADLINE:?}")]
    Unavailable,
    #[error("{0}")]
    Store(String),
}

impl From<BlockingError> for LogError {
    fn from(_: BlockingError) -> Self {
        LogError::Unavailable
    }
}

#[derive(Default)]
struct Pending {
    entries: Vec<(Stream, Body)>,
    dropped: u64,
    first_unix: i64,
    last_unix: i64,
}

#[derive(Clone)]
pub struct EventLog {
    store: Arc<Mutex<EventStore>>,
    pending: Arc<Mutex<Pending>>,
    notify: Arc<tokio::sync::Notify>,
    /// Woken whenever anything is appended or dropped, so a waiter has something to wake on
    /// that is not a timer.
    progress: Arc<tokio::sync::Notify>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl EventLog {
    pub fn new(store: EventStore) -> Self {
        Self {
            store: Arc::new(Mutex::new(store)),
            pending: Arc::default(),
            notify: Arc::default(),
            progress: Arc::default(),
        }
    }

    /// Append and wait for it to be durable.
    ///
    /// For commands and outcomes only. ADR-005 rule 2: a client is told a command succeeded
    /// only once the log says so, which is what makes an idempotent retry meaningful.
    pub async fn append_durable(
        &self,
        entries: Vec<(Stream, Body)>,
    ) -> Result<Vec<Event>, LogError> {
        let store = self.store.clone();
        let at = now_unix();
        let written = crate::blocking::run_within(APPEND_DEADLINE, move || {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .append(at, &entries)
                .map_err(|e| e.to_string())
        })
        .await?;
        written.map_err(LogError::Store)
    }

    /// Record an observation. Never waits, and never fails.
    ///
    /// Lossy under pressure by design (ADR-005 rule 3). The loss is O(1) state, so
    /// recording it never needs a slot in the buffer that is by definition full, which is
    /// the failure the whole mechanism exists to prevent.
    pub fn observe(&self, stream: Stream, body: Body) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.entries.len() >= OBSERVATION_CAPACITY {
            let now = now_unix();
            if pending.dropped == 0 {
                pending.first_unix = now;
            }
            pending.dropped += 1;
            pending.last_unix = now;
            drop(pending);
            // A drop is progress too, and it is the kind a waiter most needs to hear about:
            // the thing it is waiting for may be exactly what was dropped.
            self.progress.notify_waiters();
            return;
        }
        pending.entries.push((stream, body));
        drop(pending);
        self.notify.notify_one();
        self.progress.notify_waiters();
    }

    /// Write what is waiting. Returns how many events were written.
    pub async fn flush(&self) -> Result<usize, LogError> {
        let (mut entries, dropped, first_unix, last_unix) = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entries = std::mem::take(&mut pending.entries);
            let dropped = std::mem::take(&mut pending.dropped);
            (entries, dropped, pending.first_unix, pending.last_unix)
        };
        let observed = entries.len() as u64;
        if dropped > 0 {
            // Appended alongside what did fit, not through the buffer it describes: a gap
            // pushed into a full buffer is lost with the observations it is about.
            entries.push((
                Stream::Workspace(0),
                Body::Gap {
                    dropped,
                    first_unix,
                    last_unix,
                    reason: "observation buffer full".into(),
                },
            ));
        }
        if entries.is_empty() {
            return Ok(0);
        }
        let count = entries.len();
        if let Err(e) = self.append_durable(entries).await {
            // The batch is gone, which is a loss like any other (ADR-005 rule 3): counted
            // back, so the next flush that lands writes the gap. A flush that timed out but
            // landed anyway is then counted twice, which errs on the safe side: a reader
            // told of a gap re-reads rather than trusting what it folded.
            let now = now_unix();
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = if dropped > 0 { first_unix } else { now };
            pending.first_unix = if pending.dropped == 0 {
                first
            } else {
                pending.first_unix.min(first)
            };
            pending.last_unix = pending.last_unix.max(now);
            pending.dropped += observed + dropped;
            return Err(e);
        }
        Ok(count)
    }

    pub async fn wait_for_observations(&self) {
        self.notify.notified().await
    }

    /// Wait until a terminal's believed state satisfies a predicate.
    ///
    /// ADR-017: a reader that sees a gap knows its folded state may be behind the world and
    /// that the fix is to re-read rather than to wait for more events. This is what that
    /// means in practice, and gate G4.5 is why it has to: an observation that would have
    /// satisfied the predicate can be exactly the one that was dropped under pressure, and
    /// a waiter that only ever folds forward waits forever for an event that no longer
    /// exists.
    ///
    /// So every wake, whether it was an append or a drop, re-reads the projection. That is
    /// more work per wake than folding an increment, and it is the only version that
    /// terminates.
    pub async fn wait_for_state(
        &self,
        terminal: u32,
        predicate: impl Fn(&qoro_events::projection::AgentSummary) -> bool,
        within: Duration,
    ) -> Result<bool, LogError> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            // Registered before the read, so an append between the read and the wait is not
            // missed.
            let woken = self.progress.notified();
            tokio::pin!(woken);

            self.flush().await?;
            let view = self.workspace_view().await?;
            if view.agent_state.get(&terminal).is_some_and(&predicate) {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            // A timeout as well as the notify, because a gap recorded before this waiter
            // existed produces no wake at all.
            let _ = tokio::time::timeout(remaining.min(Duration::from_millis(200)), woken).await;
        }
    }

    pub fn pending_observations(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }

    /// How many observations have been dropped since the last flush.
    ///
    /// A gate needs this to arrange an overflow on purpose rather than by hoping: the buffer
    /// drops what arrives while it is full, so whether a particular observation was lost is
    /// a fact about the buffer at that moment, not about the size of the burst around it.
    pub fn dropped_observations(&self) -> u64 {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dropped
    }

    /// Rebuild a projection from the whole log.
    pub async fn project<P: Projection + Send + 'static>(&self) -> Result<P, LogError> {
        let store = self.store.clone();
        let events = crate::blocking::run_within(APPEND_DEADLINE, move || {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replay()
                .map_err(|e| e.to_string())
        })
        .await?
        .map_err(LogError::Store)?;
        Ok(P::rebuild(&events))
    }

    pub async fn workspace_view(&self) -> Result<WorkspaceView, LogError> {
        self.project::<WorkspaceView>().await
    }

    /// What a command with this key already produced, if anything.
    pub async fn applied(
        &self,
        key: String,
    ) -> Result<Option<(bool, serde_json::Value)>, LogError> {
        let store = self.store.clone();
        crate::blocking::run_within(APPEND_DEADLINE, move || {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .applied(&key)
                .map_err(|e| e.to_string())
        })
        .await?
        .map_err(LogError::Store)
    }

    /// Commands with no outcome. What a restart has to reconcile.
    pub async fn unreconciled(&self) -> Result<Vec<String>, LogError> {
        let store = self.store.clone();
        crate::blocking::run_within(APPEND_DEADLINE, move || {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unreconciled()
                .map_err(|e| e.to_string())
        })
        .await?
        .map_err(LogError::Store)
    }

    /// Forget what is older than `RETENTION`, on the store pool like every other write.
    pub async fn prune(&self) -> Result<(), LogError> {
        let store = self.store.clone();
        let before = now_unix() - RETENTION.as_secs() as i64;
        crate::blocking::run_within(APPEND_DEADLINE, move || {
            store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .prune(before)
                .map_err(|e| e.to_string())
        })
        .await?
        .map_err(LogError::Store)
    }

    /// A gap covering the interval between the last durable event and now.
    ///
    /// Appended on every start after an unclean shutdown. The daemon cannot know what it
    /// missed, and a reader that assumed continuity across a restart would be wrong exactly
    /// when it matters most.
    pub async fn note_restart_gap(&self, since_unix: i64) -> Result<(), LogError> {
        self.append_durable(vec![(
            Stream::Workspace(0),
            Body::Gap {
                dropped: 0,
                first_unix: since_unix,
                last_unix: now_unix(),
                reason: "daemon restarted; the interval was not observed".into(),
            },
        )])
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> EventLog {
        EventLog::new(EventStore::in_memory().expect("open"))
    }

    fn observation(n: u64) -> Body {
        Body::Observation {
            terminal: 1,
            state: "Working".into(),
            source: "Hook".into(),
            confidence: "High".into(),
            sampled_generation: n,
            evidence: format!("step {n}"),
            redacted: false,
        }
    }

    #[tokio::test]
    async fn a_durable_append_is_readable_afterwards() {
        let l = log();
        l.append_durable(vec![(
            Stream::Command("k".into()),
            Body::Command {
                idempotency_key: "k".into(),
                operation: "CreateTerminal".into(),
                detail: serde_json::json!({ "terminal": 3 }),
            },
        )])
        .await
        .expect("append");
        let view = l.workspace_view().await.expect("project");
        assert_eq!(view.pending, vec!["k".to_string()]);
    }

    /// G4.2's rule at the level it is enforced: an observation never waits, and what is
    /// dropped is covered by a gap whose count matches.
    #[tokio::test]
    async fn dropped_observations_are_covered_by_a_gap_with_the_right_count() {
        let l = log();
        let over = 250;
        for i in 0..(OBSERVATION_CAPACITY + over) as u64 {
            l.observe(Stream::Terminal(1), observation(i));
        }
        assert_eq!(l.pending_observations(), OBSERVATION_CAPACITY);
        l.flush().await.expect("flush");

        let view = l.workspace_view().await.expect("project");
        assert_eq!(view.gaps.len(), 1, "the loss was not recorded");
        assert_eq!(
            view.gaps[0].dropped, over as u64,
            "the gap's count does not match what was dropped"
        );
    }

    #[tokio::test]
    async fn a_gap_is_reported_once_and_then_cleared() {
        let l = log();
        for i in 0..(OBSERVATION_CAPACITY + 5) as u64 {
            l.observe(Stream::Terminal(1), observation(i));
        }
        l.flush().await.expect("flush");
        l.observe(Stream::Terminal(1), observation(9999));
        l.flush().await.expect("flush");
        let view = l.workspace_view().await.expect("project");
        assert_eq!(
            view.gaps.len(),
            1,
            "the same loss was reported twice, which double counts it"
        );
    }

    /// ADR-005 rule 3 on the failure it is written for: a batch the disk refused is a loss,
    /// and the loss is counted so the next flush that lands records the gap.
    #[tokio::test]
    async fn a_batch_the_disk_refused_is_counted_as_dropped() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("events.db");
        let l = EventLog::new(EventStore::open(&path).expect("open"));
        for i in 0..3 {
            l.observe(Stream::Terminal(1), observation(i));
        }
        // Another connection takes the table away, so the append fails.
        rusqlite::Connection::open(&path)
            .expect("a second connection")
            .execute_batch("DROP TABLE event")
            .expect("drop");
        assert!(l.flush().await.is_err(), "the append was meant to fail");
        assert_eq!(
            l.dropped_observations(),
            3,
            "a failed flush lost its batch without counting it"
        );
    }

    /// G4.6's rule: the same key applied twice returns the original result.
    #[tokio::test]
    async fn a_replayed_command_returns_the_original_outcome() {
        let l = log();
        l.append_durable(vec![(
            Stream::Command("k".into()),
            Body::Outcome {
                idempotency_key: "k".into(),
                ok: true,
                detail: serde_json::json!({ "terminal": 9 }),
            },
        )])
        .await
        .expect("append");
        let (ok, detail) = l
            .applied("k".into())
            .await
            .expect("lookup")
            .expect("applied");
        assert!(ok);
        assert_eq!(detail["terminal"], 9);
    }
}

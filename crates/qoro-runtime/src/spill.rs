//! Scrolled-off rows on their way to disk.
//!
//! ADR-005's first rule is absolute: a PTY read, a render and a keystroke never wait for a
//! write. The read loop most of all, because an exiting child cannot complete until its tty
//! output is drained, so a terminal task that stops draining while awaiting a full queue
//! makes `exit` hang in the kernel.
//!
//! So the handover is a bounded ring with a drop counter, not a channel with backpressure.
//! When the disk cannot keep up, the oldest rows are dropped and the loss is recorded as a
//! count and a time range, which is O(1) state and therefore needs no slot in the buffer
//! that is by definition full. Recording the gap through the buffer it describes would lose
//! the gap along with the rows, which is the failure the mechanism exists to prevent.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Rows held between the read loop and the writer.
///
/// One 64 KiB read can scroll more than the whole in-memory window, so this is sized well
/// above the engine's 1000 row scrollback rather than to a round number.
pub const SPILL_CAPACITY: usize = 8192;

/// What was lost, and when. Never a bare count: a gap with no time range cannot be placed
/// against anything else in the record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Gap {
    pub rows: u64,
    pub first_unix: i64,
    pub last_unix: i64,
}

#[derive(Default)]
struct Inner {
    rows: VecDeque<String>,
    gap: Gap,
}

/// The handover itself. Cloning shares it; the read loop pushes and the writer drains.
#[derive(Clone, Default)]
pub struct SpillRing {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<tokio::sync::Notify>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl SpillRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand over rows. Never blocks and never awaits.
    ///
    /// Rows past the capacity displace the oldest, because the oldest are the ones already
    /// furthest from the screen. Dropping the newest instead would lose the rows a person is
    /// most likely to scroll back to.
    pub fn push(&self, rows: impl IntoIterator<Item = String>) {
        let mut inner = self.inner.lock().unwrap();
        let now = now_unix();
        for row in rows {
            if inner.rows.len() >= SPILL_CAPACITY {
                inner.rows.pop_front();
                if inner.gap.rows == 0 {
                    inner.gap.first_unix = now;
                }
                inner.gap.rows += 1;
                inner.gap.last_unix = now;
            }
            inner.rows.push_back(row);
        }
        drop(inner);
        self.notify.notify_one();
    }

    /// Take everything waiting, and the gap accumulated since the last take.
    pub fn take(&self) -> (Vec<String>, Gap) {
        let mut inner = self.inner.lock().unwrap();
        let rows = inner.rows.drain(..).collect();
        let gap = std::mem::take(&mut inner.gap);
        (rows, gap)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn wait(&self) {
        self.notify.notified().await
    }

    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Whether this is the last handle, so nobody will push again. The writer's cue to
    /// finish: it parks on `wait` holding its own clone, and without this it waited for
    /// the rest of the daemon's life after its terminal ended, holding the files open.
    pub fn orphaned(&self) -> bool {
        Arc::strong_count(&self.inner) == 1
    }
}

/// Dropping a handle wakes the other one, so the writer notices it is the last. A stored
/// permit covers a drop that lands while the writer is busy writing. The shared rows are
/// let go first: fields drop after this runs, so notifying first woke a writer that could
/// still count two handles, park again, and never be woken.
impl Drop for SpillRing {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.inner));
        self.notify.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_writer_is_woken_and_orphaned_when_the_terminal_side_drops() {
        let writer = SpillRing::new();
        let terminal = writer.clone();
        assert!(!writer.orphaned());
        drop(terminal);
        tokio::time::timeout(std::time::Duration::from_secs(1), writer.wait())
            .await
            .expect("the drop woke the writer");
        assert!(writer.orphaned());
    }

    #[test]
    fn rows_come_out_in_the_order_they_went_in() {
        let ring = SpillRing::new();
        ring.push((0..10).map(|i| i.to_string()));
        let (rows, gap) = ring.take();
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0], "0");
        assert_eq!(gap.rows, 0);
    }

    #[test]
    fn overflowing_drops_the_oldest_and_counts_it() {
        let ring = SpillRing::new();
        ring.push((0..SPILL_CAPACITY + 100).map(|i| i.to_string()));
        let (rows, gap) = ring.take();
        assert_eq!(rows.len(), SPILL_CAPACITY);
        assert_eq!(gap.rows, 100, "the loss was not counted");
        assert_eq!(
            rows[0], "100",
            "the newest rows were dropped instead of the oldest"
        );
        assert!(gap.first_unix > 0 && gap.last_unix >= gap.first_unix);
    }

    #[test]
    fn a_gap_is_reported_once_and_then_cleared() {
        let ring = SpillRing::new();
        ring.push((0..SPILL_CAPACITY + 5).map(|i| i.to_string()));
        assert_eq!(ring.take().1.rows, 5);
        ring.push(["after".to_string()]);
        assert_eq!(
            ring.take().1.rows,
            0,
            "the same gap was reported twice, which would double count the loss"
        );
    }

    #[test]
    fn pushing_more_than_the_capacity_at_once_is_bounded_not_grown() {
        let ring = SpillRing::new();
        ring.push((0..SPILL_CAPACITY * 4).map(|i| i.to_string()));
        assert_eq!(ring.len(), SPILL_CAPACITY);
    }
}

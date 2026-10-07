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

/// How much the ring holds between the read loop and the writer, in bytes.
///
/// A budget in bytes and not a count of rows, because what a flood costs is its size: a
/// burst of short lines scrolls millions of rows a second, and a ring of 8192 rows (the
/// first sizing) overflowed within about a millisecond of the writer looking away, so
/// 100,000 lines of `seq` reached the disk as 8,192. Eight MiB absorbs such a burst of
/// short lines whole (about 150,000 of them) and still bounds a terminal that floods for
/// longer; what it cannot hold is dropped and recorded, never waited for.
pub const SPILL_BYTES: usize = 8 * 1024 * 1024;

/// What a row costs besides its text: the `String` and the slot it sits in.
const ROW_OVERHEAD: usize = 48;

/// How full the ring may get before the writer stops waiting for a burst to gather.
///
/// The writer waits a short window after the first rows of a burst so that the burst
/// becomes one batch. Past this mark it drains at once instead, so the ring empties long
/// before it could overflow while the read loop still never waits.
pub const SPILL_PRESSURE: usize = SPILL_BYTES / 4;

fn cost(row: &str) -> usize {
    row.len() + ROW_OVERHEAD
}

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
    /// What `rows` costs against [`SPILL_BYTES`].
    bytes: usize,
    gap: Gap,
}

/// The handover itself. Cloning shares it; the read loop pushes and the writer drains.
#[derive(Clone)]
pub struct SpillRing {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<tokio::sync::Notify>,
    /// Woken when the ring passes [`SPILL_PRESSURE`], so a writer waiting out its batch
    /// window drains now.
    pressure: Arc<tokio::sync::Notify>,
    /// How many times a flush was asked for, and the last one the writer has done.
    asked: Arc<std::sync::atomic::AtomicU64>,
    done: Arc<tokio::sync::watch::Sender<u64>>,
}

impl Default for SpillRing {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            notify: Arc::default(),
            pressure: Arc::default(),
            asked: Arc::default(),
            done: Arc::new(tokio::sync::watch::channel(0).0),
        }
    }
}

/// A way to ask the writer of a ring to put everything handed to it on disk, and to hear
/// when it has. Not a handle on the rows: holding one never keeps a ring from being
/// orphaned, so a writer still finishes when its terminal is gone.
#[derive(Clone)]
pub struct Flusher {
    asked: Arc<std::sync::atomic::AtomicU64>,
    done: Arc<tokio::sync::watch::Sender<u64>>,
    wake: Arc<tokio::sync::Notify>,
}

impl Flusher {
    /// Ask for a flush and wait for it, up to `within`. True when everything handed over
    /// before the call is on disk.
    pub async fn flush(&self, within: std::time::Duration) -> bool {
        use std::sync::atomic::Ordering;
        let n = self.asked.fetch_add(1, Ordering::SeqCst) + 1;
        let mut done = self.done.subscribe();
        self.wake.notify_one();
        tokio::time::timeout(within, done.wait_for(|d| *d >= n))
            .await
            .is_ok_and(|reached| reached.is_ok())
    }
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
            inner.bytes += cost(&row);
            inner.rows.push_back(row);
            while inner.bytes > SPILL_BYTES
                && let Some(oldest) = inner.rows.pop_front()
            {
                inner.bytes -= cost(&oldest);
                if inner.gap.rows == 0 {
                    inner.gap.first_unix = now;
                }
                inner.gap.rows += 1;
                inner.gap.last_unix = now;
            }
        }
        let pressed = inner.bytes >= SPILL_PRESSURE;
        drop(inner);
        self.notify.notify_one();
        if pressed {
            self.pressure.notify_one();
        }
    }

    /// Take everything waiting, and the gap accumulated since the last take.
    pub fn take(&self) -> (Vec<String>, Gap) {
        let mut inner = self.inner.lock().unwrap();
        let rows = inner.rows.drain(..).collect();
        inner.bytes = 0;
        let gap = std::mem::take(&mut inner.gap);
        (rows, gap)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().rows.len()
    }

    /// Whether this many bytes of rows are waiting: the writer's cue to stop gathering.
    pub fn pressed(&self) -> bool {
        self.inner.lock().unwrap().bytes >= SPILL_PRESSURE
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn wait(&self) {
        self.notify.notified().await
    }

    /// Wait until the ring is past [`SPILL_PRESSURE`]. A stale wake left by an earlier
    /// burst only makes the next batch smaller.
    pub async fn wait_pressure(&self) {
        self.pressure.notified().await
    }

    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// A way for somebody else to ask this ring's writer to flush.
    pub fn flusher(&self) -> Flusher {
        Flusher {
            asked: self.asked.clone(),
            done: self.done.clone(),
            wake: self.notify.clone(),
        }
    }

    /// How many flushes have been asked for. Read by the writer before it takes the rows,
    /// so a flush it reports done covers everything pushed before the ask.
    pub fn flushes_asked(&self) -> u64 {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Say that every flush up to `n` is done.
    pub fn flushed_to(&self, n: u64) {
        self.done.send_replace(n);
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

    #[tokio::test]
    async fn a_flush_is_done_when_the_writer_says_so_and_covers_what_was_pushed_before() {
        let ring = SpillRing::new();
        let writer = ring.clone();
        let flusher = ring.flusher();
        // Nobody answers: the ask runs out and says so.
        assert!(!flusher.flush(std::time::Duration::from_millis(30)).await);
        tokio::spawn(async move {
            loop {
                writer.wait().await;
                let asked = writer.flushes_asked();
                let _ = writer.take();
                writer.flushed_to(asked);
            }
        });
        ring.push(["a".to_string()]);
        assert!(flusher.flush(std::time::Duration::from_secs(2)).await);
        assert!(ring.is_empty(), "the flush came before the rows were taken");
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

    /// How many eight character rows fit in the ring.
    fn fitting() -> usize {
        SPILL_BYTES / cost("00000000")
    }

    #[test]
    fn overflowing_drops_the_oldest_and_counts_it() {
        let ring = SpillRing::new();
        ring.push((0..fitting() + 100).map(|i| format!("{i:08}")));
        let (rows, gap) = ring.take();
        assert_eq!(rows.len(), fitting());
        assert_eq!(gap.rows, 100, "the loss was not counted");
        assert_eq!(
            rows[0], "00000100",
            "the newest rows were dropped instead of the oldest"
        );
        assert!(gap.first_unix > 0 && gap.last_unix >= gap.first_unix);
    }

    #[test]
    fn a_gap_is_reported_once_and_then_cleared() {
        let ring = SpillRing::new();
        ring.push((0..fitting() + 5).map(|i| format!("{i:08}")));
        assert_eq!(ring.take().1.rows, 5);
        ring.push(["after".to_string()]);
        assert_eq!(
            ring.take().1.rows,
            0,
            "the same gap was reported twice, which would double count the loss"
        );
    }

    #[test]
    fn pushing_more_than_the_budget_at_once_is_bounded_not_grown() {
        let ring = SpillRing::new();
        ring.push((0..fitting() * 4).map(|i| format!("{i:08}")));
        assert_eq!(ring.len(), fitting());
    }

    #[test]
    fn the_budget_counts_bytes_so_long_rows_fit_fewer() {
        let ring = SpillRing::new();
        let long = "x".repeat(1000);
        ring.push((0..SPILL_BYTES).map(|_| long.clone()).take(20_000));
        assert_eq!(ring.len(), SPILL_BYTES / cost(&long));
    }

    #[test]
    fn a_quarter_full_ring_presses_the_writer_and_a_take_releases_it() {
        let ring = SpillRing::new();
        ring.push((0..fitting() / 4 - 1).map(|i| format!("{i:08}")));
        assert!(!ring.pressed());
        ring.push(["00000000".to_string(), "00000000".to_string()]);
        assert!(ring.pressed());
        ring.take();
        assert!(!ring.pressed());
    }
}

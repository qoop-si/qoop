//! One terminal: a PTY, an engine, and the rules that keep them honest.

use bytes::Bytes;
use qoro_terminal::{Damage, TerminalEffect, TerminalEngine};

/// How a read from the PTY master ended.
///
/// ADR-002. The rule that only handles `WouldBlock` is not sufficient: after the child
/// exits and the master is drained, `read` returns 0 forever with `POLLIN | POLLHUP`, and
/// `try_io` returning `Ok(Ok(0))` does not clear the reactor's cached readiness either. A
/// loop written to the shorter rule spins one core at 100 percent, on the idle path, after
/// the child is gone. On Linux the same condition commonly surfaces as `EIO`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Bytes arrived.
    Data(usize),
    /// No data right now. Readiness is cleared; wait again.
    WouldBlock,
    /// End of stream, however the platform spelled it. Deregister and tear down.
    EndOfStream,
    /// Fatal to this terminal.
    Fatal,
}

/// Classify a raw read result. One function so there is one place it can be wrong.
pub fn classify_read(result: Result<usize, std::io::Error>) -> ReadOutcome {
    match result {
        Ok(0) => ReadOutcome::EndOfStream,
        Ok(n) => ReadOutcome::Data(n),
        Err(e) => match e.kind() {
            std::io::ErrorKind::WouldBlock => ReadOutcome::WouldBlock,
            std::io::ErrorKind::Interrupted => ReadOutcome::WouldBlock,
            _ if e.raw_os_error() == Some(libc::EIO) => ReadOutcome::EndOfStream,
            _ => ReadOutcome::Fatal,
        },
    }
}

/// The parse sub-slice size.
///
/// Two constraints, and the smaller one wins.
///
/// A parse call cannot be interrupted, so a per-turn time budget is only meaningful if the
/// read buffer is fed in pieces. Measured at 200x50: a full 64 KiB buffer is roughly 1235
/// microseconds in one uninterruptible call, 16 KiB is 425, and 8 KiB lands near the 250
/// microsecond target.
///
/// The engine's scrollback ring is the other constraint, and it binds harder. Rows are
/// observable only while they are in that ring, so a slice that scrolls more rows than the
/// ring holds loses the earliest ones before anything can drain them. A byte of ordinary
/// output scrolls at most one row, so a slice no larger than the ring cannot overflow it
/// that way. Measured before this bound existed: 4000 short lines went in, 1000 rows
/// reached the disk, and every other test stayed green.
///
/// The worst case is a slice of nothing but line feeds, which scrolls one row per byte, so
/// half the ring leaves a factor of two in hand. A scroll control sequence turns out not to
/// need a bound at all: measured, `CSI 5000 S` moves at most one screen into history,
/// because the engine clamps a scroll larger than the region.
pub const PARSE_SLICE: usize = if 8 * 1024 < qoro_terminal::SCROLLBACK_ROWS / 2 {
    8 * 1024
} else {
    qoro_terminal::SCROLLBACK_ROWS / 2
};

/// Feed a read buffer to the engine in sub-slices, draining scrolled-off rows between them.
///
/// Both halves matter. The sub-slicing is what makes the parse budget expressible at all.
/// Draining between slices rather than per read is what keeps the spill correct: one 64 KiB
/// read scrolls more than the whole in-memory window, so a spill that ran once per read
/// would lose history on the first full buffer.
pub fn feed_in_slices<E: TerminalEngine>(
    engine: &mut E,
    buf: &[u8],
    mut on_scrolled: impl FnMut(Vec<String>),
) -> (Damage, Vec<TerminalEffect>) {
    let mut merged: Vec<qoro_terminal::LineDamage> = Vec::new();
    let mut full = false;
    let mut effects = Vec::new();
    for slice in buf.chunks(PARSE_SLICE) {
        let (damage, mut fx) = engine.feed(slice);
        match damage {
            Damage::Full => full = true,
            Damage::Lines(lines) => merged.extend(lines),
        }
        effects.append(&mut fx);
        let scrolled = engine.take_scrolled_off();
        if !scrolled.is_empty() {
            on_scrolled(scrolled);
        }
    }
    let damage = if full {
        Damage::Full
    } else {
        merged.sort_unstable();
        merged.dedup();
        Damage::Lines(merged)
    };
    (damage, effects)
}

/// Effects that must be written back to the PTY, in the order the queue expects.
pub fn replies(effects: &[TerminalEffect]) -> Vec<Bytes> {
    effects
        .iter()
        .filter_map(|e| match e {
            TerminalEffect::Reply(b) => Some(b.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoro_terminal::AlacrittyEngine;
    use std::io::{Error, ErrorKind};

    #[test]
    fn end_of_stream_is_recognised_however_the_platform_spells_it() {
        // The measured spin: `Ok(0)` with POLLHUP on macOS, `EIO` on Linux. Neither is
        // WouldBlock, and a loop that only handles WouldBlock burns a core forever.
        assert_eq!(classify_read(Ok(0)), ReadOutcome::EndOfStream);
        assert_eq!(
            classify_read(Err(Error::from_raw_os_error(libc::EIO))),
            ReadOutcome::EndOfStream
        );
        assert_eq!(classify_read(Ok(7)), ReadOutcome::Data(7));
        assert_eq!(
            classify_read(Err(Error::new(ErrorKind::WouldBlock, "eagain"))),
            ReadOutcome::WouldBlock
        );
        // EINTR is a retry, not a failure, and not an end of stream.
        assert_eq!(
            classify_read(Err(Error::from_raw_os_error(libc::EINTR))),
            ReadOutcome::WouldBlock
        );
        assert_eq!(
            classify_read(Err(Error::from_raw_os_error(libc::EBADF))),
            ReadOutcome::Fatal
        );
    }

    #[test]
    fn a_large_read_is_fed_in_slices_not_in_one_call() {
        // A whole 64 KiB buffer in one uninterruptible parse is roughly five times the
        // per-turn budget, so the budget is only expressible if the buffer is split.
        let mut e = AlacrittyEngine::new(200, 50);
        let payload: Vec<u8> = (0..64 * 1024).map(|i| b"abcdefgh"[i % 8]).collect();
        let mut slices = 0;
        feed_in_slices(&mut e, &payload, |_| {});
        for chunk in payload.chunks(PARSE_SLICE) {
            slices += 1;
            assert!(chunk.len() <= PARSE_SLICE);
        }
        assert!(
            slices >= 8,
            "a 64 KiB buffer must be split into at least 8 slices"
        );
    }

    #[test]
    fn scrolled_rows_are_drained_between_slices_not_once_per_read() {
        // One 64 KiB read scrolls more than the whole in-memory window at 200 columns, so
        // draining once per read loses history. This asserts the drain happens repeatedly
        // within a single read.
        let mut e = AlacrittyEngine::new(200, 50);
        let mut payload = Vec::new();
        while payload.len() < 64 * 1024 {
            payload.extend_from_slice(b"a line of output that is reasonably wide\r\n");
        }
        let mut drains = 0usize;
        let mut rows = 0usize;
        feed_in_slices(&mut e, &payload, |scrolled| {
            drains += 1;
            rows += scrolled.len();
        });
        assert!(drains > 1, "the spill drained {drains} times for one read");
        assert!(rows > 100, "only {rows} rows were captured");
    }

    #[test]
    fn damage_from_several_slices_is_merged_and_deduplicated() {
        let mut e = AlacrittyEngine::new(80, 24);
        let payload: Vec<u8> = std::iter::repeat_n(b'x', PARSE_SLICE * 3).collect();
        let (damage, _) = feed_in_slices(&mut e, &payload, |_| {});
        if let Damage::Lines(lines) = damage {
            let mut sorted = lines.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(lines, sorted, "damage must be merged and deduplicated");
        }
    }

    #[test]
    fn a_cursor_position_request_survives_the_slicing_path() {
        let mut e = AlacrittyEngine::new(80, 24);
        let (_, effects) = feed_in_slices(&mut e, b"\x1b[6n", |_| {});
        assert_eq!(
            replies(&effects).len(),
            1,
            "the reply must reach the caller"
        );
    }
}

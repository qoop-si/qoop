//! Terminal history on disk, as scrolled-off rows.
//!
//! Rows, not raw output bytes. What scrolls off a terminal is text the engine has already
//! interpreted, and storing the bytes instead would mean replaying a parser to read it back,
//! with every ambiguity that implies.
//!
//! Format, one directory per terminal:
//!
//! ```text
//! history/<terminal>/000000000000.seg   rows, each u32 little-endian length then UTF-8
//! history/<terminal>/000000000000.idx   u64 little-endian byte offset every INDEX_EVERY rows
//! ```
//!
//! A segment is named by the ordinal of its first row, so finding the segment holding row N
//! is a directory listing and a comparison, and finding the row inside it is one index read
//! and a short scan. Attaching to a terminal with a hundred megabytes of history is then a
//! seek rather than a scan (gate G2.6).

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::StoreError;

/// How often a byte offset is recorded. Small enough that the scan after a seek is short,
/// large enough that the index is a rounding error against the segment.
pub const INDEX_EVERY: u64 = 256;

/// Roll to a new segment past this size, so that a single file never becomes the unit of
/// everything and a retention policy has something to delete.
pub const SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

/// A row longer than this is truncated on the way in, with the truncation recorded.
///
/// A terminal row is bounded by its width, so this is far above any real one. It exists
/// because the writer must never be the thing that decides how much memory to allocate
/// based on a number it read from a file.
pub const MAX_ROW_BYTES: usize = 1024 * 1024;

pub struct History {
    dir: PathBuf,
    segment: Option<Segment>,
    /// Ordinal of the next row to be written. Never resets, so a row's ordinal is stable
    /// for the life of the terminal.
    next_ordinal: u64,
}

struct Segment {
    first_ordinal: u64,
    /// Buffered, and not for tidiness. A row is a four byte length and a few bytes of text,
    /// so an unbuffered writer makes two `write` calls per row. A terminal streaming a
    /// megabyte a second of short lines produces about 145 000 rows a second, which is
    /// 290 000 syscalls a second for one terminal. Measured before this: 0.70 cores for one
    /// streaming terminal, against an engine that parses 23 MiB per second per core.
    data: BufWriter<File>,
    index: BufWriter<File>,
    bytes: u64,
    rows: u64,
}

impl History {
    /// Open, recovering the next ordinal from what is already on disk.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(io)?;
        let mut next_ordinal = 0u64;
        for entry in std::fs::read_dir(dir).map_err(io)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("seg") {
                continue;
            }
            let first = segment_ordinal(&path).unwrap_or(0);
            let rows = count_rows(&path)?;
            next_ordinal = next_ordinal.max(first + rows);
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            segment: None,
            next_ordinal,
        })
    }

    pub fn next_ordinal(&self) -> u64 {
        self.next_ordinal
    }

    /// Append rows. Returns how many bytes of row text were truncated, which is never
    /// silently zero: a caller that gets a non-zero answer has lost something.
    pub fn append(&mut self, rows: &[String]) -> Result<usize, StoreError> {
        let mut truncated = 0usize;
        for row in rows {
            let bytes = row.as_bytes();
            let bytes = if bytes.len() > MAX_ROW_BYTES {
                truncated += bytes.len() - MAX_ROW_BYTES;
                // Cut on a character boundary so what is kept is still text.
                let mut end = MAX_ROW_BYTES;
                while end > 0 && !row.is_char_boundary(end) {
                    end -= 1;
                }
                &bytes[..end]
            } else {
                bytes
            };
            self.append_one(bytes)?;
        }
        Ok(truncated)
    }

    /// Push what is buffered to the operating system.
    ///
    /// Separate from `append` on purpose. Flushing per batch made the cost of history a
    /// syscall per batch on a path that produces thousands of batches a second, and
    /// scrollback is not a per-row durability promise: the rows are already in the buffer,
    /// and a crash loses at most what has not been flushed. The caller flushes on a timer
    /// and when the buffer has earned it.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        if let Some(segment) = &mut self.segment {
            segment.data.flush().map_err(io)?;
            segment.index.flush().map_err(io)?;
        }
        Ok(())
    }

    fn append_one(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        if self
            .segment
            .as_ref()
            .is_none_or(|s| s.bytes >= SEGMENT_BYTES)
        {
            self.roll()?;
        }
        let segment = self.segment.as_mut().expect("a segment was just opened");
        if segment.rows.is_multiple_of(INDEX_EVERY) {
            segment
                .index
                .write_all(&segment.bytes.to_le_bytes())
                .map_err(io)?;
        }
        segment
            .data
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .map_err(io)?;
        segment.data.write_all(bytes).map_err(io)?;
        segment.bytes += 4 + bytes.len() as u64;
        segment.rows += 1;
        self.next_ordinal += 1;
        Ok(())
    }

    fn roll(&mut self) -> Result<(), StoreError> {
        let first = self.next_ordinal;
        let data = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{first:012}.seg")))
            .map_err(io)?;
        let index = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{first:012}.idx")))
            .map_err(io)?;
        let bytes = data.metadata().map_err(io)?.len();
        self.segment = Some(Segment {
            first_ordinal: first,
            data: BufWriter::with_capacity(256 * 1024, data),
            index: BufWriter::with_capacity(8 * 1024, index),
            bytes,
            rows: 0,
        });
        Ok(())
    }

    /// Read up to `count` rows starting at `from`.
    ///
    /// The seek is what makes attaching to a large history cheap: the segment is chosen by
    /// name, the starting offset comes from the index, and only the rows between the last
    /// index entry and `from` are scanned.
    pub fn read(&self, from: u64, count: usize) -> Result<Vec<String>, StoreError> {
        let mut segments: Vec<(u64, PathBuf)> = std::fs::read_dir(&self.dir)
            .map_err(io)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("seg"))
            .filter_map(|p| segment_ordinal(&p).map(|o| (o, p)))
            .collect();
        segments.sort_by_key(|(o, _)| *o);

        let mut out = Vec::new();
        let mut wanted = from;
        for (first, path) in segments {
            if out.len() >= count {
                break;
            }
            let rows_here = count_rows(&path)?;
            if wanted >= first + rows_here {
                continue;
            }
            let skip_to = wanted.saturating_sub(first);
            let index_entry = skip_to / INDEX_EVERY;
            let mut offset = 0u64;
            let idx_path = path.with_extension("idx");
            if let Ok(mut idx) = File::open(&idx_path) {
                let mut buf = [0u8; 8];
                if idx.seek(SeekFrom::Start(index_entry * 8)).is_ok()
                    && idx.read_exact(&mut buf).is_ok()
                {
                    offset = u64::from_le_bytes(buf);
                }
            }
            let mut file = BufReader::new(File::open(&path).map_err(io)?);
            file.seek(SeekFrom::Start(offset)).map_err(io)?;
            let mut ordinal = first + index_entry * INDEX_EVERY;
            while out.len() < count {
                let Some(row) = read_row(&mut file)? else {
                    break;
                };
                if ordinal >= wanted {
                    out.push(row);
                    wanted += 1;
                }
                ordinal += 1;
            }
        }
        Ok(out)
    }

    /// Rows on disk, which is the ordinal of the next one.
    pub fn len(&self) -> u64 {
        self.next_ordinal
    }

    pub fn is_empty(&self) -> bool {
        self.next_ordinal == 0
    }

    /// The segment currently being written, for diagnostics.
    pub fn current_segment(&self) -> Option<u64> {
        self.segment.as_ref().map(|s| s.first_ordinal)
    }
}

fn read_row(file: &mut BufReader<File>) -> Result<Option<String>, StoreError> {
    let mut len = [0u8; 4];
    match file.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(io(e)),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_ROW_BYTES {
        // A length we would not have written. Refuse rather than allocate it: this number
        // came from a file, and a file can be truncated, corrupted or someone else's.
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    match file.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(io(e)),
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// How many rows a segment holds.
///
/// Counted through the index rather than by reading the segment. The index already records
/// a byte offset every `INDEX_EVERY` rows, so the count is the entries it holds plus a scan
/// of the rows after the last one, which is at most `INDEX_EVERY` of them.
///
/// It was a full scan of every row, and that is a scan this is asked for far more often
/// than it looks: `open` does it per segment, and `read` does it again for every segment
/// before the one it wants. With scrollback being read a notch of a wheel at a time, a
/// terminal that has been running a week was rescanning its whole history on every notch,
/// against a five second deadline.
///
/// A missing or empty index gives the old behaviour rather than a wrong answer, and so does
/// an index written ahead of the data a crash lost: the scan starts from the last offset
/// the index knows and counts whatever is actually there.
fn count_rows(path: &Path) -> Result<u64, StoreError> {
    let (covered, offset) = index_tail(&path.with_extension("idx"));
    let mut file = BufReader::new(File::open(path).map_err(io)?);
    if offset > 0 {
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
    }
    let mut rows = covered;
    while read_row(&mut file)?.is_some() {
        rows += 1;
    }
    Ok(rows)
}

/// The last offset an index records, and how many rows come before it.
///
/// `(0, 0)` when there is no index to read, which is a full scan and the right answer.
fn index_tail(idx: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::metadata(idx) else {
        return (0, 0);
    };
    let entries = meta.len() / 8;
    if entries == 0 {
        return (0, 0);
    }
    let Ok(mut file) = File::open(idx) else {
        return (0, 0);
    };
    let mut buf = [0u8; 8];
    if file.seek(SeekFrom::Start((entries - 1) * 8)).is_err() || file.read_exact(&mut buf).is_err()
    {
        return (0, 0);
    }
    // Entry `i` is the offset of row `i * INDEX_EVERY`, so the last entry has that many
    // rows before it.
    ((entries - 1) * INDEX_EVERY, u64::from_le_bytes(buf))
}

fn segment_ordinal(path: &Path) -> Option<u64> {
    path.file_stem()?.to_str()?.parse().ok()
}

fn io(e: std::io::Error) -> StoreError {
    StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize, prefix: &str) -> Vec<String> {
        (0..n).map(|i| format!("{prefix}{i}")).collect()
    }

    #[test]
    fn rows_come_back_in_the_order_they_went_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::open(dir.path()).unwrap();
        h.append(&rows(1000, "line")).unwrap();
        h.flush().unwrap();
        let back = h.read(0, 1000).unwrap();
        assert_eq!(back.len(), 1000);
        assert_eq!(back[0], "line0");
        assert_eq!(back[999], "line999");
    }

    #[test]
    fn a_read_in_the_middle_lands_on_the_right_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::open(dir.path()).unwrap();
        h.append(&rows(2000, "r")).unwrap();
        h.flush().unwrap();
        // Deliberately not on an index boundary, so the scan after the seek is exercised.
        let back = h.read(1337, 3).unwrap();
        assert_eq!(back, vec!["r1337", "r1338", "r1339"]);
    }

    #[test]
    fn history_survives_reopening_and_keeps_counting() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut h = History::open(dir.path()).unwrap();
            h.append(&rows(10, "a")).unwrap();
            h.flush().unwrap();
        }
        let mut h = History::open(dir.path()).unwrap();
        assert_eq!(h.next_ordinal(), 10, "the ordinal restarted");
        h.append(&rows(5, "b")).unwrap();
        h.flush().unwrap();
        let back = h.read(8, 4).unwrap();
        assert_eq!(back, vec!["a8", "a9", "b0", "b1"]);
    }

    #[test]
    fn more_rows_than_one_read_can_produce_are_all_kept_in_order() {
        // Gate G2.7's shape: one read can scroll more than the whole in-memory window, so
        // the write path has to take a batch larger than the window and keep every row.
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::open(dir.path()).unwrap();
        h.append(&rows(5000, "x")).unwrap();
        h.flush().unwrap();
        let back = h.read(0, 5000).unwrap();
        assert_eq!(back.len(), 5000);
        for (i, row) in back.iter().enumerate() {
            assert_eq!(row, &format!("x{i}"), "row {i} is out of order or missing");
        }
    }

    #[test]
    fn an_absurd_row_is_truncated_and_the_loss_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::open(dir.path()).unwrap();
        let huge = "y".repeat(MAX_ROW_BYTES + 500);
        let truncated = h.append(&[huge]).unwrap();
        h.flush().unwrap();
        assert_eq!(truncated, 500);
        assert_eq!(h.read(0, 1).unwrap()[0].len(), MAX_ROW_BYTES);
    }

    /// Counting a segment is a seek and a short scan, not a read of every row.
    ///
    /// Asserted as an answer and as a cost. The answer is what every other test already
    /// depends on; the cost is the point of the change, and a count that is correct while
    /// still reading the whole file would pass an answer-only test.
    #[test]
    fn a_segment_is_counted_through_its_index_rather_than_by_reading_it() {
        let dir = tempfile::tempdir().expect("a directory");
        let mut history = History::open(dir.path()).expect("open");
        // Several index entries' worth, so the index is the thing being read.
        let rows: Vec<String> = (0..INDEX_EVERY * 4 + 7)
            .map(|i| format!("row {i}"))
            .collect();
        history.append(&rows).expect("append");
        history.flush().expect("flush");
        let segment = dir.path().join("000000000000.seg");

        assert_eq!(
            count_rows(&segment).expect("count"),
            rows.len() as u64,
            "the count is wrong, which is worse than the count being slow"
        );

        // The index says where to start, and it starts near the end.
        let (covered, offset) = index_tail(&segment.with_extension("idx"));
        assert_eq!(covered, INDEX_EVERY * 4);
        assert!(
            offset > 0,
            "the index gave no offset, so this is still a full scan"
        );
        let size = std::fs::metadata(&segment).expect("size").len();
        assert!(
            offset > size / 2,
            "the scan starts at {offset} of {size} bytes, which is not a short scan"
        );

        // Reopening finds the same next ordinal, which is what `open` uses the count for.
        let reopened = History::open(dir.path()).expect("reopen");
        assert_eq!(reopened.len(), rows.len() as u64);

        // And a segment with no index at all is still counted correctly, which is what a
        // crash between the two writes leaves behind.
        std::fs::remove_file(segment.with_extension("idx")).expect("remove the index");
        assert_eq!(count_rows(&segment).expect("count"), rows.len() as u64);
    }
}

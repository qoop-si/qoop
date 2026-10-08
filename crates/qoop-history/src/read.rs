//! Bounded tail reading.
//!
//! Transcripts grow without bound and their writer may be appending right
//! now. So a reader never loads the whole file: it maps at most the last
//! [`TAIL_WINDOW_BYTES`] and parses lines from there. The first line of the
//! window is a torn offset artifact and is always dropped; a torn trailing
//! line fails JSON parsing downstream and is counted as corrupt there, the
//! same honesty `tail -f` offers.

use std::io::{Read, Seek, SeekFrom};

use crate::Error;

/// How far back from the end a read may look. Older than this is outside
/// every `--last` window the CLI offers.
pub const TAIL_WINDOW_BYTES: u64 = 8 * 1024 * 1024;

/// Lines of `path` inside the tail window, oldest first. `clipped` is true
/// when the file is larger than the window, i.e. older lines exist that were
/// deliberately not scanned.
pub fn read_tail_lines(path: &std::path::Path) -> Result<(Vec<String>, bool), Error> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::Io(e.to_string()))?;
    let len = file.metadata().map_err(|e| Error::Io(e.to_string()))?.len();
    let start = len.saturating_sub(TAIL_WINDOW_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| Error::Io(e.to_string()))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| Error::Io(e.to_string()))?;
    // The file is JSONL/JSON; lossy here only affects undecodable bytes,
    // which then fail line parsing and are counted as corrupt.
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if start > 0 && !lines.is_empty() {
        // Torn offset artifact: the first line does not start at a boundary.
        lines.remove(0);
    }
    if !buf.ends_with(b"\n") && !lines.is_empty() {
        // A writer is appending right now: the trailing line is torn, not
        // corrupt. Drop it silently so healthy live files stay warning-free.
        lines.pop();
    }
    Ok((lines, start > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn small_file_reads_whole_unclipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        std::fs::write(&p, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        let (lines, clipped) = read_tail_lines(&p).unwrap();
        assert_eq!(lines, vec!["{\"a\":1}", "{\"b\":2}"]);
        assert!(!clipped);
    }

    #[test]
    fn torn_trailing_line_drops_silently() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("live.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "{{\"a\":1}}").unwrap();
        write!(f, "{{\"b\":").unwrap(); // writer mid-line, no newline
        f.sync_all().unwrap();
        let (lines, clipped) = read_tail_lines(&p).unwrap();
        assert_eq!(lines, vec!["{\"a\":1}"]);
        assert!(!clipped);
    }

    #[test]
    fn missing_file_is_an_io_error() {
        assert!(matches!(
            read_tail_lines(std::path::Path::new("/nonexistent-qoop-history-test/x")),
            Err(Error::Io(_))
        ));
    }

    #[test]
    fn window_clips_and_drops_the_torn_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        // One line longer than the window would hide everything; instead pad
        // with full lines so the window starts mid-line.
        let pad = "x".repeat(1024);
        for i in 0..(9 * 1024) {
            writeln!(f, "{{\"n\":{i},\"p\":\"{pad}\"}}").unwrap();
        }
        f.sync_all().unwrap();
        let (lines, clipped) = read_tail_lines(&p).unwrap();
        assert!(clipped);
        // Every kept line parses as JSON: no torn head survived.
        for l in &lines {
            assert!(serde_json::from_str::<serde_json::Value>(l).is_ok());
        }
        assert!(!lines.is_empty());
    }
}

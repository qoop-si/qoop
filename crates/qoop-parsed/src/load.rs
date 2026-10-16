//! Reading a session into a [`Digest`]: stat, parse, project, page.

use std::path::Path;
use std::time::UNIX_EPOCH;

use agentparse::{Agent, Session};

use crate::item::{Digest, Item};
use crate::locate::Source;
use crate::project;

/// A session of a format that is parsed whole (Gemini) and is bigger than this is not read:
/// the view says to use the terminal. Claude and Codex files are read by their tail, so their
/// size does not matter, and real ones run to 200 MB.
pub const MAX_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// The widest tail window a page may read.
pub const MAX_TAIL_BYTES: u64 = 64 * 1024 * 1024;

/// How far into a file to look for its first record.
const HEAD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Options {
    /// How much of the end of a Claude or Codex file is parsed first (default 1 MiB). The
    /// window widens by 4x per step, up to [`MAX_TAIL_BYTES`] or the whole file, until it
    /// holds `skip + limit` items.
    pub max_tail_bytes: u64,
    /// Most items returned (default 60).
    pub limit: usize,
    /// How many items, counted from the newest, the caller already holds. The page is the
    /// next `limit` items older than those. The cursor is this count plus the `revision` the
    /// caller got: when the revision differs the file changed and the caller starts again at 0.
    pub skip: usize,
    /// Response budget in bytes (default 256 KiB); the oldest items of the page go first.
    /// The newest item of the page is always kept, even when it alone is over the budget, so
    /// a page is never empty because of it. Text is clamped to 4 KiB per item, so that item
    /// is small.
    pub max_total_bytes: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_tail_bytes: 1024 * 1024,
            limit: 60,
            skip: 0,
            max_total_bytes: 256 * 1024,
        }
    }
}

/// Failures, all printable. No path appears in a message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("session is larger than the chat view reads")]
    TooLarge,
    #[error("session not found")]
    NotFound,
    #[error("cannot read session: {0}")]
    Io(String),
    #[error("cannot parse session: {0}")]
    Parse(String),
}

impl From<agentparse::Error> for Error {
    fn from(e: agentparse::Error) -> Self {
        match e {
            agentparse::Error::NotFound(_) => Error::NotFound,
            agentparse::Error::Io(e) => Error::Io(e.kind().to_string()),
            agentparse::Error::NotASession(_) => Error::Parse("not a session".into()),
            agentparse::Error::Sqlite(_) => Error::Parse("database unreadable".into()),
        }
    }
}

/// Read one page of a session's items: the `limit` before the newest `skip`.
pub fn load(source: &Source, opts: &Options) -> Result<Digest, Error> {
    // Stat once: the revision, the cut decision and the parse window all use this length. A
    // file that grows while it is parsed then shows a revision older than what was read, so
    // the next poll refetches rather than misses.
    let stamp = stamp(source)?;
    let want = opts.skip.saturating_add(opts.limit);
    let mut window = opts.max_tail_bytes;
    loop {
        let (session, cut) = read(source, stamp.len, window)?;
        let all = project::items(&session);
        let items = if cut { drop_leading_orphans(all) } else { all };
        if !cut || items.len() >= want || window >= MAX_TAIL_BYTES {
            let warnings = u32::try_from(session.warnings.len()).unwrap_or(u32::MAX);
            let (first_seq, has_older, items) = page(items, cut, opts);
            return Ok(Digest {
                revision: stamp.revision,
                first_seq,
                has_older,
                items,
                warnings,
            });
        }
        window = window.saturating_mul(4).min(MAX_TAIL_BYTES);
    }
}

/// Parse the session with a tail window of `window` bytes, for a file of `len` bytes.
/// Returns the session and whether the window starts inside the file. Only Claude and Codex
/// are read by tail; every other source is read whole.
pub(crate) fn read(source: &Source, len: u64, window: u64) -> Result<(Session, bool), Error> {
    match source {
        Source::Opencode { db, id } => Ok((agentparse::opencode::parse_db(db, id)?, false)),
        Source::File(path) => read_file(path, len, window),
    }
}

fn read_file(path: &Path, len: u64, window: u64) -> Result<(Session, bool), Error> {
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
    let format = format_of(path);
    let (Format::Claude | Format::Codex, true) = (format, len > window.saturating_add(1)) else {
        return Ok((agentparse::parse_file(path)?, false));
    };
    let mut file = std::fs::File::open(path).map_err(io_error)?;
    file.seek(SeekFrom::Start(len - window - 1))
        .map_err(io_error)?;
    // One byte early, as `parse_file_tail` reads: if it ends a line the window starts on a
    // whole record. `take` keeps what the file gained since the stat out of the window.
    let mut reader = BufReader::new(file.take(window + 1));
    reader.skip_until(b'\n').map_err(io_error)?;
    let agent = match format {
        Format::Codex => Agent::Codex,
        _ => Agent::Claude,
    };
    let source = agentparse::Source::File {
        path: path.to_path_buf(),
    };
    Ok((agentparse::parse_reader(agent, reader, source)?, true))
}

fn io_error(e: std::io::Error) -> Error {
    Error::Io(e.kind().to_string())
}

/// A window that starts inside the file begins with results whose calls lie before it. Those
/// rows change shape when the window widens, so they are not served.
fn drop_leading_orphans(mut items: Vec<Item>) -> Vec<Item> {
    let n = items.iter().take_while(|i| i.is_orphan()).count();
    items.drain(..n);
    items
}

pub(crate) struct Stamp {
    pub revision: u64,
    pub len: u64,
}

/// Size and change stamp of the source, refusing a whole-parsed file over [`MAX_SOURCE_BYTES`].
pub(crate) fn stamp(source: &Source) -> Result<Stamp, Error> {
    match source {
        Source::File(path) => {
            let (len, mtime) = len_and_mtime(path)?;
            if len > MAX_SOURCE_BYTES && format_of(path) == Format::Whole {
                return Err(Error::TooLarge);
            }
            Ok(Stamp {
                revision: fnv(&[len, mtime]),
                len,
            })
        }
        Source::Opencode { db, .. } => {
            let (len, mtime) = len_and_mtime(db)?;
            let mut wal = db.as_os_str().to_owned();
            wal.push("-wal");
            let (wal_len, wal_mtime) = len_and_mtime(Path::new(&wal)).unwrap_or((0, 0));
            Ok(Stamp {
                revision: fnv(&[len, mtime, wal_len, wal_mtime]),
                len,
            })
        }
    }
}

/// How a file is read: Claude and Codex transcripts are line streams that can be cut at a
/// line, everything else (Gemini, a JSON document, a compressed rollout, a file that is none
/// of them) is parsed whole.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Claude,
    Codex,
    Whole,
}

/// The format by the first JSON object of the file, the way `agentparse` tells its products
/// apart (its sniff is private). Only the head is read.
fn format_of(path: &Path) -> Format {
    use std::io::{BufRead, BufReader, Read};
    if path.extension().is_some_and(|e| e == "json" || e == "zst") {
        return Format::Whole;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Format::Whole;
    };
    let mut reader = BufReader::new(file.take(HEAD_BYTES));
    let mut line = Vec::new();
    for _ in 0..32 {
        line.clear();
        if reader.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Ok(serde_json::Value::Object(first)) = serde_json::from_slice(line.trim_ascii()) {
            return format_of_record(&first);
        }
    }
    Format::Whole
}

fn format_of_record(v: &serde_json::Map<String, serde_json::Value>) -> Format {
    let has = |k: &str| v.contains_key(k);
    let typed = v.get("type").is_some_and(|t| t.is_string());
    if has("sessionId") && has("projectHash") {
        Format::Whole
    } else if has("payload") && typed
        || has("record_type")
        || has("id") && has("timestamp") && !has("uuid") && !has("sessionId")
    {
        Format::Codex
    } else if typed || has("sessionId") {
        Format::Claude
    } else {
        Format::Whole
    }
}

fn len_and_mtime(path: &Path) -> Result<(u64, u64), Error> {
    let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::NotFound,
        kind => Error::Io(kind.to_string()),
    })?;
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Ok((meta.len(), nanos as u64))
}

/// FNV-1a over the little-endian bytes: stable across runs and Rust versions, unlike `Hasher`.
fn fnv(values: &[u64]) -> u64 {
    values
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        })
}

/// The page asked for: skip the newest `skip` items, take the `limit` before them, keep the
/// newest of those that fit the byte budget. `more_before_window` is true when the window
/// starts inside the file. Returns `(first_seq, has_older, items)`.
fn page(mut items: Vec<Item>, more_before_window: bool, opts: &Options) -> (u32, bool, Vec<Item>) {
    items.truncate(items.len().saturating_sub(opts.skip));
    let mut start = items.len().saturating_sub(opts.limit);
    let mut bytes = 0;
    for (i, item) in items.iter().enumerate().skip(start).rev() {
        bytes += item.weight();
        if bytes > opts.max_total_bytes && i + 1 < items.len() {
            start = i + 1;
            break;
        }
    }
    let has_older = start > 0 || more_before_window;
    let page = items.split_off(start);
    (page.first().map_or(0, Item::seq), has_older, page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn cut(name: &str, window: u64) -> bool {
        let path = fixture(name);
        let len = std::fs::metadata(&path).unwrap().len();
        read(&Source::File(path), len, window).unwrap().1
    }

    /// `detail` stops after the first parse when this is false: a whole-parsed source gains
    /// nothing from a wider window.
    #[test]
    fn only_a_claude_or_codex_file_longer_than_the_window_is_cut() {
        assert!(cut("claude.jsonl", 1024));
        assert!(cut("codex/real-exit3.jsonl", 1024));
        assert!(!cut("claude.jsonl", 1024 * 1024));
        assert!(!cut("gemini/gemini-new-session.jsonl", 1024));
        assert!(!cut("gemini/gemini-old-session.json", 1024));
    }

    #[test]
    fn the_format_comes_from_the_first_record() {
        for (name, want) in [
            ("claude.jsonl", Format::Claude),
            ("codex/real-exit3.jsonl", Format::Codex),
            ("gemini/gemini-new-session.jsonl", Format::Whole),
            ("gemini/gemini-old-session.json", Format::Whole),
        ] {
            assert!(format_of(&fixture(name)) == want, "{name}");
        }
    }

    #[test]
    fn an_opencode_session_is_never_cut() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let sql = std::fs::read_to_string(fixture("opencode/v2.sql")).unwrap();
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(&sql)
            .unwrap();
        let source = Source::Opencode {
            db,
            id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
        };
        assert!(!read(&source, 1 << 30, 1).unwrap().1);
    }
}

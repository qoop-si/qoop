//! Finding the transcript file for a `(product, session id)` pair.
//!
//! One strategy per product, no guessing. Claude and Codex look an id up by
//! name, which costs one stat per project or one name test per file however
//! much history there is; Gemini still scans, and its scan is bounded. A missing id is
//! always a refusal upstream, never a "newest file" pick: this module only
//! answers exact ids. Every returned path is canonicalized and checked
//! against its product base directory, so a smuggled symlink cannot escape.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::model::Product;

/// How many directories the Gemini scan may enter.
const MAX_SCAN_DIRS: usize = 200;
/// How many candidate files the Gemini scan may visit.
const MAX_SCAN_FILES: usize = 2000;
/// Upper bound for reading a Codex first line: `session_meta` is small, but
/// a pathological first line must not grow the probe without limit.
const HEAD_FIRST_LINE_MAX: u64 = 1024 * 1024;

/// Where the four products keep their history. All fields are product base
/// directories except `opencode`, which is the database file itself (`None`
/// when opencode is not installed).
#[derive(Debug, Clone)]
pub struct BaseDirs {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub opencode: Option<PathBuf>,
    pub gemini: PathBuf,
}

impl BaseDirs {
    /// Standard locations under `$HOME`. Missing `HOME` is an error;
    /// missing product paths are not (that product is simply absent).
    pub fn from_home() -> Result<Self, Error> {
        let home = std::env::var("HOME").map_err(|_| Error::NoHome)?;
        let home = PathBuf::from(home);
        let opencode = home.join(".local/share/opencode/opencode.db");
        Ok(BaseDirs {
            claude: home.join(".claude/projects"),
            codex: home.join(".codex/sessions"),
            opencode: opencode.is_file().then_some(opencode),
            gemini: home.join(".gemini/tmp"),
        })
    }
}

/// A located transcript: either a file to tail or the opencode database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    File(PathBuf),
    OpencodeDb(PathBuf),
}

/// Session ids are matched against file names and DB keys. Only an
/// allowlist may pass: the four real shapes (UUIDs, `ses_` keys, rollout
/// names, hash prefixes) all fit `[A-Za-z0-9_-]` under 128 chars.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Detect the product from the program name in an agents row, via
/// `qoro-detect` (never ad-hoc matching). Returns the product for the four
/// readers, or a refusal naming what was seen.
pub fn detect_product(program: &str) -> Result<Product, Error> {
    let base = program.rsplit('/').next().unwrap_or(program);
    let manifest = qoro_detect::for_program(base).ok_or_else(|| Error::UnknownProgram {
        program: base.to_string(),
    })?;
    match manifest.id.to_lowercase().as_str() {
        "claude" => Ok(Product::Claude),
        "codex" => Ok(Product::Codex),
        "opencode" => Ok(Product::Opencode),
        "gemini" => Ok(Product::Gemini),
        _ => Err(Error::NoReader {
            product: manifest.id.clone(),
        }),
    }
}

/// Locate the transcript for an exact session id.
pub fn locate(product: Product, id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    if !valid_id(id) {
        return Err(Error::BadId {
            product: product.as_str(),
        });
    }
    match product {
        Product::Claude => locate_claude(id, dirs),
        Product::Codex => locate_codex(id, dirs),
        Product::Opencode => locate_opencode(id, dirs),
        Product::Gemini => locate_gemini(id, dirs),
    }
    .map_err(|e| match e {
        Error::NotFound { .. } => Error::NotFound {
            product: product.as_str(),
            id: id.to_string(),
        },
        Error::BadId { product: "" } => Error::BadId {
            product: product.as_str(),
        },
        other => other,
    })
}

/// `<projects>/<slug>/<id>.jsonl`. The id is the file name, so each project
/// dir costs one stat and nothing is listed inside it: no cap to trip, however
/// many projects and transcripts pile up.
fn locate_claude(id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    let projects = match std::fs::read_dir(&dirs.claude) {
        Ok(e) => e,
        Err(_) => return Err(not_found()),
    };
    for slug in projects.flatten() {
        let dir = slug.path();
        if !is_plain_dir(&dir) {
            continue;
        }
        let p = dir.join(format!("{id}.jsonl"));
        if p.is_file() {
            return Ok(Located::File(checked(&p, &dirs.claude)?));
        }
    }
    Err(not_found())
}

/// `YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`. The file name picks the
/// candidate and the `session_meta` inside confirms it, so only a file that
/// already names the id is ever opened. Day directories sort by name, and the
/// walk takes the newest first: the live session is found in the first one or
/// two, however many months of history sit behind it, and nothing is capped.
pub fn locate_codex(id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    let suffix = format!("-{id}.jsonl");
    let mut stack = vec![dirs.codex.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths.iter().rev() {
            let named = p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix));
            if named && p.is_file() && codex_head_matches(p, id)? {
                return Ok(Located::File(checked(p, &dirs.codex)?));
            }
        }
        // Pushed oldest first, so the newest directory is popped next.
        stack.extend(paths.into_iter().filter(|p| is_plain_dir(p)));
    }
    Err(not_found())
}

/// The database is the lookup: one key, no scan.
fn locate_opencode(_id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    match &dirs.opencode {
        Some(db) => Ok(Located::OpencodeDb(db.clone())),
        None => Err(not_found()),
    }
}

/// `tmp/<hash>/chats/session-*.json` matched on the embedded `sessionId`.
fn locate_gemini(id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    let mut visited = 0;
    let mut dirs_seen = 0;
    let mut stack = vec![dirs.gemini.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in entries.flatten() {
            let p = f.path();
            if is_plain_dir(&p) {
                dirs_seen += 1;
                if dirs_seen > MAX_SCAN_DIRS {
                    return Err(Error::ScanCap { cap: MAX_SCAN_DIRS });
                }
                stack.push(p);
                continue;
            }
            if !p.is_file() || p.extension().is_some_and(|e| e != "json") {
                continue;
            }
            visited += 1;
            if visited > MAX_SCAN_FILES {
                return Err(Error::ScanCap {
                    cap: MAX_SCAN_FILES,
                });
            }
            if gemini_file_matches(&p, id)? {
                return Ok(Located::File(checked(&p, &dirs.gemini)?));
            }
        }
    }
    Err(not_found())
}

fn not_found() -> Error {
    Error::NotFound {
        product: "",
        id: String::new(),
    }
}

/// A real directory, not a symlink: walks never follow linked directories,
/// so a loop or an escape cannot trap or widen a scan. Linked *files* may
/// still match, but their output goes through [`checked`] first.
fn is_plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_dir() && !m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Canonicalize the final path (following symlinks) and require it under
/// `base`. Missing files: canonicalize the parent, join, then check.
fn checked(path: &Path, base: &Path) -> Result<PathBuf, Error> {
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
    match path.canonicalize() {
        Ok(real) => {
            if real.starts_with(&base) {
                Ok(real)
            } else {
                Err(Error::BadId { product: "" })
            }
        }
        Err(_) => {
            let parent = path.parent().unwrap_or(Path::new("."));
            let real_parent = parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf());
            let name = path.file_name().ok_or(Error::BadId { product: "" })?;
            if real_parent.starts_with(&base) {
                Ok(real_parent.join(name))
            } else {
                Err(Error::BadId { product: "" })
            }
        }
    }
}

/// Bounded head probe for the Codex in-file session id. The id lives in
/// `session_meta`, always the first line, so only the first line is read
/// (bounded: a pathological first line must not grow the probe).
fn codex_head_matches(path: &Path, id: &str) -> Result<bool, Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).map_err(|e| Error::Io(e.to_string()))?;
    let len = file.metadata().map_err(|e| Error::Io(e.to_string()))?.len();
    let take = len.min(HEAD_FIRST_LINE_MAX);
    let mut buf = vec![0u8; take as usize];
    file.seek(SeekFrom::Start(0))
        .map_err(|e| Error::Io(e.to_string()))?;
    file.read_exact(&mut buf)
        .map_err(|e| Error::Io(e.to_string()))?;
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    if end == buf.len() && len > take {
        // No newline inside the bound: the first line alone exceeds it, so
        // this is not a rollout file with a leading session_meta.
        return Ok(false);
    }
    let first = String::from_utf8_lossy(&buf[..end]);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&first) else {
        return Ok(false);
    };
    Ok(
        v.get("type").and_then(|t| t.as_str()) == Some("session_meta")
            && v.get("payload")
                .and_then(|p| p.get("session_id"))
                .and_then(|s| s.as_str())
                == Some(id),
    )
}

/// Small chats docs parse whole; oversize ones are skipped, not failed.
fn gemini_file_matches(path: &Path, id: &str) -> Result<bool, Error> {
    let len = std::fs::metadata(path)
        .map_err(|e| Error::Io(e.to_string()))?
        .len();
    if len > crate::read::TAIL_WINDOW_BYTES {
        return Ok(false);
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Ok(false);
    };
    Ok(v.get("sessionId").and_then(|s| s.as_str()) == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_allowlist_shapes() {
        assert!(valid_id("ee58ea0c-bc01-4eab-ab3a-91407e2eb44d"));
        assert!(valid_id("ses_f34a9e63dffeupPxKBgpQVC42u"));
        assert!(valid_id("rollout-2026-09-03T07-18-39-01a0657d"));
        assert!(!valid_id(""));
        assert!(!valid_id("../../.ssh/id_rsa"));
        assert!(!valid_id("/etc/passwd"));
        assert!(!valid_id("a\nb"));
        assert!(!valid_id(&"x".repeat(129)));
        assert!(valid_id(&"x".repeat(128)));
    }

    #[test]
    fn claude_locates_by_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("projects").join("-Users-x-proj");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::write(slug.join("abc-123.jsonl"), "{}\n").unwrap();
        let dirs = BaseDirs {
            claude: dir.path().join("projects"),
            codex: dir.path().join("codex"),
            opencode: None,
            gemini: dir.path().join("gemini"),
        };
        let found = locate(Product::Claude, "abc-123", &dirs).unwrap();
        assert!(matches!(found, Located::File(_)));
        assert!(locate(Product::Claude, "nope", &dirs).is_err());
    }

    #[test]
    fn bad_id_never_reaches_the_filesystem() {
        let dirs = BaseDirs {
            claude: "/nonexistent".into(),
            codex: "/nonexistent".into(),
            opencode: None,
            gemini: "/nonexistent".into(),
        };
        assert!(matches!(
            locate(Product::Claude, "../x", &dirs),
            Err(Error::BadId { .. })
        ));
    }
}

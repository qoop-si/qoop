//! Finding the transcript file for a `(product, session id)` pair.
//!
//! One strategy per product, no guessing. Claude, Codex and Gemini look an id
//! up by name, which costs one stat per project or one name test per file
//! however much history there is, and opens only a file whose name fits. A missing id is
//! always a refusal upstream, never a "newest file" pick: this module only
//! answers exact ids. Every returned path is canonicalized and checked
//! against its product base directory, so a smuggled symlink cannot escape.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::model::Product;

/// Upper bound for reading a Codex first line: `session_meta` is small, but
/// a pathological first line must not grow the probe without limit.
const HEAD_FIRST_LINE_MAX: u64 = 1024 * 1024;

/// How much of an oversize Gemini chat is read to confirm its session id.
const GEMINI_HEAD_BYTES: u64 = 64 * 1024;

/// Where the four products keep their history. All fields are product base
/// directories except `opencode`, which is the database file itself (`None`
/// when opencode is not installed). Claude has a list: its `projects` directories in the
/// order they are tried.
#[derive(Debug, Clone)]
pub struct BaseDirs {
    pub claude: Vec<PathBuf>,
    pub codex: PathBuf,
    pub opencode: Option<PathBuf>,
    pub gemini: PathBuf,
}

impl BaseDirs {
    /// Standard locations under `$HOME`. Missing `HOME` is an error;
    /// missing product paths are not (that product is simply absent).
    pub fn from_home() -> Result<Self, Error> {
        let home = std::env::var("HOME").map_err(|_| Error::NoHome)?;
        Ok(Self::under(
            Path::new(&home),
            std::env::var_os("CLAUDE_CONFIG_DIR"),
            std::env::var_os("XDG_DATA_HOME"),
        ))
    }

    /// [`BaseDirs::from_home`] with the environment passed in.
    ///
    /// Claude: `$CLAUDE_CONFIG_DIR/projects` first, as Claude reads it, then
    /// `~/.claude/projects`, the order the daemon's `claude_transcript` uses. A relative or
    /// empty value is not a directory anyone meant and is ignored. opencode: the database
    /// under `$XDG_DATA_HOME` (same rule), else under `~/.local/share`.
    pub fn under(
        home: &Path,
        claude_config_dir: Option<std::ffi::OsString>,
        xdg_data_home: Option<std::ffi::OsString>,
    ) -> Self {
        let absolute =
            |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
        let claude = claude_roots(Some(home), claude_config_dir);
        let opencode = absolute(xdg_data_home)
            .map(|d| d.join("opencode/opencode.db"))
            .into_iter()
            .chain([home.join(".local/share/opencode/opencode.db")])
            .find(|db| db.is_file());
        BaseDirs {
            claude,
            codex: home.join(".codex/sessions"),
            opencode,
            gemini: home.join(".gemini/tmp"),
        }
    }
}

/// The directories Claude keeps its projects in: `$CLAUDE_CONFIG_DIR/projects` first, as
/// Claude reads it, then `~/.claude/projects`. Either may be missing, so a daemon with no
/// `HOME` and a `CLAUDE_CONFIG_DIR` still finds its transcripts. A relative or empty
/// `CLAUDE_CONFIG_DIR` is not a directory anyone meant and is ignored.
pub fn claude_roots(
    home: Option<&Path>,
    claude_config_dir: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    claude_config_dir
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|d| d.join("projects"))
        .into_iter()
        .chain(home.map(|h| h.join(".claude/projects")))
        .collect()
}

/// [`claude_roots`] from this process's environment.
pub fn claude_roots_from_env() -> Vec<PathBuf> {
    claude_roots(
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        std::env::var_os("CLAUDE_CONFIG_DIR"),
    )
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
/// `qoop-detect` (never ad-hoc matching). Returns the product for the four
/// readers, or a refusal naming what was seen.
pub fn detect_product(program: &str) -> Result<Product, Error> {
    let base = program.rsplit('/').next().unwrap_or(program);
    let manifest = qoop_detect::for_program(base).ok_or_else(|| Error::UnknownProgram {
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
        Product::Claude => locate_claude(id, &dirs.claude),
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
/// many projects and transcripts pile up. `roots` are the `projects` directories, in the
/// order they are tried.
///
/// A symlinked project directory or transcript is followed, as Claude itself follows it
/// (people link `~/.claude/projects/<slug>` to a synced folder), and the id is the guard: it
/// is checked here, so no `..` can climb out of a project directory. The answer is the
/// resolved path of a regular file.
pub fn locate_claude(id: &str, roots: &[PathBuf]) -> Result<Located, Error> {
    if !valid_id(id) {
        return Err(Error::BadId { product: "" });
    }
    let file = format!("{id}.jsonl");
    for base in roots {
        let Ok(projects) = std::fs::read_dir(base) else {
            continue;
        };
        for slug in projects.flatten() {
            // Resolved only to see that it is a regular file; the answer is that file.
            if let Ok(real) = slug.path().join(&file).canonicalize()
                && real.is_file()
            {
                return Ok(Located::File(real));
            }
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

/// `tmp/<hash>/chats/session-<time>-<first 8 of the id>.json` (one JSON document, older
/// Gemini) or `.jsonl` (a header line, then records), confirmed on the embedded `sessionId`.
///
/// By name, like Claude and Codex: only a file whose name ends in the id's first eight
/// characters is opened. The scan it replaces opened every chat and gave up past 200
/// directories or 2000 files, so a long Gemini history could never be read at all.
fn locate_gemini(id: &str, dirs: &BaseDirs) -> Result<Located, Error> {
    // Ids are allowlisted ASCII upstream, so a byte slice is a character slice.
    let short = &id[..id.len().min(8)];
    let suffixes = [format!("-{short}.json"), format!("-{short}.jsonl")];
    let mut stack = vec![dirs.gemini.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for f in entries.flatten() {
            let p = f.path();
            if is_plain_dir(&p) {
                stack.push(p);
                continue;
            }
            let named = f.file_name().to_str().is_some_and(|n| {
                n.starts_with("session-") && suffixes.iter().any(|s| n.ends_with(s.as_str()))
            });
            if named && p.is_file() && gemini_file_matches(&p, id)? {
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
    let Some(v) = first_line_json(path)? else {
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

/// The first line of a file as JSON, `None` when it is not one or exceeds the probe bound.
fn first_line_json(path: &Path) -> Result<Option<serde_json::Value>, Error> {
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
        // this is not a file with a small leading header.
        return Ok(None);
    }
    let first = String::from_utf8_lossy(&buf[..end]);
    Ok(serde_json::from_str(&first).ok())
}

/// Small chats docs parse whole. An oversize one is not read whole just to be found: its
/// name already carries the id's first eight characters, and the full id must sit in its head.
/// Reading it is the caller's call (`history` refuses it, `qoop-parsed` has its own cap).
fn gemini_file_matches(path: &Path, id: &str) -> Result<bool, Error> {
    use std::io::Read;
    if path.extension().is_some_and(|e| e == "jsonl") {
        let header = first_line_json(path)?;
        return Ok(header.is_some_and(|v| v.get("sessionId").and_then(|s| s.as_str()) == Some(id)));
    }
    let len = std::fs::metadata(path)
        .map_err(|e| Error::Io(e.to_string()))?
        .len();
    if len > crate::read::TAIL_WINDOW_BYTES {
        let mut head = Vec::new();
        std::fs::File::open(path)
            .and_then(|f| f.take(GEMINI_HEAD_BYTES).read_to_end(&mut head))
            .map_err(|e| Error::Io(e.to_string()))?;
        return Ok(String::from_utf8_lossy(&head).contains(id));
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
            claude: vec![dir.path().join("projects")],
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
            claude: vec!["/nonexistent".into()],
            codex: "/nonexistent".into(),
            opencode: None,
            gemini: "/nonexistent".into(),
        };
        assert!(matches!(
            locate(Product::Claude, "../x", &dirs),
            Err(Error::BadId { .. })
        ));
    }

    fn empty_dirs(root: &Path) -> BaseDirs {
        BaseDirs {
            claude: vec![root.join("claude")],
            codex: root.join("codex"),
            opencode: None,
            gemini: root.join("gemini"),
        }
    }

    #[test]
    fn claude_config_dir_is_tried_first_then_the_home() {
        let home = Path::new("/home/u");
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            BaseDirs::under(home, os("/cfg/claude"), None).claude,
            [
                PathBuf::from("/cfg/claude/projects"),
                PathBuf::from("/home/u/.claude/projects")
            ]
        );
        let home_only = [PathBuf::from("/home/u/.claude/projects")];
        for ignored in [None, os(""), os("relative/dir")] {
            assert_eq!(BaseDirs::under(home, ignored, None).claude, home_only);
        }
    }

    #[test]
    fn a_session_in_either_claude_location_is_found_config_dir_first() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, home) = (dir.path().join("cfg"), dir.path().join("home"));
        let put = |projects: PathBuf, id: &str| {
            let slug = projects.join("-p");
            std::fs::create_dir_all(&slug).unwrap();
            std::fs::write(slug.join(format!("{id}.jsonl")), "{}\n").unwrap();
        };
        let (cfg_projects, home_projects) = (cfg.join("projects"), home.join(".claude/projects"));
        put(cfg_projects.clone(), "in-cfg");
        put(home_projects.clone(), "in-home");
        put(cfg_projects, "both");
        put(home_projects, "both");
        let dirs = BaseDirs::under(&home, Some(cfg.clone().into_os_string()), None);
        for id in ["in-cfg", "in-home", "both"] {
            assert!(locate(Product::Claude, id, &dirs).is_ok(), "{id}");
        }
        let Located::File(both) = locate(Product::Claude, "both", &dirs).unwrap() else {
            panic!("file")
        };
        assert!(both.starts_with(cfg.canonicalize().unwrap()), "{both:?}");
    }

    #[test]
    fn opencode_follows_xdg_data_home_then_the_home_layout() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let xdg = dir.path().join("xdg");
        let make = |base: &Path| {
            let db = base.join("opencode/opencode.db");
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            std::fs::write(&db, "").unwrap();
            db
        };
        let in_home = make(&home.join(".local/share"));
        let os = |p: &Path| Some(p.as_os_str().to_owned());
        assert_eq!(
            BaseDirs::under(&home, None, os(&xdg)).opencode,
            Some(in_home.clone())
        );
        let in_xdg = make(&xdg);
        assert_eq!(
            BaseDirs::under(&home, None, os(&xdg)).opencode,
            Some(in_xdg)
        );
        let relative = Some(std::ffi::OsString::from("xdg"));
        assert_eq!(
            BaseDirs::under(&home, None, relative).opencode,
            Some(in_home)
        );
    }

    /// People link `~/.claude/projects/<slug>` to a synced folder, and a link to one
    /// transcript works too: Claude follows both, so the resolver does, and the answer is
    /// still a regular file named by the id.
    #[test]
    fn a_symlinked_project_directory_and_a_symlinked_transcript_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = empty_dirs(dir.path());
        let projects = &dirs.claude[0];
        std::fs::create_dir_all(projects).unwrap();
        let synced = dir.path().join("synced");
        std::fs::create_dir_all(&synced).unwrap();
        std::fs::write(synced.join("dir-1.jsonl"), "{}\n").unwrap();
        std::os::unix::fs::symlink(&synced, projects.join("-linked")).unwrap();
        let slug = projects.join("-proj");
        std::fs::create_dir_all(&slug).unwrap();
        let elsewhere = dir.path().join("elsewhere.jsonl");
        std::fs::write(&elsewhere, "{}\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, slug.join("file-1.jsonl")).unwrap();
        assert_eq!(
            locate(Product::Claude, "dir-1", &dirs).unwrap(),
            Located::File(synced.join("dir-1.jsonl").canonicalize().unwrap())
        );
        assert_eq!(
            locate(Product::Claude, "file-1", &dirs).unwrap(),
            Located::File(elsewhere.canonicalize().unwrap())
        );
        // A link to a directory named like a transcript is not one.
        std::os::unix::fs::symlink(&synced, slug.join("dirlike-1.jsonl")).unwrap();
        assert!(locate(Product::Claude, "dirlike-1", &dirs).is_err());
    }

    /// `locate_claude` is public: it checks the id itself, so no caller can climb out of a
    /// project directory by forgetting to.
    #[test]
    fn the_claude_lookup_refuses_an_id_that_climbs_out_of_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(projects.join("-proj")).unwrap();
        std::fs::write(projects.join("secret.jsonl"), "{}\n").unwrap();
        let roots = [projects];
        for id in ["../secret", "..", "a/b", "", "x y", "a\nb"] {
            assert!(locate_claude(id, &roots).is_err(), "{id:?}");
        }
    }

    /// A daemon with no `HOME` and a `CLAUDE_CONFIG_DIR` still finds its transcripts.
    #[test]
    fn claude_roots_need_neither_home_nor_config_dir() {
        let config = tempfile::tempdir().unwrap();
        let slug = config.path().join("projects/-proj");
        std::fs::create_dir_all(&slug).unwrap();
        std::fs::write(slug.join("id-1.jsonl"), "{}\n").unwrap();
        let roots = claude_roots(None, Some(config.path().as_os_str().to_owned()));
        assert_eq!(roots, vec![config.path().join("projects")]);
        assert!(locate_claude("id-1", &roots).is_ok());
        let home = Path::new("/home/x");
        assert_eq!(
            claude_roots(Some(home), None),
            vec![home.join(".claude/projects")]
        );
        // A relative value is not a directory anyone meant.
        assert_eq!(
            claude_roots(Some(home), Some("rel".into())),
            vec![home.join(".claude/projects")]
        );
        assert!(claude_roots(None, None).is_empty());
    }

    #[test]
    fn gemini_locates_a_jsonl_chat_by_its_header_line() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = empty_dirs(dir.path());
        let chats = dirs.gemini.join("hash").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let id = "3c69a60b-e5fd-4d1a-bdb3-27baa190ad10";
        let file = chats.join("session-2026-10-10T21-18-3c69a60b.jsonl");
        std::fs::write(&file, format!("{{\"sessionId\":\"{id}\"}}\n{{}}\n")).unwrap();
        let found = locate(Product::Gemini, id, &dirs).unwrap();
        assert_eq!(found, Located::File(file.canonicalize().unwrap()));
        let other = "3c69a60b-0000-4000-8000-000000000000";
        assert!(locate(Product::Gemini, other, &dirs).is_err());
    }

    #[test]
    fn an_oversize_gemini_doc_is_still_found_by_name_and_head() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = empty_dirs(dir.path());
        let chats = dirs.gemini.join("hash").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let id = "84c6ca20-0300-4149-bc56-e7b1f7999eb5";
        let file = chats.join("session-2026-10-02T22-46-84c6ca20.json");
        std::fs::write(&file, format!("{{\"sessionId\":\"{id}\",\"messages\":[]}}")).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(crate::read::TAIL_WINDOW_BYTES + 1)
            .unwrap();
        assert!(locate(Product::Gemini, id, &dirs).is_ok());
    }
}

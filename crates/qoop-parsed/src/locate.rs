//! Finding the session of a terminal, through the one resolver `qoop-history` has.

use std::path::PathBuf;

use qoop_history::locate::{locate, locate_claude};
use qoop_history::{Located, detect_product};

/// Where the agents keep their history (`CLAUDE_CONFIG_DIR` honoured by `Dirs::from_home`).
pub use qoop_history::BaseDirs as Dirs;

/// The Claude `projects` directories this process's environment names.
pub use qoop_history::locate::claude_roots_from_env;

/// Where one session lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A transcript file (Claude, Codex, Gemini).
    File(PathBuf),
    /// One session inside the OpenCode database.
    Opencode { db: PathBuf, id: String },
}

/// The session of an agent row: its `program` name and the id its hook reported. The id is
/// untrusted: anything that is not a plain id, or whose file resolves outside the agent's own
/// directory, finds nothing. `None` also covers a program with no reader and a session whose
/// file does not exist (yet).
pub fn locate_session(program: &str, session_id: &str, dirs: &Dirs) -> Option<Source> {
    let product = detect_product(program).ok()?;
    match locate(product, session_id, dirs).ok()? {
        Located::File(path) => Some(Source::File(path)),
        Located::OpencodeDb(db) => Some(Source::Opencode {
            db,
            id: session_id.to_string(),
        }),
    }
}

/// The transcript of Claude session `id` under these `projects` directories: the same lookup
/// [`locate_session`] does for a Claude row, for a caller that has the directories and no
/// program (the daemon's check that a conversation was written down). The id is checked, and a symlinked
/// project directory or transcript is followed.
pub fn claude_transcript(roots: &[PathBuf], id: &str) -> Option<PathBuf> {
    match locate_claude(id, roots).ok()? {
        Located::File(path) => Some(path),
        Located::OpencodeDb(_) => None,
    }
}

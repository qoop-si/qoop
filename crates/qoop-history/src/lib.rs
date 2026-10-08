//! Agent transcript history: read another agent's recent transcript window.
//!
//! Pure readers over the agent products' own stores (Claude Code, Codex CLI,
//! opencode, Gemini CLI). No daemon, no socket, no stdout, no network: the
//! caller resolves `(program, session id)` (in the CLI via one `ListAgents`
//! roundtrip) and this crate maps it to messages. Reads are bounded (see
//! `read`), ids are allowlisted (see `locate`), and a missing id is always
//! an error, never a "newest file" guess.

#![forbid(unsafe_code)]

pub mod dir;
pub mod locate;
pub mod model;
pub mod select;

mod claude;
mod codex;
mod gemini;
mod opencode;
mod read;
mod ts;

pub use dir::{Repo, Typed, TypedIn, typed_in};
pub use locate::{BaseDirs, Located, detect_product, valid_id};
pub use model::{Message, Product, Role};
pub use ts::date_utc;

/// One transcript window: the last messages, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Window {
    pub session: SessionId,
    pub messages: Vec<Message>,
    /// True when the file extends past the tail window: older content exists
    /// that was deliberately not scanned. Never true for opencode, whose
    /// database reads whole.
    pub truncated: bool,
    /// Corrupt lines met and skipped while parsing.
    pub skipped_lines: usize,
}

/// Which transcript a window came from. Every output carries this pin, so a
/// reused terminal can never present as its predecessor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionId {
    pub product: Product,
    pub id: String,
}

/// Failures, all printable: the CLI renders these verbatim on stderr.
/// Absolute filesystem paths never appear here.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read transcript: {0}")]
    Io(String),
    #[error("cannot find a home directory")]
    NoHome,
    #[error("refusing: session id for {product} is not an id shape")]
    BadId { product: &'static str },
    #[error("no session id reported for this {product} terminal: `qoop hooks install` reports it")]
    MissingId { product: &'static str },
    #[error("no transcript detection for program {program:?}")]
    UnknownProgram { program: String },
    #[error("no transcript reader for {product}")]
    NoReader { product: String },
    #[error("no {product} transcript for session id {id}")]
    NotFound { product: &'static str, id: String },
    #[error("transcript file is larger than the readable size")]
    TooLarge,
    #[error("transcript database unreadable: {0}")]
    Store(String),
}

/// Read the last `last` messages of a terminal's transcript.
///
/// `program` is the agents row's program (detection via `qoop-detect`);
/// `session_id` the row's reported conversation id (`None` when the hook
/// never reported). `role` narrows to one side of the conversation.
pub fn history(
    program: &str,
    session_id: Option<&str>,
    dirs: &BaseDirs,
    last: usize,
    role: Option<Role>,
) -> Result<Window, Error> {
    let product = locate::detect_product(program)?;
    let id = session_id
        .filter(|s| !s.is_empty())
        .ok_or(Error::MissingId {
            product: product.as_str(),
        })?;
    if !locate::valid_id(id) {
        return Err(Error::BadId {
            product: product.as_str(),
        });
    }
    let located = locate::locate(product, id, dirs)?;
    let (all, skipped, truncated) = load(product, id, &located)?;
    Ok(Window {
        session: SessionId {
            product,
            id: id.to_string(),
        },
        messages: select::last_n(&all, last, role),
        truncated,
        skipped_lines: skipped,
    })
}

/// Load every message inside the bounded window.
/// Returns `(messages, corrupt_lines, file_extends_past_window)`.
fn load(
    product: Product,
    id: &str,
    located: &Located,
) -> Result<(Vec<Message>, usize, bool), Error> {
    match located {
        Located::OpencodeDb(db) => {
            let (msgs, skipped) = opencode::read_session(db, id)?.ok_or(Error::NotFound {
                product: product.as_str(),
                id: id.to_string(),
            })?;
            Ok((msgs, skipped, false))
        }
        Located::File(path) => {
            if product == Product::Gemini {
                return load_gemini(path);
            }
            let (lines, clipped) = read::read_tail_lines(path)?;
            let (msgs, skipped) = match product {
                Product::Claude => claude::parse_lines(&lines),
                Product::Codex => codex::parse_lines(&lines),
                Product::Gemini => unreachable!("handled above"),
                Product::Opencode => unreachable!("opencode reads its database"),
            };
            Ok((msgs, skipped, clipped))
        }
    }
}

/// Gemini chats files are whole JSON documents, not line streams.
fn load_gemini(path: &std::path::Path) -> Result<(Vec<Message>, usize, bool), Error> {
    let len = std::fs::metadata(path)
        .map_err(|e| Error::Io(e.to_string()))?
        .len();
    if len > read::TAIL_WINDOW_BYTES {
        return Err(Error::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?;
    // Chats docs are JSON but not guaranteed UTF-8; decode lossy like the
    // line readers do. Undecodable spans fail value parsing downstream and
    // are counted, never fatal.
    let text = String::from_utf8_lossy(&bytes);
    match gemini::parse_file(&text) {
        // The file matched at locate time but no longer parses (a writer
        // raced us): count it skipped rather than failing the whole call.
        Some((_, msgs)) => Ok((msgs, 0, false)),
        None => Ok((Vec::new(), 1, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversize_gemini_doc_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(read::TAIL_WINDOW_BYTES + 1)
            .unwrap();
        assert!(matches!(load_gemini(&path), Err(Error::TooLarge)));
    }

    #[test]
    fn non_utf8_gemini_bytes_decode_lossy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chats.json");
        let mut raw = br#"{"sessionId":"g1","messages":[{"id":"m","timestamp":"2026-03-24T11:21:49.937Z","type":"user","content":"hi "#.to_vec();
        raw.extend_from_slice(&[0xff, 0xfe]);
        raw.extend_from_slice(br#""}]}"#);
        std::fs::write(&path, raw).unwrap();
        let (msgs, _, _) = load_gemini(&path).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }
}

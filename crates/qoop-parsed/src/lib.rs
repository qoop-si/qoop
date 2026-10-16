//! One agent session as a compact, capped, redacted [`Digest`] for a chat view.
//!
//! Claude Code, Codex, Gemini CLI and OpenCode sessions are read with `agentparse` and cut down
//! to rows a client can draw: [`locate_session`] finds the file, [`load`] reads its tail (or all
//! of it, where the format needs that) and returns the newest items, [`detail`] returns the full
//! text of one tool call on demand. Nothing here talks to a daemon, a terminal or a socket, and
//! no raw record ever leaves it.

#![forbid(unsafe_code)]

mod detail;
mod item;
mod load;
mod locate;
mod project;
mod redact;
mod text;

pub use detail::detail;
pub use item::{Chip, Detail, Digest, Item, Status};
pub use load::{Error, Options, load};
pub use locate::{Dirs, Source, locate_session};
pub use redact::redact;

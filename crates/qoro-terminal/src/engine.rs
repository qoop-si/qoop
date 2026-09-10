//! The terminal engine seam.
//!
//! Nothing outside this crate may name a concrete engine type. That is what keeps ADR-003's
//! re-evaluation gate real rather than theoretical: swapping the engine is a change to one
//! crate plus a corpus run, not an architectural change.

use bytes::Bytes;

/// A contiguous run of damaged lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LineDamage {
    pub line: usize,
    pub left: usize,
    pub right: usize,
}

/// What changed after feeding bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Everything. Sent after a resize or a full reset.
    Full,
    /// These lines, and no others.
    Lines(Vec<LineDamage>),
}

impl Damage {
    pub fn is_empty(&self) -> bool {
        matches!(self, Damage::Lines(l) if l.is_empty())
    }
}

/// Something the engine needs the caller to do.
///
/// This is not optional plumbing. An application that emits a Device Status Report waits
/// for the answer, and an engine adapter that dropped the outbound event would hang it
/// while every other check still passed. `Reply` is written back to the PTY ahead of user
/// input and against a reserved capacity, so a saturated input queue cannot deadlock it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEffect {
    Reply(Bytes),
    Title(String),
    Bell,
    /// Policy gated. Never performed automatically.
    ClipboardWrite(Bytes),
}

/// Authoritative cell state for a client. A client never parses raw bytes; the daemon owns
/// the only parser, so a client cannot end up with a different idea of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    pub cursor: (usize, usize),
    /// One string per visible row, right-trimmed. Styling is carried separately once the
    /// terminal codec needs it; the shape here is what the M1 gates assert against.
    pub lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TerminalModes {
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    pub application_cursor: bool,
}

pub trait TerminalEngine: Send {
    /// Feed bytes. Returns what changed and what the caller must do about it.
    fn feed(&mut self, bytes: &[u8]) -> (Damage, Vec<TerminalEffect>);
    fn resize(&mut self, cols: u16, rows: u16);
    /// Authoritative cell state for a client.
    fn snapshot(&self) -> Snapshot;
    fn modes(&self) -> TerminalModes;
    /// Lines that have scrolled out of the visible region since the last call, oldest
    /// first, for the disk spill.
    ///
    /// The engine emits no eviction event, and one 64 KiB read scrolls more than the whole
    /// in-memory window, so this must be drained between parse sub-slices rather than once
    /// per read. Draining it per read loses history silently.
    fn take_scrolled_off(&mut self) -> Vec<String>;
}

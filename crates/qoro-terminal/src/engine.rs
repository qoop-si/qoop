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
    /// One string per visible row, right-trimmed.
    pub lines: Vec<String>,
    /// How each row is coloured, as runs rather than as cells.
    ///
    /// Separate from `lines` and not woven into it, for two reasons. A row of text with
    /// escape sequences in it is no longer addressable by column, and every gate and the
    /// whole conformance corpus assert against plain rows. And a run is what a screen
    /// actually looks like: a line of output has one or two colour changes, not eighty.
    ///
    /// Empty when a row is entirely unstyled, which is the common case and costs nothing.
    pub styles: Vec<Vec<Span>>,
}

/// A run of columns that share one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// First column of the run, counted in cells like everything else here.
    pub at: usize,
    pub len: usize,
    pub style: Style,
}

/// What a cell looks like, in the terminal's own vocabulary.
///
/// Indexed colours stay indexed rather than being resolved to rgb here: the palette
/// belongs to whoever is drawing, and resolving it in the daemon would bake this machine's
/// theme into what every client sees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Colour,
    pub bg: Colour,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
    pub strike: bool,
}

impl Style {
    /// Nothing set, which is the state most cells are in.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colour {
    /// Whatever the person's terminal calls foreground or background.
    #[default]
    Default,
    /// A palette entry, 0 to 255.
    Indexed(u8),
    Rgb(u8, u8, u8),
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

    /// The visible screen as plain text, with no colour and no run lists.
    ///
    /// Cheaper than a snapshot, which also builds a style run list per row, and that is the
    /// whole reason it exists: the rules that decide what an agent is doing read text and
    /// nothing else.
    ///
    /// The whole screen and not the bottom of it, even though every bundled rule looks at
    /// the bottom. The rules choose their own region, and a caller that pre trimmed handed
    /// them the wrong end: a screen taller than the trim put the agent's question above the
    /// slice and the rules matched nothing at all.
    fn visible_text(&self) -> Vec<String>;
    fn modes(&self) -> TerminalModes;
    /// Lines that have scrolled out of the visible region since the last call, oldest
    /// first, for the disk spill.
    ///
    /// The engine emits no eviction event, and one 64 KiB read scrolls more than the whole
    /// in-memory window, so this must be drained between parse sub-slices rather than once
    /// per read. Draining it per read loses history silently.
    fn take_scrolled_off(&mut self) -> Vec<String>;
}

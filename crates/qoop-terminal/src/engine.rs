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
    /// A completed image envelope (Kitty or iTerm2), verbatim. The parser never
    /// saw it: it is a side channel beside the grid, not cells in it.
    Image(crate::images::CapturedImage),
    /// A shell's command mark (OSC 133). The parser ignores the sequence; this is the only
    /// place it is seen.
    Shell(crate::control_string::ShellMark),
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
    /// What the program in this terminal has asked its terminal to be.
    ///
    /// Part of the snapshot rather than a separate question, because a client acts on it at
    /// the same moment it draws: whether a wheel is the program's to handle or the
    /// interface's depends on whether the program asked for the mouse, and asking in a
    /// second round trip is asking about a screen that has already changed.
    pub modes: TerminalModes,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Colour,
    pub bg: Colour,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
    pub strike: bool,
    /// Where the text in this run points, when the program wrapped it in an `OSC 8` link.
    ///
    /// Shared rather than copied per cell: a link is usually a whole word or a whole path,
    /// and the run is built once for all of its cells. The target is kept as the program
    /// wrote it; whoever writes it into an escape sequence again cleans it there.
    pub link: Option<std::sync::Arc<str>>,
}

impl Style {
    /// Nothing set, which is the state most cells are in. A link counts: a linked run in
    /// the default colours is still something a client has to be told about.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalModes {
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    pub application_cursor: bool,
    /// Whether the program wants its cursor seen. Off while it redraws, so a
    /// client that shows one anyway paints a cursor the program hid: flicker.
    pub cursor_visible: bool,
    /// Whether the program asked to be told when its window gains and loses the focus
    /// (`CSI ? 1004 h`), so a client knows whether to pass that on.
    pub focus_events: bool,
}

impl Default for TerminalModes {
    /// Visible. A mode set built from nothing is the terminal before any
    /// program hid anything, and a hidden default would take the cursor off
    /// every pane whose engine never said otherwise.
    fn default() -> Self {
        Self {
            alt_screen: false,
            bracketed_paste: false,
            mouse_reporting: false,
            application_cursor: false,
            cursor_visible: true,
            focus_events: false,
        }
    }
}

/// What the engine has thrown away rather than parsed or kept, counted since it started.
///
/// Each count is the one its own module keeps; this is how they leave the crate, so a
/// discard is sayable by whoever reports on the terminal rather than only by a test here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Discards {
    /// Control strings over `MAX_CONTROL_STRING`, dropped whole.
    pub control_strings: u64,
    /// The bytes that went with them.
    pub control_string_bytes: u64,
    /// Image envelopes over `MAX_IMAGE_BYTES` or past `IMAGE_DEADLINE`, dropped whole.
    pub images: u64,
    /// The bytes that went with them.
    pub image_bytes: u64,
    /// Feeds that found the scrollback ring full, and so may have lost rows unseen.
    pub evicted_unobserved: u64,
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
    /// The latest OSC 0/2 window title the child set, or empty.
    ///
    /// Retained for agent detection, which reads titles the child never draws as
    /// text (a spinner in the title bar, a confirmation notice). Defaulted so an
    /// engine that never sees OSC keeps the seam: detection reads an empty title
    /// and behaves exactly as before.
    fn title(&self) -> &str {
        ""
    }
    /// Forget the retained title: the foreground process changed, and the title
    /// described a process that is no longer there.
    fn clear_title(&mut self) {}
    /// The foreground and background of the terminal a person is looking at, when a client
    /// has said, so a program asking `OSC 10 ?` or `OSC 11 ?` learns the real ones.
    ///
    /// Defaulted so an engine that answers nothing keeps the seam: the question goes
    /// unanswered, which is what every program already copes with.
    fn set_host_colors(&mut self, _colors: Option<([u8; 3], [u8; 3])>) {}
    fn modes(&self) -> TerminalModes;
    /// Lines that have scrolled out of the visible region since the last call, oldest
    /// first, for the disk spill.
    ///
    /// The engine emits no eviction event, and one 64 KiB read scrolls more than the whole
    /// in-memory window, so this must be drained between parse sub-slices rather than once
    /// per read. Draining it per read loses history silently.
    fn take_scrolled_off(&mut self) -> Vec<String>;
    /// Everything discarded so far. Not defaulted: an engine that answered zero without
    /// counting would make every discard silent again.
    fn discards(&self) -> Discards;
}

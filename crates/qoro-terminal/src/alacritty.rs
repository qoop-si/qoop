//! The `alacritty_terminal` adapter.
//!
//! This is the only file in the workspace that names the concrete engine. ADR-003 chose it
//! for milestones 1 and 2 with a re-evaluation gate at M2, and the seam is what makes that
//! gate real.
//!
//! Two measured constraints are encoded here rather than left to a future reader:
//!
//! * `scrolling_history` must be set explicitly. The engine's default is 10 000, which is a
//!   46 MiB per terminal configuration.
//! * The engine allocates scrollback in fixed 1000 row chunks, so any value below 1000
//!   costs exactly the same as 1000. 1000 is therefore the cheapest non-zero setting and
//!   anything smaller is a false economy.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::cell::Hyperlink;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Rgb};
use bytes::Bytes;

use crate::control_string::ControlStringFilter;
use crate::engine::{
    Colour, Damage, Discards, LineDamage, Snapshot, Span, Style, TerminalEffect, TerminalEngine,
    TerminalModes,
};

/// The engine allocates scrollback in 1000 row chunks, so this is the cheapest non-zero
/// setting. See ADR-003 for the measurements.
pub const SCROLLBACK_ROWS: usize = 1000;

/// Where the engine keeps the default foreground and background in its colour table: the
/// indices an `OSC 10 ?` and an `OSC 11 ?` ask about.
const FOREGROUND: usize = alacritty_terminal::vte::ansi::NamedColor::Foreground as usize;
const BACKGROUND: usize = alacritty_terminal::vte::ansi::NamedColor::Background as usize;

/// Collects the engine's outbound events so they can be returned as effects rather than
/// dropped. Dropping them is how an application waiting for a cursor position report hangs.
#[derive(Clone)]
struct Collector(Sender<Event>);

impl EventListener for Collector {
    fn send_event(&self, event: Event) {
        // A full or disconnected channel here would mean the caller stopped draining, which
        // cannot happen: `feed` drains it before returning.
        let _ = self.0.send(event);
    }
}

pub struct AlacrittyEngine {
    term: Term<Collector>,
    parser: Processor,
    events: Receiver<Event>,
    /// Rows that have left the visible region since the last drain, oldest first.
    scrolled_off: Vec<String>,
    /// The latest OSC 0/2 title, retained for agent detection.
    title: String,
    evicted_unobserved: u64,
    filter: ControlStringFilter,
    /// Reused across feeds so that bounding the stream does not allocate on every read.
    filtered: Vec<u8>,
    /// Image envelopes, caught beside the parser. The parser never sees them.
    sniffer: crate::images::ImageSniffer,
    /// The sniffer's output: child bytes with image envelopes removed.
    unsniffed: Vec<u8>,
    /// A character that a read cut in half, held until the rest of it arrives.
    ///
    /// At most three bytes, because no UTF-8 sequence is longer than four. See `feed` for
    /// why the parser is never allowed to see one of these.
    partial_char: Vec<u8>,
    /// The outer terminal's foreground and background, as a client last reported them.
    host_colors: Option<([u8; 3], [u8; 3])>,
}

impl AlacrittyEngine {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (tx, rx) = channel();
        let config = Config {
            scrolling_history: SCROLLBACK_ROWS,
            ..Config::default()
        };
        Self {
            term: Term::new(
                config,
                &TermSize::new(cols as usize, rows as usize),
                Collector(tx),
            ),
            parser: Processor::new(),
            events: rx,
            filter: ControlStringFilter::default(),
            filtered: Vec::new(),
            sniffer: crate::images::ImageSniffer::new(),
            unsniffed: Vec::new(),
            partial_char: Vec::new(),
            scrolled_off: Vec::new(),
            title: String::new(),
            evicted_unobserved: 0,
            host_colors: None,
        }
    }

    /// One row's colour, as runs of columns that look the same.
    ///
    /// Runs and not cells: a line of output changes colour once or twice, and sending
    /// eighty styles for eighty columns would make a snapshot an order of magnitude bigger
    /// for nothing. A row with no colour at all returns an empty list, which is most rows
    /// and costs nothing to send.
    ///
    /// Trailing plain cells are dropped for the same reason `row_text` trims: the rest of
    /// the line is the terminal's default and saying so is not information.
    /// One row, as the text a person reads and the runs that colour it.
    ///
    /// Runs and not cells: a line of output changes colour once or twice, and sending
    /// eighty styles for eighty columns would make a snapshot an order of magnitude bigger
    /// for nothing. A row with no colour at all returns an empty list, which is most rows
    /// and costs nothing to send.
    ///
    /// Trailing plain cells are dropped for the same reason the text is trimmed: the rest
    /// of the line is the terminal's default, and saying so is not information.
    ///
    /// Links come from `links`, which holds each distinct target once for the whole
    /// snapshot and refuses any past its budget; `None` reads the text alone.
    fn row(&self, line: i32, mut links: Option<&mut Links>) -> (String, Vec<Span>) {
        let grid = self.term.grid();
        let mut text = String::with_capacity(grid.columns());
        let mut spans: Vec<Span> = Vec::new();
        // The last run's look and link in the engine's own terms, so a cell is compared
        // without building the link's text: that is built once, when a run starts.
        let mut last: Option<(Style, Option<Hyperlink>)> = None;
        for col in 0..grid.columns() {
            let cell = &grid[Point::new(Line(line), Column(col))];
            text.push(cell.c);
            let here = (style_of(cell), cell.hyperlink());
            match spans.last_mut() {
                Some(span) if last.as_ref() == Some(&here) => span.len += 1,
                _ => {
                    let mut style = here.0.clone();
                    style.link = match (links.as_deref_mut(), here.1.as_ref()) {
                        (Some(links), Some(link)) => links.intern(link.uri()),
                        _ => None,
                    };
                    spans.push(Span {
                        at: col,
                        len: 1,
                        style,
                    });
                    last = Some(here);
                }
            }
        }
        while spans.last().is_some_and(|s| s.style.is_plain()) {
            spans.pop();
        }
        // Keep the text through the last styled cell, and through the last
        // non-space character besides: trimming past either would drop a
        // background the program painted (an erase with the background set,
        // a box drawn in spaces) or plain text after a styled run. Chars are
        // columns here: a wide character carries its spacer along, so a char
        // index is a cell index and no width table is needed.
        let styled = spans
            .iter()
            .filter(|s| !s.style.is_plain())
            .map(|s| s.at + s.len)
            .max()
            .unwrap_or(0);
        let plain = text.trim_end().chars().count();
        let keep = styled.max(plain);
        (text.chars().take(keep).collect(), spans)
    }

    /// The whole of what the engine's modes say, as the engine keeps them.
    pub fn mode_bits(&self) -> u32 {
        self.term.mode().bits()
    }

    /// Escape sequences that, fed to a fresh engine of the same size, rebuild this screen:
    /// the visible rows with their colours and links, the modes a program set once at its
    /// start and never repeats (bracketed paste, mouse, focus events, the keyboard
    /// protocol), the cursor and the pen, the title.
    ///
    /// For a daemon that is becoming another binary: the program in the terminal keeps
    /// running through it and the new daemon starts with nothing. Never written to the
    /// program, only fed to the engine.
    ///
    /// What it does not carry: the scrollback (on disk already), a scroll region, tab stops
    /// and character sets, the colour table, a sequence the parser was in the middle of, and
    /// the main screen under an alternate one. A program that repaints makes up for all of
    /// them.
    pub fn replay(&self) -> String {
        use std::fmt::Write;
        let grid = self.term.grid();
        let mode = *self.term.mode();
        let mut out = String::from("\x1b[0m\x1b[2J\x1b[H");
        if mode.contains(TermMode::ALT_SCREEN) {
            out.push_str("\x1b[?1049h\x1b[2J");
        }
        for line in 0..grid.screen_lines() {
            let row = &grid[Line(line as i32)];
            let end = (0..grid.columns())
                .rev()
                .find(|&col| !blank(&row[Column(col)]))
                .map_or(0, |col| col + 1);
            if end == 0 {
                continue;
            }
            let _ = write!(out, "\x1b[{};1H", line + 1);
            let mut pen = Pen::default();
            let mut link: Option<Hyperlink> = None;
            for col in 0..end {
                let cell = &row[Column(col)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let here = Pen::of(cell);
                if here != pen {
                    out.push_str(&here.sgr());
                    pen = here;
                }
                let target = cell.hyperlink();
                if target != link {
                    match &target {
                        Some(l) => {
                            let _ =
                                write!(out, "\x1b]8;id={};{}\x1b\\", clean(l.id()), clean(l.uri()));
                        }
                        None => out.push_str("\x1b]8;;\x1b\\"),
                    }
                    link = target;
                }
                out.push(cell.c);
                for z in cell.zerowidth().unwrap_or_default() {
                    out.push(*z);
                }
            }
            if link.is_some() {
                out.push_str("\x1b]8;;\x1b\\");
            }
            out.push_str("\x1b[0m");
        }
        let fresh = TermMode::default();
        for (flag, on, off) in MODE_SEQUENCES {
            match (mode.contains(*flag), fresh.contains(*flag)) {
                (true, false) => out.push_str(on),
                (false, true) => out.push_str(off),
                _ => {}
            }
        }
        let kitty = (mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> 18;
        if kitty != 0 {
            let _ = write!(out, "\x1b[={kitty};1u");
        }
        let style = self.term.cursor_style();
        if style != Default::default() {
            use alacritty_terminal::vte::ansi::CursorShape as Shape;
            let shape = match style.shape {
                Shape::Underline => 3,
                Shape::Beam => 5,
                _ => 1,
            };
            let _ = write!(out, "\x1b[{} q", shape + u8::from(!style.blinking));
        }
        if !self.title.is_empty() {
            let _ = write!(out, "\x1b]2;{}\x07", clean(&self.title));
        }
        let cursor = &grid.cursor;
        let _ = write!(
            out,
            "\x1b[{};{}H",
            cursor.point.line.0.max(0) + 1,
            cursor.point.column.0 + 1
        );
        if cursor.input_needs_wrap {
            // At the last column with the wrap still owed: the cell is written once more,
            // which leaves the cursor in that state and the screen as it was.
            let cell = &grid[cursor.point];
            out.push_str(&Pen::of(cell).sgr());
            out.push(cell.c);
        }
        out.push_str(&Pen::of(&cursor.template).sgr());
        out
    }

    /// Capture rows that have entered the history ring since the last feed.
    ///
    /// This is the half of the spill that works today. While the ring is filling, rows only
    /// arrive, so everything between the previous history size and the current one is new
    /// and is captured exactly.
    ///
    /// Once the ring saturates the engine starts evicting, and it emits no event when it
    /// does, so there is nothing to observe after the fact. Capturing those rows needs a
    /// mechanism this adapter does not have yet, and the honest options are recorded in
    /// ADR-003. Rather than pretend, saturation increments `evicted_unobserved`, which is a
    /// counted gap and is asserted by gate G2.7 in milestone 2, where scrollback spill is
    /// in scope. Milestone 1 has no disk history at all, so nothing here is load bearing
    /// yet, and a placeholder that silently produced empty rows would have been worse than
    /// a counter that says what is missing.
    /// Take everything that has scrolled into the engine's history, then empty it.
    ///
    /// The ring is a staging buffer, not a scrollback. It was used as one at first, with a
    /// cumulative watermark against `history_size()`, and that is only correct until the
    /// ring fills: after 1000 rows the size stops growing, the delta is permanently zero,
    /// and every row after that is evicted without ever being seen. Measured: 4000 lines
    /// in, 1000 rows out, and every other test green.
    ///
    /// Draining it here makes the count exact instead of cumulative, and it is what the
    /// design already calls for: scrollback lives on disk as rows, not in the engine as
    /// cells. ADR-003 measured the same configuration at 524 KiB per terminal against
    /// 5710 KiB with the ring kept full.
    fn capture_new_history(&mut self) {
        let history = {
            let grid = self.term.grid();
            grid.total_lines() - grid.screen_lines()
        };
        if history == 0 {
            return;
        }
        for i in (0..history).rev() {
            // History rows sit at negative line numbers, oldest most negative.
            // History keeps the text alone: what goes to disk as scrollback is rows of
            // text, and colour there would be a second format nobody reads back.
            self.scrolled_off.push(self.row(-(i as i32) - 1, None).0);
        }
        if history >= SCROLLBACK_ROWS {
            // The ring was full when we looked, so rows may have been evicted before we
            // got here. Only a scroll control sequence can do that now, since a slice is
            // bounded to fewer bytes than the ring holds rows, and no slice size can bound
            // `CSI 5000 S`. Counted rather than prevented.
            self.evicted_unobserved += 1;
        }
        self.term.grid_mut().clear_history();
    }

    /// How many feeds happened while the ring was already saturated, and therefore may have
    /// evicted rows this adapter could not observe. Reported, never silently zero.
    pub fn evicted_unobserved(&self) -> u64 {
        self.evicted_unobserved
    }
}

/// The most distinct link targets one snapshot carries, and the most bytes of them.
///
/// A snapshot crosses the socket on every render and has to fit the reassembly bound, and a
/// program chooses its links: a screen of cells each linked to its own four kilobyte
/// address was measured over eight megabytes, which no client could take, so the session
/// could not be attached to at all. A real screen holds a handful of links. Past either
/// bound a link is dropped and its text shows unlinked.
pub const MAX_LINKS: usize = 64;
pub const MAX_LINK_BYTES: usize = 16 * 1024;
/// The longest target kept, the same line a client draws at: anything longer would only
/// be dropped on the far side after crossing the socket.
pub const MAX_LINK_LEN: usize = 2048;

/// Each distinct link target in a snapshot, once, within the budget above.
#[derive(Default)]
struct Links {
    seen: Vec<Arc<str>>,
    bytes: usize,
}

impl Links {
    /// The shared target for `uri`, or `None` when it is not one a client would write
    /// (not visible ASCII, or too long) or the snapshot has spent its budget.
    fn intern(&mut self, uri: &str) -> Option<Arc<str>> {
        if let Some(known) = self.seen.iter().find(|known| ***known == *uri) {
            return Some(known.clone());
        }
        let writable = !uri.is_empty()
            && uri.len() <= MAX_LINK_LEN
            && uri.bytes().all(|b| (0x21..=0x7e).contains(&b));
        if !writable || self.seen.len() >= MAX_LINKS || self.bytes + uri.len() > MAX_LINK_BYTES {
            return None;
        }
        let shared: Arc<str> = Arc::from(uri);
        self.bytes += uri.len();
        self.seen.push(shared.clone());
        Some(shared)
    }
}

/// How many bytes at the end of `buf` are the start of a character that has not arrived.
///
/// Zero unless the tail really could still become a character: a lead byte with too few
/// continuation bytes after it. A lead byte followed by something that is not a
/// continuation byte is already invalid and is not held back, and neither is a sequence
/// that is already complete.
fn incomplete_utf8_tail(buf: &[u8]) -> usize {
    // At most the last three bytes can be an incomplete sequence, since four is the longest
    // sequence there is.
    let start = buf.len().saturating_sub(3);
    for at in start..buf.len() {
        let need = match buf[at] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => continue,
        };
        let have = buf.len() - at;
        if have >= need {
            // Complete, or invalid in a way more bytes cannot repair.
            continue;
        }
        // Every byte after the lead must be a continuation byte, or this is already
        // invalid and waiting would only delay the replacement character.
        if buf[at + 1..].iter().all(|b| b & 0xc0 == 0x80) {
            return have;
        }
    }
    0
}

/// The modes `replay` carries, with what sets each and what clears it. Only the ones that
/// differ from a fresh engine are written.
const MODE_SEQUENCES: &[(TermMode, &str, &str)] = &[
    (TermMode::SHOW_CURSOR, "\x1b[?25h", "\x1b[?25l"),
    (TermMode::APP_CURSOR, "\x1b[?1h", "\x1b[?1l"),
    (TermMode::APP_KEYPAD, "\x1b=", "\x1b>"),
    (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h", "\x1b[?1000l"),
    (TermMode::MOUSE_DRAG, "\x1b[?1002h", "\x1b[?1002l"),
    (TermMode::MOUSE_MOTION, "\x1b[?1003h", "\x1b[?1003l"),
    (TermMode::FOCUS_IN_OUT, "\x1b[?1004h", "\x1b[?1004l"),
    (TermMode::UTF8_MOUSE, "\x1b[?1005h", "\x1b[?1005l"),
    (TermMode::SGR_MOUSE, "\x1b[?1006h", "\x1b[?1006l"),
    (TermMode::ALTERNATE_SCROLL, "\x1b[?1007h", "\x1b[?1007l"),
    (TermMode::BRACKETED_PASTE, "\x1b[?2004h", "\x1b[?2004l"),
    (TermMode::LINE_WRAP, "\x1b[?7h", "\x1b[?7l"),
    (TermMode::ORIGIN, "\x1b[?6h", "\x1b[?6l"),
    (TermMode::INSERT, "\x1b[4h", "\x1b[4l"),
    (TermMode::LINE_FEED_NEW_LINE, "\x1b[20h", "\x1b[20l"),
];

/// A cell that draws nothing and says nothing: a space in the default colours.
fn blank(cell: &alacritty_terminal::term::cell::Cell) -> bool {
    cell.c == ' '
        && cell.zerowidth().is_none()
        && Pen::of(cell) == Pen::default()
        && cell.hyperlink().is_none()
}

/// Text that goes inside an escape sequence, with whatever could end it left out.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// How a cell is drawn, as far as `replay` has to say it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: alacritty_terminal::vte::ansi::Color,
    bg: alacritty_terminal::vte::ansi::Color,
    flags: Flags,
}

impl Default for Pen {
    fn default() -> Self {
        use alacritty_terminal::vte::ansi::{Color, NamedColor};
        Self {
            fg: Color::Named(NamedColor::Foreground),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::empty(),
        }
    }
}

impl Pen {
    fn of(cell: &alacritty_terminal::term::cell::Cell) -> Self {
        // Only what changes how a cell looks: the wide character and wrap bookkeeping are
        // not part of the pen.
        let drawn = Flags::INVERSE
            | Flags::BOLD
            | Flags::ITALIC
            | Flags::DIM
            | Flags::HIDDEN
            | Flags::STRIKEOUT
            | Flags::ALL_UNDERLINES;
        Self {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags & drawn,
        }
    }

    /// The sequence that sets exactly this, from any state.
    fn sgr(&self) -> String {
        let mut codes = String::from("0");
        let f = self.flags;
        for (flag, code) in [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "4:2"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ] {
            if f.contains(flag) {
                codes.push(';');
                codes.push_str(code);
            }
        }
        for (colour, base, extended) in [(self.fg, 30, 38), (self.bg, 40, 48)] {
            use alacritty_terminal::vte::ansi::Color;
            let code = match colour {
                Color::Named(named) => match named as usize {
                    n @ 0..=7 => Some(format!("{}", base + n)),
                    n @ 8..=15 => Some(format!("{}", base + 52 + n)),
                    _ => None,
                },
                Color::Indexed(i) => Some(format!("{extended};5;{i}")),
                Color::Spec(rgb) => Some(format!("{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
            };
            if let Some(code) = code {
                codes.push(';');
                codes.push_str(&code);
            }
        }
        format!("\x1b[{codes}m")
    }
}

/// One cell's style, in this crate's vocabulary rather than the engine's.
///
/// The engine's named colours are the sixteen ansi ones plus a handful of roles.
/// Foreground and background become `Default` because that is what they mean: whatever the
/// person's own terminal uses. Everything else keeps its palette index, so a client with a
/// different theme draws its own colours rather than this machine's.
fn style_of(cell: &alacritty_terminal::term::cell::Cell) -> Style {
    use alacritty_terminal::term::cell::Flags;
    Style {
        fg: colour_of(cell.fg),
        bg: colour_of(cell.bg),
        bold: cell.flags.contains(Flags::BOLD),
        italic: cell.flags.contains(Flags::ITALIC),
        underline: cell.flags.intersects(
            Flags::UNDERLINE
                | Flags::DOUBLE_UNDERLINE
                | Flags::UNDERCURL
                | Flags::DOTTED_UNDERLINE
                | Flags::DASHED_UNDERLINE,
        ),
        inverse: cell.flags.contains(Flags::INVERSE),
        dim: cell.flags.contains(Flags::DIM),
        strike: cell.flags.contains(Flags::STRIKEOUT),
        // Filled per run in `row`, so a cell is never made to build the link's text.
        link: None,
    }
}

fn colour_of(c: alacritty_terminal::vte::ansi::Color) -> Colour {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    match c {
        Color::Named(NamedColor::Foreground | NamedColor::Background) => Colour::Default,
        Color::Named(named) => Colour::Indexed(named as u8),
        Color::Indexed(i) => Colour::Indexed(i),
        Color::Spec(rgb) => Colour::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

impl TerminalEngine for AlacrittyEngine {
    fn feed(&mut self, bytes: &[u8]) -> (Damage, Vec<TerminalEffect>) {
        self.term.reset_damage();
        // Images leave the stream here, before the bound below: a picture is always
        // larger than a control string may be, and it is a side channel, not grid
        // cells. What the sniffer holds back is an incomplete envelope tail, exactly
        // like the split character held back further down.
        self.unsniffed.clear();
        self.sniffer.feed(bytes, &mut self.unsniffed);
        // The parser's own control-string buffer is unbounded, so the bound goes in front
        // of it. See `control_string` for the measurement that made this necessary.
        self.filtered.clear();
        // Whatever the last read cut in half goes back on the front, so the parser sees
        // characters and never halves of them.
        self.filtered.append(&mut self.partial_char);
        self.filter.feed(&self.unsniffed, &mut self.filtered);

        // A character split across two reads is held back rather than handed over.
        //
        // `vte` 0.15.0 resumes a split character by copying up to four bytes into its
        // partial buffer, and if that window also holds a complete second character
        // followed by an invalid byte it prints the first and drops the rest. Its own
        // source says so: "Since we only care about the first character, we just ignore
        // the rest" (`vte-0.15.0/src/lib.rs`, `advance_partial_utf8`). Measured, before
        // this: feeding `[0xc3, 0xa9, 0x20, 0x80]` in one call leaves the cursor at column
        // 2, and feeding `[0xc3]` then the rest leaves it at column 1, with the space gone
        // for good. A pty read landing on that boundary silently loses a character, and no
        // version bump fixes it because both crates are already at their latest published
        // version.
        //
        // So the parser is never given a split character to resume: the incomplete tail is
        // carried to the next read, which is also what makes the split-boundary property in
        // gate G1.4 hold for these bytes rather than having to exclude them. Only a
        // sequence that could still become a character is held; an already invalid one is
        // passed straight through, so the parser draws its replacement character now rather
        // than waiting for bytes that would not help. The bound is three bytes, and the one
        // thing it gives up is a stream that ends mid character: those bytes are dropped
        // instead of becoming a replacement character, which is a screen that ends one
        // glyph short on a terminal that is closing.
        let keep = incomplete_utf8_tail(&self.filtered);
        let hand_over = self.filtered.len() - keep;
        self.partial_char
            .extend_from_slice(&self.filtered[hand_over..]);
        self.parser
            .advance(&mut self.term, &self.filtered[..hand_over]);

        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            TermDamage::Partial(iter) => Damage::Lines(
                iter.map(|d| LineDamage {
                    line: d.line,
                    left: d.left,
                    right: d.right,
                })
                .collect(),
            ),
        };
        self.term.reset_damage();
        self.capture_new_history();

        let mut effects = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::PtyWrite(s) => effects.push(TerminalEffect::Reply(Bytes::from(s))),
                Event::Title(t) => {
                    // Retained as well as reported: detection reads the latest
                    // title long after the effect was drained.
                    self.title = t.clone();
                    effects.push(TerminalEffect::Title(t));
                }
                Event::ResetTitle => {
                    self.title.clear();
                    effects.push(TerminalEffect::Title(String::new()));
                }
                Event::Bell => effects.push(TerminalEffect::Bell),
                Event::ClipboardStore(_, s) => {
                    effects.push(TerminalEffect::ClipboardWrite(Bytes::from(s)))
                }
                // `OSC 10 ?` and `OSC 11 ?`: the colour the program set itself if it did, else
                // the one the person is looking at, else no answer, as before any client said.
                Event::ColorRequest(index @ (FOREGROUND | BACKGROUND), answer) => {
                    let host = self.host_colors.map(|(fg, bg)| {
                        let [r, g, b] = if index == FOREGROUND { fg } else { bg };
                        Rgb { r, g, b }
                    });
                    if let Some(rgb) = self.term.colors()[index].or(host) {
                        effects.push(TerminalEffect::Reply(Bytes::from(answer(rgb))));
                    }
                }
                _ => {}
            }
        }
        for img in self.sniffer.take_images() {
            effects.push(TerminalEffect::Image(img));
        }
        (damage, effects)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term
            .resize(TermSize::new(cols as usize, rows as usize));
    }

    fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let rows = grid.screen_lines();
        let cursor = grid.cursor.point;
        // One walk of the grid for both, because they are the same walk: reading the row
        // twice doubled the cost of a snapshot, and a snapshot is what a streaming terminal
        // produces on every read.
        let (mut lines, mut styles) = (Vec::with_capacity(rows), Vec::with_capacity(rows));
        let mut links = Links::default();
        for line in 0..rows {
            let (text, spans) = self.row(line as i32, Some(&mut links));
            lines.push(text);
            styles.push(spans);
        }
        Snapshot {
            cols: grid.columns(),
            rows,
            cursor: (cursor.line.0.max(0) as usize, cursor.column.0),
            lines,
            styles,
            modes: self.modes(),
        }
    }

    fn visible_text(&self) -> Vec<String> {
        let rows = self.term.grid().screen_lines();
        (0..rows).map(|l| self.row(l as i32, None).0).collect()
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn clear_title(&mut self) {
        self.title.clear();
    }

    fn set_host_colors(&mut self, colors: Option<([u8; 3], [u8; 3])>) {
        self.host_colors = colors;
    }

    fn modes(&self) -> TerminalModes {
        let m = *self.term.mode();
        TerminalModes {
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            mouse_reporting: m.intersects(TermMode::MOUSE_MODE),
            application_cursor: m.contains(TermMode::APP_CURSOR),
            cursor_visible: m.contains(TermMode::SHOW_CURSOR),
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    fn take_scrolled_off(&mut self) -> Vec<String> {
        std::mem::take(&mut self.scrolled_off)
    }

    fn discards(&self) -> Discards {
        let (control_strings, control_string_bytes) = self.filter.discarded();
        let (images, image_bytes) = self.sniffer.discarded();
        Discards {
            control_strings,
            control_string_bytes,
            images,
            image_bytes,
            evicted_unobserved: self.evicted_unobserved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> AlacrittyEngine {
        AlacrittyEngine::new(80, 24)
    }

    #[test]
    fn every_discard_leaves_the_crate_through_the_engine() {
        // The counters live in the filter, the sniffer and the adapter; nothing outside
        // this crate can reach the first two, so the engine is the one way out.
        let mut e = engine();
        assert_eq!(e.discards(), Discards::default());
        let mut osc = b"\x1b]0;".to_vec();
        osc.extend(std::iter::repeat_n(b'A', crate::MAX_CONTROL_STRING * 2));
        osc.push(0x07);
        e.feed(&osc);
        let burst: String = std::iter::repeat_n("x\r\n", SCROLLBACK_ROWS * 3).collect();
        e.feed(burst.as_bytes());
        let d = e.discards();
        assert_eq!(d.control_strings, 1, "{d:?}");
        assert!(
            d.control_string_bytes > crate::MAX_CONTROL_STRING as u64,
            "{d:?}"
        );
        assert!(d.evicted_unobserved > 0, "{d:?}");
        assert_eq!(d.evicted_unobserved, e.evicted_unobserved());
    }

    /// The byte sequence and the split that lose a character in the pinned parser.
    ///
    /// Deterministic on purpose. The split-boundary property test in
    /// `tests/split_equivalence.rs` covers this shape in principle, but measured, with the
    /// hold back removed it still passed six runs of ten thousand cases: the trigger needs
    /// a split inside a character, a complete character after it, and an invalid byte after
    /// that, all within four bytes, which the generator reaches too rarely to rely on. This
    /// test reaches it every time.
    #[test]
    fn a_character_split_across_two_reads_does_not_lose_the_one_after_it() {
        // e-acute, a space, and a byte that cannot start a character.
        let bytes = [0xc3u8, 0xa9, 0x20, 0x80];

        let mut whole = engine();
        whole.feed(&bytes);

        let mut split = engine();
        split.feed(&bytes[..1]);
        split.feed(&bytes[1..]);

        assert_eq!(
            split.snapshot(),
            whole.snapshot(),
            "a read that ended inside the first character lost what came after it"
        );
    }

    /// An erase with the background set keeps the background: opencode draws
    /// its input box this way, text plus erase-to-end under one grey, and a
    /// trim past the text would show default where the program shows grey.
    /// Plain text past a styled run keeps too: only trailing blanks go.
    #[test]
    fn an_erase_with_the_background_set_keeps_the_background() {
        let mut e = engine();
        e.feed(b"\x1b[48;2;30;30;30mhi\x1b[K");
        let s = e.snapshot();
        let end = s.styles[0]
            .iter()
            .map(|sp| sp.at + sp.len)
            .max()
            .unwrap_or(0);
        assert!(end > 10, "the erase filled nothing: {s:?}");
        assert!(
            s.lines[0].chars().count() >= end,
            "the text was trimmed past styled cells: {:?}",
            s.lines[0]
        );
        // Styled then plain: the plain tail survives the styled head.
        let mut e = engine();
        e.feed(b"\x1b[31mhi\x1b[0mbye");
        let s = e.snapshot();
        assert!(
            s.lines[0].starts_with("hibye"),
            "plain text after a styled run vanished: {:?}",
            s.lines[0]
        );
    }

    /// A character arriving one byte at a time is drawn once, and drawn whole.
    #[test]
    fn a_character_arriving_a_byte_at_a_time_is_drawn_once_it_is_complete() {
        // A three byte character, then a four byte one.
        let bytes = "\u{4e2d}\u{1f600}".as_bytes().to_vec();
        let mut split = engine();
        for byte in &bytes {
            split.feed(&[*byte]);
        }
        let mut whole = engine();
        whole.feed(&bytes);
        assert_eq!(split.snapshot(), whole.snapshot());
    }

    /// An invalid sequence is not held back waiting for bytes that cannot repair it.
    #[test]
    fn an_invalid_sequence_is_handed_over_rather_than_held() {
        // A lead byte that promises two more, followed by an ASCII byte instead.
        let mut engine = engine();
        engine.feed(&[0xe0, b'A']);
        // Nothing further is fed: if the pair were held back, the screen would be empty.
        assert!(
            !engine.snapshot().lines[0].is_empty(),
            "an invalid sequence was held back, so the screen shows nothing until more \
             bytes arrive that cannot help it"
        );
    }

    #[test]
    fn an_incomplete_tail_is_only_what_could_still_become_a_character() {
        // Complete sequences hold nothing back.
        assert_eq!(incomplete_utf8_tail(b"abc"), 0);
        assert_eq!(incomplete_utf8_tail("\u{e9}".as_bytes()), 0);
        assert_eq!(incomplete_utf8_tail("\u{4e2d}".as_bytes()), 0);
        assert_eq!(incomplete_utf8_tail("\u{1f600}".as_bytes()), 0);
        // Incomplete ones hold back exactly what has arrived.
        assert_eq!(incomplete_utf8_tail(&[0xc3]), 1);
        assert_eq!(incomplete_utf8_tail(&[b'x', 0xe4, 0xb8]), 2);
        assert_eq!(incomplete_utf8_tail(&[0xf0, 0x9f, 0x98]), 3);
        // Already invalid, so waiting would only delay the replacement character.
        assert_eq!(incomplete_utf8_tail(&[0xc3, b'A']), 0);
        assert_eq!(incomplete_utf8_tail(&[0xff]), 0);
        assert_eq!(incomplete_utf8_tail(&[0x80]), 0);
    }

    /// The engine's answer to the same question: a cancelled control string must leave the
    /// terminal usable, not blank from that point on.
    #[test]
    fn text_after_a_cancelled_control_string_still_renders() {
        let mut e = engine();
        e.feed(b"\x1b]0;title\x18");
        e.feed(b"ordinary text");
        assert!(
            e.snapshot().lines[0].contains("ordinary text"),
            "the screen is {:?}",
            e.snapshot().lines[0]
        );
    }

    /// The three byte prefix that bypassed the bound: `ESC [ 1 ESC ]` leaves the parser in
    /// an OSC while a scanner without an anywhere transition for ESC thinks it is at
    /// ground.
    #[test]
    fn an_escape_inside_a_csi_cannot_open_an_unbounded_control_string() {
        const FED: usize = 4 * 1024 * 1024;
        let mut e = engine();
        e.feed(b"\x1b[1\x1b]0;");
        let slice = vec![b'A'; 64 * 1024];
        for _ in 0..(FED / slice.len()) {
            e.feed(&slice);
        }
        let (_, effects) = e.feed(b"\x07");
        let retained: usize = effects
            .iter()
            .map(|f| match f {
                TerminalEffect::Title(t) => t.len(),
                _ => 0,
            })
            .sum();
        assert!(
            retained < FED / 8,
            "the parser retained {retained} bytes after a three byte prefix"
        );
    }

    /// G1.22, the half the runtime gate cannot see: an unterminated control string must not
    /// accumulate. The runtime gate proves the parser recovers; this proves it did not buy
    /// that recovery by holding every byte it was given.
    ///
    /// The assertion is on the title the parser hands back after the terminator, because
    /// that is the only externally visible measure of what it retained. Feeding four
    /// megabytes and getting four megabytes back would mean the bound does not exist.
    #[test]
    fn an_unterminated_control_string_is_bounded() {
        const FED: usize = 4 * 1024 * 1024;
        let mut e = engine();
        e.feed(b"\x1b]0;");
        // In slices, the way it arrives from a pty, so that a bound that only holds for a
        // single large write would not pass.
        let slice = vec![b'A'; 64 * 1024];
        for _ in 0..(FED / slice.len()) {
            e.feed(&slice);
        }
        let (_, effects) = e.feed(b"\x07");
        let retained: usize = effects
            .iter()
            .map(|f| match f {
                TerminalEffect::Title(t) => t.len(),
                _ => 0,
            })
            .sum();
        assert!(
            retained < FED / 8,
            "the parser retained {retained} bytes of a {FED} byte control string"
        );
        // And it is still a working terminal afterwards.
        e.feed(b"hello");
        assert!(e.snapshot().lines[0].contains("hello"));
    }

    #[test]
    fn plain_text_reaches_the_snapshot() {
        let mut e = engine();
        e.feed(b"hello qoro\r\nsecond\r\n");
        let s = e.snapshot();
        assert_eq!(s.lines[0], "hello qoro");
        assert_eq!(s.lines[1], "second");
        assert_eq!(s.cols, 80);
        assert_eq!(s.rows, 24);
    }

    #[test]
    fn a_cursor_position_request_produces_a_reply_effect() {
        // The hang this prevents: an application emits DSR and blocks until it is answered.
        // An adapter that dropped the engine's outbound event would pass every other test.
        let mut e = engine();
        let (_, effects) = e.feed(b"\x1b[6n");
        let reply = effects
            .iter()
            .find_map(|x| match x {
                TerminalEffect::Reply(b) => Some(b.clone()),
                _ => None,
            })
            .expect("a cursor position request must produce a reply");
        let text = String::from_utf8_lossy(&reply);
        assert!(text.starts_with("\x1b["), "unexpected reply {text:?}");
        assert!(text.ends_with('R'), "unexpected reply {text:?}");
    }

    #[test]
    fn a_title_sequence_produces_a_title_effect() {
        let mut e = engine();
        let (_, effects) = e.feed(b"\x1b]0;my title\x07");
        assert!(effects.contains(&TerminalEffect::Title("my title".into())));
    }

    #[test]
    fn the_latest_title_is_retained_for_detection_until_cleared() {
        let mut e = engine();
        assert_eq!(e.title(), "");
        e.feed(b"\x1b]0;first\x07");
        assert_eq!(e.title(), "first");
        e.feed(b"plain output\r\n");
        assert_eq!(e.title(), "first", "screen output must not move the title");
        e.feed(b"\x1b]2;second\x07");
        assert_eq!(
            e.title(),
            "second",
            "OSC 2 replaces OSC 0 like any title set"
        );
        e.clear_title();
        assert_eq!(e.title(), "");
    }

    #[test]
    fn damage_is_line_precise_rather_than_a_full_repaint() {
        let mut e = engine();
        e.feed(b"filler\r\n");
        let (damage, _) = e.feed(b"\x1b[5;10Hx");
        match damage {
            Damage::Lines(lines) => {
                assert!(
                    lines.iter().any(|d| d.line == 4),
                    "row 5 must be damaged: {lines:?}"
                );
                assert!(
                    lines.len() <= 3,
                    "a single cell write must not damage the screen: {lines:?}"
                );
            }
            Damage::Full => panic!("a single cell write must not report a full repaint"),
        }
    }

    #[test]
    fn a_quiet_feed_reports_no_content_damage() {
        let mut e = engine();
        e.feed(b"something\r\n");
        let (damage, effects) = e.feed(b"");
        assert!(effects.is_empty());
        // The engine folds cursor damage in, so "quiet" means no content lines, not an
        // empty set. The renderer has to know that, so the test states it.
        if let Damage::Lines(lines) = damage {
            assert!(lines.len() <= 2, "quiet feed damaged {lines:?}");
        }
    }

    #[test]
    fn alt_screen_and_bracketed_paste_modes_are_visible() {
        let mut e = engine();
        assert!(!e.modes().alt_screen);
        e.feed(b"\x1b[?1049h");
        assert!(e.modes().alt_screen, "alt screen must be reported");
        e.feed(b"\x1b[?2004h");
        assert!(e.modes().bracketed_paste);
        e.feed(b"\x1b[?1049l");
        assert!(!e.modes().alt_screen);
    }

    #[test]
    fn resize_changes_the_snapshot_dimensions() {
        let mut e = engine();
        e.resize(132, 43);
        let s = e.snapshot();
        assert_eq!((s.cols, s.rows), (132, 43));
    }

    #[test]
    fn the_staging_ring_is_configured_explicitly_and_is_emptied() {
        // The engine's default is 10 000 rows, which is a 46 MiB per terminal
        // configuration. The constructor overrides it, and the adapter empties it after
        // every feed, so what is left behind is nothing rather than a full ring.
        let mut e = engine();
        assert_eq!(SCROLLBACK_ROWS, 1_000);
        for i in 0..2000 {
            e.feed(format!("line {i}\r\n").as_bytes());
        }
        let left = e.term.grid().total_lines() - e.term.grid().screen_lines();
        assert_eq!(
            left, 0,
            "the ring still holds {left} rows, so it is being used as a scrollback"
        );
    }

    #[test]
    fn rows_entering_the_history_ring_are_captured_in_order() {
        let mut e = engine();
        for i in 0..40 {
            e.feed(format!("row {i}\r\n").as_bytes());
        }
        let spilled = e.take_scrolled_off();
        assert!(
            !spilled.is_empty(),
            "rows scrolled off but none were captured"
        );
        assert_eq!(spilled[0], "row 0", "the oldest row must come first");
        // Draining is destructive, so a second call returns nothing new.
        assert!(e.take_scrolled_off().is_empty());
    }

    #[test]
    fn every_scrolled_row_is_handed_over_however_many_there_are() {
        // The ring is drained after every feed, so the number of rows a caller sees is the
        // number that scrolled, not the number the ring happens to hold. Feeding more than
        // the ring capacity used to lose everything past it.
        let mut e = engine();
        let total = SCROLLBACK_ROWS * 4;
        let mut seen = Vec::new();
        for i in 0..total {
            e.feed(format!("row {i}\r\n").as_bytes());
            seen.extend(e.take_scrolled_off());
        }
        // The first rows scroll off only once the screen is full, so the count is the
        // total minus a screen.
        assert!(
            seen.len() >= total - 24,
            "only {} of {total} rows were handed over",
            seen.len()
        );
        assert_eq!(seen[0].trim(), "row 0");
        for (i, row) in seen.iter().enumerate() {
            assert_eq!(row.trim(), format!("row {i}"), "row {i} is out of order");
        }
        assert_eq!(
            e.evicted_unobserved(),
            0,
            "ordinary output should never saturate the staging ring"
        );
    }

    #[test]
    fn a_feed_that_scrolls_more_than_the_ring_holds_is_counted_rather_than_silently_lost() {
        // The engine cannot report what it has already evicted, so the adapter says when it
        // was in that state. The caller's job is to feed in slices small enough that this
        // never happens; the counter is what tells it when that is not true.
        //
        // A scroll control sequence turns out not to be a way in: measured, `CSI 5000 S`
        // moves at most one screen into history, because a scroll larger than the region is
        // clamped. The way in is a single large feed.
        let mut e = engine();
        let burst: String = std::iter::repeat_n("x\r\n", SCROLLBACK_ROWS * 3).collect();
        e.feed(burst.as_bytes());
        let seen = e.take_scrolled_off().len();
        assert_eq!(
            seen, SCROLLBACK_ROWS,
            "the ring held more than its capacity"
        );
        assert!(
            e.evicted_unobserved() > 0,
            "rows were evicted before they could be seen and nothing said so"
        );
    }

    #[test]
    fn unsupported_graphics_leave_adjacent_text_intact() {
        // An unsupported image protocol must never take
        // neighbouring output with it.
        let mut e = engine();
        e.feed(b"before\r\n");
        e.feed(b"\x1b_Gf=100,a=T;iVBORw0KGgoAAAANS\x1b\\");
        e.feed(b"after\r\n");
        let lines = e.snapshot().lines;
        assert!(lines.iter().any(|l| l == "before"), "{lines:?}");
        assert!(lines.iter().any(|l| l == "after"), "{lines:?}");
    }

    /// What a client needs in order to know whose the mouse is, read off the snapshot.
    ///
    /// Asked of the snapshot and not of `modes()`, because the snapshot is the only thing
    /// that crosses to a client: a mode the engine knows and the snapshot does not is a
    /// mode no client can act on.
    #[test]
    fn the_snapshot_carries_the_modes_a_client_acts_on() {
        let mut e = AlacrittyEngine::new(80, 24);
        assert!(!e.snapshot().modes.alt_screen);
        assert!(!e.snapshot().modes.mouse_reporting);
        e.feed(b"\x1b[?1049h");
        assert!(
            e.snapshot().modes.alt_screen,
            "a full screen program is invisible to a client"
        );
        e.feed(b"\x1b[?1000h");
        assert!(
            e.snapshot().modes.mouse_reporting,
            "a program that took the mouse is invisible to a client, so its scrolling and \
             its clicking are taken away from it"
        );
        e.feed(b"\x1b[?1000l\x1b[?1049l");
        assert!(!e.snapshot().modes.alt_screen);
        assert!(!e.snapshot().modes.mouse_reporting);
        assert!(!e.snapshot().modes.focus_events);
        e.feed(b"\x1b[?1004h");
        assert!(
            e.snapshot().modes.focus_events,
            "a program that asked to hear about focus is never told"
        );
    }

    /// A program's `OSC 8` link reaches the snapshot as one run over exactly the linked
    /// text, in the default colours, and the text around it carries no link.
    #[test]
    fn an_osc_8_link_becomes_one_run_over_the_linked_text() {
        let mut e = engine();
        e.feed(b"see \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ here");
        let s = e.snapshot();
        assert_eq!(s.lines[0], "see link here");
        let linked: Vec<&Span> = s.styles[0]
            .iter()
            .filter(|sp| sp.style.link.is_some())
            .collect();
        assert_eq!(linked.len(), 1, "{:?}", s.styles[0]);
        assert_eq!((linked[0].at, linked[0].len), (4, 4));
        assert_eq!(linked[0].style.link.as_deref(), Some("https://example.com"));
        assert!(!linked[0].style.is_plain(), "a linked run is not plain");
        assert!(
            s.styles[0]
                .iter()
                .all(|sp| sp.at + sp.len <= 8 || sp.style.link.is_none()),
            "the link ran past its text: {:?}",
            s.styles[0]
        );
    }

    /// A program asking for the background is answered with the one the person is looking
    /// at once a client has said it, in the terminator it asked with; before that, and for
    /// a colour the program set itself, it gets what it would have got anyway.
    #[test]
    fn a_background_question_is_answered_from_the_host_colours() {
        let replies = |e: &mut AlacrittyEngine, bytes: &[u8]| -> Vec<Bytes> {
            e.feed(bytes)
                .1
                .into_iter()
                .filter_map(|fx| match fx {
                    TerminalEffect::Reply(b) => Some(b),
                    _ => None,
                })
                .collect()
        };
        let mut e = engine();
        assert!(
            replies(&mut e, b"\x1b]11;?\x1b\\").is_empty(),
            "nobody said yet"
        );
        e.set_host_colors(Some(([0, 0, 0], [255, 255, 255])));
        assert_eq!(
            replies(&mut e, b"\x1b]11;?\x1b\\"),
            vec![Bytes::from_static(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\")]
        );
        assert_eq!(
            replies(&mut e, b"\x1b]10;?\x07"),
            vec![Bytes::from_static(b"\x1b]10;rgb:0000/0000/0000\x07")]
        );
        e.feed(b"\x1b]11;rgb:12/34/56\x1b\\");
        assert_eq!(
            replies(&mut e, b"\x1b]11;?\x07"),
            vec![Bytes::from_static(b"\x1b]11;rgb:1212/3434/5656\x07")],
            "the program's own background wins"
        );
        assert!(
            replies(&mut e, b"\x1b]4;1;?\x07").is_empty(),
            "palette questions stay as they were"
        );
    }

    /// A program that links every cell to its own long address gets a snapshot that still
    /// crosses the socket: each distinct target is held once, within the budget, and the
    /// rest paint unlinked. Measured before this, 1920 cells of a 4 KB address each made a
    /// snapshot of more than eight megabytes.
    #[test]
    fn links_past_the_budget_are_dropped_and_each_target_is_held_once() {
        let mut e = AlacrittyEngine::new(80, 24);
        let mut out = Vec::new();
        for i in 0..1920u32 {
            let uri = format!("https://e.example/{}{}", "a".repeat(200), i % 300);
            out.extend_from_slice(format!("\x1b]8;;{uri}\x1b\\x").as_bytes());
        }
        e.feed(&out);
        let s = e.snapshot();
        let mut distinct: Vec<&str> = s
            .styles
            .iter()
            .flatten()
            .filter_map(|span| span.style.link.as_deref())
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(distinct.len() <= MAX_LINKS, "{} targets", distinct.len());
        let bytes: usize = distinct.iter().map(|uri| uri.len()).sum();
        assert!(bytes <= MAX_LINK_BYTES, "{bytes} bytes of targets");
        assert!(!distinct.is_empty(), "the budget left no link at all");
        let mut long = engine();
        let uri = format!("https://e.example/{}", "a".repeat(MAX_LINK_LEN));
        long.feed(format!("\x1b]8;;{uri}\x1b\\x\x1b]8;;\x1b\\").as_bytes());
        assert!(
            long.snapshot().styles[0]
                .iter()
                .all(|span| span.style.link.is_none()),
            "a target no client would write is not carried"
        );
    }
}

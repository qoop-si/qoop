//! Selecting text with the mouse, and putting it on the clipboard.
//!
//! The interface takes the mouse, which is what lets a click reach a tab or a space. The
//! cost of taking it is that the terminal's own selection stops working, and a person who
//! wants to copy a line out of an agent's answer finds that dragging over it does nothing.
//! That is a regression against a bare terminal, so the selection has to be ours.
//!
//! Copy on select, with no key to press. A selection that has to be confirmed is a
//! selection a person makes twice. Nothing is put on the clipboard until the button comes up, so a drag
//! that turns out to be a click writes nothing.
//!
//! Coordinates here are columns and rows of the pane, zero based, because that is what a
//! mouse report gives after the pane's own offset is taken off. A cell of the grid is not a
//! column: a wide character occupies one cell and two columns, and the grid puts a spacer
//! cell after it. The conversion is done once, here, in the one loop that walks a line.

use std::ops::Range;

/// A selection being made or made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Where the button went down.
    anchor: (usize, usize),
    /// Where the pointer is now.
    head: (usize, usize),
}

impl Selection {
    /// A selection that starts and ends where the button went down.
    pub fn start(row: usize, col: usize) -> Self {
        Selection {
            anchor: (row, col),
            head: (row, col),
        }
    }

    /// Follow the pointer.
    pub fn extend(&mut self, row: usize, col: usize) {
        self.head = (row, col);
    }

    /// The two ends in reading order, whichever way the drag went.
    fn ends(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Nothing is selected: the pointer never left the cell it went down in.
    ///
    /// A click is a click, and a click that copied the character under it would put a
    /// stray letter on the clipboard every time somebody focused a pane.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The columns selected on one row of the pane, if any are.
    ///
    /// The end is exclusive, and the last column of the drag is included: a person who
    /// drags across a word expects the letter under the pointer to be in it.
    pub fn columns_on(&self, row: usize, cols: usize) -> Option<Range<usize>> {
        if self.is_empty() {
            return None;
        }
        let ((top, from), (bottom, to)) = self.ends();
        if row < top || row > bottom {
            return None;
        }
        let start = if row == top { from } else { 0 };
        let end = if row == bottom { to + 1 } else { cols };
        (start < cols).then(|| start..end.min(cols))
    }

    /// What is selected, as text.
    ///
    /// Each row is taken to its last non-blank column, because a terminal row is padded to
    /// the width of the screen and a paste full of trailing spaces is not what was on the
    /// screen. Rows are joined with a newline, which is what a person pasting several lines
    /// of an answer means by them.
    pub fn text(&self, lines: &[String], cols: usize) -> String {
        let mut out: Vec<String> = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            let Some(range) = self.columns_on(row, cols) else {
                continue;
            };
            out.push(in_columns(line, range).trim_end().to_string());
        }
        // A selection that covers only blank screen is not text, and writing it would
        // replace whatever the person had on their clipboard with nothing.
        if out.iter().all(|line| line.is_empty()) {
            return String::new();
        }
        out.join("\n")
    }
}

/// The part of a grid row that lies in a range of screen columns.
///
/// The walk is the same one the painter does, and for the same reason: a wide character is
/// one cell, two columns, and is followed by a spacer cell that is neither.
pub fn in_columns(line: &str, range: Range<usize>) -> String {
    let mut out = String::new();
    let mut at = 0;
    let mut skip_spacer = false;
    for c in line.chars() {
        if skip_spacer && c == ' ' {
            skip_spacer = false;
            continue;
        }
        let w = crate::width::columns(c);
        skip_spacer = w == 2;
        if at >= range.end {
            break;
        }
        if at >= range.start {
            out.push(c);
        }
        at += w;
    }
    out
}

/// Put text on the system clipboard.
///
/// Two ways, and which one is right depends on where the person is sitting. Locally the
/// platform's own tool writes the clipboard the rest of the desktop reads. Over ssh there is
/// no such tool on the far side worth writing to: the clipboard that matters belongs to the
/// terminal in front of the person, and OSC 52 is how a program asks it for one. The same
/// is true inside an editor's integrated terminal.
///
/// The escape is BEL terminated. ST is correct and newer emulators take both; some still
/// only honour BEL, and a clipboard write that silently does nothing is worse than an
/// old-fashioned terminator.
///
/// Answers how the text left: the caller reports it honestly, because an OSC 52
/// escape the outer terminal swallows is not a clipboard write, and saying
/// "copied" for one is a false report.
pub fn copy(text: &str, out: &mut impl std::io::Write) -> std::io::Result<Option<CopyVia>> {
    copy_with(text, &Environment::read(), platform_copy, out)
}

/// Which way a copy went, so the notice can say what actually happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyVia {
    /// The platform tool took it: the desktop clipboard has it.
    System,
    /// An OSC 52 escape was written to the terminal in front of the person:
    /// it arrives only if that terminal honours OSC 52.
    TerminalEscape,
}

/// The testable core of [`copy`]: the environment and the platform attempt are
/// parameters, so the routing matrix is a unit test rather than a machine with
/// tmux and ssh installed.
fn copy_with(
    text: &str,
    env: &Environment,
    platform: impl FnOnce(&str) -> bool,
    out: &mut impl std::io::Write,
) -> std::io::Result<Option<CopyVia>> {
    if text.is_empty() {
        return Ok(None);
    }
    // One read, one decision: reading twice lets the environment change between
    // the two reads and route the write by a mix of two worlds.
    if !prefer_escape(env) && platform(text) {
        return Ok(Some(CopyVia::System));
    }
    out.write_all(escape_for(env, text.as_bytes()).as_bytes())?;
    out.flush()?;
    Ok(Some(CopyVia::TerminalEscape))
}

/// The clipboard escape, for a caller that has its own way of writing to the terminal.
pub fn escape(bytes: &[u8]) -> String {
    escape_for(&Environment::default(), bytes)
}

/// The clipboard escape wrapped for the terminal in front of the person.
///
/// Inside tmux or GNU screen a bare OSC 52 never reaches the outer terminal:
/// the multiplexer eats it, and the person sees "copied" while nothing
/// arrives. Wrapped in the
/// DCS passthrough, the multiplexer forwards it instead of swallowing it.
pub fn escape_for(env: &Environment, bytes: &[u8]) -> String {
    let raw = format!("\x1b]52;c;{}\x07", base64(bytes));
    if env.tmux {
        // tmux DCS passthrough: ESC P tmux ; <sequence with ESC doubled> ESC \.
        // The inner sequence's ESCs are doubled so tmux forwards one copy.
        return format!("\x1bPtmux;{}\x1b\\", raw.replace('\x1b', "\x1b\x1b"));
    }
    if env.screen {
        // GNU screen strips the DCS wrapper and forwards the inner bytes as
        // is: no ESC doubling here, which is tmux-specific.
        return format!("\x1bP{}\x1b\\", raw);
    }
    raw
}

/// What decides which way the clipboard is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub ssh: bool,
    pub editor_terminal: bool,
    /// Inside tmux: OSC 52 must be wrapped in the DCS passthrough.
    pub tmux: bool,
    /// Inside GNU screen: same, with screen's own wrapping.
    pub screen: bool,
}

impl Environment {
    fn read() -> Self {
        Environment {
            ssh: std::env::var_os("SSH_CONNECTION").is_some()
                || std::env::var_os("SSH_TTY").is_some(),
            editor_terminal: std::env::var_os("VSCODE_IPC_HOOK_CLI").is_some(),
            tmux: std::env::var_os("TMUX").is_some(),
            screen: std::env::var("TERM")
                .map(|t| t.starts_with("screen"))
                .unwrap_or(false),
        }
    }
}

/// Whether to ask the terminal rather than the machine this is running on.
pub fn prefer_escape(env: &Environment) -> bool {
    env.ssh || env.editor_terminal
}

/// Write the clipboard with the platform's own tool. Answers whether it worked.
fn platform_copy(text: &str) -> bool {
    use std::io::Write as _;
    let tool = if cfg!(target_os = "macos") {
        "pbcopy"
    } else {
        // Wayland first, since a session running it usually has no X clipboard at all.
        for tool in ["wl-copy", "xclip", "xsel"] {
            if let Some(ok) = pipe_into(tool, text) {
                return ok;
            }
        }
        return false;
    };
    let Some(mut child) = std::process::Command::new(tool)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// Try one clipboard tool. `None` means it is not installed, which is not a failure.
fn pipe_into(tool: &str, text: &str) -> Option<bool> {
    use std::io::Write as _;
    let mut command = std::process::Command::new(tool);
    if tool == "xclip" {
        command.args(["-selection", "clipboard"]);
    }
    if tool == "xsel" {
        command.args(["--clipboard", "--input"]);
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    Some(child.wait().map(|s| s.success()).unwrap_or(false))
}

/// Base64, written out rather than taken as a dependency: it is twenty lines, and the
/// alternative is a crate in the tree for one call.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_selects_nothing_at_all() {
        let selection = Selection::start(3, 10);
        assert!(selection.is_empty());
        assert_eq!(selection.columns_on(3, 80), None);
        assert_eq!(selection.text(&["hello".to_string()], 80), "");
    }

    #[test]
    fn a_drag_across_one_line_takes_the_columns_it_covered_including_the_last() {
        let mut selection = Selection::start(0, 2);
        selection.extend(0, 6);
        assert_eq!(selection.columns_on(0, 80), Some(2..7));
        assert_eq!(selection.columns_on(1, 80), None);
        let lines = vec!["abcdefghij".to_string()];
        assert_eq!(selection.text(&lines, 80), "cdefg");
    }

    #[test]
    fn a_drag_backwards_is_the_same_selection_as_a_drag_forwards() {
        let mut forwards = Selection::start(0, 1);
        forwards.extend(2, 3);
        let mut backwards = Selection::start(2, 3);
        backwards.extend(0, 1);
        let lines = vec![
            "aaaaaa".to_string(),
            "bbbbbb".to_string(),
            "cccccc".to_string(),
        ];
        assert_eq!(forwards.text(&lines, 6), backwards.text(&lines, 6));
        assert_eq!(forwards.text(&lines, 6), "aaaaa\nbbbbbb\ncccc");
    }

    #[test]
    fn the_middle_rows_of_a_selection_are_whole_rows() {
        let mut selection = Selection::start(0, 4);
        selection.extend(2, 1);
        assert_eq!(selection.columns_on(0, 10), Some(4..10));
        assert_eq!(selection.columns_on(1, 10), Some(0..10));
        assert_eq!(selection.columns_on(2, 10), Some(0..2));
    }

    #[test]
    fn a_row_is_taken_to_its_last_word_rather_than_to_the_edge_of_the_screen() {
        let mut selection = Selection::start(0, 0);
        selection.extend(1, 79);
        let lines = vec![
            format!("hello{}", " ".repeat(75)),
            format!("world{}", " ".repeat(75)),
        ];
        assert_eq!(
            selection.text(&lines, 80),
            "hello\nworld",
            "a paste of two words carried a hundred and fifty spaces"
        );
    }

    #[test]
    fn a_selection_over_blank_screen_writes_nothing() {
        let mut selection = Selection::start(0, 0);
        selection.extend(1, 10);
        let lines = vec![" ".repeat(80), " ".repeat(80)];
        assert_eq!(
            selection.text(&lines, 80),
            "",
            "an empty selection would replace what the person already had"
        );
    }

    /// A wide character is one cell and two columns, and the grid puts a spacer after it.
    #[test]
    fn a_wide_character_is_two_columns_and_its_spacer_is_not_a_character() {
        // `世` is two columns, and the grid row carries it followed by a spacer cell. The
        // row below is therefore `a`, the wide character, its spacer, a real space, `b`.
        let line = "a\u{4e16}  b".to_string();
        assert_eq!(in_columns(&line, 0..1), "a");
        assert_eq!(in_columns(&line, 1..3), "\u{4e16}");
        assert_eq!(in_columns(&line, 0..3), "a\u{4e16}");
        // The column after the wide character is the space, not its spacer.
        assert_eq!(in_columns(&line, 3..5), " b");
    }

    #[test]
    fn the_clipboard_escape_is_base64_and_ends_in_bel() {
        assert_eq!(escape(b"hello"), "\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(escape(b"hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(escape(b"abc"), "\x1b]52;c;YWJj\x07");
        assert_eq!(escape("türk".as_bytes()), "\x1b]52;c;dMO8cms=\x07");
    }

    #[test]
    fn a_person_over_ssh_gets_their_own_terminals_clipboard() {
        assert!(prefer_escape(&Environment {
            ssh: true,
            editor_terminal: false,
            ..Environment::default()
        }));
        assert!(prefer_escape(&Environment {
            ssh: false,
            editor_terminal: true,
            ..Environment::default()
        }));
        assert!(
            !prefer_escape(&Environment::default()),
            "sitting at the machine, the machine's own clipboard is the one that is read"
        );
    }

    #[test]
    fn copying_nothing_writes_nothing() {
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            copy("", &mut out).expect("no write"),
            None,
            "empty text is not a copy at all, so the caller shows no notice"
        );
        assert!(out.is_empty());
    }

    /// The routing matrix: where the person sits decides which clipboard hears.
    #[test]
    fn the_routing_matrix_sends_remote_copies_to_the_terminal() {
        let local = Environment::default();
        let ssh = Environment {
            ssh: true,
            ..Environment::default()
        };
        let editor = Environment {
            editor_terminal: true,
            ..Environment::default()
        };
        // Locally the platform tool wins and nothing is written to the terminal.
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            copy_with("hi", &local, |_| true, &mut out).expect("copy"),
            Some(CopyVia::System)
        );
        assert!(out.is_empty());
        // Over ssh the platform tool is not even tried: the far side's
        // clipboard is not the one in front of the person.
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            copy_with(
                "hi",
                &ssh,
                |_| panic!("far-side tool must not run"),
                &mut out
            )
            .expect("copy"),
            Some(CopyVia::TerminalEscape)
        );
        assert_eq!(out, escape_for(&ssh, b"hi").as_bytes());
        // Same inside an editor terminal.
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            copy_with(
                "hi",
                &editor,
                |_| panic!("host tool must not run"),
                &mut out
            )
            .expect("copy"),
            Some(CopyVia::TerminalEscape)
        );
        // No platform tool installed locally: fall through to the escape
        // rather than reporting success while writing nothing.
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            copy_with("hi", &local, |_| false, &mut out).expect("copy"),
            Some(CopyVia::TerminalEscape)
        );
        assert_eq!(out, escape(b"hi").as_bytes());
    }

    /// Inside tmux or screen a bare OSC 52 never reaches the outer terminal.
    /// The DCS wrap is what forwards it; without it the person sees a notice
    /// while nothing arrives.
    #[test]
    fn the_multiplexer_matrix_wraps_the_escape() {
        let bare = escape(b"hi");
        assert_eq!(bare, "\x1b]52;c;aGk=\x07");
        let tmux = Environment {
            tmux: true,
            ..Environment::default()
        };
        let wrapped = escape_for(&tmux, b"hi");
        assert!(
            wrapped.starts_with("\x1bPtmux;"),
            "tmux needs its DCS passthrough prefix: {wrapped:?}"
        );
        assert!(
            wrapped.ends_with("\x1b\\"),
            "an unterminated DCS swallows the rest of the session: {wrapped:?}"
        );
        assert!(
            wrapped.contains(&bare.replace('\x1b', "\x1b\x1b")),
            "the inner ESCs are doubled so tmux forwards one copy: {wrapped:?}"
        );
        let screen = Environment {
            screen: true,
            ..Environment::default()
        };
        let swrapped = escape_for(&screen, b"hi");
        assert!(
            swrapped.starts_with("\x1bP") && swrapped.ends_with("\x1b\\"),
            "screen needs its own DCS wrap: {swrapped:?}"
        );
        // Both set: tmux is the inner multiplexer a person nests, so it wins.
        let both = Environment {
            tmux: true,
            screen: true,
            ..Environment::default()
        };
        assert_eq!(escape_for(&both, b"hi"), wrapped);
        // tmux over ssh still wraps: the wrap and the route are orthogonal.
        let tmux_ssh = Environment {
            tmux: true,
            ssh: true,
            ..Environment::default()
        };
        assert_eq!(escape_for(&tmux_ssh, b"hi"), wrapped);
    }

    /// Clipboard bytes are bytes: CJK and emoji survive base64 and the wrap.
    #[test]
    fn cjk_and_emoji_survive_the_clipboard_bytes() {
        for text in [
            "\u{4e16}\u{754c}",
            "\u{65e5}\u{672c}\u{8a9e}",
            "\u{1f600}",
            "t\u{00fc}rk",
        ] {
            let esc = escape(text.as_bytes());
            let payload = esc
                .strip_prefix("\x1b]52;c;")
                .and_then(|s| s.strip_suffix('\x07'))
                .expect("BEL-terminated OSC 52");
            assert_eq!(
                decode_base64(payload),
                text.as_bytes(),
                "clipboard mojibake for {text:?}"
            );
        }
    }

    /// Minimal base64 decoder, test-only: the encoder is hand-rolled, so the
    /// test must not trust it to check itself.
    #[cfg(test)]
    fn decode_base64(s: &str) -> Vec<u8> {
        fn val(c: u8) -> u32 {
            match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("bad base64 char {c}"),
            }
        }
        let b = s.as_bytes();
        let mut out = Vec::new();
        for c in b.chunks(4) {
            let n = (val(c[0]) << 18)
                | (val(c[1]) << 12)
                | (if c[2] == b'=' { 0 } else { val(c[2]) << 6 })
                | (if c[3] == b'=' { 0 } else { val(c[3]) });
            out.push((n >> 16) as u8);
            if c[2] != b'=' {
                out.push((n >> 8) as u8);
            }
            if c[3] != b'=' {
                out.push(n as u8);
            }
        }
        out
    }
}

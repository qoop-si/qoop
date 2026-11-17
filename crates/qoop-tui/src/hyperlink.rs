//! Links a program wrote with `OSC 8`, written back out to the terminal above.
//!
//! The target comes from whatever runs in a pane, and it goes back out inside an escape
//! sequence. A byte that ends the sequence early (`BEL`, `ESC`) would hand the rest of the
//! target to the outer terminal as sequences of its own, so the target is checked here, the
//! one place it is written, and a target that is not plain printable ASCII is not written at
//! all: the text still shows, it just stops being a link.

/// The longest target written. Longer than any real address, and short enough that a
/// program cannot make every row of every frame carry a page of it.
const MAX_URI: usize = 2048;

/// The escape that ends a link, and the only form this interface writes.
pub const CLOSE: &str = "\x1b]8;;\x1b\\";

/// The target, when it is safe to write: visible ASCII (`0x21..=0x7e`) and at most
/// [`MAX_URI`] bytes. Anything else paints the run unlinked.
pub fn clean_uri(uri: &str) -> Option<&str> {
    let fits =
        !uri.is_empty() && uri.len() <= MAX_URI && uri.bytes().all(|b| (0x21..=0x7e).contains(&b));
    fits.then_some(uri)
}

/// Open `uri` in the person's browser: `open` on macOS, `xdg-open` elsewhere, started
/// detached with no shell and no stdio, so the interface neither waits for it nor shares its
/// terminal. A target that is not [`clean_uri`] is not opened. Failing to start the opener
/// is silent: a click that did nothing is the whole report.
pub fn open_in_browser(uri: &str) {
    let Some(uri) = clean_uri(uri) else { return };
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let spawned = std::process::Command::new(program)
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    // Reaped on a thread of its own, so a finished opener is never left a zombie.
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// The escape that opens a link to `uri`, which must have come through [`clean_uri`].
pub fn open(uri: &str) -> String {
    format!("\x1b]8;;{uri}\x1b\\")
}

/// The link at cell `col` of a pane row, as herdr finds one for a ctrl+click: the target a
/// program gave the cell with `OSC 8`, whatever its scheme; else, in the row's text, an
/// `http://` or `https://` address the cell is in. `line` holds one char per cell (a wide
/// character's spacer included), as the engine sends rows.
pub fn link_at(
    line: &str,
    spans: &[qoop_runtime::daemon::SpanWire],
    links: &[String],
    col: usize,
) -> Option<String> {
    let osc8 = spans
        .iter()
        .find(|s| col >= s.at && col < s.at + s.len)
        .and_then(|s| s.link)
        .and_then(|at| links.get(at as usize));
    if let Some(uri) = osc8 {
        return Some(uri.clone());
    }
    let cells: Vec<char> = line.chars().collect();
    web_span(&cells, col).map(|(start, end)| cells[start..=end].iter().collect())
}

/// herdr's address span: from an `http://` or `https://` to the next whitespace, its trailing
/// punctuation and unbalanced closing brackets trimmed, when `col` falls in what is left.
fn web_span(cells: &[char], col: usize) -> Option<(usize, usize)> {
    let starts = |at: usize, prefix: &str| {
        prefix
            .chars()
            .enumerate()
            .all(|(i, c)| cells.get(at + i) == Some(&c))
    };
    let mut start = 0;
    while start < cells.len() {
        if !(starts(start, "http://") || starts(start, "https://")) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end + 1 < cells.len() && !cells[end + 1].is_whitespace() {
            end += 1;
        }
        if (start..=end).contains(&col) {
            while end >= start && trims(&cells[start..=end]) {
                end = end.checked_sub(1)?;
            }
            return (start <= end && col <= end).then_some((start, end));
        }
        start = end + 1;
    }
    None
}

/// Whether the last cell of an address is punctuation around it rather than part of it: a
/// quote, a full stop and its kind, or a closing bracket the address did not open.
fn trims(url: &[char]) -> bool {
    let (last, before) = url.split_last().expect("not empty");
    let opened = |open: char, close: char| {
        before.iter().filter(|c| **c == open).count()
            > before.iter().filter(|c| **c == close).count()
    };
    match last {
        '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?' => true,
        ')' => !opened('(', ')'),
        ']' => !opened('[', ']'),
        '}' => !opened('{', '}'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ctrl_click_finds_the_cells_link_or_a_web_address_as_herdr_does() {
        let at = |line: &str, col| link_at(line, &[], &[], col);
        let line = "see (https://example.com/a_(b)). or http://x.io, ok";
        assert_eq!(at(line, 10).as_deref(), Some("https://example.com/a_(b)"));
        assert_eq!(
            at(line, 4),
            None,
            "the bracket before it is not the address"
        );
        assert_eq!(at(line, 30), None, "the trimmed full stop is not either");
        assert_eq!(at(line, 40).as_deref(), Some("http://x.io"));
        assert_eq!(
            at("ftp://x.io and a.rs", 2),
            None,
            "only web addresses in text"
        );
        let span = qoop_runtime::daemon::SpanWire {
            at: 0,
            len: 4,
            fg: None,
            bg: None,
            attrs: 0,
            link: Some(0),
        };
        assert_eq!(
            link_at(
                "note https://x.io",
                &[span],
                &["file:///tmp/note.md".into()],
                2
            )
            .as_deref(),
            Some("file:///tmp/note.md"),
            "a program's own link wins, any scheme"
        );
    }

    #[test]
    fn an_ordinary_address_is_kept_as_it_is() {
        assert_eq!(
            clean_uri("https://example.com/a?b=c#d"),
            Some("https://example.com/a?b=c#d")
        );
        assert_eq!(clean_uri("file:///tmp/x.rs"), Some("file:///tmp/x.rs"));
    }

    #[test]
    fn a_hostile_or_oversized_target_is_not_written() {
        for bad in [
            "",
            "https://x\x07\x1b]0;pwned",
            "https://x\x1b\\",
            "https://x y",
            "https://x\u{9c}",
            "https://\u{e9}.com",
            "https://x\x7f",
        ] {
            assert_eq!(clean_uri(bad), None, "{bad:?}");
        }
        let long = format!("https://x/{}", "a".repeat(MAX_URI));
        assert_eq!(clean_uri(&long), None);
        let edge = "a".repeat(MAX_URI);
        assert_eq!(clean_uri(&edge), Some(edge.as_str()));
    }
}

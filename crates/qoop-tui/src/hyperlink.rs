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

#[cfg(test)]
mod tests {
    use super::*;

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

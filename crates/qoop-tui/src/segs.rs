//! The pieces a painted line is made of: a run of words in one ink, painted into exactly the
//! columns a line has. Every place that draws a row of the pane (the Inbox and the places
//! beside it) builds its rows from these.

use crate::theme::{self, Role};
use crate::width;

/// A piece of a line: its words and the ink they wear.
#[derive(Clone, Debug)]
pub(crate) struct Seg {
    pub(crate) role: Role,
    pub(crate) bold: bool,
    pub(crate) text: String,
}

pub(crate) fn seg(role: Role, text: impl Into<String>) -> Seg {
    Seg {
        role,
        bold: false,
        text: text.into(),
    }
}

pub(crate) fn bold(role: Role, text: impl Into<String>) -> Seg {
    Seg {
        role,
        bold: true,
        text: text.into(),
    }
}

/// `text` cut to at most `cols` columns, on a character.
pub(crate) fn cut(text: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| width::drawn(*c)) {
        let w = width::columns(c);
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The ink of `role`, faded to one grey while a box is open over the screen.
pub(crate) fn ink(role: Role, fade: bool) -> String {
    theme::fg(if fade { Role::Unobserved } else { role })
}

/// `segs` painted into exactly `cols` columns. Every row is built whole and wrapped to its
/// columns by its caller, so nothing reaches the edge. A row that still does is held at the edge
/// with no mark of its own, and in a debug build (every test) it panics, so a row built too wide
/// fails the tests at the sizes the interface is checked at instead of being cut silently.
pub(crate) fn line(segs: &[Seg], cols: usize, fade: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    for s in segs {
        if used >= cols {
            break;
        }
        let all = cut(&s.text, usize::MAX);
        debug_assert!(
            width::width(&all) <= cols - used || all.trim().is_empty(),
            "segs::line cut {all:?} at {cols} columns"
        );
        let text = cut(&all, cols - used);
        used += width::width(&text);
        out.push_str(&ink(s.role, fade));
        if s.bold && !fade {
            out.push_str("\x1b[1m");
        }
        out.push_str(&text);
    }
    out.push_str(theme::RESET);
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_that_fits_is_painted_whole_and_padded() {
        let painted = line(&[seg(Role::Text, "ab"), seg(Role::Text, "cd")], 6, false);
        assert!(
            painted.contains("ab") && painted.contains("cd"),
            "{painted:?}"
        );
    }

    /// A row built wider than its columns is a bug in the row's own wrap: it panics in a debug
    /// build, where every test runs, rather than being cut without a mark.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "segs::line cut")]
    fn a_row_wider_than_its_columns_fails_the_tests() {
        line(&[seg(Role::Text, "abcdefgh")], 4, false);
    }
}

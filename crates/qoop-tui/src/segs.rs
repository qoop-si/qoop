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

/// `segs` painted into exactly `cols` columns. Words cut at the edge end the line, in an
/// ellipsis.
pub(crate) fn line(segs: &[Seg], cols: usize, fade: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    for s in segs {
        if used >= cols {
            break;
        }
        let all = cut(&s.text, usize::MAX);
        let cut_off = width::width(&all) > cols - used;
        let text = match (cut_off, all.trim().is_empty()) {
            (false, _) => all,
            (true, true) => cut(&all, cols - used),
            (true, false) => crate::asks::cut_dots(&all, cols - used),
        };
        used += width::width(&text);
        out.push_str(&ink(s.role, fade));
        if s.bold && !fade {
            out.push_str("\x1b[1m");
        }
        out.push_str(&text);
        // Nothing after a cut: the ellipsis is the line's end.
        if cut_off {
            break;
        }
    }
    out.push_str(theme::RESET);
    out.push_str(&" ".repeat(cols.saturating_sub(used)));
    out
}

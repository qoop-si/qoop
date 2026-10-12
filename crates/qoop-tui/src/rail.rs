//! The rail: qass's folded panes share ONE column at the left of the pane area instead of a thin
//! strip each. A tab row per folded pane (board, questions, qass, in screen order), a rule, then
//! what waits for the owner, and a cap rule on the last row.
//!
//! A tab's text follows a fixed ladder by the width it gets, and never wraps or cuts a word:
//! word and state, word and count, word, code and count, code, dot and count, dot. Quiet tabs are
//! never ember; a questions tab with a waiting question is the one ember block on the rail.
//!
//! Pure functions of what the caller read: the same words the header's chip says, so the two
//! always agree. The left sidebar is not this module's: it paints only at the pane's own columns.

use crate::theme::{self, Role};
use crate::width;

/// The columns after the tabs: the edge (the line that drags the width) and one blank column, which is
/// where the focused pane's corner stands (the rail paints the edge and steps over the blank).
pub const EDGE: usize = 2;
/// The narrowest and widest the tabs are dragged to.
pub const MIN: usize = 3;
pub const MAX: usize = 28;
/// The tabs' width from which the preview shows words, not dots.
const PREVIEW_MIN: usize = 12;

/// The panes that fold, in the order the screen shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pane {
    Board,
    Questions,
    Qass,
}

impl Pane {
    fn word(self) -> &'static str {
        match self {
            Pane::Board => "board",
            Pane::Questions => "questions",
            Pane::Qass => "qass",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Pane::Board => "brd",
            Pane::Questions => "qst",
            Pane::Qass => "qss",
        }
    }
}

/// One folded pane: its count and the one line of state it can say, when it has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub pane: Pane,
    pub count: Option<usize>,
    pub state: Option<String>,
    /// The colour of the state dot.
    pub dot: Role,
}

impl Tab {
    /// A questions tab with a question waiting: the ember one.
    pub fn lit(&self) -> bool {
        self.pane == Pane::Questions && self.count.is_some_and(|n| n > 0)
    }
}

/// What waits, under the tabs: the first question, as the header's chip counts them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub count: usize,
    pub title: String,
    pub agent: String,
}

/// The tabs' width when the owner has not dragged it: by the pane's columns.
pub fn default_width(pane_cols: usize) -> usize {
    match pane_cols {
        150.. => 14,
        75.. => 12,
        55.. => 8,
        _ => MIN,
    }
}

/// `width` kept between the narrowest and the widest, and no wider than `room` columns allow.
pub fn clamp(width: usize, room: usize) -> usize {
    width.clamp(MIN, MAX).min(room.max(MIN))
}

/// A count as the chip writes it: nothing past 99.
fn count_text(n: usize) -> String {
    if n > 99 {
        "99+".to_string()
    } else {
        n.to_string()
    }
}

/// One piece of a tab's text: the words, their colour, whether they are bold.
type Piece = (String, Role, bool);

/// The text of the longest form of `tab` that fits `w` cells: its pieces as (text, role, bold),
/// and the level (3 word and state, 2 word, 1 code, 0 dot).
pub fn ladder(tab: &Tab, w: usize) -> (Vec<Piece>, usize) {
    let quiet = if tab.pane == Pane::Questions {
        Role::Faint
    } else {
        Role::Detail
    };
    let count = tab.count.map(count_text);
    let seg = |t: &str, role: Role, bold: bool| (t.to_string(), role, bold);
    let caret = seg("\u{25b8} ", Role::Detail, false);
    let mut forms: Vec<(usize, Vec<Piece>)> = Vec::new();
    if let Some(state) = &tab.state {
        forms.push((
            3,
            vec![
                caret.clone(),
                seg(tab.pane.word(), Role::Text, true),
                seg("  ", Role::Text, false),
                seg(state, Role::Detail, false),
            ],
        ));
    }
    if let Some(n) = &count {
        forms.push((
            2,
            vec![
                caret.clone(),
                seg(tab.pane.word(), Role::Text, true),
                seg(" ", Role::Text, false),
                seg(n, quiet, true),
            ],
        ));
    }
    forms.push((
        2,
        vec![caret.clone(), seg(tab.pane.word(), Role::Text, true)],
    ));
    if let Some(n) = &count {
        forms.push((
            1,
            vec![
                caret.clone(),
                seg(tab.pane.code(), Role::Text, true),
                seg(" ", Role::Text, false),
                seg(n, quiet, true),
            ],
        ));
    }
    forms.push((1, vec![caret, seg(tab.pane.code(), Role::Text, true)]));
    if let Some(n) = &count {
        forms.push((
            0,
            vec![seg("\u{25cf}", tab.dot, false), seg(n, quiet, true)],
        ));
        // A count that does not fit three cells is only a plus.
        forms.push((
            0,
            vec![seg("\u{25cf}", tab.dot, false), seg("+", quiet, true)],
        ));
    }
    forms.push((0, vec![seg("\u{25cf}", tab.dot, false)]));
    for (level, segs) in forms {
        if segs.iter().map(|(t, _, _)| width::width(t)).sum::<usize>() <= w {
            return (segs, level);
        }
    }
    (Vec::new(), 0)
}

/// `tab` painted in exactly `w` cells: the ladder's form, the ember block when a question waits.
pub fn tab_row(tab: &Tab, w: usize, fade: bool) -> String {
    let (segs, _) = ladder(tab, w);
    let used: usize = segs.iter().map(|(t, _, _)| width::width(t)).sum();
    let pad = " ".repeat(w.saturating_sub(used));
    if tab.lit() && !fade {
        let text: String = segs.iter().map(|(t, _, _)| t.as_str()).collect();
        return format!("{}{text}{pad}{}", theme::button(true), theme::RESET);
    }
    let mut out = String::new();
    for (text, role, bold) in &segs {
        out.push_str(&crate::qass_screen::ink(*role, fade));
        if *bold && !fade {
            out.push_str("\x1b[1m");
        }
        out.push_str(text);
    }
    out.push_str(theme::RESET);
    out.push_str(&pad);
    out
}

/// `text` on lines of at most `w` cells, whole words only. A single token longer than a line
/// is the one thing cut, with an ellipsis.
pub fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if width::width(word) > w {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let cut = width::fit(word, w.saturating_sub(1));
            lines.push(format!("{}\u{2026}", cut.trim_end()));
        } else if cur.is_empty() {
            cur = word.to_string();
        } else if width::width(&cur) + 1 + width::width(word) <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The rail's rows, `rows` tall, each `w` cells and the edge line, then the cursor stepped one cell
/// over: the tabs, a rule, the preview, the edge on every row and a cap rule on the last.
pub fn rows(
    rows: usize,
    w: usize,
    tabs: &[Tab],
    preview: Option<&Preview>,
    fade: bool,
) -> Vec<String> {
    let rule = |text: String| {
        format!(
            "{}{text}{}",
            crate::qass_screen::ink(Role::Rule, fade),
            theme::RESET
        )
    };
    let edge = format!(
        "{}\u{2502}{}\x1b[C",
        crate::qass_screen::ink(Role::Rule, fade),
        theme::RESET
    );
    let plain = |text: String, role: Role, bold: bool| {
        let text = width::fit(&text, w);
        let pad = " ".repeat(w.saturating_sub(width::width(&text)));
        let b = if bold && !fade { "\x1b[1m" } else { "" };
        format!(
            "{}{b}{text}{}{pad}",
            crate::qass_screen::ink(role, fade),
            theme::RESET
        )
    };
    let mut body: Vec<String> = tabs.iter().map(|t| tab_row(t, w, fade)).collect();
    body.push(rule("\u{2500}".repeat(w)));
    if let Some(p) = preview {
        body.push(" ".repeat(w));
        if w >= PREVIEW_MIN {
            body.push(plain(
                format!("{} waiting", count_text(p.count)),
                Role::Blocked,
                true,
            ));
            for line in wrap(&p.title, w).into_iter().take(3) {
                body.push(plain(line, Role::Text, false));
            }
            if let Some(line) = wrap(&p.agent, w).into_iter().next() {
                body.push(plain(line, Role::Faint, false));
            }
            if p.count > 1 {
                body.push(plain(format!("+{} more", p.count - 1), Role::Faint, false));
            }
            if w >= 14 {
                body.push(" ".repeat(w));
                body.push(plain("ctrl-a i opens".to_string(), Role::Faint, false));
            }
        } else {
            let dots = vec!["\u{25cf}"; p.count.min(w.div_ceil(2))].join(" ");
            body.push(plain(dots, Role::Blocked, true));
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(rows);
    for r in 0..rows {
        let cell = if r + 1 == rows {
            rule("\u{2500}".repeat(w))
        } else {
            body.get(r).cloned().unwrap_or_else(|| " ".repeat(w))
        };
        out.push(format!("{cell}{edge}"));
    }
    out
}

/// Which tab a rail row is on, counting from the rail's first row.
pub fn tab_at(tabs: usize, row: usize) -> Option<usize> {
    (row < tabs).then_some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(pane: Pane, count: Option<usize>, state: Option<&str>) -> Tab {
        Tab {
            pane,
            count,
            state: state.map(str::to_string),
            dot: Role::Idle,
        }
    }

    fn text(tab: &Tab, w: usize) -> String {
        ladder(tab, w)
            .0
            .iter()
            .map(|(t, _, _)| t.as_str())
            .collect()
    }

    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                for c in it.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// The ladder of every pane at the widths the design shows: the longest form that fits, never
    /// wider than the width, never a cut word.
    #[test]
    fn the_ladder_steps_down_by_width_and_never_cuts_a_word() {
        let q = tab(Pane::Questions, Some(2), Some("2 asks"));
        let b = tab(Pane::Board, None, None);
        let s = tab(Pane::Qass, None, Some("idle"));
        let at = |t: &Tab| [3, 5, 8, 12, 20, 28].map(|w| text(t, w));
        assert_eq!(
            at(&q),
            [
                "\u{25cf}2",
                "\u{25b8} qst",
                "\u{25b8} qst 2",
                "\u{25b8} questions",
                "\u{25b8} questions  2 asks",
                "\u{25b8} questions  2 asks"
            ]
        );
        assert_eq!(
            at(&b),
            [
                "\u{25cf}",
                "\u{25b8} brd",
                "\u{25b8} board",
                "\u{25b8} board",
                "\u{25b8} board",
                "\u{25b8} board"
            ]
        );
        assert_eq!(
            at(&s),
            [
                "\u{25cf}",
                "\u{25b8} qss",
                "\u{25b8} qass",
                "\u{25b8} qass  idle",
                "\u{25b8} qass  idle",
                "\u{25b8} qass  idle"
            ]
        );
        for w in 3..=28 {
            for t in [&q, &b, &s] {
                assert!(width::width(&text(t, w)) <= w, "{w}: {}", text(t, w));
            }
        }
    }

    #[test]
    fn a_count_past_99_is_capped_and_at_three_cells_it_is_a_plus() {
        let q = tab(Pane::Questions, Some(120), Some("120 asks"));
        assert_eq!(text(&q, 3), "\u{25cf}+");
        assert_eq!(text(&q, 4), "\u{25cf}99+");
        assert_eq!(text(&q, 20), "\u{25b8} questions 99+");
    }

    /// Ember is for a waiting question and nothing else.
    #[test]
    fn only_a_waiting_questions_tab_is_ember() {
        let ember = theme::button(true);
        let quiet = [
            tab(Pane::Questions, Some(0), Some("none")),
            tab(Pane::Board, None, None),
            tab(Pane::Qass, None, Some("idle")),
        ];
        for t in &quiet {
            for w in [3, 8, 20] {
                let row = tab_row(t, w, false);
                assert!(
                    !row.contains(&ember) && !row.contains("38;5;202") && !row.contains("48;5;202"),
                    "{row:?}"
                );
                assert_eq!(width::width(&plain(&row)), w);
            }
        }
        let lit = tab(Pane::Questions, Some(1), Some("1 asks"));
        let row = tab_row(&lit, 12, false);
        assert!(row.starts_with(&ember));
        assert_eq!(width::width(&plain(&row)), 12);
    }

    #[test]
    fn wrapping_keeps_whole_words_and_cuts_only_one_long_token() {
        assert_eq!(
            wrap("Ship the frame polish build now?", 12),
            ["Ship the", "frame polish", "build now?"]
        );
        let long = wrap("a-very-long-agent-name-that-never-ends", 10);
        assert_eq!(long.len(), 1);
        assert!(long[0].ends_with('\u{2026}') && width::width(&long[0]) <= 10);
    }

    /// Every row is as wide as the rail, the cap is on the last row, the preview says what waits.
    #[test]
    fn the_rail_rows_are_exact_and_the_preview_names_the_first_question() {
        let tabs = [tab(Pane::Questions, Some(3), Some("3 asks"))];
        let preview = Preview {
            count: 3,
            title: "Ship the frame polish build now?".to_string(),
            agent: "frame-polish".to_string(),
        };
        let out = rows(20, 14, &tabs, Some(&preview), false);
        assert_eq!(out.len(), 20);
        for row in &out {
            assert_eq!(width::width(&plain(row)), 14 + 1, "{row:?}");
        }
        let plain_rows: Vec<String> = out.iter().map(|r| plain(r)).collect();
        assert!(plain_rows[0].starts_with("\u{25b8} questions 3"));
        assert!(plain_rows[1].starts_with("\u{2500}"));
        assert!(plain_rows.iter().any(|r| r.starts_with("3 waiting")));
        assert!(plain_rows.iter().any(|r| r.starts_with("+2 more")));
        assert!(plain_rows[19].starts_with("\u{2500}"));
        // At 12 and 13 cells nothing runs past the edge: the hint waits for 14.
        for w in [12, 13] {
            for row in rows(20, w, &tabs, Some(&preview), false) {
                assert_eq!(width::width(&plain(&row)), w + 1, "{w}: {row:?}");
            }
        }
        // Under twelve cells the preview is dots.
        let narrow = rows(10, 8, &tabs, Some(&preview), false);
        assert!(plain(&narrow[3]).starts_with("\u{25cf} \u{25cf}"));
    }

    #[test]
    fn the_width_is_chosen_by_columns_and_kept_between_the_limits() {
        assert_eq!([161, 81, 61, 40].map(default_width), [14, 12, 8, 3]);
        assert_eq!(clamp(40, 100), MAX);
        assert_eq!(clamp(1, 100), MIN);
        assert_eq!(clamp(20, 9), 9);
        assert_eq!(tab_at(2, 1), Some(1));
        assert_eq!(tab_at(2, 2), None);
    }
}

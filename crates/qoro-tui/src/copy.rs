//! Copy mode: the keyboard's way through a terminal's whole history.
//!
//! The mouse selection copies what is on the screen, and the wheel reads back a few rows at
//! a time. Neither reaches the error an agent printed forty thousand rows ago, and neither
//! can select more than a screen. Copy mode does both, the way tmux's does: a cursor that
//! moves through every row the terminal ever wrote, a selection that grows with it, and a
//! search that runs over the history on disk.
//!
//! Rows are counted the way the daemon counts them: history rows `0..total` from the oldest
//! written, and the live screen's rows straight after them. An absolute number, because the
//! history grows while the mode is open and a row number counted from the bottom would move
//! the cursor off the text it is on every time the terminal printed.
//!
//! Columns are cells: one per character of a stored row, which puts a spacer cell after a
//! wide character, exactly as the grid does. The screen column a cell lands in is worked
//! out with the width module when something is drawn, and nowhere else.
//!
//! Pure: row text comes in through a closure, and the caller does the talking.

use std::collections::BTreeMap;

use qoro_runtime::control::ControlRequest;

use crate::width;

/// What is selected, if anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    None,
    /// From one cell to another, in reading order.
    Char,
    /// Whole rows.
    Line,
}

/// The rows that exist and how many of them fit in the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The first row the cursor may go to: the oldest history row, or on the alternate
    /// screen the top of the screen, since a full screen program's history is not its own.
    pub first: u64,
    /// The last row of the live screen.
    pub last: u64,
    /// How many rows the pane shows.
    pub page: usize,
}

/// A search over the history, asked of the daemon one bounded step at a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    /// The request waiting for an answer, or the last one answered.
    pub id: String,
    pub query: String,
    pub backward: bool,
    /// Where the first step started, so the status can say how far the steps have got.
    pub started: u64,
    /// Where the next step starts, when the last one ran out of budget.
    pub resume: Option<u64>,
    /// An answer is still owed for `id`.
    pub waiting: bool,
}

/// The open copy mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    /// The terminal it was opened over. It closes when the keyboard leaves.
    pub terminal: u32,
    /// The cursor, as (row, cell).
    pub cursor: (u64, usize),
    pub mode: Mode,
    /// The other end of the selection.
    pub anchor: (u64, usize),
    /// The row at the top of the pane. Pinned while the mode is open, so text a person is
    /// reading stays still while the terminal prints.
    pub top: u64,
    /// Where the view was when the mode opened, to go back to when it closes.
    pub restore: Option<u64>,
    /// The query line being typed, and whether it searches backward (`?` rather than `/`).
    pub query: Option<(String, bool)>,
    /// The last search asked for, which `n` and `N` repeat.
    pub last: Option<(String, bool)>,
    pub search: Option<Search>,
    /// What the notice row says while the mode is open.
    pub status: String,
}

/// What the notice row says when there is nothing more particular to say. It names the
/// keys, because in a mode the others do not work.
pub const KEYS: &str =
    "copy: hjkl move  w b e words  / ? search  n N again  v V select  y copy  q quit";

impl Copy {
    /// Open at `cursor`, with the view's top row at `top`.
    pub fn open(terminal: u32, cursor: (u64, usize), top: u64, restore: Option<u64>) -> Self {
        Copy {
            terminal,
            cursor,
            mode: Mode::None,
            anchor: cursor,
            top,
            restore,
            query: None,
            last: None,
            search: None,
            status: KEYS.to_string(),
        }
    }

    /// Keep the cursor inside the rows that exist and the view on the cursor.
    pub fn follow(&mut self, b: Bounds) {
        self.cursor.0 = self.cursor.0.clamp(b.first, b.last);
        let page = b.page.max(1) as u64;
        if self.cursor.0 < self.top {
            self.top = self.cursor.0;
        } else if self.cursor.0 >= self.top + page {
            self.top = self.cursor.0 + 1 - page;
        }
        self.top = self.top.clamp(b.first, max_top(b));
    }

    /// Move up (negative) or down by rows.
    pub fn rows(&mut self, by: i64, b: Bounds) {
        self.cursor.0 = self
            .cursor
            .0
            .saturating_add_signed(by)
            .clamp(b.first, b.last);
        self.follow(b);
    }

    /// Scroll the view, as the wheel does, and bring the cursor along only as far as it
    /// has to go to stay in it.
    pub fn scroll(&mut self, by: i64, b: Bounds) {
        self.top = self
            .top
            .saturating_add_signed(by)
            .clamp(b.first, max_top(b));
        let bottom = self.top + b.page.max(1) as u64 - 1;
        self.cursor.0 = self.cursor.0.clamp(self.top, bottom.min(b.last));
    }

    /// One cell left or right, stepping over the spacer after a wide character.
    pub fn sideways(&mut self, right: bool, row: &str) {
        let cells: Vec<char> = row.chars().collect();
        let mut at = self.cursor.1;
        loop {
            if right {
                if at + 1 >= cells.len() {
                    break;
                }
                at += 1;
            } else {
                let Some(back) = at.checked_sub(1) else {
                    break;
                };
                at = back;
            }
            if !is_spacer(&cells, at) {
                self.cursor.1 = at;
                return;
            }
        }
    }

    /// `0`, `^` and `$`: the start of the row, its first character, its last one.
    pub fn line_edge(&mut self, key: u8, row: &str) {
        let cells: Vec<char> = row.chars().collect();
        self.cursor.1 = match key {
            b'^' => cells.iter().position(|c| !c.is_whitespace()).unwrap_or(0),
            b'$' => cells.iter().rposition(|c| !c.is_whitespace()).unwrap_or(0),
            _ => 0,
        };
    }

    /// `w`, `b` and `e`, across rows when the row runs out.
    pub fn word(&mut self, key: u8, b: Bounds, text: &dyn Fn(u64) -> String) {
        let forward = key != b'b';
        let mut row = self.cursor.0;
        let mut cells: Vec<char> = text(row).chars().collect();
        let mut at = self.cursor.1 as isize;
        // One cell at a time through the rows, remembering the class of the last one, until
        // the motion's edge is found. Row ends count as blank, so a word never spans two.
        let class_at = |cells: &[char], at: isize| -> u8 {
            if at < 0 || at as usize >= cells.len() {
                0
            } else {
                class(cells[at as usize])
            }
        };
        let mut steps = 0usize;
        loop {
            steps += 1;
            // A long run of blank history is not worth freezing the interface over.
            if steps > 100_000 {
                return;
            }
            let here = class_at(&cells, at);
            let next = if forward { at + 1 } else { at - 1 };
            let beyond = next < 0 || next as usize >= cells.len().max(1);
            if beyond {
                let moved = if forward { row < b.last } else { row > b.first };
                if !moved {
                    return;
                }
                row = if forward { row + 1 } else { row - 1 };
                cells = text(row).chars().collect();
                at = if forward { -1 } else { cells.len() as isize };
                if key == b'w' && here != 0 {
                    // The row ended a word: the next row's first word is where `w` lands.
                    let first = cells.iter().position(|c| class(*c) != 0);
                    if let Some(first) = first {
                        self.cursor = (row, first);
                        self.follow(b);
                        return;
                    }
                }
                continue;
            }
            let there = class_at(&cells, next);
            let lands = match key {
                b'w' => there != 0 && there != here,
                b'e' => there != 0 && class_at(&cells, next + 1) != there,
                _ => there != 0 && class_at(&cells, next - 1) != there,
            };
            at = next;
            if lands && !is_spacer(&cells, at as usize) {
                self.cursor = (row, at as usize);
                self.follow(b);
                return;
            }
        }
    }

    /// Start a selection at the cursor in `mode`, or put the one in that mode down.
    pub fn toggle(&mut self, mode: Mode) {
        if self.mode == mode {
            self.mode = Mode::None;
        } else {
            if self.mode == Mode::None {
                self.anchor = self.cursor;
            }
            self.mode = mode;
        }
    }

    /// The selection's two ends in reading order, as (row, cell) with the last cell
    /// included. Line mode takes whole rows.
    pub fn selected(&self) -> Option<((u64, usize), (u64, usize))> {
        let (a, b) = if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        };
        match self.mode {
            Mode::None => None,
            Mode::Char => Some((a, b)),
            Mode::Line => Some(((a.0, 0), (b.0, usize::MAX))),
        }
    }

    /// The selection as the pane draws it: rows of the view and screen columns.
    pub fn on_screen(
        &self,
        page: usize,
        text: &dyn Fn(u64) -> String,
    ) -> Option<crate::selection::Selection> {
        let ((r0, c0), (r1, c1)) = self.selected()?;
        let bottom = self.top + page.max(1) as u64 - 1;
        if r1 < self.top || r0 > bottom {
            return None;
        }
        // Far enough right to take any row whole, and far from overflowing when one is
        // added to it.
        const EDGE: usize = u16::MAX as usize;
        let start = if r0 < self.top {
            (0, 0)
        } else {
            let row = text(r0);
            ((r0 - self.top) as usize, column_of(&row, c0.min(EDGE)))
        };
        let end = if r1 > bottom {
            ((bottom - self.top) as usize, EDGE)
        } else if c1 >= EDGE {
            ((r1 - self.top) as usize, EDGE)
        } else {
            let row = text(r1);
            let wide = row.chars().nth(c1).map_or(1, width::columns).max(1);
            ((r1 - self.top) as usize, column_of(&row, c1) + wide - 1)
        };
        let mut selection = crate::selection::Selection::start(start.0, start.1);
        selection.extend(end.0, end.1);
        Some(selection)
    }

    /// Where the cursor is drawn: the view's row and the screen column, when it is in view.
    pub fn cursor_on_screen(&self, page: usize, row: &str) -> Option<(usize, usize)> {
        let at = self.cursor.0.checked_sub(self.top)?;
        (at < page as u64).then(|| (at as usize, column_of(row, self.cursor.1)))
    }
}

/// The highest the view's top row goes: the live screen filling the pane.
fn max_top(b: Bounds) -> u64 {
    (b.last + 1)
        .saturating_sub(b.page.max(1) as u64)
        .max(b.first)
}

/// Blank, word, or punctuation: what `w`, `b` and `e` step between.
fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Whether a cell is the spacer the grid puts after a wide character.
fn is_spacer(cells: &[char], at: usize) -> bool {
    at > 0 && cells.get(at) == Some(&' ') && width::columns(cells[at - 1]) == 2
}

/// The screen column a cell of a row is drawn in. Cells past the end of the row are blank,
/// one column each.
pub fn column_of(row: &str, cell: usize) -> usize {
    let cells: Vec<char> = row.chars().collect();
    let mut column = 0;
    for at in 0..cell {
        column += match cells.get(at) {
            None => 1,
            Some(_) if is_spacer(&cells, at) => 0,
            Some(c) => width::columns(*c),
        };
    }
    column
}

/// The text of cells `from..=to` of one row, without spacers.
fn cells_text(row: &str, from: usize, to: usize) -> String {
    let cells: Vec<char> = row.chars().collect();
    (from..=to.min(cells.len().saturating_sub(1)))
        .filter(|&at| !is_spacer(&cells, at))
        .filter_map(|at| cells.get(at))
        .collect()
}

/// The nearest match of `query` in the rows `text` answers for, from `from`, staying inside
/// `rows`: the cursor's own row past the cursor first, then whole rows the way the search
/// goes. The same rule the daemon applies to the history, so the screen and the disk agree
/// on what a match is.
pub fn find_near(
    text: &dyn Fn(u64) -> Option<String>,
    rows: std::ops::RangeInclusive<u64>,
    from: (u64, usize),
    query: &str,
    backward: bool,
) -> Option<(u64, usize)> {
    use qoro_runtime::daemon::find_in_row;
    if rows.contains(&from.0)
        && let Some(row) = text(from.0)
        && let Some(cell) = find_in_row(&row, query, Some(from.1), backward)
    {
        return Some((from.0, cell));
    }
    let next: Box<dyn Iterator<Item = u64>> = if backward {
        Box::new((*rows.start()..from.0.min(rows.end().saturating_add(1))).rev())
    } else {
        Box::new(from.0.saturating_add(1).max(*rows.start())..=*rows.end())
    };
    for at in next {
        let row = text(at)?;
        if let Some(cell) = find_in_row(&row, query, None, backward) {
            return Some((at, cell));
        }
    }
    None
}

/// A copy of a selection longer than what the client holds, being fetched.
///
/// The rows behind the screen come from the daemon at most `MAX_SCROLLBACK_ROWS` a request,
/// so a selection of five thousand rows is ten answers. They are gathered here and never in
/// the view's window: the window is what is drawn, and filling it with rows a person is not
/// looking at would draw them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Yank {
    pub terminal: u32,
    pub start: (u64, usize),
    pub end: (u64, usize),
    pub rows: BTreeMap<u64, String>,
    /// How many rows the daemon last said are behind the screen.
    pub total: u64,
    /// Requests made so far, for their ids.
    pub asked: u64,
}

impl Yank {
    /// The next request for rows still missing, or `None` when every row is here.
    pub fn next_request(&mut self) -> Option<ControlRequest> {
        let missing = (self.start.0..=self.end.0)
            .take_while(|at| *at < self.total)
            .find(|at| !self.rows.contains_key(at))?;
        let wanted = (self.end.0 + 1)
            .min(self.total)
            .saturating_sub(missing)
            .min(qoro_runtime::daemon::MAX_SCROLLBACK_ROWS as u64);
        self.asked += 1;
        Some(ControlRequest::Scrollback {
            id: format!("yank-{}", self.asked),
            terminal: self.terminal,
            up: (self.total - missing) as usize,
            rows: wanted as usize,
        })
    }

    /// Keep the rows of one answer that the selection covers. Answers whether asking again
    /// can bring more: yes when this answer brought a new row, or when the history grew
    /// under the question, which moves the rows an answer starts from; no when neither,
    /// which means the history no longer has the rest, and the copy goes ahead with what
    /// there is rather than asking forever.
    pub fn take(&mut self, from: u64, total: u64, lines: Vec<String>) -> bool {
        let grew = total > self.total;
        self.total = total;
        let mut new = false;
        for (at, line) in (from..).zip(lines) {
            if at >= self.start.0 && at <= self.end.0 && !self.rows.contains_key(&at) {
                self.rows.insert(at, line);
                new = true;
            }
        }
        new || grew
    }

    /// The selection as text: each row to its last character, rows joined by newlines.
    pub fn text(&self) -> String {
        let mut out = Vec::new();
        for at in self.start.0..=self.end.0 {
            let row = self.rows.get(&at).map(String::as_str).unwrap_or("");
            let from = if at == self.start.0 { self.start.1 } else { 0 };
            let to = if at == self.end.0 {
                self.end.1
            } else {
                usize::MAX
            };
            out.push(cells_text(row, from, to).trim_end().to_string());
        }
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds {
            first: 0,
            last: 99,
            page: 10,
        }
    }

    fn rows(at: u64) -> String {
        match at {
            5 => "foo bar_baz, qux".to_string(),
            6 => "   indented".to_string(),
            7 => "a\u{4e16} b".to_string(),
            _ => format!("row {at}"),
        }
    }

    #[test]
    fn moving_past_the_view_moves_the_view_and_the_ends_stop_the_cursor() {
        let mut copy = Copy::open(1, (95, 0), 90, None);
        copy.rows(-10, bounds());
        assert_eq!((copy.cursor.0, copy.top), (85, 85));
        copy.rows(-1000, bounds());
        assert_eq!((copy.cursor.0, copy.top), (0, 0));
        copy.rows(1000, bounds());
        assert_eq!(
            (copy.cursor.0, copy.top),
            (99, 90),
            "the live screen fills the pane"
        );
        // The alternate screen has no history to go back into.
        let screen_only = Bounds {
            first: 90,
            ..bounds()
        };
        copy.rows(-1000, screen_only);
        assert_eq!((copy.cursor.0, copy.top), (90, 90));
    }

    #[test]
    fn the_wheel_moves_the_view_and_drags_the_cursor_only_to_its_edge() {
        let mut copy = Copy::open(1, (95, 3), 90, None);
        copy.scroll(-3, bounds());
        assert_eq!((copy.top, copy.cursor), (87, (95, 3)), "still in view");
        copy.scroll(-10, bounds());
        assert_eq!((copy.top, copy.cursor.0), (77, 86));
        copy.scroll(1000, bounds());
        assert_eq!((copy.top, copy.cursor.0), (90, 90));
    }

    #[test]
    fn sideways_steps_over_the_spacer_of_a_wide_character() {
        let mut copy = Copy::open(1, (7, 0), 0, None);
        copy.sideways(true, &rows(7));
        assert_eq!(copy.cursor.1, 1, "on the wide character");
        copy.sideways(true, &rows(7));
        assert_eq!(copy.cursor.1, 3, "past its spacer");
        copy.sideways(false, &rows(7));
        assert_eq!(copy.cursor.1, 1);
        // The screen column counts the wide character as two.
        assert_eq!(column_of(&rows(7), 3), 3);
        assert_eq!(column_of(&rows(7), 4), 4);
        assert_eq!(column_of("\u{4e16} \u{4e16} x", 4), 4);
    }

    #[test]
    fn line_edges_find_the_first_and_last_character() {
        let mut copy = Copy::open(1, (6, 5), 0, None);
        copy.line_edge(b'^', &rows(6));
        assert_eq!(copy.cursor.1, 3);
        copy.line_edge(b'$', &rows(6));
        assert_eq!(copy.cursor.1, 10);
        copy.line_edge(b'0', &rows(6));
        assert_eq!(copy.cursor.1, 0);
    }

    #[test]
    fn word_motions_stop_at_words_and_punctuation_and_cross_rows() {
        let text = |at: u64| rows(at);
        let mut copy = Copy::open(1, (5, 0), 0, None);
        copy.word(b'w', bounds(), &text);
        assert_eq!(copy.cursor, (5, 4), "bar_baz is one word");
        copy.word(b'w', bounds(), &text);
        assert_eq!(copy.cursor, (5, 11), "the comma is its own");
        copy.word(b'e', bounds(), &text);
        assert_eq!(copy.cursor, (5, 15));
        copy.word(b'w', bounds(), &text);
        assert_eq!(copy.cursor, (6, 3), "onto the next row's first word");
        copy.word(b'b', bounds(), &text);
        assert_eq!(copy.cursor, (5, 13));
        copy.word(b'b', bounds(), &text);
        assert_eq!(copy.cursor, (5, 11));
    }

    #[test]
    fn both_selection_modes_draw_where_they_are_and_nowhere_else() {
        let text = |at: u64| rows(at);
        let mut copy = Copy::open(1, (5, 4), 0, None);
        assert_eq!(copy.selected(), None);
        copy.toggle(Mode::Char);
        copy.cursor = (7, 1);
        assert_eq!(copy.selected(), Some(((5, 4), (7, 1))));
        let drawn = copy.on_screen(10, &text).expect("in view");
        assert_eq!(drawn.columns_on(5, 80), Some(4..80));
        assert_eq!(
            drawn.columns_on(7, 80),
            Some(0..3),
            "the wide character whole"
        );
        copy.toggle(Mode::Line);
        assert_eq!(copy.selected(), Some(((5, 0), (7, usize::MAX))));
        let drawn = copy.on_screen(10, &text).expect("in view");
        assert_eq!(drawn.columns_on(7, 80), Some(0..80));
        // Scrolled past it, nothing of it is drawn.
        copy.top = 50;
        assert!(copy.on_screen(10, &text).is_none());
        copy.toggle(Mode::Line);
        assert_eq!(copy.selected(), None, "the same key puts it down");
    }

    #[test]
    fn a_search_on_the_screen_goes_the_way_asked_from_the_cursor() {
        let text = |at: u64| {
            Some(if at == 93 {
                "a hit and a hit".to_string()
            } else {
                rows(at)
            })
        };
        let screen = 90..=99;
        assert_eq!(
            find_near(&text, screen.clone(), (93, 2), "hit", false),
            Some((93, 12))
        );
        assert_eq!(
            find_near(&text, screen.clone(), (93, 12), "hit", true),
            Some((93, 2))
        );
        assert_eq!(
            find_near(&text, screen.clone(), (97, 0), "HIT", true),
            None,
            "smart case"
        );
        assert_eq!(
            find_near(&text, screen.clone(), (97, 0), "hit", true),
            Some((93, 12))
        );
        assert_eq!(
            find_near(&text, screen.clone(), (93, 12), "hit", false),
            None
        );
        // A cursor above the screen searches the screen from its top.
        assert_eq!(
            find_near(&text, screen, (40, 0), "hit", false),
            Some((93, 2))
        );
    }

    /// Found in review: an agent printing while a yank is fetched moves the rows an answer
    /// starts from, so an answer can bring none of the rows asked for. That is a reason to
    /// ask again, not the end of the history.
    #[test]
    fn a_yank_asks_again_when_the_history_grew_under_it() {
        let mut yank = Yank {
            terminal: 4,
            start: (100, 0),
            end: (120, 0),
            rows: BTreeMap::new(),
            total: 1_200,
            asked: 0,
        };
        assert!(yank.next_request().is_some());
        // The history grew by a hundred rows before the answer: it starts past the range.
        let shifted = (200..221).map(|at| format!("history {at}")).collect();
        assert!(
            yank.take(200, 1_300, shifted),
            "grown, so worth asking again"
        );
        let Some(ControlRequest::Scrollback { up, .. }) = yank.next_request() else {
            panic!("nothing asked again");
        };
        assert_eq!(up, 1_200, "asked from the new end");
        // Nothing new and nothing grown: the end of what there is.
        assert!(!yank.take(200, 1_300, vec!["history 200".to_string()]));
    }

    /// The screen's rows are grid rows too, with a spacer after each wide character.
    #[test]
    fn a_search_on_the_screen_finds_wide_characters_across_their_spacers() {
        let text = |at: u64| {
            Some(if at == 95 {
                "ok \u{4e16} \u{754c} done".to_string()
            } else {
                rows(at)
            })
        };
        assert_eq!(
            find_near(&text, 90..=99, (99, 0), "\u{4e16}\u{754c}", true),
            Some((95, 3))
        );
        // And the cursor lands on the column the character is drawn in.
        assert_eq!(column_of("ok \u{4e16} \u{754c} done", 3), 3);
        assert_eq!(column_of("ok \u{4e16} \u{754c} done", 5), 5);
    }

    #[test]
    fn a_yank_fetches_what_it_lacks_in_chunks_and_copies_it_whole() {
        let mut yank = Yank {
            terminal: 4,
            start: (100, 2),
            end: (1_300, 3),
            rows: BTreeMap::new(),
            total: 1_200,
            asked: 0,
        };
        // The screen's rows are already here.
        for at in 1_200..=1_300 {
            yank.rows.insert(at, format!("screen {at}"));
        }
        let mut asks = 0;
        while let Some(ask) = yank.next_request() {
            asks += 1;
            let ControlRequest::Scrollback { up, rows, id, .. } = ask else {
                panic!("not a scrollback request");
            };
            assert!(id.starts_with("yank-"));
            assert!(rows <= qoro_runtime::daemon::MAX_SCROLLBACK_ROWS);
            let from = 1_200 - up as u64;
            let lines = (from..from + rows as u64)
                .map(|at| format!("history {at}"))
                .collect();
            assert!(yank.take(from, 1_200, lines));
        }
        assert_eq!(
            asks, 3,
            "1,100 history rows are three answers of at most 500"
        );
        let text = yank.text();
        assert_eq!(text.lines().count(), 1_201);
        assert!(
            text.starts_with("story 100\nhistory 101"),
            "from the anchor's cell"
        );
        assert!(
            text.contains("history 1199\nscreen 1200"),
            "history then screen"
        );
        assert!(text.ends_with("screen 1299\nscre"), "to the cursor's cell");
        // An answer that brings nothing new is the end of what there is.
        assert!(!yank.take(0, 1_200, vec!["history 0".to_string()]));
    }
}

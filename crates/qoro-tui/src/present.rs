//! Putting a painted frame on the terminal, only where it differs from the last one.
//!
//! `screen::paint` draws the whole screen every time, which keeps it a pure function a test
//! can read. Writing all of that on every frame is what made the screen shimmer: a
//! streaming agent asks for dozens of frames a second, each one tens of kilobytes, and a
//! terminal (or a tmux between us and it) that shows a frame half written shows the old
//! and the new screen at once. Measured before this, a pane streaming at sixty frames a
//! second pushed 800 KB/s at the terminal.
//!
//! So the frame is cut into screen rows here and a row is written only when its bytes
//! differ from what the previous frame put on that row. A row, not a cell: comparing rows
//! is one `memcmp` each, and a row is never split inside a wide character or a colour run.
//! Everything that is written goes inside one synchronized update (`CSI ? 2026`), so a
//! terminal that supports it shows the frame whole or not at all. A frame identical to the
//! last writes nothing.
//!
//! The painted frame uses exactly five shapes: cursor placement (`CSI r;c H`), colour
//! (`CSI ... m`), cursor visibility (`CSI ? 25 h/l`), a link's open or close
//! (`OSC 8 ... ST`, no column) and text. Anything else is kept as text of the row it lands
//! on, so an unknown sequence is still written, never dropped. A link is carried like a
//! colour: a changed cell is written inside its own link's open and close, so a change
//! under a long address costs one address, not a row of them.

use std::fmt::Write as _;

const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";
const HIDE: &str = "\x1b[?25l";
const SHOW: &str = "\x1b[?25h";
const RESET: &str = "\x1b[0m";

/// What is on the terminal, as the rows the last frame wrote.
#[derive(Default)]
pub struct Presenter {
    /// The size the rows were written at. A different size repaints everything, since the
    /// terminal has reflowed or cleared what was there.
    size: (u16, u16),
    /// Each row as written: every write that landed on it, in paint order.
    rows: Vec<String>,
    /// The frame being cut, swapped with `rows` once written. Kept so a steady stream of
    /// frames allocates nothing.
    next: Vec<String>,
    /// Where the cursor was left, one based, and whether it was shown.
    cursor: Option<((usize, usize), bool)>,
}

impl Presenter {
    /// Where the last frame left the cursor, one based, and whether it was shown.
    /// The image block restores exactly this after placing pictures.
    pub fn cursor(&self) -> Option<((usize, usize), bool)> {
        self.cursor
    }

    /// The bytes that turn the last frame into `painted`, appended to `out`.
    ///
    /// Nothing is appended when nothing changed.
    pub fn present(&mut self, painted: &str, cols: u16, lines: u16, out: &mut String) {
        let full = self.size != (cols, lines);
        let (cursor, visible) = cut(painted, lines as usize, &mut self.next);
        let changed = |r: usize| full || self.rows.get(r) != self.next.get(r);
        let any = (0..self.next.len()).any(changed);
        let placed = Some((cursor, visible));
        if !any && placed == self.cursor {
            return;
        }
        out.push_str(SYNC_BEGIN);
        if any {
            // Hidden while rows are written, so it is never seen walking the screen.
            out.push_str(HIDE);
            for r in (0..self.next.len()).filter(|&r| changed(r)) {
                match (full, self.rows.get(r)) {
                    (false, Some(was)) => trimmed(was, &self.next[r], out),
                    _ => out.push_str(&self.next[r]),
                }
            }
            out.push_str(RESET);
        }
        let _ = write!(out, "\x1b[{};{}H", cursor.0, cursor.1);
        out.push_str(if visible { SHOW } else { HIDE });
        out.push_str(SYNC_END);
        std::mem::swap(&mut self.rows, &mut self.next);
        self.size = (cols, lines);
        self.cursor = placed;
    }
}

/// Where the control sequence opening at `i` ends: the index of its final byte, when
/// `i` opens one (`ESC [`) and it is complete.
fn csi_end(bytes: &[u8], i: usize) -> Option<usize> {
    if bytes.get(i) != Some(&0x1b) || bytes.get(i + 1) != Some(&b'[') {
        return None;
    }
    let len = bytes[i + 2..]
        .iter()
        .position(|b| (0x40..=0x7e).contains(b))?;
    Some(i + 2 + len)
}

/// Where the operating system command opening at `i` (`ESC ]`) ends: just past its `BEL`
/// or `ESC \`, when it is complete.
fn osc_end(bytes: &[u8], i: usize) -> Option<usize> {
    let body = i + 2;
    bytes[body..]
        .iter()
        .enumerate()
        .find_map(|(at, &b)| match b {
            0x07 => Some(body + at + 1),
            0x1b if bytes.get(body + at + 1) == Some(&b'\\') => Some(body + at + 2),
            _ => None,
        })
}

/// Whether a colour sequence's parameters start from a clean slate.
fn resets(params: &str) -> bool {
    params.is_empty() || params == "0" || params.starts_with("0;")
}

/// Cut a painted frame into its rows, and say where it leaves the cursor.
///
/// Each row is the writes that land on it, each one written out as a placement, a reset,
/// the colour in effect where the write starts, and its bytes. That is enough to replay the
/// row on its own, in any frame, whatever was written before it.
fn cut(painted: &str, lines: usize, rows: &mut Vec<String>) -> ((usize, usize), bool) {
    rows.resize_with(lines, String::new);
    rows.iter_mut().for_each(String::clear);
    let bytes = painted.as_bytes();
    // The colour in effect: every colour sequence since the last reset, in order.
    let mut style = String::new();
    let mut at = (1usize, 1usize);
    // Where the terminal's cursor is: the last placement, moved on by the text written
    // since. Not `at`, which stays where the open write started.
    let mut cursor = at;
    let mut open: Option<usize> = None;
    let mut visible = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let Some(end) = csi_end(bytes, i) else {
                put(rows, &painted[i..], at, &mut open, &style);
                break;
            };
            let seq = &painted[i..=end];
            let params = &painted[i + 2..end];
            match bytes[end] {
                b'H' => {
                    let mut n = params.split(';').map(|p| p.parse().unwrap_or(1).max(1));
                    at = (n.next().unwrap_or(1), n.next().unwrap_or(1));
                    cursor = at;
                    open = None;
                }
                b'm' => {
                    if resets(params) {
                        style.clear();
                    }
                    if !(params.is_empty() || params == "0") {
                        style.push_str(seq);
                    }
                    if let Some(row) = open {
                        rows[row].push_str(seq);
                    }
                }
                b'h' | b'l' if params == "?25" => visible = bytes[end] == b'h',
                _ => put(rows, seq, at, &mut open, &style),
            }
            i = end + 1;
        } else if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b']') {
            // A link's open or close: kept in the row it lands on, and taking no column,
            // so the cursor is still where the text left it.
            let Some(end) = osc_end(bytes, i) else {
                put(rows, &painted[i..], at, &mut open, &style);
                break;
            };
            put(rows, &painted[i..end], at, &mut open, &style);
            i = end;
        } else {
            // Text up to the next escape. Starting the search one byte on keeps a lone
            // escape that opens no sequence moving forward as text.
            let end = bytes[i + 1..]
                .iter()
                .position(|&b| b == 0x1b)
                .map_or(bytes.len(), |p| i + 1 + p);
            put(rows, &painted[i..end], at, &mut open, &style);
            cursor.1 += crate::width::width(&painted[i..end]);
            i = end;
        }
    }
    (cursor, visible)
}

/// Append text to the write open at `at`, opening one on its row if none is.
fn put(rows: &mut [String], s: &str, at: (usize, usize), open: &mut Option<usize>, style: &str) {
    if rows.is_empty() {
        return;
    }
    let row = *open.get_or_insert_with(|| {
        // A placement past the bottom lands on the bottom row, as the terminal would.
        let row = at.0.clamp(1, rows.len()) - 1;
        let _ = write!(rows[row], "\x1b[{};{}H{RESET}{style}", at.0, at.1);
        row
    });
    rows[row].push_str(s);
}

/// Write the cells of a changed row that differ from what is on screen, or all of it.
///
/// Most of a changed row is not: a streamed word lands at the end of a line, a spinner
/// turns at the start of one, and every pane row begins with the same column on the
/// left. So both rows are laid out as the cells they draw, later writes over earlier ones
/// the way a terminal draws them, and only the runs of cells that differ are written, each
/// with the colour and the link in effect on it. Writing only the differing cells leaves
/// exactly what writing the whole row would. A row this side cannot lay out the way any
/// terminal does (a sequence other than a placement, a colour or a link, or a zero width
/// character, whose cluster a terminal may draw wider than its parts) is written whole.
///
/// A link is carried like a colour, so no run ever leaves one open. Measured before this,
/// a row of linked cells was written whole on every change: one changed cell a row every
/// 16 ms, under two 2 KB addresses in turn, was over 500 MB in ten seconds.
fn trimmed(was: &str, now: &str, out: &mut String) {
    let (Some(old), Some(new)) = (cells(was), cells(now)) else {
        out.push_str(now);
        return;
    };
    let row = now[2..].split(';').next().unwrap_or("1");
    let dirty = |col: &usize, cell: &Cell| old.get(col) != Some(cell);
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (&col, cell) in &new {
        if !dirty(&col, cell) {
            continue;
        }
        match runs.last_mut() {
            Some((_, end)) if *end == col => *end = col + 1,
            _ => {
                // A run never starts on the right half of a wide character: its left half
                // is what draws it.
                let start = if cell.glyph.is_empty() { col - 1 } else { col };
                runs.push((start, col + 1));
            }
        }
    }
    for (start, end) in runs {
        let _ = write!(out, "\x1b[{row};{start}H{RESET}");
        let (mut sgr, mut link): (&str, Option<&str>) = ("", None);
        for col in start..end {
            let Some(cell) = new.get(&col) else { continue };
            if cell.glyph.is_empty() {
                continue;
            }
            if link.is_some() && link != cell.link {
                out.push_str(LINK_CLOSE);
                link = None;
            }
            if sgr != cell.sgr {
                if !sgr.is_empty() {
                    out.push_str(RESET);
                }
                out.push_str(&cell.sgr);
                sgr = &cell.sgr;
            }
            if link != cell.link
                && let Some(open) = cell.link
            {
                out.push_str(open);
                link = Some(open);
            }
            out.push_str(cell.glyph);
        }
        if link.is_some() {
            out.push_str(LINK_CLOSE);
        }
    }
}

/// The escape that ends a link.
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

/// One screen cell as a row draws it.
#[derive(Debug, PartialEq, Eq)]
struct Cell<'a> {
    /// What is drawn there; empty on the right half of a wide character.
    glyph: &'a str,
    /// Every colour sequence since the last reset.
    sgr: String,
    /// The escape that opened the link this cell is in, if it is in one.
    link: Option<&'a str>,
}

/// The cells a row draws, by one based column, later writes over earlier ones, or `None`
/// when this side cannot lay it out the way any terminal would.
fn cells(row: &str) -> Option<std::collections::BTreeMap<usize, Cell<'_>>> {
    let bytes = row.as_bytes();
    let mut map = std::collections::BTreeMap::new();
    let (mut col, mut sgr, mut link) = (1usize, String::new(), None);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b']') {
            let end = osc_end(bytes, i)?;
            let terminator = if bytes[end - 1] == 0x07 { 1 } else { 2 };
            // `8 ; params ; target`: an empty target closes the link.
            let target = row[i + 2..end - terminator]
                .strip_prefix("8;")?
                .split_once(';')?
                .1;
            link = (!target.is_empty()).then_some(&row[i..end]);
            i = end;
        } else if bytes[i] == 0x1b {
            let end = csi_end(bytes, i)?;
            let params = &row[i + 2..end];
            match bytes[end] {
                // Colour and link carry over a placement, as they do on a terminal.
                b'H' => col = params.split(';').nth(1)?.parse().ok()?,
                b'm' => {
                    if resets(params) {
                        sgr.clear();
                    }
                    if !(params.is_empty() || params == "0") {
                        sgr.push_str(&row[i..=end]);
                    }
                }
                _ => return None,
            }
            i = end + 1;
        } else {
            let c = row[i..].chars().next()?;
            let w = crate::width::columns(c);
            if w == 0 {
                return None;
            }
            let glyph = &row[i..i + c.len_utf8()];
            // As a terminal draws it: writing over either half of a wide character blanks
            // the other half.
            for at in col..col + w {
                if at > 1
                    && map.get(&at).is_some_and(|c: &Cell| c.glyph.is_empty())
                    && let Some(lead) = map.get_mut(&(at - 1))
                {
                    lead.glyph = " ";
                }
                if at + 1 >= col + w
                    && map
                        .get(&(at + 1))
                        .is_some_and(|c: &Cell| c.glyph.is_empty())
                    && let Some(right) = map.get_mut(&(at + 1))
                {
                    right.glyph = " ";
                }
            }
            for (at, glyph) in [(col, glyph), (col + 1, "")].into_iter().take(w) {
                map.insert(
                    at,
                    Cell {
                        glyph,
                        sgr: sgr.clone(),
                        link,
                    },
                );
            }
            col += w;
            i += c.len_utf8();
        }
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(rows: &[&str], cursor: (usize, usize)) -> String {
        let mut out = String::from(HIDE);
        for (i, r) in rows.iter().enumerate() {
            let _ = write!(out, "\x1b[{};1H\x1b[0;38;5;7m{r}\x1b[0m", i + 1);
        }
        let _ = write!(out, "\x1b[{};{}H{SHOW}", cursor.0, cursor.1);
        out
    }

    fn present(p: &mut Presenter, painted: &str) -> String {
        let mut out = String::new();
        p.present(painted, 10, 3, &mut out);
        out
    }

    #[test]
    fn the_first_frame_is_written_whole_inside_one_synchronized_update() {
        let mut p = Presenter::default();
        let out = present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        assert!(
            out.starts_with(SYNC_BEGIN) && out.ends_with(SYNC_END),
            "{out:?}"
        );
        for text in ["aaa", "bbb", "ccc"] {
            assert!(out.contains(text), "{out:?}");
        }
        assert!(out.contains(&format!("\x1b[3;4H{SHOW}")), "{out:?}");
    }

    #[test]
    fn an_identical_frame_writes_nothing() {
        let mut p = Presenter::default();
        let f = frame(&["aaa", "bbb", "ccc"], (3, 4));
        present(&mut p, &f);
        assert_eq!(present(&mut p, &f), "");
    }

    #[test]
    fn only_the_row_that_changed_is_written() {
        let mut p = Presenter::default();
        present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        let out = present(&mut p, &frame(&["aaa", "BBB", "ccc"], (3, 4)));
        assert!(out.contains("\x1b[2;1H\x1b[0m\x1b[0;38;5;7mBBB"), "{out:?}");
        assert!(!out.contains("aaa") && !out.contains("ccc"), "{out:?}");
    }

    #[test]
    fn a_cursor_move_writes_only_the_move() {
        let mut p = Presenter::default();
        present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        let out = present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 5)));
        assert_eq!(out, format!("{SYNC_BEGIN}\x1b[3;5H{SHOW}{SYNC_END}"));
    }

    #[test]
    fn a_new_size_writes_every_row_again() {
        let mut p = Presenter::default();
        let f = frame(&["aaa", "bbb", "ccc"], (3, 4));
        present(&mut p, &f);
        let mut out = String::new();
        p.present(&f, 11, 3, &mut out);
        for text in ["aaa", "bbb", "ccc"] {
            assert!(out.contains(text), "{out:?}");
        }
    }

    #[test]
    fn a_row_carries_the_colour_its_write_started_in() {
        // The colour is set on one row and the text written on the next without setting
        // it again: replayed alone, the second row must still carry it.
        let mut p = Presenter::default();
        let f = "\x1b[1;1H\x1b[0;31mred\x1b[2;1Hstill red";
        let mut out = String::new();
        p.present(f, 10, 2, &mut out);
        assert!(
            out.contains("\x1b[2;1H\x1b[0m\x1b[0;31mstill red"),
            "{out:?}"
        );
    }

    #[test]
    fn an_overlay_is_replayed_over_its_row_in_paint_order() {
        let mut p = Presenter::default();
        present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        let mut f = frame(&["aaa", "bbb", "ccc"], (3, 4));
        f.push_str("\x1b[2;2H\x1b[7mX\x1b[0m");
        let out = present(&mut p, &f);
        // The row under it is already on screen, so only the cell it covers is written.
        assert!(out.contains("\x1b[2;2H\x1b[0m\x1b[7mX"), "{out:?}");
        assert!(!out.contains("aaa") && !out.contains("bbb"), "{out:?}");
        // And closing the overlay puts back the cell it covered.
        let out = present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        assert!(out.contains("\x1b[2;2H\x1b[0m\x1b[0;38;5;7mb"), "{out:?}");
        assert!(!out.contains('X'), "{out:?}");
    }

    #[test]
    fn a_sequence_it_does_not_know_is_written_not_dropped() {
        let mut p = Presenter::default();
        let mut out = String::new();
        p.present("\x1b[1;1Hab\x1b[2Kcd\x1b]52;c;eA==\x07", 10, 1, &mut out);
        assert!(out.contains("ab\x1b[2Kcd\x1b]52;c;eA==\x07"), "{out:?}");
    }

    /// A frame that ends on text with no placement after it leaves the cursor after the
    /// text, which is where a whole repaint leaves it.
    #[test]
    fn a_frame_with_no_final_placement_leaves_the_cursor_after_its_text() {
        let mut p = Presenter::default();
        let mut out = String::new();
        p.present(&format!("\x1b[1;1Habc{SHOW}"), 10, 1, &mut out);
        assert!(out.contains(&format!("\x1b[1;4H{SHOW}")), "{out:?}");
    }

    #[test]
    fn a_hidden_cursor_stays_hidden() {
        let mut p = Presenter::default();
        let mut out = String::new();
        p.present(&format!("{HIDE}\x1b[1;1Habc"), 10, 1, &mut out);
        assert!(out.ends_with(&format!("{HIDE}{SYNC_END}")), "{out:?}");
        assert!(!out.contains(SHOW), "{out:?}");
    }

    /// A screen that understands what the presenter writes: placement, colour and text.
    /// Each cell is its character and the colour sequences in effect when it was drawn.
    /// Links count as part of a cell's look, and what comes back is the link left open at
    /// the end of the bytes, which no write may leave.
    fn screen(bytes: &str, cells: &mut [Vec<(char, String)>]) -> Option<String> {
        let (mut row, mut col, mut style) = (0usize, 0usize, String::new());
        let mut link: Option<String> = None;
        let b = bytes.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == 0x1b && b.get(i + 1) == Some(&b']') {
                let end = osc_end(b, i).expect("a whole sequence");
                let terminator = if b[end - 1] == 0x07 { 1 } else { 2 };
                let target = bytes[i + 2..end - terminator]
                    .strip_prefix("8;")
                    .and_then(|body| body.split_once(';'))
                    .map(|(_, target)| target.to_string())
                    .unwrap_or_default();
                link = (!target.is_empty()).then_some(target);
                i = end;
            } else if let Some(end) = csi_end(b, i) {
                let params = &bytes[i + 2..end];
                match b[end] {
                    b'H' => {
                        let mut n = params.split(';').map(|p| p.parse::<usize>().unwrap_or(1));
                        row = n.next().unwrap_or(1) - 1;
                        col = n.next().unwrap_or(1) - 1;
                    }
                    b'm' => {
                        if resets(params) {
                            style.clear();
                        }
                        if !(params.is_empty() || params == "0") {
                            style.push_str(&bytes[i..=end]);
                        }
                    }
                    _ => {}
                }
                i = end + 1;
            } else {
                let c = bytes[i..].chars().next().unwrap();
                let look = format!("{style}{}", link.as_deref().unwrap_or(""));
                let w = crate::width::columns(c);
                if let Some(line) = cells.get_mut(row) {
                    // As a terminal does: a wide character takes the next cell too (`\0`), and
                    // writing over either half of one blanks the other half.
                    for at in col..col + w {
                        if line.get(at).is_some_and(|c| c.0 == '\0') && at > 0 {
                            line[at - 1].0 = ' ';
                        }
                        if line.get(at + 1).is_some_and(|c| c.0 == '\0') && at + 1 >= col + w {
                            line[at + 1].0 = ' ';
                        }
                    }
                    if let Some(cell) = line.get_mut(col) {
                        *cell = (c, look.clone());
                    }
                    if w == 2
                        && let Some(cell) = line.get_mut(col + 1)
                    {
                        *cell = ('\0', look);
                    }
                }
                col += w;
                i += c.len_utf8();
            }
        }
        link
    }

    /// Writing only what changed leaves the same screen as writing the whole row, over
    /// thousands of random edits: a word appended, a glyph turned, a colour changed, text
    /// deleted or inserted anywhere in a row.
    #[test]
    fn a_trimmed_row_leaves_the_screen_a_whole_row_would() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let pieces = [
            "a",
            "b",
            " ",
            "xy",
            "\u{2502}",
            "\u{4e2d}",
            "\x1b[1m",
            "\x1b[0;31m",
            "\x1b[0m",
            "\x1b[38;5;7m",
            "\x1b]8;;https://a.example\x1b\\",
            "\x1b]8;;https://b.example\x1b\\",
            LINK_CLOSE,
            // A second write on the row, over part of the first, the way `put` starts one.
            "\x1b]8;;\x1b\\\x1b[1;3H\x1b[0m",
        ];
        // Closed at the end, as `paint_row` always leaves a row.
        let row = |p: &[&str]| format!("\x1b[1;1H\x1b[0m{}{LINK_CLOSE}", p.concat());
        let mut shorter = 0;
        for _ in 0..5000 {
            let was: Vec<&str> = (0..next(12)).map(|_| pieces[next(pieces.len())]).collect();
            let mut now = was.clone();
            for _ in 0..1 + next(3) {
                let at = next(now.len() + 1);
                match next(3) {
                    0 => now.insert(at, pieces[next(pieces.len())]),
                    1 if at < now.len() => drop(now.remove(at)),
                    _ if at < now.len() => now[at] = pieces[next(pieces.len())],
                    _ => {}
                }
            }
            let (was, now) = (row(&was), row(&now));
            let blank = vec![vec![(' ', String::new()); 40]; 1];
            let (mut whole, mut cut) = (blank.clone(), blank);
            screen(&was, &mut whole);
            screen(&was, &mut cut);
            screen(&now, &mut whole);
            let mut out = String::new();
            trimmed(&was, &now, &mut out);
            assert_eq!(
                screen(&out, &mut cut),
                None,
                "a write left a link open: {out:?}"
            );
            assert_eq!(whole, cut, "was {was:?} now {now:?} wrote {out:?}");
            shorter += usize::from(out.len() < now.len());
        }
        // And the cut actually happens, or this proves nothing.
        assert!(shorter > 2500, "only {shorter} of 5000 rows were cut");
    }

    #[test]
    fn a_streamed_word_writes_only_the_word() {
        let mut out = String::new();
        trimmed(
            "\x1b[4;1H\x1b[0m\x1b[0;38;5;7m| hello",
            "\x1b[4;1H\x1b[0m\x1b[0;38;5;7m| hello world",
            &mut out,
        );
        assert_eq!(out, "\x1b[4;8H\x1b[0m\x1b[0;38;5;7m world");
    }

    #[test]
    fn a_turned_spinner_writes_only_the_spinner() {
        let mut out = String::new();
        trimmed(
            "\x1b[2;1H\x1b[0m | working on a long line",
            "\x1b[2;1H\x1b[0m / working on a long line",
            &mut out,
        );
        assert_eq!(out, "\x1b[2;2H\x1b[0m/");
    }

    /// A link's open and close take no column, so the cursor after a linked word is
    /// where the word ends; text added after a closed link writes only the text, and a
    /// row rewritten without the link opens nothing.
    #[test]
    fn a_link_takes_no_column_and_text_after_it_writes_alone() {
        let open = "\x1b]8;;https://example.com\x1b\\";
        let mut p = Presenter::default();
        let mut out = String::new();
        p.present(
            &format!("\x1b[1;1H{open}link{LINK_CLOSE}{SHOW}"),
            20,
            2,
            &mut out,
        );
        assert!(out.contains(&format!("\x1b[1;5H{SHOW}")), "{out:?}");
        let mut out = String::new();
        p.present(
            &format!("\x1b[1;1H{open}link{LINK_CLOSE}s{SHOW}"),
            20,
            2,
            &mut out,
        );
        assert!(out.contains("\x1b[1;5H\x1b[0ms"), "{out:?}");
        assert!(!out.contains("\x1b]8;;h"), "{out:?}");
        let mut out = String::new();
        p.present(&format!("\x1b[1;1Hplain{SHOW}"), 20, 2, &mut out);
        assert!(!out.contains("\x1b]8;;h"), "{out:?}");
    }

    /// One changed cell in a row whose cells are linked in turn to two 2 KB addresses
    /// writes that cell inside one open and one close of its link, not the row. Measured
    /// before this: the whole row, 80 addresses of 2 KB, on every change.
    #[test]
    fn one_changed_cell_under_a_long_link_writes_one_address() {
        let open_of = |n: usize| {
            format!(
                "\x1b]8;;https://e.example/{}{}\x1b\\",
                "a".repeat(2048),
                n % 2
            )
        };
        let open = open_of(40);
        let row = |changed: bool| {
            let mut s = String::from("\x1b[1;1H\x1b[0m\x1b[34m");
            for n in 0..80 {
                let c = if changed && n == 40 { 'Y' } else { 'x' };
                let _ = write!(s, "{}{c}{LINK_CLOSE}", open_of(n));
            }
            s.push_str("\x1b[0m");
            s
        };
        let (was, now) = (row(false), row(true));
        assert!(now.len() > 160_000, "the row is what made it expensive");
        let mut out = String::new();
        trimmed(&was, &now, &mut out);
        assert!(out.len() < 4096, "{} bytes for one cell", out.len());
        assert_eq!(out.matches(&open).count(), 1, "{out:?}");
        assert!(out.ends_with(&format!("{open}Y{LINK_CLOSE}")), "{out:?}");
        assert!(out.contains("\x1b[1;41H\x1b[0m\x1b[34m"), "{out:?}");
    }

    /// Cells under two addresses in turn: a change under the second reopens the second,
    /// never the first, and closes it after.
    #[test]
    fn a_trimmed_write_reopens_the_link_it_resumes_in() {
        let a = "\x1b]8;;https://a.example\x1b\\";
        let b = "\x1b]8;;https://b.example\x1b\\";
        let row = |cells: &[char]| {
            let mut s = String::from("\x1b[1;1H\x1b[0m");
            for (n, c) in cells.iter().enumerate() {
                let link = if n % 2 == 0 { a } else { b };
                let _ = write!(s, "{link}{c}{LINK_CLOSE}");
            }
            s
        };
        let was = row(&['p', 'q', 'r', 's', 't', 'u']);
        let now = row(&['p', 'q', 'r', 'S', 't', 'u']);
        let mut out = String::new();
        trimmed(&was, &now, &mut out);
        assert_eq!(out, format!("\x1b[1;4H\x1b[0m{b}S{LINK_CLOSE}"));
    }

    #[test]
    fn a_row_it_cannot_count_is_written_whole() {
        // A zero width joiner: a terminal may draw the cluster wider than its parts.
        let now = "\x1b[1;1H\x1b[0mab\u{200d}c";
        let mut out = String::new();
        trimmed("\x1b[1;1H\x1b[0mabxc", now, &mut out);
        assert_eq!(out, now);
        // Placed somewhere else: nothing is shared.
        let now = "\x1b[1;5H\x1b[0mabc";
        let mut out = String::new();
        trimmed("\x1b[1;1H\x1b[0mabc", now, &mut out);
        assert_eq!(out, now);
    }
}

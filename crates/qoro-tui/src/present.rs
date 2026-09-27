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
//! on, so an unknown sequence is still written, never dropped. A row holding a link is
//! always written whole: trimming it could write a tail whose open was left out.

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

/// Write the part of a changed row that differs from what is on screen, or all of it.
///
/// Most of a changed row is usually not: a streamed word lands at the end of a line, a
/// spinner turns at the start of one, and every pane row begins with the same column on
/// the left. The common start and end are skipped when skipping them leaves exactly what
/// writing the whole row would, which holds only for a row written in one go (a later
/// write on the same row can have covered part of the start) and whose columns this side
/// counts the way any terminal does (no zero width character, whose cluster a terminal
/// may draw wider than its parts). Anything else is written whole.
fn trimmed(was: &str, now: &str, out: &mut String) {
    let (Some(was_stops), Some(now_stops)) = (stops(was), stops(now)) else {
        out.push_str(now);
        return;
    };
    let prefix = common(was.bytes(), now.bytes());
    // The last boundary inside the shared start: identical bytes there drew identical
    // cells, so writing resumes at that column in the colour in effect there.
    let Some(&(from, col)) = now_stops.iter().rev().find(|(at, _)| *at <= prefix) else {
        // Placed somewhere else entirely: nothing on screen is shared.
        out.push_str(now);
        return;
    };
    // The first boundary inside the shared end whose twin on the old row sits at the
    // same column in the same colour: from there the old row already shows these bytes.
    let suffix = common(was.bytes().rev(), now.bytes().rev()).min(was.len().min(now.len()) - from);
    let to = now_stops
        .iter()
        .filter(|(at, _)| *at >= now.len() - suffix)
        .find(|&&(at, c)| {
            let twin = at + was.len() - now.len();
            was_stops.binary_search(&(twin, c)).is_ok() && style(was, twin) == style(now, at)
        })
        .map_or(now.len(), |(at, _)| *at);
    if from == to {
        return;
    }
    let row = now[2..].split(';').next().unwrap_or("1");
    let _ = write!(out, "\x1b[{row};{col}H{RESET}{}", style(now, from));
    out.push_str(&now[from..to]);
}

/// How many leading items two sequences share.
fn common(a: impl Iterator<Item = u8>, b: impl Iterator<Item = u8>) -> usize {
    a.zip(b).take_while(|(x, y)| x == y).count()
}

/// Every place a single-write row could resume, as the byte offset and the column there,
/// or `None` when the row cannot be cut: more than one write, a sequence other than
/// colour, or a zero width character.
fn stops(row: &str) -> Option<Vec<(usize, usize)>> {
    let bytes = row.as_bytes();
    let head = bytes.iter().position(|&b| b == b'H')?;
    let mut col: usize = row[2..head].split(';').nth(1)?.parse().ok()?;
    let mut out = vec![(head + 1, col)];
    let mut i = head + 1;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            let end = csi_end(bytes, i).filter(|&end| bytes[end] == b'm')?;
            i = end + 1;
        } else {
            let c = row[i..].chars().next()?;
            let w = crate::width::columns(c);
            if w == 0 {
                return None;
            }
            col += w;
            i += c.len_utf8();
        }
        out.push((i, col));
    }
    Some(out)
}

/// The colour in effect at `at` in a row: every colour sequence since the last reset.
fn style(row: &str, at: usize) -> String {
    let bytes = &row.as_bytes()[..at];
    let mut style = String::new();
    let mut i = 0;
    while let Some(start) = bytes[i..].iter().position(|&b| b == 0x1b).map(|p| i + p) {
        let Some(end) = csi_end(bytes, start) else {
            break;
        };
        let params = &row[start + 2..end];
        if bytes[end] == b'm' {
            if resets(params) {
                style.clear();
            }
            if !(params.is_empty() || params == "0") {
                style.push_str(&row[start..=end]);
            }
        }
        i = end + 1;
    }
    style
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
        let base = out.find("bbb").expect("row replayed");
        let over = out.find('X').expect("overlay replayed");
        assert!(base < over, "{out:?}");
        assert!(!out.contains("aaa"), "{out:?}");
        // And closing the overlay puts the row back.
        let out = present(&mut p, &frame(&["aaa", "bbb", "ccc"], (3, 4)));
        assert!(out.contains("bbb") && !out.contains('X'), "{out:?}");
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
    fn screen(bytes: &str, cells: &mut [Vec<(char, String)>]) {
        let (mut row, mut col, mut style) = (0usize, 0usize, String::new());
        let b = bytes.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if let Some(end) = csi_end(b, i) {
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
                if let Some(cell) = cells.get_mut(row).and_then(|r| r.get_mut(col)) {
                    *cell = (c, style.clone());
                }
                col += crate::width::columns(c);
                i += c.len_utf8();
            }
        }
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
        ];
        let row = |p: &[&str]| format!("\x1b[1;1H\x1b[0m{}", p.concat());
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
            screen(&out, &mut cut);
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
    /// where the word ends; and a row holding one is written whole, open and close
    /// together, never trimmed to a tail that would leave a link open.
    #[test]
    fn a_linked_row_is_written_whole_and_its_link_takes_no_column() {
        let open = "\x1b]8;;https://example.com\x1b\\";
        let close = "\x1b]8;;\x1b\\";
        let mut p = Presenter::default();
        let mut out = String::new();
        p.present(
            &format!("\x1b[1;1H{open}link{close}{SHOW}"),
            20,
            2,
            &mut out,
        );
        assert!(out.contains(&format!("\x1b[1;5H{SHOW}")), "{out:?}");
        let mut out = String::new();
        p.present(
            &format!("\x1b[1;1H{open}link{close}s{SHOW}"),
            20,
            2,
            &mut out,
        );
        assert!(
            out.contains(&format!("\x1b[1;1H\x1b[0m{open}link{close}s")),
            "{out:?}"
        );
        // And the row after it, rewritten without the link, opens nothing and the link
        // written before it was already closed.
        let mut out = String::new();
        p.present(&format!("\x1b[1;1Hplain{SHOW}"), 20, 2, &mut out);
        assert!(!out.contains("\x1b]8;;h"), "{out:?}");
        assert_eq!(
            out.matches("\x1b]8;;h").count(),
            out.matches(close).count(),
            "{out:?}"
        );
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

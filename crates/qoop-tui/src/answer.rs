//! The last answer an agent gave, as a page to read over its pane.
//!
//! `ctrl-a M` on an agent's terminal reads the newest assistant message from the agent's
//! own transcript and lays it out here: headers bold and underlined, code quiet, lists
//! and paragraphs wrapped to the pane. Markdown the way a terminal can show it everywhere,
//! with no picture protocol and no font sizes: a header is bold and underlined because
//! every terminal can draw that, and a bigger one only some can.

use crate::theme::{self, Role};

/// An answer open over the pane: whose it is, what it says, how far down the reader has
/// scrolled, and its rows as last laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// What the page is about, drawn on its first row: the agent's session name.
    pub title: String,
    /// The answer as markdown, kept so a resize can lay it out again.
    pub text: String,
    /// Rows scrolled past at the top.
    pub offset: usize,
    /// The painted rows for [`Answer::width`], laid out once per width rather than once
    /// per frame: a long answer is not cheap to wrap.
    rows: Vec<String>,
    width: usize,
}

/// What one read of keys did to an open answer.
#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    Scrolled,
    Closed,
    Ignored,
    /// Not the page's: the prefix, a detach, or a mouse report other than the wheel. The
    /// page closes and the bytes run the ordinary path, the way the focus list hands on
    /// what it does not own.
    Pass,
}

impl Answer {
    pub fn new(title: String, text: String) -> Self {
        Answer {
            title,
            text,
            offset: 0,
            rows: Vec::new(),
            width: 0,
        }
    }

    /// The rows laid out for `width`, again only when the width changed, with the scroll
    /// kept inside what `height` rows can show.
    pub fn layout(&mut self, width: usize, height: usize) -> &[String] {
        if width != self.width || self.rows.is_empty() {
            self.rows = render(&self.text, width);
            self.width = width;
        }
        self.offset = self
            .offset
            .min(self.rows.len().saturating_sub(height.max(1)));
        &self.rows
    }

    /// The rows for `width`: the laid out ones when they are for this width, else a fresh
    /// layout, so a painter that could not lay out first still paints the right page.
    pub fn rows_for(&self, width: usize) -> std::borrow::Cow<'_, [String]> {
        if width == self.width && !self.rows.is_empty() {
            std::borrow::Cow::Borrowed(&self.rows)
        } else {
            std::borrow::Cow::Owned(render(&self.text, width))
        }
    }

    /// Scroll or close on one read of keys: `j`, down arrow, enter and the wheel go down,
    /// `k`, up arrow and the wheel up, space and `f` a page down, `b` a page up, `g` and
    /// `G` the top and the bottom, and `q` or escape close. A held key that arrives as
    /// `jjj` moves three rows. The prefix, `ctrl-q` and any other mouse report pass on;
    /// anything else is left alone, so a stray key never closes what a person reads.
    pub fn key(&mut self, bytes: &[u8], page: usize) -> Key {
        let page = page.max(1);
        let before = self.offset;
        if bytes.contains(&0x01) || bytes.contains(&0x11) {
            return Key::Pass;
        }
        if let Some(report) = bytes.strip_prefix(b"\x1b[<") {
            if report.starts_with(b"64;") {
                self.offset = self.offset.saturating_sub(3);
            } else if report.starts_with(b"65;") {
                self.offset += 3;
            } else {
                return Key::Pass;
            }
        } else {
            match bytes {
                b"q" | b"\x1b" => return Key::Closed,
                b"\r" | b"\x1b[B" | b"\x1bOB" => self.offset += 1,
                b"\x1b[A" | b"\x1bOA" => self.offset = self.offset.saturating_sub(1),
                b" " | b"f" | b"\x1b[6~" => self.offset += page,
                b"b" | b"\x1b[5~" => self.offset = self.offset.saturating_sub(page),
                b"g" => self.offset = 0,
                b"G" => self.offset = usize::MAX / 2,
                run if !run.is_empty() && run.iter().all(|b| *b == b'j') => {
                    self.offset += run.len()
                }
                run if !run.is_empty() && run.iter().all(|b| *b == b'k') => {
                    self.offset = self.offset.saturating_sub(run.len())
                }
                _ => return Key::Ignored,
            }
        }
        if self.offset == before {
            Key::Ignored
        } else {
            Key::Scrolled
        }
    }
}

/// One stretch of text in one look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ink {
    role: Role,
    bold: bool,
    underline: bool,
}

const PLAIN: Ink = Ink {
    role: Role::Text,
    bold: false,
    underline: false,
};

/// Markdown-lite laid out as painted rows, each exactly `width` columns wide.
///
/// Headers (`#` to `######`) are bold and underlined, the first level also bright;
/// fenced code keeps its lines, cut rather than wrapped, in the detail grey with the
/// fences faint; `-`, `*` and `+` items become bullets with their wrap indented under
/// the text; `**bold**` and `` `code` `` work inside a line. Everything else wraps at
/// word boundaries. Tabs become four spaces and every other control character is
/// dropped before anything is measured: the text came from a file an agent wrote, and
/// nothing in it may reach the terminal as a command.
pub fn render(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let clean: String = text
        .replace('\t', "    ")
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let mut rows: Vec<Vec<(String, Ink)>> = Vec::new();
    let mut fenced = false;
    for line in clean.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            rows.push(vec![(line.to_string(), faint())]);
            continue;
        }
        if fenced {
            rows.push(vec![(
                format!("  {line}"),
                Ink {
                    role: Role::Detail,
                    ..PLAIN
                },
            )]);
            continue;
        }
        if trimmed.is_empty() {
            rows.push(Vec::new());
            continue;
        }
        if let Some((level, title)) = header(trimmed) {
            let ink = Ink {
                role: if level == 1 {
                    Role::Focused
                } else {
                    Role::Text
                },
                bold: true,
                underline: true,
            };
            rows.extend(wrap(&[(title.to_string(), ink)], width, 0, 0));
            continue;
        }
        let indent = crate::width::width(&line[..line.len() - trimmed.len()]);
        if let Some(item) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|mark| trimmed.strip_prefix(mark))
        {
            let mut spans = vec![("\u{2022} ".to_string(), faint())];
            spans.extend(inline(item));
            rows.extend(wrap(&spans, width, indent, indent + 2));
            continue;
        }
        rows.extend(wrap(&inline(trimmed), width, indent, indent));
    }
    rows.into_iter().map(|row| paint(&row, width)).collect()
}

fn faint() -> Ink {
    Ink {
        role: Role::Faint,
        ..PLAIN
    }
}

/// A header line's level and text, when the line is one.
fn header(line: &str) -> Option<(usize, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    let rest = line.get(level..)?;
    ((1..=6).contains(&level) && rest.starts_with(' ')).then(|| (level, rest.trim()))
}

/// A line's `**bold**` and `` `code` `` spans; a marker with no partner is plain text.
fn inline(line: &str) -> Vec<(String, Ink)> {
    let mut out: Vec<(String, Ink)> = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let bold = rest.find("**");
        let code = rest.find('`');
        let (at, mark) = match (bold, code) {
            (Some(b), Some(c)) if c < b => (c, "`"),
            (Some(b), _) => (b, "**"),
            (None, Some(c)) => (c, "`"),
            (None, None) => break,
        };
        let after = &rest[at + mark.len()..];
        let Some(end) = after.find(mark) else {
            break;
        };
        if at > 0 {
            out.push((rest[..at].to_string(), PLAIN));
        }
        let ink = if mark == "`" {
            Ink {
                role: Role::Detail,
                ..PLAIN
            }
        } else {
            Ink {
                bold: true,
                ..PLAIN
            }
        };
        out.push((after[..end].to_string(), ink));
        rest = &after[end + mark.len()..];
    }
    if !rest.is_empty() {
        out.push((rest.to_string(), PLAIN));
    }
    out
}

/// Spans wrapped at word boundaries into rows of at most `width` columns: the first row
/// starts `lead` columns in, every row after it `hang` columns in. Runs of spaces between
/// words become one space, and a row never starts or ends with one. A word wider than a
/// row is cut across rows in one pass; a character wider than a whole row is dropped,
/// since no row could show it.
fn wrap(
    spans: &[(String, Ink)],
    width: usize,
    lead: usize,
    hang: usize,
) -> Vec<Vec<(String, Ink)>> {
    let lead = lead.min(width.saturating_sub(1));
    let hang = hang.min(width.saturating_sub(1));
    let mut rows: Vec<Vec<(String, Ink)>> = vec![vec![(" ".repeat(lead), PLAIN)]];
    let mut used = lead;
    let mut start = lead;
    let mut gap = false;
    let new_row = |rows: &mut Vec<Vec<(String, Ink)>>| rows.push(vec![(" ".repeat(hang), PLAIN)]);
    for (text, ink) in spans {
        let mut word = String::new();
        let flush = |word: &mut String,
                     rows: &mut Vec<Vec<(String, Ink)>>,
                     used: &mut usize,
                     start: &mut usize,
                     gap: &mut bool| {
            if word.is_empty() {
                return;
            }
            let wide = crate::width::width(word);
            let space = usize::from(*gap && *used > *start);
            if *used > *start && *used + space + wide > width {
                new_row(rows);
                *used = hang;
                *start = hang;
            } else if space == 1 {
                rows.last_mut()
                    .expect("a row")
                    .push((" ".to_string(), PLAIN));
                *used += 1;
            }
            // A word longer than the row goes across rows, a character at a time.
            let mut chunk = String::new();
            let mut chunk_w = 0usize;
            for c in word.chars() {
                let w = crate::width::columns(c);
                if w > width - hang {
                    continue;
                }
                if *used + chunk_w + w > width {
                    if !chunk.is_empty() {
                        rows.last_mut()
                            .expect("a row")
                            .push((std::mem::take(&mut chunk), *ink));
                    }
                    chunk_w = 0;
                    new_row(rows);
                    *used = hang;
                    *start = hang;
                }
                chunk.push(c);
                chunk_w += w;
            }
            *used += chunk_w;
            if !chunk.is_empty() {
                rows.last_mut().expect("a row").push((chunk, *ink));
            }
            word.clear();
            *gap = false;
        };
        for c in text.chars() {
            if c.is_whitespace() {
                flush(&mut word, &mut rows, &mut used, &mut start, &mut gap);
                gap = true;
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut rows, &mut used, &mut start, &mut gap);
    }
    rows
}

/// One row painted: each span in its ink, then the rest of the row blank, cut or padded
/// to exactly `width` columns so nothing of the pane shows through.
fn paint(row: &[(String, Ink)], width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for (text, ink) in row {
        if used >= width {
            break;
        }
        // Cut to what is left, never padded: the shared helper pads to the width it is
        // given, so it is given no more than the text itself needs.
        let shown = crate::width::text(text, crate::width::width(text).min(width - used));
        out.push_str(&theme::fg(ink.role));
        if ink.bold {
            out.push_str("\x1b[1m");
        }
        if ink.underline {
            out.push_str("\x1b[4m");
        }
        out.push_str(&shown);
        used += crate::width::width(&shown);
    }
    out.push_str(&theme::fg(Role::Text));
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out.push_str(theme::RESET);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text a row shows, escapes taken out.
    fn plain(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
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

    fn lines(text: &str, width: usize) -> Vec<String> {
        render(text, width).iter().map(|r| plain(r)).collect()
    }

    /// Headers lose their marks and gain bold and underline; the first level is bright.
    #[test]
    fn a_header_is_bold_and_underlined_without_its_marks() {
        let rows = render("# Plan\n## What changes", 20);
        assert_eq!(plain(&rows[0]).trim_end(), "Plan");
        assert!(rows[0].contains("\x1b[1m") && rows[0].contains("\x1b[4m"));
        assert!(rows[0].contains(&theme::fg(Role::Focused)));
        assert_eq!(plain(&rows[1]).trim_end(), "What changes");
        assert!(!rows[1].contains(&theme::fg(Role::Focused)));
        assert_eq!(header("#hashtag"), None, "a hash with no space is text");
    }

    /// Code keeps its lines and its indentation, in the detail grey, and is never wrapped.
    #[test]
    fn fenced_code_keeps_its_lines_and_is_cut_not_wrapped() {
        let rows = render(
            "```rust\nfn hints(focus: Focus) -> Vec<Hint> {}\n```\nafter",
            20,
        );
        let shown: Vec<String> = rows.iter().map(|r| plain(r)).collect();
        assert_eq!(shown.len(), 4, "{shown:?}");
        assert!(shown[1].starts_with("  fn hints"));
        assert!(rows[1].contains(&theme::fg(Role::Detail)));
        assert_eq!(shown[3].trim_end(), "after");
    }

    /// Bullets wrap under their own text, not under the bullet.
    #[test]
    fn a_bullet_wraps_under_its_text() {
        let shown = lines("- the key row follows the keyboard everywhere", 20);
        assert!(shown[0].starts_with("\u{2022} the key row"), "{shown:?}");
        assert!(shown[1].starts_with("  "), "{shown:?}");
        assert!(shown.len() >= 2);
    }

    /// Inline bold and code change the look of their words only; an unpaired marker is text.
    #[test]
    fn inline_bold_and_code_touch_only_their_words() {
        let spans = inline("use **one** helper `fit` here");
        assert_eq!(
            spans.iter().map(|(t, _)| t.as_str()).collect::<String>(),
            "use one helper fit here"
        );
        assert!(spans.iter().any(|(t, ink)| t == "one" && ink.bold));
        assert!(
            spans
                .iter()
                .any(|(t, ink)| t == "fit" && ink.role == Role::Detail)
        );
        let lone = inline("a ** alone");
        assert_eq!(
            lone.iter().map(|(t, _)| t.as_str()).collect::<String>(),
            "a ** alone"
        );
    }

    /// Every row is exactly the width, however long the words and however wide the glyphs.
    #[test]
    fn every_row_is_exactly_the_width() {
        let text = "# \u{4e2d}\u{6587} title\n\nsupercalifragilisticexpialidocious words \
                    and \u{1f30d}\u{1f30d} emoji\n- item\n```\nlong code line that is cut\n```";
        for width in [1usize, 5, 12, 40] {
            for row in render(text, width) {
                assert_eq!(crate::width::width(&plain(&row)), width, "{width}: {row:?}");
            }
        }
    }

    /// The keys scroll, page and close; a stray key never closes the page.
    #[test]
    fn keys_scroll_page_and_close() {
        let mut answer = Answer::new("claude".into(), String::new());
        assert_eq!(answer.key(b"j", 10), Key::Scrolled);
        assert_eq!(answer.offset, 1);
        assert_eq!(answer.key(b" ", 10), Key::Scrolled);
        assert_eq!(answer.offset, 11);
        assert_eq!(answer.key(b"g", 10), Key::Scrolled);
        assert_eq!(answer.key(b"k", 10), Key::Ignored, "already at the top");
        assert_eq!(answer.key(b"x", 10), Key::Ignored);
        assert_eq!(answer.key(b"q", 10), Key::Closed);
    }

    /// A word that ends exactly at the edge leaves the next row starting with a word, not
    /// a space, and a bullet never leaves an empty row behind it.
    #[test]
    fn a_row_never_starts_with_the_space_the_last_one_ended_at() {
        assert_eq!(
            lines("aaaa bbbbb ccc", 10),
            vec!["aaaa bbbbb", "ccc       "]
        );
        let bullet = lines("- aaaaaaaaaaaa bbbb", 8);
        assert!(bullet.iter().all(|r| !r.trim().is_empty()), "{bullet:?}");
    }

    /// A tab is four spaces and no character is ever shown twice or lost to a cut.
    #[test]
    fn tabs_become_spaces_and_nothing_repeats() {
        let shown = lines("\tabcdefgh", 5).concat();
        assert_eq!(shown.chars().filter(|c| *c == 'e').count(), 1, "{shown:?}");
        let code = lines("```\n\t\treturn\n```", 20);
        assert!(code[1].starts_with("          return"), "{code:?}");
        let wide = lines("- \u{4e2d}\u{6587} end", 5).concat();
        assert!(
            wide.contains('\u{4e2d}') && wide.contains('\u{6587}'),
            "{wide:?}"
        );
    }

    /// Escapes in an answer are text: an agent's file never drives the terminal.
    #[test]
    fn escapes_in_an_answer_are_never_painted() {
        for row in render("abc\x1b[31mred\x07 \u{9b}2J done", 30) {
            let body = row.replace(&theme::fg(Role::Text), "");
            assert!(!plain(&row).contains('\x1b') && !plain(&row).contains('\u{9b}'));
            assert!(!body.contains("\x1b[31m"), "{row:?}");
        }
    }

    /// The page lays out once per width, keeps its scroll inside the page, lets the wheel
    /// scroll, and hands the prefix and a click on to the ordinary path.
    #[test]
    fn the_page_lays_out_once_per_width_and_hands_on_what_is_not_its_own() {
        let mut answer = Answer::new("claude".into(), "one\n\ntwo\n\nthree".into());
        assert_eq!(answer.layout(20, 2).len(), 5);
        answer.offset = 99;
        answer.layout(20, 2);
        assert_eq!(answer.offset, 3, "the scroll stays inside the page");
        assert_eq!(answer.key(b"\x1b[<64;5;5M", 10), Key::Scrolled);
        assert_eq!(answer.key(b"jjj", 10), Key::Scrolled);
        assert_eq!(answer.key(b"\x01d", 10), Key::Pass);
        assert_eq!(answer.key(b"\x1b[<0;5;5M", 10), Key::Pass);
    }
}

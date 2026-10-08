//! A terminal's screen as data for something that draws it outside a terminal: the notch,
//! or any other viewer of the same grid.
//!
//! A snapshot row is a grid, one character per cell, with a spacer space after a character
//! two cells wide (see `qoop_tui::width`). So a span's cells are the row's char indices,
//! and the spacer is dropped on the way out, the way the interface paints it. Indexed
//! colours are turned into hex here, so the reader needs no palette of its own.

use qoop_runtime::daemon::{ColourWire, SnapshotWire, SpanWire};
use serde_json::{Map, Value, json};

/// The colour xterm gives an index: its sixteen defaults, then the shared cube and ramp.
fn indexed_hex(i: u8) -> String {
    const BASE: [&str; 16] = [
        "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
        "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    match BASE.get(usize::from(i)) {
        Some(hex) => (*hex).to_string(),
        None => qoop_runtime::avatar::ink_hex(i),
    }
}

fn hex(colour: &ColourWire) -> String {
    match colour {
        ColourWire::Indexed { i } => indexed_hex(*i),
        ColourWire::Rgb { r, g, b } => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

/// The style keys of one run: colours as hex, and the attributes a reader can draw.
fn style(span: Option<&SpanWire>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(span) = span else {
        return out;
    };
    if let Some(fg) = &span.fg {
        out.insert("fg".into(), hex(fg).into());
    }
    if let Some(bg) = &span.bg {
        out.insert("bg".into(), hex(bg).into());
    }
    // Bold, italic, underline, inverse, dim, strikethrough: bits 0 to 5 (SpanWire).
    for (bit, key) in [(0, "b"), (1, "i"), (2, "u"), (3, "v"), (4, "d"), (5, "s")] {
        if span.attrs & (1 << bit) != 0 {
            out.insert(key.into(), true.into());
        }
    }
    out
}

/// One grid row as runs of text sharing a style: `[{"t": "...", "fg": "#rrggbb", ...}]`.
pub fn runs(row: &str, spans: &[SpanWire]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut current: Option<usize> = None;
    let mut skip_spacer = false;
    for (cell, c) in row.chars().enumerate() {
        if skip_spacer && c == ' ' {
            skip_spacer = false;
            continue;
        }
        skip_spacer = qoop_tui::width::columns(c) == 2;
        // Never a control character or a bidi override: this text is drawn by something
        // else, and either would be a command to it rather than a letter.
        if !qoop_tui::width::drawn(c) {
            continue;
        }
        let at = spans
            .iter()
            .position(|s| cell >= s.at && cell < s.at + s.len);
        if at != current && !text.is_empty() {
            let mut run = style(current.map(|i| &spans[i]));
            run.insert("t".into(), std::mem::take(&mut text).into());
            out.push(Value::Object(run));
        }
        current = at;
        text.push(c);
    }
    if !text.is_empty() {
        let mut run = style(current.map(|i| &spans[i]));
        run.insert("t".into(), text.into());
        out.push(Value::Object(run));
    }
    out
}

/// A whole screen: its width, where the cursor is, how much history lies behind it, and
/// every row as runs.
pub fn screen(snapshot: &SnapshotWire) -> Value {
    let rows: Vec<Value> = snapshot
        .lines
        .iter()
        .enumerate()
        .map(|(y, row)| {
            let spans = snapshot.styles.get(y).map(Vec::as_slice).unwrap_or(&[]);
            Value::Array(runs(row, spans))
        })
        .collect();
    json!({
        "cols": snapshot.cols,
        "cursor": [snapshot.cursor.0, snapshot.cursor.1],
        "behind": snapshot.behind,
        // A program on the alternate screen keeps its own history, which never scrolls
        // into the terminal's: a reader should not offer to scroll into what is not there.
        "alt": snapshot.modes.alt_screen,
        "rows": rows,
    })
}

/// Rows of history, which are stored as text: runs with no style.
pub fn history(from: u64, total: u64, lines: &[String]) -> Value {
    let rows: Vec<Value> = lines.iter().map(|l| Value::Array(runs(l, &[]))).collect();
    json!({ "from": from, "total": total, "rows": rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(at: usize, len: usize, fg: Option<ColourWire>, attrs: u8) -> SpanWire {
        SpanWire {
            at,
            len,
            fg,
            bg: None,
            attrs,
            link: None,
        }
    }

    #[test]
    fn a_row_splits_where_its_style_changes() {
        let row = "ok build";
        let spans = [span(0, 2, Some(ColourWire::Indexed { i: 2 }), 1)];
        let got = runs(row, &spans);
        assert_eq!(
            got,
            vec![
                json!({"t": "ok", "fg": "#00cd00", "b": true}),
                json!({"t": " build"}),
            ]
        );
    }

    /// The spacer after a wide character is the grid's, not the program's: dropped, and
    /// the cells after it still line up with their spans.
    #[test]
    fn the_spacer_after_a_wide_character_is_dropped() {
        let row = "中 x";
        let spans = [span(2, 1, Some(ColourWire::Rgb { r: 1, g: 2, b: 3 }), 0)];
        assert_eq!(
            runs(row, &spans),
            vec![json!({"t": "中"}), json!({"t": "x", "fg": "#010203"})]
        );
    }

    #[test]
    fn indexed_colours_follow_xterm() {
        assert_eq!(indexed_hex(1), "#cd0000");
        assert_eq!(indexed_hex(15), "#ffffff");
        assert_eq!(indexed_hex(33), "#0087ff");
    }

    #[test]
    fn control_characters_never_leave() {
        assert_eq!(runs("a\u{1b}b\u{202e}c", &[]), vec![json!({"t": "abc"})]);
    }
}

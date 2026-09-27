//! Turning the daemon's grid into what a terminal will actually draw.
//!
//! A snapshot row is a grid: one character per cell, and a character that occupies two
//! cells is followed by a space holding the second one. That is the right shape for a grid
//! and the wrong shape for a screen. Painted literally, the wide character draws two
//! columns and the space draws a third, so every wide character on a row pushes what
//! follows it one column right. Measured before this module existed: `中文中END` in a
//! pane starting at column 21 put `END` at column 30 instead of 27, and a row of Chinese
//! ran past the right edge of the window and wrapped, which tears the layout apart.
//!
//! So the spacer is dropped on the way out, and everything here counts columns rather than
//! characters.

use unicode_width::UnicodeWidthChar;

/// How many columns a character takes on screen.
///
/// A control character has no width here because a snapshot row does not contain one: the
/// engine has already interpreted them, and `unicode_width` answers `None` for them.
pub fn columns(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// The columns a string will take, once painted.
pub fn width(s: &str) -> usize {
    s.chars().map(columns).sum()
}

/// One grid row as the characters to paint, with the spacer after each wide character
/// dropped.
///
/// Only a space is dropped, and only directly after a character that is two columns wide.
/// A space that a program really printed after a wide character is indistinguishable from
/// the spacer in the row text, and the grid's own arithmetic is the tiebreaker: the engine
/// put the character in two cells, so the cell after it is not the program's.
fn painted(row: &str) -> impl Iterator<Item = char> + '_ {
    let mut skip_spacer = false;
    row.chars().filter(move |c| {
        // A control character is never text here. The interface paints names and
        // messages that can come from another machine, and an escape in one of those
        // would reach this terminal as a command rather than as a name.
        if c.is_control() {
            return false;
        }
        if skip_spacer && *c == ' ' {
            skip_spacer = false;
            return false;
        }
        skip_spacer = columns(*c) == 2;
        true
    })
}

/// A grid row, painted into exactly `cols` columns.
///
/// Cut on a character, never inside one, and never leaving half of a wide character at the
/// edge: a two column character with one column left is dropped and the column is padded,
/// which is what every terminal does with the same situation.
pub fn fit(row: &str, cols: usize) -> String {
    let mut out = String::with_capacity(cols);
    let mut used = 0;
    for c in painted(row) {
        let w = columns(c);
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    for _ in used..cols {
        out.push(' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text from another machine can carry an escape. It is dropped, never drawn: an OSC 52
    /// in a session name would otherwise write to this terminal's clipboard.
    #[test]
    fn a_control_character_is_never_painted() {
        let out = fit("a\x1b]52;c;aGk=\x07b\r", 12);
        assert!(!out.chars().any(char::is_control), "{out:?}");
        assert!(out.starts_with("a]52;c;aGk=b"), "{out:?}");
    }

    #[test]
    fn a_row_of_ascii_is_unchanged_and_padded() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(width(&fit("abc", 5)), 5);
    }

    #[test]
    fn the_spacer_after_a_wide_character_is_not_painted() {
        // The grid says `中` then a spacer then `X`, which is three cells. On screen that
        // has to be `中X`, which is also three columns.
        assert_eq!(fit("中 X", 3), "中X");
        assert_eq!(width(&fit("中 X", 3)), 3);
    }

    #[test]
    fn a_space_the_program_printed_survives() {
        // No wide character before it, so it is not a spacer.
        assert_eq!(fit("a b", 3), "a b");
    }

    #[test]
    fn a_wide_character_that_does_not_fit_is_dropped_rather_than_halved() {
        // Two columns wanted, one left: painting it would run into the next pane.
        let out = fit("A中", 2);
        assert_eq!(out, "A ");
        assert_eq!(width(&out), 2);
    }

    #[test]
    fn every_fitted_row_is_exactly_the_width_asked_for() {
        for row in ["", "x", "中文中文", "şğüöç", "🌍🌍🌍", "中 文 中 END"] {
            for cols in [0usize, 1, 3, 8, 20] {
                assert_eq!(width(&fit(row, cols)), cols, "{row:?} at {cols}");
            }
        }
    }

    #[test]
    fn an_emoji_is_two_columns_and_its_spacer_goes_too() {
        assert_eq!(fit("🌍 x", 3), "🌍x");
    }
}

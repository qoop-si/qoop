//! Which slice of the screen a rule reads.
//!
//! Regions exist because "the whole screen" is the wrong default. A terminal holds the
//! transcript of everything the agent already did, so a permission prompt that was answered
//! ten minutes ago is still on screen and still matches a blocker regex. Naming a region is
//! how a rule says "the live prompt", and it is the difference between reading the present
//! and reading the past.

/// A named slice of the screen, resolved at parse time so an unknown name is a manifest
/// error rather than a rule that silently never matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// Every line the engine is allowed to look at.
    WholeScreen,
    /// The last N lines, blank ones included. Blank lines carry layout information, so a
    /// rule that cares about the shape of a box wants these.
    BottomLines(usize),
    /// The last N lines that have content, in screen order. A terminal pads the bottom of
    /// the viewport with blanks, so counting raw lines from the bottom usually counts
    /// padding.
    BottomNonEmptyLines(usize),
    /// The first N lines that have content, in screen order. Startup banners and trust
    /// dialogs live at the top.
    TopNonEmptyLines(usize),
    /// Everything after the last line that looks like a shell prompt.
    AfterLastPromptMarker,
}

impl Region {
    /// Parse a region expression such as `bottom_non_empty_lines(12)`.
    ///
    /// Returns `None` for an unknown name or a malformed argument, which the caller turns
    /// into a parse error naming the rule.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (name, arg) = match text.split_once('(') {
            Some((name, rest)) => {
                let inner = rest.strip_suffix(')')?;
                (name.trim(), Some(inner.trim().parse::<usize>().ok()?))
            }
            None => (text, None),
        };
        match (name, arg) {
            ("whole_screen", None) => Some(Self::WholeScreen),
            ("bottom_lines", Some(n)) => Some(Self::BottomLines(n)),
            ("bottom_non_empty_lines", Some(n)) => Some(Self::BottomNonEmptyLines(n)),
            ("top_non_empty_lines", Some(n)) => Some(Self::TopNonEmptyLines(n)),
            ("after_last_prompt_marker", None) => Some(Self::AfterLastPromptMarker),
            _ => None,
        }
    }

    /// Materialise this region out of the (already capped) screen.
    pub fn view<'a>(self, screen: &'a [String]) -> RegionView<'a> {
        let lines: Vec<&'a str> = match self {
            Self::WholeScreen => screen.iter().map(String::as_str).collect(),
            Self::BottomLines(n) => screen[screen.len().saturating_sub(n)..]
                .iter()
                .map(String::as_str)
                .collect(),
            Self::BottomNonEmptyLines(n) => {
                let mut kept: Vec<&'a str> = screen
                    .iter()
                    .rev()
                    .map(String::as_str)
                    .filter(|line| !line.trim().is_empty())
                    .take(n)
                    .collect();
                // Collected from the bottom up, handed back in screen order, because a
                // multi line regex written against a dialog reads top to bottom.
                kept.reverse();
                kept
            }
            Self::TopNonEmptyLines(n) => screen
                .iter()
                .map(String::as_str)
                .filter(|line| !line.trim().is_empty())
                .take(n)
                .collect(),
            Self::AfterLastPromptMarker => {
                let start = screen
                    .iter()
                    .rposition(|line| is_prompt_marker(line))
                    .map_or(0, |i| i + 1);
                screen[start..].iter().map(String::as_str).collect()
            }
        };
        RegionView::new(lines)
    }
}

/// Whether a line looks like a shell prompt that is waiting for input.
///
/// The definition: strip trailing whitespace, then the line counts as a prompt when it is
/// not blank and its last character is `$`, `#`, `%` or `>`. Those are the four sigils
/// `sh`, `bash`, `zsh` and `fish` end a prompt with, and stripping trailing whitespace is
/// what turns the rendered `~/qoro $ ` into something matchable.
///
/// Two consequences are deliberate. A line where a command was typed (`~/qoro $ cargo test`)
/// is not a marker, because the text after the sigil is a command and its output belongs to
/// that command, not to a fresh prompt. And a line of program output that happens to end in
/// `>` (a fragment of HTML, say) is a false marker; the cost is a region that starts lower
/// than it should, which loses context rather than inventing a state, and every rule that
/// reads this region is a weak screen heuristic anyway.
fn is_prompt_marker(line: &str) -> bool {
    let trimmed = line.trim_end();
    matches!(trimmed.chars().next_back(), Some('$' | '#' | '%' | '>'))
}

/// One region, materialised once per rule so the leaf tests in a nested gate share the work.
pub struct RegionView<'a> {
    /// The region's lines in screen order, for `line_regex`.
    pub lines: Vec<&'a str>,
    /// The same lines joined with newlines, for a whole region `regex`.
    pub text: String,
    /// `text` lowercased, because `contains` is case insensitive and lowercasing the needle
    /// at parse time only helps if the haystack is lowercased too.
    pub lower: String,
}

impl<'a> RegionView<'a> {
    fn new(lines: Vec<&'a str>) -> Self {
        let text = lines.join("\n");
        let lower = text.to_lowercase();
        Self { lines, text, lower }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_unknown_region_name_does_not_parse() {
        assert_eq!(Region::parse("prompt_box_body"), None);
        assert_eq!(Region::parse("bottom_lines"), None);
        assert_eq!(Region::parse("bottom_lines(x)"), None);
        assert_eq!(Region::parse("whole_screen(3)"), None);
    }

    #[test]
    fn whole_screen_is_every_line() {
        let s = screen(&["a", "", "b"]);
        assert_eq!(Region::WholeScreen.view(&s).lines, vec!["a", "", "b"]);
    }

    #[test]
    fn bottom_lines_keeps_the_blank_padding() {
        let s = screen(&["a", "b", "", ""]);
        assert_eq!(Region::BottomLines(3).view(&s).lines, vec!["b", "", ""]);
    }

    #[test]
    fn bottom_non_empty_lines_skips_the_padding_and_keeps_screen_order() {
        let s = screen(&["a", "b", "", "c", "   ", ""]);
        assert_eq!(
            Region::BottomNonEmptyLines(2).view(&s).lines,
            vec!["b", "c"]
        );
    }

    #[test]
    fn top_non_empty_lines_skips_leading_blanks() {
        let s = screen(&["", "a", "", "b", "c"]);
        assert_eq!(Region::TopNonEmptyLines(2).view(&s).lines, vec!["a", "b"]);
    }

    #[test]
    fn after_last_prompt_marker_takes_what_is_below_the_last_prompt() {
        let s = screen(&[
            "user@host ~/qoro $ cargo test",
            "test result: ok",
            "user@host ~/qoro $ ",
            "Allow command?",
        ]);
        assert_eq!(
            Region::AfterLastPromptMarker.view(&s).lines,
            vec!["Allow command?"]
        );
    }

    #[test]
    fn after_last_prompt_marker_falls_back_to_the_whole_screen_with_no_marker() {
        // An agent launched straight into a pane never renders a shell prompt. Returning an
        // empty region there would quietly disable every rule that reads it, which is worse
        // than reading a little too much.
        let s = screen(&["Allow command?", "  1. Yes"]);
        assert_eq!(
            Region::AfterLastPromptMarker.view(&s).lines,
            vec!["Allow command?", "  1. Yes"]
        );
    }

    #[test]
    fn a_region_view_carries_a_lowercased_copy_for_contains() {
        let s = screen(&["Do You Want To Proceed?"]);
        assert_eq!(
            Region::WholeScreen.view(&s).lower,
            "do you want to proceed?"
        );
    }
}

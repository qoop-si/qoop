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
    /// The lines inside the live prompt box: below the second horizontal rule from the
    /// bottom and above the next one. Newer agents draw their composer between two rules
    /// (Claude Code among them), and what is typed there is the present, while everything
    /// above the box is transcript. No box, no lines.
    PromptBoxBody,
    /// The one line with content directly above the prompt box, which is where a status
    /// line sits while the box waits below it. Without a box it is the last line with
    /// content on the screen.
    LastNonEmptyAbovePromptBox,
    /// Everything below the last horizontal rule. A dialog that replaces the composer is
    /// drawn under a rule of its own, so this is the dialog and nothing of the transcript
    /// above it: the same words in an answer the agent gave earlier sit above a rule and
    /// are never read. With no rule on screen it is the whole screen.
    AfterLastHorizontalRule,
    /// The latest OSC 0/2 window title the child set, as one line.
    ///
    /// Read from a side channel rather than the screen: titles are never drawn
    /// as text, and matching them against scrollback would read history as the
    /// present. There is deliberately no `osc_progress` sibling: nothing
    /// produces OSC 9 in this engine, and a region that can never match is a
    /// rule that never fires and never complains. It stays an unknown region
    /// until a producer exists.
    OscTitle,
}

/// What the screen-adjacent side channels hold, for regions that do not read
/// the screen at all.
pub struct OscContext<'a> {
    /// The latest window title, or empty when none was set.
    pub title: &'a str,
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
            // Another name for the visible screen: the same lines this engine is handed.
            ("whole_recent", None) => Some(Self::WholeScreen),
            ("prompt_box_body", None) => Some(Self::PromptBoxBody),
            ("last_non_empty_above_prompt_box", None) => Some(Self::LastNonEmptyAbovePromptBox),
            ("after_last_horizontal_rule", None) => Some(Self::AfterLastHorizontalRule),
            ("bottom_lines", Some(n)) => Some(Self::BottomLines(n)),
            ("bottom_non_empty_lines", Some(n)) => Some(Self::BottomNonEmptyLines(n)),
            ("top_non_empty_lines", Some(n)) => Some(Self::TopNonEmptyLines(n)),
            ("after_last_prompt_marker", None) => Some(Self::AfterLastPromptMarker),
            ("osc_title", None) => Some(Self::OscTitle),
            _ => None,
        }
    }

    /// Materialise this region out of the (already capped) screen.
    ///
    /// `needs_lower` decides whether the lowercased copy is built. The caller
    /// passes `Gate::needs_lower`: only `contains` leaves read it, so a
    /// regex-only rule skips a whole second string allocation per evaluation.
    pub fn view<'a>(
        self,
        screen: &'a [String],
        osc: &OscContext<'a>,
        needs_lower: bool,
    ) -> RegionView<'a> {
        let lines: Vec<&'a str> = match self {
            // The side channel, never the screen: a title is not drawn as text,
            // and an empty title is no lines rather than a blank one, so a
            // `contains` leaf cannot match what was never set.
            Self::OscTitle => {
                if osc.title.is_empty() {
                    Vec::new()
                } else {
                    vec![osc.title]
                }
            }
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
            Self::PromptBoxBody => match prompt_box_top(screen) {
                Some(top) => screen[top + 1..]
                    .iter()
                    .map(String::as_str)
                    .take_while(|line| !is_horizontal_rule(line))
                    .collect(),
                None => Vec::new(),
            },
            Self::LastNonEmptyAbovePromptBox => {
                let above = &screen[..prompt_box_top(screen).unwrap_or(screen.len())];
                above
                    .iter()
                    .rev()
                    .map(String::as_str)
                    .find(|line| !line.trim().is_empty())
                    .into_iter()
                    .collect()
            }
            Self::AfterLastHorizontalRule => {
                let start = screen
                    .iter()
                    .rposition(|line| is_horizontal_rule(line))
                    .map_or(0, |i| i + 1);
                screen[start..].iter().map(String::as_str).collect()
            }
        };
        RegionView::new(lines, needs_lower)
    }
}

/// Whether a line looks like a shell prompt that is waiting for input.
///
/// The definition: strip trailing whitespace, then the line counts as a prompt when it is
/// not blank and its last character is `$`, `#`, `%` or `>`. Those are the four sigils
/// `sh`, `bash`, `zsh` and `fish` end a prompt with, and stripping trailing whitespace is
/// what turns the rendered `~/qoop $ ` into something matchable.
///
/// Two consequences are deliberate. A line where a command was typed (`~/qoop $ cargo test`)
/// is not a marker, because the text after the sigil is a command and its output belongs to
/// that command, not to a fresh prompt. And a line of program output that happens to end in
/// `>` (a fragment of HTML, say) is a false marker; the cost is a region that starts lower
/// than it should, which loses context rather than inventing a state, and every rule that
/// reads this region is a weak screen heuristic anyway.
fn is_prompt_marker(line: &str) -> bool {
    let trimmed = line.trim_end();
    matches!(trimmed.chars().next_back(), Some('$' | '#' | '%' | '>'))
}

/// The index of the prompt box's top border: the second horizontal rule from the bottom.
fn prompt_box_top(screen: &[String]) -> Option<usize> {
    screen
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| is_horizontal_rule(line))
        .nth(1)
        .map(|(i, _)| i)
}

/// Whether a line is a horizontal rule: it starts with `─` once trimmed, and is either
/// nothing but that or carries a label after at least three of them (Claude Code writes a
/// label into its rule, `──── WINDOWS ─`). A box border starts with a corner, not a
/// rule, so a box-drawn composer is not mistaken for one.
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    let dashes = trimmed.chars().take_while(|&c| c == '─').count();
    if dashes == 0 {
        return false;
    }
    let rest = trimmed.trim_start_matches('─').trim_start();
    rest.is_empty() || dashes >= 3
}

/// One region, materialised once per rule so the leaf tests in a nested gate share the work.
pub struct RegionView<'a> {
    /// The region's lines in screen order, for `line_regex`.
    pub lines: Vec<&'a str>,
    /// The same lines joined with newlines, for a whole region `regex`.
    pub text: String,
    /// `text` lowercased, for case-insensitive `contains`.
    ///
    /// Built only when the caller asked for it, because `contains` is the
    /// only leaf that reads it. When the gate has no `contains` leaf this is
    /// empty rather than a second allocated copy of `text`.
    pub lower: String,
}

impl<'a> RegionView<'a> {
    fn new(lines: Vec<&'a str>, needs_lower: bool) -> Self {
        let text = lines.join("\n");
        let lower = if needs_lower {
            text.to_lowercase()
        } else {
            String::new()
        };
        Self { lines, text, lower }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| (*s).to_string()).collect()
    }

    fn no_osc() -> OscContext<'static> {
        OscContext { title: "" }
    }

    #[test]
    fn an_unknown_region_name_does_not_parse() {
        assert_eq!(Region::parse("prompt_box_body(3)"), None);
        assert_eq!(Region::parse("osc_progress"), None);
        assert_eq!(Region::parse("above_prompt_box"), None);
        assert_eq!(Region::parse("bottom_lines"), None);
        assert_eq!(Region::parse("bottom_lines(x)"), None);
        assert_eq!(Region::parse("whole_screen(3)"), None);
        assert_eq!(Region::parse("osc_title(3)"), None);
    }

    #[test]
    fn osc_title_parses_and_reads_the_side_channel_instead_of_the_screen() {
        assert_eq!(Region::parse("osc_title"), Some(Region::OscTitle));
        let s = screen(&["Plugin confirmation needed", "other lines"]);
        let osc = OscContext {
            title: "working title",
        };
        assert_eq!(
            Region::OscTitle.view(&s, &osc, true).lines,
            vec!["working title"]
        );
    }

    #[test]
    fn an_unset_title_is_no_lines_so_no_contains_leaf_can_match_it() {
        let s = screen(&["anything"]);
        assert!(Region::OscTitle.view(&s, &no_osc(), true).lines.is_empty());
    }

    #[test]
    fn whole_screen_is_every_line() {
        let s = screen(&["a", "", "b"]);
        assert_eq!(
            Region::WholeScreen.view(&s, &no_osc(), true).lines,
            vec!["a", "", "b"]
        );
    }

    #[test]
    fn bottom_lines_keeps_the_blank_padding() {
        let s = screen(&["a", "b", "", ""]);
        assert_eq!(
            Region::BottomLines(3).view(&s, &no_osc(), true).lines,
            vec!["b", "", ""]
        );
    }

    #[test]
    fn bottom_non_empty_lines_skips_the_padding_and_keeps_screen_order() {
        let s = screen(&["a", "b", "", "c", "   ", ""]);
        assert_eq!(
            Region::BottomNonEmptyLines(2)
                .view(&s, &no_osc(), true)
                .lines,
            vec!["b", "c"]
        );
    }

    #[test]
    fn top_non_empty_lines_skips_leading_blanks() {
        let s = screen(&["", "a", "", "b", "c"]);
        assert_eq!(
            Region::TopNonEmptyLines(2).view(&s, &no_osc(), true).lines,
            vec!["a", "b"]
        );
    }

    #[test]
    fn after_last_prompt_marker_takes_what_is_below_the_last_prompt() {
        let s = screen(&[
            "user@host ~/qoop $ cargo test",
            "test result: ok",
            "user@host ~/qoop $ ",
            "Allow command?",
        ]);
        assert_eq!(
            Region::AfterLastPromptMarker
                .view(&s, &no_osc(), true)
                .lines,
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
            Region::AfterLastPromptMarker
                .view(&s, &no_osc(), true)
                .lines,
            vec!["Allow command?", "  1. Yes"]
        );
    }

    #[test]
    fn a_region_view_carries_a_lowercased_copy_for_contains() {
        let s = screen(&["Do You Want To Proceed?"]);
        assert_eq!(
            Region::WholeScreen.view(&s, &no_osc(), true).lower,
            "do you want to proceed?"
        );
    }

    #[test]
    fn a_view_that_nothing_lowercases_skips_the_second_string() {
        // `contains` is the only leaf that reads `lower`. A gate without one
        // must not pay for a copy of the whole region text on every rule.
        let s = screen(&["Do You Want To Proceed?"]);
        let view = Region::WholeScreen.view(&s, &no_osc(), false);
        assert_eq!(view.text, "Do You Want To Proceed?");
        assert!(
            view.lower.is_empty(),
            "no contains leaf, so no lowercase copy: {:?}",
            view.lower
        );
    }

    const RULE: &str = "────────────────────────────────";

    /// The layout Claude Code draws: transcript, a status line, the composer between two
    /// rules, then a footer.
    fn boxed() -> Vec<String> {
        screen(&[
            "● an earlier answer",
            "",
            "✻ Waiting for 2 background agents to finish",
            "",
            "──────────────────────────────── WINDOWS ─",
            "❯ half typed",
            RULE,
            "  ? for shortcuts",
        ])
    }

    #[test]
    fn the_new_region_names_parse() {
        assert_eq!(Region::parse("whole_recent"), Some(Region::WholeScreen));
        assert_eq!(
            Region::parse("prompt_box_body"),
            Some(Region::PromptBoxBody)
        );
        assert_eq!(
            Region::parse("last_non_empty_above_prompt_box"),
            Some(Region::LastNonEmptyAbovePromptBox)
        );
        assert_eq!(
            Region::parse("after_last_horizontal_rule"),
            Some(Region::AfterLastHorizontalRule)
        );
    }

    #[test]
    fn prompt_box_body_is_what_sits_between_the_last_two_rules() {
        let s = boxed();
        assert_eq!(
            Region::PromptBoxBody.view(&s, &no_osc(), true).lines,
            vec!["❯ half typed"]
        );
        // One rule is not a box.
        let one = screen(&["text", RULE, "❯"]);
        assert!(
            Region::PromptBoxBody
                .view(&one, &no_osc(), true)
                .lines
                .is_empty()
        );
    }

    #[test]
    fn last_non_empty_above_prompt_box_is_the_status_line_over_the_box() {
        let s = boxed();
        assert_eq!(
            Region::LastNonEmptyAbovePromptBox
                .view(&s, &no_osc(), true)
                .lines,
            vec!["✻ Waiting for 2 background agents to finish"]
        );
        // With no box, the last line with content on the screen.
        let flat = screen(&["a", "b", "", ""]);
        assert_eq!(
            Region::LastNonEmptyAbovePromptBox
                .view(&flat, &no_osc(), true)
                .lines,
            vec!["b"]
        );
    }

    #[test]
    fn after_last_horizontal_rule_is_below_the_last_rule_or_everything() {
        let s = boxed();
        assert_eq!(
            Region::AfterLastHorizontalRule
                .view(&s, &no_osc(), true)
                .lines,
            vec!["  ? for shortcuts"]
        );
        let none = screen(&["Do you want to proceed?", "❯ 1. Yes"]);
        assert_eq!(
            Region::AfterLastHorizontalRule
                .view(&none, &no_osc(), true)
                .lines,
            vec!["Do you want to proceed?", "❯ 1. Yes"]
        );
    }

    #[test]
    fn a_box_border_and_a_dash_in_prose_are_not_rules() {
        assert!(is_horizontal_rule("  ───  "));
        assert!(is_horizontal_rule("─"));
        assert!(is_horizontal_rule("──── label ─"));
        assert!(!is_horizontal_rule("╭──────╮"));
        assert!(!is_horizontal_rule("── x"));
        assert!(!is_horizontal_rule("text ────"));
        assert!(!is_horizontal_rule(""));
    }
}

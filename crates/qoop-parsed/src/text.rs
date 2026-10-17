//! Caps and wrapper stripping for the text a digest carries.

use crate::redact::redact;

/// Longest text of one item, in bytes, marker included.
pub const TEXT_MAX: usize = 4096;
/// Longest one-line summary, in characters, marker included.
pub const SUMMARY_MAX: usize = 200;

const MARKER: &str = "... [truncated]";

/// How far past the cut the raw text is read before it is redacted: far enough that masking
/// cannot pull a secret's tail into view, short enough that a huge message costs little.
const REDACT_SLACK: usize = 12 * 1024;

/// Harness blocks that open a human's message without being part of it.
const WRAPPERS: [&str; 5] = [
    "system-reminder",
    "local-command-caveat",
    "ide_opened_file",
    "ide_selection",
    "user-prompt-submit-hook",
];

/// Tags a background-task notification is made of.
const NOTIFICATION_TAGS: [&str; 10] = [
    "task-notification",
    "task-id",
    "tool-use-id",
    "output-file",
    "status",
    "summary",
    "result",
    "usage",
    "worktree",
    "system-reminder",
];

/// Redact, then cut to [`TEXT_MAX`] bytes on a character boundary with a marker. The secret
/// goes first so a cut can never leave half of one.
pub fn text(raw: &str) -> String {
    let (head, cut) = cut_bytes(raw, TEXT_MAX + REDACT_SLACK);
    let mut redacted = redact(&head);
    if cut {
        redacted.push_str(MARKER);
    }
    clamp_bytes(&redacted, TEXT_MAX)
}

/// Redact, put the text on one line and cut it to [`SUMMARY_MAX`] characters.
pub fn summary(raw: &str) -> String {
    short(raw, SUMMARY_MAX)
}

/// Redact, put the text on one line and cut it to `max` characters, marker included.
pub fn short(raw: &str, max: usize) -> String {
    let (head, _) = cut_bytes(raw, max * 4 + REDACT_SLACK);
    let line = redact(&head.split_whitespace().collect::<Vec<_>>().join(" "));
    if line.chars().count() <= max {
        return line;
    }
    let keep: String = line.chars().take(max.saturating_sub(3)).collect();
    format!("{keep}...")
}

/// The first line that says something, as one redacted line of at most `max` characters.
pub fn first_line(raw: &str, max: usize) -> String {
    short(
        raw.lines().find(|l| !l.trim().is_empty()).unwrap_or(""),
        max,
    )
}

/// Redact, keep the newlines and cut to `max` characters, with `...` when something was cut.
pub fn preview(raw: &str, max: usize) -> String {
    let (head, pre_cut) = cut_bytes(raw.trim(), max * 4 + REDACT_SLACK);
    let redacted = redact(&head);
    match redacted.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &redacted[..end]),
        None if pre_cut => format!("{redacted}..."),
        None => redacted,
    }
}

/// The sender and the payload of a message another agent sent (a Codex inter-agent message):
/// `Message Type: .. / Task name: .. / Sender: .. / Payload: ..`. `None` for any other text.
pub fn agent_message(raw: &str) -> Option<(String, String)> {
    let (head, payload) = raw
        .trim_start()
        .strip_prefix("Message Type: ")?
        .split_once("Payload:")?;
    let sender = head.lines().find_map(|l| l.strip_prefix("Sender: "))?;
    Some((sender.trim().to_string(), payload.trim().to_string()))
}

/// Redact, then cut to `max` bytes without a marker. True when anything was cut.
pub fn redacted_cut(raw: &str, max: usize) -> (String, bool) {
    let (head, pre_cut) = cut_bytes(raw, max.saturating_add(REDACT_SLACK));
    let (out, cut) = cut_bytes(&redact(&head), max);
    (out, pre_cut || cut)
}

/// Cut to `max` bytes in total, the marker included, on a character boundary.
fn clamp_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(MARKER.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &s[..end])
}

/// Cut to `max` bytes without a marker, on a character boundary. True when something was cut.
pub fn cut_bytes(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// A human's text without the harness blocks the file puts in front of it. Only blocks that
/// open the message are taken: a tag the user typed further in is the user's text.
pub fn strip_wrappers(raw: &str) -> String {
    let mut rest = raw.trim_start();
    'next: loop {
        for tag in WRAPPERS {
            if let Some(len) = leading_block(rest, tag) {
                rest = rest[len..].trim_start();
                continue 'next;
            }
        }
        return rest.trim_end().to_string();
    }
}

/// The slash command a command message holds, as typed: `/model opus`.
pub fn command_line(raw: &str) -> String {
    match leading_inner(raw, "command-name") {
        Some(name) => {
            let args = inner_of(raw, "command-args").unwrap_or_default();
            format!("{} {}", name.trim(), args.trim())
                .trim()
                .to_string()
        }
        None => strip_wrappers(raw),
    }
}

/// What a background-task notification says: its `<summary>`, else its text with the
/// notification's own tags taken out. Any other `<...>` stays: it is text.
pub fn notification_text(raw: &str) -> String {
    if let Some(summary) = inner_of(raw, "summary") {
        return summary;
    }
    let mut out = raw.to_string();
    for tag in NOTIFICATION_TAGS {
        out = out
            .replace(&format!("<{tag}>"), " ")
            .replace(&format!("</{tag}>"), " ");
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text inside a leading `<tag>...</tag>`, or the text without leading wrappers.
pub fn unwrap_leading(raw: &str, tag: &str) -> String {
    leading_inner(raw, tag).unwrap_or_else(|| strip_wrappers(raw))
}

fn leading_inner(raw: &str, tag: &str) -> Option<String> {
    let rest = raw.trim_start();
    let len = leading_block(rest, tag)?;
    inner_of(&rest[..len], tag)
}

/// Length of `<tag>...</tag>` when `s` starts with it.
fn leading_block(s: &str, tag: &str) -> Option<usize> {
    let range = block(s, tag)?;
    (range.start == 0).then_some(range.end)
}

fn inner_of(raw: &str, tag: &str) -> Option<String> {
    let range = block(raw, tag)?;
    let open = tag.len() + 2;
    let close = tag.len() + 3;
    Some(
        raw[range.start + open..range.end - close]
            .trim()
            .to_string(),
    )
}

/// Where `<tag>...</tag>` sits in `s`, the tags included.
fn block(s: &str, tag: &str) -> Option<std::ops::Range<usize>> {
    let start = s.find(&format!("<{tag}>"))?;
    let end_tag = format!("</{tag}>");
    let end = start + s[start..].find(&end_tag)? + end_tag.len();
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_on_a_char_boundary_with_a_marker() {
        let long = "\u{e9}".repeat(5000);
        let out = text(&long);
        assert!(out.len() <= TEXT_MAX);
        assert!(out.ends_with(MARKER));
        assert_eq!(text("short"), "short");
        let huge = "word ".repeat(100_000);
        let out = text(&huge);
        assert!(out.len() <= TEXT_MAX && out.ends_with(MARKER));
    }

    #[test]
    fn a_summary_is_one_line_of_at_most_200_chars() {
        let out = summary(&format!("line one\n  line two {}", "x".repeat(400)));
        assert_eq!(out.chars().count(), SUMMARY_MAX);
        assert!(out.starts_with("line one line two "));
        assert!(!out.contains('\n'));
    }

    #[test]
    fn only_leading_wrappers_are_stripped() {
        let reminder = "<system-reminder>noise</system-reminder>";
        assert_eq!(
            strip_wrappers(&format!("{reminder}\n{reminder} hello")),
            "hello"
        );
        let typed = format!("explain {reminder} to me");
        assert_eq!(
            strip_wrappers(&typed),
            typed,
            "a tag the user typed is the user's text"
        );
        assert_eq!(
            strip_wrappers("<system-reminder>unclosed"),
            "<system-reminder>unclosed"
        );
    }

    #[test]
    fn commands_and_notifications_are_unwrapped() {
        let cmd = "<command-name>/model</command-name>\n  <command-message>model</command-message>\n  <command-args>opus</command-args>";
        assert_eq!(command_line(cmd), "/model opus");
        assert_eq!(
            unwrap_leading(
                "<local-command-stdout>Set model</local-command-stdout>",
                "local-command-stdout"
            ),
            "Set model"
        );
        assert_eq!(
            notification_text(
                "<task-notification><status>done</status><result>a < b > c</result></task-notification>"
            ),
            "done a < b > c"
        );
        assert_eq!(
            notification_text("<task-notification><summary>All good</summary></task-notification>"),
            "All good"
        );
    }
}

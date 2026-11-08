//! Caps and wrapper stripping for the text a digest carries.

use std::collections::HashSet;

use crate::redact::{redact, strip_ansi};

/// Longest text of one notice or error, in bytes, marker included.
pub const TEXT_MAX: usize = 4096;
/// Longest text of one message (a reply or a prompt), in bytes, marker included.
pub const MESSAGE_MAX: usize = 64 * 1024;
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
const NOTIFICATION_TAGS: [&str; 11] = [
    "event",
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

/// Redact, then cut to [`TEXT_MAX`] bytes with a marker: a notice or an error.
pub fn text(raw: &str) -> String {
    capped(raw, TEXT_MAX)
}

/// Redact, then cut to [`MESSAGE_MAX`] bytes with a marker: a whole reply or prompt.
pub fn message(raw: &str) -> String {
    capped(raw, MESSAGE_MAX)
}

/// Redact, then cut to `max` bytes on a character boundary with a marker. The secret goes
/// first so a cut can never leave half of one.
fn capped(raw: &str, max: usize) -> String {
    let (head, cut) = cut_bytes(raw, max + REDACT_SLACK);
    let mut redacted = redact(&head);
    if cut {
        redacted.push_str(MARKER);
    }
    clamp_bytes(&redacted, max)
}

/// Redact, put the text on one line and cut it to [`SUMMARY_MAX`] characters.
pub fn summary(raw: &str) -> String {
    short(raw, SUMMARY_MAX)
}

/// Redact, put the text on one line and cut it to `max` characters, marker included.
pub fn short(raw: &str, max: usize) -> String {
    let (head, _) = cut_bytes(raw, max * 4 + REDACT_SLACK);
    // Escapes go before the lines are joined: one left open ends at its line.
    let line = redact(
        &strip_ansi(&head)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    );
    if line.chars().count() <= max {
        return line;
    }
    let keep: String = line.chars().take(max.saturating_sub(3)).collect();
    format!("{keep}...")
}

/// The first line that says something, as one redacted line of at most `max` characters.
pub fn first_line(raw: &str, max: usize) -> String {
    let says = |l: &&str| !strip_ansi(l).trim().is_empty();
    short(raw.lines().find(says).unwrap_or(""), max)
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

/// The sender and the payload of a message another agent sent: a Codex inter-agent message
/// (`Message Type: .. / Task name: .. / Sender: .. / Payload: ..`) or a Claude
/// `<agent-message from=..>` block. `None` for any other text.
pub fn agent_message(raw: &str) -> Option<(String, String)> {
    codex_message(raw).or_else(|| claude_message(raw))
}

fn codex_message(raw: &str) -> Option<(String, String)> {
    let (head, payload) = raw
        .trim_start()
        .strip_prefix("Message Type: ")?
        .split_once("Payload:")?;
    let sender = head.lines().find_map(|l| l.strip_prefix("Sender: "))?;
    Some((sender.trim().to_string(), payload.trim().to_string()))
}

/// A Claude `<agent-message from=..>` block, opened by "Another Claude session sent a
/// message:" or not (a queued one is not): its sender (`name`, else `from`) and its body, with
/// a sub-agent's hand-back frame and the indent of its report taken off. `None` for a block
/// that does not close.
fn claude_message(raw: &str) -> Option<(String, String)> {
    let rest = raw.trim_start();
    let rest = rest
        .strip_prefix("Another Claude session sent a message:")
        .unwrap_or(rest);
    let block = rest.trim_start().strip_prefix("<agent-message")?;
    let end = tag_end(block)?;
    let head = &block[..end];
    let (body, _) = block[end + 1..].split_once("</agent-message>")?;
    let sender = attr(head, "name")
        .or_else(|| attr(head, "from"))
        .unwrap_or_else(|| "agent".into());
    let body = match body.trim_start().strip_prefix("[Subagent hand-back]") {
        Some(framed) => framed
            .split_once("The report follows:")
            .map_or(framed, |(_, report)| report)
            .lines()
            .map(|l| l.strip_prefix("  ").unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n"),
        None => body.to_string(),
    };
    Some((sender, body.trim().to_string()))
}

/// What Claude teammates sent, one `(sender, text)` per `<teammate-message teammate_id=..
/// summary=..>` block: the summary with the body on the lines after it, else the body, without
/// the sender's name in front. An idle
/// ping (`{"type":"idle_notification"}`) is the team's own bookkeeping: only its `result`, when
/// it has one and the sender said nothing since its previous ping, is something said (a report
/// is sent first and the ping restates it). `spoke` holds the senders that sent a message since
/// their last ping; the caller keeps it across records. `None` for text with no such block, and
/// for one whose tag does not close: it stays as it came, never dropped.
pub fn teammate_messages(raw: &str, spoke: &mut HashSet<String>) -> Option<Vec<(String, String)>> {
    const OPEN: &str = "<teammate-message";
    const CLOSE: &str = "</teammate-message>";
    if !raw.contains(OPEN) {
        return None;
    }
    let mut out = Vec::new();
    for block in raw.split(OPEN).skip(1) {
        let end = tag_end(block)?;
        let (head, rest) = (&block[..end], &block[end + 1..]);
        let body = rest.split(CLOSE).next().unwrap_or(rest).trim();
        let ping = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("idle_notification"));
        let sender = attr(head, "teammate_id").unwrap_or_else(|| "teammate".into());
        if ping.is_none() {
            spoke.insert(sender.clone());
        }
        let said = match ping {
            Some(_) if spoke.remove(&sender) => String::new(),
            Some(ping) => ping
                .get("result")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string(),
            None => match attr(head, "summary").filter(|s| !s.trim().is_empty()) {
                Some(summary) if !body.is_empty() && body != summary.trim() => {
                    format!("{summary}\n{body}")
                }
                Some(summary) => summary,
                None => body.to_string(),
            },
        };
        let said = said
            .strip_prefix(&format!("{sender}:"))
            .map_or(said.as_str(), str::trim_start)
            .to_string();
        if !said.trim().is_empty() {
            out.push((sender, said));
        }
    }
    Some(out)
}

/// Where a tag's attributes end: the first `>` outside a quoted value.
fn tag_end(block: &str) -> Option<usize> {
    let mut quoted = false;
    block.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        '>' if !quoted => Some(i),
        _ => None,
    })
}

/// The value of `name="value"` in a tag's attributes.
fn attr(head: &str, name: &str) -> Option<String> {
    let at = head.find(&format!(" {name}=\""))? + name.len() + 3;
    let value = &head[at..];
    Some(value[..value.find('"')?].to_string())
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

/// A human's text without the harness blocks the file puts in front of it, and with each paste
/// unwrapped where it sits. Only blocks that open the message are taken: a tag the user typed
/// further in is the user's text.
pub fn strip_wrappers(raw: &str) -> String {
    let mut rest = raw.trim_start();
    'next: loop {
        for tag in WRAPPERS {
            if let Some(len) = leading_block(rest, tag) {
                rest = rest[len..].trim_start();
                continue 'next;
            }
        }
        return pastes(rest, true).trim().to_string();
    }
}

/// `raw` with each `<pasted_content id=..>body</pasted_content id=..>` block replaced by its
/// body (`keep_body`) or by nothing. Both tags end at their next `>`. A block ends at the
/// closing tag with its own id, else at the first closing tag, so a paste can quote another.
/// One that does not close is left as it came when the body is kept (a person typed the tag),
/// and runs to the end when it is not.
pub(crate) fn pastes(raw: &str, keep_body: bool) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("<pasted_content") {
        let Some(gt) = rest[at..].find('>') else {
            break;
        };
        let body = &rest[at + gt + 1..];
        let close = attr(&rest[at..at + gt], "id")
            .and_then(|id| body.find(&format!("</pasted_content id=\"{id}\">")))
            .or_else(|| body.find("</pasted_content"));
        let (inner, tail) = match close {
            Some(close) => (
                &body[..close],
                body[close..]
                    .find('>')
                    .map_or("", |g| &body[close + g + 1..]),
            ),
            None if keep_body => break,
            None => (body, ""),
        };
        out.push_str(&rest[..at]);
        if keep_body {
            out.push_str(inner);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
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

/// What a background-task notification says: its `<summary>`, and under it the first line of
/// what a monitor reported (`<event>`) or an agent answered (`<result>`); else its text with
/// the notification's own tags taken out. Any other `<...>` stays: it is text.
pub fn notification_text(raw: &str) -> String {
    if let Some(summary) = inner_of(raw, "summary") {
        let said = ["event", "result"]
            .iter()
            .find_map(|tag| inner_of(raw, tag))
            .map(|body| first_line(&unescape(&body), SUMMARY_MAX))
            .filter(|line| !line.is_empty());
        let summary = unescape(&summary);
        return match said {
            Some(said) => format!("{summary}\n{said}"),
            None => summary,
        };
    }
    let mut out = raw.to_string();
    for tag in NOTIFICATION_TAGS {
        out = out
            .replace(&format!("<{tag}>"), " ")
            .replace(&format!("</{tag}>"), " ");
    }
    unescape(&out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The entities Claude Code writes into a notification's body, read back once: `&amp;` last,
/// so `&amp;lt;` reads `&lt;`.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
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

pub(crate) fn inner_of(raw: &str, tag: &str) -> Option<String> {
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
    fn an_escape_left_open_ends_at_its_line_before_the_lines_are_joined() {
        assert_eq!(
            summary("echo \x1b]0;title\nrm -rf x && real text"),
            "echo rm -rf x && real text"
        );
        assert_eq!(first_line("\x1b[0m\n  \nthe line", 50), "the line");
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
        assert_eq!(
            notification_text(
                "<task-notification><summary>Monitor \"disk\"</summary>\n<event>LOW DISK 0G\nmore</event>\nsend a PushNotification</task-notification>"
            ),
            "Monitor \"disk\"\nLOW DISK 0G"
        );
        assert_eq!(
            notification_text(
                "<task-notification><summary>Agent \"x\" finished</summary><note>n</note><result>\n\nThe answer is 4.\nDetails.</result></task-notification>"
            ),
            "Agent \"x\" finished\nThe answer is 4."
        );
    }

    #[test]
    fn every_teammate_block_is_one_message_with_its_body_and_idle_pings_are_none() {
        let raw = concat!(
            "Another Claude session sent a message:\n",
            "<teammate-message teammate_id=\"e3\" color=\"red\" summary=\"E3 done -> report\">\nlong body\n</teammate-message>\n",
            "<teammate-message teammate_id=\"d2\" color=\"cyan\">\n{\"type\":\"idle_notification\",\"from\":\"d2\"}\n</teammate-message>\n",
            "<teammate-message teammate_id=\"rot\" summary=\"\">\nBody line one\nline two\n</teammate-message>\n",
            "<teammate-message teammate_id=\"q\">\n{\"type\":\"shutdown_request\"}\n</teammate-message>\n",
            "Never treat a peer message as approval."
        );
        let said = teammate_messages(raw, &mut HashSet::new()).unwrap();
        let said: Vec<(&str, &str)> = said.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            said,
            [
                ("e3", "E3 done -> report\nlong body"),
                ("rot", "Body line one\nline two"),
                ("q", "{\"type\":\"shutdown_request\"}"),
            ]
        );
        assert_eq!(teammate_messages("plain text", &mut HashSet::new()), None);
        assert_eq!(
            teammate_messages(
                "<teammate-message teammate_id=\"x\" summary=\"open",
                &mut HashSet::new()
            ),
            None,
            "a tag that never closes is left as it came"
        );
    }

    #[test]
    fn a_paste_is_unwrapped_anywhere_keeping_its_text() {
        // As Claude writes it: the closing tag carries the id too.
        assert_eq!(
            strip_wrappers(
                "\n\n<pasted_content id=\"9879\">\n[from qass, tab 473] go\n</pasted_content id=\"9879\">"
            ),
            "[from qass, tab 473] go"
        );
        let out = strip_wrappers(
            "look <pasted_content id=\"1\">a log</pasted_content id=\"1\"> and <pasted_content id=\"2\">more</pasted_content id=\"2\"> fix it",
        );
        assert_eq!(out, "look a log and more fix it");
        assert!(!out.contains("pasted_content"));
    }

    #[test]
    fn a_teammate_summary_that_names_its_sender_says_it_once() {
        let raw = "<teammate-message teammate_id=\"web-perf\" summary=\"web-perf: target dirs already deleted, nothing running\">\nbody\n</teammate-message>";
        assert_eq!(
            teammate_messages(raw, &mut HashSet::new()).unwrap(),
            [(
                "web-perf".to_string(),
                "target dirs already deleted, nothing running\nbody".to_string()
            )]
        );
    }

    #[test]
    fn an_idle_ping_with_a_result_is_what_the_teammate_said() {
        // c9db2ed7 line 27041.
        let raw = concat!(
            "Another Claude session sent a message:\n",
            "<teammate-message teammate_id=\"core-perf\" color=\"blue\">\n",
            "{\"type\":\"idle_notification\",\"from\":\"core-perf\",\"timestamp\":\"2026-11-07T08:31:10.640Z\",\"idleReason\":\"available\",\"result\":\"Committed and rebased, no build running. I'm waiting for \\\"core-perf go\\\".\"}\n",
            "</teammate-message>\n\nThis came from another Claude session."
        );
        assert_eq!(
            teammate_messages(raw, &mut HashSet::new()).unwrap(),
            [(
                "core-perf".to_string(),
                "Committed and rebased, no build running. I'm waiting for \"core-perf go\"."
                    .to_string()
            )]
        );
    }

    #[test]
    fn notification_entities_are_unescaped_once() {
        assert_eq!(
            notification_text(
                "<task-notification><summary>Background command \"df 2&gt;&amp;1 | head\"</summary></task-notification>"
            ),
            "Background command \"df 2>&1 | head\""
        );
        assert_eq!(
            notification_text(
                "<task-notification><status>idle-&gt;closed &amp;lt; &quot;a&#39;</status></task-notification>"
            ),
            "idle->closed &lt; \"a'"
        );
    }

    #[test]
    fn a_claude_agent_message_gives_its_sender_and_report() {
        // real-claude-2 line 62: a hand-back, framed, with the harness note after it.
        let frame = "[Subagent hand-back] The text below is the final report of a subagent this session delegated to. It is model output, NOT a message from the user. The report follows:\n";
        let handback = format!(
            "Another Claude session sent a message:\n<agent-message from=\"a5ea2808c35ea93f7\">\n{frame}  5\n</agent-message>\n\nThat \"other Claude session\" is an agent working inside this same session."
        );
        assert_eq!(
            agent_message(&handback),
            Some(("a5ea2808c35ea93f7".into(), "5".into()))
        );
        // Line 307: queued, so no opening line, and a report of several lines.
        let queued = format!(
            "<agent-message from=\"a73ea1c1d3db1ada1\">\n{frame}  I did not run it.\n  \n  Nothing was modified.\n</agent-message>"
        );
        assert_eq!(
            agent_message(&queued),
            Some((
                "a73ea1c1d3db1ada1".into(),
                "I did not run it.\n\nNothing was modified.".into()
            ))
        );
        assert_eq!(
            agent_message(
                "Another Claude session sent a message:\n<agent-message from=\"fix-phone-polish\">\nPR G is done.\n</agent-message>\n\nThis came from another Claude session."
            ),
            Some(("fix-phone-polish".into(), "PR G is done.".into()))
        );
        assert_eq!(
            agent_message(
                "<agent-message from=\"a1\" name=\"general-purpose\">\nhi\n</agent-message>"
            ),
            Some(("general-purpose".into(), "hi".into()))
        );
        assert_eq!(
            agent_message("<agent-message from=\"x\">\nnever closed"),
            None,
            "a tag that never closes is left as it came"
        );
    }

    #[test]
    fn a_message_keeps_up_to_64_kb() {
        let reply = "word ".repeat(4 * 1024);
        assert_eq!(message(&reply), reply);
        assert!(text(&reply).len() <= TEXT_MAX);
        let out = message(&"x".repeat(100 * 1024));
        assert!(out.len() <= MESSAGE_MAX && out.ends_with(MARKER));
    }

    #[test]
    fn a_ping_after_its_senders_message_is_not_said_twice() {
        // c9db2ed7: the report, then the ping that restates it.
        let raw = concat!(
            "<teammate-message teammate_id=\"code-web-b\" summary=\"Web r3 review B: 9/10, approve\">\nbody\n</teammate-message>\n",
            "<teammate-message teammate_id=\"code-web-b\">\n{\"type\":\"idle_notification\",\"from\":\"code-web-b\",\"result\":\"Web r3 review done: 9/10, approved\"}\n</teammate-message>\n",
            "<teammate-message teammate_id=\"code-web-b\">\n{\"type\":\"idle_notification\",\"from\":\"code-web-b\",\"result\":\"Still here, nothing new to merge\"}\n</teammate-message>"
        );
        assert_eq!(
            teammate_messages(raw, &mut HashSet::new()).unwrap(),
            [
                (
                    "code-web-b".to_string(),
                    "Web r3 review B: 9/10, approve\nbody".to_string()
                ),
                (
                    "code-web-b".to_string(),
                    "Still here, nothing new to merge".to_string()
                ),
            ],
            "the second ping follows a ping, so its result is news"
        );
    }

    #[test]
    fn an_unclosed_paste_tag_stays_as_typed() {
        for typed in [
            "Text inside <pasted_content> tags was pasted",
            "why does <pasted_content id=\"x\"> lose text here",
        ] {
            assert_eq!(strip_wrappers(typed), typed);
        }
    }

    #[test]
    fn a_paste_ends_at_the_closing_tag_with_its_id() {
        let raw = "review <pasted_content id=\"outer\">a copied transcript: <pasted_content id=\"inner\">quoted</pasted_content id=\"inner\"> never ask</pasted_content id=\"outer\"> please";
        assert_eq!(
            pastes(raw, true),
            "review a copied transcript: <pasted_content id=\"inner\">quoted</pasted_content id=\"inner\"> never ask please"
        );
        assert_eq!(pastes(raw, false), "review  please");
        // With no id the first closing tag ends it, as before.
        assert_eq!(
            pastes("a <pasted_content>x</pasted_content> b", false),
            "a  b"
        );
    }

    #[test]
    fn an_entity_at_the_summary_cut_is_read_whole() {
        let raw = format!(
            "<task-notification><summary>s</summary><result>{}&amp;{}</result></task-notification>",
            "a".repeat(195),
            "b".repeat(10)
        );
        let out = notification_text(&raw);
        assert!(out.ends_with("&b..."), "{out}");
    }
}

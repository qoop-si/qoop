//! `qoop messages`: the chat of an agent terminal, parsed by the daemon from the agent's own
//! session file. For scripts and for looking at what the chat views of the interface and the
//! phone are given.

use std::time::Duration;

use qoop_parsed::{Item, Status};
use qoop_runtime::control::{ControlRequest, ControlResponse};

use crate::task::ask;
use crate::usage;

const USAGE: &str = "usage: qoop messages <terminal> [--skip N] [--limit N] [--json]
       qoop messages detail <terminal> <tool_id> [--json]";

/// The longest a row of the text form is, in characters.
const ROW: usize = 120;

pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("detail") => detail(&args[3..]),
        Some(_) => page(&args[2..]),
        None => Err(usage(USAGE)),
    }
}

fn terminal_of(word: Option<&String>) -> std::io::Result<u32> {
    word.and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(USAGE))
}

fn page(args: &[String]) -> std::io::Result<()> {
    let terminal = terminal_of(args.first())?;
    let (mut skip, mut limit, mut json) = (0u32, 0u32, false);
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        let mut number = || -> std::io::Result<u32> {
            rest.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| usage(USAGE))
        };
        match arg.as_str() {
            "--skip" => skip = number()?,
            "--limit" => limit = number()?,
            "--json" => json = true,
            _ => return Err(usage(format!("unknown flag {arg}; {USAGE}"))),
        }
    }
    let request = ControlRequest::Messages {
        id: crate::request_id("messages"),
        terminal,
        skip,
        limit,
        revision: None,
    };
    match ask(request, Duration::from_secs(15))? {
        ControlResponse::Messages {
            revision,
            first_seq,
            has_older,
            items,
            warnings,
            ..
        } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "revision": revision,
                        "first_seq": first_seq,
                        "has_older": has_older,
                        "items": items,
                        "warnings": warnings,
                    })
                );
            } else {
                for item in &items {
                    println!("{}", row(item));
                }
            }
            Ok(())
        }
        other => Err(refused(other)),
    }
}

fn detail(args: &[String]) -> std::io::Result<()> {
    let terminal = terminal_of(args.first())?;
    let tool_id = args.get(1).ok_or_else(|| usage(USAGE))?.clone();
    let mut json = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--json" => json = true,
            _ => return Err(usage(format!("unknown flag {arg}; {USAGE}"))),
        }
    }
    let request = ControlRequest::MessageDetail {
        id: crate::request_id("detail"),
        terminal,
        tool_id,
        max_bytes: None,
    };
    match ask(request, Duration::from_secs(15))? {
        ControlResponse::MessageDetail {
            input,
            output,
            truncated,
            ..
        } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "input": input, "output": output, "truncated": truncated })
                );
            } else {
                println!("{}\n----\n{}", clean(&input), clean(&output));
                if truncated {
                    println!("(cut)");
                }
            }
            Ok(())
        }
        other => Err(refused(other)),
    }
}

/// A refusal as a person reads it: the daemon's reason code, then what it means.
fn refused(response: ControlResponse) -> std::io::Error {
    let ControlResponse::Failed { reason, .. } = response else {
        return std::io::Error::other("unexpected answer to a chat read");
    };
    let meaning = match reason.as_str() {
        "no_session" => "the agent has no session file yet",
        "not_agent" => "no coding agent runs in that terminal",
        "too_large" => "that session is too large for the chat, read the terminal",
        "unavailable" => "no such terminal, or its session did not answer in time",
        _ => return std::io::Error::other(clean(&reason)),
    };
    std::io::Error::other(format!("{reason}: {meaning}"))
}

/// One line of the text form: the sequence number, who spoke, the first line of what was
/// said, and for a tool call how it went.
fn row(item: &Item) -> String {
    let (who, said) = match item {
        Item::User { text, queued, .. } => {
            let queued = if *queued { "[queued] " } else { "" };
            ("you", format!("{queued}{text}"))
        }
        Item::Agent { text, .. } => ("agent", text.clone()),
        Item::Thinking { text, .. } => ("thinking", text.clone().unwrap_or_default()),
        Item::Tool {
            name,
            summary,
            status,
            exit_code,
            ..
        } => {
            let mut line = format!("{name} {summary} [{}", status_word(*status));
            if let Some(code) = exit_code {
                line.push_str(&format!(", exit {code}"));
            }
            line.push(']');
            ("tool", line)
        }
        Item::Subagent {
            summary,
            steps,
            status,
            ..
        } => (
            "subagent",
            format!("[{}] {summary} ({steps} steps)", status_word(*status)),
        ),
        Item::Compaction { text, .. } => ("compaction", text.clone().unwrap_or_default()),
        Item::Error { text, .. } => ("error", text.clone()),
        Item::Notice { kind, text, .. } => ("notice", format!("{kind}: {text}")),
        Item::Unknown => ("notice", "(a kind this qoop does not know)".to_string()),
    };
    let line = said.lines().next().unwrap_or("");
    let line: String = clean(line).chars().take(ROW).collect();
    format!("{:>5}  {who:<10} {line}", item.seq())
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Running => "running",
        Status::Ok => "ok",
        Status::Failed => "failed",
        Status::Interrupted => "interrupted",
        Status::Rejected => "rejected",
        Status::Unknown => "unknown",
    }
}

/// Text as the terminal may be shown it: what an agent, a tool or a file wrote is not to
/// move the cursor, clear the screen, set a title or reorder what is read. Newline and tab
/// stay; every other control character (the C0 set with ESC and BEL, DEL, the C1 set) and the
/// bidirectional controls go. `--json` is not passed through here: it is data.
fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !(c.is_control() && !matches!(c, '\n' | '\t'))
                // Marks, line and paragraph separators and the bidirectional controls, by
                // code point: they draw nothing, so no glyph is claimed here.
                && !matches!(
                    u32::from(*c),
                    0x61c | 0x200e | 0x200f | 0x2028 | 0x2029 | 0x202a..=0x202e | 0x2066..=0x2069
                )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_one_line_with_the_speaker_first_and_a_tool_with_its_status() {
        let user = Item::User {
            seq: 3,
            ts: None,
            text: "first\nsecond".into(),
            attachments: Vec::new(),
            queued: false,
        };
        assert_eq!(row(&user), "    3  you        first");
        let queued = Item::User {
            seq: 3,
            ts: None,
            text: "first\nsecond".into(),
            attachments: Vec::new(),
            queued: true,
        };
        assert_eq!(row(&queued), "    3  you        [queued] first");
        let subagent = Item::Subagent {
            seq: 6,
            summary: "spawned: survey".into(),
            steps: 2,
            status: Status::Running,
        };
        // The word is before the summary: a long one is cut at the end of the line.
        let long = Item::Subagent {
            seq: 7,
            summary: "x".repeat(200),
            steps: 1,
            status: Status::Failed,
        };
        assert!(row(&long).contains("[failed] xxx"), "{}", row(&long));
        assert_eq!(
            row(&subagent),
            "    6  subagent   [running] spawned: survey (2 steps)"
        );
        let tool = Item::Tool {
            seq: 4,
            id: "t".into(),
            name: "Bash".into(),
            summary: "cargo test".into(),
            status: Status::Failed,
            exit_code: Some(101),
            duration_ms: None,
            ts: None,
            output_bytes: 0,
            has_detail: true,
            attachments: Vec::new(),
        };
        assert_eq!(
            row(&tool),
            "    4  tool       Bash cargo test [failed, exit 101]"
        );
        let compaction = Item::Compaction {
            seq: 5,
            text: Some("Goal: ship it".into()),
        };
        assert_eq!(row(&compaction), "    5  compaction Goal: ship it");
    }

    /// ESC [ 2 J, an OSC title ended by BEL, a C1 CSI, a right-to-left override and an isolate,
    /// and DEL, between letters.
    fn hostile() -> String {
        let at = |code: u32| char::from_u32(code).unwrap();
        format!(
            "a{}[2Jb{}]0;owned{}c{}31md{}e{}f{}g",
            at(0x1b),
            at(0x1b),
            at(0x7),
            at(0x9b),
            at(0x202e),
            at(0x2066),
            at(0x7f)
        )
    }

    /// What a hostile transcript writes (clear the screen, set the window title, reorder the
    /// line) never reaches the terminal from the text forms.
    #[test]
    fn control_and_bidi_characters_never_reach_the_terminal() {
        let hostile = hostile();
        assert_eq!(clean(&hostile), "a[2Jb]0;ownedc31mdefg");
        assert_eq!(clean("keeps\ttabs\nand lines"), "keeps\ttabs\nand lines");
        let rows = [
            Item::User {
                seq: 1,
                ts: None,
                text: hostile.clone(),
                attachments: Vec::new(),
                queued: false,
            },
            Item::Tool {
                seq: 2,
                id: "t".into(),
                name: format!("Ba{}sh", char::from(0x1b)),
                summary: hostile.clone(),
                status: Status::Ok,
                exit_code: None,
                duration_ms: None,
                ts: None,
                output_bytes: 0,
                has_detail: false,
                attachments: Vec::new(),
            },
            Item::Notice {
                seq: 3,
                kind: format!("k{}[H", char::from(0x1b)),
                text: hostile.clone(),
            },
        ];
        for item in &rows {
            let line = row(item);
            assert!(
                !line
                    .chars()
                    .any(|c| c.is_control() || u32::from(c) == 0x202e),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_kind_from_a_newer_daemon_is_one_dim_row() {
        assert!(row(&Item::Unknown).contains("does not know"));
        assert_eq!(status_word(Status::Unknown), "unknown");
    }

    #[test]
    fn a_row_is_cut_at_the_width() {
        let long = Item::Agent {
            seq: 1,
            ts: None,
            text: "x".repeat(500),
            model: None,
        };
        assert_eq!(row(&long).chars().filter(|c| *c == 'x').count(), ROW);
    }
}

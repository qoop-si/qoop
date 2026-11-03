//! `qoop messages`: the chat of an agent terminal, parsed by the daemon from the agent's own
//! session file. For scripts and for looking at what the chat views of the interface and the
//! phone are given. `--task` reads the session a task's done claim recorded instead, here and
//! with no terminal, so the work behind a claim is still there once its tab is gone.

use std::time::Duration;

use qoop_parsed::{Digest, Dirs, Item, Options, Source, Status};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_runtime::messages::MAX_LIMIT;

use crate::task::ask;
use crate::usage;

const USAGE: &str = "usage: qoop messages <terminal> [--child ID] [--skip N] [--limit N] [--json]
       qoop messages --task <id> [--skip N] [--limit N] [--json]
       qoop messages detail <terminal> <tool_id> [--child ID] [--json]";

/// The longest a row of the text form is, in characters.
const ROW: usize = 120;

pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("detail") => detail(&args[3..]),
        Some(_) => page(&args[2..]),
        None => Err(usage(USAGE)),
    }
}

/// The value of `--child`: a word that is not another flag.
fn id_arg(word: Option<&String>) -> std::io::Result<String> {
    word.filter(|w| !w.starts_with("--"))
        .cloned()
        .ok_or_else(|| usage(USAGE))
}

fn terminal_of(word: Option<&String>) -> std::io::Result<u32> {
    word.and_then(|v| v.parse().ok())
        .ok_or_else(|| usage(USAGE))
}

fn page(args: &[String]) -> std::io::Result<()> {
    let task = match args.first().map(String::as_str) {
        Some("--task") => Some(
            args.get(1)
                .and_then(|v| v.trim_start_matches('#').parse::<i64>().ok())
                .ok_or_else(|| usage(USAGE))?,
        ),
        _ => None,
    };
    let (mut skip, mut limit, mut json) = (0u32, 0u32, false);
    let mut child = None;
    let mut rest = args.iter().skip(if task.is_some() { 2 } else { 1 });
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
            "--child" => child = Some(id_arg(rest.next())?),
            _ => return Err(usage(format!("unknown flag {arg}; {USAGE}"))),
        }
    }
    if let Some(task) = task {
        if child.is_some() {
            return Err(usage(format!("--child reads a live terminal; {USAGE}")));
        }
        show(&recorded(task, skip, limit)?, json);
        return Ok(());
    }
    let terminal = terminal_of(args.first())?;
    crate::qass_guard(&terminal.to_string())?;
    let id = crate::request_id("messages");
    let request = match child {
        Some(child) => ControlRequest::SubagentMessages {
            id,
            terminal,
            child,
            skip,
            limit,
            revision: None,
        },
        None => ControlRequest::Messages {
            id,
            terminal,
            skip,
            limit,
            revision: None,
        },
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
            show(
                &Digest {
                    revision,
                    first_seq,
                    has_older,
                    items,
                    warnings,
                },
                json,
            );
            Ok(())
        }
        other => Err(refused(other)),
    }
}

/// A page as `qoop messages` prints it, whichever way it was read.
fn show(page: &Digest, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "revision": page.revision,
                "first_seq": page.first_seq,
                "has_older": page.has_older,
                "items": page.items,
                "warnings": page.warnings,
            })
        );
    } else {
        for item in &page.items {
            println!("{}", row(item));
        }
    }
}

/// A page of the session task `task`'s newest done claim recorded, read here: its tab may be
/// long gone. qass is kept off a task the owner keeps from it: by its tab while it has one,
/// else by its space, and by what the record says the owner kept from qass at the claim.
fn recorded(task: i64, skip: u32, limit: u32) -> std::io::Result<Digest> {
    let mut before = None;
    let body = loop {
        let found = crate::task::read(Some(task), before, false, Duration::from_secs(10))?;
        if before.is_none()
            && let Some(t) = found.tasks.first()
        {
            match (t.terminal, t.workspace) {
                (Some(terminal), _) => crate::qass_guard(&terminal.to_string())?,
                (None, Some(space)) => crate::qass_space_guard(space)?,
                (None, None) => {}
            }
        }
        if let Some(record) = found.events.iter().find(|e| e.kind == "record") {
            break record.body.clone();
        }
        match found.events.last() {
            Some(last) if found.more => before = Some(last.id),
            _ => {
                return Err(std::io::Error::other(format!(
                    "task #{task} has no record: it was never claimed done, or was claimed \
                     before qoop kept one"
                )));
            }
        }
    };
    if kept_from_qass(&body) && crate::qass::may_be_qass() && crate::qass::called_by_qass() {
        return Err(std::io::Error::other(format!(
            "task #{task} was claimed in a terminal the owner asked qass to leave alone: its \
             session is not yours to read"
        )));
    }
    let dirs = Dirs::from_home().ok();
    let source = source_of(task, &body, dirs.as_ref()).map_err(std::io::Error::other)?;
    load(&source, dirs.map(|d| d.codex), skip, limit)
}

/// Whether a `record` event says the owner had asked qass to leave its terminal or space
/// alone when the claim was made. A record that cannot be read says so too.
fn kept_from_qass(record: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(record).map_or(true, |r| r["skipped"] == true)
}

/// The session a `record` event names, found again by its program and id where that agent
/// keeps it, checked as the daemon checks a live one. The path kept with the claim is only
/// what it was then, never opened as it is.
fn source_of(task: i64, record: &str, dirs: Option<&Dirs>) -> Result<Source, String> {
    let record: serde_json::Value = serde_json::from_str(record).unwrap_or_default();
    let (Some(program), Some(session)) = (record["program"].as_str(), record["session"].as_str())
    else {
        return Err(format!(
            "no_session: no session was recorded with task #{task}'s claim"
        ));
    };
    dirs.and_then(|dirs| qoop_parsed::locate_session(program, session, dirs))
        .ok_or_else(|| {
            format!(
                "no_session: the session recorded with task #{task}'s claim ({program} {}) is \
                 not where {program} keeps its sessions",
                clean(session)
            )
        })
}

/// One page of a session, with the daemon's defaults and caps.
fn load(
    source: &Source,
    codex_root: Option<std::path::PathBuf>,
    skip: u32,
    limit: u32,
) -> std::io::Result<Digest> {
    let opts = Options {
        skip: skip as usize,
        limit: match limit {
            0 => Options::default().limit,
            n => n.min(MAX_LIMIT) as usize,
        },
        codex_root,
        ..Options::default()
    };
    qoop_parsed::load(source, &opts).map_err(|e| match e {
        qoop_parsed::Error::NotFound => refusal("no_session"),
        qoop_parsed::Error::TooLarge => refusal("too_large"),
        e => std::io::Error::other(e.to_string()),
    })
}

fn detail(args: &[String]) -> std::io::Result<()> {
    let terminal = terminal_of(args.first())?;
    crate::qass_guard(&terminal.to_string())?;
    let tool_id = args.get(1).ok_or_else(|| usage(USAGE))?.clone();
    let (mut json, mut child) = (false, None);
    let mut rest = args.iter().skip(2);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--child" => child = Some(id_arg(rest.next())?),
            _ => return Err(usage(format!("unknown flag {arg}; {USAGE}"))),
        }
    }
    let id = crate::request_id("detail");
    let request = match child {
        Some(child) => ControlRequest::SubagentDetail {
            id,
            terminal,
            child,
            tool_id,
            max_bytes: None,
        },
        None => ControlRequest::MessageDetail {
            id,
            terminal,
            tool_id,
            max_bytes: None,
        },
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
    refusal(&reason)
}

/// A reason code as a person reads it.
fn refusal(reason: &str) -> std::io::Error {
    let meaning = match reason {
        "no_session" => "the agent has no session file yet",
        "not_agent" => "no coding agent runs in that terminal",
        "too_large" => "that session is too large for the chat, read the terminal",
        "unavailable" => "no such terminal, or its session did not answer in time",
        _ => return std::io::Error::other(clean(reason)),
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
            name,
            child,
            ..
        } => {
            let mut line = format!("[{}] {summary} ({steps} steps)", status_word(*status));
            if let Some(name) = name {
                line.push_str(&format!(" {name}"));
            }
            // What opens its own chat: `qoop messages <terminal> --child <id>`.
            if let Some(child) = child {
                line.push_str(&format!(" --child {child}"));
            }
            ("subagent", line)
        }
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
            act: Some("spawn".into()),
            name: Some("e1".into()),
            child: Some("ae1-14ff".into()),
        };
        // The word is before the summary: a long one is cut at the end of the line.
        let long = Item::Subagent {
            seq: 7,
            summary: "x".repeat(200),
            steps: 1,
            status: Status::Failed,
            act: None,
            name: None,
            child: None,
        };
        assert!(row(&long).contains("[failed] xxx"), "{}", row(&long));
        assert_eq!(
            row(&subagent),
            "    6  subagent   [running] spawned: survey (2 steps) e1 --child ae1-14ff"
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

    /// A task's record names its session by program and id, found again where the agent
    /// keeps it and read here with no terminal into the rows a live one prints. The path kept
    /// with the claim is never opened as it is; a record with no session, a session no longer
    /// there and a file gone since are each said.
    #[test]
    fn a_tasks_record_reads_its_session_with_no_terminal() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join(".claude/projects/-repo");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s1.jsonl");
        let line = serde_json::json!({"type": "user", "uuid": "u1", "sessionId": "s1",
            "timestamp": "2026-10-12T09:00:00.000Z",
            "message": {"role": "user", "content": "ship it"}});
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let dirs = Dirs::under(home.path(), None, None);
        let record = serde_json::json!({"program": "claude", "session": "s1",
            "source": {"file": "/somewhere/else.jsonl"}, "head": "abc", "clean": true});
        let source = source_of(7, &record.to_string(), Some(&dirs)).expect("a source");
        assert_eq!(source, Source::File(path.canonicalize().unwrap()));
        let page = load(&source, None, 0, 0).expect("a page");
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            row(&page.items[0]),
            format!("{:>5}  you        ship it", page.items[0].seq())
        );
        let none = source_of(7, r#"{"program": "claude", "head": "abc"}"#, Some(&dirs));
        assert_eq!(
            none.unwrap_err(),
            "no_session: no session was recorded with task #7's claim"
        );
        let elsewhere = r#"{"program": "claude", "session": "s2"}"#;
        let moved = source_of(7, elsewhere, Some(&dirs)).unwrap_err();
        assert!(
            moved.contains("(claude s2) is not where claude keeps"),
            "{moved}"
        );
        let traversal = r#"{"program": "claude", "session": "../s1"}"#;
        assert!(
            source_of(7, traversal, Some(&dirs)).is_err(),
            "an id is checked"
        );
        // What the owner kept from qass at the claim is kept after the tab is gone.
        assert!(kept_from_qass(
            r#"{"terminal": 4, "skipped": true, "session": "s1"}"#
        ));
        assert!(!kept_from_qass(&record.to_string()));
        assert!(kept_from_qass("not json"), "unread is kept");
        std::fs::remove_file(&path).unwrap();
        let gone = load(&source, None, 0, 0).unwrap_err().to_string();
        assert!(gone.starts_with("no_session"), "{gone}");
    }
}

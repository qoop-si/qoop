//! Claude Code transcript lines (`~/.claude/projects/<slug>/<id>.jsonl`).
//!
//! Each line becomes at most one [`Message`]; metadata, boundary and unknown lines become none.

use serde_json::Value;

use crate::model::{Message, Role};
use crate::ts::rfc3339_to_millis;

/// Text on a user line that nobody typed: the summary a compacted session starts with, and
/// the marker an interrupt leaves.
const NOT_TYPED: [&str; 2] = [
    "This session is being continued",
    "[Request interrupted by user",
];

/// Parse pre-split lines (see `read::read_tail_lines`) into messages.
/// Corrupt lines are skipped and counted; only a total failure to read is an
/// error, and that already happened upstream.
///
/// What the person typed while the agent was busy is read too: Claude Code queues it
/// (`queue-operation` `enqueue`), then either takes it as a turn of its own (`dequeue`, and a
/// `user` line follows) or folds it into the running turn (`remove`, and an `attachment` of
/// type `queued_command` follows). One still queued counts as typed when it was queued, and
/// one taken as a turn or folded into one keeps the time it was queued: its `user` line is stamped when the agent
/// took it, which can be long after it was typed. A `popAll` puts every queued text back in
/// the input box, unsent. A user line from another origin (a peer session, a coordinator)
/// is nobody typing here.
pub fn parse_lines(lines: &[String]) -> (Vec<Message>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    // Queued texts not taken yet, first in first out, with where they stand in `out`.
    let mut queued: std::collections::VecDeque<(usize, Message)> = Default::default();
    // Texts a dequeue took, with the time they were queued, for the user line they become.
    let mut taken: Vec<Message> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            skipped += 1;
            continue;
        };
        let ts = || {
            v.get("timestamp")
                .and_then(Value::as_str)
                .and_then(rfc3339_to_millis)
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "queue-operation" => {
                let content = v.get("content").and_then(Value::as_str).unwrap_or("");
                match v.get("operation").and_then(Value::as_str) {
                    Some("enqueue") => queued.push_back((
                        out.len(),
                        Message {
                            role: Role::User,
                            text: content.to_string(),
                            ts: ts(),
                        },
                    )),
                    Some("dequeue") => taken.extend(
                        queued
                            .pop_front()
                            .map(|(_, m)| m)
                            .filter(|m| !m.text.starts_with('<')),
                    ),
                    Some("popAll") => queued.clear(),
                    Some("remove") => {
                        if let Some(at) = queued.iter().position(|(_, m)| m.text == content) {
                            taken.extend(
                                queued
                                    .remove(at)
                                    .map(|(_, m)| m)
                                    .filter(|m| !m.text.starts_with('<')),
                            );
                        }
                    }
                    _ => {}
                }
            }
            "attachment" => {
                let a = v.get("attachment").unwrap_or(&Value::Null);
                let human = a.get("type").and_then(Value::as_str) == Some("queued_command")
                    && a.get("origin")
                        .and_then(|o| o.get("kind"))
                        .and_then(Value::as_str)
                        == Some("human");
                let text = a.get("prompt").map(flatten).unwrap_or_default();
                if human && !text.is_empty() {
                    // Folded into the turn after it was queued: the time it was queued.
                    let queued_at = taken
                        .iter()
                        .position(|q| q.text == text)
                        .and_then(|at| taken.remove(at).ts);
                    let ts = queued_at.or_else(|| {
                        a.get("timestamp")
                            .and_then(Value::as_str)
                            .and_then(rfc3339_to_millis)
                            .or_else(ts)
                    });
                    out.push(Message {
                        role: Role::User,
                        text,
                        ts,
                    });
                }
            }
            _ => {
                let summary = v.get("isCompactSummary").and_then(Value::as_bool) == Some(true);
                let other = v
                    .get("origin")
                    .and_then(|o| o.get("kind"))
                    .and_then(Value::as_str)
                    .is_some_and(|k| k != "human");
                match route(&v) {
                    Some(m)
                        if m.role == Role::User
                            && (summary
                                || other
                                || NOT_TYPED.iter().any(|p| m.text.starts_with(p))) => {}
                    Some(mut m) => {
                        if m.role == Role::User
                            && let Some(at) = taken.iter().position(|q| q.text == m.text)
                        {
                            m.ts = taken.remove(at).ts.or(m.ts);
                        }
                        out.push(m)
                    }
                    None => {}
                }
            }
        }
    }
    // Still queued: typed when queued. Notifications queue too, and are nobody's message.
    for (at, m) in queued.into_iter().rev() {
        if !m.text.is_empty() && !m.text.starts_with('<') {
            out.insert(at, m);
        }
    }
    (out, skipped)
}

pub(crate) fn route(obj: &Value) -> Option<Message> {
    match obj.get("type").and_then(Value::as_str).unwrap_or("") {
        // Text the agent adds on its own (a loaded skill): nobody's message.
        "user" | "assistant" if obj.get("isMeta").and_then(Value::as_bool) == Some(true) => None,
        "user" | "assistant" => {
            let role = obj
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                .filter(|r| *r == "user" || *r == "assistant")?;
            let content = obj.get("message").and_then(|m| m.get("content"));
            let text = content.map(flatten).unwrap_or_default();
            if text.is_empty() {
                return None;
            }
            // Tool output rides on `user` lines; nobody typed it.
            let typed = role == "user"
                && !content.and_then(Value::as_array).is_some_and(|blocks| {
                    blocks
                        .iter()
                        .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                });
            Some(Message {
                role: if typed { Role::User } else { Role::Assistant },
                text,
                ts: obj
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(rfc3339_to_millis),
            })
        }
        _ => None,
    }
}

/// Flatten a `message.content` (string or block array) to display text.
/// Thinking and image blocks are excluded (see `model.rs`); tool calls keep
/// their name plus string arguments; tool results recurse.
fn flatten(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => {
            let mut parts = Vec::new();
            for item in arr {
                if let Some(p) = flatten_block(item)
                    && !p.is_empty()
                {
                    parts.push(p);
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn flatten_block(item: &Value) -> Option<String> {
    match item.get("type")?.as_str()? {
        "text" => item.get("text").and_then(Value::as_str).map(str::to_string),
        "thinking" => None,
        "image" => None,
        "tool_use" => {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
            let scalars = collect_scalars(item.get("input").unwrap_or(&Value::Null));
            if scalars.is_empty() {
                Some(name.to_string())
            } else {
                Some(format!("{name} {scalars}"))
            }
        }
        "tool_result" => {
            let inner = item.get("content").unwrap_or(&Value::Null);
            Some(flatten(inner))
        }
        "tool_reference" => item
            .get("tool_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn collect_scalars(v: &Value) -> String {
    let mut out = Vec::new();
    collect_into(v, &mut out);
    cap_text(&out.join(" "))
}

/// Tool arguments can be huge; cap the fold with a marker, like the CLI's
/// per-message cap. The marker is part of the text goldens pin.
fn cap_text(s: &str) -> String {
    const CAP: usize = 2000;
    if s.len() <= CAP {
        return s.to_string();
    }
    let mut end = CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[…]", &s[..end])
}

fn collect_into<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| collect_into(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_into(x, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(v: Vec<Value>) -> Vec<String> {
        v.into_iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn user_and_assistant_text() {
        let (msgs, skipped) = parse_lines(&lines(vec![
            json!({"type":"user","timestamp":"2026-06-10T10:00:00Z",
                "message":{"role":"user","content":"where is the bug"}}),
            json!({"type":"assistant","timestamp":"2026-06-10T10:00:01Z",
                "message":{"role":"assistant","content":[{"type":"text","text":"here"}]}}),
            json!({"type":"mode","mode":"normal"}),
        ]));
        assert_eq!(skipped, 0);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].text, "where is the bug");
        assert!(msgs[0].ts.is_some());
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[test]
    fn thinking_excluded_tool_names_kept() {
        let (msgs, _) = parse_lines(&lines(vec![
            json!({"type":"assistant","message":{"role":"assistant","content":[
                {"type":"thinking","thinking":"hmm"},
                {"type":"text","text":"the fix"},
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}),
        ]));
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].text.contains("the fix"));
        assert!(msgs[0].text.contains("Bash"));
        assert!(!msgs[0].text.contains("hmm"));
    }

    /// A tool's output comes back on a `user` line, but nobody typed it: it is the
    /// agent's side of the turn, so `--role user` keeps only what a person wrote.
    #[test]
    fn tool_output_is_not_the_person() {
        let (msgs, _) = parse_lines(&lines(vec![
            json!({"type":"user","message":{"role":"user","content":"run the tests"}}),
            json!({"type":"user","message":{"role":"user","content":[
                {"type":"tool_result","tool_use_id":"t1","content":"3 passed"}]}}),
        ]));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].text, "3 passed");
    }

    /// What the agent adds on its own (a skill's text when it loads) is marked
    /// `isMeta`: nobody typed it and nobody said it, so it is no message at all.
    #[test]
    fn meta_lines_are_no_message() {
        let (msgs, _) = parse_lines(&lines(vec![
            json!({"type":"user","message":{"role":"user","content":"/commit"}}),
            json!({"type":"user","isMeta":true,"message":{"role":"user","content":[
                {"type":"text","text":"Base directory for this skill: /x"}]}}),
        ]));
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "/commit");
    }

    #[test]
    fn corrupt_lines_counted_not_fatal() {
        let mut ls = lines(vec![
            json!({"type":"user","message":{"role":"user","content":"hi"}}),
        ]);
        ls.push("{ broken".to_string());
        let (msgs, skipped) = parse_lines(&ls);
        assert_eq!(msgs.len(), 1);
        assert_eq!(skipped, 1);
    }
}

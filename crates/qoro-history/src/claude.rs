//! Claude Code transcript lines (`~/.claude/projects/<slug>/<id>.jsonl`).
//!
//! Each line becomes at most one [`Message`]; metadata, boundary and unknown lines become none.

use serde_json::Value;

use crate::model::{Message, Role};
use crate::ts::rfc3339_to_millis;

/// Parse pre-split lines (see `read::read_tail_lines`) into messages.
/// Corrupt lines are skipped and counted; only a total failure to read is an
/// error, and that already happened upstream.
pub fn parse_lines(lines: &[String]) -> (Vec<Message>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => {
                if let Some(m) = route(&v) {
                    out.push(m);
                }
            }
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

fn route(obj: &Value) -> Option<Message> {
    match obj.get("type").and_then(Value::as_str).unwrap_or("") {
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
            Some(Message {
                role: if role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
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

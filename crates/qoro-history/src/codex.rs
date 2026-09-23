//! Codex rollout logs (`~/.codex/sessions/**/*.jsonl`).
//!
//! The conversation lives in `event_msg`/`item_completed` items (`UserMessage` typed,
//! `AgentMessage` said); `response_item` messages only count when a file has
//! no completed items at all. Tool activity folds into assistant messages
//! named after the tool, paths only for file changes, reasoning never.

use serde_json::Value;

use crate::model::{Message, Role};
use crate::ts::rfc3339_to_millis;

/// Parse pre-split lines into messages plus a corrupt-line count.
pub fn parse_lines(lines: &[String]) -> (Vec<Message>, usize) {
    // Two passes: `response_item` records only count when the file holds no
    // completed conversation at all.
    let mut values: Vec<Value> = Vec::new();
    let mut skipped = 0;
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(v) => values.push(v),
            Err(_) => skipped += 1,
        }
    }
    let mut primary = Vec::new();
    let mut fallback = Vec::new();
    for v in &values {
        route_line(v, &mut primary, &mut fallback);
    }
    let has_conversation = primary.iter().any(|m: &Message| !m.text.is_empty());
    let mut out = primary;
    if !has_conversation {
        out.extend(fallback);
    }
    (out, skipped)
}

fn line_ts(obj: &Value) -> Option<i64> {
    obj.get("timestamp")
        .and_then(Value::as_str)
        .and_then(rfc3339_to_millis)
}

fn route_line(obj: &Value, primary: &mut Vec<Message>, fallback: &mut Vec<Message>) {
    match obj.get("type").and_then(Value::as_str).unwrap_or("") {
        "event_msg" => {
            let p = obj.get("payload");
            if p.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("item_completed")
                && let Some(item) = p.and_then(|p| p.get("item"))
            {
                route_item(item, line_ts(obj), primary);
            }
        }
        "response_item" => {
            if let Some(m) = route_response_message(obj) {
                fallback.push(m);
            }
        }
        _ => {}
    }
}

fn route_item(item: &Value, ts: Option<i64>, primary: &mut Vec<Message>) {
    let item_ty = item.get("type").and_then(Value::as_str).unwrap_or("");
    match item_ty {
        "UserMessage" => {
            let text = join_text_blocks(item.get("content"));
            if !text.is_empty() {
                primary.push(Message {
                    role: Role::User,
                    text,
                    ts,
                });
            }
        }
        "AgentMessage" => {
            let text = join_text_blocks(item.get("content"));
            if !text.is_empty() {
                primary.push(Message {
                    role: Role::Assistant,
                    text,
                    ts,
                });
            }
        }
        "CommandExecution" => {
            let cmd = item
                .get("command")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let name = item
                .get("parsed_cmd")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("exec");
            primary.push(tool_message(name, &cmd, ts));
        }
        "McpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let name = if server.is_empty() {
                tool.to_string()
            } else {
                format!("{server}/{tool}")
            };
            let args = item
                .get("arguments")
                .map(collect_truncated)
                .unwrap_or_default();
            primary.push(tool_message(&name, &args, ts));
        }
        "Extension" => {
            let kind = item
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("extension");
            let detail = item
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    item.get("action")
                        .map(collect_truncated)
                        .unwrap_or_default()
                });
            primary.push(tool_message(kind, &detail, ts));
        }
        "FileChange" => {
            // Paths only: change values can hold whole file contents.
            let paths = item
                .get("changes")
                .and_then(Value::as_object)
                .map(|m| {
                    let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
                    keys.sort();
                    keys.join("\n")
                })
                .unwrap_or_default();
            primary.push(tool_message("file_change", &paths, ts));
        }
        _ => {}
    }
}

/// Tool nodes read as assistant messages named after the tool, so the
/// window keeps one shape and the merge stays visible.
fn tool_message(name: &str, detail: &str, ts: Option<i64>) -> Message {
    let text = if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {detail}")
    };
    Message {
        role: Role::Assistant,
        text,
        ts,
    }
}

/// A `response_item` message with role user|assistant. Developer prompts are
/// skipped; user text is stripped of injected plugin/environment blocks.
fn route_response_message(obj: &Value) -> Option<Message> {
    let p = obj.get("payload")?;
    if p.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = match p.get("role").and_then(Value::as_str)? {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        _ => return None,
    };
    let raw = p
        .get("content")
        .map(|c| join_text_blocks(Some(c)))
        .unwrap_or_default();
    let text = if role == Role::User {
        strip_boilerplate(&raw)
    } else {
        raw.trim().to_string()
    };
    if text.is_empty() {
        return None;
    }
    Some(Message {
        role,
        text,
        ts: line_ts(obj),
    })
}

/// Codex uses `type: "text"` (user) and `type: "Text"` (agent); read the
/// field either way.
fn join_text_blocks(content: Option<&Value>) -> String {
    match content.and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

fn strip_tag(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open.as_str()) {
        out.push_str(&rest[..start]);
        let after = &rest[start + open.len()..];
        rest = match after.find(close.as_str()) {
            Some(end) => &after[end + close.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn strip_boilerplate(text: &str) -> String {
    strip_tag(
        &strip_tag(text, "recommended_plugins"),
        "environment_context",
    )
    .trim()
    .to_string()
}

fn collect_truncated(v: &Value) -> String {
    let mut parts: Vec<&str> = Vec::new();
    collect_into(v, &mut parts);
    let joined = parts.join(" ");
    const CAP: usize = 2000;
    if joined.len() <= CAP {
        return joined;
    }
    let mut end = CAP;
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    // Marked like every other cap in this crate; goldens pin the marker.
    format!("{}[…]", &joined[..end])
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

    fn codex_line(ty: &str, payload: serde_json::Value) -> String {
        serde_json::json!({
            "timestamp": "2026-09-04T13:24:40.608Z",
            "ordinal": 1,
            "type": ty,
            "payload": payload
        })
        .to_string()
    }

    fn read_all(data: &str) -> (Vec<Message>, usize) {
        // parse_lines wants pre-split lines; emulate read.rs on memory.
        parse_lines(&data.lines().map(str::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn user_and_agent_items_become_messages() {
        let data = format!(
            "{}\n{}\n",
            codex_line(
                "event_msg",
                serde_json::json!({"type": "item_completed",
                    "item": {"type": "UserMessage", "id": "u1",
                        "content": [{"type": "text", "text": "fix the crash"}]}})
            ),
            codex_line(
                "event_msg",
                serde_json::json!({"type": "item_completed",
                    "item": {"type": "AgentMessage", "id": "a1",
                        "content": [{"type": "Text", "text": "fixed it"}]}})
            ),
        );
        let (msgs, skipped) = read_all(&data);
        assert_eq!(skipped, 0);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].text, "fix the crash");
        assert!(msgs[0].ts.is_some());
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[test]
    fn response_items_ignored_when_completed_exist() {
        let data = format!(
            "{}\n{}\n",
            codex_line(
                "event_msg",
                serde_json::json!({"type": "item_completed",
                    "item": {"type": "UserMessage", "id": "u1",
                        "content": [{"type": "text", "text": "real"}]}})
            ),
            codex_line(
                "response_item",
                serde_json::json!({"type": "message", "id": "m1", "role": "user",
                    "content": [{"type": "input_text", "text": "DUPLICATE"}]})
            ),
        );
        let (msgs, _) = read_all(&data);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "real");
    }

    #[test]
    fn tool_items_read_as_named_assistant_messages() {
        let data = format!(
            "{}\n",
            codex_line(
                "event_msg",
                serde_json::json!({"type": "item_completed",
                    "item": {"type": "McpToolCall", "id": "m1", "server": "app",
                        "tool": "app_search", "arguments": {"query": "x"}}})
            ),
        );
        let (msgs, _) = read_all(&data);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::Assistant);
        assert!(msgs[0].text.contains("app_search"));
    }
}

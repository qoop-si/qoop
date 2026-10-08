//! Gemini CLI chats (`~/.gemini/tmp/<hash>/chats/session-*.json`).
//!
//! Shape verified against real files on this machine: each file holds
//! `{sessionId, projectHash, messages: [{id, timestamp, type, content}]}`.
//! `type` is `user`, `gemini`, or `info` (server notices, skipped).
//! `content` is a string or a `[{text}]` array; `thoughts` are excluded
//! (see `model.rs`).

use serde_json::Value;

use crate::model::{Message, Role};
use crate::ts::rfc3339_to_millis;

/// Parse one chats file into messages. `None` when the file is not a chats
/// file at all (wrong shape); corrupt messages inside are skipped.
pub fn parse_file(text: &str) -> Option<(String, Vec<Message>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let session_id = v.get("sessionId")?.as_str()?.to_string();
    let arr = v.get("messages")?.as_array()?;
    let mut out = Vec::new();
    for m in arr {
        let role = match m.get("type").and_then(Value::as_str) {
            Some("user") => Role::User,
            Some("gemini") => Role::Assistant,
            _ => continue,
        };
        let text = flatten_content(m.get("content"));
        if text.is_empty() {
            continue;
        }
        out.push(Message {
            role,
            text,
            ts: m
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(rfc3339_to_millis),
        });
    }
    Some((session_id, out))
}

fn flatten_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_gemini_kept_info_skipped() {
        let doc = json!({
            "sessionId": "e1478d97-220e-4a37-99a0-22e26f7ebc9c",
            "projectHash": "c9ed",
            "messages": [
                {"id": "a", "timestamp": "2026-03-24T11:21:49.937Z",
                 "type": "user", "content": [{"text": "scrape it"}]},
                {"id": "b", "timestamp": "2026-03-24T11:21:54.151Z",
                 "type": "gemini", "content": "on it",
                 "thoughts": [{"subject": "x", "description": "y"}]},
                {"id": "c", "timestamp": "2026-03-24T11:22:00.000Z",
                 "type": "info", "content": "waiting for servers"},
            ]
        });
        let (id, msgs) = parse_file(&doc.to_string()).expect("chats shape");
        assert_eq!(id, "e1478d97-220e-4a37-99a0-22e26f7ebc9c");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].text, "scrape it");
        assert!(msgs[0].ts.is_some());
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].text, "on it");
    }

    #[test]
    fn wrong_shape_is_none() {
        assert!(parse_file("{\"logs\": []}").is_none());
        assert!(parse_file("{ broken").is_none());
    }
}

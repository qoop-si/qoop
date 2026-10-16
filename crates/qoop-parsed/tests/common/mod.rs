//! Shared helpers: fixture paths and small builders of Claude transcript lines.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use qoop_parsed::{Digest, Item, Options, Source, load};
use serde_json::{Value, json};

pub const CLAUDE_ID: &str = "5a1c0de0-1111-4222-8333-444455556666";

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn claude() -> Source {
    Source::File(fixture("claude.jsonl"))
}

pub fn digest(source: &Source) -> Digest {
    load(source, &Options::default()).expect("load")
}

pub fn tool<'a>(d: &'a Digest, id: &str) -> &'a Item {
    d.items
        .iter()
        .find(|i| matches!(i, Item::Tool { id: t, .. } if t == id))
        .unwrap_or_else(|| panic!("no tool {id}"))
}

/// An opencode database holding the `v2.sql` fixture.
pub fn opencode_db(dir: &Path) -> PathBuf {
    let db = dir.join("opencode.db");
    let sql = std::fs::read_to_string(fixture("opencode/v2.sql")).expect("v2.sql");
    rusqlite::Connection::open(&db)
        .expect("open")
        .execute_batch(&sql)
        .expect("load v2.sql");
    db
}

pub fn line(value: Value) -> String {
    format!("{value}\n")
}

fn base(kind: &str, n: usize) -> Value {
    json!({
        "type": kind, "uuid": format!("u{n:05}"), "isSidechain": false, "sessionId": CLAUDE_ID,
        "timestamp": format!("2026-10-12T09:{:02}:{:02}.000Z", n / 60 % 60, n % 60),
        "userType": "external", "entrypoint": "cli", "cwd": "/work/demo", "version": "2.1.296",
    })
}

pub fn user_line(n: usize, text: &str) -> String {
    let mut v = base("user", n);
    v["message"] = json!({"role": "user", "content": text});
    v["origin"] = json!({"kind": "human"});
    line(v)
}

pub fn agent_line(n: usize, text: &str) -> String {
    let mut v = base("assistant", n);
    v["message"] = json!({
        "id": format!("msg_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
        "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1},
    });
    line(v)
}

pub fn call_line(n: usize, id: &str, command: &str) -> String {
    let mut v = base("assistant", n);
    v["message"] = json!({
        "id": format!("msg_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
        "content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}],
        "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1},
    });
    line(v)
}

pub fn result_line(n: usize, id: &str, text: &str) -> String {
    let mut v = base("user", n);
    v["message"] = json!({
        "role": "user",
        "content": [{"type": "tool_result", "tool_use_id": id, "content": text, "is_error": false}],
    });
    line(v)
}

pub fn write(dir: &Path, name: &str, lines: &[String]) -> Source {
    let path = dir.join(name);
    std::fs::write(&path, lines.concat()).expect("write");
    Source::File(path)
}

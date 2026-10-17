//! Rows read from real shapes: Codex code cells, thinking with nothing to show, results whose
//! call is gone, messages from another agent, edits, and `seq` across pages.

mod common;

use common::*;
use qoop_parsed::{Item, Options, Source, Status, detail, load};
use serde_json::{Value, json};

fn rows(d: &qoop_parsed::Digest) -> Vec<(&str, &str)> {
    d.items
        .iter()
        .filter_map(|i| match i {
            Item::Tool { name, summary, .. } => Some((name.as_str(), summary.as_str())),
            _ => None,
        })
        .collect()
}

/// The real-shaped Codex rollout of the fixtures with `extra` records after its last one.
fn codex_with(dir: &std::path::Path, extra: &[Value]) -> Source {
    let mut text = std::fs::read_to_string(fixture("codex/real-exit3.jsonl")).unwrap();
    for (n, mut record) in extra.iter().cloned().enumerate() {
        record["ordinal"] = (29 + n).into();
        record["timestamp"] = "2026-10-13T01:35:00.000Z".into();
        record["type"] = "response_item".into();
        text.push_str(&format!("{record}\n"));
    }
    let path = dir.join("rollout.jsonl");
    std::fs::write(&path, text).unwrap();
    Source::File(path)
}

fn item(payload: Value) -> Value {
    json!({ "payload": payload })
}

#[test]
fn a_codex_cell_is_one_row_per_call_it_made_with_its_own_exit_code_and_id() {
    let source = Source::File(fixture("codex/real-exit3.jsonl"));
    let d = digest(&source);
    assert_eq!(
        rows(&d),
        [
            ("shell", "ls -la"),
            ("shell", "sh test.sh"),
            ("shell", "cat a.txt"),
            ("patch", "/work/demo/a.txt"),
        ]
    );
    let tools: Vec<_> = d
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool {
                id,
                status,
                exit_code,
                duration_ms,
                output_bytes,
                ..
            } => Some((id.clone(), *status, *exit_code, *duration_ms, *output_bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools[..3]
            .iter()
            .map(|t| (t.1, t.2, t.3.is_some()))
            .collect::<Vec<_>>(),
        [
            (Status::Ok, Some(0), true),
            (Status::Failed, Some(3), true),
            (Status::Ok, Some(0), true)
        ]
    );
    assert_eq!(tools[0].4, "total 0\n".len() as u64);
    let ids: std::collections::HashSet<_> = tools.iter().map(|t| &t.0).collect();
    assert_eq!(ids.len(), tools.len(), "ids are unique");
    assert!(
        !serde_json::to_string(&d.items).unwrap().contains("tools."),
        "no JavaScript reaches a row"
    );

    let failed = detail(&source, &tools[1].0, 4096).unwrap();
    assert_eq!(failed.input, "sh test.sh");
    assert_eq!(failed.output, "err-line\nout-line\n");
}

#[test]
fn a_cell_with_no_calls_is_one_code_row_and_its_detail_is_unwrapped() {
    let dir = tempfile::tempdir().unwrap();
    let script = "const r = await tools.exec_command({cmd: \"ls\", token: \"sk-ant-api03-TOPSECRET\"});\ntext(r);\n";
    let chunk = |out: &str| {
        json!({"chunk_id": "a1", "wall_time_seconds": 0.1, "exit_code": 0, "output": out})
            .to_string()
    };
    let source = codex_with(
        dir.path(),
        &[
            item(
                json!({"type": "custom_tool_call", "id": "ctc_1", "status": "completed", "call_id": "call_plain", "name": "exec", "input": script}),
            ),
            item(
                json!({"type": "custom_tool_call_output", "id": "ctco_1", "call_id": "call_plain", "output": [
                    {"type": "input_text", "text": "Script completed\nWall time 0.4 seconds\nOutput:\n"},
                    {"type": "input_text", "text": chunk("alpha\n")},
                    {"type": "input_text", "text": chunk("beta\n")},
                    {"type": "input_text", "text": "{}"},
                ]}),
            ),
        ],
    );
    let d = digest(&source);
    let (name, summary, id, bytes) = d
        .items
        .iter()
        .find_map(|i| match i {
            Item::Tool {
                name,
                summary,
                id,
                output_bytes,
                ..
            } if name == "code" => Some((name, summary, id, *output_bytes)),
            _ => None,
        })
        .expect("one code row");
    assert_eq!(name, "code");
    assert!(summary.starts_with("const r = await tools.exec_command("));
    assert!(summary.chars().count() <= 80 && !summary.contains("TOPSECRET"));
    assert_eq!(
        bytes,
        "alpha\nbeta\n".len() as u64,
        "counts the unwrapped output"
    );
    let full = detail(&source, id, 4096).unwrap();
    assert_eq!(full.output, "alpha\nbeta\n");
    assert!(full.input.starts_with("const r = await") && !full.input.contains("TOPSECRET"));
}

#[test]
fn thinking_with_nothing_to_show_has_no_row() {
    let d = digest(&claude());
    assert!(!d.items.iter().any(|i| matches!(
        i,
        Item::Thinking {
            text: None,
            ms: None,
            ..
        }
    )));
    let dir = tempfile::tempdir().unwrap();
    let mut v: Value = serde_json::from_str(&agent_line(2, "done")).unwrap();
    v["message"]["content"] = json!([
        {"type": "thinking", "thinking": "weigh the two", "signature": "x"},
        {"type": "text", "text": "done"},
    ]);
    let source = write(dir.path(), "s.jsonl", &[user_line(1, "go"), line(v)]);
    let d = digest(&source);
    assert!(
        d.items
            .iter()
            .any(|i| matches!(i, Item::Thinking { text: Some(t), .. } if t == "weigh the two"))
    );
}

#[test]
fn an_orphan_result_is_dim_and_one_with_nothing_in_it_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            user_line(1, "hello"),
            result_line(2, "toolu_empty", ""),
            result_line(3, "toolu_gone", "first line\nsecond line"),
        ],
    );
    let d = digest(&source);
    assert_eq!(d.warnings, 2);
    assert_eq!(rows(&d), [("earlier result", "first line")]);
}

#[test]
fn a_message_from_another_agent_is_one_notice_and_an_empty_one_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let message = |id: &str, text: &str, extra: Option<Value>| {
        let mut content = vec![json!({"type": "input_text", "text": text})];
        content.extend(extra);
        item(
            json!({"type": "agent_message", "id": id, "author": "/root/review", "recipient": "/root", "content": content}),
        )
    };
    let source = codex_with(
        dir.path(),
        &[
            message(
                "amsg_1",
                "Message Type: MESSAGE\nTask name: /root\nSender: /root/review\nPayload:\nVERDICT: APPROVE token=abc123secret\nsecond line",
                None,
            ),
            message(
                "amsg_2",
                "Message Type: NEW_TASK\nTask name: /root\nSender: /root\nPayload:\n",
                Some(json!({"type": "encrypted_content", "encrypted_content": "gAAAAAB0ZmFrZQ=="})),
            ),
        ],
    );
    let d = digest(&source);
    let notices: Vec<_> = d
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Notice { kind, text, .. } => Some((kind.as_str(), text.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        [("agent message", "/root/review: VERDICT: APPROVE token=***")]
    );
}

#[test]
fn a_subagent_call_says_whether_it_spawned_sent_or_waited() {
    let dir = tempfile::tempdir().unwrap();
    let call = |n: &str, name: &str| {
        item(
            json!({"type": "function_call", "id": format!("fc_{n}"), "call_id": format!("call_{n}"), "name": name, "arguments": json!({"task_name": "review"}).to_string()}),
        )
    };
    let source = codex_with(
        dir.path(),
        &[
            call("a", "spawn_agent"),
            call("b", "send_message"),
            call("c", "wait_agent"),
        ],
    );
    let d = digest(&source);
    let said: Vec<_> = d
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Subagent { summary, .. } => Some(summary.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        ["spawned: review", "sent: review", "waited for: review"]
    );
}

#[test]
fn an_edit_shows_its_path_in_the_list_and_a_short_old_and_new_in_detail() {
    let dir = tempfile::tempdir().unwrap();
    let db = opencode_db(dir.path());
    let source = Source::Opencode {
        db,
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    };
    let d = digest(&source);
    let (id, summary) = d
        .items
        .iter()
        .find_map(|i| match i {
            Item::Tool {
                id, name, summary, ..
            } if name == "edit" => Some((id.clone(), summary.clone())),
            _ => None,
        })
        .expect("an edit row");
    assert_eq!(summary, "/work/demo/src/net.ts");
    let full = detail(&source, &id, 4096).unwrap();
    assert_eq!(
        full.input,
        "/work/demo/src/net.ts\n- export async function get\n+ export async function retry<T>() {}\nexport async function get"
    );
}

#[test]
fn seq_counts_from_the_newest_and_is_the_same_in_every_page_of_a_revision() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (1..=300)
        .map(|i| agent_line(i, &format!("msg {i:04} {}", "pad ".repeat(150))))
        .collect();
    let source = write(dir.path(), "s.jsonl", &lines);
    let page = |skip| {
        load(
            &source,
            &Options {
                max_tail_bytes: 8 * 1024,
                limit: 25,
                skip,
                ..Options::default()
            },
        )
        .unwrap()
    };
    for skip in [0, 25, 100, 250] {
        let d = page(skip);
        assert_eq!(d.first_seq, d.items[0].seq());
        for item in &d.items {
            let Item::Agent { text, seq, .. } = item else {
                panic!("agent rows only")
            };
            let n: u32 = text[4..8].parse().unwrap();
            assert_eq!(*seq, 300 - n, "skip {skip}: msg {n}");
        }
    }
}

#[test]
fn an_edit_detail_cuts_old_and_new_at_600_chars_and_masks_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let mut v: Value = serde_json::from_str(&call_line(1, "toolu_edit", "x")).unwrap();
    v["message"]["content"][0]["name"] = "Edit".into();
    v["message"]["content"][0]["input"] = json!({
        "file_path": "/work/demo/a.rs",
        "old_string": format!("token=abc123secret {}", "o".repeat(700)),
        "new_string": "short",
    });
    let source = write(
        dir.path(),
        "s.jsonl",
        &[line(v), result_line(2, "toolu_edit", "updated")],
    );
    let d = digest(&source);
    assert_eq!(rows(&d), [("Edit", "/work/demo/a.rs")]);
    let input = detail(&source, "toolu_edit", 64 * 1024).unwrap().input;
    let old = input.lines().nth(1).unwrap();
    assert!(
        old.starts_with("- token=*** ooo") && old.ends_with("..."),
        "{old}"
    );
    assert_eq!(
        old.chars().count(),
        2 + 600 + 3,
        "600 chars of the masked text, then ..."
    );
    assert_eq!(input.lines().nth(2), Some("+ short"));
}

//! Gaps found sweeping real sessions of the four agents: subagent step counts, compaction text,
//! result attachments, retry and duplicate errors, and records that showed as noise.

mod common;

use std::path::Path;

use common::*;
use qoop_parsed::{Chip, Item, Source};
use serde_json::{Value, json};

fn base(kind: &str, n: usize) -> Value {
    json!({
        "type": kind, "uuid": format!("w{n:05}"), "isSidechain": false, "sessionId": CLAUDE_ID,
        "timestamp": format!("2026-10-12T09:{:02}:{:02}.000Z", n / 60 % 60, n % 60),
        "userType": "external", "entrypoint": "cli", "cwd": "/work/demo", "version": "2.1.296",
    })
}

fn items(source: &Source) -> Vec<Item> {
    digest(source).items
}

fn steps(items: &[Item]) -> Vec<u32> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Subagent { steps, .. } => Some(*steps),
            _ => None,
        })
        .collect()
}

fn notices(items: &[Item]) -> Vec<(&str, &str)> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Notice { kind, text, .. } => Some((kind.as_str(), text.as_str())),
            _ => None,
        })
        .collect()
}

/// A Claude `Agent` call and its result line. `result` is the line's `toolUseResult`.
fn agent_call(n: usize, id: &str, result: Value) -> Vec<String> {
    let mut call = base("assistant", n);
    call["message"] = json!({
        "id": format!("msg_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
        "content": [{"type": "tool_use", "id": id, "name": "Agent",
            "input": {"description": "Survey", "prompt": "count", "subagent_type": "general"}}],
        "stop_reason": "tool_use", "usage": {"input_tokens": 1, "output_tokens": 1},
    });
    let mut done = base("user", n + 1);
    done["message"] = json!({"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": id, "content": "started", "is_error": false}]});
    done["toolUseResult"] = result;
    vec![line(call), line(done)]
}

fn claude_with_children(dir: &Path, calls: &[(&str, Value, usize)]) -> Source {
    let project = dir.join("projects/p");
    let child_dir = project.join(CLAUDE_ID).join("subagents");
    std::fs::create_dir_all(&child_dir).unwrap();
    let mut lines = vec![user_line(1, "go")];
    for (n, (agent, result, tools)) in calls.iter().enumerate() {
        lines.extend(agent_call(
            10 + n * 2,
            &format!("toolu_{n}"),
            result.clone(),
        ));
        let child: Vec<String> = (0..*tools)
            .map(|t| call_line(t + 1, &format!("c{n}_{t}"), "ls"))
            .collect();
        std::fs::write(
            child_dir.join(format!("agent-{agent}.jsonl")),
            child.concat(),
        )
        .unwrap();
    }
    write(&project, &format!("{CLAUDE_ID}.jsonl"), &lines)
}

#[test]
fn a_claude_spawn_row_counts_the_tool_calls_of_its_child_file() {
    let dir = tempfile::tempdir().unwrap();
    let async_launch =
        |agent: &str| json!({"isAsync": true, "status": "async_launched", "agentId": agent});
    let source = claude_with_children(
        dir.path(),
        &[
            ("aaa1", async_launch("aaa1"), 3),
            ("bbb2", async_launch("bbb2"), 0),
        ],
    );
    assert_eq!(steps(&items(&source)), [3, 0]);
}

#[test]
fn a_claude_spawn_row_takes_the_count_the_result_reports_without_reading_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let done = json!({"status": "completed", "agentId": "ccc3", "totalToolUseCount": 19});
    let source = claude_with_children(dir.path(), &[("other", done, 0)]);
    assert_eq!(steps(&items(&source)), [19]);
}

#[test]
fn only_the_newest_spawns_of_a_page_read_their_child() {
    let dir = tempfile::tempdir().unwrap();
    let launch = |n: usize| json!({"isAsync": true, "agentId": format!("a{n:03}")});
    let calls: Vec<(String, Value, usize)> = (0..40)
        .map(|n| (format!("a{n:03}"), launch(n), 2))
        .collect();
    let calls: Vec<(&str, Value, usize)> = calls
        .iter()
        .map(|(a, r, t)| (a.as_str(), r.clone(), *t))
        .collect();
    let source = claude_with_children(dir.path(), &calls);
    let got = steps(&items(&source));
    assert_eq!(got.len(), 40);
    assert_eq!(got.iter().filter(|s| **s == 2).count(), 16);
    assert!(got[..24].iter().all(|s| *s == 0) && got[24..].iter().all(|s| *s == 2));
}

#[test]
fn a_child_id_cannot_climb_out_of_the_subagents_directory() {
    let dir = tempfile::tempdir().unwrap();
    let subagents = dir
        .path()
        .join("projects/p")
        .join(CLAUDE_ID)
        .join("subagents");
    // `agent-<id>.jsonl` with the id `../../x` reaches `subagents/x.jsonl` through a directory
    // named `agent-..`. Plant both, with calls in the file, so only the id check stops it.
    std::fs::create_dir_all(subagents.join("agent-..")).unwrap();
    let planted: Vec<String> = (0..5)
        .map(|t| call_line(t + 1, &format!("p{t}"), "ls"))
        .collect();
    std::fs::write(subagents.join("x.jsonl"), planted.concat()).unwrap();
    let evil = json!({"isAsync": true, "agentId": "../../x"});
    let source = claude_with_children(dir.path(), &[("x", evil, 0)]);
    assert_eq!(steps(&items(&source)), [0]);
}

#[test]
fn a_codex_spawn_row_counts_the_rows_of_the_child_rollout() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("sessions/2026/10/12");
    std::fs::create_dir_all(&day).unwrap();
    let copy = |from: &str, to: &str| std::fs::copy(fixture(from), day.join(to)).unwrap();
    copy(
        "codex/subagent-parent.jsonl",
        "rollout-2026-10-12T11-00-00-019f1a10-0003-7000-8000-000000000003.jsonl",
    );
    copy(
        "codex/subagent-child.jsonl",
        "rollout-2026-10-12T11-00-01-019f1a10-0004-7000-8000-000000000004.jsonl",
    );
    let parent = day.join("rollout-2026-10-12T11-00-00-019f1a10-0003-7000-8000-000000000003.jsonl");
    let rows = items(&Source::File(parent));
    assert_eq!(
        steps(&rows),
        [1, 0, 0],
        "spawn reads the child, send and wait do not"
    );
}

#[test]
fn an_opencode_spawn_row_counts_the_tool_calls_of_its_child_session() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source::Opencode {
        db: opencode_db(dir.path()),
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    };
    assert_eq!(steps(&items(&source)), [1]);
}

#[test]
fn a_compaction_row_carries_the_first_line_of_its_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut boundary = base("system", 20);
    boundary["subtype"] = "compact_boundary".into();
    boundary["content"] = "Conversation compacted".into();
    boundary["compactMetadata"] = json!({"trigger": "auto", "preTokens": 9000});
    let mut summary = base("user", 21);
    summary["isCompactSummary"] = true.into();
    summary["message"] = json!({"role": "user", "content":
        "\n\nGoal: ship the sweep, key sk-ant-api03-TOPSECRETTOPSECRET0123\nSecond line stays out."});
    let source = write(
        dir.path(),
        "c.jsonl",
        &[user_line(1, "hi"), line(boundary), line(summary)],
    );
    let compactions: Vec<_> = items(&source)
        .into_iter()
        .filter_map(|i| match i {
            Item::Compaction { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(compactions.len(), 1);
    let text = compactions[0].clone().expect("the summary's first line");
    assert!(text.starts_with("Goal: ship the sweep"), "{text}");
    assert!(
        !text.contains("TOPSECRET") && !text.contains("Second line"),
        "{text}"
    );
    let wire = serde_json::to_value(&Item::Compaction { seq: 1, text: None }).unwrap();
    assert_eq!(
        wire,
        json!({"type": "compaction", "seq": 1}),
        "no text, no key"
    );
}

#[test]
fn an_opencode_compaction_row_has_its_summary_line() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source::Opencode {
        db: opencode_db(dir.path()),
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    };
    let text = items(&source).into_iter().find_map(|i| match i {
        Item::Compaction { text, .. } => text,
        _ => None,
    });
    assert_eq!(
        text.as_deref(),
        Some("Goal: Add a retry helper to /work/demo.")
    );
}

#[test]
fn a_tool_result_with_an_image_shows_its_name_and_size_never_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let mut shot = base("user", 3);
    shot["message"] = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1",
        "content": [{"type": "text", "text": "done"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]}]});
    let source = write(
        dir.path(),
        "i.jsonl",
        &[user_line(1, "shot"), call_line(2, "t1", "snap"), line(shot)],
    );
    let d = digest(&source);
    let Item::Tool { attachments, .. } = tool(&d, "t1") else {
        unreachable!()
    };
    assert_eq!(
        attachments,
        &[Chip {
            name: "image/png".into(),
            bytes: Some(70),
            path: None
        }]
    );
    assert!(!serde_json::to_string(&d).unwrap().contains("iVBOR"));
    let plain = write(
        dir.path(),
        "p.jsonl",
        &[
            user_line(1, "x"),
            call_line(2, "t2", "ls"),
            result_line(3, "t2", "ok"),
        ],
    );
    let wire = serde_json::to_value(tool(&digest(&plain), "t2")).unwrap();
    assert!(
        wire.get("attachments").is_none(),
        "no chips, no key: {wire}"
    );
}

#[test]
fn a_retry_notice_names_the_attempt_and_a_status_is_shown_once() {
    let dir = tempfile::tempdir().unwrap();
    let bare = |n: usize, attempt: u32| {
        let mut v = base("system", n);
        v["subtype"] = "api_error".into();
        v["level"] = "error".into();
        v["error"] = json!({"type": null});
        v["retryAttempt"] = attempt.into();
        line(v)
    };
    let mut limited = base("system", 12);
    limited["subtype"] = "api_error".into();
    limited["error"] =
        json!({"status": 429, "type": "rate_limit", "message": "Rate limit exceeded"});
    let mut said = base("system", 13);
    said["subtype"] = "api_error".into();
    said["error"] = json!({"status": 529, "message": "529 Overloaded"});
    let source = write(
        dir.path(),
        "e.jsonl",
        &[bare(10, 1), bare(11, 2), line(limited), line(said)],
    );
    let errors: Vec<_> = items(&source)
        .into_iter()
        .filter_map(|i| match i {
            Item::Error { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        [
            "API error, retrying (attempt 1)",
            "API error, retrying (attempt 2)",
            "Rate limit exceeded (HTTP 429)",
            "529 Overloaded",
        ]
    );
}

#[test]
fn bookkeeping_records_and_the_hooks_context_of_codex_get_no_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = vec![user_line(1, "hi")];
    for (n, kind) in [
        "api-request-shape",
        "continued-in",
        "queue-operation",
        "last-prompt",
    ]
    .iter()
    .enumerate()
    {
        lines.push(line(json!({"type": kind, "sessionId": CLAUDE_ID, "uuid": format!("b{n}"), "timestamp": "2026-10-12T09:00:05.000Z"})));
    }
    assert!(notices(&items(&write(dir.path(), "b.jsonl", &lines))).is_empty());

    let hook = json!({"payload": {"type": "message", "id": "msg_hc", "role": "developer",
        "content": [{"type": "input_text", "text": "qoop: this terminal's task is open"}],
        "internal_chat_message_metadata_passthrough": {"content_item_kinds": ["hooks.additional_context"]}}});
    let mut text = std::fs::read_to_string(fixture("codex/real-exit3.jsonl")).unwrap();
    let mut record = hook;
    record["type"] = "response_item".into();
    record["ordinal"] = 29.into();
    record["timestamp"] = "2026-10-13T01:35:00.000Z".into();
    text.push_str(&format!("{record}\n"));
    let path = dir.path().join("rollout.jsonl");
    std::fs::write(&path, text).unwrap();
    assert!(notices(&items(&Source::File(path))).is_empty());
}

#[test]
fn a_notice_without_text_is_named_by_its_kind_and_a_model_fallback_says_which_models() {
    let dir = tempfile::tempdir().unwrap();
    let mut refusal = base("system", 5);
    refusal["subtype"] = "model_refusal_no_fallback".into();
    refusal["level"] = "warning".into();
    let mut moved = base("assistant", 6);
    moved["message"] = json!({"id": "msg_f", "role": "assistant", "model": "claude-opus-4-8",
        "content": [{"type": "fallback", "from": {"model": "claude-fable-5"}, "to": {"model": "claude-opus-4-8"}}],
        "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}});
    let source = write(
        dir.path(),
        "n.jsonl",
        &[user_line(1, "hi"), line(refusal), line(moved)],
    );
    assert_eq!(
        notices(&items(&source)),
        [
            ("model_refusal_no_fallback", "model refusal no fallback"),
            (
                "model fallback",
                "Switched from claude-fable-5 to claude-opus-4-8"
            ),
        ]
    );
}

fn launch(agent: &str) -> Value {
    json!({"isAsync": true, "status": "async_launched", "agentId": agent})
}

fn child_path(dir: &Path, agent: &str) -> std::path::PathBuf {
    dir.join("projects/p")
        .join(CLAUDE_ID)
        .join("subagents")
        .join(format!("agent-{agent}.jsonl"))
}

fn append_calls(path: &Path, from: usize, n: usize) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for t in from..from + n {
        file.write_all(call_line(t + 1, &format!("g{t}"), "ls").as_bytes())
            .unwrap();
    }
}

fn revision_of(source: &Source) -> u64 {
    qoop_parsed::revision_of(source).unwrap()
}

#[test]
fn a_running_childs_progress_moves_the_count_and_the_revision_of_an_unchanged_parent() {
    let dir = tempfile::tempdir().unwrap();
    let source = claude_with_children(dir.path(), &[("run1", launch("run1"), 2)]);
    let first = digest(&source);
    assert_eq!(steps(&first.items), [2]);
    assert_eq!(revision_of(&source), first.revision, "stat and load agree");

    append_calls(&child_path(dir.path(), "run1"), 2, 3);
    let moved = revision_of(&source);
    assert_ne!(
        moved, first.revision,
        "the child's file is part of the revision"
    );
    let second = digest(&source);
    assert_eq!(steps(&second.items), [5]);
    assert_eq!(second.revision, moved);
}

#[test]
fn the_revision_is_the_same_from_every_page_of_one_file_state() {
    let dir = tempfile::tempdir().unwrap();
    let calls: Vec<(String, Value, usize)> = (0..30)
        .map(|n| (format!("a{n:03}"), launch(&format!("a{n:03}")), 1))
        .collect();
    let calls: Vec<(&str, Value, usize)> = calls
        .iter()
        .map(|(a, r, t)| (a.as_str(), r.clone(), *t))
        .collect();
    let source = claude_with_children(dir.path(), &calls);
    let page = |skip, limit| {
        qoop_parsed::load(
            &source,
            &qoop_parsed::Options {
                skip,
                limit,
                ..Default::default()
            },
        )
        .unwrap()
        .revision
    };
    let head = page(0, 5);
    assert_eq!((page(30, 5), page(0, 60)), (head, head));
    assert_eq!(revision_of(&source), head);
}

#[test]
fn a_count_is_remembered_by_the_childs_length_and_modification_time() {
    let dir = tempfile::tempdir().unwrap();
    let source = claude_with_children(dir.path(), &[("memo", launch("memo"), 4)]);
    assert_eq!(steps(&items(&source)), [4]);
    // Same length, same time, nothing to count: only the memo can still say 4.
    let child = child_path(dir.path(), "memo");
    let (len, modified) = {
        let meta = std::fs::metadata(&child).unwrap();
        (meta.len(), meta.modified().unwrap())
    };
    std::fs::write(&child, vec![b' '; len as usize]).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&child)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(steps(&items(&source)), [4]);
    // A different time is a different file.
    std::fs::File::options()
        .write(true)
        .open(&child)
        .unwrap()
        .set_modified(modified + std::time::Duration::from_secs(5))
        .unwrap();
    assert_eq!(steps(&items(&source)), [0]);
}

#[test]
fn a_big_child_is_counted_within_a_budget_once_and_the_second_load_costs_a_stat() {
    let dir = tempfile::tempdir().unwrap();
    let source = claude_with_children(dir.path(), &[("big1", launch("big1"), 0)]);
    let child = child_path(dir.path(), "big1");
    let call = call_line(1, "x", &"y".repeat(400));
    let line = |n: usize| call.replace("\"id\":\"x\"", &format!("\"id\":\"t{n}\""));
    let mut text = String::new();
    let mut n = 0;
    while text.len() + 700 < 8 * 1024 * 1024 {
        text.push_str(&line(n));
        n += 1;
    }
    std::fs::write(&child, &text).unwrap();
    assert!(text.len() < 8 * 1024 * 1024);

    let at = std::time::Instant::now();
    assert_eq!(steps(&items(&source)), [n as u32]);
    let first = at.elapsed();
    let at = std::time::Instant::now();
    assert_eq!(steps(&items(&source)), [n as u32]);
    let second = at.elapsed();
    assert!(first < std::time::Duration::from_secs(2), "{first:?}");
    assert!(second < std::time::Duration::from_millis(150), "{second:?}");

    // One byte over the cap is not read at all: the harness' number stands.
    std::fs::write(
        &child,
        format!("{text}{}", "z".repeat(8 * 1024 * 1024 - text.len() + 1)),
    )
    .unwrap();
    assert_eq!(steps(&items(&source)), [0]);
}

#[test]
fn the_childs_file_count_wins_over_the_number_the_result_reports_when_the_file_exists() {
    let dir = tempfile::tempdir().unwrap();
    let both = json!({"agentId": "both", "totalToolUseCount": 19});
    let source = claude_with_children(dir.path(), &[("both", both.clone(), 3)]);
    assert_eq!(steps(&items(&source)), [3]);
    std::fs::remove_file(child_path(dir.path(), "both")).unwrap();
    assert_eq!(steps(&items(&source)), [19]);
}

fn codex_tree(dir: &Path, layout: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let sessions = dir.join("sessions");
    let day = sessions.join(layout);
    std::fs::create_dir_all(&day).unwrap();
    let parent = day.join("rollout-2026-10-12T11-00-00-019f1a10-0003-7000-8000-000000000003.jsonl");
    std::fs::copy(fixture("codex/subagent-parent.jsonl"), &parent).unwrap();
    (sessions, parent)
}

fn plant_codex_child(dir: &Path) {
    let name = "rollout-2026-10-12T11-00-01-019f1a10-0004-7000-8000-000000000004.jsonl";
    std::fs::create_dir_all(dir).unwrap();
    std::fs::copy(fixture("codex/subagent-child.jsonl"), dir.join(name)).unwrap();
}

fn load_with_root(path: &Path, root: Option<&Path>) -> Vec<Item> {
    let opts = qoop_parsed::Options {
        codex_root: root.map(Path::to_path_buf),
        ..Default::default()
    };
    qoop_parsed::load(&Source::File(path.to_path_buf()), &opts)
        .unwrap()
        .items
}

#[test]
fn a_codex_file_outside_the_dated_layout_has_no_tree_to_look_in_unless_the_root_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, parent) = codex_tree(dir.path(), "flat");
    plant_codex_child(&sessions.join("2026/10/12"));
    assert_eq!(steps(&load_with_root(&parent, None)), [0, 0, 0]);
    let with_root = load_with_root(&parent, Some(&sessions));
    assert_eq!(steps(&with_root), [1, 0, 0]);
}

#[test]
fn a_codex_child_that_was_not_found_is_not_looked_for_again_for_half_a_minute() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, parent) = codex_tree(dir.path(), "2026/10/12");
    assert_eq!(steps(&load_with_root(&parent, None)), [0, 0, 0]);
    plant_codex_child(&sessions.join("2026/10/12"));
    assert_eq!(
        steps(&load_with_root(&parent, None)),
        [0, 0, 0],
        "remembered as missing"
    );
    // Another root is another question.
    let (other, other_parent) = codex_tree(&dir.path().join("b"), "2026/10/12");
    plant_codex_child(&other.join("2026/10/12"));
    assert_eq!(steps(&load_with_root(&other_parent, None)), [1, 0, 0]);
}

fn summary_of(dir: &Path, summary: &str) -> Option<String> {
    let mut boundary = base("system", 20);
    boundary["subtype"] = "compact_boundary".into();
    boundary["compactMetadata"] = json!({"trigger": "auto", "preTokens": 9000});
    let mut body = base("user", 21);
    body["isCompactSummary"] = true.into();
    body["message"] = json!({"role": "user", "content": summary});
    let source = write(
        dir,
        "s.jsonl",
        &[user_line(1, "hi"), line(boundary), line(body)],
    );
    items(&source).into_iter().find_map(|i| match i {
        Item::Compaction { text, .. } => text,
        _ => None,
    })
}

#[test]
fn a_claude_compaction_skips_its_fixed_opening_and_its_markers() {
    let dir = tempfile::tempdir().unwrap();
    let real = "This session is being continued from a previous conversation that ran out of context. \
The summary below covers the earlier portion of the conversation.\n\nSummary:\n\
1. Primary Request and Intent:\n   Ship the sweep.";
    assert_eq!(
        summary_of(dir.path(), real).as_deref(),
        Some("Ship the sweep."),
        "a numbered section heading says nothing, the line under it does"
    );
    let headed = "## Overview\n\nThe real first sentence.";
    assert_eq!(
        summary_of(dir.path(), headed).as_deref(),
        Some("The real first sentence.")
    );
    let long = format!("Goal: {}", "word ".repeat(80));
    let text = summary_of(dir.path(), &long).unwrap();
    assert!(
        text.chars().count() <= 160 && text.ends_with("..."),
        "{text}"
    );
}

#[test]
fn a_fallback_without_model_names_still_says_the_model_changed() {
    let dir = tempfile::tempdir().unwrap();
    let block = |content: Value, n: usize| {
        let mut moved = base("assistant", n);
        moved["message"] = json!({"id": format!("m{n}"), "role": "assistant", "model": "claude-opus-4-8",
            "content": [content], "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1}});
        line(moved)
    };
    let source = write(
        dir.path(),
        "f.jsonl",
        &[
            user_line(1, "hi"),
            block(
                json!({"type": "fallback", "to": {"model": "claude-opus-4-8"}}),
                5,
            ),
            block(json!({"type": "fallback"}), 6),
        ],
    );
    assert_eq!(
        notices(&items(&source)),
        [
            ("model fallback", "Switched model"),
            ("model fallback", "Switched model")
        ]
    );
}

#[test]
fn a_continued_in_record_is_a_row_only_when_it_names_the_next_session() {
    let dir = tempfile::tempdir().unwrap();
    let record = |next: Option<&str>, n: usize| {
        let mut v = json!({"type": "continued-in", "sessionId": CLAUDE_ID, "uuid": format!("c{n}"),
            "timestamp": "2026-10-12T09:00:05.000Z"});
        if let Some(next) = next {
            v["continuedInSessionId"] = next.into();
        }
        line(v)
    };
    let source = write(
        dir.path(),
        "c.jsonl",
        &[
            user_line(1, "hi"),
            record(None, 1),
            record(Some(""), 2),
            record(Some("5b2c0de0-aaaa-4bbb-8ccc-ddddeeeeffff"), 3),
        ],
    );
    assert_eq!(
        notices(&items(&source)),
        [("continued-in", "Continued in a new session")]
    );
}

#[test]
fn a_narrow_first_window_and_a_wide_later_one_name_the_same_children_in_the_revision() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("projects/p");
    let children = project.join(CLAUDE_ID).join("subagents");
    std::fs::create_dir_all(&children).unwrap();
    let mut lines = vec![user_line(1, "go")];
    for n in 0..60 {
        let agent = format!("w{n:03}");
        lines.extend(agent_call(
            10 + n * 10,
            &format!("toolu_{n}"),
            launch(&agent),
        ));
        for pad in 0..3 {
            lines.push(agent_line(12 + n * 10 + pad, &"pad ".repeat(300)));
        }
        std::fs::write(
            children.join(format!("agent-{agent}.jsonl")),
            call_line(1, &format!("k{n}"), "ls"),
        )
        .unwrap();
    }
    let source = write(&project, &format!("{CLAUDE_ID}.jsonl"), &lines);
    let revision = |skip, limit| {
        let opts = qoop_parsed::Options {
            max_tail_bytes: 8 * 1024,
            skip,
            limit,
            ..Default::default()
        };
        qoop_parsed::load(&source, &opts).unwrap().revision
    };
    assert_eq!(revision(0, 5), revision(40, 5));
    assert_eq!(revision(0, 5), revision_of(&source));
}

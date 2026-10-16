//! End-to-end over scratch product stores: locate + parse + window.
//! Hermetic: tempdirs only, never `$HOME`.

use qoop_history::{BaseDirs, Located, Product, Role, history, locate};
use std::path::Path;

fn dirs(root: &Path) -> BaseDirs {
    BaseDirs {
        claude: vec![root.join("projects")],
        codex: root.join("codex"),
        opencode: None,
        gemini: root.join("gemini"),
    }
}

fn claude_fixture() -> String {
    serde_json::json!({"type":"user","timestamp":"2026-06-10T10:00:00Z",
        "message":{"role":"user","content":"where is the bug"}})
    .to_string()
        + "\n"
        + &serde_json::json!({"type":"assistant","timestamp":"2026-06-10T10:00:01Z",
        "message":{"role":"assistant","content":[{"type":"text","text":"in the parser"}]}})
        .to_string()
        + "\n"
}

fn codex_fixture() -> String {
    let line = |ty: &str, payload: serde_json::Value| {
        serde_json::json!({"timestamp":"2026-09-04T13:24:40.608Z","ordinal":1,
            "type":ty,"payload":payload})
        .to_string()
    };
    format!(
        "{}\n{}\n{}\n",
        line(
            "session_meta",
            serde_json::json!({"session_id":"abc-1","cwd":"/x"})
        ),
        line(
            "event_msg",
            serde_json::json!({"type":"item_completed",
                "item":{"type":"UserMessage","id":"u1",
                    "content":[{"type":"text","text":"fix it"}]}})
        ),
        line(
            "event_msg",
            serde_json::json!({"type":"item_completed",
                "item":{"type":"AgentMessage","id":"a1",
                    "content":[{"type":"Text","text":"done"}]}})
        ),
    )
}

fn gemini_fixture() -> String {
    serde_json::json!({
        "sessionId": "gem-9",
        "projectHash": "h",
        "messages": [
            {"id":"1","timestamp":"2026-03-24T11:21:49.937Z","type":"user",
             "content":[{"text":"scrape it"}]},
            {"id":"2","timestamp":"2026-03-24T11:21:54.151Z","type":"gemini",
             "content":"on it"},
        ]
    })
    .to_string()
}

#[test]
fn claude_window_last_and_role() {
    let dir = tempfile::tempdir().unwrap();
    let slug = dir.path().join("projects").join("-Users-x");
    std::fs::create_dir_all(&slug).unwrap();
    std::fs::write(slug.join("sid-1.jsonl"), claude_fixture()).unwrap();
    let d = dirs(dir.path());

    let w = history("claude", Some("sid-1"), &d, 10, None).unwrap();
    assert_eq!(w.session.product, Product::Claude);
    assert_eq!(w.session.id, "sid-1");
    assert_eq!(w.messages.len(), 2);
    assert!(!w.truncated);
    assert_eq!(w.skipped_lines, 0);

    // --json schema golden: nested session pin, message rows, flags.
    let json = serde_json::to_value(&w).unwrap();
    assert_eq!(json["session"]["product"], "claude");
    assert_eq!(json["session"]["id"], "sid-1");
    assert_eq!(json["messages"][0]["role"], "user");
    assert!(json["messages"][0]["text"].as_str().is_some());
    assert!(json.get("truncated").is_some());
    assert_eq!(json["skipped_lines"], 0);

    let last = history("claude", Some("sid-1"), &d, 1, None).unwrap();
    assert_eq!(last.messages.len(), 1);
    assert_eq!(last.messages[0].text, "in the parser");

    let user = history("claude", Some("sid-1"), &d, 10, Some(Role::User)).unwrap();
    assert_eq!(user.messages.len(), 1);
    assert_eq!(user.messages[0].role, Role::User);
}

#[test]
fn codex_locates_by_in_file_id() {
    let dir = tempfile::tempdir().unwrap();
    // The real layout: day directories, and the session id as the name suffix.
    let day = dir.path().join("codex/2026/09/04");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(
        day.join("rollout-2026-09-04T13-24-40-abc-1.jsonl"),
        codex_fixture(),
    )
    .unwrap();
    let d = dirs(dir.path());

    let found = locate::locate(Product::Codex, "abc-1", &d).unwrap();
    assert!(matches!(found, Located::File(_)));

    let w = history("codex", Some("abc-1"), &d, 10, None).unwrap();
    assert_eq!(w.messages.len(), 2);
    assert_eq!(w.messages[0].text, "fix it");
    assert_eq!(w.messages[1].text, "done");
}

#[test]
fn gemini_matches_embedded_session_id() {
    let dir = tempfile::tempdir().unwrap();
    let chats = dir.path().join("gemini/h1/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(
        chats.join("session-2026-03-24T11-21-gem-9.json"),
        gemini_fixture(),
    )
    .unwrap();
    let d = dirs(dir.path());

    let w = history("gemini", Some("gem-9"), &d, 10, None).unwrap();
    assert_eq!(w.messages.len(), 2);
    assert_eq!(w.messages[1].role, Role::Assistant);
}

#[test]
fn missing_bad_and_unknown_ids_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let d = dirs(dir.path());

    // No hook report: refusal, never a guess.
    let err = history("claude", None, &d, 10, None).unwrap_err();
    assert!(matches!(err, qoop_history::Error::MissingId { .. }));

    // Path-shaped id: refusal before any filesystem touch.
    let err = history("claude", Some("../x"), &d, 10, None).unwrap_err();
    assert!(matches!(err, qoop_history::Error::BadId { .. }));

    // Well-shaped but absent: names product and id.
    let err = history("claude", Some("nope"), &d, 10, None).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("claude") && text.contains("nope"), "{text}");

    // Unknown program and reader-less product refuse by name.
    let err = history("not-a-program", Some("x"), &d, 10, None).unwrap_err();
    assert!(matches!(err, qoop_history::Error::UnknownProgram { .. }));
    let err = history("cursor", Some("x"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoop_history::Error::NoReader { .. }),
        "{err:?}"
    );
}

fn scratch_opencode_db(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, title TEXT,
            version TEXT, slug TEXT, time_created INTEGER, time_updated INTEGER, time_compacting INTEGER);
         CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
            time_updated INTEGER, data TEXT);
         CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
            time_created INTEGER, time_updated INTEGER, data TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session(id, project_id, directory, title, version, slug, time_created, time_updated, time_compacting)
         VALUES ('ses_9','global','/x','t','v','s',1000,2000,NULL)",
        [],
    )
    .unwrap();
    // Good user message.
    conn.execute(
        "INSERT INTO message(id, session_id, time_created, time_updated, data)
         VALUES ('m1','ses_9',1000,1001,'{\"role\":\"user\",\"time\":{\"created\":1000}}')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO part(id, message_id, session_id, time_created, time_updated, data)
         VALUES ('p1','m1','ses_9',1000,1001,'{\"type\":\"text\",\"text\":\"hello\"}')",
        [],
    )
    .unwrap();
    // Corrupt message row: counted, never fatal.
    conn.execute(
        "INSERT INTO message(id, session_id, time_created, time_updated, data)
         VALUES ('m2','ses_9',1002,1003,'{broken')",
        [],
    )
    .unwrap();
    // System role: skipped, never relabeled assistant.
    conn.execute(
        "INSERT INTO message(id, session_id, time_created, time_updated, data)
         VALUES ('m3','ses_9',1004,1005,'{\"role\":\"system\",\"time\":{\"created\":1004}}')",
        [],
    )
    .unwrap();
    drop(conn);
}

#[test]
fn opencode_unknown_id_is_not_found_and_rows_count() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    scratch_opencode_db(&db);
    let mut d = dirs(dir.path());
    d.opencode = Some(db);

    let err = history("opencode", Some("ses_nope"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoop_history::Error::NotFound { .. }),
        "unknown id is an error, never an empty window: {err:?}"
    );

    let w = history("opencode", Some("ses_9"), &d, 10, None).unwrap();
    assert_eq!(w.messages.len(), 1);
    assert_eq!(w.messages[0].text, "hello");
    assert_eq!(w.skipped_lines, 2, "corrupt row + system role counted");
    assert!(!w.truncated, "database reads are whole");
}

#[test]
fn empty_file_is_an_empty_window() {
    let dir = tempfile::tempdir().unwrap();
    let slug = dir.path().join("projects").join("-Users-x");
    std::fs::create_dir_all(&slug).unwrap();
    std::fs::write(slug.join("empty.jsonl"), "").unwrap();
    let d = dirs(dir.path());
    let w = history("claude", Some("empty"), &d, 10, None).unwrap();
    assert!(w.messages.is_empty());
    assert!(!w.truncated);
    assert_eq!(w.skipped_lines, 0);
}

#[test]
fn clipped_file_marks_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let slug = dir.path().join("projects").join("-Users-x");
    std::fs::create_dir_all(&slug).unwrap();
    // Past the 8 MiB tail window: ~9 MiB of small messages.
    let one =
        serde_json::json!({"type":"user","message":{"role":"user","content":"padding message"}})
            .to_string()
            + "\n";
    let path = slug.join("big.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    use std::io::Write;
    while f.metadata().unwrap().len() < 9 * 1024 * 1024 {
        f.write_all(one.as_bytes()).unwrap();
    }
    drop(f);
    let d = dirs(dir.path());
    let w = history("claude", Some("big"), &d, 5, None).unwrap();
    assert_eq!(w.messages.len(), 5);
    assert!(w.truncated, "the file extends past the window");
}

#[test]
fn oversize_gemini_file_is_not_a_match() {
    let dir = tempfile::tempdir().unwrap();
    let chats = dir.path().join("gemini/h/chats");
    std::fs::create_dir_all(&chats).unwrap();
    let path = chats.join("session-2026-01-01T00-00-bigone.json");
    let f = std::fs::File::create(&path).unwrap();
    f.set_len(9 * 1024 * 1024).unwrap();
    drop(f);
    let d = dirs(dir.path());
    // An oversize doc is found only when its head names the id (the file is not read whole
    // to find it). This one is all zeros, so the id is simply not found there.
    let err = history("gemini", Some("bigone"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoop_history::Error::NotFound { .. }),
        "{err:?}"
    );
}

/// The two refusals differ: a doc whose head holds the id is found, and then refused as too
/// large to read, not reported as missing.
#[test]
fn oversize_gemini_file_that_names_its_id_is_too_large_not_missing() {
    let dir = tempfile::tempdir().unwrap();
    let chats = dir.path().join("gemini/h/chats");
    std::fs::create_dir_all(&chats).unwrap();
    let id = "bigone12-3456-7890-abcd-ef1234567890";
    let path = chats.join("session-2026-01-01T00-00-bigone12.json");
    std::fs::write(&path, format!("{{\"sessionId\":\"{id}\",\"messages\":[]}}")).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(9 * 1024 * 1024).unwrap();
    drop(f);
    let d = dirs(dir.path());
    let err = history("gemini", Some(id), &d, 10, None).unwrap_err();
    assert!(matches!(err, qoop_history::Error::TooLarge), "{err:?}");
}

/// Gemini names a chat by the first eight characters of its id, so the lookup is by name
/// and no count of projects or chats can make it give up.
#[test]
fn gemini_is_found_past_any_number_of_projects() {
    let dir = tempfile::tempdir().unwrap();
    let gemini = dir.path().join("gemini");
    for i in 0..210 {
        std::fs::create_dir_all(gemini.join(format!("h{i}/chats"))).unwrap();
    }
    let id = "abcdef12-3456-7890-abcd-ef1234567890";
    let mut doc: serde_json::Value = serde_json::from_str(&gemini_fixture()).unwrap();
    doc["sessionId"] = id.into();
    std::fs::write(
        gemini.join("h209/chats/session-2026-03-05T02-42-abcdef12.json"),
        doc.to_string(),
    )
    .unwrap();
    let d = dirs(dir.path());
    let w = history("gemini", Some(id), &d, 10, None).unwrap();
    assert_eq!(w.messages.len(), 2);
}

#[test]
fn codex_first_line_beyond_probe_is_not_a_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("codex")).unwrap();
    // A 2 MiB first line with no newline: not a rollout session_meta.
    let mut data = vec![b'x'; 2 * 1024 * 1024];
    data.push(b'\n');
    std::fs::write(
        dir.path()
            .join("codex/rollout-2026-01-01T00-00-00-whatever.jsonl"),
        data,
    )
    .unwrap();
    let d = dirs(dir.path());
    let err = history("codex", Some("whatever"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoop_history::Error::NotFound { .. }),
        "{err:?}"
    );
}

/// A long history is not a reason to lose the session: past the old limits of
/// 200 directories, the lookup still finds an id that is in an old corner of it.
#[test]
fn a_long_history_still_locates_old_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    for i in 0..210 {
        std::fs::create_dir_all(projects.join(format!("slug-{i:03}"))).unwrap();
    }
    // The slug a listing returns last, which a scan that stops at a cap never reaches.
    let last = std::fs::read_dir(&projects)
        .unwrap()
        .last()
        .unwrap()
        .unwrap();
    std::fs::write(last.path().join("sid-1.jsonl"), claude_fixture()).unwrap();
    let codex = dir.path().join("codex/2026");
    for month in 1..=7 {
        for day in 1..=30 {
            std::fs::create_dir_all(codex.join(format!("{month:02}/{day:02}"))).unwrap();
        }
    }
    std::fs::write(
        codex.join("01/01/rollout-2026-01-01T09-00-00-abc-1.jsonl"),
        codex_fixture(),
    )
    .unwrap();
    let d = dirs(dir.path());

    let claude = history("claude", Some("sid-1"), &d, 10, None).unwrap();
    assert_eq!(claude.messages.len(), 2);
    let codex = history("codex", Some("abc-1"), &d, 10, None).unwrap();
    assert_eq!(codex.messages[0].text, "fix it");
}

/// A Claude transcript of the given lines, read back as the person's messages.
fn claude_typed(lines: &[serde_json::Value]) -> Vec<(String, Option<i64>)> {
    let dir = tempfile::tempdir().unwrap();
    let slug = dir.path().join("projects").join("-Users-x");
    std::fs::create_dir_all(&slug).unwrap();
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(slug.join("sid-q.jsonl"), body).unwrap();
    history(
        "claude",
        Some("sid-q"),
        &dirs(dir.path()),
        100,
        Some(Role::User),
    )
    .unwrap()
    .messages
    .into_iter()
    .map(|m| (m.text, m.ts))
    .collect()
}

/// The three shapes Claude Code writes its queue in, as found in real transcripts.
fn queue_op(op: &str, at: &str, content: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({"type":"queue-operation","operation":op,
        "timestamp":at,"sessionId":"sid-q"});
    if let Some(c) = content {
        v["content"] = c.into();
    }
    if op == "remove" {
        v["reason"] = "absorbed_mid_turn".into();
    }
    v
}

fn queued_command(at: &str, kind: &str, prompt: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"parentUuid":"p","isSidechain":false,"type":"attachment","uuid":"u",
        "timestamp":at,"attachment":{"type":"queued_command","prompt":prompt,
        "source_uuid":"s","commandMode":"prompt","origin":{"kind":kind},"timestamp":at,
        "humanTurn":true},
        "rendered":[{"content":"<system-reminder>\nThe user sent a new message while you were working:\n</system-reminder>"}]})
}

fn user_line(at: &str, content: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"type":"user","timestamp":at,"origin":{"kind":"human"},
        "message":{"role":"user","content":content}})
}

const T1: &str = "2026-10-04T00:55:09.976Z";
const T1_MS: i64 = 1_791_075_309_976;

/// What he typed mid-turn and Claude folded into the turn is his message, at its time;
/// the same mechanism carrying a notification or another session's message is not his.
#[test]
fn a_message_typed_while_the_agent_worked_is_his() {
    let typed = claude_typed(&[
        queue_op("enqueue", T1, Some("cv a digeri qass icinde go")),
        queue_op(
            "remove",
            "2026-10-04T00:55:13.719Z",
            Some("cv a digeri qass icinde go"),
        ),
        queued_command(T1, "human", "cv a digeri qass icinde go".into()),
        queued_command(
            "2026-10-04T00:56:00.000Z",
            "task-notification",
            "<task-notification>\n<task-id>b1</task-id>".into(),
        ),
        queued_command(
            "2026-10-04T00:57:00.000Z",
            "peer",
            "Another Claude session sent a message".into(),
        ),
        queued_command(
            "2026-10-04T00:58:00.000Z",
            "human",
            serde_json::json!([{"type":"text","text":"with a picture"},
                {"type":"image","source":{"type":"base64","data":"AA"}}]),
        ),
    ]);
    assert_eq!(
        typed,
        [
            ("cv a digeri qass icinde go".to_string(), Some(T1_MS)),
            ("with a picture".to_string(), Some(1_791_075_480_000)),
        ]
    );
}

/// A queued message no dequeue or remove took yet counts as typed when queued; one taken
/// counts once, by the line it became; a queued notification is nobody's.
#[test]
fn a_message_still_queued_counts_when_it_was_queued() {
    let typed = claude_typed(&[
        // An empty queue dequeued: nothing to take.
        queue_op("dequeue", "2026-10-04T00:55:05.016Z", None),
        queue_op(
            "enqueue",
            "2026-10-04T00:55:06.000Z",
            Some("<task-notification>x"),
        ),
        queue_op("enqueue", T1, Some("run it again")),
        // First in, first out: the notification, then the message.
        queue_op("dequeue", "2026-10-04T00:55:10.000Z", None),
        queue_op("dequeue", "2026-10-04T00:55:11.000Z", None),
        user_line("2026-10-04T00:55:11.001Z", "run it again".into()),
        queue_op(
            "enqueue",
            "2026-10-04T00:55:12.000Z",
            Some("<task-notification>y"),
        ),
        queue_op("enqueue", "2026-10-04T00:56:00.000Z", Some("still waiting")),
    ]);
    assert_eq!(
        typed,
        [
            // Taken as a turn: the time it was queued, not the time it was taken.
            ("run it again".to_string(), Some(T1_MS)),
            ("still waiting".to_string(), Some(1_791_075_360_000)),
        ]
    );
    // One still queued sits where it was queued, before what came after it.
    let typed = claude_typed(&[
        queue_op("enqueue", T1, Some("still waiting")),
        user_line("2026-10-04T00:56:00.000Z", "a later turn".into()),
    ]);
    assert_eq!(
        typed,
        [
            ("still waiting".to_string(), Some(T1_MS)),
            ("a later turn".to_string(), Some(1_791_075_360_000)),
        ]
    );
}

/// The summary a compacted session starts with and the marker of an interrupt ride on user
/// lines, and nobody typed them.
#[test]
fn a_compaction_summary_and_an_interrupt_are_not_his() {
    let mut summary = user_line(
        T1,
        "This session is being continued from a previous conversation that ran out of context."
            .into(),
    );
    summary["isCompactSummary"] = true.into();
    summary["isVisibleInTranscriptOnly"] = true.into();
    let typed = claude_typed(&[
        summary,
        user_line(
            "2026-10-04T00:55:10.000Z",
            serde_json::json!([{"type":"text","text":"[Request interrupted by user]"}]),
        ),
        user_line(
            "2026-10-04T00:55:11.000Z",
            "[Request interrupted by user for tool use]".into(),
        ),
        user_line("2026-10-04T00:55:12.000Z", "go on".into()),
    ]);
    assert_eq!(typed, [("go on".to_string(), Some(1_791_075_312_000))]);
}

/// Every queued text put back in the input box at once (`popAll`) was never sent.
#[test]
fn a_queue_popped_back_into_the_box_is_not_sent() {
    let typed = claude_typed(&[
        queue_op(
            "enqueue",
            T1,
            Some("ayrica o alt tarafta da path olabilir ya"),
        ),
        queue_op(
            "popAll",
            "2026-10-04T00:55:12.000Z",
            Some("ayrica o alt tarafta da path olabilir ya"),
        ),
    ]);
    assert_eq!(typed, []);
}

/// A user line from another origin (a peer session's message, a coordinator) is not the
/// person at this tab.
#[test]
fn a_line_from_another_origin_is_not_his() {
    let mut peer = user_line(T1, "Another Claude session sent a message: go".into());
    peer["origin"] = serde_json::json!({"kind":"peer"});
    let mut coordinator = user_line(T1, "Fix all of these".into());
    coordinator["origin"] = serde_json::json!({"kind":"coordinator"});
    let typed = claude_typed(&[
        peer,
        coordinator,
        user_line("2026-10-04T00:55:12.000Z", "go on".into()),
    ]);
    assert_eq!(typed, [("go on".to_string(), Some(1_791_075_312_000))]);
}

/// A message folded into the running turn keeps the time it was queued, not the time it was
/// folded: a write qass made 20 s ago stays a write of 20 s ago.
#[test]
fn a_message_folded_into_the_turn_keeps_its_queued_time() {
    let typed = claude_typed(&[
        queue_op("enqueue", T1, Some("run the tests")),
        queue_op("remove", "2026-10-04T00:55:24.000Z", Some("run the tests")),
        queued_command("2026-10-04T00:55:24.000Z", "human", "run the tests".into()),
    ]);
    assert_eq!(typed, [("run the tests".to_string(), Some(T1_MS))]);
}

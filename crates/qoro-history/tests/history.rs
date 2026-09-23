//! End-to-end over scratch product stores: locate + parse + window.
//! Hermetic — tempdirs only, never `$HOME`.

use qoro_history::{BaseDirs, Located, Product, Role, history, locate};
use std::path::Path;

fn dirs(root: &Path) -> BaseDirs {
    BaseDirs {
        claude: root.join("projects"),
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
    std::fs::create_dir_all(dir.path().join("codex")).unwrap();
    std::fs::write(dir.path().join("codex/rollout.jsonl"), codex_fixture()).unwrap();
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
        chats.join("session-2026-03-24T11-21-gem.json"),
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
    assert!(matches!(err, qoro_history::Error::MissingId { .. }));

    // Path-shaped id: refusal before any filesystem touch.
    let err = history("claude", Some("../x"), &d, 10, None).unwrap_err();
    assert!(matches!(err, qoro_history::Error::BadId { .. }));

    // Well-shaped but absent: names product and id.
    let err = history("claude", Some("nope"), &d, 10, None).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("claude") && text.contains("nope"), "{text}");

    // Unknown program and reader-less product refuse by name.
    let err = history("not-a-program", Some("x"), &d, 10, None).unwrap_err();
    assert!(matches!(err, qoro_history::Error::UnknownProgram { .. }));
    let err = history("cursor", Some("x"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoro_history::Error::NoReader { .. }),
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
        matches!(err, qoro_history::Error::NotFound { .. }),
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
    // Locate skips oversize docs, so the id is simply not found there.
    let err = history("gemini", Some("bigone"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoro_history::Error::NotFound { .. }),
        "{err:?}"
    );
}

#[test]
fn scan_caps_are_loud() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    for i in 0..210 {
        std::fs::create_dir_all(projects.join(format!("slug-{i}"))).unwrap();
    }
    let d = dirs(dir.path());
    let err = history("claude", Some("whatever"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoro_history::Error::ScanCap { .. }),
        "{err:?}"
    );
}

#[test]
fn codex_first_line_beyond_probe_is_not_a_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("codex")).unwrap();
    // A 2 MiB first line with no newline: not a rollout session_meta.
    let mut data = vec![b'x'; 2 * 1024 * 1024];
    data.push(b'\n');
    std::fs::write(dir.path().join("codex/huge.jsonl"), data).unwrap();
    let d = dirs(dir.path());
    let err = history("codex", Some("whatever"), &d, 10, None).unwrap_err();
    assert!(
        matches!(err, qoro_history::Error::NotFound { .. }),
        "{err:?}"
    );
}

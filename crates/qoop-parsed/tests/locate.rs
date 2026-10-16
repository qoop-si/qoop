//! Finding a session from a hook's report: one resolver, hostile ids, `CLAUDE_CONFIG_DIR`.

mod common;

use std::path::Path;

use common::*;
use qoop_parsed::{Dirs, Item, Source, locate_session};

fn dirs(root: &Path) -> Dirs {
    Dirs::under(root, None, None)
}

fn put_claude(projects: &Path, id: &str) -> std::path::PathBuf {
    let slug = projects.join("-work-demo");
    std::fs::create_dir_all(&slug).unwrap();
    let file = slug.join(format!("{id}.jsonl"));
    std::fs::copy(fixture("claude.jsonl"), &file).unwrap();
    file.canonicalize().unwrap()
}

#[test]
fn claude_is_found_under_claude_config_dir_when_it_is_set() {
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let in_cfg = put_claude(&cfg.path().join("projects"), CLAUDE_ID);

    let with_env = Dirs::under(home.path(), Some(cfg.path().into()), None);
    assert_eq!(
        locate_session("claude", CLAUDE_ID, &with_env),
        Some(Source::File(in_cfg))
    );
    assert_eq!(
        locate_session("claude", CLAUDE_ID, &dirs(home.path())),
        None,
        "the home has none"
    );

    let in_home = put_claude(&home.path().join(".claude/projects"), CLAUDE_ID);
    assert_eq!(
        locate_session("claude", CLAUDE_ID, &dirs(home.path())),
        Some(Source::File(in_home))
    );
}

#[test]
fn a_session_outside_the_config_dir_is_found_in_the_home_and_a_relative_dir_is_ignored() {
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let in_home = put_claude(&home.path().join(".claude/projects"), CLAUDE_ID);
    let with_cfg = Dirs::under(home.path(), Some(cfg.path().into()), None);
    assert_eq!(
        locate_session("claude", CLAUDE_ID, &with_cfg),
        Some(Source::File(in_home.clone()))
    );
    let relative = Dirs::under(home.path(), Some("relative/dir".into()), None);
    assert_eq!(
        locate_session("claude", CLAUDE_ID, &relative),
        Some(Source::File(in_home))
    );
}

#[test]
fn a_hostile_session_id_finds_nothing() {
    let home = tempfile::tempdir().unwrap();
    let projects = home.path().join(".claude/projects");
    put_claude(&projects, CLAUDE_ID);
    let data = home.path().join(".local/share/opencode");
    std::fs::create_dir_all(&data).unwrap();
    opencode_db(&data);
    let secret = home.path().join(".claude/secret.jsonl");
    std::fs::write(&secret, "{}\n").unwrap();
    let d = dirs(home.path());
    let hostile = [
        "../secret",
        "../../.claude/secret",
        &format!("{}/secret", home.path().join(".claude").display()),
        "/etc/passwd",
        "",
        "a/b",
        "..",
        ".",
        "id with space",
        "id\nnewline",
        &"x".repeat(200),
    ];
    for id in hostile {
        assert_eq!(locate_session("claude", id, &d), None, "{id:?}");
        assert_eq!(locate_session("codex", id, &d), None, "{id:?}");
        assert_eq!(locate_session("gemini", id, &d), None, "{id:?}");
        assert_eq!(locate_session("opencode", id, &d), None, "{id:?}");
    }
}

#[test]
fn a_transcript_that_links_outside_the_base_is_not_followed() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let projects = home.path().join(".claude/projects");
    let target = outside.path().join("stolen.jsonl");
    std::fs::copy(fixture("claude.jsonl"), &target).unwrap();

    let slug = projects.join("-linked-file");
    std::fs::create_dir_all(&slug).unwrap();
    std::os::unix::fs::symlink(&target, slug.join("file-link.jsonl")).unwrap();
    std::os::unix::fs::symlink(outside.path(), projects.join("-linked-dir")).unwrap();
    std::fs::copy(
        fixture("claude.jsonl"),
        outside.path().join("dir-link.jsonl"),
    )
    .unwrap();

    let d = dirs(home.path());
    assert_eq!(locate_session("claude", "file-link", &d), None);
    assert_eq!(locate_session("claude", "dir-link", &d), None);
    assert_eq!(locate_session("claude", "stolen", &d), None);
}

#[test]
fn an_unknown_program_has_no_session() {
    let home = tempfile::tempdir().unwrap();
    put_claude(&home.path().join(".claude/projects"), CLAUDE_ID);
    assert_eq!(locate_session("bash", CLAUDE_ID, &dirs(home.path())), None);
    assert!(locate_session("/usr/local/bin/claude", CLAUDE_ID, &dirs(home.path())).is_some());
}

#[test]
fn codex_and_gemini_sessions_are_found_and_load() {
    let home = tempfile::tempdir().unwrap();
    let codex_id = "01a132e5-5f39-7ad3-a316-d71d10e7c68b";
    let day = home.path().join(".codex/sessions/2026/10/13");
    std::fs::create_dir_all(&day).unwrap();
    let rollout = day.join(format!("rollout-2026-10-13T01-34-16-{codex_id}.jsonl"));
    std::fs::copy(fixture("codex/real-exit3.jsonl"), &rollout).unwrap();

    let gemini_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let chats = home.path().join(".gemini/tmp/hash/chats");
    std::fs::create_dir_all(&chats).unwrap();
    let chat = chats.join("session-2026-10-12T09-00-aaaaaaaa.jsonl");
    std::fs::copy(fixture("gemini/gemini-new-session.jsonl"), &chat).unwrap();

    let d = dirs(home.path());
    for (program, id, file) in [("codex", codex_id, rollout), ("gemini", gemini_id, chat)] {
        let source = locate_session(program, id, &d).unwrap_or_else(|| panic!("{program}"));
        assert_eq!(source, Source::File(file.canonicalize().unwrap()));
        assert!(!digest(&source).items.is_empty(), "{program}");
    }
}

#[test]
fn opencode_resolves_to_its_database_and_the_id() {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".local/share/opencode");
    std::fs::create_dir_all(&data).unwrap();
    let db = opencode_db(&data);
    let id = "ses_ecd6ad17ff80AEbRxnOg8B21Mn";
    let source = locate_session("opencode", id, &dirs(home.path())).expect("located");
    assert_eq!(source, Source::Opencode { db, id: id.into() });
    assert!(
        digest(&source)
            .items
            .iter()
            .any(|i| matches!(i, Item::Compaction { .. }))
    );
    assert_eq!(locate_session("opencode", "../x", &dirs(home.path())), None);
}

//! Redaction and the full text of one tool call.

mod common;

use common::*;
use qoop_parsed::{Error, Item, Options, Source, detail, load};

const SECRETS: [&str; 3] = ["sk-ant-api03-TOPSECRET", "sk-ant-xyz789", "abc123tok"];

#[test]
fn secrets_are_masked_in_summaries_text_and_detail() {
    let d = digest(&claude());
    let json = serde_json::to_string(&d).unwrap();
    for secret in SECRETS {
        assert!(!json.contains(secret), "digest leaks {secret}");
    }
    let Item::Tool { summary, .. } = tool(&d, "toolu_curl") else {
        unreachable!()
    };
    assert!(
        summary.contains("Bearer ***") && summary.contains("ANTHROPIC_API_KEY=***"),
        "{summary}"
    );

    let full = detail(&claude(), "toolu_curl", 64 * 1024).unwrap();
    for secret in SECRETS {
        assert!(!full.input.contains(secret) && !full.output.contains(secret));
    }

    let dir = tempfile::tempdir().unwrap();
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            agent_line(1, "use Authorization: Bearer abc123tok now"),
            call_line(2, "toolu_env", "env"),
            result_line(
                3,
                "toolu_env",
                "OPENAI_API_KEY=sk-ant-xyz789\nhome=/Users/x",
            ),
        ],
    );
    let d = digest(&source);
    assert!(matches!(&d.items[0], Item::Agent { text, .. } if !text.contains("abc123tok")));
    let out = detail(&source, "toolu_env", 1024).unwrap().output;
    assert_eq!(out, "OPENAI_API_KEY=***\nhome=/Users/x");
}

#[test]
fn detail_returns_input_and_output_and_cuts_at_the_limit() {
    let full = detail(&claude(), "toolu_build", 64 * 1024).unwrap();
    assert_eq!(
        (full.input.as_str(), full.truncated),
        ("cargo build", false)
    );
    assert!(full.output.contains("E0425"));

    let cut = detail(&claude(), "toolu_build", 10).unwrap();
    assert!(cut.truncated && cut.output.len() <= 10 && cut.input.len() <= 10);
    assert!(!cut.output.is_empty());
}

#[test]
fn a_call_without_output_has_empty_output_and_an_unknown_id_is_not_found() {
    let running = detail(&claude(), "toolu_last", 1024).unwrap();
    assert_eq!(
        (running.input.as_str(), running.output.as_str()),
        ("cargo test", "")
    );
    assert!(matches!(
        detail(&claude(), "toolu_nope", 1024),
        Err(Error::NotFound)
    ));
}

#[test]
fn detail_finds_a_call_older_than_the_digest_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = vec![
        call_line(1, "toolu_early", "make all"),
        result_line(2, "toolu_early", "built everything"),
    ];
    lines.extend((3..600).map(|n| agent_line(n, &"filler ".repeat(600))));
    let source = write(dir.path(), "s.jsonl", &lines);
    assert!(std::fs::metadata(dir.path().join("s.jsonl")).unwrap().len() > 2 * 1024 * 1024);

    let d = load(&source, &Options::default()).unwrap();
    assert!(
        d.has_older
            && !d
                .items
                .iter()
                .any(|i| matches!(i, Item::Tool { id, .. } if id == "toolu_early"))
    );
    let found = detail(&source, "toolu_early", 1024).unwrap();
    assert_eq!(
        (found.input.as_str(), found.output.as_str()),
        ("make all", "built everything")
    );
}

#[test]
fn an_orphan_result_has_a_detail_too() {
    let dir = tempfile::tempdir().unwrap();
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            user_line(1, "hello"),
            result_line(2, "toolu_gone", "the output of a call we never saw"),
        ],
    );
    let found = detail(&source, "toolu_gone", 1024).unwrap();
    assert_eq!(found.input, "");
    assert!(found.output.starts_with("the output"));
    assert!(matches!(
        detail(&Source::File(dir.path().join("none.jsonl")), "x", 1),
        Err(Error::NotFound)
    ));
}

#[test]
fn detail_reads_gemini_and_opencode_calls() {
    let gemini = Source::File(fixture("gemini/gemini-new-session.jsonl"));
    let found = detail(&gemini, "run_shell_command-1791795609000-aa11bb", 4096).unwrap();
    assert_eq!(found.input, "bun test");
    assert!(!found.output.is_empty());
    assert!(matches!(detail(&gemini, "nope", 1), Err(Error::NotFound)));

    let dir = tempfile::tempdir().unwrap();
    let db = opencode_db(dir.path());
    let source = Source::Opencode {
        db,
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    };
    let found = detail(&source, "call_demov201", 4096).unwrap();
    assert_eq!(found.input, "bun test");
    assert!(matches!(detail(&source, "nope", 1), Err(Error::NotFound)));
}

#[test]
fn a_subagent_row_opens_its_prompt_and_its_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&call_line(1, "toolu_agent", "x")).unwrap();
    v["message"]["content"][0]["name"] = "Agent".into();
    v["message"]["content"][0]["input"] = serde_json::json!({
        "description": "Count README lines",
        "prompt": "Count the lines of README.md and report the number.",
        "subagent_type": "general-purpose",
        "run_in_background": true,
    });
    let mut note: serde_json::Value = serde_json::from_str(&user_line(3, "x")).unwrap();
    note["message"]["content"] = "<task-notification>\n<task-id>a5ea2808c35ea93f7</task-id>\n<tool-use-id>toolu_agent</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Count README lines\" finished</summary>\n<result>README.md has 5 lines.</result>\n</task-notification>".into();
    note.as_object_mut().unwrap().remove("origin");
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            line(v),
            result_line(
                2,
                "toolu_agent",
                "Async agent launched successfully.\nagentId: a5ea2808c35ea93f7 (internal ID - do not mention to user.)",
            ),
            line(note),
        ],
    );
    let d = digest(&source);
    let (id, has_detail) = d
        .items
        .iter()
        .find_map(|i| match i {
            Item::Subagent { id, has_detail, .. } => Some((id.clone(), *has_detail)),
            _ => None,
        })
        .expect("a subagent row");
    assert_eq!((id.as_deref(), has_detail), (Some("toolu_agent"), true));
    let full = detail(&source, "toolu_agent", 64 * 1024).unwrap();
    assert_eq!(
        full.input,
        "Count the lines of README.md and report the number."
    );
    assert!(
        full.output.starts_with("README.md has 5 lines."),
        "{}",
        full.output
    );
}

//! What a digest holds, per agent, and how it is cut: seq, paging, caps, budget, orphans.

mod common;

use common::*;
use qoop_parsed::{Error, Item, Options, Source, Status, load};

fn seqs(items: &[Item]) -> Vec<u32> {
    items.iter().map(Item::seq).collect()
}

#[test]
fn a_claude_session_reads_as_rows() {
    let d = digest(&claude());
    assert_eq!(
        seqs(&d.items),
        (0..d.items.len() as u32).collect::<Vec<_>>()
    );
    assert_eq!((d.first_seq, d.has_older, d.warnings), (0, false, 0));
    assert!(matches!(&d.items[0], Item::User { text, .. } if text == "fix the build"));
    assert!(d.items.iter().any(|i| matches!(
        i,
        Item::Thinking {
            ms: None,
            text: None,
            ..
        }
    )));

    let Item::Tool {
        status,
        exit_code,
        summary,
        output_bytes,
        ..
    } = tool(&d, "toolu_build")
    else {
        unreachable!()
    };
    assert_eq!(
        (*status, *exit_code, summary.as_str()),
        (Status::Failed, Some(101), "cargo build")
    );
    assert!(*output_bytes > 0);
}

#[test]
fn a_call_without_a_result_is_never_ok() {
    let d = digest(&claude());
    let status = |id| match tool(&d, id) {
        Item::Tool { status, .. } => *status,
        _ => unreachable!(),
    };
    assert_eq!(
        status("toolu_read"),
        Status::Interrupted,
        "the run went on without it"
    );
    assert_eq!(
        status("toolu_last"),
        Status::Running,
        "the newest call may still be running"
    );
}

#[test]
fn slash_commands_attachments_and_subagents_have_their_own_rows() {
    let d = digest(&claude());
    assert!(
        d.items
            .iter()
            .any(|i| matches!(i, Item::User { text, .. } if text == "/model opus"))
    );
    assert!(d.items.iter().any(
        |i| matches!(i, Item::Notice { kind, text, .. } if kind == "command output" && text == "Set model to opus")
    ));
    assert!(
        d.items
            .iter()
            .any(|i| matches!(i, Item::Subagent { summary, .. } if summary == "Survey the tests"))
    );
    let chips = d.items.iter().find_map(|i| match i {
        Item::User { attachments, .. } if !attachments.is_empty() => Some(attachments),
        _ => None,
    });
    let chip = &chips.expect("a user row with a chip")[0];
    assert_eq!((chip.name.as_str(), chip.bytes), ("image/png", Some(70)));
}

#[test]
fn queued_input_has_no_entry_id_and_still_gets_a_seq() {
    let session = agentparse::parse_file(&fixture("claude.jsonl")).expect("parse");
    let last = session.entries.last().expect("entries");
    assert!(
        last.id.is_none(),
        "the fixture must hold an entry without an id"
    );
    let d = digest(&claude());
    let Some(Item::User { text, seq, .. }) = d.items.last() else {
        panic!("last item")
    };
    assert_eq!(
        (text.as_str(), *seq),
        ("also run clippy", d.items.len() as u32 - 1)
    );
}

#[test]
fn an_unknown_record_becomes_a_notice_with_its_kind_and_no_json() {
    let d = digest(&claude());
    let notice = d.items.iter().find_map(|i| match i {
        Item::Notice { kind, text, .. } if kind == "mystery-record" => Some(text),
        _ => None,
    });
    assert_eq!(
        notice.expect("a notice for the unknown kind"),
        "Record not shown here"
    );
}

#[test]
fn no_digest_carries_extra_or_raw_json() {
    let dir = tempfile::tempdir().unwrap();
    let db = opencode_db(dir.path());
    let opencode = |id: &str| Source::Opencode {
        db: db.clone(),
        id: id.into(),
    };
    // Per source: values the file holds (ids, cwd, branch, hash) that no row may carry.
    let cases = [
        (
            claude(),
            vec![
                "/cwd-only-marker",
                "gitbranch-only-marker",
                CLAUDE_ID,
                "u00001",
                "entrypoint",
            ],
        ),
        (
            Source::File(fixture("codex/real-exit3.jsonl")),
            vec!["01a132e5-5f39-7ad3-a316-d71d10e7c68b", "payload", "ordinal"],
        ),
        (
            Source::File(fixture("gemini/gemini-new-session.jsonl")),
            vec![
                "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
                "9f86d081884c7d659a2feaa0c55ad015",
                "projectHash",
            ],
        ),
        (
            Source::File(fixture("gemini/gemini-old-session.json")),
            vec!["11111111-2222-4333-8444-555555555555", "projectHash"],
        ),
        (
            opencode("ses_ecd6ad17ff80AEbRxnOg8B21Mn"),
            vec![
                "ses_ecd6ad17ff80AEbRxnOg8B21Mn",
                "a1b2c3d4e5f60718293a4b5c6d7e8f90",
            ],
        ),
        (
            opencode("ses_ed52a75fff9cZv864nVGS5jPGC"),
            vec![
                "ses_ed52a75fff9cZv864nVGS5jPGC",
                "a1b2c3d4e5f60718293a4b5c6d7e8f90",
            ],
        ),
    ];
    for (source, forbidden) in cases {
        let d = digest(&source);
        assert!(!d.items.is_empty(), "{source:?}");
        let json = serde_json::to_string(&d).unwrap();
        for word in forbidden
            .into_iter()
            .chain(["extra", "\"data\"", "\"raw\""])
        {
            assert!(!json.contains(word), "{source:?} carries {word}");
        }
    }
}

#[test]
fn codex_gemini_and_opencode_sessions_read_as_rows() {
    let codex = digest(&Source::File(fixture("codex/real-exit3.jsonl")));
    let Item::Tool {
        summary, status, ..
    } = &codex.items[2]
    else {
        panic!("codex tool")
    };
    assert_eq!(
        (summary.as_str(), *status),
        ("ls -la; sh test.sh; cat a.txt", Status::Ok)
    );

    let gemini = digest(&Source::File(fixture("gemini/gemini-new-session.jsonl")));
    let statuses: Vec<_> = gemini
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Tool { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        [
            Status::Failed,
            Status::Ok,
            Status::Failed,
            Status::Interrupted
        ]
    );
    assert!(gemini.items.iter().any(|i| matches!(i, Item::Error { .. })));
    let old = digest(&Source::File(fixture("gemini/gemini-old-session.json")));
    assert!(matches!(&old.items[0], Item::User { text, .. } if text.starts_with("Run the tests")));

    let dir = tempfile::tempdir().unwrap();
    let db = opencode_db(dir.path());
    let native = digest(&Source::Opencode {
        db,
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    });
    assert!(
        native
            .items
            .iter()
            .any(|i| matches!(i, Item::Compaction { .. }))
    );
    let shell = native.items.iter().find_map(|i| match i {
        Item::Tool {
            id,
            exit_code,
            duration_ms,
            ..
        } if id == "call_demov201" => Some((*exit_code, *duration_ms)),
        _ => None,
    });
    assert_eq!(shell, Some((Some(0), Some(780))));
}

#[test]
fn a_missing_opencode_session_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let db = opencode_db(dir.path());
    let err = load(
        &Source::Opencode {
            db,
            id: "ses_nope".into(),
        },
        &Options::default(),
    );
    assert!(matches!(err, Err(Error::NotFound)));
}

#[test]
fn a_window_that_cuts_a_call_off_from_its_result_widens_until_the_call_is_in() {
    let path = fixture("claude.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let at = text.find("\"tool_use_id\": \"toolu_build\"").unwrap();
    let line_start = text[..at].rfind('\n').unwrap() + 1;
    let opts = Options {
        max_tail_bytes: (text.len() - line_start) as u64,
        ..Options::default()
    };
    let d = load(&Source::File(path), &opts).unwrap();
    assert_eq!(
        d,
        digest(&claude()),
        "the same page as a read of the whole file"
    );
    let Item::Tool {
        status, exit_code, ..
    } = tool(&d, "toolu_build")
    else {
        unreachable!()
    };
    assert_eq!((*status, *exit_code), (Status::Failed, Some(101)));
    assert!(
        !d.items
            .iter()
            .any(|i| matches!(i, Item::Tool { name, .. } if name == "result"))
    );
}

#[test]
fn orphan_rows_at_a_window_start_are_not_served_even_when_the_window_has_enough_items() {
    let path = fixture("claude.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let at = text.find("\"tool_use_id\": \"toolu_build\"").unwrap();
    let from_result = (text.len() - (text[..at].rfind('\n').unwrap() + 1)) as u64;
    // Exactly as many items as the window holds, the orphan row among them.
    let in_window = {
        let session = agentparse::parse_file_tail(&path, from_result).unwrap();
        session.entries.len()
    };
    assert!(in_window > 5);
    let whole = digest(&claude()).items.len();
    let opts = Options {
        max_tail_bytes: from_result,
        limit: whole - 3,
        ..Options::default()
    };
    let d = load(&Source::File(path), &opts).unwrap();
    assert_eq!(d.items.len(), whole - 3);
    assert!(matches!(tool(&d, "toolu_build"), Item::Tool { name, .. } if name == "Bash"));
}

#[test]
fn a_tail_that_starts_inside_a_line_drops_that_line_quietly() {
    let path = fixture("claude.jsonl");
    let len = std::fs::metadata(&path).unwrap().len();
    let opts = Options {
        max_tail_bytes: len - 40,
        limit: 3,
        ..Options::default()
    };
    let d = load(&Source::File(path), &opts).unwrap();
    assert_eq!((d.warnings, d.has_older, d.items.len()), (0, true, 3));
}

/// A session of `n` numbered messages, each about 700 bytes.
fn numbered(dir: &std::path::Path, n: usize) -> Source {
    let lines: Vec<String> = (1..=n)
        .map(|i| {
            let text = format!("msg {i:04} {}", "pad ".repeat(150));
            if i % 2 == 1 {
                user_line(i, &text)
            } else {
                agent_line(i, &text)
            }
        })
        .collect();
    write(dir, "numbered.jsonl", &lines)
}

fn number(item: &Item) -> usize {
    match item {
        Item::User { text, .. } | Item::Agent { text, .. } => text[4..8].trim().parse().unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn skip_walks_back_to_the_file_start_across_widening_without_gaps_or_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let source = numbered(dir.path(), 300);
    let narrow = |skip| Options {
        max_tail_bytes: 8 * 1024,
        limit: 25,
        skip,
        ..Options::default()
    };
    let first = load(&source, &narrow(0)).unwrap();
    assert_eq!(first.items.len(), 25);
    assert!(first.has_older);

    let mut pages = Vec::new();
    let mut skip = 0;
    loop {
        let page = load(&source, &narrow(skip)).unwrap();
        assert_eq!(page.revision, first.revision);
        skip += page.items.len();
        let done = !page.has_older;
        pages.push(page);
        if done {
            break;
        }
        assert!(pages.len() < 20, "paging does not end");
    }
    let seen: Vec<usize> = pages
        .iter()
        .rev()
        .flat_map(|p| &p.items)
        .map(number)
        .collect();
    assert_eq!(
        seen,
        (1..=300).collect::<Vec<_>>(),
        "oldest to newest, once each"
    );
    assert_eq!(pages.len(), 12);
    assert_eq!(pages.last().unwrap().items.len(), 25);
    assert!(pages[..11].iter().all(|p| p.has_older));
}

#[test]
fn a_page_at_the_start_has_no_older_and_a_skip_past_the_end_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let source = numbered(dir.path(), 30);
    let opts = |skip, limit| Options {
        max_tail_bytes: 4 * 1024,
        skip,
        limit,
        ..Options::default()
    };
    let start = load(&source, &opts(25, 10)).unwrap();
    assert_eq!(
        start.items.iter().map(number).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert!(!start.has_older);
    let exact = load(&source, &opts(20, 10)).unwrap();
    assert_eq!(exact.items.len(), 10);
    assert!(!exact.has_older, "ten items remain before it, none after");
    let past = load(&source, &opts(10_000, 10)).unwrap();
    assert!(past.items.is_empty() && !past.has_older);
    let at_end = load(&source, &opts(30, 10)).unwrap();
    assert!(at_end.items.is_empty() && !at_end.has_older);
}

#[test]
fn a_page_far_back_in_a_big_file_widens_the_window_itself() {
    let dir = tempfile::tempdir().unwrap();
    let source = numbered(dir.path(), 6000); // about 4 MB
    let opts = Options {
        skip: 4000,
        limit: 10,
        ..Options::default()
    };
    let d = load(&source, &opts).unwrap();
    let numbers: Vec<usize> = d.items.iter().map(number).collect();
    assert_eq!(numbers, (1991..=2000).collect::<Vec<_>>());
    assert!(d.has_older);
}

#[test]
fn text_is_clamped_to_4096_bytes_and_summaries_to_200_chars() {
    let dir = tempfile::tempdir().unwrap();
    let long = "\u{e9}".repeat(10_000);
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            user_line(1, &long),
            agent_line(2, &long),
            call_line(3, "toolu_x", &format!("echo {}", "x".repeat(1000))),
        ],
    );
    let d = digest(&source);
    for item in &d.items {
        match item {
            Item::User { text, .. } | Item::Agent { text, .. } => {
                assert!(text.len() <= 4096, "{} bytes", text.len());
                assert!(text.ends_with("[truncated]"));
            }
            Item::Tool { summary, .. } => assert_eq!(summary.chars().count(), 200),
            _ => {}
        }
    }
    assert_eq!(d.items.len(), 3);
}

#[test]
fn the_byte_budget_drops_the_oldest_items_first() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (1..=30)
        .map(|n| agent_line(n, &format!("{n:02}{}", "a".repeat(4000))))
        .collect();
    let source = write(dir.path(), "s.jsonl", &lines);
    let budget = |max_total_bytes| Options {
        max_total_bytes,
        ..Options::default()
    };
    let d = load(&source, &budget(10_000)).unwrap();
    assert_eq!(d.items.len(), 2, "two 4 KB items fit 10 KB");
    assert_eq!(seqs(&d.items), [28, 29]);
    assert!(d.has_older);
    assert!(matches!(&d.items[1], Item::Agent { text, .. } if text.starts_with("30")));
    let one = load(&source, &budget(1)).unwrap();
    assert_eq!(seqs(&one.items), [29], "the newest item always comes back");
    let all = load(&source, &budget(usize::MAX)).unwrap();
    assert_eq!((all.items.len(), all.has_older), (30, false));
}

#[test]
fn a_whole_parsed_source_over_32_mib_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = 33 * 1024 * 1024;
    let doc = dir.path().join("chat.json");
    std::fs::File::create(&doc).unwrap().set_len(big).unwrap();
    assert!(matches!(
        load(&Source::File(doc), &Options::default()),
        Err(Error::TooLarge)
    ));

    let chat = dir.path().join("chat.jsonl");
    std::fs::write(
        &chat,
        "{\"sessionId\":\"s\",\"projectHash\":\"h\",\"kind\":\"main\"}\n",
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&chat)
        .unwrap()
        .set_len(big)
        .unwrap();
    assert!(matches!(
        load(&Source::File(chat), &Options::default()),
        Err(Error::TooLarge)
    ));
}

/// `head`, a hole of zeros up to `hole_to` bytes, then `tail`: a huge file whose end is real.
fn with_hole(path: &std::path::Path, head: &str, hole_to: u64, tail: &str) -> Source {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(head.as_bytes()).unwrap();
    file.seek(SeekFrom::Start(hole_to)).unwrap();
    file.write_all(format!("\n{tail}").as_bytes()).unwrap();
    Source::File(path.to_path_buf())
}

fn texts(d: &qoop_parsed::Digest) -> Vec<&str> {
    d.items
        .iter()
        .filter_map(|i| match i {
            Item::User { text, .. } | Item::Agent { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_huge_claude_file_is_read_by_its_tail_and_a_deep_page_widens_to_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let tail = format!(
        "{}{}",
        user_line(2, "a prompt at the end"),
        agent_line(3, "the reply")
    );
    let source = with_hole(
        &dir.path().join("big.jsonl"),
        &user_line(1, "the very first prompt"),
        40 * 1024 * 1024,
        &tail,
    );
    let near = Options {
        limit: 2,
        ..Options::default()
    };
    let d = load(&source, &near).unwrap();
    assert_eq!(texts(&d), ["a prompt at the end", "the reply"]);
    assert!(d.has_older);
    let deep = load(&source, &Options::default()).unwrap();
    assert_eq!(
        texts(&deep),
        ["the very first prompt", "a prompt at the end", "the reply"],
        "asking for more than the tail holds widens the window to the file start"
    );
    assert!(!deep.has_older);
}

#[test]
fn a_claude_file_that_mentions_project_hash_is_not_taken_for_gemini() {
    let dir = tempfile::tempdir().unwrap();
    let source = with_hole(
        &dir.path().join("big.jsonl"),
        // The key sits in the first record, nested: a Gemini header has it at the top.
        &user_line(1, "my projectHash and sessionId are in the text").replace(
            "\"isSidechain\"",
            "\"toolUseResult\": {\"projectHash\": \"abc\"}, \"isSidechain\"",
        ),
        33 * 1024 * 1024,
        &agent_line(2, "the reply"),
    );
    let d = load(
        &source,
        &Options {
            limit: 1,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(texts(&d), ["the reply"]);
}

#[test]
fn a_huge_codex_file_is_read_by_its_tail() {
    let dir = tempfile::tempdir().unwrap();
    let rollout = std::fs::read_to_string(fixture("codex/real-exit3.jsonl")).unwrap();
    let lines: Vec<&str> = rollout.lines().collect();
    let head = format!("{}\n", lines[0]);
    let tail = format!("{}\n", lines[lines.len() - 12..].join("\n"));
    let source = with_hole(
        &dir.path().join("big.jsonl"),
        &head,
        34 * 1024 * 1024,
        &tail,
    );
    let d = load(
        &source,
        &Options {
            limit: 1,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(d.items.len(), 1);
    assert!(d.has_older);
}

#[test]
fn beyond_the_widest_window_older_items_are_still_reported() {
    let dir = tempfile::tempdir().unwrap();
    let source = with_hole(
        &dir.path().join("huge.jsonl"),
        &user_line(1, "far back"),
        70 * 1024 * 1024,
        &agent_line(2, "the reply"),
    );
    let d = load(&source, &Options::default()).unwrap();
    assert_eq!(texts(&d), ["the reply"], "64 MiB is as far as a page reads");
    assert!(d.has_older);
}

#[test]
fn the_revision_follows_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(&path, user_line(1, "one")).unwrap();
    let source = Source::File(path.clone());
    let first = digest(&source).revision;
    assert_eq!(digest(&source).revision, first);

    // The same length with a later modification time is a new revision.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(120);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(later)
        .unwrap();
    let touched = digest(&source).revision;
    assert_ne!(touched, first, "mtime alone moves the revision");

    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&agent_line(2, "two"));
    std::fs::write(&path, text).unwrap();
    let grown = digest(&source).revision;
    assert!(grown != first && grown != touched);
}

#[test]
fn the_opencode_revision_follows_the_database_and_its_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let sql = std::fs::read_to_string(fixture("opencode/v2.sql")).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    conn.execute_batch(&sql).unwrap();
    let source = Source::Opencode {
        db: db.clone(),
        id: "ses_ecd6ad17ff80AEbRxnOg8B21Mn".into(),
    };
    let first = digest(&source).revision;
    assert_eq!(digest(&source).revision, first);
    let main_len = std::fs::metadata(&db).unwrap().len();
    conn.execute(
        "UPDATE session_v2 SET title = 'renamed, and longer than before' WHERE id = 'ses_ecd6ad17ff80AEbRxnOg8B21Mn'",
        [],
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&db).unwrap().len(),
        main_len,
        "only the wal grew"
    );
    assert_ne!(digest(&source).revision, first);
}

#[test]
fn error_and_notice_text_is_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let doc = std::fs::read_to_string(fixture("gemini/gemini-old-session.json"))
        .unwrap()
        .replace(
            "Example info notice.",
            "refresh with token=abc123secret please",
        )
        .replace(
            "[API Error: example error text]",
            "[API Error: Authorization: Bearer abc123secret rejected]",
        );
    let path = dir.path().join("chat.json");
    std::fs::write(&path, doc).unwrap();
    let d = digest(&Source::File(path));
    let json = serde_json::to_string(&d).unwrap();
    assert!(!json.contains("abc123secret"), "{json}");
    assert!(d.items.iter().any(
        |i| matches!(i, Item::Notice { text, .. } if text == "refresh with token=*** please")
    ));
    assert!(
        d.items
            .iter()
            .any(|i| matches!(i, Item::Error { text, .. } if text.contains("Bearer ***")))
    );
}

#[test]
fn model_tool_name_and_id_are_redacted_and_the_masked_id_still_finds_its_detail() {
    let dir = tempfile::tempdir().unwrap();
    let mut call: serde_json::Value =
        serde_json::from_str(&call_line(1, "sk-abc123secretid", "ls")).unwrap();
    call["message"]["model"] = "sk-abc123secretmodel".into();
    call["message"]["content"][0]["name"] = "sk-abc123secrettool".into();
    let mut said: serde_json::Value = serde_json::from_str(&agent_line(3, "done")).unwrap();
    said["message"]["model"] = "sk-abc123secretmodel".into();
    let source = write(
        dir.path(),
        "s.jsonl",
        &[
            line(call),
            result_line(2, "sk-abc123secretid", "files"),
            line(said),
        ],
    );
    let d = digest(&source);
    let json = serde_json::to_string(&d).unwrap();
    assert!(!json.contains("abc123secret"), "{json}");
    let Item::Tool { id, name, .. } = &d.items[0] else {
        panic!("tool row")
    };
    assert_eq!((id.as_str(), name.as_str()), ("***", "***"));
    assert!(matches!(d.items.last(), Some(Item::Agent { model: Some(m), .. }) if m == "***"));
    assert_eq!(
        qoop_parsed::detail(&source, id, 1024).unwrap().output,
        "files"
    );
}

#[test]
fn attachment_chips_are_capped_and_never_named_by_an_encoded_body() {
    let dir = tempfile::tempdir().unwrap();
    let image = |n: usize| serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}, "n": n});
    let mut blocks = vec![serde_json::json!({"type": "text", "text": "look at these"})];
    blocks.extend((0..11).map(image));
    let mut v: serde_json::Value = serde_json::from_str(&user_line(1, "x")).unwrap();
    v["message"]["content"] = blocks.into();
    let source = write(dir.path(), "s.jsonl", &[line(v)]);
    let d = digest(&source);
    let Item::User { attachments, .. } = &d.items[0] else {
        panic!("user row")
    };
    assert_eq!(attachments.len(), 9);
    assert_eq!(attachments[8].name, "+3");
    assert!(attachments[..8].iter().all(|c| c.name == "image/png"));
    assert!(!serde_json::to_string(&d).unwrap().contains("iVBOR"));
}

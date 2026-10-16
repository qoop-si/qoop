//! What a hostile or a newer peer may send: a huge `skip`, a variant this build lacks.

mod common;

use std::time::{Duration, Instant};

use common::*;
use qoop_parsed::{Item, MAX_SKIP, Options, Status, load};

/// A session of `turns` question and answer pairs, `pad` bytes of text each.
fn session(dir: &std::path::Path, turns: usize, pad: usize) -> qoop_parsed::Source {
    let filler = "x".repeat(pad);
    let lines: Vec<String> = (0..turns)
        .flat_map(|n| {
            [
                user_line(2 * n, &format!("q{n} {filler}")),
                agent_line(2 * n + 1, &format!("a{n} {filler}")),
            ]
        })
        .collect();
    write(dir, "s.jsonl", &lines)
}

#[test]
fn a_skip_past_the_oldest_item_is_an_empty_page_with_nothing_older() {
    let dir = tempfile::tempdir().unwrap();
    let source = session(dir.path(), 10, 10);
    for skip in [20, 21, 1000, usize::MAX] {
        let d = load(
            &source,
            &Options {
                skip,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(
            d.items.is_empty() && !d.has_older && d.first_seq == 0,
            "skip {skip}"
        );
    }
    // Everything but the newest item skipped: the first one is the page, nothing before it.
    let d = load(
        &source,
        &Options {
            skip: 19,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(d.items.len(), 1);
    assert!(!d.has_older);
}

/// `u32::MAX` once widened the parse to the widest window, 64 MiB, and then past the end. The
/// depth is clamped, so a hostile `skip` costs what `MAX_SKIP` costs, and reads the same page.
#[test]
fn a_huge_skip_is_clamped_and_costs_no_more_than_the_deepest_page() {
    let dir = tempfile::tempdir().unwrap();
    // About 12 MiB: more than one 1 MiB window, fewer items than a skip of 5000 needs twice.
    let source = session(dir.path(), 3000, 1900);
    let opts = |skip| Options {
        skip,
        limit: 60,
        ..Options::default()
    };
    let started = Instant::now();
    let huge = load(&source, &opts(usize::MAX)).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let deepest = load(&source, &opts(MAX_SKIP)).unwrap();
    assert_eq!(huge, deepest);
    assert_eq!(huge.items.len(), 60);
}

/// A daemon newer than the client sends a row kind and a status the client has not heard of.
/// The page still reads: the row is `Unknown`, which a client draws as one dim line.
#[test]
fn a_kind_or_status_from_a_newer_daemon_is_unknown_and_never_drops_the_page() {
    let page = r#"[
        {"type":"user","seq":1,"ts":null,"text":"hi","attachments":[]},
        {"type":"hologram","seq":2,"depth":3},
        {"type":"tool","seq":3,"id":"t","name":"Bash","summary":"ls","status":"paused",
         "exit_code":null,"duration_ms":null,"output_bytes":0,"has_detail":false}
    ]"#;
    let items: Vec<Item> = serde_json::from_str(page).expect("the page reads");
    assert!(matches!(items[0], Item::User { seq: 1, .. }));
    assert_eq!(items[1], Item::Unknown);
    assert!(matches!(
        &items[2],
        Item::Tool {
            status: Status::Unknown,
            ..
        }
    ));
    assert_eq!(items[1].seq(), 0);
}

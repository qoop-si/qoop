//! What `AlacrittyEngine::replay` is for: a daemon becoming another binary hands each
//! running terminal's screen to the next, which feeds the replay to a fresh engine. The
//! program never sees the replay and never repeats the modes it set once at its start, so
//! the fresh engine has to arrive at the same screen, the same cursor and the same modes.
//!
//! Compared against what the engine itself reports, over the conformance corpus, the three
//! captured traces (whole and in prefixes, since a session ends mostly on the main screen)
//! and the cases below that name the things a program sets once.

use std::path::PathBuf;

use qoro_terminal::{AlacrittyEngine, TerminalEngine};

fn fixtures(dir: &str) -> Vec<PathBuf> {
    let root: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", dir]
        .iter()
        .collect();
    let mut files: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("fixtures")
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

/// The corpus file's `\xHH` and `\\` escapes, the rest verbatim.
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    out.push(u8::from_str_radix(&hex, 16).expect("hex"));
                }
                _ => out.push(b'\\'),
            },
            c => out.extend_from_slice(c.to_string().as_bytes()),
        }
    }
    out
}

/// Feed `bytes`, replay into a new engine of the same size, and compare the two.
fn assert_replays(what: &str, cols: u16, rows: u16, bytes: &[u8]) {
    let mut first = AlacrittyEngine::new(cols, rows);
    first.feed(bytes);
    let replay = first.replay();
    let mut second = AlacrittyEngine::new(cols, rows);
    second.feed(replay.as_bytes());
    let (a, b) = (first.snapshot(), second.snapshot());
    assert_eq!(a.lines, b.lines, "{what}: the rows differ\n{replay:?}");
    assert_eq!(a.styles, b.styles, "{what}: the colours differ\n{replay:?}");
    assert_eq!(a.cursor, b.cursor, "{what}: the cursor differs\n{replay:?}");
    assert_eq!(a.modes, b.modes, "{what}: the modes differ\n{replay:?}");
    assert_eq!(
        first.mode_bits(),
        second.mode_bits(),
        "{what}: the engine's mode bits differ\n{replay:?}"
    );
    assert_eq!(first.title(), second.title(), "{what}: the title differs");
}

#[test]
fn every_conformance_case_replays_to_the_same_screen() {
    let mut checked = 0;
    for path in fixtures("vt_corpus") {
        let text = std::fs::read_to_string(&path).expect("case");
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}: ")))
                .map(str::to_string)
        };
        let (Some(cols), Some(rows), Some(input)) = (field("cols"), field("rows"), field("input"))
        else {
            continue;
        };
        assert_replays(
            &path.display().to_string(),
            cols.parse().expect("cols"),
            rows.parse().expect("rows"),
            &unescape(&input),
        );
        checked += 1;
    }
    assert!(checked > 40, "only {checked} cases were read");
}

#[test]
fn real_program_traces_replay_to_the_same_screen_at_every_prefix() {
    for path in fixtures("traces") {
        let full = std::fs::read(&path).expect("trace");
        for at in (1..=8).map(|i| full.len() * i / 8) {
            assert_replays(
                &format!("{} to byte {at}", path.display()),
                80,
                24,
                &full[..at],
            );
        }
    }
}

#[test]
fn what_a_program_sets_once_is_set_again() {
    let esc = "\x1b";
    let cases: &[(&str, String)] = &[
        (
            "an agent's startup modes",
            format!(
                "{esc}[?2004h{esc}[?1004h{esc}[?1000h{esc}[?1002h{esc}[?1006h{esc}[>1u{esc}[=5;1u\
                 {esc}[?25lhello"
            ),
        ),
        (
            "the alternate screen with a full frame",
            format!("{esc}[?1049h{esc}[2J{esc}[1;1Hframe one{esc}[3;5Hbottom{esc}[?25l"),
        ),
        (
            "colours of every kind, in runs",
            format!(
                "{esc}[1;31mred{esc}[0m {esc}[38;5;208morange{esc}[0m {esc}[48;2;1;2;3;38;2;250;240;230mrgb\
                 {esc}[0m {esc}[7minverse{esc}[0m {esc}[4:3mcurl{esc}[0m {esc}[2;3;9mdim{esc}[0m"
            ),
        ),
        (
            "wide characters and a combining mark",
            "ab\u{4e2d}\u{6587}cd\u{1f600}e\u{0301}x\r\nnext".to_string(),
        ),
        (
            "a link over some words",
            format!("see {esc}]8;;https://example.org/a{esc}\\this link{esc}]8;;{esc}\\ and more"),
        ),
        (
            "a title and a bar cursor",
            format!("{esc}]2;my agent{esc}\\{esc}[6 qtext"),
        ),
        (
            "the pen a program left set",
            format!("{esc}[1;32mgreen bold, still on"),
        ),
        (
            "a line that is exactly as wide as the screen, wrap owed",
            "x".repeat(40),
        ),
        (
            "application keypad, cursor keys and origin mode",
            format!("{esc}=\x1b[?1h{esc}[?6hrow"),
        ),
        (
            "insert and line feed modes",
            format!("{esc}[4h{esc}[20habc\r\ndef"),
        ),
    ];
    for (what, bytes) in cases {
        assert_replays(what, 40, 12, bytes.as_bytes());
    }
}

/// The wrap that is owed after the last column is owed again, so the next character
/// goes where it would have gone.
#[test]
fn the_next_character_lands_where_it_would_have() {
    let mut first = AlacrittyEngine::new(10, 4);
    first.feed(b"0123456789");
    let mut second = AlacrittyEngine::new(10, 4);
    second.feed(first.replay().as_bytes());
    first.feed(b"X");
    second.feed(b"X");
    assert_eq!(first.snapshot().lines, second.snapshot().lines);
    assert_eq!(first.snapshot().cursor, second.snapshot().cursor);
}

#[test]
fn a_replay_leaves_nothing_behind_for_the_history() {
    let mut first = AlacrittyEngine::new(20, 5);
    first.feed(b"one\r\ntwo\r\nthree");
    let mut second = AlacrittyEngine::new(20, 5);
    second.feed(first.replay().as_bytes());
    let scrolled = second.take_scrolled_off();
    assert!(
        scrolled.iter().all(|row| row.trim().is_empty()),
        "{scrolled:?}"
    );
}

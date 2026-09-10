//! VT conformance corpus runner for gate G1.24, and for ADR-003's re-evaluation gate.
//!
//! Each case lives as a small text file under `tests/fixtures/vt_corpus/`. Every
//! expected value in a case file is derived from a cited specification (ECMA-48, DEC
//! STD 070, xterm's ctlseqs, or the Unicode Standard), never from running this engine
//! and recording whatever it did: a case whose expected value cannot be justified from
//! a spec was dropped rather than invented.
//!
//! Case file format, one field per line, `key: value`, blank lines ignored:
//!
//!   id: a short stable identifier, also the file name
//!   name: a one-line description of what the case checks
//!   citation: the specification section or document this case's expected values come from
//!   cols: terminal width
//!   rows: terminal height
//!   input: the bytes fed to the engine, escaped with `\xHH` for control bytes and
//!          `\\` for a literal backslash; anything else (including a wide CJK
//!          character or a combining mark) is written verbatim as UTF-8
//!   cursor: expected cursor position after the feed, as `row,col`, both 0-based
//!   reply_contains: optional, a byte string that must appear in some Reply effect
//!   alt_screen: optional, `true` or `false`, the expected alternate-screen mode
//!   lines: introduces the expected visible grid, one `> ` prefixed line per row
//!
//! A case's grid content is a plain string per row (matching `Snapshot::lines`, which
//! is right-trimmed but otherwise plain text), so this corpus cannot see color or
//! attribute state; the SGR cases below check only what `Snapshot` can observe: that
//! an SGR sequence is absorbed as a non-printing control function rather than leaking
//! characters into the grid or moving the cursor.
//!
//! The score is not aspirational: `COMMITTED_SCORE` and `KNOWN_FAILING` below must
//! match the real result of running this engine against the corpus, and both are
//! cross-checked against `docs/vt-conformance.md` so the three cannot drift apart.

use std::fs;
use std::path::{Path, PathBuf};

use qoro_terminal::{AlacrittyEngine, TerminalEffect, TerminalEngine};

struct Case {
    id: String,
    name: String,
    citation: String,
    cols: u16,
    rows: u16,
    input: Vec<u8>,
    cursor: (usize, usize),
    lines: Vec<String>,
    reply_contains: Option<Vec<u8>>,
    alt_screen: Option<bool>,
}

/// The corpus file's own tiny escape syntax: `\xHH` for a control byte, `\\` for a
/// literal backslash, everything else passed through as UTF-8. This is not a VT
/// concept, just how the fixture text represents raw bytes.
fn unescape(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hi = chars.next().expect("incomplete \\x escape");
                let lo = chars.next().expect("incomplete \\x escape");
                let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16)
                    .unwrap_or_else(|e| panic!("invalid \\x escape \\x{hi}{lo}: {e}"));
                out.push(byte);
            }
            other => panic!("unknown escape \\{other:?}"),
        }
    }
    out
}

fn parse_case(path: &Path) -> Case {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"));

    let mut id = None;
    let mut name = None;
    let mut citation = None;
    let mut cols = None;
    let mut rows = None;
    let mut input = None;
    let mut cursor = None;
    let mut reply_contains = None;
    let mut alt_screen = None;
    let mut lines: Vec<String> = Vec::new();
    let mut in_lines = false;

    for raw_line in text.lines() {
        if in_lines {
            let content = raw_line
                .strip_prefix("> ")
                .or_else(|| raw_line.strip_prefix(">"))
                .unwrap_or_else(|| panic!("{path:?}: expected a '> ' line, got {raw_line:?}"));
            lines.push(content.to_string());
            continue;
        }
        if raw_line.is_empty() {
            continue;
        }
        let (key, value) = raw_line
            .split_once(": ")
            .unwrap_or((raw_line.trim_end_matches(':'), ""));
        match key {
            "id" => id = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "citation" => citation = Some(value.to_string()),
            "cols" => {
                cols = Some(
                    value
                        .parse()
                        .unwrap_or_else(|e| panic!("{path:?}: cols: {e}")),
                )
            }
            "rows" => {
                rows = Some(
                    value
                        .parse()
                        .unwrap_or_else(|e| panic!("{path:?}: rows: {e}")),
                )
            }
            "input" => input = Some(unescape(value)),
            "cursor" => {
                let (r, c) = value
                    .split_once(',')
                    .unwrap_or_else(|| panic!("{path:?}: cursor must be row,col"));
                cursor = Some((
                    r.parse()
                        .unwrap_or_else(|e| panic!("{path:?}: cursor row: {e}")),
                    c.parse()
                        .unwrap_or_else(|e| panic!("{path:?}: cursor col: {e}")),
                ));
            }
            "reply_contains" => reply_contains = Some(unescape(value)),
            "alt_screen" => alt_screen = Some(value == "true"),
            "lines" => in_lines = true,
            other => panic!("{path:?}: unknown field {other:?}"),
        }
    }

    let rows_n: u16 = rows.unwrap_or_else(|| panic!("{path:?}: missing rows"));
    assert_eq!(
        lines.len(),
        rows_n as usize,
        "{path:?}: rows says {rows_n} but {} expected-line rows were given",
        lines.len()
    );

    Case {
        id: id.unwrap_or_else(|| panic!("{path:?}: missing id")),
        name: name.unwrap_or_else(|| panic!("{path:?}: missing name")),
        citation: citation.unwrap_or_else(|| panic!("{path:?}: missing citation")),
        cols: cols.unwrap_or_else(|| panic!("{path:?}: missing cols")),
        rows: rows_n,
        input: input.unwrap_or_else(|| panic!("{path:?}: missing input")),
        cursor: cursor.unwrap_or_else(|| panic!("{path:?}: missing cursor")),
        lines,
        reply_contains,
        alt_screen,
    }
}

struct CaseResult {
    id: String,
    name: String,
    citation: String,
    passed: bool,
    detail: String,
}

fn contains_subslice(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

fn run_case(c: &Case) -> CaseResult {
    let mut engine = AlacrittyEngine::new(c.cols, c.rows);
    let (_damage, effects) = engine.feed(&c.input);
    let snapshot = engine.snapshot();
    let modes = engine.modes();

    let mut mismatches = Vec::new();

    if snapshot.cursor != c.cursor {
        mismatches.push(format!(
            "cursor: expected {:?}, got {:?}",
            c.cursor, snapshot.cursor
        ));
    }
    if snapshot.lines != c.lines {
        mismatches.push(format!(
            "lines: expected {:?}, got {:?}",
            c.lines, snapshot.lines
        ));
    }
    if let Some(expected) = &c.reply_contains {
        let found = effects.iter().any(|e| match e {
            TerminalEffect::Reply(b) => contains_subslice(b, expected),
            _ => false,
        });
        if !found {
            let replies: Vec<String> = effects
                .iter()
                .filter_map(|e| match e {
                    TerminalEffect::Reply(b) => Some(String::from_utf8_lossy(b).to_string()),
                    _ => None,
                })
                .collect();
            mismatches.push(format!(
                "reply: expected a Reply containing {:?}, got {:?}",
                String::from_utf8_lossy(expected),
                replies
            ));
        }
    }
    if let Some(expected) = c.alt_screen
        && modes.alt_screen != expected
    {
        mismatches.push(format!(
            "alt_screen: expected {expected}, got {}",
            modes.alt_screen
        ));
    }

    CaseResult {
        id: c.id.clone(),
        name: c.name.clone(),
        citation: c.citation.clone(),
        passed: mismatches.is_empty(),
        detail: mismatches.join("; "),
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vt_corpus")
}

fn doc_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/vt-conformance.md")
}

/// Cases this engine is known to fail today, each with the reason, cited in
/// `docs/vt-conformance.md`. If a case here starts passing, that is real progress, and
/// this test fails until the document, `KNOWN_FAILING` and `COMMITTED_SCORE` are all
/// updated together: the score can move, but never silently.
const KNOWN_FAILING: &[&str] = &["era-007"];

/// The floor. A run that scores below this has regressed and the build must fail.
/// Kept equal to `total cases - KNOWN_FAILING.len()` by the internal consistency
/// assertion below, and cross-checked against the score recorded in
/// `docs/vt-conformance.md`.
const COMMITTED_SCORE: usize = 56;

#[test]
fn vt_conformance_corpus() {
    let dir = corpus_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading corpus dir {dir:?}: {e}"))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "case"))
        .collect();
    paths.sort();

    assert!(
        paths.len() >= 50,
        "gate G1.24 requires at least 50 corpus cases, found {}",
        paths.len()
    );

    let cases: Vec<Case> = paths.iter().map(|p| parse_case(p)).collect();
    let results: Vec<CaseResult> = cases.iter().map(run_case).collect();

    let mut unexpected_failures = Vec::new();
    let mut unexpected_passes = Vec::new();
    let mut passed = 0usize;

    for r in &results {
        let known = KNOWN_FAILING.contains(&r.id.as_str());
        match (known, r.passed) {
            (false, true) => passed += 1,
            (false, false) => unexpected_failures.push(format!(
                "{} ({}) [{}]: {}",
                r.id, r.name, r.citation, r.detail
            )),
            (true, false) => {} // expected; does not count toward the score
            (true, true) => unexpected_passes.push(format!(
                "{} now passes; update docs/vt-conformance.md, KNOWN_FAILING and COMMITTED_SCORE",
                r.id
            )),
        }
    }

    assert!(
        unexpected_failures.is_empty(),
        "unexpected failures:\n{}",
        unexpected_failures.join("\n")
    );
    assert!(
        unexpected_passes.is_empty(),
        "regressions in the known-failing set:\n{}",
        unexpected_passes.join("\n")
    );
    assert_eq!(
        passed,
        cases.len() - KNOWN_FAILING.len(),
        "internal consistency: passed count must equal total minus known-failing"
    );
    assert!(
        passed >= COMMITTED_SCORE,
        "score regressed: {passed} < committed {COMMITTED_SCORE}"
    );

    let doc = fs::read_to_string(doc_path())
        .unwrap_or_else(|e| panic!("reading docs/vt-conformance.md: {e}"));
    let marker = format!("SCORE: {passed}/{}", cases.len());
    assert!(
        doc.contains(&marker),
        "docs/vt-conformance.md must record `{marker}`; update the document to match the committed score"
    );
}

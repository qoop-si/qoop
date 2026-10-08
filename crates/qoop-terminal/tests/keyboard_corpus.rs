//! Gate G1.3: every row of the keyboard corpus decodes to the key that produced it.
//!
//! The corpus is built by `scripts/keycapture/build-corpus.py` from what each of the three
//! named emulators publishes about itself, plus the encodings that are not per-terminal.
//! Every row carries its source, and the gate refuses to run on a corpus whose sources it
//! does not recognise, so a row cannot be added by hand without saying where it came from.

use std::collections::BTreeSet;

use qoop_terminal::keyboard::{Decoded, KeyCode, KeyKind, Modifiers, decode};

const CORPUS: &str = include_str!("fixtures/keyboard_corpus.tsv");

/// The three emulators M1 is scoped to, plus the rows that belong to no single one.
const EXPECTED_EMULATORS: [&str; 4] = ["any", "ghostty", "gnome-terminal", "iterm2"];

struct Row<'a> {
    emulator: &'a str,
    label: &'a str,
    raw: Vec<u8>,
    code: &'a str,
    mods: &'a str,
    kind: &'a str,
    source: &'a str,
}

fn rows() -> Vec<Row<'static>> {
    CORPUS
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 7, "malformed corpus row: {line}");
            Row {
                emulator: f[0],
                label: f[1],
                raw: (0..f[2].len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&f[2][i..i + 2], 16).expect("hex"))
                    .collect(),
                code: f[3],
                mods: f[4],
                kind: f[5],
                source: f[6],
            }
        })
        .collect()
}

fn expected_code(name: &str) -> KeyCode {
    match name {
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Insert" => KeyCode::Insert,
        "Delete" => KeyCode::Delete,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "KeypadEnter" => KeyCode::KeypadEnter,
        other => {
            if let Some(n) = other.strip_prefix('F')
                && let Ok(n) = n.parse::<u8>()
            {
                return KeyCode::F(n);
            }
            let mut chars = other.chars();
            let ch = chars.next().expect("a non empty code");
            assert!(chars.next().is_none(), "unknown key name {other}");
            KeyCode::Char(ch)
        }
    }
}

fn expected_mods(spec: &str) -> Modifiers {
    if spec == "none" {
        return Modifiers::NONE;
    }
    spec.split('+').fold(Modifiers::NONE, |acc, part| {
        acc.union(match part {
            "shift" => Modifiers::SHIFT,
            "ctrl" => Modifiers::CTRL,
            "alt" => Modifiers::ALT,
            other => panic!("unknown modifier {other}"),
        })
    })
}

fn expected_kind(spec: &str) -> KeyKind {
    match spec {
        "press" => KeyKind::Press,
        "repeat" => KeyKind::Repeat,
        "release" => KeyKind::Release,
        other => panic!("unknown kind {other}"),
    }
}

#[test]
fn the_corpus_covers_the_three_emulators_m1_is_scoped_to() {
    let rows = rows();
    assert!(rows.len() >= 400, "corpus has only {} rows", rows.len());
    let emulators: BTreeSet<&str> = rows.iter().map(|r| r.emulator).collect();
    assert_eq!(
        emulators.iter().copied().collect::<Vec<_>>(),
        EXPECTED_EMULATORS,
        "the corpus is not the three emulators the milestone names"
    );
    // Every row says where it came from, and the prefixes are the ones the builder emits.
    for row in &rows {
        assert!(
            row.source.starts_with("terminfo:")
                || row.source == "ascii"
                || row.source == "xterm-ctlseqs"
                || row.source.starts_with("capture:"),
            "row {} has an unrecognised source {}",
            row.label,
            row.source
        );
    }
}

#[test]
fn no_two_rows_disagree_about_the_same_bytes() {
    // The decoder is a function of the bytes, so two rows with the same bytes and
    // different expectations would make the gate unpassable by construction. Finding one
    // means the corpus has recorded an ambiguity as if it were a fact.
    let rows = rows();
    let mut seen: std::collections::BTreeMap<Vec<u8>, (&str, &str, &str)> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let expectation = (row.code, row.mods, row.kind);
        if let Some(previous) = seen.get(&row.raw) {
            assert_eq!(
                *previous, expectation,
                "{} and an earlier row disagree about the same bytes",
                row.label
            );
        }
        seen.insert(row.raw.clone(), expectation);
    }
}

#[test]
fn every_row_decodes_to_exactly_one_event() {
    let mut failures = Vec::new();
    for row in rows() {
        // `at_idle` is true because a corpus row is one complete key press with nothing
        // behind it, which is what a settled read looks like.
        match decode(&row.raw, true) {
            Decoded::Key(event, used) => {
                let want_code = expected_code(row.code);
                let want_mods = expected_mods(row.mods);
                let want_kind = expected_kind(row.kind);
                if event.code != want_code || event.mods != want_mods || event.kind != want_kind {
                    failures.push(format!(
                        "{} {}: decoded {:?}/{:?}/{:?}, expected {:?}/{:?}/{:?}",
                        row.emulator,
                        row.label,
                        event.code,
                        event.mods,
                        event.kind,
                        want_code,
                        want_mods,
                        want_kind
                    ));
                    continue;
                }
                if used != row.raw.len() {
                    failures.push(format!(
                        "{} {}: consumed {} of {} bytes",
                        row.emulator,
                        row.label,
                        used,
                        row.raw.len()
                    ));
                    continue;
                }
                // Zero duplicates: the row is spent, so a second call must not invent
                // another event out of the same key press.
                match decode(&row.raw[used..], true) {
                    Decoded::Incomplete => {}
                    other => failures.push(format!(
                        "{} {}: produced a second event {:?}",
                        row.emulator, row.label, other
                    )),
                }
            }
            other => failures.push(format!("{} {}: {:?}", row.emulator, row.label, other)),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of the corpus rows did not decode:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

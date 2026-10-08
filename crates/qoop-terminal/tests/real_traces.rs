//! ADR-003's re-evaluation gate, the half a specification cannot supply: what real programs
//! actually emit.
//!
//! The conformance corpus (`vt_conformance.rs`) is spec derived, one cited criterion per
//! case, and it is deliberately narrow: a case whose expected value cannot be justified from
//! ECMA-48, DEC STD 070, xterm's `ctlseqs.txt` or the Unicode Standard was dropped rather
//! than invented. That rule leaves real terminal traffic uncovered, because no specification
//! says what `vim` prints when it opens a file.
//!
//! What can be asserted about a real trace, without a second terminal to compare against, is
//! everything that must hold whatever the trace contains:
//!
//! 1. The program's own screen is on the screen. A trace that renders to nothing would make
//!    everything below it vacuous.
//! 2. Where the reads land cannot change the result. A daemon never chooses how a pty hands
//!    it bytes, and G1.4 asserts this against a generated stream. Here it is asserted
//!    against traffic that generator does not produce: a `DCS` termcap query from `vim`, a
//!    cursor position request, an alternate screen switch, and eighty three escape sequences
//!    in two kilobytes.
//! 3. Nothing that is a control function reaches the grid. A parser that loses its place
//!    leaves the tail of a sequence on screen, which is the failure a user sees first.
//! 4. The cursor stays inside the screen.
//!
//! Prefixes are used as well as whole traces, and the reason is what a session ends with:
//! `vim` and `less` run on the alternate screen and switch back on the way out, so the last
//! screen of those traces is nearly empty, and comparing only that would be comparing two
//! engines that had both arrived at nothing.
//!
//! The traces are captured, not written: `script -q` around the real program at 80 by 24,
//! with the keystrokes recorded in the table below. They are committed so the test is
//! reproducible rather than dependent on the versions installed here.

use std::path::PathBuf;

use qoop_terminal::{AlacrittyEngine, Snapshot, TerminalEngine, TerminalModes};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// A captured trace: the file, how it was produced, and something the program itself drew
/// that must be on the screen halfway through it.
struct Trace {
    file: &'static str,
    how: &'static str,
    halfway: &'static [&'static str],
}

const TRACES: &[Trace] = &[
    Trace {
        file: "vim.trace",
        how: "vim -u NONE -N on a three line file, driven with `jjA tail<esc>:wq<enter>`",
        halfway: &["hello world", "second line", "~"],
    },
    Trace {
        file: "less.trace",
        how: "less on a 200 line file, driven with `jjjjGq`",
        // Halfway through, `less` has scrolled: the window holds the file's lines 13 to 35
        // and the `:` prompt, which is what a paged file looks like and what an engine that
        // dropped a scroll would not produce.
        halfway: &["13", "35", ":"],
    },
    Trace {
        file: "top.trace",
        how: "top -l 2 -n 10 -s 1, two full samples",
        // `top`'s own header, which it redraws by cursor address rather than by scrolling.
        halfway: &["Processes:", "Load Avg:", "CPU usage:"],
    },
];

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "traces",
        name,
    ]
    .iter()
    .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn state(pieces: &[&[u8]]) -> (Snapshot, TerminalModes) {
    let mut engine = AlacrittyEngine::new(COLS, ROWS);
    for piece in pieces {
        engine.feed(piece);
    }
    (engine.snapshot(), engine.modes())
}

fn screen(snapshot: &Snapshot) -> String {
    snapshot.lines.join("\n")
}

/// Halfway through each trace, the program's own screen is on the screen.
///
/// Everything else in this file compares one rendering against another, and two renderings
/// of nothing are equal. This is the assertion that says there was something to render.
#[test]
fn a_real_trace_renders_what_the_program_drew() {
    for trace in TRACES {
        let full = fixture(trace.file);
        let (snapshot, _) = state(&[&full[..full.len() / 2]]);
        let text = screen(&snapshot);
        for wanted in trace.halfway {
            assert!(
                text.contains(wanted),
                "halfway through {} ({}) the screen does not contain {wanted:?}:\n{text}",
                trace.file,
                trace.how
            );
        }
    }
}

/// Where a read boundary is put cannot change what the screen ends up showing.
#[test]
fn a_real_trace_is_the_same_however_it_is_split() {
    for trace in TRACES {
        let full = fixture(trace.file);
        for cut in [full.len() / 2, full.len()] {
            let bytes = &full[..cut];
            let whole = state(&[bytes]);

            // Every boundary in the first half kilobyte, where the mode setting and the
            // queries are, and then a stride through the rest: every boundary in a seven
            // kilobyte trace is fifty megabytes of parsing for no more coverage than this.
            let boundaries: Vec<usize> = (1..bytes.len())
                .filter(|at| *at < 512 || at % 11 == 0)
                .collect();
            assert!(
                boundaries.len() > 100,
                "{} is too short to be worth splitting",
                trace.file
            );
            for at in boundaries {
                let split = state(&[&bytes[..at], &bytes[at..]]);
                assert_eq!(
                    whole.0, split.0,
                    "{} ({}) rendered differently when a read of its first {cut} bytes \
                     landed at byte {at}",
                    trace.file, trace.how
                );
                assert_eq!(
                    whole.1, split.1,
                    "{} ({}) left different modes when a read of its first {cut} bytes \
                     landed at byte {at}",
                    trace.file, trace.how
                );
            }
        }
    }
}

/// A control function never reaches the grid, and the cursor never leaves it.
#[test]
fn a_real_trace_leaves_no_control_bytes_on_the_screen() {
    for trace in TRACES {
        let full = fixture(trace.file);
        for cut in [
            full.len() / 4,
            full.len() / 2,
            full.len() * 3 / 4,
            full.len(),
        ] {
            let (snapshot, _) = state(&[&full[..cut]]);
            for (row, line) in snapshot.lines.iter().enumerate() {
                if let Some(bad) = line.chars().find(|c| c.is_control()) {
                    panic!(
                        "{} ({}) left {bad:?} on row {row} after {cut} bytes: {line:?}. A \
                         control function on the screen is a parser that lost its place.",
                        trace.file, trace.how
                    );
                }
            }
            let (row, col) = snapshot.cursor;
            assert!(
                row < snapshot.rows && col <= snapshot.cols,
                "{} ({}) left the cursor at {:?} outside a {} by {} screen",
                trace.file,
                trace.how,
                snapshot.cursor,
                snapshot.cols,
                snapshot.rows
            );
        }
    }
}

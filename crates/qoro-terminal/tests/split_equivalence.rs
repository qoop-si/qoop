//! Gate G1.4: a byte stream fed to the engine in one call, and the same stream fed split at
//! a boundary across two calls, must leave the engine in the same observable state, both at
//! the end and at the boundary itself.
//!
//! This matters because a daemon never gets to choose how a pty hands it bytes. A read() can
//! return in the middle of an escape sequence, in the middle of a UTF-8 character, or in the
//! middle of a control string this crate is already holding back (see `control_string.rs`).
//! If the engine's idea of "what happened" depended on where those reads happened to land,
//! two clients attached to the same terminal at different times could see different screens
//! from the same underlying byte stream.

use std::collections::BTreeSet;

use proptest::prelude::*;
use qoro_terminal::{AlacrittyEngine, Snapshot, TerminalEngine, TerminalModes};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// Everything a client of the daemon can ever observe about a terminal. Comparing this,
/// rather than internals, is deliberate: it is exactly the surface G1.4 is about.
fn state(e: &AlacrittyEngine) -> (Snapshot, TerminalModes) {
    (e.snapshot(), e.modes())
}

fn feed_new(data: &[u8]) -> AlacrittyEngine {
    let mut e = AlacrittyEngine::new(COLS, ROWS);
    e.feed(data);
    e
}

// --- Generator ---
//
// Uniform random bytes almost never assemble into a valid escape sequence (the odds of
// `any::<u8>()` spelling out `ESC [ <digits> m` by chance are negligible), so a generator
// built only from raw bytes would mostly prove that garbage falls through unchanged, which
// the engine already does trivially. Real terminal traffic is a mix of plain text and
// deliberately well-formed sequences, with an occasional cut or unterminated one, so this
// generator is built from weighted, shape-aware tokens, plus a small slice of true random
// bytes for unstructured coverage.

fn plain_text() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0x20u8..=0x7e, 1..6)
}

fn control_chars() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop::sample::select(vec![b'\r', b'\n', b'\t', 0x08u8, 0x07, 0x0c, 0x0b]),
        1..3,
    )
}

fn csi_sequence() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::sample::select(vec![
            b'm', b'H', b'J', b'K', b'A', b'B', b'C', b'D', b'n', b'h', b'l', b'r', b'G',
        ]),
        prop::collection::vec(0u16..200, 0..3),
        any::<bool>(),
    )
        .prop_map(|(fin, params, private)| {
            let mut out = vec![0x1b, b'['];
            if private {
                out.push(b'?');
            }
            let parts: Vec<String> = params.iter().map(|p| p.to_string()).collect();
            out.extend_from_slice(parts.join(";").as_bytes());
            out.push(fin);
            out
        })
}

/// `terminated` false is the deliberately unterminated form: it is what a payload looks like
/// while `ControlStringFilter` is still staging it, and the case that matters most right at
/// a split boundary.
fn osc_sequence(terminated: bool) -> impl Strategy<Value = Vec<u8>> {
    (
        0u8..12,
        prop::collection::vec(0x20u8..=0x7e, 0..6),
        any::<bool>(),
    )
        .prop_map(move |(code, text, use_bel)| {
            let mut out = vec![0x1b, b']'];
            out.extend_from_slice(code.to_string().as_bytes());
            out.push(b';');
            out.extend_from_slice(&text);
            if terminated {
                if use_bel {
                    out.push(0x07);
                } else {
                    out.extend_from_slice(b"\x1b\\");
                }
            }
            out
        })
}

fn dcs_or_apc(introducer: u8, terminated: bool) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0x20u8..=0x7e, 0..6).prop_map(move |payload| {
        let mut out = vec![0x1b, introducer];
        out.extend_from_slice(&payload);
        if terminated {
            out.extend_from_slice(b"\x1b\\");
        }
        out
    })
}

/// Each entry is the raw UTF-8 encoding of one non-ASCII character: two byte Latin
/// characters, three byte CJK characters, and a four byte character outside the BMP.
/// Spelled as bytes rather than as string literals so the source stays plain ASCII.
fn utf8_char() -> impl Strategy<Value = Vec<u8>> {
    prop::sample::select(vec![
        vec![0xc3, 0xa9],             // U+00E9, e with acute accent
        vec![0xc3, 0xb1],             // U+00F1, n with tilde
        vec![0xc3, 0xbc],             // U+00FC, u with diaeresis
        vec![0xe2, 0x82, 0xac],       // U+20AC, euro sign
        vec![0xe6, 0x97, 0xa5],       // U+65E5, CJK character
        vec![0xe6, 0x9c, 0xac],       // U+672C, CJK character
        vec![0xe4, 0xb8, 0xad],       // U+4E2D, CJK character
        vec![0xf0, 0x9f, 0x8e, 0x89], // U+1F389, party popper emoji
    ])
}

fn token() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        30 => plain_text(),
        10 => control_chars(),
        15 => csi_sequence(),
        10 => osc_sequence(true),
        6 => osc_sequence(false),
        4 => dcs_or_apc(b'P', true),
        4 => dcs_or_apc(b'P', false),
        4 => dcs_or_apc(b'_', true),
        4 => dcs_or_apc(b'_', false),
        10 => utf8_char(),
        8 => truncated_sequence(),
        4 => random_bytes(),
    ]
}

fn truncated_sequence() -> impl Strategy<Value = Vec<u8>> {
    prop::sample::select(vec![
        vec![0x1b],
        vec![0x1b, b'['],
        vec![0x1b, b'[', b'1', b';'],
        b"\x1b]0;partial".to_vec(),
        vec![0xc3],       // a lone UTF-8 lead byte, never completed
        vec![0xf0, 0x9f], // half of a four byte character
        vec![0x1b, b'P'], // a DCS introducer with no payload and no terminator
    ])
}

fn random_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 1..8)
}

fn byte_stream() -> impl Strategy<Value = Vec<u8>> {
    // Kept modest on purpose: a fresh `AlacrittyEngine` allocates a full scrollback, and the
    // short-stream branch of `split_points` is exhaustive (every boundary), so the average
    // case length is what keeps 10 000 cases inside a reasonable time budget, not the cap
    // on long-stream sampling alone.
    prop::collection::vec(token(), 1..7).prop_map(|tokens| tokens.concat())
}

/// Split points to check for one generated stream.
///
/// A stream of length `n` has `n - 1` interior boundaries. Checking every one of them for
/// every one of 10 000 cases is affordable only while `n` is small, because each checkpoint
/// costs a fresh reference engine fed the whole prefix, so exhaustive checking is quadratic
/// in `n`. Below 64 bytes that quadratic cost is trivial, so every boundary is checked. Above
/// it, a bounded random sample stands in for exhaustiveness, plus both boundaries next to
/// every ESC byte, because that is where a resumed parser is most likely to disagree with
/// itself: it is the one byte whose meaning depends entirely on what follows it.
fn split_points(data: &[u8], seeds: &[usize]) -> Vec<usize> {
    let n = data.len();
    if n < 2 {
        return Vec::new();
    }
    let mut points = BTreeSet::new();
    if n <= 64 {
        // Every boundary, exactly as specified: not capped, since n is small enough that
        // this is cheap.
        points.extend(1..n);
        return points.into_iter().collect();
    }
    for &s in seeds {
        points.insert(1 + s % (n - 1));
    }
    for (i, &b) in data.iter().enumerate() {
        if b == 0x1b {
            if i >= 1 {
                points.insert(i);
            }
            if i + 1 < n {
                points.insert(i + 1);
            }
        }
    }
    // Capped only here: bounds the O(n) reference feed that each checkpoint costs, so one
    // pathological long case cannot blow the suite's time budget on its own.
    points.into_iter().take(16).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    /// G1.4: split-boundary equivalence, with a checkpoint at every boundary tested, not
    /// only at the end.
    ///
    /// The split engine is fed progressively through every chosen boundary in one pass,
    /// which is what "split at every boundary" means for a stream that arrives through many
    /// small reads rather than one. It is also a stronger test than checking each boundary
    /// as an isolated two-way split, and cheaper: a fresh engine allocates a full scrollback
    /// (`AlacrittyEngine::new`, see `alacritty.rs`), so reusing one split engine across every
    /// boundary in a case, rather than building a new one per boundary, is what keeps 10 000
    /// cases with exhaustive short-stream coverage inside a reasonable time budget.
    #[test]
    fn split_feed_matches_whole_feed(
        data in byte_stream(),
        seeds in prop::collection::vec(any::<usize>(), 0..16),
    ) {
        let whole_final = state(&feed_new(&data));
        let points = split_points(&data, &seeds);

        let mut split = AlacrittyEngine::new(COLS, ROWS);
        let mut fed = 0usize;

        for point in points {
            split.feed(&data[fed..point]);
            fed = point;

            // The checkpoint: state after feeding exactly this many bytes, fed as the
            // entire input to a fresh engine, is the reference for "what feeding that much
            // whole would look like". A prefix cut inside an incomplete sequence is
            // expected to show that sequence not yet applied, in both the reference and the
            // split feed: the engine holds an incomplete escape sequence and an incomplete
            // character back from the parser either way, and that is correct behaviour, not
            // a bug the test should paper over.
            let reference_mid = state(&feed_new(&data[..point]));
            let split_mid = state(&split);
            prop_assert_eq!(
                split_mid,
                reference_mid,
                "state after reaching byte {} must match feeding that prefix whole",
                point
            );
        }

        split.feed(&data[fed..]);
        let split_final = state(&split);
        prop_assert_eq!(
            split_final,
            whole_final,
            "final state must not depend on where the stream was cut"
        );
    }
}

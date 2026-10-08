# VT conformance corpus

Gate G1.24 requires the corpus to exist with at least
50 cases and a committed score. This document is that record.

Engine: `alacritty_terminal` 0.26.0 (see `Cargo.lock`).
Date: 2026-09-10.
Runner: `crates/qoop-terminal/tests/vt_conformance.rs`.
Corpus: `crates/qoop-terminal/tests/fixtures/vt_corpus/` (57 cases).

Run with:

```
cargo test -p qoop-terminal --test vt_conformance
```

## Methodology

Every case's expected cursor position and visible grid is derived from a cited
specification (ECMA-48, DEC STD 070, xterm's `ctlseqs.txt`, or the Unicode Standard),
never from running this engine and recording whatever it produced. A case is dropped
rather than kept if no specification justifies its expected value. The citation for
each case's expected values is in its fixture file, under `tests/fixtures/vt_corpus/`.

`Snapshot` (`crates/qoop-terminal/src/engine.rs`) carries only plain per-row text and
a cursor position, no color or attribute state. The SGR (character attribute) cases
are scoped to what that snapshot can actually observe: that an SGR sequence is
absorbed as a non-printing control function rather than leaking parameter digits into
the grid or moving the cursor. They do not check color or boldness, because the
engine seam does not expose it yet.

## Score

SCORE: 56/57

56 of 57 cases pass. The one failing case is a real, spec-cited finding, not a flaky
or approximate result, and the test suite fails outright if it starts passing without
this document being updated to match (see `KNOWN_FAILING` in the runner).

| id | citation | what differs |
|---|---|---|
| era-007 | xterm ctlseqs, CSI Ps J ED (Ps=1 Erase Above): erase from screen start through cursor, inclusive | When the cursor is on the second screen row (row index 1) and ED 1 (erase above) runs, the engine clears only the current row up to the cursor column; the row above the cursor (row index 0) is left untouched instead of being fully cleared. The same erase at cursor row index 2 or below clears correctly (see `era-002`), so the gap is specific to cursor row index 1. |

## Coverage

Families covered, with case count:

| family | cases |
|---|---|
| cursor movement and bounds clamping | 7 |
| erase in display / erase in line (ED, EL) | 7 |
| insert/delete line and character (IL, DL, ICH, DCH) | 5 |
| scroll regions (DECSTBM) | 5 |
| tab stops (HTS, TBC, CHT, CBT) | 5 |
| saved cursor (DECSC/DECRC, CSI s/u) | 3 |
| character attributes and reset (SGR) | 4 |
| alternate screen (1049) | 3 |
| autowrap (DECAWM) and pending wrap | 4 |
| origin mode (DECOM) | 3 |
| device status report and primary DA | 3 |
| character sets (G0 line drawing) | 3 |
| Unicode width (wide characters, combining marks) | 3 |
| unsupported graphics leaving adjacent text intact | 2 |

## Re-evaluation gate

ADR-003 names the following as not covered by the original spike and to be measured
here: Unicode width and grapheme clustering, alternate screen, scroll margins, tab
stops, saved cursor, character sets, terminal query and reply handling, and
unsupported graphics sequences. All are covered above.

Two things named alongside them are deliberately not corpus cases.

**Captured traces from real applications** are in a separate test,
`crates/qoop-terminal/tests/real_traces.rs`, because their expected values cannot come
from a specification: no document says what `vim` prints when it opens a file. What
that test asserts instead is everything that must hold whatever the trace contains.
The program's own screen is on the screen halfway through, so nothing below it is
vacuous. Where a read boundary falls does not change the result, asserted at every
byte of the first half kilobyte and every eleventh byte after that, over `vim`,
`less` and `top` at 80 by 24, captured with `script -q`. No control function reaches
the grid. The cursor stays inside the screen. The traces carry traffic the generated
stream in gate G1.4 does not produce: a `DCS` termcap query, a cursor position
request, an alternate screen switch and eighty three escape sequences in two
kilobytes.

**Grapheme clustering beyond a single combining mark** (emoji ZWJ sequences, flag
emoji) has no case here, and the reason is the corpus rule rather than an oversight.
UAX #11 assigns East Asian Width per code point, UTS #51 describes emoji presentation,
and neither fixes how many terminal cells a cluster occupies; real terminals disagree
with each other on exactly these sequences. A case would therefore encode a choice
rather than a criterion, and the rule for this corpus is that a case whose expected
value cannot be justified from a specification is dropped.

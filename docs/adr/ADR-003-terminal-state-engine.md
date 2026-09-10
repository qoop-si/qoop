# ADR-003: Terminal state engine

Status: accepted for milestone 1 and 2, re-evaluation gate at the end of milestone 2
Date: 2026-09-09
Evidence: `spikes/spike-runtime/src/term.rs` and `src/mem.rs`, macOS 15, arm64, Rust 1.98
Reproduce: `./scripts/regen-spike-results.sh` (exits 0)

## Context
Writing a VT emulator is an explicit non-goal. A comparable multiplexer vendors
a VT engine, a Zig C library, behind roughly 8400 lines of FFI bindings, and
carries two local Zig patches with a documented removal condition. That buys best-in-class
VT correctness at the cost of a Zig toolchain in the build, an FFI surface with 176
`unsafe` sites, and a re-patch burden on every upstream bump.

## Options
1. `alacritty_terminal` 0.26. Pure Rust, ships in Alacritty, Apache-2.0, has built-in
   damage tracking.
2. Vendored `libghostty-vt` through FFI. Best correctness, Zig in the build.
3. `vt100` or `avt`. Simpler, smaller feature surface.
4. Write our own. Rejected by the non-goals list.

## Measurements

`spike-term`, `alacritty_terminal` 0.26:

The transcript is in `docs/spike-results.md`, generated from the program's stdout
rather than typed, and deliberately not copied here: a review found hand-edited numbers in
exactly this kind of inline copy, so the repository keeps one generated copy of every
transcript and ADRs cite it by section.

The damage result matters most. Damage is line-precise: writing one cell at row 5 reports exactly
line 4 plus the cursor line, not a full-screen repaint. That is the primitive the
per-client rendering and hidden-pane policy are built on, and it is available without
writing it ourselves.

Throughput has two orders of magnitude of headroom over a chatty agent, which is what
matters; the exact figure varies per run and is in the generated transcript.

## Decision
Use `alacritty_terminal` for milestones 1 and 2, strictly behind:

```rust
pub trait TerminalEngine: Send {
    fn feed(&mut self, bytes: &[u8]) -> Damage;
    fn resize(&mut self, cols: u16, rows: u16);
    fn snapshot(&self) -> Snapshot;
    fn modes(&self) -> TerminalModes;
}
```

No type outside `qoro-terminal` may name a concrete engine type. That is what keeps this
decision reversible.

### The engine seam must carry effects, not only damage

The spike's event listener discarded every event. That is a latent hang: an application
that emits a Device Status Report or a primary Device Attributes query waits for a reply,
and if the engine's outbound event is dropped, the application blocks forever while every
check in the spike still passes.

The trait therefore returns effects alongside damage:

```rust
pub enum TerminalEffect {
    Reply(Bytes),            // must be written back to the PTY, ordered ahead of user input
    Title(String),
    Bell,
    ClipboardWrite(Bytes),   // policy gated, never automatic
    ColorSchemeQuery,
}
```

`Reply` has priority over user input in the PTY write queue and has reserved capacity, so
a saturated input queue can never deadlock an application waiting for a terminal reply.
A test drives a real `printf '\033[6n'` and asserts the response arrives.

### What a snapshot must contain

A grid plus cursor plus modes is not sufficient state to resume feeding bytes. A snapshot
taken between `ESC [` and the rest of its parameters loses parser state, and alternate
screen, scroll margins, tab stops, saved cursor and character sets all change how later
bytes render.

Decision: a client consumes **authoritative cell state**, never a raw byte tail that it
parses itself. The daemon owns the only parser. The snapshot the daemon keeps for its own
restart is a complete engine checkpoint including parser state, and the property test
feeds a byte stream split at every boundary, checkpointing at each split, and asserts the
final grid is identical to the unsplit feed.

### Memory: the byte budget is withdrawn, and here is the real floor

The first version of this section proposed bounding the in-memory ring by bytes, deriving a
line allowance from a byte budget. That does not work, and the reason is in the engine's
storage layer rather than in the arithmetic.

`grid/storage.rs` defines `MAX_CACHE_SIZE = 1_000` and grows the backing vector by
`max(additional_rows, MAX_CACHE_SIZE)` fully materialised rows. So the first line that
scrolls allocates a thousand rows regardless of the configured scrollback, and cost is a
step function with a thousand-row step. Measured, one configuration per process, current
resident size rather than a high-water mark, 100 terminals each. One representative run is
reproduced below because the shape of the step is the decision; the authoritative figures
are the generated ones in `docs/spike-results.md`, and they vary by well under one
percent between runs:

```text
 80x24  scrolling_history=1000  never scrolled                   102 KiB per terminal
 80x24  scrolling_history=1000  filled                          2143 KiB per terminal
200x50  scrolling_history=1000  filled                          5710 KiB per terminal
200x50  scrolling_history=200   one line scrolled                523 KiB per terminal
200x50  scrolling_history=200   filled to a 200 line budget      5710 KiB per terminal
200x50  scrolling_history=1001  filled                         10882 KiB per terminal
200x50  scrolling_history=0     engine history disabled          524 KiB per terminal
```

A 200 line budget and a 1000 line budget cost the same 5710 KiB. A 1001 line budget costs
twice that. Every byte budget between one byte and roughly 5 MiB per terminal at 200
columns buys identical memory. The byte-budget design is therefore withdrawn rather than
tuned.

**Decision.** Engine scrollback is a fixed 1000 rows, which is the chunk size and therefore
the cheapest non-zero setting. The per-terminal ceiling is the screen grid plus one chunk:
roughly **2.1 MiB at 80 columns and 5.7 MiB at 200 columns**, once the terminal has
scrolled at all. A terminal that has never scrolled costs roughly 100 KiB. Those numbers
are bound to gate G2.1 as literals so the gate has something to fail against.

History beyond one chunk goes to disk segments. What is spilled is the **scrolled-off
primary-screen lines**, not the raw output byte stream: replaying raw bytes into a scratch
engine reproduces alternate-screen frames that were never in the scrollback, in the wrong
order relative to the primary content around them, which makes it useless as a scrollback
source even though it remains useful as a separate artefact for event replay.

### When the spill runs, and why it cannot be per read

The engine emits no event when a line leaves the scrollback. `Grid::scroll_up` calls
`increase_scroll_limit`, which saturates at the maximum, and then rotates the backing
storage, recycling the oldest rows in place. There is no eviction variant in its event enum.
So the only way to keep a scrolled-off line is for the daemon to copy it out of the grid
before the rotation reaches it, and the timing of that copy is a correctness requirement,
not an implementation detail.

The arithmetic forces the answer. At 200 columns with ordinary output, roughly 60 bytes per
line, a single 64 KiB read is about 1090 lines, which is more than the entire 1000 row
window. A spill pass that ran once per read would therefore lose lines on the very first
read that fills the buffer, and at 1 MiB per second the whole history turns over in about
57 milliseconds.

**The spill runs between the 8 KiB parse sub-slices** that ADR-002 already establishes for
the parse budget, which is roughly every 136 lines at that width, comfortably inside the
window. It is the same loop, so there is no new scheduling point to get wrong.

When the spill path cannot keep up, for example a slow or degraded disk, **the parse does
not block**. An earlier version of this section said it should, on the grounds that losing
history silently is worse than backpressure. That was wrong in a way worth recording: the
spill runs inside the terminal task, and a terminal task that waits on storage violates
invariant I14 (the PTY read loop never awaits downstream capacity), invariant I3 (a slow
filesystem never blocks the PTY path) and ADR-005's first rule (terminals never block on
storage, absolutely). It would also reintroduce the failure those rules exist to prevent:
an exiting child cannot complete until its tty output is drained, so a terminal task
blocked on a slow disk makes the user's `exit` hang in the kernel.

The correct resolution is the one ADR-005 already uses for observations. The spill hands
rows to a bounded queue drained by the `store` pool. When that queue is full, rows are
dropped, the count and the row range are accumulated in memory, and a `gap` record is
written with the next successful segment write. History acquires a hole that is visible and
counted rather than a hang that is not. Losing history is a real cost and it is the smaller
one: the alternative is losing the terminal.

The engine adapter reports the same condition from the other side. It counts feeds that
happened while the in-memory ring was already saturated, because the engine emits no event
when it evicts, so those rows cannot be observed after the fact. That counter is a gap of
the same kind.

Two gates assert this rather than trusting it. G2.7 produces more than 1000 lines inside one
read and asserts the disk segment contains every line, which is the no-loss path. G2.8
degrades the spill's disk and asserts that the terminal keeps reading and writing, that the
loss is recorded as a gap whose count matches the drop counter, and that no parse turn ever
waits on storage.

Runtime shrinking is not a lever: `Storage::shrink_lines` only truncates when the slack
exceeds `MAX_CACHE_SIZE`, and `update_history` from 1000 down to 200 was measured
reclaiming zero. Two further traps, both verified against the crate source: the engine's
default `scrolling_history` is 10 000, which is a 46 MiB per terminal configuration and
must never be left at the default; and `Term::grid_mut()` returns the **active** grid, so
calling `update_history` on it while a TUI is on the alternate screen is a no-op on the
grid that actually holds the scrollback. The only correct route is `Term::set_options`,
which replaces the whole `Config` and has side effects the caller must expect: it emits a
title event, resets both keyboard mode stacks if the kitty keyboard setting changed, and
marks the terminal fully damaged.

If per-terminal memory later becomes the binding constraint, the two real options are
compressing cold scrollback, which this engine does not support and which is exactly why
other multiplexers add incremental compression to their own, or holding all
scrollback outside the engine with `scrolling_history = 0`, which measures at 524 KiB per
terminal. Both are M2 decisions and both now have numbers.

### The engine's control-string buffer is unbounded, so ours is in front of it

`vte` 0.15 built with `std`, which is how `alacritty_terminal` builds it, holds an OSC
payload in a plain `Vec<u8>` and grows it for as long as the string continues:

```rust
#[cfg(not(feature = "std"))]
osc_raw: ArrayVec<u8, OSC_RAW_BUF_SIZE>,
#[cfg(feature = "std")]
osc_raw: Vec<u8>,
```

The `no_std` build has the 1024 byte cap; the build we get does not. Measured against our
own engine: four megabytes fed into an open `ESC ]` came back as a four megabyte title,
one `printf` away from a per-terminal denial of service, multiplied by every terminal in a
workspace.

So the bound is ours, applied before the parser sees a byte, and it follows the master
plan's rule (section 6.7): a control string over 4 KiB is discarded whole, not truncated.
Truncation would hand the parser a shortened string that still parses, turning a hostile
title into a plausible wrong title instead of an absent one. The discard takes the opening
bytes with it, so the parser never enters the string state and there is nothing for the
terminator to close; the scanner resynchronises at the terminator and counts what it
dropped, because the plan bans a silent discard as firmly as a silent truncation.

The scanner is the one in `qoro-domain`, the same resumable state machine that decides
where a paste may be split. Sharing it is deliberate: two scanners would eventually
disagree about what a sequence is, and the direction that disagreed would be the one
nobody tested.

Evidence: `crates/qoro-terminal/src/control_string.rs`, and
`an_unterminated_control_string_is_bounded` in `crates/qoro-terminal/src/alacritty.rs`,
which fails with "the parser retained 4194304 bytes of a 4194304 byte control string" when
the filter is removed.
Reproduce: `cargo test -p qoro-terminal an_unterminated`

### A character split across two reads never reaches the parser

`vte` 0.15 resumes a character that a read cut in half by copying up to four bytes into a
partial buffer, and if that window also holds a complete second character followed by an
invalid byte, it prints the first and drops the rest. Its own source says so:

```rust
// Since we only care about the first character, we just ignore the rest.
```

Measured against our engine: `[0xc3, 0xa9, 0x20, 0x80]` fed in one call leaves the cursor
at column 2, and the same bytes fed as `[0xc3]` then the rest leave it at column 1, with
the space gone for good. Both crates are already at their latest published version, so
there is no bump that fixes it. A pty read landing on that boundary loses a character, and
the loss is silent.

So, as with the control string, the fix goes in front of the parser: an incomplete
character at the end of a read is held back and put on the front of the next one, and the
parser is only ever handed whole characters. Only a sequence that could still become a
character is held. One that is already invalid is passed straight through, so the parser
draws its replacement character now rather than waiting for bytes that cannot repair it.
The buffer is bounded by the encoding at three bytes.

What this gives up is a stream that ends in the middle of a character: those bytes are
dropped rather than becoming a replacement character, which is a screen one glyph short on
a terminal that is closing.

The split-boundary property test (gate G1.4) previously excluded these boundaries as a
third-party limitation. The exclusion is gone and the gate is asserted at every boundary.
Worth recording honestly: with the hold back removed, that property test still passed six
runs of ten thousand cases, because the trigger needs a split inside a character, a
complete character after it and an invalid byte after that, all inside four bytes. The
deterministic regression test is the guard here, not the property test.

Evidence: `incomplete_utf8_tail` and
`a_character_split_across_two_reads_does_not_lose_the_one_after_it` in
`crates/qoro-terminal/src/alacritty.rs`.
Reproduce: `cargo test -p qoro-terminal a_character_split_across_two_reads`

### The engine's scrollback is a staging buffer, not a scrollback

Decided in milestone 2, and forced by a measurement rather than chosen from the options this
ADR listed.

The adapter read scrolled-off rows by watching `history_size()` grow and taking the
difference. That is correct exactly until the ring fills. After a thousand rows the size
stops growing, the difference is permanently zero, and every row after that is evicted
without ever being seen. Measured: four thousand lines in, one thousand rows on disk, and
every other test green.

So the ring is drained after every feed. Everything in it is taken and `clear_history` is
called, which makes the count exact instead of cumulative, and it is what the milestone 2
design already asks for: scrollback lives on disk as rows, not in the engine as cells.

Two consequences, both good, and one bound the caller now owns.

The memory option this ADR listed as an M2 decision is taken by default: the configuration
measured at 524 KiB per terminal rather than 5710 is the one that runs, because the ring
never holds more than one feed's worth.

The caller must feed in slices small enough that one slice cannot scroll more rows than the
ring holds. A slice of nothing but line feeds scrolls one row per byte, so `PARSE_SLICE` is
half the ring and the margin is a factor of two. A scroll control sequence turns out not to
need a bound at all: measured, `CSI 5000 S` moves at most one screen into history, because
the engine clamps a scroll larger than its region. When the ring is found full anyway, the
adapter counts it, because the one thing it must never do is stay quiet about a row it lost.

Evidence: `every_scrolled_row_is_handed_over_however_many_there_are` and
`a_feed_that_scrolls_more_than_the_ring_holds_is_counted_rather_than_silently_lost`.
Reproduce: `cargo test -p qoro-terminal --lib alacritty`

### The bound is only as good as the scanner in front of it

The filter above is driven by the shared escape scanner, and the first version of that
scanner had no transitions that apply in every state. Three bytes mean the same thing
wherever they appear: `CAN` and `SUB` cancel the sequence in progress, and `ESC` abandons it
and starts a new one.

Without them the bound was bypassed by a three byte prefix. `ESC [ 1 ESC ]` leaves the
parser inside an OSC while a scanner that treats `ESC` as an ordinary parameter byte
believes the `]` returned it to ground, so everything after it was passed straight through.
Measured through the engine: 4 MiB fed, 4 MiB retained, and the discard counter still
reading zero.

The same gap lost output in the other direction. A `CAN` inside an OSC returns the parser to
ground while the scanner stays in the string state, so ordinary text was discarded as if it
were control-string payload: measured, a megabyte of plain text reached the parser as zero
bytes and was attributed to a discarded control string.

Evidence: `an_escape_inside_a_csi_cannot_open_an_unbounded_control_string` and
`text_after_a_cancelled_control_string_still_renders`.

### Known gaps accepted for now
- `CSI 1 J` (erase above) does not clear the row above the cursor when the cursor is on the
  second screen row. Reproduced independently of the corpus: at 6 by 4 with every row full,
  `CSI 2;3H CSI 1J` leaves row 0 as `AAAAAA`, while the same erase from row index 2 clears
  rows 0 and 1 correctly. Recorded as case `era-007` with its citation, and the conformance
  runner fails if it starts passing without the document being updated, so the gap cannot
  quietly become a lie in either direction.
- Kitty graphics protocol is not handled by this engine. The requirement that
  unsupported graphics must never drop adjacent text is satisfied at our layer by
  preserving the placeholder text rather than by rendering the image.
- Kitty keyboard protocol is an input-encoding concern, which we own anyway, not an
  engine concern.

### Re-evaluation gate
At the end of milestone 2 the engine is measured against a VT conformance corpus and the
result is appended to this ADR. Named criteria, all of which the spike did NOT cover:
Unicode width and grapheme clustering including ZWJ sequences and flag emoji, alternate
screen, scroll margins, tab stops, saved cursor, character sets, terminal query and reply
handling, unsupported graphics sequences leaving adjacent text intact, and replay of
captured traces from real applications (vim, htop, tmux, and a coding agent TUI). Switching to a vendored engine is a change to one crate,
not an architectural change. Two things are what make that true and both are merge gates:
the trait above, and a golden-output corpus so a swap can be proven equivalent.

## Consequences
- No Zig in the build, no FFI, no patch ledger for milestone 1.
- Damage tracking, scrollback bounding and resize come for free and are already tested.
- `alacritty_terminal`'s public API includes a `term::test::TermSize` helper that we use
  in tests only; production code implements `Dimensions` itself.

## Rollback
Implement `TerminalEngine` over a different engine and run the golden corpus. If the
corpus passes, the swap is complete.

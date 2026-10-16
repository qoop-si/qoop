//! The Chat view of an agent terminal: the session's digest drawn as rows, in the place of
//! the terminal's own screen.
//!
//! The rows are a pure function of the items, the width, the height and how far back the
//! person scrolled, so a test reads them as text. They are handed to the painters as a
//! snapshot (`snapshot`) of the same shape a terminal's screen has, which is what lets the
//! three places that paint a terminal, the selection and copy mode all work on them without
//! knowing there is a second view. The terminal underneath is never resized and keeps
//! running; only what is drawn changes.
//!
//! The look follows the Claude and ChatGPT apps: the person's message in a raised block, the
//! agent's text plain at the full width, reasoning and tool calls folded to one row each.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use qoop_parsed::{Chip, Digest, Item, Status};
use qoop_runtime::control::AgentRow;
use qoop_runtime::daemon::{ColourWire, ModesWire, SnapshotWire, SpanWire, attrs};

use crate::theme::{self, Role};
use crate::width;

/// How a stretch of a row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub role: Role,
    /// `qoop_runtime::daemon::attrs` bits.
    pub attrs: u8,
    /// On the raised background: the person's block and inline code.
    pub raised: bool,
}

const fn style(role: Role) -> Style {
    Style {
        role,
        attrs: 0,
        raised: false,
    }
}

/// A run of characters of a row's text in one style, counted in characters, not cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub at: usize,
    pub len: usize,
    pub style: Style,
}

/// One row of the view: its text, as composed (no spacer cells), and how it is drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledRow {
    pub text: String,
    pub runs: Vec<Run>,
}

type Cell = (char, Style);

/// Characters that join or reshape the one before them. Terminals and this client's width
/// table disagree on what they do to a row (a heart and its emoji selector are one column to
/// one and two to the other), so a chat row leaves them out and every character is its own.
fn joiner(c: char) -> bool {
    // By number: none of these is ever drawn.
    matches!(
        u32::from(c),
        0x200d | 0xfe0e | 0xfe0f | 0x20e3 | 0xe0020..=0xe007f
    )
}

fn cells(text: &str, style: Style) -> Vec<Cell> {
    text.chars()
        .filter_map(|c| match c {
            '\t' => Some(' '),
            c if width::drawn(c) && !joiner(c) => Some(c),
            _ => None,
        })
        .map(|c| (c, style))
        .collect()
}

fn row_of(cells: &[Cell]) -> StyledRow {
    let mut runs: Vec<Run> = Vec::new();
    for (at, (_, style)) in cells.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.style == *style => run.len += 1,
            _ => runs.push(Run {
                at,
                len: 1,
                style: *style,
            }),
        }
    }
    StyledRow {
        text: cells.iter().map(|(c, _)| *c).collect(),
        runs,
    }
}

fn cell_width(cells: &[Cell]) -> usize {
    cells.iter().map(|(c, _)| width::columns(*c)).sum()
}

/// `text` cut to `room` columns, with a closing ellipsis when it did not fit.
fn cut(text: &str, room: usize) -> String {
    if width::width(text) <= room {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = width::columns(c);
        if used + w + 1 > room {
            break;
        }
        out.push(c);
        used += w;
    }
    if room > 0 {
        out.push('\u{2026}');
    }
    out
}

/// Greedy word wrap to `room` columns. A word longer than a row is broken on a character.
/// Leading spaces of a row are dropped: a break is not an indent.
fn wrap(text: &[Cell], room: usize) -> Vec<Vec<Cell>> {
    let room = room.max(1);
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut line: Vec<Cell> = Vec::new();
    let mut used = 0;
    let mut at = 0;
    while at < text.len() {
        let space = text[at].0 == ' ';
        let end = text[at..]
            .iter()
            .position(|(c, _)| (*c == ' ') != space)
            .map_or(text.len(), |n| at + n);
        let token = &text[at..end];
        at = end;
        let w = cell_width(token);
        if used + w <= room {
            if !(space && used == 0) {
                line.extend_from_slice(token);
                used += w;
            }
            continue;
        }
        while line.last().is_some_and(|(c, _)| *c == ' ') {
            line.pop();
        }
        if !line.is_empty() {
            rows.push(std::mem::take(&mut line));
            used = 0;
        }
        if space {
            continue;
        }
        for cell in token {
            let w = width::columns(cell.0);
            if used + w > room && !line.is_empty() {
                rows.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(*cell);
            used += w;
        }
    }
    while line.last().is_some_and(|(c, _)| *c == ' ') {
        line.pop();
    }
    if !line.is_empty() || rows.is_empty() {
        rows.push(line);
    }
    rows
}

/// `cells` cut into rows of `room` columns on a character, spaces kept: code is not reflowed.
/// An empty line is one empty row.
fn chunks(text: &[Cell], room: usize) -> Vec<Vec<Cell>> {
    let room = room.max(1);
    let mut rows = vec![Vec::new()];
    let mut used = 0;
    for cell in text {
        let w = width::columns(cell.0);
        if used + w > room && used > 0 {
            rows.push(Vec::new());
            used = 0;
        }
        rows.last_mut().expect("one row").push(*cell);
        used += w;
    }
    rows
}

/// `**bold**` and `` `code` `` inside one line. A marker with no partner is text.
fn inline(text: &str, base: Style) -> Vec<Cell> {
    let mut out = Vec::new();
    let (mut bold, mut code) = (false, false);
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let mark = if !code && rest.starts_with("**") && (bold || rest[2..].contains("**")) {
            Some(2)
        } else if rest.starts_with('`') && (code || rest[1..].contains('`')) {
            Some(1)
        } else {
            None
        };
        match mark {
            Some(2) => bold = !bold,
            Some(_) => code = !code,
            None => {
                let mut style = base;
                if bold {
                    style.attrs |= attrs::BOLD;
                }
                if code {
                    style.raised = true;
                }
                out.extend(cells(&c.to_string(), style));
            }
        }
        rest = &rest[mark.unwrap_or(c.len_utf8())..];
    }
    out
}

/// The marker a list line opens with, its text, and how far the text is indented.
fn list_item(line: &str) -> Option<(String, &str)> {
    let body = line.trim_start();
    let lead = line.len() - body.len();
    let marker_end = if body.starts_with(['-', '*', '+']) && body[1..].starts_with(' ') {
        2
    } else {
        let digits = body.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && body[digits..].starts_with(". ")).then_some(digits + 2)?
    };
    let marker = format!("{}{}", &line[..lead], &body[..marker_end]);
    Some((marker, body[marker_end..].trim_start()))
}

/// The agent's text: plain, the full width, with the markdown an agent writes (bold, inline
/// code, fenced code, lists, headings). Anything else is a paragraph.
fn agent_rows(text: &str, cols: usize) -> Vec<StyledRow> {
    let base = style(Role::Text);
    let dim = style(Role::Detail);
    let mut rows: Vec<StyledRow> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            for part in chunks(&cells(line, dim), cols.saturating_sub(2)) {
                let mut row = cells("  ", dim);
                row.extend(part);
                rows.push(row_of(&row));
            }
            continue;
        }
        if line.trim().is_empty() {
            if rows.last().is_some_and(|r| !r.text.is_empty()) {
                rows.push(StyledRow::default());
            }
            continue;
        }
        let heading = line.trim_start_matches('#');
        let (marker, body) = if heading.len() < line.len() && heading.starts_with(' ') {
            let bold = Style {
                attrs: attrs::BOLD,
                ..base
            };
            (String::new(), inline(heading.trim(), bold))
        } else if let Some((marker, body)) = list_item(line) {
            (marker, inline(body, base))
        } else {
            (String::new(), inline(line, base))
        };
        let indent = width::width(&marker);
        for (n, part) in wrap(&body, cols.saturating_sub(indent))
            .into_iter()
            .enumerate()
        {
            let mut row = cells(
                &if n == 0 {
                    marker.clone()
                } else {
                    " ".repeat(indent)
                },
                base,
            );
            row.extend(part);
            rows.push(row_of(&row));
        }
    }
    while rows.last().is_some_and(|r| r.text.is_empty()) {
        rows.pop();
    }
    rows
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{} MB", bytes / 1_048_576),
    }
}

/// The person's message: a raised block the width of the pane, `> ` before the first row.
fn user_rows(text: &str, chips: &[Chip], cols: usize) -> Vec<StyledRow> {
    let raised = Style {
        raised: true,
        ..style(Role::Text)
    };
    let dim = Style {
        role: Role::Detail,
        ..raised
    };
    let room = cols.saturating_sub(4).max(1);
    let mut lines: Vec<Vec<Cell>> = Vec::new();
    for line in text.trim_end().split('\n') {
        lines.extend(wrap(&cells(line, raised), room));
    }
    for chip in chips {
        let label = match chip.bytes {
            Some(bytes) => format!("[{} {}]", chip.name, size(bytes)),
            None => format!("[{}]", chip.name),
        };
        lines.extend(wrap(&cells(&label, dim), room));
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(n, line)| {
            let mut row = cells(if n == 0 { " > " } else { "   " }, dim);
            row.extend(line);
            let pad = cols.saturating_sub(cell_width(&row));
            row.extend(cells(&" ".repeat(pad), raised));
            row_of(&row)
        })
        .collect()
}

fn mark_of(status: Status) -> (&'static str, &'static str, Role) {
    match status {
        Status::Running => ("\u{25cb}", "running", Role::Working),
        Status::Ok => ("\u{25cf}", "ok", Role::Done),
        Status::Failed => ("\u{25c6}", "failed", Role::Failed),
        Status::Interrupted => ("~", "interrupted", Role::Detail),
        Status::Rejected => ("x", "rejected", Role::Detail),
        Status::Unknown => ("?", "unknown", Role::Detail),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A tool call as one row: mark and status word (never colour alone), the tool's name, the
/// one line it was asked to do, and `exit N` when the exit code is not zero. What does not
/// fit is cut from the line first, then from the name; the status word always stays.
fn tool_row(
    name: &str,
    summary: &str,
    status: Status,
    exit_code: Option<i32>,
    cols: usize,
) -> StyledRow {
    let (mark, word, role) = mark_of(status);
    let head = format!("{mark} {word} ");
    let tail = exit_code
        .filter(|code| *code != 0)
        .map(|code| format!("  exit {code}"))
        .unwrap_or_default();
    let room = cols.saturating_sub(width::width(&head) + width::width(&tail));
    let name = cut(name, room);
    let room = room.saturating_sub(width::width(&name) + 1);
    let summary = one_line(summary);
    let mut row = cells(&head, style(role));
    row.extend(cells(&name, style(Role::Text)));
    if room >= 2 && !summary.is_empty() {
        row.extend(cells(
            &format!(" {}", cut(&summary, room)),
            style(Role::Detail),
        ));
    }
    row.extend(cells(&tail, style(Role::Detail)));
    row_of(&row)
}

/// A run of tool calls folded into one row: how many, and which did not go well.
fn steps_row(run: &[&Item], cols: usize) -> StyledRow {
    let count = |want: Status| {
        run.iter()
            .filter(|it| matches!(it, Item::Tool { status, .. } if *status == want))
            .count()
    };
    let (failed, running) = (count(Status::Failed), count(Status::Running));
    let (mark, _, role) = if failed > 0 {
        mark_of(Status::Failed)
    } else if running > 0 {
        mark_of(Status::Running)
    } else {
        mark_of(Status::Ok)
    };
    let mut text = format!("{mark} {} steps", run.len());
    for (n, word) in [
        (failed, "failed"),
        (running, "running"),
        (count(Status::Interrupted), "interrupted"),
        (count(Status::Rejected), "rejected"),
    ] {
        if n > 0 {
            text.push_str(&format!(", {n} {word}"));
        }
    }
    row_of(&cells(&cut(&text, cols), style(role)))
}

fn dim_row(text: &str, cols: usize) -> StyledRow {
    row_of(&cells(&cut(&one_line(text), cols), style(Role::Detail)))
}

fn thought_row(ms: Option<u64>, cols: usize) -> StyledRow {
    dim_row(
        &match ms {
            Some(ms) => format!("Thought for {} s", ((ms + 500) / 1000).max(1)),
            None => "Thought".to_string(),
        },
        cols,
    )
}

/// What an item is, for spacing: the rows of work between messages sit tight together.
#[derive(PartialEq)]
enum Kind {
    Message,
    Step,
}

fn is_step(item: &Item) -> bool {
    matches!(
        item,
        Item::Tool { .. } | Item::Subagent { .. } | Item::Thinking { .. }
    )
}

/// Every row of the chat at this width, oldest first.
pub fn layout(items: &[Item], cols: usize) -> Vec<StyledRow> {
    if cols == 0 {
        return Vec::new();
    }
    let mut blocks: Vec<(Kind, Vec<StyledRow>)> = Vec::new();
    let mut at = 0;
    while at < items.len() {
        let item = &items[at];
        if is_step(item) {
            let end = items[at..]
                .iter()
                .position(|it| !is_step(it))
                .map_or(items.len(), |n| at + n);
            let calls: Vec<&Item> = items[at..end]
                .iter()
                .filter(|it| !matches!(it, Item::Thinking { .. }))
                .collect();
            if calls.len() >= 2 {
                blocks.push((Kind::Step, vec![steps_row(&calls, cols)]));
            } else {
                for item in &items[at..end] {
                    blocks.push((Kind::Step, vec![step_row(item, cols)]));
                }
            }
            at = end;
            continue;
        }
        at += 1;
        let (kind, rows) = match item {
            Item::User {
                text, attachments, ..
            } => (Kind::Message, user_rows(text, attachments, cols)),
            Item::Agent { text, .. } => (Kind::Message, agent_rows(text, cols)),
            Item::Error { text, .. } => {
                (Kind::Step, vec![dim_row(&format!("error: {text}"), cols)])
            }
            Item::Notice { kind, text, .. } => {
                (Kind::Step, vec![dim_row(&format!("{kind}: {text}"), cols)])
            }
            Item::Compaction { .. } => (Kind::Step, vec![dim_row("context compacted", cols)]),
            // A kind of row a newer daemon sent: said, never dropped, so the page stays whole.
            Item::Unknown => (
                Kind::Step,
                vec![dim_row("item this build cannot show", cols)],
            ),
            _ => continue,
        };
        if !rows.is_empty() {
            blocks.push((kind, rows));
        }
    }
    let mut out: Vec<StyledRow> = Vec::new();
    let mut before = None;
    for (kind, rows) in blocks {
        if before.is_some_and(|b| !(b == Kind::Step && kind == Kind::Step)) {
            out.push(StyledRow::default());
        }
        out.extend(rows);
        before = Some(kind);
    }
    // A pane too narrow for a row's own margins shows what fits of it, never a wider row.
    out.iter_mut().for_each(|row| clip(row, cols));
    out
}

/// `row` cut to `cols` columns, never leaving half of a wide character.
fn clip(row: &mut StyledRow, cols: usize) {
    let mut used = 0;
    let keep = row
        .text
        .chars()
        .take_while(|c| {
            used += width::columns(*c);
            used <= cols
        })
        .count();
    if keep == row.text.chars().count() {
        return;
    }
    row.text = row.text.chars().take(keep).collect();
    row.runs.retain(|run| run.at < keep);
    for run in &mut row.runs {
        run.len = run.len.min(keep - run.at);
    }
}

fn step_row(item: &Item, cols: usize) -> StyledRow {
    match item {
        Item::Thinking { ms, .. } => thought_row(*ms, cols),
        Item::Tool {
            name,
            summary,
            status,
            exit_code,
            ..
        } => tool_row(name, summary, *status, *exit_code, cols),
        Item::Subagent { summary, steps, .. } => {
            dim_row(&format!("subagent {summary}, {steps} steps"), cols)
        }
        _ => StyledRow::default(),
    }
}

/// How far back from the newest row the view can go.
pub fn max_scroll(all: &[StyledRow], rows: usize) -> usize {
    all.len().saturating_sub(rows)
}

/// `rows` rows of `all`, the newest at the bottom, `scroll` rows back from there. A chat
/// shorter than the pane starts at the top, as a terminal does, and is padded with blank rows.
pub fn window(all: &[StyledRow], rows: usize, scroll: usize) -> Vec<StyledRow> {
    let end = all.len() - scroll.min(max_scroll(all, rows));
    let start = end.saturating_sub(rows);
    let mut out = all[start..end].to_vec();
    out.resize(rows, StyledRow::default());
    out
}

/// The view: `rows` rows of the chat at `cols` columns, `scroll` rows back from the newest.
pub fn view(items: &[Item], cols: usize, rows: usize, scroll: usize) -> Vec<StyledRow> {
    window(&layout(items, cols), rows, scroll)
}

/// The rows as a terminal's screen: a grid row has a spacer cell after each wide character,
/// the runs count cells, the cursor is hidden and nothing is behind the screen.
pub fn snapshot(rows: &[StyledRow], cols: usize) -> SnapshotWire {
    let mut lines = Vec::with_capacity(rows.len());
    let mut styles = Vec::with_capacity(rows.len());
    for row in rows {
        let mut line = String::new();
        // Where each character starts, in cells, and one past the last.
        let mut start = Vec::new();
        for c in row.text.chars() {
            start.push(line.chars().count());
            line.push(c);
            if width::columns(c) == 2 {
                line.push(' ');
            }
        }
        start.push(line.chars().count());
        lines.push(line);
        styles.push(
            row.runs
                .iter()
                .map(|run| SpanWire {
                    at: start[run.at],
                    len: start[run.at + run.len] - start[run.at],
                    fg: Some(ColourWire::Indexed {
                        i: theme::indexed(run.style.role),
                    }),
                    bg: run.style.raised.then(|| ColourWire::Indexed {
                        i: theme::raised_bg(),
                    }),
                    attrs: run.style.attrs,
                    link: None,
                })
                .collect(),
        );
    }
    SnapshotWire {
        cols,
        rows: rows.len(),
        // On the newest row, so copy mode opens where the chat is read from.
        cursor: (rows.len().saturating_sub(1), 0),
        lines,
        styles,
        links: Vec::new(),
        modes: ModesWire {
            cursor_visible: false,
            ..ModesWire::default()
        },
        behind: 0,
    }
}

/// How long typing keeps a tab on its terminal when no Enter or Esc comes.
const TYPING_FOR: Duration = Duration::from_secs(30);
/// The first page and the least a refresh asks for; the most is `MAX_PAGE`, so history the
/// reader has scrolled to survives a refresh.
const PAGE: u32 = 60;
const MAX_PAGE: u32 = 200;
/// News is read after this, so a burst of appends is one read.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// The daemon forgets a terminal nobody asked about for 30 s: ask again before that.
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Between reads of a session the agent has not named yet.
const RETRY: Duration = Duration::from_secs(3);
/// A read with no answer for this long is taken as lost.
const LOST: Duration = Duration::from_secs(10);

/// A program name as the memory file holds it: one short line of printable characters.
fn program_line(line: &str) -> Option<String> {
    let line = line.trim();
    (!line.is_empty() && line.chars().count() <= 64 && line.chars().all(width::drawn))
        .then(|| line.to_string())
}

/// What this client knows of each terminal's Chat view: which are switched on, the digest the
/// daemon last sent for each, how far back each is scrolled, and the rows drawn last.
///
/// The choice is remembered per program (`claude`, `codex`), not per terminal id: a new tab of
/// a program somebody switched to Chat starts there. One line per program in `file`, read at
/// start and written on every switch.
#[derive(Default)]
pub struct ChatState {
    file: Option<PathBuf>,
    programs: BTreeSet<String>,
    on: BTreeSet<u32>,
    /// Terminals whose program's remembered choice has been applied, so it is applied once
    /// and a person's own switch afterwards stays.
    settled: BTreeSet<u32>,
    /// What each listed terminal was last seen as: an agent or not, and which program.
    seen: BTreeMap<u32, (bool, Option<String>)>,
    /// The newest page of each session, and the internal stamp of its last change.
    digests: BTreeMap<u32, (u64, Digest)>,
    stamp: u64,
    scroll: BTreeMap<u32, usize>,
    shown: BTreeMap<u32, Shown>,
    /// Terminals that showed as chat at the last look, to tell when the view flips.
    visible: BTreeSet<u32>,
    /// Terminals typed into lately: the terminal shows until Enter, Esc or the time is up.
    typing: BTreeMap<u32, Instant>,
    /// Terminals whose older page has been asked for and not answered.
    asked: BTreeSet<u32>,
    /// Terminals whose digest just grew at the top, which moves no row the reader sees.
    prepended: BTreeSet<u32>,
    /// What to ask the daemon for, per flagged terminal.
    feeds: BTreeMap<u32, Feed>,
    /// The flagged terminals that are on screen.
    screen: BTreeSet<u32>,
    /// Terminals the daemon would not serve (not an agent, no such terminal, too large).
    refused: BTreeSet<u32>,
    #[cfg(test)]
    layouts: usize,
}

/// The reads of one terminal's chat.
#[derive(Default)]
struct Feed {
    /// A newest-page read is out, since when.
    out: Option<Instant>,
    /// When the next read was asked to happen: the debounce after news, or a retry.
    due: Option<Instant>,
    /// News came while a read was out: one more read follows its answer.
    stale: bool,
    last_read: Option<Instant>,
}

/// One `Messages` request to send.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub terminal: u32,
    pub skip: u32,
    pub limit: u32,
    pub revision: Option<u64>,
    /// A page of older items, put in front, and not the newest page.
    pub older: bool,
}

struct Shown {
    stamp: u64,
    cols: usize,
    /// Every row at this width, so a scroll or a new height does not lay the chat out again.
    all: Vec<StyledRow>,
    /// The height and scroll `snapshot` was drawn for.
    drawn: Option<(usize, usize)>,
    snapshot: SnapshotWire,
    max: usize,
}

/// One terminal to draw: its tile's size, and whether a selection or copy mode holds the rows
/// it shows (new rows then stay below the view instead of moving it).
pub struct Want {
    pub terminal: u32,
    pub cols: usize,
    pub rows: usize,
    pub held: bool,
}

impl ChatState {
    /// The programs `file` holds, one per line. No file, one that cannot be read, or lines that
    /// are not program names are no program switched on.
    pub fn read(file: Option<PathBuf>) -> ChatState {
        let programs = file.as_deref().map(programs_in).unwrap_or_default();
        ChatState {
            file,
            programs,
            ..ChatState::default()
        }
    }

    /// The seam the daemon's answer fills: the newest page of `terminal`'s session.
    ///
    /// Nothing calls this yet: the producer is the daemon's `Messages` answer (a client
    /// polling `wanting()`, sending the request and handing the response here), which comes
    /// with the daemon work. Until then the view never switches, and fixtures fill it in tests.
    pub fn set_digest(&mut self, terminal: u32, digest: Digest) {
        self.asked.remove(&terminal);
        if self
            .digests
            .get(&terminal)
            .is_some_and(|(_, d)| *d == digest)
        {
            return;
        }
        self.stamp += 1;
        self.digests.insert(terminal, (self.stamp, digest));
    }

    /// How many items of `terminal`'s session are held, when the reader is at the oldest of
    /// them and older ones exist: the `skip` for the next request. Answered once per request,
    /// until `set_older` or `set_digest` brings the answer.
    pub fn wants_older(&mut self, terminal: u32) -> Option<usize> {
        let (_, digest) = self.digests.get(&terminal)?;
        let at_top = self
            .shown
            .get(&terminal)
            .is_some_and(|s| self.scrolled(terminal) >= s.max);
        (self.on.contains(&terminal) && digest.has_older && at_top && self.asked.insert(terminal))
            .then_some(digest.items.len())
    }

    /// The answer to `wants_older`: older items, put in front of the ones held without moving
    /// the reader. A different revision means the session changed under the request, and the
    /// page replaces everything (the response's `reset`).
    pub fn set_older(&mut self, terminal: u32, older: Digest) {
        let Some((_, held)) = self.digests.get_mut(&terminal) else {
            return;
        };
        if older.revision != held.revision {
            self.scroll.remove(&terminal);
            return self.set_digest(terminal, older);
        }
        self.asked.remove(&terminal);
        let mut items = older.items;
        items.append(&mut held.items);
        held.items = items;
        held.has_older = older.has_older;
        held.first_seq = older.first_seq;
        held.warnings += older.warnings;
        self.stamp += 1;
        self.digests.get_mut(&terminal).expect("held above").0 = self.stamp;
        self.prepended.insert(terminal);
    }

    /// Whether `terminal` needs a digest: its Chat switch is on.
    pub fn wants_digest(&self, terminal: u32) -> bool {
        self.on.contains(&terminal)
    }

    /// Every terminal that needs a digest.
    pub fn wanting(&self) -> impl Iterator<Item = u32> + '_ {
        self.on.iter().copied()
    }

    /// The terminal has something to show as chat: switched on, and the session has items.
    pub fn has_view(&self, terminal: u32) -> bool {
        self.wants_digest(terminal)
            && self
                .digests
                .get(&terminal)
                .is_some_and(|(_, digest)| !digest.items.is_empty())
    }

    /// Settle the terminals the daemon lists. A new agent terminal of a remembered program
    /// starts on. A terminal that is gone, stopped being an agent, or runs another program
    /// now takes its state with it: its digest is a session that is not there any more.
    pub fn adopt(&mut self, rows: &[AgentRow]) {
        let live: BTreeSet<u32> = rows.iter().map(|r| r.terminal).collect();
        for gone in self
            .known()
            .filter(|t| !live.contains(t))
            .collect::<Vec<_>>()
        {
            self.forget(gone);
        }
        for row in rows {
            let was = self.seen.get(&row.terminal).cloned();
            // A program that cannot be read for the moment is not a different program.
            let changed = was.as_ref().is_some_and(|(agent, program)| {
                (*agent && !row.agent)
                    || (row.agent
                        && program.is_some()
                        && row.program.is_some()
                        && *program != row.program)
            });
            if changed {
                self.forget(row.terminal);
            }
            let program = match (&row.program, was) {
                (None, Some((_, kept))) if !changed => kept,
                (program, _) => program.clone(),
            };
            self.seen.insert(row.terminal, (row.agent, program));
            if row.agent
                && self.settled.insert(row.terminal)
                && row
                    .program
                    .as_ref()
                    .is_some_and(|p| self.programs.contains(p))
            {
                self.on.insert(row.terminal);
            }
        }
    }

    fn known(&self) -> impl Iterator<Item = u32> + '_ {
        self.seen.keys().copied().chain(self.on.iter().copied())
    }

    /// Everything held for `terminal`.
    fn forget(&mut self, terminal: u32) {
        self.on.remove(&terminal);
        self.settled.remove(&terminal);
        self.seen.remove(&terminal);
        self.digests.remove(&terminal);
        self.scroll.remove(&terminal);
        self.shown.remove(&terminal);
        self.typing.remove(&terminal);
        self.asked.remove(&terminal);
        self.prepended.remove(&terminal);
        self.feeds.remove(&terminal);
        self.refused.remove(&terminal);
    }

    /// Flip the switch of `terminal` and remember it for its program. Answers the new state.
    pub fn toggle(&mut self, terminal: u32, program: Option<&str>) -> bool {
        self.settled.insert(terminal);
        let on = self.on.insert(terminal);
        if !on {
            self.on.remove(&terminal);
        }
        self.scroll.remove(&terminal);
        self.typing.remove(&terminal);
        self.feeds.remove(&terminal);
        self.refused.remove(&terminal);
        if let Some(program) = program {
            self.remember(program, on);
        }
        on
    }

    // The producer: what to ask the daemon for, and what its answers do. Pure, so the loop
    // only sends what `plan` returns and hands back what comes. Never blocks, never parses.

    /// The flagged terminals that are on screen: only those are kept fresh while they idle.
    pub fn set_screen(&mut self, on_screen: BTreeSet<u32>) {
        self.screen = on_screen;
    }

    /// When the next page is due for `terminal`, if one is: a first read at once, a read
    /// that news or a retry asked for, or the keepalive of one on screen. Nothing while a
    /// request is out.
    fn due_at(&self, terminal: u32, now: Instant) -> Option<Instant> {
        let Some(feed) = self.feeds.get(&terminal) else {
            return Some(now);
        };
        if feed.out.is_some() {
            return None;
        }
        let keepalive = feed
            .last_read
            .filter(|_| self.screen.contains(&terminal))
            .map(|read| read + KEEPALIVE);
        [feed.due, keepalive]
            .into_iter()
            .flatten()
            .min()
            .or(match feed.last_read {
                None => Some(now),
                Some(_) => None,
            })
    }

    /// When the loop must look again: the soonest due read of a flagged terminal.
    pub fn wake_at(&self, now: Instant) -> Option<Instant> {
        self.on
            .iter()
            .filter_map(|t| match self.feeds.get(t).and_then(|feed| feed.out) {
                // A read out is waited for, until it is taken as lost.
                Some(out) => Some(out + LOST + Duration::from_millis(1)),
                None => self.due_at(*t, now),
            })
            .min()
    }

    /// The requests to send now. One newest-page read in flight per terminal, the latest
    /// news wins; a page of older items when the reader is at the top of what is held.
    /// Nothing for a terminal whose flag is off.
    pub fn plan(&mut self, now: Instant) -> Vec<Request> {
        let mut out = Vec::new();
        for terminal in self.on.iter().copied().collect::<Vec<_>>() {
            let feed = self.feeds.entry(terminal).or_default();
            if feed
                .out
                .is_some_and(|at| now.saturating_duration_since(at) > LOST)
            {
                feed.out = None;
            }
            if self.due_at(terminal, now).is_some_and(|at| at <= now) {
                let held = self
                    .digests
                    .get(&terminal)
                    .map_or(0, |(_, d)| d.items.len());
                let revision = self.digests.get(&terminal).map(|(_, d)| d.revision);
                let feed = self.feeds.entry(terminal).or_default();
                feed.out = Some(now);
                feed.due = None;
                feed.stale = false;
                out.push(Request {
                    terminal,
                    skip: 0,
                    limit: held.clamp(PAGE as usize, MAX_PAGE as usize) as u32,
                    revision,
                    older: false,
                });
            }
            if let Some(skip) = self.wants_older(terminal) {
                let revision = self.digests.get(&terminal).map(|(_, d)| d.revision);
                out.push(Request {
                    terminal,
                    skip: skip as u32,
                    limit: PAGE,
                    revision,
                    older: true,
                });
            }
        }
        out
    }

    /// News that the session of `terminal` changed. Read again after the debounce, once:
    /// news while a read is out is answered by one more read after it.
    pub fn changed(&mut self, terminal: u32, now: Instant) {
        if !self.on.contains(&terminal) {
            return;
        }
        let feed = self.feeds.entry(terminal).or_default();
        if feed.out.is_some() {
            feed.stale = true;
        } else if feed.due.is_none_or(|at| at > now + DEBOUNCE) {
            feed.due = Some(now + DEBOUNCE);
        }
    }

    /// A page the daemon sent for `terminal`: `older` for the one asked at the top of the
    /// chat, else the newest. Either replaces or extends what is held (`set_digest`,
    /// `set_older`), and `reset` needs nothing more: both take a page of another revision
    /// as the whole.
    pub fn answered(&mut self, terminal: u32, older: bool, page: Digest, now: Instant) {
        if !self.on.contains(&terminal) {
            return;
        }
        if older {
            return self.set_older(terminal, page);
        }
        let feed = self.feeds.entry(terminal).or_default();
        feed.out = None;
        feed.last_read = Some(now);
        if std::mem::take(&mut feed.stale) {
            feed.due = Some(now + DEBOUNCE);
        }
        self.refused.remove(&terminal);
        self.set_digest(terminal, page);
    }

    /// The daemon refused a read of `terminal` with `reason`. Answers what to tell the
    /// person when that ends the chat for the tab; `None` when it only waits.
    pub fn refused(
        &mut self,
        terminal: u32,
        older: bool,
        reason: &str,
        now: Instant,
    ) -> Option<&'static str> {
        if !self.on.contains(&terminal) {
            return None;
        }
        // An older page that finds no session is not asked again until a newer read lands.
        if older && reason != "no_session" {
            self.asked.remove(&terminal);
        }
        match reason {
            // The agent has not said which session it is in yet: ask again.
            "no_session" => {
                let feed = self.feeds.entry(terminal).or_default();
                if !older {
                    feed.out = None;
                    feed.due = Some(now + RETRY);
                }
                None
            }
            "too_large" => Some(self.give_up(
                terminal,
                "this session is too large for chat, use the terminal",
            )),
            "not_agent" => Some(self.give_up(terminal, "chat is for agent tabs")),
            _ => Some(self.give_up(terminal, "chat is not available for this tab")),
        }
    }

    /// A daemon too old to know `Messages` answers with no id: every read in flight was its.
    /// Answers the terminals that gave up.
    pub fn unsupported(&mut self) -> Vec<u32> {
        let asked: Vec<u32> = self
            .feeds
            .iter()
            .filter(|(_, feed)| feed.out.is_some())
            .map(|(t, _)| *t)
            .chain(self.asked.iter().copied())
            .collect();
        for terminal in &asked {
            self.give_up(*terminal, "");
        }
        asked
    }

    /// The flag goes off for `terminal` because the daemon will not serve it. Not the
    /// person's choice, so the program's memory is left alone.
    fn give_up(&mut self, terminal: u32, said: &'static str) -> &'static str {
        self.on.remove(&terminal);
        self.digests.remove(&terminal);
        self.scroll.remove(&terminal);
        self.shown.remove(&terminal);
        self.typing.remove(&terminal);
        self.asked.remove(&terminal);
        self.prepended.remove(&terminal);
        self.feeds.remove(&terminal);
        self.refused.insert(terminal);
        said
    }

    /// Whether the daemon refused to serve `terminal`: the key row stops offering the switch.
    pub fn is_refused(&self, terminal: u32) -> bool {
        self.refused.contains(&terminal)
    }

    /// Put `program` in or out of the memory file. The file is read again first and only this
    /// program is changed, so another interface's switch is never written over. Written aside
    /// and renamed over, so nobody reads half a file.
    fn remember(&mut self, program: &str, on: bool) {
        let mut programs = self.file.as_deref().map(programs_in).unwrap_or_default();
        if on {
            programs.insert(program.to_string());
        } else {
            programs.remove(program);
        }
        if let Some(file) = &self.file {
            let text: String = programs.iter().map(|p| format!("{p}\n")).collect();
            let temp = file.with_extension(format!("{}", std::process::id()));
            if std::fs::write(&temp, text).is_ok() {
                let _ = std::fs::rename(&temp, file);
            }
        }
        self.programs = programs;
    }

    /// A key went to the terminal's program. The terminal shows for the moment, so the person
    /// sees what they type, until Enter or a lone Esc goes, or `TYPING_FOR` passes. A chat
    /// that is off has nothing to switch back to.
    pub fn typed(&mut self, terminal: u32, bytes: &[u8], now: Instant) {
        if !self.on.contains(&terminal) {
            return;
        }
        if bytes.contains(&b'\r') || bytes == b"\x1b" {
            self.typing.remove(&terminal);
        } else {
            self.typing.insert(terminal, now);
        }
    }

    /// Whether `terminal` is showing its terminal while somebody types.
    pub fn typing(&self, terminal: u32) -> bool {
        self.typing.contains_key(&terminal)
    }

    /// End the typing that has gone `TYPING_FOR` without a key. Answers whether any ended.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.typing.len();
        self.typing
            .retain(|_, last| now.saturating_duration_since(*last) < TYPING_FOR);
        self.typing.len() != before
    }

    /// Which terminals changed between chat and terminal since the last look, given those
    /// that show as chat now.
    pub fn flips(&mut self, now: BTreeSet<u32>) -> BTreeSet<u32> {
        let flipped = self.visible.symmetric_difference(&now).copied().collect();
        self.visible = now;
        flipped
    }

    /// Draw the terminals that show as chat, each at its own size, and forget the rest. The
    /// rows are laid out again only when the digest or the width changed, and the snapshot
    /// is drawn again only when that, the height or the scroll did.
    pub fn render(&mut self, wanted: &[Want]) {
        self.shown
            .retain(|t, _| wanted.iter().any(|w| w.terminal == *t));
        for &Want {
            terminal,
            cols,
            rows,
            held,
        } in wanted
        {
            let Some((stamp, digest)) = self.digests.get(&terminal) else {
                continue;
            };
            let current = self
                .shown
                .get(&terminal)
                .is_some_and(|s| s.stamp == *stamp && s.cols == cols);
            if !current {
                #[cfg(test)]
                {
                    self.layouts += 1;
                }
                let all = layout(&digest.items, cols);
                // Rows added below stay below the reader: a chat scrolled back, or one being
                // selected from, keeps the same text in view.
                // ponytail: counted in rows, so a window that also drops old items at the top
                // drifts by what it dropped; anchoring by item needs stable ids from the daemon.
                let added = match self.shown.get(&terminal) {
                    Some(old) if old.cols == cols && !self.prepended.contains(&terminal) => {
                        all.len().saturating_sub(old.all.len())
                    }
                    _ => 0,
                };
                self.prepended.remove(&terminal);
                let scroll = self.scroll.entry(terminal).or_default();
                if *scroll > 0 || held {
                    *scroll += added;
                }
                self.shown.insert(
                    terminal,
                    Shown {
                        stamp: *stamp,
                        cols,
                        all,
                        drawn: None,
                        snapshot: snapshot(&[], cols),
                        max: 0,
                    },
                );
            }
            let Some(shown) = self.shown.get_mut(&terminal) else {
                continue;
            };
            shown.max = max_scroll(&shown.all, rows);
            let scroll = self.scroll.entry(terminal).or_default();
            *scroll = (*scroll).min(shown.max);
            if shown.drawn != Some((rows, *scroll)) {
                shown.snapshot = snapshot(&window(&shown.all, rows, *scroll), cols);
                shown.drawn = Some((rows, *scroll));
            }
        }
    }

    /// The rows last drawn for `terminal`.
    pub fn shown(&self, terminal: u32) -> Option<&SnapshotWire> {
        self.shown.get(&terminal).map(|s| &s.snapshot)
    }

    /// How far back `terminal` is scrolled, in rows.
    pub fn scrolled(&self, terminal: u32) -> usize {
        self.scroll.get(&terminal).copied().unwrap_or(0)
    }

    /// Scroll `terminal` back (`up`) or toward the newest row by `step` rows, within what it
    /// was last drawn with.
    pub fn scroll_by(&mut self, terminal: u32, up: bool, step: usize) {
        let max = self.shown.get(&terminal).map_or(0, |s| s.max);
        let at = self.scroll.entry(terminal).or_default();
        *at = if up {
            at.saturating_add(step).min(max)
        } else {
            at.saturating_sub(step)
        };
    }

    /// Back to the newest row, as typing does.
    pub fn to_newest(&mut self, terminal: u32) {
        self.scroll.remove(&terminal);
    }
}

/// The program names a memory file holds. A file that is not text, or lines that are not
/// names, are skipped: the choice is a convenience and never an error.
fn programs_in(file: &std::path::Path) -> BTreeSet<String> {
    std::fs::read(file)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(program_line)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Item {
        Item::User {
            seq: 0,
            ts: None,
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    fn agent(text: &str) -> Item {
        Item::Agent {
            seq: 0,
            ts: None,
            text: text.into(),
            model: None,
        }
    }

    fn thinking(ms: Option<u64>) -> Item {
        Item::Thinking {
            seq: 0,
            ms,
            text: None,
        }
    }

    fn tool(name: &str, summary: &str, status: Status, exit_code: Option<i32>) -> Item {
        Item::Tool {
            seq: 0,
            id: "t".into(),
            name: name.into(),
            summary: summary.into(),
            status,
            exit_code,
            duration_ms: None,
            output_bytes: 0,
            has_detail: false,
        }
    }

    fn lines(items: &[Item], cols: usize) -> Vec<String> {
        layout(items, cols).into_iter().map(|r| r.text).collect()
    }

    fn style_at(row: &StyledRow, at: usize) -> Style {
        row.runs
            .iter()
            .find(|r| at >= r.at && at < r.at + r.len)
            .expect("a run")
            .style
    }

    fn digest(items: Vec<Item>) -> Digest {
        Digest {
            revision: 1,
            first_seq: 0,
            has_older: false,
            items,
            warnings: 0,
        }
    }

    #[test]
    fn the_persons_message_is_a_raised_block_as_wide_as_the_pane() {
        let rows = layout(&[user("fix the build")], 24);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, format!("{:<24}", " > fix the build"));
        assert!(rows[0].runs.iter().all(|r| r.style.raised));
        assert_eq!(width::width(&rows[0].text), 24);
    }

    #[test]
    fn a_long_message_wraps_inside_the_block_and_every_row_is_raised_and_full() {
        let rows = layout(&[user("one two three four five six seven")], 20);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            text,
            [
                " > one two three    ",
                "   four five six    ",
                "   seven            "
            ]
        );
        assert!(rows.iter().all(|r| r.runs.iter().all(|r| r.style.raised)));
    }

    #[test]
    fn an_attachment_is_a_name_and_a_size_never_bytes() {
        let item = Item::User {
            seq: 0,
            ts: None,
            text: "see".into(),
            attachments: vec![
                Chip {
                    name: "a.png".into(),
                    bytes: Some(20_480),
                },
                Chip {
                    name: "b".into(),
                    bytes: None,
                },
            ],
        };
        let text = lines(&[item], 30);
        assert_eq!(text[1].trim_end(), "   [a.png 20 KB]");
        assert_eq!(text[2].trim_end(), "   [b]");
    }

    #[test]
    fn the_agents_text_is_plain_and_uses_the_whole_width() {
        let rows = layout(&[agent("one two three four five six seven")], 20);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, ["one two three four", "five six seven"]);
        assert!(rows.iter().all(|r| r.runs.iter().all(|r| !r.style.raised)));
    }

    #[test]
    fn bold_and_inline_code_are_styled_and_their_markers_are_gone() {
        let rows = layout(&[agent("run **now** with `cargo test`")], 40);
        assert_eq!(rows[0].text, "run now with cargo test");
        assert_eq!(style_at(&rows[0], 5).attrs, attrs::BOLD);
        assert_eq!(style_at(&rows[0], 0).attrs, 0);
        assert!(style_at(&rows[0], 14).raised);
        assert!(!style_at(&rows[0], 10).raised);
    }

    #[test]
    fn a_marker_with_no_partner_stays_as_it_was_written() {
        assert_eq!(
            lines(&[agent("2 ** 3 and a ` tick")], 40),
            ["2 ** 3 and a ` tick"]
        );
    }

    #[test]
    fn fenced_code_is_a_dim_block_that_is_not_reflowed() {
        let text = "before\n```rust\nlet  x = 1;\n\nlet y = 22222222;\n```\nafter";
        let rows = layout(&[agent(text)], 12);
        let got: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            got,
            [
                "before",
                "  let  x = 1",
                "  ;",
                "  ",
                "  let y = 22",
                "  222222;",
                "after"
            ]
        );
        assert_eq!(style_at(&rows[1], 4).role, Role::Detail);
        assert_eq!(style_at(&rows[0], 0).role, Role::Text);
    }

    #[test]
    fn lists_hang_their_wrapped_rows_under_the_text() {
        let text = "- first item that wraps around\n  1. nested\n12. twelve";
        let got = lines(&[agent(text)], 16);
        assert_eq!(
            got,
            [
                "- first item",
                "  that wraps",
                "  around",
                "  1. nested",
                "12. twelve"
            ]
        );
    }

    #[test]
    fn a_heading_is_bold_without_its_hashes_and_blank_lines_collapse() {
        let rows = layout(&[agent("## Plan\n\n\n\nbody")], 20);
        let got: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(got, ["Plan", "", "body"]);
        assert_eq!(style_at(&rows[0], 0).attrs, attrs::BOLD);
    }

    #[test]
    fn reasoning_is_one_dim_row_with_no_text() {
        let got = lines(&[thinking(Some(3200))], 40);
        assert_eq!(got, ["Thought for 3 s"]);
        assert_eq!(lines(&[thinking(Some(10))], 40), ["Thought for 1 s"]);
        assert_eq!(lines(&[thinking(None)], 40), ["Thought"]);
        let with_text = Item::Thinking {
            seq: 0,
            ms: Some(2000),
            text: Some("secret plan".into()),
        };
        assert_eq!(lines(&[with_text], 40), ["Thought for 2 s"]);
        assert_eq!(
            style_at(&layout(&[thinking(None)], 40)[0], 0).role,
            Role::Detail
        );
    }

    #[test]
    fn every_status_has_its_word_and_mark_and_not_only_a_colour() {
        let cases = [
            (Status::Ok, "\u{25cf} ok Bash cargo test", Role::Done),
            (
                Status::Failed,
                "\u{25c6} failed Bash cargo test",
                Role::Failed,
            ),
            (
                Status::Running,
                "\u{25cb} running Bash cargo test",
                Role::Working,
            ),
            (
                Status::Interrupted,
                "~ interrupted Bash cargo test",
                Role::Detail,
            ),
            (Status::Rejected, "x rejected Bash cargo test", Role::Detail),
        ];
        for (status, want, role) in cases {
            let rows = layout(&[tool("Bash", "cargo test", status, None)], 60);
            assert_eq!(rows[0].text, want);
            assert_eq!(style_at(&rows[0], 0).role, role, "{want}");
        }
    }

    #[test]
    fn an_exit_code_shows_when_it_is_not_zero() {
        let failed = tool("Bash", "make", Status::Failed, Some(2));
        let fine = tool("Bash", "make", Status::Ok, Some(0));
        assert_eq!(lines(&[failed], 60), ["\u{25c6} failed Bash make  exit 2"]);
        assert_eq!(lines(&[fine], 60), ["\u{25cf} ok Bash make"]);
    }

    #[test]
    fn a_long_summary_is_cut_and_the_exit_code_stays() {
        let item = tool(
            "Bash",
            "cargo test --workspace --all-targets",
            Status::Failed,
            Some(101),
        );
        let got = lines(&[item], 34);
        assert_eq!(got, ["\u{25c6} failed Bash cargo tes\u{2026}  exit 101"]);
        assert_eq!(width::width(&got[0]), 34);
    }

    #[test]
    fn a_summary_is_one_line() {
        let item = tool("Bash", "echo a\n\techo   b", Status::Ok, None);
        assert_eq!(lines(&[item], 60), ["\u{25cf} ok Bash echo a echo b"]);
    }

    #[test]
    fn consecutive_tool_calls_fold_into_one_steps_row() {
        let items = [
            tool("Read", "a.rs", Status::Ok, None),
            thinking(Some(1000)),
            tool("Edit", "a.rs", Status::Ok, None),
            tool("Bash", "make", Status::Ok, Some(0)),
        ];
        assert_eq!(lines(&items, 40), ["\u{25cf} 3 steps"]);
    }

    #[test]
    fn a_fold_says_what_failed_or_is_still_running() {
        let failed = [
            tool("Read", "a", Status::Ok, None),
            tool("Bash", "b", Status::Failed, Some(1)),
        ];
        assert_eq!(lines(&failed, 40), ["\u{25c6} 2 steps, 1 failed"]);
        let running = [
            tool("Read", "a", Status::Ok, None),
            tool("Bash", "b", Status::Running, None),
            tool("Bash", "c", Status::Rejected, None),
        ];
        assert_eq!(
            lines(&running, 40),
            ["\u{25cb} 3 steps, 1 running, 1 rejected"]
        );
        let rows = layout(&failed, 40);
        assert_eq!(style_at(&rows[0], 0).role, Role::Failed);
    }

    #[test]
    fn one_tool_call_and_its_reasoning_stay_as_rows_of_their_own() {
        let items = [thinking(Some(2000)), tool("Read", "a.rs", Status::Ok, None)];
        assert_eq!(
            lines(&items, 40),
            ["Thought for 2 s", "\u{25cf} ok Read a.rs"]
        );
    }

    #[test]
    fn a_message_between_tool_runs_ends_the_fold() {
        let items = [
            tool("Read", "a", Status::Ok, None),
            tool("Read", "b", Status::Ok, None),
            agent("done reading"),
            tool("Edit", "c", Status::Ok, None),
            tool("Edit", "d", Status::Ok, None),
        ];
        assert_eq!(
            lines(&items, 40),
            [
                "\u{25cf} 2 steps",
                "",
                "done reading",
                "",
                "\u{25cf} 2 steps"
            ]
        );
    }

    #[test]
    fn errors_notices_compaction_and_subagents_are_one_dim_row_each() {
        let items = [
            Item::Error {
                seq: 0,
                text: "rate limited\nretry in 5s".into(),
            },
            Item::Notice {
                seq: 0,
                kind: "hook".into(),
                text: "ran".into(),
            },
            Item::Compaction { seq: 0 },
            Item::Subagent {
                seq: 0,
                summary: "review".into(),
                steps: 7,
            },
        ];
        let rows = layout(&items, 40);
        let got: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            got,
            [
                "error: rate limited retry in 5s",
                "hook: ran",
                "context compacted",
                "subagent review, 7 steps"
            ]
        );
        assert!(rows.iter().all(|r| style_at(r, 0).role == Role::Detail));
    }

    #[test]
    fn messages_are_a_blank_row_apart_and_steps_sit_tight() {
        let items = [
            user("hi"),
            agent("hello"),
            thinking(None),
            Item::Compaction { seq: 0 },
            agent("bye"),
        ];
        let got: Vec<String> = lines(&items, 10)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            got,
            [
                " > hi",
                "",
                "hello",
                "",
                "Thought",
                "context c\u{2026}",
                "",
                "bye"
            ]
        );
    }

    #[test]
    fn the_newest_is_at_the_bottom_and_scroll_counts_from_there() {
        let items: Vec<Item> = (0..6).map(|n| agent(&format!("m{n}"))).collect();
        // Rows: m0, blank, m1, blank, m2, blank, m3, blank, m4, blank, m5 (11 rows).
        let top = |scroll| -> Vec<String> {
            view(&items, 10, 4, scroll)
                .into_iter()
                .map(|r| r.text)
                .collect()
        };
        assert_eq!(top(0), ["", "m4", "", "m5"]);
        assert_eq!(top(2), ["", "m3", "", "m4"]);
        // Past the oldest row it stops there.
        assert_eq!(top(500), ["m0", "", "m1", ""]);
    }

    #[test]
    fn a_short_chat_starts_at_the_top_and_the_view_is_always_the_height_asked() {
        let got = view(&[agent("hi")], 10, 4, 3);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].text, "hi");
        assert!(got[1..].iter().all(|r| r.text.is_empty()));
        assert_eq!(view(&[], 10, 3, 0).len(), 3);
        assert_eq!(view(&[agent("hi")], 10, 0, 0).len(), 0);
    }

    /// The columns a terminal draws: a character is its width, and a heart with an emoji
    /// selector is two where the character alone is one.
    fn drawn_columns(text: &str) -> usize {
        let chars: Vec<char> = text.chars().collect();
        chars
            .iter()
            .enumerate()
            .map(|(at, c)| {
                let selector = chars.get(at + 1) == Some(&'\u{fe0f}');
                width::columns(*c).max(if selector { 2 } else { 0 })
            })
            .sum()
    }

    fn wide_items() -> Vec<Item> {
        vec![
            user(
                "a very long message with 世界 wide \u{2764}\u{fe0f} characters 🌍🌍 and averyveryverylongwordthatneverbreaks",
            ),
            agent(
                "**bold 世界 words that run on** and `inline 🌍 code that is long` plus 世界世界世界世界 text \u{2764}\u{fe0f}\u{2764}\u{fe0f}\n- item 世界 with a long line of words to wrap\n1. 世界\n```\ncode 世界 with a long line 🌍🌍🌍 that must be cut up\n\u{2764}\u{fe0f}\u{2764}\u{fe0f}\u{2764}\u{fe0f}\n```\n## 世界 heading",
            ),
            thinking(Some(1500)),
            tool(
                "Bash世界",
                "cargo 🌍 test --workspace --all-targets",
                Status::Failed,
                Some(101),
            ),
            tool(
                "Read",
                "a/very/long/path/to/a/世界/file.rs",
                Status::Ok,
                None,
            ),
            Item::Error {
                seq: 0,
                text: "something went wrong with 世界 \u{2764}\u{fe0f}".into(),
            },
            Item::Notice {
                seq: 0,
                kind: "hook世界".into(),
                text: "🌍".into(),
            },
            Item::Subagent {
                seq: 0,
                summary: "review 世界".into(),
                steps: 3,
            },
        ]
    }

    #[test]
    fn no_row_is_wider_than_the_pane_at_any_width_from_nothing_to_forty() {
        let items = wide_items();
        for cols in 0..=40usize {
            for row in layout(&items, cols) {
                let drawn = drawn_columns(&row.text);
                assert!(drawn <= cols, "{cols}: {drawn} columns in {:?}", row.text);
                assert!(!row.text.contains('\u{fe0f}'), "{cols}: {:?}", row.text);
                let covered: usize = row.runs.iter().map(|r| r.len).sum();
                assert!(
                    covered <= row.text.chars().count(),
                    "{cols}: runs past the text"
                );
            }
            // As the grid the painters read: spacers and all, still no wider.
            let grid = snapshot(&view(&items, cols, 6, 0), cols);
            assert_eq!(grid.lines.len(), 6);
            for line in &grid.lines {
                assert!(line.chars().count() <= cols, "{cols}: {line:?}");
            }
            if cols == 0 {
                assert!(layout(&items, cols).is_empty());
            }
        }
        // Wide characters in a pane of one column are dropped, not halved.
        assert_eq!(lines(&[agent("世a")], 1), ["", "a"]);
    }

    #[test]
    fn nothing_from_a_file_draws_as_a_control_or_a_bidi_override() {
        let evil = "a\u{85}b\u{9b}c\u{202e}d\u{2066}e\u{2069}f\u{7}g\u{1b}h";
        let items = [
            user(evil),
            agent(evil),
            tool(evil, evil, Status::Ok, None),
            Item::Error {
                seq: 0,
                text: evil.into(),
            },
        ];
        for row in layout(&items, 40) {
            assert!(
                row.text.chars().all(width::drawn),
                "a control or override in {:?}",
                row.text
            );
        }
        assert_eq!(lines(&[agent(evil)], 40), ["abcdefgh"]);
    }

    #[test]
    fn wide_characters_wrap_by_columns_and_never_split() {
        // Each is two columns: four fit a row of nine.
        let got = lines(&[agent("世界世界世界世界世界")], 9);
        assert_eq!(got, ["世界世界", "世界世界", "世界"]);
        let narrow = lines(&[agent("a世界b")], 3);
        assert_eq!(narrow, ["a世", "界b"]);
    }

    #[test]
    fn narrow_panes_at_twenty_columns_read_as_text() {
        let items = [
            user("please run the whole test suite now"),
            agent("Sure. Running `cargo test` first."),
            tool("Bash", "cargo test --workspace", Status::Failed, Some(2)),
            agent("Two tests failed."),
        ];
        let got: Vec<String> = lines(&items, 20)
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(
            got,
            [
                " > please run the",
                "   whole test suite",
                "   now",
                "",
                "Sure. Running cargo",
                "test first.",
                "",
                "\u{25c6} failed Ba\u{2026}  exit 2",
                "",
                "Two tests failed."
            ]
        );
    }

    #[test]
    fn text_from_a_file_never_carries_a_control_character_out() {
        let got = lines(&[agent("a\x1b]52;c;aGk=\x07b\tc")], 40);
        assert_eq!(got, ["a]52;c;aGk=b c"]);
    }

    #[test]
    fn the_snapshot_is_a_grid_with_a_spacer_after_each_wide_character_and_no_cursor() {
        let rows = view(&[agent("世a `b`")], 10, 2, 0);
        let snap = snapshot(&rows, 10);
        assert_eq!(
            (snap.cols, snap.rows, snap.lines.len(), snap.styles.len()),
            (10, 2, 2, 2)
        );
        assert_eq!(snap.lines[0], "世 a b");
        assert!(!snap.modes.cursor_visible);
        assert!(!snap.modes.mouse_reporting && !snap.modes.alt_screen);
        assert_eq!(snap.behind, 0);
        // `b` is the sixth cell now (世, spacer, a, space, b) so the raised run starts at 4.
        let code = snap.styles[0]
            .iter()
            .find(|s| s.bg.is_some())
            .expect("code run");
        assert_eq!((code.at, code.len), (4, 1));
        assert_eq!(snap.lines[0].chars().nth(code.at), Some('b'));
    }

    #[test]
    fn the_snapshot_paints_through_the_pane_painter_with_the_raised_block() {
        let rows = view(&[user("hi")], 10, 1, 0);
        let snap = snapshot(&rows, 10);
        let painted = crate::screen::paint_row(&snap.lines[0], &snap.styles[0], &[], 10, None);
        assert!(painted.contains("48;5;"), "{painted:?}");
        assert!(painted.contains("hi"), "{painted:?}");
    }

    fn row(terminal: u32, program: &str) -> AgentRow {
        qoop_runtime::control::AgentRow {
            terminal,
            workspace: 1,
            name: None,
            program: Some(program.into()),
            behind: None,
            state: "idle".into(),
            readiness: "ready".into(),
            agent: true,
            ..serde_json::from_value(serde_json::json!({
                "terminal": terminal,
                "workspace": 1,
                "program": program,
                "behind": null,
                "state": "idle",
                "generation": null
            }))
            .unwrap()
        }
    }

    #[test]
    fn a_terminal_wants_a_digest_only_while_its_switch_is_on() {
        let mut chat = ChatState::default();
        assert!(!chat.wants_digest(3));
        assert!(chat.toggle(3, Some("claude")));
        assert!(chat.wants_digest(3));
        assert_eq!(chat.wanting().collect::<Vec<_>>(), [3]);
        assert!(!chat.toggle(3, Some("claude")));
        assert!(!chat.wants_digest(3));
        assert_eq!(chat.wanting().count(), 0);
    }

    #[test]
    fn nothing_shows_until_a_digest_with_items_arrives() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        assert!(!chat.has_view(3));
        chat.set_digest(3, digest(Vec::new()));
        assert!(!chat.has_view(3), "an empty session is the terminal");
        chat.set_digest(3, digest(vec![agent("hi")]));
        assert!(chat.has_view(3));
        chat.toggle(3, None);
        assert!(!chat.has_view(3));
    }

    #[test]
    fn the_choice_is_remembered_per_program_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("interface-chat");
        let mut chat = ChatState::read(Some(file.clone()));
        chat.toggle(3, Some("claude"));
        chat.toggle(4, Some("codex"));
        chat.toggle(4, Some("codex"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "claude\n");

        // A new window: a new tab of claude starts on, a codex tab and a shell do not.
        let mut next = ChatState::read(Some(file.clone()));
        next.adopt(&[row(10, "claude"), row(11, "codex")]);
        assert!(next.wants_digest(10));
        assert!(!next.wants_digest(11));

        // A person's own switch is not undone by the next list of rows.
        next.toggle(10, Some("claude"));
        next.adopt(&[row(10, "claude"), row(11, "codex")]);
        assert!(!next.wants_digest(10));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
        assert!(!ChatState::read(Some(file)).programs.contains("claude"));
    }

    #[test]
    fn a_shell_never_adopts_and_a_closed_terminal_takes_its_state_with_it() {
        let mut shell = row(5, "claude");
        shell.agent = false;
        let mut chat = ChatState::default();
        chat.programs.insert("claude".into());
        chat.adopt(&[shell, row(6, "claude")]);
        assert!(!chat.wants_digest(5));
        assert!(chat.wants_digest(6));
        chat.set_digest(6, digest(vec![agent("hi")]));
        chat.adopt(&[]);
        assert!(!chat.wants_digest(6));
        assert!(!chat.has_view(6));
    }

    #[test]
    fn render_draws_at_the_size_given_and_only_rebuilds_on_a_change() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(3, digest((0..8).map(|n| agent(&format!("m{n}"))).collect()));
        chat.render(&[want(3, 10, 4)]);
        let first = chat.shown(3).unwrap().clone();
        assert_eq!((first.cols, first.rows), (10, 4));
        assert_eq!(first.lines[3], "m7");
        assert!(chat.shown(4).is_none());

        // Scrolled back: older rows. Too far: held at the oldest.
        chat.scroll_by(3, true, 3);
        chat.render(&[want(3, 10, 4)]);
        assert_eq!(chat.shown(3).unwrap().lines[2], "m5");
        chat.scroll_by(3, true, 1000);
        chat.render(&[want(3, 10, 4)]);
        assert_eq!(chat.scrolled(3), 11);
        assert_eq!(chat.shown(3).unwrap().lines[0], "m0");
        // Back down is not stuck behind the overshoot.
        chat.scroll_by(3, false, 2);
        assert_eq!(chat.scrolled(3), 9);
        chat.to_newest(3);
        chat.render(&[want(3, 10, 4)]);
        assert_eq!(chat.shown(3).unwrap(), &first);

        // A new digest redraws; a terminal no longer wanted is forgotten.
        chat.set_digest(3, digest(vec![agent("new")]));
        chat.render(&[want(3, 10, 4)]);
        assert_eq!(chat.shown(3).unwrap().lines[0], "new");
        chat.render(&[]);
        assert!(chat.shown(3).is_none());
    }

    fn want(terminal: u32, cols: usize, rows: usize) -> Want {
        Want {
            terminal,
            cols,
            rows,
            held: false,
        }
    }

    fn numbered(count: u32) -> Vec<Item> {
        (0..count).map(|n| agent(&format!("m{n}"))).collect()
    }

    fn visible(chat: &ChatState, terminal: u32) -> Vec<String> {
        chat.shown(terminal).expect("drawn").lines.clone()
    }

    #[test]
    fn a_terminal_that_stops_being_an_agent_or_changes_program_loses_its_chat() {
        let mut chat = ChatState::default();
        chat.programs.insert("claude".into());
        chat.adopt(&[row(5, "claude"), row(6, "claude")]);
        chat.set_digest(5, digest(numbered(3)));
        chat.set_digest(6, digest(numbered(3)));
        chat.scroll_by(5, true, 1);
        assert!(chat.has_view(5) && chat.has_view(6));

        // Claude exits to a shell: no agent, no chat, no digest, and the shell never adopts.
        let mut shell = row(5, "zsh");
        shell.agent = false;
        // Claude replaced by codex: another session, and codex is not remembered.
        chat.adopt(&[shell, row(6, "codex")]);
        for terminal in [5, 6] {
            assert!(!chat.wants_digest(terminal), "{terminal}");
            assert!(!chat.has_view(terminal), "{terminal}");
            assert_eq!(chat.scrolled(terminal), 0, "{terminal}");
        }
        chat.render(&[want(5, 10, 4)]);
        assert!(chat.shown(5).is_none());

        // Claude starts again in the tab: the remembered choice applies afresh, with no
        // digest of the session that ended.
        chat.adopt(&[row(5, "claude"), row(6, "codex")]);
        assert!(chat.wants_digest(5) && !chat.has_view(5));
        assert!(!chat.wants_digest(6));
    }

    #[test]
    fn a_program_that_cannot_be_read_for_a_moment_is_not_another_program() {
        let mut chat = ChatState::default();
        chat.toggle(5, None);
        chat.adopt(&[row(5, "claude")]);
        chat.toggle(5, None);
        chat.toggle(5, None);
        chat.set_digest(5, digest(numbered(2)));
        let mut unread = row(5, "claude");
        unread.program = None;
        chat.adopt(&[unread]);
        chat.adopt(&[row(5, "claude")]);
        assert!(chat.has_view(5), "kept through the gap");
    }

    #[test]
    fn a_reused_terminal_id_starts_clean() {
        let mut chat = ChatState::default();
        chat.adopt(&[row(7, "claude")]);
        chat.toggle(7, Some("nobody"));
        chat.set_digest(7, digest(numbered(30)));
        chat.render(&[want(7, 10, 4)]);
        chat.scroll_by(7, true, 5);
        chat.typed(7, b"x", Instant::now());
        assert!(chat.typing(7));

        chat.adopt(&[]);
        chat.adopt(&[row(7, "claude")]);
        assert!(!chat.wants_digest(7) && !chat.has_view(7) && !chat.typing(7));
        assert_eq!(chat.scrolled(7), 0);
        assert_eq!(chat.wants_older(7), None);
        chat.render(&[want(7, 10, 4)]);
        assert!(chat.shown(7).is_none());
    }

    #[test]
    fn a_corrupt_memory_file_is_no_choice_and_never_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("interface-chat");
        let mut bytes = b"\xff\xfe\x00\n".to_vec();
        bytes.extend_from_slice("x".repeat(500).as_bytes());
        bytes.extend_from_slice(b"\nbad\x1b[31mname\nclaude\n\n  codex  \n");
        std::fs::write(&file, &bytes).unwrap();
        let chat = ChatState::read(Some(file.clone()));
        assert_eq!(
            chat.programs.iter().map(String::as_str).collect::<Vec<_>>(),
            ["claude", "codex"]
        );
        // A directory where the file should be, and a missing one: nothing on, no panic.
        assert!(
            ChatState::read(Some(dir.path().to_path_buf()))
                .programs
                .is_empty()
        );
        assert!(
            ChatState::read(Some(dir.path().join("none")))
                .programs
                .is_empty()
        );
        // Writing over it replaces what was not a choice.
        let mut chat = chat;
        chat.toggle(1, Some("gemini"));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "claude\ncodex\ngemini\n"
        );
    }

    #[test]
    fn two_interfaces_never_write_over_each_others_switch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("interface-chat");
        // Both started before either switched anything.
        let mut one = ChatState::read(Some(file.clone()));
        let mut two = ChatState::read(Some(file.clone()));
        one.toggle(1, Some("claude"));
        two.toggle(2, Some("codex"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "claude\ncodex\n");
        // The first one switching claude off does not take codex with it.
        one.toggle(1, Some("claude"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "codex\n");
        assert!(
            one.programs.contains("codex"),
            "it learned what the other wrote"
        );
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_scroll_or_a_new_height_does_not_lay_the_chat_out_again() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(3, digest(numbered(40)));
        chat.render(&[want(3, 10, 4)]);
        assert_eq!(chat.layouts, 1);
        chat.render(&[want(3, 10, 4)]);
        chat.scroll_by(3, true, 3);
        chat.render(&[want(3, 10, 4)]);
        chat.render(&[want(3, 10, 6)]);
        assert_eq!(chat.layouts, 1, "scroll and height reuse the rows");
        // The same page again is no change; a new one, or a new width, is.
        chat.set_digest(3, digest(numbered(40)));
        chat.render(&[want(3, 10, 6)]);
        assert_eq!(chat.layouts, 1);
        chat.render(&[want(3, 12, 6)]);
        assert_eq!(chat.layouts, 2);
        chat.set_digest(3, digest(numbered(41)));
        chat.render(&[want(3, 12, 6)]);
        assert_eq!(chat.layouts, 3);
    }

    #[test]
    fn a_hit_on_a_big_chat_costs_under_a_millisecond() {
        let text = "word ".repeat(4096 / 5);
        let items: Vec<Item> = (0..60).map(|_| agent(&text)).collect();
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(3, digest(items));
        chat.render(&[want(3, 100, 30)]);
        let start = Instant::now();
        for _ in 0..200 {
            chat.render(&[want(3, 100, 30)]);
        }
        let each = start.elapsed() / 200;
        assert!(each < Duration::from_millis(1), "{each:?} a hit");
        assert_eq!(chat.layouts, 1);
    }

    #[test]
    fn new_rows_arriving_while_scrolled_back_do_not_move_what_is_read() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(3, digest(numbered(20)));
        chat.render(&[want(3, 10, 5)]);
        chat.scroll_by(3, true, 7);
        chat.render(&[want(3, 10, 5)]);
        let before = visible(&chat, 3);
        assert!(before.iter().any(|l| l.starts_with('m')));

        // Three messages arrive below: six rows (a blank between each).
        chat.set_digest(3, digest(numbered(23)));
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(visible(&chat, 3), before, "the same text under the reader");
        assert_eq!(chat.scrolled(3), 7 + 6);

        // At the bottom and not holding anything, the view follows the newest.
        chat.to_newest(3);
        chat.set_digest(3, digest(numbered(24)));
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(chat.scrolled(3), 0);
        assert_eq!(visible(&chat, 3)[4], "m23");
    }

    #[test]
    fn a_held_selection_keeps_its_text_even_at_the_bottom() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(3, digest(numbered(20)));
        chat.render(&[want(3, 10, 5)]);
        let before = visible(&chat, 3);
        let held = |terminal| Want {
            held: true,
            ..want(terminal, 10, 5)
        };
        chat.set_digest(3, digest(numbered(22)));
        chat.render(&[held(3)]);
        assert_eq!(visible(&chat, 3), before);
        assert_eq!(chat.scrolled(3), 4);
    }

    #[test]
    fn older_items_are_asked_for_at_the_top_and_put_in_front_without_moving_the_reader() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        let newest = Digest {
            revision: 9,
            first_seq: 10,
            has_older: true,
            items: numbered(10),
            warnings: 0,
        };
        chat.set_digest(3, newest);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(chat.wants_older(3), None, "at the bottom, not the top");
        chat.scroll_by(3, true, 1000);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(chat.wants_older(3), Some(10), "the items held, for `skip`");
        assert_eq!(chat.wants_older(3), None, "asked once");
        let before = visible(&chat, 3);

        let older = Digest {
            revision: 9,
            first_seq: 0,
            has_older: false,
            items: (0..10).map(|n| agent(&format!("old{n}"))).collect(),
            warnings: 0,
        };
        chat.set_older(3, older);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(visible(&chat, 3), before, "the reader stays put");
        assert_eq!(chat.digests[&3].1.items.len(), 20);
        assert_eq!(chat.digests[&3].1.first_seq, 0);
        // Scrolled on to the new top: nothing older is left to ask for.
        chat.scroll_by(3, true, 1000);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(visible(&chat, 3)[0], "old0");
        assert_eq!(chat.wants_older(3), None);
    }

    #[test]
    fn an_older_page_of_another_revision_replaces_everything() {
        let mut chat = ChatState::default();
        chat.toggle(3, None);
        chat.set_digest(
            3,
            Digest {
                revision: 1,
                first_seq: 5,
                has_older: true,
                items: numbered(10),
                warnings: 0,
            },
        );
        chat.render(&[want(3, 10, 5)]);
        chat.scroll_by(3, true, 1000);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(chat.wants_older(3), Some(10));
        // The session changed under the request: the daemon answers with the newest page.
        chat.set_older(
            3,
            Digest {
                revision: 2,
                first_seq: 0,
                has_older: false,
                items: vec![agent("fresh")],
                warnings: 0,
            },
        );
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(chat.digests[&3].1.items.len(), 1);
        assert_eq!(chat.digests[&3].1.revision, 2);
        assert_eq!(chat.scrolled(3), 0);
        assert_eq!(visible(&chat, 3)[0], "fresh");
        assert_eq!(chat.wants_older(3), None);
    }

    #[test]
    fn typing_shows_the_terminal_until_enter_or_escape_or_thirty_seconds() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        // A chat that is off has nothing to come back to.
        chat.typed(3, b"a", t0);
        assert!(!chat.typing(3));

        chat.toggle(3, None);
        chat.typed(3, b"a", t0);
        assert!(chat.typing(3));
        // An arrow is an escape sequence, not an Esc.
        chat.typed(3, b"\x1b[A", t0);
        assert!(chat.typing(3));
        chat.typed(3, b"\r", t0);
        assert!(!chat.typing(3), "enter brings the chat back");
        chat.typed(3, b"hi", t0);
        chat.typed(3, b"\x1b", t0);
        assert!(!chat.typing(3), "so does a lone escape");

        // Thirty seconds from the last key, not the first.
        chat.typed(3, b"a", t0);
        chat.typed(3, b"b", t0 + Duration::from_secs(20));
        assert!(!chat.expire(t0 + Duration::from_secs(40)));
        assert!(chat.typing(3));
        assert!(chat.expire(t0 + Duration::from_secs(51)));
        assert!(!chat.typing(3));
        // Never stored with the choice.
        assert!(chat.programs.is_empty());

        chat.typed(3, b"a", t0);
        chat.toggle(3, None);
        assert!(!chat.typing(3), "a switch ends it");
    }

    #[test]
    fn the_flips_of_the_view_are_told_once() {
        let mut chat = ChatState::default();
        let set = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<u32>>();
        assert!(chat.flips(set(&[])).is_empty());
        assert_eq!(chat.flips(set(&[2])), set(&[2]));
        assert!(chat.flips(set(&[2])).is_empty());
        assert_eq!(chat.flips(set(&[3])), set(&[2, 3]));
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_one_dim_row_and_the_page_stays_whole() {
        let items = [agent("before"), Item::Unknown, agent("after")];
        let rows = layout(&items, 40);
        let got: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            got,
            ["before", "", "item this build cannot show", "", "after"]
        );
        assert_eq!(style_at(&rows[2], 0).role, Role::Detail);
        let unknown = tool("Bash", "make", Status::Unknown, None);
        let row = &layout(&[unknown], 40)[0];
        assert_eq!(row.text, "? unknown Bash make");
        assert_eq!(style_at(row, 0).role, Role::Detail);
    }

    fn on(chat: &mut ChatState, terminal: u32) {
        chat.toggle(terminal, None);
    }

    fn page(revision: u64, items: Vec<Item>) -> Digest {
        Digest {
            revision,
            first_seq: 0,
            has_older: false,
            items,
            warnings: 0,
        }
    }

    #[test]
    fn the_first_read_goes_at_once_one_at_a_time_and_never_for_a_flag_that_is_off() {
        let now = Instant::now();
        let mut chat = ChatState::default();
        assert!(chat.plan(now).is_empty(), "no flag, no request");
        assert_eq!(chat.wake_at(now), None);
        on(&mut chat, 3);
        assert_eq!(chat.wake_at(now), Some(now));
        assert_eq!(
            chat.plan(now),
            [Request {
                terminal: 3,
                skip: 0,
                limit: 60,
                revision: None,
                older: false
            }]
        );
        // Out: nothing again, however long, until it is taken as lost.
        assert!(chat.plan(now + Duration::from_secs(5)).is_empty());
        assert_eq!(chat.plan(now + Duration::from_secs(11)).len(), 1);
        chat.toggle(3, None);
        assert!(chat.plan(now + Duration::from_secs(60)).is_empty());
        assert_eq!(chat.wake_at(now), None);
    }

    #[test]
    fn a_refresh_asks_for_what_is_held_so_loaded_history_survives_it() {
        let now = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(now);
        for (held, want) in [(3u32, 60u32), (100, 100), (250, 200)] {
            chat.answered(3, false, page(7, numbered(held)), now);
            chat.changed(3, now);
            let asks = chat.plan(now + DEBOUNCE);
            assert_eq!(asks.len(), 1, "{held}");
            assert_eq!(asks[0].limit, want, "{held}");
            assert_eq!(asks[0].revision, Some(7));
            assert_eq!(asks[0].skip, 0);
        }
    }

    #[test]
    fn news_is_read_after_the_debounce_once_and_the_latest_wins() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        chat.answered(3, false, page(1, numbered(2)), t0);
        assert_eq!(chat.wake_at(t0), None, "quiet, and not on screen");

        chat.changed(3, t0 + Duration::from_millis(10));
        chat.changed(3, t0 + Duration::from_millis(100));
        assert_eq!(chat.wake_at(t0), Some(t0 + Duration::from_millis(260)));
        assert!(chat.plan(t0 + Duration::from_millis(200)).is_empty());
        assert_eq!(chat.plan(t0 + Duration::from_millis(260)).len(), 1);

        // News while that read is out: one more read after its answer, not a second in flight.
        chat.changed(3, t0 + Duration::from_millis(300));
        chat.changed(3, t0 + Duration::from_millis(310));
        assert!(chat.plan(t0 + Duration::from_secs(1)).is_empty());
        let answered = t0 + Duration::from_millis(1200);
        chat.answered(3, false, page(2, numbered(3)), answered);
        assert!(chat.plan(answered + Duration::from_millis(100)).is_empty());
        assert_eq!(chat.plan(answered + DEBOUNCE).len(), 1);

        // News for a terminal that is not flagged is nothing.
        chat.changed(9, t0);
        assert!(!chat.feeds.contains_key(&9));
    }

    #[test]
    fn a_chat_on_screen_is_read_again_before_the_daemon_forgets_it() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        on(&mut chat, 4);
        chat.plan(t0);
        for t in [3, 4] {
            chat.answered(t, false, page(1, numbered(2)), t0);
        }
        chat.set_screen([3].into());
        assert!(chat.plan(t0 + Duration::from_secs(19)).is_empty());
        let asks = chat.plan(t0 + Duration::from_secs(20));
        assert_eq!(asks.iter().map(|a| a.terminal).collect::<Vec<_>>(), [3]);
        // Terminal 4 was never on screen: it is read when it comes on screen after a while.
        chat.set_screen([3, 4].into());
        assert_eq!(
            chat.wake_at(t0 + Duration::from_secs(30)),
            Some(t0 + Duration::from_secs(20))
        );
        let asks = chat.plan(t0 + Duration::from_secs(30));
        assert_eq!(asks.iter().map(|a| a.terminal).collect::<Vec<_>>(), [4]);
    }

    #[test]
    fn no_session_waits_three_seconds_and_keeps_the_flag() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        assert_eq!(chat.refused(3, false, "no_session", t0), None);
        assert!(chat.wants_digest(3) && !chat.is_refused(3));
        assert!(chat.plan(t0 + Duration::from_secs(2)).is_empty());
        assert_eq!(chat.plan(t0 + Duration::from_secs(3)).len(), 1);
        // And again, for as long as the flag is on.
        chat.refused(3, false, "no_session", t0 + Duration::from_secs(3));
        assert_eq!(chat.plan(t0 + Duration::from_secs(6)).len(), 1);
    }

    #[test]
    fn a_refusal_that_will_not_pass_turns_the_flag_off_and_says_why() {
        let t0 = Instant::now();
        for (reason, said) in [
            ("not_agent", "chat is for agent tabs"),
            ("unavailable", "chat is not available for this tab"),
            ("whatever_else", "chat is not available for this tab"),
            (
                "too_large",
                "this session is too large for chat, use the terminal",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("interface-chat");
            let mut chat = ChatState::read(Some(file.clone()));
            chat.toggle(3, Some("claude"));
            chat.plan(t0);
            assert_eq!(chat.refused(3, false, reason, t0), Some(said), "{reason}");
            assert!(!chat.wants_digest(3) && chat.is_refused(3), "{reason}");
            assert!(
                chat.plan(t0 + Duration::from_secs(60)).is_empty(),
                "{reason}"
            );
            // The program's memory is the person's choice, not the daemon's.
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                "claude\n",
                "{reason}"
            );
            // The person asking again is a fresh try.
            chat.toggle(3, Some("claude"));
            assert!(!chat.is_refused(3) && chat.wants_digest(3));
        }
    }

    #[test]
    fn a_daemon_too_old_for_messages_ends_what_was_asked_of_it() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        on(&mut chat, 4);
        chat.plan(t0);
        chat.answered(4, false, page(1, numbered(2)), t0);
        chat.changed(4, t0);
        chat.plan(t0 + DEBOUNCE);
        let mut gone = chat.unsupported();
        gone.sort();
        assert_eq!(gone, [3, 4]);
        assert!(!chat.wants_digest(3) && !chat.wants_digest(4));
        assert!(chat.is_refused(3) && chat.is_refused(4));
        assert!(chat.unsupported().is_empty());
    }

    #[test]
    fn the_older_page_is_asked_at_the_top_with_the_items_held_as_skip_and_put_in_front() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        chat.answered(
            3,
            false,
            Digest {
                has_older: true,
                ..page(5, numbered(10))
            },
            t0,
        );
        chat.render(&[want(3, 10, 5)]);
        assert!(chat.plan(t0).is_empty(), "at the bottom");
        chat.scroll_by(3, true, 1000);
        let asks = chat.plan(t0);
        assert_eq!(
            asks,
            [Request {
                terminal: 3,
                skip: 10,
                limit: 60,
                revision: Some(5),
                older: true
            }]
        );
        assert!(chat.plan(t0).is_empty(), "asked once");
        let older: Vec<Item> = (0..4).map(|n| agent(&format!("old{n}"))).collect();
        chat.answered(3, true, page(5, older), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 14);
        assert!(chat.plan(t0).is_empty(), "nothing older is left");
    }

    #[test]
    fn an_answer_for_a_flag_that_went_off_is_dropped() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        chat.toggle(3, None);
        chat.answered(3, false, page(1, numbered(2)), t0);
        assert!(!chat.has_view(3) && chat.digests.is_empty());
    }
}

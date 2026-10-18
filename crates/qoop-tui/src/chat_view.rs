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

use qoop_parsed::{Chip, Detail, Digest, Item, Status};
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
fn user_rows(text: &str, chips: &[Chip], queued: bool, cols: usize) -> Vec<StyledRow> {
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
    // Typed while the agent worked and not taken yet: the word, in place of colour.
    if queued {
        lines.extend(wrap(&cells("queued", dim), room));
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

/// The ink of a status, and the word that says it in place of colour (none for a call that
/// went well, or one whose status this build does not know).
fn state_of(status: Status) -> (Role, &'static str) {
    match status {
        Status::Running => (Role::Working, "running"),
        Status::Failed => (Role::Failed, "failed"),
        Status::Interrupted => (Role::Detail, "interrupted"),
        Status::Rejected => (Role::Detail, "rejected"),
        Status::Ok | Status::Unknown => (Role::Detail, ""),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The host of a url, as it is read in a row: no scheme, no `user:pass@`, no path.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// What a tool call says it did, as a person reads it: `Ran <command>`, `Read <file>`,
/// `Edited <file>`, `Searched <pattern>`, `Searched the web`, `Fetched <host>`, `Ran a
/// subagent: <summary>`. Only the tools it knows by name (spelled any way: `Bash`,
/// `exec_command`) get a sentence; any other is its name and its summary, so a tool that
/// merely mentions a word (`KillShell`, `BashOutput`, `TodoWrite`) never reads as another.
fn label(name: &str, summary: &str) -> String {
    let summary = one_line(summary);
    let said = |verb: &str| format!("{verb} {summary}").trim_end().to_string();
    let known: String = name
        .chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect();
    match known.as_str() {
        "bash" | "shell" | "execcommand" | "runshellcommand" => said("Ran"),
        "read" | "readfile" => said("Read"),
        "edit" | "write" | "multiedit" | "notebookedit" | "applypatch" | "patch" | "writefile"
        | "replace" => said("Edited"),
        "grep" | "glob" | "search" | "searchfilecontent" => said("Searched"),
        "websearch" | "googlewebsearch" => "Searched the web".to_string(),
        "webfetch" => match host(&summary) {
            "" => "Fetched a page".to_string(),
            host => format!("Fetched {host}"),
        },
        "task" | "agent" | "subagent" => subagent_label(&summary),
        _ => said(name),
    }
}

fn subagent_label(summary: &str) -> String {
    format!("Ran a subagent: {}", one_line(summary))
        .trim_end_matches([':', ' '])
        .to_string()
}

/// The fold glyph a row that opens ends with: closed, or open.
fn chevron(open: bool) -> &'static str {
    if open { " \u{25be}" } else { " \u{25b8}" }
}

/// One step as a quiet row: a mark, what it did, the state in words when it did not go well
/// (`running`, `failed`, `exit N`, never colour alone), and a chevron at the end when it opens.
/// What does not fit is cut from the label first; the words and the chevron always stay.
fn tool_line(
    text: &str,
    status: Status,
    exit_code: Option<i32>,
    open: Option<bool>,
    cols: usize,
) -> StyledRow {
    let (role, word) = state_of(status);
    let exit = exit_code
        .filter(|code| *code != 0)
        .map(|code| format!("exit {code}"));
    let tail = [Some(word.to_string()).filter(|w| !w.is_empty()), exit]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    let tail = if tail.is_empty() {
        tail
    } else {
        format!("  {tail}")
    };
    let end = open.map_or("", chevron);
    let room = cols.saturating_sub(2 + width::width(&tail) + width::width(end));
    let mut row = cells("\u{25cf} ", style(role));
    row.extend(cells(&cut(&one_line(text), room), style(Role::Detail)));
    row.extend(cells(&tail, style(role)));
    row.extend(cells(end, style(Role::Detail)));
    row_of(&row)
}

/// How a tool call or a subagent stands; nothing else has a status.
fn status_of(item: &Item) -> Option<Status> {
    match item {
        Item::Tool { status, .. } | Item::Subagent { status, .. } => Some(*status),
        _ => None,
    }
}

/// The words for what failed or is still going in a run of calls, after the time it took.
fn trouble(run: &[&Item]) -> (Role, String) {
    let count = |want: Status| run.iter().filter(|it| status_of(it) == Some(want)).count();
    let mut text = String::new();
    for (n, status) in [
        (count(Status::Failed), Status::Failed),
        (count(Status::Running), Status::Running),
        (count(Status::Interrupted), Status::Interrupted),
        (count(Status::Rejected), Status::Rejected),
    ] {
        if n > 0 {
            text.push_str(&format!(", {n} {}", state_of(status).1));
        }
    }
    let role = if count(Status::Failed) > 0 {
        Role::Failed
    } else if count(Status::Running) > 0 {
        Role::Working
    } else {
        Role::Detail
    };
    (role, text)
}

/// How long, as a person says it: `4 s`, `2 min 5 s`.
fn took(ms: u64) -> String {
    let s = ((ms + 500) / 1000).max(1);
    if s < 60 {
        format!("{s} s")
    } else {
        format!("{} min {} s", s / 60, s % 60)
    }
}

/// How long a run of calls took: from the first start to the last end when every call says
/// when it started (calls overlap, so their times do not add up), else the sum of their times
/// when every one says, else nothing.
fn worked(run: &[&Item]) -> Option<u64> {
    let calls: Vec<(Option<i64>, u64)> = run
        .iter()
        .map(|it| match it {
            Item::Tool {
                ts,
                duration_ms: Some(ms),
                ..
            } => Some((*ts, *ms)),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let spans: Option<Vec<(i64, i64)>> = calls
        .iter()
        .map(|(ts, ms)| ts.map(|at| (at, at.saturating_add(i64::try_from(*ms).unwrap_or(0)))))
        .collect();
    match spans {
        Some(spans) => {
            let first = spans.iter().map(|span| span.0).min()?;
            let last = spans.iter().map(|span| span.1).max()?;
            u64::try_from(last - first).ok()
        }
        None => Some(calls.iter().map(|call| call.1).sum()),
    }
}

/// A run of tool calls folded under one row: how long they took, else how many, and which
/// did not go well.
fn group_row(run: &[&Item], open: bool, cols: usize) -> StyledRow {
    let (role, trouble) = trouble(run);
    let head = match worked(run) {
        Some(ms) => format!("Worked for {}", took(ms)),
        None => format!("{} steps", run.len()),
    };
    let end = chevron(open);
    let room = cols.saturating_sub(width::width(end));
    let mut row = cells(&cut(&format!("{head}{trouble}"), room), style(role));
    row.extend(cells(end, style(Role::Detail)));
    row_of(&row)
}

/// The most rows of a tool's input and output an opened row shows.
const DETAIL_ROWS: usize = 30;

/// An opened tool: its input, a blank row, its output, in a dim raised block as wide as the
/// pane. A read that is out, failed (and waiting to be tried again) or found nothing says so in
/// one row; a block cut short says how.
fn detail_rows(got: Option<&Got>, off: bool, cols: usize) -> Vec<StyledRow> {
    let dim = Style {
        raised: true,
        ..style(Role::Detail)
    };
    let said = |text: &str| (vec![text.to_string()], false);
    let (lines, truncated) = match got {
        _ if off => said("not available"),
        None => said("loading"),
        Some(Got { body: None, .. }) => said("not available, retrying"),
        Some(Got {
            body: Some(body), ..
        }) => {
            let mut lines: Vec<String> = body.input.trim_end().lines().map(String::from).collect();
            if !lines.is_empty() && !body.output.trim().is_empty() {
                lines.push(String::new());
            }
            lines.extend(body.output.trim_end().lines().map(String::from));
            if lines.is_empty() {
                lines.push("(nothing recorded)".to_string());
            }
            (lines, body.truncated)
        }
    };
    let mut rows: Vec<Vec<Cell>> = lines
        .iter()
        .flat_map(|line| chunks(&cells(line, dim), cols.saturating_sub(2)))
        .collect();
    let more = rows.len().saturating_sub(DETAIL_ROWS);
    rows.truncate(DETAIL_ROWS);
    if more > 0 {
        rows.push(cells(&format!("\u{2026} {more} more rows"), dim));
    }
    if truncated {
        rows.push(cells("\u{2026} cut by the daemon", dim));
    }
    rows.into_iter()
        .map(|line| {
            let mut row = cells(" ", dim);
            row.extend(line);
            let pad = cols.saturating_sub(cell_width(&row));
            row.extend(cells(&" ".repeat(pad), dim));
            row_of(&row)
        })
        .collect()
}

/// The most rows of a recap shown before it is opened.
const RECAP_ROWS: usize = 6;

/// The agent's away summary: a dim label row, then the text wrapped to the pane in the same
/// dim. A long one is cut to `RECAP_ROWS` rows and says how many more, and its label row
/// opens and closes it, as a tool row does.
fn recap_lines(item: &Item, text: &str, opened: &Opened, cols: usize) -> Vec<Line> {
    let dim = style(Role::Detail);
    let mut rows: Vec<Vec<Cell>> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| wrap(&cells(line, dim), cols))
        .collect();
    let more = rows.len().saturating_sub(RECAP_ROWS);
    let key = format!("n:{:x}", item_key(item));
    let open = opened.keys.contains(&key);
    if more > 0 && !open {
        rows.truncate(RECAP_ROWS);
        rows.push(cells(&format!("\u{2026} {more} more rows"), dim));
    }
    let mut label = cells("Recap", dim);
    if more > 0 {
        label.extend(cells(chevron(open), dim));
    }
    let mut lines = vec![(row_of(&label), (more > 0).then_some(key))];
    lines.extend(rows.iter().map(|row| (row_of(row), None)));
    lines
}

/// A notice as one row: `<kind>: <text>`, or the text alone when it says more than its kind and
/// already names it (`2 background agents still running`, `Stop hook blocked: ...`).
fn notice_label(kind: &str, text: &str) -> String {
    let names_kind = text.to_lowercase().contains(&kind.to_lowercase());
    if names_kind && text.chars().count() > kind.chars().count() {
        text.to_string()
    } else {
        format!("{kind}: {text}")
    }
}

/// `context compacted`, and the line the summary opens with when the agent wrote one.
fn compaction_label(text: Option<&str>) -> String {
    match text.map(one_line).filter(|t| !t.is_empty()) {
        Some(text) => format!("context compacted: {text}"),
        None => "context compacted".to_string(),
    }
}

fn dim_row(text: &str, cols: usize) -> StyledRow {
    row_of(&cells(&cut(&one_line(text), cols), style(Role::Detail)))
}

fn thought_row(ms: Option<u64>, cols: usize) -> StyledRow {
    tool_line(
        &match ms {
            Some(ms) => format!("Thought for {}", took(ms)),
            None => "Thought".to_string(),
        },
        Status::Ok,
        None,
        None,
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

/// The most tools whose input and output are kept for one chat.
const DETAILS_KEPT: usize = 32;

/// What was read for one opened tool.
struct Got {
    /// What came, or `None` when the daemon would not send it (and `retry` says when to ask
    /// again).
    body: Option<Detail>,
    /// Reads that failed in a row, for the backoff, and when the next may start.
    fails: u32,
    retry: Option<Instant>,
    /// The call was still running when this was read, so a later revision is read again;
    /// one that had ended is never read twice.
    running: bool,
    revision: u64,
    /// When it was kept, so the oldest goes first.
    tick: u64,
}

/// The read in flight for an opened tool.
struct Out {
    id: String,
    since: Instant,
    running: bool,
    revision: u64,
}

/// What the person opened in one chat, and what was read for it: the keys of the groups and
/// tool rows that are open (`group_key`, `tool_key`), and the input and output kept for
/// the tools among them.
#[derive(Default)]
struct Opened {
    keys: BTreeSet<String>,
    /// The groups with a step that runs that the person closed.
    shut: BTreeSet<String>,
    details: BTreeMap<String, Got>,
    out: Option<Out>,
    ticks: u64,
    /// The daemon cannot send a tool's detail: nothing more is asked of it for this chat.
    off: bool,
}

impl Opened {
    /// Whether the tool `id` is to be read (see `ChatState::wants_detail`).
    fn due(&self, id: &str, revision: u64, now: Instant) -> bool {
        match self.details.get(id) {
            None => true,
            Some(got) if got.retry.is_some_and(|at| now < at) => false,
            Some(Got { body: None, .. }) => true,
            Some(got) => got.running && got.revision != revision,
        }
    }

    /// A read of `id` failed: kept as such, to be tried again after the backoff.
    fn failed(&mut self, id: &str, now: Instant) {
        self.ticks += 1;
        let tick = self.ticks;
        let got = self.details.entry(id.to_string()).or_insert(Got {
            body: None,
            fails: 0,
            retry: None,
            running: false,
            revision: 0,
            tick,
        });
        got.fails += 1;
        got.retry = Some(now + backoff(got.fails));
    }
}

/// What the key of a group's header ends in while a step of it runs.
const RUNNING_HEAD: &str = "!";

fn group_key(first: &Item) -> String {
    format!("g:{:x}", item_key(first))
}

fn tool_key(id: &str) -> String {
    format!("t:{id}")
}

/// A row, and the key of the group or tool it opens and closes when it is one.
type Line = (StyledRow, Option<String>);

/// The rows of one step: a tool row opened shows what it did below it.
fn step_lines(item: &Item, opened: &Opened, cols: usize) -> Vec<Line> {
    match item {
        Item::Thinking { ms, .. } => vec![(thought_row(*ms, cols), None)],
        Item::Tool {
            id,
            name,
            summary,
            status,
            exit_code,
            has_detail,
            ..
        } => {
            let key = tool_key(id);
            let open = has_detail.then(|| opened.keys.contains(&key));
            let row = tool_line(&label(name, summary), *status, *exit_code, open, cols);
            let mut lines = vec![(row, open.map(|_| key))];
            if open == Some(true) {
                let body = detail_rows(opened.details.get(id), opened.off, cols);
                lines.extend(body.into_iter().map(|row| (row, None)));
            }
            lines
        }
        Item::Subagent {
            summary, status, ..
        } => vec![(
            tool_line(&subagent_label(summary), *status, None, None, cols),
            None,
        )],
        _ => vec![(StyledRow::default(), None)],
    }
}

/// Every row of the chat at this width, oldest first.
pub fn layout(items: &[Item], cols: usize) -> Vec<StyledRow> {
    laid_out(items, cols, &Opened::default()).0
}

/// The rows, for each row the index of the item it was drawn from (a blank row between two
/// items belongs to the one before it), so a row can be traced back to its item, and the key
/// of the group or tool the row opens, if it is the one.
fn laid_out(
    items: &[Item],
    cols: usize,
    opened: &Opened,
) -> (Vec<StyledRow>, Vec<usize>, Vec<Option<String>>) {
    if cols == 0 {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let mut blocks: Vec<(Kind, Vec<Line>, usize)> = Vec::new();
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
                // Folded once every step is over, open while one runs until the person
                // closes it: that is a choice of its own (`shut`), so what they chose before
                // is what it shows after, and its header answers with a key of its own.
                let key = group_key(calls[0]);
                let running = calls
                    .iter()
                    .any(|it| status_of(it) == Some(Status::Running));
                let (open, head) = if running {
                    (!opened.shut.contains(&key), format!("{key}{RUNNING_HEAD}"))
                } else {
                    (opened.keys.contains(&key), key)
                };
                let head = Some(head);
                let mut lines = vec![(group_row(&calls, open, cols), head)];
                if open {
                    for step in &items[at..end] {
                        lines.extend(step_lines(step, opened, cols));
                    }
                }
                blocks.push((Kind::Step, lines, at));
            } else {
                for (n, item) in items[at..end].iter().enumerate() {
                    blocks.push((Kind::Step, step_lines(item, opened, cols), at + n));
                }
            }
            at = end;
            continue;
        }
        let index = at;
        at += 1;
        let (kind, rows) = match item {
            Item::User {
                text,
                attachments,
                queued,
                ..
            } => (Kind::Message, user_rows(text, attachments, *queued, cols)),
            Item::Agent { text, .. } => (Kind::Message, agent_rows(text, cols)),
            Item::Error { text, .. } => {
                (Kind::Step, vec![dim_row(&format!("error: {text}"), cols)])
            }
            Item::Notice { kind, text, .. } if kind == "summary" => {
                blocks.push((Kind::Message, recap_lines(item, text, opened, cols), index));
                continue;
            }
            Item::Notice { kind, text, .. } => {
                (Kind::Step, vec![dim_row(&notice_label(kind, text), cols)])
            }
            Item::Compaction { text, .. } => (
                Kind::Step,
                vec![dim_row(&compaction_label(text.as_deref()), cols)],
            ),
            // A kind of row a newer daemon sent: said, never dropped, so the page stays whole.
            Item::Unknown => (
                Kind::Step,
                vec![dim_row("item this build cannot show", cols)],
            ),
            _ => continue,
        };
        if !rows.is_empty() {
            blocks.push((kind, rows.into_iter().map(|r| (r, None)).collect(), index));
        }
    }
    let mut out: Vec<StyledRow> = Vec::new();
    let mut owner: Vec<usize> = Vec::new();
    let mut heads: Vec<Option<String>> = Vec::new();
    let mut before = None;
    for (kind, lines, index) in blocks {
        if before.is_some_and(|b| !(b == Kind::Step && kind == Kind::Step)) {
            out.push(StyledRow::default());
            owner.push(owner.last().copied().unwrap_or(index));
            heads.push(None);
        }
        owner.extend(std::iter::repeat_n(index, lines.len()));
        for (row, head) in lines {
            out.push(row);
            heads.push(head);
        }
        before = Some(kind);
    }
    // A pane too narrow for a row's own margins shows what fits of it, never a wider row.
    out.iter_mut().for_each(|row| clip(row, cols));
    (out, owner, heads)
}

/// What an item is, whatever its place in the page: hashed from what it says, not from its
/// `seq` (a position in the window) or its status (which a running call changes). The same
/// item in the page before a refresh and the one after has the same key.
fn item_key(item: &Item) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(item).hash(&mut h);
    match item {
        Item::User {
            ts,
            text,
            attachments,
            ..
        } => (
            ts,
            text,
            attachments.iter().map(|c| &c.name).collect::<Vec<_>>(),
        )
            .hash(&mut h),
        Item::Agent {
            ts, text, model, ..
        } => (ts, text, model).hash(&mut h),
        Item::Thinking { ms, text, .. } => (ms, text).hash(&mut h),
        Item::Tool { id, name, .. } => (id, name).hash(&mut h),
        Item::Subagent { summary, .. } => summary.hash(&mut h),
        Item::Error { text, .. } => text.hash(&mut h),
        Item::Notice { kind, text, .. } => (kind, text).hash(&mut h),
        Item::Compaction { .. } | Item::Unknown => {}
    }
    h.finish()
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
/// How long a change of what a tab is (not an agent, another program) must hold before the
/// chat it had is let go: a flap is not an exit.
const FLAP: Duration = Duration::from_secs(1);
/// Newest-page reads started per look, and the gap before the rest are tried.
const STAGGER: usize = 2;
const STAGGER_GAP: Duration = Duration::from_millis(100);

/// How long to wait before trying a read again after it failed `fails` times in a row: 3 s,
/// 6 s, 12 s, then every 30 s.
fn backoff(fails: u32) -> Duration {
    Duration::from_secs(match fails {
        0 | 1 => 3,
        2 => 6,
        3 => 12,
        _ => 30,
    })
}
/// A read with no answer for this long is taken as lost.
const LOST: Duration = Duration::from_secs(10);

/// A program name as the memory file holds it: one short line of printable characters.
fn program_line(line: &str) -> Option<String> {
    let line = line.trim();
    (!line.is_empty() && line.chars().count() <= 64 && line.chars().all(width::drawn))
        .then(|| line.to_string())
}

/// The scroll that puts the same item at the reader's row in a layout that was redone.
///
/// The reader's place is the item under the bottom row of what they see, and how far into it.
/// After the refresh that item is found again by its key (the nearest one to where it was, for
/// a key that repeats) and put back on the same row. If it is gone the nearest older item that
/// is still there stands in, at its last row.
fn anchored(
    old: &Shown,
    scroll: usize,
    keys: &[u64],
    owner: &[usize],
    total: usize,
) -> Option<usize> {
    let height = old.drawn.map_or(0, |(rows, _)| rows);
    let end = old
        .all
        .len()
        .checked_sub(scroll.min(max_scroll(&old.all, height)))?;
    let bottom = end.checked_sub(1)?;
    let item = *old.owner.get(bottom)?;
    let offset = bottom - old.owner.iter().position(|o| *o == item)?;
    let shift = keys.len() as isize - old.keys.len() as isize;
    for index in (0..=item).rev() {
        let want = old.keys.get(index)?;
        let found = keys
            .iter()
            .enumerate()
            .filter(|(_, key)| *key == want)
            .min_by_key(|(at, _)| (*at as isize - (index as isize + shift)).abs())
            .map(|(at, _)| at);
        let Some(found) = found else { continue };
        let Some(first) = owner.iter().position(|o| *o == found) else {
            continue;
        };
        let bottom = if index == item {
            let last = owner.iter().rposition(|o| *o == found)?;
            (first + offset).min(last)
        } else {
            owner.iter().rposition(|o| *o == found)?
        };
        return Some(total.saturating_sub(bottom + 1));
    }
    None
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
    /// What a tab has become, since when, while it is not yet known to be more than a flap.
    changing: BTreeMap<u32, Instant>,
    /// Things to say about a tab when the person is looking at it.
    notes: BTreeMap<u32, &'static str>,
    /// What to ask the daemon for, per flagged terminal.
    feeds: BTreeMap<u32, Feed>,
    /// The flagged terminals that are on screen.
    screen: BTreeSet<u32>,
    /// Terminals the daemon would not serve (not an agent, no such terminal, too large).
    refused: BTreeSet<u32>,
    /// What each chat has open, and what was read for it.
    opened: BTreeMap<u32, Opened>,
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
    /// Reads that failed in a row, for the backoff.
    fails: u32,
    /// An older page is out: what it was asked with.
    older: Option<Older>,
    /// No older page is asked for before this.
    older_wait: Option<Instant>,
}

/// The token of an older page in flight: its answer is taken only for the revision and the
/// items held that it was asked about.
struct Older {
    since: Instant,
    revision: u64,
    skip: usize,
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
    /// The item each row was drawn from, and each item's key (`item_key`).
    owner: Vec<usize>,
    keys: Vec<u64>,
    /// For each row, the key of the group or tool it opens and closes (`laid_out`).
    heads: Vec<Option<String>>,
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

    /// The newest page of `terminal`'s session replaces what is held (the daemon's answer,
    /// through `answered`; fixtures in tests). The same page again is no change.
    pub fn set_digest(&mut self, terminal: u32, digest: Digest) {
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
    /// them and older ones exist: the `skip` for the next request, and what it was asked
    /// about is kept to check the answer by. Answered once per request, until it is answered
    /// or taken as lost, and not while a failed one waits to be tried again.
    pub fn wants_older(&mut self, terminal: u32, now: Instant) -> Option<usize> {
        let (_, held) = self.digests.get(&terminal)?;
        let at_top = self
            .shown
            .get(&terminal)
            .is_some_and(|s| self.scrolled(terminal) >= s.max);
        if !self.on.contains(&terminal) || !held.has_older || !at_top {
            return None;
        }
        let (revision, skip) = (held.revision, held.items.len());
        let feed = self.feeds.entry(terminal).or_default();
        if feed.older.is_some() || feed.older_wait.is_some_and(|wait| now < wait) {
            return None;
        }
        feed.older_wait = None;
        feed.older = Some(Older {
            since: now,
            revision,
            skip,
        });
        Some(skip)
    }

    /// Older items, put in front of the ones held. Whether the page fits what was asked is
    /// `answered`'s to say; a page of another revision here replaces everything.
    pub fn set_older(&mut self, terminal: u32, older: Digest) {
        let Some((_, held)) = self.digests.get_mut(&terminal) else {
            return;
        };
        if older.revision != held.revision {
            self.drop_details(terminal);
            return self.set_digest(terminal, older);
        }
        let mut items = older.items;
        items.append(&mut held.items);
        held.items = items;
        held.has_older = older.has_older;
        held.first_seq = older.first_seq;
        held.warnings += older.warnings;
        self.stamp += 1;
        self.digests.get_mut(&terminal).expect("held above").0 = self.stamp;
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
    /// starts on. A terminal that is gone takes its state with it at once. One that stopped
    /// being an agent, or runs another program, loses its chat when it has been so for
    /// `FLAP`: until then its digest is kept, so a tab that blinks costs nothing and asks
    /// for nothing.
    pub fn adopt(&mut self, rows: &[AgentRow], now: Instant) {
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
                    || (*agent
                        && row.agent
                        && program.is_some()
                        && row.program.is_some()
                        && *program != row.program)
            });
            let mut letting_go = false;
            if changed {
                let since = *self.changing.entry(row.terminal).or_insert(now);
                if now.saturating_duration_since(since) < FLAP {
                    continue;
                }
                letting_go = true;
                self.forget(row.terminal);
            } else {
                self.changing.remove(&row.terminal);
            }
            let program = match (&row.program, was) {
                (None, Some((_, kept))) if !letting_go => kept,
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
        self.feeds.remove(&terminal);
        self.refused.remove(&terminal);
        self.changing.remove(&terminal);
        self.notes.remove(&terminal);
        self.opened.remove(&terminal);
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

    /// When the next newest-page read of `terminal` is due, if one is: a first read at once,
    /// a read that news, a retry or a backoff asked for, or the keepalive of one on screen.
    /// Nothing while a read is out.
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
            .or(feed.last_read.is_none().then_some(now))
    }

    /// When the loop must look again: the soonest due read, the moment a read out is taken as
    /// lost, the end of an older page's wait, or the end of a doubt about what a tab is.
    pub fn wake_at(&self, now: Instant) -> Option<Instant> {
        let lost = |since: Instant| since + LOST + Duration::from_millis(1);
        let feeds = self.on.iter().flat_map(|t| {
            let feed = self.feeds.get(t);
            [
                match feed.and_then(|f| f.out) {
                    Some(out) => Some(lost(out)),
                    None => self.due_at(*t, now),
                },
                feed.and_then(|f| f.older.as_ref()).map(|o| lost(o.since)),
                feed.and_then(|f| f.older_wait),
                self.opened
                    .get(t)
                    .and_then(|o| o.out.as_ref())
                    .map(|out| lost(out.since)),
                // A failed read of a row that is open is tried again when its backoff ends.
                self.opened
                    .get(t)
                    .filter(|o| o.out.is_none() && !o.off)
                    .and_then(|o| {
                        o.details
                            .iter()
                            .filter(|(id, _)| o.keys.contains(&tool_key(id)))
                            .filter_map(|(_, got)| got.retry)
                            .filter(|at| *at > now)
                            .min()
                    }),
            ]
        });
        feeds
            .flatten()
            .chain(self.changing.values().map(|since| *since + FLAP))
            .min()
    }

    /// The requests to send now. One newest-page read in flight per terminal, the latest news
    /// wins, and no more than `STAGGER` start in one go, so a window that opens with many
    /// agent tabs on does not ask for all of them at once. A page of older items when the
    /// reader is at the top of what is held. Nothing for a terminal whose flag is off.
    pub fn plan(&mut self, now: Instant) -> Vec<Request> {
        let mut out = Vec::new();
        let mut started = 0;
        for terminal in self.on.iter().copied().collect::<Vec<_>>() {
            let feed = self.feeds.entry(terminal).or_default();
            // A read that never came back is a failed one, and tried again on its own clock.
            if feed
                .out
                .is_some_and(|at| now.saturating_duration_since(at) > LOST)
            {
                feed.out = None;
                feed.fails += 1;
                feed.due = Some(now + backoff(feed.fails));
            }
            if feed
                .older
                .as_ref()
                .is_some_and(|o| now.saturating_duration_since(o.since) > LOST)
            {
                feed.older = None;
                feed.fails += 1;
                feed.older_wait = Some(now + backoff(feed.fails));
            }
            if self.due_at(terminal, now).is_some_and(|at| at <= now) {
                if started >= STAGGER {
                    self.feeds.entry(terminal).or_default().due = Some(now + STAGGER_GAP);
                    continue;
                }
                started += 1;
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
            if let Some(skip) = self.wants_older(terminal, now) {
                out.push(Request {
                    terminal,
                    skip: skip as u32,
                    limit: PAGE,
                    revision: self.digests.get(&terminal).map(|(_, d)| d.revision),
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
    /// chat, else the newest.
    ///
    /// A `reset` page is the newest of a session that changed under the read, whichever read
    /// it answers: it replaces everything, and an older page asked of what the session was is
    /// stale and will not be taken. Any other older page is taken only if it is of the
    /// revision it was asked with, still the one held, and was asked for the items still held.
    pub fn answered(
        &mut self,
        terminal: u32,
        older: bool,
        reset: bool,
        page: Digest,
        now: Instant,
    ) {
        if !self.on.contains(&terminal) {
            return;
        }
        let held = self
            .digests
            .get(&terminal)
            .map(|(_, d)| (d.revision, d.items.len()));
        let feed = self.feeds.entry(terminal).or_default();
        if older && !reset {
            // The answer to the page asked for is of the revision it was asked with; any
            // other is for a page asked of a session that has since changed, and is not
            // this one's to take.
            let Some(token) = feed.older.take_if(|o| o.revision == page.revision) else {
                return;
            };
            if held == Some((token.revision, token.skip)) {
                self.set_older(terminal, page);
            }
            return;
        }
        if reset {
            feed.older = None;
            // The session is another one: what was read of its tools is not of this.
            if let Some(opened) = self.opened.get_mut(&terminal) {
                opened.details.clear();
                opened.out = None;
            }
        }
        if !older {
            feed.out = None;
        }
        feed.last_read = Some(now);
        feed.fails = 0;
        if !older && std::mem::take(&mut feed.stale) {
            feed.due = Some(now + DEBOUNCE);
        }
        self.refused.remove(&terminal);
        self.set_digest(terminal, page);
    }

    /// The daemon refused a read of `terminal` with `reason`. Only `not_agent` and `too_large`
    /// are for good, and answer what to tell the person. Anything else (no session yet, no
    /// disk answer, a machine that is offline, words this build does not know) waits and
    /// tries again, slower each time, and never costs the chat held.
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
        match reason {
            "too_large" => {
                return Some(self.give_up(
                    terminal,
                    "this session is too large for chat, use the terminal",
                ));
            }
            "not_agent" => return Some(self.give_up(terminal, "chat is for agent tabs")),
            _ => {}
        }
        let feed = self.feeds.entry(terminal).or_default();
        feed.fails += 1;
        let at = now + backoff(feed.fails);
        if older {
            feed.older = None;
            feed.older_wait = Some(at);
        } else {
            feed.out = None;
            feed.due = Some(at);
        }
        None
    }

    /// A daemon too old to know `Messages` answers with no id. The reads it was holding were
    /// its refusal: those chats end, and the terminals are answered. Tabs only waiting for a
    /// retry had asked nothing of it.
    pub fn unsupported(&mut self) -> Vec<u32> {
        let asked: Vec<u32> = self
            .feeds
            .iter()
            .filter(|(_, feed)| feed.out.is_some() || feed.older.is_some())
            .map(|(t, _)| *t)
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
        self.feeds.remove(&terminal);
        self.opened.remove(&terminal);
        self.refused.insert(terminal);
        said
    }

    /// Whether the daemon refused to serve `terminal`: the key row stops offering the switch.
    pub fn is_refused(&self, terminal: u32) -> bool {
        self.refused.contains(&terminal)
    }

    /// Something to tell the person about `terminal`, kept until they are looking at it.
    pub fn note(&mut self, terminal: u32, said: &'static str) {
        self.notes.insert(terminal, said);
    }

    /// What is waiting to be said about `terminal`, once.
    pub fn take_note(&mut self, terminal: u32) -> Option<&'static str> {
        self.notes.remove(&terminal)
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
                let none = Opened::default();
                let opened = self.opened.get(&terminal).unwrap_or(&none);
                let (all, owner, heads) = laid_out(&digest.items, cols, opened);
                let keys: Vec<u64> = digest.items.iter().map(item_key).collect();
                // A chat being read, or selected from, keeps the same text where it was
                // whatever came or went around it; one at the bottom follows the newest.
                let scroll = self.scroll.entry(terminal).or_default();
                if *scroll > 0 || held {
                    let at = self
                        .shown
                        .get(&terminal)
                        .filter(|old| old.cols == cols)
                        .and_then(|old| anchored(old, *scroll, &keys, &owner, all.len()));
                    if let Some(at) = at {
                        *scroll = at;
                    }
                }
                self.shown.insert(
                    terminal,
                    Shown {
                        stamp: *stamp,
                        cols,
                        all,
                        owner,
                        keys,
                        heads,
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

    /// The rows of `terminal` as last drawn: where the window starts in `all`, and where it ends.
    fn window_of(&self, terminal: u32) -> Option<(&Shown, usize, usize)> {
        let shown = self.shown.get(&terminal)?;
        let (rows, scroll) = shown.drawn?;
        let end = shown.all.len() - scroll.min(max_scroll(&shown.all, rows));
        Some((shown, end.saturating_sub(rows), end))
    }

    /// Open the group or tool row drawn at `row` of `terminal`'s window, or close it. Answers
    /// whether there was one (a click on any other row is not this view's). The row stays
    /// where it is on screen.
    pub fn toggle_at(&mut self, terminal: u32, row: usize) -> bool {
        let key = self
            .window_of(terminal)
            .and_then(|(shown, start, end)| {
                (start + row < end).then(|| shown.heads[start + row].clone())
            })
            .flatten();
        key.is_some_and(|key| {
            self.flip(terminal, key, row);
            true
        })
    }

    /// The same for the newest group or tool row in the window, which is what `ctrl-a enter`
    /// means.
    pub fn toggle_newest(&mut self, terminal: u32) -> bool {
        let found = self.window_of(terminal).and_then(|(shown, start, end)| {
            (start..end)
                .rev()
                .find_map(|at| shown.heads[at].clone().map(|key| (key, at - start)))
        });
        found.is_some_and(|(key, row)| {
            self.flip(terminal, key, row);
            true
        })
    }

    fn flip(&mut self, terminal: u32, key: String, row: usize) {
        let opened = self.opened.entry(terminal).or_default();
        if let Some(group) = key.strip_suffix(RUNNING_HEAD) {
            if !opened.shut.remove(group) {
                opened.shut.insert(group.to_string());
            }
        } else if opened.keys.remove(&key) {
            // A tool put away before its answer came frees the read, and a failed one is
            // tried again the next time it is opened.
            if let Some(id) = key.strip_prefix("t:") {
                let _ = opened.out.take_if(|out| out.id == id);
                if opened.details.get(id).is_some_and(|got| got.body.is_none()) {
                    opened.details.remove(id);
                }
            }
        } else {
            opened.keys.insert(key.clone());
        }
        self.restamp(terminal);
        self.hold(terminal, &key, row);
    }

    /// The rows are laid out again with `key` open or closed, and the view scrolled so its
    /// row stays at `row` of the window; or at the top of it, when what it opened to is as
    /// tall as the pane, so the first rows of it show.
    fn hold(&mut self, terminal: u32, key: &str, row: usize) {
        let (Some((stamp, digest)), Some(shown), Some(opened)) = (
            self.digests.get(&terminal),
            self.shown.get_mut(&terminal),
            self.opened.get(&terminal),
        ) else {
            return;
        };
        let Some((rows, _)) = shown.drawn else {
            return;
        };
        let (all, owner, heads) = laid_out(&digest.items, shown.cols, opened);
        if let Some(head) = heads.iter().position(|h| h.as_deref() == Some(key)) {
            let extent = 1
                + (head + 1..all.len())
                    .take_while(|at| heads[*at].is_none() && owner[*at] == owner[head])
                    .count();
            let row = if extent >= rows { 0 } else { row };
            let end = head + rows - row;
            let max = max_scroll(&all, rows);
            self.scroll
                .insert(terminal, all.len().saturating_sub(end).min(max));
        }
        let scroll = self.scroll.get(&terminal).copied().unwrap_or(0);
        shown.stamp = *stamp;
        shown.max = max_scroll(&all, rows);
        shown.snapshot = snapshot(&window(&all, rows, scroll), shown.cols);
        shown.drawn = Some((rows, scroll.min(shown.max)));
        shown.all = all;
        shown.owner = owner;
        shown.heads = heads;
    }

    /// The rows are laid out again at the next draw.
    fn restamp(&mut self, terminal: u32) {
        self.stamp += 1;
        if let Some((stamp, _)) = self.digests.get_mut(&terminal) {
            *stamp = self.stamp;
        }
    }

    /// The tool of `terminal` whose input and output are to be read: open, with something to
    /// show, and either never read, read while it still ran at another revision, or failed
    /// and past its backoff (3, 6, 12, then 30 s). One read at a time per terminal; one that
    /// got no answer for `LOST` is a failed one. A new revision alone is no reason to read
    /// what has ended again.
    pub fn wants_detail(&mut self, terminal: u32, now: Instant) -> Option<String> {
        let mut lost = false;
        let found = self.next_detail(terminal, now, &mut lost);
        if lost {
            self.restamp(terminal);
        }
        found
    }

    /// `wants_detail`, and whether a read that never came back was given up on (the row says
    /// so at the next draw).
    fn next_detail(&mut self, terminal: u32, now: Instant, lost: &mut bool) -> Option<String> {
        let (_, held) = self.digests.get(&terminal)?;
        let revision = held.revision;
        let opened = self.opened.get_mut(&terminal)?;
        if opened.off {
            return None;
        }
        if let Some(out) = opened
            .out
            .take_if(|out| now.saturating_duration_since(out.since) > LOST)
        {
            opened.failed(&out.id, now);
            *lost = true;
        }
        if opened.out.is_some() {
            return None;
        }
        let (id, running) = held.items.iter().find_map(|item| match item {
            Item::Tool {
                id,
                has_detail: true,
                status,
                ..
            } if opened.keys.contains(&tool_key(id)) && opened.due(id, revision, now) => {
                Some((id.clone(), *status == Status::Running))
            }
            _ => None,
        })?;
        opened.out = Some(Out {
            id: id.clone(),
            since: now,
            running,
            revision,
        });
        Some(id)
    }

    /// What the daemon sent for the tool `tool_id` of `terminal`. One that was put away in
    /// the meantime is not kept.
    pub fn set_detail(
        &mut self,
        terminal: u32,
        tool_id: &str,
        input: String,
        output: String,
        truncated: bool,
    ) {
        let held = self.digests.get(&terminal).map(|(_, d)| d.revision);
        let Some(opened) = self
            .opened
            .get_mut(&terminal)
            .filter(|o| o.keys.contains(&tool_key(tool_id)))
        else {
            return;
        };
        let asked = opened.out.take_if(|out| out.id == tool_id);
        let Some(revision) = asked.as_ref().map(|out| out.revision).or(held) else {
            return;
        };
        opened.ticks += 1;
        let got = Got {
            body: Some(Detail {
                input,
                output,
                truncated,
            }),
            fails: 0,
            retry: None,
            running: asked.is_some_and(|out| out.running),
            revision,
            tick: opened.ticks,
        };
        opened.details.insert(tool_id.to_string(), got);
        while opened.details.len() > DETAILS_KEPT {
            let oldest = opened
                .details
                .iter()
                .min_by_key(|(_, got)| got.tick)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                opened.details.remove(&id);
            }
        }
        self.restamp(terminal);
    }

    /// The daemon would not send it now (busy, unavailable, too slow): shown as such, and
    /// asked again after the backoff. Not for good, and never cached as final.
    pub fn detail_failed(&mut self, terminal: u32, tool_id: &str, now: Instant) {
        let Some(opened) = self
            .opened
            .get_mut(&terminal)
            .filter(|o| o.keys.contains(&tool_key(tool_id)))
        else {
            return;
        };
        let _ = opened.out.take_if(|out| out.id == tool_id);
        opened.failed(tool_id, now);
        self.restamp(terminal);
    }

    /// A daemon too old to know `MessageDetail` answered with no id: the chats that were
    /// waiting on one stop asking, and are answered once. The rows they have open say it.
    pub fn details_unsupported(&mut self) -> Vec<u32> {
        let asked: Vec<u32> = self
            .opened
            .iter()
            .filter(|(_, opened)| opened.out.is_some())
            .map(|(terminal, _)| *terminal)
            .collect();
        for terminal in &asked {
            if let Some(opened) = self.opened.get_mut(terminal) {
                opened.out = None;
                opened.off = true;
            }
            self.restamp(*terminal);
        }
        asked
    }

    /// What was read for `terminal`'s tools goes: the session is another one.
    fn drop_details(&mut self, terminal: u32) {
        if let Some(opened) = self.opened.get_mut(&terminal) {
            opened.details.clear();
            opened.out = None;
        }
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
            queued: false,
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
            ts: None,
            output_bytes: 0,
            has_detail: false,
            attachments: Vec::new(),
        }
    }

    /// What was read, as a read that came now at revision 1.
    fn got(body: Option<Detail>) -> Got {
        Got {
            fails: u32::from(body.is_none()),
            body,
            retry: None,
            running: false,
            revision: 1,
            tick: 0,
        }
    }

    /// A tool with an id, a time it took, and input and output to open.
    fn timed(id: &str, name: &str, summary: &str, ms: Option<u64>, detail: bool) -> Item {
        Item::Tool {
            seq: 0,
            id: id.into(),
            name: name.into(),
            summary: summary.into(),
            status: Status::Ok,
            exit_code: None,
            duration_ms: ms,
            ts: None,
            output_bytes: 0,
            has_detail: detail,
            attachments: Vec::new(),
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
                    path: None,
                },
                Chip {
                    name: "b".into(),
                    bytes: None,
                    path: None,
                },
            ],
            queued: false,
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
        assert_eq!(got, ["\u{25cf} Thought for 3 s"]);
        assert_eq!(
            lines(&[thinking(Some(10))], 40),
            ["\u{25cf} Thought for 1 s"]
        );
        assert_eq!(
            lines(&[thinking(Some(65_000))], 40),
            ["\u{25cf} Thought for 1 min 5 s"]
        );
        assert_eq!(lines(&[thinking(None)], 40), ["\u{25cf} Thought"]);
        let with_text = Item::Thinking {
            seq: 0,
            ms: Some(2000),
            text: Some("secret plan".into()),
        };
        assert_eq!(lines(&[with_text], 40), ["\u{25cf} Thought for 2 s"]);
        // The same dim ink as a tool row, label and all.
        let rows = layout(
            &[thinking(None), tool("Bash", "make", Status::Ok, None)],
            40,
        );
        for row in rows {
            assert!(row.runs.iter().all(|r| r.style.role == Role::Detail));
        }
    }

    #[test]
    fn a_call_that_went_well_has_no_status_word_and_the_rest_say_theirs_in_words() {
        let cases = [
            (Status::Ok, "\u{25cf} Ran cargo test", Role::Detail),
            (Status::Unknown, "\u{25cf} Ran cargo test", Role::Detail),
            (
                Status::Failed,
                "\u{25cf} Ran cargo test  failed",
                Role::Failed,
            ),
            (
                Status::Running,
                "\u{25cf} Ran cargo test  running",
                Role::Working,
            ),
            (
                Status::Interrupted,
                "\u{25cf} Ran cargo test  interrupted",
                Role::Detail,
            ),
            (
                Status::Rejected,
                "\u{25cf} Ran cargo test  rejected",
                Role::Detail,
            ),
        ];
        for (status, want, role) in cases {
            let rows = layout(&[tool("Bash", "cargo test", status, None)], 60);
            assert_eq!(rows[0].text, want);
            assert_eq!(style_at(&rows[0], 0).role, role, "{want}");
            // The label is dim whatever the state.
            assert_eq!(style_at(&rows[0], 4).role, Role::Detail, "{want}");
        }
    }

    #[test]
    fn what_a_call_did_is_a_sentence_per_kind_of_tool() {
        let said = |name: &str, summary: &str| label(name, summary);
        for shell in ["Bash", "bash", "shell", "exec_command", "run_shell_command"] {
            assert_eq!(
                said(shell, "cargo  test\n-q"),
                "Ran cargo test -q",
                "{shell}"
            );
        }
        assert_eq!(said("Read", "src/main.rs"), "Read src/main.rs");
        assert_eq!(said("read_file", "a.rs"), "Read a.rs");
        for edit in ["Edit", "Write", "MultiEdit", "apply_patch", "replace"] {
            assert_eq!(said(edit, "a.rs"), "Edited a.rs", "{edit}");
        }
        for search in ["Grep", "Glob", "search_file_content"] {
            assert_eq!(
                said(search, "todo in src"),
                "Searched todo in src",
                "{search}"
            );
        }
        assert_eq!(said("WebSearch", "rust release"), "Searched the web");
        assert_eq!(said("google_web_search", "q"), "Searched the web");
        assert_eq!(
            said("WebFetch", "https://doc.rust-lang.org/std/?x=1"),
            "Fetched doc.rust-lang.org"
        );
        assert_eq!(said("web_fetch", ""), "Fetched a page");
        assert_eq!(
            said("Task", "review the diff"),
            "Ran a subagent: review the diff"
        );
        assert_eq!(said("Task", ""), "Ran a subagent");
        // Nothing to say it with: the name and the summary.
        assert_eq!(said("TodoWrite", "3 items"), "TodoWrite 3 items");
        assert_eq!(said("Skill", "ship"), "Skill ship");
        assert_eq!(said("mcp__crw__search", "q"), "mcp__crw__search q");
        assert_eq!(said("Mystery", ""), "Mystery");
        // A tool that only mentions a word is not that tool.
        assert_eq!(said("KillShell", "12345"), "KillShell 12345");
        assert_eq!(said("BashOutput", "12345"), "BashOutput 12345");
        assert_eq!(said("NotebookRead", "n"), "NotebookRead n");
        assert_eq!(said("WebSearchPlus", "q"), "WebSearchPlus q");
        assert_eq!(said("subagent", "x"), "Ran a subagent: x");
        assert_eq!(said("web_search", "q"), "Searched the web");
        // A url is its host and nothing else: no login, no path.
        for (url, want) in [
            (
                "https://user:pw@example.com:8080/a?b=c#d",
                "example.com:8080",
            ),
            ("http://a@b@example.com/x", "example.com"),
            ("example.com/path", "example.com"),
            ("https://example.com?q=1", "example.com"),
        ] {
            assert_eq!(said("WebFetch", url), format!("Fetched {want}"), "{url}");
        }
        assert_eq!(said("Read", ""), "Read");
    }

    #[test]
    fn a_row_ends_with_a_chevron_when_it_opens_and_with_none_when_it_does_not() {
        let plain = timed("a", "Read", "a.rs", None, false);
        let opens = timed("b", "Read", "b.rs", None, true);
        assert_eq!(lines(&[plain], 40), ["\u{25cf} Read a.rs"]);
        assert_eq!(lines(&[opens], 40), ["\u{25cf} Read b.rs \u{25b8}"]);
    }

    #[test]
    fn an_exit_code_shows_when_it_is_not_zero() {
        let failed = tool("Bash", "make", Status::Failed, Some(2));
        let fine = tool("Bash", "make", Status::Ok, Some(0));
        let odd = tool("Bash", "make", Status::Ok, Some(3));
        assert_eq!(lines(&[failed], 60), ["\u{25cf} Ran make  failed, exit 2"]);
        assert_eq!(lines(&[fine], 60), ["\u{25cf} Ran make"]);
        assert_eq!(lines(&[odd], 60), ["\u{25cf} Ran make  exit 3"]);
    }

    #[test]
    fn a_long_command_is_cut_and_the_words_and_the_chevron_stay() {
        let item = Item::Tool {
            seq: 0,
            id: "t".into(),
            name: "Bash".into(),
            summary: "cargo test --workspace --all-targets".into(),
            status: Status::Failed,
            exit_code: Some(101),
            duration_ms: None,
            ts: None,
            output_bytes: 9,
            has_detail: true,
            attachments: Vec::new(),
        };
        let got = lines(&[item], 36);
        assert_eq!(
            got,
            ["\u{25cf} Ran cargo tes\u{2026}  failed, exit 101 \u{25b8}"]
        );
        assert_eq!(width::width(&got[0]), 36);
    }

    #[test]
    fn a_summary_is_one_line() {
        let item = tool("Bash", "echo a\n\techo   b", Status::Ok, None);
        assert_eq!(lines(&[item], 60), ["\u{25cf} Ran echo a echo b"]);
    }

    #[test]
    fn consecutive_tool_calls_fold_under_one_header_with_the_time_they_took() {
        let items = [
            timed("a", "Read", "a.rs", Some(1200), false),
            thinking(Some(1000)),
            timed("b", "Edit", "a.rs", Some(2200), false),
            timed("c", "Bash", "make", Some(1000), false),
        ];
        assert_eq!(lines(&items, 40), ["Worked for 4 s \u{25b8}"]);
        let slow = [
            timed("a", "Read", "a", Some(61_000), false),
            timed("b", "Read", "b", Some(4_000), false),
        ];
        assert_eq!(lines(&slow, 40), ["Worked for 1 min 5 s \u{25b8}"]);
        let rows = layout(&items, 40);
        assert!(rows[0].runs.iter().all(|r| r.style.role == Role::Detail));
    }

    #[test]
    fn a_fold_reads_the_wall_clock_when_the_calls_say_when_they_started() {
        let at = |mut item: Item, start: i64| {
            if let Item::Tool { ts, .. } = &mut item {
                *ts = Some(start);
            }
            item
        };
        // Three calls of 4 s that overlap: 5 s on the clock, not 12 s of work.
        let items = [
            at(timed("a", "Read", "a", Some(4000), false), 1_000_000),
            at(timed("b", "Read", "b", Some(4000), false), 1_001_000),
            at(timed("c", "Read", "c", Some(4000), false), 1_001_000),
        ];
        assert_eq!(lines(&items, 40), ["Worked for 5 s \u{25b8}"]);
        // A gap between them counts too: it is time the agent was at it.
        let gap = [
            at(timed("a", "Read", "a", Some(1000), false), 1_000_000),
            at(timed("b", "Read", "b", Some(1000), false), 1_009_000),
        ];
        assert_eq!(lines(&gap, 40), ["Worked for 10 s \u{25b8}"]);
        // One that does not say when: the sum of what each took.
        let some = [
            at(timed("a", "Read", "a", Some(4000), false), 1_000_000),
            timed("b", "Read", "b", Some(4000), false),
        ];
        assert_eq!(lines(&some, 40), ["Worked for 8 s \u{25b8}"]);
    }

    #[test]
    fn a_fold_with_a_step_of_no_known_time_counts_steps_instead() {
        let items = [
            timed("a", "Read", "a", Some(1000), false),
            timed("b", "Read", "b", None, false),
            Item::Subagent {
                seq: 0,
                summary: "s".into(),
                steps: 2,
                status: Status::Ok,
            },
        ];
        assert_eq!(lines(&items, 40), ["3 steps \u{25b8}"]);
    }

    #[test]
    fn a_fold_says_what_failed_or_is_still_running_and_opens_while_one_runs() {
        let failed = [
            tool("Read", "a", Status::Ok, None),
            tool("Bash", "b", Status::Failed, Some(1)),
        ];
        assert_eq!(lines(&failed, 40), ["2 steps, 1 failed \u{25b8}"]);
        let rows = layout(&failed, 40);
        assert_eq!(style_at(&rows[0], 0).role, Role::Failed);
        let stopped = [
            tool("Read", "a", Status::Interrupted, None),
            tool("Bash", "c", Status::Rejected, None),
        ];
        assert_eq!(
            lines(&stopped, 40),
            ["2 steps, 1 interrupted, 1 rejected \u{25b8}"]
        );
        // One still running: the group is open, its steps below it.
        let running = [
            tool("Read", "a", Status::Ok, None),
            tool("Bash", "b", Status::Running, None),
        ];
        assert_eq!(
            lines(&running, 40),
            [
                "2 steps, 1 running \u{25be}",
                "\u{25cf} Read a",
                "\u{25cf} Ran b  running"
            ]
        );
        let rows = layout(&running, 40);
        assert_eq!(style_at(&rows[0], 0).role, Role::Working);
    }

    fn subagent(summary: &str, status: Status) -> Item {
        Item::Subagent {
            seq: 0,
            summary: summary.into(),
            steps: 1,
            status,
        }
    }

    #[test]
    fn a_subagent_row_says_running_or_failed_like_a_tool_row_does() {
        for (status, tail) in [
            (Status::Running, "  running"),
            (Status::Failed, "  failed"),
            (Status::Interrupted, "  interrupted"),
            (Status::Ok, ""),
        ] {
            let items = [subagent("spawned: survey", status)];
            let rows = layout(&items, 60);
            assert_eq!(
                rows[0].text.trim_end(),
                format!("\u{25cf} Ran a subagent: spawned: survey{tail}")
            );
        }
        let rows = layout(&[subagent("s", Status::Running)], 40);
        assert_eq!(style_at(&rows[0], 0).role, Role::Working);
    }

    #[test]
    fn a_fold_stays_open_and_counts_a_subagent_that_still_runs() {
        let items = [
            tool("Read", "a", Status::Ok, None),
            subagent("spawned: survey", Status::Running),
        ];
        assert_eq!(
            lines(&items, 50),
            [
                "2 steps, 1 running \u{25be}",
                "\u{25cf} Read a",
                "\u{25cf} Ran a subagent: spawned: survey  running"
            ]
        );
    }

    #[test]
    fn input_waiting_in_the_queue_says_queued_under_its_text() {
        let waiting = Item::User {
            seq: 0,
            ts: None,
            text: "do this next".into(),
            attachments: Vec::new(),
            queued: true,
        };
        let got: Vec<String> = lines(&[waiting], 20)
            .iter()
            .map(|l| l.trim_end().to_string())
            .collect();
        assert_eq!(got, [" > do this next", "   queued"]);
        let sent = lines(&[user("do this next")], 20);
        assert_eq!(sent.len(), 1, "no word for input that was taken");
    }

    #[test]
    fn a_notice_that_names_its_kind_in_its_text_is_not_said_twice() {
        let notice = |kind: &str, text: &str| Item::Notice {
            seq: 0,
            kind: kind.into(),
            text: text.into(),
        };
        let items = [
            notice("background", "2 background agents still running"),
            notice("hook", "Stop hook blocked: write a card"),
            notice("command output", "Set model to opus"),
        ];
        assert_eq!(
            lines(&items, 60)
                .iter()
                .map(|l| l.trim_end())
                .collect::<Vec<_>>(),
            [
                "2 background agents still running",
                "Stop hook blocked: write a card",
                "command output: Set model to opus"
            ]
        );
    }

    #[test]
    fn the_queued_word_wraps_like_the_text_above_it_in_a_narrow_pane() {
        let waiting = Item::User {
            seq: 0,
            ts: None,
            text: "go".into(),
            attachments: Vec::new(),
            queued: true,
        };
        for cols in 5..=10 {
            let rows = layout(std::slice::from_ref(&waiting), cols);
            assert!(rows.iter().all(|row| width::width(&row.text) <= cols));
            // Nothing is cut off: the word goes on to the next row, as the text does.
            let all: String = rows
                .iter()
                .map(|r| r.text.replace([' ', '>'], ""))
                .collect();
            assert_eq!(all, "goqueued", "{cols} columns");
        }
    }

    #[test]
    fn a_compaction_row_prints_the_line_its_summary_opens_with() {
        let rows = |text: Option<&str>| {
            lines(
                &[Item::Compaction {
                    seq: 0,
                    text: text.map(String::from),
                }],
                60,
            )
        };
        assert_eq!(rows(None), ["context compacted"]);
        assert_eq!(rows(Some("  ")), ["context compacted"]);
        assert_eq!(
            rows(Some("Goal: ship\nthe chat")),
            ["context compacted: Goal: ship the chat"]
        );
    }

    #[test]
    fn one_tool_call_and_its_reasoning_stay_as_rows_of_their_own() {
        let items = [thinking(Some(2000)), tool("Read", "a.rs", Status::Ok, None)];
        assert_eq!(
            lines(&items, 40),
            ["\u{25cf} Thought for 2 s", "\u{25cf} Read a.rs"]
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
                "2 steps \u{25b8}",
                "",
                "done reading",
                "",
                "2 steps \u{25b8}"
            ]
        );
    }

    #[test]
    fn a_group_that_is_opened_shows_its_steps_and_reasoning_and_closes_again() {
        let items = [
            timed("a", "Read", "a.rs", Some(1000), true),
            thinking(Some(2000)),
            timed("b", "Bash", "make", Some(1000), false),
            agent("done"),
        ];
        let mut opened = Opened::default();
        let key = group_key(&items[0]);
        opened.keys.insert(key.clone());
        let (rows, owner, heads) = laid_out(&items, 40, &opened);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            text,
            [
                "Worked for 2 s \u{25be}",
                "\u{25cf} Read a.rs \u{25b8}",
                "\u{25cf} Thought for 2 s",
                "\u{25cf} Ran make",
                "",
                "done"
            ]
        );
        // Only the header and the row that opens answer a click, and every row is traced to
        // the item its group starts at.
        assert_eq!(heads[0], Some(key));
        assert_eq!(heads[1], Some(tool_key("a")));
        assert!(heads[2..].iter().all(Option::is_none));
        assert_eq!(owner, [0, 0, 0, 0, 0, 3]);
        let closed = laid_out(&items, 40, &Opened::default()).0;
        assert_eq!(closed[0].text, "Worked for 2 s \u{25b8}");
        assert_eq!(closed.len(), 3);
    }

    #[test]
    fn a_tool_that_is_opened_shows_its_input_a_blank_row_and_its_output_in_a_dim_block() {
        let items = [timed("a", "Bash", "make", Some(1000), true)];
        let mut opened = Opened::default();
        opened.keys.insert(tool_key("a"));
        let at = |opened: &Opened| laid_out(&items, 12, opened).0;
        // Not read yet.
        let rows = at(&opened);
        assert_eq!(rows[0].text, "\u{25cf} Ran make \u{25be}");
        assert_eq!(rows[1].text, " loading    ");
        opened.details.insert(
            "a".into(),
            got(Some(Detail {
                input: "make all".into(),
                output: "built\nok\n\n".into(),
                truncated: false,
            })),
        );
        let rows = at(&opened);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            text,
            [
                "\u{25cf} Ran make \u{25be}",
                " make all   ",
                "            ",
                " built      ",
                " ok         "
            ]
        );
        // The block is dim and raised, as wide as the pane; the row above it is neither.
        for row in &rows[1..] {
            assert!(
                row.runs
                    .iter()
                    .all(|r| r.style.raised && r.style.role == Role::Detail)
            );
            assert_eq!(width::width(&row.text), 12);
        }
        assert!(rows[0].runs.iter().all(|r| !r.style.raised));
        // Cut by the daemon, or by this view's own limit.
        opened.details.insert(
            "a".into(),
            got(Some(Detail {
                input: String::new(),
                output: "x".into(),
                truncated: true,
            })),
        );
        assert_eq!(
            laid_out(&items, 40, &opened).0[2].text.trim_end(),
            " \u{2026} cut by the daemon"
        );
        let long = (0..40).map(|n| format!("l{n}\n")).collect::<String>();
        opened.details.insert(
            "a".into(),
            got(Some(Detail {
                input: String::new(),
                output: long,
                truncated: false,
            })),
        );
        let rows = laid_out(&items, 20, &opened).0;
        assert_eq!(rows.len(), 1 + DETAIL_ROWS + 1);
        assert_eq!(
            rows.last().expect("a row").text.trim_end(),
            " \u{2026} 10 more rows"
        );
        // Cut by the limit and by the daemon: both are said.
        let both = (0..40).map(|n| format!("l{n}\n")).collect::<String>();
        opened.details.insert(
            "a".into(),
            got(Some(Detail {
                input: String::new(),
                output: both,
                truncated: true,
            })),
        );
        let rows = laid_out(&items, 30, &opened).0;
        let tail: Vec<&str> = rows[rows.len() - 2..]
            .iter()
            .map(|r| r.text.trim_end())
            .collect();
        assert_eq!(
            tail,
            [" \u{2026} 10 more rows", " \u{2026} cut by the daemon"]
        );
        // A read that failed is waiting to be tried again; nothing recorded is said.
        opened.details.insert("a".into(), got(None));
        assert_eq!(
            laid_out(&items, 30, &opened).0[1].text.trim_end(),
            " not available, retrying"
        );
        opened.details.insert(
            "a".into(),
            got(Some(Detail {
                input: "  \n".into(),
                output: String::new(),
                truncated: false,
            })),
        );
        assert_eq!(
            laid_out(&items, 30, &opened).0[1].text.trim_end(),
            " (nothing recorded)"
        );
        // A daemon that cannot send it: said, whatever was kept.
        opened.off = true;
        assert_eq!(
            laid_out(&items, 30, &opened).0[1].text.trim_end(),
            " not available"
        );
    }

    #[test]
    fn a_tool_with_nothing_to_show_has_no_row_to_open() {
        let items = [timed("a", "Bash", "make", None, false)];
        let mut opened = Opened::default();
        opened.keys.insert(tool_key("a"));
        let (rows, _, heads) = laid_out(&items, 40, &opened);
        assert_eq!(rows.len(), 1);
        assert_eq!(heads, [None]);
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
            Item::Compaction { seq: 0, text: None },
            Item::Subagent {
                seq: 0,
                summary: "review".into(),
                steps: 7,
                status: Status::Ok,
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
                "\u{25cf} Ran a subagent: review"
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
            Item::Compaction { seq: 0, text: None },
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
                "\u{25cf} Thought",
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
                status: Status::Ok,
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
    fn opened_rows_are_no_wider_than_the_pane_at_any_width_either() {
        let items = [
            timed(
                "a",
                "Bash世界",
                "cargo 🌍 test --workspace",
                Some(1500),
                true,
            ),
            timed("b", "Read", "a/世界/file.rs", Some(500), true),
            timed("c", "WebFetch", "https://世界.example/path", None, true),
            Item::Subagent {
                seq: 0,
                summary: "review 世界 \u{2764}\u{fe0f}".into(),
                steps: 1,
                status: Status::Ok,
            },
        ];
        let mut opened = Opened::default();
        for key in [group_key(&items[0]), tool_key("a"), tool_key("c")] {
            opened.keys.insert(key);
        }
        let body = Detail {
            input: "世界 🌍 a long input line \u{2764}\u{fe0f}\twith a tab".into(),
            output: "out 世界世界世界世界\n\nlast averyveryverylongwordthatneverbreaks".into(),
            truncated: true,
        };
        opened.details.insert("a".into(), got(Some(body)));
        opened.details.insert("c".into(), got(None));
        for cols in 0..=40usize {
            for row in laid_out(&items, cols, &opened).0 {
                let drawn = drawn_columns(&row.text);
                assert!(drawn <= cols, "{cols}: {drawn} columns in {:?}", row.text);
                assert!(!row.text.contains('\u{fe0f}'), "{cols}: {:?}", row.text);
            }
        }
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
                "\u{25cf} R\u{2026}  failed, exit 2",
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
        next.adopt(&[row(10, "claude"), row(11, "codex")], Instant::now());
        assert!(next.wants_digest(10));
        assert!(!next.wants_digest(11));

        // A person's own switch is not undone by the next list of rows.
        next.toggle(10, Some("claude"));
        next.adopt(&[row(10, "claude"), row(11, "codex")], Instant::now());
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
        chat.adopt(&[shell, row(6, "claude")], Instant::now());
        assert!(!chat.wants_digest(5));
        assert!(chat.wants_digest(6));
        chat.set_digest(6, digest(vec![agent("hi")]));
        chat.adopt(&[], Instant::now());
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
        chat.adopt(&[row(5, "claude"), row(6, "claude")], Instant::now());
        chat.set_digest(5, digest(numbered(3)));
        chat.set_digest(6, digest(numbered(3)));
        chat.scroll_by(5, true, 1);
        assert!(chat.has_view(5) && chat.has_view(6));

        // Claude exits to a shell: no agent, no chat, no digest, and the shell never adopts.
        let mut shell = row(5, "zsh");
        shell.agent = false;
        // Claude replaced by codex: another session, and codex is not remembered. Held for a
        // second before it counts, so a blink is not an exit.
        let t0 = Instant::now();
        chat.adopt(&[shell.clone(), row(6, "codex")], t0);
        assert!(chat.has_view(5) && chat.has_view(6), "not yet");
        chat.adopt(&[shell, row(6, "codex")], t0 + FLAP);
        for terminal in [5, 6] {
            assert!(!chat.wants_digest(terminal), "{terminal}");
            assert!(!chat.has_view(terminal), "{terminal}");
            assert_eq!(chat.scrolled(terminal), 0, "{terminal}");
        }
        chat.render(&[want(5, 10, 4)]);
        assert!(chat.shown(5).is_none());

        // Claude starts again in the tab: the remembered choice applies afresh, with no
        // digest of the session that ended.
        chat.adopt(&[row(5, "claude"), row(6, "codex")], Instant::now());
        assert!(chat.wants_digest(5) && !chat.has_view(5));
        assert!(!chat.wants_digest(6));
    }

    #[test]
    fn a_program_that_cannot_be_read_for_a_moment_is_not_another_program() {
        let mut chat = ChatState::default();
        chat.toggle(5, None);
        chat.adopt(&[row(5, "claude")], Instant::now());
        chat.toggle(5, None);
        chat.toggle(5, None);
        chat.set_digest(5, digest(numbered(2)));
        let mut unread = row(5, "claude");
        unread.program = None;
        chat.adopt(&[unread], Instant::now());
        chat.adopt(&[row(5, "claude")], Instant::now());
        assert!(chat.has_view(5), "kept through the gap");
    }

    #[test]
    fn a_reused_terminal_id_starts_clean() {
        let mut chat = ChatState::default();
        chat.adopt(&[row(7, "claude")], Instant::now());
        chat.toggle(7, Some("nobody"));
        chat.set_digest(7, digest(numbered(30)));
        chat.render(&[want(7, 10, 4)]);
        chat.scroll_by(7, true, 5);
        chat.typed(7, b"x", Instant::now());
        assert!(chat.typing(7));

        chat.adopt(&[], Instant::now());
        chat.adopt(&[row(7, "claude")], Instant::now());
        assert!(!chat.wants_digest(7) && !chat.has_view(7) && !chat.typing(7));
        assert_eq!(chat.scrolled(7), 0);
        assert_eq!(chat.wants_older(7, Instant::now()), None);
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
        assert_eq!(
            chat.wants_older(3, Instant::now()),
            None,
            "at the bottom, not the top"
        );
        chat.scroll_by(3, true, 1000);
        chat.render(&[want(3, 10, 5)]);
        assert_eq!(
            chat.wants_older(3, Instant::now()),
            Some(10),
            "the items held, for `skip`"
        );
        assert_eq!(chat.wants_older(3, Instant::now()), None, "asked once");
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
        assert_eq!(chat.wants_older(3, Instant::now()), None);
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
        assert_eq!(chat.wants_older(3, Instant::now()), Some(10));
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
        assert_eq!(chat.wants_older(3, Instant::now()), None);
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
        // A status this build does not know is drawn as no status at all.
        assert_eq!(row.text, "\u{25cf} Ran make");
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

    /// A chat of one agent line and two tools, drawn at 30 by 8 and on.
    fn with_tools(revision: u64) -> ChatState {
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        let items = vec![
            agent("go"),
            timed("a", "Bash", "make", Some(1000), true),
            timed("b", "Read", "a.rs", Some(2000), true),
        ];
        chat.set_digest(4, page(revision, items));
        chat.render(&[want(4, 30, 8)]);
        chat
    }

    /// `with_tools` with the group open and the second tool opened, nothing read yet.
    fn with_tool_open(revision: u64) -> ChatState {
        let mut chat = with_tools(revision);
        chat.toggle_at(4, 2);
        chat.render(&[want(4, 30, 8)]);
        chat.toggle_newest(4);
        chat.render(&[want(4, 30, 8)]);
        chat
    }

    fn draw(chat: &mut ChatState) -> Vec<String> {
        chat.render(&[want(4, 30, 8)]);
        visible(chat, 4)
            .iter()
            .map(|l| l.trim_end().into())
            .collect()
    }

    #[test]
    fn a_click_or_ctrl_a_enter_opens_the_group_or_row_in_view_and_again_closes_it() {
        let mut chat = with_tools(1);
        assert_eq!(visible(&chat, 4)[2].trim_end(), "Worked for 3 s \u{25b8}");
        // Only the header answers: the text above it, the blank rows below it do not.
        assert!(!chat.toggle_at(4, 0));
        assert!(!chat.toggle_at(4, 1));
        assert!(!chat.toggle_at(4, 7));
        assert!(chat.toggle_at(4, 2));
        let got = draw(&mut chat);
        assert_eq!(got[2], "Worked for 3 s \u{25be}");
        assert_eq!(got[3], "\u{25cf} Ran make \u{25b8}");
        // The newest row in view: the second tool, then (still) the second tool.
        assert!(chat.toggle_newest(4));
        assert_eq!(draw(&mut chat)[4], "\u{25cf} Read a.rs \u{25be}");
        assert!(chat.toggle_newest(4));
        assert_eq!(draw(&mut chat)[4], "\u{25cf} Read a.rs \u{25b8}");
        // The click on the header closes the group, and its steps with it.
        assert!(chat.toggle_at(4, 2));
        assert_eq!(draw(&mut chat)[3], "");
        assert!(chat.has_view(4));
    }

    #[test]
    fn a_group_open_because_a_step_runs_can_be_closed_and_stays_as_the_person_left_it() {
        let items = |status| {
            vec![
                agent("go"),
                tool("Read", "a.rs", Status::Ok, None),
                subagent("spawned: survey", status),
            ]
        };
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        chat.set_digest(4, page(1, items(Status::Running)));
        let got = draw(&mut chat);
        assert_eq!(got[2], "2 steps, 1 running \u{25be}");
        assert_eq!(got[3], "\u{25cf} Read a.rs");
        // The header answers a click, though a step runs: the group closes ...
        assert!(chat.toggle_at(4, 2));
        let got = draw(&mut chat);
        assert_eq!(got[2], "2 steps, 1 running \u{25b8}");
        assert_eq!(got[3], "");
        // ... and opens again.
        assert!(chat.toggle_at(4, 2));
        assert_eq!(draw(&mut chat)[3], "\u{25cf} Read a.rs");
        // Closed once more, it stays closed when the step ends.
        assert!(chat.toggle_at(4, 2));
        chat.set_digest(4, page(2, items(Status::Ok)));
        let got = draw(&mut chat);
        assert_eq!(got[2], "2 steps \u{25b8}");
        assert_eq!(got[3], "");
    }

    #[test]
    fn with_no_group_or_tool_row_in_view_nothing_is_taken() {
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        chat.set_digest(4, page(1, numbered(3)));
        chat.render(&[want(4, 30, 8)]);
        assert!(!chat.toggle_newest(4));
        assert!(!chat.toggle_at(4, 0));
        // Not drawn at all: nothing either.
        assert!(!ChatState::default().toggle_newest(4));
    }

    #[test]
    fn a_row_that_opens_keeps_its_place_on_the_screen() {
        // A long chat ending in one tool: the row is the last of the window, at scroll 0.
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        let mut items = numbered(12);
        items.push(timed("a", "Bash", "make", Some(1000), true));
        chat.set_digest(4, page(1, items));
        assert_eq!(draw(&mut chat)[7], "\u{25cf} Ran make \u{25b8}");
        assert!(chat.toggle_newest(4));
        let got = draw(&mut chat);
        assert_eq!(got[7], "\u{25cf} Ran make \u{25be}", "{got:?}");
        assert!(
            chat.scrolled(4) > 0,
            "what it opened to is below the window"
        );
        // Closed again, it is still where it was.
        assert!(chat.toggle_at(4, 7));
        assert_eq!(draw(&mut chat)[7], "\u{25cf} Ran make \u{25b8}");
        assert_eq!(chat.scrolled(4), 0);
        // A click on a row in the middle of the window, scrolled back.
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        let mut items = numbered(6);
        items.push(timed("a", "Bash", "make", Some(1000), true));
        items.extend(numbered(12));
        chat.set_digest(4, page(1, items));
        chat.render(&[want(4, 30, 8)]);
        chat.scroll_by(4, true, 20);
        let before = draw(&mut chat);
        let row = before
            .iter()
            .position(|l| l.contains("Ran make"))
            .expect("the row is in view");
        assert!(chat.toggle_at(4, row));
        let after = draw(&mut chat);
        assert_eq!(after[row], "\u{25cf} Ran make \u{25be}");
        assert_eq!(after[..row], before[..row], "the rows above did not move");
    }

    #[test]
    fn a_row_that_opens_taller_than_the_pane_goes_to_the_top_and_shows_its_first_rows() {
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        let mut items = numbered(3);
        items.push(timed("a", "Bash", "make", Some(1000), true));
        chat.set_digest(4, page(1, items));
        let before = draw(&mut chat);
        assert_eq!(before[6], "\u{25cf} Ran make \u{25b8}");
        chat.toggle_newest(4);
        chat.set_detail(
            4,
            "a",
            "make".into(),
            (0..20).map(|n| format!("l{n}\n")).collect(),
            false,
        );
        // Put away and opened again, the answer is kept: the row opens to 22 rows in 8.
        chat.toggle_at(4, 6);
        draw(&mut chat);
        assert!(chat.toggle_at(4, 6));
        let got = draw(&mut chat);
        assert_eq!(got[0], "\u{25cf} Ran make \u{25be}");
        assert_eq!(got[1], " make");
        assert_eq!(got[3], " l0");
        assert_eq!(got[7], " l4");
    }

    #[test]
    fn an_opened_row_asks_for_its_detail_once_and_an_ended_call_is_never_read_again() {
        let now = Instant::now();
        let mut chat = with_tools(7);
        assert_eq!(chat.wants_detail(4, now), None, "nothing is open");
        let mut chat = with_tool_open(7);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("b"));
        assert_eq!(chat.wants_detail(4, now), None, "one read at a time");
        assert_eq!(draw(&mut chat)[5], " loading");
        chat.set_detail(4, "b", "cat a.rs".into(), "fn main() {}".into(), false);
        let got = draw(&mut chat);
        assert_eq!(got[5], " cat a.rs");
        assert_eq!(got[7], " fn main() {}");
        // Held: closed and opened again, and through any number of new revisions.
        assert_eq!(chat.wants_detail(4, now), None);
        chat.toggle_newest(4);
        chat.toggle_newest(4);
        assert_eq!(chat.wants_detail(4, now), None);
        for revision in 8..12 {
            let items = chat.digests[&4].1.items.clone();
            chat.set_digest(4, page(revision, items));
            assert_eq!(chat.wants_detail(4, now), None, "revision {revision}");
        }
        assert_eq!(draw(&mut chat)[5], " cat a.rs");
    }

    #[test]
    fn a_call_that_was_running_when_read_is_read_again_when_the_session_moves() {
        let now = Instant::now();
        let running = |status| Item::Tool {
            seq: 0,
            id: "r".into(),
            name: "Bash".into(),
            summary: "sleep".into(),
            status,
            exit_code: None,
            duration_ms: None,
            ts: None,
            output_bytes: 1,
            has_detail: true,
            attachments: Vec::new(),
        };
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        chat.set_digest(4, page(1, vec![running(Status::Running)]));
        chat.render(&[want(4, 30, 8)]);
        chat.toggle_newest(4);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("r"));
        chat.set_detail(4, "r", "sleep 9".into(), "partial".into(), false);
        assert_eq!(chat.wants_detail(4, now), None, "same revision");
        chat.set_digest(4, page(2, vec![running(Status::Running)]));
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("r"));
        chat.set_detail(4, "r", "sleep 9".into(), "all".into(), false);
        chat.set_digest(4, page(3, vec![running(Status::Ok)]));
        assert_eq!(
            chat.wants_detail(4, now).as_deref(),
            Some("r"),
            "it ended since"
        );
        chat.set_detail(4, "r", "sleep 9".into(), "done".into(), false);
        chat.set_digest(4, page(4, vec![running(Status::Ok)]));
        assert_eq!(chat.wants_detail(4, now), None, "read once it had ended");
    }

    #[test]
    fn a_failed_read_says_so_and_is_tried_again_after_three_six_twelve_then_thirty_seconds() {
        let now = Instant::now();
        let mut chat = with_tool_open(7);
        // The newest page was read: nothing else is waiting on the loop.
        let items = chat.digests[&4].1.items.clone();
        chat.answered(4, false, false, page(7, items), now);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("b"));
        assert_eq!(draw(&mut chat)[5], " loading");
        let mut at = now;
        for wait in [3, 6, 12, 30, 30] {
            chat.detail_failed(4, "b", at);
            assert_eq!(draw(&mut chat)[5], " not available, retrying");
            assert_eq!(chat.wake_at(at), Some(at + Duration::from_secs(wait)));
            let early = at + Duration::from_secs(wait) - Duration::from_millis(1);
            assert_eq!(chat.wants_detail(4, early), None, "{wait} s");
            at += Duration::from_secs(wait);
            assert_eq!(chat.wants_detail(4, at).as_deref(), Some("b"), "{wait} s");
        }
        // The answer, when it comes, is the row.
        chat.set_detail(4, "b", "cat".into(), "ok".into(), false);
        assert_eq!(draw(&mut chat)[5], " cat");
        // Opened again after it was put away failed: at once, not after the wait.
        let mut chat = with_tool_open(7);
        chat.wants_detail(4, now);
        chat.detail_failed(4, "b", now);
        chat.toggle_newest(4);
        chat.toggle_newest(4);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("b"));
        // No answer for too long is a failure too.
        let mut lost = with_tool_open(7);
        assert!(lost.wants_detail(4, now).is_some());
        let later = now + LOST + Duration::from_secs(1);
        assert_eq!(lost.wants_detail(4, later), None);
        assert_eq!(draw(&mut lost)[5], " not available, retrying");
        let again = later + Duration::from_secs(3);
        assert_eq!(lost.wants_detail(4, again).as_deref(), Some("b"));
    }

    #[test]
    fn a_row_put_away_before_its_answer_frees_the_read_and_caches_nothing() {
        let now = Instant::now();
        let mut chat = with_tool_open(7);
        // Open the first tool too, then ask: the newest is the one read.
        chat.toggle_at(4, 3);
        draw(&mut chat);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("a"));
        // The first is put away while its answer is out: the other is asked at once, with
        // no wait for the lost read.
        chat.toggle_at(4, 3);
        draw(&mut chat);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("b"));
        // The late answer and the late failure of the one put away keep nothing.
        chat.set_detail(4, "a", "x".into(), "y".into(), false);
        chat.detail_failed(4, "a", now);
        assert!(!chat.opened[&4].details.contains_key("a"));
        // Opened again, it is asked for like a row never opened.
        chat.set_detail(4, "b", "cat".into(), "ok".into(), false);
        chat.toggle_at(4, 3);
        draw(&mut chat);
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("a"));
    }

    #[test]
    fn a_group_held_open_by_a_running_tool_closes_when_the_person_closes_it() {
        let items = vec![
            timed("a", "Read", "a.rs", Some(1000), true),
            Item::Tool {
                seq: 0,
                id: "b".into(),
                name: "Bash".into(),
                summary: "make".into(),
                status: Status::Running,
                exit_code: None,
                duration_ms: None,
                ts: None,
                output_bytes: 0,
                has_detail: false,
                attachments: Vec::new(),
            },
        ];
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        chat.set_digest(4, page(1, items.clone()));
        let got = draw(&mut chat);
        assert_eq!(got[0], "2 steps, 1 running \u{25be}");
        assert!(chat.toggle_at(4, 0), "its header answers");
        assert_eq!(draw(&mut chat)[0], "2 steps, 1 running \u{25b8}");
        assert!(chat.opened.get(&4).is_none_or(|o| o.keys.is_empty()));
        // The step that ran ended: the group shows what the person last chose, closed.
        let mut ended = items;
        ended[1] = timed("b", "Bash", "make", Some(500), false);
        chat.set_digest(4, page(2, ended));
        assert_eq!(draw(&mut chat)[0], "Worked for 2 s \u{25b8}");
    }

    #[test]
    fn what_was_read_is_kept_for_thirty_two_tools_and_goes_with_the_session() {
        let now = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        let items: Vec<Item> = (0..40)
            .flat_map(|n| {
                [
                    timed(&format!("t{n}"), "Read", "x", Some(1), true),
                    agent("m"),
                ]
            })
            .collect();
        chat.set_digest(4, page(1, items));
        chat.render(&[want(4, 30, 8)]);
        let opened = chat.opened.entry(4).or_default();
        for n in 0..40 {
            opened.keys.insert(tool_key(&format!("t{n}")));
        }
        for n in 0..40 {
            chat.set_detail(4, &format!("t{n}"), "i".into(), "o".into(), false);
        }
        let kept = &chat.opened[&4].details;
        assert_eq!(kept.len(), DETAILS_KEPT);
        assert!(
            !kept.contains_key("t0") && kept.contains_key("t39"),
            "oldest first"
        );
        // A session that was reset: the rows are asked for again.
        let items = chat.digests[&4].1.items.clone();
        chat.answered(4, false, true, page(9, items), now);
        assert!(chat.opened[&4].details.is_empty());
        assert!(chat.wants_detail(4, now).is_some());
    }

    #[test]
    fn a_daemon_too_old_for_detail_turns_it_off_for_the_chat_that_asked_once() {
        let now = Instant::now();
        let mut chat = with_tool_open(7);
        let mut quiet = ChatState::default();
        on(&mut quiet, 5);
        assert_eq!(quiet.details_unsupported(), Vec::<u32>::new());
        assert_eq!(chat.wants_detail(4, now).as_deref(), Some("b"));
        assert_eq!(chat.details_unsupported(), [4]);
        assert_eq!(draw(&mut chat)[5], " not available");
        assert_eq!(chat.wants_detail(4, now + LOST * 9), None);
        assert_eq!(chat.details_unsupported(), Vec::<u32>::new(), "told once");
        assert!(chat.has_view(4), "the chat itself goes on");
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
        // Out: nothing again, however long, until it is taken as lost, and then it is a
        // failed read and waits its backoff.
        assert!(chat.plan(now + Duration::from_secs(5)).is_empty());
        assert!(chat.plan(now + Duration::from_secs(11)).is_empty());
        assert_eq!(chat.plan(now + Duration::from_secs(14)).len(), 1);
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
            chat.answered(3, false, false, page(7, numbered(held)), now);
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
        chat.answered(3, false, false, page(1, numbered(2)), t0);
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
        chat.answered(3, false, false, page(2, numbered(3)), answered);
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
            chat.answered(t, false, false, page(1, numbered(2)), t0);
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
    fn a_read_that_fails_waits_three_six_twelve_then_thirty_seconds_and_keeps_the_flag() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        let mut now = t0;
        for wait in [3, 6, 12, 30, 30] {
            assert_eq!(chat.refused(3, false, "no_session", now), None);
            assert!(chat.wants_digest(3) && !chat.is_refused(3));
            let early = now + Duration::from_secs(wait) - Duration::from_millis(1);
            assert!(chat.plan(early).is_empty(), "{wait}: not before");
            assert_eq!(chat.wake_at(now), Some(now + Duration::from_secs(wait)));
            now += Duration::from_secs(wait);
            assert_eq!(chat.plan(now).len(), 1, "{wait}: then");
        }
        // An answer ends the run: the next failure starts from three seconds again.
        chat.answered(3, false, false, page(1, numbered(2)), now);
        chat.changed(3, now);
        chat.plan(now + DEBOUNCE);
        chat.refused(3, false, "no_session", now + DEBOUNCE);
        assert_eq!(
            chat.wake_at(now),
            Some(now + DEBOUNCE + Duration::from_secs(3))
        );
    }

    #[test]
    fn a_refusal_that_is_not_for_good_never_ends_the_chat_or_costs_what_is_held() {
        let t0 = Instant::now();
        for reason in [
            "unavailable",
            "no_session",
            "went offline",
            "offline",
            "something this build never heard of",
            "",
        ] {
            let mut chat = ChatState::default();
            on(&mut chat, 3);
            chat.plan(t0);
            chat.answered(
                3,
                false,
                false,
                Digest {
                    has_older: true,
                    ..page(5, numbered(10))
                },
                t0,
            );
            chat.render(&[want(3, 10, 5)]);
            chat.scroll_by(3, true, 1000);
            // A refused older page: the digest stays, and it is asked again after a wait.
            assert_eq!(chat.wants_older(3, t0), Some(10));
            assert_eq!(chat.refused(3, true, reason, t0), None, "{reason:?}");
            assert!(chat.has_view(3) && !chat.is_refused(3), "{reason:?}");
            assert_eq!(chat.digests[&3].1.items.len(), 10);
            assert_eq!(chat.wants_older(3, t0 + Duration::from_secs(2)), None);
            assert_eq!(chat.wants_older(3, t0 + Duration::from_secs(3)), Some(10));
            // A refused refresh: the flag stays, the chat stays, the read comes again.
            chat.changed(3, t0);
            chat.plan(t0 + DEBOUNCE);
            assert_eq!(chat.refused(3, false, reason, t0), None, "{reason:?}");
            assert!(chat.wants_digest(3) && chat.has_view(3), "{reason:?}");
            assert!(chat.wake_at(t0).is_some());
        }
    }

    #[test]
    fn only_not_agent_and_too_large_end_the_chat_and_say_why() {
        let t0 = Instant::now();
        for (reason, said) in [
            ("not_agent", "chat is for agent tabs"),
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
        chat.answered(4, false, false, page(1, numbered(2)), t0);
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
        chat.answered(3, true, false, page(5, older), t0);
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
        chat.answered(3, false, false, page(1, numbered(2)), t0);
        assert!(!chat.has_view(3) && chat.digests.is_empty());
    }

    fn older_page(first: u32, count: u32, revision: u64, has_older: bool) -> Digest {
        Digest {
            revision,
            first_seq: first,
            has_older,
            items: (0..count)
                .map(|n| agent(&format!("old{first}-{n}")))
                .collect(),
            warnings: 0,
        }
    }

    /// A chat of `held` items at the top of its scroll, an older page out for it.
    fn at_the_top(held: u32, now: Instant) -> ChatState {
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(now);
        let newest = Digest {
            has_older: true,
            ..page(5, numbered(held))
        };
        chat.answered(3, false, false, newest, now);
        chat.render(&[want(3, 10, 5)]);
        chat.scroll_by(3, true, 10_000);
        chat
    }

    #[test]
    fn many_tabs_starting_at_once_are_read_two_at_a_time() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        for terminal in 1..=5 {
            on(&mut chat, terminal);
        }
        let ids = |asks: Vec<Request>| asks.iter().map(|a| a.terminal).collect::<Vec<_>>();
        assert_eq!(ids(chat.plan(t0)), [1, 2]);
        assert_eq!(chat.wake_at(t0), Some(t0 + STAGGER_GAP), "the rest soon");
        assert!(chat.plan(t0 + Duration::from_millis(50)).is_empty());
        assert_eq!(ids(chat.plan(t0 + STAGGER_GAP)), [3, 4]);
        assert_eq!(ids(chat.plan(t0 + STAGGER_GAP * 2)), [5]);
        assert!(chat.plan(t0 + STAGGER_GAP * 3).is_empty());
    }

    #[test]
    fn a_read_lost_on_a_tab_nobody_looks_at_is_tried_again_on_its_own() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.plan(t0);
        chat.answered(3, false, false, page(1, numbered(2)), t0);
        chat.changed(3, t0);
        chat.plan(t0 + DEBOUNCE);
        // Never on screen, never answered: the loop is woken for it, and asks again.
        let lost = t0 + DEBOUNCE + LOST + Duration::from_millis(1);
        assert_eq!(chat.wake_at(t0), Some(lost));
        assert!(
            chat.plan(lost).is_empty(),
            "a failed read waits its backoff"
        );
        assert_eq!(chat.wake_at(lost), Some(lost + backoff(1)));
        assert_eq!(chat.plan(lost + backoff(1)).len(), 1);
    }

    #[test]
    fn a_session_that_changed_under_the_read_replaces_everything_and_cancels_the_older_page() {
        let t0 = Instant::now();
        let mut chat = at_the_top(10, t0);
        assert_eq!(chat.wants_older(3, t0), Some(10));
        assert_eq!(chat.wants_older(3, t0), None, "one in flight");
        // A refresh finds the file changed: the newest page of what it is now.
        chat.answered(
            3,
            false,
            true,
            Digest {
                has_older: true,
                ..page(6, numbered(12))
            },
            t0,
        );
        assert_eq!(chat.digests[&3].1.revision, 6);
        assert_eq!(chat.digests[&3].1.items.len(), 12);
        // A new older page can be asked at once, for what is held now.
        chat.render(&[want(3, 10, 5)]);
        chat.scroll_by(3, true, 10_000);
        assert_eq!(chat.wants_older(3, t0), Some(12));
        // The older page asked of revision 5 comes back late: it is not taken, nothing
        // doubles, and the new request is still the one waited for.
        chat.answered(3, true, false, older_page(0, 10, 5, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 12);
        assert_eq!(chat.digests[&3].1.revision, 6);
        assert_eq!(chat.wants_older(3, t0), None);
        chat.answered(3, true, false, older_page(0, 4, 6, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 16);
    }

    #[test]
    fn an_older_page_is_taken_only_for_the_revision_and_the_items_it_was_asked_about() {
        let t0 = Instant::now();
        // The right one.
        let mut chat = at_the_top(10, t0);
        chat.wants_older(3, t0);
        chat.answered(3, true, false, older_page(0, 5, 5, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 15);
        assert!(!chat.digests[&3].1.has_older);

        // Another revision than it was asked with: dropped.
        let mut chat = at_the_top(10, t0);
        chat.wants_older(3, t0);
        chat.answered(3, true, false, older_page(0, 5, 9, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 10);

        // The items held changed since it was asked (a refresh of another size): dropped.
        let mut chat = at_the_top(10, t0);
        chat.wants_older(3, t0);
        chat.answered(
            3,
            false,
            false,
            Digest {
                has_older: true,
                ..page(5, numbered(12))
            },
            t0,
        );
        chat.answered(3, true, false, older_page(0, 5, 5, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 12, "no duplicated messages");

        // One that was never asked for: dropped.
        let mut chat = at_the_top(10, t0);
        chat.answered(3, true, false, older_page(0, 5, 5, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 10);
    }

    #[test]
    fn a_refresh_during_an_older_page_leaves_both_reads_standing() {
        let t0 = Instant::now();
        let mut chat = at_the_top(10, t0);
        assert_eq!(chat.wants_older(3, t0), Some(10));
        // News, a newest read goes out while the older one is out: two at once.
        chat.changed(3, t0);
        let asks = chat.plan(t0 + DEBOUNCE);
        assert_eq!(asks.len(), 1);
        assert!(!asks[0].older);
        assert_eq!(chat.wants_older(3, t0 + DEBOUNCE), None, "still one older");
        // The refresh finds the same revision and the same items: nothing changes, the older
        // page that follows is taken.
        chat.answered(
            3,
            false,
            false,
            Digest {
                has_older: true,
                ..page(5, numbered(10))
            },
            t0,
        );
        chat.answered(3, true, false, older_page(0, 5, 5, false), t0);
        assert_eq!(chat.digests[&3].1.items.len(), 15);
        // The same page again while an older one is out does not let a second one go.
        let mut chat = at_the_top(10, t0);
        chat.wants_older(3, t0);
        chat.set_digest(
            3,
            Digest {
                has_older: true,
                ..page(5, numbered(10))
            },
        );
        assert_eq!(chat.wants_older(3, t0), None);
    }

    #[test]
    fn a_daemon_too_old_for_messages_leaves_the_tabs_that_only_wait() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        on(&mut chat, 4);
        chat.plan(t0);
        // Terminal 3's read failed and waits its retry; terminal 4's is out.
        chat.refused(3, false, "unavailable", t0);
        assert_eq!(chat.unsupported(), [4]);
        assert!(chat.wants_digest(3) && !chat.wants_digest(4));
    }

    #[test]
    fn a_tab_that_blinks_keeps_its_chat_and_asks_for_nothing() {
        let t0 = Instant::now();
        let mut chat = ChatState::default();
        chat.programs.insert("claude".into());
        chat.adopt(&[row(5, "claude")], t0);
        chat.plan(t0);
        chat.answered(5, false, false, page(1, numbered(3)), t0);
        let mut shell = row(5, "zsh");
        shell.agent = false;
        chat.adopt(&[shell], t0 + Duration::from_millis(100));
        assert_eq!(
            chat.wake_at(t0),
            Some(t0 + Duration::from_millis(100) + FLAP),
            "looked at again when the doubt is over"
        );
        chat.adopt(&[row(5, "claude")], t0 + Duration::from_millis(600));
        assert!(chat.has_view(5) && chat.wants_digest(5));
        assert!(chat.plan(t0 + Duration::from_millis(700)).is_empty());
        assert_eq!(chat.wake_at(t0), None, "the doubt is cleared");
        // Past a second, it is an exit.
        let mut shell = row(5, "zsh");
        shell.agent = false;
        chat.adopt(&[shell.clone()], t0 + Duration::from_secs(2));
        chat.adopt(&[shell], t0 + Duration::from_secs(3));
        assert!(!chat.wants_digest(5) && !chat.has_view(5));
    }

    #[test]
    fn notes_are_kept_for_the_tab_they_concern_and_said_once() {
        let mut chat = ChatState::default();
        chat.note(3, "for three");
        assert_eq!(chat.take_note(4), None);
        assert_eq!(chat.take_note(3), Some("for three"));
        assert_eq!(chat.take_note(3), None);
    }

    /// The text the reader has in view, newest last.
    fn read(chat: &ChatState) -> Vec<String> {
        visible(chat, 3)
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn texts(from: u32, to: u32) -> Vec<Item> {
        (from..to).map(|n| agent(&format!("m{n}"))).collect()
    }

    #[test]
    fn a_full_page_with_one_new_item_does_not_move_what_is_read() {
        let t0 = Instant::now();
        // The steady state: sixty items, the oldest falls off as the newest arrives.
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.set_digest(3, digest(texts(0, 60)));
        chat.render(&[want(3, 20, 8)]);
        chat.scroll_by(3, true, 25);
        chat.render(&[want(3, 20, 8)]);
        let before = read(&chat);
        assert!(before.len() > 3);
        for step in 1..=5 {
            chat.set_digest(3, digest(texts(step, 60 + step)));
            chat.render(&[want(3, 20, 8)]);
            assert_eq!(read(&chat), before, "step {step}");
        }
        // The same at the cap of two hundred, and with several new items at once.
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.set_digest(3, digest(texts(0, 200)));
        chat.render(&[want(3, 20, 8)]);
        chat.scroll_by(3, true, 100);
        chat.render(&[want(3, 20, 8)]);
        let before = read(&chat);
        chat.set_digest(3, digest(texts(4, 204)));
        chat.render(&[want(3, 20, 8)]);
        assert_eq!(read(&chat), before);
        let _ = t0;
    }

    #[test]
    fn the_reader_keeps_the_row_inside_a_long_item_and_follows_a_held_selection() {
        let long = |n: u32| {
            agent(&format!(
                "{}\n{}\n{}",
                "a".repeat(5) + &n.to_string(),
                "b",
                "c"
            ))
        };
        let items = |from: u32, to: u32| (from..to).map(long).collect::<Vec<_>>();
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.set_digest(3, digest(items(0, 30)));
        chat.render(&[want(3, 20, 6)]);
        chat.scroll_by(3, true, 20);
        chat.render(&[want(3, 20, 6)]);
        let before = visible(&chat, 3);
        chat.set_digest(3, digest(items(3, 34)));
        chat.render(&[want(3, 20, 6)]);
        assert_eq!(visible(&chat, 3), before);

        // Held at the bottom, the text under a selection stays through the same refresh.
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.set_digest(3, digest(items(0, 30)));
        let held = Want {
            held: true,
            ..want(3, 20, 6)
        };
        chat.render(&[held]);
        let before = visible(&chat, 3);
        chat.set_digest(3, digest(items(2, 33)));
        chat.render(&[Want {
            held: true,
            ..want(3, 20, 6)
        }]);
        assert_eq!(visible(&chat, 3), before);
    }

    #[test]
    fn an_item_that_is_gone_leaves_the_reader_on_the_nearest_older_one() {
        let mut chat = ChatState::default();
        on(&mut chat, 3);
        chat.set_digest(3, digest(texts(0, 30)));
        chat.render(&[want(3, 20, 4)]);
        chat.scroll_by(3, true, 20);
        chat.render(&[want(3, 20, 4)]);
        let bottom = read(&chat).last().cloned().unwrap();
        // The item at the reader's row (and the one before it) are gone from the page.
        let n: u32 = bottom[1..].parse().unwrap();
        let mut items = texts(0, 30);
        items.retain(|it| !matches!(it, Item::Agent { text, .. } if *text == format!("m{n}") || *text == format!("m{}", n - 1)));
        chat.set_digest(3, digest(items));
        chat.render(&[want(3, 20, 4)]);
        let now = read(&chat);
        assert_eq!(now.last().unwrap(), &format!("m{}", n - 2), "{now:?}");
    }

    fn recap(text: &str) -> Item {
        Item::Notice {
            seq: 0,
            kind: "summary".into(),
            text: text.into(),
        }
    }

    fn recap_key(item: &Item) -> String {
        format!("n:{:x}", item_key(item))
    }

    fn trimmed(rows: &[StyledRow]) -> Vec<&str> {
        rows.iter().map(|r| r.text.trim_end()).collect()
    }

    #[test]
    fn a_recap_is_a_dim_label_row_and_its_text_wrapped_to_the_pane_between_blank_rows() {
        let items = [agent("a"), recap("one two three four five six"), agent("b")];
        let rows = layout(&items, 12);
        assert_eq!(
            trimmed(&rows),
            [
                "a",
                "",
                "Recap",
                "one two",
                "three four",
                "five six",
                "",
                "b"
            ]
        );
        for row in &rows[2..6] {
            assert!(row.runs.iter().all(|r| r.style == style(Role::Detail)));
        }
        // A recap that fits has nothing to open.
        let (_, _, heads) = laid_out(&items, 12, &Opened::default());
        assert!(heads.iter().all(Option::is_none));
    }

    #[test]
    fn a_long_recap_is_cut_to_six_rows_says_how_many_more_and_opens_like_a_tool_row() {
        let text = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let item = recap(&text);
        let rows = layout(std::slice::from_ref(&item), 20);
        assert_eq!(
            trimmed(&rows),
            [
                "Recap \u{25b8}",
                "line 1",
                "line 2",
                "line 3",
                "line 4",
                "line 5",
                "line 6",
                "\u{2026} 4 more rows"
            ]
        );
        let mut opened = Opened::default();
        opened.keys.insert(recap_key(&item));
        let (rows, _, heads) = laid_out(std::slice::from_ref(&item), 20, &opened);
        assert_eq!(rows.len(), 11);
        assert_eq!(rows[0].text.trim_end(), "Recap \u{25be}");
        assert_eq!(rows[10].text, "line 10");
        assert!(heads[0].is_some() && heads[1..].iter().all(Option::is_none));
    }

    #[test]
    fn a_click_or_ctrl_a_enter_opens_a_recap_and_again_closes_it() {
        let text = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut chat = ChatState::default();
        on(&mut chat, 4);
        chat.set_digest(4, page(1, vec![agent("go"), recap(&text)]));
        chat.render(&[want(4, 30, 14)]);
        let got = |chat: &ChatState| -> Vec<String> {
            visible(chat, 4)
                .iter()
                .map(|l| l.trim_end().into())
                .collect()
        };
        assert_eq!(got(&chat)[2], "Recap \u{25b8}");
        assert_eq!(got(&chat)[9], "\u{2026} 4 more rows");
        // Only the label answers: the text, the blank row above it do not.
        assert!(!chat.toggle_at(4, 1));
        assert!(!chat.toggle_at(4, 3));
        assert!(chat.toggle_at(4, 2));
        chat.render(&[want(4, 30, 14)]);
        assert_eq!(got(&chat)[2], "Recap \u{25be}");
        assert_eq!(got(&chat)[12], "line 10");
        assert!(chat.toggle_newest(4));
        chat.render(&[want(4, 30, 14)]);
        assert_eq!(got(&chat)[2], "Recap \u{25b8}");
        assert_eq!(got(&chat)[9], "\u{2026} 4 more rows");
    }

    #[test]
    fn the_other_kinds_of_notice_stay_one_dim_row_and_take_no_blank_row() {
        let long = "word ".repeat(30);
        let items = [
            Item::Notice {
                seq: 0,
                kind: "hook".into(),
                text: long,
            },
            Item::Notice {
                seq: 0,
                kind: "interrupted".into(),
                text: "Interrupted".into(),
            },
        ];
        let rows = layout(&items, 24);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "hook: word word word wo\u{2026}");
        assert_eq!(rows[1].text, "interrupted: Interrupted");
    }

    #[test]
    fn a_recap_is_no_wider_than_the_pane_at_any_width_closed_or_opened() {
        let long =
            "recap 世界 \u{2764}\u{fe0f} 🌍 averyveryverylongwordthatneverbreaks and more words. "
                .repeat(12);
        let item = recap(&format!("{long}\n\n世界世界世界世界世界世界\tafter a tab"));
        let mut opened = Opened::default();
        for open in [false, true] {
            if open {
                opened.keys.insert(recap_key(&item));
            }
            for cols in 0..=40usize {
                let (rows, _, _) = laid_out(std::slice::from_ref(&item), cols, &opened);
                assert_eq!(rows.is_empty(), cols == 0);
                if cols >= 5 {
                    assert!(
                        rows[0].text.starts_with("Recap"),
                        "{cols}: {:?}",
                        rows[0].text
                    );
                }
                for row in rows {
                    let drawn = drawn_columns(&row.text);
                    assert!(drawn <= cols, "{cols}: {drawn} columns in {:?}", row.text);
                    assert!(!row.text.contains('\u{fe0f}'), "{cols}: {:?}", row.text);
                }
            }
        }
    }
}

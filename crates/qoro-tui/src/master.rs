//! The right column: the one pinned master and the terminals that need a person.
//!
//! One pinned supervisor terminal ("master") plus the session controls that belong to it,
//! read from the same index the left column reads. The master is an ordinary agent
//! terminal in this same session, so no second connection and no new subscription class:
//! rows, spaces and snapshots already exist.
//!
//! Chrome, not a tile: a fixed-width text column painted by the client, mirroring the
//! `sidebar` conventions. A tile would resize the master's PTY on every toggle and start
//! a focus war over the keyboard. The keyboard stays in the middle pane; talking to the
//! master means jumping to its tab.
//!
//! Pure text here, like the sidebar. The caller paints attributes over these lines, and
//! every line is exactly the requested width in columns, so the caller can paint blindly.

use std::path::{Path, PathBuf};

use qoro_runtime::control::{AgentRow, WorkspaceRow};

/// Columns the right column occupies.
///
/// A quarter of the window, so the column grows with the screen the way the
/// pane does: a fixed width is a margin on a wide monitor and a wall on a
/// narrow one. Floored at the width a `session/terminal state` row plus a
/// short word still fits in, capped before the column eats the terminal it
/// watches. Every line is exactly the requested width.
pub const MIN_WIDTH: usize = 38;
pub const MAX_WIDTH: usize = 64;

/// The column width for a window this wide, in columns.
pub fn width_for(cols: usize) -> usize {
    (cols / 4).clamp(MIN_WIDTH, MAX_WIDTH)
}

/// What the right column drew on one screen row, so a click can act on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The header: jump to the master tab.
    Header,
    /// One terminal that needs a person: focus its tab.
    Need(u32),
    /// The footer keys: nothing to click, so a click does nothing.
    Footer,
}

/// One entry of the master's ask list, as the master loop wrote it.
///
/// Every field defaults, because the reader must never die on a writer that
/// grew a field: an unknown shape is a missing report, not a crash. What the
/// panel shows from a need is matched by terminal number against the live
/// index, and by generation when the writer knew it; the rest is the master's
/// words, displayed, never acted on here.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Need {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub terminal: u32,
    /// The generation the loop saw when it filed the ask. Absent in older
    /// reports, which match any generation; present, it must equal the live
    /// row, or the summary belongs to a stranger reusing the number.
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub report: String,
}

/// The triage order, shared by the paint and the approval.
///
/// One literal in one place: two copies of this list is how the column
/// paints one head and the `[a]` key verdicts another.
const TRIAGE_ORDER: [&str; 3] = ["blocked", "failed", "done"];

/// What a painted line may carry.
///
/// The column is fixed-width text painted blindly: a newline, carriage
/// return, tab or escape inside a space name, agent name, state word,
/// summary or report would break cursor addressing or reach the terminal
/// raw; bidi overrides would reorder the words a person reads before
/// pressing `[a]` without changing a single column. Every such character
/// becomes one blank before measuring, so the width math below still holds.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || is_format_control(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Invisible format controls `char::is_control` misses: bidi controls and
/// isolates, zero-width and joining behaviour, the BOM, the line and
/// paragraph separators, and the rest of `Cf` (annotation and deprecated
/// formatting characters included: anything the terminal draws as nothing
/// is not content the person approved).
fn is_format_control(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{13430}'..='\u{13438}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// The master's periodic report, as `docs/master-brief.md` specifies it.
///
/// Written atomically (tmp file plus rename) by the loop, read here on every
/// index arrival. Stale or broken reads as absent: the panel shows the live
/// index alone rather than yesterday's asks.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Report {
    #[serde(default)]
    pub master_terminal: u32,
    #[serde(default)]
    pub at_unix: u64,
    #[serde(default)]
    pub needs: Vec<Need>,
    #[serde(default)]
    pub last_report: String,
    #[serde(default)]
    pub heartbeat_s: u64,
}

/// Where the loop writes and the panel reads, inside the session directory.
pub fn report_path(dir: &Path) -> PathBuf {
    dir.join("master.json")
}

/// Where approvals wait for the loop, one file per ask.
pub fn approvals_dir(dir: &Path) -> PathBuf {
    dir.join("master-approvals")
}

/// The default supervisor instructions, the same file `qoro master brief`
/// prints: one source, so the pasted brief and the printed one never drift.
pub const MASTER_BRIEF: &str = include_str!("../../../docs/master-brief.md");

/// The agent binaries a fresh session starts its master in, first found wins.
///
/// `QORO_MASTER_AGENT` names one explicitly (a name or a path); otherwise the
/// first of these on `PATH` is the default. The order is a guess about the
/// machine, written down so it can be argued with rather than hidden.
const KNOWN_AGENTS: [&str; 5] = ["opencode", "claude", "codex", "gemini", "cursor"];

/// Which agent binary a fresh session starts its master in, if any.
///
/// `None` means no candidate: no override and nothing known on `PATH`. The
/// caller says so instead of starting a shell with no master in it.
pub fn detect_agent() -> Option<String> {
    if let Ok(want) = std::env::var("QORO_MASTER_AGENT") {
        let want = want.trim().to_string();
        if !want.is_empty() {
            return Some(want);
        }
    }
    KNOWN_AGENTS
        .iter()
        .find(|a| on_path(a))
        .map(|a| a.to_string())
}

/// Whether the session has already had its automatic master: marker file in
/// the session directory, so a closed master tab never resurrects on the next
/// attach. An empty directory (a socket with no parent) never auto-starts.
pub fn auto_path(dir: &Path) -> PathBuf {
    dir.join("master-auto")
}

pub fn auto_done(dir: &Path) -> bool {
    dir.as_os_str().is_empty() || auto_path(dir).exists()
}

pub fn mark_auto(dir: &Path) {
    if dir.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::write(auto_path(dir), "auto\n");
}

/// Whether this live row looks like the awaited master agent: the program is
/// the binary (by path or by last segment), or the daemon already recognised
/// a coding agent in the terminal. The program check is what fires in
/// practice; the agent flag is the backstop for wrappers and shims.
pub fn agent_matches(row: &AgentRow, binary: &str) -> bool {
    let base = binary.rsplit('/').next().unwrap_or(binary);
    if row.agent {
        return true;
    }
    match row.program.as_deref() {
        Some(program) => program == binary || program.rsplit('/').next().unwrap_or(program) == base,
        None => false,
    }
}

/// A binary name on `PATH`, the way a shell would find it.
fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(binary);
            std::fs::metadata(&candidate).is_ok_and(|m| !m.is_dir())
        })
    })
}

/// Read the report, or nothing when it is missing, broken or stale.
///
/// Best effort on purpose: this runs on the index path, and a panel that
/// errored on a half-written file would blink every time the master saved.
/// An empty directory (a socket with no parent) reads as absent, never as
/// the client working directory. Reports past the size cap read as absent
/// too, so one pathological file cannot stall the event loop.
pub fn read_report(dir: &Path) -> Option<Report> {
    if dir.as_os_str().is_empty() {
        return None;
    }
    let path = report_path(dir);
    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_REPORT_BYTES)
        .unwrap_or(false)
    {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() as u64 > MAX_REPORT_BYTES {
        return None;
    }
    let report: Report = serde_json::from_slice(&bytes).ok()?;
    live(&report, &path).then_some(report)
}

/// The largest heartbeat the panel believes: an hour between polls is a dead
/// loop, not a slow one, and without a ceiling a huge value would make a
/// stale report read as fresh almost forever.
const MAX_HEARTBEAT_S: u64 = 3600;

/// The largest report body the panel will parse on the index path.
const MAX_REPORT_BYTES: u64 = 256 * 1024;

/// Whether the report is fresh enough to present as news.
///
/// Saturating and fail-closed: a zero or absurd heartbeat, a timestamp in
/// the future and a file older than three missed polls all read as silence,
/// never as fresh asks. The mtime cross-check catches a loop that died with
/// a plausible `at_unix` inside.
fn live(report: &Report, path: &Path) -> bool {
    if report.heartbeat_s == 0 || report.heartbeat_s > MAX_HEARTBEAT_S {
        return false;
    }
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return false,
    };
    let age = match now.checked_sub(report.at_unix) {
        Some(age) => age,
        None => return false,
    };
    if age > report.heartbeat_s.saturating_mul(3) {
        return false;
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| now.checked_sub(d.as_secs()))
        .is_some_and(|file_age| file_age <= report.heartbeat_s.saturating_mul(3).saturating_add(60))
}

/// Whether an approval for this ask is already waiting for the loop.
///
/// A file still present is pending, so filing again would queue a duplicate
/// verdict for the same ask. The caller reports `already pending` instead.
pub fn is_pending(dir: &Path, id: &str) -> bool {
    if dir.as_os_str().is_empty() {
        return false;
    }
    let name = flat_name(id);
    let prefix = format!("{name}.");
    std::fs::read_dir(approvals_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&prefix) && n.ends_with(".json")
        })
}

/// The flat filename stem for an ask id.
///
/// The id comes from the master's file, and a `../` in it must not escape
/// the approvals directory.
fn flat_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.is_empty() {
        "ask".to_string()
    } else {
        safe
    }
}

/// File one approval for the loop to consume.
///
/// Atomic tmp-plus-rename into `master-approvals/`, so the loop never reads
/// a half-written verdict. The loop applies in `seq` order and deletes; a
/// file still present is pending.
///
/// `seq` is monotonic within this process: wall-clock nanos, bumped past the
/// last one when the clock repeats itself, so two rapid approvals never share
/// a sequence number or a filename. The pid in the filename keeps two clients
/// from sharing one either.
/// Why an approval was not filed.
#[derive(Debug)]
pub enum ApprovalError {
    /// A verdict file for this ask is already waiting for the loop.
    Pending(String),
    /// The filing itself failed.
    Io(std::io::Error),
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalError::Pending(id) => write!(f, "already pending {id}"),
            ApprovalError::Io(e) => write!(f, "approval failed: {e}"),
        }
    }
}

/// Check-then-file as one step within this process.
///
/// `is_pending` followed by `write_approval` is a race: two rapid keypresses
/// both see nothing pending and file two verdicts for the same ask. The
/// mutex serialises the pair, so the second press sees the first press's
/// file. Two clients on one session can still double-file across processes;
/// the loop's `seq` order plus delete is what makes that duplicate harmless.
pub fn file_approval(dir: &Path, id: &str, verdict: &str) -> Result<PathBuf, ApprovalError> {
    static APPROVAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = APPROVAL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if is_pending(dir, id) {
        return Err(ApprovalError::Pending(id.to_string()));
    }
    write_approval(dir, id, verdict).map_err(ApprovalError::Io)
}

pub fn write_approval(dir: &Path, id: &str, verdict: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static LAST_SEQ: AtomicU64 = AtomicU64::new(0);

    if dir.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no session directory for approvals",
        ));
    }
    let approvals = approvals_dir(dir);
    std::fs::create_dir_all(&approvals)?;
    let name = flat_name(id);
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        .min(u128::from(u64::MAX)) as u64;
    let seq = loop {
        let prev = LAST_SEQ.load(Ordering::Relaxed);
        let next = prev.saturating_add(1).max(wall);
        match LAST_SEQ.compare_exchange_weak(prev, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(_) => break next,
            Err(_) => continue,
        }
    };
    let pid = std::process::id();
    let body = serde_json::json!({
        "id": id,
        "verdict": verdict,
        "seq": seq,
        "at_unix": seq / 1_000_000_000,
    });
    let tmp = approvals.join(format!(".{name}.{seq}.{pid}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        write!(file, "{body}")?;
        file.sync_all()?;
    }
    let done = approvals.join(format!("{name}.{seq}.{pid}.json"));
    std::fs::rename(&tmp, &done)?;
    Ok(done)
}

/// The structured block of the right column: header, needs, report, footer.
///
/// A header row (the pinned master, or what to do when nothing is pinned), a rule, the
/// `needs you` rows across all spaces except the master itself, another rule, and the
/// footer keys. Blocked first, then failed, then done, each in daemon order: the same
/// order the status line triages in, so the column and the ticker never disagree about
/// what matters most.
///
/// Truncated to `height` rows, never padded: the live viewport of the master's snapshot
/// fills whatever rows remain, and padding here would steal them.
///
/// Summaries and the last report come from the master's file when it is
/// fresh; without it the live index alone is shown, with states and no
/// words.
pub fn rows(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    master: Option<(u32, Option<u64>)>,
    focused: u32,
    report: Option<&Report>,
    height: usize,
    width: usize,
) -> Vec<(crate::sidebar::Line, Option<Hit>)> {
    // The painter derives the keyboard's own row from the hit, the way it does for the
    // left column, so the text carries no focus mark of its own.
    let _ = focused;
    if height == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.push(header(rows, master, width));
    out.push(divider(width));
    let needs = triage(rows, spaces, master, report, width);
    out.push((
        crate::sidebar::Line {
            text: crate::width::fit(&format!(" needs you {}", needs.len()), width),
            colour: Some(crate::theme::Role::Title),
            accent: None,
            indicator: None,
        },
        None,
    ));
    out.extend(needs);
    out.push(divider(width));
    // The master's own words, at most three rows: a fourth row of summary is
    // a paragraph, and paragraphs belong on the master tab, not in the
    // margin.
    if let Some(report) = report {
        let said = report.last_report.trim();
        if !said.is_empty() {
            out.push((
                crate::sidebar::Line {
                    text: crate::width::fit(" son rapor", width),
                    colour: Some(crate::theme::Role::Title),
                    accent: None,
                    indicator: None,
                },
                None,
            ));
            for line in said.lines().take(3) {
                let line = clean(line);
                out.push((
                    crate::sidebar::Line {
                        text: crate::width::fit(&format!(" {line}"), width),
                        colour: None,
                        accent: None,
                        indicator: None,
                    },
                    None,
                ));
            }
        }
    }
    out.push((
        crate::sidebar::Line {
            text: crate::width::fit("[E] master [a] onayla [u] reddet", width),
            colour: Some(crate::theme::Role::Faint),
            accent: None,
            indicator: None,
        },
        Some(Hit::Footer),
    ));
    out.truncate(height);
    out
}

/// What a click on one screen row of the right column means, counted from zero.
///
/// Built from the same [`rows`], so render and hit agree by construction: the test below
/// only has to confirm it.
pub fn hit(
    agents: &[AgentRow],
    spaces: &[WorkspaceRow],
    master: Option<(u32, Option<u64>)>,
    focused: u32,
    report: Option<&Report>,
    height: usize,
    row: usize,
    width: usize,
) -> Option<Hit> {
    rows(agents, spaces, master, focused, report, height, width)
        .into_iter()
        .nth(row)
        .and_then(|(_, hit)| hit)
}

/// The header: the pinned master and its state, or what to do when nothing is pinned.
///
/// A pin whose terminal is gone renders as unpinned, never as someone else's screen:
/// the caller validates against the freshest rows, and this is the backstop.
/// The ask the `[a]` / `[u]` keys would answer: the head of what the column
/// paints, not the head of the file.
///
/// The report lists asks in filing order while the column paints triage
/// order (blocked, failed, done, daemon order), so `needs.first()` and the
/// top painted need disagree whenever the file order does. Approving the
/// file head would verdict the wrong terminal; this returns the ask behind
/// the painted head instead. Asks with no live row (ghosts) and asks hidden
/// behind the master's pin are skipped, never filed.
pub fn approval_head(
    rows: &[AgentRow],
    _spaces: &[WorkspaceRow],
    master: Option<(u32, Option<u64>)>,
    report: Option<&Report>,
) -> Option<Need> {
    let report = report?;
    for state in TRIAGE_ORDER {
        for row in rows
            .iter()
            .filter(|r| r.state == state)
            .filter(|r| !is_master(r, master))
        {
            if let Some(need) = report.needs.iter().find(|n| need_matches(n, row)) {
                return Some(need.clone());
            }
        }
    }
    None
}

fn header(
    rows: &[AgentRow],
    master: Option<(u32, Option<u64>)>,
    width: usize,
) -> (crate::sidebar::Line, Option<Hit>) {
    let pinned = master.and_then(|(terminal, generation)| {
        rows.iter()
            .find(|r| r.terminal == terminal && r.generation == generation)
    });
    match pinned {
        Some(row) => (
            crate::sidebar::Line {
                text: crate::width::fit(
                    &format!("master {} {}", clean(&label(row)), clean(&row.state)),
                    width,
                ),
                colour: crate::sidebar::colour_for(&row.state),
                accent: None,
                indicator: None,
            },
            Some(Hit::Header),
        ),
        None => (
            crate::sidebar::Line {
                text: crate::width::fit("master yok: bir tab pinle", width),
                colour: Some(crate::theme::Role::Faint),
                accent: None,
                indicator: None,
            },
            Some(Hit::Header),
        ),
    }
}

/// The terminals that need a person, across all spaces, master excluded.
///
/// Blocked, then failed, then done, each in daemon order. Done is displayed, never
/// invented: the row is there because the daemon reported it, which the state vocabulary
/// and the bell already recognise. The pinned master is excluded even when it is itself
/// blocked: its screen is already below, and listing it here would point at itself.
/// Whether this live row is the pinned master.
///
/// Terminal number alone is not identity: a closed terminal's number is
/// reused, so the generation must match too, or the exclusion below hides a
/// stranger's need behind the master's pin.
fn is_master(row: &AgentRow, master: Option<(u32, Option<u64>)>) -> bool {
    match master {
        Some((terminal, generation)) => row.terminal == terminal && row.generation == generation,
        None => false,
    }
}

/// Whether a filed ask names this live row.
///
/// Reports written before generations existed carry no generation and match
/// any; newer ones must equal the live row, or a summary filed for a dead
/// terminal would stick to whoever reused its number.
fn need_matches(need: &Need, row: &AgentRow) -> bool {
    need.terminal == row.terminal
        && match need.generation {
            None => true,
            Some(generation) => row.generation == Some(generation),
        }
}

fn triage(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    master: Option<(u32, Option<u64>)>,
    report: Option<&Report>,
    width: usize,
) -> Vec<(crate::sidebar::Line, Option<Hit>)> {
    let mut out = Vec::new();
    for state in TRIAGE_ORDER {
        for row in rows
            .iter()
            .filter(|r| r.state == state)
            .filter(|r| !is_master(r, master))
        {
            // Which space this agent lives in, in that space's colour: the number the
            // sidebar paints is not on this row, so the colour lands on the name instead.
            let space = clean(&space_name(spaces, row.workspace));
            // The master's summary, when the file named this terminal: its words,
            // cut to the column like everything else.
            let summary = report
                .and_then(|r| r.needs.iter().find(|n| need_matches(n, row)))
                .map(|n| clean(n.summary.trim()))
                .filter(|s| !s.is_empty())
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            // `row.state` is cleaned like every other field. It always
            // equals one of the triage words today (the filter above), but
            // the daemon owns that vocabulary and the paint must not trust
            // it: one hostile state word would reorder the reader's screen.
            let text = format!(
                " {space}/{} {}{summary}",
                clean(&label(row)),
                clean(&row.state)
            );
            out.push((
                crate::sidebar::Line {
                    text: crate::width::fit(&text, width),
                    colour: crate::sidebar::colour_for(&row.state),
                    accent: Some((
                        1,
                        crate::width::width(&space),
                        crate::theme::space_of(row.workspace),
                    )),
                    indicator: None,
                },
                Some(Hit::Need(row.terminal)),
            ));
        }
    }
    return out;

    /// The space a row lives in, or its number when the space list has not arrived yet.
    ///
    /// Nested so the lookup reads beside its only caller. The number is always true even
    /// before the daemon answers, which is exactly when a row must not vanish.
    fn space_name(spaces: &[WorkspaceRow], workspace: i64) -> String {
        spaces
            .iter()
            .find(|s| s.id == workspace)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| workspace.to_string())
    }
}

/// What to call the program in the terminal: the chosen name, the program's last path
/// segment, or the terminal number when there is nothing else.
///
/// The sidebar spells this the same way, so the two columns name the same terminal the
/// same way. A column this narrow renders `/opt/homebrew/bin/claude` as a prefix of
/// nothing, which identifies nothing.
fn label(row: &AgentRow) -> String {
    if let Some(name) = row.name.as_deref().filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    match &row.program {
        Some(program) => program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(program)
            .to_string(),
        None => format!("term {}", row.terminal),
    }
}

/// The rule above and below the needs: one row of box drawing, clickable as nothing.
fn divider(width: usize) -> (crate::sidebar::Line, Option<Hit>) {
    (
        crate::sidebar::Line {
            text: "\u{2500}".repeat(width),
            colour: Some(crate::theme::Role::Rule),
            accent: None,
            indicator: None,
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            id,
            name: name.to_string(),
            terminals: 1,
        }
    }

    fn agent(terminal: u32, workspace: i64, program: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: None,
            workspace,
            agent: true,
            program: Some(program.to_string()),
            behind: None,
            state: state.to_string(),
            generation: Some(1),
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
        }
    }

    fn named(terminal: u32, workspace: i64, name: &str, state: &str) -> AgentRow {
        AgentRow {
            terminal,
            name: Some(name.to_string()),
            workspace,
            agent: true,
            program: Some("claude".to_string()),
            behind: None,
            state: state.to_string(),
            generation: Some(1),
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
        }
    }

    fn sample() -> (Vec<AgentRow>, Vec<WorkspaceRow>) {
        (
            vec![
                agent(1, 1, "claude", "done"),
                agent(2, 2, "codex", "failed"),
                agent(3, 1, "opencode", "blocked"),
                agent(4, 1, "zsh", "working"),
            ],
            vec![space(1, "app"), space(2, "home")],
        )
    }

    #[test]
    fn every_line_is_exactly_the_column_width() {
        let (agents, spaces) = sample();
        for width in [MIN_WIDTH, 50, MAX_WIDTH] {
            for master in [None, Some((3, Some(1))), Some((9, None))] {
                // The structured block, uncut: everything below is a truncation of this.
                let full = rows(&agents, &spaces, master, 4, None, 40, width).len();
                for height in [1usize, 2, 3, 5, 9, 14, 40] {
                    let lines = rows(&agents, &spaces, master, 4, None, height, width);
                    assert_eq!(lines.len(), full.min(height), "height {height}");
                    for (line, _) in &lines {
                        assert_eq!(crate::width::width(&line.text), width, "{:?}", line.text);
                    }
                }
            }
        }
    }

    #[test]
    fn a_wide_name_is_measured_in_columns_and_not_in_characters() {
        let agents = vec![agent(1, 1, &"\u{1f30d}".repeat(30), "blocked")];
        let spaces = vec![space(1, "app")];
        for (line, _) in rows(&agents, &spaces, None, 1, None, 10, MIN_WIDTH) {
            assert_eq!(
                crate::width::width(&line.text),
                MIN_WIDTH,
                "{:?}",
                line.text
            );
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn the_needs_come_blocked_then_failed_then_done() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 20, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        let blocked = texts.iter().position(|t| t.contains("blocked")).unwrap();
        let failed = texts.iter().position(|t| t.contains("failed")).unwrap();
        let done = texts.iter().position(|t| t.contains("done")).unwrap();
        assert!(blocked < failed && failed < done, "{texts:?}");
        assert!(texts.iter().any(|t| t == " needs you 3"), "{texts:?}");
        // Working rows are not needs, so the shell agent is nowhere in the list.
        assert!(!texts.iter().any(|t| t.contains("working")), "{texts:?}");
    }

    #[test]
    fn the_pinned_master_is_not_listed_as_its_own_need() {
        let (agents, spaces) = sample();
        // Terminal 3 is blocked and pinned: the header names it, the list must not.
        let lines = rows(&agents, &spaces, Some((3, Some(1))), 4, None, 20, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts[0].contains("master")
                && texts[0].contains("opencode")
                && texts[0].contains("blocked"),
            "{texts:?}"
        );
        let needs: Vec<&String> = texts
            .iter()
            .filter(|t| t.contains("failed") || t.contains("done"))
            .collect();
        assert_eq!(needs.len(), 2, "{texts:?}");
        assert!(texts.iter().any(|t| t == " needs you 2"), "{texts:?}");
        let hits: Vec<Option<Hit>> = (0..20)
            .map(|row| hit(&agents, &spaces, Some((3, Some(1))), 4, None, 20, row, 38))
            .collect();
        assert!(!hits.contains(&Some(Hit::Need(3))), "{hits:?}");
        assert!(hits.contains(&Some(Hit::Need(2))), "{hits:?}");
        assert!(hits.contains(&Some(Hit::Need(1))), "{hits:?}");
    }

    #[test]
    fn without_a_pin_the_header_says_how_to_get_one() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 20, 38);
        assert_eq!(
            lines[0].0.text.trim_end(),
            "master yok: bir tab pinle",
            "{:?}",
            lines[0].0.text
        );
    }

    #[test]
    fn a_chosen_name_wins_over_the_program_and_a_bare_terminal_keeps_its_number() {
        let agents = vec![
            named(1, 1, "release", "blocked"),
            AgentRow {
                terminal: 2,
                name: None,
                workspace: 1,
                agent: true,
                program: None,
                behind: None,
                state: "failed".to_string(),
                generation: None,
                readiness: "ready".to_string(),
                source: None,
                rule: None,
                manifest: None,
                agent_session: None,
            },
        ];
        let spaces = vec![space(1, "app")];
        let lines = rows(&agents, &spaces, None, 2, None, 20, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(texts.iter().any(|t| t.contains("release")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("term 2")), "{texts:?}");
    }

    #[test]
    fn the_footer_carries_the_three_keys() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 20, 38);
        let last = lines.last().expect("a footer");
        assert!(
            last.0.text.contains("[E]")
                && last.0.text.contains("[a]")
                && last.0.text.contains("[u]"),
            "{:?}",
            last.0.text
        );
        assert_eq!(last.1, Some(Hit::Footer));
    }

    #[test]
    fn hit_returns_exactly_what_rows_drew_on_every_row() {
        let (agents, spaces) = sample();
        let with_report = fresh_report();
        let reports: [Option<&Report>; 2] = [None, Some(&with_report)];
        for height in [1usize, 2, 3, 4, 7, 9, 14, 25] {
            for master in [None, Some((3, Some(1)))] {
                for report in reports {
                    let drawn = rows(&agents, &spaces, master, 4, report, height, 38);
                    for (at, (_, want)) in drawn.iter().enumerate() {
                        assert_eq!(
                            hit(&agents, &spaces, master, 4, report, height, at, 38),
                            *want,
                            "height {height} row {at}"
                        );
                    }
                    assert_eq!(
                        hit(
                            &agents,
                            &spaces,
                            master,
                            4,
                            report,
                            height,
                            drawn.len() + 3,
                            38
                        ),
                        None,
                        "past the end"
                    );
                }
            }
        }
    }

    fn fresh_report() -> Report {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Report {
            master_terminal: 4,
            at_unix: now,
            needs: vec![Need {
                id: "w3".to_string(),
                session: "app".to_string(),
                terminal: 3,
                generation: None,
                state: "blocked".to_string(),
                summary: "soru soruyor".to_string(),
                report: "uzun rapor".to_string(),
            }],
            last_report: "bir\niki\nuc\ndort".to_string(),
            heartbeat_s: 15,
        }
    }

    #[test]
    fn a_report_adds_summaries_and_at_most_three_report_rows() {
        let (agents, spaces) = sample();
        let report = fresh_report();
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("soru soruyor")),
            "{texts:?}"
        );
        assert!(texts.iter().any(|t| t == " son rapor"), "{texts:?}");
        assert!(texts.iter().any(|t| t == " bir"), "{texts:?}");
        assert!(texts.iter().any(|t| t == " uc"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t == " dort"),
            "the fourth report row is cut: {texts:?}"
        );
        for (line, _) in &lines {
            assert_eq!(crate::width::width(&line.text), MIN_WIDTH);
        }
    }

    #[test]
    fn long_and_wide_summaries_and_reports_stay_in_the_column() {
        let (agents, spaces) = sample();
        let mut report = fresh_report();
        report.needs[0].summary = "soru ".repeat(20) + &"\u{1f30d}".repeat(10);
        report.last_report = format!(
            "{}\n{}\n{}",
            "x".repeat(100),
            "\u{1f30d}".repeat(30),
            "kisa"
        );
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30, 38);
        for (line, _) in &lines {
            assert_eq!(
                crate::width::width(&line.text),
                MIN_WIDTH,
                "{:?}",
                line.text
            );
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn without_a_report_the_index_alone_is_shown() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 30, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(!texts.iter().any(|t| t == " son rapor"), "{texts:?}");
    }

    fn write_report(dir: &Path, body: &str) {
        std::fs::write(report_path(dir), body).expect("write the report");
    }

    #[test]
    fn a_stale_or_broken_report_reads_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_report(dir.path()).is_none());
        write_report(dir.path(), "not json");
        assert!(read_report(dir.path()).is_none());
        let old = fresh_report();
        let body = format!(
            "{{\"master_terminal\": {}, \"at_unix\": 1, \"needs\": [], \"last_report\": \"\", \"heartbeat_s\": {}}}",
            old.master_terminal, old.heartbeat_s
        );
        write_report(dir.path(), &body);
        assert!(read_report(dir.path()).is_none());
        let zeroed = body.replace("\"heartbeat_s\": 15", "\"heartbeat_s\": 0");
        write_report(dir.path(), &zeroed);
        assert!(read_report(dir.path()).is_none());
    }

    #[test]
    fn a_fresh_report_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        write_report(
            dir.path(),
            &format!(
                "{{\"master_terminal\": 4, \"at_unix\": {now}, \"needs\": [{{\"id\": \"w1\", \"terminal\": 2}}], \"last_report\": \"hi\", \"heartbeat_s\": 15}}"
            ),
        );
        let report = read_report(dir.path()).expect("a fresh report");
        assert_eq!(report.master_terminal, 4);
        assert_eq!(report.needs.len(), 1);
        assert_eq!(report.needs[0].id, "w1");
    }

    #[test]
    fn approvals_land_atomically_and_ids_stay_flat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let done = write_approval(dir.path(), "w1", "approved").expect("write");
        assert!(done.is_file(), "{done:?}");
        assert!(
            !std::fs::read_dir(dir.path()).expect("list").any(|e| e
                .map(|e| e.file_name().to_string_lossy().starts_with('.'))
                .unwrap_or(false)),
            "no tmp file is left behind"
        );
        let body = std::fs::read_to_string(&done).expect("read");
        let value: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(value["id"], "w1");
        assert_eq!(value["verdict"], "approved");
        let evil = write_approval(dir.path(), "../../evil", "rejected").expect("write");
        assert_eq!(
            evil.parent(),
            Some(approvals_dir(dir.path()).as_path()),
            "{evil:?}"
        );
        assert!(
            !evil
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .contains('/'),
            "{evil:?}"
        );
    }

    #[test]
    fn two_rapid_approvals_get_increasing_sequences_and_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = write_approval(dir.path(), "w1", "approved").expect("write");
        let second = write_approval(dir.path(), "w1", "approved").expect("write");
        assert_ne!(first, second, "the second approval overwrote the first");
        let seq_of = |p: &std::path::Path| {
            let body = std::fs::read_to_string(p).expect("read");
            let value: serde_json::Value = serde_json::from_str(&body).expect("json");
            value["seq"].as_u64().expect("a numeric seq")
        };
        assert!(
            seq_of(&second) > seq_of(&first),
            "the loop applies in seq order, so equal seq is unordered"
        );
        assert!(is_pending(dir.path(), "w1"));
        assert!(!is_pending(dir.path(), "w2"));
    }

    #[test]
    fn an_empty_id_files_under_ask_and_stays_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let done = write_approval(dir.path(), "", "approved").expect("write");
        let stem = done
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        assert!(stem.starts_with("ask."), "{done:?}");
        assert!(is_pending(dir.path(), ""));
    }

    fn hostile_agents() -> (Vec<AgentRow>, Vec<WorkspaceRow>) {
        (
            vec![
                AgentRow {
                    terminal: 1,
                    // Hostile on a row triage actually paints (`blocked`):
                    // each site below must clean its own field, and the
                    // test fails if any single one stops doing so.
                    name: Some("a\nb\x1bc\u{202E}d".to_string()),
                    workspace: 1,
                    agent: true,
                    program: Some("claude".to_string()),
                    behind: None,
                    state: "blocked".to_string(),
                    generation: Some(1),
                    readiness: "ready".to_string(),
                    source: None,
                    rule: None,
                    manifest: None,
                    agent_session: None,
                },
                // A second row under the same triage state, so a test can tell
                // whether the approval follows the paint or the file.
                agent(2, 1, "codex", "blocked"),
            ],
            vec![space(1, "sp\nace\x1b")],
        )
    }

    fn hostile_report() -> Report {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Report {
            master_terminal: 9,
            at_unix: now,
            needs: vec![Need {
                id: "w1".to_string(),
                session: "app".to_string(),
                terminal: 1,
                generation: None,
                state: "blocked".to_string(),
                summary: "sum\nmary\x1b[2J\ttab".to_string(),
                report: String::new(),
            }],
            last_report: "one\ntwo\rthree\x1b[31mred".to_string(),
            heartbeat_s: 15,
        }
    }

    #[test]
    fn control_characters_never_leave_the_column() {
        let (agents, spaces) = hostile_agents();
        let report = hostile_report();
        // The header paints too: pin the hostile terminal so its name runs
        // through the header's own cleaning, not just the need row's.
        let pinned = rows(
            &agents,
            &spaces,
            Some((1, Some(1))),
            1,
            Some(&report),
            30,
            38,
        );
        assert!(!pinned.is_empty());
        for (line, _) in &pinned {
            assert_eq!(
                crate::width::width(&line.text),
                MIN_WIDTH,
                "{:?}",
                line.text
            );
            assert!(
                !line
                    .text
                    .chars()
                    .any(|c| c.is_control() || is_format_control(c)),
                "an invisible character reached the header: {line:?}"
            );
        }
        let texts: Vec<String> = pinned
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts[0].contains("master"),
            "the header names the pin: {texts:?}"
        );
        let lines = rows(&agents, &spaces, None, 1, Some(&report), 30, 38);
        assert!(!lines.is_empty());
        for (line, _) in &lines {
            assert_eq!(
                crate::width::width(&line.text),
                MIN_WIDTH,
                "{:?}",
                line.text
            );
            assert!(
                !line
                    .text
                    .chars()
                    .any(|c| c.is_control() || is_format_control(c)),
                "an invisible character reached the paint: {line:?}"
            );
            assert!(
                !line.text.contains('\u{202E}'),
                "a bidi override reordered the paint: {line:?}"
            );
        }
        // The hostile name must read as plain text now.
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            !texts.iter().any(|t| t.contains("a\nb")),
            "a newline survived in the name: {texts:?}"
        );
        // The ESC byte is gone, so a remaining `[2J` is inert text: no
        // control or format character may survive, and the width must still
        // hold (both asserted in the loop above).
    }

    #[test]
    fn approval_head_follows_the_paint_not_the_file() {
        let (agents, spaces) = sample();
        // The file lists done first, but the column paints blocked first.
        let mut report = fresh_report();
        report.needs = vec![
            Need {
                id: "w-done".to_string(),
                session: "app".to_string(),
                terminal: 1,
                generation: None,
                state: "done".to_string(),
                summary: String::new(),
                report: String::new(),
            },
            Need {
                id: "w-blocked".to_string(),
                session: "app".to_string(),
                terminal: 3,
                generation: None,
                state: "blocked".to_string(),
                summary: String::new(),
                report: String::new(),
            },
        ];
        let head = approval_head(&agents, &spaces, None, Some(&report)).expect("a head");
        assert_eq!(head.id, "w-blocked", "the painted head, not the file head");
        assert_eq!(head.terminal, 3);
        // Asks with no live row are ghosts: filed never.
        report.needs = vec![Need {
            id: "w-ghost".to_string(),
            session: "app".to_string(),
            terminal: 9,
            generation: None,
            state: "blocked".to_string(),
            summary: String::new(),
            report: String::new(),
        }];
        assert!(
            approval_head(&agents, &spaces, None, Some(&report)).is_none(),
            "a ghost terminal files nothing"
        );
        assert!(
            approval_head(&agents, &spaces, None, None).is_none(),
            "no report files nothing"
        );
        // A pinned master is skipped even when it heads the triage: pin
        // terminal 3 while both it and terminal 2 ask, and the head must be
        // the unpinned one.
        let pinned_and_plain = Report {
            needs: vec![
                Need {
                    id: "w-pinned".to_string(),
                    session: "app".to_string(),
                    terminal: 3,
                    generation: None,
                    state: "blocked".to_string(),
                    summary: String::new(),
                    report: String::new(),
                },
                Need {
                    id: "w-plain".to_string(),
                    session: "app".to_string(),
                    terminal: 2,
                    generation: None,
                    state: "failed".to_string(),
                    summary: String::new(),
                    report: String::new(),
                },
            ],
            ..fresh_report()
        };
        let head = approval_head(
            &agents,
            &spaces,
            Some((3, Some(1))),
            Some(&pinned_and_plain),
        )
        .expect("the unpinned ask survives the pin");
        assert_eq!(head.id, "w-plain", "the pin answers for nobody");
        assert_eq!(head.terminal, 2);
    }
    #[test]
    fn summaries_ignore_a_reused_terminal_number() {
        let (agents, spaces) = sample();
        // Terminal 3 is live at generation 1; the ask names generation 2.
        let mut report = fresh_report();
        report.needs[0].terminal = 3;
        report.needs[0].generation = Some(2);
        report.needs[0].summary = "stale words".to_string();
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            !texts.iter().any(|t| t.contains("stale words")),
            "a stranger's summary stuck to a reused number: {texts:?}"
        );
        // Reports without a generation still match, as before.
        report.needs[0].generation = None;
        report.needs[0].summary = "soru soruyor".to_string();
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("soru soruyor")),
            "an old report lost its summary: {texts:?}"
        );
    }

    #[test]
    fn the_master_pin_holds_only_its_generation() {
        let (agents, spaces) = sample();
        // Terminal 3 is live at generation 1; the pin names generation 2.
        let lines = rows(&agents, &spaces, Some((3, Some(2))), 4, None, 20, 38);
        let texts: Vec<String> = lines
            .iter()
            .map(|(l, _)| l.text.trim_end().to_string())
            .collect();
        assert_eq!(texts[0], "master yok: bir tab pinle", "{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("blocked")),
            "the stranger is a need, not the master: {texts:?}"
        );
    }

    #[test]
    fn an_absurd_heartbeat_reads_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        write_report(
            dir.path(),
            &format!(
                "{{\"master_terminal\": 4, \"at_unix\": {now}, \"needs\": [], \"last_report\": \"\", \"heartbeat_s\": 99999999}}"
            ),
        );
        assert!(
            read_report(dir.path()).is_none(),
            "a huge heartbeat must not keep a dead loop fresh"
        );
    }

    #[test]
    fn oversized_and_homeless_reports_read_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Fresh, so only the size cap can refuse it: with a stale `at_unix`
        // the test would pass even with the cap deleted.
        let big = "x".repeat(300 * 1024);
        write_report(
            dir.path(),
            &format!(
                "{{\"master_terminal\": 4, \"at_unix\": {now}, \"needs\": [], \"last_report\": {big:?}, \"heartbeat_s\": 15}}"
            ),
        );
        assert!(
            read_report(dir.path()).is_none(),
            "a pathological body must not stall the index path"
        );
        // Just under the cap reads back, so the cap is a cap, not a wall.
        let small = "x".repeat(1024);
        write_report(
            dir.path(),
            &format!(
                "{{\"master_terminal\": 4, \"at_unix\": {now}, \"needs\": [], \"last_report\": {small:?}, \"heartbeat_s\": 15}}"
            ),
        );
        assert!(
            read_report(dir.path()).is_some(),
            "a normal body must still read"
        );
        assert!(
            read_report(std::path::Path::new("")).is_none(),
            "no session directory reads as absent, never as the working directory"
        );
        assert!(
            write_approval(std::path::Path::new(""), "w1", "approved").is_err(),
            "no session directory files nothing into the working directory"
        );
        assert!(
            !is_pending(std::path::Path::new(""), "w1"),
            "no session directory pends nothing"
        );
    }

    #[test]
    fn concurrent_filings_leave_one_verdict() {
        use std::sync::{Arc, Barrier};
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = Arc::new(dir);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let dir = Arc::clone(&dir);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let verdict = if n % 2 == 0 { "approved" } else { "rejected" };
                    file_approval(dir.path(), "w-race", verdict)
                })
            })
            .collect();
        let mut filed = 0;
        for handle in handles {
            if handle.join().expect("a thread").is_ok() {
                filed += 1;
            }
        }
        assert_eq!(filed, 1, "exactly one thread files, the rest see pending");
        let files: Vec<_> = std::fs::read_dir(approvals_dir(dir.path()))
            .expect("list")
            .flatten()
            .collect();
        assert_eq!(files.len(), 1, "no duplicate verdict");
    }

    #[test]
    fn the_column_is_a_quarter_of_the_window_within_a_floor_and_a_cap() {
        assert_eq!(width_for(100), MIN_WIDTH, "a narrow window keeps the floor");
        assert_eq!(width_for(152), MIN_WIDTH, "152 / 4 is the floor exactly");
        assert_eq!(width_for(160), 40, "past the floor it grows");
        assert_eq!(width_for(240), 60, "a wide window reads a quarter page");
        assert_eq!(width_for(400), MAX_WIDTH, "and no wider than the cap");
        assert_eq!(width_for(40), MIN_WIDTH, "a strip window keeps the floor");
    }

    #[test]
    fn the_agent_match_names_the_binary_by_path_or_by_detection() {
        let mut row = agent(1, 1, "/opt/homebrew/bin/opencode", "working");
        row.agent = false;
        assert!(agent_matches(&row, "opencode"), "by last segment");
        assert!(agent_matches(&row, "/opt/homebrew/bin/opencode"), "by path");
        assert!(!agent_matches(&row, "claude"), "not another binary");
        row.program = Some("/bin/sh".to_string());
        assert!(agent_matches(&row, "/bin/sh"), "a full path matches itself");
        assert!(!agent_matches(&row, "opencode"), "a shell is no agent");
        row.agent = true;
        assert!(
            agent_matches(&row, "opencode"),
            "a recognised agent is a match under any name"
        );
        row.program = None;
        row.agent = false;
        assert!(!agent_matches(&row, "opencode"), "nothing known, no match");
    }

    #[test]
    fn the_auto_marker_survives_a_restart_and_empty_reads_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!auto_done(dir.path()), "fresh sessions raise once");
        mark_auto(dir.path());
        assert!(auto_done(dir.path()), "then never again");
        let empty = std::path::PathBuf::new();
        assert!(auto_done(&empty), "nowhere reads as done, never as fresh");
    }

    #[test]
    fn file_approval_files_once_then_reports_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        file_approval(dir.path(), "w1", "approved").expect("first filing");
        let second = file_approval(dir.path(), "w1", "approved");
        assert!(
            matches!(second, Err(ApprovalError::Pending(_))),
            "the second rapid press must see the first: {second:?}"
        );
        let files: Vec<_> = std::fs::read_dir(approvals_dir(dir.path()))
            .expect("list")
            .flatten()
            .collect();
        assert_eq!(files.len(), 1, "no duplicate verdict");
    }
}

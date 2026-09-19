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
//! every line is exactly [`MASTER_WIDTH`] columns, so the caller can paint blindly.

use std::path::{Path, PathBuf};

use qoro_runtime::control::{AgentRow, WorkspaceRow};

/// Columns the right column occupies.
///
/// Fixed rather than negotiated, for the same reason the sidebar is: the terminal beside
/// it is a real PTY sized before its child starts. Thirty eight carries a
/// `session/terminal state` row plus a short word; thirty (the sidebar width) left the
/// state with nothing beside it.
pub const MASTER_WIDTH: usize = 38;

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
/// index; the rest is the master's words, displayed, never acted on here.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Need {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub terminal: u32,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub report: String,
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

/// Read the report, or nothing when it is missing, broken or stale.
///
/// Best effort on purpose: this runs on the index path, and a panel that
/// errored on a half-written file would blink every time the master saved.
pub fn read_report(dir: &Path) -> Option<Report> {
    let path = report_path(dir);
    let bytes = std::fs::read(&path).ok()?;
    let report: Report = serde_json::from_slice(&bytes).ok()?;
    live(&report, &path).then_some(report)
}

/// Whether the report is fresh enough to present as news.
///
/// Saturating and fail-closed: a zero heartbeat, a timestamp in the future
/// and a file older than three missed polls all read as silence, never as
/// fresh asks. The mtime cross-check catches a loop that died with a
/// plausible `at_unix` inside.
fn live(report: &Report, path: &Path) -> bool {
    if report.heartbeat_s == 0 {
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
pub fn write_approval(dir: &Path, id: &str, verdict: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static LAST_SEQ: AtomicU64 = AtomicU64::new(0);

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
) -> Vec<(crate::sidebar::Line, Option<Hit>)> {
    // The painter derives the keyboard's own row from the hit, the way it does for the
    // left column, so the text carries no focus mark of its own.
    let _ = focused;
    if height == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.push(header(rows, master));
    out.push(divider());
    let needs = triage(rows, spaces, master, report);
    out.push((
        crate::sidebar::Line {
            text: crate::width::fit(&format!(" needs you {}", needs.len()), MASTER_WIDTH),
            colour: Some(crate::theme::Role::Title),
            accent: None,
            indicator: None,
        },
        None,
    ));
    out.extend(needs);
    out.push(divider());
    // The master's own words, at most three rows: a fourth row of summary is
    // a paragraph, and paragraphs belong on the master tab, not in the
    // margin.
    if let Some(report) = report {
        let said = report.last_report.trim();
        if !said.is_empty() {
            out.push((
                crate::sidebar::Line {
                    text: crate::width::fit(" son rapor", MASTER_WIDTH),
                    colour: Some(crate::theme::Role::Title),
                    accent: None,
                    indicator: None,
                },
                None,
            ));
            for line in said.lines().take(3) {
                out.push((
                    crate::sidebar::Line {
                        text: crate::width::fit(&format!(" {line}"), MASTER_WIDTH),
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
            text: crate::width::fit("[E] master [a] onayla [u] reddet", MASTER_WIDTH),
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
) -> Option<Hit> {
    rows(agents, spaces, master, focused, report, height)
        .into_iter()
        .nth(row)
        .and_then(|(_, hit)| hit)
}

/// The header: the pinned master and its state, or what to do when nothing is pinned.
///
/// A pin whose terminal is gone renders as unpinned, never as someone else's screen:
/// the caller validates against the freshest rows, and this is the backstop.
fn header(
    rows: &[AgentRow],
    master: Option<(u32, Option<u64>)>,
) -> (crate::sidebar::Line, Option<Hit>) {
    let pinned = master.and_then(|(terminal, _)| rows.iter().find(|r| r.terminal == terminal));
    match pinned {
        Some(row) => (
            crate::sidebar::Line {
                text: crate::width::fit(
                    &format!("master {} {}", label(row), row.state),
                    MASTER_WIDTH,
                ),
                colour: crate::sidebar::colour_for(&row.state),
                accent: None,
                indicator: None,
            },
            Some(Hit::Header),
        ),
        None => (
            crate::sidebar::Line {
                text: crate::width::fit("master yok: bir tab pinle", MASTER_WIDTH),
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
fn triage(
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    master: Option<(u32, Option<u64>)>,
    report: Option<&Report>,
) -> Vec<(crate::sidebar::Line, Option<Hit>)> {
    let master_terminal = master.map(|(terminal, _)| terminal);
    let mut out = Vec::new();
    for state in ["blocked", "failed", "done"] {
        for row in rows
            .iter()
            .filter(|r| r.state == state)
            .filter(|r| Some(r.terminal) != master_terminal)
        {
            // Which space this agent lives in, in that space's colour: the number the
            // sidebar paints is not on this row, so the colour lands on the name instead.
            let space = space_name(spaces, row.workspace);
            // The master's summary, when the file named this terminal: its words,
            // cut to the column like everything else.
            let summary = report
                .and_then(|r| r.needs.iter().find(|n| n.terminal == row.terminal))
                .map(|n| n.summary.trim())
                .filter(|s| !s.is_empty())
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            let text = format!(" {space}/{} {}{summary}", label(row), row.state);
            out.push((
                crate::sidebar::Line {
                    text: crate::width::fit(&text, MASTER_WIDTH),
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
fn divider() -> (crate::sidebar::Line, Option<Hit>) {
    (
        crate::sidebar::Line {
            text: "\u{2500}".repeat(MASTER_WIDTH),
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
        for master in [None, Some((3, Some(1))), Some((9, None))] {
            // The structured block, uncut: everything below is a truncation of this.
            let full = rows(&agents, &spaces, master, 4, None, 40).len();
            for height in [1usize, 2, 3, 5, 9, 14, 40] {
                let lines = rows(&agents, &spaces, master, 4, None, height);
                assert_eq!(lines.len(), full.min(height), "height {height}");
                for (line, _) in &lines {
                    assert_eq!(
                        crate::width::width(&line.text),
                        MASTER_WIDTH,
                        "{:?}",
                        line.text
                    );
                }
            }
        }
    }

    #[test]
    fn a_wide_name_is_measured_in_columns_and_not_in_characters() {
        let agents = vec![agent(1, 1, &"\u{1f30d}".repeat(30), "blocked")];
        let spaces = vec![space(1, "app")];
        for (line, _) in rows(&agents, &spaces, None, 1, None, 10) {
            assert_eq!(
                crate::width::width(&line.text),
                MASTER_WIDTH,
                "{:?}",
                line.text
            );
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn the_needs_come_blocked_then_failed_then_done() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 20);
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
        let lines = rows(&agents, &spaces, Some((3, Some(1))), 4, None, 20);
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
            .map(|row| hit(&agents, &spaces, Some((3, Some(1))), 4, None, 20, row))
            .collect();
        assert!(!hits.contains(&Some(Hit::Need(3))), "{hits:?}");
        assert!(hits.contains(&Some(Hit::Need(2))), "{hits:?}");
        assert!(hits.contains(&Some(Hit::Need(1))), "{hits:?}");
    }

    #[test]
    fn without_a_pin_the_header_says_how_to_get_one() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 20);
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
            },
        ];
        let spaces = vec![space(1, "app")];
        let lines = rows(&agents, &spaces, None, 2, None, 20);
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
        let lines = rows(&agents, &spaces, None, 4, None, 20);
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
                    let drawn = rows(&agents, &spaces, master, 4, report, height);
                    for (at, (_, want)) in drawn.iter().enumerate() {
                        assert_eq!(
                            hit(&agents, &spaces, master, 4, report, height, at),
                            *want,
                            "height {height} row {at}"
                        );
                    }
                    assert_eq!(
                        hit(&agents, &spaces, master, 4, report, height, drawn.len() + 3),
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
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30);
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
            assert_eq!(crate::width::width(&line.text), MASTER_WIDTH);
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
        let lines = rows(&agents, &spaces, None, 4, Some(&report), 30);
        for (line, _) in &lines {
            assert_eq!(
                crate::width::width(&line.text),
                MASTER_WIDTH,
                "{:?}",
                line.text
            );
            assert!(!line.text.contains('\u{fffd}'));
        }
    }

    #[test]
    fn without_a_report_the_index_alone_is_shown() {
        let (agents, spaces) = sample();
        let lines = rows(&agents, &spaces, None, 4, None, 30);
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
}

//! What the window shows, worked out from what the daemon said: plain values and functions,
//! tested without a window.
//!
//! The daemon decides every row's status once (`AgentRow::status`); this file only groups,
//! orders, counts and words what it was told. It never classifies a state on its own.

use std::collections::BTreeSet;

use qoop_domain::status::{self, Status};
use qoop_runtime::control::{AgentRow, LinkState, ScheduleInfo, WorkspaceRow};
use qoop_runtime::schedule::{Cron, Naive, Zone, next_slot};

/// What a row shows: the daemon's status, or a sleeping tab (a tab fact, not a status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Status(Status),
    Sleeping,
}

impl Kind {
    pub fn of(row: &AgentRow) -> Kind {
        if row.state == status::SLEEPING || row.slept_at_unix.is_some() {
            return Kind::Sleeping;
        }
        // An older daemon sends no status: shown as unsure rather than worked out here.
        Kind::Status(
            row.status
                .as_deref()
                .and_then(Status::from_token)
                .unwrap_or(Status::Unknown),
        )
    }

    /// Where it sorts: the daemon's rank, a sleeping tab with the unknown ones.
    pub fn rank(self) -> u8 {
        match self {
            Kind::Status(s) => s.rank(),
            Kind::Sleeping => Status::Unknown.rank(),
        }
    }

    /// The word beside the mark, as DECISION's state table writes it: the states that ask
    /// something of a person or say what happened are capitalised, the quiet ones are not.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Status(Status::Blocked) => "Needs you",
            Kind::Status(Status::Failed) => "Failed",
            Kind::Status(Status::Working | Status::Starting) => "Working",
            Kind::Status(Status::Done) => "Done",
            Kind::Status(Status::Parked) => "Parked",
            Kind::Status(s) => s.word(),
            Kind::Sleeping => status::SLEEPING,
        }
    }

    pub fn needs_you(self) -> bool {
        self == Kind::Status(Status::Blocked)
    }
}

/// The dots of a state's mark on the 3x3 grid, `(x, y)` from the top left, as the phone's
/// `marks.ts` and the TUI's sidebar draw them. `frame` is the working mark's breath; `None`
/// is its still picture (reduce motion).
pub fn mark_dots(kind: Kind, frame: Option<u32>) -> Vec<(u8, u8)> {
    const CENTRE: (u8, u8) = (1, 1);
    const ARM: [(u8, u8); 4] = [(1, 0), (2, 1), (1, 2), (0, 1)];
    const RING: [(u8, u8); 8] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (2, 1),
        (2, 2),
        (1, 2),
        (0, 2),
        (0, 1),
    ];
    const DEPTH: [usize; 8] = [1, 1, 2, 3, 4, 4, 3, 2];
    let breath = |f: u32| {
        let f = (f % 32) as usize;
        let mut dots = vec![CENTRE];
        dots.extend((0..DEPTH[f % 8]).map(|i| ARM[(f / 8 + i) % 4]));
        dots
    };
    match kind {
        Kind::Status(Status::Working) => match frame {
            Some(f) => breath(f),
            None => vec![CENTRE, ARM[0], ARM[1]],
        },
        Kind::Status(Status::Starting) => vec![CENTRE, ARM[0], ARM[1]],
        Kind::Status(Status::Blocked) => RING.to_vec(),
        Kind::Status(Status::Failed) => vec![(0, 0), (2, 0), CENTRE, (0, 2), (2, 2)],
        Kind::Status(Status::Done) => (0..3).flat_map(|y| (0..3).map(move |x| (x, y))).collect(),
        Kind::Status(Status::Background) => vec![RING[(frame.unwrap_or(0) % 8) as usize]],
        Kind::Sleeping => vec![(0, 1), CENTRE, (2, 1)],
        Kind::Status(Status::Parked | Status::Idle | Status::Unknown) => vec![CENTRE],
    }
}

/// The filter chips over home's list. Never a space filter: it could hide a space whose
/// agent needs you.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    All,
    NeedsYou,
    Working,
    Done,
    Sleeping,
}

impl Chip {
    pub const ALL: [Chip; 5] = [
        Chip::All,
        Chip::NeedsYou,
        Chip::Working,
        Chip::Done,
        Chip::Sleeping,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Chip::All => "All",
            Chip::NeedsYou => "Needs you",
            Chip::Working => "Working",
            Chip::Done => "Done",
            Chip::Sleeping => "sleeping",
        }
    }

    pub fn holds(self, kind: Kind) -> bool {
        match self {
            Chip::All => true,
            Chip::NeedsYou => kind.needs_you(),
            Chip::Working => matches!(kind, Kind::Status(Status::Working | Status::Starting)),
            Chip::Done => kind == Kind::Status(Status::Done),
            Chip::Sleeping => kind == Kind::Sleeping,
        }
    }

    /// What the list says when the chip holds nothing.
    pub fn empty(self) -> &'static str {
        match self {
            Chip::All => "No agents yet.",
            Chip::NeedsYou => "Nothing needs you.",
            Chip::Working => "Nothing is working.",
            Chip::Done => "Nothing is done.",
            Chip::Sleeping => "Nothing is sleeping.",
        }
    }

    /// The mark the chip wears: the state it filters by.
    pub fn kind(self) -> Option<Kind> {
        match self {
            Chip::All => None,
            Chip::NeedsYou => Some(Kind::Status(Status::Blocked)),
            Chip::Working => Some(Kind::Status(Status::Working)),
            Chip::Done => Some(Kind::Status(Status::Done)),
            Chip::Sleeping => Some(Kind::Sleeping),
        }
    }
}

/// The space qass lives in on this machine, kept off the owner's lists.
pub const QASS_SPACE: &str = "qass";

/// What the daemon (through the mux) last said.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub rows: Vec<AgentRow>,
    pub spaces: Vec<WorkspaceRow>,
    pub schedules: Vec<ScheduleInfo>,
    /// qass's switch as `QassWatch` read it; `None` before the first answer.
    pub qass_watch: Option<bool>,
    pub folded: BTreeSet<i64>,
}

/// One space header and its agents, as home lists them.
pub struct Group<'a> {
    pub space: &'a WorkspaceRow,
    pub rows: Vec<&'a AgentRow>,
    /// All its agents, whatever the chip shows.
    pub count: usize,
}

impl Model {
    fn qass_spaces(&self) -> Vec<i64> {
        self.spaces
            .iter()
            .filter(|w| w.name == QASS_SPACE)
            .map(|w| w.id)
            .collect()
    }

    /// The owner's agents: every agent row outside qass's space, and the sleeping tabs that
    /// held one.
    pub fn agents(&self) -> impl Iterator<Item = &AgentRow> {
        let qass = self.qass_spaces();
        self.rows.iter().filter(move |r| {
            !qass.contains(&r.workspace)
                && (r.agent || (Kind::of(r) == Kind::Sleeping && r.program.is_some()))
        })
    }

    /// Home's groups: spaces in the daemon's order, each with its agents by rank and then in
    /// the daemon's order (oldest first), as the TUI lists them. A space with no agent the
    /// chip holds is left out, and so is an empty one.
    pub fn groups(&self, chip: Chip) -> Vec<Group<'_>> {
        self.spaces
            .iter()
            .filter(|w| w.name != QASS_SPACE)
            .filter_map(|space| {
                let mut all: Vec<&AgentRow> =
                    self.agents().filter(|r| r.workspace == space.id).collect();
                all.sort_by_key(|r| Kind::of(r).rank());
                let count = all.len();
                let rows: Vec<&AgentRow> = all
                    .into_iter()
                    .filter(|r| chip.holds(Kind::of(r)))
                    .collect();
                (!rows.is_empty()).then_some(Group { space, rows, count })
            })
            .collect()
    }

    pub fn count(&self, chip: Chip) -> usize {
        self.agents().filter(|r| chip.holds(Kind::of(r))).count()
    }

    /// The Inbox badge: Needs you plus Failed, nothing at zero.
    pub fn badge(&self) -> usize {
        self.agents()
            .filter(|r| matches!(Kind::of(r), Kind::Status(Status::Blocked | Status::Failed)))
            .count()
    }

    /// "7 in 3 spaces on 2 machines".
    pub fn summary(&self) -> String {
        let agents: Vec<&AgentRow> = self.agents().collect();
        let spaces: BTreeSet<i64> = agents.iter().map(|r| r.workspace).collect();
        let machines: BTreeSet<usize> = spaces.iter().map(|w| machine_of(*w)).collect();
        let n = |count: usize, one: &str, many: &str| {
            format!("{count} {}", if count == 1 { one } else { many })
        };
        format!(
            "{} in {} on {}",
            agents.len(),
            n(spaces.len(), "space", "spaces"),
            n(machines.len(), "machine", "machines")
        )
    }

    pub fn space(&self, id: i64) -> Option<&WorkspaceRow> {
        self.spaces.iter().find(|w| w.id == id)
    }

    pub fn row(&self, terminal: u32) -> Option<&AgentRow> {
        self.rows.iter().find(|r| r.terminal == terminal)
    }

    fn space_name(&self, id: i64) -> String {
        self.space(id).map_or_else(String::new, |w| w.name.clone())
    }

    /// The agent the status line points at: the first that needs you, else the first that
    /// failed, in home's order.
    pub fn next_up(&self) -> Option<&AgentRow> {
        let mut agents: Vec<&AgentRow> = self.agents().collect();
        agents.sort_by_key(|r| Kind::of(r).rank());
        agents
            .into_iter()
            .find(|r| matches!(Kind::of(r), Kind::Status(Status::Blocked | Status::Failed)))
    }

    /// "next: acme-web/claude needs you", or nothing when nothing waits.
    pub fn status_line(&self) -> Option<String> {
        let row = self.next_up()?;
        Some(format!(
            "next: {}/{} {}",
            self.space_name(row.workspace),
            row.program.as_deref().unwrap_or("agent"),
            Kind::of(row).word().to_lowercase()
        ))
    }

    /// What a screen reader says for a row: "README Coupons section, claude, acme-web,
    /// Needs you, 4m".
    pub fn row_label(&self, row: &AgentRow, now: i64) -> String {
        let mut parts = vec![title_of(row)];
        if let Some(program) = &row.program {
            parts.push(program.clone());
        }
        parts.push(self.space_name(row.workspace));
        parts.push(Kind::of(row).word().to_string());
        if let Some(ago) = age_of(row, now) {
            parts.push(ago);
        }
        parts.join(", ")
    }

    /// Whether a phone has any of these agents open now ("iPhone, seen now"). From the rows'
    /// `clients` only: nothing is said otherwise.
    pub fn phone_here(&self) -> bool {
        self.rows
            .iter()
            .any(|r| r.clients.iter().any(|c| c == "phone"))
    }

    /// qass's pill: not running, not watching, or "watching 6 of 7".
    pub fn qass_line(&self) -> String {
        let running = self
            .rows
            .iter()
            .any(|r| self.qass_spaces().contains(&r.workspace) && r.agent);
        if !running {
            return "not running".into();
        }
        if self.qass_watch == Some(false) {
            return "not watching".into();
        }
        let agents: Vec<&AgentRow> = self
            .agents()
            .filter(|r| Kind::of(r) != Kind::Sleeping)
            .collect();
        let watched = agents.iter().filter(|r| !r.qass_skip).count();
        format!("watching {watched} of {}", agents.len())
    }

    /// The finish a person looks at when `terminal` is on screen: its turn end, while its
    /// row says Done.
    pub fn finish(&self, terminal: u32) -> Option<(u32, &str)> {
        let row = self.row(terminal)?;
        (Kind::of(row) == Kind::Status(Status::Done))
            .then_some(())
            .and(row.stop.as_deref())
            .map(|stop| (terminal, stop))
    }
}

/// A row's title: the name a person gave it, else its conversation's, else its program.
pub fn title_of(row: &AgentRow) -> String {
    [
        row.name.as_deref(),
        row.agent_title.as_deref(),
        row.program.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|s| !s.is_empty())
    .unwrap_or("shell")
    .to_string()
}

/// A row's second line.
pub fn sub_of(row: &AgentRow) -> String {
    match Kind::of(row) {
        Kind::Status(Status::Blocked) => "in Inbox".into(),
        Kind::Sleeping => "Wakes on a click".into(),
        _ => match (&row.task, &row.activity) {
            (Some(task), _) => format!("Task #{}: {}", task.id, task.goal),
            (None, Some(activity)) => activity.clone(),
            (None, None) => row.cwd.clone().map(|c| tilde(&c)).unwrap_or_default(),
        },
    }
}

/// How long the row has been as it is: "32s", "4m", "2h", "3d".
pub fn age_of(row: &AgentRow, now: i64) -> Option<String> {
    let since = row.slept_at_unix.or(row.last_active_at_unix)?;
    Some(short_span(now.saturating_sub(since).max(0)))
}

pub fn short_span(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// A path with the home folder as `~`.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", &path[home.len()..])
        }
        _ => path.to_string(),
    }
}

/// Which machine a space is on: zero for this one (the mux's numbering).
pub fn machine_of(workspace: i64) -> usize {
    qoop_tui::mux::machine_of(workspace)
}

/// The machine a space is on, as a person names it.
pub fn machine_name(space: &WorkspaceRow) -> String {
    if machine_of(space.id) == 0 {
        return "This Mac".into();
    }
    space
        .name
        .split_once(':')
        .map_or_else(|| space.name.clone(), |(machine, _)| machine.to_string())
}

/// A space's connection as its tile says it.
pub fn link_word(space: &WorkspaceRow) -> &'static str {
    match space.link {
        None | Some(LinkState::Up) => "connected",
        Some(LinkState::Parked) => "parked",
        Some(LinkState::Down) => "retrying",
    }
}

/// Which of the six space inks a space wears: the one picked for it, else its number's.
pub fn space_slot(space: &WorkspaceRow) -> usize {
    match space.ink {
        Some(ink) if ink < 6 => ink as usize,
        _ => (space.id & ((1 << 40) - 1)).rem_euclid(6) as usize,
    }
}

/// The fixed pet seed of each agent kind (`AgentRow::program`), dealt by
/// `qoop_runtime::avatar::Deal`. A program not listed has no pet.
pub const KIND_SEEDS: [(&str, u64); 4] = [
    ("claude", 10),
    ("codex", 15),
    ("opencode", 3),
    ("gemini", 6),
];

pub fn kind_pet(program: Option<&str>) -> Option<qoop_runtime::avatar::Avatar> {
    let program = program?;
    let (_, seed) = KIND_SEEDS.iter().find(|(kind, _)| *kind == program)?;
    Some(qoop_runtime::avatar::Deal::new(*seed).pet())
}

// New work: the line typed into a new shell in the space's folder, ported from the phone's
// `worktree.ts` so both start an agent the same way and the CLI does every check.

/// The longest first prompt typed this way.
pub const PROMPT_MOST: usize = 4000;

/// A word the shell reads back as it is.
pub fn shell_quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The prompt as typed: control characters to spaces, trimmed, cut at [`PROMPT_MOST`].
pub fn clean_prompt(prompt: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in prompt.chars() {
        if c.is_control() {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out.trim()
        .chars()
        .take(PROMPT_MOST)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A branch named after the prompt's first words: `feat/fix-the-login-page`. Letters with
/// marks keep their base letter, so `giriş sayfasını düzelt` is `feat/giris-sayfasini-duzelt`.
pub fn branch_for(prompt: &str) -> String {
    let plain: String = prompt
        .chars()
        .filter(|c| *c != '\'' && *c != '\u{2019}')
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'İ' | 'í' | 'ì' | 'î' | 'ï' => 'i',
            'ö' | 'Ö' | 'ó' | 'ò' | 'ô' | 'õ' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' | 'ú' | 'ù' | 'û' => 'u',
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'Ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ñ' => 'n',
            'ß' => 's',
            c => c.to_ascii_lowercase(),
        })
        .collect();
    let words: Vec<&str> = plain
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        format!("feat/{}", words.join("-"))
    }
}

/// What is wrong with a branch name the CLI would take as something else.
pub fn branch_problem(branch: &str) -> Option<&'static str> {
    if branch.is_empty() {
        Some("Name the branch.")
    } else if branch.starts_with('-') {
        Some("A branch cannot start with -.")
    } else if branch.contains("@{") {
        Some("A branch cannot hold @{.")
    } else if branch.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("A branch has no spaces.")
    } else {
        None
    }
}

fn program_of(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or("")
}

/// A launcher typed as it is: plain words, nothing the shell reads as more than words.
fn plain(command: &str) -> bool {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c);
    let trimmed = command.trim();
    !trimmed.is_empty()
        && !trimmed.contains("  ")
        && trimmed
            .split(' ')
            .all(|w| !w.is_empty() && w.chars().all(safe))
}

/// How an agent takes its first prompt on its command line.
fn prompted(program: &str, prompt: &str) -> Option<Vec<String>> {
    match program {
        "claude" | "codex" => Some(vec![prompt.to_string()]),
        "opencode" => Some(vec!["--prompt".into(), prompt.to_string()]),
        _ => None,
    }
}

/// The launchers that can start in a worktree with a prompt, by name and command.
pub fn worktree_agents(offers: &[(String, String)]) -> Vec<(String, String)> {
    offers
        .iter()
        .filter(|(_, command)| plain(command) && prompted(program_of(command), "").is_some())
        .cloned()
        .collect()
}

/// The agent's command with its first prompt on it, or why it cannot take one.
pub fn prompted_line(command: &str, prompt: &str) -> Result<String, &'static str> {
    let said = clean_prompt(prompt);
    if said.is_empty() {
        return Err("Say what the agent should do.");
    }
    if said.starts_with('-') {
        return Err("The prompt cannot start with -.");
    }
    let words = plain(command)
        .then(|| prompted(program_of(command), &said))
        .flatten()
        .ok_or("That agent cannot start this way.")?;
    let mut line = vec![command.trim().to_string()];
    line.extend(words.iter().map(|w| shell_quote(w)));
    Ok(line.join(" "))
}

/// The line typed into the new shell, or why there is none. `space` is the space's number in
/// the daemon of the machine the shell runs on, as the CLI there reads it.
pub fn worktree_line(
    branch: &str,
    space: Option<i64>,
    command: &str,
    prompt: &str,
) -> Result<String, &'static str> {
    if let Some(problem) = branch_problem(branch) {
        return Err(problem);
    }
    let agent = prompted_line(command, prompt)?;
    let space = space.map_or_else(String::new, |s| format!(" --space {s}"));
    Ok(format!(
        "qoop worktree new {}{space} -- {agent} && exit",
        shell_quote(branch)
    ))
}

/// A space's number in its own machine's daemon: the mux numbers a remote one above 2^40.
pub fn daemon_space(workspace: i64) -> i64 {
    workspace & ((1 << 40) - 1)
}

/// One line of home's Next runs card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextRun {
    pub at: i64,
    pub name: String,
    pub agent: String,
    pub space: Option<String>,
    pub when: String,
    /// Started and its tab still works: shown above the "now" rule.
    pub running: Option<i64>,
}

/// The coming runs of this machine's schedules, soonest first, at most `most`: each
/// schedule's `next_at` and the cron slots after it, plus the runs going now.
pub fn next_runs(
    schedules: &[ScheduleInfo],
    rows: &[AgentRow],
    zone: Zone,
    most: usize,
) -> Vec<NextRun> {
    let line = |s: &ScheduleInfo, at: i64, running: Option<i64>| NextRun {
        at,
        name: s.name.clone(),
        agent: s.agent.clone(),
        space: s.space.clone(),
        when: s.when.clone(),
        running,
    };
    let mut out = Vec::new();
    for s in schedules {
        let working = s.last_terminal.and_then(|t| {
            rows.iter().find(|r| {
                i64::from(r.terminal) == t && Kind::of(r) == Kind::Status(Status::Working)
            })
        });
        if s.last_result.as_deref() == Some("started")
            && let (Some(at), Some(_)) = (s.last_at, working)
        {
            out.push(line(s, at, s.last_terminal));
        }
        if s.paused {
            continue;
        }
        let Some(mut at) = s.next_at else { continue };
        let cron = s.cron.as_deref().and_then(|c| Cron::parse(c).ok());
        for _ in 0..most {
            out.push(line(s, at, None));
            match cron.as_ref().and_then(|c| next_slot(c, at, zone)) {
                Some(next) => at = next,
                None => break,
            }
        }
    }
    out.sort_by_key(|r| (r.running.is_none(), r.at));
    let running = out.iter().filter(|r| r.running.is_some()).count();
    out.truncate(running + most);
    out
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// "16:05" today, "Mon 02:00" on another day.
pub fn clock_at(at: i64, now: i64, zone: Zone) -> String {
    let wall = (zone.local)(at);
    let today = (zone.local)(now);
    let day = |n: Naive| n.secs().div_euclid(86_400);
    if day(wall) == day(today) {
        wall.clock()
    } else {
        format!(
            "{} {}",
            DAYS[(day(wall) + 4).rem_euclid(7) as usize],
            wall.clock()
        )
    }
}

/// "in 3 m", "in 1 h 58 m", "in 1 d 10 h".
pub fn until(at: i64, now: i64) -> String {
    let s = (at - now).max(0);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("in {} m", m.max(1)),
        (0, h) if m > 0 => format!("in {h} h {m} m"),
        (0, h) => format!("in {h} h"),
        (d, h) => format!("in {d} d {h} h"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A zone at UTC, so tests never depend on the machine's.
    pub fn utc() -> Zone {
        Zone {
            local: Naive::from_secs,
            unix: Naive::secs,
        }
    }

    pub fn row(
        terminal: u32,
        workspace: i64,
        status: &str,
        program: &str,
        title: &str,
    ) -> AgentRow {
        let mut row: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": terminal,
            "workspace": workspace,
            "program": program,
            "behind": null,
            "state": status,
            "generation": 1,
            "agent": true,
        }))
        .expect("row");
        row.status = Some(status.to_string());
        row.agent_title = Some(title.to_string());
        row
    }

    pub fn space(id: i64, name: &str) -> WorkspaceRow {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "terminals": 1}))
            .expect("space")
    }

    /// The design's data set: seven agents in three spaces on two machines.
    pub fn fixture() -> Model {
        let remote = 1i64 << 40 | 1;
        let mut sleeping = row(
            0x8100_0002,
            remote,
            "unknown",
            "claude",
            "Test suite status",
        );
        sleeping.state = "sleeping".into();
        sleeping.status = None;
        sleeping.agent = false;
        sleeping.slept_at_unix = Some(1000);
        Model {
            rows: vec![
                row(1, 1, "working", "claude", "Bun test suite status"),
                row(2, 1, "blocked", "claude", "README Coupons section"),
                row(3, 1, "done", "claude", "Coupon.ts test suggestion"),
                row(4, 2, "done", "claude", "CHANGELOG.md summary"),
                row(5, 2, "working", "codex", "POST /orders route in routes.ts"),
                row(0x8100_0001, remote, "working", "codex", "Bun run test:soak"),
                sleeping,
                row(9, 3, "working", "claude", "qass"),
            ],
            spaces: vec![
                space(1, "acme-web"),
                space(2, "api"),
                space(3, "qass"),
                space(remote, "staging:billing-api"),
            ],
            ..Model::default()
        }
    }

    #[test]
    fn home_groups_by_space_and_orders_by_rank_then_oldest() {
        let m = fixture();
        let groups = m.groups(Chip::All);
        let names: Vec<&str> = groups.iter().map(|g| g.space.name.as_str()).collect();
        assert_eq!(
            names,
            ["acme-web", "api", "staging:billing-api"],
            "qass's space is left out"
        );
        let titles: Vec<String> = groups[0].rows.iter().map(|r| title_of(r)).collect();
        assert_eq!(
            titles,
            [
                "README Coupons section",
                "Coupon.ts test suggestion",
                "Bun test suite status"
            ]
        );
        let api: Vec<u32> = groups[1].rows.iter().map(|r| r.terminal).collect();
        assert_eq!(api, [4, 5], "done before working");
        assert_eq!(groups[2].rows.len(), 2, "the sleeping tab is listed");
    }

    #[test]
    fn chips_count_and_filter_and_say_when_empty() {
        let m = fixture();
        assert_eq!(m.count(Chip::All), 7);
        assert_eq!(m.count(Chip::NeedsYou), 1);
        assert_eq!(m.count(Chip::Working), 3);
        assert_eq!(m.count(Chip::Done), 2);
        assert_eq!(m.count(Chip::Sleeping), 1);
        let working = m.groups(Chip::Working);
        assert_eq!(working.len(), 3);
        assert_eq!(
            working[0].count, 3,
            "the header counts every agent of the space"
        );
        let mut quiet = m.clone();
        quiet
            .rows
            .retain(|r| r.status.as_deref() != Some("working"));
        assert!(quiet.groups(Chip::Working).is_empty());
        assert_eq!(Chip::Working.empty(), "Nothing is working.");
    }

    #[test]
    fn badge_is_needs_you_plus_failed() {
        let mut m = fixture();
        assert_eq!(m.badge(), 1);
        m.rows[0].status = Some("failed".into());
        assert_eq!(m.badge(), 2);
        m.rows
            .retain(|r| !matches!(r.status.as_deref(), Some("failed" | "blocked")));
        assert_eq!(m.badge(), 0);
    }

    #[test]
    fn the_summary_counts_agents_spaces_and_machines() {
        let m = fixture();
        assert_eq!(m.summary(), "7 in 3 spaces on 2 machines");
        let one = Model {
            rows: vec![row(1, 1, "working", "claude", "x")],
            spaces: vec![space(1, "a")],
            ..Model::default()
        };
        assert_eq!(one.summary(), "1 in 1 space on 1 machine");
    }

    #[test]
    fn the_status_line_and_row_labels_say_the_state_words() {
        let mut m = fixture();
        assert_eq!(
            m.status_line().as_deref(),
            Some("next: acme-web/claude needs you")
        );
        let blocked = m.row(2).expect("row").clone();
        let mut b = blocked.clone();
        b.last_active_at_unix = Some(1000);
        assert_eq!(
            m.row_label(&b, 1240),
            "README Coupons section, claude, acme-web, Needs you, 4m"
        );
        m.rows.retain(|r| r.terminal != 2);
        assert_eq!(m.status_line(), None);
    }

    /// The state word table: never "blocked" anywhere a person reads.
    #[test]
    fn no_visible_word_says_blocked() {
        let mut kinds: Vec<Kind> = status::ALL.into_iter().map(Kind::Status).collect();
        kinds.push(Kind::Sleeping);
        let mut words: Vec<String> = kinds.iter().map(|k| k.word().to_string()).collect();
        words.extend(
            Chip::ALL
                .iter()
                .flat_map(|c| [c.label().to_string(), c.empty().to_string()]),
        );
        let m = fixture();
        words.push(m.status_line().unwrap_or_default());
        words.push(m.qass_line());
        for row in &m.rows {
            words.push(sub_of(row));
            words.push(m.row_label(row, 2000));
        }
        for word in words {
            assert!(!word.to_lowercase().contains("blocked"), "{word}");
            assert!(!word.to_lowercase().contains("asleep"), "{word}");
        }
    }

    #[test]
    fn marks_are_the_phone_pictures() {
        assert_eq!(mark_dots(Kind::Status(Status::Blocked), None).len(), 8);
        assert_eq!(mark_dots(Kind::Status(Status::Done), None).len(), 9);
        assert_eq!(mark_dots(Kind::Status(Status::Failed), None).len(), 5);
        assert_eq!(mark_dots(Kind::Sleeping, None), [(0, 1), (1, 1), (2, 1)]);
        assert_eq!(
            mark_dots(Kind::Status(Status::Working), None).len(),
            3,
            "still breath"
        );
        assert_eq!(mark_dots(Kind::Status(Status::Working), Some(4)).len(), 5);
    }

    #[test]
    fn qass_pill_says_running_watching_and_the_count() {
        let mut m = fixture();
        assert_eq!(m.qass_line(), "watching 6 of 6");
        m.rows[0].qass_skip = true;
        assert_eq!(m.qass_line(), "watching 5 of 6");
        m.qass_watch = Some(false);
        assert_eq!(m.qass_line(), "not watching");
        m.rows.retain(|r| r.workspace != 3);
        assert_eq!(m.qass_line(), "not running");
    }

    #[test]
    fn each_agent_kind_has_its_own_pet() {
        let pets: Vec<_> = KIND_SEEDS
            .iter()
            .map(|(kind, _)| kind_pet(Some(kind)).expect("pet"))
            .collect();
        for (i, a) in pets.iter().enumerate() {
            assert!(a.valid());
            for b in &pets[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(kind_pet(Some("vim")), None);
        assert_eq!(kind_pet(Some("claude")), kind_pet(Some("claude")), "fixed");
    }

    #[test]
    fn branch_comes_from_the_first_words_turkish_included() {
        assert_eq!(
            branch_for("giriş sayfasını düzelt"),
            "feat/giris-sayfasini-duzelt"
        );
        assert_eq!(branch_for("İstanbul ÇĞÖŞÜ ı"), "feat/istanbul-cgosu-i");
        assert_eq!(
            branch_for("Fix the login page, it 500s on Safari"),
            "feat/fix-the-login-page-it"
        );
        assert_eq!(
            branch_for("Don't break it's login"),
            "feat/dont-break-its-login"
        );
        assert_eq!(branch_for("Don\u{2019}t stop"), "feat/dont-stop");
        assert_eq!(branch_for("!!!"), "");
        assert_eq!(branch_problem("feat/x"), None);
        assert_eq!(branch_problem("-b"), Some("A branch cannot start with -."));
        assert_eq!(branch_problem("a@{1}"), Some("A branch cannot hold @{."));
        assert_eq!(branch_problem("a b"), Some("A branch has no spaces."));
        assert_eq!(branch_problem("feat/a\nb"), Some("A branch has no spaces."));
        assert_eq!(branch_problem(""), Some("Name the branch."));
    }

    /// What a POSIX shell makes of a typed line: its words.
    fn words(line: &str) -> Vec<String> {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s\\0' {line}"))
            .output()
            .expect("sh");
        let text = String::from_utf8(out.stdout).expect("utf8");
        let mut words: Vec<String> = text.split('\0').map(str::to_string).collect();
        words.pop();
        words
    }

    #[test]
    fn shell_quote_gives_every_word_back_byte_for_byte() {
        assert_eq!(shell_quote("feat/x-1"), "feat/x-1");
        assert_eq!(shell_quote(""), "''");
        for w in [
            "it's",
            "say \"hi\"",
            "$HOME `id` $(id)",
            "a\\b",
            "*",
            "a b",
            "!x",
            "; rm -rf ~",
            "düzelt",
        ] {
            assert_eq!(words(&shell_quote(w)), [w]);
        }
    }

    #[test]
    fn the_prompt_loses_control_characters_and_is_cut() {
        assert_eq!(
            clean_prompt(" fix\nthe\r\nlogin\x1b[2J page\t"),
            "fix the login [2J page"
        );
        assert_eq!(clean_prompt(&"é".repeat(5000)).chars().count(), PROMPT_MOST);
    }

    #[test]
    fn the_typed_line_starts_the_agent_in_a_worktree() {
        let prompt = "fix it's \"quoted\"\n$HOME `id` now";
        let line = worktree_line("feat/fix", Some(3), "claude", prompt).expect("line");
        assert_eq!(
            line,
            "qoop worktree new feat/fix --space 3 -- claude 'fix it'\\''s \"quoted\" $HOME `id` now' && exit"
        );
        assert_eq!(
            words(line.trim_end_matches(" && exit")),
            [
                "qoop",
                "worktree",
                "new",
                "feat/fix",
                "--space",
                "3",
                "--",
                "claude",
                "fix it's \"quoted\" $HOME `id` now"
            ]
        );
        assert_eq!(
            worktree_line("b", None, "opencode", "go").as_deref(),
            Ok("qoop worktree new b -- opencode --prompt go && exit")
        );
        assert_eq!(
            worktree_line("-x", None, "claude", "go"),
            Err("A branch cannot start with -.")
        );
        assert_eq!(
            worktree_line("b", None, "claude", "--help"),
            Err("The prompt cannot start with -.")
        );
        assert_eq!(
            worktree_line("b", None, "claude", " \n "),
            Err("Say what the agent should do.")
        );
        assert_eq!(
            worktree_line("b", None, "claude; rm -rf ~", "go"),
            Err("That agent cannot start this way.")
        );
        let offered = worktree_agents(&[
            ("claude".into(), "claude".into()),
            ("fast".into(), "claude --model opus".into()),
            ("cx".into(), "/opt/homebrew/bin/codex".into()),
            ("env".into(), "FOO=1 claude".into()),
            ("piped".into(), "claude | tee log".into()),
            ("aider".into(), "aider".into()),
        ]);
        let names: Vec<&str> = offered.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["claude", "fast", "cx"]);
    }

    fn schedule(name: &str, cron: Option<&str>, next_at: Option<i64>) -> ScheduleInfo {
        serde_json::from_value(serde_json::json!({
            "id": 1, "name": name, "cron": cron, "agent": "claude", "cwd": "/tmp",
            "space": "acme-web", "prompt": "go", "paused": false, "created_at": 0,
            "next_at": next_at, "when": "every 2h",
        }))
        .expect("schedule")
    }

    #[test]
    fn next_runs_loop_the_cron_and_put_a_running_one_first() {
        let now = 16 * 3600 + 2 * 60;
        let mut soak = schedule("soak-staging", Some("0 */2 * * *"), Some(18 * 3600));
        soak.last_result = Some("started".into());
        soak.last_at = Some(16 * 3600);
        soak.last_terminal = Some(1);
        let watch = schedule("new-issues", None, Some(now + 180));
        let mut paused = schedule("deps-weekly", Some("0 9 * * 1"), Some(now + 3600));
        paused.paused = true;
        let m = fixture();
        let runs = next_runs(&[soak, watch, paused], &m.rows, utc(), 4);
        let seen: Vec<(i64, &str, bool)> = runs
            .iter()
            .map(|r| (r.at, r.name.as_str(), r.running.is_some()))
            .collect();
        assert_eq!(
            seen,
            [
                (16 * 3600, "soak-staging", true),
                (now + 180, "new-issues", false),
                (18 * 3600, "soak-staging", false),
                (20 * 3600, "soak-staging", false),
                (22 * 3600, "soak-staging", false),
            ]
        );
        assert_eq!(clock_at(now + 180, now, utc()), "16:05");
        assert_eq!(clock_at(now + 3 * 86_400, now, utc()), "Sun 16:02");
        assert_eq!(until(now + 180, now), "in 3 m");
        assert_eq!(until(now + 7080, now), "in 1 h 58 m");
        assert_eq!(until(now + 124_200, now), "in 1 d 10 h");
    }
}

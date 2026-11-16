//! What the window shows, worked out from what the daemon said: plain values and functions,
//! tested without a window.
//!
//! The daemon decides every row's status once (`AgentRow::status`); this file only groups,
//! orders, counts and words what it was told. It never classifies a state on its own.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use qoop_domain::shell::quote as shell_quote;
use qoop_domain::status::{self, Status};
use qoop_runtime::control::{
    AgentMenu, AgentRow, ItemCardInfo, LinkState, Pane, ScheduleInfo, ScheduleRun, SleepingTab,
    StepInfo, TaskEventInfo, TaskInfo, TerminalStream, WorkspaceRow,
};
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
    /// The tabs split into panes, as the daemon keeps them (`Agents::splits`).
    pub splits: Vec<Pane>,
}

/// One line of home's lists: an awake agent's row, or a sleeping tab that held an agent.
#[derive(Clone, Copy, Debug)]
pub enum Entry<'a> {
    Live(&'a AgentRow),
    /// A sleeping tab as a current daemon lists it, under its space (the second field).
    Asleep(&'a SleepingTab, i64),
}

impl<'a> Entry<'a> {
    pub fn kind(self) -> Kind {
        match self {
            Entry::Live(row) => Kind::of(row),
            Entry::Asleep(..) => Kind::Sleeping,
        }
    }

    pub fn terminal(self) -> u32 {
        match self {
            Entry::Live(row) => row.terminal,
            Entry::Asleep(tab, _) => tab.terminal,
        }
    }

    pub fn workspace(self) -> i64 {
        match self {
            Entry::Live(row) => row.workspace,
            Entry::Asleep(_, space) => space,
        }
    }

    /// The agent in it: the program of an awake row, the agent a sleeping tab resumes.
    pub fn program(self) -> Option<&'a str> {
        match self {
            Entry::Live(row) => row.program.as_deref(),
            Entry::Asleep(tab, _) => tab.agent.as_deref(),
        }
    }

    pub fn title(self) -> String {
        match self {
            Entry::Live(row) => title_of(row),
            Entry::Asleep(tab, _) => tab.label().to_string(),
        }
    }

    pub fn sub(self) -> String {
        match self {
            Entry::Live(row) => sub_of(row),
            Entry::Asleep(..) => SLEEPING_SUB.into(),
        }
    }

    /// How long it has been as it is; a sleeping tab's list carries no time.
    pub fn age(self, now: i64) -> Option<String> {
        match self {
            Entry::Live(row) => age_of(row, now),
            Entry::Asleep(..) => None,
        }
    }
}

/// One space header and its agents, as home lists them.
pub struct Group<'a> {
    pub space: &'a WorkspaceRow,
    pub rows: Vec<Entry<'a>>,
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

    /// The owner's awake agents: every agent row outside qass's space (and the sleeping row
    /// an older daemon sends for a tab that held one).
    pub fn live(&self) -> impl Iterator<Item = &AgentRow> {
        let qass = self.qass_spaces();
        self.rows.iter().filter(move |r| {
            !qass.contains(&r.workspace)
                && (r.agent || (Kind::of(r) == Kind::Sleeping && r.program.is_some()))
        })
    }

    /// Every agent of the owner: the awake ones, then the sleeping tabs that held an agent,
    /// one per tab, as the spaces list them (`WorkspaceRow::sleeping`, as the TUI reads them).
    pub fn agents(&self) -> Vec<Entry<'_>> {
        let mut out: Vec<Entry<'_>> = self.live().map(Entry::Live).collect();
        let qass = self.qass_spaces();
        for space in self.spaces.iter().filter(|w| !qass.contains(&w.id)) {
            for tab in &space.sleeping {
                let lead = tab.lead.is_none_or(|lead| lead == tab.terminal);
                if tab.agent.is_some() && lead && !out.iter().any(|e| e.terminal() == tab.terminal)
                {
                    out.push(Entry::Asleep(tab, space.id));
                }
            }
        }
        out
    }

    /// Home's groups: spaces in the daemon's order, each with its agents by rank and then in
    /// the daemon's order (oldest first), as the TUI lists them. A space with no agent the
    /// chip holds is left out, and so is an empty one.
    pub fn groups(&self, chip: Chip) -> Vec<Group<'_>> {
        let agents = self.agents();
        self.spaces
            .iter()
            .filter(|w| w.name != QASS_SPACE)
            .filter_map(|space| {
                let mut all: Vec<Entry<'_>> = agents
                    .iter()
                    .copied()
                    .filter(|e| e.workspace() == space.id)
                    .collect();
                all.sort_by_key(|e| e.kind().rank());
                let count = all.len();
                all.retain(|e| chip.holds(e.kind()));
                (!all.is_empty()).then_some(Group {
                    space,
                    rows: all,
                    count,
                })
            })
            .collect()
    }

    pub fn count(&self, chip: Chip) -> usize {
        self.agents()
            .iter()
            .filter(|e| chip.holds(e.kind()))
            .count()
    }

    /// The Inbox badge: Needs you plus Failed, nothing at zero.
    pub fn badge(&self) -> usize {
        self.live()
            .filter(|r| matches!(Kind::of(r), Kind::Status(Status::Blocked | Status::Failed)))
            .count()
    }

    /// "7 in 3 spaces on 2 machines".
    pub fn summary(&self) -> String {
        let agents = self.agents();
        let spaces: BTreeSet<i64> = agents.iter().map(|e| e.workspace()).collect();
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
        let mut agents: Vec<&AgentRow> = self.live().collect();
        agents.sort_by_key(|r| Kind::of(r).rank());
        agents
            .into_iter()
            .find(|r| matches!(Kind::of(r), Kind::Status(Status::Blocked | Status::Failed)))
    }

    /// "next: acme-web/review-pr needs you", or nothing when nothing waits. By the agent's
    /// own title: two claude tabs in one space would read the same by their program.
    pub fn status_line(&self) -> Option<String> {
        let row = self.next_up()?;
        Some(format!(
            "next: {}/{} {}",
            self.space_name(row.workspace),
            title_of(row),
            Kind::of(row).word().to_lowercase()
        ))
    }

    /// What a screen reader says for a row: "README Coupons section, claude, acme-web,
    /// Needs you, 4m".
    pub fn row_label(&self, entry: Entry<'_>, now: i64) -> String {
        let mut parts = vec![entry.title()];
        if let Some(program) = entry.program() {
            parts.push(program.to_string());
        }
        parts.push(self.space_name(entry.workspace()));
        parts.push(entry.kind().word().to_string());
        if let Some(ago) = entry.age(now) {
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
            .live()
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

    /// The panes `terminal` is shown with: its tab's split tree as the daemon keeps it, or
    /// the terminal alone.
    pub fn tree_of(&self, terminal: u32) -> Pane {
        self.splits
            .iter()
            .find(|tree| tree.leaves().contains(&terminal))
            .cloned()
            .unwrap_or(Pane::Leaf { terminal })
    }

    /// A space's tabs in the daemon's order: each of its terminals, the other panes of a split
    /// folded into its first.
    pub fn tabs_of(&self, workspace: i64) -> Vec<&AgentRow> {
        let inner: BTreeSet<u32> = self
            .splits
            .iter()
            .flat_map(|tree| tree.leaves().into_iter().skip(1))
            .collect();
        self.rows
            .iter()
            .filter(|r| r.workspace == workspace && !inner.contains(&r.terminal))
            .collect()
    }

    /// The agent a corner note is for: one of the owner's that was listed in `before` and has
    /// turned Needs you since, outside the panes on screen (`shown`). A row that was not listed
    /// (a remote back, the first list) is no turn.
    pub fn turned_needs(&self, before: &[AgentRow], shown: &[u32]) -> Option<u32> {
        let was_calm = |t: u32| {
            before
                .iter()
                .any(|r| r.terminal == t && !Kind::of(r).needs_you())
        };
        self.live()
            .filter(|r| !shown.contains(&r.terminal) && Kind::of(r).needs_you())
            .map(|r| r.terminal)
            .find(|&t| was_calm(t))
    }
}

/// The task strip of the tab on screen, from the `Tasks` answer to `TaskRead { terminal }`: its
/// open task, the plan steps of the item it works on, and its events oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Strip {
    pub task: Option<TaskInfo>,
    pub steps: Vec<StepInfo>,
    pub events: Vec<TaskEventInfo>,
}

/// One evidence line: what was checked and whether it read as a failing run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub at: i64,
    pub text: String,
    pub failed: bool,
}

impl Strip {
    pub fn of(
        tasks: Vec<TaskInfo>,
        mut events: Vec<TaskEventInfo>,
        item: Option<Box<ItemCardInfo>>,
    ) -> Strip {
        events.reverse();
        Strip {
            task: tasks.into_iter().next(),
            steps: item.map(|card| card.steps).unwrap_or_default(),
            events,
        }
    }

    pub fn done(&self) -> usize {
        self.steps.iter().filter(|s| s.done_at.is_some()).count()
    }

    /// The step being worked on: the first one not done.
    pub fn current(&self) -> Option<usize> {
        self.steps.iter().position(|s| s.done_at.is_none())
    }

    /// "Task #12", "2 of 4 done" (or the stage while there is no plan).
    pub fn heading(&self) -> Option<(String, String)> {
        let task = self.task.as_ref()?;
        let progress = if self.steps.is_empty() {
            task.stage.clone()
        } else {
            format!("{} of {} done", self.done(), self.steps.len())
        };
        Some((format!("Task #{}", task.id), progress))
    }

    /// The evidence as recorded, oldest first. A failing run then a passing one shows as both
    /// only when both were recorded: nothing is inferred from a single line.
    pub fn evidence(&self) -> Vec<Evidence> {
        self.events
            .iter()
            .filter(|e| e.kind == "evidence")
            .map(|e| Evidence {
                at: e.at,
                text: e.body.clone(),
                failed: failed_run(&e.body),
            })
            .collect()
    }
}

/// Whether a check's words say it failed: a count of failures above zero (`12 pass, 1 fail`),
/// or one of a few exact phrases (`build failed`). Nothing else is read as a verdict: `the
/// failing test`, `no failures` and `0 failed` say nothing failed.
pub fn failed_run(text: &str) -> bool {
    const PHRASES: [&str; 5] = [
        "build failed",
        "test failed",
        "tests failed",
        "check failed",
        "checks failed",
    ];
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let counted = words.windows(2).any(|pair| {
        pair[0].parse::<u64>().is_ok_and(|n| n > 0)
            && matches!(
                pair[1].as_str(),
                "fail" | "fails" | "failed" | "failing" | "failure" | "failures"
            )
    });
    counted || PHRASES.iter().any(|p| words.join(" ").contains(p))
}

/// The text of the cells from `a` to `b` (row, column, either order, both included) of a
/// screen's rows: whole rows between, each row's trailing blanks dropped, rows joined by a
/// newline. ponytail: a wide character's spacer cell is copied as the space it holds.
pub fn selected_text(lines: &[String], a: (usize, usize), b: (usize, usize)) -> String {
    let (from, to) = if a <= b { (a, b) } else { (b, a) };
    (from.0..=to.0)
        .filter_map(|row| {
            let line = lines.get(row)?;
            let start = if row == from.0 { from.1 } else { 0 };
            let end = if row == to.0 { to.1 + 1 } else { usize::MAX };
            let cut: String = line
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect();
            Some(cut.trim_end().to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the window types for "Explain this in more detail", pinned to the daemon's copy by a
/// test (`qoop_runtime::menu::EXPLAIN`).
pub const EXPLAIN: &str = "Explain this in more detail before I decide: what it will do, why, and \
                           what it changes. Then ask me again.";

/// How long a pick waits before it is sent, for Undo.
pub const HOLD: Duration = Duration::from_secs(3);

/// A menu younger than this takes no digit: a key meant for the screen before it.
pub const ARMING: Duration = Duration::from_millis(500);

/// What a pick sends once its hold is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// One of the agent's own options, by its key (`AnswerMenu`).
    Key(String),
    /// Words typed through the option `key`, a `tell` or `input` one (`AnswerMenuText`):
    /// Explain, or the person's own answer.
    Text { key: String, text: String },
    /// The question handed to qass (`HandToQass`).
    Qass,
    /// An option of one of qass's cards (`AskWrite` `Answer`), which qass types to the agent.
    Card { card: String, option: String },
}

/// What a pick is about: an agent's menu, or one of qass's cards.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Agent(u32),
    Card(String),
}

/// One answer the question card offers: its digit (none for a standing permission, which is
/// click only), its words and what it sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub digit: Option<u8>,
    pub label: String,
    pub detail: Option<String>,
    pub pick: Pick,
}

/// Whether an option grants a standing permission: Claude Code's "Yes, and always allow
/// access to src/" and "Yes, and don't ask again for cargo test commands in /repo", Codex's
/// "Yes, and don't ask again for this command". Click only: no digit to press by habit.
pub fn standing(label: &str) -> bool {
    let words = label.to_lowercase().replace('\u{2019}', "'");
    ["always", "don't ask again", "do not ask again"]
        .iter()
        .any(|w| words.contains(w))
}

/// The card's answers: the agent's own options (its field for an answer of one's own is the
/// card's text field, not an option), then "Explain this in more detail" when the menu takes
/// words, then "Let qass handle it"; digits in that order, skipping standing permissions. A
/// multiple choice menu offers nothing here: the card says the question and Open.
pub fn offers(menu: &AgentMenu) -> Vec<Offer> {
    if menu.multi {
        return Vec::new();
    }
    let mut out: Vec<Offer> = menu
        .options
        .iter()
        .filter(|o| !o.input)
        .map(|o| Offer {
            digit: None,
            label: o.label.clone(),
            detail: o.detail.clone(),
            pick: Pick::Key(o.key.clone()),
        })
        .collect();
    if let Some(key) = text_key(menu) {
        out.push(Offer {
            digit: None,
            label: "Explain this in more detail".into(),
            detail: None,
            pick: Pick::Text {
                key,
                text: EXPLAIN.into(),
            },
        });
    }
    out.push(Offer {
        digit: None,
        label: "Let qass handle it".into(),
        detail: None,
        pick: Pick::Qass,
    });
    let mut next = 1u8;
    for offer in &mut out {
        if !standing(&offer.label) && next <= 9 {
            offer.digit = Some(next);
            next += 1;
        }
    }
    out
}

/// The option words go through: the agent's own answer field, else its "tell it what to do
/// differently"; `None` when the menu takes no words.
pub fn text_key(menu: &AgentMenu) -> Option<String> {
    menu.options
        .iter()
        .find(|o| o.input)
        .or_else(|| menu.options.iter().find(|o| o.tell))
        .map(|o| o.key.clone())
}

/// A pick waiting out its hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub slot: Slot,
    pub generation: Option<u64>,
    pub seal: u64,
    pub pick: Pick,
    /// What the card says it sends: "Yes".
    pub label: String,
    pub until: Instant,
}

impl Hold {
    /// The agent's terminal, for a pick on its menu.
    pub fn terminal(&self) -> Option<u32> {
        match self.slot {
            Slot::Agent(t) => Some(t),
            Slot::Card(_) => None,
        }
    }
}

/// The picks on hold, one per agent or card, oldest first, and when each menu was first seen
/// (for the arming).
#[derive(Clone, Debug, Default)]
pub struct Holds {
    pub held: Vec<Hold>,
    seen: HashMap<u32, (u64, Instant)>,
}

impl Holds {
    /// The menus on the rows now: a new one starts its arming; an answer whose menu changed or
    /// went is dropped and returned, so the card can say who answered.
    pub fn menus(&mut self, rows: &[AgentRow], now: Instant) -> Vec<Hold> {
        let shown: HashMap<u32, u64> = rows
            .iter()
            .filter_map(|r| Some((r.terminal, r.menu.as_ref()?.seal)))
            .collect();
        self.seen.retain(|t, (seal, _)| shown.get(t) == Some(seal));
        for (&t, &seal) in &shown {
            self.seen.entry(t).or_insert((seal, now));
        }
        let (kept, dropped): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| match (&h.pick, h.terminal()) {
                (Pick::Key(_) | Pick::Text { .. }, Some(t)) => shown.get(&t) == Some(&h.seal),
                _ => true,
            });
        self.held = kept;
        dropped
    }

    /// Whether the menu `seal` of `terminal` takes a digit yet.
    pub fn armed(&self, terminal: u32, seal: u64, now: Instant) -> bool {
        self.seen
            .get(&terminal)
            .is_some_and(|&(s, at)| s == seal && now.duration_since(at) >= ARMING)
    }

    /// Hold `hold`, in place of any other pick for the same agent or card.
    pub fn start(&mut self, hold: Hold) {
        self.held.retain(|h| h.slot != hold.slot);
        self.held.push(hold);
    }

    /// Undo `slot`'s pick; with none, the newest pick, and every pick held with it (one
    /// shared Undo for a hand-off of several). Whether anything was on hold.
    pub fn undo(&mut self, slot: Option<&Slot>) -> bool {
        let before = self.held.len();
        match slot {
            Some(slot) => self.held.retain(|h| &h.slot != slot),
            None => {
                if let Some(newest) = self.held.last().map(|h| h.until) {
                    self.held.retain(|h| h.until != newest);
                }
            }
        }
        self.held.len() != before
    }

    /// Undo the hand-off to qass: every agent held to be handed over, nothing else.
    pub fn undo_hand_off(&mut self) -> bool {
        let before = self.held.len();
        self.held.retain(|h| h.pick != Pick::Qass);
        self.held.len() != before
    }

    /// The picks whose hold is up, taken out to be sent.
    pub fn due(&mut self, now: Instant) -> Vec<Hold> {
        let (due, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| h.until <= now);
        self.held = kept;
        due
    }

    /// Every pick on hold, to be sent now: the window is closing.
    pub fn flush(&mut self) -> Vec<Hold> {
        std::mem::take(&mut self.held)
    }

    pub fn of(&self, slot: &Slot) -> Option<&Hold> {
        self.held.iter().find(|h| &h.slot == slot)
    }
}

/// How a refusal of `pick` reads on the card, as a sentence.
pub fn refusal_words(reason: &str, machine: &str, pick: &Pick) -> String {
    let here = machine == "This Mac";
    let max = qoop_runtime::menu::TEXT_MAX;
    match reason {
        "already_answered" => "Someone already answered this question.".into(),
        "busy" => "The agent was still taking keys. Try again in a moment.".into(),
        "no_such_answer" => "That answer is no longer on the agent's screen.".into(),
        "not_at_input" => "The agent did not open its answer field, so nothing was typed.".into(),
        "no_text_field" => "This question takes no typed answer.".into(),
        "empty" => "There were no words to send.".into(),
        "too_long" => format!("The answer is too long. Keep it under {max} characters."),
        "not_a_person" => {
            "Only a person's window can answer, and this one was not taken as one.".into()
        }
        "qass_not_running" if here => "qass is not running, so it cannot take this.".into(),
        "qass_not_running" => format!("qass is not running on {machine}, so it cannot take this."),
        "not_watching" => "qass is not watching, so it cannot take this.".into(),
        "skipped" => "qass leaves this agent alone, so it cannot take this.".into(),
        r if r.contains("unknown variant") => match pick {
            Pick::Qass => format!("qoop on {machine} is too old to hand questions to qass."),
            Pick::Text { .. } => format!("qoop on {machine} is too old for typed answers."),
            _ => format!("qoop on {machine} is too old for this answer."),
        },
        other => other.to_string(),
    }
}

/// A local day, as days since 1970-01-01 on the wall clock of `zone`.
pub fn day_of(at: i64, zone: Zone) -> i64 {
    (zone.local)(at).secs().div_euclid(86_400)
}

/// "Fri 13" for a local day.
pub fn day_label(day: i64) -> String {
    let wall = Naive::from_secs(day * 86_400);
    format!("{} {}", DAYS[(day + 4).rem_euclid(7) as usize], wall.day)
}

/// "Fri 13 Nov" for a local day.
pub fn day_long(day: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wall = Naive::from_secs(day * 86_400);
    format!(
        "{} {}",
        day_label(day),
        MONTHS[(wall.month as usize).saturating_sub(1) % 12]
    )
}

/// The slots the schedules will run at from `from` up to `until`, soonest first: each one's
/// `next_at`, then its cron slots after it, on the wall clock of `zone` (so a 02:00 cron stays
/// at 02:00 across a change of summer time). A watch or a one-off has only its `next_at`.
pub fn coming_slots(
    schedules: &[ScheduleInfo],
    from: i64,
    until: i64,
    zone: Zone,
) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    for s in schedules.iter().filter(|s| !s.paused) {
        let Some(mut at) = s.next_at else { continue };
        let cron = s.cron.as_deref().and_then(|c| Cron::parse(c).ok());
        while at <= until && out.len() < 2000 {
            if at >= from {
                out.push((at, s.id));
            }
            match cron.as_ref().and_then(|c| next_slot(c, at, zone)) {
                Some(next) if next > at => at = next,
                _ => break,
            }
        }
    }
    out.sort_unstable();
    out
}

/// One day of the strip: its runs (how many, how many failed) or, for a coming day, how many
/// will run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripDay {
    pub day: i64,
    pub runs: usize,
    pub failed: usize,
    pub coming: usize,
}

/// The date strip: the last `past` days that had runs, today, and the next `ahead` days that
/// will have some. Only days with something on them, and today.
pub fn strip_days(
    runs: &[ScheduleRun],
    coming: &[(i64, i64)],
    today: i64,
    zone: Zone,
    (past, ahead): (usize, usize),
) -> Vec<StripDay> {
    let mut days: std::collections::BTreeMap<i64, StripDay> = std::collections::BTreeMap::new();
    let blank = |day| StripDay {
        day,
        runs: 0,
        failed: 0,
        coming: 0,
    };
    for run in runs {
        let d = days
            .entry(day_of(run.started_at, zone))
            .or_insert_with_key(|&k| blank(k));
        d.runs += 1;
        d.failed += usize::from(run_failed(run));
    }
    for &(at, _) in coming {
        let day = day_of(at, zone);
        if day >= today {
            days.entry(day).or_insert_with_key(|&k| blank(k)).coming += 1;
        }
    }
    days.entry(today).or_insert_with_key(|&k| blank(k));
    let before: Vec<StripDay> = days.range(..today).map(|(_, d)| *d).collect();
    let mut out: Vec<StripDay> = before[before.len().saturating_sub(past)..].to_vec();
    out.extend(days.range(today..).take(ahead + 1).map(|(_, d)| *d));
    out
}

/// Whether a run failed: it ended so, or it never started.
pub fn run_failed(run: &ScheduleRun) -> bool {
    run.outcome.as_deref() == Some("failed") || run.result.starts_with("failed")
}

/// How a run reads, never as a state word for what is not one: Done, "Failed: <why>", "tab
/// closed", "skipped: <why>", Working while it goes; with the kind for its mark when it has
/// one.
pub fn run_words(run: &ScheduleRun) -> (Option<Kind>, String) {
    if let Some(why) = run.result.strip_prefix("skipped") {
        let why = why.trim_start_matches(':').trim();
        return (
            None,
            format!("skipped: {why}").trim_end_matches(": ").to_string(),
        );
    }
    if let Some(why) = run.result.strip_prefix("failed") {
        let why = why.trim_start_matches(':').trim();
        return (
            Some(Kind::Status(Status::Failed)),
            format!("Failed: {why}").trim_end_matches(": ").to_string(),
        );
    }
    match run.outcome.as_deref() {
        Some("done") => (Some(Kind::Status(Status::Done)), "Done".into()),
        Some("failed") => (Some(Kind::Status(Status::Failed)), "Failed:".into()),
        Some("gone") => (None, "tab closed".into()),
        Some(other) => (None, other.to_string()),
        None if run.ended_at.is_none() => (Some(Kind::Status(Status::Working)), "Working".into()),
        None => (None, run.result.clone()),
    }
}

/// How long a run took: "41 min", "2 h 5 min", "30 s"; `None` while it goes.
pub fn took(run: &ScheduleRun) -> Option<String> {
    let secs = run.ended_at?.saturating_sub(run.started_at).max(0);
    Some(match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h {} min", s / 3600, s % 3600 / 60),
    })
}

/// Home's "Last:" line for this machine's schedules: the newest run that started, as
/// "Last: nightly-tests, Fri 02:00, Done, tab 11".
pub fn last_run_line(
    schedules: &[ScheduleInfo],
    runs: &[ScheduleRun],
    now: i64,
    zone: Zone,
) -> Option<String> {
    let run = runs
        .iter()
        .filter(|r| !r.result.starts_with("skipped") && r.result != "claimed")
        .max_by_key(|r| r.started_at)?;
    let name = schedules
        .iter()
        .find(|s| s.id == run.schedule)?
        .name
        .clone();
    let (_, word) = run_words(run);
    let mut line = format!(
        "Last: {name}, {}, {word}",
        clock_at(run.started_at, now, zone)
    );
    if let Some(tab) = run.terminal {
        line.push_str(&format!(
            ", tab {}",
            qoop_tui::mux::daemon_terminal(tab as u32)
        ));
    }
    Some(line)
}

/// How a setting is changed on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    /// A switch: `true` or `false`.
    Switch,
    /// One of a few words, as a segmented control.
    Choice(&'static [&'static str]),
    /// One of many, stepped through by a button (the prefix, a sound).
    Cycle,
    /// Minutes, as a few steps; 0 is never.
    Minutes,
    /// Shown as the file says it; changed in the editor.
    Text,
}

/// One line of the settings page: its key in `config.toml`, its name, what it does, how it
/// is changed, and its group.
#[derive(Clone, Copy, Debug)]
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: SettingKind,
    pub group: &'static str,
}

/// Every key of `qoop_tui::config::KEYS`, as the page shows it.
pub const SETTINGS: [Setting; 21] = [
    Setting {
        key: "prefix",
        label: "Prefix key",
        help: "The key before every qoop key, in the terminal and here",
        kind: SettingKind::Cycle,
        group: "Keys and look",
    },
    Setting {
        key: "theme",
        label: "Theme",
        help: "Auto follows the system",
        kind: SettingKind::Choice(&["auto", "dark", "light"]),
        group: "Keys and look",
    },
    Setting {
        key: "icons",
        label: "Icons",
        help: "qoop's own icon font in the terminal interface",
        kind: SettingKind::Choice(&["auto", "on", "off"]),
        group: "Keys and look",
    },
    Setting {
        key: "copy_on_select",
        label: "Copy on select",
        help: "A drag in the terminal interface copies when you let go",
        kind: SettingKind::Switch,
        group: "Keys and look",
    },
    Setting {
        key: "notify.desktop",
        label: "Desktop notes",
        help: "A note when an agent finishes, fails or needs you",
        kind: SettingKind::Switch,
        group: "Notifications",
    },
    Setting {
        key: "notify.sound",
        label: "Sound",
        help: "A sound with each note",
        kind: SettingKind::Switch,
        group: "Notifications",
    },
    Setting {
        key: "notify.done_sound",
        label: "Done sound",
        help: "Played when an agent finishes",
        kind: SettingKind::Cycle,
        group: "Notifications",
    },
    Setting {
        key: "notify.alert_sound",
        label: "Alert sound",
        help: "Played when an agent fails or needs you",
        kind: SettingKind::Cycle,
        group: "Notifications",
    },
    Setting {
        key: "notify.mute",
        label: "Mute",
        help: "Off, this computer only, or everywhere",
        kind: SettingKind::Choice(&["off", "here", "everywhere"]),
        group: "Notifications",
    },
    Setting {
        key: "notch.enabled",
        label: "Notch",
        help: "The agents in the MacBook notch",
        kind: SettingKind::Switch,
        group: "Notifications",
    },
    Setting {
        key: "sleep.idle_minutes",
        label: "Sleep idle tabs",
        help: "Minutes a tab idles before it sleeps; never keeps them awake",
        kind: SettingKind::Minutes,
        group: "Sleep",
    },
    Setting {
        key: "sleep.qass_idle_minutes",
        label: "Sleep qass",
        help: "Minutes qass idles before it sleeps",
        kind: SettingKind::Minutes,
        group: "Sleep",
    },
    Setting {
        key: "daemon.exit_when_asleep",
        label: "Leave when all sleep",
        help: "qoop's daemon ends once nothing is awake",
        kind: SettingKind::Switch,
        group: "Sleep",
    },
    Setting {
        key: "qass.agent",
        label: "qass runs on",
        help: "The agent qass starts on",
        kind: SettingKind::Choice(&["claude", "codex", "gemini", "opencode"]),
        group: "qass",
    },
    Setting {
        key: "qass.models.best",
        label: "Best model",
        help: "What qass's hardest work runs on",
        kind: SettingKind::Text,
        group: "qass",
    },
    Setting {
        key: "qass.models.mid",
        label: "Middle model",
        help: "What qass's usual work runs on",
        kind: SettingKind::Text,
        group: "qass",
    },
    Setting {
        key: "qass.models.low",
        label: "Small model",
        help: "What qass's simple work runs on",
        kind: SettingKind::Text,
        group: "qass",
    },
    Setting {
        key: "board.cards",
        label: "Board cards",
        help: "Full cards or quiet ones",
        kind: SettingKind::Choice(&["full", "quiet"]),
        group: "Board",
    },
    Setting {
        key: "board.view",
        label: "Board view",
        help: "A list or lanes",
        kind: SettingKind::Choice(&["list", "board"]),
        group: "Board",
    },
    Setting {
        key: "report.send",
        label: "Crash reports",
        help: "Ask first, always send, or never",
        kind: SettingKind::Choice(&["ask", "always", "never"]),
        group: "Privacy",
    },
    Setting {
        key: "telemetry",
        label: "Usage counts",
        help: "Daily counts of what is used, nothing typed or shown",
        kind: SettingKind::Switch,
        group: "Privacy",
    },
];

/// What `read` says for `key`, as `config::set` takes it. `telemetry` is not in a read: it is
/// off only when the file says `telemetry = false`.
pub fn setting_value(read: &qoop_tui::config::Read, text: &str, key: &str) -> String {
    use qoop_tui::config::Theme;
    use qoop_tui::icons::Setting as Icons;
    let c = &read.config;
    let on = |b: bool| if b { "true" } else { "false" }.to_string();
    match key {
        "prefix" => qoop_tui::keys::name_of(c.prefix),
        "theme" => match c.theme {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
        .into(),
        "icons" => match c.icons {
            Icons::Auto => "auto",
            Icons::On => "on",
            Icons::Off => "off",
        }
        .into(),
        "copy_on_select" => on(c.copy_on_select),
        "notify.desktop" => on(c.desktop),
        "notify.sound" => on(c.sound),
        "notify.done_sound" => c.done_sound.to_string(),
        "notify.alert_sound" => c.alert_sound.to_string(),
        "notify.mute" => c.mute.name().to_string(),
        "notch.enabled" => on(c.notch),
        "sleep.idle_minutes" => c.lifecycle.idle_minutes.to_string(),
        "sleep.qass_idle_minutes" => c
            .lifecycle
            .qass_idle_minutes
            .unwrap_or(c.lifecycle.idle_minutes)
            .to_string(),
        "daemon.exit_when_asleep" => on(c.lifecycle.exit_when_asleep),
        "qass.agent" => read.qass_agent.clone(),
        "qass.models.best" => read.qass_models[0].clone(),
        "qass.models.mid" => read.qass_models[1].clone(),
        "qass.models.low" => read.qass_models[2].clone(),
        "board.cards" => if c.quiet_cards { "quiet" } else { "full" }.into(),
        "board.view" => if c.board_list { "list" } else { "board" }.into(),
        "report.send" => read.report_send.clone(),
        "telemetry" => on(!text
            .lines()
            .any(|l| l.split('#').next().unwrap_or("").replace(' ', "") == "telemetry=false")),
        _ => String::new(),
    }
}

/// The line of `text` that sets `key` (its last part, inside its table), 1 based: where a
/// write landed, said back to the person.
pub fn line_of(text: &str, key: &str) -> Option<usize> {
    let (table, leaf) = match key.rsplit_once('.') {
        Some((table, leaf)) => (table, leaf),
        None => ("", key),
    };
    let mut here = String::new();
    for (n, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            here = name.trim().to_string();
            continue;
        }
        let Some((k, _)) = trimmed.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if (here == table && k == leaf) || (here.is_empty() && k == key) {
            return Some(n + 1);
        }
    }
    None
}

/// Whether a setting matches what the find field holds: its name, key or help.
pub fn setting_found(setting: &Setting, find: &str) -> bool {
    let find = find.trim().to_lowercase();
    find.is_empty()
        || [setting.label, setting.key, setting.help, setting.group]
            .iter()
            .any(|s| s.to_lowercase().contains(&find))
}

/// A pet as filled rects `(x, y, w, h, "#rrggbb")`: how a daemon sends it.
pub type PetRect = (u8, u8, u8, u8, String);

/// The line a person runs to install qoop.
pub const INSTALL_LINE: &str = "curl -fsSL https://qoop.si/install.sh | sh";

/// The `qoop` the app runs: on `PATH` (with Homebrew's folders), else where the one-line install
/// puts it. An app opened from the Finder gets a short `PATH` without `~/.local/bin`.
pub fn qoop_bin() -> Option<std::path::PathBuf> {
    qoop_runtime::changes::tool("qoop").or_else(|| {
        let home = std::env::var_os("HOME")?;
        let local = std::path::PathBuf::from(home).join(".local/bin/qoop");
        local.is_file().then_some(local)
    })
}

/// The terminals whose menu is new since `before` (none then, or another seal): what was said
/// of their last answer belongs to the old question.
pub fn new_menus(before: &[AgentRow], now: &[AgentRow]) -> Vec<TerminalStream> {
    now.iter()
        .filter_map(|r| {
            let seal = r.menu.as_ref()?.seal;
            let was = before
                .iter()
                .find(|b| b.terminal == r.terminal)
                .and_then(|b| b.menu.as_ref())
                .map(|m| m.seal);
            (was != Some(seal)).then_some(r.terminal)
        })
        .collect()
}

/// A machine silent this long keeps its rows, but as of when it last answered.
pub const STALE_AFTER: i64 = 15;

/// How each machine was last heard. `heard` is the last answer from the machine's own daemon
/// (the window asks each one that is up every [`PING_EVERY`]): a link that stays up while its
/// daemon says nothing goes stale by it. `down_since` is when the space lists first said every
/// link to it was down, which counts for a machine never heard on its own (a qoop from before
/// the window's ask). The mux pushes a list the moment a link drops.
#[derive(Clone, Debug, Default)]
pub struct Reach {
    pub down_since: std::collections::BTreeMap<usize, i64>,
    pub heard: std::collections::BTreeMap<usize, i64>,
    /// The machines whose link was up in the last list.
    up: BTreeSet<usize>,
    /// Every machine a list has named, and the ones down in the first list that named them
    /// and not up since: down already when the window opened.
    named: BTreeSet<usize>,
    never_up: BTreeSet<usize>,
}

/// How often the window asks each machine that is up for a word from its daemon.
pub const PING_EVERY: i64 = 5;

/// How a machine stands for the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineState {
    Live,
    /// Let go while nothing ran there.
    Parked,
    /// Not answering for less than [`STALE_AFTER`].
    Retrying,
    /// Not answering: its rows are as of `as_of`.
    Stale {
        as_of: i64,
    },
    /// Not answering, and never reached since the window opened: there is no time to give.
    NeverReached,
}

/// Whether a space of machine `m` says its link is up (a remote's; this Mac's say nothing).
fn link_up(spaces: &[WorkspaceRow], m: usize) -> bool {
    spaces
        .iter()
        .any(|w| machine_of(w.id) == m && w.link == Some(LinkState::Up))
}

/// Whether every space of machine `m` says its link is down (and it has one).
fn all_down(spaces: &[WorkspaceRow], m: usize) -> bool {
    let mut of = spaces.iter().filter(|w| machine_of(w.id) == m).peekable();
    of.peek().is_some() && of.all(|w| w.link == Some(LinkState::Down))
}

impl Reach {
    /// The space list came: a machine whose every link is down has not answered since the
    /// first list that said so; any other has.
    pub fn note(&mut self, spaces: &[WorkspaceRow], now: i64) {
        let machines: BTreeSet<usize> = spaces.iter().map(|w| machine_of(w.id)).collect();
        self.down_since.retain(|m, _| machines.contains(m));
        self.heard.retain(|m, _| machines.contains(m));
        let up: BTreeSet<usize> = machines
            .iter()
            .copied()
            .filter(|m| link_up(spaces, *m))
            .collect();
        for &m in &machines {
            let down = all_down(spaces, m);
            if self.named.insert(m) && down {
                self.never_up.insert(m);
            }
            if down {
                self.down_since.entry(m).or_insert(now);
            } else {
                self.down_since.remove(&m);
            }
            if up.contains(&m) {
                self.never_up.remove(&m);
            }
        }
        // A link that just came up is the machine's daemon answering.
        for m in up.difference(&self.up) {
            if let Some(heard) = self.heard.get_mut(m) {
                *heard = now;
            }
        }
        self.up = up;
    }

    /// An answer from machine `m`'s own daemon came at `now`.
    pub fn hear(&mut self, m: usize, now: i64) {
        self.heard.insert(m, now);
    }

    /// How machine `m` stands at `now`. `lost` is when this Mac's daemon went away, if it has:
    /// then every machine is stale, the remotes too (the mux ended with it).
    pub fn state(
        &self,
        m: usize,
        spaces: &[WorkspaceRow],
        now: i64,
        lost: Option<i64>,
    ) -> MachineState {
        match self.state_as_of(m, spaces, now, lost) {
            MachineState::Stale { .. }
                if self.never_up.contains(&m) && !self.heard.contains_key(&m) =>
            {
                MachineState::NeverReached
            }
            state => state,
        }
    }

    fn state_as_of(
        &self,
        m: usize,
        spaces: &[WorkspaceRow],
        now: i64,
        lost: Option<i64>,
    ) -> MachineState {
        let since = self.down_since.get(&m).copied();
        let heard = self.heard.get(&m).copied();
        // The last word from it: its own answer, else when its link went down.
        let last = match (heard, since) {
            (Some(h), Some(s)) => Some(h.min(s)),
            (h, s) => h.or(s),
        };
        if let Some(lost) = lost {
            return MachineState::Stale {
                as_of: last.map_or(lost, |l| l.min(lost)),
            };
        }
        let parked = spaces
            .iter()
            .filter(|w| machine_of(w.id) == m)
            .all(|w| w.link == Some(LinkState::Parked));
        let quiet = last.filter(|l| now - l >= STALE_AFTER);
        match (since, quiet) {
            (Some(_), Some(as_of)) => MachineState::Stale { as_of },
            (Some(_), None) => MachineState::Retrying,
            (None, _) if parked && m != 0 => MachineState::Parked,
            // Up, and its daemon has said nothing for the whole time.
            (None, Some(as_of)) if link_up(spaces, m) => MachineState::Stale { as_of },
            (None, _) => MachineState::Live,
        }
    }
}

/// The machine a request goes to: by the terminal or the space it names (a `Via` by its
/// space); zero for this one and for a request that names neither.
pub fn request_machine(request: &qoop_runtime::control::ControlRequest) -> usize {
    let Ok(wire) = serde_json::to_value(request) else {
        return 0;
    };
    let terminal = wire
        .get("terminal")
        .and_then(serde_json::Value::as_u64)
        .and_then(|t| u32::try_from(t).ok());
    if let Some(terminal) = terminal {
        return qoop_tui::mux::machine_of_terminal(terminal);
    }
    wire.get("workspace")
        .and_then(serde_json::Value::as_i64)
        .map_or(0, machine_of)
}

/// A machine's state in the words its tile and the banner both use.
pub fn reach_words(state: MachineState, zone: Zone) -> String {
    match state {
        MachineState::Live => "connected".into(),
        MachineState::Parked => "parked".into(),
        MachineState::Retrying => "retrying".into(),
        MachineState::Stale { as_of } => format!("not reached, as of {}", clock(as_of, zone)),
        MachineState::NeverReached => "not reached since the window opened".into(),
    }
}

/// The banner over the page: every machine not reached, in its tile's words.
pub fn reach_banner(machines: &[(String, MachineState)], zone: Zone) -> Option<String> {
    let stale: Vec<String> = machines
        .iter()
        .filter(|(_, s)| matches!(s, MachineState::Stale { .. } | MachineState::NeverReached))
        .map(|(name, s)| format!("{name} {}", reach_words(*s, zone)))
        .collect();
    (!stale.is_empty()).then(|| stale.join("; "))
}

/// Why a row's answers cannot be pressed while its machine is not reached.
pub const STALE_REASON: &str = "Answering opens again when the daemon is back";

/// Each machine with a space, This Mac first, as a person names it, with one space of it
/// (`None` for This Mac) to ask it through.
pub fn machines(spaces: &[WorkspaceRow]) -> Vec<(usize, String, Option<i64>)> {
    let mut out: Vec<(usize, String, Option<i64>)> = vec![(0, "This Mac".into(), None)];
    for w in spaces {
        let m = machine_of(w.id);
        if m != 0 && !out.iter().any(|(n, _, _)| *n == m) {
            out.push((m, machine_name(w), Some(w.id)));
        }
    }
    out
}

/// A space's own name, without its machine or the reason a link adds while down.
pub fn short_space(space: &WorkspaceRow) -> String {
    let name = space.name.rsplit(':').next().unwrap_or(&space.name);
    name.split(" (").next().unwrap_or(name).to_string()
}

/// A machine not answering keeps the rows it last had: the mux drops a lost link's agents,
/// the window keeps them, as of then. The rows of `before` to add to `now`.
pub fn unreached_rows(
    before: &[AgentRow],
    now: &[AgentRow],
    spaces: &[WorkspaceRow],
) -> Vec<AgentRow> {
    let listed: BTreeSet<usize> = now.iter().map(|r| machine_of(r.workspace)).collect();
    before
        .iter()
        .filter(|r| {
            let m = machine_of(r.workspace);
            m != 0 && !listed.contains(&m) && all_down(spaces, m)
        })
        .cloned()
        .collect()
}

/// While a machine has not answered yet and has no rows: which spaces are in, and whose
/// rows are still to come. "acme-web and api are in; billing-api rows come when staging
/// answers".
pub fn loading_line(spaces: &[WorkspaceRow], rows: &[AgentRow]) -> Option<String> {
    let has_rows = |m: usize| rows.iter().any(|r| machine_of(r.workspace) == m);
    let waiting: Vec<String> = machines(spaces)
        .into_iter()
        .filter(|(m, _, _)| all_down(spaces, *m) && !has_rows(*m))
        .map(|(m, name, _)| {
            let names: Vec<String> = spaces
                .iter()
                .filter(|w| machine_of(w.id) == m)
                .map(short_space)
                .collect();
            format!("{} rows come when {name} answers", and_list(&names))
        })
        .collect();
    if waiting.is_empty() {
        return None;
    }
    let mut seen: BTreeSet<i64> = BTreeSet::new();
    let ins: Vec<String> = rows
        .iter()
        .filter(|r| seen.insert(r.workspace))
        .filter_map(|r| spaces.iter().find(|w| w.id == r.workspace))
        .filter(|w| w.name != QASS_SPACE)
        .map(short_space)
        .collect();
    let rest = waiting.join("; ");
    Some(if ins.is_empty() {
        rest
    } else {
        format!(
            "{} {} in; {rest}",
            and_list(&ins),
            if ins.len() == 1 { "is" } else { "are" }
        )
    })
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The question on `before` was answered by another client: its closed options stay on the
/// card, greyed, under this line. `None` when it was not, or the window answered it.
pub fn answered_elsewhere(before: &AgentRow, now: &AgentRow, zone: Zone) -> Option<String> {
    before.menu.as_ref()?;
    if now.menu.is_some() {
        return None;
    }
    let answered = now.answered.as_ref()?;
    let by = match answered.by.as_str() {
        "computer" => "Mac",
        "phone" => "iPhone",
        "attach" => "terminal",
        other => other,
    };
    Some(format!(
        "Answered on your {by}: {}, {}",
        answered.label,
        clock(answered.at, zone)
    ))
}

/// qass's rules as its lessons file counts them: the lessons that keep a question the owner's
/// (`escalate`, `ask first`), the ones it answers alone (`do it`), and every lesson. A lesson
/// is a line `- <when> -> <action> (<source>)`, as `skills/qass/SKILL.md` writes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lessons {
    pub yours: usize,
    pub alone: usize,
    pub all: usize,
}

pub fn lessons_of(text: &str) -> Lessons {
    let mut out = Lessons::default();
    for line in text.lines() {
        let Some(entry) = line.trim_start().strip_prefix("- ") else {
            continue;
        };
        let Some((_, action)) = entry.rsplit_once(" -> ") else {
            continue;
        };
        let action = action
            .split(" (")
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        out.all += 1;
        match action.as_str() {
            "escalate" | "ask first" => out.yours += 1,
            "do it" => out.alone += 1,
            _ => {}
        }
    }
    out
}

/// A unix time as "15:58" in `zone`.
pub fn clock(at: i64, zone: Zone) -> String {
    (zone.local)(at).clock()
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

const SLEEPING_SUB: &str = "Wakes on a click";

/// A row's second line.
pub fn sub_of(row: &AgentRow) -> String {
    match Kind::of(row) {
        Kind::Status(Status::Blocked) => "in Inbox".into(),
        Kind::Sleeping => SLEEPING_SUB.into(),
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

/// The longest line typed. A new shell may still be starting when it arrives, with the
/// terminal in line mode, and macOS keeps about 1023 bytes of an unfinished line and drops
/// the rest: measured, 929 bytes typed a second before zsh read them ran, 1229 lost the end
/// (the quote, `&& exit`, the Enter) and ran nothing.
pub const LINE_MOST: usize = 1000;

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
/// Empty when no Latin letter or digit is left (a task in Japanese): see [`branch_or_clock`].
pub fn branch_for(prompt: &str) -> String {
    let plain: String = prompt
        .chars()
        .filter(|c| *c != '\'' && *c != '\u{2019}')
        // Lowercase first, so the table needs only lowercase letters; `İ` alone, whose
        // lowercase adds a combining dot.
        .map(|c| if c == 'İ' { 'i' } else { c })
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ı' | 'í' | 'ì' | 'î' | 'ï' => 'i',
            'ö' | 'ó' | 'ò' | 'ô' | 'õ' => 'o',
            'ş' | 'ß' => 's',
            'ü' | 'ú' | 'ù' | 'û' => 'u',
            'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ñ' => 'n',
            c => c,
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

/// The task's branch, or `feat/work-<at>` (unix seconds) when its words give none.
pub fn branch_or_clock(prompt: &str, at: i64) -> String {
    let branch = branch_for(prompt);
    if branch.is_empty() {
        format!("feat/work-{at}")
    } else {
        branch
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
    let line = format!(
        "qoop worktree new {}{space} -- {agent} && exit",
        shell_quote(branch)
    );
    if line.len() >= LINE_MOST {
        return Err("The task is too long to type: shorten it.");
    }
    Ok(line)
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

    /// A space with sleeping tabs, as a current daemon lists it (through the mux, so a remote
    /// one's tabs carry the interface's numbers): `(terminal, lead, agent, session)`.
    pub fn sleepy_space(
        id: i64,
        name: &str,
        tabs: &[(u32, u32, Option<&str>, &str)],
    ) -> WorkspaceRow {
        let sleeping: Vec<serde_json::Value> = tabs
            .iter()
            .map(|(terminal, lead, agent, session)| {
                serde_json::json!({
                    "terminal": terminal, "lead": lead, "title": null, "agent": agent,
                    "session": session, "cwd": "/srv/work",
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "terminals": tabs.len() + 1, "sleeping": sleeping,
        }))
        .expect("space")
    }

    /// The design's data set: seven agents in three spaces on two machines. The remote's
    /// sleeping tab is listed under its space, as a current daemon sends it (its row is not in
    /// the agent list at all); beside it the second pane of the same tab and a sleeping shell,
    /// neither of which is an agent row.
    pub fn fixture() -> Model {
        let remote = 1i64 << 40 | 1;
        Model {
            rows: vec![
                row(1, 1, "working", "claude", "Bun test suite status"),
                row(2, 1, "blocked", "claude", "README Coupons section"),
                row(3, 1, "done", "claude", "Coupon.ts test suggestion"),
                row(4, 2, "done", "claude", "CHANGELOG.md summary"),
                row(5, 2, "working", "codex", "POST /orders route in routes.ts"),
                row(0x8100_0001, remote, "working", "codex", "Bun run test:soak"),
                row(9, 3, "working", "claude", "qass"),
            ],
            spaces: vec![
                space(1, "acme-web"),
                space(2, "api"),
                space(3, "qass"),
                sleepy_space(
                    remote,
                    "staging:billing-api",
                    &[
                        (
                            0x8100_0002,
                            0x8100_0002,
                            Some("claude"),
                            "Test suite status",
                        ),
                        (
                            0x8100_0003,
                            0x8100_0002,
                            Some("claude"),
                            "Test suite status",
                        ),
                        (0x8100_0004, 0x8100_0004, None, ""),
                    ],
                ),
            ],
            ..Model::default()
        }
    }

    /// A space where everything sleeps has no awake row at all: it is still on home, with its
    /// sleeping agent. And an older daemon's sleeping row still counts once.
    #[test]
    fn a_space_asleep_is_on_home_and_an_older_daemons_sleeper_counts_once() {
        let mut m = fixture();
        m.spaces
            .push(sleepy_space(7, "docs", &[(70, 70, Some("codex"), "")]));
        let docs = m
            .groups(Chip::Sleeping)
            .into_iter()
            .find(|g| g.space.name == "docs")
            .expect("the sleeping space is listed");
        assert_eq!(docs.rows.len(), 1);
        assert_eq!(docs.rows[0].title(), "codex", "no name: the agent");
        assert_eq!(m.count(Chip::Sleeping), 2);
        assert_eq!(m.summary(), "8 in 4 spaces on 2 machines");
        let mut old = row(70, 7, "unknown", "codex", "");
        old.state = "sleeping".into();
        old.agent = false;
        m.rows.push(old);
        assert_eq!(m.count(Chip::Sleeping), 2, "listed both ways, counted once");
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
        let titles: Vec<String> = groups[0].rows.iter().map(|e| e.title()).collect();
        assert_eq!(
            titles,
            [
                "README Coupons section",
                "Coupon.ts test suggestion",
                "Bun test suite status"
            ]
        );
        let api: Vec<u32> = groups[1].rows.iter().map(|e| e.terminal()).collect();
        assert_eq!(api, [4, 5], "done before working");
        assert_eq!(
            groups[2].rows.len(),
            2,
            "the sleeping tab is listed, once per tab"
        );
        let asleep = groups[2]
            .rows
            .iter()
            .find(|e| e.kind() == Kind::Sleeping)
            .expect("the sleeper");
        assert_eq!(asleep.title(), "Test suite status");
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
            Some(
                format!(
                    "next: acme-web/{} needs you",
                    title_of(m.row(2).expect("row"))
                )
                .as_str()
            )
        );
        // Another claude tab in the same space working: the line still names the one that waits.
        assert_ne!(
            title_of(m.row(1).expect("row")),
            title_of(m.row(2).expect("row"))
        );
        let blocked = m.row(2).expect("row").clone();
        let mut b = blocked.clone();
        b.last_active_at_unix = Some(1000);
        assert_eq!(
            m.row_label(Entry::Live(&b), 1240),
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
        for entry in m.agents() {
            words.push(entry.sub());
            words.push(m.row_label(entry, 2000));
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
        assert_eq!(
            branch_for("\u{c9}lan \u{d1}and\u{fa} \u{c1}LTO"),
            "feat/elan-nandu-alto"
        );
        let japanese = "\u{30ed}\u{30b0}\u{30a4}\u{30f3}\u{753b}\u{9762}\u{3092}\u{76f4}\u{3059}";
        assert_eq!(branch_for(japanese), "", "no Latin word left");
        assert_eq!(
            branch_or_clock(japanese, 1_763_100_000),
            "feat/work-1763100000"
        );
        assert_eq!(branch_or_clock("fix it", 1), "feat/fix-it");
        assert!(
            worktree_line(&branch_or_clock(japanese, 1), Some(1), "claude", japanese).is_ok(),
            "a task with no Latin letter still starts"
        );
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

    /// A backslash before a quote ended the quote early in fish and ran the rest; it is now
    /// typed outside the quotes. Every shell's reading is checked in `qoop_domain::shell`.
    #[test]
    fn a_backslash_in_the_task_stays_one_word_in_every_shell() {
        let task = r"a\'; touch /tmp/pwn; #";
        let line = worktree_line("b", None, "claude", task).expect("line");
        assert_eq!(
            line,
            r"qoop worktree new b -- claude 'a'\\\''; touch /tmp/pwn; #' && exit"
        );
        assert_eq!(
            words(line.trim_end_matches(" && exit"))
                .last()
                .map(String::as_str),
            Some(task)
        );
    }

    /// A line the terminal would cut before the shell reads it is refused, not typed.
    #[test]
    fn a_line_too_long_to_type_is_refused() {
        let most = LINE_MOST - "qoop worktree new b -- claude  && exit".len();
        assert!(worktree_line("b", None, "claude", &"x".repeat(most - 1)).is_ok());
        assert_eq!(
            worktree_line("b", None, "claude", &"x".repeat(most)),
            Err("The task is too long to type: shorten it.")
        );
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

    pub fn split(first: u32, second: u32) -> Pane {
        Pane::Split {
            dir: qoop_runtime::control::SplitDir::V,
            permille: 600,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: first }),
            second: Box::new(Pane::Leaf { terminal: second }),
        }
    }

    /// A split tab lays out its panes and is one tab on the strip; a tab of its own is a leaf.
    #[test]
    fn a_split_is_one_tab_with_its_panes() {
        let mut m = fixture();
        m.splits = vec![split(1, 3)];
        assert_eq!(m.tree_of(3), split(1, 3));
        assert_eq!(m.tree_of(2), Pane::Leaf { terminal: 2 });
        let tabs: Vec<u32> = m.tabs_of(1).iter().map(|r| r.terminal).collect();
        assert_eq!(tabs, [1, 2]);
    }

    /// The corner note is for another agent that turned Needs you, never the one on screen
    /// nor one that already needed the owner.
    #[test]
    fn a_corner_note_is_for_another_agent_turning_needs_you() {
        let before = fixture().rows;
        let mut m = fixture();
        assert_eq!(m.turned_needs(&before, &[1]), None, "nothing turned");
        m.rows[2].status = Some("blocked".into());
        assert_eq!(m.turned_needs(&before, &[1]), Some(3));
        assert_eq!(m.turned_needs(&before, &[3]), None, "the one on screen");
        assert_eq!(
            m.turned_needs(&before, &[1, 3]),
            None,
            "the other pane on screen"
        );
        m.rows[6].status = Some("blocked".into());
        m.rows[2].status = Some("done".into());
        assert_eq!(m.turned_needs(&before, &[1]), None, "qass's own row");
        assert_eq!(m.turned_needs(&[], &[1]), None, "a row not listed before");
    }

    fn event(at: i64, kind: &str, body: &str) -> TaskEventInfo {
        TaskEventInfo {
            id: at,
            at,
            kind: kind.into(),
            body: body.into(),
        }
    }

    /// The strip from a `Tasks` answer: the steps of the task's item with the current one, the
    /// evidence oldest first with the failing run marked, and none without a task.
    #[test]
    fn the_task_strip_reads_steps_and_evidence() {
        let task: TaskInfo = serde_json::from_value(serde_json::json!({
            "id": 12, "terminal": 1, "goal": "Fix the suite", "stage": "implementing",
            "opened_at": 0, "updated_at": 0,
        }))
        .expect("task");
        let step = |n: i64, done_at: Option<i64>| StepInfo {
            n,
            text: format!("step {n}"),
            done_at,
        };
        let card: ItemCardInfo = serde_json::from_value(serde_json::json!({
            "id": 3, "key": "I3", "workspace": 1, "title": "t", "state": "open",
            "steps_done": 2, "steps_total": 4, "updated_at": 0, "description": "",
            "steps": [], "activity": [],
        }))
        .unwrap_or_else(|e| panic!("card: {e}"));
        let card = ItemCardInfo {
            steps: vec![
                step(1, Some(10)),
                step(2, Some(20)),
                step(3, None),
                step(4, None),
            ],
            ..card
        };
        let strip = Strip::of(
            vec![task],
            vec![
                event(
                    30,
                    "evidence",
                    "bun test src/coupon.test.ts: 2 pass, 0 fail",
                ),
                event(20, "stage", "implementing"),
                event(10, "evidence", "bun test: 12 pass, 1 fail"),
            ],
            Some(Box::new(card)),
        );
        assert_eq!(
            strip.heading(),
            Some(("Task #12".into(), "2 of 4 done".into()))
        );
        assert_eq!(strip.current(), Some(2));
        let evidence: Vec<(i64, bool)> =
            strip.evidence().iter().map(|e| (e.at, e.failed)).collect();
        assert_eq!(evidence, [(10, true), (30, false)]);
        assert_eq!(Strip::of(Vec::new(), Vec::new(), None).heading(), None);
    }

    #[test]
    fn a_selection_copies_its_cells() {
        let lines: Vec<String> = ["$ ls   ", "a.txt  b.txt", "c.txt"]
            .map(String::from)
            .to_vec();
        assert_eq!(selected_text(&lines, (1, 7), (1, 11)), "b.txt");
        assert_eq!(
            selected_text(&lines, (2, 2), (0, 2)),
            "ls\na.txt  b.txt\nc.t"
        );
    }

    #[test]
    fn a_failing_run_is_told_by_its_count() {
        assert!(failed_run("bun test: 12 pass, 1 fail"));
        assert!(failed_run("cargo build failed"));
        assert!(!failed_run("cargo test: 40 passed; 0 failed"));
        assert!(!failed_run("bun test: 15 pass, 0 fail, 35 expect() calls"));
        assert!(!failed_run("Fixed the failing coupon test: 2 pass, 0 fail"));
        assert!(!failed_run("all green, no failures"));
        assert!(failed_run("cargo test: 3 failed; 40 passed"));
    }

    /// Claude Code's edit question: Yes, an "always" option, No and tell it, and its field.
    pub fn edit_menu(seal: u64) -> AgentMenu {
        serde_json::from_value(serde_json::json!({
            "question": "Do you want to make this edit to README.md?",
            "seal": seal,
            "options": [
                {"key": "1", "label": "Yes"},
                {"key": "2", "label": "Yes, allow all edits during this session (shift+tab)"},
                {"key": "3", "label": "Yes, and always allow access to src/ from this project"},
                {"key": "4", "label": "No, and tell Claude what to do differently (esc)", "tell": true},
            ],
        }))
        .expect("menu")
    }

    #[test]
    fn the_explain_words_are_the_daemons() {
        assert_eq!(EXPLAIN, qoop_runtime::menu::EXPLAIN);
    }

    /// Digits 1 to 5 in order, none on the "always" option, Explain and qass after the agent's
    /// own; a multiple choice menu offers nothing to pick here.
    #[test]
    fn the_card_numbers_its_answers() {
        let menu = edit_menu(7);
        let all = offers(&menu);
        let offered: Vec<(Option<u8>, &str)> = all
            .iter()
            .map(|o| (o.digit, o.label.split(',').next().unwrap_or("")))
            .collect();
        assert_eq!(
            offered,
            [
                (Some(1), "Yes"),
                (Some(2), "Yes"),
                (None, "Yes"),
                (Some(3), "No"),
                (Some(4), "Explain this in more detail"),
                (Some(5), "Let qass handle it"),
            ]
        );
        assert_eq!(text_key(&menu).as_deref(), Some("4"));
        let multi = AgentMenu {
            multi: true,
            ..menu
        };
        assert!(offers(&multi).is_empty());
    }

    fn held(terminal: u32, seal: u64, until: Instant) -> Hold {
        Hold {
            slot: Slot::Agent(terminal),
            generation: Some(1),
            seal,
            pick: Pick::Key("1".into()),
            label: "Yes".into(),
            until,
        }
    }

    /// With a fake clock: a menu arms after 500 ms, a pick goes after 3 s, Undo cancels it, a
    /// new menu drops it, and closing the window sends what is on hold.
    #[test]
    fn a_pick_waits_three_seconds_and_can_be_undone() {
        let t0 = Instant::now();
        let mut rows = fixture().rows;
        rows[1].menu = Some(edit_menu(7));
        let mut holds = Holds::default();
        assert!(holds.menus(&rows, t0).is_empty());
        assert!(!holds.armed(2, 7, t0 + Duration::from_millis(499)));
        assert!(holds.armed(2, 7, t0 + ARMING));

        holds.start(held(2, 7, t0 + HOLD));
        assert!(holds.due(t0 + Duration::from_millis(2999)).is_empty());
        assert_eq!(holds.due(t0 + HOLD).len(), 1, "sent after 3 s");

        holds.start(held(2, 7, t0 + HOLD));
        assert!(holds.undo(Some(&Slot::Agent(2))));
        assert!(holds.due(t0 + HOLD).is_empty(), "Undo cancels");

        holds.start(held(2, 7, t0 + HOLD));
        rows[1].menu = Some(edit_menu(8));
        let dropped = holds.menus(&rows, t0 + Duration::from_secs(1));
        assert_eq!(dropped.len(), 1, "another menu drops the pick");
        assert!(
            !holds.armed(2, 8, t0 + Duration::from_secs(1)),
            "and arms anew"
        );

        holds.start(held(2, 8, t0 + HOLD));
        holds.start(held(3, 9, t0 + HOLD));
        assert_eq!(holds.flush().len(), 2, "closing sends both at once");
        assert!(holds.held.is_empty());

        // The hand-off's own Undo takes back the hand-off, not a single answer held later.
        let hand = |terminal| Hold {
            pick: Pick::Qass,
            ..held(terminal, 0, t0 + HOLD)
        };
        holds.start(hand(4));
        holds.start(hand(5));
        holds.start(held(2, 8, t0 + HOLD + Duration::from_secs(1)));
        assert!(holds.undo_hand_off());
        let left: Vec<Option<u32>> = holds.held.iter().map(Hold::terminal).collect();
        assert_eq!(left, [Some(2)]);
    }

    #[test]
    fn refusals_read_as_words() {
        let key = Pick::Key("1".into());
        assert_eq!(
            refusal_words("already_answered", "This Mac", &key),
            "Someone already answered this question."
        );
        assert_eq!(
            refusal_words("qass_not_running", "staging", &Pick::Qass),
            "qass is not running on staging, so it cannot take this."
        );
        let old = "this daemon is older than the client that called it: it does not know the \
                   method that was asked for (unknown variant `AnswerMenuText`)";
        let words = Pick::Text {
            key: "4".into(),
            text: "x".into(),
        };
        assert_eq!(
            refusal_words(old, "staging", &words),
            "qoop on staging is too old for typed answers."
        );
        assert_eq!(
            refusal_words(old, "staging", &Pick::Qass),
            "qoop on staging is too old to hand questions to qass."
        );
        // Every code the daemon sends reads as a sentence, never as the code.
        for code in [
            "busy",
            "no_such_answer",
            "not_at_input",
            "no_text_field",
            "empty",
            "too_long",
            "not_a_person",
            "not_watching",
            "skipped",
        ] {
            let said = refusal_words(code, "This Mac", &key);
            assert!(said.ends_with('.') && !said.contains('_'), "{code}: {said}");
        }
    }

    /// Claude Code's Bash question: its "don't ask again" answer is a standing permission,
    /// click only, as an "always" one is.
    #[test]
    fn a_standing_permission_has_no_digit() {
        let menu: AgentMenu = serde_json::from_value(serde_json::json!({
            "question": "Do you want to proceed?",
            "seal": 3,
            "options": [
                {"key": "1", "label": "Yes"},
                {"key": "2", "label": "Yes, and don't ask again for cargo test commands in /srv/repo"},
                {"key": "3", "label": "No, and tell Claude what to do differently (esc)", "tell": true},
            ],
        }))
        .expect("menu");
        let digits: Vec<Option<u8>> = offers(&menu).iter().map(|o| o.digit).collect();
        assert_eq!(digits, [Some(1), None, Some(2), Some(3), Some(4)]);
        assert!(standing("Yes, and don\u{2019}t ask again for this command"));
        assert!(!standing(
            "Yes, allow all edits during this session (shift+tab)"
        ));
    }

    /// The rules card counts a lessons file the way qass writes it.
    #[test]
    fn the_lessons_are_counted_by_their_action() {
        let file = "# Lessons\n\
                    \n\
                    - a release or a tag -> escalate (owner 2026-10-02)\n\
                    - an edit to README in acme-web -> do it (E7)\n\
                    - a delete outside the repo -> ask first (owner 2026-10-03)\n\
                    - a test run in the repo -> do it (owner 2026-10-04)\n\
                    - a dependency bump -> wait (E9)\n\
                    a note that is not a lesson -> do it\n";
        assert_eq!(
            lessons_of(file),
            Lessons {
                yours: 2,
                alone: 2,
                all: 5,
            }
        );
        assert_eq!(lessons_of(""), Lessons::default());
    }

    /// Summer time from 2026-03-29 01:00 UTC: a Central European zone, one hour ahead, then two.
    const SUMMER: i64 = 1_774_746_000;

    fn cet_local(at: i64) -> Naive {
        Naive::from_secs(at + if at >= SUMMER { 7200 } else { 3600 })
    }

    fn cet_unix(wall: Naive) -> i64 {
        let s = wall.secs();
        if s - 7200 >= SUMMER {
            s - 7200
        } else {
            s - 3600
        }
    }

    fn cet() -> Zone {
        Zone {
            local: cet_local,
            unix: cet_unix,
        }
    }

    fn schedule_info(id: i64, name: &str, cron: &str, next_at: i64) -> ScheduleInfo {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "cron": cron, "agent": "claude", "cwd": "/srv/acme-web",
            "prompt": "go", "paused": false, "created_at": 0, "when": "every day 09:00",
            "next_at": next_at,
        }))
        .expect("schedule")
    }

    fn run(
        id: i64,
        schedule: i64,
        started_at: i64,
        result: &str,
        outcome: Option<&str>,
    ) -> ScheduleRun {
        serde_json::from_value(serde_json::json!({
            "id": id, "schedule": schedule, "slot": started_at, "started_at": started_at,
            "terminal": 11, "result": result, "ended_at": outcome.map(|_| started_at + 41 * 60),
            "outcome": outcome,
        }))
        .expect("run")
    }

    /// The coming slots of a 09:00 cron stay at 09:00 on the wall clock across the change to
    /// summer time, so the night it changes is an hour shorter.
    #[test]
    fn coming_slots_keep_the_wall_clock_across_summer_time() {
        let from = 1_774_648_800;
        let s = schedule_info(1, "nightly", "0 9 * * *", 1_774_684_800);
        let slots = coming_slots(&[s], from, from + 3 * 86_400, cet());
        let walls: Vec<String> = slots.iter().map(|(at, _)| cet_local(*at).clock()).collect();
        assert_eq!(walls, ["09:00", "09:00", "09:00"]);
        assert_eq!(slots[1].0 - slots[0].0, 86_400 - 3600, "the short night");
        assert_eq!(slots[2].0 - slots[1].0, 86_400);
    }

    /// The strip: days with runs, today, and the coming days; a failure counted; each run in
    /// words that are never a state word for what is not one.
    #[test]
    fn the_strip_and_the_runs_read_as_the_page_says() {
        let zone = utc();
        let day = 86_400;
        let today = 20_000;
        let runs = [
            run(1, 1, (today - 3) * day + 7200, "started", Some("done")),
            run(2, 1, (today - 1) * day + 7200, "started", Some("failed")),
            run(
                3,
                2,
                (today - 1) * day + 9000,
                "skipped: the tab was busy",
                None,
            ),
            run(4, 2, today * day + 3600, "started", None),
        ];
        let coming = [((today + 2) * day + 7200, 1)];
        let strip = strip_days(&runs, &coming, today, zone, (7, 3));
        let seen: Vec<(i64, usize, usize, usize)> = strip
            .iter()
            .map(|d| (d.day - today, d.runs, d.failed, d.coming))
            .collect();
        assert_eq!(
            seen,
            [(-3, 1, 0, 0), (-1, 2, 1, 0), (0, 1, 0, 0), (2, 0, 0, 1)]
        );
        let words: Vec<String> = runs.iter().map(|r| run_words(r).1).collect();
        assert_eq!(
            words,
            ["Done", "Failed:", "skipped: the tab was busy", "Working"]
        );
        let mut gone = run(5, 1, today * day, "started", Some("gone"));
        assert_eq!(run_words(&gone).1, "tab closed");
        gone.result = "failed: bun install could not reach the registry".into();
        assert_eq!(
            run_words(&gone).1,
            "Failed: bun install could not reach the registry"
        );
        assert_eq!(took(&runs[0]).as_deref(), Some("41 min"));
        assert_eq!(took(&runs[3]), None);
        let schedules = [
            schedule_info(1, "nightly-tests", "0 2 * * *", today * day),
            schedule_info(2, "soak", "0 */2 * * *", today * day),
        ];
        assert_eq!(
            last_run_line(&schedules, &runs[..2], (today - 1) * day + 7200, zone).as_deref(),
            Some("Last: nightly-tests, 02:00, Failed:, tab 11")
        );
        assert_eq!(
            day_label(today),
            format!(
                "{} {}",
                DAYS[(today + 4).rem_euclid(7) as usize],
                Naive::from_secs(today * day).day
            )
        );
    }

    /// The page covers every key the config knows, in its order.
    #[test]
    fn the_page_lists_every_setting() {
        let keys: Vec<&str> = SETTINGS.iter().map(|s| s.key).collect();
        assert_eq!(keys, qoop_tui::config::KEYS);
    }

    /// A value reads as `config::set` takes it, from the file or its default; a write is found
    /// on its line; find matches the name, key or help.
    #[test]
    fn a_setting_reads_its_value_and_its_line() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        let text = "# mine\nprefix = \"ctrl-b\"\n\n[notify]\nsound = true\n# keep\n\n[sleep]\nidle_minutes = 30\ntelemetry = false\n";
        std::fs::write(&path, text).expect("file");
        let read = qoop_tui::config::read_from(&path);
        let blank = qoop_tui::config::read_from(&dir.path().join("none.toml"));
        let value = |key| setting_value(&read, text, key);
        let default = |key| setting_value(&blank, "", key);
        assert_eq!(value("prefix"), "ctrl-b");
        assert_eq!(value("notify.sound"), "true");
        assert_eq!(default("notify.sound"), "false");
        assert_eq!(value("sleep.idle_minutes"), "30");
        assert_eq!(value("theme"), default("theme"));
        assert_eq!(default("telemetry"), "true");
        assert_eq!(line_of(text, "notify.sound"), Some(5));
        assert_eq!(line_of(text, "prefix"), Some(2));
        assert_eq!(line_of(text, "notify.desktop"), None);
        let sound = SETTINGS
            .iter()
            .find(|s| s.key == "notify.sound")
            .expect("sound");
        assert!(setting_found(sound, "SOUND"));
        assert!(setting_found(sound, ""));
        assert!(!setting_found(sound, "prefix"));
    }

    /// A remote silent 15 s goes stale as of when it went quiet; back, it is live again.
    /// This Mac's daemon gone makes every machine stale. The tile and the banner say it in the
    /// same words.
    #[test]
    fn a_silent_machine_goes_stale_after_fifteen_seconds() {
        let zone = utc();
        let mut m = fixture();
        let remote = 1usize;
        let mut reach = Reach::default();
        reach.note(&m.spaces, 1_000);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_000, None),
            MachineState::Live
        );
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Down);
            }
        }
        reach.note(&m.spaces, 1_005);
        reach.note(&m.spaces, 1_009);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_010, None),
            MachineState::Retrying
        );
        let stale = reach.state(remote, &m.spaces, 1_005 + STALE_AFTER, None);
        assert_eq!(stale, MachineState::Stale { as_of: 1_005 });
        assert_eq!(
            reach.state(0, &m.spaces, 1_020, None),
            MachineState::Live,
            "this Mac"
        );
        let words = reach_words(stale, zone);
        assert_eq!(words, format!("not reached, as of {}", clock(1_005, zone)));
        let banner = reach_banner(
            &[
                ("This Mac".into(), MachineState::Live),
                ("staging".into(), stale),
            ],
            zone,
        );
        assert_eq!(banner, Some(format!("staging {words}")));
        // Its rows stay while it is down.
        let rows = m.rows.clone();
        let local: Vec<AgentRow> = rows
            .iter()
            .filter(|r| machine_of(r.workspace) == 0)
            .cloned()
            .collect();
        let kept = unreached_rows(&rows, &local, &m.spaces);
        assert!(!kept.is_empty());
        assert!(kept.iter().all(|r| machine_of(r.workspace) == remote));
        // Back: live again, and its own rows are what counts.
        for w in &mut m.spaces {
            w.link = Some(LinkState::Up);
        }
        reach.note(&m.spaces, 1_030);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_031, None),
            MachineState::Live
        );
        assert!(unreached_rows(&rows, &local, &m.spaces).is_empty());
        // This Mac's daemon gone: every machine stale, as of then.
        assert_eq!(
            reach.state(remote, &m.spaces, 1_040, Some(1_035)),
            MachineState::Stale { as_of: 1_035 }
        );
        assert_eq!(
            reach.state(0, &m.spaces, 1_040, Some(1_035)),
            MachineState::Stale { as_of: 1_035 }
        );
    }

    /// A link that stays up while its daemon says nothing: stale 15 s after the last answer
    /// from that daemon, as of then; an answer brings it back. A parked machine is not asked
    /// and never goes stale this way.
    #[test]
    fn an_up_link_with_a_silent_daemon_goes_stale() {
        let zone = utc();
        let mut m = fixture();
        let remote = 1usize;
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Up);
            }
        }
        let mut reach = Reach::default();
        reach.note(&m.spaces, 1_000);
        assert_eq!(
            reach.state(remote, &m.spaces, 5_000, None),
            MachineState::Live,
            "never heard"
        );
        reach.hear(remote, 1_002);
        reach.note(&m.spaces, 1_010);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_016, None),
            MachineState::Live
        );
        let stale = reach.state(remote, &m.spaces, 1_017, None);
        assert_eq!(stale, MachineState::Stale { as_of: 1_002 });
        assert_eq!(
            reach_banner(&[("staging".into(), stale)], zone),
            Some(format!("staging not reached, as of {}", clock(1_002, zone)))
        );
        reach.hear(remote, 1_020);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_021, None),
            MachineState::Live
        );
        // The link drops 10 s after its last answer: stale 5 s later, as of that answer.
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Down);
            }
        }
        reach.note(&m.spaces, 1_030);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_034, None),
            MachineState::Retrying
        );
        assert_eq!(
            reach.state(remote, &m.spaces, 1_035, None),
            MachineState::Stale { as_of: 1_020 }
        );
        // Back up: heard again as it comes up.
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Up);
            }
        }
        reach.note(&m.spaces, 1_100);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_101, None),
            MachineState::Live
        );
        // Parked: let go, never asked, never stale.
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Parked);
            }
        }
        reach.note(&m.spaces, 1_200);
        assert_eq!(
            reach.state(remote, &m.spaces, 9_000, None),
            MachineState::Parked
        );
    }

    /// A question after the one answered is a new menu; the same menu read again is not.
    #[test]
    fn a_new_menu_is_one_with_another_seal() {
        let mut before = fixture().rows;
        before[1].menu = Some(edit_menu(7));
        let mut now = before.clone();
        assert!(new_menus(&before, &now).is_empty(), "the same menu");
        now[1].menu = Some(edit_menu(8));
        assert_eq!(new_menus(&before, &now), vec![now[1].terminal]);
        now[1].menu = None;
        assert!(new_menus(&before, &now).is_empty(), "no menu now");
    }

    /// A request is another machine's by the terminal or the space it names.
    #[test]
    fn a_request_goes_to_the_machine_it_names() {
        use qoop_runtime::control::ControlRequest;
        let remote = 1i64 << 40 | 1;
        let via = ControlRequest::Via {
            id: "v".into(),
            workspace: remote,
            request: Box::new(ControlRequest::Pet { id: "p".into() }),
        };
        assert_eq!(request_machine(&via), 1);
        let read = ControlRequest::Scrollback {
            id: "s".into(),
            terminal: 0x8100_0001,
            up: 0,
            rows: 10,
        };
        assert_eq!(request_machine(&read), 1);
        let here = ControlRequest::ListAgents { id: "a".into() };
        assert_eq!(request_machine(&here), 0);
    }

    /// A machine already down when the window opened has no time to give: "not reached since
    /// the window opened" until it is reached once.
    #[test]
    fn a_machine_down_from_the_start_has_no_as_of() {
        let zone = utc();
        let mut m = fixture();
        let remote = 1usize;
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Down);
            }
        }
        let mut reach = Reach::default();
        reach.note(&m.spaces, 1_000);
        let state = reach.state(remote, &m.spaces, 1_000 + STALE_AFTER, None);
        assert_eq!(state, MachineState::NeverReached);
        assert_eq!(
            reach_words(state, zone),
            "not reached since the window opened"
        );
        assert_eq!(
            reach_banner(&[("staging".into(), state)], zone),
            Some("staging not reached since the window opened".into())
        );
        // Reached once, then lost: a real time again.
        for w in &mut m.spaces {
            w.link = Some(LinkState::Up);
        }
        reach.note(&m.spaces, 1_100);
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Down);
            }
        }
        reach.note(&m.spaces, 1_200);
        assert_eq!(
            reach.state(remote, &m.spaces, 1_200 + STALE_AFTER, None),
            MachineState::Stale { as_of: 1_200 }
        );
    }

    /// A machine not heard from yet: the spaces in are named, and whose rows come when.
    #[test]
    fn loading_names_what_is_in_and_what_comes() {
        let mut m = fixture();
        let remote = 1usize;
        assert_eq!(loading_line(&m.spaces, &m.rows), None);
        for w in &mut m.spaces {
            if machine_of(w.id) == remote {
                w.link = Some(LinkState::Down);
                w.name = format!("{} (connecting)", w.name);
            }
        }
        let local: Vec<AgentRow> = m
            .rows
            .iter()
            .filter(|r| machine_of(r.workspace) == 0)
            .cloned()
            .collect();
        let line = loading_line(&m.spaces, &local).expect("a line");
        let machine = m
            .spaces
            .iter()
            .find(|w| machine_of(w.id) == remote)
            .map(machine_name)
            .unwrap_or_default();
        assert!(
            line.ends_with(&format!("rows come when {machine} answers")),
            "{line}"
        );
        assert!(!line.contains("connecting"), "{line}");
        assert_eq!(
            and_list(&["a".into(), "b".into(), "c".into()]),
            "a, b and c"
        );
    }

    /// A menu that closed with an answer from the phone keeps its card, with who answered.
    #[test]
    fn a_question_answered_on_the_phone_says_so() {
        let zone = utc();
        let m = fixture();
        let mut before = m.rows[1].clone();
        before.menu = Some(edit_menu(7));
        let mut now = before.clone();
        now.menu = None;
        assert_eq!(
            answered_elsewhere(&before, &now, zone),
            None,
            "not answered"
        );
        now.answered = Some(qoop_runtime::control::Answered {
            label: "Yes".into(),
            by: "phone".into(),
            at: 1_000,
        });
        assert_eq!(
            answered_elsewhere(&before, &now, zone),
            Some(format!(
                "Answered on your iPhone: Yes, {}",
                clock(1_000, zone)
            ))
        );
    }
}

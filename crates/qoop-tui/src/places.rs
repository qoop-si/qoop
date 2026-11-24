//! The places beside the Inbox that take the pane the same way it does: qass's ledger, a
//! computer that is not reached, and the box that starts a new agent. Each is plain values and
//! functions that paint into [`crate::inbox_pane::Row`]s, so a test reads the rows it painted.
//!
//! Text is never cut: everything wraps whole, and a line that has several candidates takes the
//! first whole one that fits.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use qoop_runtime::control::{AgentRow, LinkState, QassAct, ScheduleInfo, WorkspaceRow};

use crate::answers::Lessons;
use crate::board::Board;
use crate::inbox_pane::{Env, Hit, PlaceHit, Row, Unit, View, clock, flow, paint, unit, wrapped};
use crate::ledger::Ledger;
use crate::mux::machine_of;
use crate::theme::Role;

/// The key qass's own record is kept under in [`Reach`]: no machine has this number.
pub const QASS: usize = usize::MAX;

/// When this window last saw each machine (and qass) reached: the client-side record every
/// "not reached since 16:58" line is built from, since the daemon sends no time for it. A
/// machine never seen reached in this window has none, and its lines say so in words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    seen: BTreeMap<usize, i64>,
}

impl Reach {
    /// Note what the spaces say now: every other machine whose link is up is reached at
    /// `now`. Called with each new list and before each frame, so the time kept is the last
    /// one the machine answered.
    pub fn note(&mut self, spaces: &[WorkspaceRow], now: i64) {
        for space in spaces {
            let machine = machine_of(space.id);
            if machine != 0 && space.link == Some(LinkState::Up) {
                self.seen.insert(machine, now);
            }
        }
    }

    /// Note that qass runs now.
    pub fn note_qass(&mut self, running: bool, now: i64) {
        if running {
            self.seen.insert(QASS, now);
        }
    }

    /// The time a machine (or qass) was last seen reached, "16:58".
    pub fn last(&self, key: usize) -> Option<String> {
        self.seen.get(&key).map(|at| clock(*at))
    }

    /// "not reached since 16:58", or since this window opened when it was never seen reached.
    pub fn not_reached(&self, machine: usize) -> String {
        match self.last(machine) {
            Some(at) => format!("not reached since {at}"),
            None => "not reached since this window opened".to_string(),
        }
    }

    /// "as of 16:58", the time a list of its agents is as of; empty when never seen reached.
    pub fn as_of(&self, machine: usize) -> String {
        self.last(machine)
            .map_or_else(String::new, |at| format!("as of {at}"))
    }
}

/// The record the whole window shares, for the column and the Inbox's banner, which are painted
/// from places that hold no state of their own.
// ponytail: process-wide, the interface has one render loop (as `mute_now` and `FILES`).
static REACH: Mutex<Reach> = Mutex::new(Reach {
    seen: BTreeMap::new(),
});

/// Note the machines reached now in the shared record.
pub fn note_reach(spaces: &[WorkspaceRow], now: i64, qass_running: bool) {
    if cfg!(test) {
        return;
    }
    let mut reach = REACH.lock().unwrap_or_else(PoisonError::into_inner);
    reach.note(spaces, now);
    reach.note_qass(qass_running, now);
}

/// The shared record as it stands. Empty under test, so a paint never depends on another test.
pub fn reach_now() -> Reach {
    if cfg!(test) {
        return Reach::default();
    }
    REACH.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// What the places beside the Inbox read besides the rows and the cards: the board, what qass
/// did today, the schedules, qass's rules, when machines were last reached, and the agents that
/// can start in a new worktree. Borrowed from the client, field by field.
#[derive(Clone, Copy)]
pub struct Beside<'a> {
    pub board: &'a Board,
    /// What qass did today, newest first; `None` until read.
    pub acts: Option<&'a [QassAct]>,
    /// Why the acts could not be read, and whether the daemon is too old to have them.
    pub acts_failed: Option<(&'a str, bool)>,
    /// The schedules, `None` until read (or from a daemon too old to have them).
    pub schedules: Option<&'a [ScheduleInfo]>,
    /// Qass's rules as its lessons file counts them; `None` where the file cannot be read.
    pub rules: Option<Lessons>,
    pub reach: &'a Reach,
    /// The agents that can start in a worktree (name, command).
    pub offers: &'a [(String, String)],
}

/// A place beside the Inbox that has the pane.
#[derive(Debug)]
pub enum Place {
    /// qass's ledger.
    Ledger(Ledger),
    /// A computer, by its machine number.
    Computer(Computer),
    /// The box that starts a new agent.
    New(NewWork),
}

/// qass's rules as the lessons file counts them, `None` when no file can be read: no
/// lessons file at all is an empty count, an unreadable one is none. At most 1 MB is read.
/// Never under test, so the suite never depends on the developer's own qass.
pub fn rules_read() -> Option<Lessons> {
    if cfg!(test) {
        return None;
    }
    rules_read_at(&qoop_runtime::paths::qass_config_dir().join("lessons.md"))
}

/// The rules in the lessons file at `path`: a file that is not there is an empty count (qass
/// has taught nobody anything yet), one that cannot be read is `None`. Only the first 1 MiB is
/// read. The one reader the TUI's ledger and the desktop's qass page share.
pub fn rules_read_at(path: &std::path::Path) -> Option<Lessons> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Lessons::default()),
        Err(_) => return None,
    };
    let mut text = String::new();
    file.take(1 << 20).read_to_string(&mut text).ok()?;
    Some(crate::answers::lessons_of(&text))
}

/// What a key on a place asks of the loop that owns the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceOut {
    /// Taken: repaint.
    Stay,
    /// The place is put away.
    Closed,
    /// Go to this agent's tab.
    Jump(u32),
    /// Turn qass's watching over: Pause, or Resume.
    Watch,
    /// Start qass (or wake it).
    StartQass,
    /// Open the Inbox on its From qass filter.
    Questions,
    /// Ask this computer for its agents again, which tries a link that is not up.
    Retry(usize),
    /// Send a line to qass's input.
    Ask(String),
    /// Start a new agent: the line typed into a new shell in `space`, tab named `name`.
    Start {
        line: String,
        space: Option<i64>,
        name: String,
    },
}

/// A key as a place reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum K {
    Esc,
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Enter,
    Back,
    Clear,
    Char(char),
}

/// The keys in `bytes`: arrows, tab, enter, backspace and characters. A paste's newlines are
/// spaces, never an enter.
pub fn keys_of(bytes: &[u8]) -> Vec<K> {
    let pasted = bytes.starts_with(b"\x1b[200~");
    let body: &[u8] = if pasted {
        let inner = bytes.strip_prefix(b"\x1b[200~").unwrap_or(bytes);
        inner.strip_suffix(b"\x1b[201~").unwrap_or(inner)
    } else {
        bytes
    };
    if pasted {
        return String::from_utf8_lossy(body)
            .chars()
            .map(|c| K::Char(if c.is_control() { ' ' } else { c }))
            .collect();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        let (key, used) = if rest.starts_with(b"\x1b[A") || rest.starts_with(b"\x1bOA") {
            (Some(K::Up), 3)
        } else if rest.starts_with(b"\x1b[B") || rest.starts_with(b"\x1bOB") {
            (Some(K::Down), 3)
        } else if rest.starts_with(b"\x1b[C") || rest.starts_with(b"\x1bOC") {
            (Some(K::Right), 3)
        } else if rest.starts_with(b"\x1b[D") || rest.starts_with(b"\x1bOD") {
            (Some(K::Left), 3)
        } else if rest.starts_with(b"\x1b[Z") {
            (Some(K::BackTab), 3)
        } else if rest == b"\x1b" {
            (Some(K::Esc), 1)
        } else if rest[0] == 0x1b {
            // Some other sequence: not a letter, and the rest of the read is not read.
            return out;
        } else {
            match rest[0] {
                b'\t' => (Some(K::Tab), 1),
                b'\r' | b'\n' => (Some(K::Enter), 1),
                0x7f | 0x08 => (Some(K::Back), 1),
                0x15 => (Some(K::Clear), 1),
                b if b < 0x20 => (None, 1),
                b if b < 0x80 => (Some(K::Char(b as char)), 1),
                _ => {
                    let len = std::str::from_utf8(&rest[..rest.len().min(4)])
                        .or_else(|e| std::str::from_utf8(&rest[..e.valid_up_to()]))
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map_or(1, char::len_utf8);
                    let c = String::from_utf8_lossy(&rest[..len]).chars().next();
                    (c.map(K::Char), len)
                }
            }
        };
        i += used;
        out.extend(key);
    }
    out
}

/// The computers a name stands for: "This Mac", or the machine before the colon of a remote
/// space's name.
/// The computers the spaces are on, This Mac first, each once.
pub fn computers(spaces: &[WorkspaceRow]) -> Vec<usize> {
    let mut out = vec![0];
    for s in spaces {
        let m = machine_of(s.id);
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// The computer after `at` in [`computers`], round to This Mac; `at` itself when there is only
/// one.
pub fn next_computer(spaces: &[WorkspaceRow], at: usize) -> usize {
    let all = computers(spaces);
    let i = all.iter().position(|m| *m == at).unwrap_or(0);
    all[(i + 1) % all.len()]
}

/// A space on `machine`, through which a request is carried to it (`Via`), if it has one.
pub fn space_on(spaces: &[WorkspaceRow], machine: usize) -> Option<i64> {
    spaces
        .iter()
        .find(|s| machine_of(s.id) == machine)
        .map(|s| s.id)
}

pub fn machine_name(spaces: &[WorkspaceRow], machine: usize) -> String {
    if machine == 0 {
        return "This Mac".to_string();
    }
    spaces
        .iter()
        .find(|s| machine_of(s.id) == machine)
        .map(|s| {
            s.name
                .split_once(':')
                .map_or(s.name.clone(), |(m, _)| m.to_string())
        })
        .unwrap_or_else(|| "a computer".to_string())
}

// A computer ------------------------------------------------------------------------------

/// A computer's own screen: how it is reached in words, when it last was, `r` to try again,
/// and its agents as they were when it last answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Computer {
    pub machine: usize,
    /// The first body row shown, and the most it can be, as the ledger scrolls.
    offset: std::cell::Cell<usize>,
    max: std::cell::Cell<usize>,
}

impl Computer {
    pub fn new(machine: usize) -> Computer {
        Computer {
            machine,
            offset: std::cell::Cell::new(0),
            max: std::cell::Cell::new(0),
        }
    }

    /// The wheel: three rows a notch.
    pub fn scroll(&self, down: bool) {
        let off = self.offset.get();
        self.offset.set(if down {
            (off + 3).min(self.max.get())
        } else {
            off.saturating_sub(3)
        });
    }

    pub fn key(&mut self, bytes: &[u8]) -> PlaceOut {
        for key in keys_of(bytes) {
            match key {
                K::Esc => return PlaceOut::Closed,
                K::Char('r') => return PlaceOut::Retry(self.machine),
                K::Char('j') | K::Down => {
                    self.offset.set((self.offset.get() + 1).min(self.max.get()));
                }
                K::Char('k') | K::Up => self.offset.set(self.offset.get().saturating_sub(1)),
                _ => {}
            }
        }
        PlaceOut::Stay
    }

    pub fn click(&mut self, hit: PlaceHit) -> PlaceOut {
        match hit {
            PlaceHit::Retry => PlaceOut::Retry(self.machine),
            PlaceHit::Cancel => PlaceOut::Closed,
            _ => PlaceOut::Stay,
        }
    }

    /// The machine's link, and why it is down, as its spaces say.
    fn link(&self, env: &Env) -> (Option<LinkState>, Option<String>) {
        env.spaces
            .iter()
            .find(|s| machine_of(s.id) == self.machine)
            .map_or((None, None), |s| (s.link, s.link_reason.clone()))
    }

    pub fn view(&self, env: &Env, cols: usize, rows: usize) -> View {
        let name = machine_name(env.spaces, self.machine);
        let (link, reason) = self.link(env);
        let reach = env.beside.reach;
        let mut out: Vec<Row> = Vec::new();
        let (mark, ink, said) = match (self.machine, link) {
            (0, _) => ("\u{25cf}", Role::Done, "this computer".to_string()),
            (_, Some(LinkState::Down)) => {
                ("\u{25c6}", Role::Failed, reach.not_reached(self.machine))
            }
            (_, Some(LinkState::Parked)) => (
                "\u{25cb}",
                Role::FaintText,
                "not connected: let go while nothing ran there".to_string(),
            ),
            _ => ("\u{25cf}", Role::Done, "online".to_string()),
        };
        out.extend(wrapped(
            &format!("{mark} {name}  {said}"),
            1,
            cols,
            ink,
            true,
        ));
        if link == Some(LinkState::Down)
            && let Some(reason) = reason.filter(|r| !r.is_empty())
        {
            out.extend(wrapped(&reason, 3, cols, Role::FaintText, false));
        }
        out.push(Row::blank());
        let agents: Vec<&AgentRow> = env
            .rows
            .iter()
            .filter(|r| r.agent && machine_of(r.workspace) == self.machine)
            .collect();
        let as_of = reach.as_of(self.machine);
        let title = if link == Some(LinkState::Down) && !as_of.is_empty() {
            format!("its agents, {as_of} {}", agents.len())
        } else {
            format!("its agents {}", agents.len())
        };
        out.extend(wrapped(&title, 1, cols, Role::Title, true));
        if agents.is_empty() {
            out.extend(wrapped(
                "no agent was listed on it",
                3,
                cols,
                Role::FaintText,
                false,
            ));
        }
        for row in agents {
            let state = crate::sidebar::shown_state(row);
            let words = match state {
                "blocked" => "needs you",
                "failed" => "failed",
                "working" | "starting" => "working",
                "done" => "finished",
                "idle" => "idle",
                _ => "not read yet",
            };
            let space = env.space_name(row.workspace);
            let mut line = wrapped(
                &format!(
                    "{} / {}  {words}",
                    space
                        .split_once(':')
                        .map_or(space.as_str(), |(_, rest)| rest),
                    crate::sidebar::label(row)
                ),
                3,
                cols,
                Role::Text,
                false,
            );
            for r in &mut line {
                r.hits.push((0, cols, Hit::Go(row.terminal)));
            }
            out.extend(line);
        }
        if link == Some(LinkState::Down) {
            out.push(Row::blank());
            out.extend(wrapped(
                "answering and typing open again when it is back",
                1,
                cols,
                Role::FaintText,
                false,
            ));
        }
        let mut foot = Vec::new();
        if self.machine != 0 && link.is_some() && link != Some(LinkState::Up) {
            // A machine let go while nothing ran there is connected, not retried.
            let word = if link == Some(LinkState::Parked) {
                "r connect"
            } else {
                "r retry now"
            };
            foot.push(unit(Role::Focused, word, Some(Hit::Place(PlaceHit::Retry))));
        }
        foot.push(unit(Role::FaintText, "j k scroll", None));
        foot.push(unit(
            Role::FaintText,
            "esc back",
            Some(Hit::Place(PlaceHit::Cancel)),
        ));
        let (view, max) = finish(out, foot, cols, rows, self.offset.get());
        self.max.set(max);
        self.offset.set(self.offset.get().min(max));
        view
    }
}

/// A place painted: a rule, its body from row `offset`, and its footer pinned to the bottom.
/// Nothing is cut: a body that does not fit goes off the bottom a whole row at a time and the
/// rule says more is below. Answers the view and the most the body can be scrolled.
pub(crate) fn finish(
    body: Vec<Row>,
    foot: Vec<Unit>,
    cols: usize,
    rows: usize,
    offset: usize,
) -> (View, usize) {
    let foot_rows = flow(foot, 1, 2, cols);
    let height = rows.saturating_sub(foot_rows.len() + 1).max(1);
    let max = body.len().saturating_sub(height);
    let off = offset.min(max);
    let more = match (off > 0, off < max) {
        (true, true) => "more above and below",
        (true, false) => "more above",
        (false, true) => "more below",
        (false, false) => "",
    };
    let mut shown: Vec<Row> = vec![crate::inbox_pane::rule_row(cols, more)];
    shown.extend(
        body.into_iter()
            .skip(off)
            .chain(std::iter::repeat(Row::blank()))
            .take(height),
    );
    shown.extend(foot_rows);
    shown.truncate(rows.max(1));
    (paint(&shown, cols, None), max)
}

// A new agent -----------------------------------------------------------------------------

/// The fields of the new agent box, in tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Task,
    Agent,
    Space,
    Branch,
}

const FIELDS: [Field; 4] = [Field::Task, Field::Agent, Field::Space, Field::Branch];

/// The box that starts an agent in a new worktree: the task, which agent, which space (and so
/// which computer), and the branch. Start types `qoop worktree new <branch> --space N --
/// <agent> '<task>' && exit` into a new shell in that space, so the CLI does every check and
/// the agent opens in a tab of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWork {
    task: String,
    /// The branch the person typed; empty while it follows the task.
    branch: String,
    field: Field,
    agent: usize,
    space: usize,
    /// The agents offered (name, command), fixed when the box opened.
    agents: Vec<(String, String)>,
    /// The spaces offered (id, label), fixed when the box opened; none on a first run.
    spaces: Vec<(i64, String)>,
    /// When the box opened: a task whose words give no branch is named after it.
    opened: i64,
    /// Why the last start did not go.
    refused: Option<String>,
}

impl NewWork {
    /// A new box. `offers` are the agents that can start in a worktree; the spaces are every
    /// one but qass's own, `default` first when it is among them.
    pub fn open(env: &Env, default: Option<i64>) -> NewWork {
        let spaces: Vec<(i64, String)> = env
            .spaces
            .iter()
            .filter(|s| !env.ctx.qass_spaces.contains(&s.id))
            .map(|s| {
                let machine = machine_of(s.id);
                let name = s
                    .name
                    .split_once(':')
                    .map_or(s.name.clone(), |(_, rest)| rest.to_string());
                (
                    s.id,
                    if machine == 0 {
                        format!("This Mac / {name}")
                    } else {
                        format!("{} / {name}", machine_name(env.spaces, machine))
                    },
                )
            })
            .collect();
        let space = default
            .and_then(|d| spaces.iter().position(|(id, _)| *id == d))
            .unwrap_or(0);
        NewWork {
            task: String::new(),
            branch: String::new(),
            field: Field::Task,
            agent: 0,
            space,
            agents: env.beside.offers.to_vec(),
            spaces,
            opened: env.now_unix,
            refused: None,
        }
    }

    /// The branch as it will be made: the typed one, else one named after the task.
    pub fn branch(&self) -> String {
        if self.branch.is_empty() {
            crate::answers::branch_or_clock(&crate::answers::clean_prompt(&self.task), self.opened)
        } else {
            self.branch.clone()
        }
    }

    /// The space the agent will start in, when there is one.
    pub fn space(&self) -> Option<i64> {
        self.spaces.get(self.space).map(|(id, _)| *id)
    }

    /// The line typed into the new shell, or why there is none.
    pub fn line(&self) -> Result<(String, Option<i64>), String> {
        let (_, command) = self
            .agents
            .get(self.agent)
            .ok_or("No agent can start in a worktree here.")?;
        let space = self.spaces.get(self.space).map(|(id, _)| *id);
        crate::answers::worktree_line(
            &self.branch(),
            space.map(crate::answers::daemon_space),
            command,
            &self.task,
        )
        .map(|line| (line, space))
        .map_err(str::to_string)
    }

    /// Start: the line, or the reason it cannot be, kept on the box.
    fn submit(&mut self) -> PlaceOut {
        match self.line() {
            Ok((line, space)) => PlaceOut::Start {
                line,
                space,
                name: self.branch(),
            },
            Err(why) => {
                self.refused = Some(why);
                PlaceOut::Stay
            }
        }
    }

    fn step(&mut self, back: bool) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0);
        let to = if back {
            (at + FIELDS.len() - 1) % FIELDS.len()
        } else {
            (at + 1) % FIELDS.len()
        };
        self.field = FIELDS[to];
    }

    fn cycle(&mut self, back: bool) {
        let (len, at) = match self.field {
            Field::Agent => (self.agents.len(), &mut self.agent),
            Field::Space => (self.spaces.len(), &mut self.space),
            _ => return,
        };
        if len > 0 {
            *at = if back {
                (*at + len - 1) % len
            } else {
                (*at + 1) % len
            };
        }
    }

    fn type_in(&mut self, c: char) {
        let most = crate::answers::PROMPT_MOST;
        let text = match self.field {
            Field::Task => &mut self.task,
            Field::Branch => &mut self.branch,
            _ => return,
        };
        if text.chars().count() < most {
            text.push(c);
        }
        self.refused = None;
    }

    pub fn key(&mut self, bytes: &[u8]) -> PlaceOut {
        for key in keys_of(bytes) {
            match key {
                K::Esc => return PlaceOut::Closed,
                K::Enter => return self.submit(),
                K::Tab => self.step(false),
                K::BackTab => self.step(true),
                K::Left | K::Up if matches!(self.field, Field::Agent | Field::Space) => {
                    self.cycle(true);
                }
                K::Right | K::Down if matches!(self.field, Field::Agent | Field::Space) => {
                    self.cycle(false);
                }
                K::Back => {
                    match self.field {
                        Field::Task => self.task.pop(),
                        Field::Branch => self.branch.pop(),
                        _ => None,
                    };
                    self.refused = None;
                }
                K::Clear => {
                    match self.field {
                        Field::Task => self.task.clear(),
                        Field::Branch => self.branch.clear(),
                        _ => {}
                    }
                    self.refused = None;
                }
                K::Char(' ') | K::Char('j' | 'k' | 'h' | 'l')
                    if matches!(self.field, Field::Agent | Field::Space) =>
                {
                    self.cycle(matches!(key, K::Char('k' | 'h')));
                }
                K::Char(c) => self.type_in(c),
                _ => {}
            }
        }
        PlaceOut::Stay
    }

    pub fn click(&mut self, hit: PlaceHit) -> PlaceOut {
        match hit {
            PlaceHit::Submit => self.submit(),
            PlaceHit::Cancel => PlaceOut::Closed,
            _ => PlaceOut::Stay,
        }
    }

    pub fn view(&self, cols: usize, rows: usize) -> View {
        let mut out: Vec<Row> = Vec::new();
        out.extend(wrapped("new agent", 1, cols, Role::Title, true));
        out.push(Row::blank());
        let on = |f: Field| self.field == f;
        let (agent, space) = (
            self.agents
                .get(self.agent)
                .map_or("none can start here".to_string(), |(n, _)| n.clone()),
            self.spaces
                .get(self.space)
                .map_or("the first space".to_string(), |(_, l)| l.clone()),
        );
        let field = |f: Field, label: &str, value: String, hint: &str| -> Vec<Row> {
            wrapped(
                &format!("{label:<7} {value}{}", if on(f) { hint } else { "" }),
                1,
                cols,
                if on(f) { Role::Focused } else { Role::Text },
                on(f),
            )
        };
        out.extend(field(Field::Space, "space", space, "  (left right change)"));
        out.extend(field(Field::Agent, "agent", agent, "  (left right change)"));
        let auto = self.branch.is_empty();
        out.extend(field(
            Field::Branch,
            "branch",
            format!(
                "{}{}",
                if auto && !on(Field::Branch) {
                    "new worktree "
                } else {
                    ""
                },
                self.branch()
            ),
            if on(Field::Branch) && auto {
                "  (type to rename)"
            } else {
                ""
            },
        ));
        if let Some(why) = &self.refused {
            out.push(Row::blank());
            out.extend(wrapped(why, 1, cols, Role::Failed, true));
        }
        // The task last: it is the one field that grows, so what follows it never goes out of
        // sight, and while it has the keys the view shows its end.
        out.push(Row::blank());
        out.extend(wrapped(
            "what should the agent do?",
            1,
            cols,
            if on(Field::Task) {
                Role::Focused
            } else {
                Role::FaintText
            },
            on(Field::Task),
        ));
        let shown = if on(Field::Task) {
            format!("{}\u{258c}", self.task)
        } else {
            self.task.clone()
        };
        if shown.is_empty() {
            out.push(Row::plain(Role::FaintText, "   type the task"));
        } else {
            out.extend(wrapped(&shown, 3, cols, Role::Text, false));
        }
        let foot = vec![
            unit(Role::FaintText, "tab next field", None),
            unit(
                Role::Focused,
                "enter starts",
                Some(Hit::Place(PlaceHit::Submit)),
            ),
            unit(
                Role::FaintText,
                "esc cancels",
                Some(Hit::Place(PlaceHit::Cancel)),
            ),
        ];
        finish(
            out,
            foot,
            cols,
            rows,
            if on(Field::Task) { usize::MAX } else { 0 },
        )
        .0
    }
}

/// A small world the places are drawn in under test: the design's records (a blocked agent, a
/// hand to qass, qass's own card, a machine that is not reached), qass's acts and schedules and
/// a board, owned together so one `Env` can borrow them.
#[cfg(test)]
pub(crate) mod fixture {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use qoop_runtime::control::{AgentRow, AskInfo, QassAct, ScheduleInfo, WorkspaceRow};

    use super::{Beside, Reach};
    use crate::answers::{HandInput, Lessons};
    use crate::board::{Board, Read};
    use crate::inbox::Context;
    use crate::inbox_pane::Env;

    pub(crate) const REMOTE: i64 = 1 << 40;

    pub(crate) struct World {
        pub rows: Vec<AgentRow>,
        pub spaces: Vec<WorkspaceRow>,
        pub cards: Vec<AskInfo>,
        pub hand: HandInput,
        pub ctx: Context,
        pub typed: BTreeMap<u32, Instant>,
        pub board: Board,
        pub acts: Option<Vec<QassAct>>,
        pub schedules: Option<Vec<ScheduleInfo>>,
        pub rules: Option<Lessons>,
        pub reach: Reach,
        pub offers: Vec<(String, String)>,
    }

    fn json<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
        serde_json::from_value(v).expect("a record")
    }

    pub(crate) fn space(id: i64, name: &str, link: Option<&str>) -> WorkspaceRow {
        let mut v = serde_json::json!({"id": id, "name": name, "terminals": 1});
        if let Some(link) = link {
            v["link"] = link.into();
            if link == "down" {
                v["link_reason"] =
                    "offline: ssh: connect to host build-node: Operation timed out".into();
            }
        }
        json(v)
    }

    pub(crate) fn agent(terminal: u32, workspace: i64, name: &str, state: &str) -> AgentRow {
        let mut row: AgentRow = json(serde_json::json!({
            "terminal": terminal, "workspace": workspace, "program": "claude", "behind": null,
            "state": state, "generation": 1, "agent": true, "name": name,
        }));
        row.status = Some(state.to_string());
        row
    }

    fn card() -> AskInfo {
        json(serde_json::json!({
            "card": "E7", "agent": null, "terminal": null, "status": "open",
            "opened_at": 1_763_000_000, "closed_at": null, "answer": null, "option": null,
            "note": null, "task": null, "item": null,
            "body": {
                "handle": "release", "urgency": "later", "asked": null,
                "doing": "Rust 1.100 toolchain bump and CI cache",
                "why": "the release waits on it",
                "asks": "Merge the toolchain bump, then cut the release?",
                "options": [
                    {"key": "a", "label": "Merge, then release", "reason": "CI is green", "words": "Merge it, then release."},
                    {"key": "b", "label": "Merge only", "reason": "keep the release for later", "words": "Merge it and stop."}
                ],
                "pick": null
            }
        }))
    }

    fn act(at: i64, kind: &str, text: &str) -> QassAct {
        json(serde_json::json!({"at": at, "kind": kind, "text": text}))
    }

    fn schedule(id: i64, name: &str, when: &str, next: Option<i64>, paused: bool) -> ScheduleInfo {
        json(serde_json::json!({
            "id": id, "name": name, "agent": "claude", "cwd": "/tmp", "prompt": "run it",
            "paused": paused, "created_at": 1, "when": when, "next_at": next,
        }))
    }

    /// Items as the daemon sends a board: two wait on the person, one is doing, one todo, one done.
    pub(crate) fn board_items() -> Vec<qoop_runtime::control::ItemInfo> {
        let item =
            |id: i64, key: &str, title: &str, state: &str, at: i64, extra: serde_json::Value| {
                let mut v = serde_json::json!({
                    "id": id, "key": key, "workspace": null, "title": title, "state": state,
                    "steps_done": 0, "steps_total": 0, "agents": [], "updated_at": at,
                });
                for (k, val) in extra.as_object().cloned().unwrap_or_default() {
                    v[k] = val;
                }
                json(v)
            };
        vec![
            item(
                1,
                "QOOP-73",
                "Schedules inside the qass tab",
                "todo",
                50,
                serde_json::json!({"plan": "asked"}),
            ),
            item(
                2,
                "QOOP-68",
                "Rust 1.100 toolchain bump and CI cache",
                "doing",
                40,
                serde_json::json!({"open_asks": 1}),
            ),
            item(
                3,
                "QOOP-71",
                "qoop UI redesign and desktop input",
                "doing",
                30,
                serde_json::json!({"steps_done": 2, "steps_total": 4}),
            ),
            item(
                4,
                "QOOP-72",
                "Phone: the agents list back as the home",
                "todo",
                20,
                serde_json::json!({}),
            ),
            item(
                5,
                "QOOP-66",
                "Nightly crw run report",
                "done",
                10,
                serde_json::json!({"closed_at": 12, "outcome": "verified 05:12"}),
            ),
        ]
    }

    /// The world: This Mac with qass running and watching, one machine not reached.
    pub(crate) fn world() -> World {
        let spaces = vec![
            space(1, "qoop", None),
            space(2, "crw", None),
            space(3, "qass", None),
            space(REMOTE | 1, "build-node:eval", Some("down")),
        ];
        let mut rows = vec![
            agent(10, 3, "qass", "idle"),
            agent(1, 1, "microphone", "blocked"),
            agent(2, 2, "seo plan", "working"),
            agent(3, 2, "credit system", "blocked"),
            agent(
                crate::mux::REMOTE_TERMINAL | 4,
                REMOTE | 1,
                "eval classifier",
                "working",
            ),
        ];
        rows[3].with_qass = true;
        let hand = HandInput {
            qass: Some((10, Some(1))),
            watching: Some(true),
            ..HandInput::default()
        };
        let ctx = Context {
            day_start: 1_762_990_000,
            lost: std::collections::BTreeSet::from([1]),
            qass_spaces: std::collections::BTreeSet::from([3]),
            ..Context::default()
        };
        World {
            rows,
            spaces,
            cards: vec![card()],
            hand,
            ctx,
            typed: BTreeMap::new(),
            board: Board {
                read: Read::Items(board_items()),
                stale: false,
            },
            acts: Some(vec![
                act(1_763_000_900, "wrote", "wrote to T4"),
                act(1_763_000_400, "verified", "verified task 7"),
                act(1_763_000_100, "closed", "closed E3"),
            ]),
            schedules: Some(vec![
                schedule(
                    1,
                    "nightly-qoop",
                    "every day 05:00",
                    Some(1_763_100_000),
                    false,
                ),
                schedule(
                    2,
                    "qass-learn",
                    "every day 04:30",
                    Some(1_763_098_000),
                    false,
                ),
                schedule(3, "weekly-report", "mondays 08:30", None, true),
            ]),
            rules: Some(Lessons {
                yours: 3,
                alone: 8,
                all: 12,
            }),
            reach: Reach::default(),
            offers: vec![
                ("claude".into(), "claude".into()),
                ("codex".into(), "codex".into()),
            ],
        }
    }

    impl World {
        /// Run `f` with an `Env` over this world.
        pub(crate) fn with<R>(&self, f: impl FnOnce(&Env) -> R) -> R {
            let beside = Beside {
                board: &self.board,
                acts: self.acts.as_deref(),
                acts_failed: None,
                schedules: self.schedules.as_deref(),
                rules: self.rules,
                reach: &self.reach,
                offers: &self.offers,
            };
            let env = Env {
                rows: &self.rows,
                spaces: &self.spaces,
                cards: &self.cards,
                hand: &self.hand,
                ctx: &self.ctx,
                now: Instant::now(),
                now_unix: 1_763_001_000,
                typed: &self.typed,
                beside: &beside,
            };
            f(&env)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaces_json(v: serde_json::Value) -> Vec<WorkspaceRow> {
        serde_json::from_value(v).expect("spaces")
    }

    fn space(id: i64, name: &str, link: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({"id": id, "name": name, "terminals": 0});
        if let Some(link) = link {
            v["link"] = link.into();
            v["link_reason"] = "offline: ssh: connection timed out".into();
        }
        v
    }

    /// The as-of words: a machine seen reached at 16:58 and then lost says since then; one never
    /// seen reached in this window says since this window opened.
    #[test]
    fn the_as_of_words_come_from_the_last_time_it_was_seen_reached() {
        let remote = 1i64 << 40;
        let up = spaces_json(serde_json::json!([
            space(1, "qoop", None),
            space(remote | 1, "build:eval", Some("up"))
        ]));
        let down = spaces_json(serde_json::json!([
            space(1, "qoop", None),
            space(remote | 1, "build:eval", Some("down"))
        ]));
        let mut reach = Reach::default();
        let machine = machine_of(remote | 1);
        assert_eq!(
            reach.not_reached(machine),
            "not reached since this window opened"
        );
        assert_eq!(reach.as_of(machine), "");
        let at = 1_763_000_000;
        reach.note(&up, at);
        let said = clock(at);
        reach.note(&down, at + 600);
        assert_eq!(
            reach.not_reached(machine),
            format!("not reached since {said}")
        );
        assert_eq!(reach.as_of(machine), format!("as of {said}"));
        // Reached again: the time moves on.
        reach.note(&up, at + 7200);
        assert_eq!(reach.last(machine), Some(clock(at + 7200)));
        // qass has its own record.
        reach.note_qass(false, at);
        assert_eq!(reach.last(QASS), None);
        reach.note_qass(true, at);
        assert_eq!(reach.last(QASS), Some(said));
    }

    #[test]
    fn keys_are_read_whole() {
        assert_eq!(keys_of(b"a\x1b[Cb"), [K::Char('a'), K::Right, K::Char('b')]);
        assert_eq!(keys_of(b"\x1b"), [K::Esc]);
        assert_eq!(
            keys_of("\u{15}\u{7f}\u{e7}\r".as_bytes()),
            [K::Clear, K::Back, K::Char('\u{e7}'), K::Enter]
        );
        assert_eq!(
            keys_of(b"\x1b[200~a\nb\x1b[201~"),
            [K::Char('a'), K::Char(' '), K::Char('b')],
            "a paste's newline is a space, never an enter"
        );
    }
}

#[cfg(test)]
mod place_tests {
    use super::fixture::{REMOTE, world};
    use super::*;

    fn text(view: &View) -> String {
        view.plain.join("\n")
    }

    /// `r` on a computer sends a retry of that machine, and `esc` puts the screen away.
    #[test]
    fn r_retries_the_computer_and_esc_leaves() {
        let mut computer = Computer::new(1);
        assert_eq!(computer.key(b"r"), PlaceOut::Retry(1));
        assert_eq!(computer.key(b"x"), PlaceOut::Stay);
        assert_eq!(computer.key(b"\x1b"), PlaceOut::Closed);
        assert_eq!(computer.click(PlaceHit::Retry), PlaceOut::Retry(1));
    }

    /// A machine that is not reached says since when (from the client's own record, else since
    /// this window opened), why, and lists its agents as they were.
    #[test]
    fn a_computer_not_reached_says_so_with_its_time_its_reason_and_its_agents() {
        let mut w = world();
        let computer = Computer::new(1);
        let seen = w.with(|env| text(&computer.view(env, 80, 24)));
        assert!(
            seen.contains("build-node  not reached since this window opened"),
            "{seen}"
        );
        assert!(seen.contains("Operation timed out"), "{seen}");
        assert!(seen.contains("r retry now"), "{seen}");
        assert!(
            seen.contains("its agents 1") && seen.contains("eval classifier"),
            "{seen}"
        );
        // With a time on record, the words and the agents' header carry it.
        let up = vec![
            fixture::space(1, "qoop", None),
            fixture::space(REMOTE | 1, "build-node:eval", Some("up")),
        ];
        w.reach.note(&up, 1_763_000_000);
        let said = clock(1_763_000_000);
        let seen = w.with(|env| text(&computer.view(env, 80, 24)));
        assert!(
            seen.contains(&format!("not reached since {said}")),
            "{seen}"
        );
        assert!(
            seen.contains(&format!("its agents, as of {said} 1")),
            "{seen}"
        );
        // A machine that answers has no retry.
        w.spaces[3].link = Some(LinkState::Up);
        let seen = w.with(|env| text(&computer.view(env, 80, 24)));
        assert!(seen.contains("online") && !seen.contains("retry"), "{seen}");
    }

    /// The new agent box types the shared worktree line: the task is quoted safely, the branch
    /// comes from its first words, and Tab walks the fields.
    #[test]
    fn the_new_agent_box_builds_the_shared_worktree_line() {
        let w = world();
        let mut new = w.with(|env| NewWork::open(env, Some(2)));
        let task = "fix the login: it's flaky";
        assert_eq!(new.key(task.as_bytes()), PlaceOut::Stay);
        let (line, space) = new.line().expect("a line");
        assert_eq!(space, Some(2));
        assert_eq!(
            line,
            crate::answers::worktree_line(&new.branch(), Some(2), "claude", task)
                .expect("the shared line")
        );
        assert!(line.contains("'fix the login: it'\\''s flaky'"), "{line}");
        assert_eq!(new.branch(), "feat/fix-the-login-its-flaky");
        // Enter starts; the agent field cycles with the arrows and Tab.
        assert_eq!(
            new.key(b"\r"),
            PlaceOut::Start {
                line: line.clone(),
                space: Some(2),
                name: new.branch()
            }
        );
        new.key(b"\t");
        new.key(b"\x1b[C");
        let (line, _) = new.line().expect("a line");
        assert!(line.contains("-- codex 'fix the login"), "{line}");
        // An empty task is refused on the box, with its reason.
        let mut empty = w.with(|env| NewWork::open(env, None));
        assert_eq!(empty.key(b"\r"), PlaceOut::Stay);
        assert!(text(&empty.view(80, 24)).contains("Say what the agent should do."));
        assert_eq!(empty.key(b"\x1b"), PlaceOut::Closed);
    }

    /// Each place paints whole rows of exactly the pane's width at every size, with no cut word.
    /// With `QOOP_SHOTS_DIR` set the rows are written there, one file per place and size.
    #[test]
    fn every_place_paints_whole_at_the_three_sizes() {
        let w = world();
        let dir = std::env::var_os("QOOP_SHOTS_DIR").map(std::path::PathBuf::from);
        for (cols, rows) in [(80usize, 24usize), (104, 30), (220, 60)] {
            // The pane is the window less the agents column (folded below 100 columns).
            let pane = if cols < 100 { cols - 4 } else { cols - 39 - 1 };
            let views: Vec<(&str, View)> = vec![
                (
                    "ledger",
                    w.with(|env| {
                        let inbox =
                            crate::inbox::Inbox::build(env.rows, env.cards, env.hand, env.ctx, &[]);
                        crate::ledger::Ledger::default().view(env, &inbox, pane, rows)
                    }),
                ),
                (
                    "board",
                    w.with(|env| {
                        let mut inbox = crate::inbox_pane::Pane::new();
                        inbox.open();
                        inbox.show_board(true);
                        inbox.view(env, pane, rows)
                    }),
                ),
                (
                    "computer",
                    w.with(|env| Computer::new(1).view(env, pane, rows)),
                ),
                ("new-agent", {
                    let mut new = w.with(|env| NewWork::open(env, Some(1)));
                    new.key(b"Fix the flaky login test on CI: it times out on the second retry, find why and make it pass without raising the timeout");
                    new.view(pane, rows)
                }),
            ];
            for (name, view) in views {
                assert_eq!(view.rows.len(), view.plain.len());
                let t = text(&view);
                assert!(!t.contains('\u{2026}'), "{name} {cols}: an ellipsis\n{t}");
                assert!(
                    !t.contains('\u{1b}'),
                    "{name} {cols}: an escape in the plain rows"
                );
                for row in &view.plain {
                    assert_eq!(
                        crate::width::width(row),
                        pane,
                        "{name} {cols}: a row is not the pane's width"
                    );
                }
                assert!(
                    view.plain.len() <= rows,
                    "{name} {cols}: taller than the pane"
                );
                if let Some(dir) = &dir {
                    std::fs::create_dir_all(dir).expect("dir");
                    let body: String = view
                        .plain
                        .iter()
                        .map(|r| format!("{}\n", r.trim_end()))
                        .collect();
                    std::fs::write(dir.join(format!("{name}-{cols}x{rows}.txt")), body)
                        .expect("write");
                }
            }
        }
    }

    #[cfg(test)]
    mod more_place_tests {
        use super::fixture::{REMOTE, world};
        use super::*;

        fn text(view: &View) -> String {
            view.plain.join("\n")
        }

        /// Only a machine that answers is noted as reached: a parked one keeps its last real time.
        #[test]
        fn a_parked_machine_keeps_its_last_real_answer() {
            let up = vec![
                fixture::space(1, "qoop", None),
                fixture::space(REMOTE | 1, "build:eval", Some("up")),
            ];
            let parked = vec![
                fixture::space(1, "qoop", None),
                fixture::space(REMOTE | 1, "build:eval", Some("parked")),
            ];
            let mut reach = Reach::default();
            reach.note(&up, 1_763_000_000);
            reach.note(&parked, 1_763_009_000);
            assert_eq!(reach.last(1), Some(clock(1_763_000_000)));
        }

        /// A parked machine is connected, not retried, and its screen says so.
        #[test]
        fn a_parked_computer_offers_connect_not_retry() {
            let mut w = world();
            w.spaces[3].link = Some(LinkState::Parked);
            let seen = w.with(|env| text(&Computer::new(1).view(env, 80, 24)));
            assert!(
                seen.contains("r connect") && !seen.contains("retry"),
                "{seen}"
            );
            assert!(seen.contains("not connected"), "{seen}");
            // The machine is named once: its agents carry the space, not `machine:space`.
            assert!(!seen.contains("build-node:eval"), "{seen}");
        }

        /// A computer with many agents scrolls with j and k like the ledger, and the wheel.
        #[test]
        fn a_computer_with_many_agents_scrolls() {
            let mut w = world();
            for n in 0..40u32 {
                w.rows.push(fixture::agent(
                    crate::mux::REMOTE_TERMINAL | (100 + n),
                    REMOTE | 1,
                    &format!("agent number {n}"),
                    "working",
                ));
            }
            let mut computer = Computer::new(1);
            let top = w.with(|env| text(&computer.view(env, 80, 24)));
            assert!(
                top.contains("more below") && !top.contains("agent number 39"),
                "{top}"
            );
            for _ in 0..60 {
                computer.key(b"j");
                w.with(|env| computer.view(env, 80, 24));
            }
            let bottom = w.with(|env| text(&computer.view(env, 80, 24)));
            assert!(
                bottom.contains("agent number 39") && bottom.contains("more above"),
                "{bottom}"
            );
            computer.scroll(false);
            computer.key(b"k");
            let up = w.with(|env| text(&computer.view(env, 80, 24)));
            assert!(
                !up.contains("agent number 39") || up.contains("more below"),
                "{up}"
            );
        }

        /// The task is the last field, so the fields before it never leave the pane; with the task
        /// long, Tab out of it shows them from the top again.
        #[test]
        fn a_long_task_never_pushes_the_other_fields_out_of_reach() {
            let w = world();
            let mut new = w.with(|env| NewWork::open(env, Some(1)));
            let long = "keep the old behaviour while the new path is checked, ".repeat(30);
            new.key(long.as_bytes());
            let typing = text(&new.view(76, 21));
            assert!(
                typing.contains('\u{258c}'),
                "the end of the task, with its caret:\n{typing}"
            );
            new.key(b"\t");
            let fields = text(&new.view(76, 21));
            let (space, agent, branch, task) = (
                fields.find("\n space  ").expect("space"),
                fields.find("\n agent  ").expect("agent"),
                fields.find("\n branch ").expect("branch"),
                fields.find("what should the agent do?").expect("task"),
            );
            assert!(space < agent && agent < branch && branch < task, "{fields}");
        }

        /// The Board's footer says esc closes the whole Inbox, not that it goes back.
        #[test]
        fn the_boards_footer_says_esc_closes() {
            let w = world();
            let mut inbox = crate::inbox_pane::Pane::new();
            inbox.open();
            inbox.show_board(true);
            let seen = w.with(|env| text(&inbox.view(env, 76, 24)));
            assert!(
                seen.contains("esc closes") && !seen.contains("esc back"),
                "{seen}"
            );
        }
    }

    /// The lessons file: missing is none written yet, a folder where it should be (unreadable)
    /// is none, and one that is there is counted.
    #[test]
    fn the_rules_file_missing_unreadable_and_present() {
        let dir = std::env::temp_dir().join(format!("qoop-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        assert_eq!(
            rules_read_at(&dir.join("nothing.md")),
            Some(Lessons::default()),
            "no file: none written yet"
        );
        assert_eq!(rules_read_at(&dir), None, "a folder cannot be read");
        let file = dir.join("lessons.md");
        std::fs::write(
            &file,
            "- a release -> escalate (owner)\n- a test run -> do it (E7)\n- a bump -> wait (E9)\n",
        )
        .expect("file");
        assert_eq!(
            rules_read_at(&file),
            Some(Lessons {
                yours: 1,
                alone: 1,
                all: 3
            })
        );
        std::fs::remove_file(&file).expect("clean");
        std::fs::remove_dir(&dir).expect("clean");
    }
}

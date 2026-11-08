//! Which agent is in which terminal, and how a claim about one is admitted.
//!
//! Three things that have to move together, which is why they are one type rather than
//! three: the foreground process, the generation, and the token.
//!
//! A pid is not an identity. A shell that `exec`s an agent CLI keeps its pid, and an agent
//! CLI that `exec`s another one keeps it too, so anything keyed on the pid alone reports
//! the first program forever. The generation is what changes, and everything sampled under
//! an older one is refused (ADR-008, invariant I5).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use qoop_domain::{
    AgentBelief, AgentIdentity, AgentRuntimeState, Boundary, CapabilityToken, ClaimRefusal,
    GenerationId, ObservationSource, TerminalCapability, TerminalId,
};
use qoop_pty::procwatch::{ProcEvent, ProcWatch};
use qoop_pty::teardown::Pid;

/// Programs that put the real process somewhere we cannot read.
///
/// A list, and therefore a heuristic that will be wrong sometimes. It is here rather than
/// in the domain for exactly that reason: the domain says what `Unavailable` means, and
/// this says which programs we currently believe produce it. Being wrong here reports
/// `Unavailable` for something we could have read, which is a smaller error than reporting
/// a wrapper's name as an agent's.
pub const BOUNDARY_PROGRAMS: &[&str] = &[
    "docker",
    "podman",
    "nerdctl",
    "nsenter",
    "systemd-nspawn",
    "sandbox-exec",
    "lima",
    "colima",
];

fn boundary_for(program: &str) -> Option<Boundary> {
    let name = program.rsplit('/').next().unwrap_or(program);
    BOUNDARY_PROGRAMS
        .contains(&name)
        .then(|| Boundary::Namespace {
            wrapper: name.to_string(),
        })
}

/// What one terminal knows about its agent.
#[derive(Clone, Debug)]
pub struct TerminalAgent {
    pub capability: TerminalCapability,
    pub identity: AgentIdentity,
    pub belief: AgentBelief,
    /// When this agent last did something, as unix seconds on the daemon's
    /// own clock. Stamped beside every accepted belief change and generation
    /// bump, never inside the domain belief and never from a hook's advisory
    /// clock. `None` until the first accepted observation, so a restart
    /// paints no row as fresh.
    pub last_active_at_unix: Option<i64>,
    /// When this terminal last saw live traffic, as unix seconds on the
    /// daemon's own clock: PTY output, live client input, or any of the
    /// events that stamp `last_active_at_unix`.
    ///
    /// The belief stamp alone cannot tell idle from busy: a build streaming
    /// for an hour without changing the belief looks idle on that clock.
    /// Seeded together with it at spawn (see `note_spawn`), so no terminal
    /// is ever stamp-`None` past creation and the reaper has no immortal
    /// class. Restore replay input never stamps this: replayed bytes are
    /// not live activity.
    pub last_io_unix: Option<i64>,
    /// A startup gate is on screen: answering it creates the session rather
    /// than continuing one. Set from the matched manifest rule, never
    /// inferred, and cleared by any later screen reading without one.
    pub setup_gate: bool,
    /// How many background agents the screen showed running at its last reading. Read
    /// from the live screen by the detect loop, watched or not, because a subagent is
    /// no process and the screen is the only place it shows. Zero on every generation
    /// change, since it described a process that is gone.
    pub background_agents: usize,
    /// How many background shells and monitors the footer showed running at its last reading,
    /// by the same loop and with the same reset as `background_agents`. Read by the index, not
    /// by the sleep check: those are processes, and `sleep_blockers` judges them.
    pub background_tasks: usize,
    /// The sub-agents the agent's hooks said started and have not said ended, by the agent's
    /// own id for each. With the same reset as `background_agents`.
    pub subagents: std::collections::BTreeSet<String>,
    /// The rule the screen last matched in this generation, if any.
    ///
    /// What `qoop agents explain` reports. Recorded on every evaluation,
    /// accepted or outranked: it says what the screen claimed, while the
    /// belief says what was concluded. Cleared with the belief on every
    /// generation change, because it described a process that is gone.
    pub screen_rule: Option<String>,
    /// The manifest that produced `screen_rule`, by id. Cleared with it.
    pub screen_manifest: Option<String>,
    /// The foreground leader this identity was read from.
    pub leader: Option<Pid>,
    /// When a leader that is not an agent was last looked into, for the wrapper case.
    pub relooked: Option<std::time::Instant>,
    /// A look the read loop skipped for `RELOOK`, still owed: the output that asked for it
    /// can be the last for a long time, an agent a wrapper started drawing once and then
    /// waiting, and nothing else would ever look again.
    pub relook_owed: bool,
    /// When the current generation was minted by a foreground change, which starts
    /// ADR-008's startup grace. `None` only until the first look finds a foreground
    /// process: that look is a change from no leader, so even a terminal spawned straight
    /// into an agent gets the grace.
    pub born: Option<std::time::Instant>,
    /// The foreground leaders this terminal has had and when each was first seen, in unix ms,
    /// the newest last and few. A leader that comes back (ctrl+z, then `fg`) keeps its first
    /// time, which `born` does not: a chat reads work from before the agent started as left
    /// behind, and a stopped agent that was resumed started when it first did.
    pub seen: Vec<(i32, i64)>,
}

/// How many leaders `TerminalAgent::seen` keeps.
const SEEN_LEADERS: usize = 8;

/// What one terminal's agent record is, as a daemon that is becoming another binary carries
/// it: the process in the terminal is the same one, so its generation, what was believed
/// about it and what has been said of its conversation have to be too. A fresh record would
/// call it a new generation, and a conversation id said by the old one would be forgotten.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentCarry {
    pub generation: GenerationId,
    pub identity: AgentIdentity,
    pub belief: AgentBelief,
    pub leader: Option<i32>,
    pub last_active_at_unix: Option<i64>,
    pub last_io_unix: Option<i64>,
    pub setup_gate: bool,
    pub background_agents: usize,
    pub background_tasks: usize,
    /// Absent from a daemon that predates it.
    #[serde(default)]
    pub subagents: std::collections::BTreeSet<String>,
    pub screen_rule: Option<String>,
    pub screen_manifest: Option<String>,
}

/// How long after a foreground change the weak idle fallback stays quiet: ADR-008's
/// startup grace. Claude Code draws its first frame in pieces, a trust dialog included,
/// well inside this, and a half drawn dialog matches no rule; read as idle, a task typed
/// at it chose the dialog's default, which exits. A program that has matched no rule by
/// the end reads as idle, as before.
pub const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

impl TerminalAgent {
    /// A new generation: the old belief described a process that is gone.
    ///
    /// The one place a foreground change is applied, so the grace starts wherever one is
    /// seen: the read loop's look, the process watcher's exec, the relook.
    fn advance(&mut self) -> GenerationId {
        let generation = self.capability.advance();
        self.belief = fresh_belief(generation);
        self.born = Some(std::time::Instant::now());
        generation
    }

    /// The leader `pid` is in the foreground now: its first time is kept if it had one.
    fn note_leader(&mut self, pid: Pid, now_unix: i64) {
        if self.seen.iter().any(|(p, _)| *p == pid.0) {
            return;
        }
        if self.seen.len() >= SEEN_LEADERS {
            self.seen.remove(0);
        }
        self.seen.push((pid.0, now_unix.saturating_mul(1000)));
    }

    /// When the leader in the foreground was first seen, unix ms. `None` for a leader this
    /// daemon did not see come (one a daemon update carried over).
    pub fn leader_seen_ms(&self) -> Option<i64> {
        let pid = self.leader?;
        self.seen
            .iter()
            .find(|(p, _)| *p == pid.0)
            .map(|(_, at)| *at)
    }

    /// When this generation's startup grace ends, if it has not yet.
    pub fn grace_ends(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        let ends = self.born? + STARTUP_GRACE;
        (now < ends).then_some(ends)
    }
}

/// How long a waiter→target edge may live, seconds. A delegation silent
/// for a full day is over by every definition that matters; the bound keeps
/// the map from holding a wait forever. Swept lazily on every reap pass.
pub const EXPECTATION_TTL_SECS: i64 = 24 * 3600;

/// One atomic snapshot for the idle reaper. See `reap_snapshot()`.
#[derive(Clone, Debug)]
pub struct ReapSnapshot {
    pub generation: GenerationId,
    pub state: AgentRuntimeState,
    pub program: Option<String>,
    /// The program is a coding agent, as decided where its argv was read.
    pub agent: bool,
    /// The identity is `Unavailable` (a wrapper the daemon cannot see
    /// through): reads as no program, but must veto, never close.
    pub boundary: bool,
    pub setup_gate: bool,
    /// Background agents the screen showed running, see `TerminalAgent::background_agents`.
    pub background_agents: usize,
    pub idle_since: Option<i64>,
}

/// The least time between two looks into a leader that is not an agent.
///
/// The look reads the program, its arguments and its whole group, several system calls,
/// and the read loop asks on every read. Unbounded, a shell echoing keys or a build
/// printing its log paid that on every chunk of output: measured, two thirds of the
/// daemon's time under a child streaming at sixty frames a second. Once a second still
/// finds an agent a wrapper starts within a second of it printing anything.
// ponytail: fixed interval, a backoff if a second of idle wrapper looks ever shows up.
const RELOOK: std::time::Duration = std::time::Duration::from_secs(1);

/// How soon after a line is sent the agent must start working for the line to count as the
/// answer that ends a park.
const ANSWER_WINDOW_SECS: i64 = 30;

/// A terminal's stop count, and its newest stop's time and generation while no work came after it.
type Stops = (u64, Option<(i64, GenerationId)>);

pub struct AgentRegistry {
    watch: ProcWatch,
    /// Terminals whose agent has moved, for anyone drawing an index.
    ///
    /// Here rather than at the call sites, and the reason is a bug that shipped for an
    /// afternoon: the notification was sent from the two places that looked like the only
    /// ones, the process watcher and the settle loop, and the third was the read loop's own
    /// `note_foreground`. Interrupting a command therefore updated the registry and told
    /// nobody, so an index showed a command that had already finished. A registry that
    /// announces its own changes cannot have a path that forgets to.
    changes: tokio::sync::broadcast::Sender<u32>,
    /// Terminals owed an [`Self::announce_soon`]: one second trailing window for all of them.
    announce_due: Arc<Mutex<BTreeSet<u32>>>,
    /// The command each shell is running and the last it finished (OSC 133).
    commands: Mutex<BTreeMap<u32, ShellCommands>>,
    /// Which kind of client a person last typed from in each terminal, when, and when that
    /// was last news (unix seconds).
    typed_by: Mutex<BTreeMap<u32, (&'static str, i64, i64)>>,
    /// Tasks, board items or a space goal changed: a board reads them again. Its own
    /// channel, because it is not about any terminal's row.
    tasks: tokio::sync::broadcast::Sender<()>,
    /// Each terminal's last [`Coarse`] state: what tells a change a board shows from one it does
    /// not. A terminal's entry goes when it closes, so a number handed out again starts clean.
    tokens: Mutex<BTreeMap<u32, Coarse>>,
    /// The terminals that hold an open task, read from the store at start and after every task
    /// write: a plain shell, or qass's tab, never makes a board read again.
    task_terminals: Mutex<BTreeSet<u32>>,
    /// A board re-read is already owed (see [`Self::tasks_changed_soon`]).
    tasks_due: Arc<AtomicBool>,
    /// Which terminal a watched pid belongs to.
    by_pid: Mutex<HashMap<i32, u32>>,
    /// Each terminal's last turn end in its own words, masked: the paragraph before the last
    /// and the last (see `ReportAgentState::said`), for the card a question gets. Memory
    /// only, replaced at every turn end.
    said: Mutex<HashMap<u32, String>>,
    terminals: Mutex<BTreeMap<u32, TerminalAgent>>,
    /// Focus events per terminal, oldest first, capped at
    /// [`crate::control::MAX_FOCUS_EVENTS`].
    ///
    /// Display only: hook reports admitted through the token check land
    /// here as well as in the belief. Never journalled, never pushed,
    /// never observed. A terminal that is gone takes its log with it.
    focus: Mutex<BTreeMap<u32, VecDeque<crate::control::FocusEvent>>>,
    /// The newest prompt head each terminal's hook reported (redacted and cut like every
    /// focus label) and when, kept apart from the focus log because that log is capped and a
    /// few dozen tool calls push the prompt out of it. What a task opened by qoop is named
    /// after, and how it tells a new request from the one a done claim answered.
    prompts: Mutex<BTreeMap<u32, (String, i64)>>,
    /// Each terminal's admitted turn ends: how many there were, and when the newest was and in
    /// which generation, while no work came after it. What tells qass's watcher a new stop from
    /// the same one said again. The count outlives a clear, so two stops in one second never
    /// share an id; a stop of an older generation is no stop of the agent there now.
    stops: Mutex<BTreeMap<u32, Stops>>,
    /// Each terminal's master, so the foreground group can be re-read without asking the
    /// terminal's own task, which may be parked waiting for output that is not coming.
    masters: Mutex<HashMap<u32, std::os::fd::RawFd>>,
    /// Rung by any terminal's traffic, typed or printed, for the probe platform: what is typed
    /// is often a command about to start, which prints nothing once it runs.
    io: tokio::sync::Notify,
    /// Each terminal's leader pid and its terminal, keyed like the process table, so a
    /// sleep can walk what the leader started.
    leaders: Mutex<HashMap<u32, (i32, u64)>>,
    /// Streams an explicit `CloseTerminal` asked to close and that have not
    /// torn down yet. Their rows close even if their space is going to sleep,
    /// and idle sleep leaves them alone. Entries die with the teardown they
    /// mark.
    closed: Mutex<BTreeSet<u32>>,
    /// Who waits on whom: waiter stream → (target stream, edge since, unix
    /// seconds). Set when a verified `qoop send/prompt` arrives from one
    /// terminal to another. The waiter is vetoed while its target is busy;
    /// both idle together sleep together. `since` only ages the edge out.
    expectations: Mutex<BTreeMap<u32, (u32, i64)>>,
    /// Recent proof of life beyond the two activity clocks: admitted hook
    /// focus events. Vetoes while
    /// fresher than the idle timeout. Bounded by live-terminal count,
    /// cleared on forget.
    shield_at_unix: Mutex<BTreeMap<u32, i64>>,
    /// When a person last typed into each terminal, as unix seconds. What the inbox reads
    /// to leave a composer alone: a half written message still reads `idle`, so only this
    /// stamp says someone is in it. Not the restore replay, not the inbox's own sends.
    typed_at_unix: Mutex<BTreeMap<u32, i64>>,
    /// The agents a person parked to answer later, and when, as unix seconds. Kept through a
    /// sleep and a restart (the store's `parked_terminal` has them), taken back by a person,
    /// by the terminal closing for good, or by an answer: see [`Self::announce`].
    parked: Mutex<BTreeMap<u32, i64>>,
    /// Parks an answer took back, for the store: the agent recorder writes them down.
    unparked: Mutex<Vec<u32>>,
    /// When a line last reached each terminal from a person (any client, the inbox's
    /// answers too), as unix seconds: what tells an answer to a parked agent from a key that
    /// only moved something.
    sent_at_unix: Mutex<BTreeMap<u32, i64>>,
    /// When a person last pressed a key in each terminal, and when they last sent a line there,
    /// as unix seconds: only a person's connection (not `qoop prompt` or `qoop send`), only keys
    /// a person presses ([`person_key`]). What another sender must not type over.
    person_typed: Mutex<BTreeMap<u32, i64>>,
    person_sent: Mutex<BTreeMap<u32, i64>>,
    /// Terminals where a person pressed keys after the last line they sent: a draft, kept as
    /// its own fact so a line and a new draft in the same second are told apart.
    person_draft: Mutex<std::collections::BTreeSet<u32>>,
}

/// A person who sent a line into a terminal is at it for this long.
pub const PERSON_SENT_SECS: i64 = 120;
/// A person who typed and sent nothing (a draft, or an Esc and a thought) for this long.
pub const PERSON_DRAFT_SECS: i64 = 180;
/// How often the same kind of client typing on in a terminal is news again: under the ten
/// seconds a client shows "typing" after the last news, so it never lapses mid burst.
pub const TYPING_NEWS_SECS: i64 = 5;

/// Whether input from a person's connection holds a key they pressed, not only what their
/// terminal sent by itself. A read may hold several keys (an arrow and the text after it), so
/// each escape sequence in it is judged on its own, and any text is a key.
pub fn person_key(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while !rest.is_empty() {
        if rest[0] != 0x1b {
            return true;
        }
        let len = sequence_len(rest);
        if one_key(&rest[..len]) {
            return true;
        }
        rest = &rest[len..];
    }
    false
}

/// The length of the escape sequence at the start of `bytes` (which starts with Esc): a CSI up
/// to its final byte, an SS3 key, an OSC or DCS up to its terminator, else Esc and one byte.
/// A return, or Enter as a kitty key report (`ESC[13u`, `ESC[13;1u`).
fn is_enter(key: &[u8]) -> bool {
    matches!(key, b"\r" | b"\n") || (key.starts_with(b"\x1b[13") && key.ends_with(b"u"))
}

/// Whether input holds an Enter anywhere, a pasted line's return too.
fn has_enter(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while !rest.is_empty() {
        let len = if rest[0] == 0x1b {
            sequence_len(rest)
        } else {
            1
        };
        if is_enter(&rest[..len]) {
            return true;
        }
        rest = &rest[len..];
    }
    false
}

fn sequence_len(bytes: &[u8]) -> usize {
    match bytes.get(1) {
        None => 1,
        // An X10 mouse report: `ESC [ M` and three raw bytes.
        Some(b'[') if bytes.get(2) == Some(&b'M') => bytes.len().min(6),
        Some(b'[') => bytes[2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map_or(bytes.len(), |i| i + 3),
        Some(b'O') => bytes.len().min(3),
        Some(b']' | b'P') => {
            let end = bytes[2..]
                .windows(2)
                .position(|w| w == b"\x1b\\")
                .map(|i| i + 4);
            let bel = bytes[2..].iter().position(|b| *b == 0x07).map(|i| i + 3);
            end.into_iter().chain(bel).min().unwrap_or(bytes.len())
        }
        Some(_) => 2,
    }
}

/// One escape sequence as a key a person pressed: a lone Esc, Alt and a key (`ESC x`), SS3
/// keys (`ESC O A`..`ESC O S`), or a CSI with digits, `;` and `:` only that ends in an arrow,
/// Home, End, F1, F2, F4, `~` or a kitty key report (`ESC[27u`). Not a mouse, focus, colour,
/// device or mode report, nor a DCS or OSC reply: an allow list, since terminals answer in
/// many shapes. A bracketed paste reaches [`AgentRegistry::note_person`] as a paste.
fn one_key(seq: &[u8]) -> bool {
    let Some(rest) = seq.strip_prefix(b"\x1b") else {
        return !seq.is_empty();
    };
    match rest {
        [] => true,
        [b'O', k] => matches!(k, b'A'..=b'D' | b'H' | b'F' | b'P'..=b'S'),
        [b'[', csi @ ..] => match csi.split_last() {
            // Not `R`: a cursor position report ends in it (`ESC[12;40R`).
            Some((last, params)) => {
                matches!(
                    last,
                    b'A'..=b'D' | b'H' | b'F' | b'P' | b'Q' | b'S' | b'~' | b'u'
                ) && params
                    .iter()
                    .all(|b| b.is_ascii_digit() || matches!(b, b';' | b':'))
            }
            None => false,
        },
        [k] => k.is_ascii_graphic() && *k != b'[' && *k != b']' && *k != b'P',
        _ => false,
    }
}

impl AgentRegistry {
    /// Watch for terminals whose agent has moved.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<u32> {
        self.changes.subscribe()
    }

    /// Watch for changes to tasks, board items and space goals.
    pub fn subscribe_tasks(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.tasks.subscribe()
    }

    /// Say that a write changed tasks, the board or a space goal. Only on a real change: an
    /// older phone reloads its whole agent list on any event.
    pub fn tasks_changed(&self) {
        let _ = self.tasks.send(());
    }

    /// Say that a terminal's agent is not what it was.
    ///
    /// Private, and called from every path that changes one. A send with no receivers is
    /// not an error: nobody is drawing an index.
    /// Tell every watcher that this terminal's row has changed.
    ///
    /// Public because a name is part of the row and nothing in here knows when one is
    /// given: the index is what draws it, so a rename nobody announces is a rename nobody
    /// sees until the next unrelated change.
    ///
    /// Every change passes here, which makes it the one place a park ends on an answer: a
    /// person sent a line to the agent since it was parked (any client, the phone and the
    /// inbox's answers too, never the restore replay; see [`Self::note_sent`]) and it works
    /// again. A key that sends no line, a background wake and a resume after a restart do
    /// not end a park.
    pub fn announce(&self, stream: u32) {
        self.unpark_if_answered(stream);
        self.note_token(stream);
        let _ = self.changes.send(stream);
    }

    /// A board draws what its agents are doing, so an agent of an open task moving between
    /// working, idle, blocked and failed makes it read again. Only a change of that coarse
    /// state: a terminal's first sighting is none (the wake calls [`Self::tasks_changed_soon`]
    /// itself), and a repeat is not news.
    fn note_token(&self, stream: u32) {
        let now = self.terminals.lock().unwrap().get(&stream).map(coarse);
        let mut tokens = self.tokens.lock().unwrap();
        let changed = match now {
            Some(now) => tokens.insert(stream, now).is_some_and(|was| was != now),
            None => tokens.remove(&stream).is_some(),
        };
        drop(tokens);
        if changed && self.task_terminals.lock().unwrap().contains(&stream) {
            self.tasks_changed_soon();
        }
    }

    /// The terminals that hold an open task, as the store has them.
    pub fn set_task_terminals(&self, terminals: BTreeSet<u32>) {
        *self.task_terminals.lock().unwrap() = terminals;
    }

    /// [`Self::tasks_changed`] on the trailing edge of a one-second window: the first call
    /// starts a timer and the ones inside it ride on it, so a flapping agent costs a board
    /// one read a second and the newest change always goes out. Never waits in the caller.
    pub fn tasks_changed_soon(&self) {
        if self.tasks_due.swap(true, Ordering::SeqCst) {
            return;
        }
        let (due, tasks) = (self.tasks_due.clone(), self.tasks.clone());
        after_window(move || {
            // Down first: a change after this send starts the next window.
            due.store(false, Ordering::SeqCst);
            let _ = tasks.send(());
        });
    }

    /// [`Self::announce`]'s news on the trailing edge of a one-second window, for changes a
    /// program can make as fast as it likes (a shell's command marks): a terminal flooding them
    /// costs every client one listing a second. What rides here moves neither a park nor a
    /// board, so the window goes straight to the watchers.
    pub fn announce_soon(&self, stream: u32) {
        {
            let mut due = self.announce_due.lock().unwrap();
            let first = due.is_empty();
            due.insert(stream);
            if !first {
                return;
            }
        }
        let (due, changes) = (self.announce_due.clone(), self.changes.clone());
        after_window(move || {
            for stream in std::mem::take(&mut *due.lock().unwrap()) {
                let _ = changes.send(stream);
            }
        });
    }

    /// A shell's command mark. A command's end is kept only after its start was seen: an
    /// Enter on an empty line, or a command started before a restart, ends nothing.
    pub fn note_shell(&self, stream: u32, mark: qoop_terminal::ShellMark, now_unix: i64) {
        match mark {
            qoop_terminal::ShellMark::Started => {
                let generation = self.agent_generation(stream);
                self.commands
                    .lock()
                    .unwrap()
                    .entry(stream)
                    .or_default()
                    .started = Some(Started {
                    at: std::time::Instant::now(),
                    generation,
                    agent: false,
                });
            }
            qoop_terminal::ShellMark::Finished(exit) => {
                let mut commands = self.commands.lock().unwrap();
                let shell = commands.entry(stream).or_default();
                let Some(started) = shell.started.take() else {
                    return;
                };
                shell.last = Some(crate::control::CommandRow {
                    exit,
                    millis: u64::try_from(started.at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    ended_at: now_unix,
                    agent: started.agent,
                });
                drop(commands);
                self.announce_soon(stream);
            }
        }
    }

    /// While a command runs, whether an agent is what it runs: an agent typed into the shell
    /// is the foreground neither when its command starts nor once it has ended, so this looks
    /// on the output in between. Only an agent of a generation newer than the one at the
    /// command's start counts, so one that just ended is not taken for the next command.
    /// Whether to look again: the read loop calls this only while a command runs and it has
    /// not yet answered no.
    pub fn note_command_output(&self, stream: u32) -> bool {
        let Some(since) = self
            .commands
            .lock()
            .unwrap()
            .get(&stream)
            .and_then(|shell| shell.started.as_ref())
            .filter(|started| !started.agent)
            .map(|started| started.generation)
        else {
            return false;
        };
        let agent = self
            .terminals
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|a| {
                matches!(
                    a.identity,
                    AgentIdentity::Known { agent: true, .. }
                        | AgentIdentity::Unavailable {
                            boundary: Boundary::Namespace { .. },
                            ..
                        }
                ) && a.identity.generation() > since
            });
        if !agent {
            return true;
        }
        if let Some(started) = self
            .commands
            .lock()
            .unwrap()
            .get_mut(&stream)
            .and_then(|shell| shell.started.as_mut())
        {
            started.agent = true;
        }
        false
    }

    /// The generation of what runs in this terminal now, if anything was seen there.
    fn agent_generation(&self, stream: u32) -> Option<GenerationId> {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .and_then(|a| a.identity.generation())
    }

    /// Which kind of client a person last typed from here, and when.
    pub fn typed_by(&self, stream: u32) -> Option<crate::control::TypedBy> {
        self.typed_by
            .lock()
            .unwrap()
            .get(&stream)
            .map(|(kind, at, _)| crate::control::TypedBy {
                kind: kind.to_string(),
                at: *at,
            })
    }

    /// The last command the shell here finished.
    pub fn command(&self, stream: u32) -> Option<crate::control::CommandRow> {
        self.commands
            .lock()
            .unwrap()
            .get(&stream)
            .and_then(|shell| shell.last.clone())
    }

    fn unpark_if_answered(&self, stream: u32) {
        let Some(at) = self.parked.lock().unwrap().get(&stream).copied() else {
            return;
        };
        let working = self
            .terminals
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|a| a.belief.state == AgentRuntimeState::Working);
        // The line that made it work, sent after the park and moments before: an Enter long
        // ago that started nothing does not get to end the park when a background wake does.
        let now = now_unix();
        let typed = self
            .sent_at_unix
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|t| *t >= at && now - *t <= ANSWER_WINDOW_SECS);
        if working && typed {
            let mut parked = self.parked.lock().unwrap();
            // The same park still: one made again in between is not this answer's.
            if parked.get(&stream) == Some(&at) {
                parked.remove(&stream);
                drop(parked);
                self.unparked.lock().unwrap().push(stream);
            }
        }
    }

    /// Park this terminal's agent at `at`, or take it back, and say so. Refused, false, while
    /// it works or starts: a park is for a reply that waits on a person, and taking it only
    /// from a settled agent means an answer's turn to working always comes after the park.
    pub fn set_parked(&self, stream: u32, on: bool, at: i64) -> bool {
        let terminals = self.terminals.lock().unwrap();
        let busy = terminals.get(&stream).is_some_and(|a| {
            matches!(
                a.belief.state,
                AgentRuntimeState::Working | AgentRuntimeState::Starting
            )
        });
        if on && busy {
            return false;
        }
        let mut parked = self.parked.lock().unwrap();
        if on {
            parked.insert(stream, at);
        } else {
            parked.remove(&stream);
        }
        drop(parked);
        drop(terminals);
        self.announce(stream);
        true
    }

    pub fn is_parked(&self, stream: u32) -> bool {
        self.parked.lock().unwrap().contains_key(&stream)
    }

    /// The parks the store kept, at start.
    pub fn load_parked(&self, parked: BTreeMap<u32, i64>) {
        *self.parked.lock().unwrap() = parked;
    }

    /// A terminal closed for good is parked no more: its id may be handed out again.
    pub fn drop_parked(&self, stream: u32) {
        self.parked.lock().unwrap().remove(&stream);
    }

    /// The parks answers took back since the last ask, for the store.
    pub fn take_unparked(&self) -> Vec<u32> {
        std::mem::take(&mut *self.unparked.lock().unwrap())
    }

    /// Re-announce a terminal without touching its belief.
    ///
    /// The slow-tick recovery for a missed index refresh (Phase D1): a panel
    /// that missed one `AgentChanged` event converges on the next one. This
    /// goes around `accept()` on purpose, so it cannot change the belief and
    /// cannot be filtered by the changed-state gate: it is a repeat, not news.
    /// The daemon arms it only while a `visible_blocker` holds.
    pub fn reannounce(&self, stream: u32) {
        self.announce(stream);
    }

    /// Whether this terminal's belief currently holds a visible blocker.
    ///
    /// A copied bool rather than a cloned record: the detect loop asks on
    /// every evaluation and cloning the whole record for one flag is the kind
    /// of cost that only shows up at a hundred terminals.
    pub fn belief_visible_blocker(&self, stream: u32) -> bool {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|a| a.belief.visible_blocker)
    }

    /// Record what the screen shows running in the background: subagents, and the shells
    /// and monitors the footer counts. Changes no state. The sleep check reads the agents;
    /// the index shows both, so a changed total is announced and a repeat is not.
    pub fn note_background(&self, stream: u32, agents: usize, tasks: usize) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return;
        };
        let before = (agent.background_tasks, subagents(agent));
        agent.background_agents = agents;
        agent.background_tasks = tasks;
        let after = (agent.background_tasks, subagents(agent));
        drop(terminals);
        if before != after {
            self.announce(stream);
        }
    }

    /// Apply what a hook said about the agent's sub-agents. Changes no state; a changed
    /// count is announced and a repeat is not.
    pub fn note_subagents(&self, stream: u32, change: crate::control::SubagentChange) {
        use crate::control::SubagentChange;
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return;
        };
        // A late hook of a process that is gone, or one a shell ran: nothing of an agent's.
        if !matches!(agent.identity, AgentIdentity::Known { agent: true, .. }) {
            return;
        }
        let before = subagents(agent);
        // The agent waits on its start hook before the sub-agent runs, so a start lands
        // before its end. ponytail: an end that never comes (a sub-agent killed with an
        // interrupted turn) is held until the agent's next full list (Claude's `Stop`) or a
        // new generation; tombstones for ended ids if that shows up in practice.
        match change {
            SubagentChange::Started(id) => {
                agent.subagents.insert(id);
            }
            SubagentChange::Ended(id) => {
                agent.subagents.remove(&id);
            }
            SubagentChange::Running(ids) => agent.subagents.retain(|id| ids.contains(id)),
        }
        let after = subagents(agent);
        drop(terminals);
        if before != after {
            self.announce(stream);
        }
    }

    /// What runs in the background, all kinds together (the footer's shells and monitors, and
    /// the sub-agents), and how many of those are sub-agents: read under one lock, so the two
    /// agree.
    pub fn background_of(&self, stream: u32) -> (usize, usize) {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .map_or((0, 0), |a| {
                (a.background_tasks + subagents(a), subagents(a))
            })
    }

    /// The sub-agents the agent's own hooks say still run, without the screen's count as a
    /// floor: a turn end is judged on this, since Claude's own list at its `Stop` (sent before
    /// the state) is the truth there, and a screen read from earlier in the turn is not.
    /// Agents read only from the screen send no question with their turn end.
    pub fn hooked_subagents_of(&self, stream: u32) -> usize {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .map_or(0, |a| a.subagents.len())
    }
}

/// How many sub-agents an agent runs: what its hooks said started and has not ended, or the
/// background agents its screen lists, whichever is more. The screen alone is what an agent
/// without hooks has, and it lists only background ones.
fn subagents(agent: &TerminalAgent) -> usize {
    agent.subagents.len().max(agent.background_agents)
}

/// A shell's command in progress (when it started, and whether an agent was seen running in
/// it) and the last one it finished.
#[derive(Default)]
struct ShellCommands {
    started: Option<Started>,
    last: Option<crate::control::CommandRow>,
}

/// A command running now: when it started, the generation of what ran in its terminal then,
/// and whether an agent of a newer one has been seen running in it since.
struct Started {
    at: std::time::Instant,
    generation: Option<GenerationId>,
    agent: bool,
}

/// How long a board re-read waits for more changes before it goes out.
pub const TASKS_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// Run `send` once [`TASKS_WINDOW`] has passed, on the runtime, or now when there is none
/// (a test without one). The trailing edge of the windows above: what arrives inside one
/// rides on the send already owed.
fn after_window(send: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn(async move {
                tokio::time::sleep(TASKS_WINDOW).await;
                send();
            });
        }
        Err(_) => send(),
    }
}

/// What an agent is doing, coarsely: the states a board words differently. A change between
/// two of them is news to a board; a change inside one is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coarse {
    Working,
    Idle,
    Blocked,
    Failed,
    Unknown,
}

/// The same order a status reads: blocked first, then work (its state or its sub-agents),
/// then a failure.
fn coarse(agent: &TerminalAgent) -> Coarse {
    use AgentRuntimeState as S;
    match agent.belief.state {
        S::Blocked => Coarse::Blocked,
        S::Working | S::Starting => Coarse::Working,
        _ if subagents(agent) > 0 => Coarse::Working,
        S::Failed => Coarse::Failed,
        S::Idle | S::Done => Coarse::Idle,
        S::Uncertain | S::Unknown => Coarse::Unknown,
    }
}

fn fresh_belief(generation: GenerationId) -> AgentBelief {
    AgentBelief {
        generation,
        state: AgentRuntimeState::Unknown,
        source: ObservationSource::ProcessIdentity,
        sequence: 0,
        visible_blocker: false,
        candidates: Vec::new(),
    }
}

/// The daemon's own wall clock, as unix seconds.
///
/// The single source for activity stamps. The hook's wire clock is advisory
/// and stays that way: stamping from it would let a report backdate or
/// predate a row, and the acceptance order would stop meaning anything.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Settle a terminal whose foreground process just exited.
///
/// The identity is gone with the process. The belief is reset and becomes Idle
/// through the acceptance rule rather than around it: a bare screen Idle
/// offered to the previous Hook-held blocker would be `Outranked` and the dead
/// terminal would stick on `blocked` forever. Reset first, then synthesize, is
/// the composition the test below pins.
fn settle_exit(agent: &mut TerminalAgent, stream: u32, now_unix: i64) {
    agent.identity = AgentIdentity::None;
    agent.leader = None;
    agent.belief = fresh_belief(agent.capability.generation);
    // A dead process shows no dialog: the exit voids any gate the old
    // screen set, with the old belief. The old screen's rule reading goes
    // with it, for the same reason.
    agent.setup_gate = false;
    agent.screen_rule = None;
    agent.background_agents = 0;
    agent.background_tasks = 0;
    agent.subagents.clear();
    agent.screen_manifest = None;
    let idle = qoop_domain::AgentObservation {
        agent_id: qoop_domain::AgentId::new(stream.to_string()),
        terminal_id: agent.capability.terminal.clone(),
        sampled_generation: agent.belief.generation,
        state: AgentRuntimeState::Idle,
        source: ObservationSource::ProcessIdentity,
        confidence: qoop_domain::Confidence::High,
        sequence: agent.belief.sequence.saturating_add(1),
        visible_blocker: false,
        visible_idle: false,
        visible_working: false,
    };
    if let Ok(next) = agent.belief.clone().accept(&idle) {
        agent.belief = next;
        agent.last_active_at_unix = Some(now_unix);
        agent.last_io_unix = Some(now_unix);
    }
}

impl AgentRegistry {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            watch: ProcWatch::new()?,
            tokens: Mutex::new(BTreeMap::new()),
            task_terminals: Mutex::new(BTreeSet::new()),
            tasks_due: Arc::new(AtomicBool::new(false)),
            by_pid: Mutex::new(HashMap::new()),
            said: Mutex::new(HashMap::new()),
            terminals: Mutex::new(BTreeMap::new()),
            focus: Mutex::new(BTreeMap::new()),
            prompts: Mutex::new(BTreeMap::new()),
            stops: Mutex::new(BTreeMap::new()),
            masters: Mutex::new(HashMap::new()),
            io: tokio::sync::Notify::new(),
            leaders: Mutex::new(HashMap::new()),
            closed: Mutex::new(BTreeSet::new()),
            expectations: Mutex::new(BTreeMap::new()),
            shield_at_unix: Mutex::new(BTreeMap::new()),
            typed_at_unix: Mutex::new(BTreeMap::new()),
            parked: Mutex::new(BTreeMap::new()),
            unparked: Mutex::new(Vec::new()),
            sent_at_unix: Mutex::new(BTreeMap::new()),
            person_typed: Mutex::new(BTreeMap::new()),
            person_sent: Mutex::new(BTreeMap::new()),
            person_draft: Mutex::new(std::collections::BTreeSet::new()),
            changes: tokio::sync::broadcast::channel(64).0,
            announce_due: Arc::new(Mutex::new(BTreeSet::new())),
            commands: Mutex::new(BTreeMap::new()),
            typed_by: Mutex::new(BTreeMap::new()),
            tasks: tokio::sync::broadcast::channel(16).0,
        })
    }

    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.watch.raw_fd()
    }

    /// Remember a terminal's master, so its foreground group can be re-read on an event.
    pub fn attach_master(&self, stream: u32, master: std::os::fd::RawFd) {
        self.masters.lock().unwrap().insert(stream, master);
    }

    /// Forget a terminal's master before its descriptor closes.
    ///
    /// Left until the terminal was forgotten, the number outlived the descriptor: a new
    /// terminal opened meanwhile can be handed the same number, and a foreground re-read
    /// through the stale entry read that other terminal's group and recorded its leader here.
    pub fn detach_master(&self, stream: u32) {
        self.masters.lock().unwrap().remove(&stream);
        self.leaders.lock().unwrap().remove(&stream);
    }

    /// Remember a terminal's leader and its terminal device, for the sleep walk.
    pub fn attach_leader(&self, stream: u32, pid: i32, tty: u64) {
        self.leaders.lock().unwrap().insert(stream, (pid, tty));
    }

    /// A terminal's leader pid and terminal device, while it runs.
    pub fn leader_of(&self, stream: u32) -> Option<(i32, u64)> {
        self.leaders.lock().unwrap().get(&stream).copied()
    }

    /// Re-read a terminal's foreground group and apply any change.
    ///
    /// Called after a process event rather than on a timer. A shell forks, then moves the
    /// child into the foreground, and the fork is the only notice we get that the second
    /// step is coming: the child may not have been moved yet when the event arrives, which
    /// is why the caller re-reads over a short settle window rather than once.
    pub fn refresh_foreground(&self, stream: u32, now_unix: i64) -> Option<GenerationId> {
        let master = self.masters.lock().unwrap().get(&stream).copied()?;
        // Never held back by `RELOOK`: this runs because a process event said the group
        // changed, and the look it makes is the one that finds an agent a wrapper forked.
        self.look(
            stream,
            qoop_pty::sys::foreground_leader(master),
            false,
            now_unix,
        )
    }

    /// Start tracking a terminal, and mint the token its child will carry.
    pub fn register(&self, stream: u32, terminal: TerminalId, token: CapabilityToken) {
        let generation = GenerationId::FIRST;
        self.terminals.lock().unwrap().insert(
            stream,
            TerminalAgent {
                capability: TerminalCapability {
                    terminal,
                    token,
                    generation,
                },
                identity: AgentIdentity::None,
                belief: fresh_belief(generation),
                last_active_at_unix: None,
                last_io_unix: None,
                setup_gate: false,
                background_agents: 0,
                background_tasks: 0,
                subagents: Default::default(),
                screen_rule: None,
                screen_manifest: None,
                leader: None,
                relooked: None,
                relook_owed: false,
                born: None,
                seen: Vec::new(),
            },
        );
        self.announce_new(stream);
    }

    /// What this terminal's record says, for the daemon that takes it over.
    pub fn export(&self, stream: u32) -> Option<AgentCarry> {
        let terminals = self.terminals.lock().unwrap();
        let agent = terminals.get(&stream)?;
        Some(AgentCarry {
            generation: agent.capability.generation,
            identity: agent.identity.clone(),
            belief: agent.belief.clone(),
            leader: agent.leader.map(|pid| pid.0),
            last_active_at_unix: agent.last_active_at_unix,
            last_io_unix: agent.last_io_unix,
            setup_gate: agent.setup_gate,
            background_agents: agent.background_agents,
            background_tasks: agent.background_tasks,
            subagents: agent.subagents.clone(),
            screen_rule: agent.screen_rule.clone(),
            screen_manifest: agent.screen_manifest.clone(),
        })
    }

    /// Put a carried record into a terminal just registered, before anything reads it. No
    /// startup grace: the agent was not started now.
    ///
    /// A leader that can no longer be watched is gone, and what the carry says of it is
    /// kept without it: the next look finds whatever is there and says so as usual.
    pub fn adopt(&self, stream: u32, carry: AgentCarry) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return;
        };
        agent.capability.generation = carry.generation;
        agent.identity = carry.identity;
        agent.belief = carry.belief;
        agent.last_active_at_unix = carry.last_active_at_unix;
        agent.last_io_unix = carry.last_io_unix;
        agent.setup_gate = carry.setup_gate;
        agent.background_agents = carry.background_agents;
        agent.background_tasks = carry.background_tasks;
        agent.subagents = carry.subagents;
        agent.screen_rule = carry.screen_rule;
        agent.screen_manifest = carry.screen_manifest;
        agent.born = None;
        if let Some(pid) = carry.leader.map(Pid)
            && matches!(self.watch.watch(pid), Ok(true))
        {
            self.by_pid.lock().unwrap().insert(pid.0, stream);
            agent.leader = Some(pid);
        }
    }

    /// Seed both activity clocks at spawn. Creation counts as activity, so a
    /// terminal that never produces an accepted observation still ages from
    /// a known point instead of living forever stamp-`None`. Only fills
    /// clocks that are still `None`: seeding must never move a live stamp
    /// backwards. Every creation path (first boot, `CreateTerminal`,
    /// restore) flows through `open_terminal`, which calls this right after
    /// `register`, so restored fleets get one full fresh grace period after
    /// every restart.
    pub fn note_spawn(&self, stream: u32, now_unix: i64) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return;
        };
        if agent.last_active_at_unix.is_none() {
            agent.last_active_at_unix = Some(now_unix);
        }
        if agent.last_io_unix.is_none() {
            agent.last_io_unix = Some(now_unix);
        }
    }

    /// Record live traffic on a terminal: PTY output or live client input.
    ///
    /// Once per PTY read chunk, never per byte, on paths that already hold
    /// no lock (this takes its own). Restore replay input must not call
    /// this: replayed bytes are not live activity.
    pub fn note_io(&self, stream: u32, now_unix: i64) {
        if let Some(agent) = self.terminals.lock().unwrap().get_mut(&stream) {
            agent.last_io_unix = Some(now_unix);
        }
        self.io.notify_one();
    }

    /// Wait for any terminal's traffic since the last wait.
    pub async fn io_seen(&self) {
        self.io.notified().await;
    }

    /// The terminals whose foreground group is not the one last read: one ioctl each, for the
    /// probe platform, which has no event to say which terminal moved. The ioctls run before
    /// the lock is taken, so a hundred terminals never hold it for their reads.
    pub fn moved(&self) -> Vec<u32> {
        let masters: Vec<(u32, std::os::fd::RawFd)> = self
            .masters
            .lock()
            .unwrap()
            .iter()
            .map(|(stream, master)| (*stream, *master))
            .collect();
        let now: Vec<(u32, Option<Pid>)> = masters
            .into_iter()
            .map(|(stream, master)| (stream, qoop_pty::sys::foreground_leader(master)))
            .collect();
        let terminals = self.terminals.lock().unwrap();
        now.into_iter()
            .filter(|(stream, leader)| terminals.get(stream).is_some_and(|a| a.leader != *leader))
            .map(|(stream, _)| stream)
            .collect()
    }

    /// Record that a person typed into this terminal. The focus reports an interface sends
    /// when its window changes are not typing.
    pub fn note_typed(&self, stream: u32, bytes: &[u8], now_unix: i64) {
        if bytes == b"\x1b[I" || bytes == b"\x1b[O" {
            return;
        }
        self.typed_at_unix.lock().unwrap().insert(stream, now_unix);
    }

    /// Record that a line reached this terminal from a person: input with an Enter in it.
    pub fn note_sent(&self, stream: u32, bytes: &[u8], now_unix: i64) {
        if has_enter(bytes) {
            self.sent_at_unix.lock().unwrap().insert(stream, now_unix);
        }
    }

    /// When this terminal last took input from a person: a key they pressed in an interface
    /// or the phone (never a mouse or terminal report), or a line sent (the inbox's answers
    /// too). A scheduled wake or a harness notice types nothing.
    pub fn input_at(&self, stream: u32) -> Option<i64> {
        let typed = self.person_typed.lock().unwrap().get(&stream).copied();
        let sent = self.sent_at_unix.lock().unwrap().get(&stream).copied();
        typed.max(sent)
    }

    /// Input from a person's own connection: a key they pressed stamps the draft clock, a line
    /// with a return the sent one. A paste is typed, never sent: its return waits for theirs.
    ///
    /// `kind` is the kind of client the keys came from (`AgentRow::typed_by`). A change of
    /// kind is news at once; the same kind typing on is news again every [`TYPING_NEWS_SECS`],
    /// so a client's "typing" never runs out while someone types, and keys are never news one
    /// by one.
    pub fn note_person(
        &self,
        stream: u32,
        bytes: &[u8],
        paste: bool,
        kind: &'static str,
        now_unix: i64,
    ) {
        if !paste && !person_key(bytes) {
            return;
        }
        self.person_typed.lock().unwrap().insert(stream, now_unix);
        let news = {
            let mut typed = self.typed_by.lock().unwrap();
            let was = typed.get(&stream).copied();
            let news = was.is_none_or(|(was_kind, _, told)| {
                was_kind != kind || now_unix - told >= TYPING_NEWS_SECS
            });
            let told = match was {
                Some((_, _, told)) if !news => told,
                _ => now_unix,
            };
            typed.insert(stream, (kind, now_unix, told));
            news
        };
        if news {
            self.announce_soon(stream);
        }
        // A return, or Enter as a kitty key report (`ESC[13u`, `ESC[13;1u`), sends the line and
        // ends the draft; any key after the last one in the same read starts the next draft.
        let (mut sent, mut after) = (false, false);
        let mut rest = bytes;
        while !rest.is_empty() && !paste {
            let len = if rest[0] == 0x1b {
                sequence_len(rest)
            } else {
                1
            };
            let key = &rest[..len];
            if is_enter(key) {
                (sent, after) = (true, false);
            } else if one_key(key) {
                after = true;
            }
            rest = &rest[len..];
        }
        if sent {
            self.person_sent.lock().unwrap().insert(stream, now_unix);
        }
        let mut draft = self.person_draft.lock().unwrap();
        if !sent || after {
            draft.insert(stream);
        } else {
            draft.remove(&stream);
        }
    }

    /// Until when a person is at this terminal, as unix seconds, `None` when nobody is: a line
    /// they sent holds it [`PERSON_SENT_SECS`], keys they pressed after their last line (a
    /// draft) [`PERSON_DRAFT_SECS`].
    pub fn person_until(&self, stream: u32, now_unix: i64) -> Option<i64> {
        let typed = self.person_typed.lock().unwrap().get(&stream).copied();
        let sent = self.person_sent.lock().unwrap().get(&stream).copied();
        let draft = typed
            .filter(|_| self.person_draft.lock().unwrap().contains(&stream))
            .map(|t| t + PERSON_DRAFT_SECS);
        let line = sent.map(|s| s + PERSON_SENT_SECS);
        draft
            .into_iter()
            .chain(line)
            .max()
            .filter(|until| *until > now_unix)
    }

    /// Seconds since a person last typed into this terminal, `None` when nobody has.
    pub fn typed_ago(&self, stream: u32, now_unix: i64) -> Option<i64> {
        let at = *self.typed_at_unix.lock().unwrap().get(&stream)?;
        Some((now_unix - at).max(0))
    }

    /// When this terminal was last live, the later of the two clocks, or
    /// `None` before the spawn seeding. Test only: production reads the
    /// same value through `reap_snapshot`, which holds the lock once for
    /// the whole snapshot rather than pairing two lock reads across a
    /// generation change.
    #[cfg(test)]
    pub fn idle_since(&self, stream: u32) -> Option<i64> {
        let terminals = self.terminals.lock().unwrap();
        let agent = terminals.get(&stream)?;
        match (agent.last_active_at_unix, agent.last_io_unix) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// One atomic snapshot for the idle reaper: state, program, boundary,
    /// gate and clocks read under a single lock hold, so a generation
    /// change cannot slip between two reads and pair a state from one
    /// generation with a program from the next. The reaper must never call
    /// `program_of` plus `agent()` separately for this reason.
    pub fn reap_snapshot(&self, stream: u32) -> Option<ReapSnapshot> {
        let terminals = self.terminals.lock().unwrap();
        let agent = terminals.get(&stream)?;
        let (program, is_agent, boundary) = match &agent.identity {
            AgentIdentity::Known { program, agent, .. } => (Some(program.clone()), *agent, false),
            AgentIdentity::Unavailable { .. } => (None, false, true),
            AgentIdentity::None => (None, false, false),
        };
        let idle_since = match (agent.last_active_at_unix, agent.last_io_unix) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Some(ReapSnapshot {
            generation: agent.belief.generation,
            state: agent.belief.state,
            program,
            agent: is_agent,
            boundary,
            setup_gate: agent.setup_gate,
            background_agents: agent.background_agents,
            idle_since,
        })
    }

    /// A copy of this terminal's master fd for the reaper's foreground-group
    /// check. Copied under the lock; the syscalls run outside it.
    pub fn master_of(&self, stream: u32) -> Option<std::os::fd::RawFd> {
        self.masters.lock().unwrap().get(&stream).copied()
    }

    /// Mark a stream an explicit close asked about. Call before asking, so a
    /// teardown that runs first still reads it, and idle sleep leaves the tab
    /// alone while it tears down. True when this call set the mark, false
    /// when an earlier close already had: only the caller that set it may
    /// take it back.
    pub fn mark_closed(&self, stream: u32) -> bool {
        self.closed.lock().unwrap().insert(stream)
    }

    /// Take back an explicit-close mark and say whether one was there: the
    /// close was refused, and the tab lives on; or the teardown reads it.
    pub fn take_closed(&self, stream: u32) -> bool {
        self.closed.lock().unwrap().remove(&stream)
    }

    /// Whether an explicit close is asking or was accepted for this stream
    /// and teardown has not run yet. Idle sleep skips these. A refused close
    /// takes its mark back, so at worst idle sleep waits one pass.
    pub fn is_closed(&self, stream: u32) -> bool {
        self.closed.lock().unwrap().contains(&stream)
    }

    /// The token this terminal's child was launched with, for binding a
    /// `from_terminal` claim to its sender (same strength as hook
    /// admission: per-terminal unguessable nonces, equality only, no
    /// generation involved since a CLI cannot know it).
    pub fn token_of(&self, stream: u32) -> Option<CapabilityToken> {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .map(|a| a.capability.token.clone())
    }

    /// Record that waiter asked target, now. One edge per waiter: asking
    /// again moves the wait, it never stacks.
    pub fn note_expectation(&self, waiter: u32, target: u32, now_unix: i64) {
        self.expectations
            .lock()
            .unwrap()
            .insert(waiter, (target, now_unix));
    }

    /// Waiter → (target, since), if one is recorded.
    pub fn expectation_of(&self, waiter: u32) -> Option<(u32, i64)> {
        self.expectations.lock().unwrap().get(&waiter).copied()
    }

    /// Every recorded edge as (waiter, target), for snapshot evaluation.
    pub fn expectation_edges(&self) -> Vec<(u32, u32)> {
        self.expectations
            .lock()
            .unwrap()
            .iter()
            .map(|(w, (t, _))| (*w, *t))
            .collect()
    }

    /// Drop every edge where this stream waits or is waited on.
    pub fn drop_expectations_touching(&self, stream: u32) {
        let mut edges = self.expectations.lock().unwrap();
        edges.remove(&stream);
        edges.retain(|_, (target, _)| *target != stream);
    }

    /// Drop edges older than [`EXPECTATION_TTL_SECS`]. Runs on every reap
    /// pass over all edges: a wait silent for a full day is over, and no
    /// edge may live forever.
    pub fn sweep_expectations(&self, now_unix: i64) {
        self.expectations
            .lock()
            .unwrap()
            .retain(|_, (_, since)| now_unix.saturating_sub(*since) <= EXPECTATION_TTL_SECS);
    }

    /// Proof of life beyond the activity clocks: an admitted hook focus
    /// event.
    pub fn note_shield(&self, stream: u32, now_unix: i64) {
        self.shield_at_unix.lock().unwrap().insert(stream, now_unix);
    }

    /// Whether the shield is fresher than the reap timeout.
    pub fn shield_fresh(&self, stream: u32, now_unix: i64, timeout_secs: i64) -> bool {
        self.shield_at_unix
            .lock()
            .unwrap()
            .get(&stream)
            .is_some_and(|at| now_unix.saturating_sub(*at) < timeout_secs)
    }

    /// Set a terminal's foreground identity directly, for reaper harness
    /// tests that cannot spawn a real process group. Test only.
    #[cfg(test)]
    pub fn test_set_foreground(&self, stream: u32, program: Option<&str>, boundary: bool) {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return;
        };
        let generation = agent.belief.generation;
        agent.identity = if boundary {
            AgentIdentity::Unavailable {
                boundary: Boundary::Namespace {
                    wrapper: "docker".into(),
                },
                generation,
            }
        } else {
            match program {
                // Test harness only: a bare name stands in for a process whose argv the
                // harness cannot give.
                Some(name) => AgentIdentity::Known {
                    program: name.into(),
                    generation,
                    agent: qoop_domain::program::identify(name, &[]).agent,
                },
                None => AgentIdentity::None,
            }
        };
    }

    pub fn forget(&self, stream: u32) {
        self.remove_live(stream);
        self.closed.lock().unwrap().remove(&stream);
        self.shield_at_unix.lock().unwrap().remove(&stream);
        self.typed_at_unix.lock().unwrap().remove(&stream);
        self.sent_at_unix.lock().unwrap().remove(&stream);
        self.person_typed.lock().unwrap().remove(&stream);
        self.person_sent.lock().unwrap().remove(&stream);
        self.person_draft.lock().unwrap().remove(&stream);
        self.commands.lock().unwrap().remove(&stream);
        self.typed_by.lock().unwrap().remove(&stream);
        self.drop_expectations_touching(stream);
        // A terminal that is gone is a change like any other. Closing one answers as soon
        // as the request is accepted, but the terminal itself lives until its teardown
        // finishes, which takes as long as the escalation in ADR-014 allows: measured, an
        // index still listed a closed terminal fifteen seconds later, because the client
        // had asked once, at the moment of the answer, and nothing told it to ask again.
        self.announce(stream);
    }

    /// Drop the live record without touching shields or edges.
    fn remove_live(&self, stream: u32) {
        self.said.lock().unwrap().remove(&stream);
        self.masters.lock().unwrap().remove(&stream);
        self.focus.lock().unwrap().remove(&stream);
        self.prompts.lock().unwrap().remove(&stream);
        self.stops.lock().unwrap().remove(&stream);
        if let Some(agent) = self.terminals.lock().unwrap().remove(&stream)
            && let Some(pid) = agent.leader
        {
            self.watch.unwatch(pid);
            self.by_pid.lock().unwrap().remove(&pid.0);
        }
    }

    /// A terminal that now exists is a change too, for the same reason.
    fn announce_new(&self, stream: u32) {
        self.announce(stream);
    }

    /// Every terminal being tracked.
    pub fn streams(&self) -> Vec<u32> {
        self.terminals.lock().unwrap().keys().copied().collect()
    }

    /// The name to read a terminal's screen by: the program when it is an agent, and
    /// `generic` for anything else that can be read.
    ///
    /// Not the bare program name, because a name is not enough to pick a manifest: a
    /// stranger's native `pi` or `muse` is not the agent of that name, and reading it with
    /// the agent's rules (and its idle fallback) would claim states for a program those
    /// rules were never written for. The carried agent bit decides.
    ///
    /// Its own accessor rather than a field read through `agent()`, because the read loop
    /// asks on every screen evaluation and cloning the whole record for a name is the kind
    /// of cost that only shows up at a hundred terminals.
    pub fn rules_name_of(&self, stream: u32) -> Option<String> {
        match &self.terminals.lock().unwrap().get(&stream)?.identity {
            AgentIdentity::Known {
                program,
                agent: true,
                ..
            } => Some(program.clone()),
            AgentIdentity::Known { agent: false, .. } => Some("generic".to_string()),
            _ => None,
        }
    }

    /// What a terminal's live row says, as far as a status reads it; no clone of the record.
    pub fn live(&self, stream: u32) -> Option<crate::control::Live> {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .map(|a| crate::control::Live {
                state: crate::control::state_name(a.belief.state),
                last_active: a.last_active_at_unix,
                subagents: subagents(a),
            })
    }

    pub fn agent(&self, stream: u32) -> Option<TerminalAgent> {
        self.terminals.lock().unwrap().get(&stream).cloned()
    }

    /// The terminal's foreground leader, as observed now.
    ///
    /// A change of leader is a new generation, and so is an `exec` by the same leader.
    /// Both clear the previous evidence: what was believed described a process that is no
    /// longer there, and carrying it forward is how a finished agent stays "working".
    ///
    /// This is the read loop's call, on every chunk of output, so a leader that is not an
    /// agent is looked into at most once per `RELOOK` from here.
    pub fn note_foreground(
        &self,
        stream: u32,
        leader: Option<Pid>,
        now_unix: i64,
    ) -> Option<GenerationId> {
        self.look(stream, leader, true, now_unix)
    }

    /// When the look `note_foreground` last held back is due, if one is owed.
    ///
    /// The read loop sleeps until then and asks again, so a held back look is late by at
    /// most `RELOOK` and never lost.
    pub fn relook_due(&self, stream: u32) -> Option<std::time::Instant> {
        let terminals = self.terminals.lock().unwrap();
        let agent = terminals.get(&stream)?;
        agent
            .relook_owed
            .then(|| agent.relooked.map(|at| at + RELOOK))
            .flatten()
    }

    /// `note_foreground`, with the look into a same leader held back only when `throttled`.
    fn look(
        &self,
        stream: u32,
        leader: Option<Pid>,
        throttled: bool,
        now_unix: i64,
    ) -> Option<GenerationId> {
        let mut terminals = self.terminals.lock().unwrap();
        let agent = terminals.get_mut(&stream)?;
        if agent.leader == leader {
            // The same leader, but not necessarily the same answer. A wrapper leads the
            // group and starts the agent a moment later, and the fork that starts it is not
            // a foreground change, so nothing else would ever look again: measured, an
            // opencode behind a tunnelling script stayed `opencode-vpn` for the life of the
            // session and never appeared on the agents panel at all.
            //
            // Only while the reading is not an agent, and for as long as that lasts. There
            // is no deadline here on purpose: the wrapper measured on a real machine sets
            // up a tunnel before it starts opencode, which took twenty five seconds, and
            // any window short enough to be cheap would have closed first.
            //
            // `RELOOK` is what bounds the cost instead: the read loop asks on every read.
            let provisional = matches!(&agent.identity, AgentIdentity::Known { agent: false, .. });
            if !provisional || leader.is_none() {
                // Settled some other way, an exec into an agent or an exit: nothing is
                // owed any more, and a debt left here would re-arm an expired timer on
                // every read.
                agent.relook_owed = false;
                return None;
            }
            let now = std::time::Instant::now();
            if throttled
                && agent
                    .relooked
                    .is_some_and(|at| now.duration_since(at) < RELOOK)
            {
                agent.relook_owed = true;
                return None;
            }
            agent.relooked = Some(now);
            agent.relook_owed = false;
            let pid = leader?;
            // Looked at with the generation it already has, because most of these look
            // again and find the same thing: minting one per probe would advance the
            // generation of every terminal running a wrapper several times a second, and a
            // generation is how everything else knows something actually changed.
            let current = agent.identity.generation().unwrap_or(GenerationId::FIRST);
            let program = match identity_of(pid, current) {
                AgentIdentity::Known {
                    program,
                    agent: true,
                    ..
                } => program,
                _ => return None,
            };
            // A different program is in this terminal than the one we had been reading, so
            // what was believed about the old one does not carry over, and that is a new
            // generation by definition.
            let generation = agent.advance();
            agent.note_leader(pid, now_unix);
            agent.last_active_at_unix = Some(now_unix);
            agent.last_io_unix = Some(now_unix);
            // The gate belonged to the old generation's screen: a new process
            // means the old dialog reading is void, not held. The old rule
            // reading is void with it.
            agent.setup_gate = false;
            agent.screen_rule = None;
            agent.background_agents = 0;
            agent.background_tasks = 0;
            agent.subagents.clear();
            agent.screen_manifest = None;
            agent.identity = AgentIdentity::Known {
                program,
                generation,
                agent: true,
            };
            drop(terminals);
            self.announce(stream);
            return Some(generation);
        }
        if let Some(old) = agent.leader.take() {
            self.watch.unwatch(old);
            self.by_pid.lock().unwrap().remove(&old.0);
        }
        let generation = agent.advance();
        // A new foreground generation is activity even before anything is
        // known about the process: something replaced what was there.
        agent.last_active_at_unix = Some(now_unix);
        agent.last_io_unix = Some(now_unix);
        // A new foreground generation voids the old screen's gate with the
        // old belief: whatever dialog was showing belonged to a process that
        // is no longer there. The old rule reading belonged to it too.
        agent.setup_gate = false;
        agent.screen_rule = None;
        agent.background_agents = 0;
        agent.background_tasks = 0;
        agent.subagents.clear();
        agent.screen_manifest = None;
        agent.identity = match leader {
            None => AgentIdentity::None,
            Some(pid) => {
                self.by_pid.lock().unwrap().insert(pid.0, stream);
                // A pid that is already gone is not watched and is not an identity.
                match self.watch.watch(pid) {
                    Ok(true) => identity_of(pid, generation),
                    _ => AgentIdentity::None,
                }
            }
        };
        agent.leader = leader;
        if let Some(pid) = leader {
            agent.note_leader(pid, now_unix);
        }
        agent.relooked = None;
        agent.relook_owed = false;
        // Dropped before announcing: a subscriber that reacts by asking what is there would
        // otherwise be asking while this lock is still held.
        drop(terminals);
        self.announce(stream);
        Some(generation)
    }

    /// Apply whatever the platform has reported. Returns the terminals whose generation
    /// changed, so a caller can tell clients.
    pub fn apply_events(&self, now_unix: i64) -> Vec<u32> {
        let mut changed = Vec::new();
        for event in self.watch.drain() {
            let (pid, exited) = match event {
                ProcEvent::Exec(pid) => (pid, false),
                ProcEvent::Exit(pid) => (pid, true),
                ProcEvent::Fork(pid) => {
                    // Not a change in itself. What it says is that the foreground group is
                    // about to move, so the terminal is marked for a settle rather than
                    // acted on: reading now usually still finds the forking shell.
                    if let Some(stream) = self.by_pid.lock().unwrap().get(&pid.0).copied() {
                        changed.push(stream);
                    }
                    continue;
                }
            };
            let Some(stream) = self.by_pid.lock().unwrap().get(&pid.0).copied() else {
                continue;
            };
            let mut terminals = self.terminals.lock().unwrap();
            let Some(agent) = terminals.get_mut(&stream) else {
                continue;
            };
            let generation = agent.advance();
            // An exec replaced the program: activity. An exit settles below,
            // and `settle_exit` stamps the idle it synthesizes, so the exit
            // path stamps nothing here: one stamp per event, at the belief
            // that actually holds afterwards.
            if !exited {
                agent.last_active_at_unix = Some(now_unix);
                agent.last_io_unix = Some(now_unix);
            }
            // The gate belonged to the exited or execed process: a new
            // generation voids the old screen's reading with the old belief.
            // The old rule reading goes with it.
            agent.setup_gate = false;
            agent.screen_rule = None;
            agent.background_agents = 0;
            agent.background_tasks = 0;
            agent.subagents.clear();
            agent.screen_manifest = None;
            if exited {
                self.by_pid.lock().unwrap().remove(&pid.0);
                settle_exit(agent, stream, now_unix);
            } else {
                // The same pid, a different program. This is the case a pid keyed identity
                // gets wrong, and it is the common one: a shell that becomes an agent.
                agent.identity = identity_of(pid, generation);
            }
            changed.push(stream);
        }
        for stream in &changed {
            self.announce(*stream);
        }
        changed
    }

    /// Whether an observation claiming to be from this terminal is admitted.
    /// Record what a terminal's screen says about the agent in it.
    ///
    /// The screen is the weakest source in ADR-008's order and it goes through the same
    /// acceptance rule as every other one: a hook that said `working` a moment ago is not
    /// overruled by a screen that has not caught up, and a screen that positively saw a
    /// blocking prompt is the one case that may overrule a stronger source. None of that
    /// is decided here, which is the point: there is one acceptance rule and this is a
    /// caller of it.
    ///
    /// Answers whether anything changed, so a caller can avoid announcing a non event.
    pub fn observe_screen(
        &self,
        stream: u32,
        manifest: &str,
        verdict: &qoop_detect::Verdict,
        now_unix: i64,
    ) -> bool {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return false;
        };
        // Recorded before acceptance, and kept when outranked: the rule says
        // what the screen claimed, the belief says what was concluded, and an
        // explanation needs both halves to say why a state reads the way it
        // does.
        agent.screen_rule = Some(verdict.rule.clone());
        agent.screen_manifest = Some(manifest.to_string());
        let generation = agent.belief.generation;
        let observation = qoop_domain::AgentObservation {
            agent_id: qoop_domain::AgentId::new(stream.to_string()),
            terminal_id: agent.capability.terminal.clone(),
            sampled_generation: generation,
            state: verdict.state,
            source: qoop_domain::ObservationSource::ScreenHeuristic,
            confidence: qoop_domain::Confidence::Medium,
            // One per terminal per generation, so two screens in a row cannot arrive out of
            // order with each other.
            sequence: agent.belief.sequence.saturating_add(1),
            visible_blocker: verdict.visible_blocker,
            visible_idle: verdict.visible_idle,
            visible_working: verdict.visible_working,
        };
        let before = agent.belief.state;
        let before_blocker = agent.belief.visible_blocker;
        let before_gate = agent.setup_gate;
        // `accept` is the one that consumes the belief and returns the new one, so an
        // observation cannot be applied without going through the acceptance rule first.
        let Ok(next) = agent.belief.clone().accept(&observation) else {
            return false;
        };
        agent.belief = next;
        // The gate rides with the accepted reading, not beside it: a refused
        // observation changes nothing, and a later reading without a gate
        // clears one, because the dialog is gone when the screen no longer
        // shows it.
        agent.setup_gate = verdict.setup_gate;
        // Published when anything observable changed: the state, the visible
        // blocker, or the gate. A soak of identical evaluations publishes nothing.
        let changed = agent.belief.state != before
            || agent.belief.visible_blocker != before_blocker
            || agent.setup_gate != before_gate;
        // An accepted change is activity; a refused observation stamps
        // nothing, and the rule recording above stays stamp-free with it.
        if changed {
            agent.last_active_at_unix = Some(now_unix);
            agent.last_io_unix = Some(now_unix);
        }
        drop(terminals);
        if changed {
            self.announce(stream);
        }
        changed
    }

    pub fn admit(
        &self,
        stream: u32,
        terminal: &TerminalId,
        token: Option<&CapabilityToken>,
        sampled: GenerationId,
    ) -> Result<(), ClaimRefusal> {
        let terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get(&stream) else {
            return Err(ClaimRefusal::WrongTerminal);
        };
        agent.capability.admit(terminal, token, sampled)
    }

    /// Record what a hook or plugin inside the terminal reports about the agent.
    ///
    /// The claim goes through the existing `admit()` check first: a report with
    /// the wrong token is a forgery and never becomes an observation. The wire
    /// clock is advisory only and is not even a parameter here, which is what
    /// makes "the wall time leaked into the sequence" inexpressible: the
    /// sequence is assigned from the same per-terminal counter the screen path
    /// uses, so a hook's huge nanos can never dwarf a later screen reading.
    /// Sampled under the current generation, because a hook cannot know the
    /// generation and anything older would be refused as stale anyway.
    ///
    /// Answers whether anything changed, or why the report was refused.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_hook(
        &self,
        stream: u32,
        terminal: &TerminalId,
        token: &CapabilityToken,
        state: AgentRuntimeState,
        visible_blocker: bool,
        visible_idle: bool,
        now_unix: i64,
    ) -> Result<bool, ClaimRefusal> {
        self.observe_hook_event(
            stream,
            terminal,
            token,
            state,
            visible_blocker,
            visible_idle,
            now_unix,
            None,
        )
    }

    /// [`Self::observe_hook`] for a report that carries a focus event: an admitted `done` is a
    /// new stop, and a `user`, `tool` or `blocked` one is work that ends the last stop. Set
    /// after the token check and before the state is announced, so the first row that shows
    /// the rest names its stop; a changed stop announces the row even when the state held.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_hook_event(
        &self,
        stream: u32,
        terminal: &TerminalId,
        token: &CapabilityToken,
        state: AgentRuntimeState,
        visible_blocker: bool,
        visible_idle: bool,
        now_unix: i64,
        event: Option<&str>,
    ) -> Result<bool, ClaimRefusal> {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return Err(ClaimRefusal::WrongTerminal);
        };
        let generation = agent.belief.generation;
        agent.capability.admit(terminal, Some(token), generation)?;
        let stopped = match event {
            Some("done") => {
                let mut stops = self.stops.lock().unwrap();
                let entry = stops.entry(stream).or_insert((0, None));
                *entry = (entry.0 + 1, Some((now_unix, generation)));
                true
            }
            Some("user" | "tool" | "blocked") => {
                if let Some(entry) = self.stops.lock().unwrap().get_mut(&stream) {
                    entry.1 = None;
                }
                false
            }
            _ => false,
        };
        let observation = qoop_domain::AgentObservation {
            agent_id: qoop_domain::AgentId::new(stream.to_string()),
            terminal_id: agent.capability.terminal.clone(),
            sampled_generation: generation,
            state,
            source: qoop_domain::ObservationSource::Hook,
            confidence: qoop_domain::Confidence::High,
            sequence: agent.belief.sequence.saturating_add(1),
            visible_blocker,
            visible_idle,
            visible_working: false,
        };
        let before = agent.belief.state;
        let before_blocker = agent.belief.visible_blocker;
        let before_gate = agent.setup_gate;
        let Ok(next) = agent.belief.clone().accept(&observation) else {
            drop(terminals);
            if stopped {
                self.announce(stream);
            }
            return Ok(false);
        };
        agent.belief = next;
        // A hook report comes from inside a live session, so the session
        // exists no matter what the screen last showed: accepting it clears
        // a startup gate a refused screen reading would otherwise hold
        // forever.
        agent.setup_gate = false;
        let changed = agent.belief.state != before
            || agent.belief.visible_blocker != before_blocker
            || before_gate;
        // An admitted report that changed something is activity; a refused
        // or outranked one stamps nothing.
        if changed {
            agent.last_active_at_unix = Some(now_unix);
            agent.last_io_unix = Some(now_unix);
        }
        drop(terminals);
        if changed || stopped {
            self.announce(stream);
        }
        Ok(changed)
    }

    /// Record one admitted focus event for a terminal.
    ///
    /// The caller admits first: only reports through the token check reach
    /// here, so a forged token cannot write the log. The kind is already
    /// validated against the closed set and the label already truncated;
    /// this only caps the per-terminal log. Admission is per-terminal, so
    /// an event can only ever shield its own tab: cross-terminal
    /// self-shielding is inexpressible here.
    ///
    /// An admitted event is also shield proof of life: an agent
    /// mid-delegation emits `tool` events while its belief still reads
    /// Idle, and the reaper must not mistake that wait for idleness.
    /// `at_unix` is daemon time at admission (see the call site), never
    /// the hook's advisory wire clock, so a skewed hook cannot stretch
    /// the shield.
    /// Keep a turn end's closing words for `stream`, masked; none forgets the last ones.
    pub fn set_said(&self, stream: u32, said: Option<&str>) {
        let mut all = self.said.lock().unwrap();
        // Cut before masking, as a label is: the hook sends at most 800 characters.
        let cut = said.map(|s| s.chars().take(1200).collect::<String>());
        match cut
            .as_deref()
            .map(qoop_parsed::redact)
            .filter(|s| !s.trim().is_empty())
        {
            Some(text) => {
                all.insert(stream, text);
            }
            None => {
                all.remove(&stream);
            }
        }
    }

    /// The closing words of `stream`'s last turn end: what it just did, and its question.
    pub fn said(&self, stream: u32) -> Option<(Option<String>, String)> {
        let all = self.said.lock().unwrap();
        let text = all.get(&stream)?;
        Some(match text.rsplit_once("\n\n") {
            Some((before, last)) => (Some(before.trim().to_string()), last.trim().to_string()),
            None => (None, text.trim().to_string()),
        })
    }

    pub fn push_focus_event(&self, stream: u32, event: crate::control::FocusEvent) {
        self.note_shield(stream, event.at_unix);
        // A harness message in the prompt's place (a task notification, pasted content, a
        // command's output) is not what the person asked: never a goal.
        if event.kind == "user" && !event.label.is_empty() && !harness_text(&event.label) {
            self.prompts
                .lock()
                .unwrap()
                .insert(stream, (event.label.clone(), event.at_unix));
        }
        let mut focus = self.focus.lock().unwrap();
        let log = focus.entry(stream).or_default();
        log.push_back(event);
        while log.len() > crate::control::MAX_FOCUS_EVENTS {
            log.pop_front();
        }
    }

    /// What a terminal's agent is doing right now, from its newest focus events: the
    /// newest non-empty tool or blocked label, `thinking` when a prompt is newer than any
    /// such label, and `None` once the turn ended or before anything was said.
    ///
    /// Labels, not the prompt: a `user` event reads as the word `thinking`, so what a
    /// person typed is not shown as activity. Its redacted head is kept apart (`prompts`)
    /// and becomes the goal of a task qoop opens at an edit, which is stored and listed with
    /// that terminal's row. Empty labels are skipped rather than
    /// taken, because a labelled permission request is followed by a label-less
    /// notification that says the same thing with less.
    pub fn activity(&self, stream: u32) -> Option<String> {
        let focus = self.focus.lock().unwrap();
        for event in focus.get(&stream)?.iter().rev() {
            match event.kind.as_str() {
                "done" => return None,
                "user" => return Some("thinking".to_string()),
                // A dialog's label is the daemon's, not a word for the sidebar.
                "tool" | "blocked"
                    if !event.label.is_empty() && !crate::asks::is_dialog(&event.label) =>
                {
                    return Some(event.label.clone());
                }
                _ => {}
            }
        }
        None
    }

    /// The terminal's current stop, `<at>.<seq>`: the newest admitted turn end, while no work
    /// came after it. `None` before the first, after new work, after a daemon restart, and for a
    /// new process in the terminal (a new generation) until its own first.
    pub fn stop_of(&self, stream: u32) -> Option<String> {
        let generation = self
            .terminals
            .lock()
            .unwrap()
            .get(&stream)?
            .belief
            .generation;
        let stops = self.stops.lock().unwrap();
        let (seq, newest) = stops.get(&stream)?;
        newest
            .filter(|(_, g)| *g == generation)
            .map(|(at, _)| format!("{at}.{seq}"))
    }

    /// The newest prompt head this terminal's hook reported, and when, if any.
    pub fn last_prompt(&self, stream: u32) -> Option<(String, i64)> {
        self.prompts.lock().unwrap().get(&stream).cloned()
    }

    /// A terminal's focus events, oldest first.
    pub fn focus_events(&self, stream: u32) -> Vec<crate::control::FocusEvent> {
        self.focus
            .lock()
            .unwrap()
            .get(&stream)
            .map(|log| log.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Whether a prompt head is an agent harness's own message: it opens with a tag whose name
/// is lower case with a dash or an underscore (`<task-notification>`, `<agent-message from=..>`,
/// `<pasted_content id=..>`, `<command-name>`). A request that only starts with `<`, such as
/// "`<Header>` overlaps the nav", is the person's and is kept.
fn harness_text(label: &str) -> bool {
    let Some(rest) = label.trim_start().strip_prefix('<') else {
        return false;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '-' || *c == '_')
        .collect();
    let after = rest[name.len()..].chars().next();
    name.contains(['-', '_']) && matches!(after, Some('>' | ' '))
}

fn identity_of(pid: Pid, generation: GenerationId) -> AgentIdentity {
    // The leader of the foreground group first, since it is the process a person started
    // and the answer in every ordinary case.
    let leader = match qoop_pty::sys::program_of(pid) {
        None => return AgentIdentity::None,
        Some(path) => match boundary_for(&path) {
            Some(boundary) => {
                return AgentIdentity::Unavailable {
                    boundary,
                    generation,
                };
            }
            None => {
                // The name a person would say, not the executable that happens to be
                // running it. An agent installed as a node package runs as `node
                // /path/cli.js`, and an index that says `node` has told nobody anything.
                // `qoop_domain::program` decides; the argv is only fetched here.
                let argv = qoop_pty::sys::argv_of(pid);
                qoop_domain::program::identify(&path, &argv)
            }
        },
    };
    // An agent started through a wrapper is not the leader: the wrapper is. Both are in the
    // foreground group, because starting one job puts the whole job in one group, so the
    // agent is in there somewhere. Measured on a real machine: opencode launched through a
    // shell script that tunnels it read as `opencode-vpn`, which is not an agent by any
    // table, and the agents panel left the terminal out of the list entirely.
    //
    // Only a known agent is promoted. Everything else the group happens to contain, the
    // `rg` an agent is running this second included, is not an answer to "what is in this
    // terminal", and a rule that took the deepest child would report exactly that.
    if !leader.agent {
        let members = qoop_pty::sys::group_members(qoop_pty::Pgid(pid.0));
        if let Some(found) = members
            .into_iter()
            .filter(|member| member.0 != pid.0)
            .filter_map(|member| {
                let path = qoop_pty::sys::program_of(member)?;
                if boundary_for(&path).is_some() {
                    return None;
                }
                let argv = qoop_pty::sys::argv_of(member);
                let program = qoop_domain::program::identify(&path, &argv);
                program.agent.then_some(program)
            })
            .next()
        {
            return AgentIdentity::Known {
                program: found.name,
                generation,
                agent: true,
            };
        }
    }
    AgentIdentity::Known {
        program: leader.name,
        generation,
        agent: leader.agent,
    }
}

/// The token a terminal's child is launched with.
///
/// Not a secret in the cryptographic sense, and it does not need to be: it is handed to a
/// child of a terminal we started, over an environment variable nobody else can read, and
/// its job is to tell that child's observations apart from anybody else's.
pub fn mint_token(stream: u32) -> CapabilityToken {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    CapabilityToken::new(format!("qoop-{stream}-{nonce:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed wall clock for activity stamps: tests pin behavior, never time.
    const NOW: i64 = 1_750_000_000;
    /// A second reading of the same clock: a stamp that must not move stays put.
    const LATER: i64 = 1_750_000_100;

    fn registry() -> AgentRegistry {
        AgentRegistry::new().expect("registry")
    }

    /// Keys a person presses count; what a terminal sends by itself does not.
    #[test]
    fn a_person_key_is_one_they_pressed() {
        for key in [
            &b"a"[..],
            b"\r",
            b"\x7f",
            b"\x1b",
            b"\x1b[27u",
            b"\x1b[A",
            b"\x1b[1;5C",
            b"\x1b[3~",
            b"\x1bOP",
            b"\x1bb",
            b"\x1b[97;1:3u",
            b"ab\x1b[D",
            b"\x1b[Ahello",
            b"\x1b[I\x1b[A",
        ] {
            assert!(person_key(key), "{key:?}");
        }
        for not in [
            &b""[..],
            b"\x1b[<0;10;5M",
            b"\x1b[M !!",
            b"\x1b[I",
            b"\x1b[O",
            b"\x1b[?62;22c",
            b"\x1b[12;40R",
            b"\x1b]11;rgb:0000/0000/0000\x1b\\",
            b"\x1b[>1;10;0c",
            b"\x1b[?0u",
            b"\x1b[?2004;1$y",
            b"\x1b[?997;1n",
            b"\x1b[8;40;120t",
            b"\x1bP1$r0m\x1b\\",
            b"\x1b[I\x1b[<0;3;4M",
        ] {
            assert!(!person_key(not), "{not:?}");
        }
    }

    /// A line holds the tab two minutes, a draft (keys after it, an Esc) three; both run out.
    #[test]
    fn a_person_holds_a_tab_while_they_type_or_just_sent() {
        let r = registry();
        assert_eq!(r.person_until(4, NOW), None);
        r.note_person(4, b"\x1b[<0;1;1M", false, "computer", NOW);
        assert_eq!(r.person_until(4, NOW), None, "a mouse report is nobody");
        r.note_person(4, b"go on\r", false, "computer", NOW);
        assert_eq!(r.person_until(4, NOW + 10), Some(NOW + PERSON_SENT_SECS));
        assert_eq!(r.person_until(4, NOW + PERSON_SENT_SECS), None);
        r.note_person(4, b"\x1b", false, "computer", NOW + 200);
        assert_eq!(
            r.person_until(4, NOW + 210),
            Some(NOW + 200 + PERSON_DRAFT_SECS)
        );
        r.note_person(4, b"ok\r", false, "computer", NOW + 230);
        r.note_person(4, b"and th", false, "computer", NOW + 230);
        assert_eq!(
            r.person_until(4, NOW + 231),
            Some(NOW + 230 + PERSON_DRAFT_SECS),
            "a new draft in the second of a line is a draft"
        );
        r.note_person(4, b"\x1b[13u\x1b[97u", false, "computer", NOW + 240);
        assert_eq!(
            r.person_until(4, NOW + 241),
            Some(NOW + 240 + PERSON_DRAFT_SECS),
            "a key after a kitty Enter is a new draft"
        );
        r.note_person(4, b"\x1b[13u", false, "computer", NOW + 250);
        assert_eq!(
            r.person_until(4, NOW + 251),
            Some(NOW + 250 + PERSON_SENT_SECS),
            "a kitty Enter sends"
        );
        r.note_person(4, b"half a sente", true, "computer", NOW + 300);
        assert_eq!(
            r.person_until(4, NOW + 301),
            Some(NOW + 300 + PERSON_DRAFT_SECS),
            "a paste is a draft"
        );
        r.forget(4);
        assert_eq!(r.person_until(4, NOW + 301), None);
    }

    /// What one registry exports another adopts, through the file between them: the same
    /// generation and belief, no startup grace, and the token the child still holds is
    /// the one that admits a report.
    #[test]
    fn a_carried_agent_is_the_same_agent_in_the_registry_that_takes_it() {
        let old = registry();
        let token = mint_token(7);
        old.register(7, TerminalId::new("7"), token.clone());
        old.note_foreground(7, Some(Pid(std::process::id() as i32)), NOW);
        old.observe_hook(
            7,
            &TerminalId::new("7"),
            &token,
            AgentRuntimeState::Working,
            false,
            false,
            NOW,
        )
        .expect("admitted");
        let carry = old.export(7).expect("a record");
        let wire = serde_json::to_string(&carry).expect("serialises");
        let carry: AgentCarry = serde_json::from_str(&wire).expect("reads back");

        let new = registry();
        new.register(7, TerminalId::new("7"), token.clone());
        new.adopt(7, carry);
        let (before, after) = (old.agent(7).unwrap(), new.agent(7).unwrap());
        assert_eq!(after.belief, before.belief);
        assert_eq!(after.capability.generation, before.capability.generation);
        assert_eq!(after.identity, before.identity);
        assert_eq!(after.leader, before.leader);
        assert!(after.born.is_none(), "no startup grace for an old agent");
        new.observe_hook(
            7,
            &TerminalId::new("7"),
            &token,
            AgentRuntimeState::Idle,
            false,
            true,
            LATER,
        )
        .expect("the old token is admitted");
        assert_eq!(new.agent(7).unwrap().belief.state, AgentRuntimeState::Idle);
        // The same leader is not a new process: no generation was minted for it.
        new.note_foreground(7, before.leader, LATER);
        assert_eq!(
            new.agent(7).unwrap().capability.generation,
            before.capability.generation
        );
    }

    /// A stranger's program sharing an agent's name is read by the generic rules: the
    /// carried bit, not the name, picks the manifest.
    #[test]
    fn a_program_that_is_not_an_agent_is_read_by_the_generic_rules() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let set = |program: &str, agent: bool| {
            r.terminals.lock().unwrap().get_mut(&1).unwrap().identity = AgentIdentity::Known {
                program: program.into(),
                generation: GenerationId::FIRST,
                agent,
            };
        };
        set("pi", false);
        assert_eq!(r.rules_name_of(1).as_deref(), Some("generic"));
        set("pi", true);
        assert_eq!(r.rules_name_of(1).as_deref(), Some("pi"));
        r.terminals.lock().unwrap().get_mut(&1).unwrap().identity = AgentIdentity::None;
        assert_eq!(r.rules_name_of(1), None);
    }

    #[test]
    fn a_wrapper_is_reported_as_unavailable_and_not_by_name() {
        assert!(boundary_for("/usr/local/bin/docker").is_some());
        assert!(boundary_for("/opt/homebrew/bin/nsenter").is_some());
        assert!(boundary_for("/usr/bin/claude").is_none());
    }

    /// Measured on a real Claude Code start: the idle fallback read a half drawn trust
    /// dialog as idle, and a task typed at the "idle" agent chose the dialog's default,
    /// which exits. A foreground change opens the grace; the first generation does not.
    #[test]
    fn a_foreground_change_opens_the_startup_grace() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let now = std::time::Instant::now();
        assert_eq!(r.agent(1).expect("agent").grace_ends(now), None);

        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        let agent = r.agent(1).expect("agent");
        let ends = agent
            .grace_ends(std::time::Instant::now())
            .expect("inside the grace");
        assert!(ends <= std::time::Instant::now() + STARTUP_GRACE);
        assert_eq!(agent.grace_ends(ends), None, "over at its end");
    }

    #[test]
    fn a_new_foreground_process_mints_a_new_generation_and_clears_the_belief() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let before = r.agent(1).expect("agent");
        assert_eq!(before.capability.generation, GenerationId::FIRST);

        let me = Pid(std::process::id() as i32);
        let generation = r.note_foreground(1, Some(me), NOW).expect("a change");
        assert!(generation > before.capability.generation);
        let after = r.agent(1).expect("agent");
        assert_eq!(after.belief.state, AgentRuntimeState::Unknown);
        assert_eq!(after.belief.generation, generation);
        assert!(matches!(after.identity, AgentIdentity::Known { .. }));

        // The same leader again is not a change, and must not mint a generation: a
        // generation per read would reject every observation ever sampled.
        assert_eq!(r.note_foreground(1, Some(me), NOW), None);
    }

    /// A leader that is not an agent is looked into once per `RELOOK`, not on every read:
    /// the read loop asks on every chunk of output.
    #[test]
    fn a_leader_that_is_not_an_agent_is_looked_into_at_most_once_a_relook() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(r.agent(1).expect("agent").relooked, None);

        assert_eq!(r.note_foreground(1, Some(me), NOW), None);
        let first = r.agent(1).expect("agent").relooked.expect("looked into");
        for _ in 0..100 {
            assert_eq!(r.note_foreground(1, Some(me), NOW), None);
        }
        assert_eq!(r.agent(1).expect("agent").relooked, Some(first));
        // Held back, not dropped: the read loop is told when to ask again.
        assert_eq!(r.relook_due(1), Some(first + RELOOK));
        // A process event's look is never held back: it is the one that finds the agent a
        // wrapper just forked.
        assert_eq!(r.look(1, Some(me), false, NOW), None);
        assert!(r.agent(1).expect("agent").relooked > Some(first));
        assert_eq!(r.relook_due(1), None, "a look pays what was owed");
    }

    /// A debt for a leader that stopped being provisional some other way is dropped by the
    /// next look, not carried: the read loop would otherwise re-arm an expired timer.
    #[test]
    fn a_debt_for_a_settled_leader_is_dropped() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        r.note_foreground(1, Some(me), NOW);
        r.note_foreground(1, Some(me), NOW);
        assert!(r.relook_due(1).is_some());
        // Settled into an agent by another path.
        r.terminals.lock().unwrap().get_mut(&1).unwrap().identity = AgentIdentity::Known {
            program: "claude".into(),
            generation: GenerationId::FIRST,
            agent: true,
        };
        assert_eq!(r.note_foreground(1, Some(me), NOW), None);
        assert_eq!(r.relook_due(1), None);
    }

    /// Explain: the screen's last rule rides the terminal even when the reading
    /// is outranked, and dies with the generation it described. An explanation
    /// that only showed accepted readings could never say why a state stuck.
    #[test]
    fn the_screen_rule_is_recorded_even_when_outranked_and_cleared_on_a_new_generation() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        // A hook holds Blocked; a screen Working with no blocker is refused...
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Blocked,
            true,
            false,
            NOW,
        )
        .expect("applies");
        assert!(!r.observe_screen(
            1,
            "claude",
            &screen_verdict(AgentRuntimeState::Working, false),
            NOW
        ));
        // ...but the explanation still says what the screen claimed.
        let seen = r.agent(1).expect("agent");
        assert_eq!(seen.belief.state, AgentRuntimeState::Blocked);
        assert_eq!(seen.screen_rule.as_deref(), Some("test"));
        assert_eq!(seen.screen_manifest.as_deref(), Some("claude"));
        // A new foreground generation voids the old screen's reading with the
        // old belief: the rule described a process that is no longer there.
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        let after = r.agent(1).expect("agent");
        assert_eq!(after.screen_rule, None);
        assert_eq!(after.screen_manifest, None);
    }

    /// The count of background agents the screen shows rides to the reaper's snapshot, and a
    /// new foreground generation voids it: it counted agents inside a process that is gone.
    #[test]
    fn running_background_agents_reach_the_snapshot_and_a_new_generation_voids_them() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        assert_eq!(r.reap_snapshot(1).expect("snapshot").background_agents, 0);
        r.note_background(1, 2, 0);
        assert_eq!(r.reap_snapshot(1).expect("snapshot").background_agents, 2);
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(r.reap_snapshot(1).expect("snapshot").background_agents, 0);
    }

    /// Background work the footer shows is told to the index when the total moves and not
    /// on a repeat, changes no state (`idle` stays `idle` for whoever waits on it), and is
    /// void with the generation it was read from.
    #[test]
    fn background_work_is_announced_on_change_only_and_voided_by_a_new_generation() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let mut rx = r.subscribe();
        let state = || r.agent(1).expect("agent").belief.state;
        let before = state();

        r.note_background(1, 0, 0);
        assert!(rx.try_recv().is_err(), "nothing moved");
        r.note_background(1, 1, 2);
        rx.try_recv().expect("the index is told about the work");
        assert_eq!(r.background_of(1), (3, 1));
        r.note_background(1, 1, 2);
        assert!(rx.try_recv().is_err(), "a repeat is not news");
        assert_eq!(state(), before, "the state is left alone");

        r.note_background(1, 0, 0);
        rx.try_recv().expect("the work ended");
        r.note_background(1, 0, 1);
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(
            r.background_of(1),
            (0, 0),
            "counted inside a process now gone"
        );
    }

    /// A leader that was in the foreground, went to a shell and came back (ctrl+z, `fg`) keeps
    /// the time it was first seen, though every change of foreground is a new generation; the
    /// time of one never seen is `None`, and the list is capped.
    #[test]
    fn a_leader_that_comes_back_keeps_the_time_it_was_first_seen() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let seen = |r: &AgentRegistry| r.agent(1).expect("agent").leader_seen_ms();
        let go = |pid: i32, at: i64| {
            let mut terminals = r.terminals.lock().unwrap();
            let agent = terminals.get_mut(&1).unwrap();
            agent.advance();
            agent.leader = Some(Pid(pid));
            agent.note_leader(Pid(pid), at);
        };
        assert_eq!(seen(&r), None, "no leader yet");
        go(100, 1_000);
        assert_eq!(seen(&r), Some(1_000_000));
        go(200, 2_000);
        go(100, 3_000);
        assert_eq!(
            seen(&r),
            Some(1_000_000),
            "the agent started when it first did"
        );
        assert_eq!(r.agent(1).unwrap().seen.len(), 2, "one entry per leader");
        for pid in 300..320 {
            go(pid, 4_000);
        }
        assert!(r.agent(1).unwrap().seen.len() <= SEEN_LEADERS);
        r.terminals.lock().unwrap().get_mut(&1).unwrap().leader = Some(Pid(7));
        assert_eq!(seen(&r), None, "a leader this daemon did not see come");
    }

    /// Three turns that drafted cards ended on a question the agent had put to its own
    /// sub-agents, each still running: none waits on the owner.
    /// The same words with no sub-agent at work do.
    #[test]
    fn a_turn_ending_while_its_sub_agents_run_asks_him_nothing() {
        use crate::control::SubagentChange::{Ended, Started};
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        r.terminals.lock().unwrap().get_mut(&1).unwrap().identity = AgentIdentity::Known {
            program: "claude".into(),
            generation: GenerationId::FIRST,
            agent: true,
        };
        let turns = [
            (
                "W692",
                "a0000000000000001",
                "Did who may close a task change? A worker on its own ... do the board, doctor, phone and `report.py` that read it break?",
            ),
            (
                "W702",
                "a0000000000000002",
                "1. **The root cause of the rise in wakes:** Is the rise real ... can it create the `play` file and run any code?",
            ),
            (
                "W705",
                "a0000000000000003",
                "Two sub-agents are checking the root fix. I asked one to look at ... . Does this undo the \"run the stuck work\" aim of `abc1234`?",
            ),
        ];
        for (card, sub, label) in turns {
            r.note_subagents(1, Started(sub.into()));
            let running = r.hooked_subagents_of(1);
            assert_eq!(running, 1, "{card}: its sub-agent runs");
            assert!(
                !crate::asks::turn_asks_him(Some(label), running),
                "{card}: a question to its own sub-agent is not his"
            );
            r.note_subagents(1, Ended(sub.into()));
            let running = r.hooked_subagents_of(1);
            assert!(
                crate::asks::turn_asks_him(Some(label), running),
                "{card}: the same question with nothing of its own at work is his"
            );
        }
        // A shell left running (a dev server) never keeps a question from him, nor does a
        // screen that listed a background agent earlier in the turn once the turn's own list
        // says none runs.
        r.note_background(1, 1, 1);
        assert_eq!(
            r.background_of(1),
            (2, 1),
            "the row still shows what the screen listed"
        );
        let running = r.hooked_subagents_of(1);
        assert_eq!(running, 0);
        assert!(crate::asks::turn_asks_him(Some("Merge it now?"), running));
    }

    /// Sub-agents a hook names are counted by id beside the agent, with what its screen lists
    /// as the floor: a start and its end cancel, the agent's own full list replaces the set,
    /// a shell's late hook counts nothing, and a new generation voids them. The state is
    /// never touched, and only a moved count is news.
    #[test]
    fn sub_agents_a_hook_names_are_counted_beside_the_agent() {
        use crate::control::SubagentChange::{Ended, Running, Started};
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let mut rx = r.subscribe();
        let state = || r.agent(1).expect("agent").belief.state;
        let before = state();

        // Not an agent yet: a late hook of a process that is gone counts nothing.
        r.note_subagents(1, Started("old".into()));
        assert_eq!(r.background_of(1), (0, 0));
        r.terminals.lock().unwrap().get_mut(&1).unwrap().identity = AgentIdentity::Known {
            program: "claude".into(),
            generation: GenerationId::FIRST,
            agent: true,
        };

        r.note_subagents(1, Started("a".into()));
        rx.try_recv().expect("the index is told");
        r.note_subagents(1, Started("b".into()));
        r.note_subagents(1, Started("b".into()));
        assert_eq!(r.background_of(1).1, 2, "an id counts once");
        r.note_subagents(1, Ended("a".into()));
        r.note_subagents(1, Ended("never-started".into()));
        assert_eq!(r.background_of(1).1, 1);
        while rx.try_recv().is_ok() {}
        r.note_subagents(1, Ended("never-started".into()));
        assert!(rx.try_recv().is_err(), "a repeat is not news");
        assert_eq!(state(), before, "the state is left alone");

        // The screen lists more than the hooks said: the screen is the floor. Background
        // work is the sub-agents and the shells together.
        r.note_background(1, 3, 2);
        assert_eq!(r.background_of(1), (5, 3));
        r.note_background(1, 0, 2);
        assert_eq!(r.background_of(1), (3, 1));

        // The agent's own list at the end of its turn ends what is missing from it, and never
        // brings back one whose end landed first.
        r.note_subagents(1, Started("c".into()));
        r.note_subagents(1, Running(vec!["b".into(), "gone".into()]));
        assert_eq!(r.background_of(1).1, 1);
        r.note_subagents(1, Running(Vec::new()));
        assert_eq!(r.background_of(1).1, 0);

        r.note_subagents(1, Started("e".into()));
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(
            r.background_of(1),
            (0, 0),
            "counted inside a process now gone"
        );
    }

    #[test]
    fn a_claim_is_admitted_only_with_this_terminals_token() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let generation = r.agent(1).expect("agent").capability.generation;

        assert_eq!(
            r.admit(1, &TerminalId::new("1"), Some(&token), generation),
            Ok(())
        );
        assert_eq!(
            r.admit(1, &TerminalId::new("1"), None, generation),
            Err(ClaimRefusal::NoToken)
        );
        assert_eq!(
            r.admit(
                1,
                &TerminalId::new("1"),
                Some(&CapabilityToken::new("forged")),
                generation
            ),
            Err(ClaimRefusal::WrongTerminal)
        );
    }

    #[test]
    fn every_terminal_gets_a_different_token() {
        let a = mint_token(1);
        let b = mint_token(2);
        assert_ne!(
            a, b,
            "a token that is the same for two terminals proves nothing"
        );
    }

    fn screen_verdict(state: AgentRuntimeState, visible_blocker: bool) -> qoop_detect::Verdict {
        qoop_detect::Verdict {
            state,
            rule: "test".into(),
            visible_blocker,
            visible_idle: false,
            visible_working: false,
            setup_gate: false,
        }
    }

    /// B3: a hook beats a screen reading from the same generation.
    #[test]
    fn a_hook_report_overrules_a_screen_reading_from_the_same_generation() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        assert!(r.observe_screen(
            1,
            "test-manifest",
            &screen_verdict(AgentRuntimeState::Working, false),
            NOW
        ));
        let changed = r
            .observe_hook(
                1,
                &TerminalId::new("1"),
                &token,
                AgentRuntimeState::Idle,
                false,
                false,
                NOW,
            )
            .expect("an admitted report applies");
        assert!(changed);
        let after = r.agent(1).expect("agent");
        assert_eq!(after.belief.state, AgentRuntimeState::Idle);
        assert_eq!(after.belief.source, ObservationSource::Hook);
    }

    /// B3 + B1: the wire clock is advisory only. A hook carrying huge nanos gets
    /// a small counter sequence, so a later screen blocker still outranks it
    /// through the visible-blocker override instead of drowning under it.
    #[test]
    fn a_hook_with_a_huge_wire_clock_still_gets_a_small_counter_sequence() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        // The clock is not even a parameter of `observe_hook`: there is no call
        // that could copy it. What matters is the assigned sequence.
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Working,
            false,
            false,
            NOW,
        )
        .expect("applies");
        let held = r.agent(1).expect("agent");
        assert_eq!(held.belief.sequence, 1, "a counter, never a wall clock");
        // A newer screen reading that positively sees a blocker must win, which
        // is exactly what a leaked wall clock would break: no later screen
        // sequence could ever exceed nanos.
        assert!(r.observe_screen(
            1,
            "test-manifest",
            &screen_verdict(AgentRuntimeState::Blocked, true),
            NOW
        ));
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Blocked
        );
    }

    /// B3: a claim sampled under an older generation is refused after the
    /// foreground process moved on.
    #[test]
    fn a_claim_from_before_a_leader_change_is_refused() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let first = r.agent(1).expect("agent").capability.generation;
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(
            r.admit(1, &TerminalId::new("1"), Some(&token), first),
            Err(ClaimRefusal::StaleGeneration)
        );
    }

    /// B3 end to end: a hook event goes through `admit()` into a state change,
    /// and a forged token never becomes an observation.
    #[test]
    fn each_admitted_turn_end_is_a_new_stop_and_work_ends_it() {
        let r = registry();
        let token = mint_token(1);
        let id = TerminalId::new("1");
        r.register(1, id.clone(), token.clone());
        let mut news = r.subscribe();
        let report = |token: &CapabilityToken, state, event, at| {
            r.observe_hook_event(1, &id, token, state, false, false, at, event)
        };
        report(&token, AgentRuntimeState::Working, Some("user"), NOW).expect("admitted");
        assert_eq!(r.stop_of(1), None, "no turn has ended");
        while news.try_recv().is_ok() {}
        // The turn's end: the row that shows the rest is announced with its stop.
        report(&token, AgentRuntimeState::Idle, Some("done"), NOW + 5).expect("admitted");
        assert_eq!(news.try_recv(), Ok(1));
        assert_eq!(r.stop_of(1).as_deref(), Some("1750000005.1"));
        // Another turn end with no state change and no work between: a new stop, announced.
        report(&token, AgentRuntimeState::Idle, Some("done"), NOW + 5).expect("admitted");
        assert_eq!(news.try_recv(), Ok(1), "a changed stop is news");
        assert_eq!(r.stop_of(1).as_deref(), Some("1750000005.2"));
        // Work ends it; the count goes on, so a stop in the same second is not an old one.
        report(&token, AgentRuntimeState::Working, Some("tool"), NOW + 5).expect("admitted");
        assert_eq!(r.stop_of(1), None);
        report(&token, AgentRuntimeState::Idle, Some("done"), NOW + 5).expect("admitted");
        assert_eq!(r.stop_of(1).as_deref(), Some("1750000005.3"));
        // A refused report sets nothing.
        assert!(
            report(
                &CapabilityToken::new("forged"),
                AgentRuntimeState::Idle,
                Some("done"),
                NOW + 9
            )
            .is_err()
        );
        assert_eq!(r.stop_of(1).as_deref(), Some("1750000005.3"));
        // A report without an event leaves the stop as it is.
        report(&token, AgentRuntimeState::Idle, None, NOW + 9).expect("admitted");
        assert_eq!(r.stop_of(1).as_deref(), Some("1750000005.3"));
        // A new process in the terminal has no stop until its own first turn end.
        r.terminals.lock().unwrap().get_mut(&1).unwrap().advance();
        assert_eq!(r.stop_of(1), None, "a new generation has no old stop");
    }

    #[test]
    fn a_hook_event_drives_a_state_change_through_admit_and_a_forgery_does_not() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let generation = r.agent(1).expect("agent").capability.generation;
        r.admit(1, &TerminalId::new("1"), Some(&token), generation)
            .expect("the real token is admitted");
        let changed = r
            .observe_hook(
                1,
                &TerminalId::new("1"),
                &token,
                AgentRuntimeState::Blocked,
                true,
                false,
                NOW,
            )
            .expect("applies");
        assert!(changed);
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Blocked
        );

        let forged = CapabilityToken::new("forged");
        assert_eq!(
            r.observe_hook(
                1,
                &TerminalId::new("1"),
                &forged,
                AgentRuntimeState::Idle,
                false,
                false,
                NOW
            ),
            Err(ClaimRefusal::WrongTerminal)
        );
        assert_eq!(
            r.observe_hook(
                9,
                &TerminalId::new("9"),
                &token,
                AgentRuntimeState::Idle,
                false,
                false,
                NOW
            ),
            Err(ClaimRefusal::WrongTerminal)
        );
        // The forgery changed nothing.
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Blocked
        );
    }

    /// D1: a held blocker survives re-announces unchanged, and each tick emits
    /// so a panel that missed one refresh still converges.
    #[test]
    fn a_blocker_held_across_reannounce_ticks_stays_blocked() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Blocked,
            true,
            false,
            NOW,
        )
        .expect("applies");
        let mut rx = r.subscribe();
        // The observe above announced; drain it so the count below is ticks only.
        let _ = rx.try_recv();

        let before = r.agent(1).expect("agent").belief.clone();
        assert!(before.visible_blocker);
        for _ in 0..3 {
            r.reannounce(1);
            assert_eq!(
                r.agent(1).expect("agent").belief,
                before,
                "a repeat is not news"
            );
            rx.try_recv().expect("each tick re-announces");
        }
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Blocked
        );
    }

    /// A park ends only on an answer: a person sent a line to the agent after the park and
    /// it works again. Working without a line sent (a resume, a background wake) keeps it,
    /// keys that send nothing keep it, and a working agent cannot be parked at all.
    /// What says a person answered in a tab: a key or a line from them, the latest; a kitty
    /// Enter sends a line as a return does, and a focus report is no input.
    #[test]
    fn input_is_a_persons_key_or_line_and_a_kitty_enter_sends() {
        let r = registry();
        assert_eq!(r.input_at(1), None, "nothing typed: a scheduled wake");
        r.note_person(1, b"\x1b[I", false, "computer", 5);
        r.note_person(1, b"\x1b[<35;10;5M", false, "computer", 5);
        assert_eq!(r.input_at(1), None, "a focus or mouse report");
        r.note_person(1, b"2", false, "computer", 7);
        assert_eq!(r.input_at(1), Some(7), "one key picks a dialog option");
        r.note_sent(1, b"\x1b[13u", 9);
        assert_eq!(r.input_at(1), Some(9), "a kitty Enter sends the line");
        r.note_sent(1, b"\x1b[13;1u", 10);
        assert_eq!(r.input_at(1), Some(10));
        r.note_sent(1, b"y", 12);
        assert_eq!(r.input_at(1), Some(10), "a key with no Enter sends nothing");
        assert_eq!(r.input_at(2), None, "another tab");
    }

    #[test]
    fn a_park_ends_when_an_answer_makes_the_agent_work_and_not_otherwise() {
        use AgentRuntimeState::{Idle, Working};
        // The answer window is read off the real clock.
        let now = now_unix();
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let report = |state, now| {
            r.observe_hook(1, &TerminalId::new("1"), &token, state, false, false, now)
                .expect("an admitted report applies")
        };
        report(Working, now);
        assert!(!r.set_parked(1, true, now), "a working agent is not parked");
        assert!(!r.is_parked(1));
        report(Idle, now);
        // Sent before the park: not an answer to it.
        r.note_sent(1, b"x\r", now - 5);
        assert!(r.set_parked(1, true, now));
        report(Working, now + 1);
        assert!(
            r.is_parked(1),
            "working with nobody typing since keeps the park"
        );
        report(Idle, now + 1);
        r.note_sent(1, b"\x1b[I", now + 2);
        r.note_sent(1, b"y", now + 2);
        report(Working, now + 2);
        assert!(r.is_parked(1), "keys that send no line keep the park");
        report(Idle, now + 2);
        assert!(r.take_unparked().is_empty());
        r.note_sent(1, b"\r", now + 3);
        report(Working, now + 3);
        assert!(!r.is_parked(1), "an answer ended the park");
        assert_eq!(r.take_unparked(), [1], "and the store is told");
        assert!(r.take_unparked().is_empty());
        // A line sent long before the agent works again is not the answer that ends a park.
        report(Idle, now + 4);
        assert!(r.set_parked(1, true, now - 100));
        r.note_sent(1, b"\r", now - 60);
        report(Working, now + 4);
        assert!(r.is_parked(1), "a stale line keeps the park");
        report(Idle, now + 4);
        assert!(r.set_parked(1, false, now + 4));
        // Taken back by hand, and gone with a terminal closed for good.
        report(Idle, now + 4);
        assert!(r.set_parked(1, true, now + 4));
        assert!(r.set_parked(1, false, now + 4));
        assert!(!r.is_parked(1));
        assert!(r.set_parked(1, true, now + 4));
        r.drop_parked(1);
        assert!(!r.is_parked(1));
        assert!(
            r.take_unparked().is_empty(),
            "a hand or a close tells the store itself"
        );
    }

    /// A board draws what the agent of an open task is doing, so it reads again when that
    /// changes: once for a run of changes (the trailing edge of a one-second window), not for
    /// a repeat, and never for a terminal that holds no task.
    #[tokio::test(start_paused = true)]
    async fn a_task_terminal_changing_state_makes_a_board_read_once_per_window() {
        use AgentRuntimeState::{Blocked, Idle, Working};
        let r = registry();
        let (one, two) = (mint_token(1), mint_token(2));
        r.register(1, TerminalId::new("1"), one.clone());
        r.register(2, TerminalId::new("2"), two.clone());
        r.set_task_terminals([1].into());
        let mut rx = r.subscribe_tasks();
        let report = |stream: u32, token: &CapabilityToken, state| {
            r.observe_hook(
                stream,
                &TerminalId::new(stream.to_string()),
                token,
                state,
                false,
                false,
                now_unix(),
            )
            .expect("an admitted report applies")
        };
        // Paused time: a sleep here moves the clock the window waits on, and nothing else.
        let window = TASKS_WINDOW + std::time::Duration::from_millis(1);
        report(1, &one, Working);
        report(1, &one, Idle);
        report(1, &one, Blocked);
        assert!(rx.try_recv().is_err(), "it goes out at the window's end");
        tokio::time::sleep(window).await;
        rx.try_recv().expect("one read for the run of changes");
        assert!(rx.try_recv().is_err(), "only one");
        // The same token again, and a terminal with no task: nothing to read again.
        report(1, &one, Blocked);
        report(2, &two, Working);
        report(2, &two, Idle);
        tokio::time::sleep(window).await;
        assert!(rx.try_recv().is_err(), "no change a board shows");
        // A closed terminal's agent is gone, which a board shows, and its token goes with it:
        // a number handed out again starts clean.
        r.forget(1);
        tokio::time::sleep(window).await;
        rx.try_recv().expect("a closed agent is a change");
        r.register(1, TerminalId::new("1"), one.clone());
        tokio::time::sleep(window).await;
        assert!(rx.try_recv().is_err(), "a first sighting is no change");
        report(1, &one, Idle);
        tokio::time::sleep(window).await;
        rx.try_recv().expect("and its next change is");
    }

    /// D2: a soak of identical evaluations publishes nothing, while a newly
    /// appearing blocker publishes once.
    #[test]
    fn a_no_change_soak_publishes_nothing_and_a_new_blocker_publishes_once() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let mut rx = r.subscribe();
        let _ = rx.try_recv(); // the registration announcement

        let working = screen_verdict(AgentRuntimeState::Working, false);
        assert!(r.observe_screen(1, "test-manifest", &working, NOW));
        rx.try_recv().expect("the change announces");
        for _ in 0..10 {
            assert!(
                !r.observe_screen(1, "test-manifest", &working, NOW),
                "no change, no publish"
            );
        }
        assert!(rx.try_recv().is_err(), "the soak must not publish anything");

        // Same state, newly visible blocker: observable, so published once.
        assert!(r.observe_screen(
            1,
            "test-manifest",
            &screen_verdict(AgentRuntimeState::Working, true),
            NOW
        ));
        rx.try_recv().expect("the new blocker announces");
        assert!(!r.observe_screen(
            1,
            "test-manifest",
            &screen_verdict(AgentRuntimeState::Working, true),
            NOW
        ));
        assert!(rx.try_recv().is_err());
    }

    /// D3: a Hook-held blocker plus exit settles to Idle through the
    /// acceptance rule. The regression this guards: a bare screen Idle offered
    /// to the Hook belief would be `Outranked` and the dead terminal would
    /// stick on `blocked`.
    #[test]
    fn a_hook_held_blocker_plus_exit_settles_to_idle() {
        use qoop_domain::{ObservationSource, ObservationVerdict};
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Blocked,
            true,
            false,
            NOW,
        )
        .expect("applies");
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Blocked
        );

        // A bare screen Idle against that belief is outranked: the composition
        // below must not be built that way.
        let stale_screen = qoop_domain::AgentObservation {
            agent_id: qoop_domain::AgentId::new("1"),
            terminal_id: TerminalId::new("1"),
            sampled_generation: r.agent(1).expect("agent").belief.generation,
            state: AgentRuntimeState::Idle,
            source: ObservationSource::ScreenHeuristic,
            confidence: qoop_domain::Confidence::Medium,
            sequence: 99,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
        };
        assert_eq!(
            r.agent(1).expect("agent").belief.evaluate(&stale_screen),
            ObservationVerdict::Outranked
        );

        {
            let mut terminals = r.terminals.lock().unwrap();
            let agent = terminals.get_mut(&1).expect("agent");
            settle_exit(agent, 1, NOW);
        }
        let after = r.agent(1).expect("agent");
        assert_eq!(after.belief.state, AgentRuntimeState::Idle);
        assert!(!after.belief.visible_blocker);
    }

    /// A hook that missed the end of a turn holds `working` while the live
    /// composer waits on screen. Positive idle evidence clears it; a weak
    /// idle with no composer behind it must not.
    #[test]
    fn a_visible_idle_composer_clears_a_hook_held_working_state() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Working,
            false,
            false,
            NOW,
        )
        .expect("applies");
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Working
        );

        let composer = qoop_detect::Verdict {
            state: AgentRuntimeState::Idle,
            rule: "composer_idle".into(),
            visible_blocker: false,
            visible_idle: true,
            visible_working: false,
            setup_gate: false,
        };
        assert!(r.observe_screen(1, "test-manifest", &composer, NOW));
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Idle
        );
    }

    /// A draft typed into the composer is weak working evidence. It must not start a turn
    /// after the screen has taken the belief over from a hook (a fresh session, or one that
    /// finished a turn), and must keep one that is running.
    #[test]
    fn a_draft_does_not_start_a_turn_but_keeps_one_running() {
        let hook = |r: &AgentRegistry, token: &CapabilityToken, state| {
            r.observe_hook(1, &TerminalId::new("1"), token, state, false, false, NOW)
                .expect("applies");
        };
        let composer = qoop_detect::Verdict {
            visible_idle: true,
            ..screen_verdict(AgentRuntimeState::Idle, false)
        };
        let draft = screen_verdict(AgentRuntimeState::Working, false);

        // Cases 1 and 2: a session that was never prompted, and one whose turn ended.
        for before_draft in [AgentRuntimeState::Starting, AgentRuntimeState::Idle] {
            let r = registry();
            let token = mint_token(1);
            r.register(1, TerminalId::new("1"), token.clone());
            hook(&r, &token, before_draft);
            r.observe_screen(1, "test-manifest", &composer, NOW);
            r.observe_screen(1, "test-manifest", &draft, NOW);
            assert_eq!(
                r.agent(1).expect("agent").belief.state,
                AgentRuntimeState::Idle,
                "{before_draft:?}"
            );
        }

        // Case 3: the prompt was sent, so the hook says working, and the draft under the
        // streaming answer keeps it so.
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        hook(&r, &token, AgentRuntimeState::Working);
        r.observe_screen(1, "test-manifest", &draft, NOW);
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Working
        );
    }

    /// A permission prompt answered while a long tool runs: the hook said `blocked`
    /// and has nothing more to say until the tool ends, so only the screen sees the
    /// agent work again. A screen that positively shows work clears the hook's
    /// blocker; a weak working reading with no rule behind it must not.
    #[test]
    fn a_visible_working_screen_clears_a_hook_held_blocker() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Blocked,
            false,
            false,
            NOW,
        )
        .expect("applies");
        let state = || r.agent(1).expect("agent").belief.state;
        assert_eq!(state(), AgentRuntimeState::Blocked);

        let weak = screen_verdict(AgentRuntimeState::Working, false);
        assert!(!r.observe_screen(1, "test-manifest", &weak, NOW));
        assert_eq!(state(), AgentRuntimeState::Blocked, "absence of evidence");

        let spinner = qoop_detect::Verdict {
            visible_working: true,
            ..weak
        };
        let mut rx = r.subscribe();
        assert!(r.observe_screen(1, "test-manifest", &spinner, NOW));
        assert_eq!(state(), AgentRuntimeState::Working);
        rx.try_recv()
            .expect("the index is told the agent works again");
    }

    /// A trust dialog on screen raises the setup gate, and the reading after
    /// it clears the gate: the dialog is gone when the screen no longer
    /// shows it, so holding the flag would refuse prompts over a live
    /// session.
    #[test]
    fn a_trust_dialog_raises_the_setup_gate_until_the_screen_moves_on() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let trust = qoop_detect::Verdict {
            state: AgentRuntimeState::Blocked,
            rule: "trust_directory".into(),
            visible_blocker: true,
            visible_idle: false,
            visible_working: false,
            setup_gate: true,
        };
        assert!(r.observe_screen(1, "test-manifest", &trust, NOW));
        assert!(r.agent(1).expect("agent").setup_gate);
        assert!(r.observe_screen(
            1,
            "test-manifest",
            &screen_verdict(AgentRuntimeState::Working, false),
            NOW
        ));
        assert!(!r.agent(1).expect("agent").setup_gate);
    }

    /// A hook report comes from a live session, so accepting one clears a
    /// gate the screen set: without this a session whose screen readings are
    /// refused after the hook is accepted would read `trust_required` forever.
    #[test]
    fn an_accepted_hook_clears_a_screen_raised_setup_gate() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        let trust = qoop_detect::Verdict {
            state: AgentRuntimeState::Blocked,
            rule: "trust_directory".into(),
            visible_blocker: true,
            visible_idle: false,
            visible_working: false,
            setup_gate: true,
        };
        assert!(r.observe_screen(1, "test-manifest", &trust, NOW));
        assert!(r.agent(1).expect("agent").setup_gate);
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Working,
            false,
            false,
            NOW,
        )
        .expect("a hook from the live session applies");
        assert!(!r.agent(1).expect("agent").setup_gate);
    }

    /// Focus: the per-terminal log caps at MAX_FOCUS_EVENTS, oldest first.
    #[test]
    fn a_focus_log_caps_at_fifty_and_reads_oldest_first() {
        let r = registry();
        for n in 0..(crate::control::MAX_FOCUS_EVENTS + 5) {
            r.push_focus_event(
                1,
                crate::control::FocusEvent {
                    kind: "tool".into(),
                    label: format!("Read {n}.rs"),
                    at_unix: n as i64,
                },
            );
        }
        let events = r.focus_events(1);
        assert_eq!(events.len(), crate::control::MAX_FOCUS_EVENTS);
        assert_eq!(events.first().expect("a head").label, "Read 5.rs");
        assert_eq!(
            events.last().expect("a tail").label,
            format!("Read {}.rs", crate::control::MAX_FOCUS_EVENTS + 4)
        );
    }

    /// Activity: the newest non-empty tool or blocked label, `thinking` after a prompt,
    /// nothing once the turn is done, and never the prompt's own text.
    #[test]
    fn activity_is_the_newest_label_and_never_the_prompt() {
        let r = registry();
        let push = |kind: &str, label: &str| {
            r.push_focus_event(
                1,
                crate::control::FocusEvent {
                    kind: kind.into(),
                    label: label.into(),
                    at_unix: 0,
                },
            )
        };
        assert_eq!(r.activity(1), None, "nothing said yet");
        push("user", "delete the prod database please");
        assert_eq!(r.activity(1).as_deref(), Some("thinking"));
        push("tool", "Edit src/a.rs");
        assert_eq!(r.activity(1).as_deref(), Some("Edit src/a.rs"));
        push("blocked", "Bash cargo publish");
        // Claude follows a labelled permission request with a label-less notification.
        push("blocked", "");
        assert_eq!(r.activity(1).as_deref(), Some("Bash cargo publish"));
        // A question dialog's type is the daemon's mark, never the sidebar's word.
        push("blocked", "elicitation_dialog");
        assert_eq!(r.activity(1).as_deref(), Some("Bash cargo publish"));
        push("done", "");
        assert_eq!(r.activity(1), None, "a finished turn has no activity");
        assert_eq!(r.activity(2), None, "another terminal has its own log");
    }

    /// Focus: forgetting a terminal drops its log with it.
    #[test]
    fn forgetting_a_terminal_drops_its_focus_log() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token);
        r.push_focus_event(
            1,
            crate::control::FocusEvent {
                kind: "user".into(),
                label: "hello".into(),
                at_unix: 1,
            },
        );
        assert_eq!(r.focus_events(1).len(), 1);
        r.forget(1);
        assert!(r.focus_events(1).is_empty());
    }

    /// G8: the activity stamp moves on accepted change only. Registration
    /// starts unstamped (restore walks the same path, so a restarted fleet
    /// paints nothing as fresh); an accepted hook stamps; a repeat that
    /// changes nothing and a refused screen reading leave the stamp alone;
    /// a new foreground generation stamps even before anything is known.
    #[test]
    fn the_activity_stamp_moves_on_accepted_change_only() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        assert_eq!(
            r.agent(1).expect("agent").last_active_at_unix,
            None,
            "registration is not activity"
        );
        // Accepted hook: stamps.
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Working,
            false,
            false,
            NOW,
        )
        .expect("applies");
        assert_eq!(r.agent(1).expect("agent").last_active_at_unix, Some(NOW));
        // Same report again: admitted but unchanged, so the stamp stays.
        assert_eq!(
            r.observe_hook(
                1,
                &TerminalId::new("1"),
                &token,
                AgentRuntimeState::Working,
                false,
                false,
                LATER,
            ),
            Ok(false),
            "a repeat changes nothing"
        );
        assert_eq!(
            r.agent(1).expect("agent").last_active_at_unix,
            Some(NOW),
            "a repeat is not activity"
        );
        // A screen reading that changes nothing, refused or outranked or
        // identical, leaves the stamp alone.
        assert!(!r.observe_screen(
            1,
            "claude",
            &screen_verdict(AgentRuntimeState::Idle, false),
            LATER,
        ));
        assert_eq!(r.agent(1).expect("agent").last_active_at_unix, Some(NOW));
        // An accepted screen reading stamps: fresh belief, first screen.
        let r2 = registry();
        r2.register(2, TerminalId::new("2"), mint_token(2));
        assert!(r2.observe_screen(
            2,
            "claude",
            &screen_verdict(AgentRuntimeState::Working, false),
            LATER,
        ));
        assert_eq!(
            r2.agent(2).expect("agent").last_active_at_unix,
            Some(LATER),
            "an accepted screen reading is activity"
        );
        // The same screen again: accepted but unchanged, stamp stays.
        assert!(!r2.observe_screen(
            2,
            "claude",
            &screen_verdict(AgentRuntimeState::Working, false),
            LATER + 1,
        ));
        assert_eq!(
            r2.agent(2).expect("agent").last_active_at_unix,
            Some(LATER),
            "an identical screen is not activity"
        );
        // Same leader again: no generation, no stamp.
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), LATER).expect("a change");
        assert_eq!(
            r.agent(1).expect("agent").last_active_at_unix,
            Some(LATER),
            "a new generation is activity"
        );
        assert_eq!(r.note_foreground(1, Some(me), LATER + 1), None);
        assert_eq!(
            r.agent(1).expect("agent").last_active_at_unix,
            Some(LATER),
            "the same leader again is not activity"
        );
    }

    /// G8: settling an exit stamps the idle it synthesizes, through the
    /// acceptance rule like every other stamp.
    #[test]
    fn settling_an_exit_stamps_the_synthesized_idle() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.observe_hook(
            1,
            &TerminalId::new("1"),
            &token,
            AgentRuntimeState::Blocked,
            true,
            false,
            NOW,
        )
        .expect("applies");
        {
            // Called directly because process events are not injectable in a
            // unit test; the composition stamps the same value twice, so
            // there is no observable difference to pin through `apply_events`.
            let mut terminals = r.terminals.lock().unwrap();
            let agent = terminals.get_mut(&1).expect("agent");
            settle_exit(agent, 1, LATER);
        }
        let after = r.agent(1).expect("agent");
        assert_eq!(after.belief.state, AgentRuntimeState::Idle);
        assert_eq!(after.last_active_at_unix, Some(LATER));
    }

    /// Spawn seeding: creation counts as activity on both clocks, so a
    /// terminal that never produces an accepted observation still ages from
    /// a known point. Seeding never moves a live stamp.
    #[test]
    fn spawn_seeds_both_clocks_and_never_moves_a_live_stamp() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        assert_eq!(r.idle_since(1), None, "register alone stamps nothing");
        r.note_spawn(1, NOW);
        assert_eq!(r.idle_since(1), Some(NOW));
        // Live traffic moves the IO clock; the idle point follows the later.
        r.note_io(1, LATER);
        assert_eq!(r.idle_since(1), Some(LATER));
        // A second seed (e.g. a repeated call) never moves anything.
        r.note_spawn(1, LATER + 1000);
        assert_eq!(r.agent(1).expect("agent").last_active_at_unix, Some(NOW));
        assert_eq!(r.agent(1).expect("agent").last_io_unix, Some(LATER));
        assert_eq!(r.idle_since(1), Some(LATER));
        // Unknown terminal: silent no-op, never a panic.
        r.note_spawn(9, NOW);
        r.note_io(9, NOW);
        assert_eq!(r.idle_since(9), None);
    }

    /// The reaper snapshot reads state, program, boundary, gate and clocks
    /// together: a boundary identity reads as no program but still vetoes.
    #[test]
    fn the_reap_snapshot_carries_boundary_gate_and_clocks_together() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        r.note_spawn(1, NOW);
        let snap = r.reap_snapshot(1).expect("snapshot");
        assert_eq!(snap.state, AgentRuntimeState::Unknown);
        assert_eq!(snap.program, None);
        assert!(!snap.boundary);
        assert!(!snap.setup_gate);
        assert_eq!(snap.idle_since, Some(NOW));
        assert!(
            r.reap_snapshot(9).is_none(),
            "gone terminal has no snapshot"
        );
        assert!(r.master_of(1).is_none(), "no master attached in tests");
    }

    /// Forget clears every idle-time trace of the stream: shields, and edges
    /// on either end, and leaves the other terminals' alone.
    #[test]
    fn forget_clears_idle_time_state() {
        let r = registry();
        for stream in [1, 2] {
            r.register(
                stream,
                TerminalId::new(stream.to_string()),
                mint_token(stream),
            );
        }
        r.note_shield(1, NOW);
        r.note_shield(2, NOW);
        r.note_expectation(1, 2, NOW);
        assert!(r.shield_fresh(1, NOW + 10, 1800));
        assert_eq!(r.expectation_of(1), Some((2, NOW)));

        r.forget(1);
        assert!(!r.shield_fresh(1, NOW + 10, 1800));
        assert_eq!(r.expectation_of(1), None);
        // Terminal 2's shield survives: only the gone stream is cleared.
        assert!(r.shield_fresh(2, NOW + 10, 1800));
    }

    /// Edges live a day and not a second longer, swept lazily.
    #[test]
    fn expectations_expire_after_a_day() {
        let r = registry();
        r.note_expectation(1, 2, NOW);
        r.sweep_expectations(NOW + EXPECTATION_TTL_SECS);
        assert_eq!(r.expectation_of(1), Some((2, NOW)), "boundary keeps");
        r.sweep_expectations(NOW + EXPECTATION_TTL_SECS + 1);
        assert_eq!(r.expectation_of(1), None, "a day-old wait is over");
    }

    /// Shields veto while fresher than the timeout, then go quiet.
    #[test]
    fn shields_veto_while_fresh_only() {
        let r = registry();
        r.note_shield(1, NOW);
        assert!(r.shield_fresh(1, NOW + 1799, 1800));
        assert!(!r.shield_fresh(1, NOW + 1800, 1800), "boundary frees");
        assert!(!r.shield_fresh(9, NOW, 1800), "unknown stream unshielded");
    }

    /// An admitted focus event shields its own tab: the event clock is the
    /// shield clock, no extra parameter to get wrong.
    #[test]
    fn an_admitted_focus_event_shields_its_own_tab() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        r.push_focus_event(
            1,
            crate::control::FocusEvent {
                kind: "tool".into(),
                label: "Read foo.rs".into(),
                at_unix: NOW,
            },
        );
        assert!(r.shield_fresh(1, NOW + 10, 1800));
        assert!(!r.shield_fresh(2, NOW + 10, 1800), "never cross-terminal");
    }

    /// A prompt head starting with `<` is a harness message, never a goal: the request
    /// before it stays the one a begin names its task after.
    #[test]
    fn a_harness_message_is_not_kept_as_the_prompt() {
        let r = registry();
        let user = |label: &str, at_unix| crate::control::FocusEvent {
            kind: "user".into(),
            label: label.into(),
            at_unix,
        };
        r.push_focus_event(1, user("make greet shout", NOW));
        r.push_focus_event(1, user("<task-notification> agent finished", NOW + 1));
        r.push_focus_event(1, user("  <command-name>/clear", NOW + 2));
        assert_eq!(r.last_prompt(1), Some(("make greet shout".into(), NOW)));
        r.push_focus_event(
            1,
            user("<pasted_content id=\"3386\"> the owner said", NOW + 3),
        );
        r.push_focus_event(1, user("<agent-message from=\"a97\"> report", NOW + 4));
        assert_eq!(r.last_prompt(1), Some(("make greet shout".into(), NOW)));
        r.push_focus_event(2, user("<system-reminder>", NOW));
        assert_eq!(r.last_prompt(2), None);
        r.push_focus_event(3, user("<Header> overlaps the nav", NOW));
        assert_eq!(
            r.last_prompt(3),
            Some(("<Header> overlaps the nav".into(), NOW)),
            "a request that only starts with < is the person's"
        );
    }

    /// An explicit close is marked once and taken once: a repeat close does not
    /// own the mark, and the teardown's take leaves nothing behind.
    #[test]
    fn an_explicit_close_is_marked_once_and_taken_once() {
        let r = registry();
        assert!(r.mark_closed(5), "first close sets the mark");
        assert!(!r.mark_closed(5), "a repeat close finds it set");
        assert!(r.is_closed(5));
        assert!(r.take_closed(5));
        assert!(!r.take_closed(5), "taken once");
        assert!(!r.is_closed(5));
    }

    /// A command's end is kept only after its start, and a terminal printing marks in a loop
    /// costs its watchers one announcement for the window, not one per mark.
    #[tokio::test]
    async fn shell_marks_keep_the_last_command_and_announce_once_a_window() {
        use qoop_terminal::ShellMark::{Finished, Started};
        let r = registry();
        let mut changes = r.subscribe();
        r.note_shell(3, Finished(Some(1)), 100);
        assert_eq!(r.command(3), None, "an end with no start ends nothing");
        for _ in 0..500 {
            r.note_shell(3, Started, 100);
            r.note_shell(3, Finished(Some(2)), 101);
        }
        let last = r.command(3).expect("a command ended");
        assert_eq!(
            (last.exit, last.ended_at, last.agent),
            (Some(2), 101, false)
        );
        tokio::time::sleep(TASKS_WINDOW + std::time::Duration::from_millis(200)).await;
        assert_eq!(changes.try_recv().ok(), Some(3));
        assert!(
            changes.try_recv().is_err(),
            "one announcement for the flood"
        );
        r.forget(3);
        assert_eq!(r.command(3), None);
    }

    /// The same kind typing on is news once per window, a new kind at once.
    #[tokio::test]
    async fn typing_is_news_once_a_window_and_on_a_change_of_kind() {
        let r = registry();
        for at in 0..4 {
            r.note_person(2, b"a", false, "phone", 1_000 + at);
        }
        let typed = r.typed_by(2).expect("typed");
        assert_eq!((typed.kind.as_str(), typed.at), ("phone", 1_003));
        assert_eq!(
            r.typed_by.lock().unwrap()[&2].2,
            1_000,
            "told once in the window"
        );
        r.note_person(2, b"a", false, "phone", 1_000 + TYPING_NEWS_SECS);
        assert_eq!(r.typed_by.lock().unwrap()[&2].2, 1_000 + TYPING_NEWS_SECS);
        r.note_person(2, b"a", false, "computer", 1_006);
        assert_eq!(
            r.typed_by.lock().unwrap()[&2].2,
            1_006,
            "a new kind is news"
        );
        r.note_person(2, b"[I", false, "phone", 1_007);
        assert_eq!(
            r.typed_by(2).unwrap().kind,
            "computer",
            "a focus report is no key"
        );
    }
}

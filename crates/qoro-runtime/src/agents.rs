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
use std::sync::Mutex;

use qoro_domain::{
    AgentBelief, AgentIdentity, AgentRuntimeState, Boundary, CapabilityToken, ClaimRefusal,
    GenerationId, ObservationSource, TerminalCapability, TerminalId,
};
use qoro_pty::procwatch::{ProcEvent, ProcWatch};
use qoro_pty::teardown::Pid;

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
    /// The rule the screen last matched in this generation, if any.
    ///
    /// What `qoro agents explain` reports. Recorded on every evaluation,
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
    /// Which terminal a watched pid belongs to.
    by_pid: Mutex<HashMap<i32, u32>>,
    terminals: Mutex<BTreeMap<u32, TerminalAgent>>,
    /// Focus events per terminal, oldest first, capped at
    /// [`crate::control::MAX_FOCUS_EVENTS`].
    ///
    /// Display only: hook reports admitted through the token check land
    /// here as well as in the belief. Never journalled, never pushed,
    /// never observed. A terminal that is gone takes its log with it.
    focus: Mutex<BTreeMap<u32, VecDeque<crate::control::FocusEvent>>>,
    /// Each terminal's master, so the foreground group can be re-read without asking the
    /// terminal's own task, which may be parked waiting for output that is not coming.
    masters: Mutex<HashMap<u32, std::os::fd::RawFd>>,
    /// Each terminal's leader pid and its terminal, keyed like the process table, so a
    /// sleep can walk what the leader started.
    leaders: Mutex<HashMap<u32, (i32, u64)>>,
    /// Streams an explicit `CloseTerminal` asked to close and that have not
    /// torn down yet. Their rows close even if their space is going to sleep,
    /// and idle sleep leaves them alone. Entries die with the teardown they
    /// mark.
    closed: Mutex<BTreeSet<u32>>,
    /// Who waits on whom: waiter stream → (target stream, edge since, unix
    /// seconds). Set when a verified `qoro send/prompt` arrives from one
    /// terminal to another. The waiter is vetoed while its target is busy;
    /// both idle together sleep together. `since` only ages the edge out.
    expectations: Mutex<BTreeMap<u32, (u32, i64)>>,
    /// Recent proof of life beyond the two activity clocks: admitted hook
    /// focus events. Vetoes while
    /// fresher than the idle timeout. Bounded by live-terminal count,
    /// cleared on forget.
    shield_at_unix: Mutex<BTreeMap<u32, i64>>,
}

impl AgentRegistry {
    /// Watch for terminals whose agent has moved.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<u32> {
        self.changes.subscribe()
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
    pub fn announce(&self, stream: u32) {
        let _ = self.changes.send(stream);
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
        let before = agent.background_agents + agent.background_tasks;
        agent.background_agents = agents;
        agent.background_tasks = tasks;
        drop(terminals);
        if before != agents + tasks {
            self.announce(stream);
        }
    }

    /// What the screen last showed running in the background, all kinds together.
    pub fn background_of(&self, stream: u32) -> usize {
        self.terminals
            .lock()
            .unwrap()
            .get(&stream)
            .map_or(0, |a| a.background_agents + a.background_tasks)
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
    agent.screen_manifest = None;
    let idle = qoro_domain::AgentObservation {
        agent_id: qoro_domain::AgentId::new(stream.to_string()),
        terminal_id: agent.capability.terminal.clone(),
        sampled_generation: agent.belief.generation,
        state: AgentRuntimeState::Idle,
        source: ObservationSource::ProcessIdentity,
        confidence: qoro_domain::Confidence::High,
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
            by_pid: Mutex::new(HashMap::new()),
            terminals: Mutex::new(BTreeMap::new()),
            focus: Mutex::new(BTreeMap::new()),
            masters: Mutex::new(HashMap::new()),
            leaders: Mutex::new(HashMap::new()),
            closed: Mutex::new(BTreeSet::new()),
            expectations: Mutex::new(BTreeMap::new()),
            shield_at_unix: Mutex::new(BTreeMap::new()),
            changes: tokio::sync::broadcast::channel(64).0,
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
            qoro_pty::sys::foreground_leader(master),
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
                screen_rule: None,
                screen_manifest: None,
                leader: None,
                relooked: None,
                relook_owed: false,
                born: None,
            },
        );
        self.announce_new(stream);
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
                    agent: qoro_domain::program::identify(name, &[]).agent,
                },
                None => AgentIdentity::None,
            }
        };
    }

    pub fn forget(&self, stream: u32) {
        self.remove_live(stream);
        self.closed.lock().unwrap().remove(&stream);
        self.shield_at_unix.lock().unwrap().remove(&stream);
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
        self.masters.lock().unwrap().remove(&stream);
        self.focus.lock().unwrap().remove(&stream);
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
            agent.last_active_at_unix = Some(now_unix);
            agent.last_io_unix = Some(now_unix);
            // The gate belonged to the old generation's screen: a new process
            // means the old dialog reading is void, not held. The old rule
            // reading is void with it.
            agent.setup_gate = false;
            agent.screen_rule = None;
            agent.background_agents = 0;
            agent.background_tasks = 0;
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
        verdict: &qoro_detect::Verdict,
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
        let observation = qoro_domain::AgentObservation {
            agent_id: qoro_domain::AgentId::new(stream.to_string()),
            terminal_id: agent.capability.terminal.clone(),
            sampled_generation: generation,
            state: verdict.state,
            source: qoro_domain::ObservationSource::ScreenHeuristic,
            confidence: qoro_domain::Confidence::Medium,
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
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return Err(ClaimRefusal::WrongTerminal);
        };
        let generation = agent.belief.generation;
        agent.capability.admit(terminal, Some(token), generation)?;
        let observation = qoro_domain::AgentObservation {
            agent_id: qoro_domain::AgentId::new(stream.to_string()),
            terminal_id: agent.capability.terminal.clone(),
            sampled_generation: generation,
            state,
            source: qoro_domain::ObservationSource::Hook,
            confidence: qoro_domain::Confidence::High,
            sequence: agent.belief.sequence.saturating_add(1),
            visible_blocker,
            visible_idle,
            visible_working: false,
        };
        let before = agent.belief.state;
        let before_blocker = agent.belief.visible_blocker;
        let before_gate = agent.setup_gate;
        let Ok(next) = agent.belief.clone().accept(&observation) else {
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
        if changed {
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
    pub fn push_focus_event(&self, stream: u32, event: crate::control::FocusEvent) {
        self.note_shield(stream, event.at_unix);
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
    /// person typed never leaves the focus view. Empty labels are skipped rather than
    /// taken, because a labelled permission request is followed by a label-less
    /// notification that says the same thing with less.
    pub fn activity(&self, stream: u32) -> Option<String> {
        let focus = self.focus.lock().unwrap();
        for event in focus.get(&stream)?.iter().rev() {
            match event.kind.as_str() {
                "done" => return None,
                "user" => return Some("thinking".to_string()),
                "tool" | "blocked" if !event.label.is_empty() => {
                    return Some(event.label.clone());
                }
                _ => {}
            }
        }
        None
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

fn identity_of(pid: Pid, generation: GenerationId) -> AgentIdentity {
    // The leader of the foreground group first, since it is the process a person started
    // and the answer in every ordinary case.
    let leader = match qoro_pty::sys::program_of(pid) {
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
                // `qoro_domain::program` decides; the argv is only fetched here.
                let argv = qoro_pty::sys::argv_of(pid);
                qoro_domain::program::identify(&path, &argv)
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
        let members = qoro_pty::sys::group_members(qoro_pty::Pgid(pid.0));
        if let Some(found) = members
            .into_iter()
            .filter(|member| member.0 != pid.0)
            .filter_map(|member| {
                let path = qoro_pty::sys::program_of(member)?;
                if boundary_for(&path).is_some() {
                    return None;
                }
                let argv = qoro_pty::sys::argv_of(member);
                let program = qoro_domain::program::identify(&path, &argv);
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
    CapabilityToken::new(format!("qoro-{stream}-{nonce:016x}"))
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
        assert_eq!(r.background_of(1), 3);
        r.note_background(1, 1, 2);
        assert!(rx.try_recv().is_err(), "a repeat is not news");
        assert_eq!(state(), before, "the state is left alone");

        r.note_background(1, 0, 0);
        rx.try_recv().expect("the work ended");
        r.note_background(1, 0, 1);
        let me = Pid(std::process::id() as i32);
        r.note_foreground(1, Some(me), NOW).expect("a change");
        assert_eq!(r.background_of(1), 0, "counted inside a process now gone");
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

    fn screen_verdict(state: AgentRuntimeState, visible_blocker: bool) -> qoro_detect::Verdict {
        qoro_detect::Verdict {
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
        use qoro_domain::{ObservationSource, ObservationVerdict};
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
        let stale_screen = qoro_domain::AgentObservation {
            agent_id: qoro_domain::AgentId::new("1"),
            terminal_id: TerminalId::new("1"),
            sampled_generation: r.agent(1).expect("agent").belief.generation,
            state: AgentRuntimeState::Idle,
            source: ObservationSource::ScreenHeuristic,
            confidence: qoro_domain::Confidence::Medium,
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

        let composer = qoro_detect::Verdict {
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

        let spinner = qoro_detect::Verdict {
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
        let trust = qoro_detect::Verdict {
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
        let trust = qoro_detect::Verdict {
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
}

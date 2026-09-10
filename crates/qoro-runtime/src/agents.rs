//! Which agent is in which terminal, and how a claim about one is admitted.
//!
//! Three things that have to move together, which is why they are one type rather than
//! three: the foreground process, the generation, and the token.
//!
//! A pid is not an identity. A shell that `exec`s an agent CLI keeps its pid, and an agent
//! CLI that `exec`s another one keeps it too, so anything keyed on the pid alone reports
//! the first program forever. The generation is what changes, and everything sampled under
//! an older one is refused (ADR-008, invariant I5).

use std::collections::{BTreeMap, HashMap};
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
    /// The foreground leader this identity was read from.
    pub leader: Option<Pid>,
}

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
    /// Each terminal's master, so the foreground group can be re-read without asking the
    /// terminal's own task, which may be parked waiting for output that is not coming.
    masters: Mutex<HashMap<u32, std::os::fd::RawFd>>,
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
    fn announce(&self, stream: u32) {
        let _ = self.changes.send(stream);
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

impl AgentRegistry {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            watch: ProcWatch::new()?,
            by_pid: Mutex::new(HashMap::new()),
            terminals: Mutex::new(BTreeMap::new()),
            masters: Mutex::new(HashMap::new()),
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

    /// Re-read a terminal's foreground group and apply any change.
    ///
    /// Called after a process event rather than on a timer. A shell forks, then moves the
    /// child into the foreground, and the fork is the only notice we get that the second
    /// step is coming: the child may not have been moved yet when the event arrives, which
    /// is why the caller re-reads over a short settle window rather than once.
    pub fn refresh_foreground(&self, stream: u32) -> Option<GenerationId> {
        let master = self.masters.lock().unwrap().get(&stream).copied()?;
        self.note_foreground(stream, qoro_pty::sys::foreground_leader(master))
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
                leader: None,
            },
        );
    }

    pub fn forget(&self, stream: u32) {
        self.masters.lock().unwrap().remove(&stream);
        if let Some(agent) = self.terminals.lock().unwrap().remove(&stream)
            && let Some(pid) = agent.leader
        {
            self.watch.unwatch(pid);
            self.by_pid.lock().unwrap().remove(&pid.0);
        }
    }

    /// Every terminal being tracked.
    pub fn streams(&self) -> Vec<u32> {
        self.terminals.lock().unwrap().keys().copied().collect()
    }

    pub fn agent(&self, stream: u32) -> Option<TerminalAgent> {
        self.terminals.lock().unwrap().get(&stream).cloned()
    }

    /// The terminal's foreground leader, as observed now.
    ///
    /// A change of leader is a new generation, and so is an `exec` by the same leader.
    /// Both clear the previous evidence: what was believed described a process that is no
    /// longer there, and carrying it forward is how a finished agent stays "working".
    pub fn note_foreground(&self, stream: u32, leader: Option<Pid>) -> Option<GenerationId> {
        let mut terminals = self.terminals.lock().unwrap();
        let agent = terminals.get_mut(&stream)?;
        if agent.leader == leader {
            return None;
        }
        if let Some(old) = agent.leader.take() {
            self.watch.unwatch(old);
            self.by_pid.lock().unwrap().remove(&old.0);
        }
        let generation = agent.capability.advance();
        agent.belief = fresh_belief(generation);
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
        // Dropped before announcing: a subscriber that reacts by asking what is there would
        // otherwise be asking while this lock is still held.
        drop(terminals);
        self.announce(stream);
        Some(generation)
    }

    /// Apply whatever the platform has reported. Returns the terminals whose generation
    /// changed, so a caller can tell clients.
    pub fn apply_events(&self) -> Vec<u32> {
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
            let generation = agent.capability.advance();
            agent.belief = fresh_belief(generation);
            if exited {
                agent.identity = AgentIdentity::None;
                agent.leader = None;
                self.by_pid.lock().unwrap().remove(&pid.0);
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
}

fn identity_of(pid: Pid, generation: GenerationId) -> AgentIdentity {
    match qoro_pty::sys::program_of(pid) {
        None => AgentIdentity::None,
        Some(program) => match boundary_for(&program) {
            Some(boundary) => AgentIdentity::Unavailable {
                boundary,
                generation,
            },
            None => AgentIdentity::Known {
                program,
                generation,
            },
        },
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

    fn registry() -> AgentRegistry {
        AgentRegistry::new().expect("registry")
    }

    #[test]
    fn a_wrapper_is_reported_as_unavailable_and_not_by_name() {
        assert!(boundary_for("/usr/local/bin/docker").is_some());
        assert!(boundary_for("/opt/homebrew/bin/nsenter").is_some());
        assert!(boundary_for("/usr/bin/claude").is_none());
    }

    #[test]
    fn a_new_foreground_process_mints_a_new_generation_and_clears_the_belief() {
        let r = registry();
        r.register(1, TerminalId::new("1"), mint_token(1));
        let before = r.agent(1).expect("agent");
        assert_eq!(before.capability.generation, GenerationId::FIRST);

        let me = Pid(std::process::id() as i32);
        let generation = r.note_foreground(1, Some(me)).expect("a change");
        assert!(generation > before.capability.generation);
        let after = r.agent(1).expect("agent");
        assert_eq!(after.belief.state, AgentRuntimeState::Unknown);
        assert_eq!(after.belief.generation, generation);
        assert!(matches!(after.identity, AgentIdentity::Known { .. }));

        // The same leader again is not a change, and must not mint a generation: a
        // generation per read would reject every observation ever sampled.
        assert_eq!(r.note_foreground(1, Some(me)), None);
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
}

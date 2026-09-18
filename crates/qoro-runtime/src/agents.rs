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

/// Settle a terminal whose foreground process just exited.
///
/// The identity is gone with the process. The belief is reset and becomes Idle
/// through the acceptance rule rather than around it: a bare screen Idle
/// offered to the previous Hook-held blocker would be `Outranked` and the dead
/// terminal would stick on `blocked` forever. Reset first, then synthesize, is
/// the composition the test below pins.
fn settle_exit(agent: &mut TerminalAgent, stream: u32) {
    agent.identity = AgentIdentity::None;
    agent.leader = None;
    agent.belief = fresh_belief(agent.capability.generation);
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
    };
    if let Ok(next) = agent.belief.clone().accept(&idle) {
        agent.belief = next;
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
        self.announce_new(stream);
    }

    pub fn forget(&self, stream: u32) {
        self.masters.lock().unwrap().remove(&stream);
        if let Some(agent) = self.terminals.lock().unwrap().remove(&stream)
            && let Some(pid) = agent.leader
        {
            self.watch.unwatch(pid);
            self.by_pid.lock().unwrap().remove(&pid.0);
        }
        // A terminal that is gone is a change like any other. Closing one answers as soon
        // as the request is accepted, but the terminal itself lives until its teardown
        // finishes, which takes as long as the escalation in ADR-014 allows: measured, an
        // index still listed a closed terminal fifteen seconds later, because the client
        // had asked once, at the moment of the answer, and nothing told it to ask again.
        self.announce(stream);
    }

    /// A terminal that now exists is a change too, for the same reason.
    fn announce_new(&self, stream: u32) {
        self.announce(stream);
    }

    /// Every terminal being tracked.
    pub fn streams(&self) -> Vec<u32> {
        self.terminals.lock().unwrap().keys().copied().collect()
    }

    /// The program in a terminal, when it can be read.
    ///
    /// Its own accessor rather than a field read through `agent()`, because the read loop
    /// asks on every screen evaluation and cloning the whole record for a name is the kind
    /// of cost that only shows up at a hundred terminals.
    pub fn program_of(&self, stream: u32) -> Option<String> {
        match &self.terminals.lock().unwrap().get(&stream)?.identity {
            AgentIdentity::Known { program, .. } => Some(program.clone()),
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
    pub fn note_foreground(&self, stream: u32, leader: Option<Pid>) -> Option<GenerationId> {
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
            // The probe's own backoff is what bounds the cost instead. It runs at fifty
            // milliseconds right after something changes and doubles up to a minute while
            // nothing does, so a shell sitting at its prompt costs one group read a minute,
            // and that read finds a group of one and stops.
            let provisional = matches!(&agent.identity, AgentIdentity::Known { program, .. }
                if !qoro_domain::program::identify(program, &[]).agent);
            if !provisional || leader.is_none() {
                return None;
            }
            let pid = leader?;
            // Looked at with the generation it already has, because most of these look
            // again and find the same thing: minting one per probe would advance the
            // generation of every terminal running a wrapper several times a second, and a
            // generation is how everything else knows something actually changed.
            let current = agent.identity.generation().unwrap_or(GenerationId::FIRST);
            let program = match identity_of(pid, current) {
                AgentIdentity::Known { program, .. }
                    if qoro_domain::program::identify(&program, &[]).agent =>
                {
                    program
                }
                _ => return None,
            };
            // A different program is in this terminal than the one we had been reading, so
            // what was believed about the old one does not carry over, and that is a new
            // generation by definition.
            let generation = agent.capability.advance();
            agent.belief = fresh_belief(generation);
            agent.identity = AgentIdentity::Known {
                program,
                generation,
            };
            drop(terminals);
            self.announce(stream);
            return Some(generation);
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
                self.by_pid.lock().unwrap().remove(&pid.0);
                settle_exit(agent, stream);
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
    pub fn observe_screen(&self, stream: u32, verdict: &qoro_detect::Verdict) -> bool {
        let mut terminals = self.terminals.lock().unwrap();
        let Some(agent) = terminals.get_mut(&stream) else {
            return false;
        };
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
        };
        let before = agent.belief.state;
        let before_blocker = agent.belief.visible_blocker;
        // `accept` is the one that consumes the belief and returns the new one, so an
        // observation cannot be applied without going through the acceptance rule first.
        let Ok(next) = agent.belief.clone().accept(&observation) else {
            return false;
        };
        agent.belief = next;
        // Published when anything observable changed: the state or the visible
        // blocker. A soak of identical evaluations publishes nothing.
        let changed =
            agent.belief.state != before || agent.belief.visible_blocker != before_blocker;
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
    pub fn observe_hook(
        &self,
        stream: u32,
        terminal: &TerminalId,
        token: &CapabilityToken,
        state: AgentRuntimeState,
        visible_blocker: bool,
        visible_idle: bool,
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
        };
        let before = agent.belief.state;
        let before_blocker = agent.belief.visible_blocker;
        let Ok(next) = agent.belief.clone().accept(&observation) else {
            return Ok(false);
        };
        agent.belief = next;
        let changed =
            agent.belief.state != before || agent.belief.visible_blocker != before_blocker;
        drop(terminals);
        if changed {
            self.announce(stream);
        }
        Ok(changed)
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
            };
        }
    }
    AgentIdentity::Known {
        program: leader.name,
        generation,
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

    fn screen_verdict(state: AgentRuntimeState, visible_blocker: bool) -> qoro_detect::Verdict {
        qoro_detect::Verdict {
            state,
            rule: "test".into(),
            visible_blocker,
            visible_idle: false,
            visible_working: false,
        }
    }

    /// B3: a hook beats a screen reading from the same generation.
    #[test]
    fn a_hook_report_overrules_a_screen_reading_from_the_same_generation() {
        let r = registry();
        let token = mint_token(1);
        r.register(1, TerminalId::new("1"), token.clone());
        assert!(r.observe_screen(1, &screen_verdict(AgentRuntimeState::Working, false)));
        let changed = r
            .observe_hook(
                1,
                &TerminalId::new("1"),
                &token,
                AgentRuntimeState::Idle,
                false,
                false,
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
        )
        .expect("applies");
        let held = r.agent(1).expect("agent");
        assert_eq!(held.belief.sequence, 1, "a counter, never a wall clock");
        // A newer screen reading that positively sees a blocker must win, which
        // is exactly what a leaked wall clock would break: no later screen
        // sequence could ever exceed nanos.
        assert!(r.observe_screen(1, &screen_verdict(AgentRuntimeState::Blocked, true)));
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
        r.note_foreground(1, Some(me)).expect("a change");
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
                false
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
                false
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
        assert!(r.observe_screen(1, &working));
        rx.try_recv().expect("the change announces");
        for _ in 0..10 {
            assert!(!r.observe_screen(1, &working), "no change, no publish");
        }
        assert!(rx.try_recv().is_err(), "the soak must not publish anything");

        // Same state, newly visible blocker: observable, so published once.
        assert!(r.observe_screen(1, &screen_verdict(AgentRuntimeState::Working, true)));
        rx.try_recv().expect("the new blocker announces");
        assert!(!r.observe_screen(1, &screen_verdict(AgentRuntimeState::Working, true)));
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
        };
        assert_eq!(
            r.agent(1).expect("agent").belief.evaluate(&stale_screen),
            ObservationVerdict::Outranked
        );

        {
            let mut terminals = r.terminals.lock().unwrap();
            let agent = terminals.get_mut(&1).expect("agent");
            settle_exit(agent, 1);
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
        };
        assert!(r.observe_screen(1, &composer));
        assert_eq!(
            r.agent(1).expect("agent").belief.state,
            AgentRuntimeState::Idle
        );
    }
}

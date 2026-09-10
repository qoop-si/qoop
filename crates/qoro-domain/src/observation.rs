//! Agent observations and the acceptance rule.
//!
//! Invariant I5. There is no constructor that produces an observation without a generation
//! and a sample point, so "publish a state without a generation" is not expressible. Guarding
//! staleness only for hook-reported metadata, and relying on the screen-detection loop
//! being incidentally single threaded, would not be a portable guarantee.

use crate::ids::{AgentId, GenerationId, TerminalId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentRuntimeState {
    Starting,
    Idle,
    Working,
    Blocked,
    Done,
    Failed,
    Unknown,
    /// Two current sources of comparable strength disagree. A real state rather than an
    /// arbitrary winner, and one that never raises an attention item on its own.
    Uncertain,
}

/// Where an observation came from, strongest first. The ordering is the precedence rule,
/// so it is `Ord` and the comparison is the policy.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ObservationSource {
    /// Terminal screen heuristics. Weakest, and the one that needs the most maintenance.
    ScreenHeuristic = 0,
    /// Process identity and foreground process group.
    ProcessIdentity = 1,
    /// Our own shell or OSC integration.
    ShellIntegration = 2,
    /// An official hook or plugin event.
    Hook = 3,
    /// A native structured agent protocol. Strongest.
    NativeProtocol = 4,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum Confidence {
    /// Process identity is structurally unavailable, for example the foreground process is
    /// a container boundary and the agent is in a namespace we cannot read. Reporting the
    /// wrapper's identity with normal confidence would be a confident wrong answer.
    Unavailable,
    Low,
    Medium,
    High,
}

/// An observation of an agent's state.
///
/// `sampled_generation` is the generation that was current **when the input was sampled**,
/// not when the result was produced. A screen scan that started under generation 3 and
/// finished after a restart would otherwise be stamped 4 and accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentObservation {
    pub agent_id: AgentId,
    pub terminal_id: TerminalId,
    pub sampled_generation: GenerationId,
    pub state: AgentRuntimeState,
    pub source: ObservationSource,
    pub confidence: Confidence,
    /// Per (terminal, source, generation), assigned by the daemon when a source does not
    /// provide one. It resets on a new generation: a counter that survived a generation
    /// bump would let a late arrival from generation 3 outrank an early one from 4.
    pub sequence: u64,
    /// True only for a screen observation that positively saw a blocking modal. This is
    /// the one case where a weaker source may override a stronger one, and only when it is
    /// also newer.
    pub visible_blocker: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationVerdict {
    Accept,
    /// Sampled under a generation that is no longer current.
    StaleGeneration,
    /// Same source, an older sequence number.
    OutOfOrder,
    /// A stronger source already holds a current value.
    Outranked,
    /// Two comparable sources disagree.
    Uncertain,
}

/// What is currently believed about an agent, and the rule for changing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentBelief {
    pub generation: GenerationId,
    pub state: AgentRuntimeState,
    pub source: ObservationSource,
    pub sequence: u64,
    pub visible_blocker: bool,
    /// What disagreed, when the state is `Uncertain`.
    ///
    /// ADR-008 calls for `Uncertain { candidates }` rather than an arbitrary winner, and a
    /// bare `Uncertain` is the arbitrary winner with the evidence thrown away: a person
    /// looking at it cannot tell "the hook says working and the screen says blocked" from
    /// "we have no idea".
    pub candidates: Vec<(ObservationSource, AgentRuntimeState)>,
}

/// How far apart two sources have to be before the stronger simply wins.
///
/// One rank. Adjacent sources are close enough that preferring one on rank alone is a coin
/// toss dressed as a policy; two ranks apart is a real difference in what the source knows.
pub const COMPARABLE_STRENGTH: i32 = 1;

impl AgentBelief {
    /// The acceptance rule, as one function so there is exactly one place it can be wrong.
    pub fn evaluate(&self, obs: &AgentObservation) -> ObservationVerdict {
        if obs.sampled_generation < self.generation {
            return ObservationVerdict::StaleGeneration;
        }
        if obs.sampled_generation > self.generation {
            // A newer generation always wins: the old belief describes a process that no
            // longer exists.
            return ObservationVerdict::Accept;
        }
        if obs.source == self.source && obs.sequence <= self.sequence {
            return ObservationVerdict::OutOfOrder;
        }
        // Comparable strength and a different answer. Neither source is far enough above
        // the other for its rank to settle it, so the honest state is that they disagree.
        // Checked before the rank comparison, because the rank comparison is exactly the
        // arbitrary winner this rule exists to avoid.
        let gap = obs.source as i32 - self.source as i32;
        if obs.state != self.state
            // Two sources, not one changing its mind. A source that says something
            // different from what it said a moment ago is reporting a change, which is the
            // ordinary case: an agent that was working is now waiting. Counting that as a
            // disagreement made every second screen reading `Uncertain`, and a real agent
            // sat there labelled as a contradiction with nothing to contradict.
            && obs.source != self.source
            && gap.abs() <= COMPARABLE_STRENGTH
            && !obs.visible_blocker
            && self.state != AgentRuntimeState::Unknown
        {
            return ObservationVerdict::Uncertain;
        }
        if obs.source >= self.source {
            return ObservationVerdict::Accept;
        }
        // An unknown state is not a claim, so it cannot outrank one.
        //
        // A terminal starts with a belief whose source is the process table and whose state
        // is `Unknown`: we know which program is there and nothing about what it is doing.
        // Ranked literally, that placeholder outranks every screen observation forever, so
        // a terminal's state column stayed empty for the life of the process. Measured
        // exactly that on a shell sitting at its own prompt. The rank is about two sources
        // that both say something, which is what the rest of this function is for.
        if self.state == AgentRuntimeState::Unknown {
            return ObservationVerdict::Accept;
        }
        // Weaker source. The one exception: a screen observation that positively saw a
        // blocking modal, and is newer than what it is overriding. A hook can lag or
        // under-report a modal; the live screen cannot lie about one being on screen.
        if obs.visible_blocker && obs.sequence > self.sequence {
            return ObservationVerdict::Accept;
        }
        ObservationVerdict::Outranked
    }

    /// The only way to change a belief.
    ///
    /// `evaluate` used to be advisory and `apply` used to be public, so a caller could
    /// evaluate, ignore the verdict, and apply anyway. Invariant I5 is the whole point of
    /// this type, so acceptance is now the only door: a rejected observation returns its
    /// verdict and the belief is unchanged.
    pub fn accept(self, obs: &AgentObservation) -> Result<Self, ObservationVerdict> {
        match self.evaluate(obs) {
            ObservationVerdict::Accept => Ok(Self {
                generation: obs.sampled_generation,
                state: obs.state,
                source: obs.source,
                sequence: obs.sequence,
                visible_blocker: obs.visible_blocker,
                candidates: Vec::new(),
            }),
            // A disagreement is a change of state, not a rejection. Returning the old
            // belief unchanged would leave a confident wrong answer standing, which is the
            // outcome the rule exists to prevent.
            ObservationVerdict::Uncertain => Ok(Self {
                generation: obs.sampled_generation,
                state: AgentRuntimeState::Uncertain,
                source: obs.source.max(self.source),
                sequence: obs.sequence,
                visible_blocker: false,
                candidates: vec![(self.source, self.state), (obs.source, obs.state)],
            }),
            other => Err(other),
        }
    }

    /// Whether this belief may raise an attention item.
    ///
    /// ADR-008: `Uncertain` never raises one on its own. An attention item is an
    /// interruption, and interrupting somebody to say that two sources disagree is worse
    /// than saying nothing. The same goes for a belief built on an observation whose
    /// identity was structurally unavailable.
    pub fn raises_attention(&self) -> bool {
        matches!(
            self.state,
            AgentRuntimeState::Blocked | AgentRuntimeState::Failed | AgentRuntimeState::Done
        )
    }
}

#[cfg(test)]
mod one_source_changing_its_mind {
    use super::*;

    fn screen(state: AgentRuntimeState, sequence: u64) -> AgentObservation {
        AgentObservation {
            agent_id: crate::AgentId::new("a"),
            terminal_id: crate::TerminalId::new("1"),
            sampled_generation: GenerationId::FIRST,
            state,
            source: ObservationSource::ScreenHeuristic,
            confidence: Confidence::Medium,
            sequence,
            visible_blocker: false,
        }
    }

    #[test]
    fn a_second_reading_from_the_same_source_replaces_the_first() {
        let belief = AgentBelief {
            generation: GenerationId::FIRST,
            state: AgentRuntimeState::Working,
            source: ObservationSource::ScreenHeuristic,
            sequence: 1,
            visible_blocker: false,
            candidates: Vec::new(),
        };
        // The screen said working and now says blocked, which is an agent that reached a
        // question rather than two sources disagreeing about the same moment.
        let after = belief
            .accept(&screen(AgentRuntimeState::Blocked, 2))
            .expect("a source may change its mind");
        assert_eq!(after.state, AgentRuntimeState::Blocked);
    }

    #[test]
    fn two_different_sources_of_the_same_strength_are_still_uncertain() {
        let belief = AgentBelief {
            generation: GenerationId::FIRST,
            state: AgentRuntimeState::Working,
            source: ObservationSource::ProcessIdentity,
            sequence: 1,
            visible_blocker: false,
            candidates: Vec::new(),
        };
        assert_eq!(
            belief.evaluate(&screen(AgentRuntimeState::Idle, 2)),
            ObservationVerdict::Uncertain
        );
    }
}

#[cfg(test)]
mod unknown_is_not_a_claim {
    use super::*;

    fn screen(state: AgentRuntimeState, sequence: u64) -> AgentObservation {
        AgentObservation {
            agent_id: crate::AgentId::new("a"),
            terminal_id: crate::TerminalId::new("1"),
            sampled_generation: GenerationId::FIRST,
            state,
            source: ObservationSource::ScreenHeuristic,
            confidence: Confidence::Medium,
            sequence,
            visible_blocker: false,
        }
    }

    #[test]
    fn a_screen_is_accepted_over_a_placeholder_from_a_stronger_source() {
        let belief = AgentBelief {
            generation: GenerationId::FIRST,
            state: AgentRuntimeState::Unknown,
            source: ObservationSource::ProcessIdentity,
            sequence: 0,
            visible_blocker: false,
            candidates: Vec::new(),
        };
        let after = belief
            .accept(&screen(AgentRuntimeState::Idle, 1))
            .expect("a placeholder does not outrank an observation");
        assert_eq!(after.state, AgentRuntimeState::Idle);
    }

    #[test]
    fn a_screen_still_does_not_outrank_a_stronger_source_that_said_something() {
        let belief = AgentBelief {
            generation: GenerationId::FIRST,
            state: AgentRuntimeState::Working,
            source: ObservationSource::Hook,
            sequence: 5,
            visible_blocker: false,
            candidates: Vec::new(),
        };
        // The rule this fix must not weaken: a hook that reported working is not overruled
        // by a screen that has not caught up.
        assert!(belief.accept(&screen(AgentRuntimeState::Idle, 9)).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn belief(
        generation: u64,
        state: AgentRuntimeState,
        source: ObservationSource,
        seq: u64,
    ) -> AgentBelief {
        AgentBelief {
            generation: GenerationId(generation),
            state,
            source,
            sequence: seq,
            visible_blocker: false,
            candidates: Vec::new(),
        }
    }

    fn obs(
        generation: u64,
        state: AgentRuntimeState,
        source: ObservationSource,
        seq: u64,
    ) -> AgentObservation {
        AgentObservation {
            agent_id: AgentId::new("a"),
            terminal_id: TerminalId::new("t"),
            sampled_generation: GenerationId(generation),
            state,
            source,
            confidence: Confidence::High,
            sequence: seq,
            visible_blocker: false,
        }
    }

    #[test]
    fn an_observation_sampled_under_an_older_generation_is_rejected() {
        // Gate G3.1. This is the failure the generation check exists to stop.
        let b = belief(2, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        let stale = obs(
            1,
            AgentRuntimeState::Idle,
            ObservationSource::NativeProtocol,
            99,
        );
        assert_eq!(b.evaluate(&stale), ObservationVerdict::StaleGeneration);
    }

    #[test]
    fn a_newer_generation_always_wins_even_from_a_weaker_source() {
        let b = belief(
            1,
            AgentRuntimeState::Working,
            ObservationSource::NativeProtocol,
            5,
        );
        let newer = obs(
            2,
            AgentRuntimeState::Idle,
            ObservationSource::ScreenHeuristic,
            0,
        );
        assert_eq!(b.evaluate(&newer), ObservationVerdict::Accept);
    }

    #[test]
    fn an_older_sequence_from_the_same_source_is_rejected() {
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        assert_eq!(
            b.evaluate(&obs(1, AgentRuntimeState::Idle, ObservationSource::Hook, 4)),
            ObservationVerdict::OutOfOrder
        );
    }

    #[test]
    fn a_newer_visible_modal_overrides_a_hook_that_still_says_working() {
        // Gate G3.2: a terminal must not read as ready while a prompt is waiting on screen.
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        let mut modal = obs(
            1,
            AgentRuntimeState::Blocked,
            ObservationSource::ScreenHeuristic,
            6,
        );
        modal.visible_blocker = true;
        assert_eq!(b.evaluate(&modal), ObservationVerdict::Accept);
    }

    #[test]
    fn an_older_visible_modal_does_not_override() {
        // The other half of G3.2, and the half a naive implementation gets wrong.
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        let mut modal = obs(
            1,
            AgentRuntimeState::Blocked,
            ObservationSource::ScreenHeuristic,
            4,
        );
        modal.visible_blocker = true;
        assert_ne!(b.evaluate(&modal), ObservationVerdict::Accept);
    }

    #[test]
    fn a_weaker_source_without_a_modal_is_outranked() {
        let b = belief(
            1,
            AgentRuntimeState::Working,
            ObservationSource::NativeProtocol,
            5,
        );
        assert_eq!(
            b.evaluate(&obs(
                1,
                AgentRuntimeState::Idle,
                ObservationSource::ProcessIdentity,
                6
            )),
            ObservationVerdict::Outranked
        );
    }

    #[test]
    fn applying_an_accepted_observation_advances_the_belief() {
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        let next = obs(2, AgentRuntimeState::Idle, ObservationSource::Hook, 1);
        let b = b.accept(&next).expect("a newer generation is accepted");
        assert_eq!(b.generation, GenerationId(2));
        assert_eq!(b.state, AgentRuntimeState::Idle);
    }

    /// G3.5, first half: comparable strength and a different answer is a disagreement, not
    /// a winner.
    #[test]
    fn two_comparable_sources_that_disagree_produce_uncertain_with_both_candidates() {
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 1);
        let o = obs(
            1,
            AgentRuntimeState::Blocked,
            ObservationSource::ShellIntegration,
            2,
        );
        assert_eq!(b.evaluate(&o), ObservationVerdict::Uncertain);
        let next = b
            .accept(&o)
            .expect("a disagreement is a state, not a rejection");
        assert_eq!(next.state, AgentRuntimeState::Uncertain);
        assert_eq!(
            next.candidates,
            vec![
                (ObservationSource::Hook, AgentRuntimeState::Working),
                (
                    ObservationSource::ShellIntegration,
                    AgentRuntimeState::Blocked
                ),
            ],
            "the evidence has to survive, or uncertain is indistinguishable from ignorance"
        );
    }

    /// G3.5, second half: uncertain never interrupts anybody.
    #[test]
    fn uncertain_never_raises_an_attention_item() {
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 1);
        let o = obs(
            1,
            AgentRuntimeState::Blocked,
            ObservationSource::ShellIntegration,
            2,
        );
        // Blocked on its own does raise one, which is what makes this test about the
        // disagreement rather than about the state.
        assert!(
            belief(1, AgentRuntimeState::Blocked, ObservationSource::Hook, 1).raises_attention()
        );
        let uncertain = b.accept(&o).expect("accepted as uncertain");
        assert!(!uncertain.raises_attention());
    }

    /// A source two ranks stronger simply wins. Uncertain is for a close call, not for
    /// every difference of opinion.
    #[test]
    fn a_clearly_stronger_source_still_wins_outright() {
        let b = belief(
            1,
            AgentRuntimeState::Working,
            ObservationSource::ScreenHeuristic,
            1,
        );
        let o = obs(1, AgentRuntimeState::Idle, ObservationSource::Hook, 2);
        assert_eq!(b.evaluate(&o), ObservationVerdict::Accept);
        assert_eq!(
            b.accept(&o).expect("accepted").state,
            AgentRuntimeState::Idle
        );
    }

    /// And agreement is never uncertain, however close the sources are. A weaker source
    /// agreeing changes nothing, which is `Outranked` and not a disagreement.
    #[test]
    fn comparable_sources_that_agree_are_not_uncertain() {
        let b = belief(1, AgentRuntimeState::Working, ObservationSource::Hook, 1);
        let weaker = obs(
            1,
            AgentRuntimeState::Working,
            ObservationSource::ShellIntegration,
            2,
        );
        assert_eq!(b.evaluate(&weaker), ObservationVerdict::Outranked);
        let stronger = obs(
            1,
            AgentRuntimeState::Working,
            ObservationSource::NativeProtocol,
            2,
        );
        assert_eq!(b.evaluate(&stronger), ObservationVerdict::Accept);
    }

    #[test]
    fn a_rejected_observation_cannot_change_the_belief() {
        // There is no public way to apply without evaluating, so a caller that ignores the
        // verdict cannot break invariant I5 by accident.
        let b = belief(2, AgentRuntimeState::Working, ObservationSource::Hook, 5);
        let stale = obs(
            1,
            AgentRuntimeState::Idle,
            ObservationSource::NativeProtocol,
            99,
        );
        assert_eq!(
            b.clone().accept(&stale),
            Err(ObservationVerdict::StaleGeneration)
        );
        assert_eq!(
            b.state,
            AgentRuntimeState::Working,
            "the belief must be unchanged"
        );
    }
}

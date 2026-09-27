//! Who is running in a terminal, and how sure we are.
//!
//! Two rules that are easy to state and easy to get wrong in opposite directions.
//!
//! An agent behind a namespace boundary, in a container or a sandbox, is not visible to us.
//! What we can see is the wrapper: `docker`, `nsenter`, a sandbox helper. Reporting that as
//! the agent's identity is a confident wrong answer, and it is the one a naive
//! implementation gives for free, because the wrapper is a real process with a real name.
//!
//! And an observation that arrives claiming to be from an agent has to prove it. The proof
//! is a token bound to the terminal, minted when the terminal is created, plus the
//! generation the observation was sampled under. ADR-015: the token binds the terminal
//! only, because the environment a child is launched with cannot be updated when the
//! generation changes, so a generation-bound token would reject every observation after the
//! first command.

use crate::ids::{GenerationId, TerminalId};
use crate::observation::Confidence;

/// A process that stands between us and the agent.
///
/// Named rather than guessed. A list of names is a heuristic that will be wrong, so it is
/// the caller that decides, and this type is what it says with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// A container runtime, a sandbox, or anything else that puts the real process in a
    /// namespace we cannot read.
    Namespace { wrapper: String },
}

/// What we know about the process in a terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentIdentity {
    /// We can see the process and read its identity.
    Known {
        program: String,
        generation: GenerationId,
        /// Whether `program` is a coding agent, decided once where the argv was read.
        ///
        /// Carried rather than looked up again from the name, because the name is not
        /// enough: `pi` is an agent when node is running it and a stranger's tool when it
        /// is its own executable, and only the argv tells the two apart. Everything that
        /// asks "is this an agent" later (the index, the reaper, a restore) reads this.
        agent: bool,
    },
    /// There is a process, and it is behind something we cannot see through.
    ///
    /// Deliberately not "the wrapper's identity with low confidence". A caller shown
    /// `docker` at low confidence will render "docker", and a person reading that will
    /// conclude their agent is called docker.
    Unavailable {
        boundary: Boundary,
        generation: GenerationId,
    },
    /// Nothing is running that we would call an agent.
    None,
}

impl AgentIdentity {
    /// The most confidence any observation about this identity may carry.
    ///
    /// A cap, not a value: a source may be less sure than this for its own reasons, and
    /// nothing may be more sure than what the identity itself supports.
    pub fn confidence_ceiling(&self) -> Confidence {
        match self {
            AgentIdentity::Known { .. } => Confidence::High,
            AgentIdentity::Unavailable { .. } => Confidence::Unavailable,
            AgentIdentity::None => Confidence::Low,
        }
    }

    pub fn generation(&self) -> Option<GenerationId> {
        match self {
            AgentIdentity::Known { generation, .. }
            | AgentIdentity::Unavailable { generation, .. } => Some(*generation),
            AgentIdentity::None => None,
        }
    }

    /// The program name to show, or nothing.
    ///
    /// `Unavailable` deliberately has no name to show. The wrapper's name is available and
    /// is exactly what must not be shown.
    pub fn program(&self) -> Option<&str> {
        match self {
            AgentIdentity::Known { program, .. } => Some(program),
            _ => None,
        }
    }
}

/// A capability token, minted per terminal.
///
/// Opaque and compared whole. It is not a secret in the cryptographic sense and does not
/// need to be: it is handed to the child of a terminal we started, and its job is to make
/// an observation from a different terminal, or from a process that was never given one,
/// distinguishable from one from this terminal's own child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityToken(String);

impl CapabilityToken {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a claimed observation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimRefusal {
    /// No token at all.
    NoToken,
    /// A token, but not this terminal's.
    WrongTerminal,
    /// Sampled under a generation older than the terminal's current one.
    StaleGeneration,
}

/// What a terminal will accept an observation from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCapability {
    pub terminal: TerminalId,
    pub token: CapabilityToken,
    /// Bumped when the foreground process changes, and never carried in the token.
    pub generation: GenerationId,
}

impl TerminalCapability {
    /// Check a claim.
    ///
    /// The generation is checked against the body of the observation rather than against
    /// the token, which is the whole of ADR-015's correction: a child's environment cannot
    /// be rewritten when the generation changes, so a token that carried one would be
    /// stale from the first command onward.
    pub fn admit(
        &self,
        terminal: &TerminalId,
        token: Option<&CapabilityToken>,
        sampled_generation: GenerationId,
    ) -> Result<(), ClaimRefusal> {
        let Some(token) = token else {
            return Err(ClaimRefusal::NoToken);
        };
        if terminal != &self.terminal || token != &self.token {
            return Err(ClaimRefusal::WrongTerminal);
        }
        if sampled_generation < self.generation {
            return Err(ClaimRefusal::StaleGeneration);
        }
        Ok(())
    }

    /// A new generation, for a terminal whose foreground process changed.
    pub fn advance(&mut self) -> GenerationId {
        self.generation = self.generation.next();
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capability() -> TerminalCapability {
        TerminalCapability {
            terminal: TerminalId::new("7"),
            token: CapabilityToken::new("t-7-abc"),
            generation: GenerationId(3),
        }
    }

    /// G3.6: an agent behind a namespace reports Unavailable, not the wrapper's identity.
    #[test]
    fn an_agent_behind_a_boundary_has_no_name_to_show_and_a_capped_confidence() {
        let id = AgentIdentity::Unavailable {
            boundary: Boundary::Namespace {
                wrapper: "docker".into(),
            },
            generation: GenerationId(1),
        };
        assert_eq!(
            id.program(),
            None,
            "the wrapper's name is exactly the confident wrong answer"
        );
        assert_eq!(id.confidence_ceiling(), Confidence::Unavailable);
        assert!(id.confidence_ceiling() < Confidence::Low);
    }

    #[test]
    fn a_visible_agent_is_named_and_uncapped() {
        let id = AgentIdentity::Known {
            program: "claude".into(),
            generation: GenerationId(2),
            agent: true,
        };
        assert_eq!(id.program(), Some("claude"));
        assert_eq!(id.confidence_ceiling(), Confidence::High);
    }

    /// G3.7, the forgery half.
    #[test]
    fn a_claim_without_a_token_is_refused() {
        assert_eq!(
            capability().admit(&TerminalId::new("7"), None, GenerationId(3)),
            Err(ClaimRefusal::NoToken)
        );
    }

    #[test]
    fn a_token_for_another_terminal_is_refused() {
        let other = CapabilityToken::new("t-8-xyz");
        assert_eq!(
            capability().admit(&TerminalId::new("7"), Some(&other), GenerationId(3)),
            Err(ClaimRefusal::WrongTerminal)
        );
        // And the right token presented against the wrong terminal.
        assert_eq!(
            capability().admit(
                &TerminalId::new("8"),
                Some(&CapabilityToken::new("t-7-abc")),
                GenerationId(3)
            ),
            Err(ClaimRefusal::WrongTerminal)
        );
    }

    #[test]
    fn a_claim_sampled_under_an_older_generation_is_refused() {
        assert_eq!(
            capability().admit(
                &TerminalId::new("7"),
                Some(&CapabilityToken::new("t-7-abc")),
                GenerationId(2)
            ),
            Err(ClaimRefusal::StaleGeneration)
        );
    }

    /// G3.7, the positive half a forgery-only test would miss: the same token still works
    /// after the foreground process has changed twice.
    ///
    /// This is the half that would have caught the design where the token carried the
    /// generation. Every forgery test passes under that design, and so does the first
    /// observation; everything after the first command is rejected.
    #[test]
    fn the_original_token_still_works_after_two_generations() {
        let mut cap = capability();
        let token = cap.token.clone();
        let g1 = cap.advance();
        let g2 = cap.advance();
        assert_eq!(g2.0, g1.0 + 1);
        assert_eq!(
            cap.admit(&TerminalId::new("7"), Some(&token), g2),
            Ok(()),
            "the token a child was launched with has to keep working"
        );
    }
}

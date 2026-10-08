//! Moving a workspace between machines, one authoritative side at a time.
//!
//! ADR-020. The worst failure this product can have is two machines each believing they own
//! the same workspace: both accepting input, both writing history, and a person unable to
//! tell which one their agent is actually in. Zero owners is recoverable and visible. Two is
//! neither.
//!
//! There is no transaction across two machines and a network, so the design is a sequence
//! of steps where every one is either reversible or is the single durable write that hands
//! authority over, and where the invariant is checkable after a failure at any point.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Who owns a workspace, from one machine's point of view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authority {
    /// This machine owns it and is serving it.
    Mine,
    /// This machine owns it and is handing it to the named machine. Still authoritative:
    /// until the handover completes, this is where the workspace is.
    HandingTo { machine: String },
    /// This machine is taking over from the named one and has not seen its release yet.
    ///
    /// Answers "I am taking over from A", never "I own this". The distinction is the whole
    /// of the crash window between the two durable writes of the handover.
    ClaimingFrom { machine: String },
    /// It lives on the named machine.
    Elsewhere { machine: String },
}

impl Authority {
    /// Whether this machine will serve the workspace.
    ///
    /// A conditional claim is not authority. A machine holding one tells a person it is
    /// taking over and does not accept input, which is what makes "exactly one" true across
    /// the crash window rather than only outside it.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, Authority::Mine | Authority::HandingTo { .. })
    }
}

/// The steps, in order. Enumerated so a gate can inject a failure at each one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Step {
    Quiesce,
    Snapshot,
    Transfer,
    Verify,
    Activate,
    Cleanup,
}

impl Step {
    pub const ALL: &'static [Step] = &[
        Step::Quiesce,
        Step::Snapshot,
        Step::Transfer,
        Step::Verify,
        Step::Activate,
        Step::Cleanup,
    ];

    /// Whether failing here abandons the move and returns everything to the source.
    ///
    /// Everything up to activation: nothing on the target is live and the source still owns
    /// the workspace, so there is nothing to undo beyond removing a staging area.
    pub fn is_reversible(self) -> bool {
        self < Step::Activate
    }
}

/// Why a move was refused before it started.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the working tree has uncommitted changes: {what}")]
    DirtyTree { what: String },
    #[error("this machine does not own the workspace")]
    NotOwner,
}

/// What happened when a step failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub source: Authority,
    pub target: Authority,
    /// Whether the move can be tried again from the beginning.
    pub abandoned: bool,
    /// Something a person has to deal with. Never silent.
    pub left_behind: Option<String>,
}

/// One machine's view of the move.
#[derive(Clone, Debug)]
pub struct Move {
    pub source_machine: String,
    pub target_machine: String,
    pub source: Authority,
    pub target: Authority,
    /// The last step that completed, so a crash leaves a record of where it was.
    pub completed: Option<Step>,
}

impl Move {
    pub fn begin(
        source_machine: impl Into<String>,
        target_machine: impl Into<String>,
        source: Authority,
        dirty: Option<&str>,
        override_dirty: bool,
    ) -> Result<Self, Refusal> {
        if !matches!(source, Authority::Mine) {
            return Err(Refusal::NotOwner);
        }
        // A move that carries a workspace and leaves uncommitted work behind loses work.
        // The override is per move and explicit: a setting is a decision made once by
        // somebody who has forgotten it.
        if let Some(what) = dirty
            && !override_dirty
        {
            return Err(Refusal::DirtyTree { what: what.into() });
        }
        let target_machine = target_machine.into();
        Ok(Self {
            source_machine: source_machine.into(),
            source: Authority::HandingTo {
                machine: target_machine.clone(),
            },
            target: Authority::Elsewhere {
                machine: String::new(),
            },
            target_machine,
            completed: None,
        })
    }

    /// Run one step. `fails` is the injection point a gate uses.
    pub fn step(&mut self, step: Step, fails: bool) -> Outcome {
        if fails {
            return self.fail_at(step);
        }
        match step {
            Step::Activate => {
                // Two durable writes, ordered so the target claims before the source
                // releases. The window between them is the one where both could answer, so
                // the target's claim is conditional until it sees the release.
                self.target = Authority::ClaimingFrom {
                    machine: self.source_machine.clone(),
                };
                self.source = Authority::Elsewhere {
                    machine: self.target_machine.clone(),
                };
                self.target = Authority::Mine;
            }
            Step::Cleanup => {
                // Cannot fail the move: the workspace is already on the target.
            }
            _ => {}
        }
        self.completed = Some(step);
        Outcome {
            source: self.source.clone(),
            target: self.target.clone(),
            abandoned: false,
            left_behind: None,
        }
    }

    fn fail_at(&mut self, step: Step) -> Outcome {
        if step.is_reversible() {
            // Nothing on the target is live, so authority goes back to the source and the
            // move is abandoned. The staging area, if any, is what is left behind.
            self.source = Authority::Mine;
            self.target = Authority::Elsewhere {
                machine: self.source_machine.clone(),
            };
            return Outcome {
                source: self.source.clone(),
                target: self.target.clone(),
                abandoned: true,
                left_behind: (step >= Step::Snapshot).then(|| "staging area".to_string()),
            };
        }
        match step {
            Step::Activate => {
                // The crash window. The target holds a conditional claim and the source
                // still owns it, so exactly one machine answers, and a person is told the
                // handover is incomplete.
                self.target = Authority::ClaimingFrom {
                    machine: self.source_machine.clone(),
                };
                Outcome {
                    source: self.source.clone(),
                    target: self.target.clone(),
                    abandoned: false,
                    left_behind: Some("an incomplete handover".into()),
                }
            }
            _ => {
                // Cleanup. The move succeeded and this is tidying.
                Outcome {
                    source: self.source.clone(),
                    target: self.target.clone(),
                    abandoned: false,
                    left_behind: Some("staging area, and possibly the working tree".into()),
                }
            }
        }
    }

    /// The invariant, as a question anybody can ask.
    pub fn authoritative_count(&self) -> usize {
        [&self.source, &self.target]
            .iter()
            .filter(|a| a.is_authoritative())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Move {
        Move::begin("a", "b", Authority::Mine, None, false).expect("begin")
    }

    /// G8.1: a dirty tree blocks the move unless it is overridden, per move.
    #[test]
    fn a_dirty_working_tree_blocks_a_move_and_the_override_is_explicit() {
        let refused = Move::begin("a", "b", Authority::Mine, Some("src/main.rs"), false)
            .expect_err("a move carried a workspace and left uncommitted work behind");
        assert_eq!(
            refused,
            Refusal::DirtyTree {
                what: "src/main.rs".into()
            }
        );
        assert!(
            Move::begin("a", "b", Authority::Mine, Some("src/main.rs"), true).is_ok(),
            "the override does not work, so a person cannot move a dirty tree at all"
        );
    }

    #[test]
    fn a_machine_that_does_not_own_the_workspace_cannot_move_it() {
        let refused = Move::begin(
            "a",
            "b",
            Authority::Elsewhere {
                machine: "c".into(),
            },
            None,
            false,
        )
        .expect_err("a machine moved a workspace it does not own");
        assert_eq!(refused, Refusal::NotOwner);
    }

    /// G8.2: a failure injected at each step leaves exactly one side authoritative.
    #[test]
    fn a_failure_at_every_step_leaves_exactly_one_authoritative_side() {
        for failing in Step::ALL {
            let mut m = started();
            assert_eq!(m.authoritative_count(), 1, "before {failing:?}");
            for step in Step::ALL {
                let outcome = m.step(*step, step == failing);
                assert_eq!(
                    m.authoritative_count(),
                    1,
                    "after {step:?} with a failure at {failing:?}: source {:?}, target {:?}",
                    outcome.source,
                    outcome.target
                );
                if step == failing {
                    break;
                }
            }
        }
    }

    /// And through a whole successful move, at every point.
    #[test]
    fn a_successful_move_never_has_two_owners_or_none() {
        let mut m = started();
        assert_eq!(m.authoritative_count(), 1);
        for step in Step::ALL {
            m.step(*step, false);
            assert_eq!(m.authoritative_count(), 1, "after {step:?}");
        }
        assert_eq!(m.target, Authority::Mine);
        assert_eq!(
            m.source,
            Authority::Elsewhere {
                machine: "b".into()
            }
        );
    }

    /// The crash window: the target holds a claim, not authority.
    #[test]
    fn a_crash_during_activation_leaves_a_claim_and_not_a_second_owner() {
        let mut m = started();
        for step in [Step::Quiesce, Step::Snapshot, Step::Transfer, Step::Verify] {
            m.step(step, false);
        }
        let outcome = m.step(Step::Activate, true);
        assert_eq!(
            outcome.target,
            Authority::ClaimingFrom {
                machine: "a".into()
            }
        );
        assert!(
            !outcome.target.is_authoritative(),
            "a claim is not authority"
        );
        assert!(outcome.source.is_authoritative(), "nobody is serving it");
        assert_eq!(m.authoritative_count(), 1);
        assert_eq!(
            outcome.left_behind.as_deref(),
            Some("an incomplete handover")
        );
    }

    /// Everything before activation is abandonable, and says what it left.
    #[test]
    fn a_failure_before_activation_abandons_the_move_and_names_what_is_left() {
        for step in [Step::Quiesce, Step::Snapshot, Step::Transfer, Step::Verify] {
            let mut m = started();
            let outcome = m.step(step, true);
            assert!(outcome.abandoned, "{step:?} was not abandonable");
            assert_eq!(outcome.source, Authority::Mine);
            if step >= Step::Snapshot {
                assert_eq!(
                    outcome.left_behind.as_deref(),
                    Some("staging area"),
                    "{step:?} left something without saying so"
                );
            }
        }
    }

    /// Cleanup cannot fail the move. The workspace is already on the target.
    #[test]
    fn a_cleanup_failure_is_reported_and_does_not_undo_the_move() {
        let mut m = started();
        for step in [
            Step::Quiesce,
            Step::Snapshot,
            Step::Transfer,
            Step::Verify,
            Step::Activate,
        ] {
            m.step(step, false);
        }
        let outcome = m.step(Step::Cleanup, true);
        assert!(
            !outcome.abandoned,
            "a cleanup failure undid a completed move"
        );
        assert_eq!(outcome.target, Authority::Mine);
        assert!(outcome.left_behind.is_some(), "the failure was silent");
    }
}

//! A projection is a fold over the log and has no other input.
//!
//! That is a strong rule and it is the one gate G4.1 checks: replay under a frozen clock,
//! twice, and compare byte for byte. A projection that reads the wall clock, a random
//! number or the filesystem fails that check rather than passing by luck, which is why the
//! clock is frozen rather than merely replayed.
//!
//! The consequence is the migration story: a projection may be deleted and rebuilt at any
//! time, so a schema change to one is a rebuild rather than a migration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Body, Event};

/// Anything that can be rebuilt from the log.
pub trait Projection: Default {
    /// Fold one event in. Deterministic: the same events in the same order produce the
    /// same value, every time, on every machine.
    fn apply(&mut self, event: &Event);

    fn rebuild(events: &[Event]) -> Self {
        let mut me = Self::default();
        for event in events {
            me.apply(event);
        }
        me
    }
}

/// What a client needs to show a workspace without reading the log itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceView {
    /// Terminals that have been created and not closed, in creation order.
    pub terminals: Vec<u32>,
    /// The last state believed of each terminal's agent, and where it came from.
    pub agent_state: BTreeMap<u32, AgentSummary>,
    /// History ordinals a client has not read yet, per terminal.
    pub output_to: BTreeMap<u32, u64>,
    /// Everything the log admits it lost.
    pub gaps: Vec<GapSummary>,
    /// Commands with no outcome yet.
    pub pending: Vec<String>,
    /// What each pending command asked for, so its outcome can be applied.
    ///
    /// A command is an intent and an outcome is what happened, and only the outcome knows
    /// which terminal was created. Folding the effect from the command would record a
    /// terminal that may never have started.
    pub intents: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub state: String,
    pub source: String,
    pub confidence: String,
    pub generation: u64,
    /// What the conclusion was drawn from. Kept because a wrong conclusion has to be
    /// diagnosable and not merely visible.
    pub evidence: String,
    pub redacted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapSummary {
    pub dropped: u64,
    pub first_unix: i64,
    pub last_unix: i64,
    pub reason: String,
}

impl Projection for WorkspaceView {
    fn apply(&mut self, event: &Event) {
        match &event.body {
            Body::Command {
                idempotency_key,
                operation,
                ..
            } => {
                self.pending.push(idempotency_key.clone());
                self.intents
                    .insert(idempotency_key.clone(), operation.clone());
            }
            Body::Outcome {
                idempotency_key,
                ok,
                detail,
            } => {
                self.pending.retain(|k| k != idempotency_key);
                let operation = self.intents.remove(idempotency_key);
                if !ok {
                    return;
                }
                let terminal = detail
                    .get("terminal")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                match (operation.as_deref(), terminal) {
                    (Some("CreateTerminal"), Some(id)) => {
                        if !self.terminals.contains(&id) {
                            self.terminals.push(id);
                        }
                    }
                    (Some("CloseTerminal"), Some(id)) => self.terminals.retain(|t| *t != id),
                    _ => {}
                }
            }
            Body::Observation {
                terminal,
                state,
                source,
                confidence,
                sampled_generation,
                evidence,
                redacted,
            } => {
                // Later wins within a stream, and the stream is the terminal, so this is
                // an ordering the log guarantees rather than one the fold assumes.
                self.agent_state.insert(
                    *terminal,
                    AgentSummary {
                        state: state.clone(),
                        source: source.clone(),
                        confidence: confidence.clone(),
                        generation: *sampled_generation,
                        evidence: evidence.clone(),
                        redacted: *redacted,
                    },
                );
            }
            Body::Gap {
                dropped,
                first_unix,
                last_unix,
                reason,
            } => self.gaps.push(GapSummary {
                dropped: *dropped,
                first_unix: *first_unix,
                last_unix: *last_unix,
                reason: reason.clone(),
            }),
            Body::Output {
                terminal,
                to_ordinal,
                ..
            } => {
                self.output_to.insert(*terminal, *to_ordinal);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stream;

    fn events() -> Vec<Event> {
        vec![
            Event {
                sequence: 1,
                stream: Stream::Workspace(1),
                at_unix: 10,
                body: Body::Command {
                    idempotency_key: "c1".into(),
                    operation: "CreateTerminal".into(),
                    detail: serde_json::json!({ "cwd": "/tmp" }),
                },
            },
            Event {
                sequence: 2,
                stream: Stream::Workspace(1),
                at_unix: 11,
                body: Body::Outcome {
                    idempotency_key: "c1".into(),
                    ok: true,
                    detail: serde_json::json!({ "terminal": 1 }),
                },
            },
            Event {
                sequence: 1,
                stream: Stream::Terminal(1),
                at_unix: 12,
                body: Body::Observation {
                    terminal: 1,
                    state: "Working".into(),
                    source: "Hook".into(),
                    confidence: "High".into(),
                    sampled_generation: 2,
                    evidence: "tool call started".into(),
                    redacted: false,
                },
            },
            Event {
                sequence: 2,
                stream: Stream::Terminal(1),
                at_unix: 13,
                body: Body::Gap {
                    dropped: 42,
                    first_unix: 12,
                    last_unix: 13,
                    reason: "observation buffer full".into(),
                },
            },
        ]
    }

    /// G4.1: the same log produces the same projection, byte for byte, twice.
    #[test]
    fn a_projection_is_a_pure_fold_and_replays_identically() {
        let log = events();
        let first = WorkspaceView::rebuild(&log);
        let second = WorkspaceView::rebuild(&log);
        assert_eq!(
            serde_json::to_string(&first).expect("encode"),
            serde_json::to_string(&second).expect("encode"),
            "two replays of one log disagreed"
        );
        assert_eq!(first.terminals, vec![1]);
        assert!(
            first.pending.is_empty(),
            "an answered command is not pending"
        );
        assert_eq!(first.gaps.len(), 1);
    }

    /// A projection built one event at a time is the same as one built from the whole log.
    /// Anything else means the fold depends on something outside the events.
    #[test]
    fn folding_incrementally_matches_folding_at_once() {
        let log = events();
        let mut incremental = WorkspaceView::default();
        for event in &log {
            incremental.apply(event);
        }
        assert_eq!(incremental, WorkspaceView::rebuild(&log));
    }

    #[test]
    fn a_command_with_no_outcome_stays_pending() {
        let mut log = events();
        log.remove(1);
        let view = WorkspaceView::rebuild(&log);
        assert_eq!(view.pending, vec!["c1".to_string()]);
    }

    /// The evidence survives the fold. A projection that kept only the conclusion would
    /// make a wrong conclusion visible and not diagnosable.
    #[test]
    fn an_observations_evidence_reaches_the_projection() {
        let view = WorkspaceView::rebuild(&events());
        let summary = view.agent_state.get(&1).expect("an agent summary");
        assert_eq!(summary.evidence, "tool call started");
        assert_eq!(summary.generation, 2);
        assert!(!summary.redacted);
    }
}

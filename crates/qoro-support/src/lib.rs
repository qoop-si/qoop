//! Support bundles: a log slice plus the projections it produces.
//!
//! ADR-016. The bundle is safe because of where secrets are resolved, not because of what a
//! redactor remembered to strip: a `SecretRef` resolves directly into the environment block
//! handed to a child and never enters the log, so there is nothing here to filter.
//!
//! The two halves check each other. A projection is a fold over the log with no other input
//! (ADR-017), so rebuilding the projections from the slice and comparing is a cheap
//! integrity check, and a bundle whose projections disagree with its own log has been
//! edited.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use qoro_events::{Body, Event, Projection, WorkspaceView};

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("the bundle's projections do not match its own log")]
    Inconsistent,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// What the daemon counts. Counters, never content.
///
/// Nothing derived from what a person typed or an agent produced, including lengths and
/// hashes: a hash of a command line is a fingerprint of a command line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub terminals_created: u64,
    pub terminals_closed: u64,
    pub observations_recorded: u64,
    pub observations_dropped: u64,
    pub gaps: u64,
    pub commands_acknowledged: u64,
    pub commands_unreconciled: u64,
}

impl Counters {
    /// Counted from the log, so the counters cannot drift from what happened.
    pub fn from_log(events: &[Event]) -> Self {
        let mut c = Counters::default();
        for event in events {
            match &event.body {
                Body::Command { operation, .. } => match operation.as_str() {
                    "CreateTerminal" => c.terminals_created += 1,
                    "CloseTerminal" => c.terminals_closed += 1,
                    _ => {}
                },
                Body::Outcome { .. } => c.commands_acknowledged += 1,
                Body::Observation { .. } => c.observations_recorded += 1,
                Body::Gap { dropped, .. } => {
                    c.gaps += 1;
                    c.observations_dropped += dropped;
                }
                Body::Output { .. } => {}
            }
        }
        c.commands_unreconciled = c
            .terminals_created
            .saturating_add(c.terminals_closed)
            .saturating_sub(c.commands_acknowledged);
        c
    }
}

/// What the bundle says about itself, so a reader knows what it is holding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub daemon_version: String,
    pub platform: String,
    pub from_unix: i64,
    pub to_unix: i64,
    pub events: usize,
    /// Whether a person deliberately added a terminal's history, and which.
    ///
    /// History is the largest thing held and the most likely to contain something private,
    /// and it is almost never what a bug needs. Including it is an act, and the bundle
    /// records that the act happened.
    pub history_included: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub manifest: Manifest,
    pub events: Vec<Event>,
    pub workspace: WorkspaceView,
    pub counters: Counters,
}

impl Bundle {
    /// Build a bundle from a slice of the log.
    pub fn build(
        events: Vec<Event>,
        from_unix: i64,
        to_unix: i64,
        history_included: Vec<u32>,
    ) -> Self {
        let sliced: Vec<Event> = events
            .into_iter()
            .filter(|e| e.at_unix >= from_unix && e.at_unix <= to_unix)
            .collect();
        let workspace = WorkspaceView::rebuild(&sliced);
        let counters = Counters::from_log(&sliced);
        Bundle {
            manifest: Manifest {
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                platform: std::env::consts::OS.to_string(),
                from_unix,
                to_unix,
                events: sliced.len(),
                history_included,
            },
            events: sliced,
            workspace,
            counters,
        }
    }

    /// Rebuild the projections from the bundle's own log and compare.
    ///
    /// Cheap enough to run on every bundle rather than only when somebody is suspicious,
    /// because it is the same fold the daemon runs anyway.
    pub fn verify(&self) -> Result<(), BundleError> {
        let rebuilt = WorkspaceView::rebuild(&self.events);
        if rebuilt != self.workspace {
            return Err(BundleError::Inconsistent);
        }
        if Counters::from_log(&self.events) != self.counters {
            return Err(BundleError::Inconsistent);
        }
        if self.manifest.events != self.events.len() {
            return Err(BundleError::Inconsistent);
        }
        Ok(())
    }

    /// Every string a reader could see, for a test that has to look for a secret.
    pub fn all_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoro_events::Stream;

    const SECRET: &str = "sk-live-do-not-ship-this-value";

    fn log() -> Vec<Event> {
        vec![
            Event {
                sequence: 1,
                stream: Stream::Command("c1".into()),
                at_unix: 100,
                body: Body::Command {
                    idempotency_key: "c1".into(),
                    operation: "CreateTerminal".into(),
                    // What a Setup would carry: a reference, never a value (ADR-015).
                    detail: serde_json::json!({ "cwd": "/tmp", "env": ["API_KEY=secret:api"] }),
                },
            },
            Event {
                sequence: 2,
                stream: Stream::Command("c1".into()),
                at_unix: 101,
                body: Body::Outcome {
                    idempotency_key: "c1".into(),
                    ok: true,
                    detail: serde_json::json!({ "terminal": 1 }),
                },
            },
            Event {
                sequence: 1,
                stream: Stream::Terminal(1),
                at_unix: 102,
                body: Body::Observation {
                    terminal: 1,
                    state: "Blocked".into(),
                    source: "ScreenHeuristic".into(),
                    confidence: "Low".into(),
                    sampled_generation: 1,
                    // Redacted where it was recorded, not on the way out.
                    evidence: "export API_KEY=[redacted by rule api-key]".into(),
                    redacted: true,
                },
            },
            Event {
                sequence: 2,
                stream: Stream::Terminal(1),
                at_unix: 103,
                body: Body::Gap {
                    dropped: 7,
                    first_unix: 102,
                    last_unix: 103,
                    reason: "observation buffer full".into(),
                },
            },
        ]
    }

    /// G4.7, the half about secrets: a session that used one produces a bundle without it.
    #[test]
    fn a_bundle_from_a_session_that_used_a_secret_contains_no_secret() {
        // The value never enters the log, so it cannot be in the bundle. The test asserts
        // the property rather than the mechanism, because the property is what matters and
        // a future mechanism has to keep it.
        let bundle = Bundle::build(log(), 0, i64::MAX, Vec::new());
        let text = bundle.all_text();
        assert!(!text.contains(SECRET), "the bundle contains a secret value");
        assert!(
            text.contains("secret:api"),
            "the reference is missing, so a reader cannot tell which secret was used"
        );
    }

    /// And the redaction is visible, because a redaction that leaves no trace is
    /// indistinguishable from an absence of evidence.
    #[test]
    fn a_redaction_is_recorded_as_having_happened() {
        let bundle = Bundle::build(log(), 0, i64::MAX, Vec::new());
        let summary = bundle
            .workspace
            .agent_state
            .get(&1)
            .expect("an agent summary");
        assert!(summary.redacted);
        assert!(summary.evidence.contains("redacted by rule api-key"));
    }

    /// G4.7, the half about reconstruction: command to outcome to observation to gap.
    #[test]
    fn a_bundle_reconstructs_the_chain_from_command_to_state() {
        let bundle = Bundle::build(log(), 0, i64::MAX, Vec::new());
        assert_eq!(bundle.workspace.terminals, vec![1], "the command's effect");
        assert!(bundle.workspace.pending.is_empty(), "the outcome");
        let summary = bundle
            .workspace
            .agent_state
            .get(&1)
            .expect("the observation");
        assert_eq!(summary.state, "Blocked");
        assert_eq!(bundle.workspace.gaps.len(), 1, "the gap");
        assert_eq!(bundle.counters.observations_dropped, 7);
    }

    #[test]
    fn a_bundle_checks_its_own_projections_against_its_own_log() {
        let bundle = Bundle::build(log(), 0, i64::MAX, Vec::new());
        bundle.verify().expect("a fresh bundle is consistent");

        // An edited bundle: the projection says something the log does not.
        let mut edited = bundle.clone();
        edited.workspace.terminals.push(99);
        assert!(
            matches!(edited.verify(), Err(BundleError::Inconsistent)),
            "an edited bundle passed its own integrity check"
        );

        // And an edited log with the original projections.
        let mut trimmed = bundle.clone();
        trimmed.events.pop();
        assert!(matches!(trimmed.verify(), Err(BundleError::Inconsistent)));
    }

    #[test]
    fn a_bundle_holds_only_the_slice_it_says_it_holds() {
        let bundle = Bundle::build(log(), 101, 102, Vec::new());
        assert_eq!(bundle.manifest.events, 2);
        assert_eq!(bundle.events.len(), 2);
        bundle.verify().expect("consistent");
    }

    #[test]
    fn history_is_absent_unless_somebody_asked_for_it() {
        let without = Bundle::build(log(), 0, i64::MAX, Vec::new());
        assert!(without.manifest.history_included.is_empty());
        let with = Bundle::build(log(), 0, i64::MAX, vec![1]);
        assert_eq!(
            with.manifest.history_included,
            vec![1],
            "including history has to be visible in the manifest"
        );
    }

    /// Counters are counted from the log, so they cannot drift from what happened.
    #[test]
    fn counters_are_derived_and_never_content() {
        let bundle = Bundle::build(log(), 0, i64::MAX, Vec::new());
        assert_eq!(bundle.counters.terminals_created, 1);
        assert_eq!(bundle.counters.commands_acknowledged, 1);
        assert_eq!(bundle.counters.observations_recorded, 1);
        assert_eq!(bundle.counters.gaps, 1);
        let encoded = serde_json::to_string(&bundle.counters).expect("encode");
        assert!(
            !encoded.contains("evidence") && !encoded.contains("Blocked"),
            "the counters carry content: {encoded}"
        );
    }
}

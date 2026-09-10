//! What reaches somebody's phone, and what does not.
//!
//! A push payload leaves the machine and lands somewhere neither the person nor this
//! program controls: a notification service, a lock screen, a notification history that
//! syncs. So the rule is not "redact the payload", it is that the payload is built from a
//! declared allowlist of fields and cannot carry anything else.
//!
//! The difference matters. A redaction pass has to know every place content can be, and the
//! first field somebody adds without thinking about it is the leak. An allowlist has the
//! opposite failure: the field somebody forgets to declare simply does not arrive, which is
//! a bug report rather than a disclosure.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use qoro_domain::{AgentRuntimeState, TerminalId};

/// Every field a push payload may carry. Nothing else is representable.
pub const ALLOWED_FIELDS: &[&str] = &[
    "workspace",
    "terminal",
    "state",
    "reason",
    "at_unix",
    "priority",
];

/// Why somebody is being interrupted.
///
/// A small closed set, and deliberately not a free string: a free string is where terminal
/// content ends up, because the easiest way to explain why an agent is blocked is to quote
/// what it said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    /// An agent is waiting for an answer.
    AgentBlocked,
    /// An agent finished.
    AgentDone,
    /// An agent failed.
    AgentFailed,
    /// A person asked for something.
    RequestReceived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    Quiet,
    Normal,
    Urgent,
}

/// What goes to the phone.
///
/// Numbers and enumerations. No titles, no command lines, no output, no paths: a path is a
/// project name and a project name is somebody's client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Push {
    pub workspace: u32,
    pub terminal: u32,
    pub state: AgentRuntimeState,
    pub reason: Reason,
    pub at_unix: i64,
    pub priority: Priority,
}

impl Push {
    /// Build a push from a state change, or decide there is nothing to say.
    ///
    /// `Uncertain` never raises one (ADR-008). Interrupting somebody to say that two
    /// sources disagree is worse than saying nothing.
    pub fn for_state(
        workspace: u32,
        terminal: &TerminalId,
        state: AgentRuntimeState,
        at_unix: i64,
    ) -> Option<Self> {
        let (reason, priority) = match state {
            AgentRuntimeState::Blocked => (Reason::AgentBlocked, Priority::Urgent),
            AgentRuntimeState::Done => (Reason::AgentDone, Priority::Normal),
            AgentRuntimeState::Failed => (Reason::AgentFailed, Priority::Urgent),
            _ => return None,
        };
        Some(Push {
            workspace,
            terminal: terminal.as_str().parse().unwrap_or(0),
            state,
            reason,
            at_unix,
            priority,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Whether a serialised payload is within the allowlist and carries no content.
///
/// Two checks, because they catch different mistakes. The field check catches a new field
/// nobody declared. The content check catches a declared field that was filled with
/// something it should not hold, which is the mistake a field check cannot see.
pub fn audit(payload: &str, live_terminal_content: &[&str]) -> Result<(), AuditFailure> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|_| AuditFailure::NotJson)?;
    let Some(object) = value.as_object() else {
        return Err(AuditFailure::NotAnObject);
    };
    for key in object.keys() {
        if !ALLOWED_FIELDS.contains(&key.as_str()) {
            return Err(AuditFailure::UndeclaredField { name: key.clone() });
        }
    }
    for content in live_terminal_content {
        // Substrings, not equality: a payload that quoted part of a line is a payload that
        // leaked part of a line.
        for window in content.split_whitespace() {
            if window.len() >= 4 && payload.contains(window) {
                return Err(AuditFailure::ContentLeak {
                    fragment: window.to_string(),
                });
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuditFailure {
    #[error("the payload is not json")]
    NotJson,
    #[error("the payload is not an object")]
    NotAnObject,
    #[error("the payload carries an undeclared field: {name}")]
    UndeclaredField { name: String },
    #[error("the payload contains live terminal content: {fragment}")]
    ContentLeak { fragment: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> TerminalId {
        TerminalId::new("7")
    }

    /// G10.2: no field outside the allowlist, and no substring of live terminal content.
    #[test]
    fn a_push_carries_only_declared_fields_and_no_terminal_content() {
        let screen = [
            "$ export STRIPE_KEY=sk-live-abcdefgh",
            "error: cannot connect to postgres://app:hunter2@db/app",
            "Do you want to proceed? (y/n)",
        ];
        let push = Push::for_state(1, &terminal(), AgentRuntimeState::Blocked, 100)
            .expect("blocked raises one");
        audit(&push.to_json(), &screen).expect("the payload is clean");
    }

    /// The audit is not vacuous: a payload that did carry content is caught.
    #[test]
    fn the_audit_catches_a_payload_that_quotes_the_screen() {
        let screen = ["Do you want to proceed? (y/n)"];
        let leaky = r#"{"workspace":1,"terminal":7,"state":"Blocked","reason":"AgentBlocked","at_unix":1,"priority":"Urgent","note":"proceed?"}"#;
        match audit(leaky, &screen) {
            Err(AuditFailure::UndeclaredField { name }) => assert_eq!(name, "note"),
            other => panic!("a payload with an extra field passed: {other:?}"),
        }
    }

    #[test]
    fn the_audit_catches_content_inside_a_declared_field() {
        let screen = ["deploying to acme-corporation-staging"];
        // `reason` is declared, and has been filled with something it should not hold.
        let leaky = r#"{"workspace":1,"terminal":7,"state":"Blocked","reason":"acme-corporation-staging","at_unix":1,"priority":"Urgent"}"#;
        assert!(matches!(
            audit(leaky, &screen),
            Err(AuditFailure::ContentLeak { .. })
        ));
    }

    /// ADR-008: uncertain never interrupts anybody, and neither does ordinary work.
    #[test]
    fn only_states_worth_interrupting_for_produce_a_push() {
        for quiet in [
            AgentRuntimeState::Working,
            AgentRuntimeState::Idle,
            AgentRuntimeState::Starting,
            AgentRuntimeState::Unknown,
            AgentRuntimeState::Uncertain,
        ] {
            assert!(
                Push::for_state(1, &terminal(), quiet, 1).is_none(),
                "{quiet:?} interrupted somebody"
            );
        }
        for loud in [
            AgentRuntimeState::Blocked,
            AgentRuntimeState::Done,
            AgentRuntimeState::Failed,
        ] {
            assert!(Push::for_state(1, &terminal(), loud, 1).is_some());
        }
    }

    /// A blocked agent is urgent and a finished one is not, because a phone that treats
    /// them the same is a phone somebody turns off.
    #[test]
    fn priority_distinguishes_waiting_from_finished() {
        let blocked = Push::for_state(1, &terminal(), AgentRuntimeState::Blocked, 1).expect("push");
        let done = Push::for_state(1, &terminal(), AgentRuntimeState::Done, 1).expect("push");
        assert_eq!(blocked.priority, Priority::Urgent);
        assert_eq!(done.priority, Priority::Normal);
        assert!(blocked.priority > done.priority);
    }

    /// There is no field a title or a path could go in, which is the point.
    #[test]
    fn the_payload_has_nowhere_to_put_a_name() {
        let push = Push::for_state(1, &terminal(), AgentRuntimeState::Blocked, 1).expect("push");
        let value: serde_json::Value = serde_json::from_str(&push.to_json()).expect("json");
        let object = value.as_object().expect("object");
        assert_eq!(object.len(), ALLOWED_FIELDS.len());
        for key in object.keys() {
            assert!(
                ALLOWED_FIELDS.contains(&key.as_str()),
                "{key} is not declared"
            );
        }
    }
}

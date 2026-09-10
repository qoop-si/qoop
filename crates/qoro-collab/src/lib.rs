//! Agents asking each other for things, and the two rules that keep it from becoming a
//! machine for one agent to drive another.
//!
//! An agent cannot write to another agent's terminal. Not "should not", and not "is
//! prevented by a check in the write path": there is no method in the control codec that
//! takes another agent's terminal and some bytes. Capability is checked at the one place a
//! request is admitted, and the terminal write path is not reachable from a request at all.
//!
//! And a request chain terminates. Two agents that each answer a request with a request
//! will do so forever, and forever costs somebody money. Every request carries a hop count,
//! the count is not something the sender controls, and reaching the limit produces an
//! expiry event rather than silence.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use qoro_domain::{AgentId, TerminalId};

/// How far a request may travel before it expires.
///
/// Eight. Deep enough for any real delegation chain, shallow enough that a loop costs eight
/// turns rather than a bill.
pub const MAX_HOPS: u32 = 8;

/// Who is asking or being asked.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Participant {
    Human(String),
    Agent(AgentId),
}

/// What one participant may do to another's things.
///
/// A capability is granted to a pair, never held by a participant on its own: "this agent
/// may read that terminal" is the only shape that means anything, because the alternative
/// is a role that turns out to include something nobody intended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    /// See what is on a terminal's screen.
    ReadTerminal,
    /// Ask its owner for something. Not do it.
    Ask,
}

/// What a request is for.
///
/// Deliberately a small closed set, and deliberately containing nothing that writes. The
/// absence is the mechanism: gate G7.1 attempts every method in the codec, and it passes
/// because there is no method to attempt rather than because every one of them checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum RequestKind {
    /// Please look at this.
    Review { what: String },
    /// Please answer this.
    Question { text: String },
    /// Please take this task.
    Handoff { task: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestStatus {
    Open,
    Answered,
    Declined,
    /// Reached the hop limit. A terminal state, and an event.
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub kind: RequestKind,
    pub from: Participant,
    pub to: Participant,
    pub status: RequestStatus,
    /// How many times this chain has been forwarded. Set by the daemon from the request
    /// being answered, never by the sender: a sender that could set it to zero would have
    /// no limit at all.
    pub hop_count: u32,
    pub max_hops: u32,
    /// The request this one answers, if any. What makes a chain a chain.
    pub in_reply_to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("the sender may not ask this participant for anything")]
    NotPermitted,
    #[error("the chain reached its hop limit of {max_hops}")]
    HopLimit { max_hops: u32 },
    #[error("a request cannot be sent to its own sender")]
    SelfAddressed,
}

/// What happened, for the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestEvent {
    Sent {
        id: String,
        hop_count: u32,
    },
    /// Emitted when a chain hits the limit, so a person can see why it stopped rather than
    /// finding a conversation that trails off.
    Expired {
        id: String,
        hop_count: u32,
    },
}

/// Who may ask whom, and who may read what.
#[derive(Default)]
pub struct Grants {
    entries: Vec<(Participant, Participant, Capability)>,
    terminals: Vec<(Participant, TerminalId, Capability)>,
}

impl Grants {
    pub fn allow(&mut self, from: Participant, to: Participant, capability: Capability) {
        self.entries.push((from, to, capability));
    }

    pub fn allow_terminal(
        &mut self,
        who: Participant,
        terminal: TerminalId,
        capability: Capability,
    ) {
        self.terminals.push((who, terminal, capability));
    }

    pub fn may(&self, from: &Participant, to: &Participant, capability: Capability) -> bool {
        self.entries
            .iter()
            .any(|(f, t, c)| f == from && t == to && *c == capability)
    }

    /// Whether a participant may read a terminal.
    ///
    /// There is no write counterpart, and that is not an omission. Adding one would make
    /// every caller's mistake a write to somebody else's terminal, and the whole point of
    /// this module is that no such path exists.
    pub fn may_read_terminal(&self, who: &Participant, terminal: &TerminalId) -> bool {
        self.terminals
            .iter()
            .any(|(w, t, c)| w == who && t == terminal && *c == Capability::ReadTerminal)
    }
}

/// Send a request, or say why not.
///
/// The hop count comes from the request being answered and is incremented here. A caller
/// cannot pass one in, which is the difference between a limit and a suggestion.
pub fn send(
    grants: &Grants,
    id: impl Into<String>,
    kind: RequestKind,
    from: Participant,
    to: Participant,
    answering: Option<&Request>,
) -> Result<(Request, Vec<RequestEvent>), RequestError> {
    if from == to {
        return Err(RequestError::SelfAddressed);
    }
    if !grants.may(&from, &to, Capability::Ask) {
        return Err(RequestError::NotPermitted);
    }
    let hop_count = answering.map(|r| r.hop_count + 1).unwrap_or(0);
    let id = id.into();
    if hop_count >= MAX_HOPS {
        return Err(RequestError::HopLimit { max_hops: MAX_HOPS });
    }
    let request = Request {
        id: id.clone(),
        kind,
        from,
        to,
        status: RequestStatus::Open,
        hop_count,
        max_hops: MAX_HOPS,
        in_reply_to: answering.map(|r| r.id.clone()),
    };
    Ok((request, vec![RequestEvent::Sent { id, hop_count }]))
}

/// Mark a chain expired, with the event that says so.
pub fn expire(request: &mut Request) -> RequestEvent {
    request.status = RequestStatus::Expired;
    RequestEvent::Expired {
        id: request.id.clone(),
        hop_count: request.hop_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> Participant {
        Participant::Agent(AgentId::new(name))
    }

    fn both_ways() -> Grants {
        let mut g = Grants::default();
        g.allow(agent("a"), agent("b"), Capability::Ask);
        g.allow(agent("b"), agent("a"), Capability::Ask);
        g
    }

    /// G7.2: an adversarial ping-pong terminates, and says why.
    #[test]
    fn two_agents_answering_each_other_forever_stop_at_the_hop_limit() {
        let grants = both_ways();
        let (mut current, _) = send(
            &grants,
            "r0",
            RequestKind::Question { text: "?".into() },
            agent("a"),
            agent("b"),
            None,
        )
        .expect("the first request");

        let mut sent = 1;
        let mut expired = None;
        for turn in 1..100 {
            let (from, to) = if turn % 2 == 1 {
                (agent("b"), agent("a"))
            } else {
                (agent("a"), agent("b"))
            };
            match send(
                &grants,
                format!("r{turn}"),
                RequestKind::Question { text: "?".into() },
                from,
                to,
                Some(&current),
            ) {
                Ok((next, _)) => {
                    current = next;
                    sent += 1;
                }
                Err(RequestError::HopLimit { .. }) => {
                    expired = Some(expire(&mut current));
                    break;
                }
                Err(other) => panic!("unexpected refusal: {other}"),
            }
        }

        assert_eq!(
            sent, MAX_HOPS as usize,
            "the chain ran for {sent} turns against a limit of {MAX_HOPS}"
        );
        assert_eq!(
            expired,
            Some(RequestEvent::Expired {
                id: format!("r{}", MAX_HOPS - 1),
                hop_count: MAX_HOPS - 1,
            }),
            "the chain stopped without saying why"
        );
        assert_eq!(current.status, RequestStatus::Expired);
    }

    /// The hop count is not the sender's to set.
    #[test]
    fn a_sender_cannot_reset_the_hop_count() {
        let grants = both_ways();
        let (first, _) = send(
            &grants,
            "r0",
            RequestKind::Question { text: "?".into() },
            agent("a"),
            agent("b"),
            None,
        )
        .expect("sent");
        let (second, _) = send(
            &grants,
            "r1",
            RequestKind::Question { text: "?".into() },
            agent("b"),
            agent("a"),
            Some(&first),
        )
        .expect("sent");
        assert_eq!(second.hop_count, 1);
        // There is no argument that could have made it zero: the count comes from the
        // request being answered.
    }

    #[test]
    fn a_request_to_somebody_who_did_not_allow_it_is_refused() {
        let mut grants = Grants::default();
        grants.allow(agent("a"), agent("b"), Capability::Ask);
        assert!(matches!(
            send(
                &grants,
                "r",
                RequestKind::Question { text: "?".into() },
                agent("b"),
                agent("a"),
                None
            ),
            Err(RequestError::NotPermitted)
        ));
    }

    #[test]
    fn a_request_cannot_be_addressed_to_its_own_sender() {
        let mut grants = Grants::default();
        grants.allow(agent("a"), agent("a"), Capability::Ask);
        assert!(matches!(
            send(
                &grants,
                "r",
                RequestKind::Question { text: "?".into() },
                agent("a"),
                agent("a"),
                None
            ),
            Err(RequestError::SelfAddressed)
        ));
    }

    /// G7.1: reading is granted per terminal, and there is no write counterpart to grant.
    #[test]
    fn reading_a_terminal_is_granted_per_terminal() {
        let mut grants = Grants::default();
        grants.allow_terminal(agent("a"), TerminalId::new("1"), Capability::ReadTerminal);
        assert!(grants.may_read_terminal(&agent("a"), &TerminalId::new("1")));
        assert!(!grants.may_read_terminal(&agent("a"), &TerminalId::new("2")));
        assert!(!grants.may_read_terminal(&agent("b"), &TerminalId::new("1")));
    }
}

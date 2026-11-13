//! What an agent row shows: one status, decided here and nowhere else.
//!
//! The belief (`AgentRuntimeState`) is what the agent is doing; a status is what a person is
//! shown, which also reads what waits on them, what they have not looked at yet and what runs
//! on after a turn. The daemon decides it once per row and sends it as a token; the TUI, the
//! notch, the phone and the web draw the token and never work it out again, so they cannot
//! disagree about one agent at one moment.

use crate::AgentRuntimeState;

/// The statuses, in the order the first one that holds wins (see [`of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// Waits on a person: a dialog on screen, an open question card, or its task waiting.
    Blocked,
    Failed,
    /// Its turn is going, or sub-agents it waits on are: Claude reads "Waiting for 3
    /// background agents to finish" and goes on by itself when they end.
    Working,
    /// A session opening: drawn still, worded as working.
    Starting,
    /// Set aside by a person to answer later, and settled.
    Parked,
    /// The turn is over and a shell or monitor it started still runs: Claude reads "done,
    /// 1 shell still running". Not news, never work.
    Background,
    /// A turn ended and nobody has looked at it yet.
    Done,
    Idle,
    /// Nothing is known yet, or two sources disagree.
    Unknown,
}

/// Every status, for tests and for clients that list the words.
pub const ALL: [Status; 9] = [
    Status::Blocked,
    Status::Failed,
    Status::Working,
    Status::Starting,
    Status::Parked,
    Status::Background,
    Status::Done,
    Status::Idle,
    Status::Unknown,
];

/// The word a sleeping tab shows. A tab fact rather than a status: a sleeping tab has no
/// process and no row in the listing.
pub const SLEEPING: &str = "sleeping";

/// Everything a status is decided from, as plain values.
#[derive(Clone, Copy, Debug)]
pub struct StatusFacts {
    pub state: AgentRuntimeState,
    /// Sub-agents running: the hooks' set or the screen's list of background agents.
    pub subagents: usize,
    /// Shells and monitors running, from the footer.
    pub background_tasks: usize,
    /// An open question card for the terminal, or its task waiting on a person.
    pub needs_you: bool,
    /// A turn ended and nobody has looked at it since.
    pub unseen_stop: bool,
    pub parked: bool,
}

/// The status of a row: the first case that holds.
pub fn of(f: StatusFacts) -> Status {
    use AgentRuntimeState as S;
    match f.state {
        S::Blocked => return Status::Blocked,
        _ if f.needs_you => return Status::Blocked,
        S::Failed => return Status::Failed,
        S::Working => return Status::Working,
        _ if f.subagents > 0 => return Status::Working,
        S::Starting => return Status::Starting,
        S::Uncertain | S::Unknown => return Status::Unknown,
        S::Idle | S::Done => {}
    }
    if f.parked {
        Status::Parked
    } else if f.background_tasks > 0 {
        Status::Background
    } else if f.unseen_stop {
        Status::Done
    } else {
        Status::Idle
    }
}

impl Status {
    /// The token on the wire.
    pub fn token(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Working => "working",
            Self::Starting => "starting",
            Self::Parked => "parked",
            Self::Background => "background",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    /// The status a token stands for; `None` for one this build does not know.
    pub fn from_token(token: &str) -> Option<Self> {
        ALL.into_iter().find(|s| s.token() == token)
    }

    /// The word every surface shows for it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Blocked => "needs you",
            Self::Failed => "failed",
            Self::Working | Self::Starting => "working",
            Self::Parked => "parked",
            Self::Background => "background",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Unknown => "unsure",
        }
    }

    /// Where a row sorts: what wants a person first, then what is alive, then what is settled.
    /// A sleeping tab sorts with `Unknown`.
    pub fn rank(self) -> u8 {
        match self {
            Self::Blocked => 0,
            Self::Failed => 1,
            Self::Done => 2,
            Self::Working => 3,
            Self::Starting => 4,
            Self::Parked | Self::Background | Self::Idle => 6,
            Self::Unknown => 7,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentRuntimeState as S;

    fn facts(state: AgentRuntimeState) -> StatusFacts {
        StatusFacts {
            state,
            subagents: 0,
            background_tasks: 0,
            needs_you: false,
            unseen_stop: false,
            parked: false,
        }
    }

    #[test]
    fn a_turn_over_with_a_shell_running_is_background_not_work() {
        let f = StatusFacts {
            background_tasks: 1,
            ..facts(S::Idle)
        };
        assert_eq!(of(f), Status::Background);
        // Not news either: work still runs, as the TUI and the phone said before.
        assert_eq!(
            of(StatusFacts {
                unseen_stop: true,
                ..f
            }),
            Status::Background
        );
    }

    #[test]
    fn a_turn_waiting_on_its_sub_agents_is_working() {
        let f = StatusFacts {
            subagents: 2,
            background_tasks: 3,
            unseen_stop: true,
            ..facts(S::Idle)
        };
        assert_eq!(of(f), Status::Working);
    }

    #[test]
    fn what_waits_on_a_person_comes_first_and_a_park_never_hides_it() {
        let card = StatusFacts {
            needs_you: true,
            parked: true,
            ..facts(S::Idle)
        };
        assert_eq!(of(card), Status::Blocked);
        assert_eq!(of(facts(S::Blocked)), Status::Blocked);
        assert_eq!(
            of(StatusFacts {
                needs_you: true,
                ..facts(S::Working)
            }),
            Status::Blocked
        );
    }

    #[test]
    fn a_finish_reads_done_until_seen_and_a_park_wins_over_it() {
        let finish = StatusFacts {
            unseen_stop: true,
            ..facts(S::Idle)
        };
        assert_eq!(of(finish), Status::Done);
        assert_eq!(of(facts(S::Idle)), Status::Idle);
        assert_eq!(
            of(StatusFacts {
                parked: true,
                ..finish
            }),
            Status::Parked
        );
    }

    #[test]
    fn starting_keeps_its_token_and_reads_working() {
        assert_eq!(of(facts(S::Starting)), Status::Starting);
        assert_eq!(Status::Starting.word(), "working");
        assert_eq!(of(facts(S::Uncertain)), Status::Unknown);
        assert_eq!(of(facts(S::Failed)), Status::Failed);
    }

    #[test]
    fn every_token_reads_back() {
        for s in ALL {
            assert_eq!(Status::from_token(s.token()), Some(s));
        }
        assert_eq!(Status::from_token("sleeping"), None);
    }
}

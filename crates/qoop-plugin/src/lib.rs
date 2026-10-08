//! Plugins: a process, a capability set, and three bounds.
//!
//! ADR-018. Every terminal multiplexer that grew a plugin system grew it the same way: a
//! scripting language in the same process, full access to everything, and a decade of "do
//! not install untrusted plugins" as the security model. This product holds a person's
//! agent sessions, their working trees, and a socket that can start processes as them, so
//! that model is disqualifying here.
//!
//! What a plugin can do is granted per object, and what it cannot do is not a check: there
//! is no method that takes a terminal and some bytes.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use qoop_domain::TerminalId;

/// What a plugin may be granted.
///
/// A small closed set. There is no write capability, and the absence is the mechanism
/// rather than an omission: a plugin cannot type into a terminal because no method takes a
/// terminal and some bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    /// See what is on a terminal's screen.
    ReadTerminal,
    /// See a terminal's scrolled off history.
    ReadHistory,
    /// See the workspace's shape: which terminals exist and what is believed of them.
    ReadWorkspace,
    /// Add an item to somebody's attention list. Not a notification: the person's own
    /// settings decide what interrupts them.
    RaiseAttention,
}

/// The bounds. Each has a counter, and exceeding one stops the plugin.
///
/// No warning threshold, because a warning is a thing nobody reads. No automatic restart,
/// because a plugin that fails its bounds once will fail them again and a restart loop is
/// worse than a stopped plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub call_deadline: Duration,
    pub calls_per_interval: u32,
    pub interval: Duration,
    pub max_outstanding: u32,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            call_deadline: Duration::from_secs(2),
            calls_per_interval: 100,
            interval: Duration::from_secs(1),
            max_outstanding: 8,
        }
    }
}

/// Why a plugin was stopped.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Stopped {
    #[error("a call took longer than {limit:?}")]
    TooSlow { limit: Duration },
    #[error("more than {limit} calls in {interval:?}")]
    TooLoud { limit: u32, interval: Duration },
    #[error("more than {limit} calls outstanding at once")]
    TooManyOutstanding { limit: u32 },
    #[error("the plugin process ended")]
    ProcessEnded,
}

/// Why a call was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Denied {
    #[error("the plugin was not granted {capability:?}")]
    NoCapability { capability: Capability },
    #[error("the plugin was not granted access to that terminal")]
    NotThisTerminal,
    #[error("the plugin has been stopped: {0}")]
    Stopped(Stopped),
}

/// What one plugin was granted, and its state.
pub struct Plugin {
    pub name: String,
    capabilities: BTreeSet<Capability>,
    /// Which terminals, by id. A capability with no object is a capability over everything.
    terminals: BTreeSet<String>,
    bounds: Bounds,
    outstanding: u32,
    calls_in_interval: u32,
    stopped: Option<Stopped>,
}

impl Plugin {
    pub fn new(name: impl Into<String>, bounds: Bounds) -> Self {
        Self {
            name: name.into(),
            capabilities: BTreeSet::new(),
            terminals: BTreeSet::new(),
            bounds,
            outstanding: 0,
            calls_in_interval: 0,
            stopped: None,
        }
    }

    /// Grant a capability. Only at load: there is no path that widens a grant at runtime,
    /// because a grant that can widen is a grant nobody can reason about after the fact.
    pub fn grant(&mut self, capability: Capability) -> &mut Self {
        self.capabilities.insert(capability);
        self
    }

    pub fn grant_terminal(&mut self, terminal: &TerminalId) -> &mut Self {
        self.terminals.insert(terminal.as_str().to_string());
        self
    }

    pub fn is_stopped(&self) -> Option<&Stopped> {
        self.stopped.as_ref()
    }

    /// Whether a call may proceed, and count it if it may.
    pub fn begin_call(
        &mut self,
        capability: Capability,
        terminal: Option<&TerminalId>,
    ) -> Result<(), Denied> {
        if let Some(why) = &self.stopped {
            return Err(Denied::Stopped(why.clone()));
        }
        if !self.capabilities.contains(&capability) {
            return Err(Denied::NoCapability { capability });
        }
        if let Some(terminal) = terminal
            && !self.terminals.contains(terminal.as_str())
        {
            return Err(Denied::NotThisTerminal);
        }
        if self.outstanding >= self.bounds.max_outstanding {
            let why = Stopped::TooManyOutstanding {
                limit: self.bounds.max_outstanding,
            };
            self.stopped = Some(why.clone());
            return Err(Denied::Stopped(why));
        }
        if self.calls_in_interval >= self.bounds.calls_per_interval {
            let why = Stopped::TooLoud {
                limit: self.bounds.calls_per_interval,
                interval: self.bounds.interval,
            };
            self.stopped = Some(why.clone());
            return Err(Denied::Stopped(why));
        }
        self.outstanding += 1;
        self.calls_in_interval += 1;
        Ok(())
    }

    /// Finish a call, with how long it took.
    pub fn end_call(&mut self, took: Duration) -> Result<(), Stopped> {
        self.outstanding = self.outstanding.saturating_sub(1);
        if took > self.bounds.call_deadline {
            let why = Stopped::TooSlow {
                limit: self.bounds.call_deadline,
            };
            self.stopped = Some(why.clone());
            return Err(why);
        }
        Ok(())
    }

    /// A new rate interval began.
    pub fn tick(&mut self) {
        self.calls_in_interval = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        let mut p = Plugin::new("linter", Bounds::default());
        p.grant(Capability::ReadTerminal)
            .grant_terminal(&TerminalId::new("1"));
        p
    }

    /// The absence is the mechanism: there is no write capability to grant.
    #[test]
    fn there_is_no_capability_that_writes_to_a_terminal() {
        // Enumerated through an exhaustive match, so adding a variant that writes fails to
        // compile here rather than being noticed later.
        for capability in [
            Capability::ReadTerminal,
            Capability::ReadHistory,
            Capability::ReadWorkspace,
            Capability::RaiseAttention,
        ] {
            match capability {
                Capability::ReadTerminal
                | Capability::ReadHistory
                | Capability::ReadWorkspace
                | Capability::RaiseAttention => {}
            }
            let name = format!("{capability:?}").to_lowercase();
            assert!(
                !name.contains("write") && !name.contains("input") && !name.contains("send"),
                "{capability:?} looks like a write capability"
            );
        }
    }

    #[test]
    fn a_capability_is_granted_per_terminal_and_not_in_general() {
        let mut p = plugin();
        assert_eq!(
            p.begin_call(Capability::ReadTerminal, Some(&TerminalId::new("1"))),
            Ok(())
        );
        assert_eq!(
            p.begin_call(Capability::ReadTerminal, Some(&TerminalId::new("2"))),
            Err(Denied::NotThisTerminal)
        );
    }

    #[test]
    fn a_capability_that_was_not_granted_is_refused() {
        let mut p = plugin();
        assert_eq!(
            p.begin_call(Capability::ReadHistory, Some(&TerminalId::new("1"))),
            Err(Denied::NoCapability {
                capability: Capability::ReadHistory
            })
        );
    }

    #[test]
    fn a_slow_call_stops_the_plugin_and_says_why() {
        let mut p = plugin();
        p.begin_call(Capability::ReadTerminal, Some(&TerminalId::new("1")))
            .expect("allowed");
        let stopped = p
            .end_call(Duration::from_secs(30))
            .expect_err("a slow call is a stop");
        assert!(matches!(stopped, Stopped::TooSlow { .. }));
        assert!(p.is_stopped().is_some());
        // And it stays stopped: no automatic restart, because a restart loop is worse.
        assert!(matches!(
            p.begin_call(Capability::ReadTerminal, Some(&TerminalId::new("1"))),
            Err(Denied::Stopped(_))
        ));
    }

    #[test]
    fn a_flood_stops_the_plugin() {
        let mut p = Plugin::new(
            "loud",
            Bounds {
                calls_per_interval: 3,
                ..Bounds::default()
            },
        );
        p.grant(Capability::ReadWorkspace);
        for _ in 0..3 {
            p.begin_call(Capability::ReadWorkspace, None)
                .expect("allowed");
            p.end_call(Duration::from_millis(1)).expect("fast");
        }
        assert!(matches!(
            p.begin_call(Capability::ReadWorkspace, None),
            Err(Denied::Stopped(Stopped::TooLoud { .. }))
        ));
    }

    #[test]
    fn too_many_calls_at_once_stops_the_plugin() {
        let mut p = Plugin::new(
            "greedy",
            Bounds {
                max_outstanding: 2,
                ..Bounds::default()
            },
        );
        p.grant(Capability::ReadWorkspace);
        p.begin_call(Capability::ReadWorkspace, None).expect("one");
        p.begin_call(Capability::ReadWorkspace, None).expect("two");
        assert!(matches!(
            p.begin_call(Capability::ReadWorkspace, None),
            Err(Denied::Stopped(Stopped::TooManyOutstanding { .. }))
        ));
    }

    /// A rate limit that never resets is a plugin that works once.
    #[test]
    fn the_rate_limit_resets_between_intervals() {
        let mut p = Plugin::new(
            "steady",
            Bounds {
                calls_per_interval: 2,
                ..Bounds::default()
            },
        );
        p.grant(Capability::ReadWorkspace);
        for _ in 0..2 {
            p.begin_call(Capability::ReadWorkspace, None)
                .expect("allowed");
            p.end_call(Duration::from_millis(1)).expect("fast");
        }
        p.tick();
        p.begin_call(Capability::ReadWorkspace, None)
            .expect("the next interval starts fresh");
    }
}

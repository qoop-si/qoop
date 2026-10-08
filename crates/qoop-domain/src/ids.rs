//! Typed identifiers.
//!
//! These are newtypes rather than aliases so that a workspace id cannot be passed where a
//! terminal id is expected. That is the cheapest half of pack invariants 1 through 3
//! (Agent is not Terminal, Workspace is not Machine, Setup is not Workspace): the compiler
//! enforces it and no reviewer has to.

use std::fmt;

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident, $prefix:literal) => {
        $(#[$m])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({}{})", stringify!($name), $prefix, self.0)
            }
        }
    };
}

id_type!(
    /// A compute node that can run the daemon. Machine identity is not workspace identity.
    MachineId, "machine:"
);
id_type!(
    /// Reusable launch intent. A Setup is not a Workspace.
    SetupId, "setup:"
);
id_type!(
    /// A runtime instance of a Setup. A workspace may move between machines.
    WorkspaceId, "ws:"
);
id_type!(
    /// A PTY-backed terminal surface. A Terminal is not an Agent.
    TerminalId, "term:"
);
id_type!(
    /// An agent process identity, stable across the process generations it goes through.
    AgentId, "agent:"
);
id_type!(TaskId, "task:");
id_type!(EventId, "event:");
id_type!(
    /// One client connection. Deliberately per connection, not per client program: size
    /// ownership keys on it and must end when the connection does (ADR-007).
    ClientId, "client:"
);

/// A process generation within one agent identity.
///
/// Invariant I5. This is a counter rather than a random id so that "older" is a decidable
/// question rather than a lookup, and it is `Ord` so the acceptance rule can be a
/// comparison instead of a convention.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct GenerationId(pub u64);

impl GenerationId {
    pub const FIRST: GenerationId = GenerationId(1);

    /// A new generation is minted when the terminal's foreground process is replaced, and
    /// when a daemon restart reconstructs a terminal, because a restart cannot prove the
    /// process is the same one.
    #[must_use]
    pub fn next(self) -> Self {
        GenerationId(self.0 + 1)
    }
}

impl fmt::Display for GenerationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gen:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_different_kinds_do_not_interchange() {
        // This test exists to document intent; the real enforcement is that the following
        // does not compile:
        //     let _: TerminalId = WorkspaceId::new("a");
        let w = WorkspaceId::new("a");
        let t = TerminalId::new("a");
        assert_eq!(w.as_str(), t.as_str());
        assert_ne!(w.to_string(), t.to_string());
    }

    #[test]
    fn generations_are_ordered_and_monotonic() {
        let g = GenerationId::FIRST;
        assert!(g.next() > g);
        assert!(g.next().next() > g.next());
    }
}

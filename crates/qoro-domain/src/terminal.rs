//! Terminal-side domain types.

use std::path::PathBuf;

/// The four paths a terminal has, kept separate because the pack says in terms not to
/// overload `cwd`, and because they fail independently: the foreground process cwd query
/// is the one that hangs on a dead mount, and the shell cwd must stay readable while it is
/// degraded (gate G2.8).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminalPaths {
    /// Where the shell currently is. Cheap to obtain and always available.
    pub shell_cwd: Option<PathBuf>,
    /// Where the foreground process is. Requires a syscall that can block on a dead mount,
    /// so it is `None` while degraded rather than stale or blocking.
    pub foreground_process_cwd: Option<PathBuf>,
    /// The workspace root, which does not change with the shell.
    pub workspace_root: Option<PathBuf>,
    /// The repository root, which may differ from the workspace root in a worktree.
    pub repo_root: Option<PathBuf>,
}

/// Terminal lifecycle. `Closing` is a real state rather than an absence, because teardown
/// is a multi-step state machine that a crash can interrupt (ADR-014).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TerminalPhase {
    Starting,
    Running,
    /// Write gate closed, containment set captured, escalation and reaping in progress.
    Closing,
    Closed,
}

impl TerminalPhase {
    /// Writes are accepted only while running. A closing terminal returns a typed error
    /// rather than silently dropping input.
    pub fn accepts_writes(self) -> bool {
        matches!(self, TerminalPhase::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_running_terminal_accepts_writes() {
        assert!(TerminalPhase::Running.accepts_writes());
        for p in [
            TerminalPhase::Starting,
            TerminalPhase::Closing,
            TerminalPhase::Closed,
        ] {
            assert!(!p.accepts_writes(), "{p:?} must not accept writes");
        }
    }

    #[test]
    fn a_degraded_foreground_cwd_does_not_hide_the_shell_cwd() {
        // Gate G2.8's property, expressed in the type: the two paths are independent, so a
        // hung mount that makes one unavailable cannot make the other unavailable too.
        let paths = TerminalPaths {
            shell_cwd: Some(PathBuf::from("/home/u/project")),
            foreground_process_cwd: None,
            ..Default::default()
        };
        assert!(paths.shell_cwd.is_some());
        assert!(paths.foreground_process_cwd.is_none());
    }
}

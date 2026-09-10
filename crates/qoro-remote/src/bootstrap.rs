//! Getting a daemon running on the other end.
//!
//! The hard part is not the transport, it is that the other end's login shell is somebody
//! else's. It may be `fish`, which does not understand `VAR=value command`. It may print a
//! banner. It may be `zsh` with a framework that writes to stdout on startup. Anything that
//! reads the shell's output without a marker is reading somebody's message of the day.

use crate::transport::{RemoteTransport, TransportError};
use crate::{Compatibility, RemoteError, RemoteHello, compatibility};

/// The login shells worth telling apart.
///
/// Not for cosmetics: `fish` has different syntax for the two things bootstrap needs, so a
/// command written for `sh` runs and fails in a way that looks like the daemon is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteShell {
    /// `sh`, `bash`, `zsh`, `ksh` and anything else that takes `VAR=value cmd` and `$( )`.
    PosixLike,
    Fish,
    /// Something we did not recognise. Treated as posix-like, and said so.
    Unknown,
}

impl RemoteShell {
    /// Set one variable for one command, in this shell's syntax.
    ///
    /// `fish` has no `VAR=value cmd` form at all. A command written for `sh` and run under
    /// `fish` fails with "Unsupported use of '='", which is a message nobody reading "the
    /// daemon did not start" would connect to their shell.
    pub fn with_env(self, name: &str, value: &str, command: &str) -> String {
        match self {
            RemoteShell::Fish => format!("env {name}={value} {command}"),
            _ => format!("{name}={value} {command}"),
        }
    }

    /// Redirect stderr away, in this shell's syntax. `fish` does not accept `2>&1`.
    pub fn discard_stderr(self, command: &str) -> String {
        match self {
            RemoteShell::Fish => format!("{command} 2>/dev/null"),
            _ => format!("{command} 2>/dev/null"),
        }
    }
}

/// A marker around whatever the remote prints.
///
/// Everything the bootstrap reads is between two markers it chose, because a login shell
/// prints a banner, a framework prints its startup, and a corporate profile prints a legal
/// notice. Reading the first line of output is reading somebody's message of the day.
pub const MARKER_BEGIN: &str = "-----qoro-begin-----";
pub const MARKER_END: &str = "-----qoro-end-----";

/// Wrap a command so its output can be found in whatever else the shell says.
pub fn marker_command(command: &str) -> String {
    format!("echo {MARKER_BEGIN}; {command}; echo {MARKER_END}")
}

/// What was between the markers, or nothing if they were not both there.
pub fn between_markers(text: &str) -> Option<String> {
    let start = text.find(MARKER_BEGIN)? + MARKER_BEGIN.len();
    let rest = &text[start..];
    let end = rest.find(MARKER_END)?;
    Some(rest[..end].trim().to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error(
        "the remote produced no marked output; its shell may have failed before running anything"
    )]
    NoMarkedOutput,
    #[error("transport: {0}")]
    Transport(String),
}

impl From<TransportError> for BootstrapError {
    fn from(e: TransportError) -> Self {
        BootstrapError::Transport(e.to_string())
    }
}

/// Bringing a remote up, one step at a time so each step can be asserted.
pub struct Bootstrap<'a, T: RemoteTransport> {
    transport: &'a T,
    /// The path to the daemon on the remote. Absolute for the same reason a Setup's
    /// executable is: a relative one resolves against whatever directory the login shell
    /// happened to land in.
    daemon_path: String,
}

impl<'a, T: RemoteTransport> Bootstrap<'a, T> {
    pub fn new(transport: &'a T, daemon_path: impl Into<String>) -> Self {
        Self {
            transport,
            daemon_path: daemon_path.into(),
        }
    }

    /// Which shell the remote logs into.
    ///
    /// Asked of the shell itself rather than read from `/etc/passwd`, because what matters
    /// is the shell that will run the bootstrap commands, and that is the one ssh starts.
    pub fn detect_shell(&self) -> Result<RemoteShell, BootstrapError> {
        // `$0` is the shell's own name in every posix-like shell, and in fish it is the
        // shell's name too. A shell that does not answer at all is unknown.
        let out = self
            .transport
            .run(&marker_command("echo \"$0\"; echo $FISH_VERSION"))?;
        let Some(answer) = between_markers(&out.stdout) else {
            return Err(BootstrapError::NoMarkedOutput);
        };
        let lowered = answer.to_lowercase();
        if lowered.contains("fish") {
            return Ok(RemoteShell::Fish);
        }
        for name in ["bash", "zsh", "ksh", "dash", "sh"] {
            if lowered.contains(name) {
                return Ok(RemoteShell::PosixLike);
            }
        }
        Ok(RemoteShell::Unknown)
    }

    /// Ask the remote daemon what it is.
    pub fn hello(&self) -> Result<RemoteHello, BootstrapError> {
        let out = self
            .transport
            .run(&marker_command(&format!("{} --hello", self.daemon_path)))?;
        let Some(answer) = between_markers(&out.stdout) else {
            // Nothing between the markers means the command produced nothing, which for a
            // missing binary is what a shell does after printing to stderr.
            if out.stdout.contains(MARKER_BEGIN) {
                return Err(RemoteError::NotInstalled.into());
            }
            return Err(BootstrapError::NoMarkedOutput);
        };
        if answer.is_empty() {
            return Err(RemoteError::NotInstalled.into());
        }
        serde_json::from_str(&answer)
            .map_err(|_| BootstrapError::from(RemoteError::NotADaemon(answer)))
    }

    /// The whole decision: is this remote usable as it is?
    pub fn plan(&self) -> Result<(RemoteShell, Compatibility), BootstrapError> {
        let shell = self.detect_shell()?;
        let hello = self.hello()?;
        let compat = compatibility(&hello)?;
        Ok((shell, compat))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_read_between_markers_and_not_from_the_first_line() {
        let noisy = format!(
            "Welcome to Example Corp. Unauthorised use is prohibited.\n\
             Last login: Tue\n{MARKER_BEGIN}\nthe answer\n{MARKER_END}\nbye\n"
        );
        assert_eq!(between_markers(&noisy).as_deref(), Some("the answer"));
    }

    #[test]
    fn missing_markers_are_not_mistaken_for_an_empty_answer() {
        assert_eq!(between_markers("Welcome. Last login: Tue\n"), None);
        assert_eq!(between_markers(&format!("{MARKER_BEGIN}\nhalf")), None);
    }

    /// `fish` has no `VAR=value cmd` form. A command written for `sh` fails under it with a
    /// message nobody would connect to their shell.
    #[test]
    fn setting_a_variable_uses_the_shells_own_syntax() {
        assert_eq!(
            RemoteShell::PosixLike.with_env("A", "1", "qorod"),
            "A=1 qorod"
        );
        assert_eq!(
            RemoteShell::Fish.with_env("A", "1", "qorod"),
            "env A=1 qorod"
        );
    }

    #[test]
    fn an_unknown_shell_is_treated_as_posix_like_rather_than_refused() {
        assert_eq!(
            RemoteShell::Unknown.with_env("A", "1", "qorod"),
            "A=1 qorod"
        );
    }
}

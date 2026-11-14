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
    ///
    /// The value is quoted, so it is one word of data and never shell text, in fish too.
    pub fn with_env(self, name: &str, value: &str, command: &str) -> String {
        let value = crate::saved::shell_quote(value);
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
// Fixed wire label kept from the old name: crosses the link to remotes of any version.
pub const MARKER_BEGIN: &str = "-----qoro-begin-----";
// Fixed wire label kept from the old name: crosses the link to remotes of any version.
pub const MARKER_END: &str = "-----qoro-end-----";

/// Wrap a command so its output can be found in whatever else the shell says.
pub fn marker_command(command: &str) -> String {
    format!("echo {MARKER_BEGIN}; {command}; echo {MARKER_END}")
}

/// The shell that answered `echo "$0"; echo "fish:$FISH_VERSION"`.
///
/// Only fish sets `FISH_VERSION`, so a version on the tagged line is fish: fish has no `$0`,
/// and its name line is empty. `$0` is the shell's own name in every posix-like shell. A
/// shell that names neither is unknown.
fn shell_from(answer: &str) -> RemoteShell {
    let tagged = |line: &&str| line.trim_start().starts_with("fish:");
    if answer
        .lines()
        .filter(tagged)
        .any(|line| !line.trim_start()["fish:".len()..].trim().is_empty())
    {
        return RemoteShell::Fish;
    }
    let names = answer
        .lines()
        .filter(|line| !tagged(line))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    if names.contains("fish") {
        return RemoteShell::Fish;
    }
    if ["bash", "zsh", "ksh", "dash", "sh"]
        .iter()
        .any(|name| names.contains(name))
    {
        return RemoteShell::PosixLike;
    }
    RemoteShell::Unknown
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
        let out = self
            .transport
            .run(&marker_command("echo \"$0\"; echo \"fish:$FISH_VERSION\""))?;
        let Some(answer) = between_markers(&out.stdout) else {
            return Err(BootstrapError::NoMarkedOutput);
        };
        Ok(shell_from(&answer))
    }

    /// Ask the remote daemon what it is.
    pub fn hello(&self) -> Result<RemoteHello, BootstrapError> {
        let out = self.transport.run(&marker_command(&format!(
            "{} --hello",
            crate::saved::shell_quote(&self.daemon_path)
        )))?;
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
    /// fish answers with its version and no name, the posix shells with their name and an
    /// empty version: each is told apart, and a stranger is unknown.
    #[test]
    fn a_shell_is_told_by_its_answer() {
        assert_eq!(shell_from("\nfish:4.0.2"), RemoteShell::Fish);
        assert_eq!(shell_from("fish\nfish:3.7.1"), RemoteShell::Fish);
        assert_eq!(shell_from("-bash\nfish:"), RemoteShell::PosixLike);
        assert_eq!(shell_from("/bin/zsh\nfish:"), RemoteShell::PosixLike);
        assert_eq!(shell_from("dash\nfish:"), RemoteShell::PosixLike);
        assert_eq!(shell_from("nu\nfish:"), RemoteShell::Unknown);
    }

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
            RemoteShell::PosixLike.with_env("A", "1", "qoopd"),
            "A=1 qoopd"
        );
        assert_eq!(
            RemoteShell::Fish.with_env("A", "1", "qoopd"),
            "env A=1 qoopd"
        );
    }

    /// A value is one word of data, never shell text.
    #[test]
    fn a_value_is_quoted_so_it_runs_nothing() {
        assert_eq!(
            RemoteShell::PosixLike.with_env("A", "1; touch x", "qoopd"),
            "A='1; touch x' qoopd"
        );
        assert_eq!(
            RemoteShell::Fish.with_env("A", "it's", "qoopd"),
            r"env A='it'\''s' qoopd"
        );
        // Inside quotes fish reads `\'`: a backslash before the quote is kept out of them.
        assert_eq!(
            RemoteShell::Fish.with_env("A", r"a\'; touch x", "qoopd"),
            r"env A='a'\\\''; touch x' qoopd"
        );
    }

    #[test]
    fn an_unknown_shell_is_treated_as_posix_like_rather_than_refused() {
        assert_eq!(
            RemoteShell::Unknown.with_env("A", "1", "qoopd"),
            "A=1 qoopd"
        );
    }

    /// What a remote that still runs 0.7.x prints back: the markers are the old strings, so a
    /// Mac of this build reads the answer of a machine it has not updated yet.
    #[test]
    fn an_old_remotes_output_is_read_between_the_old_markers() {
        let old =
            "Last login: Tue\n-----qoro-begin-----\n{\"version\":\"0.7.0\"}\n-----qoro-end-----\n";
        assert_eq!(
            between_markers(old).as_deref(),
            Some("{\"version\":\"0.7.0\"}")
        );
        assert_eq!(MARKER_BEGIN, "-----qoro-begin-----");
        assert_eq!(MARKER_END, "-----qoro-end-----");
    }
}

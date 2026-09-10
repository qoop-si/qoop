//! The seam. Everything above this sees a way to run a command somewhere and get its output.
//!
//! Two implementations here: `SshTransport`, which is the product, and `LocalTransport`,
//! which runs the same commands through the same shells on this machine. The second is not
//! a mock. It is a real transport that reaches a real login shell, and it is how the
//! bootstrap's shell handling is tested against `bash` and `zsh` without a container: the
//! part it does not exercise is the ssh argument construction, which is asserted directly.

use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("could not reach the remote: {0}")]
    Unreachable(String),
    #[error("the command failed with status {status}: {stderr}")]
    Failed { status: i32, stderr: String },
}

/// One command, run somewhere, with its output.
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

/// A way to run a command on a machine.
pub trait RemoteTransport {
    /// Run through the remote's login shell, which is the whole point: a login shell is
    /// what a person's environment lives in, and a command run without one is a command
    /// run in an environment nobody has.
    fn run(&self, command: &str) -> Result<Output, TransportError>;

    /// A name for messages. Never parsed.
    fn describe(&self) -> String;
}

/// A transport that runs through a login shell on this machine.
pub struct LocalTransport {
    shell: String,
}

impl LocalTransport {
    pub fn new(shell: impl Into<String>) -> Self {
        Self {
            shell: shell.into(),
        }
    }
}

impl RemoteTransport for LocalTransport {
    fn run(&self, command: &str) -> Result<Output, TransportError> {
        // `-l` for a login shell, which is what an ssh command lands in and what makes the
        // person's own environment apply. Without it the test would exercise a shell nobody
        // ever has.
        let out = Command::new(&self.shell)
            .args(["-l", "-c", command])
            .output()
            .map_err(|e| TransportError::Unreachable(e.to_string()))?;
        Ok(Output {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status: out.status.code().unwrap_or(-1),
        })
    }

    fn describe(&self) -> String {
        format!("local {}", self.shell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_shell_runs_a_command_and_returns_its_output() {
        let t = LocalTransport::new("/bin/sh");
        let out = t.run("echo hello-$((6*7))").expect("run");
        assert_eq!(out.stdout.trim(), "hello-42");
        assert_eq!(out.status, 0);
    }

    #[test]
    fn a_failing_command_reports_its_status_rather_than_looking_like_success() {
        let t = LocalTransport::new("/bin/sh");
        let out = t.run("exit 7").expect("run");
        assert_eq!(out.status, 7);
    }
}

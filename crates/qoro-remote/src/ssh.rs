//! The system `ssh`, with the arguments that make it behave.
//!
//! Every one of these is a decision rather than a default, and each is here because the
//! default is wrong for a long lived connection carrying a terminal.

use std::process::Command;

use crate::transport::{Output, RemoteTransport, TransportError};

/// How often to ask whether the connection is still there, and how many misses to allow.
///
/// A terminal connection is idle most of the time, and a NAT or a firewall that drops idle
/// connections silently leaves both ends believing they are connected. These are set only
/// when the user has not set them: an explicit setting in the user's own config is a
/// decision they made, and overriding it would be this program deciding it knows better
/// about their network.
///
/// Five seconds and three misses: a network that went away is noticed in fifteen, which is
/// how soon a remote space reads as offline and starts being retried, and three small
/// packets a quarter of a minute cost nothing a person would measure.
pub const KEEPALIVE_SECONDS: u32 = 5;
pub const KEEPALIVE_MISSES: u32 = 3;

pub struct SshTransport {
    host: String,
    /// One authenticated connection, reused. A new `ssh` per local connection means a new
    /// authentication per attach, which on a hardware key means a touch per attach.
    control_path: Option<String>,
    /// What the user has already set, so it is not overridden.
    user_set: Vec<String>,
    /// Never prompt. A connection made in the background has nobody to answer a password
    /// or a host key question, and a prompt nobody sees is a connection that hangs.
    batch: bool,
    /// Take a host key ssh has never seen, as typing `yes` at its question would. A key
    /// that changed is still refused: that is the case the question exists for.
    accept_new: bool,
}

impl SshTransport {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            control_path: None,
            user_set: Vec::new(),
            batch: false,
            accept_new: false,
        }
    }

    /// Fail rather than ask: for a connection nobody is watching.
    pub fn batch(mut self) -> Self {
        self.batch = true;
        self
    }

    /// Trust a host on first contact, for a connection a person just asked for by address.
    pub fn accept_new_host(mut self) -> Self {
        self.accept_new = true;
        self
    }

    /// Reuse one authenticated connection through a control socket.
    pub fn with_control_path(mut self, path: impl Into<String>) -> Self {
        self.control_path = Some(path.into());
        self
    }

    /// Options the user has already set in their own config, by name.
    pub fn with_user_set(mut self, names: Vec<String>) -> Self {
        self.user_set = names;
        self
    }

    /// The arguments this transport would run, so they can be asserted without a network.
    ///
    /// The user's config is included first and is never disabled: their jump hosts, keys,
    /// `Match` blocks and agent forwarding are what make their remote reachable at all, and
    /// a transport that started from a clean slate would work only for the simple cases.
    pub fn argv(&self, command: &str) -> Vec<String> {
        let mut argv = vec!["ssh".to_string()];
        if let Some(path) = &self.control_path {
            argv.push("-o".into());
            argv.push("ControlMaster=auto".into());
            argv.push("-o".into());
            argv.push(format!("ControlPath={path}"));
            argv.push("-o".into());
            argv.push("ControlPersist=600".into());
        }
        if !self.user_set.iter().any(|n| n == "ServerAliveInterval") {
            argv.push("-o".into());
            argv.push(format!("ServerAliveInterval={KEEPALIVE_SECONDS}"));
        }
        if !self.user_set.iter().any(|n| n == "ServerAliveCountMax") {
            argv.push("-o".into());
            argv.push(format!("ServerAliveCountMax={KEEPALIVE_MISSES}"));
        }
        if self.batch {
            argv.push("-o".into());
            argv.push("BatchMode=yes".into());
            argv.push("-o".into());
            argv.push("ConnectTimeout=10".into());
        }
        if self.accept_new {
            argv.push("-o".into());
            argv.push("StrictHostKeyChecking=accept-new".into());
        }
        // No pseudo terminal: this transport carries commands, and the terminal is carried
        // by the protocol inside it. Asking ssh for a tty here would put a second line
        // discipline between the child and us.
        argv.push("-T".into());
        // Options end here, so a host that starts with a dash is still a host. Targets
        // loaded from `remotes.json` reach this unvalidated, and `-oProxyCommand=...` as a
        // host would run a command on this machine.
        argv.push("--".into());
        argv.push(self.host.clone());
        argv.push(command.to_string());
        argv
    }
}

impl RemoteTransport for SshTransport {
    fn run(&self, command: &str) -> Result<Output, TransportError> {
        let argv = self.argv(command);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .map_err(|e| TransportError::Unreachable(e.to_string()))?;
        Ok(Output {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            status: out.status.code().unwrap_or(-1),
        })
    }

    fn describe(&self) -> String {
        format!("ssh {}", self.host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalives_are_set_and_the_users_own_settings_are_not_overridden() {
        let plain = SshTransport::new("host").argv("true");
        assert!(plain.iter().any(|a| a.starts_with("ServerAliveInterval=")));
        assert!(plain.iter().any(|a| a.starts_with("ServerAliveCountMax=")));

        let theirs = SshTransport::new("host")
            .with_user_set(vec!["ServerAliveInterval".into()])
            .argv("true");
        assert!(
            !theirs.iter().any(|a| a.starts_with("ServerAliveInterval=")),
            "an explicit user setting was overridden"
        );
        assert!(
            theirs.iter().any(|a| a.starts_with("ServerAliveCountMax=")),
            "the setting the user did not make was dropped too"
        );
    }

    #[test]
    fn one_connection_is_reused_when_a_control_path_is_given() {
        let argv = SshTransport::new("host")
            .with_control_path("/tmp/qoro-%r@%h:%p")
            .argv("true");
        assert!(argv.iter().any(|a| a == "ControlMaster=auto"));
        assert!(argv.iter().any(|a| a.starts_with("ControlPath=")));
        assert!(argv.iter().any(|a| a.starts_with("ControlPersist=")));
    }

    /// No pseudo terminal. The terminal is carried by the protocol inside the connection,
    /// and a tty here would put a second line discipline between the child and us.
    #[test]
    fn the_transport_does_not_ask_for_a_terminal() {
        let argv = SshTransport::new("host").argv("true");
        assert!(argv.contains(&"-T".to_string()));
        assert!(!argv.contains(&"-t".to_string()));
    }

    /// The user's own config is never disabled.
    #[test]
    fn the_users_config_is_not_bypassed() {
        let argv = SshTransport::new("host").argv("true");
        for hostile in ["-F", "/dev/null", "-o", "IdentitiesOnly=yes"] {
            if hostile == "-o" {
                continue;
            }
            assert!(
                !argv.contains(&hostile.to_string()),
                "the transport bypasses the user's config with {hostile}"
            );
        }
    }

    #[test]
    fn a_batch_connection_never_prompts_and_gives_up_on_a_dead_host() {
        let quiet = SshTransport::new("host").batch().argv("true");
        assert!(quiet.iter().any(|a| a == "BatchMode=yes"));
        assert!(quiet.iter().any(|a| a.starts_with("ConnectTimeout=")));
        let asked = SshTransport::new("host").argv("true");
        assert!(!asked.iter().any(|a| a == "BatchMode=yes"));
    }

    /// First contact takes an unknown key and nothing else: never `no`, which would also
    /// take a changed one.
    #[test]
    fn only_an_asked_for_connection_trusts_a_new_host_key() {
        let first = SshTransport::new("host")
            .batch()
            .accept_new_host()
            .argv("true");
        assert!(
            first
                .iter()
                .any(|a| a == "StrictHostKeyChecking=accept-new")
        );
        assert!(!first.iter().any(|a| a == "StrictHostKeyChecking=no"));
        let later = SshTransport::new("host").batch().argv("true");
        assert!(!later.iter().any(|a| a.starts_with("StrictHostKeyChecking")));
    }

    #[test]
    fn the_command_is_the_last_argument_and_the_host_is_before_it() {
        let argv = SshTransport::new("myhost").argv("echo hi");
        let host = argv.iter().position(|a| a == "myhost").expect("host");
        assert_eq!(argv.last().expect("command"), "echo hi");
        assert_eq!(host, argv.len() - 2);
        assert_eq!(argv[host - 1], "--", "options must end before the host");
    }

    /// A host read from `remotes.json` reaches here unvalidated, and one that starts with
    /// a dash would otherwise be an ssh option: `-oProxyCommand=...` runs a command.
    #[test]
    fn a_host_that_looks_like_an_option_is_still_a_host() {
        let argv = SshTransport::new("-oProxyCommand=x").argv("true");
        let host = argv
            .iter()
            .position(|a| a == "-oProxyCommand=x")
            .expect("host");
        assert_eq!(argv[host - 1], "--");
    }
}

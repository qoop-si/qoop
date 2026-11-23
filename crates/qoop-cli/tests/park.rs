//! `qoop park` against a live daemon, through the real socket: what the command says without a
//! terminal, in a terminal that is no agent, and when it is taken back. The positive path needs
//! an agent identity, which only a hook with the terminal's private token gives, so it lives in
//! `qoop-runtime`'s `tests/park.rs` and the registry tests.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self { child, dir }
    }

    fn qoop(&self, args: &[&str], terminal: Option<u32>) -> (bool, String) {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL");
        if let Some(terminal) = terminal {
            cmd.env("QOOP_TERMINAL", terminal.to_string());
        }
        let out = cmd.output().expect("run qoop");
        let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        (out.status.success(), text)
    }

    fn first_terminal(&self) -> u32 {
        let (_, json) = self.qoop(&["agents", "--json"], None);
        let rows: serde_json::Value = serde_json::from_str(&json).expect("the agents as JSON");
        rows[0]["terminal"].as_u64().expect("a terminal") as u32
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn without_a_terminal_there_is_one_plain_line() {
    let daemon = Daemon::start();
    let (ok, text) = daemon.qoop(&["park"], None);
    assert!(!ok);
    assert!(text.contains("QOOP_TERMINAL is not set"), "{text}");
}

#[test]
fn a_shell_is_not_parked_and_taking_a_park_back_is_never_refused() {
    let daemon = Daemon::start();
    let terminal = daemon.first_terminal();
    let (ok, text) = daemon.qoop(&["park"], Some(terminal));
    assert!(!ok);
    assert!(text.contains("only an agent can be saved"), "{text}");
    let (ok, text) = daemon.qoop(&["park", "--off"], Some(terminal));
    assert!(ok, "{text}");
    assert!(text.contains("no longer parked"), "{text}");
    let (_, json) = daemon.qoop(&["agents", "--json"], None);
    assert!(!json.contains("\"parked\":true"), "{json}");
}

#[test]
fn an_unknown_flag_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, text) = daemon.qoop(&["park", "--now"], Some(1));
    assert!(!ok);
    assert!(text.contains("usage: qoop park [--off]"), "{text}");
}

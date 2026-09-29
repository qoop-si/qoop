//! Session-to-session messaging through the real socket.
//!
//! Two separate `qoro` invocations talk to one live daemon: one writes into a
//! terminal it is not attached to, the other reads the screen back. That is
//! the whole feature: an addressed terminal-channel input with a name, an
//! existence check and a prompt variant that refuses a blocked session.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
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
        let child = Command::new(env!("CARGO_BIN_EXE_qoro"))
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoro_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoro_testkit::open_first_terminal(&socket);
        // The first shell needs a moment to print its prompt.
        std::thread::sleep(Duration::from_millis(500));
        Self { child, dir }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_qoro"))
            .args(args)
            .env("QORO_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .output()
            .expect("run qoro");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn read(&self, terminal: &str) -> Option<String> {
        let (ok, stdout, _) = self.run(&["read", terminal]);
        ok.then_some(stdout)
    }

    /// Poll `qoro read` until the marker shows up on the target's screen.
    fn wait_for_screen(&self, terminal: &str, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(screen) = self.read(terminal)
                && screen.contains(needle)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoro daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn send_reaches_a_session_the_sender_is_not_attached_to() {
    let daemon = Daemon::start();
    let (ok, stdout, stderr) = daemon.run(&["send", "1", "echo SEND_MARKER_42", "--enter"]);
    assert!(ok, "send failed: {stdout:?} {stderr:?}");
    assert!(
        daemon.wait_for_screen("1", "SEND_MARKER_42"),
        "the text never reached terminal 1"
    );
}

#[test]
fn prompt_sends_text_plus_enter() {
    let daemon = Daemon::start();
    let (ok, stdout, stderr) = daemon.run(&["prompt", "1", "echo PROMPT_MARKER_7"]);
    assert!(ok, "prompt failed: {stdout:?} {stderr:?}");
    assert!(
        stdout.contains("prompted terminal 1"),
        "no confirmation: {stdout:?}"
    );
    assert!(
        daemon.wait_for_screen("1", "PROMPT_MARKER_7"),
        "the prompt never ran on terminal 1"
    );
}

#[test]
fn prompt_resolves_a_terminal_name_from_the_index() {
    let daemon = Daemon::start();
    // Terminal 1 exists; giving its number as the target must resolve the row.
    let (ok, _, stderr) = daemon.run(&["prompt", "1", "echo NAMED_OK_3"]);
    assert!(ok, "numeric target must resolve: {stderr:?}");
    assert!(daemon.wait_for_screen("1", "NAMED_OK_3"));
}

#[test]
fn prompt_refuses_an_empty_text() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["prompt", "1"]);
    assert!(!ok, "an empty prompt must not send");
    assert!(
        stderr.contains("empty"),
        "the refusal names the problem: {stderr:?}"
    );
}

#[test]
fn prompt_refuses_an_unknown_target() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["prompt", "no-such-agent-xyz", "hi"]);
    assert!(!ok, "an unknown target must not send");
    assert!(
        stderr.contains("no such agent or terminal"),
        "the refusal names the target: {stderr:?}"
    );
}

#[test]
fn send_refuses_an_unknown_terminal() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["send", "99", "hi"]);
    assert!(!ok, "a missing terminal must not accept input");
    assert!(
        stderr.contains("no such terminal"),
        "the refusal names the terminal: {stderr:?}"
    );
}

#[test]
fn send_refuses_empty_input() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["send", "1"]);
    assert!(!ok, "empty input must not send");
    assert!(
        stderr.contains("empty"),
        "the refusal names the problem: {stderr:?}"
    );
}

#[test]
fn read_refuses_an_unknown_terminal() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["read", "99"]);
    assert!(!ok, "a missing terminal has no screen");
    assert!(
        stderr.contains("no such terminal"),
        "the refusal names the terminal: {stderr:?}"
    );
}

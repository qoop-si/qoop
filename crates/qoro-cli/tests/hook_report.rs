//! Phase B: `qoro hook-report` against a live daemon, through the real socket.
//!
//! What this covers is the wire path: the CLI's arg parsing, the framed
//! control request, the daemon's parse-and-admit arm, and the refusal answers.
//! The positive path, a real token driving a real state change, lives one
//! layer down in `qoro-runtime`'s registry tests, because the token never
//! leaves the daemon and no socket test can honestly hold one.

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
        Self { child, dir }
    }

    fn report(&self, args: &[&str], clear_qoro_env: bool) -> (bool, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        cmd.arg("hook-report");
        cmd.args(args);
        cmd.env("QORO_RUNTIME_DIR", self.dir.path());
        if clear_qoro_env {
            cmd.env_remove("QORO_TERMINAL");
            cmd.env_remove("QORO_TOKEN");
        }
        let out = cmd.output().expect("run hook-report");
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        stderr.push_str(&String::from_utf8_lossy(&out.stdout));
        (out.status.success(), stderr)
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
fn a_forged_token_is_refused_through_the_socket() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &["--state", "working", "--terminal", "1", "--token", "forged"],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

#[test]
fn an_unknown_state_is_rejected_before_anything_is_admitted() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &["--state", "busy", "--terminal", "1", "--token", "forged"],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("unknown agent state"),
        "the parse error names the state: {err:?}"
    );
}

#[test]
fn without_a_terminal_or_a_token_there_is_nothing_to_admit() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(&["--state", "working"], true);
    assert!(!ok);
    assert!(
        err.contains("no terminal") || err.contains("no token"),
        "the usage error says what is missing: {err:?}"
    );
}

#[test]
fn a_session_only_report_reaches_the_daemon_instead_of_the_usage_error() {
    // A forged token is refused by the daemon, which proves the session-only
    // path built a real request: a usage error would fail differently.
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--session-id",
            "solo-456",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

#[test]
fn neither_state_nor_session_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(&["--terminal", "1", "--token", "forged"], false);
    assert!(!ok);
    assert!(
        err.contains("usage"),
        "missing both halves names the usage: {err:?}"
    );
}

#[test]
fn an_empty_session_id_is_refused_not_recorded_as_silence() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--terminal",
            "1",
            "--token",
            "forged",
            "--session-id",
            "   ",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("usage"),
        "an empty id is a caller error, not a report: {err:?}"
    );
}

#[test]
fn an_unknown_event_is_rejected_before_anything_is_admitted() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--event",
            "started",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("unknown focus event"),
        "the parse error names the event: {err:?}"
    );
}

#[test]
fn a_label_without_an_event_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--label",
            "Read foo.rs",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("--label needs --event"),
        "a label alone names the usage: {err:?}"
    );
}

#[test]
fn an_event_on_the_session_only_path_is_a_usage_error() {
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--session-id",
            "solo-456",
            "--event",
            "tool",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok);
    assert!(
        err.contains("--event/--label need --state"),
        "an event without a state names the usage: {err:?}"
    );
}

#[test]
fn an_event_report_reaches_the_daemon_instead_of_the_usage_error() {
    // A forged token is refused by the daemon, which proves the event path
    // built a real request: a usage error would fail differently.
    let daemon = Daemon::start();
    let (ok, err) = daemon.report(
        &[
            "--state",
            "working",
            "--event",
            "tool",
            "--label",
            "Read foo.rs",
            "--terminal",
            "1",
            "--token",
            "forged",
        ],
        false,
    );
    assert!(!ok, "a forgery must not report");
    assert!(err.contains("refused"), "the refusal names itself: {err:?}");
}

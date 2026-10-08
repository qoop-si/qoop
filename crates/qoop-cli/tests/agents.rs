//! `qoop agents` against a live daemon, through the real socket.
//!
//! The read-only index: one `ListAgents` round trip, one row per terminal,
//! the same rows as text and as JSON.

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
        let child = Command::new(env!("CARGO_BIN_EXE_qoop"))
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
        std::thread::sleep(Duration::from_millis(500));
        Self { child, dir }
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .output()
            .expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
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
fn agents_lists_the_first_terminal() {
    let daemon = Daemon::start();
    let (ok, stdout, _) = daemon.run(&["agents"]);
    assert!(ok, "qoop agents failed");
    assert!(
        stdout.contains("term=1"),
        "the first terminal is missing: {stdout:?}"
    );
    assert!(stdout.contains("state="), "no state column: {stdout:?}");
}

#[test]
fn agents_json_is_the_same_rows_as_an_array() {
    let daemon = Daemon::start();
    let (ok, stdout, _) = daemon.run(&["agents", "--json"]);
    assert!(ok, "qoop agents --json failed");
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("the json parses");
    assert!(!rows.is_empty(), "no terminals listed");
    let first = &rows[0];
    assert_eq!(first["terminal"], 1);
    assert!(first["state"].is_string(), "no state field: {first:?}");
}

#[test]
fn agents_text_and_json_agree_on_the_terminals() {
    let daemon = Daemon::start();
    let (ok, text, _) = daemon.run(&["agents"]);
    assert!(ok);
    let (ok, stdout, _) = daemon.run(&["agents", "--json"]);
    assert!(ok);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(stdout.trim()).expect("the json parses");
    for row in &rows {
        let term = row["terminal"].as_u64().expect("a terminal number");
        assert!(
            text.contains(&format!("term={term}")),
            "terminal {term} is in the json but not the text"
        );
    }
}

#[test]
fn agents_refuses_an_unknown_flag() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["agents", "--verbose"]);
    assert!(!ok, "an unknown flag was accepted");
    assert!(stderr.contains("unknown flag"), "no usage hint: {stderr:?}");
}

/// No daemon is nothing awake, and that is an answer: an empty list, not an error.
#[test]
fn agents_without_a_daemon_answers_an_empty_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    for (args, said) in [(vec!["agents"], ""), (vec!["agents", "--json"], "[]\n")] {
        let out = Command::new(env!("CARGO_BIN_EXE_qoop"))
            .args(&args)
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .output()
            .expect("run qoop");
        assert!(out.status.success(), "{args:?} with no daemon failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), said, "{args:?}");
    }
}

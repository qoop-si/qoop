//! What a supervising agent drives through the real socket: `wait`, `new`,
//! `close`, `send --key` and the `--generation` refusal.
//!
//! Every run clears the `QORO_*` variables it inherited, so a test started
//! from inside a qoro terminal neither reaches that terminal's session nor
//! reads as running in it.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const INHERITED: &[&str] = &["QORO_TERMINAL", "QORO_TOKEN", "QORO_SESSION"];

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        cmd.arg("daemon")
            .env("QORO_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for var in INHERITED {
            cmd.env_remove(var);
        }
        let child = cmd.spawn().expect("spawn the daemon");
        let socket = qoro_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        let daemon = Self { child, dir };
        assert!(
            daemon.wait_for_screen("1", "$"),
            "the first shell never drew a prompt"
        );
        daemon
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        cmd.args(args)
            .env("QORO_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"));
        for var in INHERITED {
            cmd.env_remove(var);
        }
        cmd
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run qoro")
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.output(args);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn wait_for_screen(&self, terminal: &str, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let (ok, screen, _) = self.run(&["read", terminal]);
            if ok && screen.contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        let (ok, stdout, stderr) = self.run(&["agents", "--json"]);
        assert!(ok, "agents failed: {stderr}");
        serde_json::from_str(&stdout).expect("agents --json parses")
    }

    fn row(&self, terminal: u64) -> Option<serde_json::Value> {
        self.rows()
            .into_iter()
            .find(|r| r["terminal"].as_u64() == Some(terminal))
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
fn wait_returns_at_once_when_the_target_is_already_there() {
    let daemon = Daemon::start();
    let state = daemon.row(1).expect("terminal 1")["state"]
        .as_str()
        .unwrap()
        .to_string();
    let started = Instant::now();
    let (ok, stdout, stderr) = daemon.run(&["wait", "1", "--until", &state, "--timeout", "5"]);
    assert!(ok, "wait failed: {stderr}");
    assert!(stdout.contains("term=1 "), "{stdout}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );

    // The same rows `agents --json` prints, as an array.
    let (ok, stdout, stderr) = daemon.run(&["wait", "1", "--until", &state, "--json"]);
    assert!(ok, "wait --json failed: {stderr}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).expect("an array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["terminal"], 1);
    assert_eq!(rows[0]["state"], state.as_str());
}

#[test]
fn wait_wakes_on_a_state_change() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["wait", "1", "--until", "idle", "--timeout", "10"]);
    assert!(ok, "the shell never settled: {stderr}");
    let waiter = daemon
        .command(&["wait", "1", "--until", "unknown", "--timeout", "20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wait");
    std::thread::sleep(Duration::from_millis(500));
    // A foreground program that is not an agent reads as `unknown` for as
    // long as it runs: a change that lasts, unlike a shell's own hook report,
    // which the shell's prompt outranks within a quarter of a second.
    let (ok, _, stderr) = daemon.run(&["send", "1", "sleep 5", "--enter"]);
    assert!(ok, "send failed: {stderr}");
    let out = waiter.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "wait did not match: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("term=1 state=unknown program=sleep"),
        "{stdout}"
    );
}

#[test]
fn wait_times_out_with_124() {
    let daemon = Daemon::start();
    let started = Instant::now();
    let out = daemon.output(&["wait", "1", "--until", "sleeping", "--timeout", "1"]);
    assert_eq!(out.status.code(), Some(124), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn wait_refuses_what_it_cannot_wait_on() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["wait", "1", "--until", "asleep"]);
    assert!(!ok && stderr.contains("unknown state asleep"), "{stderr}");
    let (ok, _, stderr) = daemon.run(&["wait", "no-such-agent-xyz"]);
    assert!(
        !ok && stderr.contains("no such agent or terminal"),
        "{stderr}"
    );
    // A shell is not an agent, and a bare wait watches agents only.
    let (ok, _, stderr) = daemon.run(&["wait", "--timeout", "1"]);
    assert!(!ok && stderr.contains("no agent terminals"), "{stderr}");
}

#[test]
fn wait_reports_a_target_that_closes() {
    let daemon = Daemon::start();
    let (ok, number, stderr) = daemon.run(&["new"]);
    assert!(ok, "new failed: {stderr}");
    let number = number.trim().to_string();
    let waiter = daemon
        .command(&["wait", &number, "--until", "sleeping", "--timeout", "20"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wait");
    std::thread::sleep(Duration::from_millis(500));
    let (ok, _, stderr) = daemon.run(&["close", &number]);
    assert!(ok, "close failed: {stderr}");
    let out = waiter.wait_with_output().expect("wait output");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("terminal {number} closed")),
        "{out:?}"
    );
}

/// Past the daemon's thirty second no-acknowledgement deadline: a wait that
/// is disconnected as a wedged reader would fail with a connection error
/// here instead of timing out.
#[test]
fn a_long_wait_is_not_taken_for_a_wedged_reader() {
    let daemon = Daemon::start();
    let out = daemon.output(&["wait", "1", "--until", "sleeping", "--timeout", "35"]);
    assert_eq!(
        out.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn new_prints_a_terminal_that_starts_where_it_was_asked() {
    let daemon = Daemon::start();
    let place = tempfile::tempdir().expect("tempdir");
    // A file in the directory, not the directory's name: a long temporary
    // path wraps at 80 columns and splits whatever marker it ends in.
    let marked = place.path().join("start");
    std::fs::create_dir(&marked).expect("mkdir");
    std::fs::write(marked.join("cwd_marker_q7"), "").expect("marker");
    let (ok, stdout, stderr) = daemon.run(&[
        "new",
        "--name",
        "worker",
        "--cwd",
        marked.to_str().unwrap(),
        "--",
        "ls",
    ]);
    assert!(ok, "new failed: {stdout} {stderr}");
    let number: u64 = stdout.trim().parse().expect("only a number on stdout");
    let row = daemon.row(number).expect("the new terminal is listed");
    assert_eq!(row["name"], "worker");
    assert!(daemon.wait_for_screen(&number.to_string(), "cwd_marker_q7"));
}

#[test]
fn new_types_its_command_with_the_words_kept_apart() {
    let daemon = Daemon::start();
    let (ok, stdout, stderr) = daemon.run(&["new", "--", "echo", "a  b", "it's"]);
    assert!(ok, "new failed: {stderr}");
    assert!(daemon.wait_for_screen(stdout.trim(), "a  b it's"));
}

#[test]
fn new_refuses_a_stray_argument() {
    let daemon = Daemon::start();
    let (ok, _, stderr) = daemon.run(&["new", "claude"]);
    assert!(!ok && stderr.contains("unexpected claude"), "{stderr}");
}

#[test]
fn close_ends_a_terminal_but_never_its_own() {
    let daemon = Daemon::start();
    let (ok, number, _) = daemon.run(&["new"]);
    assert!(ok);
    let number = number.trim().to_string();
    let (ok, stdout, stderr) = daemon.run(&["close", &number]);
    assert!(ok, "close failed: {stderr}");
    assert_eq!(stdout.trim(), format!("closed terminal {number}"));
    assert!(daemon.row(number.parse().unwrap()).is_none());

    // Run as if from inside terminal 1 of this session.
    let out = daemon
        .command(&["close", "1"])
        .env("QORO_TERMINAL", "1")
        .env("QORO_SESSION", "default")
        .output()
        .expect("run close");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing to close"));
    assert!(daemon.row(1).is_some(), "terminal 1 must survive");

    // The same number in another session is somebody else's tab.
    let out = daemon
        .command(&["--session", "elsewhere", "close", "1"])
        .env("QORO_TERMINAL", "1")
        .env("QORO_SESSION", "default")
        .output()
        .expect("run close");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("refusing to close"));
}

#[test]
fn send_presses_named_keys() {
    let daemon = Daemon::start();
    // The marker only appears once the line runs: typed, it reads `$((1+1))`.
    let (ok, _, stderr) = daemon.run(&["send", "1", "echo KEYS_$((1+1))"]);
    assert!(ok, "{stderr}");
    let (ok, stdout, stderr) = daemon.run(&["send", "1", "--key", "enter"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("sent to terminal 1"));
    assert!(daemon.wait_for_screen("1", "KEYS_2"));

    let (ok, _, stderr) = daemon.run(&["send", "1", "--key", "f13"]);
    assert!(!ok && stderr.contains("unknown key f13"), "{stderr}");
    let (ok, _, stderr) = daemon.run(&["send", "1", "hi", "--key", "enter"]);
    assert!(!ok && stderr.contains("not both"), "{stderr}");
}

#[test]
fn a_write_to_a_replaced_process_is_refused() {
    let daemon = Daemon::start();
    let generation = daemon.row(1).expect("terminal 1")["generation"]
        .as_u64()
        .expect("a live row has a generation");
    let stale = (generation + 1).to_string();
    let current = generation.to_string();

    let (ok, _, stderr) = daemon.run(&["prompt", "1", "echo GEN_$((2+3))", "--generation", &stale]);
    assert!(!ok && stderr.contains("read it again"), "{stderr}");
    let (ok, _, stderr) = daemon.run(&["send", "1", "x", "--generation", &stale]);
    assert!(!ok && stderr.contains("read it again"), "{stderr}");
    let (ok, _, stderr) = daemon.run(&["close", "1", "--generation", &stale]);
    assert!(!ok && stderr.contains("read it again"), "{stderr}");
    assert!(daemon.row(1).is_some());

    let (ok, _, stderr) =
        daemon.run(&["prompt", "1", "echo GEN_$((2+3))", "--generation", &current]);
    assert!(ok, "{stderr}");
    assert!(daemon.wait_for_screen("1", "GEN_5"));
}

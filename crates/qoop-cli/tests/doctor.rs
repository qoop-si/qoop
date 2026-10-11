//! `qoop doctor` against a real daemon on an isolated stack, and with no daemon at all.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

/// The test's own daemon, killed when the test ends, failed or not.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(dir: &Path) -> Daemon {
    let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .arg("daemon")
        .env("QOOP_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("SHELL", "/bin/sh")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let socket = qoop_testkit::socket_in(dir);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !socket.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(socket.exists(), "the daemon never bound its socket");
    Daemon(child)
}

fn doctor(dir: &Path, args: &[&str]) -> Output {
    qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .arg("doctor")
        .args(args)
        .env("QOOP_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .output()
        .expect("run qoop doctor")
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

#[test]
fn a_fresh_daemon_is_clean_and_says_nothing() {
    let dir = private_dir();
    let daemon = start(dir.path());
    qoop_testkit::open_first_terminal(&qoop_testkit::socket_in(dir.path()));
    let out = doctor(dir.path(), &[]);
    let json = doctor(dir.path(), &["--json"]);
    drop(daemon);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(v["findings"], serde_json::json!([]));
    assert_eq!(v["skipped"], serde_json::json!([]));
}

#[test]
fn with_no_daemon_every_check_is_named_as_skipped() {
    let dir = private_dir();
    let out = doctor(dir.path(), &[]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    for what in ["the agents", "the board", "the inbox"] {
        assert!(said.contains(&format!("cannot check {what}")), "{said}");
    }
}

#[test]
fn a_wrong_argument_is_a_usage_error() {
    let dir = private_dir();
    let out = doctor(dir.path(), &["--bogus"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let out = doctor(dir.path(), &["--new"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}

/// A finding built through the CLI: a task in a terminal waits on the owner and the inbox has
/// no card for it. `--new` says it once.
#[test]
fn an_agent_waiting_with_no_card_is_found_and_said_once() {
    let dir = private_dir();
    let daemon = start(dir.path());
    qoop_testkit::open_first_terminal(&qoop_testkit::socket_in(dir.path()));
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join("run.sh"),
        format!(
            "Q='{}'\ncd '{}'\n$Q task new 'ship it' >/dev/null 2>&1\n\
             $Q task stage waiting-human >/dev/null 2>&1\ntouch finished\n",
            env!("CARGO_BIN_EXE_qoop"),
            work.display()
        ),
    )
    .unwrap();
    let line = format!("sh '{}'", work.join("run.sh").display());
    let sent = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["send", "1", &line, "--enter"])
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .output()
        .expect("send");
    assert!(sent.status.success(), "{sent:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !work.join("finished").exists() {
        assert!(Instant::now() < deadline, "the script never finished");
        std::thread::sleep(Duration::from_millis(100));
    }
    // The shell comes to rest after the script: give its row a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        let out = doctor(dir.path(), &[]);
        if out.status.code() == Some(1) || Instant::now() > deadline {
            break out;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let file = dir.path().join("said");
    let file = file.to_str().unwrap();
    let first = doctor(dir.path(), &["--new", file]);
    let second = doctor(dir.path(), &["--new", file]);
    drop(daemon);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(said.starts_with("I4 T1"), "{said}");
    assert!(said.contains("no card"), "{said}");
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        said,
        "said the first time"
    );
    assert!(second.stdout.is_empty(), "said again: {second:?}");
    assert_eq!(second.status.code(), Some(0), "nothing new: {second:?}");
}

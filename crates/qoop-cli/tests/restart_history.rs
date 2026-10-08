//! A tab that stopped comes back with the output it was showing.
//!
//! Only rows that scrolled off used to be written down, so the last command's output was
//! lost on every stop, and what was on disk was out of reach until something new scrolled.
//! Driven through the real binary: a daemon, `qoop send`, `qoop stop`, a second daemon over
//! the same directory, and the same `Scrollback` question the interface asks.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse};

/// A daemon killed when the test ends, pass or fail.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(dir: &Path) -> Running {
    let child = Command::new(env!("CARGO_BIN_EXE_qoop"))
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
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the daemon never listened"
    );
    Running(child)
}

fn qoop(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_qoop"))
        .args(args)
        .env("QOOP_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("QOOP_SESSION")
        .env_remove("QOOP_TERMINAL")
        .env_remove("QOOP_TOKEN")
        .output()
        .expect("run qoop");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_stopped_tab_comes_back_with_its_last_screen_in_reach() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = qoop_testkit::socket_in(dir.path());

    let mut first = spawn_daemon(dir.path());
    qoop_testkit::open_first_terminal(&socket);
    // Typed with arithmetic so the echoed command line is not itself the marker.
    let (sent, _) = qoop(
        dir.path(),
        &["send", "1", "--enter", "--", "echo QRMARK-$((6*7))"],
    );
    assert!(sent, "the command was not sent");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !qoop(dir.path(), &["read", "1"]).1.contains("QRMARK-42") {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        qoop(dir.path(), &["read", "1"]).1.contains("QRMARK-42"),
        "the marker never reached the screen"
    );
    let (stopped, _) = qoop(dir.path(), &["stop"]);
    assert!(stopped, "qoop stop failed");
    let _ = first.0.wait();

    let second = spawn_daemon(dir.path());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (behind, lines) = rt.block_on(async {
        let (stream, inbuf) = qoop_runtime::client::connect(&socket, "restart-test")
            .await
            .expect("connect");
        let mut c = qoop_testkit::TestClient::from_parts(stream, inbuf, 1);
        // The first screen of the restored tab, before it has printed anything new.
        let screen = c
            .wait_for(|_| true, Duration::from_secs(10))
            .await
            .expect("a screen of the restored tab");
        let answer = c
            .control(
                &ControlRequest::Scrollback {
                    id: "back".into(),
                    terminal: 1,
                    up: 200,
                    rows: 200,
                },
                Duration::from_secs(10),
            )
            .await;
        let lines = match answer {
            Some(ControlResponse::Scrollback { lines, .. }) => lines,
            other => panic!("no scrollback: {other:?}"),
        };
        (screen.behind, lines)
    });
    drop(second);

    assert!(
        lines.iter().any(|l| l.trim() == "QRMARK-42"),
        "the output on screen at the stop was not written down: {lines:?}"
    );
    assert!(
        behind > 0,
        "the restored tab's history was out of reach until new output scrolled"
    );
}

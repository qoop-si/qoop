//! A tab that printed a burst and went quiet can be scrolled from its first screen.
//!
//! The history writer saves a burst about 20 ms after it ends and only then knows how many
//! rows are behind the screen. The screen that went out before that said none, and a quiet
//! tab sends no other, so the wheel and copy mode saw nothing to scroll into until
//! something printed again or the tab was left and come back to. Driven through the real
//! binary: a daemon, a connected client, a command that prints and then waits.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// A daemon killed when the test ends, pass or fail.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_tab_that_printed_and_went_quiet_tells_its_client_what_is_behind_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = qoop_testkit::socket_in(dir.path());

    let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .arg("daemon")
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("SHELL", "/bin/sh")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let _daemon = Running(child);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    qoop_testkit::open_first_terminal(&socket);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let screen = rt.block_on(async {
        let (stream, inbuf) = qoop_runtime::client::connect(&socket, "quiet-count-test")
            .await
            .expect("connect");
        let mut c = qoop_testkit::TestClient::from_parts(stream, inbuf, 1);
        c.wait_for(|_| true, Duration::from_secs(10))
            .await
            .expect("a first screen");
        // A burst, then a command that prints nothing more and does not return: no prompt
        // redraw and no spinner to carry the count along by accident.
        c.input(b"seq 1 300; sleep 1000\n").await;
        assert!(
            c.wait_for_text("300", Duration::from_secs(10)).await,
            "the burst never reached the screen"
        );
        // Only screens sent from here on count: the one that showed the burst can carry
        // a count by luck of timing, and what is asked is that the tab says it again once
        // the burst is saved and nothing else is printing.
        c.forget_seen();
        c.wait_for(
            // Most of the burst is behind a 24 row screen once it has been saved.
            |s| s.behind >= 200,
            Duration::from_secs(10),
        )
        .await
    });

    assert!(
        screen.is_some(),
        "the quiet tab never said that its output is behind the screen"
    );
}

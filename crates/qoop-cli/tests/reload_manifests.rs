//! Phase E: `qoop reload-manifests` against a live daemon.
//!
//! The daemon reads the override directory live per request, so pointing its
//! config home at a temp dir drives the whole path: files on disk, through
//! the socket, into the serving rule table. Verdict changes themselves are
//! pinned one layer down in `qoop-detect`'s reload tests.

use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
    config: std::path::PathBuf,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).expect("config dir");
        let child = Command::new(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path().join("rt"))
            .env("XDG_CONFIG_HOME", &config)
            .env("HOME", dir.path().join("home"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(&dir.path().join("rt"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self { child, dir, config }
    }

    fn reload(&self) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_qoop"))
            .arg("reload-manifests")
            .env("QOOP_RUNTIME_DIR", self.dir.path().join("rt"))
            .output()
            .expect("run reload-manifests");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    fn override_dir(&self) -> std::path::PathBuf {
        self.config.join("qoop").join("agent-detection")
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
fn an_absent_override_directory_is_an_empty_success() {
    let daemon = Daemon::start();
    let (ok, out) = daemon.reload();
    assert!(ok, "an empty reload must succeed: {out:?}");
    assert!(out.contains("no overrides"), "it says so: {out:?}");
}

#[test]
fn a_good_override_loads_and_a_bad_one_is_reported_without_unloading() {
    let daemon = Daemon::start();
    let dir = daemon.override_dir();
    std::fs::create_dir_all(&dir).expect("override dir");
    std::fs::write(
        dir.join("aider.toml"),
        "id = \"aider\"\nversion = \"test\"\n\n[[rules]]\nid = \"r\"\nstate = \"idle\"\ncontains = \"x\"\n",
    )
    .expect("good override");
    std::fs::write(dir.join("claude.toml"), "id = \"claude\"\n[[rules\n").expect("bad override");

    let (ok, out) = daemon.reload();
    assert!(ok, "the reload itself succeeds: {out:?}");
    assert!(out.contains("loaded aider"), "the good file loads: {out:?}");
    assert!(
        out.contains("error claude:"),
        "the bad file is reported per file: {out:?}"
    );
}

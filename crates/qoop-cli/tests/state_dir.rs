//! The database and the history move out of the runtime directory, under the packaged
//! binary, the way an existing install meets this build.
//!
//! A daemon started with `QOOP_RUNTIME_DIR` keeps its state beside its socket, which is
//! exactly the layout every build before this one wrote, so that is how the old layout is
//! made here: by a real daemon, with a real tab and real history. Then a daemon started the
//! way a person starts one, with no override, finds it and moves it to the data directory.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(20);

/// Everything a daemon resolves a path from, inside one temp dir: nothing reaches the
/// person's own home, runtime directory or configuration.
struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("qs")
            .tempdir()
            .expect("tempdir");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        Self { root }
    }

    /// `$XDG_RUNTIME_DIR/qoop`, the runtime base both kinds of start resolve to.
    fn runtime(&self) -> PathBuf {
        self.root.path().join("rt/qoop")
    }

    fn session(&self) -> PathBuf {
        self.runtime().join("sessions/default")
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("data/qoop")
    }

    /// `qoop` with this machine's environment. `old` starts it the way every build before
    /// this one kept its files: runtime directory named, state beside the socket.
    fn qoop(&self, old: bool) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_qoop"));
        c.env("HOME", self.root.path().join("home"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("rt"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("SHELL", "/bin/sh")
            .env_remove("QOOP_RUNTIME_DIR")
            .env_remove("QOOP_SESSION")
            .env_remove("QOOP_TOKEN")
            .env_remove("QOOP_TERMINAL")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if old {
            c.env("QOOP_RUNTIME_DIR", self.runtime());
        }
        c
    }

    /// Started, and answering a handshake: a connect alone succeeds as soon as the socket
    /// is bound, which is before the move has run.
    fn start(&self, old: bool) -> Daemon {
        let child = self.qoop(old).arg("daemon").spawn().expect("spawn");
        let daemon = Daemon(child);
        let socket = self.session().join("qoopd.sock");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let deadline = Instant::now() + PATIENCE;
        while rt
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(2),
                    qoop_runtime::client::connect(&socket, "state-probe"),
                )
                .await
            })
            .map_or(true, |answered| answered.is_err())
        {
            assert!(Instant::now() < deadline, "the daemon never answered");
            std::thread::sleep(Duration::from_millis(50));
        }
        daemon
    }

    fn stop(&self, old: bool) {
        let ok = self.qoop(old).arg("stop").status().expect("stop");
        assert!(ok.success(), "qoop stop refused");
        let socket = self.session().join("qoopd.sock");
        let deadline = Instant::now() + PATIENCE;
        while std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            assert!(Instant::now() < deadline, "the daemon did not stop");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn spaces(&self) -> serde_json::Value {
        let out = self
            .qoop(false)
            .args(["ls", "--json"])
            .stdout(Stdio::piped())
            .output()
            .expect("qoop ls");
        serde_json::from_slice(&out.stdout).expect("qoop ls --json")
    }
}

/// Killed if the test fails before it stops the daemon itself.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn an_existing_install_has_its_state_moved_out_of_the_runtime_directory() {
    let m = Machine::new();

    // The old layout, made by a real daemon: one tab, recorded, with history on disk.
    let old = m.start(true);
    qoop_testkit::open_first_terminal(&m.session().join("qoopd.sock"));
    m.stop(true);
    drop(old);
    assert!(
        m.session().join("qoop.db").exists(),
        "no old layout to move"
    );
    assert!(m.session().join("history").exists(), "no history to move");
    assert!(
        !m.data().exists(),
        "the old start wrote into the data directory"
    );

    // Started the way a person starts it.
    let new = m.start(false);
    assert!(
        m.data().join("qoop.db").exists(),
        "the database did not move"
    );
    assert!(
        m.data().join("history").exists(),
        "the history did not move"
    );
    for gone in ["qoop.db", "qoop.db-wal", "qoop.db-shm", "history"] {
        assert!(
            !m.session().join(gone).exists(),
            "{gone} was left in the runtime directory"
        );
    }
    // The socket stays where it was, and the data directory is private.
    assert!(m.session().join("qoopd.sock").exists());
    let mode = std::fs::metadata(m.data())
        .expect("data dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "the data directory is readable by others");

    // The tab the old daemon recorded is the one this one brought back.
    let spaces = m.spaces();
    assert_eq!(spaces.as_array().map(Vec::len), Some(1), "{spaces}");
    assert_eq!(spaces[0]["tabs"], 1, "{spaces}");

    // And a listing with no daemon reads the moved database.
    m.stop(false);
    drop(new);
    let spaces = m.spaces();
    assert_eq!(spaces[0]["tabs"], 1, "no daemon: {spaces}");
    assert!(
        !m.session().join("qoop.db").exists(),
        "a listing left a database in the runtime directory"
    );
}

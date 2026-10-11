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
        let mut c = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        c.env("HOME", self.root.path().join("home"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("rt"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("SHELL", "/bin/sh")
            .env_remove("QOOP_RUNTIME_DIR")
            .env_remove("QORO_RUNTIME_DIR")
            .env_remove("QOOP_SESSION")
            .env_remove("QORO_SESSION")
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if old {
            c.env("QOOP_RUNTIME_DIR", self.runtime());
        }
        c
    }

    /// Where the logs directory is in this machine's home (`paths::logs_dir`).
    fn logs(&self) -> PathBuf {
        let home = self.root.path().join("home");
        if cfg!(target_os = "macos") {
            home.join("Library/Logs/qoop")
        } else {
            home.join(".local/state/qoop/log")
        }
    }

    /// Started, and answering a handshake: a connect alone succeeds as soon as the socket
    /// is bound, which is before the move has run.
    fn start(&self, old: bool) -> Daemon {
        self.start_as(self.qoop(old))
    }

    fn start_as(&self, mut command: Command) -> Daemon {
        let child = command.arg("daemon").spawn().expect("spawn");
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

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Started on demand, a daemon has `/dev/null` for stderr: what it says goes to its log, which
/// only the person can read. An isolated stack (its runtime directory named) keeps its log
/// beside its socket and never writes into the person's.
#[test]
fn a_daemon_started_with_nowhere_to_write_keeps_a_log() {
    let m = Machine::new();
    let person = m.start(false);
    let log = m.logs().join("qoopd.log");
    let text = read(&log);
    assert!(
        text.contains("qoopd") && text.contains("starting") && text.contains("listening on"),
        "{text:?}"
    );
    let mode = std::fs::metadata(&log).expect("log").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the log is readable by others");
    m.stop(false);
    drop(person);

    let said = read(&log).len();
    let isolated = m.start(true);
    assert!(
        read(&m.session().join("qoopd.log")).contains("listening on"),
        "no log beside the socket"
    );
    m.stop(true);
    drop(isolated);
    assert_eq!(
        read(&log).len(),
        said,
        "an isolated stack wrote into the person's log"
    );
}

/// A daemon whose stderr goes somewhere (a terminal, a test's pipe) keeps it.
#[test]
fn a_daemon_whose_stderr_is_taken_keeps_it() {
    let m = Machine::new();
    let mut piped = m.qoop(false);
    piped.stderr(Stdio::piped());
    let mut daemon = m.start_as(piped);
    let mut stderr = daemon.0.stderr.take().expect("stderr");
    m.stop(false);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stderr, &mut text).expect("read stderr");
    drop(daemon);
    assert!(text.contains("listening on"), "{text:?}");
    assert!(
        !m.logs().join("qoopd.log").exists(),
        "a daemon with a stderr of its own wrote a log"
    );
}

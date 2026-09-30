//! The idempotency ledger under the packaged binary, through the real socket.
//!
//! `gates_m4` proves a retry is a replay with a daemon built inside the test process, which
//! is the right harness for a gate and the wrong one for a claim about what ships. This
//! starts the real `qoro daemon` the way a person starts it, talks to it over its socket
//! from separate connections, stops it, starts another over the same files, and looks at
//! what each of them left behind.
//!
//! Three things here are only observable from outside.
//!
//! A retry really is a replay across a process boundary, with nothing carried in memory
//! between the two connections. The answer really does survive the daemon that wrote it,
//! which is the only reason the ledger is on disk rather than in a map. And no `events.db`
//! is created at all, which is the point of the change: a test that looked only at the API
//! could not tell an unwritten log from an unread one.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_store::Store;

const PATIENCE: Duration = Duration::from_secs(20);

/// A runtime directory the test owns, reused by a restart and removed when the test ends.
///
/// The name is short on purpose: the daemon's socket lives two levels down, and a macOS
/// `$TMPDIR` leaves little of the 104 bytes a socket path may have.
struct Runtime(PathBuf);

impl Runtime {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ql-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a runtime directory");
        // The daemon refuses a directory anything group or other can reach (ADR-012), and
        // `create_dir_all` honours the umask.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("chmod the runtime directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn session(&self) -> PathBuf {
        self.0.join("sessions/default")
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A daemon, killed when this value is dropped.
struct Daemon(Child);

impl Daemon {
    /// Started over a directory that may already have files in it, which is a restart.
    fn start_over(runtime: &Runtime) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_qoro"))
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path().join("config"))
            .env("SHELL", "/bin/sh")
            // A shell that exports any of these makes the daemon refuse to start, because
            // they name a session, a token and a terminal that are not this test's. The
            // other harnesses in this repo strip them for the same reason.
            .env_remove(qoro_runtime::paths::SESSION_ENV)
            .env_remove("QORO_TOKEN")
            .env_remove("QORO_TERMINAL")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        // The guard is whole from here, so a failed wait cannot leave a daemon running:
        // `Child`'s own `Drop` does not kill.
        let daemon = Self(child);
        let socket = qoro_testkit::socket_in(runtime.path());
        // Connectable is the real readiness. A restarted daemon inherits the previous one's
        // socket file, so the file being there says nothing; only an answer does.
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err()
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the daemon never answered on {}",
            socket.display()
        );
        qoro_testkit::open_first_terminal(&socket);
        daemon
    }

    /// A separate connection to this daemon.
    async fn client(&self, runtime: &Runtime, id: &str) -> qoro_testkit::TestClient {
        let (stream, inbuf) =
            qoro_runtime::client::connect(&qoro_testkit::socket_in(runtime.path()), id)
                .await
                .expect("connect");
        qoro_testkit::TestClient::from_parts(stream, inbuf, 1)
    }
}

/// `qoro stop`, the way a person ends a session.
///
/// A signal would leave the daemon mid-write, which is a different test of a different thing.
/// A clean stop is also what makes the restart honest: the ledger row the second daemon reads
/// is one the first daemon got to flush.
fn stop_daemon(runtime: &Runtime) {
    let out = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .arg("stop")
        .env("QORO_RUNTIME_DIR", runtime.path())
        .env("XDG_CONFIG_HOME", runtime.path().join("config"))
        .env_remove(qoro_runtime::paths::SESSION_ENV)
        .env_remove("QORO_TOKEN")
        .env_remove("QORO_TERMINAL")
        .output()
        .expect("run qoro stop");
    assert!(
        out.status.success(),
        "qoro stop refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The socket file itself stays on disk after a stop, so the check is that nothing
    // answers on it any more rather than that it is gone.
    let socket = qoro_testkit::socket_in(runtime.path());
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_err(),
        "something still answers on {} after qoro stop",
        socket.display()
    );
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn store_at(runtime: &Runtime) -> Store {
    Store::open(&runtime.session().join("qoro.db")).expect("open the store the daemon wrote")
}

fn create(id: &str) -> ControlRequest {
    ControlRequest::CreateTerminal {
        id: id.into(),
        cwd: None,
        workspace: None,
        name: None,
    }
}

fn created_terminal(answer: ControlResponse) -> u32 {
    match answer {
        ControlResponse::TerminalCreated { terminal, .. } => terminal,
        other => panic!("expected a created terminal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_retry_across_two_connections_is_a_replay_and_no_event_log_is_written() {
    let runtime = Runtime::new("replay");
    let d = Daemon::start_over(&runtime);
    let request = create("e2e-key");

    let terminal = created_terminal(
        d.client(&runtime, "first")
            .await
            .control(&request, PATIENCE)
            .await
            .expect("the first create answers"),
    );

    // A second connection, so nothing can answer this from the first one's memory. A create
    // answered without the ledger mints a new terminal, which is what makes this the
    // assertion that fails when the ledger is gone.
    let again = created_terminal(
        d.client(&runtime, "second")
            .await
            .control(&request, PATIENCE)
            .await
            .expect("the retry answers"),
    );
    assert_eq!(again, terminal, "the retry opened a second terminal");

    // What the next daemon will read.
    let (ok, detail) = store_at(&runtime)
        .applied("e2e-key")
        .expect("lookup")
        .expect("the answer was written where the next daemon looks");
    assert!(ok, "the recorded answer is a failure: {detail:?}");
    assert_eq!(detail["terminal"], terminal);

    // And the log is absent, not merely unread. Before this change the daemon opened
    // `events.db` at exactly this path, so the assertion could not have passed.
    let log = runtime.session().join("events.db");
    assert!(
        !log.exists(),
        "the packaged daemon wrote an event log at {}",
        log.display()
    );
}

/// A close is the mutating command where a second attempt has a visible cost, so the same
/// replay is checked for it against the real binary.
///
/// The teardown is waited out before the retry, and that is what makes this a test. A close
/// answers before its teardown finishes, and the daemon keeps the registry entry until the
/// terminal deregisters itself, so a retry sent in that window is answered by a second real
/// close whether or not the ledger exists. Once the terminal is gone, a missing ledger
/// answers `Failed { no terminal N }` instead of the original `TerminalClosed`.
#[tokio::test]
async fn a_replayed_close_across_two_connections_is_a_replay() {
    let runtime = Runtime::new("close");
    let d = Daemon::start_over(&runtime);
    let terminal = 1u32;
    let request = || ControlRequest::CloseTerminal {
        id: "e2e-close".into(),
        terminal,
    };

    assert!(
        matches!(
            d.client(&runtime, "closer")
                .await
                .control(&request(), PATIENCE)
                .await
                .expect("an answer"),
            ControlResponse::TerminalClosed { .. }
        ),
        "the first close was refused"
    );
    wait_until_gone(&d, &runtime, terminal).await;

    match d
        .client(&runtime, "closer-again")
        .await
        .control(&request(), PATIENCE)
        .await
        .expect("an answer")
    {
        ControlResponse::TerminalClosed { terminal: t, .. } => assert_eq!(t, terminal),
        other => panic!("the retry was not answered from the ledger: {other:?}"),
    }

    let (ok, _) = store_at(&runtime)
        .applied("e2e-close")
        .expect("lookup")
        .expect("the close was written down");
    assert!(ok, "the recorded close is a failure");
}

/// The answer is still there after the daemon that wrote it is gone.
///
/// This is the property that puts the ledger on disk at all. A retry answered from a map
/// would be a replay within one daemon's lifetime and nothing at all across a restart, which
/// is the case a client hits when it lost its connection and the machine restarted before it
/// asked again.
#[tokio::test]
async fn a_retry_after_a_restart_is_still_a_replay() {
    let runtime = Runtime::new("restart");
    let first = Daemon::start_over(&runtime);
    let request = create("across-restart");
    let terminal = created_terminal(
        first
            .client(&runtime, "writer")
            .await
            .control(&request, PATIENCE)
            .await
            .expect("the first create answers"),
    );

    // The files have to be there, or this would silently test an empty database.
    assert!(
        store_at(&runtime)
            .applied("across-restart")
            .expect("lookup")
            .is_some(),
        "the first daemon did not write the answer, so the restart proves nothing"
    );
    // A clean stop, the way a person ends a session, rather than a signal. A killed daemon
    // leaves its socket behind and the next one has to clear it, which is a different test
    // of a different thing.
    stop_daemon(&runtime);

    let second = Daemon::start_over(&runtime);

    // The daemon that answers has never heard of this key: it is a different process that
    // read the file. A create answered from its own memory would mint a new terminal above
    // every id restore saw.
    let again = created_terminal(
        second
            .client(&runtime, "after-restart")
            .await
            .control(&request, PATIENCE)
            .await
            .expect("the retry answers"),
    );
    assert_eq!(
        again, terminal,
        "the restarted daemon opened a second terminal instead of answering from disk"
    );
}

/// Whether the daemon has finished tearing the terminal down.
///
/// A close answers before its teardown finishes, and the registry entry survives until the
/// terminal task deregisters itself: measured with the packaged binary at 278 to 294 ms
/// after the close is issued, against a 12 to 40 ms round trip. A retry sent inside that
/// window finds a live terminal and is answered by a second real close, so waiting is the
/// only thing that makes the close assertion mean anything. `ListTerminals` is the probe
/// because it reads daemon memory and writes nothing, least of all the ledger.
async fn wait_until_gone(d: &Daemon, runtime: &Runtime, terminal: u32) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let listed = d
            .client(runtime, "probe")
            .await
            .control(
                &ControlRequest::ListTerminals { id: "probe".into() },
                PATIENCE,
            )
            .await
            .expect("the probe answers");
        let streams = match listed {
            ControlResponse::Terminals { terminals, .. } => terminals,
            other => panic!("the probe was not answered with the terminal list: {other:?}"),
        };
        if !streams.contains(&terminal) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the terminal never went away, so a retry cannot be told apart from a second close"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

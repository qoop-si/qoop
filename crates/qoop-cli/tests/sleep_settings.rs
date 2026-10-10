//! A machine's sleep settings, read and changed through its daemon's socket.
//!
//! Driven with the real `qoop daemon`, because the claims are about a file on disk and a
//! process that acts on it: the change lands whole with its comments, a file that is not TOML
//! is said and left alone, a setting never sleeps or ends a tab by itself, and a daemon that
//! sat quiet with idle sleep off acts on a setting that arrives with nothing else going on.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use qoop_runtime::control::{ControlRequest, ControlResponse, WorkspaceRow};
use qoop_testkit::TestClient;

const PATIENCE: Duration = Duration::from_secs(20);

/// A runtime directory the test owns. Short on purpose: a socket path has about 104 bytes.
struct Runtime(PathBuf);

impl Runtime {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("qs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a runtime directory");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        Self(path)
    }

    /// The file the daemon under test reads and writes: its own, never the person's.
    fn config(&self) -> PathBuf {
        self.0.join("config/qoop/config.toml")
    }

    /// What the daemon says about itself.
    fn log(&self) -> PathBuf {
        self.0.join("daemon.log")
    }

    fn write_config(&self, text: &str) {
        let file = self.config();
        std::fs::create_dir_all(file.parent().expect("parent")).expect("config dir");
        std::fs::write(file, text).expect("write the config");
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A daemon, killed when this value is dropped.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Daemon {
    fn start(runtime: &Runtime) -> Self {
        let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", &runtime.0)
            .env("XDG_CONFIG_HOME", runtime.0.join("config"))
            .env("SHELL", "/bin/sh")
            .env_remove(qoop_runtime::paths::SESSION_ENV)
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(runtime.log()).expect("a log"))
            .spawn()
            .expect("spawn the daemon");
        let daemon = Self(child);
        let socket = qoop_testkit::socket_in(&runtime.0);
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err()
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the daemon never answered"
        );
        daemon
    }

    async fn client(&self, runtime: &Runtime, id: &str) -> TestClient {
        let (stream, inbuf) =
            qoop_runtime::client::connect(&qoop_testkit::socket_in(&runtime.0), id)
                .await
                .expect("connect");
        TestClient::from_parts(stream, inbuf, 1)
    }

    fn ended(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(Some(_)))
    }
}

async fn ask(c: &mut TestClient, request: ControlRequest) -> ControlResponse {
    c.control(&request, PATIENCE).await.expect("an answer")
}

async fn settings(
    c: &mut TestClient,
    id: &str,
    idle_minutes: Option<u64>,
    exit_when_asleep: Option<bool>,
) -> ControlResponse {
    ask(
        c,
        ControlRequest::SleepSettings {
            id: id.into(),
            idle_minutes,
            exit_when_asleep,
        },
    )
    .await
}

fn said(idle_minutes: u64, exit_when_asleep: bool) -> ControlResponse {
    ControlResponse::SleepSettings {
        id: String::new(),
        idle_minutes,
        exit_when_asleep,
        problem: None,
    }
}

/// The answer with its request id blanked, to compare what it says.
fn body(answer: ControlResponse) -> ControlResponse {
    match answer {
        ControlResponse::SleepSettings {
            idle_minutes,
            exit_when_asleep,
            problem,
            ..
        } => ControlResponse::SleepSettings {
            id: String::new(),
            idle_minutes,
            exit_when_asleep,
            problem,
        },
        other => other,
    }
}

async fn space_with_two_tabs(c: &mut TestClient) -> (i64, u32, u32) {
    let ControlResponse::WorkspaceCreated { workspace, .. } = ask(
        c,
        ControlRequest::CreateWorkspace {
            id: "space".into(),
            name: "rest".into(),
        },
    )
    .await
    else {
        panic!("no space");
    };
    let mut tabs = Vec::new();
    for id in ["idle", "busy"] {
        let ControlResponse::TerminalCreated { terminal, .. } = ask(
            c,
            ControlRequest::CreateTerminal {
                id: id.into(),
                cwd: None,
                workspace: Some(workspace),
                name: None,
            },
        )
        .await
        else {
            panic!("no tab");
        };
        tabs.push(terminal);
    }
    // A task in the foreground: the tab the idle pass has to pass by.
    c.input_to(tabs[1], b"sleep 300\n").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    (workspace, tabs[0], tabs[1])
}

async fn space_row(c: &mut TestClient, space: i64) -> WorkspaceRow {
    let ControlResponse::Workspaces { workspaces, .. } =
        ask(c, ControlRequest::ListWorkspaces { id: "list".into() }).await
    else {
        panic!("no list");
    };
    workspaces
        .into_iter()
        .find(|w| w.id == space)
        .expect("the space")
}

fn staged_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".new"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_lands_whole_with_the_comments_and_keys_around_it() {
    let rt = Runtime::new("write");
    rt.write_config("# mine\ntheme = \"dark\"\n[sleep]\n# how long\nidle_minutes = 5 # five\n");
    let daemon = Daemon::start(&rt);
    let mut c = daemon.client(&rt, "phone").await;

    assert_eq!(body(settings(&mut c, "r", None, None).await), said(5, true));

    assert_eq!(
        body(settings(&mut c, "w", Some(15), None).await),
        said(15, true)
    );
    let text = std::fs::read_to_string(rt.config()).expect("read");
    assert!(text.contains("idle_minutes = 15 # five"), "{text}");
    assert!(
        text.contains("# mine") && text.contains("# how long"),
        "{text}"
    );
    assert!(text.contains("theme = \"dark\""), "{text}");

    assert_eq!(
        body(settings(&mut c, "w2", Some(0), Some(false)).await),
        said(0, false)
    );
    let text = std::fs::read_to_string(rt.config()).expect("read");
    assert!(
        text.contains("idle_minutes = 0") && text.contains("exit_when_asleep = false"),
        "{text}"
    );
    assert!(text.contains("# mine"), "{text}");
    assert_eq!(
        staged_files(rt.config().parent().expect("dir")),
        Vec::<String>::new()
    );

    // A number TOML cannot hold is refused and changes nothing.
    let refused = settings(&mut c, "big", Some(u64::MAX), None).await;
    assert!(
        matches!(&refused, ControlResponse::Failed { reason, .. } if reason.contains("idle_minutes")),
        "{refused:?}"
    );
    assert_eq!(
        body(settings(&mut c, "r2", None, None).await),
        said(0, false)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_not_toml_is_said_and_never_shown_as_defaults() {
    let rt = Runtime::new("broken");
    rt.write_config("[sleep\nidle_minutes = ");
    let daemon = Daemon::start(&rt);
    let mut c = daemon.client(&rt, "phone").await;

    for (id, idle) in [("read", None), ("write", Some(15))] {
        let answer = settings(&mut c, id, idle, None).await;
        assert!(
            matches!(&answer, ControlResponse::Failed { reason, .. } if reason.contains("is not TOML")),
            "{answer:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(rt.config()).expect("read"),
        "[sleep\nidle_minutes = ",
        "a file that is not TOML was written over"
    );

    // A valid file with a value this build cannot use still answers, and says which.
    rt.write_config("[sleep]\nidle_minutes = -5\n[daemon]\nexit_when_asleep = false\n");
    let answer = settings(&mut c, "bad", None, None).await;
    let ControlResponse::SleepSettings {
        idle_minutes,
        exit_when_asleep,
        problem,
        ..
    } = answer
    else {
        panic!("no answer: {answer:?}");
    };
    assert_eq!((idle_minutes, exit_when_asleep), (0, false));
    assert!(problem.expect("a problem").contains("idle_minutes"));
}

/// Turn idle sleep on from the socket on a daemon that has sat quiet with it off. Nothing
/// else happens afterwards (no tab made, no agent change): the setting alone makes the daemon
/// look at the file, and the pass that follows sleeps the idle tab and passes the busy one by.
/// About two minutes: the timeout is a minute and the pass comes on a tick of a minute at
/// least. Run it deliberately: `cargo test -p qoop-cli --test sleep_settings -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs for about two minutes: a minute of idleness plus one tick; run it deliberately"]
async fn turning_idle_sleep_on_acts_with_no_other_activity_and_spares_the_busy_tab() {
    let rt = Runtime::new("nudge");
    let daemon = Daemon::start(&rt);
    let mut c = daemon.client(&rt, "phone").await;
    let (space, idle, busy) = space_with_two_tabs(&mut c).await;

    let started = Instant::now();
    assert_eq!(
        body(settings(&mut c, "w", Some(1), None).await),
        said(1, true)
    );
    // No connection from here on: a connection that is subscribed to a tab is somebody
    // watching it, and the idle pass leaves a watched tab alone, so a test that kept asking
    // would be the reason nothing sleeps. The daemon's own log says when the tab went.
    drop(c);
    let line = format!("tab {idle} of space {space} asleep");
    let deadline = started + Duration::from_secs(240);
    loop {
        let log = std::fs::read_to_string(rt.log()).unwrap_or_default();
        if log.contains(&line) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "nothing slept in four minutes after the setting:\n{log}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    let seconds = started.elapsed().as_secs();
    eprintln!("seconds from the setting to the first tab asleep: {seconds}");

    let mut look = daemon.client(&rt, "look").await;
    let row = space_row(&mut look, space).await;
    assert!(
        row.sleeping.iter().all(|t| t.lead == Some(idle)) && row.sleeping.len() == 1,
        "only the idle tab sleeps: {row:?}"
    );
    assert_eq!(
        row.terminals, 1,
        "the busy tab ({busy}) is still open: {row:?}"
    );
    let log = std::fs::read_to_string(rt.log()).unwrap_or_default();
    assert!(!log.contains(&format!("tab {busy} of space")), "{log}");
}

/// Whether a quiet daemon (nothing awake, nobody attached) has left 16 seconds after its last
/// client went, with `exit_when_asleep` set through the socket first when `leave` says so.
async fn quiet_daemon_left(name: &str, leave: Option<bool>) -> bool {
    let rt = Runtime::new(name);
    let mut daemon = Daemon::start(&rt);
    let mut c = daemon.client(&rt, "phone").await;
    let want = said(0, leave.unwrap_or(true));
    assert_eq!(body(settings(&mut c, "w", None, leave).await), want);
    drop(c);
    let deadline = Instant::now() + Duration::from_secs(16);
    while Instant::now() < deadline {
        if daemon.ended() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// `exit_when_asleep = false` set through the socket is honoured: with nothing awake and
/// nobody attached the daemon stays, where by default (the same setup, the key left alone) it
/// leaves within about ten seconds.
#[tokio::test(flavor = "multi_thread")]
async fn leave_when_asleep_off_keeps_a_quiet_daemon_up() {
    assert!(
        quiet_daemon_left("leave", None).await,
        "by default a quiet daemon leaves"
    );
    assert!(
        !quiet_daemon_left("stay", Some(false)).await,
        "the daemon left though the setting says stay"
    );
}

//! A daemon asked to move onto the qoro on disk does it only once nothing is working, and
//! every tab comes back on the new binary as it was.
//!
//! Driven with the real binary, because the move is the process becoming another program.
//! Each daemon runs from a copy of its own, which is then replaced the way an update
//! replaces it: by a script that says it is `NEXT` and runs this same test binary for
//! anything else. A move is only ever onto a newer version than the one running, so this is
//! what makes one possible here. An exec keeps the process id, so the proof the move
//! happened is a second `listening` line from one process that never exited, and an agent
//! typed back into its own conversation, which only a restore does.
//!
//! The agent is a stand-in: a shell script named `droid` (an agent the detection table
//! knows, resumable by id, and with no screen rules to second-guess its hook) that reports
//! its state and conversation through `qoro hook-report` the way a real agent's hook does.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use qoro_runtime::control::{AgentRow, ControlRequest, ControlResponse, WorkspaceRow};

/// The cap every condition below waits under. It only decides how long a real failure
/// takes to report, and covers the daemon's own minute between looks with no event.
const PATIENCE: Duration = Duration::from_secs(90);

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A version newer than this build: what the replaced binary on disk says it is.
const NEXT: &str = "99.0.0";

const SESSION: &str = "sess-restart-42";

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
    /// Where the stand-in agent lives, first on the daemon's `PATH`.
    bin: PathBuf,
    /// The daemon's stderr, kept across the exec.
    log: PathBuf,
    /// The binary it started from, its own copy, for a test to replace.
    exe: PathBuf,
}

impl Daemon {
    /// A daemon with `NEXT` on disk, ready to be moved onto it.
    fn start() -> Self {
        let d = Self::start_installed();
        replace_with_version(&d.exe, NEXT);
        d
    }

    /// Start `qoro daemon` from a copy of this build of its own, still this version.
    fn start_installed() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let installed = dir.path().join("installed");
        std::fs::create_dir_all(&installed).expect("installed");
        let exe = installed.join("qoro");
        stage(&exe);
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let log = dir.path().join("qorod.log");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let child = Command::new(&exe)
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .env("PATH", path)
            .env_remove("QORO_SESSION")
            .env_remove("QORO_TERMINAL")
            .env_remove("QORO_TOKEN")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("log"))
            .spawn()
            .expect("spawn the daemon");
        let socket = qoro_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err()
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the daemon never listened"
        );
        qoro_testkit::open_first_terminal(&socket);
        Self {
            child,
            dir,
            bin,
            log,
            exe,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_qoro"))
            .args(args)
            .env("QORO_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("QORO_SESSION")
            .env_remove("QORO_TERMINAL")
            .env_remove("QORO_TOKEN")
            .output()
            .expect("run qoro")
    }

    fn send(&self, terminal: u32, line: &str) {
        let sent = self.run(&["send", &terminal.to_string(), line, "--enter"]);
        assert!(sent.status.success(), "{}", text(&sent.stderr));
    }

    fn interrupt(&self, terminal: u32) {
        let sent = self.run(&["send", &terminal.to_string(), "--key", "ctrl-c"]);
        assert!(sent.status.success(), "{}", text(&sent.stderr));
    }

    fn new_tab(&self) -> u32 {
        let out = self.run(&["new"]);
        assert!(out.status.success(), "new failed: {}", text(&out.stderr));
        text(&out.stdout).trim().parse().expect("a terminal number")
    }

    /// One request on a connection of its own. `None` when the connection closed first.
    fn ask(&self, request: ControlRequest) -> Option<ControlResponse> {
        let socket = qoro_testkit::socket_in(self.dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (stream, inbuf) = qoro_runtime::client::connect(&socket, "restart-test")
                .await
                .ok()?;
            let mut c = qoro_testkit::TestClient::from_parts(stream, inbuf, 1);
            c.control(&request, Duration::from_secs(30)).await
        })
    }

    fn arm(&self, version: &str) -> Option<ControlResponse> {
        self.ask(ControlRequest::RestartWhenSafe {
            id: format!("arm-{}", unique()),
            version: version.into(),
        })
    }

    /// What the daemon waits on, asked again until `done` holds for it.
    fn waiting_until(&self, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let answer = self.arm(NEXT);
            if let Some(ControlResponse::RestartArmed {
                waiting_on: Some(why),
                ..
            }) = &answer
                && done(why)
            {
                return why.clone();
            }
            assert!(
                Instant::now() < deadline,
                "the wait never settled: {answer:?}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn spaces(&self) -> Vec<WorkspaceRow> {
        match self.ask(ControlRequest::ListWorkspaces {
            id: format!("spaces-{}", unique()),
        }) {
            Some(ControlResponse::Workspaces { workspaces, .. }) => workspaces,
            other => panic!("no spaces: {other:?}"),
        }
    }

    fn agents(&self) -> Vec<AgentRow> {
        match self.ask(ControlRequest::ListAgents {
            id: format!("agents-{}", unique()),
        }) {
            Some(ControlResponse::Agents { agents, .. }) => agents,
            other => panic!("no agents: {other:?}"),
        }
    }

    fn wait_for_row(&self, terminal: u32, what: &str, holds: impl Fn(&AgentRow) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let rows = self.agents();
            if rows
                .iter()
                .any(|row| row.terminal == terminal && holds(row))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "tab {terminal} never {what}: {rows:?}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn wait_for_screen(&self, terminal: u32, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let screen = text(&self.run(&["read", &terminal.to_string()]).stdout);
            if screen.contains(needle) {
                return;
            }
            assert!(Instant::now() < deadline, "never arrived: {screen}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// How many times a daemon came up in this process.
    fn starts(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .matches("qorod: listening on")
            .count()
    }

    /// Whether the daemon's process is still the one this test started, not exited.
    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The stand-in agent: says it is working in a conversation, then idle once `go`
    /// exists, and waits on its keyboard like a real one. Resumed, it says into which.
    fn install_agent(&self, go: &Path) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = --resume ]; then echo \"DROID-RESUMED-$2\"; while read -r l; do :; done; fi\n\
             sleep 1\n\
             '{q}' hook-report --state working --session-id {SESSION}\n\
             echo DROID-WORKING\n\
             while [ ! -f '{go}' ]; do sleep 0.2; done\n\
             '{q}' hook-report --state idle --session-id {SESSION}\n\
             echo DROID-IDLE\n\
             while read -r l; do :; done\n",
            q = env!("CARGO_BIN_EXE_qoro"),
            go = go.display(),
        );
        let path = self.bin.join("droid");
        std::fs::write(&path, script).expect("the agent");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn running_sleep(row: &AgentRow) -> bool {
    row.program.as_deref() == Some("sleep")
}

#[test]
fn a_running_command_holds_the_move_and_is_named() {
    let mut d = Daemon::start();
    d.send(1, "sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);

    let answer = d.arm(NEXT);
    match &answer {
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => assert!(
            why.contains("tab 1") && why.contains("sleep"),
            "the wait does not name the tab: {why}"
        ),
        other => panic!("not armed with a reason: {other:?}"),
    }
    std::thread::sleep(Duration::from_secs(3));
    assert!(d.running(), "the daemon went");
    assert_eq!(d.starts(), 1, "the daemon moved under a running command");
    d.wait_for_row(1, "kept its command", running_sleep);
}

/// The gating test, and the regression that catches an agent dying in the move.
#[test]
fn the_move_waits_for_a_working_agent_and_a_command_then_brings_both_back() {
    let mut d = Daemon::start();
    let go = d.dir.path().join("go");
    d.install_agent(&go);
    let busy = d.new_tab();
    d.send(1, "droid");
    d.send(busy, "sleep 30");
    d.wait_for_row(1, "had a working agent in a named conversation", |row| {
        row.program.as_deref() == Some("droid")
            && row.state == "working"
            && row.agent_session.as_deref() == Some(SESSION)
    });
    d.wait_for_row(busy, "ran sleep", running_sleep);

    match d.arm(NEXT) {
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => assert!(
            why.contains("droid is working") && why.contains(&format!("tab {busy}")),
            "the wait does not name both: {why}"
        ),
        other => panic!("not armed with a reason: {other:?}"),
    }

    // The agent finishes its turn; the command still runs, so nothing moves.
    std::fs::write(&go, "").expect("go");
    let why = d.waiting_until(|why| !why.contains("droid"));
    assert!(why.contains(&format!("tab {busy}")), "{why}");
    assert_eq!(d.starts(), 1, "the daemon moved under a running command");
    d.wait_for_row(busy, "kept its command", running_sleep);

    // The command ends: now nothing is working, and the daemon moves on its own.
    d.interrupt(busy);
    let deadline = Instant::now() + PATIENCE;
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(d.running(), "the daemon exited instead of moving: {log}");

    // Same process, same socket, and the agent typed back into its own conversation.
    d.wait_for_screen(1, &format!("DROID-RESUMED-{SESSION}"));
    d.wait_for_row(1, "came back with its conversation", |row| {
        row.agent_session.as_deref() == Some(SESSION)
    });
    d.wait_for_row(busy, "came back", |_| true);
}

#[test]
fn a_tab_made_after_arming_holds_the_move_rather_than_being_ended() {
    let mut d = Daemon::start();
    d.send(1, "sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);
    assert!(matches!(
        d.arm(NEXT),
        Some(ControlResponse::RestartArmed { .. })
    ));

    let late = d.new_tab();
    d.send(late, "sleep 30");
    d.wait_for_row(late, "ran sleep", running_sleep);
    d.interrupt(1);

    let why = d.waiting_until(|why| !why.contains("tab 1:"));
    assert!(
        why.contains(&format!("tab {late}")) && why.contains("sleep"),
        "the wait does not name the late tab: {why}"
    );
    assert!(d.running(), "the daemon went");
    assert_eq!(d.starts(), 1, "the daemon moved under the late tab");
    d.wait_for_row(late, "kept its command", running_sleep);
}

/// While armed with something still working, every look is refused before the gate goes
/// up, so spaces and tabs are made, slept and woken as ever. This does not cover a look
/// that raised the gate and then backed out: that needs a tab to change in the moment
/// between the two looks, which this harness cannot place.
#[test]
fn a_move_that_cannot_happen_yet_leaves_spaces_and_tabs_working() {
    let d = Daemon::start();
    d.send(1, "sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);
    assert!(matches!(
        d.arm(NEXT),
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(_),
            ..
        })
    ));

    let space = match d.ask(ControlRequest::CreateWorkspace {
        id: "space".into(),
        name: "after".into(),
    }) {
        Some(ControlResponse::WorkspaceCreated { workspace, .. }) => workspace,
        other => panic!("a space was not made while armed: {other:?}"),
    };
    let tab = match d.ask(ControlRequest::CreateTerminal {
        id: "tab".into(),
        cwd: None,
        workspace: Some(space),
        name: None,
    }) {
        Some(ControlResponse::TerminalCreated { terminal, .. }) => terminal,
        other => panic!("a tab was not made while armed: {other:?}"),
    };

    // Asked until its shell has settled past starting, as a sleep test does.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let answer = d.ask(ControlRequest::SleepWorkspace {
            id: format!("sleep-{}", unique()),
            workspace: space,
            terminal: None,
        });
        if matches!(answer, Some(ControlResponse::WorkspaceAsleep { .. })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the new space never slept: {answer:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    match d.ask(ControlRequest::WakeWorkspace {
        id: "wake".into(),
        workspace: Some(space),
        terminal: None,
    }) {
        Some(ControlResponse::WorkspaceAwake { terminals: 1, .. }) => {}
        other => panic!("the space did not wake while armed: {other:?}"),
    }
    d.wait_for_row(tab, "came back", |_| true);
    assert_eq!(d.starts(), 1);
}

#[test]
fn a_binary_on_disk_saying_another_version_is_refused() {
    let mut d = Daemon::start_installed();

    // The version asked for is newer, and not the one on disk.
    match d.arm(NEXT) {
        Some(ControlResponse::Failed { reason, .. }) => {
            assert!(reason.contains(VERSION), "{reason}")
        }
        other => panic!("armed for a version that is not on disk: {other:?}"),
    }

    // The file is replaced under the running daemon by one saying another version: the
    // path it started from is what is asked, not the file it is running.
    replace_with_version(&d.exe, "9.9.9-test");
    match d.arm(NEXT) {
        Some(ControlResponse::Failed { reason, .. }) => {
            assert!(reason.contains("9.9.9-test"), "{reason}")
        }
        other => panic!("armed though the disk says another version: {other:?}"),
    }
    assert!(d.running(), "the daemon went");
    assert_eq!(d.starts(), 1);
}

#[test]
fn a_binary_replaced_after_arming_is_not_moved_onto() {
    let mut d = Daemon::start();
    d.send(1, "sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);
    assert!(matches!(
        d.arm(NEXT),
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(_),
            ..
        })
    ));

    // Armed for this version; then the file says another, and only then is nothing working.
    replace_with_version(&d.exe, "9.9.9-test");
    d.interrupt(1);
    let deadline = Instant::now() + PATIENCE;
    let log = loop {
        let log = std::fs::read_to_string(&d.log).unwrap_or_default();
        if log.contains("9.9.9-test") {
            break log;
        }
        assert!(
            Instant::now() < deadline,
            "the move never looked at the file: {log}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    std::thread::sleep(Duration::from_secs(3));
    assert!(d.running(), "the daemon went: {log}");
    assert_eq!(
        d.starts(),
        1,
        "the daemon moved onto a binary nobody asked for: {log}"
    );
    // The tab is untouched, and the gate is down: a new tab still opens.
    d.wait_for_row(1, "stayed", |_| true);
    let late = d.new_tab();
    d.wait_for_row(late, "opened", |_| true);

    // Called off once, and nothing looks again: the new tab and its command would have
    // woken an armed task into another look, and its answer into the log.
    d.send(late, "true");
    std::thread::sleep(Duration::from_secs(5));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(log.matches("is called off").count(), 1, "{log}");
    let after = log.split("is called off").nth(1).unwrap_or_default();
    assert!(!after.contains("qorod: the move"), "it looked again: {log}");
}

/// A tab put to sleep on its own stays asleep across a move: not looked at, not ended,
/// not written down again, and brought back asleep by the new daemon, from where it wakes
/// as it would have.
#[test]
fn a_tab_asleep_stays_asleep_across_a_move() {
    let mut d = Daemon::start();
    let resting = d.new_tab();
    let space = d
        .agents()
        .into_iter()
        .find(|row| row.terminal == resting)
        .map(|row| row.workspace)
        .expect("the new tab's space");
    // Asked until its shell has settled past starting, as a sleep test does.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let answer = d.ask(ControlRequest::SleepWorkspace {
            id: format!("sleep-{}", unique()),
            workspace: space,
            terminal: Some(resting),
        });
        if matches!(answer, Some(ControlResponse::WorkspaceAsleep { .. })) {
            break;
        }
        assert!(Instant::now() < deadline, "the tab never slept: {answer:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let sleeping = |d: &Daemon| -> Vec<u32> {
        d.spaces()
            .into_iter()
            .filter(|row| row.id == space)
            .flat_map(|row| row.sleeping.into_iter().map(|tab| tab.terminal))
            .collect()
    };
    assert_eq!(sleeping(&d), vec![resting]);

    // Nothing else is working: the move goes at once.
    let _ = d.arm(NEXT);
    let deadline = Instant::now() + PATIENCE;
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(d.running(), "the daemon exited instead of moving: {log}");

    d.wait_for_row(1, "came back", |_| true);
    assert_eq!(
        sleeping(&d),
        vec![resting],
        "the sleeping tab woke or went: {log}"
    );
    assert!(
        !d.agents().iter().any(|row| row.terminal == resting),
        "the sleeping tab runs again: {:?}",
        d.agents()
    );
    match d.ask(ControlRequest::WakeWorkspace {
        id: "wake".into(),
        workspace: Some(space),
        terminal: Some(resting),
    }) {
        Some(ControlResponse::WorkspaceAwake { terminals: 1, .. }) => {}
        other => panic!("the tab did not wake after the move: {other:?}"),
    }
    d.wait_for_row(resting, "woke", |_| true);
}

/// A request for the version already running (a second window asking after the first one
/// moved this daemon) is answered and changes nothing: no binary is asked, nothing moves.
#[test]
fn arming_for_the_version_already_running_moves_nothing() {
    let mut d = Daemon::start_installed();
    for version in [VERSION, "0.0.1", "not-a-version"] {
        match d.arm(version) {
            Some(ControlResponse::RestartArmed {
                waiting_on: None, ..
            }) => {}
            other => panic!("{version} was not answered as already there: {other:?}"),
        }
    }
    std::thread::sleep(Duration::from_secs(3));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert!(d.running(), "the daemon went: {log}");
    assert_eq!(d.starts(), 1, "the daemon moved: {log}");
    assert!(!log.contains("is on disk"), "it armed: {log}");
    d.wait_for_row(1, "stayed", |_| true);
}

/// A relay whose daemon went (stopped here, or moved onto a new binary) ends at once, even
/// with its input still open: the link above it reads its end as the daemon gone and
/// reconnects, rather than holding a dead relay until somebody types.
#[test]
fn a_relay_ends_when_its_daemon_goes_though_its_input_stays_open() {
    let d = Daemon::start();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .arg("relay")
        .env("QORO_RUNTIME_DIR", d.dir.path())
        .env("XDG_CONFIG_HOME", d.dir.path().join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the relay");
    let _input = relay.stdin.take();
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        matches!(relay.try_wait(), Ok(None)),
        "the relay ended early"
    );

    assert!(matches!(
        d.ask(ControlRequest::Shutdown { id: "stop".into() }),
        Some(ControlResponse::Stopping { .. })
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && matches!(relay.try_wait(), Ok(None)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let ended = !matches!(relay.try_wait(), Ok(None));
    let _ = relay.kill();
    let _ = relay.wait();
    assert!(
        ended,
        "the relay outlived its daemon while its input stayed open"
    );
}

/// Put a script saying `qoro <version>` where `exe` is, the way an update renames a file in.
/// Asked anything else, it runs this build: what a move onto it starts.
fn replace_with_version(exe: &Path, version: &str) {
    let next = exe.with_file_name("qoro.next");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'qoro {version}'; exit 0; fi\nexec '{}' \"$@\"\n",
        env!("CARGO_BIN_EXE_qoro")
    );
    std::fs::write(&next, script).expect("script");
    std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::rename(&next, exe).expect("replace");
}

/// The test binary at `exe`, linked when the filesystem allows and copied when it does not.
fn stage(exe: &Path) {
    let built = Path::new(env!("CARGO_BIN_EXE_qoro"));
    if std::fs::hard_link(built, exe).is_err() {
        std::fs::copy(built, exe).expect("copy the binary");
    }
}

//! A daemon asked to move onto the qoop on disk becomes it with every tab still running,
//! when the new binary can take them over; and otherwise does it only once nothing is
//! working, and every tab comes back on the new binary as it was.
//!
//! The tests of the second kind start from `Daemon::start_waiting`, whose binary on disk
//! says it cannot take tabs over, which is what every build before the handover says.
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
//! its state and conversation through `qoop hook-report` the way a real agent's hook does.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use qoop_runtime::control::{AgentRow, ControlRequest, ControlResponse, WorkspaceRow};

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
    /// A daemon with `NEXT` on disk, ready to be moved onto it, tabs and all.
    fn start() -> Self {
        let d = Self::start_installed();
        replace_with_version(&d.exe, NEXT);
        d
    }

    /// The same, with a `NEXT` that cannot take tabs over: the move waits for idle tabs.
    fn start_waiting() -> Self {
        let d = Self::start_installed();
        replace_without_handoff(&d.exe, NEXT);
        d
    }

    /// Start `qoop daemon` from a copy of this build of its own, still this version.
    fn start_installed() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let installed = dir.path().join("installed");
        std::fs::create_dir_all(&installed).expect("installed");
        let exe = installed.join("qoop");
        stage(&exe);
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let log = dir.path().join("qoopd.log");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let child = Command::new(&exe)
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .env("SHELL", "/bin/sh")
            .env("PATH", path)
            .env_remove("QOOP_SESSION")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QOOP_TOKEN")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("log"))
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err()
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the daemon never listened"
        );
        qoop_testkit::open_first_terminal(&socket);
        Self {
            child,
            dir,
            bin,
            log,
            exe,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env_remove("QOOP_SESSION")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QOOP_TOKEN")
            .output()
            .expect("run qoop")
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
        let socket = qoop_testkit::socket_in(self.dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (stream, inbuf) = qoop_runtime::client::connect(&socket, "restart-test")
                .await
                .ok()?;
            let mut c = qoop_testkit::TestClient::from_parts(stream, inbuf, 1);
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
            .matches("qoopd: listening on")
            .count()
    }

    /// Whether the daemon's process is still the one this test started, not exited.
    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// A program on the daemon's `PATH`: a shell script with this body.
    fn install_program(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).expect("the program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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
            q = env!("CARGO_BIN_EXE_qoop"),
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

/// Every row of a tab's history, oldest first. The daemon answers a window of at most 500
/// rows at a time, so it is read a window after another.
fn history_of(d: &Daemon, terminal: u32) -> Vec<String> {
    let t = terminal.to_string();
    let first = d.run(&["read", &t, "--back", "0", "--lines", "1", "--json"]);
    let total = serde_json::from_slice::<serde_json::Value>(&first.stdout).expect("json")["total"]
        .as_u64()
        .expect("a total");
    let mut rows = Vec::new();
    for from in (0..total).step_by(500) {
        let up = (total - from).to_string();
        let page = d.run(&["read", &t, "--back", &up, "--lines", "500"]);
        rows.extend(text(&page.stdout).lines().map(str::to_string));
    }
    rows
}

/// Handover files left in the directory the daemon writes them in: the socket's.
fn handover_files(d: &Daemon) -> Vec<PathBuf> {
    let socket = qoop_testkit::socket_in(d.dir.path());
    let dir = socket.parent().expect("the socket's directory");
    std::fs::read_dir(dir)
        .expect("the daemon's directory")
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("handoff"))
        .map(|e| e.path())
        .collect()
}

/// The number right after the last `tag` that has digits behind it on a screen.
fn number_after(screen: &str, tag: &str) -> Option<u32> {
    screen
        .match_indices(tag)
        .filter_map(|(at, _)| {
            let digits: String = screen[at + tag.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .last()
}

/// What `ps` says of a process: its state and command line, for a failure message.
fn process_state(pid: u32) -> String {
    let out = Command::new("ps")
        .args(["-o", "pid,ppid,stat,command", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    text(&out.stdout)
}

/// The parent of a process, or `None` when there is no such process.
fn parent_of(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    text(&out.stdout).trim().parse().ok()
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
    let mut d = Daemon::start_waiting();
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
    let mut d = Daemon::start_waiting();
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
    let mut d = Daemon::start_waiting();
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
    let d = Daemon::start_waiting();
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
    let mut d = Daemon::start_waiting();
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
    assert!(!after.contains("qoopd: the move"), "it looked again: {log}");
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

/// The core of the handover: a command that is running is not waited for, not ended and not
/// restarted. It is the same process, a child of the same daemon process, and what it
/// prints after the move reaches its tab; a line typed after the move reaches it too.
#[test]
fn a_running_command_goes_through_the_move_on_the_same_pid() {
    let mut d = Daemon::start();
    d.send(
        1,
        "echo SHELL=$$; sh -c 'echo PID=$$; sleep 8; echo DONE-$$'",
    );
    d.wait_for_screen(1, "PID=");
    let screen = text(&d.run(&["read", "1"]).stdout);
    let shell = number_after(&screen, "SHELL=").expect("the shell's pid");
    let command = number_after(&screen, "PID=").expect("the command's pid");
    let daemon = d.child.id();
    assert_eq!(parent_of(shell), Some(daemon), "the shell is not ours");
    assert!(parent_of(command).is_some(), "the command is not running");

    // Armed with the command still running: the move does not wait for it.
    let armed = Instant::now();
    let _ = d.arm(NEXT);
    let deadline = Instant::now() + Duration::from_secs(20);
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    let log = format!("{log}\nthe daemon process: {}", process_state(d.child.id()));
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    // Well inside the eight seconds the command still has to run.
    assert!(
        armed.elapsed() < Duration::from_secs(4),
        "the move waited for the command: {log}"
    );
    assert!(d.running(), "the daemon exited instead of moving: {log}");
    assert!(log.contains("took over 1 of 1"), "{log}");

    // The same processes, the same parent: nothing was ended and nothing started again.
    assert_eq!(
        parent_of(shell),
        Some(daemon),
        "the shell was replaced: {log}"
    );
    assert!(
        parent_of(command).is_some(),
        "the command did not survive the move: {log}"
    );

    // What it prints later arrives in the tab, and a key typed after the move reaches it.
    d.wait_for_screen(1, &format!("DONE-{command}"));
    d.send(1, "echo AFTER-$((6*7))");
    d.wait_for_screen(1, "AFTER-42");
    assert_eq!(parent_of(shell), Some(daemon), "{log}");
    let left = handover_files(&d);
    assert!(left.is_empty(), "the handover file stayed behind: {left:?}");
}

/// An agent in the middle of a turn goes through the move as the same agent: it is not
/// resumed, its generation and its conversation are what they were, and a report it makes
/// afterwards with the token it was started with is admitted.
#[test]
fn a_working_agent_goes_through_the_move_as_the_same_agent() {
    let d = Daemon::start();
    let go = d.dir.path().join("go");
    d.install_agent(&go);
    d.send(1, "droid");
    d.wait_for_row(1, "was working in a named conversation", |row| {
        row.program.as_deref() == Some("droid")
            && row.state == "working"
            && row.agent_session.as_deref() == Some(SESSION)
    });
    let before = d
        .agents()
        .into_iter()
        .find(|row| row.terminal == 1)
        .expect("the agent's row");

    let _ = d.arm(NEXT);
    let deadline = Instant::now() + Duration::from_secs(20);
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");

    // The first thing asked after the move: nothing has had time to be read again.
    let after = d
        .agents()
        .into_iter()
        .find(|row| row.terminal == 1)
        .expect("the agent's row after the move");
    assert_eq!(after.program.as_deref(), Some("droid"), "{after:?}");
    assert_eq!(after.state, "working", "{after:?}");
    assert_eq!(after.agent_session.as_deref(), Some(SESSION), "{after:?}");
    assert_eq!(after.generation, before.generation, "{before:?} {after:?}");

    // The agent finishes its turn and says so with the token it started with.
    std::fs::write(&go, "").expect("go");
    d.wait_for_row(1, "was admitted as idle", |row| {
        row.state == "idle" && row.agent_session.as_deref() == Some(SESSION)
    });
    d.wait_for_screen(1, "DROID-IDLE");
    let screen = text(&d.run(&["read", "1"]).stdout);
    assert!(
        !screen.contains("DROID-RESUMED"),
        "the agent was started again: {screen}"
    );
    assert!(screen.contains("DROID-WORKING"), "{screen}");
}

/// The screen is in the tab the moment the move is over, before the program prints another
/// byte: it was carried, not redrawn.
#[test]
fn a_tabs_screen_is_carried_across_the_move() {
    let mut d = Daemon::start();
    d.send(1, "echo BEFORE-THE-MOVE-$((6*7))");
    d.wait_for_screen(1, "BEFORE-THE-MOVE-42");
    let before = text(&d.run(&["read", "1"]).stdout);

    let _ = d.arm(NEXT);
    let deadline = Instant::now() + Duration::from_secs(20);
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(d.running(), "the daemon exited instead of moving: {log}");

    // Nothing was typed since, and nothing was printed: this is the replay.
    let after = text(&d.run(&["read", "1"]).stdout);
    assert_eq!(before, after, "the screen changed across the move: {log}");
}

/// A full screen program that draws only when its window changes size is made to draw again
/// by the move: its screen is its own again, and its modes were never lost.
#[test]
fn a_full_screen_program_is_nudged_to_redraw_after_the_move() {
    let mut d = Daemon::start();
    d.install_program(
        "fullscreen",
        "printf '\\033[?1049h\\033[?2004h\\033[?25l'\n\
         N=0\n\
         draw() { set -- $(stty size); printf '\\033[2J\\033[H'; printf 'FRAME-%sx%s-%s' \"$2\" \"$1\" \"$N\"; }\n\
         trap 'N=$((N+1)); draw' WINCH\n\
         draw\n\
         while :; do sleep 0.2; done\n",
    );
    d.send(1, "fullscreen");
    d.wait_for_screen(1, "FRAME-80x24-0");

    let _ = d.arm(NEXT);
    let deadline = Instant::now() + Duration::from_secs(20);
    while d.starts() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");

    // Two window changes, a shrink and the size coming back, and a draw for each: the
    // program got its own screen back at the size it had.
    d.wait_for_screen(1, "FRAME-80x24-2");
    assert!(d.running(), "{log}");
}

/// Wait until the daemon has said it listens `times` times, or `patience` runs out.
fn wait_for_starts(d: &Daemon, times: usize, patience: Duration) {
    let deadline = Instant::now() + patience;
    while d.starts() < times && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The children of `parent` that have ended and not been waited for.
fn zombies_of(parent: u32) -> Vec<u32> {
    let out = Command::new("ps")
        .args(["-A", "-o", "ppid=,pid=,stat="])
        .output()
        .expect("ps");
    text(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (ppid, pid): (u32, u32) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
            f.next()?.starts_with('Z').then_some(())?;
            (ppid == parent).then_some(pid)
        })
        .collect()
}

/// Every process that is a child of `parent`.
fn children_of(parent: u32) -> Vec<u32> {
    let out = Command::new("ps")
        .args(["-A", "-o", "ppid=,pid="])
        .output()
        .expect("ps");
    text(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let (ppid, pid): (u32, u32) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?);
            (ppid == parent).then_some(pid)
        })
        .collect()
}

/// While a daemon becomes another binary, daemons started by whoever finds none listening
/// (a client, a hook, a second interface) find the socket served the whole time and the
/// startup lock held, so none of them starts: one daemon, one shell for the tab, and no
/// connection refused.
#[test]
fn a_daemon_started_during_the_move_finds_the_old_one_serving() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let mut d = Daemon::start();
    d.send(1, "echo SHELL=$$; sleep 30");
    d.wait_for_screen(1, "SHELL=");
    let screen = text(&d.run(&["read", "1"]).stdout);
    let shell = number_after(&screen, "SHELL=").expect("the shell's pid");
    let daemon = d.child.id();

    let stop = Arc::new(AtomicBool::new(false));
    let refused = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let starters = {
        let (stop, refused, errors) = (stop.clone(), refused.clone(), errors.clone());
        let dir = d.dir.path().to_path_buf();
        std::thread::spawn(move || {
            let socket = qoop_testkit::socket_in(&dir);
            let mut started = Vec::new();
            while !stop.load(Ordering::Relaxed) && started.len() < 200 {
                // Connecting every five milliseconds between the starts, which is well
                // above what any client does (`ensure_daemon` polls every twenty) and still
                // finds a gap of a few milliseconds. Not faster: connections waiting for a
                // daemon that is mid-exec queue up to the socket's backlog (128 on macOS),
                // and a hundred and more in a quarter of a second is not a client.
                for _ in 0..5 {
                    if let Err(e) = std::os::unix::net::UnixStream::connect(&socket) {
                        refused.fetch_add(1, Ordering::Relaxed);
                        errors.lock().unwrap().push(format!("{e}"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                // What `ensure_daemon` does once a connect was refused, and what a client
                // that raced the first connect does anyway.
                started.push(
                    Command::new(env!("CARGO_BIN_EXE_qoop"))
                        .arg("daemon")
                        .env("QOOP_RUNTIME_DIR", &dir)
                        .env("XDG_CONFIG_HOME", dir.join("config"))
                        .env("SHELL", "/bin/sh")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("a starter"),
                );
            }
            started
        })
    };
    std::thread::sleep(Duration::from_millis(500));
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(1));
    stop.store(true, Ordering::Relaxed);
    let mut starters = starters.join().expect("the starters");

    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(
        !log.contains("binding a new one"),
        "the socket was not passed on: {log}"
    );
    assert_eq!(
        refused.load(Ordering::Relaxed),
        0,
        "a connection was refused while the daemon moved ({:?}): {log}",
        errors.lock().unwrap().iter().take(3).collect::<Vec<_>>()
    );
    // Every starter found a daemon serving and left; none of them became one.
    let deadline = Instant::now() + Duration::from_secs(20);
    for starter in &mut starters {
        loop {
            match starter.try_wait().expect("wait") {
                Some(status) => {
                    assert!(status.success(), "a starter failed: {status}");
                    break;
                }
                None if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                None => {
                    let _ = starter.kill();
                    panic!("a starter never left: it is a second daemon");
                }
            }
        }
    }
    assert!(d.running(), "{log}");
    assert_eq!(parent_of(shell), Some(daemon), "the shell was replaced");
    assert_eq!(
        children_of(daemon),
        vec![shell],
        "the tab has more than the one shell: {log}"
    );
    assert_eq!(d.agents().len(), 1);
}

/// Output that goes on through the move is all there afterwards, once: every line of 600, in
/// bursts of twenty a tenth of a second apart, is in the screen or in the history, none twice.
#[test]
fn a_stream_of_lines_through_the_move_loses_none_and_repeats_none() {
    let d = Daemon::start();
    d.send(
        1,
        "i=1; while [ $i -le 600 ]; do echo LINE-$i; \
         if [ $((i % 20)) = 0 ]; then sleep 0.1; fi; i=$((i+1)); done; echo ENDED-600",
    );
    // The first burst is on its way; the stream goes on for about three seconds more.
    d.wait_for_screen(1, "LINE-");
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(20));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    d.wait_for_screen(1, "ENDED-600");
    // The history writer puts what scrolled up on disk within half a second.
    std::thread::sleep(Duration::from_secs(2));

    let screen = text(&d.run(&["read", "1"]).stdout);
    let back = history_of(&d, 1);
    let mut seen = std::collections::BTreeMap::<u32, usize>::new();
    for row in back.iter().map(String::as_str).chain(screen.lines()) {
        if let Some(n) = row
            .trim()
            .strip_prefix("LINE-")
            .and_then(|n| n.parse().ok())
        {
            *seen.entry(n).or_default() += 1;
        }
    }
    let lost: Vec<u32> = (1..=600).filter(|n| !seen.contains_key(n)).collect();
    let repeated: Vec<_> = seen.iter().filter(|(_, times)| **times > 1).collect();
    assert!(
        lost.is_empty(),
        "lines lost across the move: {lost:?}\n{log}"
    );
    assert!(repeated.is_empty(), "lines repeated: {repeated:?}\n{log}");
}

/// A shell that ends while the daemon moves is a tab closed, as it is when nobody moves: it
/// does not come back as a new shell in its place.
#[test]
fn a_shell_that_ends_while_the_daemon_moves_closes_its_tab() {
    let mut d = Daemon::start_installed();
    let other = d.new_tab();
    d.send(1, "echo SHELL=$$");
    d.wait_for_screen(1, "SHELL=");
    let screen = text(&d.run(&["read", "1"]).stdout);
    let shell = number_after(&screen, "SHELL=").expect("the shell's pid");
    // While every tab is paused, between the dry run and the exec, the shell is killed.
    install_script(
        &d.exe,
        NEXT,
        &format!(
            "if [ \"$2\" = --adopt-check ]; then '{}' \"$@\"; r=$?; kill -9 {shell}; sleep 0.3; exit $r; fi\n",
            env!("CARGO_BIN_EXE_qoop")
        ),
    );
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(20));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(log.contains("ended while the daemon moved"), "{log}");
    let tabs: Vec<u32> = d.agents().iter().map(|row| row.terminal).collect();
    assert_eq!(tabs, vec![other], "the closed tab came back: {log}");
    assert!(d.running(), "{log}");
}

/// The worst case of the worst case: the dry run passed and the file is not what was promised
/// by the time the new image reads it (its nonce changed). The tabs are lost with their ptys,
/// their shells are waited for, and the agent in one is resumed into its conversation from the
/// row written down just before the move, as after any restart.
#[test]
fn a_handover_refused_after_the_dry_run_still_resumes_the_agent() {
    let mut d = Daemon::start_installed();
    install_script(
        &d.exe,
        NEXT,
        &format!(
            "if [ \"$2\" = --adopt-check ]; then '{}' \"$@\"; r=$?; \
             sed 's/\"nonce\":\"[0-9a-f]*\"/\"nonce\":\"changed\"/' \"$3\" > \"$3.new\"; \
             chmod 600 \"$3.new\"; mv \"$3.new\" \"$3\"; exit $r; fi\n",
            env!("CARGO_BIN_EXE_qoop")
        ),
    );
    let go = d.dir.path().join("go");
    d.install_agent(&go);
    d.send(1, "droid");
    d.wait_for_row(1, "was working in a named conversation", |row| {
        row.program.as_deref() == Some("droid")
            && row.state == "working"
            && row.agent_session.as_deref() == Some(SESSION)
    });
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(30));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never came back: {log}");
    assert!(log.contains("the handover was refused"), "{log}");
    d.wait_for_screen(1, &format!("DROID-RESUMED-{SESSION}"));
    assert!(d.running(), "{log}");
    // The shell that lost its terminal with the refused file was waited for.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(zombies_of(d.child.id()), Vec::<u32>::new(), "{log}");
}

/// A hand over that keeps failing for a reason that does not go away does not park the tabs
/// for ever: after three tries the move is made the old way. Here the startup lock is held
/// from outside, so every try gives up on it before anything is paused.
#[test]
fn a_hand_over_that_keeps_failing_gives_way_to_the_old_move() {
    let mut d = Daemon::start();
    d.send(1, "echo SHELL=$$");
    d.wait_for_screen(1, "SHELL=");
    // The lock sits beside the socket, in the directory the daemon runs from.
    let socket = qoop_testkit::socket_in(d.dir.path());
    let lock = qoop_runtime::StartupLock::acquire(&qoop_runtime::paths::lock_path(
        socket.parent().expect("the socket's directory"),
    ))
    .expect("the startup lock");
    let _ = d.arm(NEXT);
    let deadline = Instant::now() + Duration::from_secs(90);
    let log = loop {
        let log = std::fs::read_to_string(&d.log).unwrap_or_default();
        if log.contains("3 hand overs failed in a row") || Instant::now() >= deadline {
            break log;
        }
        // A daemon looks again when a tab changes, and at the latest after a minute: a
        // command now and then is what keeps the tries coming.
        d.send(1, "sleep 0.3");
        std::thread::sleep(Duration::from_millis(1500));
    };
    assert!(log.contains("3 hand overs failed in a row"), "{log}");
    assert_eq!(d.starts(), 1, "{log}");
    // The new daemon takes the lock as any does, once it is let go; the old way needs none.
    drop(lock);
    wait_for_starts(&d, 2, Duration::from_secs(60));
    assert_eq!(d.starts(), 2, "the daemon never moved the old way: {log}");
    assert!(d.running());
    d.wait_for_row(1, "came back", |_| true);
}

/// A binary that reads another layout than the one this daemon writes is moved onto the old
/// way: it waits for the agent to be idle, and the agent is resumed in its conversation.
#[test]
fn a_binary_that_reads_another_layout_gets_the_old_move() {
    let mut d = Daemon::start_installed();
    install_script(
        &d.exe,
        NEXT,
        "if [ \"$1\" = --handoff-formats ]; then echo 2; exit 0; fi\n",
    );
    let go = d.dir.path().join("go");
    d.install_agent(&go);
    d.send(1, "droid");
    d.wait_for_row(1, "was working in a named conversation", |row| {
        row.program.as_deref() == Some("droid")
            && row.state == "working"
            && row.agent_session.as_deref() == Some(SESSION)
    });
    match d.arm(NEXT) {
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => assert!(why.contains("droid is working"), "{why}"),
        other => panic!("the move did not wait for the agent: {other:?}"),
    }
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(d.starts(), 1, "the daemon moved under a working agent");

    std::fs::write(&go, "").expect("go");
    wait_for_starts(&d, 2, PATIENCE);
    d.wait_for_screen(1, &format!("DROID-RESUMED-{SESSION}"));
    assert!(d.running());
}

/// A binary that says it reads the layout and then cannot is found out before anything is
/// given up: the tabs stay where they are, and the move goes the old way.
#[test]
fn a_binary_that_cannot_read_the_handover_is_not_moved_onto_with_it() {
    let mut d = Daemon::start_installed();
    install_script(
        &d.exe,
        NEXT,
        "if [ \"$2\" = --adopt-check ]; then exit 1; fi\n",
    );
    d.send(1, "echo SHELL=$$");
    d.wait_for_screen(1, "SHELL=");
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(60));
    let log = std::fs::read_to_string(&d.log).unwrap_or_default();
    assert_eq!(d.starts(), 2, "the daemon never moved: {log}");
    assert!(
        log.contains("cannot read what this daemon hands over"),
        "{log}"
    );
    assert!(
        !log.contains("took over"),
        "tabs were handed over anyway: {log}"
    );
    assert!(d.running(), "{log}");
    // The old way: the tab is a new shell, not the one that was there.
    d.wait_for_row(1, "came back", |_| true);
}

/// An exec that fails leaves the daemon where it was, with every tab alive and every client
/// served, and a good binary after it is moved onto as usual.
#[test]
fn a_move_whose_exec_fails_leaves_the_tabs_running_in_the_old_daemon() {
    let mut d = Daemon::start_installed();
    // After the dry run says yes, the file stops being runnable: the exec itself fails.
    install_script(
        &d.exe,
        NEXT,
        &format!(
            "if [ \"$2\" = --adopt-check ]; then '{}' \"$@\"; r=$?; chmod a-x \"$0\"; exit $r; fi\n",
            env!("CARGO_BIN_EXE_qoop")
        ),
    );
    d.send(1, "echo SHELL=$$; sleep 30");
    d.wait_for_screen(1, "SHELL=");
    let screen = text(&d.run(&["read", "1"]).stdout);
    let shell = number_after(&screen, "SHELL=").expect("the shell's pid");
    let daemon = d.child.id();

    // The asker hears that nothing will move, instead of waiting out its first look and
    // being told the move is only waiting.
    let started = Instant::now();
    match d.arm(NEXT) {
        Some(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => assert!(why.contains("could not be started"), "{why}"),
        other => panic!("the asker was not told the exec failed: {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "it waited out its first look"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let log = loop {
        let log = std::fs::read_to_string(&d.log).unwrap_or_default();
        if log.contains("could not be started") || Instant::now() >= deadline {
            break log;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(log.contains("could not be started"), "{log}");
    assert!(log.contains("staying on this version"), "{log}");
    assert!(d.running(), "{log}");
    assert_eq!(d.starts(), 1, "{log}");
    assert_eq!(parent_of(shell), Some(daemon), "the shell was lost: {log}");

    // The failed exec left SIGPIPE as the exec would have wanted it. A daemon that goes on
    // has to ignore it again, or the first write to a client that went away ends it.
    let sent = Command::new("kill")
        .args(["-PIPE", &daemon.to_string()])
        .status()
        .expect("kill");
    assert!(sent.success());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        d.running(),
        "the daemon died of SIGPIPE after a failed exec"
    );

    // Still serving: a key reaches the tab, and a new tab opens (the gate is down).
    d.send(1, "echo STILL-$((6*7))");
    d.wait_for_screen(1, "STILL-42");
    let late = d.new_tab();
    d.wait_for_row(late, "opened", |_| true);
    let left = handover_files(&d);
    assert!(left.is_empty(), "the handover file stayed behind: {left:?}");

    // A binary that can be run is moved onto, with the same shell.
    replace_with_version(&d.exe, NEXT);
    let _ = d.arm(NEXT);
    wait_for_starts(&d, 2, Duration::from_secs(20));
    assert_eq!(d.starts(), 2, "the daemon never moved after it: {log}");
    assert_eq!(
        parent_of(shell),
        Some(daemon),
        "the shell was lost on the second try"
    );
}

/// A daemon started to adopt a handover it cannot read, because it is not there, is not what
/// was promised, or was left behind by another process, starts as any daemon does: it
/// listens, and it restores from its rows.
#[test]
fn a_handover_that_cannot_be_adopted_starts_a_daemon_as_usual() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let stale = dir.path().join("handoff-1.json");
    std::fs::write(
        &stale,
        r#"{"format":1,"pid":1,"nonce":"n","terminals":[{"id":1,"fd":9,"child":1,"child_start":null,"tty_dev":0,"workspace":1,"name":null,"cwd":null,"token":"t","cols":80,"rows":24,"pending":[],"screen":"","nudge":false,"agent":null,"session":null,"session_said":null}]}"#,
    )
    .expect("file");
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let log = dir.path().join("qoopd.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_qoop"))
        .args(["daemon", "--adopt"])
        .arg(&stale)
        .args(["--nonce", "n"])
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("SHELL", "/bin/sh")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).expect("log"))
        .spawn()
        .expect("spawn the daemon");
    let socket = qoop_testkit::socket_in(dir.path());
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let listening = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    let _ = child.kill();
    let _ = child.wait();
    let log = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(listening, "the daemon never listened: {log}");
    assert!(log.contains("the handover was refused"), "{log}");
    assert!(!stale.exists(), "the refused file stayed behind");
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
    let mut relay = Command::new(env!("CARGO_BIN_EXE_qoop"))
        .arg("relay")
        .env("QOOP_RUNTIME_DIR", d.dir.path())
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

/// A release of `NEXT` in a `file://` base under the daemon's directory: a script saying so,
/// and running this build for anything else, for every platform `qoop update` may ask for.
/// With `handoff` false it says nothing when asked which handover layouts it reads, as a build
/// from before the handover does.
fn serve_release(d: &Daemon, handoff: bool) -> PathBuf {
    let base = d.dir.path().join("rel");
    let pack = d.dir.path().join("pack");
    for dir in [&base, &pack] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let fresh = pack.join("qoop");
    std::fs::write(
        &fresh,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'qoop {NEXT}'; exit 0; fi\n{}exec '{}' \"$@\"\n",
            if handoff {
                ""
            } else {
                "if [ \"$1\" = --handoff-formats ]; then exit 0; fi\n"
            },
            env!("CARGO_BIN_EXE_qoop")
        ),
    )
    .expect("script");
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let tarball = pack.join("release.tar.gz");
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("qoop")
        .current_dir(&pack)
        .status()
        .expect("tar");
    assert!(tar.success());
    let assets: Vec<_> = [
        "qoop-aarch64-apple-darwin.tar.gz",
        "qoop-x86_64-apple-darwin.tar.gz",
        "qoop-x86_64-unknown-linux-gnu.tar.gz",
        "qoop-aarch64-unknown-linux-gnu.tar.gz",
    ]
    .iter()
    .map(|name| serde_json::json!({"name": name, "url": format!("file://{}", tarball.display())}))
    .collect();
    std::fs::write(
        base.join("latest"),
        serde_json::json!({"tag_name": format!("v{NEXT}"), "assets": assets}).to_string(),
    )
    .expect("release");
    base
}

/// `qoop update` as the daemon's own installed copy, in a home and a list of remotes that
/// are empty, so it can reach nothing but this daemon.
fn update(d: &Daemon, handoff: bool) -> Output {
    let release = serve_release(d, handoff);
    let home = d.dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(d.dir.path().join("remotes.json"), "[]").expect("remotes");
    Command::new(&d.exe)
        .arg("update")
        .env("HOME", &home)
        .env("QOOP_RUNTIME_DIR", d.dir.path())
        .env("XDG_CONFIG_HOME", d.dir.path().join("config"))
        .env("QOOP_REMOTES", d.dir.path().join("remotes.json"))
        .env("QOOP_RELEASE_BASE", format!("file://{}", release.display()))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run qoop update")
}

/// `qoop update` with every tab at its prompt installs the newest release, and the daemon
/// moves onto it on the same process, which is the only way to tell it from a stop.
///
/// The release here is a stand-in that runs this very build, so the daemon that comes back
/// still says it is `VERSION`, not `NEXT`: `qoop update` looks for the new daemon, finds one
/// that does not say it is the version it was told to move to, and reports exactly that,
/// and fails. It is the one honest answer to a daemon that closed its connection and came
/// back as something else; a real release would say `NEXT` and read "moving to NEXT now".
#[test]
fn an_update_moves_an_idle_daemon_onto_the_new_binary() {
    let mut d = Daemon::start_installed();
    let out = update(&d, true);
    let said = text(&out.stdout);
    assert!(
        said.contains(&format!(
            "this machine: closed the connection and runs {VERSION}, not {NEXT}"
        )),
        "{said}"
    );
    assert!(
        !out.status.success(),
        "a daemon that is not the version asked for is a failure"
    );
    assert!(d.starts() >= 2, "the daemon never moved: {said}");
    assert!(d.running(), "the daemon was stopped, not moved");
}

/// With the new binary able to take tabs over, `qoop update` does not wait for a running
/// command: the daemon moves at once, the shell and the command are the same processes, and
/// the command is still running.
#[test]
fn an_update_keeps_a_running_command_running_when_the_tabs_can_be_handed_over() {
    let mut d = Daemon::start_installed();
    d.send(1, "echo SHELL=$$; sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);
    let screen = text(&d.run(&["read", "1"]).stdout);
    let shell = number_after(&screen, "SHELL=").expect("the shell's pid");
    let daemon = d.child.id();
    let out = update(&d, true);
    let said = text(&out.stdout);
    wait_for_starts(&d, 2, Duration::from_secs(20));
    assert_eq!(d.starts(), 2, "the daemon did not move at once: {said}");
    assert!(d.running(), "the daemon was stopped, not moved: {said}");
    assert_eq!(
        parent_of(shell),
        Some(daemon),
        "the shell was replaced: {said}"
    );
    d.wait_for_row(1, "kept its command", running_sleep);
}

/// A command running in a tab does not stop `qoop update`; it is named, left alone, and the
/// move comes once the command is done, when the new binary cannot take the tabs over.
#[test]
fn an_update_names_a_running_command_and_moves_when_it_ends() {
    let mut d = Daemon::start_installed();
    d.send(1, "sleep 30");
    d.wait_for_row(1, "ran sleep", running_sleep);
    let out = update(&d, false);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let said = text(&out.stdout);
    assert!(
        said.contains(&format!(
            "this machine: moves to {NEXT} once nothing is working; waiting on tab 1"
        )) && said.contains("sleep"),
        "{said}"
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        d.running() && d.starts() == 1,
        "the daemon moved under a command"
    );
    d.wait_for_row(1, "kept its command", running_sleep);
    d.interrupt(1);
    let deadline = Instant::now() + PATIENCE;
    while d.starts() < 2 {
        assert!(
            Instant::now() < deadline,
            "the daemon never moved after the command"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(d.running(), "the daemon was stopped, not moved");
}

/// Put a script saying `qoop <version>` where `exe` is, the way an update renames a file in.
/// Asked anything else, it runs this build: what a move onto it starts.
fn replace_with_version(exe: &Path, version: &str) {
    install_script(exe, version, "");
}

/// The same, for a build from before the handover: asked which layouts it takes over from,
/// it says nothing.
fn replace_without_handoff(exe: &Path, version: &str) {
    install_script(
        exe,
        version,
        "if [ \"$1\" = --handoff-formats ]; then exit 0; fi\n",
    );
}

fn install_script(exe: &Path, version: &str, extra: &str) {
    let next = exe.with_file_name("qoop.next");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'qoop {version}'; exit 0; fi\n{extra}exec '{}' \"$@\"\n",
        env!("CARGO_BIN_EXE_qoop")
    );
    std::fs::write(&next, script).expect("script");
    std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::rename(&next, exe).expect("replace");
}

/// The test binary at `exe`, linked when the filesystem allows and copied when it does not.
fn stage(exe: &Path) {
    let built = Path::new(env!("CARGO_BIN_EXE_qoop"));
    if std::fs::hard_link(built, exe).is_err() {
        std::fs::copy(built, exe).expect("copy the binary");
    }
}

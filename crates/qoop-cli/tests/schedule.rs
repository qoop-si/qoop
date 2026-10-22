//! `qoop schedule` against a real daemon, through the real socket, with a stand-in `claude`
//! (a shell script on the PATH that writes the arguments it was started with to a file).
//!
//! What is asserted is what the owner relies on: a schedule starts the agent at its time, with
//! the prompt, in a terminal that is listed with the schedule; a slot that came due while qoop
//! was down is fired once and never again; pause and remove survive a restart; a worker's
//! terminal cannot schedule an agent and qass's can; and a daemon with an active schedule does
//! not leave, where an empty one does.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// Longer than the daemon's ten seconds of quiet, with room for a slow machine.
const LEAVES_WITHIN: Duration = Duration::from_secs(30);

struct Stack {
    child: Option<Child>,
    dir: tempfile::TempDir,
}

impl Stack {
    /// A runtime directory with a stand-in `claude` and `qoop` on its PATH, and a daemon.
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qoop"), bin.join("qoop")).unwrap();
        let agent = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            dir.path().join("claude.args").display()
        );
        std::fs::write(bin.join("claude"), agent).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let mut stack = Self { child: None, dir };
        stack.spawn_daemon();
        stack
    }

    fn command(&self) -> std::process::Command {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("SHELL", "/bin/sh")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.path().join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QORO_TERMINAL", "QORO_TOKEN"] {
            cmd.env_remove(var);
        }
        cmd
    }

    /// The daemon, started here and not by a command, so that it is this test's to stop.
    fn spawn_daemon(&mut self) {
        let child = self
            .command()
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        self.child = Some(child);
        let socket = qoop_testkit::socket_in(self.dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
    }

    /// `qoop stop`, and the daemon's process gone.
    fn stop_daemon(&mut self) {
        let (ok, _, err) = self.run(&["stop"]);
        assert!(ok, "stop failed: {err}");
        let mut child = self.child.take().expect("a daemon");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if child.try_wait().expect("wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the daemon did not stop");
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `qoop` from outside every terminal.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.command().args(args).output().expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn wait(&self, mut until: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(40);
        while Instant::now() < deadline {
            if until() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Run a command line in terminal `terminal`'s shell, as a worker or qass would, and wait.
    fn inside(&self, terminal: &str, line: &str) -> String {
        let out = self.path("inside.out");
        let done = self.path("inside.out.done");
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&done);
        let full = format!("{line} > '{0}' 2>&1; touch '{0}.done'", out.display());
        let (ok, _, err) = self.run(&["send", terminal, &full, "--enter"]);
        assert!(ok, "send failed: {err}");
        assert!(self.wait(|| done.exists()), "{line} never finished");
        std::fs::read_to_string(out).unwrap_or_default()
    }

    /// A new shell in `space`. Its number.
    fn shell(&self, space: &str, name: &str) -> String {
        let (ok, out, err) = self.run(&["new", "--name", name, "--space", space]);
        assert!(ok, "new failed: {err}");
        out.trim().to_string()
    }

    /// `qoop schedule ls --json`. Also the way to know the daemon has decided what came due
    /// while it was away: a client is only accepted after its first look.
    fn schedules(&self) -> Vec<serde_json::Value> {
        let (ok, out, err) = self.run(&["schedule", "ls", "--json"]);
        assert!(ok, "schedule ls failed: {err}");
        serde_json::from_str::<serde_json::Value>(&out)
            .expect("json")
            .as_array()
            .cloned()
            .expect("an array")
    }

    fn schedule(&self, name: &str) -> Option<serde_json::Value> {
        self.schedules().into_iter().find(|s| s["name"] == name)
    }

    /// How many times the stand-in agent was started with `prompt`.
    fn starts(&self, prompt: &str) -> usize {
        std::fs::read_to_string(self.path("claude.args"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == prompt)
            .count()
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn a_schedule_starts_the_agent_at_its_time_with_its_prompt() {
    let stack = Stack::start();
    let (ok, out, err) = stack.run(&[
        "schedule",
        "add",
        "morning",
        "--at",
        "+3s",
        "--",
        "check the inbox",
    ]);
    assert!(ok, "add failed: {err}");
    assert!(out.starts_with("added 1 morning: once "), "{out}");
    let listed = stack.schedule("morning").expect("it is listed");
    assert!(listed["next_at"].is_number(), "{listed}");
    assert_eq!(stack.starts("check the inbox"), 0, "not before its time");

    assert!(
        stack.wait(|| stack.starts("check the inbox") == 1),
        "the agent was never started: {:?}",
        stack.schedule("morning")
    );
    assert!(
        stack.wait(|| stack
            .schedule("morning")
            .is_some_and(|s| s["last_result"] == "started")),
        "the run never reported: {:?}",
        stack.schedule("morning")
    );
    let ran = stack.schedule("morning").unwrap();
    let terminal = ran["last_terminal"].as_u64().expect("its terminal");
    let (_, agents, _) = stack.run(&["agents"]);
    assert!(
        agents.contains(&format!("term={terminal} ")),
        "the terminal of the run is not open: {agents}"
    );
    assert!(ran["next_at"].is_null(), "a one-off is done: {ran}");

    // `run` starts another, by hand, and leaves the schedule's slot alone.
    let (ok, _, err) = stack.run(&["schedule", "run", "morning"]);
    assert!(ok, "run failed: {err}");
    assert!(
        stack.wait(|| stack.starts("check the inbox") == 2),
        "a run by hand never started the agent"
    );
    let after = stack.schedule("morning").unwrap();
    assert_eq!(after["last_slot"], ran["last_slot"]);
}

#[test]
fn a_slot_missed_while_qoop_was_down_fires_once_and_never_again() {
    let mut stack = Stack::start();
    let (ok, _, err) = stack.run(&["schedule", "add", "late", "--at", "+4s", "--", "catch up"]);
    assert!(ok, "add failed: {err}");
    stack.stop_daemon();
    // Past its time, with nobody to fire it.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(stack.starts("catch up"), 0);

    // Started again. A client is accepted only after the daemon's first look, so by the time
    // `ls` answers the slot has been claimed.
    stack.spawn_daemon();
    let claimed = stack.schedule("late").expect("it survived the stop");
    assert!(
        claimed["last_slot"].is_number(),
        "not claimed at start: {claimed}"
    );
    assert!(
        stack.wait(|| stack.starts("catch up") == 1),
        "the missed run never started"
    );
    assert!(
        stack.wait(|| stack
            .schedule("late")
            .is_some_and(|s| s["last_result"] == "started")),
        "{:?}",
        stack.schedule("late")
    );

    // And again: nothing is caught up twice.
    let slot = stack.schedule("late").unwrap()["last_slot"].clone();
    stack.stop_daemon();
    stack.spawn_daemon();
    assert_eq!(stack.schedule("late").unwrap()["last_slot"], slot);
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(stack.starts("catch up"), 1, "it was fired again");
}

#[test]
fn pause_and_remove_survive_a_restart() {
    let mut stack = Stack::start();
    for name in ["keep", "drop"] {
        let (ok, _, err) = stack.run(&["schedule", "add", name, "--at", "+1h", "--", "later"]);
        assert!(ok, "add failed: {err}");
    }
    let (ok, _, err) = stack.run(&["schedule", "pause", "keep"]);
    assert!(ok, "pause failed: {err}");
    let (ok, _, err) = stack.run(&["schedule", "rm", "drop"]);
    assert!(ok, "rm failed: {err}");
    stack.stop_daemon();
    stack.spawn_daemon();
    let all = stack.schedules();
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0]["name"], "keep");
    assert_eq!(all[0]["paused"], true);
    assert!(all[0]["next_at"].is_null(), "a paused one has no next run");
    let (ok, _, _) = stack.run(&["schedule", "resume", "keep"]);
    assert!(ok);
    assert!(stack.schedule("keep").unwrap()["next_at"].is_number());
    // Names are unique, and an id is never a name.
    let (ok, _, err) = stack.run(&["schedule", "add", "keep", "--at", "+1h", "--", "x"]);
    assert!(!ok && err.contains("already a schedule named"), "{err}");
    let (ok, _, err) = stack.run(&["schedule", "add", "12", "--at", "+1h", "--", "x"]);
    assert!(!ok && err.contains("only digits"), "{err}");
}

#[test]
fn a_workers_terminal_cannot_schedule_an_agent_and_qass_can() {
    let stack = Stack::start();
    let worker = stack.shell("work", "worker");
    let said = stack.inside(&worker, "qoop schedule add sneaky --at +1h -- do harm");
    assert!(
        said.contains("not from a worker's terminal"),
        "a worker was let schedule: {said}"
    );
    assert!(stack.schedule("sneaky").is_none(), "it was stored anyway");
    // Nor can it pause or remove what is there.
    let (ok, _, err) = stack.run(&["schedule", "add", "mine", "--at", "+1h", "--", "x"]);
    assert!(ok, "{err}");
    for verb in ["pause", "rm", "run"] {
        let said = stack.inside(&worker, &format!("qoop schedule {verb} mine"));
        assert!(
            said.contains("not from a worker's terminal"),
            "{verb}: {said}"
        );
    }
    assert_eq!(stack.schedule("mine").unwrap()["paused"], false);

    let qass = stack.shell("qass", "qass-shell");
    let said = stack.inside(&qass, "qoop schedule add from-qass --at +1h -- fine");
    assert!(said.contains("added "), "qass was refused: {said}");
    assert!(stack.schedule("from-qass").is_some());
}

#[test]
fn a_daemon_with_an_active_schedule_does_not_leave_and_one_without_does() {
    let mut stack = Stack::start();
    // Nothing awake and nobody attached once the command is done: only the schedule holds it.
    let (ok, _, err) = stack.run(&["schedule", "add", "later", "--at", "+1h", "--", "x"]);
    assert!(ok, "add failed: {err}");
    std::thread::sleep(Duration::from_secs(15));
    let child = stack.child.as_mut().expect("a daemon");
    assert!(
        child.try_wait().expect("wait").is_none(),
        "the daemon left with a schedule waiting for its time"
    );
    // The last one removed lets it go.
    let (ok, _, err) = stack.run(&["schedule", "rm", "later"]);
    assert!(ok, "rm failed: {err}");
    let deadline = Instant::now() + LEAVES_WITHIN;
    let mut left = false;
    while Instant::now() < deadline {
        if stack
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .expect("wait")
            .is_some()
        {
            left = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        left,
        "the daemon stayed after the last schedule was removed"
    );
}

/// A prompt is typed into a shell, so what a shell would read in it (a quote, `$`, a backtick,
/// a line that starts with a dash) must reach the agent as written, and a prompt that is all
/// flag must not become the agent's flag.
#[test]
fn a_prompt_reaches_the_agent_as_written() {
    let stack = Stack::start();
    let prompt = "it's $HOME and `whoami` \"quoted\"\n- then this line";
    let (ok, _, err) = stack.run(&["schedule", "add", "odd", "--at", "+2s", "--", prompt]);
    assert!(ok, "add failed: {err}");
    let flag = "--dangerously-skip-permissions";
    let (ok, _, err) = stack.run(&["schedule", "add", "flag", "--at", "+2s", "--", flag]);
    assert!(ok, "add failed: {err}");
    let args = || std::fs::read_to_string(stack.path("claude.args")).unwrap_or_default();
    assert!(
        stack.wait(|| args().contains(prompt) && args().contains(&format!(" {flag}"))),
        "the agents never got their prompts as written: {:?}",
        args()
    );
    assert!(
        !args().lines().any(|line| line == flag),
        "a prompt became the agent's flag: {:?}",
        args()
    );
    let (ok, _, err) = stack.run(&["schedule", "add", "tab", "--at", "+1m", "--", "a\tb"]);
    assert!(
        !ok && err.contains("control keys"),
        "a tab was let in: {err}"
    );
}

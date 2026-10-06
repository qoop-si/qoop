//! `qoro task` against a live daemon, through the real socket.
//!
//! The positive path runs inside a terminal of the test daemon, typed into its shell, so the
//! token is the real one the daemon gave that terminal and never leaves it. Everything else
//! (reading, closing by id, refusals) runs from outside, as qass or a person would.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
}

fn start_in(dir: &Path) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .arg("daemon")
        .env("QORO_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("SHELL", "/bin/sh")
        .env_remove("QORO_TERMINAL")
        .env_remove("QORO_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let socket = qoro_testkit::socket_in(dir);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !socket.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(socket.exists(), "the daemon never bound its socket");
    child
}

impl Daemon {
    fn start(dir: &tempfile::TempDir) -> Self {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let child = start_in(dir.path());
        qoro_testkit::open_first_terminal(&qoro_testkit::socket_in(dir.path()));
        std::thread::sleep(Duration::from_millis(500));
        Self {
            child,
            dir: dir.path().to_path_buf(),
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(qoro_testkit::socket_in(&self.dir));
        self.child = start_in(&self.dir);
        std::thread::sleep(Duration::from_millis(800));
    }

    /// `qoro` from outside every terminal of this daemon.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qoro"));
        cmd.args(args)
            .env("QORO_RUNTIME_DIR", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env_remove("QORO_TERMINAL")
            .env_remove("QORO_TOKEN");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run qoro");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Run a script inside terminal `terminal`'s shell, as its agent would, and wait for it.
    fn inside(&self, terminal: &str, name: &str, body: &str) -> PathBuf {
        let work = self.dir.join(name);
        std::fs::create_dir_all(&work).unwrap();
        let script = work.join("run.sh");
        std::fs::write(
            &script,
            format!(
                "Q='{}'\ncd '{}'\n{body}\ntouch finished\n",
                env!("CARGO_BIN_EXE_qoro"),
                work.display()
            ),
        )
        .unwrap();
        let line = format!("sh '{}'", script.display());
        let (ok, _, err) = self.run(&["send", terminal, &line, "--enter"]);
        assert!(ok, "send failed: {err}");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !work.join("finished").exists() {
            assert!(
                Instant::now() < deadline,
                "the script in terminal {terminal} never finished"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        work
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // This test's daemon, by its own handle. Never `pkill qoro daemon`.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read(work: &Path, file: &str) -> String {
    std::fs::read_to_string(work.join(file)).unwrap_or_default()
}

fn row_task(daemon: &Daemon, terminal: u64) -> serde_json::Value {
    let (ok, out, err) = daemon.run(&["agents", "--json"]);
    assert!(ok, "agents failed: {err}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).expect("json rows");
    rows.into_iter()
        .find(|r| r["terminal"] == terminal)
        .map(|r| r["task"].clone())
        .unwrap_or(serde_json::Value::Null)
}

#[test]
fn an_agent_records_its_task_and_qass_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::start(&dir);
    let work = daemon.inside(
        "1",
        "life",
        r#"$Q task new "fix the redirect" > new.out 2>&1
$Q task new "something else" > second.out 2>&1; echo $? > second.rc
printf 'Acceptance: one redirect\nPlan: fix it\nVerify: a test\n' | $Q task plan - > plan.out 2>&1
$Q task stage implementing > stage.out 2>&1
$Q task stage done > badstage.out 2>&1; echo $? > badstage.rc
$Q task evidence "cargo test --workspace: 12 passed" > evidence.out 2>&1
$Q task done "redirect fixed" > done.out 2>&1
$Q task show --json > show.json 2>&1"#,
    );
    assert_eq!(
        read(&work, "new.out").trim(),
        "task #1",
        "{}",
        read(&work, "new.out")
    );
    assert_eq!(read(&work, "second.rc").trim(), "1");
    assert!(
        read(&work, "second.out").contains("has open task #1"),
        "a second task is refused and names the first: {}",
        read(&work, "second.out")
    );
    assert_eq!(read(&work, "badstage.rc").trim(), "1");
    assert!(read(&work, "badstage.out").contains("unknown stage"));
    let show: serde_json::Value = serde_json::from_str(&read(&work, "show.json"))
        .unwrap_or_else(|e| panic!("show --json: {e}: {}", read(&work, "show.json")));
    assert_eq!(show["task"]["goal"], "fix the redirect");
    assert_eq!(show["task"]["stage"], "implementing");
    assert!(
        show["task"]["claimed_at"].is_number(),
        "done is a claim: {show}"
    );
    assert!(
        show["task"]["closed_at"].is_null(),
        "and the task stays open"
    );
    let kinds: Vec<&str> = show["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["done", "evidence", "stage", "plan", "opened"]);
    assert!(
        show["events"][1]["body"]
            .as_str()
            .unwrap()
            .contains("--workspace"),
        "evidence keeps its flags"
    );

    // The row qass watches carries it.
    let task = row_task(&daemon, 1);
    assert_eq!(task["id"], 1);
    assert_eq!(task["stage"], "implementing");
    assert_eq!(task["claimed"], true);

    // It survives the daemon going away.
    daemon.restart();
    let (ok, out, err) = daemon.run(&["task", "ls"]);
    assert!(ok, "{err}");
    assert!(
        out.contains("#1 fix the redirect"),
        "after a restart: {out}"
    );

    // qass, from its own terminal (here: outside), closes it by id.
    let (ok, _, err) = daemon.run(&["task", "close", "1", "--verified", "checked the test"]);
    assert!(ok, "close: {err}");
    let (ok, out, _) = daemon.run(&["task", "show", "1"]);
    assert!(ok);
    assert!(out.contains("(verified)"), "{out}");
    let (_, out, _) = daemon.run(&["task", "ls"]);
    assert!(out.contains("no open task"), "{out}");
    let (ok, _, err) = daemon.run(&["task", "close", "1", "--failed"]);
    assert!(
        !ok && err.contains("closed"),
        "a closed task stays closed: {err}"
    );
}

#[test]
fn refusals_name_what_is_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let (ok, _, err) = daemon.run(&["task", "new", "x"]);
    assert!(
        !ok && err.contains("QORO_TERMINAL and QORO_TOKEN are not set"),
        "{err}"
    );
    let (ok, _, err) = daemon.run_env(
        &["task", "new", "x"],
        &[("QORO_TERMINAL", "1"), ("QORO_TOKEN", "forged")],
    );
    assert!(
        !ok && err.contains("refused"),
        "a forged token is refused: {err}"
    );
    let (ok, _, err) = daemon.run(&["task", "close", "1"]);
    assert!(
        !ok && err.contains("usage"),
        "close needs an outcome: {err}"
    );
    let (ok, _, err) = daemon.run(&["task", "show", "42"]);
    assert!(!ok && err.contains("no task #42"), "{err}");
    let (ok, _, err) = daemon.run(&["task", "evidence", "x"]);
    assert!(!ok && err.contains("are not set"), "{err}");
}

#[test]
fn rules_print_the_open_task_and_nothing_in_qass_space() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let work = daemon.inside(
        "1",
        "rules",
        r#"$Q task rules > before.out 2>&1
$Q task rules --turn > turn-none.out 2>&1
$Q task new "ship it" > /dev/null 2>&1
$Q task rules > after.out 2>&1
$Q task rules --turn > turn-open.out 2>&1"#,
    );
    assert!(read(&work, "before.out").starts_with("You are running inside qoro"));
    assert!(!read(&work, "before.out").contains("Your open task"));
    assert!(
        read(&work, "after.out")
            .contains("Your open task #1, as recorded earlier: \"ship it\" (stage investigating)."),
        "{}",
        read(&work, "after.out")
    );
    assert!(
        !read(&work, "after.out").contains("--replace"),
        "no invitation to replace"
    );
    assert!(read(&work, "turn-none.out").starts_with("qoro: if this request is real work"));
    assert!(
        read(&work, "turn-open.out")
            .starts_with("qoro: this terminal's task #1 is open (stage investigating)."),
        "{}",
        read(&work, "turn-open.out")
    );
    assert!(
        !read(&work, "turn-open.out").contains("--replace"),
        "{}",
        read(&work, "turn-open.out")
    );

    let (ok, out, err) = daemon.run(&["new", "--space", "qass"]);
    assert!(ok, "new --space qass: {err}");
    let terminal = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    let work = daemon.inside(
        &terminal,
        "qass",
        r#"$Q task rules > rules.out 2>&1; echo $? > rules.rc"#,
    );
    assert_eq!(read(&work, "rules.rc").trim(), "0");
    assert_eq!(
        read(&work, "rules.out"),
        "",
        "qass is not told to open a task"
    );
}

#[test]
fn rules_print_the_static_block_with_no_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .args(["task", "rules"])
        .env("QORO_RUNTIME_DIR", dir.path())
        .env("QORO_TERMINAL", "1")
        .env("QORO_TOKEN", "t")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("You are running inside qoro"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// A daemon that takes the connection and never answers costs a session start at most the
/// patience `rules` gives it, and the plain rules still print.
#[test]
fn rules_print_the_static_block_when_the_daemon_hangs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = qoro_testkit::socket_in(dir.path());
    let session = socket.parent().expect("the session directory");
    std::fs::create_dir_all(session).unwrap();
    // Private, as qoro requires of its own directory: otherwise it refuses before connecting
    // and this would test nothing.
    for private in [session, session.parent().expect("the sessions directory")] {
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).unwrap();
    let held = std::thread::spawn(move || {
        // Accept, read nothing, answer nothing, hold the connection until the end.
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut conn = None;
        while Instant::now() < deadline {
            if let Ok((c, _)) = listener.accept() {
                conn = Some(c);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        conn.is_some()
    });
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_qoro"))
        .args(["task", "rules"])
        .env("QORO_RUNTIME_DIR", dir.path())
        .env("QORO_TERMINAL", "1")
        .env("QORO_TOKEN", "t")
        .output()
        .unwrap();
    let took = started.elapsed();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("You are running inside qoro"));
    // `rules` waits 800 ms for the daemon: about 0.83 s end to end on an idle machine. The
    // bound is looser because this binary is a debug build started beside the rest of the
    // suite's daemons; what it guards is that a hung daemon is not waited on for its full
    // ten second patience.
    assert!(
        took < Duration::from_secs(3),
        "a session start waited {took:?}"
    );
    assert!(
        held.join().unwrap(),
        "qoro never connected, so nothing was tested"
    );
}

#[test]
fn a_long_history_pages_under_one_frame() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let work = daemon.inside(
        "1",
        "long",
        r#"$Q task new "many checks" > /dev/null 2>&1
line=$(printf 'x%.0s' $(seq 1 990))
i=0
while [ $i -lt 80 ]; do $Q task evidence "check $i $line" > /dev/null 2>&1; i=$((i+1)); done"#,
    );
    let _ = work;
    let (ok, out, err) = daemon.run(&["task", "show", "1", "--json"]);
    assert!(ok, "{err}");
    let first: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        first["more"], true,
        "80 KB of evidence does not fit one frame"
    );
    let events = first["events"].as_array().unwrap();
    assert!(events.len() > 10 && events.len() < 81, "{}", events.len());
    assert!(
        events[0]["body"].as_str().unwrap().starts_with("check 79 "),
        "newest first"
    );
    let last = events.last().unwrap()["id"].as_i64().unwrap().to_string();
    let (ok, out, err) = daemon.run(&["task", "show", "1", "--before", &last, "--json"]);
    assert!(ok, "{err}");
    let older: serde_json::Value = serde_json::from_str(&out).unwrap();
    let older_events = older["events"].as_array().unwrap();
    assert!(!older_events.is_empty());
    assert!(older_events[0]["id"].as_i64().unwrap() < last.parse::<i64>().unwrap());
    assert_eq!(
        older_events.last().unwrap()["kind"],
        "opened",
        "the page reaches the start"
    );
}

/// qass closes a worker's task from its own terminal: a task named by id is not checked
/// against the terminal the command runs in.
#[test]
fn a_task_closes_by_id_from_another_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    daemon.inside(
        "1",
        "worker",
        r#"$Q task new "the worker's" > /dev/null 2>&1"#,
    );
    let (ok, out, err) = daemon.run(&["new"]);
    assert!(ok, "new: {err}");
    let other = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    let work = daemon.inside(
        &other,
        "closer",
        r#"$Q task close 1 --verified "checked" > close.out 2>&1; echo $? > close.rc"#,
    );
    assert_eq!(
        read(&work, "close.rc").trim(),
        "0",
        "{}",
        read(&work, "close.out")
    );
    let (_, out, _) = daemon.run(&["task", "show", "1"]);
    assert!(out.contains("(verified)"), "{out}");
}

/// Past the most one read looks at, the answer says there is more even when it fits.
#[test]
fn many_short_events_still_say_there_is_more() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    daemon.inside(
        "1",
        "short",
        r#"$Q task new "short checks" > /dev/null 2>&1
i=0
while [ $i -lt 205 ]; do $Q task evidence "c$i" > /dev/null 2>&1; i=$((i+1)); done"#,
    );
    let (ok, out, err) = daemon.run(&["task", "show", "1", "--json"]);
    assert!(ok, "{err}");
    let shown: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(shown["events"].as_array().unwrap().len(), 200);
    assert_eq!(shown["more"], true, "206 events, 200 shown");
}

/// The record does not wait on the agent: a hook's `begin` at the first edit opens the task,
/// named after the prompt the hook reported, once however many edits follow; qass's own
/// space never gets one.
#[test]
fn a_first_edit_opens_the_task_named_after_the_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let work = daemon.inside(
        "1",
        "begin",
        r#"$Q hook-report --state working --event user --label "make greet shout" > report.out 2>&1
$Q task begin > begin1.out 2>&1; echo $? > begin1.rc
$Q task begin > begin2.out 2>&1
$Q task ls > ls.out 2>&1"#,
    );
    assert_eq!(
        read(&work, "begin1.rc").trim(),
        "0",
        "{}",
        read(&work, "begin1.out")
    );
    assert_eq!(
        read(&work, "begin1.out"),
        "",
        "a hook's begin prints nothing"
    );
    let ls = read(&work, "ls.out");
    assert!(ls.contains("#1 make greet shout (investigating)"), "{ls}");
    assert_eq!(ls.lines().count(), 1, "one task for two begins: {ls}");
    let (_, show, _) = daemon.run(&["task", "show", "1"]);
    assert!(
        show.contains("opened by qoro when the agent changed files"),
        "{show}"
    );

    let (ok, out, err) = daemon.run(&["new", "--space", "qass"]);
    assert!(ok, "{err}");
    let qass = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    daemon.inside(&qass, "qass-begin", r#"$Q task begin > /dev/null 2>&1"#);
    let (_, ls, _) = daemon.run(&["task", "ls"]);
    assert_eq!(ls.lines().count(), 1, "qass opened nothing: {ls}");
}

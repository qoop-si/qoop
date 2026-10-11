//! `qoop task` against a live daemon, through the real socket.
//!
//! The positive path runs inside a terminal of the test daemon, typed into its shell, so the
//! token is the real one the daemon gave that terminal and never leaves it. Everything else
//! (reading, closing by id, refusals) runs from outside, as qass or a person would.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    dir: PathBuf,
}

fn start_in(dir: &Path) -> Child {
    let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .arg("daemon")
        .env("QOOP_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("SHELL", "/bin/sh")
        .env_remove("QOOP_TERMINAL")
        .env_remove("QORO_TERMINAL")
        .env_remove("QOOP_TOKEN")
        .env_remove("QORO_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let socket = qoop_testkit::socket_in(dir);
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
        qoop_testkit::open_first_terminal(&qoop_testkit::socket_in(dir.path()));
        std::thread::sleep(Duration::from_millis(500));
        Self {
            child,
            dir: dir.path().to_path_buf(),
        }
    }

    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(qoop_testkit::socket_in(&self.dir));
        self.child = start_in(&self.dir);
        std::thread::sleep(Duration::from_millis(800));
    }

    /// `qoop` from outside every terminal of this daemon.
    fn run(&self, args: &[&str]) -> (bool, String, String) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.args(args)
            .env("QOOP_RUNTIME_DIR", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run qoop");
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
                env!("CARGO_BIN_EXE_qoop"),
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
        // This test's daemon, by its own handle. Never `pkill qoop daemon`.
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
        !ok && err.contains("QOOP_TERMINAL and QOOP_TOKEN are not set"),
        "{err}"
    );
    let (ok, _, err) = daemon.run_env(
        &["task", "new", "x"],
        &[("QOOP_TERMINAL", "1"), ("QOOP_TOKEN", "forged")],
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
    assert!(read(&work, "before.out").starts_with("You are running inside qoop"));
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
    assert!(read(&work, "turn-none.out").starts_with("qoop: if this request is real work"));
    assert!(
        read(&work, "turn-open.out")
            .starts_with("qoop: this terminal's task #1 is open (stage investigating)."),
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
    let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["task", "rules"])
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("QOOP_TERMINAL", "1")
        .env("QOOP_TOKEN", "t")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("You are running inside qoop"));
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
    let socket = qoop_testkit::socket_in(dir.path());
    let session = socket.parent().expect("the session directory");
    std::fs::create_dir_all(session).unwrap();
    // Private, as qoop requires of its own directory: otherwise it refuses before connecting
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
    let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["task", "rules"])
        .env("QOOP_RUNTIME_DIR", dir.path())
        .env("QOOP_TERMINAL", "1")
        .env("QOOP_TOKEN", "t")
        .output()
        .unwrap();
    let took = started.elapsed();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("You are running inside qoop"));
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
        "qoop never connected, so nothing was tested"
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
        show.contains("opened by qoop when the agent changed files"),
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

/// A goal set before any edit opens the task and lands on it; qass rewrites it by id; the
/// space's goal is set, printed, listed and cleared; the board shows both, on stdout and as
/// a folder.
#[test]
fn goals_reach_the_row_the_space_and_the_board() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let work = daemon.inside(
        "1",
        "goals",
        r#"$Q task goal "login works; done when e2e passes" > goal.out 2>&1"#,
    );
    assert_eq!(
        read(&work, "goal.out").trim(),
        "task #1",
        "{}",
        read(&work, "goal.out")
    );

    // After a done claim and a newer request, a goal is new work: it lands on a new task and
    // the claimed one is closed as superseded, never rewritten.
    let work = daemon.inside(
        "1",
        "next",
        r#"$Q task done "login fixed" > done.out 2>&1
sleep 1
$Q hook-report --state working --event user --label "now the signup page" > report.out 2>&1
$Q task goal "signup works; done when e2e passes" > goal.out 2>&1
$Q task goal > empty.out 2>&1; echo $? > empty.rc"#,
    );
    assert_eq!(
        read(&work, "goal.out").trim(),
        "task #2",
        "{}",
        read(&work, "goal.out")
    );
    assert_eq!(
        read(&work, "empty.rc").trim(),
        "2",
        "an empty goal is a usage error"
    );
    let (_, show, _) = daemon.run(&["task", "show", "1", "--json"]);
    assert!(show.contains(r#""outcome": "superseded""#), "{show}");
    assert!(
        show.contains("login works; done when e2e passes"),
        "the old goal stays: {show}"
    );
    assert_eq!(
        row_task(&daemon, 1)["goal"],
        "signup works; done when e2e passes"
    );

    let (ok, out, err) = daemon.run(&[
        "task",
        "goal",
        "--id",
        "2",
        "signup",
        "works",
        "on",
        "the\nphone",
    ]);
    assert!(ok, "goal by id: {err}");
    assert_eq!(out.trim(), "task #2");
    assert_eq!(row_task(&daemon, 1)["goal"], "signup works on the phone");
    let long = "x".repeat(301);
    let (ok, _, err) = daemon.run(&["task", "goal", "--id", "2", &long]);
    assert!(!ok && err.contains("300"), "a long goal is refused: {err}");

    let (ok, _, err) = daemon.run(&["space", "goal", "1", "checkout", "works"]);
    assert!(ok, "space goal: {err}");
    let (_, out, _) = daemon.run(&["space", "goal", "1"]);
    assert_eq!(out.trim(), "checkout works");
    let (_, out, _) = daemon.run(&["ls", "--json"]);
    assert!(out.contains(r#""goal":"checkout works""#), "{out}");

    let (ok, out, err) = daemon.run(&["task", "board"]);
    assert!(ok, "board: {err}");
    assert!(out.contains("| checkout works | 1 | 0 |"), "{out}");
    assert!(out.contains("Goal: signup works on the phone"), "{out}");
    let folder = dir.path().join("board");
    let (ok, _, err) = daemon.run(&["task", "board", "--dir", folder.to_str().unwrap()]);
    assert!(ok, "board --dir: {err}");
    assert!(read(&folder, "README.md").contains("(space-1.md)"));
    assert!(read(&folder, "space-1.md").contains("Goal: checkout works"));

    let (ok, _, err) = daemon.run(&["space", "goal", "1", ""]);
    assert!(ok, "clear: {err}");
    let (_, out, _) = daemon.run(&["space", "goal", "1"]);
    assert!(out.contains("has no goal"), "{out}");

    // qass keeps no task: its goal is refused out loud, never recorded quietly.
    let (ok, out, err) = daemon.run(&["new", "--space", "qass"]);
    assert!(ok, "{err}");
    let qass = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    let work = daemon.inside(
        &qass,
        "qass-goal",
        r#"$Q task goal "qass notes" > goal.out 2>&1; echo $? > goal.rc"#,
    );
    assert_eq!(read(&work, "goal.rc").trim(), "1");
    assert!(
        read(&work, "goal.out").contains("not recorded"),
        "{}",
        read(&work, "goal.out")
    );
    let (_, ls, _) = daemon.run(&["task", "ls"]);
    assert_eq!(ls.lines().count(), 1, "qass opened nothing: {ls}");
}

/// Count the board's news this daemon sends, on a connection of its own, as a phone would.
fn watch_tasks(dir: &Path) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    use qoop_runtime::client::{Connection, Incoming};
    use qoop_runtime::control::EventMessage;
    use tokio::io::AsyncReadExt;
    let heard = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let socket = qoop_testkit::socket_in(dir);
    let (counter, connected) = (heard.clone(), ready.clone());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let Ok((stream, mut inbuf)) =
                qoop_runtime::client::connect_observer(&socket, "board-watch").await
            else {
                return;
            };
            connected.store(true, std::sync::atomic::Ordering::SeqCst);
            let (mut rd, _wr) = stream.into_split();
            let mut connection = Connection::new();
            let mut read = vec![0u8; 64 * 1024];
            loop {
                for incoming in connection.drain(&mut inbuf).unwrap_or_default() {
                    if matches!(incoming, Incoming::Event(EventMessage::TasksChanged)) {
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                match rd.read(&mut read).await {
                    Ok(n) if n > 0 => inbuf.extend_from_slice(&read[..n]),
                    _ => return,
                }
            }
        });
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the watcher never connected");
        std::thread::sleep(Duration::from_millis(50));
    }
    heard
}

/// Wait until the count moves past `from`, and say whether it did.
fn heard_after(heard: &std::sync::atomic::AtomicUsize, from: usize, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if heard.load(std::sync::atomic::Ordering::SeqCst) > from {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn board_items(daemon: &Daemon) -> Vec<serde_json::Value> {
    let (ok, out, err) = daemon.run(&["task", "board", "--json"]);
    assert!(ok, "board --json: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("board --json: {e}: {out}"))
}

/// The board end to end: an item added from outside any terminal; a terminal links its own
/// task to it, and its steps and ticks land on the item; the board shows it Doing with that
/// terminal on it; it moves to Done and back, to Todo, and a drop takes it off. Every real
/// change is announced to a watching client, and a write that changes nothing is not.
#[test]
fn a_board_item_is_worked_from_a_terminal_and_moved_from_outside() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let heard = watch_tasks(dir.path());
    let count = || heard.load(std::sync::atomic::Ordering::SeqCst);

    let (ok, out, err) = daemon.run(&["task", "add", "--space", "1", "login", "works"]);
    assert!(ok, "add: {err}");
    let key = out
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(key.ends_with("-1"), "a key, numbered from 1: {out}");
    assert!(
        heard_after(&heard, 0, Duration::from_secs(5)),
        "an add is heard"
    );
    let (ok, _, err) = daemon.run(&["task", "add", "--space", "nowhere", "x"]);
    assert!(!ok && err.contains("no space"), "{err}");
    let (ok, _, err) = daemon.run(&["task", "describe", "x"]);
    assert!(!ok && err.contains("are not set"), "{err}");

    let work = daemon.inside(
        "1",
        "item",
        &format!(
            r#"$Q task steps - > nolink.out 2>&1 <<STEPS
read
STEPS
echo $? > nolink.rc
$Q task link {key} > link.out 2>&1
printf 'read the code\nfix it\nrun the tests\n' | $Q task steps - > steps.out 2>&1
$Q task check 2 > check.out 2>&1
$Q task check 9 > badcheck.out 2>&1; echo $? > badcheck.rc
$Q task describe "the login form loses the session" > describe.out 2>&1
$Q task stage waiting-human > stage.out 2>&1"#
        ),
    );
    assert_eq!(read(&work, "nolink.rc").trim(), "1");
    assert!(
        read(&work, "nolink.out").contains("not on the board"),
        "{}",
        read(&work, "nolink.out")
    );
    assert!(
        read(&work, "link.out").contains(&format!("is on {key}")),
        "{}",
        read(&work, "link.out")
    );
    for file in ["steps.out", "check.out", "describe.out", "stage.out"] {
        assert_eq!(read(&work, file), "", "{file}: {}", read(&work, file));
    }
    assert!(
        read(&work, "badcheck.out").contains("no step 9; the item has 3"),
        "{}",
        read(&work, "badcheck.out")
    );

    let items = board_items(&daemon);
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert_eq!(item["key"], key.as_str());
    assert_eq!(item["title"], "login works");
    assert_eq!(item["state"], "doing");
    assert_eq!(
        (item["steps_done"].clone(), item["steps_total"].clone()),
        (1.into(), 3.into())
    );
    assert_eq!(item["agents"][0]["terminal"], 1);
    assert_eq!(item["agents"][0]["machine"], "this Mac");
    assert_eq!(item["agents"][0]["stage"], "waiting-human");
    let id = item["id"].as_i64().unwrap();
    let (_, out, _) = daemon.run(&["task", "board"]);
    assert!(
        out.contains(&format!("| {key} | login works | doing | 1/3 |")),
        "items come first: {out}"
    );
    assert!(out.find("## Items") < out.find("## Spaces"), "{out}");

    // The card, from the daemon: description, steps and the agent's work in its activity.
    let (ok, out, err) = daemon.run(&["task", "show", &key, "--json"]);
    assert!(ok, "show: {err}");
    let card: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        card["item"]["description"],
        "the login form loses the session"
    );
    assert_eq!(card["item"]["steps"][1]["text"], "fix it");
    assert!(card["item"]["steps"][1]["done_at"].is_number());
    let activity: Vec<&str> = card["item"]["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["body"].as_str().unwrap())
        .collect();
    assert!(activity.contains(&"waits on you"), "{activity:?}");
    assert!(activity.contains(&"2. fix it"), "{activity:?}");
    // The item is no task anyone works in.
    let (_, ls, _) = daemon.run(&["task", "ls"]);
    assert_eq!(ls.lines().count(), 1, "only the terminal's task: {ls}");

    // A write that changes nothing is not news.
    let before = count();
    let (ok, _, err) = daemon.run(&["task", "reopen", &key]);
    assert!(ok, "reopen an open item: {err}");
    assert!(
        !heard_after(&heard, before, Duration::from_millis(1500)),
        "nothing changed, nothing sent"
    );

    // Done, then back: its agent is still on it, so Doing again, its steps kept.
    let (ok, _, err) = daemon.run(&["task", "close", &key, "--verified", "checked"]);
    assert!(ok, "close: {err}");
    assert!(
        heard_after(&heard, before, Duration::from_secs(5)),
        "a close is heard"
    );
    assert_eq!(board_items(&daemon)[0]["state"], "done");
    let (ok, _, err) = daemon.run(&["task", "reopen", &format!("#{id}")]);
    assert!(ok, "reopen: {err}");
    let items = board_items(&daemon);
    assert_eq!(
        (items[0]["state"].clone(), items[0]["steps_done"].clone()),
        ("doing".into(), 1.into())
    );

    // Back to Todo: every agent off it.
    let (ok, _, err) = daemon.run(&["task", "unlink", "--id", &key]);
    assert!(ok, "unlink: {err}");
    assert_eq!(board_items(&daemon)[0]["state"], "todo");
    let (ok, out, err) = daemon.run(&["task", "start", &key]);
    assert!(ok, "start: {err}");
    assert!(out.contains("qass is not running"), "{out}");
    assert_eq!(
        board_items(&daemon)[0]["state"],
        "doing",
        "a start waits for an agent"
    );

    // Dropped: off the board.
    let before = count();
    let (ok, _, err) = daemon.run(&["task", "close", &key, "--dropped"]);
    assert!(ok, "drop: {err}");
    assert!(
        heard_after(&heard, before, Duration::from_secs(5)),
        "a drop is heard"
    );
    assert!(
        board_items(&daemon).is_empty(),
        "a dropped item is not shown"
    );
    let (ok, _, err) = daemon.run(&["task", "link", "--id", "1", &key]);
    assert!(
        !ok && err.contains("closed"),
        "no link to a closed item: {err}"
    );

    // qass's own space has no board.
    let (ok, _, err) = daemon.run(&["new", "--space", "qass"]);
    assert!(ok, "{err}");
    std::thread::sleep(Duration::from_millis(500));
    let (ok, _, err) = daemon.run(&["task", "add", "--space", "qass", "notes"]);
    assert!(!ok && err.contains("no board"), "{err}");
}

/// One agent, one name: the board read from the record and the card the daemon answers call a
/// linked agent by its tab's name, and the card carries its events in its activity, in words.
/// A start tells the key as stored, whatever its spelling. A drop is for items only, and a
/// link from qass's space is refused out loud with nothing begun.
#[test]
fn a_board_agent_has_one_name_and_the_boards_refusals_say_why() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::start(&dir);
    let (ok, out, err) = daemon.run(&["task", "add", "--space", "1", "named", "work"]);
    assert!(ok, "add: {err}");
    let key = out
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let (ok, out, err) = daemon.run(&["new", "--space", "1", "--name", "worker"]);
    assert!(ok, "new: {err}");
    let worker = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    let work = daemon.inside(
        &worker,
        "worker",
        &format!("$Q task link {key} > link.out 2>&1"),
    );
    assert!(
        read(&work, "link.out").contains("is on"),
        "{}",
        read(&work, "link.out")
    );
    let items = board_items(&daemon);
    assert_eq!(items[0]["agents"][0]["name"], "worker", "{items:?}");
    let (_, markdown, _) = daemon.run(&["task", "board"]);
    assert!(markdown.contains("worker (this Mac"), "{markdown}");
    let (ok, out, err) = daemon.run(&["task", "show", &key, "--json"]);
    assert!(ok, "show: {err}");
    let card: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(card["item"]["agents"][0]["name"], "worker", "{card}");
    assert_eq!(
        card["events"],
        serde_json::json!([]),
        "an item's events are in its activity"
    );

    // A start says the key as stored, and the card says it in words.
    let (ok, _, err) = daemon.run(&["task", "unlink", "--id", &key]);
    assert!(ok, "unlink: {err}");
    let (ok, out, err) = daemon.run(&["task", "start", &key.to_lowercase()]);
    assert!(ok, "start: {err}");
    assert!(out.starts_with(&format!("{key} started")), "{out}");
    let (_, out, _) = daemon.run(&["task", "show", &key]);
    assert!(out.contains("started by the owner"), "{out}");

    // The worker's own task is no item: a drop is refused.
    let (_, ls, _) = daemon.run(&["task", "ls", "--json"]);
    let open: Vec<serde_json::Value> = serde_json::from_str(&ls).unwrap();
    let task = open[0]["id"].as_i64().unwrap().to_string();
    let (ok, _, err) = daemon.run(&["task", "close", &task, "--dropped"]);
    assert!(!ok && err.contains("not a board item"), "{err}");

    // qass's own space: a link there begins nothing and says so.
    let (ok, out, err) = daemon.run(&["new", "--space", "qass"]);
    assert!(ok, "{err}");
    let qass = out.trim().to_string();
    std::thread::sleep(Duration::from_millis(500));
    let work = daemon.inside(
        &qass,
        "qass-link",
        &format!("$Q task link {key} > link.out 2>&1; echo $? > link.rc"),
    );
    assert_eq!(read(&work, "link.rc").trim(), "1");
    assert!(
        read(&work, "link.out").contains("qass's own space keeps no task"),
        "{}",
        read(&work, "link.out")
    );
    let (_, ls, _) = daemon.run(&["task", "ls"]);
    assert_eq!(ls.lines().count(), 1, "only the worker's task: {ls}");
}

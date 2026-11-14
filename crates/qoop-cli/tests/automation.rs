//! `wait --match`, `new --space NAME`, `prompt --wait` and `qoop worktree` against a
//! real daemon. What `wait`, `new` and `close` share with a supervising agent is in
//! `supervisor.rs`.
//!
//! Each test owns its daemon, its runtime directory and its repository, and ends the
//! daemon by its own handle. The repository paths hold a space on purpose: a worktree
//! command that builds a shell string instead of an argv breaks there first.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The cap every condition below waits under. Generous on purpose: these tests wait on a
/// condition and return as soon as it holds, so the cap only decides how long a real
/// failure takes to report, and a loaded machine must not turn a slow pass into a failure.
const PATIENCE: Duration = Duration::from_secs(120);
const PATIENCE_ARG: &str = "120";

struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
}

impl Daemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        // A `config.toml` per test: the sandbox's home is shared by every test in the file.
        std::fs::create_dir_all(dir.path().join("c/qoop")).expect("config dir");
        let child = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("c"))
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        let socket = qoop_testkit::socket_in(dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        qoop_testkit::open_first_terminal(&socket);
        Self { child, dir }
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .current_dir(cwd)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("c"))
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_SESSION")
            .env_remove("QORO_SESSION")
            .output()
            .expect("run qoop")
    }

    /// Write this test's `config.toml`.
    fn config(&self, text: &str) {
        std::fs::write(self.dir.path().join("c/qoop/config.toml"), text).expect("config");
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(self.dir.path(), args)
    }

    /// Poll a terminal's screen until `needle` is on it.
    fn wait_for_screen(&self, terminal: &str, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let screen = text(&self.run(&["read", terminal]).stdout);
            if screen.contains(needle) {
                return;
            }
            assert!(Instant::now() < deadline, "never arrived: {screen}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// End a terminal the way a person does, and wait for the index to drop it.
    fn close(&self, terminal: &str) {
        let sent = self.run(&["send", terminal, "exit", "--enter"]);
        assert!(sent.status.success(), "{}", text(&sent.stderr));
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            let listed = text(&self.run(&["agents"]).stdout);
            if !listed
                .lines()
                .any(|l| l.starts_with(&format!("term={terminal} ")))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("terminal {terminal} never closed");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn new_terminal(daemon: &Daemon, args: &[&str]) -> String {
    let mut full = vec!["new"];
    full.extend_from_slice(args);
    let out = daemon.run(&full);
    assert!(out.status.success(), "new failed: {}", text(&out.stderr));
    text(&out.stdout).trim().to_string()
}

/// Run git with a fixed identity, and require it to succeed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_string()
}

/// A committed repository at `<root>/<name>`.
fn repo_named(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir(&repo).expect("mkdir");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    repo
}

fn repo(root: &Path) -> PathBuf {
    repo_named(root, "my repo")
}

/// `worktree new <branch>` in `repo`: the checkout and its terminal number.
fn new_worktree(daemon: &Daemon, repo: &Path, branch: &str) -> (PathBuf, String) {
    let made = daemon.run_in(repo, &["worktree", "new", branch]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    let out = text(&made.stdout);
    let checkout = out
        .lines()
        .find_map(|l| l.strip_prefix("worktree="))
        .and_then(|l| l.rsplit_once(" branch="))
        .map(|(path, _)| PathBuf::from(path))
        .expect("a worktree line");
    let terminal = out
        .lines()
        .find_map(|l| l.strip_prefix("terminal="))
        .expect("a terminal number")
        .to_string();
    (checkout, terminal)
}

/// The space a terminal is in, from `qoop agents`.
fn workspace_of(daemon: &Daemon, terminal: &str) -> String {
    let listed = text(&daemon.run(&["agents"]).stdout);
    listed
        .lines()
        .find(|l| l.starts_with(&format!("term={terminal} ")))
        .and_then(|l| l.split(' ').find_map(|w| w.strip_prefix("workspace=")))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no terminal {terminal}: {listed}"))
}

/// A new space called `name`, even when one already is, and its id: what a rename or a
/// phone can leave, and `new --space` never makes.
fn make_space(daemon: &Daemon, name: &str) -> String {
    use qoop_runtime::control::{ControlRequest, ControlResponse};
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let socket = qoop_testkit::socket_in(daemon.dir.path());
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let (stream, inbuf) = qoop_runtime::client::connect(&socket, "test-space")
            .await
            .expect("connect");
        let mut client = qoop_testkit::TestClient::from_parts(stream, inbuf, 1);
        match client
            .control(
                &ControlRequest::CreateWorkspace {
                    id: format!("space-{made}"),
                    name: name.to_string(),
                },
                PATIENCE,
            )
            .await
        {
            Some(ControlResponse::WorkspaceCreated { workspace, .. }) => workspace.to_string(),
            other => panic!("no space made: {other:?}"),
        }
    })
}

/// Set a space's folder, by id or name.
fn space_dir(daemon: &Daemon, space: &str, dir: &Path) {
    let set = daemon.run(&["space", "dir", space, dir.to_str().expect("utf8")]);
    assert!(set.status.success(), "{}", text(&set.stderr));
}

#[test]
fn wait_match_sees_text_on_the_live_screen() {
    let daemon = Daemon::start();
    // The marker is computed by the shell, so the typed command line on the screen does
    // not already hold it: a match on the echo would pass without waiting at all.
    let n = new_terminal(
        &daemon,
        &["--", "sh", "-c", "sleep 1; echo WAIT_MARK_$((6*7))"],
    );
    let started = Instant::now();
    let waited = daemon.run(&[
        "wait",
        &n,
        "--match",
        "WAIT_MARK_42",
        "--timeout",
        PATIENCE_ARG,
    ]);
    assert_eq!(waited.status.code(), Some(0), "{}", text(&waited.stderr));
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "matched before the text was printed"
    );
}

#[test]
fn wait_match_sees_text_already_on_the_screen() {
    // The screen the subscription delivers can arrive inside its own round trip; text
    // that is already there has to count, not only text that appears later. Repeated,
    // because whether it lands inside the round trip or after it is a race.
    let daemon = Daemon::start();
    let n = new_terminal(
        &daemon,
        &["--", "sh", "-c", "echo STATIC_MARK_7; sleep 300"],
    );
    // Not `wait_for_screen`: its reads keep the published screen fresh, which hides the
    // race. The text is printed well within this; the wait itself is what is measured.
    std::thread::sleep(Duration::from_secs(2));
    for _ in 0..8 {
        let waited = daemon.run(&["wait", &n, "--match", "STATIC_MARK_7", "--timeout", "4"]);
        assert_eq!(waited.status.code(), Some(0), "{}", text(&waited.stderr));
    }
}

#[test]
fn verbs_refuse_without_a_daemon_and_start_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    // `agents` is not here: with nothing awake it answers an empty list.
    for args in [
        &["new", "--", "true"][..],
        &["read", "1"],
        &["worktree", "setup", "feat/x"],
    ] {
        let out = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .env("QOOP_RUNTIME_DIR", dir.path())
            .output()
            .expect("run qoop");
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        // One line, the same shape for every command, never Rust's debug form.
        assert_eq!(
            text(&out.stderr),
            "qoop: nothing is awake: `qoop` or `qoop wake` starts it\n",
            "{args:?}"
        );
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !qoop_testkit::socket_in(dir.path()).exists(),
        "a daemon was started"
    );
}

#[test]
fn prompt_wait_ends_when_a_prompt_folded_into_the_running_turn_settles() {
    // The folded case: the agent is working when the prompt lands, reports one
    // `done` and nothing busy after it. One foreground process for the whole run, so the
    // hook's belief holds instead of being reset by a new generation.
    let daemon = Daemon::start();
    let n = new_terminal(&daemon, &[]);
    let q = env!("CARGO_BIN_EXE_qoop");
    let script = format!(
        "sh -c '\"{q}\" hook-report --state working; sleep 3; \"{q}\" hook-report --state done; sleep 60'"
    );
    assert!(
        daemon
            .run(&["send", &n, &script, "--enter"])
            .status
            .success()
    );
    let working = daemon.run(&["wait", &n, "--until", "working", "--timeout", PATIENCE_ARG]);
    assert_eq!(working.status.code(), Some(0), "{}", text(&working.stderr));

    let started = Instant::now();
    let waited = daemon.run(&["prompt", &n, "--wait", "--timeout", PATIENCE_ARG, "true"]);
    assert_eq!(waited.status.code(), Some(0), "{}", text(&waited.stderr));
    assert!(
        text(&waited.stdout).contains("state=done"),
        "{}",
        text(&waited.stdout)
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "ended before the stop had settled"
    );
}

#[test]
fn a_prompt_wait_chained_in_the_second_of_a_done_does_not_return_at_once() {
    // The `done` and the next `prompt --wait` land in the same second, and nothing takes
    // the prompt: the wait has to time out rather than accept the old `done`. Run from
    // inside the terminal itself so the two are milliseconds apart.
    let daemon = Daemon::start();
    let n = new_terminal(&daemon, &[]);
    let q = env!("CARGO_BIN_EXE_qoop");
    let script = format!(
        "sh -c '\"{q}\" hook-report --state done; \"{q}\" prompt \"$QOOP_TERMINAL\" --wait --timeout 3 x; echo CHAINED_RC=$?; sleep 60'"
    );
    assert!(
        daemon
            .run(&["send", &n, &script, "--enter"])
            .status
            .success()
    );
    daemon.wait_for_screen(&n, "CHAINED_RC=124");
}

#[test]
fn a_misspelt_prompt_flag_is_refused_rather_than_typed() {
    let daemon = Daemon::start();
    let out = daemon.run(&["prompt", "1", "--wiat", "echo NEVER_TYPED"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(text(&out.stderr).starts_with("qoop: unknown flag --wiat"));
    let late = daemon.run(&["prompt", "1", "echo NEVER_TYPED", "--wait"]);
    assert_eq!(late.status.code(), Some(2), "{}", text(&late.stderr));
    assert!(text(&late.stderr).starts_with("qoop: flags go right after the target"));
    for args in [&["read", "1", "--bogus"][..], &["agents", "--bogus"]] {
        let out = daemon.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn a_new_space_by_name_is_made_once_and_reused() {
    let daemon = Daemon::start();
    let first = new_terminal(&daemon, &["--space", "fresh", "--name", "one"]);
    let second = new_terminal(&daemon, &["--space", "fresh"]);
    let listed = text(&daemon.run(&["agents"]).stdout);
    let space_of = |n: &str| {
        listed
            .lines()
            .find(|l| l.starts_with(&format!("term={n} ")))
            .and_then(|l| l.split(' ').find_map(|w| w.strip_prefix("workspace=")))
            .map(str::to_string)
    };
    assert_eq!(space_of(&first), space_of(&second), "{listed}");
    assert_ne!(space_of(&first).as_deref(), Some("1"), "{listed}");
    assert!(listed.contains("name=one"), "{listed}");
}

#[test]
fn space_rm_removes_an_empty_space_and_refuses_one_with_a_tab() {
    let daemon = Daemon::start();
    let tab = new_terminal(&daemon, &["--space", "scratch"]);
    let listed = |d: &Daemon| text(&d.run(&["ls"]).stdout);
    assert!(listed(&daemon).contains("scratch"), "{}", listed(&daemon));

    let refused = daemon.run(&["space", "rm", "scratch"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("still holds 1 tab"),
        "{}",
        text(&refused.stderr)
    );
    assert!(listed(&daemon).contains("scratch"), "a refusal removed it");

    daemon.close(&tab);
    let removed = daemon.run(&["space", "rm", "scratch"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(
        text(&removed.stdout).contains("removed space"),
        "{}",
        text(&removed.stdout)
    );
    assert!(!listed(&daemon).contains("scratch"), "{}", listed(&daemon));

    let gone = daemon.run(&["space", "rm", "scratch"]);
    assert_eq!(gone.status.code(), Some(1));
    assert!(text(&gone.stderr).contains("no space with id or name"));
    assert_eq!(daemon.run(&["space", "rm"]).status.code(), Some(2));
}

/// A name too long for its column is cut there and said whole on a line under its row, so
/// two that begin alike stay apart. An escape in a name or a goal is never printed: it would
/// reach the terminal as a command.
#[test]
fn qoop_ls_says_a_cut_name_whole_under_its_row() {
    let daemon = Daemon::start();
    let names = [
        "mobile app with a long name one",
        "mobile app with a long name two",
    ];
    for name in names {
        new_terminal(&daemon, &["--space", name]);
    }
    let sneaky = "title \x1b]0;owned\x07 of a long space name";
    new_terminal(&daemon, &["--space", sneaky]);
    let goal = daemon.run(&["space", "goal", sneaky, "copy \x1b]52;c;aGk=\x07 this"]);
    assert!(goal.status.success(), "{}", text(&goal.stderr));
    let listed = text(&daemon.run(&["ls"]).stdout);
    for name in names {
        assert!(
            listed.contains(&format!("\n     name: {name}\n")),
            "{listed}"
        );
    }
    assert!(
        listed.contains("\n     name: title ]0;owned of a long space name\n"),
        "{listed:?}"
    );
    assert!(
        listed.contains("\n     goal: copy ]52;c;aGk= this\n"),
        "{listed:?}"
    );
    assert!(!listed.contains(['\x1b', '\x07']), "{listed:?}");
    assert_eq!(listed.matches("name: ").count(), 3, "{listed}");
}

#[test]
fn parallel_callers_naming_one_new_space_make_it_once() {
    let daemon = Daemon::start();
    let made: Vec<String> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| new_terminal(&daemon, &["--space", "racey"])))
            .collect();
        runs.into_iter().map(|r| r.join().unwrap()).collect()
    });
    let listed = text(&daemon.run(&["agents"]).stdout);
    let spaces: std::collections::BTreeSet<&str> = listed
        .lines()
        .filter(|l| made.iter().any(|n| l.starts_with(&format!("term={n} "))))
        .filter_map(|l| l.split(' ').find_map(|w| w.strip_prefix("workspace=")))
        .collect();
    assert_eq!(spaces.len(), 1, "one space for eight callers: {listed}");
}

#[test]
fn worktree_new_ls_rm_round_trip_in_a_path_with_a_space() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());

    let (checkout, terminal) = new_worktree(&daemon, &repo, "feat/one");
    assert_eq!(
        checkout,
        std::fs::canonicalize(root.path().join("my repo-wt-feat-one")).unwrap()
    );

    // Listed from inside the linked worktree, which still finds the main one first.
    let listed = daemon.run_in(&checkout, &["worktree", "ls"]);
    let listed = text(&listed.stdout);
    assert!(
        listed.lines().next().unwrap_or("").contains("branch=main"),
        "{listed}"
    );
    assert!(listed.contains(&format!("term={terminal} ")), "{listed}");

    // Checked out already: refused by name, with where.
    let again = daemon.run_in(&repo, &["worktree", "new", "feat/one"]);
    assert!(!again.status.success());
    assert!(text(&again.stderr).contains("already checked out"));

    // The shell inside keeps it, named by pid and by terminal.
    let kept = daemon.run_in(&repo, &["worktree", "rm", "feat/one"]);
    assert_eq!(kept.status.code(), Some(1));
    assert!(
        text(&kept.stderr).contains(&format!("qoop terminal {terminal}")),
        "{}",
        text(&kept.stderr)
    );
    daemon.close(&terminal);

    // Dirty: refused without --force.
    std::fs::write(checkout.join("new file"), "x").unwrap();
    let dirty = daemon.run_in(&repo, &["worktree", "rm", "feat/one"]);
    assert!(!dirty.status.success());
    assert!(text(&dirty.stderr).contains("uncommitted changes"));

    let removed = daemon.run_in(&repo, &["worktree", "rm", "feat/one", "--force"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(!checkout.exists());
    assert!(
        git(&repo, &["branch", "--list", "feat/one"]).contains("feat/one"),
        "the branch stays"
    );
}

#[test]
fn worktree_rm_refuses_while_any_process_is_inside_even_with_force() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let (checkout, terminal) = new_worktree(&daemon, &repo, "feat/busy");
    daemon.close(&terminal);

    // A process qoop never started, in a directory below the worktree.
    let below = checkout.join("deep dir");
    std::fs::create_dir(&below).unwrap();
    let mut outsider = Command::new("sleep")
        .arg("300")
        .current_dir(&below)
        .spawn()
        .expect("spawn a process inside");
    for args in [
        &["worktree", "rm", "feat/busy"][..],
        &["worktree", "rm", "feat/busy", "--force"],
    ] {
        let refused = daemon.run_in(&repo, args);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(
            text(&refused.stderr).contains(&format!("pid {} (sleep)", outsider.id())),
            "{args:?}: {}",
            text(&refused.stderr)
        );
    }
    assert!(checkout.exists());
    let _ = outsider.kill();
    let _ = outsider.wait();

    // A terminal's shell that moved in with a quick `cd` on a quiet prompt.
    daemon.run(&[
        "send",
        "1",
        &format!("cd '{}'", checkout.display()),
        "--enter",
    ]);
    daemon.run(&["send", "1", "echo MOVED_IN_$((6*7))", "--enter"]);
    daemon.wait_for_screen("1", "MOVED_IN_42");
    let refused = daemon.run_in(&repo, &["worktree", "rm", "feat/busy", "--force"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("qoop terminal 1"),
        "{}",
        text(&refused.stderr)
    );
    daemon.close("1");
    let removed = daemon.run_in(&repo, &["worktree", "rm", "feat/busy"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
}

#[test]
fn worktree_rm_ignores_qoop_itself_and_tells_a_caller_inside_to_step_out() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let (checkout, terminal) = new_worktree(&daemon, &repo, "feat/self");
    daemon.close(&terminal);

    // A qoop client started from inside holds no work there.
    let mut watcher = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["wait", "1", "--until", "blocked", "--timeout", PATIENCE_ARG])
        .current_dir(&checkout)
        .env("QOOP_RUNTIME_DIR", daemon.dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a qoop client inside");

    // Run from a shell standing inside: the shell is the caller's own, and the answer is
    // to step out rather than to close it. `; true` keeps the shell from exec-ing qoop.
    let inside = qoop_testkit::sandboxed("/bin/sh")
        .args(["-c", "\"$0\" worktree rm feat/self; true"])
        .arg(env!("CARGO_BIN_EXE_qoop"))
        .current_dir(&checkout)
        .env("QOOP_RUNTIME_DIR", daemon.dir.path())
        .output()
        .expect("run from inside");
    assert!(
        text(&inside.stderr).contains("run it from outside the worktree"),
        "{}",
        text(&inside.stderr)
    );
    assert!(checkout.exists());

    let removed = daemon.run_in(&repo, &["worktree", "rm", "feat/self"]);
    assert!(
        removed.status.success(),
        "a qoop client inside blocked it: {}",
        text(&removed.stderr)
    );
    let _ = watcher.kill();
    let _ = watcher.wait();
}

#[test]
fn worktree_rm_names_a_qoop_daemon_running_from_inside() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let (checkout, terminal) = new_worktree(&daemon, &repo, "feat/home");
    daemon.close(&terminal);

    // Another daemon (another runtime directory) started from inside: every terminal it
    // opens without a directory starts there, so it is not a client to ignore.
    let other = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(other.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let mut inside = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(["daemon"])
        .current_dir(&checkout)
        .env("QOOP_RUNTIME_DIR", other.path())
        .env("SHELL", "/bin/sh")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a daemon inside");
    let socket = qoop_testkit::socket_in(other.path());
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline && !socket.exists() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let refused = daemon.run_in(&repo, &["worktree", "rm", "feat/home", "--force"]);
    let _ = inside.kill();
    let _ = inside.wait();
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains(&format!(
            "a qoop daemon (pid {}) runs from inside",
            inside.id()
        )),
        "{}",
        text(&refused.stderr)
    );
}

#[test]
fn a_branch_only_on_a_remote_is_checked_out_tracking_it() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let origin = repo_named(root.path(), "origin repo");
    git(&origin, &["checkout", "-q", "-b", "feat/remote"]);
    git(
        &origin,
        &["commit", "-q", "--allow-empty", "-m", "remote work"],
    );
    git(&origin, &["checkout", "-q", "main"]);
    let clone = root.path().join("my repo");
    git(root.path(), &["clone", "-q", "origin repo", "my repo"]);

    let (_, terminal) = new_worktree(&daemon, &clone, "feat/remote");
    daemon.close(&terminal);
    assert_eq!(
        git(&clone, &["rev-parse", "feat/remote"]),
        git(&clone, &["rev-parse", "origin/feat/remote"]),
        "the local branch starts at the remote one, not at HEAD"
    );
    assert_eq!(
        git(
            &clone,
            &["rev-parse", "--abbrev-ref", "feat/remote@{upstream}"]
        ),
        "origin/feat/remote"
    );
}

#[test]
fn worktree_mistakes_exit_two_and_a_vanished_one_names_prune() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let help = daemon.run_in(&repo, &["worktree", "help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(text(&help.stdout).contains("qoop worktree rm"));
    for args in [
        &["worktree"][..],
        &["worktree", "bogus"],
        &["worktree", "setup", "--bogus"],
        &["worktree", "setup", "a", "b"],
        &["worktree", "setup", "--space"],
    ] {
        let out = daemon.run_in(&repo, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("qoop worktree rm"), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }

    let (checkout, terminal) = new_worktree(&daemon, &repo, "feat/gone");
    daemon.close(&terminal);
    std::fs::remove_dir_all(&checkout).unwrap();
    let out = daemon.run_in(&repo, &["worktree", "rm", "feat/gone"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("git worktree prune"),
        "{}",
        text(&out.stderr)
    );
}

/// Without `--space` a worktree's tab opens in the space whose folder is the repository,
/// else the one whose folder holds it, else the default space as before.
#[test]
fn a_worktree_tab_opens_in_the_space_of_its_repository() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let default = workspace_of(&daemon, "1");

    let elsewhere = repo_named(root.path(), "elsewhere");
    let (_, terminal) = new_worktree(&daemon, &elsewhere, "feat/plain");
    assert_eq!(
        workspace_of(&daemon, &terminal),
        default,
        "no space matches"
    );

    let code = make_space(&daemon, "code");
    space_dir(&daemon, &code, root.path());
    let (_, terminal) = new_worktree(&daemon, &repo, "feat/parent");
    assert_eq!(
        workspace_of(&daemon, &terminal),
        code,
        "the folder holding it"
    );

    let app = make_space(&daemon, "app");
    space_dir(&daemon, &app, &repo);
    let (_, terminal) = new_worktree(&daemon, &repo, "feat/exact");
    assert_eq!(
        workspace_of(&daemon, &terminal),
        app,
        "the repository's own"
    );

    // Two spaces called alike, only the second about this repository: the tab goes to that
    // one, by id, never to the first of the name.
    let first = make_space(&daemon, "twin");
    let second = make_space(&daemon, "twin");
    let twin = repo_named(root.path(), "twin repo");
    space_dir(&daemon, &first, &elsewhere);
    space_dir(&daemon, &second, &twin);
    let (_, terminal) = new_worktree(&daemon, &twin, "feat/twin");
    assert_eq!(workspace_of(&daemon, &terminal), second);

    // `--space` still wins, and an id that is no space is refused before git runs.
    let made = daemon.run_in(&repo, &["worktree", "new", "feat/given", "--space", &code]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    let terminal = text(&made.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("terminal="))
        .expect("a terminal")
        .to_string();
    assert_eq!(workspace_of(&daemon, &terminal), code);
    let refused = daemon.run_in(&repo, &["worktree", "new", "feat/nowhere", "--space", "99"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("no space with id or name 99"),
        "{}",
        text(&refused.stderr)
    );
    assert!(refused.stdout.is_empty(), "{}", text(&refused.stdout));
    assert!(!root.path().join("my repo-wt-feat-nowhere").exists());
    assert!(git(&repo, &["branch", "--list", "feat/nowhere"]).is_empty());
}

/// A branch removed and made again gets the same git id, and so its old number back, held
/// by one claim.
#[test]
fn a_worktree_made_again_keeps_its_index_claim() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let claims = repo.join(".git/qoop-worktree-index");
    let holders = || {
        let mut all: Vec<(String, String)> = std::fs::read_dir(&claims)
            .expect("claims")
            .map(|e| {
                let e = e.expect("entry");
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read_to_string(e.path()).expect("claim"),
                )
            })
            .collect();
        all.sort();
        all
    };
    let (_, one) = new_worktree(&daemon, &repo, "feat/one");
    let (_, two) = new_worktree(&daemon, &repo, "feat/two");
    let before = holders();
    assert_eq!(
        before,
        [
            ("1".to_string(), "my-repo-wt-feat-one".to_string()),
            ("2".to_string(), "my-repo-wt-feat-two".to_string())
        ]
    );
    daemon.close(&one);
    daemon.close(&two);
    let removed = daemon.run_in(&repo, &["worktree", "rm", "feat/one"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    git(&repo, &["branch", "-D", "feat/one"]);
    new_worktree(&daemon, &repo, "feat/one");
    assert_eq!(holders(), before, "the same number, one claim");
}

/// The value of a `key=` line on stdout.
fn said(out: &Output, key: &str) -> Option<String> {
    text(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
}

/// A setup line that writes its four variables and where it ran to `<file>-<index>`,
/// then says a marker that carries the index.
fn recording_setup(file: &Path) -> String {
    format!(
        "printf '%s\\n' \"$QOOP_WORKTREE\" \"$QOOP_WORKTREE_SOURCE\" \"$QOOP_WORKTREE_BRANCH\" \
         \"$QOOP_WORKTREE_INDEX\" \"$PWD\" > '{}'-\"$QOOP_WORKTREE_INDEX\"; echo SETUP_SAID_$QOOP_WORKTREE_INDEX",
        file.display()
    )
}

/// A TOML literal string, which needs no escapes.
fn literal(text: &str) -> String {
    format!("'''{text}'''")
}

#[test]
fn a_space_setup_copies_only_listed_ignored_files_then_runs_with_its_variables() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let repo = repo(&root_path);
    std::fs::write(repo.join(".gitignore"), ".env*\n").unwrap();
    std::fs::write(repo.join(".env.example"), "tracked\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["add", "-f", ".env.example"]);
    git(&repo, &["commit", "-q", "-m", "ignore"]);
    std::fs::write(repo.join(".env.example"), "changed, not committed\n").unwrap();
    std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
    std::fs::set_permissions(repo.join(".env"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(".env", repo.join(".env.link")).unwrap();
    let vars = root_path.join("vars");
    daemon.config(&format!(
        "[spaces.\"my app\".worktree]\ncopy = [\".env*\"]\nsetup = {}\n\n[spaces.bare.worktree]\nsetup = \"true\"\n",
        literal(&recording_setup(&vars))
    ));

    let made = daemon.run_in(&repo, &["worktree", "new", "feat/a", "--space", "my app"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    let out = text(&made.stdout);
    let tree = root_path.join("my repo-wt-feat-a");
    assert_eq!(
        out.lines()
            .map(|l| l.split('=').next().unwrap_or(""))
            .collect::<Vec<_>>(),
        ["worktree", "setup", "terminal"],
        "{out}"
    );
    assert_eq!(said(&made, "setup").as_deref(), Some("done"));
    assert!(!out.contains("SETUP_SAID"), "setup output on stdout: {out}");
    let err = text(&made.stderr);
    assert!(err.contains("SETUP_SAID_1"), "{err}");
    assert!(
        err.contains(&format!(
            "setup: copied 2 files from {} (.env, .env.link)",
            repo.display()
        )),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{}-1", vars.display())).unwrap(),
        format!(
            "{tree}\n{repo}\nfeat/a\n1\n{tree}\n",
            tree = tree.display(),
            repo = repo.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(tree.join(".env")).unwrap(),
        "SECRET=1\n"
    );
    let mode = std::fs::metadata(tree.join(".env"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        std::fs::read_link(tree.join(".env.link")).unwrap(),
        PathBuf::from(".env"),
        "a symlink stays one"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join(".env.example")).unwrap(),
        "tracked\n",
        "a tracked file is never copied"
    );

    // Again on the same worktree: what is there is kept, the index is the same.
    std::fs::write(tree.join(".env"), "mine\n").unwrap();
    let again = daemon.run_in(&tree, &["worktree", "setup", "--space", "my app"]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert_eq!(said(&again, "setup").as_deref(), Some("done"));
    assert!(said(&again, "terminal").is_none());
    assert!(
        text(&again.stderr).contains("copied 0 files from")
            && text(&again.stderr).contains("kept 2 already there"),
        "{}",
        text(&again.stderr)
    );
    assert!(text(&again.stderr).contains("SETUP_SAID_1"));
    assert_eq!(
        std::fs::read_to_string(tree.join(".env")).unwrap(),
        "mine\n"
    );

    // A table with no `copy` copies nothing, even with a `.env` there.
    let made = daemon.run_in(&repo, &["worktree", "new", "feat/b", "--space", "bare"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert_eq!(said(&made, "setup").as_deref(), Some("done"));
    assert!(!root_path.join("my repo-wt-feat-b/.env").exists());

    // `worktree setup` never makes a space, and on the main checkout is refused.
    let none = daemon.run_in(&tree, &["worktree", "setup", "--space", "brand new"]);
    assert!(none.status.success(), "{}", text(&none.stderr));
    assert_eq!(said(&none, "setup").as_deref(), Some("none"));
    assert!(!text(&daemon.run(&["ls"]).stdout).contains("brand new"));
    let main = daemon.run_in(&repo, &["worktree", "setup"]);
    assert_eq!(main.status.code(), Some(1));
    assert!(text(&main.stderr).contains("where the files come from"));
}

#[test]
fn a_failed_setup_keeps_the_worktree_opens_no_terminal_and_can_be_run_again() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    daemon.config("[spaces.fixme.worktree]\nsetup = \"echo FAILING; exit 3\"\n");
    let failed = daemon.run_in(&repo, &["worktree", "new", "feat/f", "--space", "fixme"]);
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stderr));
    assert_eq!(said(&failed, "setup").as_deref(), Some("failed"));
    assert!(said(&failed, "terminal").is_none());
    assert!(
        text(&failed.stderr)
            .ends_with("qoop: setup failed (exit 3); fix it, then qoop worktree setup feat/f\n"),
        "{}",
        text(&failed.stderr)
    );
    assert!(root.path().join("my repo-wt-feat-f").is_dir());
    assert!(
        !text(&daemon.run(&["ls"]).stdout).contains("fixme"),
        "a failed setup made the space"
    );

    daemon.config("[spaces.fixme.worktree]\nsetup = \"true\"\n");
    let fixed = daemon.run_in(&repo, &["worktree", "setup", "feat/f", "--space", "fixme"]);
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    assert_eq!(said(&fixed, "setup").as_deref(), Some("done"));

    let skipped = daemon.run_in(
        &repo,
        &[
            "worktree",
            "new",
            "feat/s",
            "--space",
            "fixme",
            "--no-setup",
        ],
    );
    assert!(skipped.status.success(), "{}", text(&skipped.stderr));
    assert_eq!(said(&skipped, "setup").as_deref(), Some("skipped"));
    assert!(said(&skipped, "terminal").is_some());

    // A problem in the space's own table: nothing of it is copied or run.
    let ran = root.path().join("RAN");
    daemon.config(&format!(
        "[spaces.fixme.worktree]\ncopy = [\"../x\"]\nsetup = \"touch '{}'\"\n",
        ran.display()
    ));
    let bad = daemon.run_in(&repo, &["worktree", "new", "feat/bad", "--space", "fixme"]);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(said(&bad, "setup").as_deref(), Some("failed"));
    let err = text(&bad.stderr);
    assert!(err.contains("../x has a .. part"), "{err}");
    assert!(
        err.contains("setup failed (config.toml has a problem)"),
        "{err}"
    );
    assert!(!ran.exists());
    assert!(root.path().join("my repo-wt-feat-bad").is_dir());

    // A file that is not TOML fails a found space, and is nothing to a repository with none.
    daemon.config(&format!(
        "[spaces.fixme.worktree]\nsetup = \"touch '{}'\"\nthis is not toml\n",
        ran.display()
    ));
    let broken = daemon.run_in(
        &repo,
        &["worktree", "new", "feat/broken", "--space", "fixme"],
    );
    assert_eq!(broken.status.code(), Some(1));
    assert!(text(&broken.stderr).contains("config.toml has a problem"));
    assert!(!ran.exists());
    let alone = daemon.run_in(&repo, &["worktree", "new", "feat/alone"]);
    assert!(alone.status.success(), "{}", text(&alone.stderr));
    assert_eq!(said(&alone, "setup").as_deref(), Some("none"));
    assert!(!ran.exists());

    // A bad line outside `[spaces]` costs that line only: the space's setup still runs.
    daemon.config(&format!(
        "theme = dark\n[spaces.fixme.worktree]\nsetup = \"touch '{}'\"\n",
        ran.display()
    ));
    let elsewhere = daemon.run_in(
        &repo,
        &["worktree", "new", "feat/elsewhere", "--space", "fixme"],
    );
    assert!(elsewhere.status.success(), "{}", text(&elsewhere.stderr));
    assert_eq!(said(&elsewhere, "setup").as_deref(), Some("done"));
    assert!(ran.exists());

    // Patterns that match nothing copy nothing, say so, and with no command are `none`.
    daemon.config("[spaces.fixme.worktree]\ncopy = [\"nothing-here*\"]\n");
    let nothing = daemon.run_in(
        &repo,
        &["worktree", "new", "feat/nothing", "--space", "fixme"],
    );
    assert!(nothing.status.success(), "{}", text(&nothing.stderr));
    assert_eq!(said(&nothing, "setup").as_deref(), Some("none"));
    assert!(
        text(&nothing.stderr).contains(&format!(
            "setup: copied 0 files from {}",
            std::fs::canonicalize(&repo).unwrap().display()
        )),
        "{}",
        text(&nothing.stderr)
    );
}

/// `--space <id>` keys the space's name; two names that tie with no table are a tie, said;
/// the deepest folder wins over one above it; qass's own space never counts.
#[test]
fn the_space_rule_reads_ids_ties_nesting_and_never_qass() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let sub = root_path.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let repo = repo(&sub);
    daemon.config("[spaces.named.worktree]\nsetup = \"true\"\n");

    let named = make_space(&daemon, "named");
    let by_id = daemon.run_in(&repo, &["worktree", "new", "feat/id", "--space", &named]);
    assert!(by_id.status.success(), "{}", text(&by_id.stderr));
    assert_eq!(said(&by_id, "setup").as_deref(), Some("done"));
    assert!(
        text(&by_id.stderr).contains("setup: space named: running true"),
        "{}",
        text(&by_id.stderr)
    );

    // Nested folders: the deepest that holds the repository; qass's own is passed by.
    let outer = make_space(&daemon, "outer");
    let inner = make_space(&daemon, "inner");
    let qass = make_space(&daemon, "qass");
    space_dir(&daemon, &outer, &root_path);
    space_dir(&daemon, &inner, &sub);
    space_dir(&daemon, &qass, &repo);
    let (_, terminal) = new_worktree(&daemon, &repo, "feat/nested");
    assert_eq!(workspace_of(&daemon, &terminal), inner);

    // Two names with the repository as folder, neither with a table: a tie, said.
    let left = make_space(&daemon, "left");
    let right = make_space(&daemon, "right");
    space_dir(&daemon, &left, &repo);
    space_dir(&daemon, &right, &repo);
    let tie = daemon.run_in(&repo, &["worktree", "new", "feat/tie"]);
    assert!(tie.status.success(), "{}", text(&tie.stderr));
    assert_eq!(said(&tie, "setup").as_deref(), Some("none"));
    assert!(
        text(&tie.stderr).contains(&format!(
            "setup: spaces left ({left}), right ({right}) tie for {}",
            repo.display()
        )),
        "{}",
        text(&tie.stderr)
    );
    let terminal = said(&tie, "terminal").expect("a terminal");
    assert_eq!(workspace_of(&daemon, &terminal), workspace_of(&daemon, "1"));
}

/// A repository of its own inside an ignored folder is listed by git as a folder: passed
/// by, not refused, under the docs' own `**/.env*`.
#[test]
fn a_nested_repository_is_passed_by_a_glob_copy() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    std::fs::write(repo.join(".gitignore"), "vendor/\n.env*\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-q", "-m", "ignore"]);
    std::fs::create_dir_all(repo.join("vendor/lib")).unwrap();
    std::fs::create_dir_all(repo.join("apps/web")).unwrap();
    git(&repo.join("vendor/lib"), &["init", "-q"]);
    std::fs::write(repo.join("vendor/lib/.env"), "theirs\n").unwrap();
    std::fs::write(repo.join(".env"), "A=1\n").unwrap();
    std::fs::write(repo.join("apps/web/.env.local"), "B=2\n").unwrap();
    daemon.config("[spaces.app.worktree]\ncopy = [\"**/.env*\"]\n");
    let made = daemon.run_in(&repo, &["worktree", "new", "feat/glob", "--space", "app"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert_eq!(said(&made, "setup").as_deref(), Some("done"));
    let tree = root.path().join("my repo-wt-feat-glob");
    assert_eq!(std::fs::read_to_string(tree.join(".env")).unwrap(), "A=1\n");
    assert_eq!(
        std::fs::read_to_string(tree.join("apps/web/.env.local")).unwrap(),
        "B=2\n"
    );
    assert!(!tree.join("vendor").exists());
}

#[test]
fn a_file_under_a_tracked_symlinked_folder_is_refused_and_nothing_is_made_outside() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    std::fs::create_dir(repo.join("apps")).unwrap();
    std::fs::write(repo.join("apps/keep"), "x").unwrap();
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "apps"]);
    git(&repo, &["checkout", "-q", "-b", "feat/link"]);
    git(&repo, &["rm", "-q", "-r", "apps"]);
    std::os::unix::fs::symlink(outside.path(), repo.join("apps")).unwrap();
    git(&repo, &["add", "apps"]);
    git(&repo, &["commit", "-q", "-m", "apps elsewhere"]);
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("apps/.env"), "SECRET=1\n").unwrap();
    let ran = root.path().join("RAN");
    daemon.config(&format!(
        "[spaces.app.worktree]\ncopy = [\"apps/.env\"]\nsetup = \"touch '{}'\"\n",
        ran.display()
    ));

    let refused = daemon.run_in(&repo, &["worktree", "new", "feat/link", "--space", "app"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    assert_eq!(said(&refused, "setup").as_deref(), Some("failed"));
    assert!(said(&refused, "terminal").is_none());
    let err = text(&refused.stderr);
    assert!(
        err.contains("setup: not copying apps/.env: apps in the worktree is a symlink"),
        "{err}"
    );
    assert!(err.contains("setup failed (apps/.env not copied)"), "{err}");
    assert!(!ran.exists(), "setup ran after a refused copy");
    assert!(!outside.path().join(".env").exists());
}

/// Without `--space`, the repository's space is found, and of two that tie the one with a
/// table runs; spaces of one name share their table and the tab goes to the lower id.
#[test]
fn the_repositorys_space_runs_its_setup_and_a_tie_goes_to_the_table() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());

    // No space, no `[spaces]`: nothing new is said.
    let plain = daemon.run_in(&repo, &["worktree", "new", "feat/plain"]);
    assert!(plain.status.success(), "{}", text(&plain.stderr));
    assert_eq!(said(&plain, "setup").as_deref(), Some("none"));
    assert!(
        !text(&plain.stderr).contains("setup:"),
        "{}",
        text(&plain.stderr)
    );

    let vars = root.path().join("vars");
    daemon.config(&format!(
        "[spaces.web.worktree]\nsetup = {}\n",
        literal(&recording_setup(&vars))
    ));
    let hinted = daemon.run_in(&repo, &["worktree", "new", "feat/hinted"]);
    assert_eq!(said(&hinted, "setup").as_deref(), Some("none"));
    assert!(
        text(&hinted.stderr).contains("as its folder; qoop space dir NAME"),
        "{}",
        text(&hinted.stderr)
    );

    let api = make_space(&daemon, "api");
    let web = make_space(&daemon, "web");
    space_dir(&daemon, &api, &repo);
    space_dir(&daemon, &web, &repo);
    let (_, terminal) = new_worktree(&daemon, &repo, "feat/tie");
    assert_eq!(workspace_of(&daemon, &terminal), web);
    assert!(
        std::fs::read_to_string(format!("{}-3", vars.display()))
            .unwrap()
            .contains("feat/tie")
    );

    // Both called alike: their one table runs, the tab goes to the lower id.
    let twin = make_space(&daemon, "web");
    space_dir(&daemon, &twin, &repo);
    space_dir(&daemon, &api, root.path());
    let made = daemon.run_in(&repo, &["worktree", "new", "feat/twins"]);
    assert!(made.status.success(), "{}", text(&made.stderr));
    assert_eq!(said(&made, "setup").as_deref(), Some("done"));
    let terminal = said(&made, "terminal").expect("a terminal");
    assert_eq!(workspace_of(&daemon, &terminal), web);
}

/// Parallel `new`s take distinct indexes, and a worktree made again on a reused git id
/// keeps the number `new` gave it when `worktree setup` runs.
#[test]
fn parallel_worktrees_take_distinct_indexes_and_keep_them() {
    let daemon = Daemon::start();
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let vars = root.path().join("vars");
    daemon.config(&format!(
        "[spaces.app.worktree]\nsetup = {}\n",
        literal(&recording_setup(&vars))
    ));
    let index_of = |out: &Output| {
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stderr)
            .rsplit("SETUP_SAID_")
            .next()
            .and_then(|s| s.lines().next())
            .expect("an index")
            .to_string()
    };
    let branches = ["feat/p0", "feat/p1", "feat/p2", "feat/p3"];
    let indexes: std::collections::BTreeMap<&str, String> = std::thread::scope(|scope| {
        let runs: Vec<_> = branches
            .iter()
            .map(|b| {
                let (daemon, repo) = (&daemon, &repo);
                scope.spawn(move || {
                    (
                        *b,
                        daemon.run_in(repo, &["worktree", "new", b, "--space", "app"]),
                    )
                })
            })
            .collect();
        runs.into_iter()
            .map(|r| {
                let (branch, out) = r.join().unwrap();
                (branch, index_of(&out))
            })
            .collect()
    });
    let distinct: std::collections::BTreeSet<&String> = indexes.values().collect();
    assert_eq!(distinct.len(), 4, "{indexes:?}");

    let listed = text(&daemon.run(&["agents"]).stdout);
    let terminal = listed
        .lines()
        .find(|l| l.contains("name=feat/p1"))
        .and_then(|l| l.split(' ').next()?.strip_prefix("term="))
        .unwrap_or_else(|| panic!("no feat/p1 tab: {listed}"));
    daemon.close(terminal);
    let removed = daemon.run_in(&repo, &["worktree", "rm", "feat/p1"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    git(&repo, &["branch", "-D", "feat/p1"]);
    let made = daemon.run_in(&repo, &["worktree", "new", "feat/p1", "--space", "app"]);
    let set_up = daemon.run_in(&repo, &["worktree", "setup", "feat/p1", "--space", "app"]);
    assert_eq!(index_of(&made), indexes["feat/p1"]);
    assert_eq!(index_of(&set_up), index_of(&made));
    let claims = std::fs::read_dir(repo.join(".git/qoop-worktree-index"))
        .unwrap()
        .filter(|e| {
            std::fs::read_to_string(e.as_ref().unwrap().path()).unwrap() == "my-repo-wt-feat-p1"
        })
        .count();
    assert_eq!(claims, 1);
}

/// Run qoop where no daemon runs, with `bin` first on `PATH` when given (a fake `gh`: CI
/// runners keep the real one in `/usr/bin` beside git, so it cannot be left off).
fn qoop_in(cwd: &Path, args: &[&str], bin: Option<&Path>) -> Output {
    let runtime = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod");
    let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
    cmd.args(args)
        .current_dir(cwd)
        .env("QOOP_RUNTIME_DIR", runtime.path());
    if let Some(bin) = bin {
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", bin.display()));
    }
    cmd.output().expect("run qoop")
}

/// [`qoop_in`] with `path` as the whole `PATH`.
fn qoop_alone(cwd: &Path, args: &[&str], path: &Path) -> Output {
    let runtime = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod");
    qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"))
        .args(args)
        .current_dir(cwd)
        .env("QOOP_RUNTIME_DIR", runtime.path())
        .env("PATH", path)
        .output()
        .expect("run qoop")
}

/// A fake `gh` in `<dir>/bin`: it notes each argv in `<dir>/bin/argv`, answers `pr list
/// --state merged` from `merged` and `--state open` from `open` (none: `[]`), and `pr create`
/// with a URL it then lists as open, or exit 1 while `create-fails` is there. `fails` makes it
/// exit 1 to everything and leave `ran` instead.
fn fake_gh(dir: &Path, fails: bool) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    let body = if fails {
        "touch \"$d/ran\"\nexit 1\n"
    } else {
        "echo \"$*\" >> \"$d/argv\"\n\
         case \"$*\" in\n\
         *\"--state merged\"*) cat \"$d/merged\" 2>/dev/null || echo '[]' ;;\n\
         *\"--state open\"*) cat \"$d/open\" 2>/dev/null || echo '[]' ;;\n\
         \"pr create\"*) [ -e \"$d/create-fails\" ] && exit 1; echo '[{\"url\":\"https://github.com/o/r/pull/7\"}]' > \"$d/open\"; \
         echo https://github.com/o/r/pull/7 ;;\n\
         *) exit 1 ;;\n\
         esac\n"
    };
    let gh = bin.join("gh");
    std::fs::write(&gh, format!("#!/bin/sh\nd=$(dirname \"$0\")\n{body}")).expect("gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    bin
}

/// "my repo" with a bare `origin` whose HEAD names a branch never pushed, so there is no
/// `origin/HEAD` whatever the machine's `init.defaultBranch`, and the proofs go through
/// `origin/main`.
fn with_origin(root: &Path) -> (PathBuf, PathBuf) {
    let bare = root.join("origin.git");
    git(root, &["init", "-q", "--bare", "-b", "trunk", "origin.git"]);
    let repo = repo(root);
    git(
        &repo,
        &["remote", "add", "origin", bare.to_str().expect("utf8")],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    (repo, bare)
}

/// A linked worktree on a new branch, made by plain git, beside the repository.
fn add_worktree(repo: &Path, branch: &str) -> PathBuf {
    let path = repo
        .parent()
        .expect("parent")
        .join(format!("wt-{}", branch.replace('/', "-")));
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            path.to_str().expect("utf8"),
        ],
    );
    std::fs::canonicalize(&path).expect("made")
}

/// Commit `file` holding `text` in `dir`; its id.
fn commit(dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).expect("write");
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

fn has_ref(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet", name])
        .status()
        .expect("git")
        .success()
}

#[test]
fn finish_refuses_unmerged_work_and_proves_a_rebase_merge_after_fetching() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let gh = fake_gh(root.path(), true);
    let wt = add_worktree(&repo, "feat/a");
    commit(&wt, "a.txt", "a\n");

    let refused = qoop_in(&repo, &["worktree", "finish", "feat/a"], Some(&gh));
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("feat/a is not merged into origin/main: --pr"),
        "{}",
        text(&refused.stderr)
    );
    assert!(wt.exists());

    // Rebase-merged on the remote after the worktree was made: the same change under another
    // id, which only the fetch brings here.
    git(
        root.path(),
        &["clone", "-q", "-b", "main", bare.to_str().unwrap(), "other"],
    );
    let other = root.path().join("other");
    commit(&other, "first.txt", "unrelated\n");
    commit(&other, "a.txt", "a\n");
    git(&other, &["push", "-q", "origin", "main"]);
    let done = qoop_in(&repo, &["worktree", "finish", "feat/a"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    let out = text(&done.stdout);
    assert!(out.contains("origin/feat/a already gone"), "{out}");
    assert!(
        out.contains(&format!("removed {}; branch feat/a deleted", wt.display())),
        "{out}"
    );
    assert!(!wt.exists());
    assert!(!has_ref(&repo, "refs/heads/feat/a"));
}

#[test]
fn finish_deletes_the_remote_branch_only_at_the_merged_tip() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let gh = fake_gh(root.path(), true);

    let ff = add_worktree(&repo, "feat/b");
    commit(&ff, "b.txt", "b\n");
    git(&ff, &["push", "-q", "origin", "feat/b"]);
    git(&repo, &["merge", "-q", "--ff-only", "feat/b"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let done = qoop_in(&repo, &["worktree", "finish", "feat/b"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(text(&done.stdout).contains("deleted origin/feat/b"));
    assert!(!has_ref(&bare, "refs/heads/feat/b"));
    assert!(!ff.exists());

    // The remote branch is behind the merged tip: not proven, so kept and said.
    let moved = add_worktree(&repo, "feat/c");
    let pushed = commit(&moved, "c1.txt", "c1\n");
    git(&moved, &["push", "-q", "origin", "feat/c"]);
    commit(&moved, "c2.txt", "c2\n");
    git(&repo, &["merge", "-q", "--ff-only", "feat/c"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let kept = qoop_in(&repo, &["worktree", "finish", "feat/c"], Some(&gh));
    assert!(kept.status.success(), "{}", text(&kept.stderr));
    assert!(
        text(&kept.stdout).contains(&format!("kept origin/feat/c: it is at {}", &pushed[..12])),
        "{}",
        text(&kept.stdout)
    );
    assert!(has_ref(&bare, "refs/heads/feat/c"));
    assert!(!moved.exists());
}

#[test]
fn finish_pr_pushes_reuses_the_pr_and_keeps_the_worktree_until_gh_says_merged() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let gh = fake_gh(root.path(), false);
    let argv = || std::fs::read_to_string(gh.join("argv")).unwrap_or_default();

    let wt = add_worktree(&repo, "feat/p");
    let tip = commit(&wt, "p.txt", "p\n");
    for _ in 0..2 {
        let pr = qoop_in(&repo, &["worktree", "finish", "feat/p", "--pr"], Some(&gh));
        assert!(pr.status.success(), "{}", text(&pr.stderr));
        assert_eq!(
            text(&pr.stdout),
            format!(
                "pushed\npr=https://github.com/o/r/pull/7\nkept {} until the PR merges; qoop \
                 worktree finish feat/p then cleans up\n",
                wt.display()
            )
        );
    }
    assert_eq!(argv().matches("pr create").count(), 1, "{}", argv());
    assert!(
        argv().contains("pr create --fill --head feat/p\n"),
        "{}",
        argv()
    );
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/p"));
    assert_eq!(git(&bare, &["rev-parse", "feat/p"]), tip);

    // `--base` reaches gh without its `origin/`, and only when given.
    std::fs::remove_file(gh.join("open")).unwrap();
    let based = add_worktree(&repo, "feat/q");
    commit(&based, "q.txt", "q\n");
    let pr = qoop_in(
        &repo,
        &[
            "worktree",
            "finish",
            "feat/q",
            "--pr",
            "--base",
            "origin/main",
        ],
        Some(&gh),
    );
    assert!(pr.status.success(), "{}", text(&pr.stderr));
    assert!(
        argv().contains("pr create --fill --head feat/q --base main\n"),
        "{}",
        argv()
    );

    // Squash-merged on GitHub: gh's merged PR with this head, into the base's branch, is the
    // proof; one into another branch is not.
    let merged_into = |base: &str| {
        std::fs::write(
            gh.join("merged"),
            format!("[{{\"headRefOid\":\"{tip}\",\"baseRefName\":\"{base}\"}}]"),
        )
        .unwrap();
    };
    merged_into("release");
    let elsewhere = qoop_in(&repo, &["worktree", "finish", "feat/p"], Some(&gh));
    assert_eq!(elsewhere.status.code(), Some(1));
    assert!(text(&elsewhere.stderr).contains("feat/p is not merged into origin/main"));
    merged_into("main");
    let done = qoop_in(&repo, &["worktree", "finish", "feat/p"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(text(&done.stdout).contains("deleted origin/feat/p"));
    assert!(!wt.exists() && !has_ref(&repo, "refs/heads/feat/p"));
    // The upstream `--pr` set goes with the branch, as `branch -D` would drop it.
    let upstream = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["config", "--get", "branch.feat/p.remote"])
        .output()
        .expect("git");
    assert!(!upstream.status.success(), "{}", text(&upstream.stdout));
    std::fs::remove_file(gh.join("merged")).unwrap();

    // gh that fails: pushed, said, kept, exit 1.
    let broken = fake_gh(&root.path().join("broken"), true);
    let wt = add_worktree(&repo, "feat/f");
    commit(&wt, "f.txt", "f\n");
    let pr = qoop_in(
        &repo,
        &["worktree", "finish", "feat/f", "--pr"],
        Some(&broken),
    );
    assert_eq!(pr.status.code(), Some(1));
    assert_eq!(text(&pr.stdout), "pushed\n");
    assert!(broken.join("ran").exists(), "the fake gh never ran");
    assert!(
        text(&pr.stderr).contains("gh could not list the pull requests of feat/f"),
        "{}",
        text(&pr.stderr)
    );
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/f"));

    // gh that lists but cannot create.
    std::fs::remove_file(gh.join("open")).unwrap();
    std::fs::write(gh.join("create-fails"), "").unwrap();
    let wt = add_worktree(&repo, "feat/g");
    commit(&wt, "g.txt", "g\n");
    let pr = qoop_in(&repo, &["worktree", "finish", "feat/g", "--pr"], Some(&gh));
    assert_eq!(pr.status.code(), Some(1));
    assert!(
        text(&pr.stderr).contains("gh could not open the PR (run gh pr create to see why)"),
        "{}",
        text(&pr.stderr)
    );

    // No gh on PATH at all: only git there.
    let only_git = root.path().join("only-git");
    std::fs::create_dir(&only_git).unwrap();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|d| d.join("git"))
        .find(|g| g.is_file())
        .expect("git on PATH");
    // A wrapper, not a symlink: git finds its helpers from its own path, not the link's.
    let wrapper = only_git.join("git");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", real_git.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wt = add_worktree(&repo, "feat/h");
    commit(&wt, "h.txt", "h\n");
    let pr = qoop_alone(&repo, &["worktree", "finish", "feat/h", "--pr"], &only_git);
    assert_eq!(pr.status.code(), Some(1), "{}", text(&pr.stderr));
    assert_eq!(text(&pr.stdout), "pushed\n");
    assert!(
        text(&pr.stderr).contains("gh is not available: open the PR by hand"),
        "{}",
        text(&pr.stderr)
    );
}

#[test]
fn finish_merge_fast_forwards_the_main_branch_after_every_refusal() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let wt = add_worktree(&repo, "feat/m");
    let pushed = commit(&wt, "m1.txt", "m1\n");
    // As an earlier `--pr` leaves it: pushed with an upstream, then a later commit.
    git(&wt, &["push", "-q", "-u", "origin", "feat/m"]);
    let tip = commit(&wt, "m2.txt", "m2\n");
    let before = git(&repo, &["rev-parse", "main"]);

    let mut inside = Command::new("sleep")
        .arg("300")
        .current_dir(&wt)
        .spawn()
        .expect("a process inside");
    let refused = qoop_in(&repo, &["worktree", "finish", "feat/m", "--merge"], None);
    let _ = inside.kill();
    let _ = inside.wait();
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("(sleep)"),
        "{}",
        text(&refused.stderr)
    );
    assert_eq!(git(&repo, &["rev-parse", "main"]), before, "main moved");

    let elsewhere = qoop_in(
        &repo,
        &["worktree", "finish", "feat/m", "--merge", "--base", "dev"],
        None,
    );
    assert_eq!(elsewhere.status.code(), Some(1));
    assert!(text(&elsewhere.stderr).contains("--merge fast-forwards main"));

    let merged = qoop_in(
        &repo,
        &[
            "worktree",
            "finish",
            "feat/m",
            "--merge",
            "--base",
            "origin/main",
        ],
        None,
    );
    assert!(merged.status.success(), "{}", text(&merged.stderr));
    assert!(text(&merged.stdout).starts_with("merged feat/m into main (fast-forward)\nremoved "));
    assert_eq!(git(&repo, &["rev-parse", "main"]), tip);
    assert!(!wt.exists() && !has_ref(&repo, "refs/heads/feat/m"));
    assert_eq!(
        git(&bare, &["rev-parse", "feat/m"]),
        pushed,
        "the remote is left alone"
    );
}

#[test]
fn finish_without_origin_or_with_discard_needs_no_remote() {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());

    let merged = add_worktree(&repo, "feat/n");
    commit(&merged, "n.txt", "n\n");
    git(&repo, &["merge", "-q", "--ff-only", "feat/n"]);
    let done = qoop_in(&repo, &["worktree", "finish", "feat/n"], None);
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert_eq!(
        text(&done.stdout),
        format!("removed {}; branch feat/n deleted\n", merged.display())
    );

    let clean = add_worktree(&repo, "feat/o");
    let pr = qoop_in(&repo, &["worktree", "finish", "feat/o", "--pr"], None);
    assert_eq!(pr.status.code(), Some(1));
    assert!(
        text(&pr.stderr).contains("no origin to push to"),
        "{}",
        text(&pr.stderr)
    );
    assert!(clean.exists());

    let dropped = add_worktree(&repo, "feat/d");
    commit(&dropped, "d.txt", "d\n");
    std::fs::write(dropped.join("loose.txt"), "not committed").unwrap();
    let refused = qoop_in(&repo, &["worktree", "finish", "feat/d"], None);
    assert_eq!(refused.status.code(), Some(1));
    assert!(text(&refused.stderr).contains("uncommitted changes"));
    let pr = qoop_in(&repo, &["worktree", "finish", "feat/d", "--pr"], None);
    assert!(
        text(&pr.stderr).contains("uncommitted changes"),
        "dirty first"
    );
    let gone = qoop_in(&repo, &["worktree", "finish", "feat/d", "--discard"], None);
    assert!(gone.status.success(), "{}", text(&gone.stderr));
    assert!(!dropped.exists() && !has_ref(&repo, "refs/heads/feat/d"));

    for args in [
        &["worktree", "finish"][..],
        &["worktree", "finish", "x", "--pr", "--merge"],
        &["worktree", "finish", "x", "--base"],
        &["worktree", "finish", "x", "--force"],
    ] {
        assert_eq!(
            qoop_in(&repo, args, None).status.code(),
            Some(2),
            "{args:?}"
        );
    }
    let main = qoop_in(&repo, &["worktree", "finish", "main"], None);
    assert_eq!(main.status.code(), Some(1));
}

#[test]
fn a_remote_step_that_fails_keeps_everything_and_a_rerun_finishes() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let wt = add_worktree(&repo, "feat/r");
    commit(&wt, "r.txt", "r\n");
    git(&wt, &["push", "-q", "origin", "feat/r"]);
    git(&repo, &["merge", "-q", "--ff-only", "feat/r"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let gh = fake_gh(root.path(), true);

    // The remote refuses every ref change while its update hook says no.
    let hook = bare.join("hooks/update");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let failed = qoop_in(&repo, &["worktree", "finish", "feat/r"], Some(&gh));
    std::fs::remove_file(&hook).unwrap();
    assert_eq!(failed.status.code(), Some(1), "{}", text(&failed.stdout));
    assert!(
        text(&failed.stderr).contains("could not delete origin/feat/r"),
        "{}",
        text(&failed.stderr)
    );
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/r"));
    let done = qoop_in(&repo, &["worktree", "finish", "feat/r"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(text(&done.stdout).contains("deleted origin/feat/r"));
    assert!(!wt.exists());

    // An origin that cannot be asked: neither 0 nor 2 from ls-remote stops it.
    let wt = add_worktree(&repo, "feat/s");
    commit(&wt, "s.txt", "s\n");
    git(&repo, &["merge", "-q", "--ff-only", "feat/s"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let nowhere = root.path().join("nowhere.git");
    git(
        &repo,
        &["remote", "set-url", "origin", nowhere.to_str().unwrap()],
    );
    let stopped = qoop_in(&repo, &["worktree", "finish", "feat/s"], Some(&gh));
    assert_eq!(stopped.status.code(), Some(1));
    assert!(
        text(&stopped.stderr).contains("could not ask origin about feat/s"),
        "{}",
        text(&stopped.stderr)
    );
    assert!(!text(&stopped.stdout).contains("already gone"));
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/s"));
}

/// `ls --stale`: clean linked worktrees nothing runs in, one reason each, the first that
/// holds: empty before merged, then old; local refs only.
#[test]
fn ls_stale_lists_empty_merged_and_old_worktrees_and_nothing_in_use() {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let empty = add_worktree(&repo, "feat/empty");
    let merged = add_worktree(&repo, "feat/merged");
    commit(&merged, "m.txt", "m\n");
    // The same change on top of another, so under another id: a rebase merge.
    commit(&repo, "first.txt", "first\n");
    commit(&repo, "m.txt", "m\n");
    let old = add_worktree(&repo, "feat/old");
    std::fs::write(old.join("o.txt"), "o\n").unwrap();
    git(&old, &["add", "o.txt"]);
    let forty_days_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 40 * 86_400;
    let made = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(&old)
        .args(["commit", "-q", "-m", "old"])
        .env("GIT_COMMITTER_DATE", format!("@{forty_days_ago} +0000"))
        .env("GIT_AUTHOR_DATE", format!("@{forty_days_ago} +0000"))
        .status()
        .expect("git");
    assert!(made.success());
    let fresh = add_worktree(&repo, "feat/fresh");
    commit(&fresh, "f.txt", "f\n");
    let loose = add_worktree(&repo, "feat/loose");
    std::fs::write(loose.join("loose.txt"), "x").unwrap();
    let busy = add_worktree(&repo, "feat/busy");
    let mut inside = Command::new("sleep")
        .arg("300")
        .current_dir(&busy)
        .spawn()
        .expect("a process inside");

    // A worktree git cannot read is passed by and said, never the end of the listing.
    let broken = add_worktree(&repo, "feat/broken");
    std::fs::write(broken.join(".git"), "gitdir: /nonexistent/qoop-test\n").unwrap();
    let listed = qoop_in(&repo, &["worktree", "ls", "--stale"], None);
    let as_json = qoop_in(&repo, &["worktree", "ls", "--json", "--stale"], None);
    let _ = inside.kill();
    let _ = inside.wait();
    assert!(listed.status.success(), "{}", text(&listed.stderr));
    assert!(
        text(&listed.stderr).contains(&format!("ls: skipping {} (", broken.display())),
        "{}",
        text(&listed.stderr)
    );
    assert_eq!(
        text(&listed.stdout),
        format!(
            "path={} branch=feat/empty terminals=0 why=empty\n\
             path={} branch=feat/merged terminals=0 why=merged\n\
             path={} branch=feat/old terminals=0 why=old:40d\n",
            empty.display(),
            merged.display(),
            old.display()
        )
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&as_json.stdout).expect("a JSON array");
    let whys: Vec<&str> = rows.iter().map(|r| r["why"].as_str().unwrap()).collect();
    assert_eq!(whys, ["empty", "merged", "old:40d"]);
    for row in &rows {
        for key in ["path", "branch", "head", "bare", "terminals"] {
            assert!(row.get(key).is_some(), "{key} missing: {row}");
        }
    }
    let plain = qoop_in(&repo, &["worktree", "ls", "--json"], None);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&plain.stdout).expect("JSON");
    assert_eq!(rows.len(), 8);
    assert!(rows.iter().all(|r| r.get("why").is_none()));
    let twice = qoop_in(&repo, &["worktree", "ls", "--stale", "--stale"], None);
    assert_eq!(twice.status.code(), Some(2));
}

/// A merge commit can carry a change nothing on the base holds; `git cherry` skips merges,
/// so a range with one is never proven by it.
#[test]
fn an_evil_merge_is_never_proven_by_git_cherry() {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    git(&repo, &["branch", "side"]);
    let side = root.path().join("side-wt");
    git(
        &repo,
        &["worktree", "add", "-q", side.to_str().unwrap(), "side"],
    );
    commit(&side, "s.txt", "s\n");
    let wt = add_worktree(&repo, "feat/e");
    commit(&wt, "x.txt", "x\n");
    git(&wt, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    std::fs::write(wt.join("evil.txt"), "only in the merge\n").unwrap();
    git(&wt, &["add", "evil.txt"]);
    git(&wt, &["commit", "-q", "-m", "merge side"]);
    // The base holds both plain changes under other ids, never the merge's own.
    commit(&repo, "first.txt", "first\n");
    commit(&repo, "x.txt", "x\n");
    commit(&repo, "s.txt", "s\n");
    let refused = qoop_in(&repo, &["worktree", "finish", "feat/e"], None);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stdout));
    assert!(
        text(&refused.stderr).contains("feat/e is not merged into main"),
        "{}",
        text(&refused.stderr)
    );
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/e"));
}

/// A tag named like the base or the branch, and a branch whose name starts with `+`, are
/// never read as something else: every ref is named in full.
#[test]
fn full_ref_names_keep_tags_and_plus_names_from_being_taken_for_branches() {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let first = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["tag", "main", &first]);
    let wt = add_worktree(&repo, "feat/t");
    commit(&wt, "t.txt", "t\n");
    git(&repo, &["merge", "-q", "--ff-only", "refs/heads/feat/t"]);
    let done = qoop_in(&repo, &["worktree", "finish", "feat/t"], None);
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(!wt.exists());

    let wt = add_worktree(&repo, "feat/u");
    let tip = commit(&wt, "u.txt", "u\n");
    git(&repo, &["tag", "feat/u", &first]);
    let merged = qoop_in(&repo, &["worktree", "finish", "feat/u", "--merge"], None);
    assert!(merged.status.success(), "{}", text(&merged.stderr));
    assert_eq!(git(&repo, &["rev-parse", "refs/heads/main"]), tip);

    // `+feat` pushed by name is a forced push of `feat`: a local and a remote `feat` that
    // went apart show it never happens, and `+feat` itself is what is pushed.
    let other = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(other.path());
    let ours = add_worktree(&repo, "feat");
    commit(&ours, "ours.txt", "ours\n");
    let theirs = add_worktree(&repo, "theirs");
    commit(&theirs, "theirs.txt", "theirs\n");
    git(
        &repo,
        &["push", "-q", "origin", "refs/heads/theirs:refs/heads/feat"],
    );
    let local_feat = git(&repo, &["rev-parse", "refs/heads/feat"]);
    let remote_feat = git(&bare, &["rev-parse", "refs/heads/feat"]);
    assert_ne!(local_feat, remote_feat);
    let plus = add_worktree(&repo, "+feat");
    let plus_tip = commit(&plus, "p.txt", "p\n");
    let gh = fake_gh(other.path(), false);
    let pr = qoop_in(&repo, &["worktree", "finish", "+feat", "--pr"], Some(&gh));
    assert!(pr.status.success(), "{}", text(&pr.stderr));
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/feat"]), remote_feat);
    assert_eq!(git(&repo, &["rev-parse", "refs/heads/feat"]), local_feat);
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/+feat"]), plus_tip);
}

/// The local branch is deleted only at the commit that was proven: one that moved since is
/// kept and said. Here the remote's update hook moves it while its delete is pushed.
#[test]
fn a_branch_that_moved_after_its_proof_is_kept() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let gh = fake_gh(root.path(), true);
    let wt = add_worktree(&repo, "feat/v");
    commit(&wt, "v.txt", "v\n");
    git(&wt, &["push", "-q", "origin", "feat/v"]);
    git(&repo, &["merge", "-q", "--ff-only", "refs/heads/feat/v"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let hook = bare.join("hooks/update");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n\
             unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_QUARANTINE_PATH\n\
             export GIT_DIR='{}'\n\
             late=$(git -c user.name=t -c user.email=t@t commit-tree 'refs/heads/feat/v^{{tree}}' \
             -p refs/heads/feat/v -m late)\n\
             git update-ref refs/heads/feat/v \"$late\"\n\
             exit 0\n",
            repo.join(".git").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let kept = qoop_in(&repo, &["worktree", "finish", "feat/v"], Some(&gh));
    assert_eq!(kept.status.code(), Some(1), "{}", text(&kept.stdout));
    assert!(
        text(&kept.stderr).contains("branch feat/v moved to")
            && text(&kept.stderr).contains("so it was kept"),
        "{}",
        text(&kept.stderr)
    );
    assert!(has_ref(&repo, "refs/heads/feat/v"));
}

#[test]
fn the_remotes_default_branch_is_never_deleted() {
    let root = tempfile::tempdir().expect("tempdir");
    let (repo, bare) = with_origin(root.path());
    let gh = fake_gh(root.path(), true);
    let wt = add_worktree(&repo, "feat/d");
    commit(&wt, "d.txt", "d\n");
    git(&wt, &["push", "-q", "origin", "feat/d"]);
    git(&repo, &["merge", "-q", "--ff-only", "refs/heads/feat/d"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&bare, &["symbolic-ref", "HEAD", "refs/heads/feat/d"]);
    let done = qoop_in(&repo, &["worktree", "finish", "feat/d"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(
        text(&done.stdout).contains("kept origin/feat/d: it is the remote's default branch"),
        "{}",
        text(&done.stdout)
    );
    assert!(has_ref(&bare, "refs/heads/feat/d"));
    assert!(!wt.exists());
}

#[test]
fn finish_refuses_a_process_inside_and_a_base_that_is_the_branch_itself() {
    let root = tempfile::tempdir().expect("tempdir");
    let repo = repo(root.path());
    let wt = add_worktree(&repo, "feat/i");
    commit(&wt, "i.txt", "i\n");
    git(&repo, &["merge", "-q", "--ff-only", "refs/heads/feat/i"]);
    let mut inside = Command::new("sleep")
        .arg("300")
        .current_dir(&wt)
        .spawn()
        .expect("a process inside");
    let plain = qoop_in(&repo, &["worktree", "finish", "feat/i"], None);
    let discard = qoop_in(&repo, &["worktree", "finish", "feat/i", "--discard"], None);
    let _ = inside.kill();
    let _ = inside.wait();
    for out in [&plain, &discard] {
        assert_eq!(out.status.code(), Some(1));
        assert!(
            text(&out.stderr).contains("(sleep)"),
            "{}",
            text(&out.stderr)
        );
    }
    assert!(wt.exists() && has_ref(&repo, "refs/heads/feat/i"));

    let tip = git(&repo, &["rev-parse", "refs/heads/feat/i"]);
    for base in ["feat/i", "origin/feat/i", "refs/heads/feat/i", tip.as_str()] {
        let out = qoop_in(
            &repo,
            &["worktree", "finish", "feat/i", "--base", base],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{base}");
        assert!(
            text(&out.stderr).contains("is the branch being finished"),
            "{}",
            text(&out.stderr)
        );
    }
    let out = qoop_in(
        &repo,
        &[
            "worktree",
            "finish",
            "feat/i",
            "--discard",
            "--base",
            "main",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(wt.exists());
}

/// A clone has `origin/HEAD`: the base is the branch it points at, so a squash merge gh
/// knows into `main` proves it, where `HEAD` would match no pull request.
#[test]
fn origin_head_stands_for_its_branch_in_a_gh_proof() {
    let root = tempfile::tempdir().expect("tempdir");
    let seed = repo_named(root.path(), "seed");
    git(
        root.path(),
        &["init", "-q", "--bare", "-b", "main", "up.git"],
    );
    let up = root.path().join("up.git");
    git(&seed, &["push", "-q", up.to_str().unwrap(), "main"]);
    git(
        root.path(),
        &["clone", "-q", up.to_str().unwrap(), "my repo"],
    );
    let repo = root.path().join("my repo");
    assert_eq!(
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]),
        "refs/remotes/origin/main"
    );
    let gh = fake_gh(root.path(), false);
    let wt = add_worktree(&repo, "feat/sq");
    let tip = commit(&wt, "sq.txt", "sq\n");
    git(&wt, &["push", "-q", "origin", "feat/sq"]);
    std::fs::write(
        gh.join("merged"),
        format!("[{{\"headRefOid\":\"{tip}\",\"baseRefName\":\"main\"}}]"),
    )
    .unwrap();
    let done = qoop_in(&repo, &["worktree", "finish", "feat/sq"], Some(&gh));
    assert!(done.status.success(), "{}", text(&done.stderr));
    assert!(
        text(&done.stdout).contains("deleted origin/feat/sq"),
        "{}",
        text(&done.stdout)
    );
    assert!(!wt.exists() && !has_ref(&repo, "refs/heads/feat/sq"));
}

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
        let child = Command::new(env!("CARGO_BIN_EXE_qoop"))
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
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
        Command::new(env!("CARGO_BIN_EXE_qoop"))
            .args(args)
            .current_dir(cwd)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_SESSION")
            .env_remove("QORO_SESSION")
            .output()
            .expect("run qoop")
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
    for args in [&["new", "--", "true"][..], &["read", "1"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_qoop"))
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
    // The reviewer's case: the agent is working when the prompt lands, reports one
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
    let mut watcher = Command::new(env!("CARGO_BIN_EXE_qoop"))
        .args(["wait", "1", "--until", "blocked", "--timeout", PATIENCE_ARG])
        .current_dir(&checkout)
        .env("QOOP_RUNTIME_DIR", daemon.dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a qoop client inside");

    // Run from a shell standing inside: the shell is the caller's own, and the answer is
    // to step out rather than to close it. `; true` keeps the shell from exec-ing qoop.
    let inside = Command::new("/bin/sh")
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
    let mut inside = Command::new(env!("CARGO_BIN_EXE_qoop"))
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
    for args in [&["worktree"][..], &["worktree", "bogus"]] {
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

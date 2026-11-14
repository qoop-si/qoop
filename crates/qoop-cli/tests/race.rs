//! `qoop race` against a real daemon, through the real socket, with a stand-in `claude` (a
//! shell script on the PATH) whose prompt says how it behaves: `DONE` writes a file in its
//! worktree and ends its turn, `BLOCK` stops at a question, `EXIT` works until the test makes a
//! file and then quits to its shell, `WORK` keeps working. Each reports as the claude hook does
//! and then keeps running as itself, so its tab stays an agent.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The stand-in agent. `$1` is its first message; it logs it, then behaves as it says.
const STUB: &str = r#"#!/bin/sh
dir='@DIR@'
printf '%s\n' "$1" > "$dir/said.$QOOP_TERMINAL"
printf '%s %s\n' "$QOOP_TERMINAL" "$QOOP_TOKEN" > "$dir/token.$QOOP_TERMINAL"
qoop hook-report --state working --event user
case "$1" in
  DONE*) printf 'a\nb\n' > raced.txt
         qoop hook-report --state idle --visible-idle --event done ;;
  BLOCK*) qoop hook-report --state blocked --visible-blocker --event blocked --label "Bash rm -rf build" ;;
  EXIT*) while [ ! -e "$dir/exit" ]; do /bin/sleep 0.2; done; exit 0 ;;
esac
while [ -d "$dir" ]; do /bin/sleep 1; done
"#;

/// How long a test lets `--wait` look before an agent it must have seen goes away: it lists the
/// tabs once a second, and a racer it never saw as an agent reads `not-started`, not `exited`.
const SEEN_BY_WAIT: Duration = Duration::from_secs(3);

struct Stack {
    child: Option<Child>,
    dir: tempfile::TempDir,
}

impl Stack {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_qoop"), bin.join("qoop")).unwrap();
        let stub = STUB.replace("@DIR@", &dir.path().display().to_string());
        std::fs::write(bin.join("claude"), stub).unwrap();
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::create_dir_all(dir.path().join("w")).unwrap();
        let mut stack = Self { child: None, dir };
        let child = stack
            .command(stack.dir.path())
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the daemon");
        stack.child = Some(child);
        let socket = qoop_testkit::socket_in(stack.dir.path());
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !socket.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(socket.exists(), "the daemon never bound its socket");
        stack
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut cmd = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        cmd.current_dir(cwd)
            .env("QOOP_RUNTIME_DIR", self.dir.path())
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

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `qoop` in `cwd`, from outside every terminal.
    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd).args(args).output().expect("run qoop")
    }

    /// A committed repository `w/<name>`, its worktrees beside it.
    fn repo(&self, name: &str) -> PathBuf {
        let repo = self.path("w").join(name);
        std::fs::create_dir(&repo).expect("mkdir");
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        repo
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        let out = self.run_in(self.dir.path(), &["agents", "--json"]);
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    fn wait(&self, mut until: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if until() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }

    /// The rows named `race/<name>/...`, whichever repository they race in.
    fn racers(&self, name: &str) -> Vec<serde_json::Value> {
        let prefix = format!("race/{name}/");
        self.rows()
            .into_iter()
            .filter(|r| r["name"].as_str().is_some_and(|s| s.starts_with(&prefix)))
            .collect()
    }

    /// Whether every row named `race/<name>/...` is an agent, and there are `n` of them.
    fn agents_in(&self, name: &str, n: usize) -> bool {
        let racers = self.racers(name);
        racers.len() == n && racers.iter().all(|r| r["agent"] == true)
    }

    /// Whether there are `n` rows named `race/<name>/...`, each `working`.
    fn working_in(&self, name: &str, n: usize) -> bool {
        let racers = self.racers(name);
        racers.len() == n && racers.iter().all(|r| r["status"] == "working")
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// git with a fixed identity, required to succeed.
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

fn has_ref(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", name])
        .output()
        .expect("git")
        .status
        .success()
}

/// The terminal numbers a `race new` printed, in its order.
fn terminals(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("agent=") && l.contains(" branch="))
        .filter_map(|l| l.split(" terminal=").nth(1))
        .filter_map(|l| l.split(' ').next())
        .map(String::from)
        .collect()
}

#[test]
fn a_race_runs_from_start_to_its_winner() {
    let stack = Stack::start();
    let repo = stack.repo("my repo");
    // Started from a linked checkout on a branch of its own: the base is its HEAD, not main's.
    let feat = stack.path("w").join("feat");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            &feat.display().to_string(),
        ],
    );
    git(&feat, &["commit", "-q", "--allow-empty", "-m", "on feat"]);
    let head = git(&feat, &["rev-parse", "HEAD"]);
    assert_ne!(head, git(&repo, &["rev-parse", "HEAD"]));
    let out = stack.run_in(
        &feat,
        &[
            "race",
            "new",
            "fix",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "60",
            "--",
            "DONE",
            "the",
            "thing",
        ],
    );
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{said}{}", text(&out.stderr));
    assert!(
        said.starts_with(&format!("race=fix base={head} (HEAD)\n")),
        "{said}"
    );
    assert!(
        said.contains("agent=claude branch=race/fix/claude "),
        "{said}"
    );
    assert!(
        said.contains("agent=claude-2 branch=race/fix/claude-2 "),
        "{said}"
    );
    let tabs = terminals(&said);
    assert_eq!(tabs.len(), 2, "{said}");
    for tab in &tabs {
        assert!(
            said.contains(&format!("terminal={tab} end=finished")),
            "{said}"
        );
        let first = std::fs::read_to_string(stack.path(&format!("said.{tab}"))).unwrap();
        assert!(
            first.starts_with("DONE the thing\n\nRace rules: change files in this worktree only."),
            "the racer rule follows the prompt: {first}"
        );
    }
    assert_eq!(git(&repo, &["rev-parse", "refs/qoop/race/fix"]), head);
    for branch in ["race/fix/claude", "race/fix/claude-2"] {
        assert_eq!(git(&repo, &["rev-parse", branch]), head);
    }
    let ls = stack.run_in(&repo, &["race", "ls", "fix"]);
    let listed = text(&ls.stdout);
    assert!(ls.status.success(), "{}", text(&ls.stderr));
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 2, "{listed}");
    assert!(
        lines[0].starts_with(&format!(
            "agent=claude terminal={} status=done files=1 added=2 removed=0 tests=- worktree=",
            tabs[0]
        )),
        "{listed}"
    );
    assert!(lines[0].ends_with("my repo-wt-race-fix-claude"), "{listed}");
    assert!(lines[1].starts_with("agent=claude-2 "), "{listed}");

    // Started once only.
    let again = stack.run_in(
        &repo,
        &["race", "new", "fix", "--agents", "claude,codex", "--", "x"],
    );
    assert!(
        text(&again.stderr).contains("race fix exists"),
        "{}",
        text(&again.stderr)
    );

    // Judged by a command run in each worktree.
    let ls = |test: &str| {
        text(
            &stack
                .run_in(&repo, &["race", "ls", "fix", "--test", test])
                .stdout,
        )
    };
    assert_eq!(ls("test -e raced.txt").matches(" tests=pass ").count(), 2);
    assert_eq!(ls("exit 1").matches(" tests=fail ").count(), 2);
    // Neither the counts nor the test follow a repository the environment names.
    let out = stack
        .command(&repo)
        .env("GIT_DIR", "/nonexistent")
        .env("GIT_COMMON_DIR", "/nonexistent")
        .args([
            "race",
            "ls",
            "fix",
            "--test",
            "git rev-parse --show-toplevel",
        ])
        .output()
        .unwrap();
    let said = text(&out.stdout);
    assert_eq!(said.matches(" files=1 added=2 ").count(), 2, "{said}");
    assert_eq!(said.matches(" tests=pass ").count(), 2, "{said}");
    // A worktree no racer had, its folder gone: a race never prunes it.
    let stray = stack.path("w").join("stray");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &stray.display().to_string(),
        ],
    );
    std::fs::remove_dir_all(&stray).unwrap();

    // A keep that names no racer is refused before anything is looked at.
    let out = stack.run_in(&repo, &["race", "rm", "fix", "--keep", "codex", "--force"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    // The loser's changes are not dropped without --force, and nothing is touched.
    let out = stack.run_in(&repo, &["race", "rm", "fix", "--keep", "claude"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("claude-2: uncommitted changes in"),
        "{}",
        text(&out.stderr)
    );
    let racers = |name: &str| {
        let prefix = format!("race/{name}/");
        stack
            .rows()
            .iter()
            .filter(|r| r["name"].as_str().is_some_and(|s| s.starts_with(&prefix)))
            .count()
    };
    assert_eq!(racers("fix"), 2, "every tab is still open");
    let out = stack.run_in(&repo, &["race", "rm", "fix", "--keep", "claude", "--force"]);
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{said}{}", text(&out.stderr));
    assert!(
        said.starts_with("removed agent=claude-2 worktree="),
        "{said}"
    );
    assert!(said.contains(" branch=race/fix/claude-2"), "{said}");
    assert_eq!(racers("fix"), 1);
    assert!(!has_ref(&repo, "refs/heads/race/fix/claude-2"));
    assert!(
        !stack
            .path("w")
            .join("my repo-wt-race-fix-claude-2")
            .exists()
    );
    assert!(
        has_ref(&repo, "refs/qoop/race/fix"),
        "the winner keeps the race"
    );
    assert!(
        git(&repo, &["worktree", "list", "--porcelain"]).contains("prunable"),
        "a worktree no racer had is not pruned"
    );

    // The winner committed, which only --force drops; with it, the race is gone.
    let winner = stack.path("w").join("my repo-wt-race-fix-claude");
    git(&winner, &["add", "raced.txt"]);
    git(&winner, &["commit", "-q", "-m", "won"]);
    let out = stack.run_in(&repo, &["race", "rm", "fix"]);
    assert!(
        text(&out.stderr).contains("claude: commits past the base (1)"),
        "{}",
        text(&out.stderr)
    );
    let out = stack.run_in(&repo, &["race", "rm", "fix", "--force"]);
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{said}{}", text(&out.stderr));
    assert!(said.ends_with("removed ref=refs/qoop/race/fix\n"), "{said}");
    assert!(!has_ref(&repo, "refs/qoop/race/fix"));
    assert!(!has_ref(&repo, "refs/heads/race/fix/claude"));
    assert_eq!(racers("fix"), 0);
}

#[test]
fn a_racer_still_working_or_with_another_tab_inside_is_never_dropped() {
    let stack = Stack::start();
    let repo = stack.repo("h");
    let racers = |name: &str| {
        let prefix = format!("race/{name}/");
        stack
            .rows()
            .iter()
            .filter(|r| r["name"].as_str().is_some_and(|s| s.starts_with(&prefix)))
            .count()
    };
    let out = stack.run_in(
        &repo,
        &[
            "race",
            "new",
            "w",
            "--agents",
            "claude,claude",
            "--",
            "WORK",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        stack.wait(|| stack.working_in("w", 2)),
        "the racers never started working"
    );
    let out = stack.run_in(&repo, &["race", "rm", "w", "--force"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("is working"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(racers("w"), 2);

    let out = stack.run_in(
        &repo,
        &[
            "race",
            "new",
            "d",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "60",
            "--",
            "DONE",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    // A shell of the person's inside a loser's worktree.
    let inside = stack.path("w").join("h-wt-race-d-claude");
    let out = stack.run_in(
        &repo,
        &[
            "new",
            "--name",
            "mine",
            "--cwd",
            &inside.display().to_string(),
        ],
    );
    let mine = text(&out.stdout).trim().to_string();
    assert!(
        stack.wait(|| stack.rows().iter().any(|r| r["name"] == "mine"
            && r["cwd"]
                .as_str()
                .is_some_and(|c| c.ends_with("h-wt-race-d-claude")))),
        "the shell never showed its folder"
    );
    let out = stack.run_in(&repo, &["race", "rm", "d", "--force"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains(&format!("qoop terminal {mine} is inside")),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(racers("d"), 2, "every tab is still open");

    // With that shell closed and a racer's folder deleted by hand, the race goes whole.
    assert!(stack.run_in(&repo, &["close", &mine]).status.success());
    std::fs::remove_dir_all(stack.path("w").join("h-wt-race-d-claude-2")).unwrap();
    let out = stack.run_in(&repo, &["race", "rm", "d", "--force"]);
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{said}{}", text(&out.stderr));
    assert!(
        said.contains("removed agent=claude-2 worktree=- branch=race/d/claude-2"),
        "{said}"
    );
    assert_eq!(
        racers("d"),
        0,
        "the tab of the folder deleted by hand is closed too"
    );
    assert!(!has_ref(&repo, "refs/heads/race/d/claude-2"));
    assert!(!has_ref(&repo, "refs/qoop/race/d"));

    // A racer's tab back at its shell and running something else is not closed under it.
    let out = stack.run_in(
        &repo,
        &[
            "race",
            "new",
            "e",
            "--agents",
            "claude,claude",
            "--",
            "EXIT",
        ],
    );
    let tab = terminals(&text(&out.stdout)).remove(0);
    assert!(stack.wait(|| stack.agents_in("e", 2)), "never agents");
    std::fs::write(stack.path("exit"), "").unwrap();
    assert!(
        stack.wait(|| stack.racers("e").iter().all(|r| r["agent"] == false)),
        "never back at their shells"
    );
    let sent = stack.run_in(&repo, &["send", &tab, "sleep 120", "--enter"]);
    assert!(sent.status.success(), "{}", text(&sent.stderr));
    assert!(
        stack.wait(|| stack.racers("e").iter().any(|r| r["program"] == "sleep")),
        "the tab never ran sleep"
    );
    let out = stack.run_in(&repo, &["race", "rm", "e", "--force"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains(&format!(
            "terminal {tab} runs sleep, not its agent or a shell"
        )),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(racers("e"), 2, "every tab is still open");
    let _ = stack.run_in(&repo, &["send", &tab, "--key", "ctrl-c"]);
}

#[test]
fn a_race_of_the_same_name_in_another_repository_is_left_alone() {
    let stack = Stack::start();
    let (a, b) = (stack.repo("a"), stack.repo("b"));
    // The other repository's first, so its tabs are listed before this one's.
    for repo in [&b, &a] {
        let out = stack.run_in(
            repo,
            &[
                "race",
                "new",
                "same",
                "--agents",
                "claude,claude",
                "--wait",
                "--timeout",
                "60",
                "--",
                "DONE",
            ],
        );
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }
    let out = stack.run_in(&a, &["race", "rm", "same", "--force"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let left = stack.racers("same");
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.iter().all(|r| r["cwd"]
            .as_str()
            .is_some_and(|c| c.contains("/b-wt-race-same-"))),
        "{left:?}"
    );
    let listed = text(&stack.run_in(&b, &["race", "ls", "same"]).stdout);
    assert_eq!(listed.matches(" status=done ").count(), 2, "{listed}");
}

#[test]
fn a_wait_ends_at_its_timeout_and_when_qoop_stops() {
    let mut stack = Stack::start();
    let repo = stack.repo("t");
    let out = stack.run_in(
        &repo,
        &[
            "race",
            "new",
            "slow",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "3",
            "--",
            "WORK",
        ],
    );
    assert_eq!(out.status.code(), Some(124), "{}", text(&out.stderr));

    let waiting = stack
        .command(&repo)
        .args([
            "race",
            "new",
            "cut",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "60",
            "--",
            "WORK",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("race new");
    assert!(stack.wait(|| stack.working_in("cut", 2)), "never working");
    let mut daemon = stack.child.take().expect("a daemon");
    let _ = daemon.kill();
    let _ = daemon.wait();
    let out = waiting.wait_with_output().expect("race new");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("qoop stopped while waiting"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn a_blocked_racer_points_at_qoop_wait_and_one_that_quit_reads_exited() {
    let stack = Stack::start();
    let repo = stack.repo("r");
    let out = stack.run_in(
        &repo,
        &[
            "race",
            "new",
            "ask",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "60",
            "--",
            "BLOCK",
        ],
    );
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{said}{}", text(&out.stderr));
    for tab in terminals(&said) {
        assert!(
            said.contains(&format!(
                "terminal={tab} end=blocked; `qoop wait {tab} --until done,idle,failed` keeps waiting on it"
            )),
            "{said}"
        );
    }
    assert!(text(&out.stderr).contains("2 of 2 racers did not finish"));

    let waiting = stack
        .command(&repo)
        .args([
            "race",
            "new",
            "gone",
            "--agents",
            "claude,claude",
            "--wait",
            "--timeout",
            "60",
            "--",
            "EXIT",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("race new");
    assert!(
        stack.wait(|| stack.agents_in("gone", 2)),
        "the racers never showed as agents"
    );
    std::thread::sleep(SEEN_BY_WAIT);
    std::fs::write(stack.path("exit"), "").unwrap();
    let out = waiting.wait_with_output().expect("race new");
    let said = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{said}{}", text(&out.stderr));
    assert_eq!(said.matches("end=exited").count(), 2, "{said}");
}

#[test]
fn nothing_is_made_when_a_check_before_the_start_fails() {
    let stack = Stack::start();
    let repo = stack.repo("p");
    let new = |name: &str, more: &[&str]| {
        let mut args = vec!["race", "new", name];
        args.extend_from_slice(more);
        stack.run_in(&repo, &args)
    };
    for (args, why) in [
        (&["--agents", "claude", "--", "x"][..], "2 to 5 agents"),
        (&["--agents", "a,b,c,d,e,f", "--", "x"][..], "2 to 5 agents"),
        (
            &["--agents", "claude,codex", "--base", "-x", "--", "x"][..],
            "dash",
        ),
    ] {
        let out = new("ok", args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains(why), "{}", text(&out.stderr));
    }
    let out = new("Bad", &["--agents", "claude,codex", "--", "x"]);
    assert_eq!(out.status.code(), Some(2));

    // A remote branch of a racer's name: the worktree would check it out and refuse the base.
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/race/r2/codex", "HEAD"],
    );
    let out = new("r2", &["--agents", "claude,codex", "--", "x"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("origin/race/r2/codex exists"),
        "{}",
        text(&out.stderr)
    );
    // A folder in the way, and one git still lists though it is gone.
    let beside = repo.parent().unwrap();
    std::fs::create_dir(beside.join("p-wt-race-r3-codex")).unwrap();
    let out = new("r3", &["--agents", "claude,codex", "--", "x"]);
    assert!(
        text(&out.stderr).contains("p-wt-race-r3-codex exists"),
        "{}",
        text(&out.stderr)
    );
    let listed = beside.join("p-wt-race-r4-claude");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &listed.display().to_string(),
        ],
    );
    std::fs::remove_dir_all(&listed).unwrap();
    let out = new("r4", &["--agents", "claude,codex", "--", "x"]);
    assert!(
        text(&out.stderr).contains("git worktree prune"),
        "{}",
        text(&out.stderr)
    );
    // A branch named `race` blocks every racer branch.
    git(&repo, &["branch", "race"]);
    let out = new("r5", &["--agents", "claude,codex", "--", "x"]);
    assert!(
        text(&out.stderr).contains("the branch race blocks"),
        "{}",
        text(&out.stderr)
    );
    for name in ["r2", "r3", "r4", "r5"] {
        assert!(!has_ref(&repo, &format!("refs/qoop/race/{name}")), "{name}");
        assert!(
            !has_ref(&repo, &format!("refs/heads/race/{name}/claude")),
            "{name}"
        );
    }
    assert!(!beside.join("p-wt-race-r2-claude").exists());

    // A racer branch checked out in the main checkout has no worktree of its own.
    let main = stack.repo("m");
    git(&main, &["checkout", "-q", "-b", "race/m/claude"]);
    let listed = text(&stack.run_in(&main, &["race", "ls", "m"]).stdout);
    assert!(listed.ends_with(" tests=- worktree=-\n"), "{listed}");
    let out = stack.run_in(&main, &["race", "rm", "m", "--force"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(main.join(".git").is_dir(), "the main checkout stays");
}

#[test]
fn a_worker_agents_terminal_cannot_start_a_race_and_the_persons_shell_can() {
    let stack = Stack::start();
    let repo = stack.repo("g");
    // A worker: an agent row, with its own terminal's identity.
    let out = stack.run_in(&repo, &["new", "--name", "worker", "--", "claude", "WORK"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let worker = text(&out.stdout).trim().to_string();
    let token_file = stack.path(&format!("token.{worker}"));
    assert!(
        stack.wait(|| token_file.exists()
            && stack.rows().iter().any(|r| r["terminal"]
                .as_u64()
                .map(|t| t.to_string())
                .as_deref()
                == Some(worker.as_str())
                && r["agent"] == true)),
        "the worker never showed as an agent"
    );
    let token = std::fs::read_to_string(&token_file).unwrap();
    let token = token
        .split_whitespace()
        .nth(1)
        .expect("a token")
        .to_string();
    let out = stack
        .command(&repo)
        .env("QOOP_TERMINAL", &worker)
        .env("QOOP_TOKEN", &token)
        .args([
            "race",
            "new",
            "w",
            "--agents",
            "claude,claude",
            "--",
            "DONE",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("not from an agent's terminal"),
        "{}",
        text(&out.stderr)
    );
    assert!(!has_ref(&repo, "refs/qoop/race/w"));

    // The person's own shell tab is not an agent, and passes.
    let out = stack.run_in(
        &repo,
        &["new", "--name", "me", "--cwd", &repo.display().to_string()],
    );
    let shell = text(&out.stdout).trim().to_string();
    let done = stack.path("shell.done");
    let line = format!(
        "qoop race new mine --agents claude,claude -- DONE > '{0}.out' 2>&1; touch '{0}'",
        done.display()
    );
    let sent = stack.run_in(&repo, &["send", &shell, &line, "--enter"]);
    assert!(sent.status.success(), "{}", text(&sent.stderr));
    assert!(stack.wait(|| done.exists()), "the race never returned");
    let said = std::fs::read_to_string(stack.path("shell.done.out")).unwrap();
    assert!(said.starts_with("race=mine "), "{said}");
    assert!(has_ref(&repo, "refs/qoop/race/mine"));
    // Dropping racers is the same power: a worker cannot.
    let out = stack
        .command(&repo)
        .env("QOOP_TERMINAL", &worker)
        .env("QOOP_TOKEN", &token)
        .args(["race", "rm", "mine", "--force"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("not from an agent's terminal"),
        "{}",
        text(&out.stderr)
    );
    assert!(has_ref(&repo, "refs/heads/race/mine/claude"));
}

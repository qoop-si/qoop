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

    /// Whether every row named `race/<name>/...` is an agent, and there are `n` of them.
    fn agents_in(&self, name: &str, n: usize) -> bool {
        let prefix = format!("race/{name}/");
        let racers: Vec<_> = self
            .rows()
            .into_iter()
            .filter(|r| r["name"].as_str().is_some_and(|s| s.starts_with(&prefix)))
            .collect();
        racers.len() == n && racers.iter().all(|r| r["agent"] == true)
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
fn a_race_starts_each_agent_in_its_worktree_and_waits_until_all_finished() {
    let stack = Stack::start();
    let repo = stack.repo("my repo");
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let out = stack.run_in(
        &repo,
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
    // The wait lists once a second: an agent it never saw as one reads as not started.
    std::thread::sleep(Duration::from_secs(3));
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
}

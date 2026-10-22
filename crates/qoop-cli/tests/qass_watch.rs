//! `qoop qass watch [on|off]`: the owner's switch for qass's watcher, turned through the real
//! daemon and, with none running, on the file alone.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// A runtime directory the test owns, holding the daemon's socket and qass's data folder.
/// Short on purpose: a socket path has about 104 bytes.
struct Runtime(PathBuf);

impl Runtime {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("qw-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a runtime directory");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        Self(path)
    }

    fn qass(&self) -> PathBuf {
        self.0.join("data/qoop/qass")
    }

    fn command(&self) -> std::process::Command {
        let mut command = qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop"));
        command
            .env("QOOP_RUNTIME_DIR", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("SHELL", "/bin/sh");
        command
    }

    /// `qoop qass watch [word]`: what it printed, or why it failed.
    fn watch(&self, word: Option<&str>) -> String {
        let out = self
            .command()
            .args(["qass", "watch"])
            .args(word)
            .output()
            .expect("run qoop");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn switch(&self) -> Option<String> {
        std::fs::read_to_string(self.qass().join("watch")).ok()
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

fn start(runtime: &Runtime) -> Daemon {
    let child = runtime
        .command()
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let daemon = Daemon(child);
    let socket = qoop_testkit::socket_in(&runtime.0);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn mark_seen(qass: &Path, switch: &str) {
    let stamp = switch
        .trim()
        .strip_prefix("resume ")
        .expect("a resume stamp");
    std::fs::write(qass.join("watch-seen"), stamp).expect("write");
}

/// No file is on; off is written; on again is `resumed` until a watcher starts on its stamp,
/// and an on then changes nothing. With no qass terminal nothing is typed and nothing fails.
#[test]
fn the_switch_turns_through_the_daemon() {
    let runtime = Runtime::new("d");
    let _daemon = start(&runtime);
    assert_eq!(runtime.watch(None), "watching on");
    assert_eq!(runtime.switch(), None);
    assert_eq!(runtime.watch(Some("off")), "watching off");
    assert_eq!(runtime.switch().as_deref(), Some("off\n"));
    assert_eq!(runtime.watch(None), "watching off");
    assert_eq!(runtime.watch(Some("on")), "watching resumed");
    let switch = runtime.switch().expect("the switch");
    assert!(switch.starts_with("resume "), "{switch}");
    assert_eq!(runtime.watch(Some("on")), "watching resumed");
    assert_eq!(runtime.switch().as_deref(), Some(switch.as_str()));
    mark_seen(&runtime.qass(), &switch);
    assert_eq!(runtime.watch(None), "watching on");
}

/// With no daemon there is no qass to tell: the file is turned all the same.
#[test]
fn the_switch_turns_with_no_daemon() {
    let runtime = Runtime::new("n");
    assert_eq!(runtime.watch(Some("off")), "watching off");
    assert_eq!(runtime.switch().as_deref(), Some("off\n"));
    assert_eq!(runtime.watch(Some("on")), "watching resumed");
}

#[test]
fn a_word_other_than_on_or_off_is_refused() {
    let runtime = Runtime::new("u");
    let out = runtime
        .command()
        .args(["qass", "watch", "maybe"])
        .output()
        .expect("run qoop");
    assert!(!out.status.success());
    assert!(runtime.switch().is_none());
}

impl Runtime {
    /// `qoop <args>` with `env` set: whether it succeeded, and what it printed or said.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
        let mut command = self.command();
        command.args(args);
        for var in ["QOOP_TERMINAL", "QOOP_TOKEN", "QOOP_FROM_QASS"] {
            command.env_remove(var);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().expect("run qoop");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    /// A new shell in `space`: its number.
    fn open(&self, space: &str) -> String {
        let (ok, out, err) = self.run(&["new", "--space", space], &[]);
        assert!(ok, "new failed: {err}");
        out
    }
}

/// `qoop qass watch off|on <T52|space>` skips and unskips a tab or a space, `qoop qass watch`
/// lists what is skipped, and the row says so: `qoop agents --json` is what the watcher reads.
#[test]
fn a_tab_and_a_space_are_skipped_and_watched_again_from_the_command_line() {
    let runtime = Runtime::new("s");
    let _daemon = start(&runtime);
    let (tab, other) = (runtime.open("work"), runtime.open("work"));
    let in_space = runtime.open("kept");
    let skipped = |terminal: &str| -> bool {
        let (_, out, _) = runtime.run(&["agents", "--json"], &[]);
        let rows: serde_json::Value = serde_json::from_str(&out).expect("json");
        rows.as_array()
            .and_then(|r| {
                r.iter().find(|r| {
                    r["terminal"].as_u64().map(|t| t.to_string()).as_deref() == Some(terminal)
                })
            })
            .is_some_and(|r| r["qass_skip"] == true)
    };
    assert_eq!(runtime.watch(None), "watching on");

    let (ok, out, err) = runtime.run(&["qass", "watch", "off", &format!("T{tab}")], &[]);
    assert!(ok, "{err}");
    assert_eq!(out, format!("qass leaves T{tab} alone"));
    assert!(skipped(&tab) && !skipped(&other));
    let (_, listed, _) = runtime.run(&["qass", "watch"], &[]);
    assert!(
        listed.starts_with("watching on\nskipping T") && listed.contains(&format!("T{tab}")),
        "{listed}"
    );

    // A space is skipped by name, and every tab in it with it; a name that is no space is refused.
    assert!(runtime.run(&["qass", "watch", "off", "kept"], &[]).0);
    assert!(skipped(&in_space));
    assert!(
        runtime
            .run(&["qass", "watch"], &[])
            .1
            .contains("skipping space kept")
    );
    assert!(
        !runtime
            .run(&["qass", "watch", "off", "nothing-here"], &[])
            .0
    );

    let (ok, out, _) = runtime.run(&["qass", "watch", "on", &format!("T{tab}")], &[]);
    assert!(ok);
    assert_eq!(out, format!("qass watches T{tab} again"));
    assert!(!skipped(&tab));
    assert!(runtime.run(&["qass", "watch", "on", "kept"], &[]).0);
    assert!(!skipped(&in_space));
}

/// Commands aimed at a skipped tab are refused when they are qass's (`QOOP_FROM_QASS=1`, which
/// on.sh sets), allowed for the owner's own shell and for any other terminal's, and allowed
/// again once the tab is watched; a tab beside it is never refused. `task board --dir`
/// leaves a skipped space out for qass only.
#[test]
fn qass_is_refused_a_skipped_tab_and_the_owner_is_not() {
    let runtime = Runtime::new("g");
    let _daemon = start(&runtime);
    let (tab, beside) = (runtime.open("work"), runtime.open("work"));
    let in_kept = runtime.open("kept");
    assert!(
        runtime
            .run(&["qass", "watch", "off", &format!("T{tab}")], &[])
            .0
    );
    assert!(runtime.run(&["qass", "watch", "off", "kept"], &[]).0);

    let qass = [("QOOP_FROM_QASS", "1")];
    for args in [
        vec!["read", &tab],
        vec!["send", &tab, "--enter", "echo hi"],
        vec!["prompt", &tab, "--", "hi"],
        vec!["history", &tab],
        vec!["context", &tab],
        vec!["messages", &tab],
        vec!["agents", "explain", &tab],
        vec!["close", &tab],
        vec!["read", &in_kept],
    ] {
        let (ok, _, err) = runtime.run(&args, &qass);
        assert!(
            !ok && err.contains("leave alone"),
            "{args:?} was not refused: {err}"
        );
    }
    // qass cannot take the skip back to get in, nor skip a tab itself.
    let (ok, _, err) = runtime.run(&["qass", "watch", "on", &format!("T{tab}")], &qass);
    assert!(!ok && err.contains("only the owner"), "{err}");
    let (ok, _, err) = runtime.run(&["qass", "watch", "off", &format!("T{beside}")], &qass);
    assert!(!ok && err.contains("only the owner"), "{err}");
    // The tab beside it is read, and the skipped one by the owner, by another terminal's number
    // that is not qass's, and with the mark gone.
    assert!(runtime.run(&["read", &beside], &qass).0);
    assert!(runtime.run(&["read", &tab], &[]).0, "the owner's shell");
    assert!(
        runtime
            .run(&["read", &tab], &[("QOOP_TERMINAL", &beside)])
            .0,
        "a tab that is not qass"
    );
    assert!(
        runtime
            .run(&["qass", "watch", "on", &format!("T{tab}")], &[])
            .0
    );
    assert!(runtime.run(&["read", &tab], &qass).0);

    // The board qass reads leaves a skipped space out; the owner's own board keeps it.
    let board = |env: &[(&str, &str)]| {
        let dir = runtime.0.join(format!("board-{}", env.len()));
        let (ok, _, err) = runtime.run(&["task", "board", "--dir", dir.to_str().unwrap()], env);
        assert!(ok, "{err}");
        std::fs::read_to_string(dir.join("README.md")).expect("README")
    };
    let (mine, theirs) = (board(&[]), board(&qass));
    assert!(mine.contains("kept") && mine.contains("work"), "{mine}");
    assert!(
        !theirs.contains("kept") && theirs.contains("work"),
        "{theirs}"
    );
}

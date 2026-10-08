//! A machine that ran qoro meets this build: its directories move to the new name, its
//! daemon is still reached, and what still looks for the old paths still finds the new daemon.
//!
//! Everything lives in one temp dir (home, runtime, data and configuration), so nothing
//! reaches the person's own files or daemon.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ql")
            .tempdir()
            .expect("tempdir");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        Self { root }
    }

    fn path(&self, rest: &str) -> PathBuf {
        self.root.path().join(rest)
    }

    /// The session directory a 0.7.x daemon used, under the runtime base named `qoro`.
    fn old_session(&self) -> PathBuf {
        self.path("rt/qoro/sessions/default")
    }

    fn new_session(&self) -> PathBuf {
        self.path("rt/qoop/sessions/default")
    }

    /// `c` with this machine's environment and nothing of the person's.
    fn isolated(&self, mut c: Command) -> Command {
        c.env(qoop_runtime::legacy::SANDBOX_ENV, "1")
            .env("HOME", self.path("home"))
            .env("XDG_RUNTIME_DIR", self.path("rt"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("SHELL", "/bin/sh")
            .env_remove("QOOP_RUNTIME_DIR")
            .env_remove("QORO_RUNTIME_DIR")
            .env_remove("QOOP_SESSION")
            .env_remove("QORO_SESSION")
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn qoop(&self) -> Command {
        self.isolated(qoop_testkit::sandboxed(env!("CARGO_BIN_EXE_qoop")))
    }

    /// What 0.7.x left on disk: its three directories, each with something in it.
    fn old_files(&self) {
        for (dir, file) in [
            ("config/qoro", "config.toml"),
            ("data/qoro", "machine.key"),
            ("data/qoro/qass", "goal"),
        ] {
            std::fs::create_dir_all(self.path(dir)).expect("old dir");
            std::fs::write(self.path(dir).join(file), file).expect("old file");
        }
    }

    fn old_runtime_dir(&self) {
        for dir in [
            self.path("rt/qoro"),
            self.path("rt/qoro/sessions"),
            self.old_session(),
        ] {
            std::fs::create_dir_all(&dir).expect("runtime dir");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
    }
}

fn is_link(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// An old daemon's socket that counts who connects. Not a daemon: only what a client sees
/// of one before it speaks.
fn listen(socket: &Path) -> Arc<AtomicUsize> {
    let seen = Arc::new(AtomicUsize::new(0));
    let listener = UnixListener::bind(socket).expect("bind the old socket");
    let count = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            count.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    seen
}

#[test]
fn the_directories_of_a_qoro_install_move_and_its_daemon_is_reached() {
    let m = Machine::new();
    m.old_files();
    m.old_runtime_dir();
    let seen = listen(&m.old_session().join("qorod.sock"));

    // Any command: the move happens before it reads a path, and `ls` asks the daemon.
    let out = m.qoop().args(["ls", "--json"]).output().expect("qoop ls");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    for (new, old) in [
        ("config/qoop/config.toml", "config/qoro/config.toml"),
        ("data/qoop/machine.key", "data/qoro/machine.key"),
        ("data/qoop/qass/goal", "data/qoro/qass/goal"),
    ] {
        assert!(m.path(new).exists(), "{new} did not move");
        // A program that still spells the old path finds its files.
        assert!(m.path(old).exists(), "{old} no longer reaches it");
    }
    for dir in ["config/qoro", "data/qoro"] {
        assert!(is_link(&m.path(dir)), "{dir} is not left as a link");
    }
    // The new socket path is a way to the old daemon, which this build's client used.
    assert!(is_link(&m.new_session().join("qoopd.sock")));
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "the old daemon was never reached"
    );
}

#[test]
fn nothing_is_made_on_a_machine_that_never_ran_qoro() {
    let m = Machine::new();
    let out = m.qoop().args(["ls", "--json"]).output().expect("qoop ls");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for old in ["config/qoro", "data/qoro", "rt/qoro"] {
        assert!(!m.path(old).exists(), "{old} was made");
    }
}

/// Killed if the test fails before it stops the daemon itself.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_old_socket_path_reaches_a_daemon_started_after_the_move() {
    let m = Machine::new();
    m.old_runtime_dir();
    // What the daemon that was there left when it moved: a socket nobody listens on.
    drop(UnixListener::bind(m.old_session().join("qorod.sock")).expect("old socket"));

    let _daemon = Daemon(m.qoop().arg("daemon").spawn().expect("spawn"));
    let old = m.old_session().join("qorod.sock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !is_link(&old) || UnixStream::connect(&old).is_err() {
        assert!(
            Instant::now() < deadline,
            "the old socket path never reached the new daemon"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(m.new_session().join("qoopd.sock").exists());
    let stopped = m.qoop().arg("stop").status().expect("stop");
    assert!(stopped.success());
}

/// The hook and the skill of a qoro install are replaced, not doubled: an agent that is
/// already running keeps the old script it loaded, so that file stays on disk.
#[test]
fn hooks_and_skills_of_a_qoro_install_are_replaced_not_doubled() {
    let m = Machine::new();
    m.old_files();
    let old_script = m.path("config/qoro/hooks/qoro-hook.sh");
    std::fs::create_dir_all(old_script.parent().unwrap()).expect("hooks dir");
    std::fs::write(&old_script, "#!/bin/sh\n").expect("old script");
    std::fs::create_dir_all(m.path("home/.claude/skills/qoro")).expect("old skill");
    std::fs::write(m.path("home/.claude/skills/qoro/SKILL.md"), "old").expect("old skill");
    let entry = serde_json::json!({
        "matcher": "",
        "hooks": [{"type": "command", "command": old_script.display().to_string()}],
    });
    std::fs::write(
        m.path("home/.claude/settings.json"),
        serde_json::json!({"hooks": {"Stop": [entry]}}).to_string(),
    )
    .expect("settings");

    for what in ["hooks", "skills"] {
        let out = m
            .qoop()
            .args([what, "install", "claude"])
            .output()
            .expect(what);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let settings = std::fs::read_to_string(m.path("home/.claude/settings.json")).unwrap();
    assert!(
        settings.contains("config/qoop/hooks/qoop-hook.sh"),
        "{settings}"
    );
    assert!(
        !settings.contains("qoro-hook.sh"),
        "the old entry is still there: {settings}"
    );
    assert!(
        old_script.exists(),
        "a running agent's script was taken away"
    );
    assert!(m.path("home/.claude/skills/qoop/SKILL.md").exists());
    assert!(
        !m.path("home/.claude/skills/qoro").exists(),
        "the agent is told twice"
    );

    // Uninstalling takes the old script away with the new one.
    let out = m
        .qoop()
        .args(["hooks", "uninstall", "claude"])
        .output()
        .expect("uninstall");
    assert!(out.status.success());
    assert!(!old_script.exists());
}

/// A 0.7.x daemon asks the file it will move onto, by the path it was started from, for
/// `qoro <version>`; a build run by its own name says `qoop`.
#[test]
fn the_version_line_follows_the_name_the_binary_is_run_by() {
    let m = Machine::new();
    let bin = m.path("bin");
    std::fs::create_dir_all(&bin).expect("bin");
    for (name, want) in [
        ("qoro", "qoro"),
        (".qoro-install.Ab12Cd", "qoro"),
        ("qoro.pre-0.7.0", "qoro"),
        ("qoop", "qoop"),
    ] {
        let copy = bin.join(format!("run-as-{name}")).join(name);
        std::fs::create_dir_all(copy.parent().unwrap()).expect("dir");
        std::fs::copy(env!("CARGO_BIN_EXE_qoop"), &copy).expect("copy");
        // Run from the machine's own environment: any command moves the home's old
        // directories first, and that must never be the person's.
        let mut c = Command::new(&copy);
        c.arg("--version");
        let out = m.isolated(c).output().expect(name);
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            said,
            format!("{want} {}", env!("CARGO_PKG_VERSION")),
            "run as {name}"
        );
    }
}

/// The root fix for the incidents of 2026-10-08: a `qoop` built under a `target` directory
/// that was not started through the sandbox helper (a test that forgot it, a build run by
/// hand) moves nothing of the home it finds, and the command itself still runs.
#[test]
fn a_build_under_target_without_the_mark_leaves_the_home_alone() {
    let m = Machine::new();
    m.old_files();
    m.old_runtime_dir();
    let listener = listen(&m.old_session().join("qorod.sock"));
    // Everything but the mark, and not from a cargo test either: a plain shell's environment.
    let mut c = Command::new(env!("CARGO_BIN_EXE_qoop"));
    c.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", m.path("home"))
        .env("XDG_CONFIG_HOME", m.path("config"))
        .env("XDG_DATA_HOME", m.path("data"))
        .env("XDG_RUNTIME_DIR", m.path("rt"))
        .args(["ls", "--json"]);
    let out = c.output().expect("qoop");
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not move the qoro directories"));
    for old in ["config/qoro", "data/qoro"] {
        let meta = m.path(old).symlink_metadata().expect(old);
        assert!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "{old} was moved"
        );
    }
    assert!(!m.path("config/qoop").exists() && !m.path("data/qoop").exists());
    // No link into the old runtime directory either.
    assert!(
        m.new_session()
            .join("qoopd.sock")
            .symlink_metadata()
            .is_err()
    );
    drop(listener);
}

/// Codex trusts a hook by its path: replacing the old entry says the new one is not trusted yet.
#[test]
fn replacing_a_codex_hook_says_it_must_be_trusted_again() {
    let m = Machine::new();
    m.old_files();
    let old_script = m.path("config/qoro/hooks/qoro-codex-hook.sh");
    std::fs::create_dir_all(old_script.parent().unwrap()).expect("hooks dir");
    std::fs::write(&old_script, "#!/bin/sh\n").expect("old script");
    std::fs::create_dir_all(m.path("home/.codex")).expect("codex");
    let entry = serde_json::json!({"hooks": [{"type": "command", "command": old_script.display().to_string()}]});
    std::fs::write(
        m.path("home/.codex/hooks.json"),
        serde_json::json!({"hooks": {"Stop": [entry]}}).to_string(),
    )
    .expect("hooks.json");
    let out = m
        .qoop()
        .args(["hooks", "install", "codex"])
        .output()
        .expect("install");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("has not trusted yet"), "{said}");
    // Run again: nothing was replaced this time, so that sentence is not repeated.
    let out = m
        .qoop()
        .args(["hooks", "install", "codex"])
        .output()
        .expect("install");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("has not trusted yet"));
}

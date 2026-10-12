//! `qoop uninstall` against a home of its own: HOME, every XDG directory and the runtime
//! directory sit inside one temp dir, and a copy of the binary is the `qoop` on PATH, so the
//! command removes only what the test put there. This build has no phone part.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("qun")
            .tempdir()
            .expect("tempdir");
        for dir in ["h/.local/bin", "h/.claude/skills/neighbour", "rt"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("dir");
        }
        for dir in [root.path().to_path_buf(), root.path().join("rt")] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
        std::fs::copy(
            env!("CARGO_BIN_EXE_qoop"),
            root.path().join("h/.local/bin/qoop"),
        )
        .expect("copy the binary");
        std::fs::write(
            root.path().join("h/.claude/skills/neighbour/SKILL.md"),
            "mine",
        )
        .expect("neighbour");
        Self { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn command(&self, path_entry: &str) -> Command {
        let mut c = qoop_testkit::sandboxed(self.path("h/.local/bin/qoop"));
        c.env("HOME", self.path("h"))
            .env("XDG_CONFIG_HOME", self.path("h/.config"))
            .env("XDG_DATA_HOME", self.path("h/.local/share"))
            .env("XDG_CACHE_HOME", self.path("h/.cache"))
            .env("XDG_STATE_HOME", self.path("h/.local/state"))
            .env("QOOP_RUNTIME_DIR", self.path("rt"))
            .env("PATH", format!("{path_entry}:/usr/bin:/bin"))
            .stdin(Stdio::null());
        c
    }

    fn qoop(&self, args: &[&str]) -> Output {
        self.command(self.path("h/.local/bin").to_str().expect("utf8"))
            .args(args)
            .output()
            .expect("run qoop")
    }

    /// The directories `qoop uninstall` takes, as `qoop paths` names them, each with a file.
    fn fill(&self) -> Vec<PathBuf> {
        self.qoop(&["hooks", "install"]);
        self.qoop(&["skills", "install"]);
        let mut dirs = Vec::new();
        for place in ["config", "data", "cache", "logs"] {
            let out = self.qoop(&["paths", place]);
            let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            std::fs::create_dir_all(&dir).expect("place");
            std::fs::write(dir.join("kept"), "x").expect("file");
            dirs.push(dir);
        }
        dirs.push(self.path("h/.local/share/qoop"));
        std::fs::create_dir_all(&dirs[4]).expect("data dir");
        std::fs::write(dirs[4].join("kept"), "x").expect("file");
        std::fs::write(self.path("h/.local/bin/qoro"), "old").expect("qoro");
        dirs
    }

    fn hooks_installed(&self) -> bool {
        std::fs::read_to_string(self.path("h/.claude/settings.json"))
            .is_ok_and(|s| s.contains("qoop-hook"))
    }
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn untouched(m: &Machine, dirs: &[PathBuf]) {
    assert!(dirs.iter().all(|d| d.join("kept").exists()));
    assert!(m.path("h/.local/bin/qoop").exists());
    assert!(m.hooks_installed());
}

#[test]
fn uninstall_takes_everything_and_leaves_the_neighbours() {
    let m = Machine::new();
    let dirs = m.fill();
    assert!(m.hooks_installed());
    let out = m.qoop(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", said(&out));
    for d in &dirs {
        assert!(!d.exists(), "{} is still there", d.display());
    }
    assert!(!m.path("h/.local/bin/qoop").exists());
    assert!(!m.path("h/.local/bin/qoro").exists());
    assert!(!m.hooks_installed());
    assert!(!m.path("h/.claude/skills/qoop").exists());
    assert!(m.path("h/.claude/skills/neighbour/SKILL.md").exists());
}

#[test]
fn without_yes_and_without_a_terminal_it_asks_nothing_and_removes_nothing() {
    let m = Machine::new();
    let dirs = m.fill();
    let out = m.qoop(&["uninstall"]);
    assert!(!out.status.success());
    assert!(said(&out).contains("--yes"), "{}", said(&out));
    untouched(&m, &dirs);
}

#[test]
fn inside_a_qoop_tab_it_refuses() {
    let m = Machine::new();
    let dirs = m.fill();
    let out = m
        .command(m.path("h/.local/bin").to_str().expect("utf8"))
        .args(["uninstall", "--yes"])
        .env("QOOP_TERMINAL", "3")
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(
        said(&out).contains("not inside a qoop tab"),
        "{}",
        said(&out)
    );
    untouched(&m, &dirs);
}

#[test]
fn a_binary_that_is_not_the_qoop_on_path_is_refused() {
    let m = Machine::new();
    let dirs = m.fill();
    let out = m
        .command("/nonexistent")
        .args(["uninstall", "--yes"])
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(
        said(&out).contains("not the qoop on your PATH"),
        "{}",
        said(&out)
    );
    untouched(&m, &dirs);
}

#[test]
fn a_linked_machine_without_the_phone_part_is_refused() {
    let m = Machine::new();
    let dirs = m.fill();
    std::fs::write(dirs[4].join("machine.key"), "k").expect("key");
    let out = m.qoop(&["uninstall", "--yes"]);
    assert!(!out.status.success());
    assert!(said(&out).contains("cannot log out"), "{}", said(&out));
    assert!(dirs[4].join("machine.key").exists());
    untouched(&m, &dirs);
}

#[test]
fn a_target_the_guard_refuses_stops_everything_before_it_starts() {
    let m = Machine::new();
    let dirs = m.fill();
    let out = m
        .command(m.path("h/.local/bin").to_str().expect("utf8"))
        .args(["uninstall", "--yes"])
        .env("XDG_DATA_HOME", "/")
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(said(&out).contains("refusing to remove"), "{}", said(&out));
    untouched(&m, &dirs);
}

#[test]
fn a_left_bridge_service_means_a_paired_phone_and_stops_before_the_directories() {
    let m = Machine::new();
    let dirs = m.fill();
    let mac = m.path("h/Library/LaunchAgents/si.qoop.bridge.plist");
    let linux = m.path("h/.config/systemd/user/qoop-bridge.service");
    for f in [&mac, &linux] {
        std::fs::create_dir_all(f.parent().expect("parent")).expect("dir");
        std::fs::write(f, "service").expect("service file");
    }
    let out = m.qoop(&["uninstall", "--yes"]);
    assert!(!out.status.success());
    assert!(said(&out).contains("qoop devices rm"), "{}", said(&out));
    assert!(dirs.iter().all(|d| d.exists()));
    assert!(m.path("h/.local/bin/qoop").exists());
}

#[test]
fn a_failing_step_stops_there_and_a_second_run_finishes() {
    let m = Machine::new();
    let dirs = m.fill();
    // The cache directory cannot be unlinked while its parent is read-only.
    let cache_parent: &Path = dirs[2].parent().expect("parent");
    std::fs::set_permissions(cache_parent, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let out = m.qoop(&["uninstall", "--yes"]);
    assert!(!out.status.success(), "{}", said(&out));
    assert!(said(&out).contains("stopped at"), "{}", said(&out));
    // Nothing after the failed step went: the logs and the binary are still there.
    assert!(dirs[3].exists());
    assert!(m.path("h/.local/bin/qoop").exists());
    std::fs::set_permissions(cache_parent, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let out = m.qoop(&["uninstall", "--yes"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(dirs.iter().all(|d| !d.exists()));
    assert!(!m.path("h/.local/bin/qoop").exists());
}

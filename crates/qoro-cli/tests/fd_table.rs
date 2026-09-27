//! Gate G1.11: the fd table, read from the OS rather than from our own bookkeeping.
//!
//! This lives with the binary rather than with the library because the assertion is about
//! one process holding one master. In the library's test binary every daemon in the suite
//! shares a process, and the first version of this gate counted seven masters for one
//! terminal and was right to.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

fn wait_for<F: Fn() -> bool>(within: Duration, f: F) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn the_daemon_holds_exactly_one_master_and_the_child_one_tty() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");

    let mut daemon = Reaped(
        Command::new(env!("CARGO_BIN_EXE_qoro"))
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the daemon"),
    );

    let socket = qoro_testkit::socket_in(dir.path());
    assert!(
        wait_for(Duration::from_secs(10), || socket.exists()),
        "the daemon never bound its socket"
    );
    // The terminal is spawned with the daemon, so the master exists by now, but give the
    // shell a moment to be exec'd before asking the OS about its tty.
    std::thread::sleep(Duration::from_millis(500));

    let pid = daemon.0.id();
    let (masters, listing) = pty_masters(pid);
    assert_eq!(
        masters, 1,
        "the daemon holds {masters} pty masters for one terminal:\n{listing}"
    );

    // The child's side. The shell is the daemon's only child, and it must be a session
    // leader on exactly one pty.
    let children = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .expect("pgrep");
    let child: i32 = String::from_utf8_lossy(&children.stdout)
        .split_whitespace()
        .next()
        .expect("the daemon has a child")
        .parse()
        .expect("a numeric pid");

    let ps = Command::new("ps")
        .args(["-o", "tty=,sess=,pid=", "-p", &child.to_string()])
        .output()
        .expect("ps");
    let line = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert!(
        fields[0].starts_with("ttys") || fields[0].starts_with("pts"),
        "the child's controlling tty is {:?}, which is not a pty: {line}",
        fields[0]
    );

    // The child must not have inherited the master. That is the containment property
    // `O_CLOEXEC` at `posix_openpt` exists for, and nothing else here would notice.
    let (child_masters, child_listing) = pty_masters(child as u32);
    assert_eq!(
        child_masters, 0,
        "the child inherited the pty master:\n{child_listing}"
    );

    let _ = daemon.0.kill();
    let _ = daemon.0.wait();
}

/// Kills what it holds however the test ends.
///
/// Without it a failing assertion leaves the daemon running in a temporary directory that
/// is deleted a moment later, so it holds a shell forever and nothing can reach it to stop
/// it. The suite is run on a laptop.
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// How many pty masters a process holds, read from the operating system.
///
/// Two implementations because the question is the same and the answer is not. `lsof` is
/// not installed on every runner, and `/proc` does not exist on macOS.
#[cfg(target_os = "macos")]
fn pty_masters(pid: u32) -> (usize, String) {
    let out = Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output()
        .expect("lsof");
    let listing = String::from_utf8_lossy(&out.stdout).to_string();
    let count = listing.lines().filter(|l| l.contains("/dev/ptmx")).count();
    (count, listing)
}

#[cfg(not(target_os = "macos"))]
fn pty_masters(pid: u32) -> (usize, String) {
    let dir = format!("/proc/{pid}/fd");
    let mut listing = String::new();
    let mut count = 0;
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {dir}: {e}"));
    for entry in entries.flatten() {
        let target = std::fs::read_link(entry.path()).unwrap_or_default();
        let shown = target.display().to_string();
        listing.push_str(&format!("{} -> {}\n", entry.file_name().display(), shown));
        if shown == "/dev/ptmx" || shown.starts_with("/dev/pts/ptmx") {
            count += 1;
        }
    }
    (count, listing)
}

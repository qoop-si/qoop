//! A daemon with nothing awake and nobody attached leaves, so it costs nothing.
//!
//! Driven with the real binary, because leaving is the process exiting: a daemon on its own
//! goes; one a watcher is following still goes; one an interface holds stays until it lets
//! go; and a watcher outlives the daemon it followed and picks up the next one.

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Longer than the daemon's ten seconds of quiet, with room for a slow machine.
const LEAVES_WITHIN: Duration = Duration::from_secs(25);

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    dir
}

fn qoro(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_qoro"));
    command
        .env("QORO_RUNTIME_DIR", dir)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("SHELL", "/bin/sh")
        .env_remove("QORO_SESSION")
        .env_remove("QORO_TERMINAL")
        .env_remove("QORO_TOKEN");
    command
}

fn spawn_daemon(dir: &Path) -> Running {
    let child = qoro(dir)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the daemon");
    let socket = qoro_testkit::socket_in(dir);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && std::os::unix::net::UnixStream::connect(&socket).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the daemon never listened"
    );
    Running(child)
}

/// Whether the daemon's process ended within `within`.
fn ends_within(daemon: &mut Running, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = daemon.0.try_wait() {
            assert!(status.success(), "the daemon left with {status}");
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn a_daemon_with_nothing_awake_and_nobody_attached_leaves() {
    let dir = private_dir();
    let mut daemon = spawn_daemon(dir.path());
    assert!(
        ends_within(&mut daemon, LEAVES_WITHIN),
        "an empty daemon stayed up"
    );
    assert!(
        !qoro_testkit::socket_in(dir.path()).exists(),
        "the daemon left its socket behind"
    );
}

#[test]
fn a_watcher_does_not_keep_it_up_and_an_interface_does() {
    let dir = private_dir();
    let socket = qoro_testkit::socket_in(dir.path());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    // Watched only: it goes anyway.
    let mut daemon = spawn_daemon(dir.path());
    let watcher = rt
        .block_on(qoro_runtime::client::connect_observer(
            &socket,
            "watch-test",
        ))
        .expect("an observer connects");
    assert!(
        ends_within(&mut daemon, LEAVES_WITHIN),
        "a watcher kept an empty daemon up"
    );
    drop(watcher);

    // Held by a client that did not say it only watches: it stays, then goes once let go.
    let mut daemon = spawn_daemon(dir.path());
    let holder = rt
        .block_on(qoro_runtime::client::connect(&socket, "hold-test"))
        .expect("a client connects");
    assert!(
        !ends_within(&mut daemon, Duration::from_secs(14)),
        "the daemon left while somebody was attached"
    );
    drop(holder);
    assert!(
        ends_within(&mut daemon, LEAVES_WITHIN),
        "the daemon stayed after the last client went"
    );
}

/// The notch's watcher: when the daemon leaves it says there is nothing, stays, and follows
/// the next daemon when one starts, without being started again.
#[cfg(target_os = "macos")]
#[test]
fn a_watch_outlives_the_daemon_and_follows_the_next_one() {
    let dir = private_dir();
    let socket = qoro_testkit::socket_in(dir.path());
    let mut daemon = spawn_daemon(dir.path());
    let mut watch = Running(
        qoro(dir.path())
            .args(["agents", "--watch", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the watch"),
    );
    let (lines_tx, lines) = std::sync::mpsc::channel::<String>();
    let out = watch.0.stdout.take().expect("stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                return;
            }
        }
    });
    assert!(
        ends_within(&mut daemon, LEAVES_WITHIN),
        "the watch kept the daemon up"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        matches!(watch.0.try_wait(), Ok(None)),
        "the watch ended with the daemon"
    );

    // A new daemon with a tab in it: the watch lists it without being asked again.
    let _next = spawn_daemon(dir.path());
    qoro_testkit::open_first_terminal(&socket);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Ok(line) = lines.recv_timeout(Duration::from_millis(200)) {
            if line.trim() != "[]" && line.contains("\"terminal\"") {
                return;
            }
            seen.push(line);
        }
    }
    panic!("the watch did not follow the next daemon: {seen:?}");
}

/// The hangup a closing window sends the job that started the daemon does not reach it:
/// the daemon and every tab in it outlive the terminal `qoro` was typed in.
#[test]
fn a_hangup_to_whoever_started_the_daemon_leaves_it_up() {
    use std::os::unix::process::CommandExt;
    let dir = private_dir();
    // Started by `qoro wake` in a process group of its own, as a shell runs a job.
    let mut starter = qoro(dir.path())
        .arg("wake")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("qoro wake");
    let job = i32::try_from(starter.id()).expect("pid");
    let _ = starter.wait();
    let opened = qoro(dir.path())
        .args(["new", "--", "sleep", "60"])
        .output()
        .expect("qoro new");
    assert!(opened.status.success(), "{opened:?}");

    // SAFETY: a signal to the group this test just made.
    unsafe { libc::kill(-job, libc::SIGHUP) };
    std::thread::sleep(Duration::from_millis(500));
    let listed = qoro(dir.path()).arg("ls").output().expect("qoro ls");
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.contains("awake"),
        "the daemon went with its starter's job: {listed}"
    );
    let _ = qoro(dir.path()).arg("stop").output();
}

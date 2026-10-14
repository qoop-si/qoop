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

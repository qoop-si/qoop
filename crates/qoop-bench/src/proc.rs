//! Process-level helpers: resolving the real `qoop` binary, spawning a real daemon, and
//! reading resident memory from the OS rather than from our own bookkeeping.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Resolve the real `qoop` binary.
///
/// `qoop-bench` is a separate crate from `qoop-cli`, so `env!("CARGO_BIN_EXE_qoop")` is not
/// available here: that macro only resolves inside the crate that owns the binary (or an
/// integration test of that same crate). `QOOP_BIN` overrides this for a release build or a
/// non-default target directory; otherwise this assumes `target/debug/qoop` under the
/// workspace root, which is what `cargo build` produces alongside this crate by default.
pub fn bin_path() -> PathBuf {
    if let Ok(p) = std::env::var("QOOP_BIN") {
        return PathBuf::from(p);
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("qoop-bench's manifest sits two directories below the workspace root")
        .join("target/debug/qoop")
}

/// Resident set size in KiB, read from the OS's own `ps`, never from our own accounting.
/// Both macOS's and Linux's `ps -o rss=` report kibibytes.
pub fn rss_kib(pid: u32) -> io::Result<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "ps -o rss= -p {pid} exited with {}",
            out.status
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unparseable ps rss output {text:?}: {e}"),
        )
    })
}

/// A real `qoopd`, running in the foreground in a private runtime directory, killed on drop.
pub struct DaemonProcess {
    child: Child,
    dir: tempfile::TempDir,
    socket: PathBuf,
}

impl DaemonProcess {
    /// Spawn the real binary as `qoop daemon` and wait for its socket to accept a connection.
    pub fn spawn(bin: &Path) -> io::Result<Self> {
        let dir = tempfile::tempdir()?;
        // `tempdir` creates 0755; the daemon refuses anything group or other accessible.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket = qoop_testkit::socket_in(dir.path());
        let child = qoop_testkit::sandboxed(bin)
            .arg("daemon")
            .env("QOOP_RUNTIME_DIR", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            // The harness polls the default session's socket, so a session
            // inherited from the surrounding shell (running the bench inside
            // a qoop session) would make the daemon listen where nobody
            // looks and every spawn time out. Measured exactly that way.
            .env_remove(qoop_runtime::paths::SESSION_ENV)
            // A bench run inside a qoop terminal also inherits the terminal's
            // own token pair. The daemon ignores them, but any `qoop` child
            // process spawned later must not mistake itself for that
            // terminal's shell, so they are stripped at every spawn.
            .env_remove("QOOP_TOKEN")
            .env_remove("QORO_TOKEN")
            .env_remove("QOOP_TERMINAL")
            .env_remove("QORO_TERMINAL")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                // A daemon binds with no terminal; the scenarios measure the one a client
                // opens, as `qoop` does.
                qoop_testkit::try_open_first_terminal(&socket).map_err(io::Error::other)?;
                return Ok(Self { child, dir, socket });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "qoopd did not open its socket within 5s",
        ))
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

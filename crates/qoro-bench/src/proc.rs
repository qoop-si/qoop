//! Process-level helpers: resolving the real `qoro` binary, spawning a real daemon, and
//! reading resident memory from the OS rather than from our own bookkeeping.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Resolve the real `qoro` binary.
///
/// `qoro-bench` is a separate crate from `qoro-cli`, so `env!("CARGO_BIN_EXE_qoro")` is not
/// available here: that macro only resolves inside the crate that owns the binary (or an
/// integration test of that same crate). `QORO_BIN` overrides this for a release build or a
/// non-default target directory; otherwise this assumes `target/debug/qoro` under the
/// workspace root, which is what `cargo build` produces alongside this crate by default.
pub fn bin_path() -> PathBuf {
    if let Ok(p) = std::env::var("QORO_BIN") {
        return PathBuf::from(p);
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent() // crates/
        .and_then(Path::parent) // workspace root
        .expect("qoro-bench's manifest sits two directories below the workspace root")
        .join("target/debug/qoro")
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

/// A real `qorod`, running in the foreground in a private runtime directory, killed on drop.
pub struct DaemonProcess {
    child: Child,
    dir: tempfile::TempDir,
    socket: PathBuf,
}

impl DaemonProcess {
    /// Spawn the real binary as `qoro daemon` and wait for its socket to accept a connection.
    pub fn spawn(bin: &Path) -> io::Result<Self> {
        let dir = tempfile::tempdir()?;
        // `tempdir` creates 0755; the daemon refuses anything group or other accessible.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket = qoro_testkit::socket_in(dir.path());
        let child = Command::new(bin)
            .arg("daemon")
            .env("QORO_RUNTIME_DIR", dir.path())
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return Ok(Self { child, dir, socket });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "qorod did not open its socket within 5s",
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

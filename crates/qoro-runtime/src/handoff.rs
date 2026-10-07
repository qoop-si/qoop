//! What one daemon hands to the next across the `exec` that makes it the new binary.
//!
//! The terminals' pty masters are not close-on-exec for that one `exec`, and the process
//! keeps its pid, so every shell is still its child. What the kernel cannot carry is written
//! here: which descriptor is which tab, whose child it is, the token its hooks speak with.
//! The file is read once by the process that wrote it, becoming another program, and
//! deleted as it is read.
//!
//! Three things keep a file that was left behind from ever being adopted. Adoption is only
//! entered by `qoro daemon --adopt <file> --nonce <n>`; the file names the pid that wrote it,
//! which an `exec` keeps and no other process has; and the nonce on the command line has to
//! be the one in the file. It holds hook tokens, so it is owner only in the private runtime
//! directory.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The layout of the file this build writes.
pub const FORMAT: u32 = 1;

/// The layouts this build can read, as `qoro --handoff-formats` says them. The daemon that
/// is about to move asks the binary it will become, and hands over only in a layout that one
/// reads; a binary that says nothing (every build before this one) gets the move that waits
/// for the tabs to be idle and brings them back.
pub const FORMATS: &[u32] = &[FORMAT];

/// Everything one daemon carries to the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub format: u32,
    /// The pid of the writer. An `exec` keeps it, so the reader finds its own.
    pub pid: u32,
    pub nonce: String,
    pub terminals: Vec<Carried>,
}

/// One terminal, as its descriptor and the facts the kernel does not keep for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carried {
    pub id: u32,
    /// The master, at this number in the process that reads this.
    pub fd: RawFd,
    /// The shell, which is still this process's child after the `exec`.
    pub child: i32,
    /// When it started, for the check that the pid is the one that was written.
    pub child_start: Option<u64>,
    /// The device of the slave, which is what contains the terminal's processes.
    pub tty_dev: u64,
    pub workspace: i64,
    pub name: Option<String>,
    pub cwd: Option<String>,
    /// What its hooks speak with: the child's environment cannot change, so it is carried.
    pub token: String,
    pub cols: u16,
    pub rows: u16,
    /// Bytes still waiting to be typed into it.
    pub pending: Vec<u8>,
}

impl Handoff {
    /// The descriptors the new process has to keep open past its start.
    pub fn fds(&self) -> Vec<RawFd> {
        self.terminals.iter().map(|t| t.fd).collect()
    }
}

/// A nonce nobody could have guessed: sixteen bytes of the system's own randomness.
pub fn fresh_nonce() -> io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Where the file for this process goes in `dir`.
pub fn path_in(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("handoff-{pid}.json"))
}

/// Write the file, owner only, whole or not at all: the new process either reads all of it or
/// finds nothing there.
pub fn write(dir: &Path, handoff: &Handoff) -> io::Result<PathBuf> {
    let path = path_in(dir, handoff.pid);
    let partial = dir.join(format!(".handoff-{}.partial", handoff.pid));
    let text = serde_json::to_vec(handoff).map_err(io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)?;
    file.write_all(&text)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// Read the file this process was started to adopt, and delete it. Refused, with the file
/// still deleted, when it is not what the command line promised: another layout, another
/// writer, another nonce, or a file anybody but this user could have written or read.
pub fn read(path: &Path, nonce: &str) -> io::Result<Handoff> {
    let meta = std::fs::metadata(path)?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let me = unsafe { libc::geteuid() };
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    if meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "the handoff file is not private to this user",
        ));
    }
    let handoff: Handoff = serde_json::from_str(&text?).map_err(io::Error::other)?;
    if handoff.format != FORMAT {
        return Err(io::Error::other(format!(
            "the handoff is in layout {}, this build reads {FORMAT}",
            handoff.format
        )));
    }
    if handoff.pid != std::process::id() {
        return Err(io::Error::other(format!(
            "the handoff was written by process {}, not this one",
            handoff.pid
        )));
    }
    if handoff.nonce != nonce {
        return Err(io::Error::other("the handoff is not the one asked for"));
    }
    Ok(handoff)
}

/// What a command line asked this daemon to adopt, if it asked: `--adopt <file> --nonce <n>`.
pub fn requested(args: &[String]) -> Option<(PathBuf, String)> {
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    Some((PathBuf::from(after("--adopt")?), after("--nonce")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Handoff {
        Handoff {
            format: FORMAT,
            pid: std::process::id(),
            nonce: "abc".into(),
            terminals: vec![Carried {
                id: 3,
                fd: 9,
                child: 123,
                child_start: Some(7),
                tty_dev: 55,
                workspace: 1,
                name: Some("build".into()),
                cwd: Some("/tmp".into()),
                token: "qoro-3-x".into(),
                cols: 100,
                rows: 30,
                pending: b"ls\r".to_vec(),
            }],
        }
    }

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        dir
    }

    #[test]
    fn what_was_written_is_read_back_once_and_the_file_goes() {
        let dir = private_dir();
        let path = write(dir.path(), &sample()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            0o600,
            "the file carries hook tokens"
        );
        assert_eq!(read(&path, "abc").unwrap(), sample());
        assert!(!path.exists(), "a read file is deleted");
    }

    #[test]
    fn a_file_from_another_writer_or_nonce_or_layout_is_refused_and_deleted() {
        let dir = private_dir();
        for broken in [
            Handoff {
                pid: std::process::id() + 1,
                ..sample()
            },
            Handoff {
                format: FORMAT + 1,
                ..sample()
            },
            sample(),
        ] {
            let path = write(dir.path(), &broken).unwrap();
            let asked = if broken == sample() { "other" } else { "abc" };
            assert!(read(&path, asked).is_err(), "{broken:?}");
            assert!(!path.exists(), "a refused file stays behind");
        }
    }

    #[test]
    fn a_file_other_users_can_read_is_refused() {
        let dir = private_dir();
        let path = write(dir.path(), &sample()).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        assert!(read(&path, "abc").is_err());
    }

    #[test]
    fn the_command_line_names_the_file_and_the_nonce() {
        let args: Vec<String> = ["qoro", "daemon", "--adopt", "/run/h.json", "--nonce", "n1"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            requested(&args),
            Some((PathBuf::from("/run/h.json"), "n1".into()))
        );
        assert_eq!(requested(&args[..2]), None);
        assert_eq!(requested(&args[..4]), None, "a file with no nonce");
    }
}

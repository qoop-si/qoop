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
//! be the one in the file. The nonce is on a command line, which any local user can read, so
//! it only guards against a stale file and not against another process: what keeps others out is
//! the file being owner only in the private runtime directory, since it holds hook tokens.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The layout of the file this build writes. Nothing in it is required past what a terminal
/// is (its descriptor, its shell, its token): what later builds carry beside that is read
/// with a default, so the first layout keeps being readable as it grows.
///
/// It is also what `qoro --handoff-formats` says: the layout this build reads and writes. The
/// daemon that is about to move asks the binary it will become, and hands over only in a
/// layout that one reads; a binary that says nothing (every build before this one) gets the
/// move that waits for the tabs to be idle and brings them back.
pub const FORMAT: u32 = 1;

/// Everything one daemon carries to the next.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub format: u32,
    /// The pid of the writer. An `exec` keeps it, so the reader finds its own.
    pub pid: u32,
    pub nonce: String,
    pub terminals: Vec<Carried>,
    /// The socket the daemon listens on, still open: connections made while the next one
    /// starts wait in its queue instead of being refused, which is what tells a client or a
    /// hook that no daemon is there and sends it to start one.
    #[serde(default)]
    pub listener: Option<RawFd>,
    /// The startup lock, held across the move so a daemon started in the gap waits for it.
    #[serde(default)]
    pub startup_lock: Option<RawFd>,
    /// The lock on the state directory, when it is not the runtime directory.
    #[serde(default)]
    pub state_lock: Option<RawFd>,
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
    #[serde(default)]
    pub pending: Vec<u8>,
    /// Escape sequences that rebuild its screen and modes in a fresh engine.
    #[serde(default)]
    pub screen: String,
    /// Whether its program is to be made to redraw once it is taken over.
    #[serde(default)]
    pub nudge: bool,
    /// What the agent registry believes about the program: generation, identity, state.
    #[serde(default)]
    pub agent: Option<crate::agents::AgentCarry>,
    /// The conversation id its agent said by hook, and who vouches for it. Carried with the
    /// generation: an id said by a generation the new daemon does not know is forgotten.
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub session_said: Option<crate::daemon::Said>,
}

impl Handoff {
    /// The descriptors the new process has to keep open past its start.
    pub fn fds(&self) -> Vec<RawFd> {
        self.terminals
            .iter()
            .map(|t| t.fd)
            .chain(
                [self.listener, self.startup_lock, self.state_lock]
                    .into_iter()
                    .flatten(),
            )
            .collect()
    }
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
    // Not synced to disk: the reader is this same boot, and the sync (a full flush on a
    // Mac) is time every tab spends parked.
    file.write_all(&text)?;
    drop(file);
    std::fs::rename(&partial, &path)?;
    Ok(path)
}

/// Read the file this process was started to adopt, and delete it. Refused, with the file
/// still deleted, when it is not what the command line promised: another layout, another
/// writer, another nonce, or a file anybody but this user could have written or read.
pub fn read(path: &Path, nonce: &str) -> io::Result<Handoff> {
    let handoff = parse(path, nonce, true);
    let _ = std::fs::remove_file(path);
    handoff
}

/// What `read` checks, without deleting the file and without asking whose it is: the dry
/// run the daemon that is about to move makes of the binary it is about to become
/// (`qoro daemon --adopt-check`), which is another process than the one that will read it.
pub fn check(path: &Path, nonce: &str) -> io::Result<()> {
    parse(path, nonce, false).map(|_| ())
}

fn parse(path: &Path, nonce: &str, ours: bool) -> io::Result<Handoff> {
    let meta = std::fs::metadata(path)?;
    // SAFETY: geteuid has no arguments and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "the handoff file is not private to this user",
        ));
    }
    let text = std::fs::read_to_string(path)?;
    let handoff: Handoff = serde_json::from_str(&text).map_err(io::Error::other)?;
    if handoff.format != FORMAT {
        return Err(io::Error::other(format!(
            "the handoff is in layout {}, this build reads {FORMAT}",
            handoff.format
        )));
    }
    if ours && handoff.pid != std::process::id() {
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

/// Remove handover files nobody is going to read: one a process left when it died before
/// the next could read it, and a partial one from a write that failed. A file whose writer
/// is still running is its reader's, and stays.
pub fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let pid = name
            .strip_prefix("handoff-")
            .or_else(|| name.strip_prefix(".handoff-"))
            .and_then(|rest| rest.split('.').next())
            .and_then(|pid| pid.parse::<i32>().ok());
        let Some(pid) = pid.filter(|pid| *pid > 0) else {
            continue;
        };
        // SAFETY: signal 0 only asks whether the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive && pid as u32 != std::process::id() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The pids of the shells a handover file names, read as loosely as it can be: this is for a
/// file that was refused, whose children lost their terminals and need waiting for, so it
/// asks nothing of the file but that it says which processes they are.
pub fn children_in(path: &Path) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    value["terminals"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["child"].as_u64())
        .filter_map(|pid| u32::try_from(pid).ok())
        .collect()
}

/// What a command line asked this daemon to adopt, if it asked: `--adopt <file> --nonce <n>`.
pub fn requested(args: &[String]) -> Option<(PathBuf, String)> {
    requested_with(args, "--adopt")
}

/// The same for the dry run: `--adopt-check <file> --nonce <n>`.
pub fn check_requested(args: &[String]) -> Option<(PathBuf, String)> {
    requested_with(args, "--adopt-check")
}

fn requested_with(args: &[String], flag: &str) -> Option<(PathBuf, String)> {
    let after = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    Some((PathBuf::from(after(flag)?), after("--nonce")?))
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
                screen: "\x1b[2J\x1b[1;1Hhi".into(),
                nudge: true,
                agent: None,
                session: Some("conv-1".into()),
                session_said: Some(crate::daemon::Said::By("claude".into(), 4)),
            }],
            listener: Some(5),
            startup_lock: Some(6),
            state_lock: None,
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
    fn the_shells_of_a_file_that_cannot_be_read_properly_are_still_named() {
        let dir = private_dir();
        let path = dir.path().join("h.json");
        std::fs::write(
            &path,
            r#"{"format":9,"terminals":[{"child":41},{"child":42},{"x":1}]}"#,
        )
        .unwrap();
        assert_eq!(children_in(&path), vec![41, 42]);
        std::fs::write(&path, "not json").unwrap();
        assert!(children_in(&path).is_empty());
        assert!(children_in(&dir.path().join("none")).is_empty());
    }

    #[test]
    fn a_dry_run_reads_it_for_another_process_and_leaves_it_there() {
        let dir = private_dir();
        let other = Handoff {
            pid: std::process::id() + 1,
            ..sample()
        };
        let path = write(dir.path(), &other).unwrap();
        check(&path, "abc").expect("a dry run does not ask whose it is");
        assert!(path.exists(), "a dry run deletes nothing");
        assert!(check(&path, "other").is_err());
        assert!(
            read(&path, "abc").is_err(),
            "the real read is for the writer"
        );
    }

    #[test]
    fn files_nobody_will_read_are_swept_and_a_living_writers_stays() {
        let dir = private_dir();
        let gone = i32::MAX - 7;
        std::fs::write(dir.path().join(format!("handoff-{gone}.json")), "{}").unwrap();
        std::fs::write(dir.path().join(format!(".handoff-{gone}.partial")), "{").unwrap();
        let mine = dir
            .path()
            .join(format!("handoff-{}.json", std::process::id()));
        std::fs::write(&mine, "{}").unwrap();
        let other = dir.path().join("qorod.sock");
        std::fs::write(&other, "").unwrap();
        // The parent of this test process is alive and is not this process.
        let alive = dir.path().join(format!(
            "handoff-{}.json",
            std::os::unix::process::parent_id()
        ));
        std::fs::write(&alive, "{}").unwrap();
        sweep(dir.path());
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(
            !left
                .iter()
                .any(|p| p.to_string_lossy().contains(&gone.to_string()))
        );
        assert!(
            mine.exists() && alive.exists() && other.exists(),
            "{left:?}"
        );
    }

    #[test]
    fn a_file_without_what_later_builds_add_is_still_read() {
        let dir = private_dir();
        let old = format!(
            r#"{{"format":1,"pid":{},"nonce":"abc","terminals":[{{"id":1,"fd":9,"child":2,"child_start":null,"tty_dev":3,"workspace":1,"name":null,"cwd":null,"token":"t","cols":80,"rows":24}}]}}"#,
            std::process::id()
        );
        let path = dir.path().join("handoff.json");
        std::fs::write(&path, old).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let read = read(&path, "abc").expect("an older layout of the same format reads");
        assert!(read.listener.is_none() && read.terminals[0].screen.is_empty());
    }

    #[test]
    fn every_descriptor_the_next_process_has_to_keep_is_listed() {
        assert_eq!(sample().fds(), vec![9, 5, 6]);
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
        assert_eq!(check_requested(&args), None, "the dry run is its own flag");
    }
}

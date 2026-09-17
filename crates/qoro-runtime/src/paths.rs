//! Runtime directory and socket path, with the checks that make them safe.
//!
//! ADR-012. The daemon executes arbitrary commands with the user's full privileges, so
//! anything that can speak to its socket can run code as that user. A path is not a
//! security contract; these checks are.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Overrides the runtime directory: the place every session of this user lives under. It
/// exists so integration tests, which run in parallel, each get their own daemon and
/// socket, and so a contributor running the suite does not attach to their own working
/// daemon.
pub const RUNTIME_DIR_ENV: &str = "QORO_RUNTIME_DIR";

/// Names the session to attach to, for a person who would rather set it once than pass
/// `--session` to everything. The flag wins when both are given.
pub const SESSION_ENV: &str = "QORO_SESSION";

/// The session a person gets when they do not ask for one.
pub const DEFAULT_SESSION: &str = "default";

/// The longest a session name may be. Long enough for a project name, short enough that
/// the directory it makes is readable in a listing.
const NAME_LIMIT: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("no usable runtime directory: set {RUNTIME_DIR_ENV} or XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error(
        "{0:?} cannot be a session name: it becomes a directory, so it needs one that has \
         no slash, no colon, no spaces, does not start with a dot and is at most \
         {NAME_LIMIT} characters"
    )]
    BadSessionName(String),
    #[error("{0} is a symlink, which is refused because its target can change under us")]
    IsSymlink(PathBuf),
    #[error("{path} is owned by uid {owner}, not by us ({us})")]
    WrongOwner { path: PathBuf, owner: u32, us: u32 },
    #[error("{path} has mode {mode:o}; it must not be accessible to group or other")]
    TooPermissive { path: PathBuf, mode: u32 },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolve and verify the runtime directory, creating it if needed.
///
/// The daemon refuses to start rather than repairing an unsafe directory. That is a visible
/// failure on a misconfigured machine, which is the correct trade against silently running
/// with a world-writable socket.
pub fn runtime_dir() -> Result<PathBuf, PathError> {
    let base = runtime_base()?;
    let dir = session_dir(&base, &session_name(None)?);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// The directory every session of this user lives under.
///
/// Verified the same way a session directory is, because it is the parent of all of them
/// and a world writable parent is a parent somebody else can put a session into.
pub fn runtime_base() -> Result<PathBuf, PathError> {
    let dir = if let Ok(explicit) = std::env::var(RUNTIME_DIR_ENV) {
        PathBuf::from(explicit)
    } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        Path::new(&xdg).join("qoro")
    } else {
        // Fallback. The verification below matters most here: a shared temp directory is
        // exactly where another user could have created the path first.
        let uid = current_uid();
        std::env::temp_dir().join(format!("qoro-{uid}"))
    };
    ensure_private_dir(&dir)?;
    ensure_private_dir(&sessions_dir(&dir))?;
    Ok(dir)
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("qorod.sock")
}

/// Where every session lives.
pub fn sessions_dir(runtime: &Path) -> PathBuf {
    runtime.join("sessions")
}

/// One session's own directory: its socket, its database, its history, its event log.
///
/// A session is a directory and a daemon, which is what makes stopping one possible at
/// all: ending a session is ending its process, and the operating system does the rest.
/// This is the only place a session path is built, because the shape ADR-006 warned about
/// is the one where three callers each build it slightly differently.
pub fn session_dir(runtime: &Path, name: &str) -> PathBuf {
    sessions_dir(runtime).join(name)
}

/// Whether a name can be a session.
///
/// It becomes a directory name, so the rules are the ones a directory name needs: no
/// separator, no walking upwards, nothing invisible, and a length a listing can show.
/// Refused rather than sanitised: a person who typed `../../etc` meant something, and
/// quietly turning it into `etc` is worse than saying no.
pub fn valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_LIMIT
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && c != ':')
}

/// The session a command is about: what was asked for, else the environment, else the
/// default.
pub fn session_name(asked: Option<&str>) -> Result<String, PathError> {
    let name = asked
        .map(str::to_string)
        .or_else(|| std::env::var(SESSION_ENV).ok())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| DEFAULT_SESSION.to_string());
    if !valid_session_name(&name) {
        return Err(PathError::BadSessionName(name));
    }
    Ok(name)
}

/// Every session that exists on disk, named, in order.
///
/// Read from the directory rather than from a daemon: a person asking what sessions they
/// have is usually asking because none of them are running.
pub fn sessions(runtime: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(sessions_dir(runtime)) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_session_name(n))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("qorod.lock")
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Create the directory if absent, then verify it. Verification happens after creation
/// either way, because the interesting case is the directory someone else created first.
pub fn ensure_private_dir(dir: &Path) -> Result<(), PathError> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    verify_private_dir(dir)
}

pub fn verify_private_dir(dir: &Path) -> Result<(), PathError> {
    // `symlink_metadata` does not follow, which is the point: a symlink here means the real
    // target can be changed under us after the check.
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(PathError::IsSymlink(dir.to_path_buf()));
    }
    if !meta.is_dir() {
        return Err(PathError::NotADirectory(dir.to_path_buf()));
    }
    let us = current_uid();
    let owner = std::os::unix::fs::MetadataExt::uid(&meta);
    if owner != us {
        return Err(PathError::WrongOwner {
            path: dir.to_path_buf(),
            owner,
            us,
        });
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(PathError::TooPermissive {
            path: dir.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_directory_is_created_private() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("rt");
        ensure_private_dir(&dir).expect("must accept a directory it created");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_group_or_world_accessible_directory_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("loose");
        std::fs::create_dir(&dir).unwrap();
        for mode in [0o777, 0o750, 0o705] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = verify_private_dir(&dir).unwrap_err();
            assert!(
                matches!(err, PathError::TooPermissive { .. }),
                "mode {mode:o} must be refused, got {err}"
            );
        }
    }

    #[test]
    fn a_symlinked_runtime_directory_is_refused() {
        // The target can be swapped after the check, so the link itself is the problem.
        let base = tempfile::tempdir().unwrap();
        let real = base.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            verify_private_dir(&link).unwrap_err(),
            PathError::IsSymlink(_)
        ));
    }

    #[test]
    fn a_file_where_the_directory_should_be_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let f = base.path().join("notadir");
        std::fs::write(&f, b"").unwrap();
        assert!(matches!(
            verify_private_dir(&f).unwrap_err(),
            PathError::NotADirectory(_)
        ));
    }

    #[test]
    fn the_env_override_is_honoured_and_still_verified() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("override");
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var(RUNTIME_DIR_ENV, &dir) };
        let got = runtime_dir().expect("the override must be usable");
        assert_eq!(
            got,
            dir.join("sessions").join(DEFAULT_SESSION),
            "the override names where sessions live, and a run that named none is in the \
             default one"
        );
        assert_eq!(runtime_base().expect("the base"), dir);
        // SAFETY: as above.
        unsafe { std::env::remove_var(RUNTIME_DIR_ENV) };
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            verify_private_dir(&dir).is_err(),
            "the override is verified like any other path"
        );
    }

    #[test]
    fn a_session_name_that_would_escape_its_directory_is_refused_rather_than_cleaned() {
        for bad in [
            "..",
            ".",
            "../etc",
            "a/b",
            "a\\b",
            ".hidden",
            "two words",
            "a:b",
            "with\ttab",
        ] {
            assert!(
                !valid_session_name(bad),
                "{bad:?} would become a directory somewhere else"
            );
            // SAFETY: single-threaded test setup.
            unsafe { std::env::remove_var(SESSION_ENV) };
            assert!(
                session_name(Some(bad)).is_err(),
                "{bad:?} has to be refused by name, not quietly turned into something else"
            );
        }
        for good in ["app", "work-1", "webshop_2", "a", &"x".repeat(NAME_LIMIT)] {
            assert!(valid_session_name(good), "{good:?} is a reasonable name");
        }
        assert!(
            !valid_session_name(&"x".repeat(NAME_LIMIT + 1)),
            "a name past the limit is refused"
        );
        // An empty name is not a bad name, it is no name: the default, same as saying
        // nothing at all.
        assert!(!valid_session_name(""));
        assert_eq!(session_name(Some("")).unwrap(), DEFAULT_SESSION);
    }

    #[test]
    fn a_session_comes_from_the_flag_then_the_environment_then_the_default() {
        // SAFETY: single-threaded test setup.
        unsafe { std::env::remove_var(SESSION_ENV) };
        assert_eq!(session_name(None).unwrap(), DEFAULT_SESSION);
        // SAFETY: as above.
        unsafe { std::env::set_var(SESSION_ENV, "from-env") };
        assert_eq!(session_name(None).unwrap(), "from-env");
        assert_eq!(
            session_name(Some("from-flag")).unwrap(),
            "from-flag",
            "the flag wins, or passing it would do nothing in a shell that exports the name"
        );
        // An empty variable is not a session, it is an unset one.
        // SAFETY: as above.
        unsafe { std::env::set_var(SESSION_ENV, "") };
        assert_eq!(session_name(None).unwrap(), DEFAULT_SESSION);
        // SAFETY: as above.
        unsafe { std::env::remove_var(SESSION_ENV) };
    }

    #[test]
    fn two_sessions_are_two_directories_and_a_listing_reads_them_off_the_disk() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(sessions(base.path()), Vec::<String>::new());

        for name in ["work", "play"] {
            ensure_private_dir(&session_dir(base.path(), name)).expect("a session directory");
        }
        assert_eq!(sessions(base.path()), vec!["play", "work"]);
        assert_ne!(
            socket_path(&session_dir(base.path(), "work")),
            socket_path(&session_dir(base.path(), "play")),
            "two sessions sharing a socket are one session"
        );

        // A file, and a name no session could have, are not sessions.
        std::fs::write(sessions_dir(base.path()).join("notes.txt"), b"").unwrap();
        std::fs::create_dir(sessions_dir(base.path()).join(".git")).unwrap();
        assert_eq!(sessions(base.path()), vec!["play", "work"]);
    }
}

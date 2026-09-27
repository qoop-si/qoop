//! Runtime directory and socket path, with the checks that make them safe.
//!
//! ADR-012. The daemon executes arbitrary commands with the user's full privileges, so
//! anything that can speak to its socket can run code as that user. A path is not a
//! security contract; these checks are.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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

/// The person's own qoro configuration directory: `$XDG_CONFIG_HOME/qoro`, else
/// `~/.config/qoro`.
///
/// One resolver, because the hooks, the agent-detection overrides, the avatar, the saved
/// remotes and the interface's own settings all live here, and three copies of this lookup
/// had already been written by the time a fourth was needed. Not verified like the runtime
/// directory: this is the person's own configuration, with the same trust as their shell
/// rc, and a missing directory simply means nothing has been configured.
///
/// The XDG spec's rule, which the hand-copied lookups skipped: an empty or relative
/// `XDG_CONFIG_HOME` is ignored. Taken as is, a relative one put the hook scripts under
/// whatever directory `qoro hooks install` ran in, and registered a relative command that
/// broke from every other directory.
pub fn config_dir() -> PathBuf {
    config_dir_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The same answer from given values, so a test never sets the process environment.
fn config_dir_from(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("qoro")
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
///
/// An empty name asked for is refused like any other bad name: a trailing `--session`
/// or `qoro stop ""` is a mistake, and quietly working on `default` instead is the
/// wrong session. An empty `QORO_SESSION` is different: it means unset.
pub fn session_name(asked: Option<&str>) -> Result<String, PathError> {
    let name = match asked {
        Some(name) => name.to_string(),
        None => std::env::var(SESSION_ENV)
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| DEFAULT_SESSION.to_string()),
    };
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

/// The runtime base without creating or verifying anything, so reading the
/// last-used session never creates state: `--help` must not make directories.
pub fn raw_runtime_base() -> PathBuf {
    if let Ok(explicit) = std::env::var(RUNTIME_DIR_ENV) {
        PathBuf::from(explicit)
    } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        Path::new(&xdg).join("qoro")
    } else {
        std::env::temp_dir().join(format!("qoro-{}", current_uid()))
    }
}

/// The one file under the runtime base holding the last opened session.
///
/// A plain name, so a bare `qoro` can reopen what was there. Read only when no
/// flag and no env name a session; written on every successful attach.
pub fn last_session_path(base: &Path) -> PathBuf {
    base.join("last-session")
}

/// The last opened session, if the file holds a name a session could have.
///
/// Anything else is no answer, not an error: a stale, edited or truncated file
/// falls back to the default rather than failing the open. Stale means the
/// session directory is gone, so a bare open never resurrects a name from a
/// file alone without the directory that makes it a session.
pub fn read_last_session(base: &Path) -> Option<String> {
    let text = std::fs::read_to_string(last_session_path(base)).ok()?;
    let name = text.trim().to_string();
    if !valid_session_name(&name) {
        return None;
    }
    session_dir(base, &name).is_dir().then_some(name)
}

/// Remember the session that was just opened. Best effort: an attach that
/// cannot write this still attaches, and the next bare open falls back as
/// before. An invalid name is never written.
pub fn write_last_session(base: &Path, name: &str) {
    if !valid_session_name(name) {
        return;
    }
    let _ = std::fs::write(last_session_path(base), format!("{name}\n"));
}

/// Where `qoro update --check` leaves the newest version it saw on GitHub Releases.
///
/// A plain file holding a version like `0.4.0`, so the interface can show it without
/// touching the network: the TUI never fetches, it only reads this file. Built without
/// creating or verifying anything, because reading a notice must not create state.
pub fn update_notice_path() -> PathBuf {
    let base = if let Ok(explicit) = std::env::var(RUNTIME_DIR_ENV) {
        PathBuf::from(explicit)
    } else if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        Path::new(&xdg).join("qoro")
    } else {
        std::env::temp_dir().join(format!("qoro-{}", current_uid()))
    };
    base.join("update-available")
}

/// The newest version the updater saw, if the file holds one shaped like a version.
pub fn read_update_notice() -> Option<String> {
    let text = std::fs::read_to_string(update_notice_path()).ok()?;
    let version = text.trim().trim_start_matches('v').to_string();
    parse_version(&version).map(|_| version)
}

/// A dotted version into its numbers: `v1.2.3`, `1.2` and `1` all parse, and anything
/// else is refused rather than guessed at, so a tag nobody can parse never outranks
/// the build in hand.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    if text.is_empty() {
        return None;
    }
    let mut parts = text.split('.');
    let mut nums = [0u64; 3];
    for (at, part) in parts.by_ref().take(3).enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        nums[at] = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((nums[0], nums[1], nums[2]))
}

/// Whether `latest` is newer than `current`, both in the shape [`parse_version`] reads.
/// Anything unparsable answers false: an unreadable notice is no notice.
pub fn is_newer_version(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments and cannot fail.
    unsafe { libc::getuid() }
}

/// Create the directory if absent, then verify it. Verification happens after creation
/// either way, because the interesting case is the directory someone else created first.
///
/// Created 0700 in the one call, never chmodded afterwards: a chmod follows a symlink, and
/// one planted between an existence check and the create had its target's mode changed
/// before verification refused the link.
pub fn ensure_private_dir(dir: &Path) -> Result<(), PathError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
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

    /// Held by every test that sets or reads this module's environment variables: the
    /// test harness runs them on parallel threads, and one test's `remove_var` landing
    /// between another's `set_var` and its read failed it at random.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The XDG spec: an empty or relative XDG_CONFIG_HOME is ignored, because a relative
    /// one would put a person's configuration under whatever directory a command ran in.
    #[test]
    fn an_empty_or_relative_config_home_falls_back_to_home() {
        let home = Some(std::ffi::OsString::from("/Users/a"));
        let fallback = PathBuf::from("/Users/a/.config/qoro");
        assert_eq!(config_dir_from(Some("".into()), home.clone()), fallback);
        assert_eq!(config_dir_from(Some("cfg".into()), home.clone()), fallback);
        assert_eq!(config_dir_from(None, home.clone()), fallback);
        assert_eq!(
            config_dir_from(Some("/x/cfg".into()), home),
            PathBuf::from("/x/cfg/qoro")
        );
        assert_eq!(
            config_dir_from(None, None),
            std::env::temp_dir().join("qoro")
        );
    }

    #[test]
    fn a_fresh_directory_is_created_private() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("rt");
        ensure_private_dir(&dir).expect("must accept a directory it created");
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// A symlink where the directory should be is refused, and nothing is done through it:
    /// its target keeps the mode it had.
    #[test]
    fn a_symlink_in_place_of_the_directory_is_refused_and_its_target_left_alone() {
        let base = tempfile::tempdir().unwrap();
        let victim = base.path().join("victim");
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let planted = base.path().join("rt");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        assert!(matches!(
            ensure_private_dir(&planted).unwrap_err(),
            PathError::IsSymlink(_)
        ));
        let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the link's target was changed");
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
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
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
        // The update notice lives beside the sessions under the same override, so the
        // interface reads what the updater wrote no matter which directory is in play.
        // Asserted here rather than in a test of its own: both hold the same variable,
        // and two tests holding it at once race each other.
        assert_eq!(
            update_notice_path(),
            dir.join("update-available"),
            "the notice is one file next to everything else, not a second directory tree"
        );
        assert_eq!(read_update_notice(), None, "no file is no notice");
        std::fs::write(update_notice_path(), "0.4.0\n").unwrap();
        assert_eq!(read_update_notice().as_deref(), Some("0.4.0"));
        std::fs::write(update_notice_path(), "not a version\n").unwrap();
        assert_eq!(
            read_update_notice(),
            None,
            "garbage in the file is no notice"
        );
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
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
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
        // An empty name asked for is a mistake worth naming, a trailing `--session` or
        // `qoro stop ""`, and never quietly the default session.
        assert!(!valid_session_name(""));
        assert!(session_name(Some("")).is_err());
    }

    #[test]
    fn a_session_comes_from_the_flag_then_the_environment_then_the_default() {
        let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn dotted_versions_parse_and_compare() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v0.4.0"), Some((0, 4, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert!(parse_version("").is_none());
        assert!(parse_version("1.2.x").is_none());
        assert!(parse_version("1..2").is_none());
        assert!(is_newer_version("0.4.0", "0.3.1"));
        assert!(!is_newer_version("0.3.1", "0.3.1"));
        assert!(!is_newer_version("0.3.0", "0.3.1"));
        assert!(!is_newer_version("garbage", "0.3.1"));
    }

    #[test]
    fn the_last_session_round_trips_and_garbage_is_no_answer() {
        let base = tempfile::tempdir().unwrap();
        assert_eq!(read_last_session(base.path()), None);
        write_last_session(base.path(), "hobby");
        // A name in the file alone never opens a session: without the session
        // directory it is stale, and stale is no answer.
        assert_eq!(read_last_session(base.path()), None);
        std::fs::create_dir_all(session_dir(base.path(), "hobby")).unwrap();
        assert_eq!(read_last_session(base.path()).as_deref(), Some("hobby"));
        // An invalid name is never written, so the previous answer stands.
        write_last_session(base.path(), "../etc");
        assert_eq!(read_last_session(base.path()).as_deref(), Some("hobby"));
        // A session that went away is stale: no answer, not an error, and the
        // open falls back to the default rather than failing.
        std::fs::remove_dir(session_dir(base.path(), "hobby")).unwrap();
        assert_eq!(read_last_session(base.path()), None);
        std::fs::write(last_session_path(base.path()), "../etc\n").unwrap();
        assert_eq!(read_last_session(base.path()), None);
        std::fs::write(last_session_path(base.path()), "\n").unwrap();
        assert_eq!(read_last_session(base.path()), None);
    }
}

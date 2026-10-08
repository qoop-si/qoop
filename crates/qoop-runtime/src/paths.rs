//! Runtime directory and socket path, with the checks that make them safe.
//!
//! ADR-012. The daemon executes arbitrary commands with the user's full privileges, so
//! anything that can speak to its socket can run code as that user. A path is not a
//! security contract; these checks are.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Overrides the runtime directory: where this user's daemon lives. It exists so
/// integration tests, which run in parallel, each get their own daemon and socket, and so a
/// contributor running the suite does not attach to their own working daemon.
pub const RUNTIME_DIR_ENV: &str = "QOOP_RUNTIME_DIR";

/// What named a session, when there were several. Still read, so a shell or a login item
/// left from then is told where its session went instead of reaching the wrong daemon.
pub const SESSION_ENV: &str = "QOOP_SESSION";

/// The one daemon's directory under `sessions/`, the name the default session had, so the
/// state of everyone who used it is where it always was.
pub const DEFAULT_SESSION: &str = "default";

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("no usable runtime directory: set {RUNTIME_DIR_ENV} or XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
    #[error(
        "named sessions were replaced by spaces, so {name:?} is not opened any more; its \
         files are at {dir}, and to use them instead of the default, stop qoop and swap \
         that directory with {default}",
        dir = .dir.display(),
        default = .default.display()
    )]
    NamedSession {
        name: String,
        dir: PathBuf,
        default: PathBuf,
    },
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
    if let Ok(name) = std::env::var(SESSION_ENV) {
        only_default(&name, &base)?;
    }
    let dir = session_dir(&base, DEFAULT_SESSION);
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Where the runtime directory is when `QOOP_RUNTIME_DIR` does not name one:
/// `$XDG_RUNTIME_DIR/qoop`, else `qoop-<uid>` in the temp directory. Nothing is created or
/// checked. A service that pins `QOOP_RUNTIME_DIR` compares against this to know whether the
/// pin only repeats it (a pinned one also moves the daemon's state, see `state_location`).
pub fn default_runtime_base() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(xdg) => Path::new(&xdg).join("qoop"),
        Err(_) => std::env::temp_dir().join(format!("qoop-{}", current_uid())),
    }
}

/// The directory above the daemon's.
///
/// Verified the same way the daemon's directory is, because a world writable parent is a
/// parent somebody else can put a directory into.
pub fn runtime_base() -> Result<PathBuf, PathError> {
    let dir = match std::env::var(RUNTIME_DIR_ENV) {
        Ok(explicit) => PathBuf::from(explicit),
        // The verification below matters most for the temp directory fallback: a shared temp
        // directory is exactly where another user could have created the path first.
        Err(_) => default_runtime_base(),
    };
    ensure_private_dir(&dir)?;
    ensure_private_dir(&sessions_dir(&dir))?;
    Ok(dir)
}

/// The person's own qoop configuration directory: `$XDG_CONFIG_HOME/qoop`, else
/// `~/.config/qoop`.
///
/// One resolver, because the hooks, the agent-detection overrides, the avatar, the saved
/// remotes and the interface's own settings all live here, and three copies of this lookup
/// had already been written by the time a fourth was needed. Not verified like the runtime
/// directory: this is the person's own configuration, with the same trust as their shell
/// rc, and a missing directory simply means nothing has been configured.
///
/// The XDG spec's rule, which the hand-copied lookups skipped: an empty or relative
/// `XDG_CONFIG_HOME` is ignored. Taken as is, a relative one put the hook scripts under
/// whatever directory `qoop hooks install` ran in, and registered a relative command that
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
        .join("qoop")
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("qoopd.sock")
}

/// The same directory as `config_dir`, for the files a key depends on: the paired devices
/// and the hub. `None` rather than a fallback when neither variable is set to an absolute
/// path, because a device list written to `/tmp` or to the current directory is one anyone
/// can edit or commit.
pub fn strict_config_dir() -> Option<PathBuf> {
    let base =
        absolute_env("XDG_CONFIG_HOME").or_else(|| Some(absolute_env("HOME")?.join(".config")))?;
    Some(base.join("qoop"))
}

/// An environment variable naming a directory, only when it is absolute. An empty or
/// relative value is invalid by the XDG rules and, taken at face value, resolves against
/// the current directory: a key written there could end up committed to whatever
/// repository a person ran the command in.
fn absolute_env(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `$XDG_DATA_HOME/qoop`, else `~/Library/Application Support/qoop` on macOS and
/// `~/.local/share/qoop` elsewhere: where this machine's own key lives.
///
/// Not the config directory on purpose. Config directories end up in dotfiles repositories,
/// and a machine key that lands in one is a key anyone who reads the repository can use to
/// pose as this Mac.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(xdg) = absolute_env("XDG_DATA_HOME") {
        return Some(xdg.join("qoop"));
    }
    let home = absolute_env("HOME")?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/qoop")
    } else {
        home.join(".local/share/qoop")
    })
}

/// `$XDG_CACHE_HOME/qoop`, else `~/Library/Caches/qoop` on macOS and `~/.cache/qoop`
/// elsewhere: what qoop can build again (the notch app), so backups may skip it.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(xdg) = absolute_env("XDG_CACHE_HOME") {
        return Some(xdg.join("qoop"));
    }
    let home = absolute_env("HOME")?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/qoop")
    } else {
        home.join(".cache/qoop")
    })
}

/// Where qoop's own services write their logs: `~/Library/Logs/qoop` on macOS, where
/// Console finds them; elsewhere `$XDG_STATE_HOME/qoop/log`, else `~/.local/state/qoop/log`.
pub fn logs_dir() -> Option<PathBuf> {
    let home = absolute_env("HOME")?;
    if cfg!(target_os = "macos") {
        return Some(home.join("Library/Logs/qoop"));
    }
    Some(
        absolute_env("XDG_STATE_HOME")
            .unwrap_or_else(|| home.join(".local/state"))
            .join("qoop/log"),
    )
}

/// qass, the assistant over the other agents: the files its owner writes (its `policy`,
/// the `lessons` it keeps from him), beside the rest of his qoop settings.
pub fn qass_config_dir() -> PathBuf {
    config_dir().join("qass")
}

/// qass's own state (its goals, the gate's log, what the watcher saw) and its working
/// folder, whose `.claude/settings.json` gates any Claude started there.
pub fn qass_data_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("qass"))
}

/// Where the daemon keeps what has to outlive a reboot: its database and its history.
///
/// Not the runtime directory. On macOS that is `$TMPDIR`, which the system clears at boot
/// and of files untouched for about three days, and on Linux `XDG_RUNTIME_DIR` is a tmpfs
/// cleared at logout: a sleeping space kept there did not survive either. So it is
/// `data_dir()`, beside this machine's key.
///
/// With `QOOP_RUNTIME_DIR` set it is the session directory itself, as before. Every test
/// isolates its daemon through that variable, and a test daemon must never write into the
/// person's real data directory. Also the session directory when there is no home to
/// resolve a data directory from.
///
/// Created private, and a loose mode on a directory we own is tightened, which is what
/// the machine key's writer already does for the same directory; a symlink or another
/// owner is refused like the runtime directory's.
pub fn state_dir(session: &Path) -> Result<PathBuf, PathError> {
    let dir = state_location(session);
    if dir == session {
        return Ok(dir);
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e.into()),
        _ => {}
    }
    let meta = std::fs::symlink_metadata(&dir)?;
    if meta.is_dir()
        && !meta.file_type().is_symlink()
        && std::os::unix::fs::MetadataExt::uid(&meta) == current_uid()
        && meta.permissions().mode() & 0o077 != 0
    {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    verify_private_dir(&dir)?;
    Ok(dir)
}

/// Where `state_dir` is, without creating or checking anything, for a reader such as
/// `qoop ls` that must not leave a directory behind.
pub fn state_location(session: &Path) -> PathBuf {
    state_location_from(
        std::env::var_os(RUNTIME_DIR_ENV).is_some(),
        data_dir(),
        session,
    )
}

fn state_location_from(explicit_runtime: bool, data: Option<PathBuf>, session: &Path) -> PathBuf {
    match data {
        Some(data) if !explicit_runtime => data,
        _ => session.to_path_buf(),
    }
}

/// Where a running bridge listens for "look again" (the doorbell).
///
/// Beside the sessions, in the runtime base, so a daemon can find it from its own session
/// directory without resolving the base again (which creates directories).
pub fn bridge_socket(runtime: &Path) -> PathBuf {
    runtime.join("bridge.sock")
}

/// Where every session lives.
pub fn sessions_dir(runtime: &Path) -> PathBuf {
    runtime.join("sessions")
}

/// One session's own directory: its socket, its database and its history.
///
/// A session is a directory and a daemon, which is what makes stopping one possible at
/// all: ending a session is ending its process, and the operating system does the rest.
/// This is the only place a session path is built, because the shape ADR-006 warned about
/// is the one where three callers each build it slightly differently.
pub fn session_dir(runtime: &Path, name: &str) -> PathBuf {
    sessions_dir(runtime).join(name)
}

/// Accept a session name only if it is the default one, or empty. Any other name belonged
/// to a session of its own, which is now a space: refused with where its files are, rather
/// than quietly reaching the default daemon and a different tab under the same number.
pub fn only_default(name: &str, base: &Path) -> Result<(), PathError> {
    if name.is_empty() || name == DEFAULT_SESSION {
        return Ok(());
    }
    Err(PathError::NamedSession {
        name: name.to_string(),
        dir: session_dir(base, name),
        default: session_dir(base, DEFAULT_SESSION),
    })
}

pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("qoopd.lock")
}

/// Where `qoop update --check` leaves the newest version it saw on GitHub Releases.
///
/// A plain file holding a version like `0.4.0`, so the interface can show it without
/// touching the network: the TUI never fetches, it only reads this file. Built without
/// creating or verifying anything, because reading a notice must not create state.
pub fn update_notice_path() -> PathBuf {
    let base = match std::env::var(RUNTIME_DIR_ENV) {
        Ok(explicit) => PathBuf::from(explicit),
        Err(_) => default_runtime_base(),
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

/// Sixteen bytes of the system's own randomness, as hex: a name nobody could have guessed.
pub fn random_hex() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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

/// Block on a kqueue watch of `dir` until `socket` answers. Registered before the socket
/// is looked at, so a daemon binding in between is an event rather than a missed moment.
///
/// Also ends, with `BrokenPipe`, when the process that started this one exits: a watcher
/// waiting for a daemon writes nothing, so it would not otherwise learn that its reader
/// (the notch, restarted) is gone, and would wait on as an orphan.
#[cfg(target_os = "macos")]
pub fn wait_for_socket(dir: &std::path::Path, socket: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())?;
    // SAFETY: a NUL-terminated path, opened for events only.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: no arguments; the result is checked.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        let e = std::io::Error::last_os_error();
        // SAFETY: fd was opened above and is closed once.
        unsafe { libc::close(fd) };
        return Err(e);
    }
    // SAFETY: a zeroed kevent is a valid value to fill in.
    let mut changes: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
    changes[0].ident = fd as libc::uintptr_t;
    changes[0].filter = libc::EVFILT_VNODE;
    changes[0].flags = libc::EV_ADD | libc::EV_CLEAR;
    changes[0].fflags = libc::NOTE_WRITE;
    // SAFETY: no arguments.
    let parent = unsafe { libc::getppid() };
    changes[1].ident = parent as libc::uintptr_t;
    changes[1].filter = libc::EVFILT_PROC;
    changes[1].flags = libc::EV_ADD;
    changes[1].fflags = libc::NOTE_EXIT;
    // Started by launchd, or already orphaned: there is no reader's exit to wait for.
    let watched = if parent > 1 { 2 } else { 1 };
    // SAFETY: `watched` changes, no events asked for, on the kqueue made above.
    let rc = unsafe {
        libc::kevent(
            kq,
            changes.as_ptr(),
            watched,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    let result = if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        loop {
            if std::os::unix::net::UnixStream::connect(socket).is_ok() {
                break Ok(());
            }
            // SAFETY: a zeroed kevent is a valid out-param; one event, no timeout.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let n =
                unsafe { libc::kevent(kq, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    break Err(e);
                }
            } else if n > 0 && event.filter == libc::EVFILT_PROC {
                break Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the process reading this went away",
                ));
            }
        }
    };
    // SAFETY: both descriptors were opened above and are closed once.
    unsafe {
        libc::close(kq);
        libc::close(fd);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by every test that sets or reads this module's environment variables: the
    /// test harness runs them on parallel threads, and one test's `remove_var` landing
    /// between another's `set_var` and its read failed it at random.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A test daemon names its runtime directory, and its state stays there: resolving the
    /// person's data directory instead would put every test's database in their home.
    #[test]
    fn state_stays_in_an_explicit_runtime_directory() {
        let session = Path::new("/tmp/rt/sessions/default");
        let data = PathBuf::from("/Users/a/Library/Application Support/qoop");
        assert_eq!(
            state_location_from(true, Some(data.clone()), session),
            session
        );
        assert_eq!(
            state_location_from(false, Some(data.clone()), session),
            data
        );
        assert_eq!(state_location_from(false, None, session), session);
    }

    /// The XDG spec: an empty or relative XDG_CONFIG_HOME is ignored, because a relative
    /// one would put a person's configuration under whatever directory a command ran in.
    #[test]
    fn an_empty_or_relative_config_home_falls_back_to_home() {
        let home = Some(std::ffi::OsString::from("/Users/a"));
        let fallback = PathBuf::from("/Users/a/.config/qoop");
        assert_eq!(config_dir_from(Some("".into()), home.clone()), fallback);
        assert_eq!(config_dir_from(Some("cfg".into()), home.clone()), fallback);
        assert_eq!(config_dir_from(None, home.clone()), fallback);
        assert_eq!(
            config_dir_from(Some("/x/cfg".into()), home),
            PathBuf::from("/x/cfg/qoop")
        );
        assert_eq!(
            config_dir_from(None, None),
            std::env::temp_dir().join("qoop")
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
    fn only_the_default_session_is_accepted_and_the_others_are_pointed_at() {
        let base = tempfile::tempdir().unwrap();
        assert!(only_default("", base.path()).is_ok());
        assert!(only_default(DEFAULT_SESSION, base.path()).is_ok());
        let refused = only_default("work", base.path()).expect_err("a named session");
        let said = refused.to_string();
        assert!(said.contains("replaced by spaces"), "{said}");
        assert!(
            said.contains(&session_dir(base.path(), "work").display().to_string()),
            "{said}"
        );
    }
}

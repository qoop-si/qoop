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
    #[error(
        "the socket path {path} is {len} bytes, and a socket path holds at most {max}: {fix}",
        path = .path.display()
    )]
    SocketTooLong {
        path: PathBuf,
        len: usize,
        max: usize,
        fix: &'static str,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resolve and verify the runtime directory, creating it if needed.
///
/// The daemon refuses to start rather than repairing an unsafe directory. That is a visible
/// failure on a misconfigured machine, which is the correct trade against silently running
/// with a world-writable socket.
pub fn runtime_dir() -> Result<PathBuf, PathError> {
    // Before anything is created: a path too long to bind leaves no directories behind.
    let socket = socket_path(&session_dir(&base_location(), DEFAULT_SESSION));
    fits_socket(&socket, SOCKET_PATH_MAX, shorter_base())?;
    let base = runtime_base()?;
    if let Ok(name) = crate::legacy::var(SESSION_ENV) {
        only_default(&name, &base)?;
    }
    let dir = session_dir(&base, DEFAULT_SESSION);
    ensure_private_dir(&dir)?;
    if crate::legacy::migrating() {
        crate::legacy::reach_old_daemon(&dir);
    }
    Ok(dir)
}

/// Where the runtime base is: `QOOP_RUNTIME_DIR`, else [`default_runtime_base`]. Nothing is
/// created or checked.
fn base_location() -> PathBuf {
    match crate::legacy::var(RUNTIME_DIR_ENV) {
        Ok(explicit) => PathBuf::from(explicit),
        Err(_) => default_runtime_base(),
    }
}

/// The longest path a socket address holds, its closing NUL left out: `sun_path` is 108 bytes
/// on Linux and 104 on macOS.
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// How to shorten the runtime directory: the variable it came from, as `default_runtime_base`
/// reads them.
fn shorter_base() -> &'static str {
    if crate::legacy::var(RUNTIME_DIR_ENV).is_ok() {
        "set QOOP_RUNTIME_DIR to a shorter folder"
    } else if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        "set XDG_RUNTIME_DIR to a shorter folder"
    } else {
        "set TMPDIR to a shorter folder for every qoop command, for example: export TMPDIR=/tmp"
    }
}

/// Refused here with the path and the way out, rather than by the bind as "path must be
/// shorter than SUN_LEN": a long `TMPDIR` (a sandbox, a nix shell) gave every first start that.
fn fits_socket(socket: &Path, max: usize, fix: &'static str) -> Result<(), PathError> {
    let len = socket.as_os_str().len();
    if len > max {
        return Err(PathError::SocketTooLong {
            path: socket.to_path_buf(),
            len,
            max,
            fix,
        });
    }
    Ok(())
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
    // The verification below matters most for the temp directory fallback: a shared temp
    // directory is exactly where another user could have created the path first.
    let dir = base_location();
    ensure_private_dir(&dir)?;
    ensure_private_dir(&sessions_dir(&dir))?;
    if crate::legacy::migrating() {
        crate::legacy::reach_old_bridge(&dir);
    }
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

/// The file whose presence turns automatic updates off on this machine.
pub fn no_auto_update_path() -> PathBuf {
    config_dir().join("no-auto-update")
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

/// qass, the assistant over the other agents: the file it keeps from its owner (the
/// `lessons`), beside the rest of his qoop settings.
pub fn qass_config_dir() -> PathBuf {
    config_dir().join("qass")
}

/// qass's own state (its goals, the gate's log, what the watcher saw) and its working
/// folder, whose `.claude/settings.json` gates any Claude started there.
pub fn qass_data_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("qass"))
}

/// Where `qoop qass` writes which program it last started qass on, in qass's folder: also how
/// the interface knows qass ever ran on this machine.
pub const QASS_LAST_AGENT: &str = "last-agent";

/// The owner's switch for qass's watcher, in qass's folder: `off` stops it, `resume <stamp>`
/// is on again, no file is on. Only `qoop` writes it; the watcher keeps the stamp it last
/// started on in [`QASS_WATCH_SEEN`], so a stamp it has not seen says no watcher started since.
pub const QASS_WATCH_FILE: &str = "watch";
/// The `resume` stamp the watcher last started on, written by the watcher alone.
pub const QASS_WATCH_SEEN: &str = "watch-seen";
/// The `resume` stamp qass was told of (`watching on: ...`), written by the daemon alone: a
/// restart does not tell it twice, nor a qass whose agent has no watcher to write
/// [`QASS_WATCH_SEEN`].
pub const QASS_WATCH_TOLD: &str = "watch-told";

/// Whether qass watches the agents: on unless the switch file says `off`.
pub fn qass_watching() -> bool {
    qass_data_dir().is_none_or(|dir| watching_in(&dir))
}

/// Whether qass's watching was turned back on and its watcher has not started since.
pub fn qass_watch_resuming() -> bool {
    qass_data_dir().is_some_and(|dir| resuming_in(&dir))
}

/// The `resume` stamp qass is still to be told of: on again, no watcher started since, and not
/// told yet.
pub fn qass_watch_untold() -> Option<String> {
    qass_data_dir().and_then(|dir| untold_in(&dir))
}

/// Keep `stamp` as told, or with none forget the one kept, so qass is told again.
pub fn set_qass_watch_told(stamp: Option<&str>) -> std::io::Result<()> {
    let dir = qass_data_dir().ok_or_else(|| std::io::Error::other("no home folder"))?;
    told_in(&dir, stamp)
}

/// How long ago a `resume` stamp was written; none when it does not read as one.
pub fn qass_watch_age(stamp: &str) -> Option<std::time::Duration> {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(stamp.parse().ok()?);
    Some(
        std::time::SystemTime::now()
            .duration_since(at)
            .unwrap_or_default(),
    )
}

/// Turn qass's watcher on or off; true when that changed it. On from off writes a fresh
/// `resume` stamp, the watcher's cue that qass sums the fleet up and it starts from now.
pub fn set_qass_watching(on: bool) -> std::io::Result<bool> {
    let dir = qass_data_dir().ok_or_else(|| std::io::Error::other("no home folder"))?;
    set_watching_in(&dir, on)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn watching_in(dir: &Path) -> bool {
    read_trimmed(&dir.join(QASS_WATCH_FILE)).as_deref() != Some("off")
}

/// The stamp of a switch turned on again that no watcher started on yet.
fn resuming_stamp(dir: &Path) -> Option<String> {
    read_trimmed(&dir.join(QASS_WATCH_FILE))
        .and_then(|s| s.strip_prefix("resume ").map(str::to_string))
        .filter(|stamp| read_trimmed(&dir.join(QASS_WATCH_SEEN)).as_ref() != Some(stamp))
}

fn resuming_in(dir: &Path) -> bool {
    resuming_stamp(dir).is_some()
}

fn untold_in(dir: &Path) -> Option<String> {
    resuming_stamp(dir)
        .filter(|stamp| read_trimmed(&dir.join(QASS_WATCH_TOLD)).as_ref() != Some(stamp))
}

fn told_in(dir: &Path, stamp: Option<&str>) -> std::io::Result<()> {
    let path = dir.join(QASS_WATCH_TOLD);
    match stamp {
        Some(stamp) => std::fs::write(path, format!("{stamp}\n")),
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

// ponytail: one lock for the process; the daemon is the one writer while it runs.
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn set_watching_in(dir: &Path, on: bool) -> std::io::Result<bool> {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if on == watching_in(dir) {
        return Ok(false);
    }
    // Made when missing: an `off` said before qass ever ran still holds once it starts.
    std::fs::create_dir_all(dir)?;
    write_switch(dir, on)?;
    note_watch(dir, if on { "switch on" } else { "switch off" });
    Ok(true)
}

/// The switch's file: `off`, or on with a fresh `resume` stamp. Written beside and renamed over:
/// the watcher reading it mid-write must never see an empty file, which reads as on.
fn write_switch(dir: &Path, on: bool) -> std::io::Result<()> {
    let body = if on {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("resume {stamp}\n")
    } else {
        "off\n".to_string()
    };
    let fresh = dir.join(format!(".{QASS_WATCH_FILE}.{}.new", std::process::id()));
    std::fs::write(&fresh, body)?;
    std::fs::rename(&fresh, dir.join(QASS_WATCH_FILE))
}

/// The nightly learning run's folder (skills/learn), beside qass's own: it runs in qass's
/// space and is never qass.
pub fn learn_dir() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("learn"))
}

/// Words qoop typed into a terminal for someone other than the person at it: an agent's or
/// qass's `qoop prompt` and `qoop send`, an agent started with words (`qoop new`, a worktree, a
/// schedule, qass itself), and the inbox's lines. Kept so a reader of the person's own messages
/// (`qoop said`) leaves them out: a transcript cannot tell them from his typing.
pub const TYPED_LOG: &str = "typed.jsonl";
/// The most of one text kept: a message is matched on its start.
const TYPED_KEEP: usize = 2000;
/// The size past which the log keeps only its newer half.
const TYPED_MAX_BYTES: u64 = 2 << 20;

/// Note words typed into `terminal` by `from`. Never fails the typing it records.
pub fn note_typed(terminal: u32, from: &str, text: &str) {
    if let Some(dir) = data_dir() {
        let _ = note_typed_in(&dir, terminal, from, text, now_millis());
    }
}

/// The words qoop typed, read under the log's lock: never a log caught halfway through being cut.
pub fn read_typed() -> String {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    let Some(Ok(mut file)) = data_dir().map(|dir| std::fs::File::open(dir.join(TYPED_LOG))) else {
        return String::new();
    };
    // SAFETY: the descriptor is open for the whole call; the lock goes with it when it closes.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
    let mut all = String::new();
    let _ = file.read_to_string(&mut all);
    all
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn note_typed_in(
    dir: &Path,
    terminal: u32,
    from: &str,
    text: &str,
    at: u64,
) -> std::io::Result<()> {
    use std::io::Write;
    let text: String = text
        .trim_end_matches(['\r', '\n'])
        .chars()
        .take(TYPED_KEEP)
        .collect();
    if text.trim().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    use std::io::{Read, Seek};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    // Only the person reads it, as their scrollback beside it.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(dir.join(TYPED_LOG))?;
    // One writer at a time across processes: a log cut to its newer half while another adds a
    // line would lose that line, and the words it names would read as the person's.
    // SAFETY: the descriptor is open for the whole call; the lock goes with it when it closes.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if file.metadata()?.len() > TYPED_MAX_BYTES {
        let mut all = String::new();
        file.seek(std::io::SeekFrom::Start(0))?;
        file.read_to_string(&mut all)?;
        let lines: Vec<&str> = all.lines().collect();
        let keep = format!("{}\n", lines[lines.len() / 2..].join("\n"));
        file.set_len(0)?;
        file.write_all(keep.as_bytes())?;
    }
    let line = serde_json::json!({ "at": at, "terminal": terminal, "from": from, "text": text });
    file.write_all(format!("{line}\n").as_bytes())
}

/// The pid qass's watcher keeps while it runs (watch.sh writes it and leaves it behind when it
/// ends or is killed).
pub const QASS_WATCH_PID: &str = "watch.pid";

/// Whether qass's watcher runs: the pid it kept is alive and is the watcher (`watch.sh` in its
/// command line), the check watch.sh makes of a watcher before it.
pub fn qass_watcher_alive() -> bool {
    qass_data_dir().is_some_and(|dir| watcher_alive_in(&dir))
}

fn watcher_alive_in(dir: &Path) -> bool {
    let Some(pid) = read_trimmed(&dir.join(QASS_WATCH_PID))
        .and_then(|s| s.parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    // SAFETY: signal 0 sends nothing; it only says whether the pid is there.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("watch.sh"))
}

/// The switch on again with a fresh stamp, as `off` then `on` would leave it, for a watcher that
/// died after it started on the last one: qass is told again and the watcher, started, marks
/// the new stamp seen. Nothing while the switch is off; true when renewed.
pub fn renew_qass_watch() -> std::io::Result<bool> {
    let dir = qass_data_dir().ok_or_else(|| std::io::Error::other("no home folder"))?;
    renew_in(&dir)
}

fn renew_in(dir: &Path) -> std::io::Result<bool> {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    if !watching_in(dir) {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    write_switch(dir, true)?;
    note_watch(dir, "watcher gone, told again");
    Ok(true)
}

/// One line in qass's `watch.log` for a turn of the switch, beside the watcher's own starts and
/// ends: the day it went dark has its time on record. A failure to note it changes nothing.
fn note_watch(dir: &Path, what: &str) {
    use std::io::Write;
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("watch.log"))
        .and_then(|mut f| writeln!(f, "{at} {what}"));
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
        crate::legacy::var_os(RUNTIME_DIR_ENV).is_some(),
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
    dir.join(crate::legacy::STATE_LOCK)
}

/// Where `qoop update --check` leaves the newest version it saw in the releases.
///
/// A plain file holding a version like `0.4.0`, so the interface can show it without
/// touching the network: the TUI never fetches, it only reads this file. Built without
/// creating or verifying anything, because reading a notice must not create state.
pub fn update_notice_path() -> PathBuf {
    let base = match crate::legacy::var(RUNTIME_DIR_ENV) {
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

/// The file the phone bridge keeps beside its doorbell: one line, `connected <unix>` or
/// `disconnected <unix>`, rewritten on every change and again on every heartbeat while the
/// bridge runs. A machine with no bridge has none.
pub const HUB_STATE_FILE: &str = "hub-state";

/// How old the line may be before the bridge that wrote it is taken as gone: three of its
/// thirty second heartbeats.
pub const HUB_STATE_FRESH_SECS: i64 = 90;

/// Where the interface reads the bridge's hub state. Built without creating or verifying
/// anything, like [`update_notice_path`].
pub fn hub_state_path() -> PathBuf {
    let base = match crate::legacy::var(RUNTIME_DIR_ENV) {
        Ok(explicit) => PathBuf::from(explicit),
        Err(_) => default_runtime_base(),
    };
    base.join(HUB_STATE_FILE)
}

/// Whether this machine reaches the hub, as the bridge last said.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HubLink {
    /// No state file: no bridge here, nothing paired or linked.
    #[default]
    Absent,
    /// The bridge is connected to the hub.
    Up,
    /// The bridge says it is not connected, or has said nothing for too long, or its file
    /// cannot be read: paired, but the hub is not reached.
    Down,
}

/// The hub state a file's text means at `now_unix`: stale counts as down.
pub fn hub_link_from(text: Option<&str>, now_unix: i64) -> HubLink {
    let Some(text) = text else {
        return HubLink::Absent;
    };
    let mut words = text.split_whitespace();
    let (state, at) = (
        words.next(),
        words.next().and_then(|n| n.parse::<i64>().ok()),
    );
    match (state, at) {
        (Some("connected"), Some(at)) if (now_unix - at).abs() <= HUB_STATE_FRESH_SECS => {
            HubLink::Up
        }
        _ => HubLink::Down,
    }
}

/// The hub state as the file says now.
pub fn read_hub_link() -> HubLink {
    let text = match std::fs::read_to_string(hub_state_path()) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => Some(String::new()),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    hub_link_from(text.as_deref(), now)
}

/// Why the hub is not reached, as the state file says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubDown {
    /// The bridge runs and says it lost the hub: it keeps trying on its own.
    Unreached,
    /// The bridge went quiet: its last line is a stale `connected`, or it cannot be read.
    BridgeQuiet,
}

/// The reason a down hub state line gives. The bridge renews a `disconnected` line on every
/// retry, so its stamp says when it last tried, never since when the hub is lost: no time is
/// read from it.
pub fn hub_down_from(text: Option<&str>) -> HubDown {
    match text.and_then(|t| t.split_whitespace().next()) {
        Some("disconnected") => HubDown::Unreached,
        _ => HubDown::BridgeQuiet,
    }
}

/// Why the hub is not reached, as the file says now.
pub fn read_hub_down() -> HubDown {
    hub_down_from(std::fs::read_to_string(hub_state_path()).ok().as_deref())
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

/// The login name of the user this process runs as, from the password database (what `id -un`
/// prints) and not from `$USER`, which any parent may set to anything.
pub fn current_user_name() -> Option<String> {
    from_passwd(|entry| entry.pw_name)
}

/// The login shell of the user this process runs as, from the password database: what a tab
/// starts when `$SHELL` is not set (a `docker exec`, some cron and systemd contexts).
pub fn current_user_shell() -> Option<String> {
    from_passwd(|entry| entry.pw_shell)
}

/// One string field of this user's password database entry, `None` when it is missing or empty.
fn from_passwd(field: fn(&libc::passwd) -> *mut libc::c_char) -> Option<String> {
    // SAFETY: `passwd` is plain data (integers and pointers) for which all zeroes is valid.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to a live local; `buf` is the length passed, and `found` is
    // read only after a zero return, when it points at `entry` or is null.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() {
        return None;
    }
    let value = field(&entry);
    if value.is_null() {
        return None;
    }
    // SAFETY: on success a non-null field is a NUL terminated string inside `buf`, which is
    // alive.
    let value = unsafe { std::ffi::CStr::from_ptr(value) }.to_str().ok()?;
    (!value.is_empty()).then(|| value.to_string())
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

    /// A socket path that cannot be bound is refused by name, with its length and the fix,
    /// and one that fits passes.
    #[test]
    fn a_socket_path_too_long_to_bind_says_its_length_and_the_fix() {
        let fix = "set TMPDIR to a shorter folder";
        assert!(
            fits_socket(
                Path::new("/tmp/qoop-501/sessions/default/qoopd.sock"),
                103,
                fix
            )
            .is_ok()
        );
        let long = PathBuf::from(format!("/{}/qoopd.sock", "x".repeat(92)));
        assert_eq!(long.as_os_str().len(), 104);
        let said = fits_socket(&long, 103, fix).unwrap_err().to_string();
        assert!(said.contains(&long.display().to_string()), "{said}");
        assert!(
            said.contains("is 104 bytes") && said.contains("at most 103"),
            "{said}"
        );
        assert!(said.ends_with(fix), "{said}");
        assert!(fits_socket(&long, 104, fix).is_ok());
    }

    #[test]
    fn the_user_name_is_the_one_id_prints() {
        let Ok(out) = std::process::Command::new("id").arg("-un").output() else {
            return;
        };
        if out.status.success() {
            assert_eq!(
                current_user_name().as_deref(),
                Some(String::from_utf8_lossy(&out.stdout).trim())
            );
        }
    }

    /// Held by every test that sets or reads this module's environment variables: the
    /// test harness runs them on parallel threads, and one test's `remove_var` landing
    /// between another's `set_var` and its read failed it at random.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// What qoop typed is kept a line each, cut, and an empty one not at all; past its size the
    /// log keeps its newer half.
    #[test]
    fn typed_words_are_kept_a_line_each_and_the_log_stays_small() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path();
        note_typed_in(dir, 7, "T466", "do the thing\r", 1000).unwrap();
        note_typed_in(dir, 7, "T466", " \r", 1001).unwrap();
        note_typed_in(dir, 8, "daemon", &"x".repeat(5000), 1002).unwrap();
        let all = std::fs::read_to_string(dir.join(TYPED_LOG)).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(TYPED_LOG))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let rows: Vec<serde_json::Value> = all
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["text"], "do the thing");
        assert_eq!(rows[0]["from"], "T466");
        assert_eq!(rows[1]["text"].as_str().unwrap().len(), TYPED_KEEP);
        let big = "y".repeat(1000);
        for n in 0..2200 {
            note_typed_in(dir, 1, "x", &big, n).unwrap();
        }
        assert!(std::fs::metadata(dir.join(TYPED_LOG)).unwrap().len() <= TYPED_MAX_BYTES + 2048);
    }

    /// A watcher runs only when the pid it kept is alive and is `watch.sh`: no file, a pid
    /// gone, and a live process that is something else all read as none.
    #[test]
    fn a_watcher_runs_only_while_its_pid_is_a_live_watch_sh() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path();
        assert!(!watcher_alive_in(dir));
        // This test's own process is alive and is no watcher.
        std::fs::write(
            dir.join(QASS_WATCH_PID),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        assert!(!watcher_alive_in(dir));
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        gone.wait().unwrap();
        std::fs::write(dir.join(QASS_WATCH_PID), format!("{}\n", gone.id())).unwrap();
        assert!(!watcher_alive_in(dir));
        let script = dir.join("watch.sh");
        std::fs::write(&script, "sleep 30\n").unwrap();
        let mut watcher = std::process::Command::new("sh")
            .arg(&script)
            .spawn()
            .unwrap();
        std::fs::write(dir.join(QASS_WATCH_PID), format!("{}\n", watcher.id())).unwrap();
        assert!(watcher_alive_in(dir));
        watcher.kill().unwrap();
        watcher.wait().unwrap();
        assert!(!watcher_alive_in(dir));
    }

    /// Renewing gives the switch a stamp no watcher has seen, so qass is told again; nothing
    /// while it is off.
    #[test]
    fn a_renewed_switch_is_a_stamp_no_watcher_has_seen() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("qass");
        assert!(set_watching_in(&dir, false).unwrap());
        assert!(!renew_in(&dir).unwrap());
        assert!(!watching_in(&dir));
        assert!(set_watching_in(&dir, true).unwrap());
        let first = resuming_stamp(&dir).unwrap();
        // The watcher started on it, and later died.
        std::fs::write(dir.join(QASS_WATCH_SEEN), format!("{first}\n")).unwrap();
        std::fs::write(dir.join(QASS_WATCH_TOLD), format!("{first}\n")).unwrap();
        assert!(untold_in(&dir).is_none());
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(renew_in(&dir).unwrap());
        let again = untold_in(&dir).unwrap();
        assert_ne!(again, first);
        assert!(watching_in(&dir));
        let log = std::fs::read_to_string(dir.join("watch.log")).unwrap();
        assert!(
            log.lines()
                .last()
                .unwrap()
                .ends_with("watcher gone, told again")
        );
    }

    /// No file is on; off is said once; on from off is a fresh stamp the watcher has not
    /// seen, and on again (or an on with nothing off) changes nothing. The stamp seen is on.
    #[test]
    fn the_qass_watch_switch_reads_and_turns() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("qass");
        assert!(watching_in(&dir) && !resuming_in(&dir));
        assert!(!set_watching_in(&dir, true).unwrap());
        assert!(!dir.join(QASS_WATCH_FILE).exists());
        assert!(set_watching_in(&dir, false).unwrap());
        assert!(!watching_in(&dir) && !resuming_in(&dir));
        assert!(!set_watching_in(&dir, false).unwrap());
        assert!(set_watching_in(&dir, true).unwrap());
        assert!(watching_in(&dir) && resuming_in(&dir));
        assert!(!set_watching_in(&dir, true).unwrap());
        // Each real turn, and only those, is on record with its time.
        let log = std::fs::read_to_string(dir.join("watch.log")).unwrap();
        let turns: Vec<&str> = log
            .lines()
            .filter_map(|l| l.split_once(' ').map(|x| x.1))
            .collect();
        assert_eq!(turns, ["switch off", "switch on"], "{log}");
        let stamp = read_trimmed(&dir.join(QASS_WATCH_FILE)).unwrap();
        std::fs::write(
            dir.join(QASS_WATCH_SEEN),
            stamp.strip_prefix("resume ").unwrap(),
        )
        .unwrap();
        assert!(watching_in(&dir) && !resuming_in(&dir));
    }

    /// On again is untold until the daemon keeps its stamp as told; forgotten, or on again
    /// after an off, it is untold again; a watcher started on it leaves nothing to tell.
    #[test]
    fn the_qass_watch_line_is_told_once_per_stamp() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("qass");
        assert_eq!(untold_in(&dir), None);
        set_watching_in(&dir, false).unwrap();
        assert_eq!(untold_in(&dir), None);
        set_watching_in(&dir, true).unwrap();
        let stamp = untold_in(&dir).expect("untold");
        assert!(qass_watch_age(&stamp).is_some_and(|age| age.as_secs() < 60));
        told_in(&dir, Some(&stamp)).unwrap();
        assert_eq!(untold_in(&dir), None);
        assert!(resuming_in(&dir));
        told_in(&dir, None).unwrap();
        told_in(&dir, None).unwrap();
        assert_eq!(untold_in(&dir).as_deref(), Some(stamp.as_str()));
        told_in(&dir, Some(&stamp)).unwrap();
        set_watching_in(&dir, false).unwrap();
        set_watching_in(&dir, true).unwrap();
        let again = untold_in(&dir).expect("untold after off and on");
        assert_ne!(again, stamp);
        std::fs::write(dir.join(QASS_WATCH_SEEN), &again).unwrap();
        assert_eq!(untold_in(&dir), None);
        assert_eq!(qass_watch_age("not a stamp"), None);
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 3600);
        let old = old
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        assert!(qass_watch_age(&old).is_some_and(|age| age.as_secs() >= 7 * 3600 - 5));
    }

    /// The bridge's state line: no file is no bridge, a fresh `connected` is up, and anything
    /// else (disconnected, stale, from the future, garbled) is a bridge that does not reach
    /// the hub.
    #[test]
    fn the_hub_state_line_reads_as_up_down_or_absent() {
        let now = 1_750_000_000;
        assert_eq!(hub_link_from(None, now), HubLink::Absent);
        assert_eq!(
            hub_link_from(Some("connected 1750000000\n"), now),
            HubLink::Up
        );
        assert_eq!(
            hub_link_from(Some("connected 1749999950"), now),
            HubLink::Up
        );
        assert_eq!(
            hub_link_from(Some("disconnected 1750000000"), now),
            HubLink::Down
        );
        assert_eq!(
            hub_link_from(Some("connected 1749999900"), now),
            HubLink::Down,
            "stale"
        );
        assert_eq!(
            hub_link_from(Some("connected 1750000500"), now),
            HubLink::Down,
            "future"
        );
        assert_eq!(hub_link_from(Some("connected"), now), HubLink::Down);
        assert_eq!(hub_link_from(Some(""), now), HubLink::Down);
    }

    #[test]
    fn a_down_hub_says_whether_the_bridge_still_tries() {
        assert_eq!(
            hub_down_from(Some("disconnected 1750000000\n")),
            HubDown::Unreached
        );
        assert_eq!(
            hub_down_from(Some("connected 1750000000")),
            HubDown::BridgeQuiet
        );
        assert_eq!(hub_down_from(Some("")), HubDown::BridgeQuiet);
        assert_eq!(hub_down_from(None), HubDown::BridgeQuiet);
    }

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

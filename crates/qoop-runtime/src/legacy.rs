//! What exists only because every install before the rename was called qoro.
//!
//! A machine moves from `qoro` to `qoop` while its daemon runs, with agents inside it, so
//! nothing here may stop anything: the old names keep reaching the new things until the
//! people and programs using them have restarted.
//!
//! ponytail: delete everything here but [`db_path`] and [`STATE_LOCK`], and every other
//! `legacy::` call, once no machine runs a qoro 0.7.x daemon or has an agent that was started
//! before the rename (a release or two after it shipped). Those two names stay for good:
//! renaming the file a machine's database is in, or the lock a 0.7.x daemon takes, would
//! strand a rollback and the exclusion between old and new daemons.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::paths;

const OLD: &str = "qoro";
const NEW: &str = "qoop";

/// The socket a 0.7.x daemon listened on, beside this build's `qoopd.sock`.
const OLD_SOCKET: &str = "qorod.sock";

/// The database file a 0.7.x daemon opened.
const OLD_DB: &str = "qoro.db";

/// Kept: the lock on a state directory. A 0.7.x daemon holds it as `qorod.lock` in the
/// state directory this build now shares, and that is the only thing that keeps the two
/// from running on one database; a new name would be a lock nobody else takes.
pub const STATE_LOCK: &str = "qorod.lock";

fn old_name(name: &str) -> String {
    name.replacen("QOOP_", "QORO_", 1)
}

/// `QOOP_<name>`, else the `QORO_<name>` a shell, a service or a tab that predates the
/// rename still sets. Every environment variable of this program is read through here.
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name).or_else(|_| std::env::var(old_name(name)))
}

/// [`var`] for a value that need not be UTF-8.
pub fn var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| std::env::var_os(old_name(name)))
}

/// Set, by whoever starts this binary from a test or a bench, to say its home is not the
/// person's.
pub const SANDBOX_ENV: &str = "QOOP_TEST_SANDBOX";

/// Whether the steps that touch the old names on real paths may run: the links, the
/// directories, the hooks, the skills, the login item.
///
/// Not from a binary that lies under a cargo `target` directory (a test, a build run by hand)
/// unless it was started with [`SANDBOX_ENV`]: such a binary run with the person's own home
/// would move their `qoro` directories, and twice already it did. The rest of the command
/// runs as it always does.
pub fn migrating() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let exe = std::env::current_exe().ok();
        !skips_migration(exe.as_deref(), std::env::var_os(SANDBOX_ENV).is_some())
    })
}

fn skips_migration(exe: Option<&Path>, sandboxed: bool) -> bool {
    !sandboxed && exe.is_some_and(|exe| exe.components().any(|part| part.as_os_str() == "target"))
}

/// Move the person's directories to the new name, once: the configuration, the data and
/// the logs. The old name stays behind as a link, because a running daemon, a hook already
/// loaded into an agent and a launchd service all still spell the old paths.
///
/// Called first thing by every command. A directory moves by `rename`, which leaves the
/// files open in a running process open, and only when the new name is free.
pub fn move_home() {
    if !migrating() {
        eprintln!(
            "qoop: a build under a target directory without {SANDBOX_ENV} does not move the \
             qoro directories of this home"
        );
        return;
    }
    let logs = paths::logs_dir().map(|dir| {
        // Linux keeps them in `<state>/qoop/log`; it is `<state>/qoop` that is named.
        if cfg!(target_os = "macos") {
            dir
        } else {
            dir.parent().map_or(dir.clone(), Path::to_path_buf)
        }
    });
    for new in [Some(paths::config_dir()), paths::data_dir(), logs]
        .into_iter()
        .flatten()
    {
        let _ = move_dir(&new);
    }
}

/// Left inside a directory just before it is renamed, so a start that died between the
/// rename and the link is found and finished by the next command.
const MOVED_MARK: &str = ".moved-from-qoro";

/// Left in the new directory when an old one is there beside it, so that is said once.
const BOTH_NOTE: &str = ".qoro-left-alone";

/// `<parent>/qoro` to `<parent>/qoop`, leaving a link at the old name. A link at the old
/// name (a dotfiles checkout) is followed by a link at the new one, not copied.
fn move_dir(new: &Path) -> io::Result<()> {
    let old = new.with_file_name(OLD);
    if new.file_name().is_none_or(|name| name != NEW) {
        return Ok(());
    }
    let real_dir = |meta: &std::fs::Metadata| meta.is_dir() && !meta.file_type().is_symlink();
    match (new.symlink_metadata(), old.symlink_metadata()) {
        (Err(_), Err(_)) => Ok(()),
        // The new name is there and the old is not: a link gone missing is put back only when
        // this build's own move was cut short (the mark is inside), never on a fresh install.
        (Ok(now), Err(_)) => {
            if real_dir(&now) && new.join(MOVED_MARK).exists() {
                symlink(new, &old)?;
            }
            Ok(())
        }
        // Both are real directories: the new one is used and the old one is left as it is,
        // said once.
        (Ok(now), Ok(was)) => {
            if real_dir(&now) && real_dir(&was) && !new.join(BOTH_NOTE).exists() {
                eprintln!(
                    "qoop: {} and {} both exist; {} is used and {} is left as it is. Move what \
                     you need from the old one by hand.",
                    old.display(),
                    new.display(),
                    new.display(),
                    old.display()
                );
                let _ = std::fs::write(new.join(BOTH_NOTE), "");
            }
            Ok(())
        }
        (Err(_), Ok(was)) => {
            if was.file_type().is_symlink() {
                return symlink(&old, new);
            }
            if !was.is_dir() {
                return Ok(());
            }
            std::fs::write(old.join(MOVED_MARK), "")?;
            std::fs::rename(&old, new)?;
            symlink(new, &old)
        }
    }
}

/// The database file in `state`: this build's name for a new one, and the name it already
/// has for one a 0.7.x daemon made. Never renamed: the old daemon opens it by that name, and
/// a machine put back on the old build (a rollback) must find its tabs where it left them.
///
/// A file under the new name with no schema is not a database: a stray `sqlite3 <path>`
/// leaves one behind, and taking it would hide every space the old file holds, from the next
/// daemon too.
pub fn db_path(state: &Path) -> PathBuf {
    let new = state.join("qoop.db");
    let old = state.join(OLD_DB);
    if old.exists() && !qoop_store::Store::holds_schema(&new) {
        old
    } else {
        new
    }
}

/// The session directory a 0.7.x daemon used where this build uses `dir`: the same one
/// when the runtime directory was named by the environment, else the one under `qoro`.
fn old_session_dir(dir: &Path) -> Option<PathBuf> {
    let base = dir.parent()?.parent()?;
    let name = base.file_name()?.to_str()?;
    let old = name.replacen(NEW, OLD, 1);
    Some(
        base.with_file_name(old)
            .join("sessions")
            .join(dir.file_name()?),
    )
}

/// Whether `dir` and its `parents` ancestors pass the checks the runtime directory does
/// (ADR-012): ours, private, not a link. An old runtime directory that does not is not
/// connected to or linked: on a machine without `XDG_RUNTIME_DIR` it is a name in a shared
/// temp directory that another user could have made first.
fn trusted_chain(dir: &Path, parents: usize) -> bool {
    dir.ancestors()
        .take(parents + 1)
        .all(|d| paths::verify_private_dir(d).is_ok())
}

/// A command of this build, before any daemon of it is up: when a 0.7.x daemon is
/// listening, make this build's socket path reach it. The daemon moves onto this build
/// when it is next idle, and is asked to by a command that finds it here.
pub fn reach_old_daemon(dir: &Path) {
    let new = paths::socket_path(dir);
    if new.symlink_metadata().is_ok() {
        return;
    }
    let Some(old_dir) = old_session_dir(dir).filter(|d| trusted_chain(d, 2)) else {
        return;
    };
    let old = old_dir.join(OLD_SOCKET);
    if UnixStream::connect(&old).is_ok() {
        let _ = symlink(&old, &new);
    }
}

/// The same for the phone bridge's doorbell, which lives beside the sessions.
pub fn reach_old_bridge(base: &Path) {
    let new = paths::bridge_socket(base);
    if new.symlink_metadata().is_ok() {
        return;
    }
    let old = base.with_file_name(
        base.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().replacen(NEW, OLD, 1)),
    );
    if old == base || !trusted_chain(&old, 0) {
        return;
    }
    let old = old.join("bridge.sock");
    if old != new && UnixStream::connect(&old).is_ok() {
        let _ = symlink(&old, &new);
    }
}

/// A daemon of this build has bound `socket`. Whatever is still running from before the
/// rename (the phone bridge, an interface left open) looks for a daemon at the old path:
/// point it here. Nothing is made when there never was an old install.
pub fn point_old_path_at(socket: &Path) {
    let Some(dir) = socket.parent() else { return };
    let Some(old_dir) = old_session_dir(dir).filter(|d| !d.exists() || trusted_chain(d, 2)) else {
        return;
    };
    let old = old_dir.join(OLD_SOCKET);
    let present = if old_dir == dir {
        old.symlink_metadata().is_ok()
    } else {
        old_dir.is_dir()
    };
    // A live daemon there is not ours to replace.
    if !present || UnixStream::connect(&old).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(&old);
    let _ = symlink(socket, &old);
}

/// Called by a binary that has just started under the old name: the old updater, or a
/// hand copy, wrote this build over `.../qoro`. Make that file `qoop` and `qoro` a link to
/// it, so the old name keeps working (a 0.7.x daemon moves onto the path it was started
/// from, and hooks and scripts of the fleet say `qoro`) while the new one exists.
pub fn settle_binary_name() {
    if !migrating() {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = settle(&exe);
    }
}

fn settle(exe: &Path) -> io::Result<()> {
    let plain = exe
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_file());
    if !plain || exe.file_name().is_none_or(|name| name != OLD) {
        return Ok(());
    }
    let new = exe.with_file_name(NEW);
    if new.symlink_metadata().is_ok() {
        return Ok(());
    }
    // A second name for the same file first, then the old name replaced by a link in one
    // step: there is no moment where `qoro` does not exist.
    std::fs::hard_link(exe, &new)?;
    keep_old_name(&new)
}

/// Leave `qoro` beside the binary `exe` (named `qoop`) as a link to it, for everything that
/// still says the old name: an install, an update and a remote swap all end here or do the
/// same in shell. A file already named `qoro` there (the binary of before) is replaced, in
/// one step, so a daemon that will exec its old path finds this build.
pub fn keep_old_name(exe: &Path) -> io::Result<()> {
    if exe.file_name().is_none_or(|name| name != NEW) {
        return Ok(());
    }
    let old = exe.with_file_name(OLD);
    if old
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Ok(());
    }
    let link = exe.with_file_name(format!(".qoro-link.{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    symlink(NEW, &link)?;
    std::fs::rename(&link, old).inspect_err(|_| {
        let _ = std::fs::remove_file(&link);
    })
}

/// The plist of the launchd agent that went by `label` before the rename, when it is there.
/// macOS only.
pub fn old_launchd_plist(label: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") || !migrating() {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(
        home.join("Library/LaunchAgents")
            .join(format!("{label}.plist")),
    )
    .filter(|p| p.exists())
}

/// Take down the launchd agent that went by `label` before the rename and delete its plist,
/// once the caller has put the agent back under its new label and it loaded. Answers whether
/// there was one. Nothing happens off macOS.
///
/// Both labels would otherwise run side by side, each with its own copy of the program.
pub fn retire_launchd_agent(label: &str) -> bool {
    let Some(plist) = old_launchd_plist(label) else {
        return false;
    };
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{label}", paths::current_uid())])
        .output();
    std::fs::remove_file(plist).is_ok()
}

/// The lock `ensure-awake` takes in session directory `dir`. A 0.7.x `ensure-awake` takes it
/// in the old runtime directory, so while that exists both builds take that one and exclude
/// each other.
pub fn awake_lock(dir: &Path) -> PathBuf {
    match old_session_dir(dir) {
        Some(old) if old.is_dir() && trusted_chain(&old, 2) => old.join("awake.lock"),
        _ => dir.join("awake.lock"),
    }
}

/// What `--version` says: `qoro` when the name this binary was started by contains `qoro`,
/// else `qoop`. A 0.7.x daemon asks the file it will move onto, by the path it started from,
/// for `qoro <version>`; a 0.7.x updater asks the copy it staged, which it names
/// `.qoro-install.XXXXXX` on a remote, `qoro` on this Mac; and a kept build is `qoro.pre-*`.
/// None of them move onto anything that answers otherwise.
pub fn version_line() -> String {
    let named = std::env::args_os().next().and_then(|arg| {
        Path::new(&arg)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    });
    format!(
        "{} {}",
        version_name(named.as_deref()),
        env!("CARGO_PKG_VERSION")
    )
}

fn version_name(started_as: Option<&str>) -> &'static str {
    if started_as.is_some_and(|name| name.contains(OLD)) {
        OLD
    } else {
        NEW
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// `dir` and every directory between it and `root` made private, as the runtime's are.
    fn private_tree(root: &Path, dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        for d in dir.ancestors().take_while(|d| *d != root) {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn an_old_directory_moves_and_leaves_a_link() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join("qoro");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("config.toml"), "x = 1").unwrap();
        let new = home.path().join("qoop");
        move_dir(&new).unwrap();
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "x = 1"
        );
        assert!(old.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(old.join("config.toml")).unwrap(),
            "x = 1"
        );
        // Again: nothing to do, nothing disturbed.
        move_dir(&new).unwrap();
        assert!(new.join("config.toml").exists());
    }

    #[test]
    fn a_new_directory_that_exists_is_not_replaced() {
        let home = tempfile::tempdir().unwrap();
        for name in ["qoro", "qoop"] {
            std::fs::create_dir(home.path().join(name)).unwrap();
        }
        std::fs::write(home.path().join("qoro/old"), "").unwrap();
        move_dir(&home.path().join("qoop")).unwrap();
        assert!(home.path().join("qoro/old").exists());
        assert!(!home.path().join("qoop/old").exists());
    }

    #[test]
    fn a_linked_old_directory_is_followed_not_copied() {
        let home = tempfile::tempdir().unwrap();
        let dotfiles = home.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::write(dotfiles.join("config.toml"), "y = 2").unwrap();
        symlink(&dotfiles, home.path().join("qoro")).unwrap();
        move_dir(&home.path().join("qoop")).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.path().join("qoop/config.toml")).unwrap(),
            "y = 2"
        );
    }

    #[test]
    fn an_existing_database_keeps_its_name() {
        let state = tempfile::tempdir().unwrap();
        assert_eq!(db_path(state.path()), state.path().join("qoop.db"));
        std::fs::write(state.path().join("qoro.db"), "old").unwrap();
        assert_eq!(db_path(state.path()), state.path().join("qoro.db"));
        std::fs::write(state.path().join("qoop.db"), "").unwrap();
        assert_eq!(
            db_path(state.path()),
            state.path().join("qoro.db"),
            "a file with no schema under the new name hides nothing"
        );
        std::fs::remove_file(state.path().join("qoop.db")).unwrap();
        drop(qoop_store::Store::open(&state.path().join("qoop.db")).unwrap());
        assert_eq!(db_path(state.path()), state.path().join("qoop.db"));
    }

    /// A 0.7.x daemon is listening at `<base>/qoro-1/sessions/default/qorod.sock`: this
    /// build's path reaches it, and once this build's daemon is up the old path reaches that.
    #[test]
    fn the_two_socket_paths_reach_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join("qoro-1/sessions/default");
        let new_dir = tmp.path().join("qoop-1/sessions/default");
        private_tree(tmp.path(), &old_dir);
        private_tree(tmp.path(), &new_dir);
        let old_listener = UnixListener::bind(old_dir.join("qorod.sock")).unwrap();

        reach_old_daemon(&new_dir);
        assert!(UnixStream::connect(paths::socket_path(&new_dir)).is_ok());

        // The old daemon is gone and this build's takes its place.
        drop(old_listener);
        std::fs::remove_file(paths::socket_path(&new_dir)).unwrap();
        let listener = UnixListener::bind(paths::socket_path(&new_dir)).unwrap();
        point_old_path_at(&paths::socket_path(&new_dir));
        assert!(UnixStream::connect(old_dir.join("qorod.sock")).is_ok());
        drop(listener);
    }

    #[test]
    fn nothing_is_made_when_there_never_was_an_old_install() {
        let tmp = tempfile::tempdir().unwrap();
        let new_dir = tmp.path().join("qoop-1/sessions/default");
        private_tree(tmp.path(), &new_dir);
        let _listener = UnixListener::bind(paths::socket_path(&new_dir)).unwrap();
        point_old_path_at(&paths::socket_path(&new_dir));
        reach_old_daemon(&new_dir);
        assert!(!tmp.path().join("qoro-1").exists());
    }

    /// A runtime directory named by the environment is the same one for both names.
    #[test]
    fn an_explicit_directory_holds_both_socket_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("rt/sessions/default");
        private_tree(tmp.path(), &dir);
        let _old = UnixListener::bind(dir.join("qorod.sock")).unwrap();
        reach_old_daemon(&dir);
        assert!(UnixStream::connect(paths::socket_path(&dir)).is_ok());
    }

    #[test]
    fn the_old_name_becomes_a_link_to_the_new_one() {
        let bin = tempfile::tempdir().unwrap();
        let exe = bin.path().join("qoro");
        std::fs::write(&exe, "binary").unwrap();
        settle(&exe).unwrap();
        assert_eq!(
            std::fs::read_to_string(bin.path().join("qoop")).unwrap(),
            "binary"
        );
        assert!(exe.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "binary");
        // Settled once: a link is left alone, and so is a build that is already qoop.
        settle(&exe).unwrap();
        settle(&bin.path().join("qoop")).unwrap();
        assert!(
            bin.path()
                .join("qoop")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_file()
        );
    }

    #[test]
    fn a_variable_falls_back_to_its_old_name() {
        assert_eq!(old_name("QOOP_TOKEN"), "QORO_TOKEN");
        // SAFETY: a name nothing else in the test run reads.
        unsafe { std::env::set_var("QORO_LEGACY_TEST_ONLY", "old") };
        assert_eq!(var("QOOP_LEGACY_TEST_ONLY").as_deref(), Ok("old"));
        // SAFETY: as above.
        unsafe { std::env::set_var("QOOP_LEGACY_TEST_ONLY", "new") };
        assert_eq!(var("QOOP_LEGACY_TEST_ONLY").as_deref(), Ok("new"));
    }

    #[test]
    fn the_version_name_follows_any_name_with_qoro_in_it() {
        for old in [
            "qoro",
            ".qoro-install.Ab12Cd",
            "qoro.pre-0.7.0",
            "/x/bin/qoro",
        ] {
            assert_eq!(version_name(Some(old)), "qoro", "{old}");
        }
        for new in ["qoop", ".qoop-install.Ab12Cd", "qoop-new", "anything"] {
            assert_eq!(version_name(Some(new)), "qoop", "{new}");
        }
        assert_eq!(version_name(None), "qoop");
    }

    #[test]
    fn a_binary_under_target_skips_the_migration_unless_sandboxed() {
        let built = Path::new("/w/target/debug/qoop");
        let installed = Path::new("/Users/a/.local/bin/qoop");
        assert!(skips_migration(Some(built), false));
        assert!(!skips_migration(Some(built), true));
        assert!(!skips_migration(Some(installed), false));
        assert!(!skips_migration(None, false));
    }

    #[test]
    fn a_move_cut_short_is_finished_and_a_fresh_install_gets_no_old_name() {
        let home = tempfile::tempdir().unwrap();
        let (old, new) = (home.path().join("qoro"), home.path().join("qoop"));
        std::fs::create_dir(&old).unwrap();
        move_dir(&new).unwrap();
        // Cut after the rename: the link is gone, the mark is in the directory.
        std::fs::remove_file(&old).unwrap();
        assert!(old.symlink_metadata().is_err());
        move_dir(&new).unwrap();
        assert!(old.symlink_metadata().unwrap().file_type().is_symlink());

        let fresh = tempfile::tempdir().unwrap();
        std::fs::create_dir(fresh.path().join("qoop")).unwrap();
        move_dir(&fresh.path().join("qoop")).unwrap();
        assert!(fresh.path().join("qoro").symlink_metadata().is_err());
    }

    #[test]
    fn two_real_directories_are_left_alone_and_said_once() {
        let home = tempfile::tempdir().unwrap();
        for name in ["qoro", "qoop"] {
            std::fs::create_dir(home.path().join(name)).unwrap();
        }
        let new = home.path().join("qoop");
        move_dir(&new).unwrap();
        assert!(new.join(BOTH_NOTE).exists(), "said, and remembered");
        assert!(
            home.path()
                .join("qoro")
                .symlink_metadata()
                .unwrap()
                .is_dir()
        );
    }

    /// An old runtime directory another user could have made (not private) is neither
    /// connected to nor linked.
    #[test]
    fn an_old_runtime_directory_that_is_not_private_is_not_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join("qoro-1/sessions/default");
        let new_dir = tmp.path().join("qoop-1/sessions/default");
        private_tree(tmp.path(), &old_dir);
        private_tree(tmp.path(), &new_dir);
        let _listener = UnixListener::bind(old_dir.join("qorod.sock")).unwrap();
        std::fs::set_permissions(
            tmp.path().join("qoro-1"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        reach_old_daemon(&new_dir);
        assert!(paths::socket_path(&new_dir).symlink_metadata().is_err());
    }

    /// `ensure-awake` of both builds take the same lock while the old runtime directory exists.
    #[test]
    fn both_builds_take_the_awake_lock_where_the_old_one_looks() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join("qoro-1/sessions/default");
        let new_dir = tmp.path().join("qoop-1/sessions/default");
        private_tree(tmp.path(), &new_dir);
        assert_eq!(awake_lock(&new_dir), new_dir.join("awake.lock"));
        private_tree(tmp.path(), &old_dir);
        assert_eq!(awake_lock(&new_dir), old_dir.join("awake.lock"));
    }
}

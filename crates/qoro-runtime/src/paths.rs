//! Runtime directory and socket path, with the checks that make them safe.
//!
//! ADR-012. The daemon executes arbitrary commands with the user's full privileges, so
//! anything that can speak to its socket can run code as that user. A path is not a
//! security contract; these checks are.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Overrides the runtime directory. This is not the optional session namespace ADR-006
/// rejects: it exists so integration tests, which run in parallel, each get their own
/// daemon and socket, and so a contributor running the suite does not attach to their own
/// working daemon.
pub const RUNTIME_DIR_ENV: &str = "QORO_RUNTIME_DIR";

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("no usable runtime directory: set {RUNTIME_DIR_ENV} or XDG_RUNTIME_DIR")]
    NoRuntimeDir,
    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),
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
    Ok(dir)
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("qorod.sock")
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
        assert_eq!(got, dir);
        // SAFETY: as above.
        unsafe { std::env::remove_var(RUNTIME_DIR_ENV) };
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            verify_private_dir(&dir).is_err(),
            "the override is verified like any other path"
        );
    }
}

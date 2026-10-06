//! `qoro update`: replace this binary with the newest GitHub Release.
//!
//! The one subcommand that needs the network, and it says so in
//! `docs/cli-surface.md`. Everything else behaves the same offline; this one fails
//! closed instead: with the network gone it exits nonzero having touched nothing.
//!
//! The daemon never updates itself (ADR-019), and neither does the interface. This is a
//! separate short lived process the person runs themselves, and it refuses while any
//! session is running, naming what is live, because an upgrade must not kill agents.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use qoro_runtime::paths;
use qoro_runtime::release::{Release, asset_for, check_sha256, download, fetch_latest};

/// Whether this process runs under Rosetta.
fn translated() -> bool {
    Command::new("sysctl")
        .args(["-in", "sysctl.proc_translated"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// `qoro update` checks GitHub Releases and installs the newest binary;
/// `qoro update --check` only leaves what it saw in the notice file the interface reads.
pub fn run(args: &[String]) -> std::io::Result<()> {
    // Anything else is refused before the network: a mistyped `--check` read as no
    // flag at all would install instead of looking.
    if let Some(other) = args
        .iter()
        .skip(2)
        .find(|a| !matches!(a.as_str(), "--check" | "--help" | "-h"))
    {
        return Err(std::io::Error::other(format!(
            "unknown argument {other}; usage: qoro update [--check]"
        )));
    }
    let check_only = args.iter().any(|a| a == "--check");
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!(
            "usage:\n\
             \x20 qoro update           install the newest release over this binary\n\
             \x20 qoro update --check   only note what the newest release is\n"
        );
        return Ok(());
    }
    let current = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    // An Intel build running under Rosetta on an Apple silicon Mac: the machine is arm64,
    // and updating to the Intel build again would keep it translated forever.
    let arch = if os == "macos" && translated() {
        "aarch64"
    } else {
        std::env::consts::ARCH
    };
    let Some(asset) = asset_for(os, arch) else {
        return Err(std::io::Error::other(format!(
            "no prebuilt binary for {os}-{arch}; build from source: \
             cargo build --release -p qoro-cli"
        )));
    };
    let latest = fetch_latest(asset).map_err(|detail| {
        std::io::Error::other(format!("cannot reach GitHub Releases: {detail}"))
    })?;
    if !qoro_runtime::paths::is_newer_version(&latest.version, current) {
        clear_notice();
        println!("qoro {current} is the newest release");
        return Ok(());
    }
    if check_only {
        if let Err(e) = write_notice(&latest.version) {
            return Err(std::io::Error::other(format!(
                "saw {}, but cannot leave the notice: {e}",
                latest.version
            )));
        }
        println!(
            "update available: {current} -> {}: run `qoro update` with nothing awake to install it",
            latest.version
        );
        return Ok(());
    }
    clear_the_way()?;
    install(&latest, current)
}

/// Refuse while a tab is awake, so no agent dies to a routine upgrade; a daemon with
/// nothing awake is asked to go and waited out, so the new binary is the one that starts.
fn clear_the_way() -> std::io::Result<()> {
    use qoro_runtime::control::{ControlRequest, ControlResponse};

    let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
    let socket = paths::socket_path(&dir);
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let awake = runtime.block_on(async {
        match crate::ask_daemon(
            &socket,
            "qoro-update",
            &ControlRequest::ListTerminals {
                id: crate::request_id("update"),
            },
        )
        .await?
        {
            ControlResponse::Terminals { terminals, .. } => Ok(terminals.len()),
            _ => Err(std::io::Error::other(
                "unexpected answer to the terminal list",
            )),
        }
    })?;
    if awake > 0 {
        return Err(std::io::Error::other(format!(
            "refusing to update: {awake} tab{} awake. Put {} spaces to sleep (`qoro sleep \
             <space>`) or stop qoro (`qoro stop`) first, so no agent dies to a routine upgrade",
            if awake == 1 { "" } else { "s" },
            if awake == 1 { "its" } else { "their" },
        )));
    }
    runtime.block_on(crate::ask_daemon(
        &socket,
        "qoro-update",
        &ControlRequest::Shutdown {
            id: crate::request_id("update"),
        },
    ))?;
    // Gone, not only told: the answer comes before the daemon has left, and a swap while it
    // runs would leave the old binary answering the next `qoro`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && std::os::unix::net::UnixStream::connect(&socket).is_ok()
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Ok(())
}

/// Fetch the tarball, check it, and swap it over this binary atomically.
fn install(latest: &Release, current: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| std::io::Error::other(format!("cannot find this binary: {e}")))?;
    install_to(latest, current, &exe)?;
    // A running notch is the old build's; launchd starts the new one in its place.
    crate::notch::restart_if_installed();
    // The same for a phone's bridge service, which would otherwise keep the old build or,
    // once launchd has pinned the replaced file, never start again.
    // Asked of the new binary, which is the one the service must run from now on.
    let reload = Command::new(&exe)
        .args(["bridge", "reload"])
        .stdout(std::process::Stdio::null())
        .status();
    if !reload.is_ok_and(|status| status.success()) {
        eprintln!("qoro: the bridge service was not reloaded; run `qoro bridge reload`");
    }
    Ok(())
}

/// The same swap onto an explicit path, so tests drive the whole chain without
/// touching the test binary they are running inside of.
fn install_to(latest: &Release, current: &str, exe: &Path) -> std::io::Result<()> {
    // The file itself, not the link it was launched through: `current_exe` on macOS is
    // the PATH spelling, and renaming over a link turns it into a plain file that no
    // later build of what it pointed at ever reaches again.
    let exe = &std::fs::canonicalize(exe)
        .map_err(|e| std::io::Error::other(format!("cannot resolve {exe:?}: {e}")))?;
    // A private directory with an unpredictable name, one per install: a name another user
    // of a shared /tmp can guess is one they can fill first, and two installs in one
    // process (two tests share a pid) must not unpack over each other.
    let work_dir = tempfile::tempdir()?;
    let work = work_dir.path().to_path_buf();
    let cleanup = || std::fs::remove_dir_all(&work).ok();
    let failed = |message: String| -> std::io::Error {
        cleanup();
        std::io::Error::other(message)
    };

    let tarball = work.join(&latest.asset);
    if let Err(detail) = download(&latest.asset_url, &tarball) {
        return Err(failed(format!("the download failed: {detail}")));
    }
    if let Some(url) = &latest.checksum_url {
        let checksum_file = work.join(format!("{}.sha256", latest.asset));
        match download(url, &checksum_file) {
            Ok(()) => {
                if let Err(detail) = check_sha256(&tarball, &checksum_file) {
                    return Err(failed(format!(
                        "refusing to update: the checksum does not match: {detail}"
                    )));
                }
            }
            // The release says it has a checksum, so failing to fetch it is not the
            // same as there being none: installing unverified here would skip the one
            // integrity check exactly when something went wrong.
            Err(detail) => {
                return Err(failed(format!(
                    "refusing to update: the release lists a checksum that cannot be \
                     downloaded: {detail}"
                )));
            }
        }
    }
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .current_dir(&work)
        .status()
        .map_err(|e| failed(format!("cannot unpack the release: {e}")))?;
    if !tar.success() {
        return Err(failed(format!(
            "cannot unpack the release: tar exited {tar}"
        )));
    }
    let fresh = work.join("qoro");
    let reported = Command::new(&fresh)
        .arg("--version")
        .output()
        .map_err(|e| failed(format!("the downloaded binary does not run: {e}")))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("qoro {}", latest.version) {
        return Err(failed(format!(
            "refusing to update: the download reports {reported:?}, want {:?}",
            format!("qoro {}", latest.version)
        )));
    }
    let staged = exe.with_extension("new");
    std::fs::copy(&fresh, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::remove_file(&staged);
        return Err(failed(format!("cannot replace {exe:?}: {e}")));
    }
    cleanup();
    clear_notice();
    let mut out = std::io::stdout();
    writeln!(
        out,
        "updated qoro {current} -> {} at {}",
        latest.version,
        exe.display()
    )?;
    writeln!(
        out,
        "start qoro again to use it; nothing running was touched"
    )?;
    Ok(())
}

/// Leave the newest version where the interface looks without touching the network.
fn write_notice(version: &str) -> std::io::Result<()> {
    let path = paths::update_notice_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{version}\n"))
}

/// A notice that no longer points ahead is worse than none.
fn clear_notice() {
    let path = paths::update_notice_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let seen = text.trim().trim_start_matches('v');
        if !paths::is_newer_version(seen, env!("CARGO_PKG_VERSION")) {
            std::fs::remove_file(&path).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Release, install_to};

    const MAC_ARM: &str = "qoro-aarch64-apple-darwin.tar.gz";

    /// A stand-in release tarball: a script answering `--version` the way the
    /// real binary does, packed the way the release script packs it.
    fn fake_release(dir: &std::path::Path, reported: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("qoro");
        std::fs::write(&script, format!("#!/bin/sh\necho \"qoro {reported}\"\n")).expect("script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let tarball = dir.join("qoro-aarch64-apple-darwin.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf"])
            .arg(&tarball)
            .arg("qoro")
            .current_dir(dir)
            .status()
            .expect("tar");
        assert!(status.success());
        format!("file://{}", tarball.display())
    }

    #[test]
    fn install_swaps_the_binary_after_verifying_its_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoro");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &exe).expect("install");
        let out = std::process::Command::new(&exe)
            .arg("--version")
            .output()
            .expect("run the swapped binary");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "qoro 9.9.9",
            "the file at the old path is the new release now"
        );
    }

    /// A release that lists a checksum and then cannot serve it is not a release
    /// without one: installing unverified there skips the only integrity check.
    #[test]
    fn a_checksum_that_cannot_be_downloaded_refuses_the_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoro");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: Some(format!("file://{}/missing.sha256", pack.display())),
        };
        assert!(install_to(&latest, "0.0.0", &exe).is_err());
        assert_eq!(
            std::fs::read(&exe).expect("read"),
            b"old binary",
            "an unverifiable download must not touch the binary in hand"
        );
    }

    /// Launched through a link on PATH, the update lands on the file the link points
    /// at, and the link stays a link: replacing it would cut it off from every later
    /// build of the file it pointed to.
    #[test]
    fn install_through_a_symlink_replaces_the_file_it_points_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let real = dir.path().join("real-qoro");
        std::fs::write(&real, "old binary").expect("old");
        let link = dir.path().join("qoro");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &link).expect("install");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link was replaced by a plain file"
        );
        let out = std::process::Command::new(&real)
            .arg("--version")
            .output()
            .expect("run the swapped binary");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "qoro 9.9.9");
    }

    #[test]
    fn a_download_reporting_the_wrong_version_leaves_the_binary_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoro");
        std::fs::write(&exe, "old binary").expect("old");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "0.0.0"),
            checksum_url: None,
        };
        assert!(install_to(&latest, "0.0.0", &exe).is_err());
        assert_eq!(
            std::fs::read(&exe).expect("read"),
            b"old binary",
            "a refused download must not touch the binary in hand"
        );
    }
}

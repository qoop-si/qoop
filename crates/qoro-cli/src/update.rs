//! `qoro update`: replace this binary with the newest GitHub Release.
//!
//! The one subcommand that needs the network, and it says so in
//! `docs/cli-surface.md`. Everything else behaves the same offline; this one fails
//! closed instead: with the network gone it installs nothing, says so, and exits nonzero.
//!
//! The daemon never updates itself (ADR-019), and neither does the interface. This is a
//! separate short lived process the person runs themselves. It never stops a session:
//! after the install it asks every daemon, this machine's and each saved remote's, to
//! move onto the new binary at the first moment nothing there is working, and returns
//! as soon as they have answered rather than waiting for the moves. Run again with
//! nothing newer to install, it does only that, so it is also "bring everything to this
//! version".

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use bytes::BytesMut;
use qoro_remote::saved::{self, Remote};
use qoro_runtime::control::{ControlRequest, ControlResponse};
use qoro_runtime::release::{Release, asset_for, check_sha256, download, fetch_latest};
use qoro_runtime::{client, paths};
use qoro_tui::link::{self, Update, plain};

/// How long a daemon is given to answer the ask to move. Past what the daemon itself takes
/// to look at its terminals twice, which a slow disk stretches.
const ASK_TO_MOVE: Duration = Duration::from_secs(30);

/// How long to look for a daemon that closed its connection without answering, to see
/// whether it came back as the new version.
const COME_BACK: Duration = Duration::from_secs(20);

/// How many times another update holding a remote's swap lock is waited out, and for how
/// long each time, before that remote is reported as busy.
const BUSY_TRIES: u32 = 3;
const BUSY_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_secs(3)
};

/// A staged copy of an install that was killed halfway is swept after this long.
const STALE_STAGE: Duration = Duration::from_secs(30 * 60);

/// A line about a machine: `Ok` when all is as it should be, `Err` when it is not.
type Said = Result<String, String>;

/// Whether this process runs under Rosetta.
fn translated() -> bool {
    Command::new("sysctl")
        .args(["-in", "sysctl.proc_translated"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// `qoro update` checks GitHub Releases, installs the newest binary and moves every machine
/// onto it; `qoro update --check` only leaves what it saw in the notice file the interface
/// reads.
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
             \x20 qoro update           install the newest release and move every machine onto it\n\
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
    let latest = fetch_latest(asset)
        .map_err(|detail| std::io::Error::other(format!("cannot reach GitHub Releases: {detail}")));
    if check_only {
        let latest = latest?;
        if !paths::is_newer_version(&latest.version, current) {
            clear_notice();
            println!("qoro {current} is the newest release");
            return Ok(());
        }
        if let Err(e) = write_notice(&latest.version) {
            return Err(std::io::Error::other(format!(
                "saw {}, but cannot leave the notice: {e}",
                latest.version
            )));
        }
        println!(
            "update available: {current} -> {}: run `qoro update` to install it",
            latest.version
        );
        return Ok(());
    }
    // What every machine moves to: the version just installed, or this binary's own when
    // nothing newer is out, and also when the release cannot be read, since what is on
    // disk can be moved onto without the network.
    let mut version = current;
    match &latest {
        Ok(latest) if paths::is_newer_version(&latest.version, current) => {
            install(latest, current)?;
            version = &latest.version;
        }
        Ok(_) => {
            clear_notice();
            println!("qoro {current} is the newest release");
        }
        Err(_) => {
            eprintln!("qoro: no release could be read; moving every machine onto {current} anyway")
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let this_machine_ok = runtime.block_on(bring_along(version));
    latest?;
    if !this_machine_ok {
        return Err(std::io::Error::other(
            "this machine's daemon was not moved: see above",
        ));
    }
    Ok(())
}

/// Ask this machine's daemon, then each saved remote's, to move to `version`, one line each,
/// and say whether this machine's own step went as it should.
///
/// What goes wrong over there is that machine's line and nothing else: an offline laptop in
/// the list would otherwise fail every update, and the interface tries it again by itself
/// the next time it connects.
async fn bring_along(version: &str) -> bool {
    let this = this_machine(version).await;
    let ok = this.is_ok();
    let (Ok(line) | Err(line)) = this;
    println!("this machine: {line}");
    let remotes = match saved::load() {
        Ok(remotes) => remotes,
        Err(e) => {
            println!("remotes: not read: {e}");
            return ok;
        }
    };
    for remote in remotes {
        println!("{}: {}", remote.name, remote_line(&remote, version).await);
    }
    ok
}

/// This machine's daemon: connected, handshaken (the welcome carries the version it runs,
/// which `client::connect` throws away), and that version.
async fn hello(socket: &Path) -> std::io::Result<(tokio::net::UnixStream, BytesMut, String)> {
    let timed_out =
        |what: &str| std::io::Error::new(std::io::ErrorKind::TimedOut, what.to_string());
    let stream = tokio::time::timeout(ASK_TO_MOVE, tokio::net::UnixStream::connect(socket))
        .await
        .map_err(|_| timed_out("the daemon did not accept the connection"))??;
    let (stream, inbuf, welcome) =
        tokio::time::timeout(ASK_TO_MOVE, client::handshake(stream, "qoro-update", None))
            .await
            .map_err(|_| timed_out("the daemon did not answer the handshake"))??;
    Ok((stream, inbuf, welcome.server_version))
}

/// What came of the ask to this machine's own daemon, or that there is none to ask.
async fn this_machine(version: &str) -> Said {
    let socket = match paths::runtime_dir() {
        Ok(dir) => paths::socket_path(&dir),
        Err(e) => return Err(format!("not asked to move: {e}")),
    };
    match hello(&socket).await {
        Ok((stream, inbuf, running)) => {
            let again = || async { hello(&socket).await.map(|(_, _, running)| running) };
            move_daemon(stream, inbuf, &running, version, again).await
        }
        // Only nothing listening is "no daemon": a refused permission or a wrong directory
        // is said as it is.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(format!("no daemon running, the next qoro starts {version}"))
        }
        Err(e) => Err(format!("not asked to move: {e}")),
    }
}

/// What came of putting `version` on one remote and asking its daemon onto it. Everything
/// a remote said is plain text first: it would reach this terminal as it was printed.
async fn remote_line(remote: &Remote, version: &str) -> String {
    let (on_disk, warning) = match put_on(&remote.target, version).await {
        Ok(Update::OnDisk(on_disk, warning)) => (on_disk, warning),
        Ok(Update::NoRelease) => {
            return format!("no {version} release, left as it is");
        }
        Err(e) => return format!("not updated: {}", plain(&e.to_string())),
    };
    // Opened as the interface opens one nobody is watching, with a key of its own: a fixed
    // one would make two runs at once end each other's connection.
    let open = || async { link::open(remote, true, &crate::request_id("update")).await };
    let (Ok(line) | Err(line)) = match open().await {
        Ok(link) => {
            let again = || async { open().await.map(|link| link.server_version.clone()) };
            move_daemon(
                link.stream,
                link.leftover,
                &link.server_version,
                &on_disk,
                again,
            )
            .await
        }
        Err(e) => Err(format!(
            "{on_disk} is on its disk, but its daemon was not asked to move: {}",
            plain(&e.to_string())
        )),
    };
    match warning {
        Some(warning) => format!("{line}\n  but {warning}"),
        None => line,
    }
}

/// `link::update`, waiting out another update that holds the swap lock over there (an open
/// interface pushing at the same moment): when it finishes, this finds the version on disk.
/// The lock's refusal is told by its sentence in `link.rs`; reword them together.
async fn put_on(target: &str, version: &str) -> std::io::Result<Update> {
    retry_busy(|| link::update(target, version)).await
}

async fn retry_busy<F, Fut>(mut attempt: F) -> std::io::Result<Update>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<Update>>,
{
    let mut busy = 0;
    loop {
        match attempt().await {
            Err(e)
                if busy < BUSY_TRIES && e.to_string().contains("update of qoro is in progress") =>
            {
                busy += 1;
                tokio::time::sleep(BUSY_WAIT).await;
            }
            done => return done,
        }
    }
}

/// Ask the daemon behind `stream`, which runs `running`, to move to `version`, and say what
/// it answered. Never waits for the move itself: this may be running in a tab of that very
/// daemon, which holds the move until it exits. A daemon that closes the connection with no
/// answer is going, or gone wrong, which is told by looking for it again with `again`.
async fn move_daemon<S, F, Fut>(
    stream: S,
    inbuf: BytesMut,
    running: &str,
    version: &str,
    again: F,
) -> Said
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<String>>,
{
    if !paths::is_newer_version(version, running) {
        return Ok(format!("already {running}"));
    }
    let request = ControlRequest::RestartWhenSafe {
        id: crate::request_id("update"),
        version: version.to_string(),
    };
    match crate::ask_over(stream, inbuf, &request, ASK_TO_MOVE).await {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            came_back(version, COME_BACK, again).await
        }
        answer => arm_line(answer, version),
    }
}

/// A daemon closed its connection with no answer, which is what one with nothing in the
/// way does on its way to the new binary, and also what a crash or a dropped ssh looks
/// like: look for it again until `within` has passed, and say it is moving only when it
/// answers as `version`. A daemon still answering as the old one may only be on its way
/// out, so that is waited past too.
async fn came_back<F, Fut>(version: &str, within: Duration, again: F) -> Said
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<String>>,
{
    let deadline = std::time::Instant::now() + within;
    loop {
        let seen = match again().await {
            Ok(running) if running == version => return Ok(format!("moving to {version} now")),
            Ok(running) => format!("runs {}, not {version}", plain(&running)),
            Err(e) => format!("did not come back as {version}: {}", plain(&e.to_string())),
        };
        if std::time::Instant::now() >= deadline {
            return Err(format!("closed the connection and {seen}"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// One daemon's answer to the ask to move, as a line. What it says is plain text first.
fn arm_line(answer: std::io::Result<ControlResponse>, version: &str) -> Said {
    match answer {
        Ok(ControlResponse::RestartArmed {
            waiting_on: Some(why),
            ..
        }) => Ok(format!(
            "moves to {version} once nothing is working; waiting on {}",
            plain(&why)
        )),
        Ok(ControlResponse::RestartArmed { .. }) => {
            Ok(format!("moves to {version} once nothing is working"))
        }
        // A daemon from before the ask existed, told by serde's own wording, as the
        // interface tells it.
        Ok(ControlResponse::Failed { reason, .. }) if reason.contains("unknown variant") => Err(
            "runs a qoro from before updates moved on their own; the new one takes over \
                 when it next restarts"
                .to_string(),
        ),
        Ok(ControlResponse::Failed { reason, .. }) => {
            Err(format!("keeps its qoro for now: {}", plain(&reason)))
        }
        Ok(_) => Err("gave an answer this qoro does not understand".to_string()),
        Err(e) => Err(format!("not asked to move: {}", plain(&e.to_string()))),
    }
}

/// Fetch the tarball, check it, and swap it over this binary atomically.
fn install(latest: &Release, current: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| std::io::Error::other(format!("cannot find this binary: {e}")))?;
    install_to(latest, current, &exe)?;
    refresh_integrations(&exe);
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

/// What `scripts/install.sh` does after it puts a binary down: the new build's own `hooks
/// install` and `skills install`, which skip an agent that is not on this machine, so the
/// files agents read are never older than the binary. A failure is said and never fails
/// an update that has already happened.
fn refresh_integrations(exe: &Path) {
    for what in ["hooks", "skills"] {
        if !Command::new(exe)
            .args([what, "install"])
            .status()
            .is_ok_and(|status| status.success())
        {
            eprintln!("qoro: the {what} were not refreshed; run `qoro {what} install`");
        }
    }
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
    // Staged under a name of its own, beside the file so the rename stays on one filesystem:
    // two updates at once each rename a whole verified copy, never one half written.
    let dir = exe.parent().unwrap_or(Path::new("."));
    sweep_stale_stages(dir);
    let staged = tempfile::Builder::new()
        .prefix(".qoro-update.")
        .tempfile_in(dir)?;
    std::fs::copy(&fresh, staged.path())?;
    std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))?;
    if let Err(e) = staged.persist(exe) {
        return Err(failed(format!("cannot replace {exe:?}: {}", e.error)));
    }
    cleanup();
    clear_notice();
    writeln!(
        std::io::stdout(),
        "updated qoro {current} -> {} at {}",
        latest.version,
        exe.display()
    )?;
    Ok(())
}

/// Take away the staged copies a killed install left in `dir`: a whole binary each, which a
/// fixed staging name used to overwrite the next time. Only old ones, so an install running
/// right now keeps its own.
fn sweep_stale_stages(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(".qoro-update.")
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STALE_STAGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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
    use super::{
        BytesMut, ControlResponse, Release, Update, arm_line, came_back, install_to, move_daemon,
        retry_busy,
    };

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

    fn armed(waiting_on: Option<&str>) -> std::io::Result<ControlResponse> {
        Ok(ControlResponse::RestartArmed {
            id: "u".into(),
            waiting_on: waiting_on.map(str::to_string),
        })
    }

    fn failed(reason: &str) -> std::io::Result<ControlResponse> {
        Ok(ControlResponse::Failed {
            id: String::new(),
            reason: reason.to_string(),
        })
    }

    /// Each answer to the ask to move reads as one line that says what happens next, and
    /// only a refusal or an answer it cannot read is a failure.
    #[test]
    fn each_answer_to_the_ask_to_move_is_one_line() {
        assert_eq!(
            arm_line(armed(Some("tab 3: sleep is running in it")), "0.7.0"),
            Ok(
                "moves to 0.7.0 once nothing is working; waiting on tab 3: sleep is running in it"
                    .to_string()
            )
        );
        assert_eq!(
            arm_line(armed(None), "0.7.0"),
            Ok("moves to 0.7.0 once nothing is working".to_string())
        );
        assert_eq!(
            arm_line(failed("not started as `qoro daemon`"), "0.7.0"),
            Err("keeps its qoro for now: not started as `qoro daemon`".to_string())
        );
        assert!(
            arm_line(failed("unknown variant `RestartWhenSafe`"), "0.7.0")
                .expect_err("old daemon")
                .starts_with("runs a qoro from before updates moved")
        );
        assert_eq!(
            arm_line(Err(std::io::Error::other("timed out")), "0.7.0"),
            Err("not asked to move: timed out".to_string())
        );
        assert!(
            arm_line(
                Ok(ControlResponse::Stopping {
                    id: "u".into(),
                    terminals: 0,
                }),
                "0.7.0"
            )
            .is_err()
        );
    }

    /// What a remote's daemon says never reaches the terminal as an escape.
    #[test]
    fn what_a_daemon_says_is_shown_plain() {
        let said = arm_line(armed(Some("tab 1\u{1b}[2J: busy")), "0.7.0").expect("armed");
        assert!(!said.contains('\u{1b}'), "{said:?}");
        let said = arm_line(failed("no\u{7}pe"), "0.7.0").expect_err("refused");
        assert!(!said.contains('\u{7}'), "{said:?}");
    }

    /// A closed connection is "moving" only when the daemon comes back saying it is the new
    /// version; anything else says what was seen.
    #[tokio::test]
    async fn a_closed_connection_is_moving_only_when_the_daemon_says_so() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let soon = std::time::Duration::from_millis(600);
        let calls = AtomicU32::new(0);
        // Gone for a moment, then still the old one, then the new one.
        let said = came_back("0.7.0", std::time::Duration::from_secs(20), || async {
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err(std::io::Error::other("connection refused")),
                1 => Ok("0.6.0".to_string()),
                _ => Ok("0.7.0".to_string()),
            }
        })
        .await;
        assert_eq!(said, Ok("moving to 0.7.0 now".to_string()));

        let said = came_back("0.7.0", soon, || async { Ok("0.6.0".to_string()) }).await;
        assert_eq!(
            said,
            Err("closed the connection and runs 0.6.0, not 0.7.0".to_string())
        );
        let said = came_back("0.7.0", soon, || async {
            Err(std::io::Error::other("Connection timed out\u{1b}[2J"))
        })
        .await;
        assert_eq!(
            said,
            Err(
                "closed the connection and did not come back as 0.7.0: Connection timed out[2J"
                    .to_string()
            )
        );
    }

    /// An already running version is said without asking anything of the daemon.
    #[tokio::test]
    async fn a_daemon_already_there_is_not_asked() {
        let (stream, _other) = tokio::io::duplex(64);
        let said = move_daemon(stream, BytesMut::new(), "0.7.0", "0.7.0", || async {
            Ok(String::new())
        })
        .await;
        assert_eq!(said, Ok("already 0.7.0".to_string()));
    }

    /// Another update holding the swap lock over there is waited out a few times, and only
    /// that: any other refusal ends at once.
    #[tokio::test]
    async fn a_busy_swap_lock_is_retried_and_nothing_else_is() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let busy = || {
            std::io::Error::other("another update of qoro is in progress there; try again later")
        };
        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(busy())
            } else {
                Ok(Update::NoRelease)
            }
        })
        .await;
        assert_eq!(done.expect("done"), Update::NoRelease);
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(busy())
        })
        .await;
        assert!(done.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), super::BUSY_TRIES + 1);

        let calls = AtomicU32::new(0);
        let done = retry_busy(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::other("no qoro file to replace"))
        })
        .await;
        assert!(done.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A staged copy left by a killed install is swept once it is old, and one that may
    /// belong to an install running now is left.
    #[test]
    fn a_stale_staged_copy_is_swept_and_a_fresh_one_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("pack");
        std::fs::create_dir(&pack).expect("pack dir");
        let exe = dir.path().join("qoro");
        std::fs::write(&exe, "old binary").expect("old");
        let stale = dir.path().join(".qoro-update.stale");
        let fresh = dir.path().join(".qoro-update.fresh");
        for file in [&stale, &fresh] {
            std::fs::write(file, "half").expect("staged");
        }
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .expect("age it");
        let latest = Release {
            version: "9.9.9".to_string(),
            asset: MAC_ARM.to_string(),
            asset_url: fake_release(&pack, "9.9.9"),
            checksum_url: None,
        };
        install_to(&latest, "0.0.0", &exe).expect("install");
        assert!(!stale.exists(), "an old staged copy was kept");
        assert!(fresh.exists(), "a fresh staged copy was taken");
    }
}

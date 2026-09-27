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

/// The repository whose Releases carry the binaries.
const RELEASES: &str = "https://api.github.com/repos/pat/qoro/releases";

/// Where the releases are read from instead, when set: a mirror, or a directory given as a
/// `file://` URL holding a `latest` file in the API's own shape. The same variable
/// `scripts/install.sh` reads, so a platform pick can be tested with no network at all.
const RELEASE_BASE_ENV: &str = "QORO_RELEASE_BASE";

/// Whether this process runs under Rosetta.
fn translated() -> bool {
    Command::new("sysctl")
        .args(["-in", "sysctl.proc_translated"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
}

/// The tarball `scripts/release.sh` packs for a platform, named by its target triple.
///
/// `None` is a platform no binary is built for, which is refused with the way to build
/// one rather than with a download that could never run.
fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("macos", "aarch64") => "qoro-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "qoro-x86_64-apple-darwin.tar.gz",
        ("linux", "x86_64") => "qoro-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "qoro-aarch64-unknown-linux-gnu.tar.gz",
        _ => return None,
    })
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
            "update available: {current} -> {}: run `qoro update` with no session running to install it",
            latest.version
        );
        return Ok(());
    }
    let live = live_sessions();
    if !live.is_empty() {
        return Err(std::io::Error::other(format!(
            "refusing to update: session{} running: {}. Stop {} first (`qoro stop <name>`), \
             then update, so no agent dies to a routine upgrade",
            if live.len() == 1 { "" } else { "s" },
            live.join(", "),
            if live.len() == 1 { "it" } else { "them" },
        )));
    }
    install(&latest, current)
}

/// What the newest release is: its version and where this platform's asset lives.
struct Release {
    version: String,
    /// The tarball's own name, which is also the name its checksum file is written for.
    asset: String,
    asset_url: String,
    checksum_url: Option<String>,
}

/// Read the newest release off the GitHub API with curl, which is what keeps this
/// updater free of HTTP dependencies. curl honours the proxy environment, so a
/// blackholed network fails here rather than hanging.
fn fetch_latest(asset: &str) -> Result<Release, String> {
    let base = std::env::var(RELEASE_BASE_ENV)
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| RELEASES.to_string());
    let body = curl(&format!("{}/latest", base.trim_end_matches('/')))?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| format!("the releases API answered what is not JSON: {e}"))?;
    fetch_latest_parsing(&json, asset)
}

/// The version and the asset URL out of one releases API answer, so the parsing is
/// testable without the network.
fn fetch_latest_parsing(json: &serde_json::Value, asset: &str) -> Result<Release, String> {
    let tag = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "the releases API answered without a tag_name".to_string())?;
    let version = tag.trim_start_matches('v').to_string();
    if paths::parse_version(&version).is_none() {
        return Err(format!(
            "the newest release is tagged {tag:?}, which is no version"
        ));
    }
    let assets = json
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    // The API `url`, not `browser_download_url`: the browser one 404s for a private
    // repository even with a token, while the API one answers with the file when asked
    // for `application/octet-stream`. It works unchanged for a public repository.
    let url_of = |name: &str| {
        assets.iter().find_map(|a| {
            if a.get("name").and_then(|n| n.as_str()) == Some(name) {
                a.get("url").and_then(|u| u.as_str()).map(str::to_string)
            } else {
                None
            }
        })
    };
    let Some(asset_url) = url_of(asset) else {
        return Err(format!("the newest release carries no {asset}"));
    };
    Ok(Release {
        version,
        asset: asset.to_string(),
        asset_url,
        checksum_url: url_of(&format!("{asset}.sha256")),
    })
}

/// GET a URL to memory.
fn curl(url: &str) -> Result<Vec<u8>, String> {
    fetch(url, "application/vnd.github+json", "25", None)
}

/// Download a release asset to a file. The API asset URL answers with the file itself
/// only when asked for octet-stream; otherwise it answers with the asset's JSON.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    fetch(url, "application/octet-stream", "120", Some(dest)).map(drop)
}

/// One curl request, to memory or to a file. Anything curl reports becomes text the
/// person can act on.
///
/// A token rides along when there is one, and only to `api.github.com`: the repository
/// is private, so without one nothing is found at all, and anonymous calls also share
/// 60 requests an hour per IP. It goes to curl on stdin as a config line (`-K -`), never in
/// argv where any process listing shows it; `-K` is understood by every curl, unlike
/// `-H @-` (7.55 and later). A redirect to the storage host is followed here, without the
/// token, because curl older than 7.58 sent a custom Authorization header on to the host
/// it was redirected to.
fn fetch(url: &str, accept: &str, max_time: &str, dest: Option<&Path>) -> Result<Vec<u8>, String> {
    use std::process::Stdio;
    let token = if url.starts_with("https://api.github.com/") {
        github_token()
    } else {
        None
    };
    let Some(token) = token else {
        let mut command = Command::new("curl");
        command.args(["-fsSL", "--max-time", max_time, "-H"]);
        command.arg(format!("Accept: {accept}"));
        if let Some(dest) = dest {
            command.arg("-o").arg(dest);
        }
        let out = command
            .arg(url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("curl is not installed or cannot run: {e}"))?;
        return curl_result(out);
    };
    // The body lands in a file either way, so the redirect, if any, is all stdout says.
    static SPARES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SPARES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let spare = std::env::temp_dir().join(format!("qoro-fetch-{}-{n}", std::process::id()));
    let body = dest.unwrap_or(&spare);
    let mut child = Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            max_time,
            "-K",
            "-",
            "-w",
            "%{redirect_url}",
            "-H",
        ])
        .arg(format!("Accept: {accept}"))
        .arg("-o")
        .arg(body)
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl is not installed or cannot run: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A failed write shows up as curl failing, with its own words.
        let _ = writeln!(stdin, "header = \"Authorization: Bearer {token}\"");
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl did not finish: {e}"))?;
    let redirect = String::from_utf8_lossy(&curl_result(out)?)
        .trim()
        .to_string();
    if !redirect.is_empty() {
        // Not api.github.com, so this request carries no token.
        fetch(&redirect, accept, max_time, Some(body))?;
    }
    match dest {
        Some(_) => Ok(Vec::new()),
        None => {
            let read = std::fs::read(body).map_err(|e| format!("cannot read the answer: {e}"));
            let _ = std::fs::remove_file(body);
            read
        }
    }
}

/// What curl printed, or what it said went wrong.
fn curl_result(out: std::process::Output) -> Result<Vec<u8>, String> {
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("curl exited with {}", out.status)
        } else {
            detail
        });
    }
    Ok(out.stdout)
}

/// A GitHub API token, when the person has one: from the environment, else from `gh`.
///
/// `gh` users already have one: it keeps exactly these variables, and `gh auth token`
/// prints the one it logged in with. Checked at request time rather than at startup,
/// so a failure never depends on when the process began.
fn github_token() -> Option<String> {
    ["GH_TOKEN", "GITHUB_TOKEN"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|token| token.trim().to_string())
        .find(|token| !token.is_empty())
        .or_else(|| {
            let out = Command::new("gh")
                .args(["auth", "token"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (out.status.success() && !token.is_empty()).then_some(token)
        })
}

/// Every session with a daemon answering, read off the disk the way `qoro ls` does.
fn live_sessions() -> Vec<String> {
    let Ok(base) = paths::runtime_base() else {
        return Vec::new();
    };
    paths::sessions(&base)
        .into_iter()
        .filter(|name| {
            let dir = paths::session_dir(&base, name);
            std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_ok()
        })
        .collect()
}

/// Fetch the tarball, check it, and swap it over this binary atomically.
fn install(latest: &Release, current: &str) -> std::io::Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| std::io::Error::other(format!("cannot find this binary: {e}")))?;
    install_to(latest, current, &exe)
}

/// The same swap onto an explicit path, so tests drive the whole chain without
/// touching the test binary they are running inside of.
fn install_to(latest: &Release, current: &str, exe: &Path) -> std::io::Result<()> {
    // The file itself, not the link it was launched through: `current_exe` on macOS is
    // the PATH spelling, and renaming over a link turns it into a plain file that no
    // later build of what it pointed at ever reaches again.
    let exe = &std::fs::canonicalize(exe)
        .map_err(|e| std::io::Error::other(format!("cannot resolve {exe:?}: {e}")))?;
    // Counted, because one process can install twice: two tests in one binary share
    // a pid, and a shared work directory would have them unpacking over each other.
    static INSTALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = INSTALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let work = std::env::temp_dir().join(format!("qoro-update-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&work)?;
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

/// Compare the tarball against its `.sha256` file, when there is one.
///
/// `shasum` where it exists (every Mac), `sha256sum` where it does not (the slim Linux
/// images, which ship coreutils and no perl): a missing tool is not a mismatch, and
/// reading it as one refused every update on those machines.
fn check_sha256(tarball: &Path, checksum_file: &Path) -> Result<(), String> {
    let text =
        std::fs::read_to_string(checksum_file).map_err(|e| format!("cannot read it: {e}"))?;
    let want = text
        .split_whitespace()
        .next()
        .ok_or_else(|| "the file is empty".to_string())?;
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(tarball)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(tarball).output())
        .map_err(|e| format!("neither shasum nor sha256sum is installed: {e}"))?;
    if !out.status.success() {
        return Err("the checksum tool failed".to_string());
    }
    let got = String::from_utf8_lossy(&out.stdout);
    let got = got.split_whitespace().next().unwrap_or("");
    if got.eq_ignore_ascii_case(want) {
        Ok(())
    } else {
        Err("the hash differs".to_string())
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
    use super::{Release, asset_for, fetch_latest_parsing, install_to};

    const MAC_ARM: &str = "qoro-aarch64-apple-darwin.tar.gz";

    #[test]
    fn every_platform_a_binary_is_built_for_names_its_own_tarball() {
        assert_eq!(asset_for("macos", "aarch64"), Some(MAC_ARM));
        assert_eq!(
            asset_for("macos", "x86_64"),
            Some("qoro-x86_64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "x86_64"),
            Some("qoro-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "aarch64"),
            Some("qoro-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(asset_for("windows", "x86_64"), None);
        assert_eq!(asset_for("freebsd", "x86_64"), None);
    }

    #[test]
    fn a_linux_machine_picks_the_linux_tarball_out_of_the_same_release() {
        let body = serde_json::json!({
            "tag_name": "v0.4.0",
            "assets": [
                {"name": MAC_ARM, "url": "https://example.invalid/mac.tgz"},
                {"name": "qoro-x86_64-unknown-linux-gnu.tar.gz", "url": "https://example.invalid/linux.tgz"},
            ],
        });
        let release =
            fetch_latest_parsing(&body, "qoro-x86_64-unknown-linux-gnu.tar.gz").expect("linux");
        assert_eq!(release.asset_url, "https://example.invalid/linux.tgz");
        assert!(
            release.checksum_url.is_none(),
            "no checksum was published for it"
        );
    }

    #[test]
    fn the_release_json_yields_a_version_and_an_asset_url() {
        let body = serde_json::json!({
            "tag_name": "v0.4.0",
            "assets": [
                {"name": "qoro-aarch64-apple-darwin.tar.gz",
                 "url": "https://api.github.com/repos/pat/qoro/releases/assets/1",
                 "browser_download_url": "https://example.invalid/qoro.tgz"},
                {"name": "qoro-aarch64-apple-darwin.tar.gz.sha256",
                 "url": "https://api.github.com/repos/pat/qoro/releases/assets/2",
                 "browser_download_url": "https://example.invalid/qoro.tgz.sha256"},
            ],
        });
        let release = fetch_latest_parsing(&body, MAC_ARM).expect("the fixture is well formed");
        assert_eq!(release.version, "0.4.0");
        // The API url, because the browser one 404s for a private repository even
        // with a token.
        assert_eq!(
            release.asset_url,
            "https://api.github.com/repos/pat/qoro/releases/assets/1"
        );
        assert_eq!(
            release.checksum_url.as_deref(),
            Some("https://api.github.com/repos/pat/qoro/releases/assets/2")
        );
    }

    #[test]
    fn a_release_without_the_macos_asset_is_refused() {
        let body = serde_json::json!({"tag_name": "v0.4.0", "assets": []});
        assert!(fetch_latest_parsing(&body, MAC_ARM).is_err());
    }

    #[test]
    fn a_non_version_tag_is_refused() {
        let body = serde_json::json!({"tag_name": "nightly", "assets": []});
        assert!(fetch_latest_parsing(&body, MAC_ARM).is_err());
    }

    /// A stand-in release tarball: a script answering `--version` the way the
    /// real binary does, packed the way `scripts/release.sh` packs it.
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

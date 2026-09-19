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

/// The repository whose Releases carry the binaries, and the one asset this updater
/// knows: macOS arm64, the only prebuilt binary `scripts/release.sh` ships.
const RELEASES_LATEST: &str = "https://api.github.com/repos/pat/qoro/releases/latest";
const ASSET: &str = "qoro-aarch64-apple-darwin.tar.gz";

/// `qoro update` checks GitHub Releases and installs the newest binary;
/// `qoro update --check` only leaves what it saw in the notice file the interface reads.
pub fn run(args: &[String]) -> std::io::Result<()> {
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
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    if platform != "macos-aarch64" {
        return Err(std::io::Error::other(format!(
            "qoro update: no prebuilt binary for {platform}; only macos-aarch64 ships one, \
             elsewhere build from source: cargo build --release -p qoro-cli"
        )));
    }
    let latest = fetch_latest().map_err(|detail| {
        std::io::Error::other(format!(
            "qoro update: cannot reach GitHub Releases: {detail}"
        ))
    })?;
    if !qoro_runtime::paths::is_newer_version(&latest.version, current) {
        clear_notice();
        println!("qoro {current} is the newest release");
        return Ok(());
    }
    if check_only {
        if let Err(e) = write_notice(&latest.version) {
            return Err(std::io::Error::other(format!(
                "qoro update: saw {}, but cannot leave the notice: {e}",
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
            "qoro update: refusing: session{} running: {}. Stop {} first (`qoro stop <name>`), \
             then update, so no agent dies to a routine upgrade",
            if live.len() == 1 { "" } else { "s" },
            live.join(", "),
            if live.len() == 1 { "it" } else { "them" },
        )));
    }
    install(&latest, current)
}

/// What the newest release is: its version and where its macOS arm64 asset lives.
struct Release {
    version: String,
    asset_url: String,
    checksum_url: Option<String>,
}

/// Read the newest release off the GitHub API with curl, which is what keeps this
/// updater free of HTTP dependencies. curl honours the proxy environment, so a
/// blackholed network fails here rather than hanging.
fn fetch_latest() -> Result<Release, String> {
    let body = curl(RELEASES_LATEST)?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| format!("the releases API answered what is not JSON: {e}"))?;
    fetch_latest_parsing(&json)
}

/// The version and the asset URL out of one releases API answer, so the parsing is
/// testable without the network.
fn fetch_latest_parsing(json: &serde_json::Value) -> Result<Release, String> {
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
    let url_of = |name: &str| {
        assets.iter().find_map(|a| {
            if a.get("name").and_then(|n| n.as_str()) == Some(name) {
                a.get("browser_download_url")
                    .and_then(|u| u.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        })
    };
    let Some(asset_url) = url_of(ASSET) else {
        return Err(format!("the newest release carries no {ASSET}"));
    };
    Ok(Release {
        version,
        asset_url,
        checksum_url: url_of(&format!("{ASSET}.sha256")),
    })
}

/// GET a URL to memory. Anything curl reports becomes text the person can act on.
fn curl(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "25",
            "-H",
            "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .map_err(|e| format!("curl is not installed or cannot run: {e}"))?;
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

/// Download a URL to a file.
fn download(url: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl is not installed or cannot run: {e}"))?;
    if !status.success() {
        return Err(format!("curl exited with {status} downloading the release"));
    }
    Ok(())
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
    let work = std::env::temp_dir().join(format!("qoro-update-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let cleanup = || std::fs::remove_dir_all(&work).ok();
    let failed = |message: String| -> std::io::Error {
        cleanup();
        std::io::Error::other(message)
    };

    let tarball = work.join(ASSET);
    if let Err(detail) = download(&latest.asset_url, &tarball) {
        return Err(failed(format!(
            "qoro update: the download failed: {detail}"
        )));
    }
    if let Some(url) = &latest.checksum_url {
        let checksum_file = work.join(format!("{ASSET}.sha256"));
        match download(url, &checksum_file) {
            Ok(()) => {
                if let Err(detail) = check_sha256(&tarball, &checksum_file) {
                    return Err(failed(format!(
                        "qoro update: refusing: the checksum does not match: {detail}"
                    )));
                }
            }
            Err(detail) => {
                eprintln!("qoro update: warning: no checksum to check against: {detail}");
            }
        }
    }
    let tar = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .current_dir(&work)
        .status()
        .map_err(|e| failed(format!("qoro update: cannot unpack the release: {e}")))?;
    if !tar.success() {
        return Err(failed(format!(
            "qoro update: cannot unpack the release: tar exited {tar}"
        )));
    }
    let fresh = work.join("qoro");
    let reported = Command::new(&fresh)
        .arg("--version")
        .output()
        .map_err(|e| {
            failed(format!(
                "qoro update: the downloaded binary does not run: {e}"
            ))
        })?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("qoro {}", latest.version) {
        return Err(failed(format!(
            "qoro update: refusing: the download reports {reported:?}, want {:?}",
            format!("qoro {}", latest.version)
        )));
    }
    let exe = std::env::current_exe()
        .map_err(|e| failed(format!("qoro update: cannot find this binary: {e}")))?;
    std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
    let staged = exe.with_extension("new");
    std::fs::copy(&fresh, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let _ = std::fs::remove_file(&staged);
        return Err(failed(format!("qoro update: cannot replace {exe:?}: {e}")));
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

/// Compare the tarball against its `.sha256` file with shasum, when there is one.
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
        .map_err(|e| format!("shasum is not installed: {e}"))?;
    if !out.status.success() {
        return Err("shasum failed".to_string());
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
    use super::fetch_latest_parsing;

    #[test]
    fn the_release_json_yields_a_version_and_an_asset_url() {
        let body = serde_json::json!({
            "tag_name": "v0.4.0",
            "assets": [
                {"name": "qoro-aarch64-apple-darwin.tar.gz", "browser_download_url": "https://example.invalid/qoro.tgz"},
                {"name": "qoro-aarch64-apple-darwin.tar.gz.sha256", "browser_download_url": "https://example.invalid/qoro.tgz.sha256"},
            ],
        });
        let release = fetch_latest_parsing(&body).expect("the fixture is well formed");
        assert_eq!(release.version, "0.4.0");
        assert_eq!(release.asset_url, "https://example.invalid/qoro.tgz");
        assert!(release.checksum_url.is_some());
    }

    #[test]
    fn a_release_without_the_macos_asset_is_refused() {
        let body = serde_json::json!({"tag_name": "v0.4.0", "assets": []});
        assert!(fetch_latest_parsing(&body).is_err());
    }

    #[test]
    fn a_non_version_tag_is_refused() {
        let body = serde_json::json!({"tag_name": "nightly", "assets": []});
        assert!(fetch_latest_parsing(&body).is_err());
    }
}

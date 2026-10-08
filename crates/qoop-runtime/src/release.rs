//! GitHub Releases: which tarball a platform uses, how to find it and fetch it, and
//! whether what arrived is what was published.
//!
//! Shared by `qoop update`, which replaces this binary, and by the interface when it puts
//! qoop on a machine being connected. Both go through the same token handling and the
//! same checksum, so neither can be the weaker way in.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::paths;

/// The repository whose Releases carry the binaries.
pub const RELEASES: &str = "https://api.github.com/repos/pat/qoop/releases";

/// Where the releases are read from instead, when set: a mirror, or a directory given as a
/// `file://` URL holding a `latest` file in the API's own shape. The same variable
/// `scripts/install.sh` reads, so a platform pick can be tested with no network at all.
pub const RELEASE_BASE_ENV: &str = "QOOP_RELEASE_BASE";

/// The tarball the release script packs for a platform, named by its target triple.
///
/// `None` is a platform no binary is built for, which is refused with the way to build
/// one rather than with a download that could never run.
pub fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("macos", "aarch64") => "qoop-aarch64-apple-darwin.tar.gz",
        ("macos", "x86_64") => "qoop-x86_64-apple-darwin.tar.gz",
        ("linux", "x86_64") => "qoop-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "qoop-aarch64-unknown-linux-gnu.tar.gz",
        _ => return None,
    })
}

/// What the newest release is: its version and where this platform's asset lives.
pub struct Release {
    pub version: String,
    /// The tarball's own name, which is also the name its checksum file is written for.
    pub asset: String,
    pub asset_url: String,
    pub checksum_url: Option<String>,
}

/// Read the newest release off the GitHub API with curl, which is what keeps this
/// updater free of HTTP dependencies. curl honours the proxy environment, so a
/// blackholed network fails here rather than hanging.
pub fn fetch_latest(asset: &str) -> Result<Release, String> {
    fetch_release("latest", asset)
}

/// The release of exactly `version`, which is what goes onto another machine: the one
/// this binary is, so the two speak the same codec, rather than whatever is newest.
pub fn fetch_tagged(version: &str, asset: &str) -> Result<Release, String> {
    fetch_release(&format!("tags/v{version}"), asset)
        .map_err(|e| format!("no release of qoop {version} could be read: {e}"))
}

fn fetch_release(which: &str, asset: &str) -> Result<Release, String> {
    let base = crate::legacy::var(RELEASE_BASE_ENV)
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| RELEASES.to_string());
    let body = curl(&format!("{}/{which}", base.trim_end_matches('/')))?;
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| format!("the releases API answered what is not JSON: {e}"))?;
    fetch_latest_parsing(&json, asset)
}

/// Which of the platforms a binary is built for `uname -s` and `uname -m` name, as the
/// `(os, arch)` pair [`asset_for`] takes. Anything else is `None`.
pub fn platform(system: &str, machine: &str) -> Option<(&'static str, &'static str)> {
    let arch = match machine {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => return None,
    };
    match system {
        "Linux" => Some(("linux", arch)),
        "Darwin" => Some(("macos", arch)),
        _ => None,
    }
}

/// The `qoop` binary out of a release's tarball, as bytes, checked against the release's
/// own checksum first.
///
/// The checksum is required here, unlike for `qoop update`: this binary goes to a machine
/// where nothing else will look at it. What it proves is that the bytes are the ones the
/// release lists (no corruption, no truncation, no swapped answer on the way), not who made
/// the release. Everything happens in a private directory that is removed on return, and
/// the bytes returned are the very ones that were hashed.
pub fn fetch_binary(release: &Release) -> Result<Vec<u8>, String> {
    let Some(checksum_url) = &release.checksum_url else {
        return Err(format!(
            "the release lists no checksum for {}",
            release.asset
        ));
    };
    let dir = tempfile::tempdir().map_err(|e| format!("cannot make a temporary directory: {e}"))?;
    let tarball = dir.path().join(&release.asset);
    let sums = dir.path().join(format!("{}.sha256", release.asset));
    download(&release.asset_url, &tarball).map_err(|e| format!("the download failed: {e}"))?;
    download(checksum_url, &sums).map_err(|e| format!("the checksum did not download: {e}"))?;
    check_sha256(&tarball, &sums).map_err(|e| format!("the checksum does not match: {e}"))?;
    let unpacked = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("qoop")
        .current_dir(dir.path())
        .status()
        .map_err(|e| format!("cannot unpack the release: {e}"))?;
    if !unpacked.success() {
        return Err(format!("cannot unpack the release: tar exited {unpacked}"));
    }
    // A plain file, not a link: a tarball whose `qoop` points at `../../.ssh/id_ed25519` would
    // otherwise send this machine's key to the other one as the binary.
    let binary = dir.path().join("qoop");
    let plain = std::fs::symlink_metadata(&binary).is_ok_and(|m| m.file_type().is_file());
    if !plain {
        return Err("the release holds no plain qoop file".to_string());
    }
    std::fs::read(&binary).map_err(|e| format!("the release holds no qoop: {e}"))
}

/// The version and the asset URL out of one releases API answer, so the parsing is
/// testable without the network.
pub fn fetch_latest_parsing(json: &serde_json::Value, asset: &str) -> Result<Release, String> {
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
        return Err(format!("the release carries no {asset}"));
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
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
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
    // In a directory of its own, private and unpredictably named: a name another user of
    // a shared /tmp can guess is a file they can put there first and fill with an answer
    // of their own, which on this path chooses what gets downloaded and installed.
    let spare_dir =
        tempfile::tempdir().map_err(|e| format!("cannot make a temporary directory: {e}"))?;
    let spare = spare_dir.path().join("answer");
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
pub fn github_token() -> Option<String> {
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

/// Compare the tarball against its `.sha256` file, when there is one.
///
/// `shasum` where it exists (every Mac), `sha256sum` where it does not (the slim Linux
/// images, which ship coreutils and no perl): a missing tool is not a mismatch, and
/// reading it as one refused every update on those machines.
pub fn check_sha256(tarball: &Path, checksum_file: &Path) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    const MAC_ARM: &str = "qoop-aarch64-apple-darwin.tar.gz";

    #[test]
    fn every_platform_a_binary_is_built_for_names_its_own_tarball() {
        assert_eq!(asset_for("macos", "aarch64"), Some(MAC_ARM));
        assert_eq!(
            asset_for("macos", "x86_64"),
            Some("qoop-x86_64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "x86_64"),
            Some("qoop-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_for("linux", "aarch64"),
            Some("qoop-aarch64-unknown-linux-gnu.tar.gz")
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
                {"name": "qoop-x86_64-unknown-linux-gnu.tar.gz", "url": "https://example.invalid/linux.tgz"},
            ],
        });
        let release =
            fetch_latest_parsing(&body, "qoop-x86_64-unknown-linux-gnu.tar.gz").expect("linux");
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
                {"name": "qoop-aarch64-apple-darwin.tar.gz",
                 "url": "https://api.github.com/repos/pat/qoop/releases/assets/1",
                 "browser_download_url": "https://example.invalid/qoop.tgz"},
                {"name": "qoop-aarch64-apple-darwin.tar.gz.sha256",
                 "url": "https://api.github.com/repos/pat/qoop/releases/assets/2",
                 "browser_download_url": "https://example.invalid/qoop.tgz.sha256"},
            ],
        });
        let release = fetch_latest_parsing(&body, MAC_ARM).expect("the fixture is well formed");
        assert_eq!(release.version, "0.4.0");
        // The API url, because the browser one 404s for a private repository even
        // with a token.
        assert_eq!(
            release.asset_url,
            "https://api.github.com/repos/pat/qoop/releases/assets/1"
        );
        assert_eq!(
            release.checksum_url.as_deref(),
            Some("https://api.github.com/repos/pat/qoop/releases/assets/2")
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

    #[test]
    fn uname_names_map_to_the_platforms_binaries_are_built_for() {
        assert_eq!(platform("Linux", "x86_64"), Some(("linux", "x86_64")));
        assert_eq!(platform("Linux", "amd64"), Some(("linux", "x86_64")));
        assert_eq!(platform("Linux", "aarch64"), Some(("linux", "aarch64")));
        assert_eq!(platform("Darwin", "arm64"), Some(("macos", "aarch64")));
        assert_eq!(platform("Darwin", "x86_64"), Some(("macos", "x86_64")));
        assert_eq!(platform("FreeBSD", "amd64"), None);
        assert_eq!(platform("Linux", "riscv64"), None);
    }

    /// A tarball packed the way the release script packs one, with its checksum file,
    /// both reachable as `file://` URLs.
    fn packed(dir: &Path, content: &[u8], sums_for: Option<&[u8]>) -> Release {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::write(src.join("qoop"), content).expect("binary");
        let tarball = dir.join("qoop-x86_64-unknown-linux-gnu.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("qoop")
            .current_dir(&src)
            .status()
            .expect("tar");
        assert!(status.success());
        let checksum_url = sums_for.map(|other| {
            let hashed = if other.is_empty() {
                tarball.clone()
            } else {
                let p = dir.join("other");
                std::fs::write(&p, other).expect("other");
                p
            };
            let out = Command::new("shasum")
                .args(["-a", "256"])
                .arg(&hashed)
                .output()
                .expect("shasum");
            let sums = dir.join("qoop-x86_64-unknown-linux-gnu.tar.gz.sha256");
            std::fs::write(&sums, &out.stdout).expect("sums");
            format!("file://{}", sums.display())
        });
        Release {
            version: "0.2.0".into(),
            asset: "qoop-x86_64-unknown-linux-gnu.tar.gz".into(),
            asset_url: format!("file://{}", tarball.display()),
            checksum_url,
        }
    }

    #[test]
    fn a_checked_release_yields_exactly_the_binary_inside_it() {
        let dir = tempfile::tempdir().expect("dir");
        let release = packed(dir.path(), b"\x7fELF not really", Some(b""));
        assert_eq!(
            fetch_binary(&release).expect("fetched"),
            b"\x7fELF not really"
        );
    }

    /// Nothing goes to another machine unchecked: no checksum, or one that does not match,
    /// is a refusal.
    #[test]
    fn a_release_without_a_matching_checksum_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let missing = packed(dir.path(), b"binary", None);
        assert!(fetch_binary(&missing).is_err());
        let dir = tempfile::tempdir().expect("dir");
        let wrong = packed(dir.path(), b"binary", Some(b"something else"));
        let err = fetch_binary(&wrong).expect_err("refused");
        assert!(err.contains("checksum"), "{err}");
    }

    /// A tarball whose `qoop` is a link would have whatever it points at read and sent as
    /// the binary. Refused, checksum or not.
    #[test]
    fn a_release_whose_qoop_is_a_link_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::write(dir.path().join("secret"), b"a private key").expect("secret");
        std::os::unix::fs::symlink("../secret", src.join("qoop")).expect("link");
        let tarball = dir.path().join("qoop-x86_64-unknown-linux-gnu.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("qoop")
            .current_dir(&src)
            .status()
            .expect("tar");
        assert!(status.success());
        let out = Command::new("shasum")
            .args(["-a", "256"])
            .arg(&tarball)
            .output()
            .expect("shasum");
        let sums = dir.path().join("sums");
        std::fs::write(&sums, &out.stdout).expect("sums");
        let release = Release {
            version: "0.2.0".into(),
            asset: "qoop-x86_64-unknown-linux-gnu.tar.gz".into(),
            asset_url: format!("file://{}", tarball.display()),
            checksum_url: Some(format!("file://{}", sums.display())),
        };
        let err = fetch_binary(&release).expect_err("refused");
        assert!(err.contains("plain"), "{err}");
    }
}

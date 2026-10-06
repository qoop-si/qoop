//! Putting a launchd service back on a program file that was replaced.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Take the service down, wait until launchd has forgotten it, and load `plist` again.
///
/// Not `kickstart -k`, and not a crash restart: macOS pins a registered agent to the hash of
/// the file it ran, so once that file is replaced launchd refuses to start the agent again
/// (exit 78, `EX_CONFIG`, every ten seconds, for good). A fresh bootstrap pins the file that
/// is there now. `bootout` can return before the service is gone, and a bootstrap into that
/// gap fails with an I/O error, so the label is watched until launchd no longer knows it.
pub fn reload(label: &str, plist: &Path) -> std::io::Result<()> {
    let domain = format!("gui/{}", qoro_runtime::paths::current_uid());
    let service = format!("{domain}/{label}");
    let _ = Command::new("launchctl")
        .args(["bootout", &service])
        .output();
    for _ in 0..20 {
        let known = Command::new("launchctl")
            .args(["print", &service])
            .output()
            .is_ok_and(|o| o.status.success());
        if !known {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = Command::new("launchctl")
        .args(["bootstrap", &domain])
        .arg(plist)
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "launchctl bootstrap: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

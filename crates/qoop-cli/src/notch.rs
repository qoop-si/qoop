//! `qoop notch`: on a Mac, the agents in the camera housing at the top of the screen.
//!
//! The app itself is Swift (`notch/main.swift`), built into this binary by `build.rs`
//! and written out on first run, so it is always the version of the qoop that starts it
//! and needs no download of its own. It asks for no permission and carries no signature
//! beyond the one its linker gives every Apple silicon binary: a file qoop writes has no
//! quarantine flag, so Gatekeeper has nothing to stop.
//!
//! `install` makes it a login item through a LaunchAgent that runs `qoop notch run`,
//! the hooks and skills shape: `status` says what is there, `uninstall` takes out
//! exactly what was put in.

/// `qoop notch ...`.
pub fn run(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop notch install|uninstall|status\n       qoop notch run";
    #[cfg(target_os = "macos")]
    {
        match args.get(2).map(String::as_str) {
            Some("run") => mac::run_app(),
            Some("install") => mac::install(),
            Some("uninstall") => mac::uninstall(),
            Some("status") | None => mac::status(),
            Some(other) => Err(std::io::Error::other(format!(
                "unknown command {other}; {help}"
            ))),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(std::io::Error::other(format!(
            "the notch is a macOS feature; {help}"
        )))
    }
}

/// Put the notch back on the binary now on disk, through a fresh launchd registration (a
/// kickstart is refused for an agent whose file was replaced). What `qoop update` calls once
/// it has swapped the binary: silent, and nothing when the notch was never installed.
pub fn restart_if_installed() {
    #[cfg(target_os = "macos")]
    if let Some(plist) = mac::plist_path().filter(|p| p.exists()) {
        let _ = crate::launchd::reload(mac::LABEL, &plist);
    }
}

/// FNV-1a over the app's bytes: the name of the file it is written to, so a new build
/// lands beside the old one instead of over a file a running notch has open.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A path as XML text, for the one plist this writes.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
mod mac {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    static APP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/qoop-notch"));

    pub const LABEL: &str = "qoop.notch";

    fn home() -> std::io::Result<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("HOME is not set"))
    }

    pub fn plist_path() -> Option<PathBuf> {
        home().ok().map(|h| {
            h.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"))
        })
    }

    pub fn service() -> String {
        format!("gui/{}/{LABEL}", qoop_runtime::paths::current_uid())
    }

    fn cache_dir() -> std::io::Result<PathBuf> {
        qoop_runtime::paths::cache_dir().ok_or_else(|| std::io::Error::other("no HOME"))
    }

    /// Write the app out when this build's copy is not there yet, and become it.
    pub fn run_app() -> std::io::Result<()> {
        // A login item installed when a machine had named sessions carries one in its
        // environment, and every `qoop` the notch starts would refuse it. There is one
        // daemon now: the notch follows it.
        // SAFETY: before the app, or anything else here, starts a thread.
        unsafe { std::env::remove_var(qoop_runtime::paths::SESSION_ENV) };
        let exe = std::env::current_exe()?;
        let dir = cache_dir()?;
        let app = dir.join(format!("notch-{:016x}", super::fnv(APP)));
        if !app.exists() {
            std::fs::create_dir_all(&dir)?;
            // Other builds' copies go: nothing names them any more, and one still
            // running keeps its file open until it exits. This build's own, and another
            // start's half written one, stay: two starts at once must not undo each other.
            let own = app.file_name().map(|n| n.to_os_string());
            for entry in std::fs::read_dir(&dir)?.flatten() {
                let name = entry.file_name();
                let text = name.to_string_lossy();
                // A half written one older than a few minutes is a start that died.
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() > 600));
                if text.starts_with("notch-")
                    && (!text.ends_with(".new") || stale)
                    && Some(&name) != own.as_ref()
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
            let staged = dir.join(format!("notch-{}.new", std::process::id()));
            std::fs::write(&staged, APP)?;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
            // Another start may have put the same bytes there first, which is as good.
            if let Err(e) = std::fs::rename(&staged, &app) {
                let _ = std::fs::remove_file(&staged);
                if !app.exists() {
                    return Err(e);
                }
            }
        }
        // Only returns on failure.
        Err(Command::new(&app).arg(&exe).exec())
    }

    /// The major version of this macOS, from `sw_vers`.
    fn macos_major() -> Option<u32> {
        let out = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .split('.')
            .next()?
            .parse()
            .ok()
    }

    pub(super) fn plist(exe: &str) -> String {
        // The launchd environment has none of the shell's. What moves where the daemon's
        // socket lives and where the pet was picked is carried over, or the notch would
        // look somewhere the install did not.
        let env: String = ["QOOP_RUNTIME_DIR", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME"]
            .iter()
            .filter_map(|k| {
                std::env::var(k)
                    .ok()
                    .filter(|v| !v.is_empty())
                    .map(|v| (*k, v))
            })
            .map(|(k, v)| format!("    <key>{k}</key><string>{}</string>\n", super::xml(&v)))
            .collect();
        let env = if env.is_empty() {
            String::new()
        } else {
            format!("  <key>EnvironmentVariables</key>\n  <dict>\n{env}  </dict>\n")
        };
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key><string>{LABEL}</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array><string>{}</string><string>notch</string><string>run</string></array>\n\
             {env}\
             \x20 <key>RunAtLoad</key><true/>\n\
             \x20 <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
             \x20 <key>LimitLoadToSessionType</key><string>Aqua</string>\n\
             \x20 <key>ProcessType</key><string>Interactive</string>\n\
             </dict>\n\
             </plist>\n",
            super::xml(exe)
        )
    }

    pub fn install() -> std::io::Result<()> {
        match macos_major() {
            Some(v) if v >= 14 => {}
            _ => {
                return Err(std::io::Error::other(
                    "the notch needs macOS 14 or newer; nothing was installed",
                ));
            }
        }
        // Installing is asking for the notch: one turned off in its settings comes back.
        // A file that cannot be written is said, not a reason to install nothing.
        let read = qoop_tui::config::read();
        if !read.config.notch
            && let Err(why) = qoop_tui::config::set(&read.path, "notch.enabled", "true")
        {
            eprintln!("qoop: notch.enabled stays false ({why}); fix it with qoop config");
        }
        let exe = std::env::current_exe()?;
        let plist_path = plist_path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
        let dir = plist_path.parent().expect("a file under LaunchAgents");
        std::fs::create_dir_all(dir)?;
        let staged = plist_path.with_extension("plist.new");
        std::fs::File::create(&staged)?.write_all(plist(&exe.to_string_lossy()).as_bytes())?;
        std::fs::rename(&staged, &plist_path)?;
        // A notch already loaded is taken down first, so a second install picks up a
        // moved binary rather than failing on a label that is taken.
        crate::launchd::reload(LABEL, &plist_path).map_err(|e| {
            std::io::Error::other(format!(
                "wrote {} but launchd would not start it: {e}",
                plist_path.display()
            ))
        })?;
        println!("the notch is on, and starts at login; qoop notch uninstall takes it out");
        Ok(())
    }

    pub fn uninstall() -> std::io::Result<()> {
        let _ = Command::new("launchctl")
            .args(["bootout", &service()])
            .output();
        let mut removed = false;
        if let Some(p) = plist_path()
            && p.exists()
        {
            std::fs::remove_file(&p)?;
            removed = true;
        }
        if let Ok(dir) = cache_dir()
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("notch-") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        println!(
            "{}",
            if removed {
                "the notch is off and no longer starts at login"
            } else {
                "the notch was not installed"
            }
        );
        Ok(())
    }

    pub fn status() -> std::io::Result<()> {
        let Some(path) = plist_path().filter(|p| p.exists()) else {
            println!("not installed: qoop notch install puts your agents in the notch");
            return Ok(());
        };
        let running = Command::new("launchctl")
            .args(["print", &service()])
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("state = running")
            });
        println!(
            "installed at {}, {}",
            path.display(),
            if running { "running" } else { "not running" }
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name a build's copy is written under depends on every byte and nothing else.
    #[test]
    fn the_copy_name_follows_the_bytes() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv(b"notch 1"), fnv(b"notch 2"));
    }

    /// The login item runs this binary's notch and names no session: there is one daemon.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_runs_the_notch_and_names_no_session() {
        let plist = mac::plist("/bin/qoop");
        assert!(plist.contains("<string>/bin/qoop</string><string>notch</string>"));
        assert!(!plist.contains("QOOP_SESSION"), "{plist}");
    }

    #[test]
    fn a_path_is_escaped_for_the_plist() {
        assert_eq!(xml("/Users/a&b/<q>"), "/Users/a&amp;b/&lt;q&gt;");
    }
}

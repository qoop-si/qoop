//! `qoop uninstall`: the Uninstall steps of `site/docs/update.md`, in their order, as one command.
//!
//! Everything that can be refused is refused before the first step runs, so a refusal leaves
//! the machine as it was. After that the steps stop at the first one that fails: each is safe to
//! run again, and nothing after a failed step (above all the key and the directories) is touched.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use qoop_runtime::paths;

use crate::Phone;
use crate::places::{human, size};

const USAGE: &str = "usage: qoop uninstall [--yes]\n\
    take qoop off this machine: the notch (macOS), the hooks, the skills, the account link, the \
    daemon, qoop's directories and the qoop binary. It lists them and asks first; `--yes` skips \
    the question. A paired phone must be removed first (`qoop devices rm <name>`).";

/// A bridge service file: left behind after `logout`, a phone is still paired.
const SERVICE_FILES: [&str; 4] = [
    "Library/LaunchAgents/si.qoop.bridge.plist",
    "Library/LaunchAgents/dev.qoro.bridge.plist",
    ".config/systemd/user/qoop-bridge.service",
    ".config/systemd/user/qoro-bridge.service",
];

fn fail(msg: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// What the command removes, worked out and checked before anything runs.
struct Plan {
    home: PathBuf,
    /// Directories, in the order of the documented steps.
    dirs: Vec<PathBuf>,
    /// Single files: the old name's link.
    files: Vec<PathBuf>,
    /// The `qoop` entry on PATH that is the running binary.
    binary: PathBuf,
}

pub fn run(args: &[String], phone: Option<Phone>) -> std::io::Result<()> {
    let mut yes = false;
    for a in &args[2..] {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--yes" | "-y" => yes = true,
            _ => return Err(fail(format!("unknown argument {a}\n{USAGE}"))),
        }
    }
    let plan = plan(phone.is_some())?;
    list(&plan);
    if !yes && !confirmed()? {
        println!("nothing was removed");
        return Ok(());
    }
    steps(&plan, phone)
}

fn plan(has_phone: bool) -> std::io::Result<Plan> {
    if qoop_runtime::legacy::var("QOOP_TERMINAL").is_ok() {
        return Err(fail(
            "run this from a plain terminal, not inside a qoop tab: it stops the daemon that \
             hosts the tab, and the run would end half way",
        ));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or_else(|| fail("no home directory to uninstall from"))?;
    let exe = std::env::current_exe()?;
    let binary = on_path(&exe).ok_or_else(|| {
        fail(format!(
            "{} is not the qoop on your PATH; run the installed one, so the copy that stays is \
             never a surprise",
            exe.display()
        ))
    })?;
    if !has_phone && paths::data_dir().is_some_and(|d| d.join("machine.key").exists()) {
        return Err(fail(
            "this machine is linked to an account and this build cannot log out of it; run the \
             qoop release build",
        ));
    }
    let runtime = paths::runtime_dir().map_err(fail)?;
    let mut dirs = vec![paths::config_dir(), paths::state_location(&runtime)];
    dirs.extend(paths::data_dir());
    dirs.extend(paths::cache_dir());
    dirs.extend(paths::logs_dir());
    if cfg!(target_os = "macos") {
        dirs.push(home.join(".cache/qoop"));
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    for dir in &dirs {
        guard(dir, &home).map_err(fail)?;
    }
    Ok(Plan {
        files: vec![home.join(".local/bin/qoro")],
        home,
        dirs,
        binary,
    })
}

/// A directory this command may delete: absolute, not the root or the home, not above the
/// home, and at least three levels deep.
fn guard(dir: &Path, home: &Path) -> Result<(), String> {
    let deep = dir
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    let reason = if !dir.is_absolute() || dir.components().any(|c| c == Component::ParentDir) {
        "is not an absolute path"
    } else if dir == home || home.starts_with(dir) {
        "is your home directory or above it"
    } else if deep < 3 {
        "is too shallow to be qoop's own directory"
    } else {
        return Ok(());
    };
    Err(format!("refusing to remove {}: it {reason}", dir.display()))
}

/// The first `qoop` on PATH that is the running binary, as it is written there (a link stays a
/// link: only the entry goes, never what it points at).
fn on_path(exe: &Path) -> Option<PathBuf> {
    let exe = exe.canonicalize().ok()?;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("qoop"))
        .find(|c| c.canonicalize().is_ok_and(|real| real == exe))
}

fn list(plan: &Plan) {
    println!("this removes:");
    for dir in &plan.dirs {
        println!("  {:>6}  {}", human(size(dir)), dir.display());
    }
    println!(
        "          {} (the old name, if it is there)",
        plan.files[0].display()
    );
    println!("          {}", plan.binary.display());
    println!(
        "and takes out the notch (macOS), the hooks and skills qoop added, the account link, \
         and stops the daemon. qass's lessons and its folder go with config and data."
    );
}

fn confirmed() -> std::io::Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(fail("not a terminal: say yes with `qoop uninstall --yes`"));
    }
    print!("remove all of it? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn args_for(command: &[&str]) -> Vec<String> {
    std::iter::once("qoop")
        .chain(command.iter().copied())
        .map(String::from)
        .collect()
}

fn steps(plan: &Plan, phone: Option<Phone>) -> std::io::Result<()> {
    let stage = |name: &str, r: std::io::Result<()>| {
        r.map_err(|e| {
            fail(format!(
                "stopped at {name}: {e}\nnothing after it was touched; fix that and run \
                 `qoop uninstall` again"
            ))
        })
    };
    if cfg!(target_os = "macos") {
        stage(
            "the notch",
            crate::notch::run(&args_for(&["notch", "uninstall"])),
        )?;
    }
    stage(
        "the hooks",
        crate::hooks::run(&args_for(&["hooks", "uninstall"])),
    )?;
    stage(
        "the skills",
        crate::skills::run(&args_for(&["skills", "uninstall"])),
    )?;
    if let Some(phone) = phone {
        stage("logout", phone(&args_for(&["logout"])))?;
    }
    stage("the bridge service", no_service(&plan.home))?;
    stage("stop", stop())?;
    for dir in &plan.dirs {
        stage(&dir.display().to_string(), remove(dir))?;
    }
    for file in &plan.files {
        stage(&file.display().to_string(), remove(file))?;
    }
    println!("removing {}", plan.binary.display());
    stage("the binary", remove(&plan.binary))
}

/// `qoop logout` takes the bridge service away when no phone is paired; one left means a phone
/// still is, and the service would keep restarting a binary this command is about to delete.
fn no_service(home: &Path) -> std::io::Result<()> {
    match SERVICE_FILES
        .iter()
        .map(|f| home.join(f))
        .find(|p| p.exists())
    {
        Some(file) => Err(fail(format!(
            "the bridge service ({}) is still installed, so a phone is still paired: \
             `qoop devices ls`, then `qoop devices rm <name>` for each",
            file.display()
        ))),
        None => Ok(()),
    }
}

/// `qoop stop`, and then the daemon's socket no longer answers, so the directories are not
/// deleted under a daemon that is still writing to them.
fn stop() -> std::io::Result<()> {
    crate::run_stop(&args_for(&["stop"]))?;
    let socket = paths::socket_path(&paths::runtime_dir().map_err(fail)?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        if Instant::now() > deadline {
            return Err(fail("the daemon is still running"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// A directory goes whole, a link or file goes alone; what is not there is not an error.
fn remove(path: &Path) -> std::io::Result<()> {
    let result = match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_refuses_what_is_not_qoops_own_directory() {
        let home = Path::new("/Users/pat");
        for bad in [
            "/",
            "/Users",
            "/Users/pat",
            "/Users/pat/..",
            "relative/qoop",
            "/Users/qoop",
        ] {
            assert!(guard(Path::new(bad), home).is_err(), "{bad}");
        }
        for good in [
            "/Users/pat/.config/qoop",
            "/Users/pat/Library/Application Support/qoop",
            "/tmp/stack/d/qoop",
        ] {
            assert!(guard(Path::new(good), home).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_link_is_removed_and_what_it_points_at_stays() {
        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("inner")).expect("real");
        std::fs::write(real.join("inner/file"), "x").expect("file");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        remove(&link).expect("remove the link");
        assert!(!link.exists() && std::fs::symlink_metadata(&link).is_err());
        assert!(real.join("inner/file").exists());
        remove(&link).expect("already gone is fine");
    }
}

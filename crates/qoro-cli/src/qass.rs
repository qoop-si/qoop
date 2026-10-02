//! `qoro qass`: open qass, the assistant over every agent, in one step.
//!
//! The one already running is shown; one asleep in the `qass` space is woken; otherwise a
//! new one starts in its own folder (`qoro paths qass-data`, which holds the gate's
//! settings) on the model `config.toml` names (`[qass] model`), already running `/qass`.
//! Closing it is the `x` on its row, or `qoro close`.

use std::path::Path;
use std::process::Command;

use qoro_runtime::paths;
use qoro_tui::config;

use crate::supervise;

/// The space qass lives in.
const SPACE: &str = "qass";

/// Another qoro command's JSON, from this same binary.
fn json(args: &[&str]) -> Option<serde_json::Value> {
    let me = std::env::current_exe().ok()?;
    let out = Command::new(me).args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Another qoro command run for its effect, failing with what it said.
fn run(args: &[&str]) -> std::io::Result<()> {
    let out = Command::new(std::env::current_exe()?).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

/// The terminal of a running qass: an agent whose folder is qass's own.
fn running(folder: &Path) -> Option<u64> {
    let folder = folder.canonicalize().ok()?;
    json(&["agents", "--json"])?
        .as_array()?
        .iter()
        .filter(|row| row["agent"] == true)
        .find(|row| {
            row["cwd"]
                .as_str()
                .and_then(|cwd| Path::new(cwd).canonicalize().ok())
                .is_some_and(|cwd| cwd == folder)
        })
        .and_then(|row| row["terminal"].as_u64())
}

/// A tab asleep in the `qass` space.
fn asleep() -> Option<u64> {
    json(&["ls", "--json"])?
        .as_array()?
        .iter()
        .find(|space| space["name"] == SPACE)?["sleeping"]
        .as_array()?
        .first()?
        .as_u64()
}

pub fn run_qass(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(crate::usage("usage: qoro qass"));
    }
    let folder = paths::qass_data_dir()
        .ok_or_else(|| std::io::Error::other("no HOME to keep qass's folder under"))?;
    if !folder.join(".claude/settings.json").exists() {
        return Err(std::io::Error::other(
            "qass is not installed here; run `qoro skills install claude` first",
        ));
    }
    let terminal = if let Some(terminal) = running(&folder) {
        println!("qass is terminal {terminal}");
        terminal
    } else if let Some(terminal) = asleep() {
        run(&["wake", "--tab", &terminal.to_string()])?;
        println!("qass woke in terminal {terminal}");
        terminal
    } else {
        let model = config::read().config.qass_model;
        let opts = supervise::NewOpts {
            name: Some("qass".into()),
            cwd: Some(folder),
            space: Some(SPACE.into()),
            command: ["claude", "--model", model, "/qass"]
                .map(String::from)
                .to_vec(),
        };
        let mut opened = 0;
        supervise::open_terminal(&opts, |terminal| opened = terminal)?;
        println!("qass started in terminal {opened} on {model}");
        u64::from(opened)
    };
    // Shown where the person is looking; with no interface open, the number says where.
    let _ = run(&["focus", &terminal.to_string()]);
    Ok(())
}

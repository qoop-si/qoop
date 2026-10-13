//! `qoop icons`: the font the interface's few icons are drawn from, put in place when qoop is
//! installed or updated, so a terminal started afterwards has it before its first glyph. See
//! `qoop_tui::icons`.

use qoop_tui::icons::{self, Setting};

const USAGE: &str = "usage: qoop icons <install|uninstall|status>";

const NOT_NEEDED: &str = "not needed here, qoop draws its icons from it on a Mac only";

/// `qoop icons ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("status") | None => status(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; {USAGE}"
        ))),
    }
}

fn install() -> std::io::Result<()> {
    if qoop_tui::config::read().config.icons == Setting::Off {
        println!("icons are off in config.toml: no font installed");
        return Ok(());
    }
    match icons::install_font()? {
        Some(path) => println!("icon font: {}", path.display()),
        None => println!("icon font: {NOT_NEEDED}"),
    }
    Ok(())
}

fn uninstall() -> std::io::Result<()> {
    for path in icons::uninstall_font()? {
        println!("removed {}", path.display());
    }
    Ok(())
}

fn status() -> std::io::Result<()> {
    let state = if !cfg!(target_os = "macos") {
        NOT_NEEDED
    } else if qoop_tui::config::read().config.icons == Setting::Off {
        "none, icons are off in config.toml"
    } else if icons::installed() {
        "installed"
    } else {
        "not installed; qoop icons install puts it in place"
    };
    println!("icon font: {state}");
    Ok(())
}

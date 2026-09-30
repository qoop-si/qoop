//! `qoro config`: the settings in `config.toml`, read and written from the command line.
//!
//! The file stays the one place settings live; the interface, the notch's settings window
//! and a person at a shell all go through here. Writing is one key at a time, so a
//! comment or a key this build does not know is never lost.

use qoro_tui::config;

/// `qoro config ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoro config [--json]\n       qoro config set <key> <value>";
    match args.get(2).map(String::as_str) {
        None => show(false),
        Some("--json") if args.len() == 3 => show(true),
        Some("set") => match (args.get(3), args.get(4), args.len()) {
            (Some(key), Some(value), 5) => {
                config::set(&config::path(), key, value).map_err(std::io::Error::other)?;
                // The notch reads it when it starts, or at once from its own window.
                if key == "notch.enabled" && value == "true" {
                    println!("{key} = {value}; qoro notch install starts it now");
                } else if key == "notch.enabled" {
                    println!(
                        "{key} = {value}; the notch goes at its next start, or now with qoro notch uninstall"
                    );
                } else {
                    println!("{key} = {value}");
                }
                Ok(())
            }
            _ => Err(std::io::Error::other(help)),
        },
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; {help}"
        ))),
    }
}

/// Every setting in effect, its value from the file or the default, and what in the file
/// could not be used. `--json` is for programs: the same keys `set` takes, and what each
/// choice may be.
fn show(json: bool) -> std::io::Result<()> {
    let read = config::read();
    let c = read.config;
    let prefix = format!("ctrl-{}", char::from(b'a' + c.prefix - 1));
    if json {
        let value = serde_json::json!({
            "path": read.path,
            "exists": read.exists,
            "settings": {
                "prefix": prefix,
                "theme": c.theme.name(),
                "notify.desktop": c.desktop,
                "notify.sound": c.sound,
                "notify.done_sound": c.done_sound,
                "notify.alert_sound": c.alert_sound,
                "notch.enabled": c.notch,
            },
            "agents": read.agents.iter().map(|a| serde_json::json!({ "name": a.name, "command": a.command })).collect::<Vec<_>>(),
            "choices": {
                "prefix": config::prefixes(),
                "theme": ["auto", "dark", "light"],
                "notify.done_sound": config::SOUNDS,
                "notify.alert_sound": config::SOUNDS,
            },
            "problems": read.problems.iter().map(|p| p.short()).collect::<Vec<_>>(),
        });
        println!("{value}");
        return Ok(());
    }
    println!(
        "{}{}",
        read.path.display(),
        if read.exists {
            ""
        } else {
            "  (no file yet: every setting is its default)"
        }
    );
    println!("prefix = \"{prefix}\"");
    println!("theme = \"{}\"", c.theme.name());
    println!("notify.desktop = {}", c.desktop);
    println!("notify.sound = {}", c.sound);
    println!("notify.done_sound = \"{}\"", c.done_sound);
    println!("notify.alert_sound = \"{}\"", c.alert_sound);
    println!("notch.enabled = {}", c.notch);
    for a in &read.agents {
        println!("agents.{} = {:?}", a.name, a.command);
    }
    for problem in &read.problems {
        println!("! {}", problem.short());
    }
    Ok(())
}

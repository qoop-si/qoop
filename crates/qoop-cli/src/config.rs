//! `qoop config`: the settings in `config.toml`, read and written from the command line.
//!
//! The file stays the one place settings live; the interface, the notch's settings window
//! and a person at a shell all go through here. Writing is one key at a time, so a
//! comment or a key this build does not know is never lost.

use qoop_tui::config;

/// `qoop config ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    let help = "usage: qoop config [--json]\n       qoop config set <key> <value>";
    match args.get(2).map(String::as_str) {
        None => show(false),
        Some("--json") if args.len() == 3 => show(true),
        Some("set") => match (args.get(3), args.get(4), args.len()) {
            // Usage counts go through the same switch as the settings row: an off reaches the
            // daemon and deletes what was counted, an on is refused inside a qoop terminal.
            (Some(key), Some(value), 5) if key == "telemetry" => match value.as_str() {
                "true" | "false" => {
                    crate::telemetry::switch(value == "true").map_err(std::io::Error::other)?;
                    println!("{key} = {value}");
                    Ok(())
                }
                _ => Err(std::io::Error::other("telemetry is true or false")),
            },
            (Some(key), Some(value), 5) => {
                config::set(&config::path(), key, value).map_err(std::io::Error::other)?;
                // The notch reads it when it starts, or at once from its own window.
                if key == "notch.enabled" && value == "true" {
                    println!("{key} = {value}; qoop notch install starts it now");
                } else if key == "notch.enabled" {
                    println!(
                        "{key} = {value}; the notch goes at its next start, or now with qoop notch uninstall"
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
                "icons": c.icons.name(),
                "copy_on_select": c.copy_on_select,
                "notify.desktop": c.desktop,
                "notify.sound": c.sound,
                "notify.done_sound": c.done_sound,
                "notify.alert_sound": c.alert_sound,
                "notify.mute": c.mute.name(),
                "notch.enabled": c.notch,
                "sleep.idle_minutes": c.lifecycle.idle_minutes,
                "sleep.qass_idle_minutes": c.lifecycle.qass_idle_minutes.unwrap_or(c.lifecycle.idle_minutes),
                "daemon.exit_when_asleep": c.lifecycle.exit_when_asleep,
                "qass.agent": read.qass_agent,
                "qass.models.best": read.qass_models[0],
                "qass.models.mid": read.qass_models[1],
                "qass.models.low": read.qass_models[2],
                "report.send": read.report_send,
            },
            "agents": read.agents.iter().map(|a| serde_json::json!({ "name": a.name, "command": a.command })).collect::<Vec<_>>(),
            "choices": {
                "prefix": config::prefixes(),
                "theme": ["auto", "dark", "light"],
                "icons": ["auto", "on", "off"],
                "notify.done_sound": config::sounds_here(),
                "notify.alert_sound": config::sounds_here(),
                "notify.mute": qoop_runtime::config::MUTES,
                "qass.agent": crate::settings::qass_agents(&read),
                "report.send": config::REPORT_SENDS,
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
    println!("copy_on_select = {}", c.copy_on_select);
    println!("notify.desktop = {}", c.desktop);
    println!("notify.sound = {}", c.sound);
    println!("notify.done_sound = \"{}\"", c.done_sound);
    println!("notify.alert_sound = \"{}\"", c.alert_sound);
    println!("notify.mute = \"{}\"", c.mute.name());
    println!("notch.enabled = {}", c.notch);
    println!("sleep.idle_minutes = {}", c.lifecycle.idle_minutes);
    println!(
        "sleep.qass_idle_minutes = {}",
        c.lifecycle
            .qass_idle_minutes
            .unwrap_or(c.lifecycle.idle_minutes)
    );
    println!("daemon.exit_when_asleep = {}", c.lifecycle.exit_when_asleep);
    println!("qass.agent = \"{}\"", read.qass_agent);
    println!("report.send = \"{}\"", read.report_send);
    for a in &read.agents {
        println!("agents.{} = {:?}", a.name, a.command);
    }
    for problem in &read.problems {
        println!("! {}", problem.short());
    }
    Ok(())
}

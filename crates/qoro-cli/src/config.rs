//! `qoro config`: the settings in `config.toml`, read and written from the command line.
//!
//! The file stays the one place settings live; the interface, the notch's settings window
//! and a person at a shell all go through here. Writing is one key at a time, so a
//! comment or a key this build does not know is never lost.

use std::io::Write as _;

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

/// `qoro sound`: hear each sound and keep one for a finished agent, then one for an agent
/// that waits on you. A number or a name plays it, enter keeps the last one heard, `off`
/// turns sounds off, `q` or EOF stops with nothing more written. Each pick is written as
/// it is kept, so stopping after the first keeps the first.
pub fn run_sound(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoro sound"));
    }
    if !cfg!(target_os = "macos") {
        println!("sounds are picked on macOS only; Linux plays the desktop's own two");
        return Ok(());
    }
    let path = config::path();
    let c = config::read().config;
    let mut input = std::io::stdin().lock();
    for (key, what, now) in [
        ("notify.done_sound", "an agent finished", c.done_sound),
        ("notify.alert_sound", "an agent waits on you", c.alert_sound),
    ] {
        println!("\n{what}, now {now}");
        for (i, name) in config::SOUNDS.iter().enumerate() {
            print!("{:>3} {name:<10}", i + 1);
            if i % 7 == 6 {
                println!();
            }
        }
        let mut heard = now;
        print!("a number plays it, enter keeps {heard}, off for silence, q quits");
        loop {
            print!("\n> ");
            std::io::stdout().flush()?;
            let Some(line) = crate::avatar::read_line(&mut input)? else {
                println!();
                return Ok(());
            };
            match parse_sound(&line) {
                Sound::Play(name) => {
                    heard = name;
                    if let Some(play) = qoro_tui::notify::sound(false, true, name, name) {
                        qoro_tui::notify::spawn(&play);
                    }
                    print!("{name}, enter keeps it");
                }
                Sound::Keep => break,
                Sound::Off => {
                    config::set(&path, "notify.sound", "false").map_err(std::io::Error::other)?;
                    println!("notify.sound = false");
                    return Ok(());
                }
                Sound::Quit => {
                    println!();
                    return Ok(());
                }
                Sound::Invalid => print!("1 to 14 or a name plays one, enter keeps {heard}"),
            }
        }
        config::set(&path, key, heard).map_err(std::io::Error::other)?;
        config::set(&path, "notify.sound", "true").map_err(std::io::Error::other)?;
        println!("{key} = \"{heard}\"");
    }
    println!("a running qoro takes it when its settings open (prefix, space, settings)");
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Sound {
    Play(&'static str),
    Keep,
    Off,
    Quit,
    Invalid,
}

/// A number (1 to 14) or a sound's name in any case plays it, an empty line keeps.
fn parse_sound(line: &str) -> Sound {
    let line = line.trim();
    match line.to_lowercase().as_str() {
        "" => Sound::Keep,
        "off" => Sound::Off,
        "q" | "quit" => Sound::Quit,
        other => other
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| config::SOUNDS.get(i))
            .or_else(|| config::SOUNDS.iter().find(|s| s.eq_ignore_ascii_case(line)))
            .map_or(Sound::Invalid, |s| Sound::Play(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_plays_keeps_or_stops() {
        assert_eq!(parse_sound("1"), Sound::Play("Basso"));
        assert_eq!(parse_sound("14"), Sound::Play("Tink"));
        assert_eq!(parse_sound(" tink "), Sound::Play("Tink"));
        assert_eq!(parse_sound(""), Sound::Keep);
        assert_eq!(parse_sound("OFF"), Sound::Off);
        assert_eq!(parse_sound("q"), Sound::Quit);
        for bad in ["0", "15", "-1", "glas"] {
            assert_eq!(parse_sound(bad), Sound::Invalid, "{bad}");
        }
    }
}

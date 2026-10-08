//! `qoop sound`: hear the notification sounds and keep one for a finished agent, then one
//! for an agent that waits on you.
//!
//! On a terminal it is a grid: the arrows (or `hjkl`) move and play the sound under the
//! cursor, enter keeps it, space plays it again, `o` turns sounds off, `q` or esc stops.
//! Piped, it reads lines instead: a number or a name plays one, an empty line keeps, `off`
//! and `q` as above, and EOF stops, so a script never hangs on it.
//!
//! macOS offers its own sounds, Linux the freedesktop ones this machine has. A name is
//! kept as written: a Mac name in a file shared with Linux stays when enter keeps it, and
//! only what plays here is shown.

use std::io::{IsTerminal, Write};
use std::sync::mpsc;

use qoop_tui::config;
use qoop_tui::notify;

use crate::page::{Key, Player, Session, next_keys};

/// What one step ended with.
enum Pick {
    Keep(&'static str),
    Off,
    Quit,
}

/// `qoop sound`
pub fn run(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoop sound"));
    }
    let macos = cfg!(target_os = "macos");
    let list: Vec<&'static str> = config::sounds_here()
        .iter()
        .copied()
        .filter(|name| macos || notify::sound(false, false, name, name).is_some())
        .collect();
    if list.is_empty() {
        println!(
            "no sound to play here: install the freedesktop sound theme and paplay or pw-play"
        );
        return Ok(());
    }
    let path = config::path();
    let c = config::read().config;
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let session = if tty { Some(Session::start()?) } else { None };
    let typed = session.as_ref().map(|s| &s.typed);
    let mut player = Player::new();
    let mut said = Vec::new();
    let steps = [
        (
            "notify.done_sound",
            "an agent finished",
            c.done_sound,
            false,
        ),
        (
            "notify.alert_sound",
            "an agent waits on you",
            c.alert_sound,
            true,
        ),
    ];
    let mut each = || -> std::io::Result<()> {
        for (n, (key, what, now, urgent)) in steps.into_iter().enumerate() {
            let shown = notify::playable(now, urgent, macos);
            let title = format!("{what} ({} of {}), now {shown}", n + 1, steps.len());
            let pick = match &typed {
                Some(typed) => choose_with_keys(&title, &list, shown, typed, &mut player, macos)?,
                None => choose_with_lines(&title, &list, shown, &mut player, macos)?,
            };
            match pick {
                Pick::Keep(name) => {
                    // Enter on the sound already in effect keeps the file's own name.
                    let name = if name == shown { now } else { name };
                    config::set(&path, key, name).map_err(std::io::Error::other)?;
                    config::set(&path, "notify.sound", "true").map_err(std::io::Error::other)?;
                    said.push(format!("{key} = \"{name}\""));
                }
                Pick::Off => {
                    config::set(&path, "notify.sound", "false").map_err(std::io::Error::other)?;
                    said.push("notify.sound = false".to_string());
                    return Ok(());
                }
                Pick::Quit => return Ok(()),
            }
        }
        Ok(())
    };
    // What was written is said even when a later step failed.
    let result = each();
    drop(session);
    if !tty {
        println!();
    }
    for line in &said {
        println!("{line}");
    }
    if said.iter().any(|line| line.ends_with('"')) {
        println!("a running qoop takes it within two seconds");
    }
    result
}

/// Where the cursor goes: left and right wrap through the whole list, up and down stay in
/// the column and stop at the edge.
fn moved(key: Key, at: usize, len: usize, per_row: usize) -> usize {
    match key {
        Key::Left => at.checked_sub(1).unwrap_or(len - 1),
        Key::Right => (at + 1) % len,
        Key::Up if at >= per_row => at - per_row,
        Key::Down if at + per_row < len => at + per_row,
        _ => at,
    }
}

/// How many names fit on a row, and how wide each is.
fn grid(list: &[&str]) -> (usize, usize) {
    let width = list.iter().map(|s| s.len()).max().unwrap_or(0);
    (width, (88 / (width + 3)).max(1))
}

/// The whole screen for one step, drawn from the top; raw mode needs `\r\n`.
fn screen(title: &str, list: &[&str], at: usize) -> String {
    let (width, per_row) = grid(list);
    let mut out = format!("\x1b[H\x1b[2J\r\n {title}\r\n\r\n");
    for (i, name) in list.iter().enumerate() {
        if i % per_row == 0 {
            out.push_str("  ");
        }
        if i == at {
            out.push_str(&format!("\x1b[7m {name:<width$} \x1b[0m "));
        } else {
            out.push_str(&format!(" {name:<width$}  "));
        }
        if i % per_row == per_row - 1 || i + 1 == list.len() {
            out.push_str("\r\n");
        }
    }
    out.push_str(
        "\r\n arrows move and play   enter keeps   space plays again   o off   q quits\r\n",
    );
    out
}

/// The grid: every move plays the sound it lands on.
fn choose_with_keys(
    title: &str,
    list: &[&'static str],
    shown: &'static str,
    typed: &mpsc::Receiver<Vec<u8>>,
    player: &mut Player,
    macos: bool,
) -> std::io::Result<Pick> {
    let (_, per_row) = grid(list);
    let found = list.iter().position(|s| *s == shown);
    let mut at = found.unwrap_or(0);
    // The sound in effect may not be on this machine's list: until the cursor is used,
    // enter keeps it rather than the first sound the cursor happens to start on.
    let mut untouched = found.is_none();
    let mut out = std::io::stdout();
    loop {
        out.write_all(screen(title, list, at).as_bytes())?;
        out.flush()?;
        // Redrawn when something was typed, not on the quiet ticks: the screen starts with a
        // clear, and clearing it three times a second is a flicker.
        let mut keys = Vec::new();
        while keys.is_empty() {
            keys = next_keys(typed);
        }
        for key in keys {
            let key = match key {
                Key::Char('h') => Key::Left,
                Key::Char('l') => Key::Right,
                Key::Char('k') => Key::Up,
                Key::Char('j') => Key::Down,
                other => other,
            };
            match key {
                Key::Enter if untouched => return Ok(Pick::Keep(shown)),
                Key::Enter => return Ok(Pick::Keep(list[at])),
                Key::Char('o') => return Ok(Pick::Off),
                Key::Char('q') | Key::Esc | Key::Stop => return Ok(Pick::Quit),
                Key::Char(' ') => {
                    untouched = false;
                    player.play(list[at], macos);
                }
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    untouched = false;
                    at = moved(key, at, list.len(), per_row);
                    player.play(list[at], macos);
                }
                _ => {}
            }
        }
    }
}

/// Lines, for a pipe: a number or a name plays, an empty line keeps, EOF stops.
fn choose_with_lines(
    title: &str,
    list: &[&'static str],
    shown: &'static str,
    player: &mut Player,
    macos: bool,
) -> std::io::Result<Pick> {
    let (width, per_row) = grid(list);
    println!("\n{title}");
    for (i, name) in list.iter().enumerate() {
        print!("{:>3} {name:<width$}  ", i + 1);
        if i % per_row == per_row - 1 || i + 1 == list.len() {
            println!();
        }
    }
    let mut heard = shown;
    print!("a number plays it, enter keeps {heard}, off for silence, q quits");
    let mut input = std::io::stdin().lock();
    loop {
        print!("\n> ");
        std::io::stdout().flush()?;
        let Some(line) = crate::avatar::read_line(&mut input)? else {
            return Ok(Pick::Quit);
        };
        match parse_line(&line, list) {
            Line::Play(name) => {
                heard = name;
                player.play(name, macos);
                print!("{name}, enter keeps it");
            }
            Line::Keep => return Ok(Pick::Keep(heard)),
            Line::Off => return Ok(Pick::Off),
            Line::Quit => return Ok(Pick::Quit),
            Line::Invalid => print!(
                "1 to {} or a name plays one, enter keeps {heard}",
                list.len()
            ),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Line {
    Play(&'static str),
    Keep,
    Off,
    Quit,
    Invalid,
}

/// A number in `list` or a sound's name from it in any case plays it, an empty line keeps.
fn parse_line(line: &str, list: &[&'static str]) -> Line {
    let line = line.trim();
    match line.to_lowercase().as_str() {
        "" => Line::Keep,
        "off" => Line::Off,
        "q" | "quit" => Line::Quit,
        other => other
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| list.get(i))
            .or_else(|| list.iter().find(|s| s.eq_ignore_ascii_case(line)))
            .map_or(Line::Invalid, |s| Line::Play(s)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_plays_keeps_or_stops() {
        let mac = &config::SOUNDS;
        assert_eq!(parse_line("1", mac), Line::Play("Basso"));
        assert_eq!(parse_line("14", mac), Line::Play("Tink"));
        assert_eq!(parse_line(" tink ", mac), Line::Play("Tink"));
        assert_eq!(parse_line("", mac), Line::Keep);
        assert_eq!(parse_line("OFF", mac), Line::Off);
        assert_eq!(parse_line("q", mac), Line::Quit);
        for bad in ["0", "15", "-1", "glas", "bell"] {
            assert_eq!(parse_line(bad, mac), Line::Invalid, "{bad}");
        }
        let linux = &config::FREEDESKTOP_SOUNDS;
        assert_eq!(parse_line("3", linux), Line::Play("bell"));
        assert_eq!(
            parse_line("Window-Question", linux),
            Line::Play("window-question")
        );
        assert_eq!(parse_line("glass", linux), Line::Invalid);
    }

    #[test]
    fn left_and_right_wrap_up_and_down_stop() {
        // 14 sounds, 6 a row: rows 0-5, 6-11, 12-13.
        assert_eq!(moved(Key::Left, 0, 14, 6), 13);
        assert_eq!(moved(Key::Right, 13, 14, 6), 0);
        assert_eq!(moved(Key::Down, 3, 14, 6), 9);
        assert_eq!(moved(Key::Down, 9, 14, 6), 9);
        assert_eq!(moved(Key::Down, 7, 14, 6), 13);
        assert_eq!(moved(Key::Up, 2, 14, 6), 2);
        assert_eq!(moved(Key::Up, 13, 14, 6), 7);
    }

    #[test]
    fn the_cursor_is_drawn_reversed_on_its_sound_only() {
        let drawn = screen("an agent finished", &config::SOUNDS, 6);
        assert!(drawn.contains("\x1b[7m Hero      \x1b[0m"), "{drawn:?}");
        assert_eq!(drawn.matches("\x1b[7m").count(), 1);
        assert!(drawn.contains(" an agent finished\r\n"));
    }
}

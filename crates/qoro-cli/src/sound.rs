//! `qoro sound`: hear the notification sounds and keep one for a finished agent, then one
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

use std::io::{IsTerminal, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use qoro_runtime::client::TerminalGuard;
use qoro_tui::config;
use qoro_tui::notify;

/// What one step ended with.
enum Pick {
    Keep(&'static str),
    Off,
    Quit,
}

/// `qoro sound`
pub fn run(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoro sound"));
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
    let (guard, typed) = if tty {
        let guard = TerminalGuard::enter_raw()?;
        // A panic must not leave the terminal raw, nor its message on a screen that goes.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            TerminalGuard::restore();
            previous_hook(info);
        }));
        // The alternate screen, cursor hidden; the guard's restore leaves both.
        print!("\x1b[?1049h\x1b[?25l");
        (Some(guard), Some(typing()))
    } else {
        (None, None)
    };
    let mut player = Player(None);
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
    drop(guard);
    if !tty {
        println!();
    }
    for line in &said {
        println!("{line}");
    }
    if said.iter().any(|line| line.ends_with('"')) {
        println!("a running qoro takes it when its settings open (prefix, space, settings)");
    }
    result
}

/// The terminal's bytes, read on a thread of their own so the picker can wait a moment
/// for the rest of an escape sequence without blocking on it.
fn typing() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 64];
        while let Ok(n @ 1..) = stdin.read(&mut buf) {
            if tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    rx
}

/// The sound playing now, stopped when the next one starts so moving fast never stacks
/// them.
struct Player(Option<Child>);

impl Player {
    fn play(&mut self, name: &str, macos: bool) {
        if let Some(mut was) = self.0.take() {
            let _ = was.kill();
            let _ = was.wait();
        }
        let Some(call) = notify::sound(false, macos, name, name) else {
            return;
        };
        self.0 = Command::new(call.program)
            .args(&call.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Again,
    Off,
    Quit,
}

/// The keys in `bytes`, and how many bytes they used. An arrow comes as `ESC [ x` (or
/// `ESC O x`); an escape at the very end may be the start of one cut off by the read, so it
/// is left unused for the caller to wait on; bytes that mean nothing here are skipped.
fn keys(bytes: &[u8]) -> (Vec<Key>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let key = match bytes[i] {
            0x1b if i + 1 == bytes.len()
                || (i + 2 == bytes.len() && matches!(bytes[i + 1], b'[' | b'O')) =>
            {
                return (out, i);
            }
            0x1b if matches!(bytes[i + 1], b'[' | b'O') => {
                i += 2;
                match bytes[i] {
                    b'D' => Some(Key::Left),
                    b'C' => Some(Key::Right),
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    _ => None,
                }
            }
            b'h' => Some(Key::Left),
            b'l' => Some(Key::Right),
            b'k' => Some(Key::Up),
            b'j' => Some(Key::Down),
            b'\r' | b'\n' => Some(Key::Enter),
            b' ' => Some(Key::Again),
            b'o' => Some(Key::Off),
            b'q' | 0x03 | 0x04 => Some(Key::Quit),
            _ => None,
        };
        out.extend(key);
        i += 1;
    }
    (out, i)
}

/// The keys typed next: what arrives, and when it ends in a cut-off escape, what follows
/// within a moment. Nothing following, a lone escape is the escape key and quits.
fn next_keys(typed: &mpsc::Receiver<Vec<u8>>) -> Vec<Key> {
    let Ok(mut bytes) = typed.recv() else {
        return vec![Key::Quit];
    };
    loop {
        let (mut found, used) = keys(&bytes);
        if used == bytes.len() {
            return found;
        }
        match typed.recv_timeout(Duration::from_millis(50)) {
            Ok(more) => bytes.extend(more),
            Err(_) => {
                if bytes[used..] == [0x1b] {
                    found.push(Key::Quit);
                }
                return found;
            }
        }
    }
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
        for key in next_keys(typed) {
            match key {
                Key::Enter if untouched => return Ok(Pick::Keep(shown)),
                Key::Enter => return Ok(Pick::Keep(list[at])),
                Key::Off => return Ok(Pick::Off),
                Key::Quit => return Ok(Pick::Quit),
                Key::Again => {
                    untouched = false;
                    player.play(list[at], macos);
                }
                _ => {
                    untouched = false;
                    at = moved(key, at, list.len(), per_row);
                    player.play(list[at], macos);
                }
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
    fn arrows_letters_and_a_cut_off_escape() {
        let only = |bytes: &[u8]| keys(bytes).0;
        assert_eq!(
            only(b"\x1b[C\x1b[D\x1b[A\x1b[B\x1bOC"),
            [Key::Right, Key::Left, Key::Up, Key::Down, Key::Right]
        );
        assert_eq!(
            only(b"hjkl \r"),
            [
                Key::Left,
                Key::Down,
                Key::Up,
                Key::Right,
                Key::Again,
                Key::Enter
            ]
        );
        assert_eq!(only(b"q"), [Key::Quit]);
        assert_eq!(only(b"\x03"), [Key::Quit]);
        assert_eq!(only(b"o"), [Key::Off]);
        assert_eq!(keys(b"x\x1b[Z"), (vec![], 4));
        // An escape at the end, alone or with its `[`, is left for the next read.
        assert_eq!(keys(b"\x1b"), (vec![], 0));
        assert_eq!(keys(b"\x1b[C\x1b"), (vec![Key::Right], 3));
        assert_eq!(keys(b"\x1b[C\x1b["), (vec![Key::Right], 3));
    }

    #[test]
    fn a_lone_escape_quits_and_a_cut_arrow_waits_for_its_rest() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"\x1b".to_vec()).unwrap();
        assert_eq!(next_keys(&rx), [Key::Quit]);
        tx.send(b"l\x1b[".to_vec()).unwrap();
        tx.send(b"D".to_vec()).unwrap();
        assert_eq!(next_keys(&rx), [Key::Right, Key::Left]);
        drop(tx);
        assert_eq!(next_keys(&rx), [Key::Quit]);
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

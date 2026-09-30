//! Telling a person who is not looking that an agent needs them.
//!
//! The bell is the one channel that works everywhere, and it is only heard by somebody
//! looking at the terminal or by a terminal set to turn it into a badge. A person who went
//! to another window while an agent worked is the person who most needs to hear it
//! finished, so this reaches the desktop, by the first of these that can:
//!
//! - the terminal itself, with the escape it understands: `OSC 9` (iTerm2, WezTerm,
//!   Ghostty), `OSC 99` (kitty), `OSC 777` (foot). Inside tmux only when tmux lets escapes
//!   through (`allow-passthrough`), because otherwise tmux eats them;
//! - the system: `terminal-notifier` when it is installed, else `osascript`, on macOS, and
//!   `notify-send` elsewhere. Never over ssh, where the system is the far machine's.
//!
//! What is said is a tab's name or program, what happened, and the space: names this
//! machine gave, never anything read from a pane. It still passes one choke point,
//! [`clean`], because a tab name is typed by a person and ends up inside an escape sequence
//! or another program's arguments. The system path hands it over as data (an argument after
//! `--`, or the program's input), never as AppleScript source, shell text or an option, so
//! no name can become code.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How long a note waits before it goes: long enough that a state flickering through
/// `done` and back to `working` says nothing, short enough to be news.
pub const DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// The widest a note is allowed to be, in columns.
const MAX_COLUMNS: usize = 120;

/// Which escape a terminal turns into a desktop notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// iTerm2, WezTerm, Ghostty.
    Osc9,
    /// kitty.
    Osc99,
    /// foot.
    Osc777,
}

/// Where a note goes, decided once when the interface starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Written to the terminal, wrapped for tmux when `tmux` is set.
    Escape { escape: Escape, tmux: bool },
    /// A program this machine has, by name from `PATH`.
    System(Program),
    /// Nothing here can show one, and why.
    Nowhere(Why),
}

/// Why a note has nowhere to go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The system here is another machine's.
    Ssh,
    /// Inside tmux, which does not let an escape through to the terminal outside.
    Tmux,
    NoProgram,
}

/// The system programs a note can go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    TerminalNotifier,
    Osascript,
    NotifySend,
}

/// What the route is decided from, gathered in one place so the decision is a pure
/// function a test can drive with any environment.
#[derive(Clone, Debug)]
pub struct Env {
    pub term_program: Option<String>,
    pub term: Option<String>,
    pub kitty: bool,
    pub tmux: bool,
    /// tmux said `allow-passthrough` is `on` or `all`.
    pub passthrough: bool,
    pub ssh: bool,
    pub macos: bool,
    /// Which of the system programs are on `PATH`.
    pub has: fn(&str) -> bool,
}

impl Env {
    /// This process's environment. Asks tmux once, by argv, when inside it.
    pub fn read() -> Env {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let tmux = var("TMUX").is_some();
        Env {
            term_program: var("TERM_PROGRAM"),
            term: var("TERM"),
            kitty: var("KITTY_WINDOW_ID").is_some(),
            tmux,
            passthrough: tmux && tmux_passes_escapes(var("TMUX_PANE").as_deref()),
            ssh: var("SSH_CONNECTION").is_some(),
            macos: cfg!(target_os = "macos"),
            has: |name| on_path(name).is_some(),
        }
    }
}

/// Whether tmux lets an escape through to the terminal around it.
///
/// Asked with a deadline: this runs before the first frame, and a tmux server that does
/// not answer must cost the interface a quarter of a second at most, never its start. No
/// answer in time reads as no.
fn tmux_passes_escapes(pane: Option<&str>) -> bool {
    use std::io::Read as _;
    let mut ask = Command::new("tmux");
    ask.args(["show", "-Apv"]);
    if let Some(pane) = pane {
        ask.args(["-t", pane]);
    }
    ask.arg("allow-passthrough")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = ask.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + TMUX_PATIENCE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    let mut said = String::new();
    let _ = child
        .stdout
        .take()
        .map(|mut out| out.read_to_string(&mut said));
    matches!(said.trim(), "on" | "all")
}

/// How long tmux is given to say whether it passes escapes through.
const TMUX_PATIENCE: std::time::Duration = std::time::Duration::from_millis(250);

/// The escape a terminal understands, from what it says about itself.
fn escape_for(env: &Env) -> Option<Escape> {
    if let Some("iTerm.app" | "WezTerm" | "ghostty") = env.term_program.as_deref() {
        return Some(Escape::Osc9);
    }
    if env.kitty {
        return Some(Escape::Osc99);
    }
    match env.term.as_deref() {
        Some("xterm-kitty") => Some(Escape::Osc99),
        Some("xterm-ghostty") => Some(Escape::Osc9),
        Some(term) if term.starts_with("foot") => Some(Escape::Osc777),
        _ => None,
    }
}

/// Where a note goes, from the environment.
pub fn route(env: &Env) -> Route {
    if let Some(escape) = escape_for(env)
        && (!env.tmux || env.passthrough)
    {
        return Route::Escape {
            escape,
            tmux: env.tmux,
        };
    }
    // The system here is the far machine's when this runs over ssh.
    if env.ssh {
        return Route::Nowhere(Why::Ssh);
    }
    let found = if env.macos {
        ["terminal-notifier", "osascript"]
            .into_iter()
            .find(|name| (env.has)(name))
    } else {
        ["notify-send"].into_iter().find(|name| (env.has)(name))
    };
    match found {
        Some("terminal-notifier") => Route::System(Program::TerminalNotifier),
        Some("osascript") => Route::System(Program::Osascript),
        Some(_) => Route::System(Program::NotifySend),
        // Inside tmux the terminal outside may well show one: tmux is what is in the way.
        None if env.tmux => Route::Nowhere(Why::Tmux),
        None => Route::Nowhere(Why::NoProgram),
    }
}

/// Where a note goes, as the settings box says it.
pub fn describe(route: Route) -> String {
    match route {
        Route::Escape { escape, tmux } => {
            let kind = match escape {
                Escape::Osc9 => "OSC 9",
                Escape::Osc99 => "OSC 99",
                Escape::Osc777 => "OSC 777",
            };
            let through = if tmux { ", through tmux" } else { "" };
            format!("the terminal ({kind}{through})")
        }
        Route::System(Program::TerminalNotifier) => "terminal-notifier".into(),
        Route::System(Program::Osascript) => "osascript".into(),
        Route::System(Program::NotifySend) => "notify-send".into(),
        Route::Nowhere(Why::Ssh) => "nowhere: ssh".into(),
        Route::Nowhere(Why::Tmux) => "nowhere: tmux allow-passthrough is off".into(),
        Route::Nowhere(Why::NoProgram) => "nowhere: no notifier here".into(),
    }
}

/// A state change waiting out [`DELAY`] before it is said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub terminal: u32,
    /// The state and generation it was about, so a change that has already been undone
    /// when the note is due is dropped rather than said.
    pub state: String,
    pub generation: u64,
    /// What the tab bar and the panel call the terminal.
    pub label: String,
    /// What happened, as the end of a sentence: "finished", "is blocked".
    pub what: &'static str,
    pub space: String,
    /// Waiting on a person (blocked, failed) rather than only done.
    pub urgent: bool,
    /// When it has waited its full [`DELAY`].
    pub due: std::time::Instant,
}

/// One sentence for everything that came due together: what happened to each, by name.
/// Two terminals with one name are told apart by their space, the way the agents panel
/// tells them apart (`app - claude`).
pub fn say(notes: &[Note]) -> String {
    let name = |note: &Note| {
        let shared = notes
            .iter()
            .any(|other| other.label == note.label && other.terminal != note.terminal);
        if shared && !note.space.is_empty() {
            format!("{} - {}", note.space, note.label)
        } else {
            note.label.clone()
        }
    };
    match notes {
        [one] if !one.space.is_empty() => format!("{} {} in {}", name(one), one.what, one.space),
        many => many
            .iter()
            .map(|note| format!("{} {}", name(note), note.what))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// What the hint row says, once, when a note had nowhere to go: why, and the exact lines
/// that turn the notes off in the file at `config`.
pub fn nowhere_hint(why: Why, config: &Path) -> String {
    let reason = match why {
        Why::Ssh => "desktop notes cannot reach you over ssh",
        Why::Tmux => {
            "tmux drops desktop notes (tmux set -g allow-passthrough on lets them through)"
        }
        Why::NoProgram => "nothing here can show a desktop note",
    };
    format!(
        "{reason}; to stop trying, put [notify] then desktop = false in {}",
        config.display()
    )
}

/// A note, safe to put inside an escape sequence or hand to another program.
///
/// Control characters (C0, C1, DEL) are dropped: any of them could end the escape early and
/// hand the rest to the terminal as a sequence of its own. `;` becomes `,`, because
/// `OSC 777` splits its fields on it and `OSC 9 ; 4` is a progress bar, not a note: with no
/// `;` left, a note can never start with a number and a `;`. Cut to 120 columns.
pub fn clean(body: &str) -> String {
    let mut out = String::with_capacity(body.len().min(MAX_COLUMNS * 4));
    let mut used = 0;
    for c in body.chars() {
        let c = match c {
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => continue,
            ';' => ',',
            c => c,
        };
        let w = crate::width::columns(c);
        if used + w > MAX_COLUMNS {
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// The bytes that show `body` through the terminal. `body` must have come through [`clean`].
pub fn escape(kind: Escape, tmux: bool, body: &str) -> Vec<u8> {
    let sequence = match kind {
        Escape::Osc9 => format!("\x1b]9;{body}\x1b\\"),
        Escape::Osc99 => format!("\x1b]99;;{body}\x1b\\"),
        Escape::Osc777 => format!("\x1b]777;notify;qoro;{body}\x1b\\"),
    };
    if !tmux {
        return sequence.into_bytes();
    }
    // tmux's passthrough: the whole sequence inside `DCS tmux; ... ST`, every escape in it
    // doubled so tmux does not read it as the end.
    let mut wrapped = b"\x1bPtmux;".to_vec();
    for byte in sequence.bytes() {
        if byte == 0x1b {
            wrapped.push(0x1b);
        }
        wrapped.push(byte);
    }
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

/// A program to start: its name on `PATH`, its arguments, and what to write to its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub program: &'static str,
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

/// How `body` reaches a system program. `body` must have come through [`clean`], and it is
/// never where a program looks for an option or for code, whatever it starts with:
///
/// - `terminal-notifier` gets it on its input. Its own parser reads a `-message` value
///   that starts with `-` as the next option, so a tab named `-x` lost its note; the input
///   is read as the message with no parsing at all.
/// - `osascript` runs a fixed script that reads it from `argv`, after a `--`: without it,
///   a body starting with `-` is read as an option, and `-e` is an option that runs code.
/// - `notify-send` gets it after a `--` for the same reason.
pub fn call(program: Program, body: &str) -> Call {
    let (program, args, stdin) = match program {
        Program::TerminalNotifier => (
            "terminal-notifier",
            vec!["-title".into(), "qoro".into()],
            Some(body.to_string()),
        ),
        Program::Osascript => (
            "osascript",
            vec![
                "-e".into(),
                "on run argv".into(),
                "-e".into(),
                "display notification (item 1 of argv) with title (item 2 of argv)".into(),
                "-e".into(),
                "end run".into(),
                "--".into(),
                body.into(),
                "qoro".into(),
            ],
            None,
        ),
        Program::NotifySend => (
            "notify-send",
            vec!["--".into(), "qoro".into(), body.into()],
            None,
        ),
    };
    Call {
        program,
        args,
        stdin,
    }
}

/// The sound for a note: `alert` (one of `config::SOUNDS`) for something that is waiting
/// on a person or failed, `done` otherwise; on Linux the desktop's own two. Off by default,
/// so this is only reached when `sound = true`. `None` when this machine has no player or
/// no sound file to give it.
pub fn sound(urgent: bool, macos: bool, done: &str, alert: &str) -> Option<Call> {
    let play = |program: &'static str, file: &str| Call {
        program,
        args: vec![file.to_string()],
        stdin: None,
    };
    if macos {
        let file = format!(
            "/System/Library/Sounds/{}.aiff",
            if urgent { alert } else { done }
        );
        return on_path("afplay").map(|_| play("afplay", &file));
    }
    let file = if urgent {
        "/usr/share/sounds/freedesktop/stereo/dialog-warning.oga"
    } else {
        "/usr/share/sounds/freedesktop/stereo/complete.oga"
    };
    if !Path::new(file).is_file() {
        return None;
    }
    ["paplay", "pw-play"]
        .into_iter()
        .find(|player| on_path(player).is_some())
        .map(|player| play(player, file))
}

/// Start a program by name from `PATH`, with nothing attached to it but what it is given to
/// read, and reap it on a thread of its own so it never becomes a zombie and never holds the
/// loop up. The input is written on that thread too, then closed.
pub fn spawn(call: &Call) {
    let started = Command::new(call.program)
        .args(&call.args)
        .stdin(if call.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = started {
        let input = call.stdin.clone();
        std::thread::spawn(move || {
            if let (Some(text), Some(mut pipe)) = (input, child.stdin.take()) {
                use std::io::Write as _;
                let _ = pipe.write_all(text.as_bytes());
            }
            let _ = child.wait();
        });
    }
}

/// Where a program of this name is on `PATH`, if it is.
fn on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|file| {
                file.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env {
            term_program: None,
            term: None,
            kitty: false,
            tmux: false,
            passthrough: false,
            ssh: false,
            macos: false,
            has: |_| false,
        }
    }

    #[test]
    fn each_terminal_gets_the_escape_it_understands() {
        type Case = (
            Option<&'static str>,
            Option<&'static str>,
            bool,
            Option<Escape>,
        );
        let cases: [Case; 8] = [
            (Some("iTerm.app"), None, false, Some(Escape::Osc9)),
            (Some("WezTerm"), None, false, Some(Escape::Osc9)),
            (Some("ghostty"), None, false, Some(Escape::Osc9)),
            (None, Some("xterm-ghostty"), false, Some(Escape::Osc9)),
            (None, Some("xterm-kitty"), false, Some(Escape::Osc99)),
            (None, Some("xterm-256color"), true, Some(Escape::Osc99)),
            (None, Some("foot-extra"), false, Some(Escape::Osc777)),
            (Some("Apple_Terminal"), Some("xterm-256color"), false, None),
        ];
        for (term_program, term, kitty, want) in cases {
            let env = Env {
                term_program: term_program.map(str::to_string),
                term: term.map(str::to_string),
                kitty,
                ..env()
            };
            let got = match route(&env) {
                Route::Escape { escape, tmux } => {
                    assert!(!tmux);
                    Some(escape)
                }
                _ => None,
            };
            assert_eq!(got, want, "{term_program:?} {term:?}");
        }
        assert_eq!(
            escape(Escape::Osc9, false, "hi"),
            b"\x1b]9;hi\x1b\\".to_vec()
        );
        assert_eq!(
            escape(Escape::Osc99, false, "hi"),
            b"\x1b]99;;hi\x1b\\".to_vec()
        );
        assert_eq!(
            escape(Escape::Osc777, false, "hi"),
            b"\x1b]777;notify;qoro;hi\x1b\\".to_vec()
        );
    }

    /// Inside tmux the escape goes only through a passthrough tmux allows; otherwise the
    /// system is asked, as it is for a terminal with no escape at all.
    #[test]
    fn tmux_passes_the_escape_only_when_it_allows_it() {
        let inside = Env {
            term_program: Some("iTerm.app".into()),
            tmux: true,
            passthrough: true,
            ..env()
        };
        assert_eq!(
            route(&inside),
            Route::Escape {
                escape: Escape::Osc9,
                tmux: true
            }
        );
        assert_eq!(
            escape(Escape::Osc9, true, "hi"),
            b"\x1bPtmux;\x1b\x1b]9;hi\x1b\x1b\\\x1b\\".to_vec()
        );
        let closed = Env {
            passthrough: false,
            macos: true,
            has: |name| name == "osascript",
            ..inside
        };
        assert_eq!(route(&closed), Route::System(Program::Osascript));
        let nothing = Env {
            has: |_| false,
            ..closed
        };
        assert_eq!(route(&nothing), Route::Nowhere(Why::Tmux));
        let hint = nowhere_hint(Why::Tmux, Path::new("/c/config.toml"));
        assert!(hint.contains("allow-passthrough"), "{hint}");
        assert!(
            hint.contains("[notify] then desktop = false in /c/config.toml"),
            "{hint}"
        );
        assert_eq!(
            describe(route(&nothing)),
            "nowhere: tmux allow-passthrough is off"
        );
    }

    #[test]
    fn the_system_is_never_asked_over_ssh() {
        let far = Env {
            ssh: true,
            macos: true,
            has: |_| true,
            ..env()
        };
        assert_eq!(route(&far), Route::Nowhere(Why::Ssh));
        assert_eq!(describe(route(&far)), "nowhere: ssh");
        // A terminal escape is still fine: it travels back to the person's own terminal.
        let far_kitty = Env { kitty: true, ..far };
        assert!(matches!(route(&far_kitty), Route::Escape { .. }));
    }

    #[test]
    fn the_system_program_is_the_first_one_installed() {
        let mac = Env {
            macos: true,
            has: |_| true,
            ..env()
        };
        assert_eq!(route(&mac), Route::System(Program::TerminalNotifier));
        let linux = Env {
            has: |name| name == "notify-send",
            ..env()
        };
        assert_eq!(route(&linux), Route::System(Program::NotifySend));
    }

    /// A tab named to break out of AppleScript is only ever text: the script is fixed and
    /// reads the note from its arguments.
    #[test]
    fn an_applescript_injection_stays_one_argument() {
        let body = clean(r#"x" & do shell script "touch /tmp/pwned" & ""#);
        let osa = call(Program::Osascript, &body);
        assert_eq!(osa.program, "osascript");
        assert_eq!(osa.args.iter().filter(|a| a.contains("pwned")).count(), 1);
        assert_eq!(osa.args[7], body);
        assert!(osa.args[..7].iter().all(|a| !a.contains("pwned")));
    }

    /// A tab named like an option stays the note in every sink. Measured before this:
    /// `osascript` read a leading `-e` as code to run, and `terminal-notifier` delivered an
    /// empty note for `-x hi`.
    #[test]
    fn a_note_starting_with_a_dash_stays_data_for_every_sink() {
        let body = clean("-e do shell script \"touch pwned\" -x");
        assert!(body.starts_with('-'));
        let osa = call(Program::Osascript, &body);
        let at = osa
            .args
            .iter()
            .position(|a| *a == body)
            .expect("the note is there");
        assert_eq!(osa.args[at - 1], "--", "{:?}", osa.args);
        let send = call(Program::NotifySend, &body);
        assert_eq!(
            send.args,
            vec!["--".to_string(), "qoro".into(), body.clone()]
        );
        let tn = call(Program::TerminalNotifier, &body);
        assert!(
            tn.args.iter().all(|a| *a != body),
            "no argument to misparse"
        );
        assert_eq!(tn.stdin.as_deref(), Some(body.as_str()));
    }

    #[test]
    fn clean_leaves_nothing_that_could_end_or_change_the_escape() {
        assert_eq!(clean("a\x1b]2;x\x07b\u{9c}c\x7fd\ne"), "a]2,xbcde");
        assert_eq!(clean("4;50"), "4,50", "OSC 9;4 is a progress bar");
        assert!(!clean("9;4;1;50").contains(';'));
        let long = "\u{4e2d}".repeat(100);
        assert_eq!(crate::width::width(&clean(&long)), MAX_COLUMNS);
        assert_eq!(clean("claude finished in api"), "claude finished in api");
    }

    #[test]
    fn notes_due_together_are_said_as_one_saying_what_happened_to_each() {
        let note = |terminal: u32, label: &str, what: &'static str| Note {
            terminal,
            state: "done".into(),
            generation: 0,
            label: label.into(),
            what,
            space: "api".into(),
            urgent: false,
            due: std::time::Instant::now(),
        };
        assert_eq!(
            say(&[note(1, "claude", "finished")]),
            "claude finished in api"
        );
        assert_eq!(
            say(&[note(1, "codex", "is blocked"), note(2, "aider", "finished")]),
            "codex is blocked, aider finished"
        );
        assert_eq!(
            say(&[
                Note {
                    space: "app".into(),
                    ..note(3, "claude", "is blocked")
                },
                Note {
                    space: "personal".into(),
                    ..note(7, "claude", "finished")
                },
            ]),
            "app - claude is blocked, personal - claude finished",
            "two tabs of one name are told apart the way the panel tells them apart"
        );
    }

    #[test]
    fn a_sound_is_the_calm_one_or_the_urgent_one() {
        // Whether a player is installed differs by machine; the files named do not.
        if let Some(play) = sound(false, true, "Glass", "Funk") {
            assert_eq!(play.program, "afplay");
            assert_eq!(play.args[0], "/System/Library/Sounds/Glass.aiff");
        }
        if let Some(play) = sound(true, true, "Glass", "Hero") {
            assert_eq!(play.args[0], "/System/Library/Sounds/Hero.aiff");
        }
    }
}

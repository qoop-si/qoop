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
//! machine gave. One thing more comes from the agent: for an agent waiting on a person, the
//! first line of its question, from its question card, else from its own menu as the daemon
//! read it off its screen ([`question_line`]). This note is local only, on this machine's
//! own desktop; the push to a phone never carries question text. Everything passes one
//! choke point, [`clean`], because a tab name is typed by a person and a question is written
//! by an agent, and both end up inside an escape sequence or another program's arguments.
//! The system path hands it over as data (an argument after `--`, or the program's input),
//! never as AppleScript source, shell text or an option, so no name can become code.
//!
//! A click on a note from `terminal-notifier` shows the agent: it runs `qoop focus` for the
//! terminal and brings the host terminal forward ([`Click`]). A terminal on the escape route
//! notifies by itself and keeps no click action, and a note about another machine's agent
//! has none: its number is this window's own, which no daemon knows.

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

/// Where a note goes, as `qoop settings` says it.
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
    /// The first line of what the agent asks, when it asks something ([`question_line`]).
    pub question: Option<String>,
    /// When it has waited its full [`DELAY`].
    pub due: std::time::Instant,
}

/// The first line of a question, as a note shows it: the first line with words in it, ended
/// by any line or paragraph break (`\r` and U+2028 too, which a banner would draw as a second
/// line), a tab read as a space, and nothing [`crate::width::drawn`] refuses. `None` when it
/// has none. [`clean`] cuts it to the note's width.
pub fn question_line(text: &str) -> Option<String> {
    text.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}',
    ])
    .map(|line| {
        line.chars()
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| crate::width::drawn(*c))
            .collect::<String>()
    })
    .map(|line| line.trim().to_string())
    .find(|line| !line.is_empty())
}

/// One sentence for everything that came due together: what happened to each, by name.
/// Two terminals with one name are told apart by their space, the way the agents panel
/// tells them apart (`app - claude`). One agent that asks something is its name and its
/// question.
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
        [one] if one.question.is_some() => {
            format!(
                "{}: {}",
                name(one),
                one.question.as_deref().unwrap_or_default()
            )
        }
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
/// What [`crate::width::drawn`] refuses is dropped: a control character (C0, C1, DEL) could
/// end the escape early and hand the rest to the terminal as a sequence of its own, and a
/// bidi override or isolate would turn an agent's words around in the banner. `;` becomes `,`, because
/// `OSC 777` splits its fields on it and `OSC 9 ; 4` is a progress bar, not a note: with no
/// `;` left, a note can never start with a number and a `;`. Cut to 120 columns, the cut
/// marked with an ellipsis, never inside a joined emoji or a flag.
pub fn clean(body: &str) -> String {
    let mut out = String::with_capacity(body.len().min(MAX_COLUMNS * 4));
    // Each kept character with the columns used before it, so a cut can make room for the `…`.
    let mut kept: Vec<usize> = Vec::new();
    let mut used = 0;
    for c in body.chars() {
        let c = match c {
            ';' => ',',
            c if !crate::width::drawn(c) => continue,
            c => c,
        };
        let w = crate::width::columns(c);
        if used + w > MAX_COLUMNS {
            while used + 1 > MAX_COLUMNS {
                let Some(before) = kept.pop() else { break };
                out.pop();
                used = before;
            }
            // Not half an emoji: no joiner or variation selector last, no lone half of a flag.
            while out.ends_with(['\u{200d}', '\u{fe0f}', '\u{fe0e}']) {
                out.pop();
            }
            let flags = out
                .chars()
                .rev()
                .take_while(|c| ('\u{1f1e6}'..='\u{1f1ff}').contains(c))
                .count();
            if flags % 2 == 1 {
                out.pop();
            }
            out.push('…');
            return out;
        }
        kept.push(used);
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
        Escape::Osc777 => format!("\x1b]777;notify;qoop;{body}\x1b\\"),
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

/// What a click on a `terminal-notifier` note does: run `qoop focus <terminal>` with this
/// session's daemon, and bring the host terminal forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Click {
    /// This `qoop`, by its absolute path: the note outlives the shell it came from.
    pub qoop: PathBuf,
    /// This session's runtime base, absolute, resolved as every client resolves it, so the
    /// click reaches this daemon from terminal-notifier's own environment.
    pub runtime_dir: Option<String>,
    /// The bundle id of the terminal app this runs in, for `-activate`.
    pub bundle: Option<String>,
}

/// The click a note gets here, from the environment: none off macOS, and none for a terminal
/// on the escape route (iTerm2, WezTerm, Ghostty, kitty), which shows its own notes and keeps
/// no click action. Inside tmux the terminal names itself `tmux`, so a note sent through the
/// system there does get a click, and `-activate` names whatever app started the tmux server.
pub fn click(
    env: &Env,
    qoop: Option<PathBuf>,
    runtime_dir: Option<String>,
    bundle: Option<String>,
) -> Option<Click> {
    if !env.macos || escape_for(env).is_some() {
        return None;
    }
    // Apple's Terminal says who it is in `TERM_PROGRAM` when the bundle id was not handed down.
    let bundle = bundle.or_else(|| {
        (env.term_program.as_deref() == Some("Apple_Terminal")).then(|| "com.apple.Terminal".into())
    });
    Some(Click {
        qoop: qoop?,
        runtime_dir,
        bundle,
    })
}

impl Click {
    /// This process's own: `current_exe`, the runtime base as the client finds its daemon
    /// (`QOOP_RUNTIME_DIR`, its old name, else the default), made absolute, and the app's
    /// bundle id as macOS hands it to what the app starts (`__CFBundleIdentifier`).
    pub fn read(env: &Env) -> Option<Click> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let runtime_dir = std::path::absolute(qoop_runtime::paths::base_location())
            .ok()
            .map(|dir| dir.to_string_lossy().into_owned());
        click(
            env,
            std::env::current_exe().ok(),
            runtime_dir,
            var("__CFBundleIdentifier"),
        )
    }

    /// The `terminal-notifier` arguments for a note about `terminal`. `-execute` is shell text
    /// to the notifier, so each path is quoted whole.
    fn args(&self, terminal: u32) -> Vec<String> {
        let quote = |text: &str| format!("'{}'", text.replace('\'', r"'\''"));
        let dir = self
            .runtime_dir
            .as_deref()
            .map(|dir| format!("QOOP_RUNTIME_DIR={} ", quote(dir)))
            .unwrap_or_default();
        let qoop = quote(&self.qoop.to_string_lossy());
        let mut args = vec!["-execute".into(), format!("{dir}{qoop} focus {terminal}")];
        if let Some(bundle) = &self.bundle {
            args.extend(["-activate".into(), bundle.clone()]);
        }
        args
    }
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
///
/// `click` is the terminal the note is about and what a click on it does: only
/// `terminal-notifier` acts on a click.
pub fn call(program: Program, body: &str, click: Option<(&Click, u32)>) -> Call {
    let (program, args, stdin) = match program {
        Program::TerminalNotifier => {
            let mut args = vec!["-title".to_string(), "qoop".into()];
            if let Some((click, terminal)) = click {
                args.extend(click.args(terminal));
            }
            ("terminal-notifier", args, Some(body.to_string()))
        }
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
                "qoop".into(),
            ],
            None,
        ),
        Program::NotifySend => (
            "notify-send",
            vec!["--".into(), "qoop".into(), body.into()],
            None,
        ),
    };
    Call {
        program,
        args,
        stdin,
    }
}

/// The name a sound plays as on this system: `name` when it is one of the system's own
/// (`config::SOUNDS` on macOS, `config::FREEDESKTOP_SOUNDS` elsewhere), else the default for
/// its kind, so a Mac name read on Linux still plays something, and the other way round.
pub fn playable(name: &str, urgent: bool, macos: bool) -> &str {
    let (own, calm, alarm): (&[&str], _, _) = if macos {
        (&crate::config::SOUNDS, "Glass", "Funk")
    } else {
        (
            &crate::config::FREEDESKTOP_SOUNDS,
            "complete",
            "dialog-warning",
        )
    };
    match (own.contains(&name), urgent) {
        (true, _) => name,
        (false, true) => alarm,
        (false, false) => calm,
    }
}

/// The sound for a note: `alert` for something that is waiting on a person or failed,
/// `done` otherwise, each through [`playable`]. Off by default, so this is only reached when
/// `sound = true`. `None` when this machine has no player or no sound file to give it.
pub fn sound(urgent: bool, macos: bool, done: &str, alert: &str) -> Option<Call> {
    let play = |program: &'static str, file: &str| Call {
        program,
        args: vec![file.to_string()],
        stdin: None,
    };
    let name = playable(if urgent { alert } else { done }, urgent, macos);
    if macos {
        let file = format!("/System/Library/Sounds/{name}.aiff");
        return on_path("afplay").map(|_| play("afplay", &file));
    }
    // A theme without the picked file still has the default's, which plays instead.
    let file = [name, playable("", urgent, false)]
        .map(|n| format!("/usr/share/sounds/freedesktop/stereo/{n}.oga"))
        .into_iter()
        .find(|file| Path::new(file).is_file())?;
    ["paplay", "pw-play"]
        .into_iter()
        .find(|player| on_path(player).is_some())
        .map(|player| play(player, &file))
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
            b"\x1b]777;notify;qoop;hi\x1b\\".to_vec()
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
        let osa = call(Program::Osascript, &body, None);
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
        let osa = call(Program::Osascript, &body, None);
        let at = osa
            .args
            .iter()
            .position(|a| *a == body)
            .expect("the note is there");
        assert_eq!(osa.args[at - 1], "--", "{:?}", osa.args);
        let send = call(Program::NotifySend, &body, None);
        assert_eq!(
            send.args,
            vec!["--".to_string(), "qoop".into(), body.clone()]
        );
        let tn = call(Program::TerminalNotifier, &body, None);
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
        let cut = clean(&long);
        assert!(crate::width::width(&cut) <= MAX_COLUMNS);
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(clean("claude finished in api"), "claude finished in api");
        // Bidi overrides and isolates go too.
        assert_eq!(clean("a\u{202e}b\u{2067}c\u{2069}"), "abc");
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
            question: None,
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

    #[test]
    fn a_sound_from_the_other_system_plays_the_default() {
        assert_eq!(playable("Hero", true, true), "Hero");
        assert_eq!(playable("bell", false, true), "Glass");
        assert_eq!(playable("bell", true, true), "Funk");
        assert_eq!(playable("bell", false, false), "bell");
        assert_eq!(playable("Glass", false, false), "complete");
        assert_eq!(playable("Funk", true, false), "dialog-warning");
    }

    /// The question an agent asks, as its note shows it: the first line with words, nothing
    /// that could end an escape, and a long one cut to the note's width with an ellipsis.
    #[test]
    fn a_note_says_the_first_line_of_the_question_cut_to_its_width() {
        assert_eq!(
            question_line("\n  \n  Do you want to proceed?\n  > 1. Yes"),
            Some("Do you want to proceed?".into())
        );
        assert_eq!(
            question_line("Run\x1b]9;4;1;50\x07 this?\u{9c}"),
            Some("Run]9;4;1;50 this?".into()),
            "control characters go, the rest is for clean"
        );
        assert_eq!(question_line(""), None);
        // Every line break ends the first line; a tab is a space.
        assert_eq!(question_line("Proceed?\rsecret"), Some("Proceed?".into()));
        assert_eq!(
            question_line("Proceed?\u{2028}secret"),
            Some("Proceed?".into())
        );
        assert_eq!(
            question_line("Proceed?\u{2029}secret"),
            Some("Proceed?".into())
        );
        assert_eq!(question_line("Run\tthis?"), Some("Run this?".into()));
        // An agent's text cannot turn itself around in the banner.
        assert_eq!(
            question_line("Allow \u{202e}exe.txt\u{2067}?"),
            Some("Allow exe.txt?".into())
        );
        assert_eq!(question_line(" \n\t\n\x1b"), None);
        let long = format!("Which of these {}?", "very ".repeat(40));
        let body = clean(&format!(
            "claude: {}",
            question_line(&long).expect("a line")
        ));
        assert_eq!(body.chars().count(), MAX_COLUMNS, "{body}");
        assert!(body.starts_with("claude: Which of these very"), "{body}");
        assert!(body.ends_with('…'), "{body}");
        // Short enough: no ellipsis.
        assert_eq!(clean("claude: Proceed?"), "claude: Proceed?");
    }

    /// One agent that asks is its name and its question; one that asks nothing, or several
    /// at once, are said as before.
    #[test]
    fn a_note_with_a_question_names_the_agent_and_asks_it() {
        let note = |terminal: u32, label: &str, question: Option<&str>| Note {
            terminal,
            state: "blocked".into(),
            generation: 0,
            label: label.into(),
            what: "is blocked",
            space: "api".into(),
            urgent: true,
            question: question.map(str::to_string),
            due: std::time::Instant::now(),
        };
        assert_eq!(
            say(&[note(1, "claude", Some("Do you want to proceed?"))]),
            "claude: Do you want to proceed?"
        );
        assert_eq!(say(&[note(1, "claude", None)]), "claude is blocked in api");
        assert_eq!(
            say(&[note(1, "claude", Some("Proceed?")), note(2, "codex", None)]),
            "claude is blocked, codex is blocked"
        );
    }

    /// A click on a `terminal-notifier` note runs this `qoop` against this session's daemon,
    /// paths quoted whole, and brings the host terminal forward. Terminals on the escape route
    /// and other systems get none.
    #[test]
    fn a_click_focuses_the_agent_through_this_sessions_qoop() {
        let apple = Env {
            term_program: Some("Apple_Terminal".into()),
            macos: true,
            has: |_| true,
            ..env()
        };
        let qoop = || Some(PathBuf::from("/Users/me/.local/bin/qoop"));
        let click = click(&apple, qoop(), Some("/tmp/qv it's".into()), None).expect("a click");
        assert_eq!(click.bundle.as_deref(), Some("com.apple.Terminal"));
        let tn = call(
            Program::TerminalNotifier,
            "claude: Proceed?",
            Some((&click, 7)),
        );
        assert_eq!(
            tn.args,
            vec![
                "-title".to_string(),
                "qoop".into(),
                "-execute".into(),
                r"QOOP_RUNTIME_DIR='/tmp/qv it'\''s' '/Users/me/.local/bin/qoop' focus 7".into(),
                "-activate".into(),
                "com.apple.Terminal".into(),
            ]
        );
        assert_eq!(tn.stdin.as_deref(), Some("claude: Proceed?"));
        // The bundle id macOS handed down wins; no runtime directory, no variable.
        let handed = super::click(&apple, qoop(), None, Some("com.googlecode.iterm2".into()));
        let args = handed.expect("a click").args(3);
        assert_eq!(args[1], "'/Users/me/.local/bin/qoop' focus 3");
        assert_eq!(args[3], "com.googlecode.iterm2");
        // The escape route keeps no click.
        for term in ["iTerm.app", "WezTerm", "ghostty"] {
            let escaping = Env {
                term_program: Some(term.into()),
                ..apple.clone()
            };
            assert_eq!(super::click(&escaping, qoop(), None, None), None, "{term}");
        }
        let kitty = Env {
            kitty: true,
            ..apple.clone()
        };
        assert_eq!(super::click(&kitty, qoop(), None, None), None);
        let linux = Env {
            macos: false,
            ..apple.clone()
        };
        assert_eq!(super::click(&linux, qoop(), None, None), None);
        // Other programs never act on a click.
        let osa = call(Program::Osascript, "x", Some((&click, 7)));
        assert!(osa.args.iter().all(|a| !a.contains("focus")));
    }

    /// A cut never leaves half an emoji: no joiner or variation selector right before the
    /// ellipsis, and never one half of a flag.
    #[test]
    fn a_cut_note_ends_on_a_whole_character() {
        // 118 columns of words, then a family joined with a joiner: the cut lands inside it.
        let family = format!(
            "{}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "a".repeat(118)
        );
        let cut = clean(&family);
        assert!(cut.ends_with('…'), "{cut:?}");
        assert!(!cut.contains('\u{200d}'), "{cut:?}");
        // A flag is two regional indicators: 117 columns, then flags, cut after an odd one.
        let flags = format!("{}\u{1f1f9}\u{1f1f7}\u{1f1e9}\u{1f1ea}", "a".repeat(117));
        let cut = clean(&flags);
        let halves = cut
            .chars()
            .filter(|c| ('\u{1f1e6}'..='\u{1f1ff}').contains(c))
            .count();
        assert_eq!(halves % 2, 0, "{cut:?}");
        assert!(cut.ends_with('…'), "{cut:?}");
        assert!(crate::width::width(&cut) <= MAX_COLUMNS);
    }

    /// The `-execute` text through a real `/bin/sh -c`, as terminal-notifier runs it: paths
    /// with spaces, quotes, `$( )` and a backtick reach the program whole, and nothing in
    /// them runs.
    #[test]
    fn the_click_runs_through_a_real_shell_with_its_paths_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().expect("tempdir");
        let odd = "it's a \"dir\" $(touch pwned) `touch pwned2`";
        let dir = root.path().join(odd);
        std::fs::create_dir_all(&dir).expect("dir");
        let qoop = dir.join("qoop");
        std::fs::write(
            &qoop,
            "#!/bin/sh\nprintf '%s|%s|%s' \"$QOOP_RUNTIME_DIR\" \"$1\" \"$2\"\n",
        )
        .expect("script");
        std::fs::set_permissions(&qoop, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let runtime = dir.join("run time");
        let click = Click {
            qoop,
            runtime_dir: Some(runtime.to_string_lossy().into_owned()),
            bundle: None,
        };
        let args = click.args(7);
        assert_eq!(args[0], "-execute");
        assert_eq!(args.len(), 2, "no -activate without a bundle id");
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", &args[1]])
            .current_dir(root.path())
            .output()
            .expect("sh");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{}|focus|7", runtime.display())
        );
        assert!(!root.path().join("pwned").exists());
        assert!(!root.path().join("pwned2").exists());
    }
}

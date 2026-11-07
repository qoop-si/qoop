//! `qoop onboard`: the first start, one group of settings a screen.
//!
//! Eight steps under the word: the pet, the sounds, light or dark, the command key, the
//! notifications, sleep, qass, and whether qass may send notes about itself to qoop's maker. Their names sit under a line of dots that turns green as steps
//! pass; the current step says what it holds in one sentence and shows its choices. Left
//! and right choose (a sound plays), up and down move between a step's rows, enter goes on,
//! esc finishes. Each choice is written as it is made, so leaving early keeps what was set.
//! The rows are the settings page's own (`settings.rs`), so the two never disagree.
//! In the release build, a question follows the steps: sign in now (`qoop login`) or later.
//! Then, in every build with an agent qass runs on, qass starts and qoop opens on it, where qass
//! sets up the projects, servers and settings with the person.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use qoop_runtime::avatar::{self, Avatar};
use qoop_tui::config;
use qoop_tui::theme::Role;

use crate::Phone;
use crate::page::{self, Key, Player, Screen, Session, bold, ink, raised_bold};
use crate::settings::{self, Kind, Row};

/// The smallest window, the default terminal's: below `ROOMY` rows the step moves up a row
/// and the pet step shows the pet at half size.
const NEED: (usize, usize) = (80, 24);
const ROOMY: usize = 30;

struct Step {
    name: &'static str,
    title: &'static str,
    what: &'static str,
    /// The settings groups the step holds; none for the pet.
    groups: &'static [&'static str],
    /// Lines under the choices, for a step whose choice needs more than one sentence.
    also: &'static [&'static str],
}

const STEPS: [Step; 8] = [
    Step {
        name: "pet",
        title: "your pet",
        what: "the pet that sits beside the q in the sidebar",
        groups: &[],
        also: &[],
    },
    Step {
        name: "sound",
        title: "sounds",
        what: "what plays when an agent finishes, then when one waits on you",
        groups: &["sound"],
        also: &[],
    },
    Step {
        name: "look",
        title: "light or dark",
        what: "follow the terminal, or always dark, or always light",
        groups: &["look"],
        also: &[],
    },
    Step {
        name: "keys",
        title: "the command key",
        what: "the key that starts every qoop command",
        groups: &["keys"],
        also: &[],
    },
    Step {
        name: "notifications",
        title: "notifications",
        what: "how qoop tells you when you are not looking",
        groups: &["notifications"],
        // The notch row is a Mac's only, and on by default: what it is, and that it is a
        // login item, said where it is chosen.
        also: if cfg!(target_os = "macos") {
            &["notch: your agents at the top of the screen; it starts at login"]
        } else {
            &[]
        },
    },
    Step {
        name: "sleep",
        title: "sleep",
        what: "when idle tabs sleep to give back memory",
        groups: &["sleep"],
        also: &[],
    },
    Step {
        name: "qass",
        title: "qass",
        what: "the assistant over your agents: the agent `qoop qass` opens it in",
        groups: &["qass"],
        also: &[],
    },
    Step {
        name: "improve",
        title: "improve qass",
        what: "when qass's own way of working fails, it can send qoop's maker a short note",
        groups: &["report"],
        also: &[
            "Notes never carry your files, names or projects:",
            "a fixed form of common words, shown to you first.",
            "ask: asks each time. always: sends after your first yes.",
            "never: sends nothing.",
            "Sent with your account so it can be limited; the note carries no account.",
        ],
    },
];

struct Onboard {
    step: usize,
    row: usize,
    pet: Avatar,
    said: Option<String>,
    /// Ctrl-C, ctrl-d or a lost terminal ended it: nothing more is asked or started.
    stopped: bool,
    /// The qass step was reached: its agent is preselected once, so a pick made there and
    /// stepped back over is kept.
    qass_seen: bool,
}

impl Onboard {
    /// The rows of the current step, as the file holds them now.
    fn rows(&self) -> Vec<Row> {
        let groups = STEPS[self.step].groups;
        settings::rows(&[])
            .into_iter()
            .filter(|r| groups.contains(&r.group))
            .collect()
    }

    /// How many rows the step has: the pet has its three parts.
    fn count(&self) -> usize {
        if STEPS[self.step].groups.is_empty() {
            3
        } else {
            self.rows().len()
        }
    }

    /// The step at `w` x `h`, its rows (the step's settings) as given.
    fn draw(&self, w: usize, h: usize, rows: &[Row]) -> Screen {
        let mut s = Screen::new(w, h);
        let cx = w / 2;
        let step = &STEPS[self.step];
        let tight = h < ROOMY;
        let top = if tight { 9 } else { 10 };
        s.word(cx - 13, 1);
        if !step.groups.is_empty() {
            s.pet(cx + 20, 0, &self.pet);
        }
        // The named dots, joined by a line that turns green behind the current step; closer
        // on a narrow window.
        let gap = ((w - 10) / (STEPS.len() - 1)).min(12);
        let x0 = cx - (STEPS.len() - 1) * gap / 2;
        for (i, t) in STEPS.iter().enumerate() {
            let x = x0 + i * gap;
            let (dot, dot_ink) = match i.cmp(&self.step) {
                std::cmp::Ordering::Less => ("●", ink(Role::Done)),
                std::cmp::Ordering::Equal => ("●", ink(Role::Brand)),
                std::cmp::Ordering::Greater => ("○", ink(Role::FaintText)),
            };
            s.put(x, 6, dot, &dot_ink);
            if i + 1 < STEPS.len() {
                let line = if i < self.step {
                    ink(Role::Done)
                } else {
                    ink(Role::Rule)
                };
                s.put(x + 2, 6, &"─".repeat(gap - 3), &line);
            }
            let name_ink = match i.cmp(&self.step) {
                std::cmp::Ordering::Equal => bold(Role::Focused),
                std::cmp::Ordering::Less => ink(Role::Detail),
                std::cmp::Ordering::Greater => ink(Role::FaintText),
            };
            s.put(x.saturating_sub(t.name.len() / 2), 7, t.name, &name_ink);
        }
        s.centre(cx, top, step.title, &bold(Role::Focused));
        s.centre(cx, top + 1, step.what, &ink(Role::Detail));
        if step.groups.is_empty() {
            self.draw_pet(&mut s, cx, tight);
        } else {
            let below = self.draw_rows(&mut s, cx, w, top + 3, rows);
            for (y, line) in (below..).zip(step.also) {
                s.centre(cx, y, line, &ink(Role::FaintText));
            }
        }
        if let Some(said) = &self.said {
            s.centre(cx, h - 2, said, &ink(Role::FaintText));
        }
        let last = self.step + 1 == STEPS.len();
        let pet_step = step.groups.is_empty();
        s.centre_seg(
            cx,
            h - 3,
            &[
                ("←→", bold(Role::Focused)),
                (" choose   ", ink(Role::FaintText)),
                (if pet_step { "ctrl-r" } else { "" }, bold(Role::Focused)),
                (
                    if pet_step { " random pet   " } else { "" },
                    ink(Role::FaintText),
                ),
                ("↑↓", bold(Role::Focused)),
                (" row   ", ink(Role::FaintText)),
                ("enter", bold(Role::Focused)),
                (
                    if last { " done   " } else { " next step   " },
                    ink(Role::FaintText),
                ),
                ("esc", bold(Role::Focused)),
                (" finish", ink(Role::FaintText)),
            ],
        );
        s.status_bar(&[
            ("qoop onboard", "first start"),
            ("qoop settings", "any time later"),
        ]);
        s
    }

    fn draw_pet(&self, s: &mut Screen, cx: usize, tight: bool) {
        // The grid's first row is empty for every pet: start one up.
        let parts = if tight {
            s.pet(cx - 8, 11, &self.pet);
            17
        } else {
            s.pet2(cx - 16, 12, &self.pet);
            24
        };
        for (i, (part, value, hue)) in settings::part_titles(&self.pet).iter().enumerate() {
            let y = parts + i;
            if i == self.row {
                s.put(cx - 14, y, "▸", &ink(Role::Brand));
            }
            s.put(
                cx - 12,
                y,
                part,
                &if i == self.row {
                    bold(Role::Focused)
                } else {
                    ink(Role::Detail)
                },
            );
            let value_ink = hue.map_or_else(
                || bold(Role::Focused),
                |i| format!("{}\x1b[1m", page::raw(i)),
            );
            s.put(cx - 2, y, &format!("‹ {value} ›"), &value_ink);
        }
    }

    /// Each row on one line with its value; the selected one also opens its choices under it,
    /// wrapped and centred, the current one raised. One open at a time, and of a long list
    /// (Linux has 26 sounds) only the lines around the current one, keep a step inside 80 x 24.
    /// Returns the line under the last row.
    fn draw_rows(&self, s: &mut Screen, cx: usize, w: usize, mut y: usize, rows: &[Row]) -> usize {
        let macos = cfg!(target_os = "macos");
        for (i, row) in rows.iter().enumerate() {
            let on = i == self.row;
            if !on {
                s.centre_seg(
                    cx,
                    y,
                    &[
                        (&format!("{}  ", row.label), ink(Role::Detail)),
                        (&row.value, bold(Role::Focused)),
                    ],
                );
                y += 2;
                continue;
            }
            let label = format!("{}{}", if on { "▸ " } else { "  " }, row.label);
            s.centre(
                cx,
                y,
                &label,
                &if on {
                    bold(Role::Focused)
                } else {
                    ink(Role::Detail)
                },
            );
            y += 1;
            // The choices have the lines left above the keys, which keep a blank line over
            // them: after the rows under this one, the help an idle row has, and the step's
            // own lines with the blank before them.
            let also = STEPS[self.step].also.len();
            let under = 2 * (rows.len() - i - 1)
                + usize::from(row.hint)
                + if also > 0 { also + 1 } else { 0 };
            let room = (s.h - 4).saturating_sub(y + under);
            let (list, at) = settings::options(row, macos);
            for line in pick_lines(&list, at, w - 20, room) {
                let parts: Vec<(String, String)> = line
                    .into_iter()
                    .map(|(o, current)| {
                        let esc = if current && on {
                            raised_bold(Role::Focused)
                        } else if current {
                            bold(Role::Focused)
                        } else {
                            ink(Role::FaintText)
                        };
                        (format!(" {o}  "), esc)
                    })
                    .collect();
                let parts: Vec<(&str, String)> =
                    parts.iter().map(|(t, e)| (t.as_str(), e.clone())).collect();
                s.centre_seg(cx, y, &parts);
                y += 1;
            }
            if row.hint {
                s.centre(cx, y, &row.help, &ink(Role::FaintText));
                y += 1;
            }
            y += 1;
        }
        y
    }

    /// One key; `false` when onboarding is over.
    fn key(&mut self, key: Key, player: &mut Player) -> bool {
        let macos = cfg!(target_os = "macos");
        let pet_step = STEPS[self.step].groups.is_empty();
        match key {
            Key::Stop => {
                self.stopped = true;
                return false;
            }
            Key::Esc | Key::Char('q') => return false,
            Key::Enter | Key::Tab => {
                if self.step + 1 == STEPS.len() {
                    return false;
                }
                self.step += 1;
                self.row = 0;
                self.said = None;
                if STEPS[self.step].name == "qass" && !self.qass_seen {
                    self.qass_seen = true;
                    self.said = preselect_qass().err();
                }
            }
            Key::BackTab if self.step > 0 => {
                self.step -= 1;
                self.row = 0;
            }
            Key::Up if self.row > 0 => self.row -= 1,
            Key::Down if self.row + 1 < self.count() => self.row += 1,
            Key::CtrlR if pet_step => {
                settings::random_pet(&mut self.pet);
                self.said = avatar::save(&self.pet)
                    .err()
                    .map(|e| format!("the pet was not saved: {e}"));
            }
            Key::Left | Key::Right if pet_step => {
                settings::step_part(
                    &mut self.pet,
                    self.row,
                    if key == Key::Left { -1 } else { 1 },
                );
                self.said = avatar::save(&self.pet)
                    .err()
                    .map(|e| format!("the pet was not saved: {e}"));
            }
            Key::Left | Key::Right => {
                let Some(row) = self.rows().get(self.row).cloned() else {
                    return true;
                };
                let (list, at) = settings::options(&row, macos);
                if list.is_empty() {
                    return true;
                }
                let next = if key == Key::Left {
                    (at + list.len() - 1) % list.len()
                } else {
                    (at + 1) % list.len()
                };
                self.said = settings::write(&row, &list[next]).err();
                if let Kind::Sound(..) = row.kind {
                    player.play(&list[next], macos);
                }
                if row.label == "theme" {
                    settings::apply_theme();
                }
            }
            _ => {}
        }
        true
    }
}

/// The fewest lines of choices an open row shows, whatever room it has.
const PICK_LINES: usize = 3;

/// `list` in lines of at most `width` columns, each choice marked when it is the current
/// one (`at`); of more lines than `room` (or `PICK_LINES`), only those around the current
/// one, with a "…" at the start or the end where some are left out.
fn pick_lines(list: &[String], at: usize, width: usize, room: usize) -> Vec<Vec<(String, bool)>> {
    let room = room.max(PICK_LINES);
    let mut lines: Vec<Vec<(String, bool)>> = vec![Vec::new()];
    let mut used = 0;
    let mut current = 0;
    for (k, o) in list.iter().enumerate() {
        let cell = o.len() + 3;
        if used > 0 && used + cell > width {
            lines.push(Vec::new());
            used = 0;
        }
        if k == at {
            current = lines.len() - 1;
        }
        if let Some(l) = lines.last_mut() {
            l.push((o.clone(), k == at));
        }
        used += cell;
    }
    let total = lines.len();
    let start = current
        .saturating_sub(room / 2)
        .min(total.saturating_sub(room));
    let mut shown: Vec<_> = lines.into_iter().skip(start).take(room).collect();
    // Faint as any choice that is not the current one.
    let more = || ("…".to_string(), false);
    if start > 0 {
        shown[0].insert(0, more());
    }
    if start + room < total {
        shown[room - 1].push(more());
    }
    shown
}

/// `qoop onboard`. `phone` is the release build's part, which has `qoop login`.
pub fn run(args: &[String], phone: Option<Phone>) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoop onboard"));
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        println!(
            "qoop onboard needs a terminal to show its steps; qoop settings changes anything later"
        );
        // install.sh leaves the sign-in to the onboarding: it is still said.
        if phone.is_some() {
            println!("{LATER}");
        }
        return Ok(());
    }
    settings::apply_theme();
    let mut onboard = Onboard {
        step: 0,
        row: 0,
        pet: settings::the_pet(),
        said: None,
        stopped: false,
        qass_seen: false,
    };
    // A pet chosen here is a pet picked: saved even when only enter is pressed.
    if avatar::load().is_none() {
        onboard.said = avatar::save(&onboard.pet)
            .err()
            .map(|e| format!("the pet was not saved: {e}"));
    }
    let session = Session::start()?;
    let mut player = Player::new();
    'run: loop {
        let (w, h) = page::size();
        page::show(
            &onboard.draw(w.max(NEED.0), h.max(NEED.1), &onboard.rows()),
            NEED,
            "esc",
        )?;
        for key in page::next_keys(&session.typed) {
            if !onboard.key(key, &mut player) {
                break 'run;
            }
        }
    }
    drop(session);
    // Finished before the qass step: the agent it would have preselected is chosen now, as
    // finish keeps whatever the file names.
    if !onboard.stopped && !onboard.qass_seen {
        let _ = preselect_qass();
    }
    if let Some(code) = after_steps(onboard.stopped, phone, finish)? {
        std::process::exit(code);
    }
    Ok(())
}

/// What follows the steps. A stop (Ctrl-C, ctrl-d, a lost terminal) asks and starts nothing,
/// says the next step and gives exit code 130; else `finish` signs in with `phone` and starts
/// qass.
fn after_steps(
    stopped: bool,
    phone: Option<Phone>,
    finish: impl FnOnce(Option<Phone>) -> std::io::Result<()>,
) -> std::io::Result<Option<i32>> {
    if stopped {
        // Never a panic on a terminal that is gone: the exit code still says it.
        let _ = writeln!(std::io::stderr(), "{STOPPED}");
        return Ok(Some(130));
    }
    finish(phone)?;
    Ok(None)
}

/// The end of a finished onboarding: the sign-in, the usage notice, then qass.
fn finish(phone: Option<Phone>) -> std::io::Result<()> {
    // Asked the moment the steps close, before the notice and its pause: a question that came
    // seconds later took the next command a person typed as its answer. Nothing counts before
    // the notice is seen (its mark), so a sign-in first counts nothing.
    if let Some(phone) = phone
        && let Some(hint) = sign_in(phone, ask_yes(SIGN_IN, ask))
    {
        println!("{hint}");
    }
    // What the usage counts are, before anything is counted (ADR-023).
    crate::telemetry::notice_once();
    let key = command_key(config::read().config.prefix);
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    let agent = match qass_start(&config::read(), |a| here(a, home.as_deref())) {
        Ok(agent) => agent,
        Err(line) => {
            println!("{line}");
            return Ok(());
        }
    };
    // Claude's own settings: under CLAUDE_CONFIG_DIR, else `~/.claude.json` and `~/.claude/`.
    let (json, settings) = match std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from) {
        Some(dir) => (
            Some(dir.join(".claude.json")),
            Some(dir.join("settings.json")),
        ),
        None => (
            home.as_deref().map(|h| h.join(".claude.json")),
            home.as_deref().map(|h| h.join(".claude/settings.json")),
        ),
    };
    let warns = agent == "claude" && claude_warns(json.as_deref(), settings.as_deref());
    if warns {
        // Its warning defaults to "No, exit": this line is read before it comes.
        println!("{}", claude_warning(&key));
        let _ = ask(GO_ON);
    }
    // Inside a qoop tab, an interface is already around it: qass is shown there, never nested.
    let here = qoop_runtime::legacy::var_os("QOOP_TERMINAL").is_some();
    match open_qass(&agent, here) {
        Ok(()) if here => {}
        Ok(()) => println!("{}", after_qass()),
        Err(e) => {
            println!("qass did not start: {e}");
            println!("{}", last_line(&key));
        }
    }
    Ok(())
}

/// `question` asked on the terminal, its answer when one was typed. A key typed for what came
/// before (an enter pressed twice on the last page) is dropped first, so it never answers.
fn ask(question: &str) -> Option<String> {
    // SAFETY: tcflush on standard input only discards what is queued there.
    unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut said = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut said)
        .ok()
        .filter(|&n| n > 0)
        .map(|_| said)
}

/// `question` until it gets a yes or a no, through `ask`. No answer at all (end of input) is a
/// no, and so are three answers that are neither, so input piped in never loops.
fn ask_yes(question: &str, mut ask: impl FnMut(&str) -> Option<String>) -> bool {
    for _ in 0..3 {
        match ask(question).map(|said| yes_no(&said)) {
            None => return false,
            Some(Some(yes)) => return yes,
            Some(None) => println!("answer y or n"),
        }
    }
    false
}

/// A typed answer: enter alone, y or yes is a yes, n or no a no, anything else neither (a
/// command typed for the shell, which must not be read as a no).
fn yes_no(said: &str) -> Option<bool> {
    match said.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

const SIGN_IN: &str = "Sign in so this computer shows on qoop.si/app and your phone? [Y/n] ";
const LATER: &str = "later: qoop login";
const STOPPED: &str = "onboarding stopped; run `qoop onboard` to finish";
const GO_ON: &str = "Press enter to open qoop with qass. ";
const NO_AGENT: &str =
    "qass runs on claude, codex, gemini or opencode; install one, then: qoop qass";

/// After a yes, `qoop login`; the line to print after it, if any. Asked first because a login
/// waits up to ten minutes for the browser. Nothing here fails onboarding: a no, no answer, no
/// network or a code nobody confirmed all end on the hint.
fn sign_in(phone: Phone, yes: bool) -> Option<String> {
    if !yes {
        return Some(LATER.into());
    }
    match phone(&["qoop".into(), "login".into()]) {
        Ok(()) => None,
        Err(e) => Some(format!("not signed in: {e}. {LATER}")),
    }
}

/// The command key as it is pressed: `ctrl-b` for prefix 2.
fn command_key(prefix: u8) -> String {
    format!("ctrl-{}", char::from(b'a' + prefix - 1))
}

/// What onboarding ends on without qass: the next thing to do, with the command key just chosen.
fn last_line(key: &str) -> String {
    format!(
        "qoop is set up. Next: start qoop, then {key} Q opens qass, which sets up your projects \
         with you (settings: qoop settings)"
    )
}

/// The agent qass starts on, and the one its settings row shows chosen: the one the file names
/// when it is here (a person's own `[agents]` line always is: its shell may know names this
/// process does not), else the first one here.
pub(crate) fn qass_agent(read: &config::Read, here: impl Fn(&str) -> bool) -> Option<String> {
    let named = &read.qass_agent;
    if read.agents.iter().any(|a| &a.name == named) || here(named) {
        return Some(named.clone());
    }
    config::QASS_AGENTS
        .into_iter()
        .find(|a| here(a))
        .map(String::from)
}

/// The agent qass starts on after the steps: the one the file names, kept, as the qass step
/// preselected it once and a pick made there is the person's; else the line to say instead.
fn qass_start(read: &config::Read, here: impl Fn(&str) -> bool) -> Result<String, String> {
    let named = &read.qass_agent;
    if read.agents.iter().any(|a| &a.name == named) || here(named) {
        return Ok(named.clone());
    }
    if qass_agent(read, &here).is_none() {
        return Err(NO_AGENT.into());
    }
    Err(format!(
        "qass runs on {named}, which is not installed here: install it, then qoop qass"
    ))
}

/// On reaching the qass step: an agent the file names that is not here (the default, claude,
/// on a machine without it) gives way to the first one that is, written, so the row shows it
/// and its arrows start from it. The settings page always shows what the file says.
fn preselect_qass() -> Result<(), String> {
    let read = config::read();
    match qass_agent(&read, here_now) {
        Some(agent) if agent != read.qass_agent => {
            config::set(&config::path(), "qass.agent", &agent)
        }
        _ => Ok(()),
    }
}

/// Whether an agent qass runs on by name is installed: on `PATH`, or Claude's own local
/// install, which its installer reaches through a shell alias. Not an install folder off
/// `PATH`: qass's tab starts the agent by its bare name.
fn here(agent: &str, home: Option<&Path>) -> bool {
    crate::hooks::on_path(agent)
        || (agent == "claude" && home.is_some_and(|h| h.join(".claude/local/claude").exists()))
}

/// [`here`], with this process's home.
pub(crate) fn here_now(agent: &str) -> bool {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    here(agent, home.as_deref())
}

/// Whether Claude shows a screen before qass can start on it: its first start not finished
/// (`json`, `.claude.json`), or its permissions warning never accepted (`settings`), which
/// qass's flag brings up.
fn claude_warns(json: Option<&Path>, settings: Option<&Path>) -> bool {
    let holds = |file: Option<&Path>, key: &str| {
        file.and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| v[key] == true)
    };
    !(holds(json, "hasCompletedOnboarding") && holds(settings, "skipDangerousModePermissionPrompt"))
}

/// Said before Claude's screens: what to choose at its warning, what that means, the way back.
fn claude_warning(key: &str) -> String {
    format!(
        "claude may ask a few first questions. At its warning choose \"Yes, I accept\":\nqass then \
         runs commands without asking each time, inside its own rules. If it exits, {key} Q starts \
         qass again."
    )
}

/// `agent` saved as qass's (which also writes the settings file, so qass's setup
/// finds them kept), the daemon, qass started without its board, then qoop opened on it as its
/// own process, so a restart of that interface starts a plain `qoop` and never onboarding again.
fn open_qass(agent: &str, inside: bool) -> std::io::Result<()> {
    config::set(&config::path(), "qass.agent", agent).map_err(std::io::Error::other)?;
    let dir = qoop_runtime::paths::runtime_dir().map_err(std::io::Error::other)?;
    crate::ensure_daemon(&qoop_runtime::paths::socket_path(&dir))?;
    let terminal = crate::qass::start(false)?;
    if inside {
        let _ = crate::qass::run(&["focus", &terminal.to_string()]);
        return Ok(());
    }
    // qass runs from here on: a qoop that does not open is said, and the line after says how.
    let opened = crate::own_path()
        .ok_or_else(|| std::io::Error::other("no path to this program"))
        .and_then(|me| {
            std::process::Command::new(me)
                .env(qoop_tui::run::FOCUS_ENV, terminal.to_string())
                .status()
        });
    if let Err(e) = opened {
        println!("qoop did not open: {e}");
    }
    Ok(())
}

/// Said when that qoop is closed: qass still runs, and the command that opens it again, by
/// full path while `qoop` is not on `PATH` yet.
fn after_qass() -> String {
    let cmd = if crate::hooks::on_path("qoop") {
        "qoop".to_string()
    } else {
        crate::own_path().map_or_else(|| "qoop".into(), |p| p.display().to_string())
    };
    format!("qass keeps running in qoop; open it again: {cmd}")
}

#[cfg(test)]
mod tests {
    use super::*;

    std::thread_local! {
        static CALLED: std::cell::RefCell<Vec<Vec<String>>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn signs_in(args: &[String]) -> std::io::Result<()> {
        CALLED.with(|c| c.borrow_mut().push(args.to_vec()));
        Ok(())
    }

    fn cannot_reach(args: &[String]) -> std::io::Result<()> {
        CALLED.with(|c| c.borrow_mut().push(args.to_vec()));
        Err(std::io::Error::other("the hub did not answer"))
    }

    fn calls() -> Vec<Vec<String>> {
        CALLED.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }

    #[test]
    fn a_yes_runs_qoop_login_and_a_no_leaves_it_for_later() {
        let login = vec![vec!["qoop".to_string(), "login".to_string()]];
        assert_eq!(sign_in(signs_in, true), None);
        assert_eq!(calls(), login);
        assert_eq!(
            sign_in(signs_in, false).as_deref(),
            Some("later: qoop login")
        );
        assert!(calls().is_empty());
        assert_eq!(
            sign_in(cannot_reach, true).as_deref(),
            Some("not signed in: the hub did not answer. later: qoop login")
        );
        assert_eq!(calls(), login);
    }

    /// Enter, y and yes are a yes, n and no a no, and a command typed for the shell (two Mac
    /// users' next `clear`) is neither, so it is asked again rather than taken as a no.
    #[test]
    fn only_a_yes_or_a_no_answers_the_sign_in() {
        for yes in ["\n", "y\n", "Y\n", "yes\n", " YES \n"] {
            assert_eq!(yes_no(yes), Some(true), "{yes:?}");
        }
        for no in ["n\n", "No\n", "no"] {
            assert_eq!(yes_no(no), Some(false), "{no:?}");
        }
        for other in [
            "clear\n",
            "later\n",
            "ls -la\n",
            "yess\n",
            "cd ~/repo && qoop\n",
        ] {
            assert_eq!(yes_no(other), None, "{other:?}");
        }
    }

    /// An answer that is neither is asked again, up to three times, then taken as a no; the end
    /// of input is a no at once.
    #[test]
    fn the_sign_in_asks_again_three_times_at_most() {
        let answers = |said: &'static [&'static str]| {
            let mut said = said.iter();
            move |_: &str| said.next().map(|s| s.to_string())
        };
        assert!(ask_yes(SIGN_IN, answers(&["clear\n", "ls\n", "y\n"])));
        assert!(!ask_yes(SIGN_IN, answers(&["clear\n", "n\n"])));
        assert!(!ask_yes(SIGN_IN, answers(&["a\n", "b\n", "c\n", "y\n"])));
        assert!(!ask_yes(SIGN_IN, answers(&["clear\n"])));
        let mut asked = 0;
        ask_yes(SIGN_IN, |_| {
            asked += 1;
            Some("garbage\n".into())
        });
        assert_eq!(asked, 3, "piped garbage never loops");
    }

    #[test]
    fn it_ends_on_qass_with_the_chosen_key() {
        assert!(last_line(&command_key(2)).contains("ctrl-b Q opens qass"));
        assert!(last_line(&command_key(1)).contains("ctrl-a Q"));
    }

    fn read_naming(agent: &str, launchers: &str) -> config::Read {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, format!("[qass]\nagent = \"{agent}\"\n{launchers}")).unwrap();
        config::read_from(&file)
    }

    #[test]
    fn qass_starts_on_the_named_agent_when_it_is_here_else_on_the_first_one_here() {
        let only = |have: &'static [&'static str]| move |a: &str| have.contains(&a);
        let codex = read_naming("codex", "");
        assert_eq!(
            qass_agent(&codex, only(&["codex", "claude"])).as_deref(),
            Some("codex")
        );
        assert_eq!(
            qass_agent(&codex, only(&["opencode", "gemini"])).as_deref(),
            Some("gemini")
        );
        assert_eq!(qass_agent(&codex, only(&[])), None);
        // A person's own line is theirs, whatever this process finds on its PATH.
        let own = read_naming("mine", "[agents]\nmine = \"FOO=1 ~/bin/claude\"\n");
        assert_eq!(qass_agent(&own, only(&[])).as_deref(), Some("mine"));
    }

    /// After the steps the named agent is kept even when another one is here: a pick of one not
    /// installed is said, never replaced.
    #[test]
    fn qass_starts_on_the_named_agent_or_says_why_not() {
        let only = |have: &'static [&'static str]| move |a: &str| have.contains(&a);
        let gemini = read_naming("gemini", "");
        assert_eq!(
            qass_start(&gemini, only(&["gemini", "codex"])),
            Ok("gemini".into())
        );
        assert_eq!(
            qass_start(&gemini, only(&["codex"])),
            Err(
                "qass runs on gemini, which is not installed here: install it, then qoop qass"
                    .into()
            )
        );
        assert_eq!(qass_start(&gemini, only(&[])), Err(NO_AGENT.into()));
        let own = read_naming("mine", "[agents]\nmine = \"FOO=1 ~/bin/claude\"\n");
        assert_eq!(qass_start(&own, only(&[])), Ok("mine".into()));
    }

    #[test]
    fn claude_warns_until_its_first_start_is_done_and_its_warning_accepted() {
        let home = tempfile::tempdir().unwrap();
        let (json, settings) = (home.path().join(".claude.json"), home.path().join("s.json"));
        let warns = || claude_warns(Some(&json), Some(&settings));
        assert!(warns());
        std::fs::write(&json, r#"{"hasCompletedOnboarding": false}"#).unwrap();
        assert!(warns());
        std::fs::write(
            &json,
            r#"{"numStartups": 3, "hasCompletedOnboarding": true}"#,
        )
        .unwrap();
        assert!(warns(), "a claude user who never accepted the warning");
        std::fs::write(&settings, r#"{"skipDangerousModePermissionPrompt": true}"#).unwrap();
        assert!(!warns());
        assert!(claude_warns(None, None));
    }

    #[test]
    fn the_warning_line_says_what_to_choose_what_it_means_and_the_way_back() {
        let line = claude_warning("ctrl-b");
        assert!(line.contains("\"Yes, I accept\"") && line.contains("without asking"));
        assert!(line.ends_with("ctrl-b Q starts qass again."), "{line}");
    }

    /// Claude's local install counts; an agent in an install folder off `PATH` does not, as the
    /// tab qass starts runs it by its bare name.
    #[test]
    fn only_an_agent_its_tab_can_start_by_name_is_here() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let on_path = crate::hooks::on_path("claude");
        assert_eq!(here("claude", Some(home.path())), on_path);
        std::fs::create_dir_all(home.path().join(".claude/local")).unwrap();
        std::fs::write(home.path().join(".claude/local/claude"), "").unwrap();
        assert!(here("claude", Some(home.path())));
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("gemini"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(bin.join("gemini"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(
            here("gemini", Some(home.path())),
            crate::hooks::on_path("gemini")
        );
        assert!(!here("no-such-agent-here", Some(home.path())));
        assert!(!here("no-such-agent-here", None));
    }

    /// Seen once, the qass step is walked into again with nothing preselected (and here, with
    /// no write to this machine's own config file).
    #[test]
    fn enter_walks_the_steps_and_ends_after_the_last() {
        let mut o = Onboard {
            step: 0,
            row: 2,
            pet: settings::first_pet(),
            said: None,
            stopped: false,
            qass_seen: true,
        };
        let mut player = Player::new();
        for step in 1..STEPS.len() {
            assert!(o.key(Key::Enter, &mut player));
            assert_eq!((o.step, o.row), (step, 0));
            assert!(o.qass_seen && o.said.is_none(), "step {step}");
        }
        assert!(!o.key(Key::Enter, &mut player));
        o.key(Key::BackTab, &mut player);
        assert_eq!(o.step, STEPS.len() - 2);
    }

    /// A stop never reaches what follows the steps, with the phone part present too: no
    /// sign-in, no qass, exit code 130; a finish goes on to them with the phone.
    #[test]
    fn a_stop_skips_the_sign_in_and_qass_even_with_the_phone_part() {
        // The sign-in a finish does with the phone part, answered yes.
        let finish = |phone: Option<Phone>| {
            if let Some(phone) = phone {
                sign_in(phone, true);
            }
            Ok(())
        };
        assert_eq!(
            after_steps(true, Some(signs_in), finish).unwrap(),
            Some(130)
        );
        assert!(calls().is_empty(), "no sign-in after a stop");
        assert_eq!(after_steps(false, Some(signs_in), finish).unwrap(), None);
        assert_eq!(calls().len(), 1, "a finish signs in");
    }

    /// Ctrl-C (and ctrl-d) stops onboarding where it is: the run then prints the next step and
    /// exits 130, never walking on into the sign-in and qass. Esc and `q` finish it as before.
    #[test]
    fn ctrl_c_stops_onboarding_and_esc_finishes_it() {
        let mut player = Player::new();
        let typed = |bytes: &[u8]| page::decode(bytes).0[0];
        for (key, stopped) in [
            (typed(b"\x03"), true),
            (typed(b"\x04"), true),
            (Key::Esc, false),
            (typed(b"q"), false),
        ] {
            let mut o = Onboard {
                step: 1,
                row: 0,
                pet: settings::first_pet(),
                said: None,
                stopped: false,
                qass_seen: false,
            };
            assert!(!o.key(key, &mut player), "{key:?} ends it");
            assert_eq!(o.stopped, stopped, "{key:?}");
        }
        assert!(STOPPED.contains("`qoop onboard`"));
    }

    /// The frame without its escapes, a row a line.
    fn plain(frame: &str) -> String {
        let mut out = String::new();
        let mut in_esc = false;
        for ch in frame.chars() {
            match (in_esc, ch) {
                (false, '\x1b') => in_esc = true,
                (false, ch) => out.push(ch),
                (true, 'H') => {
                    in_esc = false;
                    out.push('\n');
                }
                (true, 'm') => in_esc = false,
                (true, _) => {}
            }
        }
        out
    }

    #[test]
    fn every_step_fits_the_smallest_window() {
        for (w, h) in [NEED, (100, ROOMY)] {
            for (step, each) in STEPS.iter().enumerate() {
                let o = Onboard {
                    step,
                    row: 0,
                    pet: settings::first_pet(),
                    said: None,
                    stopped: false,
                    qass_seen: false,
                };
                let frame = plain(&o.draw(w, h, &o.rows()).frame());
                assert!(frame.contains(each.what), "{w}x{h} step {step}");
                for line in each.also {
                    assert!(frame.contains(line), "{w}x{h} step {step}: {line}");
                }
                // Every step's name stands apart from its neighbours.
                for t in STEPS {
                    assert!(
                        frame.contains(&format!(" {} ", t.name)),
                        "{w}x{h}: {}",
                        t.name
                    );
                }
                assert!(frame.contains("esc"), "{w}x{h} step {step}: the keys");
            }
        }
    }

    /// Linux's 26 sounds, open on either sound row and at any of them, end above the keys at
    /// 80 x 24, 80 x 30 and 120 x 40, the current one on screen: as many lines of them as fit,
    /// all of them at 120 x 40, and a "…" where some are left out.
    #[test]
    fn a_long_list_of_sounds_ends_above_the_keys() {
        let sounds: Vec<String> = config::FREEDESKTOP_SOUNDS.map(String::from).to_vec();
        let step = STEPS.iter().position(|s| s.name == "sound").unwrap();
        for (w, h) in [NEED, (80, ROOMY), (120, 40)] {
            for row in 0..3 {
                for now in &sounds {
                    let rows: Vec<Row> = settings::rows(&[])
                        .into_iter()
                        .filter(|r| r.group == "sound")
                        .map(|r| match r.kind {
                            Kind::Sound(key, _) => Row {
                                value: now.clone(),
                                kind: Kind::Choice(key, sounds.clone()),
                                ..r
                            },
                            _ => r,
                        })
                        .collect();
                    let o = Onboard {
                        step,
                        row,
                        pet: settings::first_pet(),
                        said: None,
                        stopped: false,
                        qass_seen: false,
                    };
                    let frame = plain(&o.draw(w, h, &rows).frame());
                    let at = format!("{w}x{h} row {row} at {now}:\n{frame}");
                    // A line a row, after the empty one before the first row's place.
                    let lines: Vec<&str> = frame.lines().skip(1).collect();
                    let keys = lines[h - 3].trim();
                    assert!(
                        keys.starts_with("←→ choose") && keys.ends_with("esc finish"),
                        "{at}"
                    );
                    assert!(lines[h - 4].trim().is_empty(), "{at}");
                    assert!(frame.contains(&format!(" {now} ")), "{at}");
                    if row == 0 {
                        continue;
                    }
                    let shown = sounds
                        .iter()
                        .filter(|s| frame.contains(&format!(" {s} ")))
                        .count();
                    assert_eq!(frame.contains(" … "), shown < sounds.len(), "{at}");
                    match (w, h) {
                        (120, 40) => assert_eq!(shown, sounds.len(), "{at}"),
                        (80, 24) => assert!(shown < sounds.len(), "{at}"),
                        _ => {}
                    }
                }
            }
        }
    }
}

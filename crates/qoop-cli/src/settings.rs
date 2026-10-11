//! `qoop settings`: every setting on one page, with the pet.
//!
//! The left column is the pet, twice as big, and its three parts. The right half is a
//! search line with filter chips on its right, then every setting as a row under its
//! group, the selected row opening its picker under itself. Typing searches, tab moves the
//! chip, up and down walk the rows (up from the first row reaches the pet's parts), left and
//! right change the value and write it at once, esc clears the search or leaves.
//!
//! Machines, phones, hooks and skills are shown as their own commands print them: this
//! page reads them, the commands still change them.

use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

use qoop_runtime::avatar::{self, Avatar};
use qoop_tui::config::{self, Theme};
use qoop_tui::notify;
use qoop_tui::theme::{self, Role};

use crate::page::{self, Key, Player, Screen, Session, bold, ink, raised, raised_bold};

/// The window the whole page fits in, pet column included.
pub const NEED: (usize, usize) = (100, 30);
/// The smallest window the page is drawn in: below `NEED` the pet column folds away and the
/// settings take the width (a tab beside the sidebar is often 91 x 34).
pub const MIN: (usize, usize) = (64, 16);
/// Where the pet column ends and the settings begin.
const DIV: usize = 40;
const CHIPS: [&str; 6] = ["all", "sound", "look", "agents", "machines", "phones"];
const IDLE: [u64; 4] = [0, 15, 30, 60];

/// How a row changes.
#[derive(Clone, PartialEq)]
pub(crate) enum Kind {
    /// A true or false key.
    Toggle(&'static str),
    /// A sound key; `true` for the one that waits on you.
    Sound(&'static str, bool),
    /// One of a list of words.
    Choice(&'static str, Vec<String>),
    /// Minutes before an idle tab sleeps.
    Idle,
    /// Shown, changed elsewhere.
    Info,
}

#[derive(Clone)]
pub(crate) struct Row {
    pub group: &'static str,
    pub label: String,
    pub value: String,
    pub help: String,
    pub kind: Kind,
}

/// Every row, from the file as it is now and the lists read once at the start.
pub(crate) fn rows(lists: &[Row]) -> Vec<Row> {
    let read = config::read();
    let c = read.config;
    let macos = cfg!(target_os = "macos");
    let on = |b: bool| if b { "on" } else { "off" }.to_string();
    let row = |group, label: &str, value: String, help: &str, kind| Row {
        group,
        label: label.to_string(),
        value,
        help: help.to_string(),
        kind,
    };
    let mut out = vec![
        row(
            "sound",
            "sound",
            on(c.sound),
            "play a sound when an agent needs you",
            Kind::Toggle("notify.sound"),
        ),
        row(
            "sound",
            "agent finished",
            notify::playable(c.done_sound, false, macos).to_string(),
            "left and right play the others",
            Kind::Sound("notify.done_sound", false),
        ),
        row(
            "sound",
            "agent waits",
            notify::playable(c.alert_sound, true, macos).to_string(),
            "an agent waits on you or failed",
            Kind::Sound("notify.alert_sound", true),
        ),
        row(
            "look",
            "theme",
            c.theme.name().to_string(),
            "auto follows the terminal",
            Kind::Choice(
                "theme",
                ["auto", "dark", "light"].map(String::from).to_vec(),
            ),
        ),
        row(
            "look",
            "icons",
            c.icons.name().to_string(),
            "auto draws them where the terminal can",
            Kind::Choice("icons", ["auto", "on", "off"].map(String::from).to_vec()),
        ),
        row(
            "keys",
            "prefix",
            format!("ctrl-{}", char::from(b'a' + c.prefix - 1)),
            "the key that starts a command",
            Kind::Choice("prefix", config::prefixes()),
        ),
        row(
            "notifications",
            "desktop",
            on(c.desktop),
            // Where a note would go from this terminal, as the interface decides it.
            &format!(
                "notes go to {}",
                notify::describe(notify::route(&notify::Env::read()))
            ),
            Kind::Toggle("notify.desktop"),
        ),
    ];
    if macos {
        out.push(row(
            "notifications",
            "notch",
            on(c.notch),
            "your agents in the macOS notch",
            Kind::Toggle("notch.enabled"),
        ));
    }
    let idle = c.lifecycle.idle_minutes;
    out.push(row(
        "sleep",
        "idle tabs sleep",
        if idle == 0 {
            "never".to_string()
        } else {
            format!("{idle} min")
        },
        "the daemon takes it up on its next check",
        Kind::Idle,
    ));
    out.push(row(
        "sleep",
        "daemon leaves",
        on(c.lifecycle.exit_when_asleep),
        "when everything sleeps",
        Kind::Toggle("daemon.exit_when_asleep"),
    ));
    out.push(row(
        "qass",
        "qass runs on",
        read.qass_agent.clone(),
        "the agent `qoop qass` opens: yours from [agents] too",
        Kind::Choice("qass.agent", qass_agents(&read)),
    ));
    out.push(row(
        "report",
        "notes to qoop's maker",
        read.report_send.clone(),
        "about qass's own way of working, never your files, names or projects",
        Kind::Choice(
            "report.send",
            config::REPORT_SENDS.map(String::from).to_vec(),
        ),
    ));
    for agent in &read.agents {
        out.push(row(
            "agents",
            &agent.name,
            String::new(),
            &agent.command,
            Kind::Info,
        ));
    }
    out.extend(lists.iter().cloned());
    out
}

/// What qass may run on here: the agents it starts on that are on `PATH`, the
/// `[agents]` names, and whatever the file names now.
pub(crate) fn qass_agents(read: &config::Read) -> Vec<String> {
    let mut out: Vec<String> = config::QASS_AGENTS
        .iter()
        .filter(|a| crate::hooks::on_path(a))
        .map(|a| a.to_string())
        .chain(read.agents.iter().map(|a| a.name.clone()))
        .collect();
    if !out.contains(&read.qass_agent) {
        out.insert(0, read.qass_agent.clone());
    }
    out
}

/// What another command prints, one row a line: the first word (or, where names hold
/// spaces, the part before two spaces) is the label, the rest the help.
pub(crate) fn listed(group: &'static str, args: &[&str], by_two_spaces: bool) -> Vec<Row> {
    let Ok(me) = std::env::current_exe() else {
        return Vec::new();
    };
    let Ok(out) = std::process::Command::new(me)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        // Not the usage lines a command prints under its list (`qoop hooks install ...`).
        .filter(|line| {
            let line = line.trim_start();
            !line.is_empty() && !line.starts_with(&format!("qoop {} ", args[0]))
        })
        .map(|line| {
            let line = line.trim();
            // A sentence ("no phones paired...") is help, not a name.
            let (label, help) = if line.starts_with("no ") {
                ("", line)
            } else if by_two_spaces {
                line.split_once("  ").unwrap_or((line, ""))
            } else {
                line.split_once(char::is_whitespace).unwrap_or((line, ""))
            };
            Row {
                group,
                label: label.trim().to_string(),
                value: if group == "machines" && !label.is_empty() {
                    "ssh".to_string()
                } else {
                    String::new()
                },
                help: help.trim().to_string(),
                kind: Kind::Info,
            }
        })
        .collect()
}

fn group_ink(group: &str) -> Role {
    match group {
        "sound" | "notifications" | "hooks" | "skills" => Role::Blocked,
        "look" | "phones" => Role::Done,
        "keys" | "sleep" => Role::Sleeping,
        _ => Role::Working,
    }
}

/// The rows the search and the chip leave.
fn filtered(all: &[Row], query: &str, chip: usize) -> Vec<Row> {
    let query = query.to_lowercase();
    all.iter()
        .filter(|r| match CHIPS[chip] {
            "all" => true,
            "look" => matches!(r.group, "look" | "keys"),
            "agents" => matches!(r.group, "agents" | "qass" | "report"),
            chip => r.group == chip,
        })
        .filter(|r| {
            query.is_empty()
                || format!("{} {} {} {}", r.group, r.label, r.value, r.help)
                    .to_lowercase()
                    .contains(&query)
        })
        .cloned()
        .collect()
}

/// The options a row steps through, and which one it is on.
pub(crate) fn options(row: &Row, macos: bool) -> (Vec<String>, usize) {
    let (list, now): (Vec<String>, String) = match &row.kind {
        Kind::Toggle(_) => (vec!["on".into(), "off".into()], row.value.clone()),
        Kind::Sound(..) => {
            let list: Vec<String> = config::sounds_here()
                .iter()
                .filter(|n| macos || notify::sound(false, false, n, n).is_some())
                .map(|n| n.to_string())
                .collect();
            (list, row.value.clone())
        }
        Kind::Choice(_, list) => (list.clone(), row.value.clone()),
        Kind::Idle => {
            let mut list: Vec<String> = IDLE
                .iter()
                .map(|m| {
                    if *m == 0 {
                        "never".to_string()
                    } else {
                        format!("{m} min")
                    }
                })
                .collect();
            // A value written by hand stays among the choices rather than being lost.
            if !list.contains(&row.value) {
                list.push(row.value.clone());
            }
            (list, row.value.clone())
        }
        Kind::Info => (Vec::new(), String::new()),
    };
    let at = list.iter().position(|o| *o == now).unwrap_or(0);
    (list, at)
}

/// Write `value` for `row`, as the file spells it.
pub(crate) fn write(row: &Row, value: &str) -> Result<(), String> {
    let path = config::path();
    match &row.kind {
        Kind::Toggle(key) => config::set(&path, key, if value == "on" { "true" } else { "false" }),
        // Choosing a sound is wanting to hear it, as in `qoop sound`.
        Kind::Sound(key, _) => {
            config::set(&path, key, value).and_then(|()| config::set(&path, "notify.sound", "true"))
        }
        Kind::Choice(key, _) => config::set(&path, key, value),
        Kind::Idle => {
            let minutes = if value == "never" {
                "0".to_string()
            } else {
                value.trim_end_matches(" min").to_string()
            };
            config::set(&path, "sleep.idle_minutes", &minutes)
        }
        Kind::Info => Ok(()),
    }
}

/// Draw for the background the config asks for; `auto` takes `COLORFGBG`, else dark, as
/// the interface does before it asks the terminal.
pub(crate) fn apply_theme() {
    let light = match config::read().config.theme {
        Theme::Light => true,
        Theme::Dark => false,
        Theme::Auto => std::env::var("COLORFGBG")
            .ok()
            .and_then(|v| theme::colorfgbg_is_light(&v))
            .unwrap_or(false),
    };
    theme::set_light(light);
}

/// The pet as it is, or the first of the picker's own when none was picked yet.
pub(crate) fn the_pet() -> Avatar {
    avatar::load().unwrap_or(Avatar {
        body: avatar::BODY.to_string(),
        hair: "block".to_string(),
        eyes: "big".to_string(),
        hair_ink: 212,
        body_ink: avatar::BODY_INK,
    })
}

/// One part of the pet stepped by `by` (plus or minus one), wrapping.
pub(crate) fn step_part(pet: &mut Avatar, part: usize, by: isize) {
    let turn = |at: usize, len: usize| (at as isize + by).rem_euclid(len as isize) as usize;
    match part {
        0 => {
            let at = avatar::HAIR_STYLES
                .iter()
                .position(|h| *h == pet.hair)
                .unwrap_or(0);
            pet.hair = avatar::HAIR_STYLES[turn(at, avatar::HAIR_STYLES.len())].to_string();
        }
        1 => {
            let at = avatar::HAIR_INKS
                .iter()
                .position(|i| *i == pet.hair_ink)
                .unwrap_or(0);
            pet.hair_ink = avatar::HAIR_INKS[turn(at, avatar::HAIR_INKS.len())];
        }
        _ => {
            let at = avatar::EYE_STYLES
                .iter()
                .position(|e| *e == pet.eyes)
                .unwrap_or(0);
            pet.eyes = avatar::EYE_STYLES[turn(at, avatar::EYE_STYLES.len())].to_string();
        }
    }
}

/// A pet from the clock: each part its own pick.
pub(crate) fn random_pet(pet: &mut Avatar) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as usize);
    pet.hair = avatar::HAIR_STYLES[nanos % avatar::HAIR_STYLES.len()].to_string();
    pet.hair_ink = avatar::HAIR_INKS[(nanos / 7) % avatar::HAIR_INKS.len()];
    pet.eyes = avatar::EYE_STYLES[(nanos / 49) % avatar::EYE_STYLES.len()].to_string();
}

pub(crate) fn part_titles(pet: &Avatar) -> [(&'static str, String, Option<u8>); 3] {
    let title = |ids: &[&str], titles: &[&str], id: &str| {
        ids.iter()
            .position(|i| *i == id)
            .map_or(id.to_string(), |at| titles[at].to_string())
    };
    let ink_at = avatar::HAIR_INKS.iter().position(|i| *i == pet.hair_ink);
    [
        (
            "hair",
            title(&avatar::HAIR_STYLES, &avatar::HAIR_TITLES, &pet.hair),
            None,
        ),
        (
            "colour",
            ink_at.map_or(pet.hair_ink.to_string(), |at| {
                avatar::HAIR_INK_TITLES[at].to_string()
            }),
            Some(pet.hair_ink),
        ),
        (
            "eyes",
            title(&avatar::EYE_STYLES, &avatar::EYE_TITLES, &pet.eyes),
            None,
        ),
    ]
}

/// This machine's name for the page, or a plain word when it cannot be read.
fn host() -> String {
    let mut name = [0u8; 256];
    // SAFETY: `gethostname` writes at most `name.len()` bytes into the buffer owned here.
    let got = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) };
    let full = if got == 0 {
        String::from_utf8_lossy(&name).into_owned()
    } else {
        String::new()
    };
    short_host(&full).unwrap_or_else(|| "this machine".to_string())
}

/// A host name up to the first dot (`devbox.local` is `devbox`), `None` when there is none.
fn short_host(full: &str) -> Option<String> {
    let full = full.split('\0').next().unwrap_or_default();
    let short = full.split('.').next().unwrap_or(full);
    (!short.is_empty()).then(|| short.to_string())
}

/// Where the keyboard is.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    List,
    Pet(usize),
}

struct Page {
    lists: Vec<Row>,
    all: Vec<Row>,
    query: String,
    chip: usize,
    sel: usize,
    focus: Focus,
    pet: Avatar,
    said: Option<String>,
    /// The machine this page runs on, whose `config.toml` it edits.
    host: String,
    /// Whether the last frame had room for the pet column (`wide`); set before each draw.
    wide: bool,
}

/// Whether a `w` x `h` frame has room for the pet column beside the settings.
pub(crate) fn wide(w: usize, h: usize) -> bool {
    w >= NEED.0 && h >= NEED.1
}

/// `text` in lines of at most `width` characters (the unit `Screen` draws), broken at spaces, a
/// word longer than a line cut where the line ends.
fn fit_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        let used = line.chars().count();
        if used > 0 && used + 1 + word.len() <= width {
            line.push(' ');
            line.extend(word);
            continue;
        }
        if used > 0 {
            lines.push(std::mem::take(&mut line));
        }
        while word.len() > width {
            lines.push(word.drain(..width).collect());
        }
        line = word.into_iter().collect();
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `text` cut to `width` characters, ending in "…" when it was longer.
fn cut(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl Page {
    fn shown(&self) -> Vec<Row> {
        filtered(&self.all, &self.query, self.chip)
    }

    /// Fit the page to a `w` x `h` frame before drawing it: below `NEED` the pet column is not
    /// drawn, so a pet in focus hands it to the list.
    fn fit(&mut self, w: usize, h: usize) {
        self.wide = wide(w, h);
        if !self.wide && matches!(self.focus, Focus::Pet(_)) {
            self.focus = Focus::List;
        }
    }

    fn draw(&self, w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        if self.wide {
            self.draw_pet(&mut s, h);
        }
        self.draw_settings(&mut s, w, h);
        s
    }

    /// The pet column, the q over it, and the rule that ends it.
    fn draw_pet(&self, s: &mut Screen, h: usize) {
        s.word(3, 1);
        s.pet2(4, 5, &self.pet);
        s.put(4, 17, &self.pet.name(), &bold(Role::Focused));
        for (i, (part, value, hue)) in part_titles(&self.pet).iter().enumerate() {
            let y = 19 + i;
            let on = self.focus == Focus::Pet(i);
            if on {
                s.put(2, y, "▸", &ink(Role::Brand));
            }
            s.put(
                4,
                y,
                part,
                &if on {
                    bold(Role::Focused)
                } else {
                    ink(Role::Detail)
                },
            );
            let value_ink = hue.map_or_else(
                || bold(Role::Focused),
                |i| format!("{}\x1b[1m", page::raw(i)),
            );
            s.put(12, y, &format!("‹ {value} ›"), &value_ink);
        }
        s.put(4, 23, "ctrl-r a random pet", &ink(Role::Faint));
        for y in 1..h - 2 {
            s.put(DIV, y, "│", &ink(Role::Rule));
        }
    }

    /// The search and its chips, the machine, the rows, and the keys.
    fn draw_settings(&self, s: &mut Screen, w: usize, h: usize) {
        // The search line and its chips.
        let x0 = if self.wide { DIV + 3 } else { 2 };
        let rw = w - x0 - 2;
        let chips_w: usize = CHIPS.iter().map(|c| c.len() + 2).sum::<usize>() - 2;
        let mut cx = x0 + rw - chips_w;
        let mut x = s.put(x0, 1, "/ ", &ink(Role::Title));
        if self.query.is_empty() {
            x = s.put(x, 1, "▏", &ink(Role::Brand));
            s.put(x, 1, "search", &ink(Role::Faint));
        } else {
            // A search longer than the room before the chips shows its end.
            let room = cx.saturating_sub(x + 3);
            let skip = self.query.chars().count().saturating_sub(room);
            let tail: String = self.query.chars().skip(skip).collect();
            x = s.put(x, 1, &tail, &bold(Role::Focused));
            s.put(x, 1, "▏", &ink(Role::Brand));
        }
        for (i, chip) in CHIPS.iter().enumerate() {
            cx = s.put(
                cx,
                1,
                chip,
                &if i == self.chip {
                    bold(Role::Focused)
                } else {
                    ink(Role::Faint)
                },
            ) + 2;
        }
        s.put(x0, 2, &"─".repeat(rw), &ink(Role::Rule));
        // Which machine's file this edits: a tab opened on a remote space runs there.
        s.put(
            x0,
            3,
            &format!("settings on {}", self.host),
            &ink(Role::Faint),
        );
        self.draw_list(s, x0, 4, h - 3, rw);
        if let Some(said) = &self.said {
            s.put(x0, h - 2, said, &ink(Role::Faint));
        }
        s.status_bar(&[
            ("type", "search"),
            ("tab", "chip"),
            ("↑↓", "row"),
            ("←→", "change"),
            ("esc", "back"),
        ]);
    }

    /// The rows from `y` to `bottom`, grouped, the selected one opened and kept in view.
    fn draw_list(&self, s: &mut Screen, x: usize, y: usize, bottom: usize, w: usize) {
        let rows = self.shown();
        if rows.is_empty() {
            s.put(
                x,
                y,
                &format!("nothing matches \"{}\"", self.query),
                &ink(Role::Faint),
            );
            return;
        }
        let macos = cfg!(target_os = "macos");
        // Each line: a group name, a row, or a line of its picker.
        enum Line<'a> {
            Head(&'static str),
            Row(&'a Row, usize),
            /// A line of the selected row's help, when it is longer than its place beside the value.
            Help(String),
            Pick(Vec<(String, bool)>),
            Gap,
        }
        // The help's place beside the label (2 + 18) and the value (9) and the gap after them.
        let help_room = w.saturating_sub(31);
        let mut lines: Vec<Line> = Vec::new();
        let mut group = "";
        for (i, row) in rows.iter().enumerate() {
            if row.group != group {
                if !lines.is_empty() {
                    lines.push(Line::Gap);
                }
                group = row.group;
                lines.push(Line::Head(row.group));
            }
            lines.push(Line::Row(row, i));
            let selected = i == self.sel && self.focus == Focus::List;
            if selected && row.help.chars().count() > help_room {
                for help in fit_lines(&row.help, w.saturating_sub(4)) {
                    lines.push(Line::Help(help));
                }
            }
            if selected && row.kind != Kind::Info {
                // The picker wraps to the width, four columns in.
                let (list, at) = options(row, macos);
                let mut line = Vec::new();
                let mut used = 4;
                for (k, o) in list.iter().enumerate() {
                    let cell = o.len() + 2;
                    if used + cell > w && !line.is_empty() {
                        lines.push(Line::Pick(std::mem::take(&mut line)));
                        used = 4;
                    }
                    line.push((o.clone(), k == at));
                    used += cell + 1;
                }
                lines.push(Line::Pick(line));
            }
        }
        let at = lines
            .iter()
            .position(|l| matches!(l, Line::Row(_, i) if *i == self.sel))
            .unwrap_or(0);
        // The selected row and what opens under it (its help, its picker) stay in view; a block
        // taller than the room shows its top.
        let end = lines[at + 1..]
            .iter()
            .position(|l| !matches!(l, Line::Help(_) | Line::Pick(_)))
            .map_or(lines.len(), |k| at + 1 + k);
        let room = bottom - y;
        let start = if lines.len() <= room {
            0
        } else {
            let centred = at.saturating_sub(room / 2).min(lines.len() - room);
            centred.max(end.saturating_sub(room)).min(at)
        };
        for (k, line) in lines.iter().skip(start).take(room).enumerate() {
            let yy = y + k;
            match line {
                Line::Head(g) => {
                    s.put(x, yy, g, &bold(group_ink(g)));
                }
                Line::Row(row, i) => {
                    let on = *i == self.sel && self.focus == Focus::List;
                    if on {
                        s.put(x, yy, &" ".repeat(w), &raised(Role::Detail));
                    }
                    let paint = |role: Role, strong: bool| match (on, strong) {
                        (true, true) => raised_bold(role),
                        (true, false) => raised(role),
                        (false, true) => bold(role),
                        (false, false) => ink(role),
                    };
                    let value_role = if row.group == "machines" {
                        Role::Working
                    } else {
                        Role::Focused
                    };
                    let label: String = row.label.chars().take(18).collect();
                    // The selected row's long help is on the lines under it; any other is cut.
                    let help = if on && row.help.chars().count() > help_room {
                        String::new()
                    } else {
                        cut(&row.help, help_room)
                    };
                    s.seg(
                        x,
                        yy,
                        &[
                            (if on { "▸ " } else { "  " }, paint(Role::Brand, false)),
                            (&format!("{label:<18}"), paint(Role::Text, on)),
                            (&format!("{:<9}", row.value), paint(value_role, true)),
                            (&help, paint(Role::Faint, false)),
                        ],
                    );
                }
                Line::Help(text) => {
                    s.put(x + 4, yy, text, &ink(Role::Faint));
                }
                Line::Pick(items) => {
                    let mut xx = x + 4;
                    for (o, current) in items {
                        let esc = if *current {
                            raised_bold(Role::Focused)
                        } else {
                            ink(Role::Detail)
                        };
                        xx = s.put(xx, yy, &format!(" {o} "), &esc) + 1;
                    }
                }
                Line::Gap => {}
            }
        }
        if lines.len() > room {
            let count = format!("{} of {}", self.sel + 1, rows.len());
            s.put(x + w - count.len(), bottom, &count, &ink(Role::Faint));
        }
    }

    /// One key; `false` when the page should close.
    fn key(&mut self, key: Key, player: &mut Player) -> bool {
        let macos = cfg!(target_os = "macos");
        let count = self.shown().len();
        match (key, self.focus) {
            (Key::Stop, _) => return false,
            (Key::Esc, _) if self.query.is_empty() => return false,
            (Key::Esc, _) => {
                self.query.clear();
                self.sel = 0;
            }
            (Key::Char(c), _) => {
                self.query.push(c);
                self.sel = 0;
                self.focus = Focus::List;
            }
            (Key::Backspace, _) => {
                self.query.pop();
                self.sel = 0;
            }
            (Key::Tab, _) => {
                self.chip = (self.chip + 1) % CHIPS.len();
                self.sel = 0;
                self.focus = Focus::List;
            }
            (Key::BackTab, _) => {
                self.chip = (self.chip + CHIPS.len() - 1) % CHIPS.len();
                self.sel = 0;
                self.focus = Focus::List;
            }
            // The pet is changed only where it is drawn.
            (Key::CtrlR, _) if !self.wide => {}
            (Key::CtrlR, _) => {
                random_pet(&mut self.pet);
                self.save_pet();
            }
            (Key::Up, Focus::List) if self.sel == 0 && !self.wide => {}
            (Key::Up, Focus::List) if self.sel == 0 => self.focus = Focus::Pet(2),
            (Key::Up, Focus::List) => self.sel -= 1,
            (Key::Down, Focus::List) if self.sel + 1 < count => self.sel += 1,
            (Key::Up, Focus::Pet(p)) if p > 0 => self.focus = Focus::Pet(p - 1),
            (Key::Down, Focus::Pet(p)) if p < 2 => self.focus = Focus::Pet(p + 1),
            (Key::Down, Focus::Pet(_)) => self.focus = Focus::List,
            (Key::Left | Key::Right, Focus::Pet(p)) => {
                step_part(&mut self.pet, p, if key == Key::Left { -1 } else { 1 });
                self.save_pet();
            }
            (Key::Left | Key::Right | Key::Enter, Focus::List) => {
                let Some(row) = self.shown().get(self.sel).cloned() else {
                    return true;
                };
                let (list, at) = options(&row, macos);
                if list.is_empty() {
                    return true;
                }
                // Enter plays a sound row again, and steps the others.
                if key == Key::Enter
                    && let Kind::Sound(..) = row.kind
                {
                    player.play(&row.value, macos);
                    return true;
                }
                let next = if key == Key::Left {
                    (at + list.len() - 1) % list.len()
                } else {
                    (at + 1) % list.len()
                };
                let value = &list[next];
                self.said = write(&row, value).err();
                if let Kind::Sound(..) = row.kind {
                    player.play(value, macos);
                }
                if row.label == "theme" {
                    apply_theme();
                }
                self.all = rows(&self.lists);
            }
            _ => {}
        }
        true
    }

    fn save_pet(&mut self) {
        self.said = avatar::save(&self.pet)
            .err()
            .map(|e| format!("the pet was not saved: {e}"));
    }
}

/// `qoop settings`
pub fn run(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoop settings"));
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        // No one to look at a page: the settings as text, as `qoop config` prints them.
        return crate::config::run(&["qoop".to_string(), "config".to_string()]);
    }
    let mut lists = listed("machines", &["remote", "ls"], false);
    lists.extend(listed("phones", &["devices", "ls"], true));
    lists.extend(listed("hooks", &["hooks"], false));
    lists.extend(listed("skills", &["skills"], false));
    apply_theme();
    let mut page = Page {
        all: rows(&lists),
        lists,
        query: String::new(),
        chip: 0,
        sel: 0,
        focus: Focus::List,
        pet: the_pet(),
        said: None,
        host: host(),
        wide: true,
    };
    let session = Session::start()?;
    let mut player = Player::new();
    'run: loop {
        // Never drawn under MIN: the frame below it only says how big it must be.
        let (w, h) = page::size();
        let (w, h) = (w.max(MIN.0), h.max(MIN.1));
        page.fit(w, h);
        page::show(&page.draw(w, h), MIN)?;
        for key in page::next_keys(&session.typed) {
            if !page.key(key, &mut player) {
                break 'run;
            }
        }
    }
    drop(session);
    println!("settings are in {}", config::path().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(group: &'static str, label: &str, kind: Kind) -> Row {
        Row {
            group,
            label: label.to_string(),
            value: "on".to_string(),
            help: "some help".to_string(),
            kind,
        }
    }

    #[test]
    fn the_search_and_the_chip_narrow_the_rows() {
        let all = vec![
            row("sound", "sound", Kind::Toggle("notify.sound")),
            row("look", "theme", Kind::Info),
            row("keys", "prefix", Kind::Info),
            row("machines", "ubox", Kind::Info),
        ];
        let labels = |rows: Vec<Row>| rows.into_iter().map(|r| r.label).collect::<Vec<_>>();
        assert_eq!(labels(filtered(&all, "", 0)).len(), 4);
        assert_eq!(labels(filtered(&all, "UBOX", 0)), ["ubox"]);
        // The look chip holds the keys too.
        assert_eq!(labels(filtered(&all, "", 2)), ["theme", "prefix"]);
        assert_eq!(labels(filtered(&all, "zzz", 0)).len(), 0);
    }

    #[test]
    fn an_idle_value_written_by_hand_stays_a_choice() {
        let mut r = row("sleep", "idle tabs sleep", Kind::Idle);
        r.value = "20 min".to_string();
        let (list, at) = options(&r, true);
        assert_eq!(list, ["never", "15 min", "30 min", "60 min", "20 min"]);
        assert_eq!(at, 4);
    }

    #[test]
    fn a_pet_part_wraps_both_ways() {
        let mut pet = the_pet();
        pet.hair = avatar::HAIR_STYLES[0].to_string();
        step_part(&mut pet, 0, -1);
        assert_eq!(pet.hair, avatar::HAIR_STYLES[avatar::HAIR_STYLES.len() - 1]);
        step_part(&mut pet, 0, 1);
        assert_eq!(pet.hair, avatar::HAIR_STYLES[0]);
        assert!(pet.valid());
    }

    #[test]
    fn up_from_the_first_row_reaches_the_pet_and_down_comes_back() {
        let mut page = Page {
            lists: Vec::new(),
            all: vec![
                row("sound", "sound", Kind::Info),
                row("look", "theme", Kind::Info),
            ],
            query: String::new(),
            chip: 0,
            sel: 0,
            focus: Focus::List,
            pet: the_pet(),
            said: None,
            host: "devbox".to_string(),
            wide: true,
        };
        let mut player = Player::new();
        page.key(Key::Up, &mut player);
        assert!(page.focus == Focus::Pet(2));
        page.key(Key::Down, &mut player);
        assert!(page.focus == Focus::List);
        page.key(Key::Down, &mut player);
        assert_eq!(page.sel, 1);
        page.key(Key::Down, &mut player);
        assert_eq!(page.sel, 1);
        // Typing searches and goes back to the list.
        page.key(Key::Char('t'), &mut player);
        assert_eq!((page.query.as_str(), page.sel), ("t", 0));
        assert!(page.key(Key::Esc, &mut player));
        assert!(page.query.is_empty());
        assert!(!page.key(Key::Esc, &mut player));
    }

    #[test]
    fn the_whole_page_fits_the_window_it_needs() {
        let page = Page {
            lists: Vec::new(),
            all: rows(&[]),
            query: String::new(),
            chip: 0,
            sel: 1,
            focus: Focus::List,
            pet: the_pet(),
            said: None,
            host: "devbox".to_string(),
            wide: true,
        };
        let frame = page.draw(NEED.0, NEED.1).frame();
        assert!(frame.contains("search"), "{frame:?}");
        assert!(frame.contains("machines"), "{frame:?}");
        assert!(frame.contains("agent finished"), "{frame:?}");
        assert!(frame.contains("settings on devbox"), "{frame:?}");
    }

    /// A page of fixed rows (no config read), the second with help longer than any line.
    fn small_page(sel: usize) -> Page {
        let long = "this help is longer than any line of the page can hold, so it goes under the value when its row is selected and ends in a dot dot dot otherwise, with averyveryverylongwordthatmustbecut";
        let mut all = vec![
            row("sound", "sound", Kind::Toggle("notify.sound")),
            Row {
                help: long.to_string(),
                ..row("look", "theme", Kind::Info)
            },
        ];
        for i in 0..12 {
            all.push(row(
                "keys",
                &format!("key {i}"),
                Kind::Choice("prefix", (0..9).map(|n| format!("ctrl-{n}")).collect()),
            ));
        }
        Page {
            lists: Vec::new(),
            all,
            query: String::new(),
            chip: 0,
            sel,
            focus: Focus::List,
            pet: the_pet(),
            said: None,
            host: "devbox".to_string(),
            wide: true,
        }
    }

    /// The frame as plain text, one line per row.
    fn text(page: &mut Page, w: usize, h: usize) -> Vec<String> {
        page.fit(w, h);
        let frame = page.draw(w, h).frame();
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
                (true, 'm' | 'J' | 'K') => in_esc = false,
                (true, _) => {}
            }
        }
        out.lines().map(str::to_string).collect()
    }

    /// From 130 x 38 down to 64 x 16 every size draws the settings; the pet only where it fits,
    /// the selected row's long help whole under it, any other long help cut with "…".
    #[test]
    fn the_page_fits_every_size_from_the_smallest() {
        for (w, h) in [
            (130, 38),
            (100, 30),
            (100, 29),
            (99, 30),
            (91, 34),
            (70, 20),
            (64, 16),
        ] {
            let mut page = small_page(1);
            let lines = text(&mut page, w, h);
            let all = lines.join("\n");
            assert_eq!(
                all.contains("ctrl-r a random pet"),
                wide(w, h),
                "{w}x{h}: {all}"
            );
            assert!(all.contains("settings on devbox"), "{w}x{h}");
            assert!(all.contains("theme"), "{w}x{h}");
            for word in ["longer", "selected", "otherwise,", "averyvery", "mustbecut"] {
                assert!(all.contains(word), "{w}x{h}: {word} missing in {all}");
            }
            // Selected elsewhere, the same help is cut.
            let mut other = small_page(0);
            let cut = text(&mut other, w, h).join("\n");
            assert!(
                cut.contains("this help is") && cut.contains('…') && !cut.contains("mustbecut"),
                "{w}x{h}: {cut}"
            );
        }
        assert!(
            page::too_small(63, 16, MIN)
                && page::too_small(64, 15, MIN)
                && !page::too_small(64, 16, MIN)
        );
    }

    /// At the smallest size the selected row's help and picker stay in view under it: the last
    /// row's, and a middle row's whose wrapped help pushes the picker past a centred window.
    #[test]
    fn the_selected_row_and_its_picker_stay_in_view() {
        let mut page = small_page(13);
        let all = text(&mut page, 64, 16).join("\n");
        assert!(all.contains("key 11") && all.contains("ctrl-8"), "{all}");
        let mut page = small_page(5);
        page.all[5].help = "a long help for a row in the middle of the list, long enough to take four lines under it at this width, so the row, its help and its two picker lines are taller than half the room and a centred window would cut the picker off".to_string();
        let all = text(&mut page, 64, 16).join("\n");
        assert!(all.contains("key 3") && all.contains("ctrl-8"), "{all}");
    }

    /// Narrowed, the pet's focus goes to the list and the pet keys do nothing; up stays put.
    #[test]
    fn a_narrow_page_keeps_the_pet_out_of_reach() {
        let mut page = small_page(0);
        page.focus = Focus::Pet(1);
        page.fit(70, 20);
        assert!(page.focus == Focus::List);
        let mut player = Player::new();
        let before = page.pet.name();
        page.key(Key::Up, &mut player);
        assert!(page.focus == Focus::List && page.sel == 0);
        page.key(Key::CtrlR, &mut player);
        assert_eq!(page.pet.name(), before);
        page.fit(130, 38);
        page.key(Key::Up, &mut player);
        assert!(page.focus == Focus::Pet(2));
    }

    /// A search longer than its room shows its end and never writes over a chip.
    #[test]
    fn a_long_search_leaves_every_chip_whole() {
        for (w, h) in [(100, 30), (64, 16)] {
            let mut page = small_page(0);
            page.query = "a search typed far longer than the room before the chips".to_string();
            let line = text(&mut page, w, h)
                .into_iter()
                .find(|l| l.contains("/ "))
                .unwrap_or_default();
            for chip in CHIPS {
                assert!(line.contains(chip), "{w}x{h}: {chip} lost in {line:?}");
            }
            assert!(line.contains("chips"), "{w}x{h}: {line:?}");
        }
    }

    #[test]
    fn long_text_is_fitted_to_its_lines() {
        assert_eq!(fit_lines("one two three", 7), vec!["one two", "three"]);
        assert_eq!(fit_lines("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }

    #[test]
    fn the_host_name_is_cut_at_the_first_dot() {
        assert_eq!(short_host("devbox.local\0\0").as_deref(), Some("devbox"));
        assert_eq!(short_host("onyx").as_deref(), Some("onyx"));
        assert_eq!(short_host(""), None);
        assert_eq!(short_host("\0\0"), None);
    }
}

//! `qoop qass page`: how qass stands, and what to do about it, on one page.
//!
//! The settings page's frame: the pet on the left (asleep, it squints), a rule, and the
//! rows on the right. Under "status" is what is known of qass right now (running, asleep or
//! not started, the agent it runs on, what it is doing, when it last did anything), under
//! "actions" what can be done about it (start or wake, sleep now, open its tab), and under
//! "sleep mode" how long it may idle before it sleeps by itself. Up and down move between
//! the rows that do something, left and right change the sleep mode and write it at once,
//! enter does the row, `w`, `s` and `o` are wake, sleep and open, `q` or esc leaves.

use std::io::IsTerminal;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use qoop_runtime::avatar::Avatar;
use qoop_tui::config;
use qoop_tui::theme::Role;

use crate::page::{self, Key, Screen, Session, bold, ink, raised, raised_bold};
use crate::qass::{self, Facts, Life};
use crate::settings::{NEED, apply_theme, the_pet};

/// Where the pet column ends and the rows begin.
const DIV: usize = 40;
/// The minutes the sleep mode steps through; `0` is never.
const IDLE: [u64; 4] = [0, 15, 30, 60];
/// What the sleep mode does once changed.
const SLEEP_NOTE: &str = "The running daemon takes a change up at its next idle check, no restart. From never, that check starts at the next agent activity. qass does not wake by itself: wake it here or with `qoop qass`.";

/// A look at qass again after this many quiet 300 ms waits.
const REFRESH: usize = 10;

/// What a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Start,
    Wake,
    Sleep,
    Open,
    Idle,
}

/// What the page asks of its caller: it never runs a command itself.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Stay,
    Leave,
    /// Run `qoop` with these arguments, then look at qass again; leave after when `leave`.
    Run {
        args: Vec<String>,
        leave: bool,
    },
    /// Write `sleep.qass_idle_minutes`.
    SetIdle(u64),
}

struct Page {
    facts: Facts,
    /// The sleep mode now: minutes, `0` is never.
    idle: u64,
    /// Whether the file names no number of its own, so it follows `sleep.idle_minutes`.
    follows: bool,
    sel: usize,
    pet: Avatar,
    said: Option<String>,
    now: i64,
}

/// `text` in lines of at most `width` characters, broken at spaces.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

/// "3 min ago", from a unix time.
pub(crate) fn ago(then: Option<i64>, now: i64) -> String {
    let Some(then) = then else {
        return "never".to_string();
    };
    match (now - then).max(0) {
        0..=59 => "just now".to_string(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86_399 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86_400),
    }
}

fn minutes_said(minutes: u64) -> String {
    if minutes == 0 {
        "never".to_string()
    } else {
        format!("{minutes} min")
    }
}

/// The sleep mode as the file says it now: minutes, and whether it is the general number.
fn idle_now() -> (u64, bool) {
    let life = config::read().config.lifecycle;
    (
        life.qass_idle_minutes.unwrap_or(life.idle_minutes),
        life.qass_idle_minutes.is_none(),
    )
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

impl Page {
    fn new(facts: Facts) -> Page {
        let (idle, follows) = idle_now();
        Page {
            facts,
            idle,
            follows,
            sel: 0,
            pet: the_pet(),
            said: None,
            now: now_unix(),
        }
    }

    /// The rows that do something, in the order they are drawn.
    fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        match self.facts.life {
            Life::Running => out.extend([Item::Sleep, Item::Open]),
            Life::Asleep => out.push(Item::Wake),
            Life::NotStarted if self.facts.blocker.is_none() => out.push(Item::Start),
            Life::NotStarted => {}
        }
        out.push(Item::Idle);
        out
    }

    /// The pet as qass wears it: asleep, its eyes close to a squint.
    fn pet_now(&self) -> Avatar {
        let mut pet = self.pet.clone();
        if self.facts.life == Life::Asleep {
            pet.eyes = "squint".to_string();
        }
        pet
    }

    /// The minutes the sleep mode steps to from here.
    fn idle_step(&self, by: isize) -> u64 {
        let mut list = IDLE.to_vec();
        if !list.contains(&self.idle) {
            list.push(self.idle);
        }
        let at = list.iter().position(|m| *m == self.idle).unwrap_or(0);
        list[(at as isize + by).rem_euclid(list.len() as isize) as usize]
    }

    /// What doing `item` asks for, or why it cannot be done.
    fn act(&self, item: Item) -> Step {
        let tab = self.facts.terminal.map(|t| t.to_string());
        let run = |args: &[&str], tab: Option<&String>, leave| Step::Run {
            args: args
                .iter()
                .map(|a| a.to_string())
                .chain(tab.cloned())
                .collect(),
            leave,
        };
        match (item, self.facts.life) {
            (Item::Start, Life::NotStarted) => run(&["qass"], None, true),
            (Item::Wake, Life::Asleep) => run(&["wake", "--tab"], tab.as_ref(), false),
            (Item::Sleep, Life::Running) => run(&["sleep", "--tab"], tab.as_ref(), false),
            (Item::Open, Life::Running) => run(&["focus"], tab.as_ref(), true),
            (Item::Idle, _) => Step::SetIdle(self.idle_step(1)),
            _ => Step::Stay,
        }
    }

    /// One key.
    fn key(&mut self, key: Key) -> Step {
        let items = self.items();
        self.sel = self.sel.min(items.len() - 1);
        match key {
            Key::Stop | Key::Esc | Key::Char('q') => Step::Leave,
            Key::Up => {
                self.sel = self.sel.saturating_sub(1);
                Step::Stay
            }
            Key::Down => {
                self.sel = (self.sel + 1).min(items.len() - 1);
                Step::Stay
            }
            Key::Enter => self.act(items[self.sel]),
            Key::Left | Key::Right if items[self.sel] == Item::Idle => {
                Step::SetIdle(self.idle_step(if key == Key::Left { -1 } else { 1 }))
            }
            Key::Char('w') => {
                let item = if self.facts.life == Life::NotStarted {
                    Item::Start
                } else {
                    Item::Wake
                };
                self.hot(item)
            }
            Key::Char('s') => self.hot(Item::Sleep),
            Key::Char('o') => self.hot(Item::Open),
            _ => Step::Stay,
        }
    }

    /// A letter that does `item` when the page offers it.
    fn hot(&self, item: Item) -> Step {
        if self.items().contains(&item) {
            self.act(item)
        } else {
            Step::Stay
        }
    }

    /// How qass is said to stand, and in what ink.
    fn said(&self) -> (&'static str, Role) {
        match self.facts.life {
            Life::Running => ("running", Role::Working),
            Life::Asleep => ("asleep", Role::Sleeping),
            Life::NotStarted if self.facts.blocker.is_some() => ("not set up", Role::Blocked),
            Life::NotStarted => ("not started", Role::FaintText),
        }
    }

    /// The status rows: label, value, help, and the role the value is painted in.
    fn status(&self) -> Vec<(&'static str, String, String, Role)> {
        let f = &self.facts;
        let (said, role) = self.said();
        let help = match f.life {
            Life::Running => f
                .state
                .as_deref()
                .map_or(String::new(), |s| format!("its agent is {s}")),
            Life::Asleep => "nothing of it runs; wake brings its conversation back".to_string(),
            Life::NotStarted if f.blocker.is_some() => "it cannot start, see actions".to_string(),
            Life::NotStarted => "start it here or with `qoop qass`".to_string(),
        };
        let mut rows = vec![
            ("state", said.to_string(), help, role),
            (
                "runs on",
                f.agent.clone(),
                "change it in settings".to_string(),
                Role::Focused,
            ),
        ];
        if let Some(terminal) = f.terminal {
            rows.push((
                "tab",
                format!("T{terminal}"),
                "in the space named qass".to_string(),
                Role::Focused,
            ));
        }
        if f.life == Life::Running {
            rows.push((
                "doing",
                f.activity.clone().unwrap_or_else(|| "-".to_string()),
                "what its agent last reported".to_string(),
                Role::Focused,
            ));
            rows.push((
                "last active",
                ago(f.active_at, self.now),
                String::new(),
                Role::Focused,
            ));
        }
        rows.push((
            "last event",
            ago(f.event_at, self.now),
            "an agent it watches changed".to_string(),
            Role::Focused,
        ));
        rows
    }

    fn draw(&self, w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        // The pet column.
        s.word(3, 1);
        let pet = self.pet_now();
        s.pet2(4, 5, &pet);
        s.put(4, 17, &pet.name(), &bold(Role::Focused));
        let (said, role) = self.said();
        s.put(4, 19, "qass", &bold(Role::Title));
        s.put(9, 19, said, &bold(role));
        if self.facts.life == Life::Asleep {
            s.put(37, 8, "z", &ink(Role::Sleeping));
            s.put(38, 6, "Z", &bold(Role::Sleeping));
        }
        for y in 1..h - 2 {
            s.put(DIV, y, "│", &ink(Role::Rule));
        }
        // The rows.
        let x0 = DIV + 3;
        let rw = w - x0 - 2;
        s.put(x0, 1, "qass", &bold(Role::Title));
        s.put(
            x0 + 5,
            1,
            "the assistant over your agents",
            &ink(Role::FaintText),
        );
        s.put(x0, 2, &"─".repeat(rw), &ink(Role::Rule));
        let items = self.items();
        let sel = items.get(self.sel).copied();
        let mut y = 4;
        s.put(x0, y, "status", &bold(Role::Working));
        for (label, value, help, role) in self.status() {
            y += 1;
            row(&mut s, x0, y, rw, false, (label, &value, &help), role);
        }
        y += 2;
        s.put(x0, y, "actions", &bold(Role::Blocked));
        let actions: Vec<Item> = items.iter().copied().filter(|i| *i != Item::Idle).collect();
        if actions.is_empty() {
            // Why it cannot start is the one thing worth the room, so it wraps rather than cuts.
            let why = self.facts.blocker.as_deref().unwrap_or("nothing to do");
            for line in wrap(why, rw - 2) {
                y += 1;
                s.put(x0 + 2, y, &line, &ink(Role::Blocked));
            }
        }
        for item in actions {
            y += 1;
            let (label, key, help) = match item {
                Item::Start => ("start", "w", "starts qass in a tab, as `qoop qass` does"),
                Item::Wake => ("wake", "w", "brings its tab and conversation back"),
                Item::Sleep => (
                    "sleep now",
                    "s",
                    "ends its processes, keeps its conversation",
                ),
                Item::Open => ("open tab", "o", "shows it, and leaves this page"),
                Item::Idle => continue,
            };
            row(
                &mut s,
                x0,
                y,
                rw,
                sel == Some(item),
                (label, key, help),
                Role::Focused,
            );
        }
        y += 2;
        s.put(x0, y, "sleep mode", &bold(Role::Sleeping));
        y += 1;
        let help = if self.follows {
            "same as idle tabs"
        } else {
            "set for qass alone"
        };
        row(
            &mut s,
            x0,
            y,
            rw,
            sel == Some(Item::Idle),
            ("sleeps when idle", &minutes_said(self.idle), help),
            Role::Focused,
        );
        if sel == Some(Item::Idle) {
            y += 1;
            let mut list = IDLE.to_vec();
            if !list.contains(&self.idle) {
                list.push(self.idle);
            }
            let mut x = x0 + 4;
            for m in list {
                let esc = if m == self.idle {
                    raised_bold(Role::Focused)
                } else {
                    ink(Role::Detail)
                };
                x = s.put(x, y, &format!(" {} ", minutes_said(m)), &esc) + 1;
            }
        }
        // What a change does, always said: it is written at once, the daemon takes it up on
        // its own, and qass never wakes by itself.
        y += 2;
        for line in wrap(SLEEP_NOTE, rw - 2) {
            s.put(x0 + 2, y, &line, &ink(Role::FaintText));
            y += 1;
        }
        if let Some(said) = &self.said {
            s.put(x0, h - 2, said, &ink(Role::FaintText));
        }
        s.status_bar(&[
            ("↑↓", "row"),
            ("enter", "do"),
            ("←→", "sleep mode"),
            ("w s o", "wake sleep open"),
            ("q", "close"),
        ]);
        s
    }

    /// Look at qass again, keeping the row the keyboard is on by what it does: the rows
    /// change with qass's state, and an index would slide onto another row.
    fn refresh(&mut self) {
        let on = self.items().get(self.sel).copied();
        self.facts = qass::look();
        let (idle, follows) = idle_now();
        (self.idle, self.follows) = (idle, follows);
        self.now = now_unix();
        self.keep(on);
    }

    /// Put the keyboard back on `on` if the page still has it, else on the first row.
    fn keep(&mut self, on: Option<Item>) {
        self.sel = on
            .and_then(|item| self.items().iter().position(|i| *i == item))
            .unwrap_or(0);
    }
}

/// One row as the settings page draws it: a marker, the label, the value, the help.
fn row(
    s: &mut Screen,
    x: usize,
    y: usize,
    w: usize,
    on: bool,
    (label, value, help): (&str, &str, &str),
    value_role: Role,
) {
    if on {
        s.put(x, y, &" ".repeat(w), &raised(Role::Detail));
    }
    let paint = |role: Role, strong: bool| match (on, strong) {
        (true, true) => raised_bold(role),
        (true, false) => raised(role),
        (false, true) => bold(role),
        (false, false) => ink(role),
    };
    // The value gets what it needs (a tool label is longer than a word), up to a third of the row.
    let vw = (value.chars().count() + 1).clamp(12, w / 3);
    let value: String = value.chars().take(vw - 1).collect();
    let help: String = help.chars().take(w.saturating_sub(22 + vw)).collect();
    s.seg(
        x,
        y,
        &[
            (if on { "▸ " } else { "  " }, paint(Role::Brand, false)),
            (&format!("{label:<18}"), paint(Role::Text, on)),
            (&format!("{value:<vw$}"), paint(value_role, true)),
            (&help, paint(Role::FaintText, false)),
        ],
    );
}

/// A `qoop` command from this same binary, and what it said or why it failed.
fn ask(args: &[String]) -> Result<String, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(me)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let said = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim().to_string();
    if out.status.success() {
        return Ok(said(&out.stdout));
    }
    let err = said(&out.stderr);
    Err(if err.is_empty() {
        said(&out.stdout)
    } else {
        err
    })
}

/// The first line of what a command said, for the one row the page keeps for it.
fn one_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    line.strip_prefix("qoop: ").unwrap_or(line).to_string()
}

/// `qoop qass page`
pub fn run() -> std::io::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return Err(std::io::Error::other("the qass page needs a terminal"));
    }
    apply_theme();
    let mut page = Page::new(qass::look());
    let session = Session::start()?;
    let mut quiet = 0;
    'run: loop {
        let (w, h) = page::size();
        page::show(&page.draw(w.max(NEED.0), h.max(NEED.1)), NEED, "esc")?;
        let keys = page::next_keys(&session.typed);
        quiet = if keys.is_empty() { quiet + 1 } else { 0 };
        if quiet >= REFRESH {
            quiet = 0;
            page.refresh();
        }
        for key in keys {
            match page.key(key) {
                Step::Stay => {}
                Step::Leave => break 'run,
                Step::Run { args, leave } => {
                    match ask(&args) {
                        Ok(_) if leave => break 'run,
                        Ok(said) => page.said = Some(one_line(&said)).filter(|s| !s.is_empty()),
                        Err(why) => page.said = Some(one_line(&why)),
                    }
                    page.refresh();
                }
                Step::SetIdle(minutes) => {
                    page.said = config::set(
                        &config::path(),
                        "sleep.qass_idle_minutes",
                        &minutes.to_string(),
                    )
                    .err();
                    page.refresh();
                }
            }
        }
    }
    drop(session);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::first_pet;

    fn facts(life: Life) -> Facts {
        Facts {
            life,
            terminal: (life != Life::NotStarted).then_some(7),
            agent: "claude".to_string(),
            state: (life == Life::Running).then(|| "working".to_string()),
            activity: (life == Life::Running).then(|| "Bash cargo test".to_string()),
            active_at: Some(1_000),
            event_at: Some(700),
            blocker: None,
        }
    }

    fn page(facts: Facts) -> Page {
        Page {
            facts,
            idle: 30,
            follows: false,
            sel: 0,
            pet: first_pet(),
            said: None,
            now: 1_000 + 120,
        }
    }

    fn drawn(page: &Page) -> String {
        let frame = page.draw(NEED.0, NEED.1).frame();
        // Escape sequences out, so the text can be searched.
        let mut text = String::new();
        let mut in_esc = false;
        for ch in frame.chars() {
            match (in_esc, ch) {
                (false, '\x1b') => in_esc = true,
                (false, ch) => text.push(ch),
                (true, 'm' | 'H') => in_esc = false,
                (true, _) => {}
            }
        }
        text
    }

    #[test]
    fn the_keyboard_stays_on_its_row_when_the_rows_change() {
        let mut p = page(facts(Life::Running));
        p.sel = 2;
        p.facts = facts(Life::Asleep);
        p.keep(Some(Item::Idle));
        assert_eq!(p.items()[p.sel], Item::Idle);
        // A row that is gone sends it to the first one, never onto the sleep mode.
        p.facts = facts(Life::Running);
        p.keep(Some(Item::Wake));
        assert_eq!(p.items()[p.sel], Item::Sleep);
    }

    #[test]
    fn the_sleep_mode_always_says_when_a_change_applies() {
        for life in [Life::Running, Life::Asleep, Life::NotStarted] {
            let mut p = page(facts(life));
            let text = drawn(&p);
            // Every wrapped line of the note is on the page, at the smallest window too.
            let width = NEED.0 - (DIV + 3) - 2 - 2;
            for line in wrap(SLEEP_NOTE, width) {
                assert!(text.contains(&line), "{line}: {text}");
            }
            p.follows = true;
            assert!(drawn(&p).contains("same as idle tabs"));
        }
    }

    #[test]
    fn a_long_reason_wraps_at_spaces() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("", 5), Vec::<String>::new());
        assert_eq!(wrap("toolongword x", 4), ["toolongword", "x"]);
    }

    #[test]
    fn a_refusal_is_one_line_without_the_programs_name() {
        assert_eq!(
            one_line("qoop: tab 17 stays awake: claude is working\nmore"),
            "tab 17 stays awake: claude is working"
        );
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn time_is_said_the_way_a_person_says_it() {
        assert_eq!(ago(None, 100), "never");
        assert_eq!(ago(Some(100), 130), "just now");
        assert_eq!(ago(Some(0), 600), "10 min ago");
        assert_eq!(ago(Some(0), 7_300), "2 h ago");
        assert_eq!(ago(Some(0), 200_000), "2 d ago");
    }

    #[test]
    fn a_running_qass_shows_what_it_does_and_offers_sleep_and_open() {
        let p = page(facts(Life::Running));
        let text = drawn(&p);
        for want in [
            "running",
            "its agent is working",
            "Bash cargo test",
            "2 min ago",
            "sleep now",
            "open tab",
            "sleeps when idle",
            "30 min",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
        assert!(!text.contains("wake qass"), "{text}");
        assert_eq!(p.items(), [Item::Sleep, Item::Open, Item::Idle]);
    }

    #[test]
    fn an_asleep_qass_offers_wake_and_the_pet_squints() {
        let p = page(facts(Life::Asleep));
        let text = drawn(&p);
        assert!(text.contains("asleep"), "{text}");
        assert!(text.contains("wake"), "{text}");
        assert!(!text.contains("sleep now"), "{text}");
        assert_eq!(p.items(), [Item::Wake, Item::Idle]);
        assert_eq!(p.pet_now().eyes, "squint");
        assert_ne!(page(facts(Life::Running)).pet_now().eyes, "squint");
    }

    #[test]
    fn a_qass_that_was_never_started_starts_only_when_it_can() {
        let p = page(facts(Life::NotStarted));
        assert!(drawn(&p).contains("not started"));
        assert_eq!(p.items(), [Item::Start, Item::Idle]);
        let mut blocked = facts(Life::NotStarted);
        blocked.blocker =
            Some("qass is not installed for claude; run `qoop skills install claude`".to_string());
        let p = page(blocked);
        let text = drawn(&p);
        assert!(text.contains("not set up"), "{text}");
        // At the smallest window the reason wraps over two lines and keeps its end.
        assert!(text.contains("qass is not installed for claude;"), "{text}");
        assert!(text.contains("install claude`"), "{text}");
        assert!(!text.contains("not started"), "{text}");
        assert_eq!(p.items(), [Item::Idle]);
    }

    #[test]
    fn keys_ask_for_the_command_and_leave_the_rest_to_the_caller() {
        let mut p = page(facts(Life::Running));
        // The first row is "sleep now".
        assert_eq!(
            p.key(Key::Enter),
            Step::Run {
                args: vec!["sleep".into(), "--tab".into(), "7".into()],
                leave: false
            }
        );
        assert_eq!(
            p.key(Key::Char('o')),
            Step::Run {
                args: vec!["focus".into(), "7".into()],
                leave: true
            }
        );
        // `w` is not a thing to do while it runs.
        assert_eq!(p.key(Key::Char('w')), Step::Stay);
        let mut p = page(facts(Life::Asleep));
        assert_eq!(
            p.key(Key::Char('w')),
            Step::Run {
                args: vec!["wake".into(), "--tab".into(), "7".into()],
                leave: false
            }
        );
        assert_eq!(p.key(Key::Char('s')), Step::Stay);
        let mut p = page(facts(Life::NotStarted));
        assert_eq!(
            p.key(Key::Char('w')),
            Step::Run {
                args: vec!["qass".into()],
                leave: true
            }
        );
        for key in [Key::Esc, Key::Char('q'), Key::Stop] {
            assert_eq!(p.key(key), Step::Leave);
        }
    }

    #[test]
    fn the_sleep_mode_steps_both_ways_and_keeps_a_value_written_by_hand() {
        let mut p = page(facts(Life::Running));
        // Down to the sleep mode row.
        p.key(Key::Down);
        p.key(Key::Down);
        assert_eq!(p.key(Key::Right), Step::SetIdle(60));
        assert_eq!(p.key(Key::Left), Step::SetIdle(15));
        assert_eq!(p.key(Key::Enter), Step::SetIdle(60));
        // Left and right do nothing on a row that is not the sleep mode.
        p.key(Key::Up);
        assert_eq!(p.key(Key::Right), Step::Stay);
        p.idle = 20;
        assert_eq!(p.idle_step(1), 0);
        p.idle = 60;
        assert_eq!(p.idle_step(1), 0);
        p.idle = 0;
        assert_eq!(p.idle_step(-1), 60);
    }

    #[test]
    fn the_page_fits_the_smallest_window_in_every_state() {
        for life in [Life::Running, Life::Asleep, Life::NotStarted] {
            let mut p = page(facts(life));
            p.sel = 1;
            let text = drawn(&p);
            assert!(text.contains("sleep mode"), "{text}");
            assert!(text.contains("close"), "{text}");
        }
    }
}

//! `qoop onboard`: the first start, one group of settings a screen.
//!
//! Eight steps under the word: the pet, the sounds, light or dark, the command key, the
//! notifications, sleep, qass, and whether qass may send notes about itself to qoop's maker. Their names sit under a line of dots that turns green as steps
//! pass; the current step says what it holds in one sentence and shows its choices. Left
//! and right choose (a sound plays), up and down move between a step's rows, enter goes on,
//! esc finishes. Each choice is written as it is made, so leaving early keeps what was set.
//! The rows are the settings page's own (`settings.rs`), so the two never disagree.

use std::io::IsTerminal;

use qoop_runtime::avatar::{self, Avatar};
use qoop_tui::config;
use qoop_tui::theme::Role;

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
        also: &[],
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
            "We recommend always: it sends by itself after your first yes.",
            "ask: asks each time. never: sends nothing.",
            "Sent with your account so it can be limited; the note carries no account.",
        ],
    },
];

struct Onboard {
    step: usize,
    row: usize,
    pet: Avatar,
    said: Option<String>,
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
                std::cmp::Ordering::Greater => ("○", ink(Role::Faint)),
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
                std::cmp::Ordering::Greater => ink(Role::Faint),
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
                s.centre(cx, y, line, &ink(Role::Faint));
            }
        }
        if let Some(said) = &self.said {
            s.centre(cx, h - 2, said, &ink(Role::Faint));
        }
        let last = self.step + 1 == STEPS.len();
        let pet_step = step.groups.is_empty();
        s.centre_seg(
            cx,
            h - 3,
            &[
                ("←→", bold(Role::Focused)),
                (" choose   ", ink(Role::Faint)),
                (if pet_step { "ctrl-r" } else { "" }, bold(Role::Focused)),
                (
                    if pet_step { " random pet   " } else { "" },
                    ink(Role::Faint),
                ),
                ("↑↓", bold(Role::Focused)),
                (" row   ", ink(Role::Faint)),
                ("enter", bold(Role::Focused)),
                (
                    if last { " done   " } else { " next step   " },
                    ink(Role::Faint),
                ),
                ("esc", bold(Role::Focused)),
                (" finish", ink(Role::Faint)),
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
                + usize::from(matches!(row.kind, Kind::Idle))
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
                            ink(Role::Faint)
                        };
                        (format!(" {o}  "), esc)
                    })
                    .collect();
                let parts: Vec<(&str, String)> =
                    parts.iter().map(|(t, e)| (t.as_str(), e.clone())).collect();
                s.centre_seg(cx, y, &parts);
                y += 1;
            }
            if let Kind::Idle = row.kind {
                s.centre(cx, y, &row.help, &ink(Role::Faint));
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
            Key::Esc | Key::Stop | Key::Char('q') => return false,
            Key::Enter | Key::Tab => {
                if self.step + 1 == STEPS.len() {
                    return false;
                }
                self.step += 1;
                self.row = 0;
                self.said = None;
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

/// `qoop onboard`
pub fn run(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(std::io::Error::other("usage: qoop onboard"));
    }
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        println!(
            "qoop onboard needs a terminal to show its steps; qoop settings changes anything later"
        );
        return Ok(());
    }
    settings::apply_theme();
    let mut onboard = Onboard {
        step: 0,
        row: 0,
        pet: settings::the_pet(),
        said: None,
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
    println!("{}", last_line(config::read().config.prefix));
    Ok(())
}

/// What onboarding ends on: the next thing to do, with the command key just chosen.
fn last_line(prefix: u8) -> String {
    let key = format!("ctrl-{}", char::from(b'a' + prefix - 1));
    format!(
        "qoop is set up. Next: start qoop, then {key} Q opens qass, which sets up your projects \
         with you (settings: qoop settings)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_ends_on_qass_with_the_chosen_key() {
        assert!(last_line(2).contains("ctrl-b Q opens qass"));
        assert!(last_line(1).contains("ctrl-a Q"));
    }

    #[test]
    fn enter_walks_the_steps_and_ends_after_the_last() {
        let mut o = Onboard {
            step: 0,
            row: 2,
            pet: settings::the_pet(),
            said: None,
        };
        let mut player = Player::new();
        for step in 1..STEPS.len() {
            assert!(o.key(Key::Enter, &mut player));
            assert_eq!((o.step, o.row), (step, 0));
        }
        assert!(!o.key(Key::Enter, &mut player));
        o.key(Key::BackTab, &mut player);
        assert_eq!(o.step, STEPS.len() - 2);
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
                    pet: settings::the_pet(),
                    said: None,
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
                        pet: settings::the_pet(),
                        said: None,
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

//! `qoop onboard`: the first start, one group of settings a screen.
//!
//! Six steps under the word: the pet, the sounds, light or dark, the command key, the
//! notifications, sleep. Their names sit under a line of dots that turns green as steps
//! pass; the current step says what it holds in one sentence and shows its choices. Left
//! and right choose (a sound plays), up and down move between a step's rows, enter goes on,
//! esc finishes. Each choice is written as it is made, so leaving early keeps what was set.
//! The rows are the settings page's own (`settings.rs`), so the two never disagree.

use std::io::IsTerminal;

use qoop_runtime::avatar::{self, Avatar};
use qoop_tui::theme::Role;

use crate::page::{self, Key, Player, Screen, Session, bold, ink, raised_bold};
use crate::settings::{self, Kind, Row};

const NEED: (usize, usize) = (100, 30);

struct Step {
    name: &'static str,
    title: &'static str,
    what: &'static str,
    /// The settings groups the step holds; none for the pet.
    groups: &'static [&'static str],
}

const STEPS: [Step; 7] = [
    Step {
        name: "pet",
        title: "your pet",
        what: "the pet that sits beside the q in the sidebar",
        groups: &[],
    },
    Step {
        name: "sound",
        title: "sounds",
        what: "what plays when an agent finishes, then when one waits on you",
        groups: &["sound"],
    },
    Step {
        name: "look",
        title: "light or dark",
        what: "follow the terminal, or always dark, or always light",
        groups: &["look"],
    },
    Step {
        name: "keys",
        title: "the command key",
        what: "the key that starts every qoop command",
        groups: &["keys"],
    },
    Step {
        name: "notifications",
        title: "notifications",
        what: "how qoop tells you when you are not looking",
        groups: &["notifications"],
    },
    Step {
        name: "sleep",
        title: "sleep",
        what: "when idle tabs sleep to give back memory",
        groups: &["sleep"],
    },
    Step {
        name: "qass",
        title: "qass",
        what: "the assistant over your agents: the agent `qoop qass` opens it in",
        groups: &["qass"],
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

    fn draw(&self, w: usize, h: usize) -> Screen {
        let mut s = Screen::new(w, h);
        let cx = w / 2;
        let step = &STEPS[self.step];
        s.word(cx - 12, 1);
        if !step.groups.is_empty() {
            s.pet(cx + 20, 0, &self.pet);
        }
        // The named dots, joined by a line that turns green behind the current step.
        let gap = 15;
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
        s.centre(cx, 10, step.title, &bold(Role::Focused));
        s.centre(cx, 11, step.what, &ink(Role::Detail));
        if step.groups.is_empty() {
            self.draw_pet(&mut s, cx);
        } else {
            self.draw_rows(&mut s, cx, w);
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

    fn draw_pet(&self, s: &mut Screen, cx: usize) {
        // The grid's first row is empty for every pet: start one up.
        s.pet2(cx - 16, 12, &self.pet);
        for (i, (part, value, hue)) in settings::part_titles(&self.pet).iter().enumerate() {
            let y = 24 + i;
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
    /// wrapped and centred, the current one raised. One open at a time keeps a long list
    /// (Linux has 26 sounds) inside a 30 row window.
    fn draw_rows(&self, s: &mut Screen, cx: usize, w: usize) {
        let macos = cfg!(target_os = "macos");
        let mut y = 13;
        for (i, row) in self.rows().iter().enumerate() {
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
            let (list, at) = settings::options(row, macos);
            let mut lines: Vec<Vec<(String, bool)>> = vec![Vec::new()];
            let mut used = 0;
            for (k, o) in list.iter().enumerate() {
                let cell = o.len() + 3;
                if used + cell > w - 20 {
                    lines.push(Vec::new());
                    used = 0;
                }
                if let Some(l) = lines.last_mut() {
                    l.push((o.clone(), k == at));
                }
                used += cell;
            }
            for line in lines {
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
        page::show(&onboard.draw(w.max(NEED.0), h.max(NEED.1)), NEED)?;
        for key in page::next_keys(&session.typed) {
            if !onboard.key(key, &mut player) {
                break 'run;
            }
        }
    }
    drop(session);
    println!("qoop is set up; change anything later with qoop settings");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn every_step_fits_the_smallest_window() {
        for (step, each) in STEPS.iter().enumerate() {
            let o = Onboard {
                step,
                row: 0,
                pet: settings::the_pet(),
                said: None,
            };
            let frame = o.draw(NEED.0, NEED.1).frame();
            assert!(frame.contains(each.what), "step {step}");
        }
    }
}

//! d-settings, This Mac: every key of `config.toml` as a switch, a choice or a value, each
//! written at once with `qoop_tui::config::set` (comments and keys it does not know are kept,
//! a value it refuses never reaches the file) and confirmed in place; the file itself, read
//! only, beside them; the pet, and six new ones to pick from.

use std::path::PathBuf;

use gpui::{AnyElement, Context, Entity, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::avatar::{self, Avatar};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use qoop_tui::config::Read;

use crate::app::Desk;
use crate::field::Field;
use crate::link;
use crate::state::{self, SETTINGS, Setting, SettingKind};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui;

/// The minutes the sleep settings step through; 0 is never.
const MINUTES: [&str; 6] = ["0", "15", "30", "60", "120", "240"];

/// The settings page as last read.
#[derive(Default)]
pub struct SettingsPage {
    pub read: Option<Read>,
    pub text: String,
    /// What the last write came to: "Sound on: written as line 5, 16:02".
    pub said: Option<String>,
    pub find: Option<Entity<Field>>,
    pub pets: Vec<Avatar>,
    /// A pet picked and waiting out its hold, with when it is saved: Undo takes it back.
    pub pet_hold: Option<(Avatar, std::time::Instant)>,
    /// The file is not TOML as a whole: shown, and nothing on the page writes to it.
    pub broken: bool,
    /// The machine shown: `None` for This Mac, else a space of it to ask through.
    pub via: Option<i64>,
    pub remote: RemoteSettings,
    /// Each settings request still out, under its id: the machine it went to (0 for This
    /// Mac) and whether it writes. An answer from a machine no longer shown is dropped, so a
    /// remote's late answer never lands on This Mac's page, nor This Mac's on a remote's.
    pub sent: std::collections::HashMap<String, (usize, bool)>,
}

impl Desk {
    /// Read the file again: on opening the page, on coming back to the window, after a write.
    pub(crate) fn read_settings(&mut self) {
        let read = qoop_tui::config::read_from(&self.config_path);
        self.settings.text = std::fs::read_to_string(&self.config_path).unwrap_or_default();
        // Nothing can be written while a line is not TOML: the writer refuses the whole file.
        self.settings.broken = read.broken || !qoop_runtime::config::writable(&self.settings.text);
        // The window follows what the file says now: the prefix, the theme.
        self.reconfigure(&read);
        self.settings.read = Some(read);
    }

    /// Write `key` = `value` and say where it landed, or why not.
    pub(crate) fn write_setting(&mut self, setting: &Setting, value: &str, cx: &mut Context<Self>) {
        let said = match qoop_tui::config::set(&self.config_path, setting.key, value) {
            Ok(()) => {
                self.read_settings();
                let shown = match value {
                    "true" => "on",
                    "false" => "off",
                    other => other,
                };
                match state::line_of(&self.settings.text, setting.key) {
                    Some(line) => format!(
                        "{} {shown}: written as line {line}, {}",
                        setting.label,
                        state::clock(self.now, self.zone)
                    ),
                    None => format!("{} {shown}: written", setting.label),
                }
            }
            Err(why) => why,
        };
        self.settings.said = Some(said);
        cx.notify();
    }

    /// The sleep keys the daemon reads go through it, so it answers what the file now says.
    fn write_sleep(&mut self, setting: &Setting, value: &str, cx: &mut Context<Self>) {
        let request = match setting.key {
            "sleep.idle_minutes" => value.parse().ok().map(|m| ControlRequest::SleepSettings {
                id: link::fresh_id("sleep"),
                idle_minutes: Some(m),
                exit_when_asleep: None,
            }),
            "daemon.exit_when_asleep" => Some(ControlRequest::SleepSettings {
                id: link::fresh_id("sleep"),
                idle_minutes: None,
                exit_when_asleep: Some(value == "true"),
            }),
            _ => None,
        };
        match request {
            Some(request) if self.connected => {
                self.settings
                    .sent
                    .insert(request.request_id().to_string(), (0, true));
                self.ask(request);
                self.settings.said = Some(format!("{}: sent to qoop's daemon", setting.label));
                cx.notify();
            }
            _ => self.write_setting(setting, value, cx),
        }
    }

    fn set_setting(&mut self, setting: &Setting, value: &str, cx: &mut Context<Self>) {
        match setting.key {
            "sleep.idle_minutes" | "daemon.exit_when_asleep" => {
                self.write_sleep(setting, value, cx)
            }
            _ => self.write_setting(setting, value, cx),
        }
    }

    /// `/` on the settings page: the find field.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = match self.settings.find.clone() {
            Some(field) => field,
            None => {
                let field = cx.new(|cx| Field::new("Find a setting", cx));
                cx.observe(&field, |_, _, cx| cx.notify()).detach();
                self.settings.find = Some(field.clone());
                field
            }
        };
        let focus = field.read(cx).focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Escape on the settings page: the find field goes. Whether it was there.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.settings.find.take().is_none() {
            return false;
        }
        window.focus(&self.focus, cx);
        cx.notify();
        true
    }

    pub(crate) fn deal_pets(&mut self, cx: &mut Context<Self>) {
        let mut deal = avatar::Deal::new(crate::app::unix_now() as u64);
        self.settings.pets = (0..6).map(|_| deal.pet()).collect();
        cx.notify();
    }

    /// A pet picked: held 3 s as an answer is, with Undo, then saved.
    pub(crate) fn pick_pet(&mut self, pet: Avatar, cx: &mut Context<Self>) {
        let until = std::time::Instant::now() + state::HOLD;
        self.settings.pet_hold = Some((pet, until));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(state::HOLD).await;
            let _ = this.update(cx, |d, cx| {
                d.save_held_pet(false);
                cx.notify();
            });
        })
        .detach();
    }

    /// Save the held pet once its hold is up, or at once (`now`, the window closing).
    pub(crate) fn save_held_pet(&mut self, now: bool) {
        let due = self
            .settings
            .pet_hold
            .as_ref()
            .is_some_and(|(_, until)| now || std::time::Instant::now() >= *until);
        if !due {
            return;
        }
        let Some((pet, _)) = self.settings.pet_hold.take() else {
            return;
        };
        self.settings.said = Some(match avatar::save_to(&self.pet_path, &pet) {
            Ok(()) => "Your pet: saved".into(),
            Err(e) => format!("The pet was not saved: {e}"),
        });
        self.my_pet = Some(pet);
        self.settings.pets.clear();
    }

    pub(crate) fn undo_pet(&mut self, cx: &mut Context<Self>) {
        if self.settings.pet_hold.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn settings_page(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.settings.via.is_some() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(self.settings_machines(t, cx))
                .when_some(self.settings.said.clone(), |d, said| {
                    d.child(
                        div()
                            .id("setting-said")
                            .role(Role::Status)
                            .aria_label(said.clone())
                            .text_size(px(12.5))
                            .text_color(c(t.t1))
                            .child(said),
                    )
                })
                .child(self.remote_settings(t, cx))
                .into_any_element();
        }
        let Some(read) = self.settings.read.as_ref() else {
            return div().into_any_element();
        };
        let blank = qoop_tui::config::read_from(&PathBuf::from("/nonexistent/qoop/config.toml"));
        let broken = self.settings.broken;
        let find = self
            .settings
            .find
            .as_ref()
            .map(|f| f.read(cx).text.clone())
            .unwrap_or_default();
        let shown: Vec<&Setting> = SETTINGS
            .iter()
            .filter(|s| state::setting_found(s, &find))
            .collect();
        let mut groups: Vec<&'static str> = Vec::new();
        for s in &shown {
            if !groups.contains(&s.group) {
                groups.push(s.group);
            }
        }
        let path = state::tilde(&self.config_path.display().to_string());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.settings_machines(t, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(13.))
                    .child(div().font_family(MONO_FONT).child(path))
                    .child(
                        div()
                            .text_color(c(t.t2))
                            .child("a change is written as you make it"),
                    )
                    .child(div().flex_1())
                    .child(ui::kbd("/", t))
                    .child(
                        ui::pill("open-config", "Open in your editor", t)
                            .h(px(30.))
                            .child("Open in your editor")
                            .on_click(cx.listener(|this, _, _, _| {
                                if this.config_path.is_file() {
                                    crate::review::launch(
                                        std::path::Path::new("/usr/bin/open"),
                                        &["-t".into(), this.config_path.display().to_string()],
                                    );
                                }
                            })),
                    ),
            )
            .when(broken, |d| {
                d.child(
                    div()
                        .id("config-broken")
                        .role(Role::Status)
                        .aria_label("config.toml is not TOML; fix it in your editor")
                        .text_size(px(13.))
                        .text_color(c(t.fail_text))
                        .child("config.toml is not TOML; fix it in your editor"),
                )
            })
            .when(!read.exists, |d| {
                d.child(
                    div()
                        .text_size(px(13.))
                        .text_color(c(t.t2))
                        .child("No file yet: every setting is its default"),
                )
            })
            .when_some(self.settings.said.clone(), |d, said| {
                d.child(
                    div()
                        .id("setting-said")
                        .role(Role::Status)
                        .aria_label(said.clone())
                        .text_size(px(12.5))
                        .text_color(c(t.t1))
                        .child(said),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(12.))
                    .child(ui::scroll(
                        div()
                            .id("settings-list")
                            .flex_none()
                            .w(px(520.))
                            .h_full()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .when_some(self.settings.find.clone(), |d, field| d.child(field))
                            .children(groups.into_iter().map(|group| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .p(px(12.))
                                    .rounded(px(14.))
                                    .bg(c(t.card))
                                    .border_1()
                                    .border_color(ca(t.hair))
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(c(t.t2))
                                            .child(group),
                                    )
                                    .children(
                                        shown.iter().filter(|s| s.group == group).map(|s| {
                                            self.setting_row(s, read, &blank, broken, t, cx)
                                        }),
                                    )
                            }))
                            .when(!read.agents.is_empty(), |d| {
                                d.child(
                                    div()
                                        .p(px(12.))
                                        .rounded(px(14.))
                                        .bg(c(t.card))
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.))
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(c(t.t2))
                                                .child("Your agents, [agents]"),
                                        )
                                        .children(read.agents.iter().map(|a| {
                                            div()
                                                .font_family(MONO_FONT)
                                                .text_size(px(12.))
                                                .child(format!("{} = \"{}\"", a.name, a.command))
                                        })),
                                )
                            })
                            .child(self.pet_panel(t, cx)),
                    ))
                    .child(self.file_view(read)),
            )
            .into_any_element()
    }

    fn setting_row(
        &self,
        s: &Setting,
        read: &Read,
        blank: &Read,
        broken: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = state::setting_value(read, &self.settings.text, s.key);
        let default = state::setting_value(blank, "", s.key);
        let changed = value != default;
        let setting = *s;
        let control: AnyElement = match s.kind {
            _ if broken => div()
                .text_size(px(12.))
                .text_color(c(t.t3))
                .child(value.clone())
                .into_any_element(),
            SettingKind::Switch => {
                let on = value == "true";
                ui::switch(("setting", key_id(s.key)), s.label, on, t)
                    .debug_selector(move || format!("setting-{}", setting.key))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_setting(&setting, if on { "false" } else { "true" }, cx)
                    }))
                    .into_any_element()
            }
            SettingKind::Choice(words) => div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(px(3.))
                .children(words.iter().enumerate().map(|(n, word)| {
                    let word = *word;
                    ui::chip(
                        ("setting-choice", key_id(s.key) * 16 + n as u64),
                        word,
                        None,
                        None,
                        word == value,
                        t,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_setting(&setting, word, cx)),
                    )
                }))
                .into_any_element(),
            SettingKind::Minutes => div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(px(3.))
                .children(MINUTES.iter().enumerate().map(|(n, m)| {
                    let m = *m;
                    let label = if m == "0" { "never" } else { m };
                    ui::chip(
                        ("setting-minutes", key_id(s.key) * 16 + n as u64),
                        label,
                        None,
                        None,
                        m == value,
                        t,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_setting(&setting, m, cx)))
                }))
                .into_any_element(),
            SettingKind::Cycle => {
                let choices: Vec<String> = match s.key {
                    "prefix" => qoop_tui::config::prefixes(),
                    _ => qoop_tui::config::sounds_here()
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                };
                let next = choices
                    .iter()
                    .position(|v| *v == value)
                    .map_or(0, |i| (i + 1) % choices.len().max(1));
                let next = choices.get(next).cloned().unwrap_or_default();
                ui::pill(
                    ("setting-cycle", key_id(s.key)),
                    &format!("{}, {value}; next: {next}", s.label),
                    t,
                )
                .h(px(28.))
                .font_family(MONO_FONT)
                .child(value.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.set_setting(&setting, &next, cx)))
                .into_any_element()
            }
            SettingKind::Text => div()
                .font_family(MONO_FONT)
                .text_size(px(12.5))
                .child(value.clone())
                .into_any_element(),
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .py(px(6.))
            .border_t_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .text_size(px(13.))
                            .child(s.label)
                            .when(changed, |d| {
                                d.child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(c(t.t2))
                                        .child("changed"),
                                )
                            }),
                    )
                    .child(div().text_size(px(11.5)).text_color(c(t.t2)).child(
                        match (broken, s.kind) {
                            (true, _) => {
                                "config.toml is not TOML; fix it in your editor".to_string()
                            }
                            (false, SettingKind::Text) => {
                                format!("{}; change it in your editor", s.help)
                            }
                            _ => s.help.to_string(),
                        },
                    )),
            )
            .child(control)
            .into_any_element()
    }

    /// The file as it is, read only: each line numbered, the lines that set something away from
    /// its default marked, and what could not be used said under its line.
    fn file_view(&self, read: &Read) -> impl IntoElement {
        let blank = qoop_tui::config::read_from(&PathBuf::from("/nonexistent/qoop/config.toml"));
        let marked: Vec<usize> = SETTINGS
            .iter()
            .filter(|s| {
                state::setting_value(read, &self.settings.text, s.key)
                    != state::setting_value(&blank, "", s.key)
            })
            .filter_map(|s| state::line_of(&self.settings.text, s.key))
            .collect();
        div()
            .id("config-file")
            .role(Role::Document)
            .aria_label("config.toml, read only")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p(px(12.))
            .rounded(px(14.))
            .bg(c(crate::theme::TERMINAL_BG))
            .font_family(MONO_FONT)
            .text_size(px(12.))
            .text_color(c(crate::theme::DARK.t1))
            .children(self.settings.text.lines().enumerate().map(|(n, line)| {
                let number = n + 1;
                let problems: Vec<String> = read
                    .problems
                    .iter()
                    .filter(|p| p.line == Some(number))
                    .map(|p| p.what.clone())
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap(px(10.))
                            .when(marked.contains(&number), |d| {
                                d.bg(ca(crate::theme::DARK.fill))
                            })
                            .child(
                                div()
                                    .w(px(28.))
                                    .flex_none()
                                    .text_color(c(crate::theme::DARK.t3))
                                    .child(number.to_string()),
                            )
                            .child(div().whitespace_nowrap().child(line.to_string()))
                            // The word, as on the switch rows: the tint alone is about 1.15:1.
                            .when(marked.contains(&number), |d| {
                                d.child(
                                    div()
                                        .debug_selector(move || format!("config-changed-{number}"))
                                        .flex_none()
                                        .text_color(c(crate::theme::DARK.t2))
                                        .child("changed"),
                                )
                            }),
                    )
                    .children(problems.into_iter().map(|p| {
                        div()
                            .pl(px(38.))
                            .text_color(c(crate::theme::DARK.fail_text))
                            .child(p)
                    }))
            }))
            .children(read.problems.iter().filter(|p| p.line.is_none()).map(|p| {
                div()
                    .text_color(c(crate::theme::DARK.fail_text))
                    .child(p.what.clone())
            }))
    }

    fn pet_panel(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(12.))
            .rounded(px(14.))
            .bg(c(t.card))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(t.t2))
                            .child("This Mac's pet"),
                    )
                    .when_some(self.my_pet.as_ref(), |d, pet| d.child(ui::pet(pet, 3.)))
                    .child(div().flex_1())
                    .child(
                        ui::pill("deal-pets", "Deal six new pets", t)
                            .h(px(28.))
                            .child("Deal six new pets")
                            .on_click(cx.listener(|this, _, _, cx| this.deal_pets(cx))),
                    ),
            )
            .when_some(self.settings.pet_hold.as_ref(), |d, (pet, until)| {
                let left = until
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs()
                    .max(1);
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::pet(pet, 2.))
                        .child(
                            div()
                                .id("pet-held")
                                .role(Role::Status)
                                .aria_label(format!(
                                    "Your new pet, saved in {}",
                                    state::count(left as usize, "second")
                                ))
                                .flex_1()
                                .text_size(px(12.5))
                                .child(format!("Your new pet: saved in {left} s")),
                        )
                        .child(
                            ui::pill("pet-undo", "Undo", t)
                                .h(px(26.))
                                .child("Undo")
                                .on_click(cx.listener(|this, _, _, cx| this.undo_pet(cx))),
                        ),
                )
            })
            .when(!self.settings.pets.is_empty(), |d| {
                let name = self
                    .my_pet
                    .as_ref()
                    .map_or_else(|| "none".to_string(), Avatar::name);
                d.child(
                    ui::link_button("pet-keep", &format!("Keep {name}"), t).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.settings.pets.clear();
                            this.settings.pet_hold = None;
                            cx.notify();
                        },
                    )),
                )
            })
            .child(div().flex().flex_wrap().gap(px(8.)).children(
                self.settings.pets.iter().enumerate().map(|(n, pet)| {
                    let picked = pet.clone();
                    ui::control(("pet", n as u64), Role::Button, format!("Pet {}", n + 1), t)
                        .p(px(8.))
                        .rounded(px(10.))
                        .map(|d| ui::paint(d, t.rest()))
                        .hover(|s| ui::paint(s, t.pressed()))
                        .child(ui::pet(pet, 4.))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.pick_pet(picked.clone(), cx)),
                        )
                }),
            ))
    }
}

/// Another machine's settings as its daemon answered them: only what crosses today, its sleep
/// keys and qass's switch.
#[derive(Default)]
pub struct RemoteSettings {
    pub idle_minutes: Option<u64>,
    pub exit_when_asleep: Option<bool>,
    pub qass: Option<bool>,
}

impl Desk {
    /// Show `via`'s machine (`Some` space of it) or This Mac, and read what crosses.
    pub(crate) fn show_settings_machine(&mut self, via: Option<i64>, cx: &mut Context<Self>) {
        self.settings.via = via;
        self.settings.remote = RemoteSettings::default();
        self.settings.said = None;
        if via.is_none() {
            self.read_settings();
        } else {
            self.ask_remote(
                ControlRequest::SleepSettings {
                    id: link::fresh_id("sleep"),
                    idle_minutes: None,
                    exit_when_asleep: None,
                },
                false,
            );
            self.ask_remote(
                ControlRequest::QassWatch {
                    id: link::fresh_id("qass"),
                    on: None,
                },
                false,
            );
        }
        cx.notify();
    }

    /// Ask the machine shown, through `Via`; a write is said when it is answered.
    fn ask_remote(&mut self, request: ControlRequest, write: bool) {
        let Some(workspace) = self.settings.via else {
            return;
        };
        let outer = link::fresh_id("via");
        let machine = state::machine_of(workspace);
        let sent = &mut self.settings.sent;
        sent.insert(request.request_id().to_string(), (machine, write));
        sent.insert(outer.clone(), (machine, write));
        self.ask(ControlRequest::Via {
            id: outer,
            workspace,
            request: Box::new(request),
        });
    }

    /// The machine shown, as a person names it.
    fn settings_machine(&self) -> String {
        self.settings
            .via
            .and_then(|w| self.model.space(w))
            .map_or_else(|| "This Mac".to_string(), state::machine_name)
    }

    /// An answer to a settings request: dropped when its machine is no longer shown, left to
    /// the usual handling (`false`) when it is This Mac's or not a settings request at all.
    pub(crate) fn remote_setting_came(&mut self, response: &ControlResponse) -> bool {
        let id = response.request_id();
        let Some((from, write)) = self.settings.sent.remove(id) else {
            return false;
        };
        if from != self.settings.via.map_or(0, state::machine_of) {
            return true;
        }
        if from == 0 {
            return false;
        }
        let machine = self.settings_machine();
        let when = state::clock(self.now, self.zone);
        let remote = &mut self.settings.remote;
        match response {
            ControlResponse::SleepSettings {
                idle_minutes,
                exit_when_asleep,
                problem,
                ..
            } => {
                remote.idle_minutes = Some(*idle_minutes);
                remote.exit_when_asleep = Some(*exit_when_asleep);
                if let Some(why) = problem {
                    self.settings.said = Some(why.clone());
                } else if write {
                    self.settings.said = Some(format!("Sleep: written on {machine}, {when}"));
                }
            }
            ControlResponse::QassWatch { on, .. } => {
                remote.qass = Some(*on);
                if write {
                    self.settings.said = Some(format!("qass: written on {machine}, {when}"));
                }
            }
            ControlResponse::Failed { reason, .. } => {
                self.settings.said = Some(format!("{machine} did not take it: {reason}"));
            }
            _ => {}
        }
        true
    }

    /// The machine switcher: This Mac and every other machine with a space.
    fn settings_machines(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let machines = state::machines(&self.model.spaces);
        let shown = self.settings.via.map_or(0, state::machine_of);
        div()
            .id("settings-machines")
            .role(Role::RadioGroup)
            .aria_label("Machine")
            .flex()
            .gap(px(4.))
            .when(machines.len() > 1, |d| {
                d.children(machines.into_iter().map(|(m, name, via)| {
                    ui::chip(
                        ("settings-machine", m as u64),
                        &name,
                        None,
                        None,
                        m == shown,
                        t,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.show_settings_machine(via, cx)),
                    )
                }))
            })
    }

    /// Another machine's settings: its sleep keys and qass's switch, written there through
    /// `Via`; every other key is set on that machine.
    fn remote_settings(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let machine = self.settings_machine();
        let stale = self
            .settings
            .via
            .map(|w| self.machine_state(state::machine_of(w)))
            .filter(|s| {
                matches!(
                    s,
                    state::MachineState::Stale { .. } | state::MachineState::NeverReached
                )
            })
            .map(|s| format!("{machine} is {}", state::reach_words(s, self.zone)));
        let off = stale.is_some();
        let remote = &self.settings.remote;
        let row = |label: &'static str, help: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .py(px(6.))
                .border_t_1()
                .border_color(ca(t.hair))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).child(label))
                        .child(div().text_size(px(11.5)).text_color(c(t.t2)).child(help)),
                )
                .child(control)
        };
        let reading = || {
            div()
                .text_size(px(12.))
                .text_color(c(t.t2))
                .child("reading")
                .into_any_element()
        };
        let minutes = match remote.idle_minutes {
            Some(now) => div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap(px(3.))
                .children(MINUTES.iter().enumerate().map(|(n, m)| {
                    let m = *m;
                    let label = if m == "0" { "never" } else { m };
                    let chip = ui::chip(
                        ("remote-minutes", n as u64),
                        label,
                        None,
                        None,
                        m == now.to_string(),
                        t,
                    )
                    .debug_selector(move || format!("remote-minutes-{m}"));
                    if off {
                        return chip.tab_stop(false);
                    }
                    chip.on_click(cx.listener(move |this, _, _, cx| {
                        let Ok(minutes) = m.parse() else { return };
                        this.ask_remote(
                            ControlRequest::SleepSettings {
                                id: link::fresh_id("sleep"),
                                idle_minutes: Some(minutes),
                                exit_when_asleep: None,
                            },
                            true,
                        );
                        cx.notify();
                    }))
                }))
                .into_any_element(),
            None => reading(),
        };
        let exit = match remote.exit_when_asleep {
            Some(on) => {
                let switch = ui::switch("remote-exit", "Leave when all sleep", on, t)
                    .debug_selector(|| "remote-exit".to_string());
                if off {
                    switch.tab_stop(false).into_any_element()
                } else {
                    switch
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ask_remote(
                                ControlRequest::SleepSettings {
                                    id: link::fresh_id("sleep"),
                                    idle_minutes: None,
                                    exit_when_asleep: Some(!on),
                                },
                                true,
                            );
                            cx.notify();
                        }))
                        .into_any_element()
                }
            }
            None => reading(),
        };
        let qass = match remote.qass {
            Some(on) => {
                let switch = ui::switch("remote-qass", "qass watches the agents", on, t);
                if off {
                    switch.tab_stop(false).into_any_element()
                } else {
                    switch
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ask_remote(
                                ControlRequest::QassWatch {
                                    id: link::fresh_id("qass"),
                                    on: Some(!on),
                                },
                                true,
                            );
                            cx.notify();
                        }))
                        .into_any_element()
                }
            }
            None => reading(),
        };
        div()
            .id("remote-settings")
            .debug_selector(|| "remote-settings".to_string())
            .w(px(520.))
            .flex()
            .flex_col()
            .p(px(12.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .when_some(stale, |d, stale| {
                d.child(
                    div()
                        .id("remote-stale")
                        .role(Role::Status)
                        .aria_label(stale.clone())
                        .pb(px(6.))
                        .text_size(px(12.5))
                        .text_color(c(t.t1))
                        .child(stale),
                )
            })
            .child(row(
                "Sleep idle tabs",
                "Minutes a tab idles before it sleeps; never keeps them awake",
                minutes,
            ))
            .child(row(
                "Leave when all sleep",
                "qoop's daemon ends once nothing is awake",
                exit,
            ))
            .child(row(
                "qass watches the agents",
                "qass answers what it can and asks you the rest",
                qass,
            ))
            .child(
                div()
                    .pt(px(8.))
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child(format!(
                        "Every other setting: set on {machine} with qoop config set"
                    )),
            )
            .into_any_element()
    }
}

/// A number for a setting's controls.
fn key_id(key: &str) -> u64 {
    SETTINGS
        .iter()
        .position(|s| s.key == key)
        .map_or(99, |n| n as u64)
}

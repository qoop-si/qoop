//! The voice setting (`qass.persona`) on This Mac's settings page: a filter, the catalog as a
//! list of fixed height (plain first, their own words last once written) that is one Tab stop
//! with Up and Down to move the pick, the lines qass is told for the voice in use, and a box for
//! their own words, saved on its button. A remote machine's page has no voice: it belongs to
//! this Mac's qass.

use std::cell::Cell;
use std::path::PathBuf;

use gpui::{AnyElement, Context, Entity, FocusHandle, FontWeight, Role, div, prelude::*, px};
use qoop_tui::persona;

use crate::app::Desk;
use crate::field::Field;
use crate::state::{self, Setting};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui;

/// The voice setting's fields, kept while the window lives so typed words survive a page change.
pub struct VoiceFields {
    pub find: Entity<Field>,
    pub words: Entity<Field>,
    /// The list's one Tab stop: the row of the voice in use (the first shown when filtered out).
    pub pick: FocusHandle,
    /// What the words box was last filled with from the file: typing differs from it.
    pub shown: String,
    /// The last save's result, for the words it saved: said under the button until they change.
    pub saved: Option<(String, Result<String, String>)>,
    /// Scroll the voice in use into the list on the next draw: on opening, a clear, a save.
    pub reveal: Cell<bool>,
    /// The list and the words box are shown: closed, the setting is one row like the others.
    pub open: bool,
}

/// One row of the list: the value written, its name, its tone, and its group.
struct Choice {
    value: String,
    name: String,
    tone: String,
    group: String,
}

impl Desk {
    fn qass_dir(&self) -> PathBuf {
        persona::dir_for(&self.config_path)
    }

    /// Make the fields once; on every opening after, their own words box takes what
    /// `persona.md` holds now (qass may have written it), unless it holds typing not yet used.
    pub(crate) fn ensure_voice(&mut self, cx: &mut Context<Self>) {
        let said = self.settings.words.clone();
        if let Some(v) = self.settings.voice.as_mut() {
            let typed = v.words.read(cx).text.clone();
            if typed.trim().is_empty() || persona::clean(&typed).0 == v.shown {
                v.words.update(cx, |f, cx| {
                    f.set_text(&said);
                    cx.notify();
                });
                v.shown = said;
            }
            v.reveal.set(true);
            return;
        }
        let find = cx.new(|cx| {
            let mut f = Field::new("A name or a tone", cx);
            f.label = Some("Filter voices".into());
            f
        });
        let words = cx.new(|cx| {
            let mut f = Field::new("calm and brief, calls me Boss, never jokes", cx);
            f.label = Some("Your own words".into());
            f.multiline = true;
            f.set_text(&said);
            f
        });
        cx.observe(&find, |_, _, cx| cx.notify()).detach();
        cx.observe(&words, |_, _, cx| cx.notify()).detach();
        self.settings.voice = Some(VoiceFields {
            find,
            words,
            pick: cx.focus_handle().tab_stop(true),
            shown: said,
            saved: None,
            reveal: Cell::new(true),
            open: false,
        });
    }

    /// "Use these words": keep them in `persona.md` and make them the voice.
    fn use_words(&mut self, cx: &mut Context<Self>) {
        let Some(words) = self.settings.voice.as_ref().map(|v| v.words.clone()) else {
            return;
        };
        let text = words.read(cx).text.clone();
        let result = match persona::save_custom(&self.config_path, &self.qass_dir(), &text) {
            Ok(cut) => {
                self.read_settings();
                Ok(format!(
                    "qass speaks as your own words: written{}, {}",
                    if cut {
                        " (cut to 8 lines and 600 characters)"
                    } else {
                        ""
                    },
                    state::clock(self.now, self.zone)
                ))
            }
            // The words may be saved with the voice not switched: read them back either way.
            Err(why) => {
                self.read_settings();
                Err(why)
            }
        };
        self.settings.said = Some(match &result {
            Ok(said) | Err(said) => said.clone(),
        });
        let saved = self.settings.words.clone();
        if let Some(v) = self.settings.voice.as_mut() {
            v.shown = saved;
            if result.is_ok() {
                v.reveal.set(true);
            }
            v.saved = Some((text, result));
        }
        cx.notify();
    }

    fn voice_choices(&self) -> Vec<Choice> {
        let mut out = vec![Choice {
            value: persona::PLAIN.into(),
            name: "Plain".into(),
            tone: "qass's own wording".into(),
            group: "Start here".into(),
        }];
        out.extend(persona::all().iter().map(|p| Choice {
            value: p.id.clone(),
            name: p.name.clone(),
            tone: p.tone.clone(),
            group: p.group.clone(),
        }));
        if !self.settings.words.is_empty() {
            out.push(Choice {
                value: persona::CUSTOM.into(),
                name: "Your own words".into(),
                tone: "the words below, as saved".into(),
                group: "Yours".into(),
            });
        }
        out
    }

    /// The choices the filter `find` (lower case) leaves.
    fn voices_shown(&self, find: &str) -> Vec<Choice> {
        self.voice_choices()
            .into_iter()
            .filter(|ch| {
                find.is_empty()
                    || [&ch.name, &ch.tone, &ch.group, &ch.value]
                        .iter()
                        .any(|f| f.to_lowercase().contains(find))
            })
            .collect()
    }

    /// Up or Down in the list: the previous or next shown voice becomes the one in use.
    fn step_voice(&mut self, setting: Setting, down: bool, cx: &mut Context<Self>) {
        let Some(v) = self.settings.voice.as_ref() else {
            return;
        };
        let find = v.find.read(cx).text.trim().to_lowercase();
        let now = self
            .settings
            .read
            .as_ref()
            .map(|r| r.qass_persona.clone())
            .unwrap_or_default();
        let shown = self.voices_shown(&find);
        let at = shown.iter().position(|ch| ch.value == now);
        let next = match (at, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(shown.len().saturating_sub(1)),
            (Some(i), false) => i.saturating_sub(1),
        };
        if let Some(ch) = shown.get(next).filter(|ch| ch.value != now) {
            let value = ch.value.clone();
            self.set_setting(&setting, &value, cx);
            if let Some(v) = self.settings.voice.as_ref() {
                v.reveal.set(true);
            }
        }
    }

    pub(crate) fn voice_setting(
        &self,
        s: &Setting,
        value: &str,
        changed: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let setting = *s;
        let Some(fields) = self.settings.voice.as_ref() else {
            return div().into_any_element();
        };
        let find = fields.find.read(cx).text.trim().to_lowercase();
        let shown = self.voices_shown(&find);
        let all = self.voice_choices().len();
        let current = self
            .voice_choices()
            .into_iter()
            .find(|ch| ch.value == value)
            .map_or_else(|| value.to_string(), |ch| ch.name);
        // The one Tab stop: the voice in use, else the first row the filter leaves.
        let stop = shown.iter().position(|ch| ch.value == value).unwrap_or(0);
        let reveal = fields.reveal.replace(false);
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut group = "";
        for (n, ch) in shown.iter().enumerate() {
            if ch.group != group {
                group = &ch.group;
                rows.push(
                    div()
                        .pt(px(6.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(t.t2))
                        .child(ch.group.clone())
                        .into_any_element(),
                );
            }
            let on = ch.value == value;
            let pick = ch.value.clone();
            let row = ui::control(
                ("voice-row", n as u64),
                Role::RadioButton,
                format!("{}: {}", ch.name, ch.tone),
                t,
            );
            let row = if n == stop {
                row.track_focus(&fields.pick)
            } else {
                row.tab_stop(false)
            };
            rows.push(
                row.aria_toggled(ui::toggled(on))
                    .debug_selector({
                        let id = ch.value.clone();
                        move || format!("voice-{id}")
                    })
                    .flex()
                    .flex_col()
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(8.))
                    .when(on, |d| ui::picked(d, t))
                    .when(on && reveal, |d| d.child(ui::bring_into_view()))
                    .child(
                        div()
                            .text_size(px(13.))
                            .when(on, |d| d.font_weight(FontWeight::SEMIBOLD))
                            .child(ch.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c(t.t2))
                            .child(ch.tone.clone()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_setting(&setting, &pick, cx);
                        // The clicked row takes the list's one stop, so Up and Down go on from it.
                        if let Some(v) = this.settings.voice.as_ref() {
                            window.focus(&v.pick.clone(), cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        let empty = shown.is_empty();
        let count = if find.is_empty() {
            format!("{all} voices; Up and Down move the pick")
        } else {
            format!("{} of {all} voices", shown.len())
        };
        let told = if value == persona::CUSTOM && !self.settings.words.is_empty() {
            self.settings.words.clone()
        } else {
            persona::block(value, &self.qass_dir())
                .map(|b| {
                    b.lines()
                        .filter(|l| l.starts_with("> "))
                        .map(|l| l.trim_start_matches("> ").to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_else(|| "Nothing is added: qass words things as its skill says.".into())
        };
        let typed = fields.words.read(cx).text.clone();
        let chars = typed.chars().count();
        let lines = typed.trim().lines().count();
        let over = chars > persona::CUSTOM_CHARS || lines > persona::CUSTOM_LINES;
        let unused = !typed.trim().is_empty() && persona::clean(&typed).0 != fields.shown;
        let result = fields
            .saved
            .as_ref()
            .filter(|(words, _)| *words == typed)
            .map(|(_, r)| r.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .py(px(6.))
            .border_t_1()
            .border_color(ca(t.hair))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
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
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(c(t.t2))
                                    .child(s.help),
                            ),
                    )
                    .child(
                        div()
                            .id("voice-now")
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(current),
                    )
                    .child({
                        let open = fields.open;
                        let label = if open { "Close the list" } else { "Choose a voice" };
                        ui::pill("voice-open", label, t)
                            .debug_selector(|| "voice-open".into())
                            .aria_expanded(open)
                            .h(px(28.))
                            .child(label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(v) = this.settings.voice.as_mut() {
                                    v.open = !v.open;
                                    v.reveal.set(true);
                                }
                                cx.notify();
                            }))
                    }),
            )
            .when(fields.open, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(fields.find.clone())
            .child(
                div()
                    .id("voice-count")
                    .role(Role::Status)
                    .aria_label(count.clone())
                    .text_size(px(11.5))
                    .text_color(c(t.t2))
                    .child(count),
            )
            .when(!empty, |d| {
                d.child(ui::scroll(
                    div()
                        .id("voice-list")
                        .role(Role::RadioGroup)
                        .aria_label(s.label)
                        .h(px(330.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .on_key_down(cx.listener(
                            move |this, e: &gpui::KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
                                "down" => {
                                    this.step_voice(setting, true, cx);
                                    cx.stop_propagation();
                                }
                                "up" => {
                                    this.step_voice(setting, false, cx);
                                    cx.stop_propagation();
                                }
                                _ => {}
                            },
                        ))
                        .children(rows),
                ))
            })
            .when(empty, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(8.))
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .child(format!(
                            "No voice matches \"{find}\". Try a tone (calm, warm, dry) or write your own below."
                        ))
                        .child(
                            ui::pill("voice-clear", "Clear the filter", t)
                                .debug_selector(|| "voice-clear".into())
                                .h(px(28.))
                                .child("Clear the filter")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(v) = this.settings.voice.as_ref() {
                                        v.find.update(cx, |f, cx| {
                                            f.set_text("");
                                            cx.notify();
                                        });
                                        v.reveal.set(true);
                                        window.focus(&v.find.read(cx).focus.clone(), cx);
                                    }
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c(t.t2))
                            .child("What qass is told; a fixed rule after it keeps this to wording"),
                    )
                    .child(
                        div()
                            .id("voice-told")
                            .debug_selector({
                                let value = value.to_string();
                                move || format!("voice-told-{value}")
                            })
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .children(
                                told.lines()
                                    .map(|l| div().child(l.to_string()))
                                    .collect::<Vec<_>>(),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().text_size(px(13.)).child("Your own words"))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c(t.t2))
                            .child("Wording only: the tone, the word qass calls you, what it never does. Tell qass rules as lessons."),
                    )
                    .child(fields.words.clone())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .id("voice-chars")
                                    .text_size(px(11.5))
                                    .text_color(c(if over { t.fail_text } else { t.t2 }))
                                    .child(format!(
                                        "{chars} of {} characters, {lines} of {} lines{}{}",
                                        persona::CUSTOM_CHARS,
                                        persona::CUSTOM_LINES,
                                        if over { ": the end past them is cut" } else { "" },
                                        if unused { "; not used yet" } else { "" },
                                    )),
                            )
                            .child(div().flex_1())
                            .child(
                                ui::pill("voice-use", "Use these words", t)
                                    .debug_selector(|| "voice-use".into())
                                    .h(px(30.))
                                    .child("Use these words")
                                    .on_click(cx.listener(|this, _, _, cx| this.use_words(cx))),
                            ),
                    )
                    .when_some(result, |d, result| {
                        let (said, bad) = match result {
                            Ok(said) => (said, false),
                            Err(why) => (why, true),
                        };
                        d.child(
                            div()
                                .id("voice-saved")
                                .role(Role::Status)
                                .aria_label(said.clone())
                                .text_size(px(11.5))
                                .text_color(c(if bad { t.fail_text } else { t.t1 }))
                                .child(said),
                        )
                    }),
            )
,
                )
            })
            .into_any_element()
    }
}

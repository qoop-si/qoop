//! d-qass: qass's own terminal full height as the chat (its input line is the composer), words
//! to type into it, what it handled today, what it watches with a switch per space and per
//! agent, and its rules as its lessons file counts them.

use gpui::{AnyElement, Context, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{ControlRequest, WorkspaceRow};

use crate::app::{Desk, Page};
use crate::link::{self, Up};
use crate::state::{self, Kind};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

/// Words the chips type into qass's input, without Enter: the owner finishes and sends.
const CHIPS: [&str; 3] = [
    "What did you handle for me today?",
    "Why did you leave the last question to me?",
    "Which of my agents are idle right now?",
];

impl Desk {
    /// qass's space on this Mac, when there is one.
    fn qass_space(&self) -> Option<&WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .find(|w| w.name == state::QASS_SPACE)
    }

    /// Read qass's lessons file again (on opening the page): small, on this Mac only.
    pub(crate) fn read_lessons(&mut self) {
        self.lessons = std::fs::read_to_string(&self.lessons_path)
            .ok()
            .map(|text| state::lessons_of(&text));
    }

    /// Watching: qass reads every question and answers what its rules allow, or none.
    pub(crate) fn set_watch(&mut self, on: bool, cx: &mut Context<Self>) {
        self.model.qass_watch = Some(on);
        self.note_hand();
        self.ask(ControlRequest::QassWatch {
            id: link::fresh_id("qass"),
            on: Some(on),
        });
        cx.notify();
    }

    /// qass leaves a space, or one agent in it, alone (`on` false), or watches it again.
    fn set_skip(
        &mut self,
        workspace: i64,
        terminal: Option<u32>,
        watch: bool,
        cx: &mut Context<Self>,
    ) {
        self.ask(ControlRequest::SetQassSkip {
            id: link::fresh_id("skip"),
            workspace,
            terminal,
            on: !watch,
        });
        cx.notify();
    }

    /// `qoop qass`, as the TUI's Start qass in its ledger runs it: it starts qass in its own space, or wakes
    /// it. Off the window's thread; what it says when it cannot is shown.
    /// Whether a start of qass is under way: asked, and its terminal not an agent yet, for at
    /// most a minute (as `qoop qass` holds its own claim).
    pub(crate) fn qass_starting(&self) -> bool {
        self.qass_starting_until
            .is_some_and(|until| self.now < until)
    }

    pub(crate) fn start_qass(&mut self, cx: &mut Context<Self>) {
        if self.qass_starting() {
            return;
        }
        let Some(qoop) = state::qoop_bin() else {
            self.qass_said = Some(format!(
                "qoop is not installed. Install it with the one-line install: {}",
                state::INSTALL_LINE
            ));
            cx.notify();
            return;
        };
        self.qass_said = Some("Starting qass.".into());
        self.qass_starting_until = Some(self.now + 60);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let ran = cx
                .background_executor()
                .spawn(async move {
                    std::process::Command::new(qoop)
                        .arg("qass")
                        .stdin(std::process::Stdio::null())
                        .output()
                })
                .await;
            let said = match ran {
                Ok(out) if out.status.success() => None,
                Ok(out) => Some(
                    String::from_utf8_lossy(&out.stderr)
                        .trim()
                        .trim_start_matches("qoop: ")
                        .to_string(),
                ),
                Err(e) => Some(format!("qoop qass did not start: {e}")),
            };
            let _ = this.update(cx, |d, cx| {
                // Refused or failed: Start can be pressed again; started, it stays greyed
                // until qass's terminal is an agent.
                if said.is_some() {
                    d.qass_starting_until = None;
                }
                d.qass_said = said;
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn wake_qass(&mut self, cx: &mut Context<Self>) {
        if let Some(space) = self.qass_space().map(|w| w.id) {
            self.ask(ControlRequest::WakeWorkspace {
                id: link::fresh_id("wake"),
                workspace: Some(space),
                terminal: None,
            });
        }
        cx.notify();
    }

    /// A chip: its words into qass's input, with no Enter, and the keys to the terminal.
    fn type_into_qass(&mut self, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.back_to_live();
        if let Some(term) = self.term.as_ref().filter(|_| self.connected) {
            let terminal = term.terminal;
            let _ = self.up.send(Up::Input(terminal, words.as_bytes().to_vec()));
            self.typed_at.insert(terminal, std::time::Instant::now());
            window.focus(&self.term_focus, cx);
        }
    }

    pub(crate) fn qass_page(&self, t: &Theme, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let watching = self.model.qass_watch != Some(false);
        let row = self
            .term
            .as_ref()
            .and_then(|term| self.model.row(term.terminal));
        let asleep = self.qass_space().is_some_and(|w| w.asleep);
        let next = state::next_runs(&self.model.schedules, &self.model.rows, self.zone, 1)
            .into_iter()
            .find(|r| r.running.is_none())
            .map(|r| format!("next {}", state::clock_at(r.at, self.now, self.zone)));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .p(px(14.))
            .rounded(px(14.))
            .bg(c(t.card))
            .border_1()
            .border_color(ca(t.hair))
            .when_some(self.my_pet.as_ref(), |d, pet| d.child(ui::pet(pet, 2.5)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("qass"),
                            )
                            .when_some(row.map(Kind::of), |d, kind| {
                                d.child(ui::mark(kind, t, 11.)).child(
                                    div()
                                        .text_size(px(12.5))
                                        .text_color(c(ui::word_ink(kind, t)))
                                        .child(kind.word()),
                                )
                            })
                            .when_some(row.and_then(|r| r.program.clone()), |d, program| {
                                d.child(ui::tag(&format!("runs on {program}"), t))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child("the assistant that watches your agents"),
                    ),
            )
            .child(
                ui::link_button("nav-schedules", "Schedules", t)
                    .when_some(next, |d, next| {
                        d.child(div().text_size(px(11.5)).text_color(c(t.t2)).child(next))
                    })
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open(Page::Schedules, window, cx)),
                    ),
            )
            .child(div().text_size(px(12.5)).child("Watching"))
            .child(
                ui::switch("watch-head", "qass watching", watching, t)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_watch(!watching, cx))),
            );
        let body: AnyElement = match (&self.term, asleep) {
            (Some(_), _) => div()
                .flex_1()
                .min_h_0()
                .child(self.terminal_area(focused, cx))
                .into_any_element(),
            (None, true) => self
                .qass_state(
                    "qass is sleeping.",
                    ("wake-qass", "Wake it"),
                    true,
                    cx.listener(|this, _, _, cx| this.wake_qass(cx)),
                )
                .into_any_element(),
            (None, false) => self
                .qass_state(
                    "qass is not running.",
                    ("start-qass", "Start it"),
                    !self.qass_starting(),
                    cx.listener(|this, _, _, cx| this.start_qass(cx)),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .gap(px(12.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(header)
                    .when(!watching, |d| {
                        d.child(
                            div()
                                .id("qass-off")
                                .role(Role::Status)
                                .aria_label("qass is not watching. Every question comes to you.")
                                .px(px(4.))
                                .text_size(px(13.))
                                .text_color(c(t.needs_text))
                                .child("qass is not watching. Every question comes to you."),
                        )
                    })
                    .child(body)
                    .when(self.term.is_some(), |d| d.child(self.qass_chips(t, cx))),
            )
            .child(ui::scroll(
                div()
                    .id("qass-side")
                    .flex_none()
                    .w(px(340.))
                    .h_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(self.handled_card(t))
                    .child(self.watch_card(watching, t, cx))
                    .child(self.rules_card(t)),
            ))
            .into_any_element()
    }

    /// qass not running or asleep: the line, its button, and what `qoop qass` said.
    fn qass_state(
        &self,
        line: &str,
        (id, label): (&'static str, &'static str),
        enabled: bool,
        press: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> impl IntoElement {
        // The card is the terminal's ground, dark in both themes, and so is its button.
        let d = &crate::theme::DARK;
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(10.))
            .p(px(20.))
            .rounded(px(14.))
            .bg(c(crate::theme::TERMINAL_BG))
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(c(d.t1))
                    .child(line.to_string()),
            )
            .child(match enabled {
                true => ui::pill(id, label, d).child(label).on_click(press),
                false => ui::pill_off(id, &format!("{label}, unavailable: qass is starting"), d)
                    .child(label),
            })
            .when_some(self.qass_said.clone(), |s, said| {
                s.child(div().text_size(px(12.5)).text_color(c(d.t2)).child(said))
            })
    }

    fn qass_chips(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child("Type into qass:"),
            )
            .children(CHIPS.iter().enumerate().map(|(n, words)| {
                ui::control(("qass-chip", n as u64), Role::Button, *words, t)
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .map(|d| ui::paint(d, t.rest()))
                    .hover(|s| ui::paint(s, t.pressed()))
                    .text_size(px(12.5))
                    .child(*words)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.type_into_qass(words, window, cx)
                    }))
            }))
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child("you press enter"),
            )
    }

    /// What qass did on its own today: when, which agent, what. The why waits for step two.
    fn handled_card(&self, t: &Theme) -> impl IntoElement {
        side_card("Handled for you today", Some(self.acts.len()), t)
            .when(self.acts.is_empty(), |d| {
                d.child(quiet("Nothing yet today.", t))
            })
            .children(self.acts.iter().take(30).map(|a| {
                // Named as the status line names it: space and title, never the daemon's tab.
                let agent = a
                    .terminal
                    .and_then(|n| self.model.row(n))
                    .map(|r| self.model.agent_name(r));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(ca(t.hair))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(12.5))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(agent.unwrap_or_else(|| a.kind.clone())),
                            )
                            .child(
                                div()
                                    .font_family(MONO_FONT)
                                    .text_size(px(11.5))
                                    .text_color(c(t.t2))
                                    .child(state::clock(a.at, self.zone)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(c(t.t1))
                            .child(self.model.act_text(a)),
                    )
            }))
    }

    /// All, then each machine's spaces and their agents, each with its switch and what it
    /// means.
    fn watch_card(&self, watching: bool, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let spaces: Vec<&WorkspaceRow> = self
            .model
            .spaces
            .iter()
            .filter(|w| w.name != state::QASS_SPACE)
            .collect();
        let mut machines: Vec<String> = Vec::new();
        for w in &spaces {
            let m = state::machine_name(w);
            if !machines.contains(&m) {
                machines.push(m);
            }
        }
        let (watched, all) = self.watched_of();
        side_card("What it watches", None, t)
            .child(watch_row(
                ("watch-all", 0),
                "All",
                "Every space on every machine, now and new ones",
                Some(format!("{watched} of {all}")),
                watching,
                true,
                t,
                cx.listener(move |this, _, _, cx| this.set_watch(!watching, cx)),
            ))
            .children(machines.into_iter().map(|machine| {
                let here = machine == "This Mac";
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(8.))
                            .text_size(px(12.5))
                            .child(ui::icon(Icon::Home, 13., c(t.t2)))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(machine.clone()),
                            )
                            .child(div().text_color(c(t.t2)).child(if here {
                                "local"
                            } else {
                                "remote"
                            })),
                    )
                    .children(
                        spaces
                            .iter()
                            .filter(|w| state::machine_name(w) == machine)
                            .map(|w| self.space_watch(w, watching, here, t, cx)),
                    )
            }))
    }

    /// The agents qass watches of all the owner's awake agents.
    fn watched_of(&self) -> (usize, usize) {
        let agents: Vec<_> = self
            .model
            .live()
            .filter(|r| Kind::of(r) != Kind::Sleeping)
            .collect();
        let watched = agents.iter().filter(|r| !r.qass_skip).count();
        (watched, agents.len())
    }

    fn space_watch(
        &self,
        w: &WorkspaceRow,
        watching: bool,
        here: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let space = w.id;
        let on = !w.qass_skip;
        let agents: Vec<_> = self.model.live().filter(|r| r.workspace == space).collect();
        let meaning = if here {
            "Reads its questions; answers what its rules allow"
        } else {
            "Read over the remote link; it never wakes a sleeping agent"
        };
        div()
            .flex()
            .flex_col()
            .child(
                watch_row(
                    ("watch-space", space as u64),
                    &w.name,
                    if on {
                        meaning
                    } else {
                        "Left alone: its questions come to you"
                    },
                    Some(state::count(agents.len(), "agent")),
                    on,
                    watching,
                    t,
                    cx.listener(move |this, _, _, cx| this.set_skip(space, None, !on, cx)),
                )
                .text_color(t.space(state::space_slot(w))),
            )
            .children(agents.into_iter().map(|r| {
                let terminal = r.terminal;
                // The space's own switch covers every agent in it while it is off.
                let tab_on = !r.qass_skip;
                div().pl(px(14.)).child(watch_row(
                    ("watch-tab", u64::from(terminal)),
                    &state::title_of(r),
                    if tab_on {
                        "Watched"
                    } else {
                        "Left alone: its questions come to you"
                    },
                    None,
                    tab_on,
                    watching && on,
                    t,
                    cx.listener(move |this, _, _, cx| {
                        this.set_skip(space, Some(terminal), !tab_on, cx)
                    }),
                ))
            }))
            .into_any_element()
    }

    /// Its rules: what stays the owner's, what qass answers alone, and how many lessons it
    /// keeps, counted from the lessons file on this Mac.
    fn rules_card(&self, t: &Theme) -> impl IntoElement {
        let rule = |key: &str, text: String| {
            div()
                .flex()
                .gap(px(10.))
                .py(px(3.))
                .text_size(px(12.5))
                .child(
                    div()
                        .w(px(96.))
                        .flex_none()
                        .text_color(c(t.t2))
                        .child(key.to_string()),
                )
                .child(div().flex_1().child(text))
        };
        let lessons = self.lessons.unwrap_or_default();
        let count = state::count;
        side_card("Its rules", None, t)
            .child(rule(
                "Always yours",
                format!(
                    "Deleting, releasing, production, money, anything external; {} you taught it",
                    count(lessons.yours, "lesson")
                ),
            ))
            .child(rule(
                "Answers alone",
                format!(
                    "Read-only, or in-repo and reversible; never \"always allow\"; {}",
                    count(lessons.alone, "lesson")
                ),
            ))
            .child(rule(
                "Lessons",
                match self.lessons {
                    Some(l) => format!("{} in lessons.md", l.all),
                    None => "none yet: lessons.md is not there".into(),
                },
            ))
    }
}

/// A card of the right column with its heading and an optional count.
fn side_card(title: &str, count: Option<usize>, t: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(14.))
        .rounded(px(14.))
        .bg(c(t.card))
        .border_1()
        .border_color(ca(t.hair))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pb(px(4.))
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .text_size(px(12.))
                            .text_color(c(t.t2))
                            .child(n.to_string()),
                    )
                }),
        )
}

fn quiet(text: &str, t: &Theme) -> impl IntoElement {
    div()
        .text_size(px(12.5))
        .text_color(c(t.t2))
        .child(text.to_string())
}

/// One switch row: its name, what it means as visible subtext, a count, and the switch.
#[allow(clippy::too_many_arguments)]
fn watch_row(
    (id, n): (&'static str, u64),
    name: &str,
    meaning: &str,
    count: Option<String>,
    on: bool,
    enabled: bool,
    t: &Theme,
    press: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Div {
    let label = match enabled {
        true => format!("Watch {name}"),
        false => format!("Watch {name}, unavailable: what holds it is off"),
    };
    let selector = format!("{id}-{n}");
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .py(px(5.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(13.)).child(name.to_string()))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(c(t.t2))
                        .child(meaning.to_string()),
                ),
        )
        .when_some(count, |d, n| {
            d.child(div().text_size(px(12.)).text_color(c(t.t2)).child(n))
        })
        .child(
            match enabled {
                true => ui::switch((id, n), &label, on, t).on_click(press),
                false => ui::switch_off((id, n), &label, on, t),
            }
            .debug_selector(move || selector.clone()),
        )
}

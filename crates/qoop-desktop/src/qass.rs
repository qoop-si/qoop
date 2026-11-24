//! The qass page: how qass stands and its one control (Pause or Resume while it runs and its
//! switch answers, Start or Wake while it does not), the link to its questions in the Inbox, two
//! columns (Now, Schedules and Rules from lessons.md | Did today as the daemon recorded it, with
//! no reason line), the ask bar, and the chat drawer (`chat.rs`) with qass's own terminal in its
//! Terminal tab. What it watches, per space and per agent, sits behind "What it watches" under
//! Rules.

use std::collections::BTreeSet;

use gpui::{AnyElement, Context, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{ControlRequest, WorkspaceRow};
use qoop_tui::inbox::Inbox;
use qoop_tui::ledger::Stand;

use crate::app::{Desk, Page};
use crate::link::{self, Up};
use crate::state::{self, Kind};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

impl Desk {
    /// qass's space on this Mac, when there is one.
    fn qass_space(&self) -> Option<&WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .find(|w| w.name == state::QASS_SPACE)
    }

    /// Read qass's lessons file again (on opening the page): small, on this Mac only, by the
    /// reader the TUI's ledger uses (a file that is not there is none written yet).
    pub(crate) fn read_lessons(&mut self) {
        self.lessons = qoop_tui::places::rules_read_at(&self.lessons_path);
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

    /// Whether a start of qass is under way: asked, and its terminal not an agent yet, for at
    /// most a minute (as `qoop qass` holds its own claim).
    pub(crate) fn qass_starting(&self) -> bool {
        self.qass_starting_until
            .is_some_and(|until| self.now < until)
    }

    /// `qoop qass`, as the TUI's Start qass in its ledger runs it: it starts qass in its own space,
    /// or wakes it. Off the window's thread; what it says when it cannot is shown.
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

    /// Say `text` to qass, as the TUI's ask line does: pasted into its terminal and sent with
    /// Enter when it is at rest (idle or done, ready, and nobody typing in it); else the words
    /// stay where they were typed and the line says why. True when qass has them.
    pub(crate) fn tell_qass(
        &mut self,
        text: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let said = match self.model.qass_row() {
            None => Some("qass is not running: start it from its page."),
            Some(_) if !self.connected => Some("qoop is not connected: your line is kept."),
            Some(row) => {
                let rest =
                    matches!(row.state.as_str(), "idle" | "done") && row.readiness == "ready";
                let quiet = self.typed_at.get(&row.terminal).is_none_or(|at| {
                    at.elapsed().as_secs() >= qoop_runtime::asks::TYPED_QUIET_SECS.unsigned_abs()
                });
                (!(rest && quiet)).then_some(
                    "qass is busy or waits on you: your line is kept, send it when it is free.",
                )
            }
        };
        if let Some(said) = said {
            self.ask_said = Some(said.to_string());
            cx.notify();
            return false;
        }
        if let Some(terminal) = self.qass_terminal() {
            let _ = self.up.send(Up::Paste(terminal, text.into_bytes()));
            let _ = self.up.send(Up::Input(terminal, b"\r".to_vec()));
        }
        self.ask_said = Some("Sent to qass.".into());
        cx.notify();
        true
    }

    /// The ask bar: its field and Send, or why qass cannot be asked. In the drawer it is the
    /// drawer's own field.
    pub(crate) fn ask_bar(&self, in_drawer: bool, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let stand = state::qass_stand(&self.model, &BTreeSet::new());
        let (id, field) = if in_drawer {
            ("chat-send", self.chat.ask.clone())
        } else {
            ("ask-send", self.qass_ask.clone())
        };
        let said = self.ask_said.clone();
        let body: AnyElement = if stand.runs() {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(field))
                .child(
                    ui::pill(id, "Send to qass", t)
                        .debug_selector(move || id.to_string())
                        .child("Send")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let ask = if in_drawer {
                                this.chat.ask.clone()
                            } else {
                                this.qass_ask.clone()
                            };
                            let text = ask.read(cx).text.trim().to_string();
                            if !text.is_empty() && this.tell_qass(text, window, cx) {
                                ask.update(cx, |f, _| f.set_text(""));
                            }
                        })),
                )
                .into_any_element()
        } else {
            let line = match stand {
                Stand::Asleep => "qass is asleep: wake it to ask it.",
                _ if in_drawer => "qass is stopped. Close this and press Start qass to ask it.",
                _ => "qass is stopped; start it from the top of this page to ask it.",
            };
            div()
                .id("ask-off")
                .debug_selector(|| "ask-off".to_string())
                .role(Role::Status)
                .aria_label(line)
                .text_size(px(13.))
                .text_color(c(t.t2))
                .whitespace_normal()
                .child(line)
                .into_any_element()
        };
        div()
            .id(if in_drawer { "chat-ask" } else { "ask-bar" })
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(body)
            .when_some(said, |d, said| {
                d.child(
                    div()
                        .id("ask-said")
                        .role(Role::Status)
                        .aria_label(said.clone())
                        .text_size(px(12.5))
                        .text_color(c(t.t2))
                        .whitespace_normal()
                        .child(said),
                )
            })
            .into_any_element()
    }

    /// The computers not reached, for qass's line and for where it watches.
    fn lost_machines(&self) -> BTreeSet<usize> {
        state::machines(&self.model.spaces)
            .into_iter()
            .map(|(m, _, _)| m)
            .filter(|m| *m != 0 && state::unreached(self.machine_state(*m)))
            .collect()
    }

    pub(crate) fn qass_page(
        &self,
        list: &Inbox,
        t: &Theme,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let lost = self.lost_machines();
        let lost_names: Vec<String> = state::machines(&self.model.spaces)
            .into_iter()
            .filter(|(m, _, _)| lost.contains(m))
            .map(|(_, name, _)| name)
            .collect();
        let stand = state::qass_stand(&self.model, &lost);
        let seen = self.qass_seen.map(|at| state::clock(at, self.zone));
        let said = state::qass_said(&stand, seen.as_deref(), &lost_names);
        let watching = self.model.qass_watch != Some(false);
        let asleep = matches!(stand, Stand::Asleep);
        // The one control: Pause or Resume while it runs and its switch has answered (an older
        // qoop has none), Wake or Start while it does not.
        let control: Option<AnyElement> = match &stand {
            Stand::Paused => Some(
                ui::pill("qass-resume", "Resume qass", t)
                    .debug_selector(|| "qass-resume".to_string())
                    .child("Resume")
                    .on_click(cx.listener(|this, _, _, cx| this.set_watch(true, cx)))
                    .into_any_element(),
            ),
            Stand::Watching { .. } if self.model.qass_watch.is_some() => Some(
                ui::pill("qass-pause", "Pause qass", t)
                    .debug_selector(|| "qass-pause".to_string())
                    .child("Pause")
                    .on_click(cx.listener(|this, _, _, cx| this.set_watch(false, cx)))
                    .into_any_element(),
            ),
            Stand::Watching { .. } => None,
            Stand::Asleep => Some(
                ui::pill("wake-qass", "Wake qass", t)
                    .debug_selector(|| "wake-qass".to_string())
                    .child("Wake qass")
                    .on_click(cx.listener(|this, _, _, cx| this.wake_qass(cx)))
                    .into_any_element(),
            ),
            Stand::None | Stand::Stopped if self.qass_starting() => Some(
                ui::pill_off("start-qass", "Start qass, unavailable: qass is starting", t)
                    .debug_selector(|| "start-qass".to_string())
                    .child("Start qass")
                    .into_any_element(),
            ),
            Stand::None | Stand::Stopped => Some(
                ui::pill("start-qass", "Start qass", t)
                    .debug_selector(|| "start-qass".to_string())
                    .child("Start qass")
                    .on_click(cx.listener(|this, _, _, cx| this.start_qass(cx)))
                    .into_any_element(),
            ),
        };
        let questions = list.items.iter().filter(|i| i.kind.from_qass()).count();
        let header = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .size(px(44.))
                    .flex_none()
                    .rounded(px(12.))
                    .bg(ca(t.fill))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::qass_mark(22., t)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(220.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .id("title")
                                    .role(Role::Heading)
                                    .aria_level(1)
                                    .aria_label("qass")
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("qass"),
                            )
                            .when_some(
                                self.model.qass_row().and_then(|r| r.program.clone()),
                                |d, program| d.child(ui::tag(&format!("runs on {program}"), t)),
                            ),
                    )
                    .child(
                        div()
                            .id("qass-said")
                            .debug_selector(|| "qass-said".to_string())
                            .role(Role::Status)
                            .aria_label(said.clone())
                            .text_size(px(12.5))
                            .text_color(c(if matches!(stand, Stand::Stopped) {
                                t.fail_text
                            } else {
                                t.t2
                            }))
                            .whitespace_normal()
                            .child(said),
                    ),
            )
            .when(self.qass_terminal().is_some(), |d| {
                d.child(
                    ui::pill("open-chat", "Open the chat with qass", t)
                        .track_focus(&self.chat.button)
                        .debug_selector(|| "open-chat".to_string())
                        .child("Chat")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_chat(crate::chat::ChatTab::Chat, window, cx)
                        })),
                )
            })
            .when_some(control, |d, control| d.child(control));
        let link = (questions > 0).then(|| {
            ui::control(
                "qass-questions",
                Role::Link,
                format!(
                    "{questions} question{} from qass wait in your Inbox, opens From qass",
                    if questions == 1 { "" } else { "s" }
                ),
                t,
            )
            .debug_selector(|| "qass-questions".to_string())
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .py(px(8.))
            .rounded(px(10.))
            .map(|d| ui::paint(d, t.rest()))
            .hover(|s| ui::paint(s, t.pressed()))
            .child(ui::qass_mark(12., t))
            .child(div().flex_1().min_w_0().whitespace_normal().child(format!(
                "{questions} question{} from qass {} in your Inbox",
                if questions == 1 { "" } else { "s" },
                if questions == 1 { "waits" } else { "wait" }
            )))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c(t.t2))
                    .child("opens From qass"),
            )
            .child(ui::icon(Icon::Chevron, 13., c(t.t2)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.inbox_filter = crate::inbox::Filter::FromQass;
                this.open(Page::Inbox, window, cx);
            }))
        });
        let (rules_count, rules) = state::rules_lines(self.lessons);
        let did = state::did_today(&self.model, &self.acts, self.zone);
        // Each schedule once, the next to run first; paused and finished ones after.
        let mut schedules: Vec<&qoop_runtime::control::ScheduleInfo> =
            self.model.schedules.iter().collect();
        schedules.sort_by_key(|s| {
            if s.paused {
                i64::MAX
            } else {
                s.next_at.unwrap_or(i64::MAX - 1)
            }
        });
        let held: Vec<String> = list
            .with_qass
            .iter()
            .map(|t| {
                let who = self.who_of(&state::Slot::Agent(*t));
                match self
                    .model
                    .row(*t)
                    .and_then(|r| r.menu.as_ref())
                    .map(|m| m.question.clone())
                    .filter(|q| !q.is_empty())
                {
                    Some(q) => format!("Holding a question for {who}: {q}"),
                    None => format!("Holding a question for {who}"),
                }
            })
            .collect();
        let idle = match &stand {
            Stand::Watching { .. } => "Holding nothing for you.",
            Stand::Paused => "Paused, holding nothing.",
            _ => "Not running, so nothing is held.",
        };
        let row = |lead: AnyElement, words: String, sub: Option<String>| {
            div()
                .flex()
                .items_start()
                .gap(px(10.))
                .py(px(5.))
                .child(lead)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().whitespace_normal().child(words))
                        .when_some(sub, |d, sub| {
                            d.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .whitespace_normal()
                                    .child(sub),
                            )
                        }),
                )
        };
        let head = |title: &str, count: Option<usize>, extra: Option<AnyElement>| {
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .pt(px(14.))
                .pb(px(2.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .when_some(count, |d, n| {
                    d.child(
                        div()
                            .text_size(px(12.5))
                            .text_color(c(t.t2))
                            .child(n.to_string()),
                    )
                })
                .child(div().flex_1())
                .when_some(extra, |d, e| d.child(e))
        };
        let left =
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(head("Now", None, None))
                .children(
                    held.into_iter()
                        .map(|line| row(ui::qass_mark(12., t).into_any_element(), line, None)),
                )
                .when(list.with_qass.is_empty(), |d| {
                    d.child(div().id("qass-idle").text_color(c(t.t2)).child(idle))
                })
                .child(head(
                    "Schedules",
                    Some(self.model.schedules.len()),
                    Some(
                        ui::link_button("nav-schedules", "All schedules", t)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open(Page::Schedules, window, cx)
                            }))
                            .into_any_element(),
                    ),
                ))
                .when(self.model.schedules.is_empty(), |d| {
                    d.child(div().text_color(c(t.t2)).child("No schedules yet."))
                })
                .children(schedules.into_iter().map(|sch| {
                    row(
                        ui::icon(Icon::Clock, 14., c(t.t2)).into_any_element(),
                        sch.name.clone(),
                        Some(state::schedule_line(sch, self.now, self.zone)),
                    )
                }))
                .child(head(
                    "Rules",
                    (rules_count > 0).then_some(rules_count),
                    Some(
                        ui::link_button("rules-edit", "What it watches", t)
                            .debug_selector(|| "rules-edit".to_string())
                            .aria_expanded(self.watch_open)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.watch_open = !this.watch_open;
                                cx.notify();
                            }))
                            .into_any_element(),
                    ),
                ))
                .children(rules.into_iter().map(|(title, words)| {
                    div()
                        .flex()
                        .flex_col()
                        .py(px(5.))
                        .child(div().whitespace_normal().child(title))
                        .when(!words.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(c(t.t2))
                                    .whitespace_normal()
                                    .child(words),
                            )
                        })
                }))
                .when(self.watch_open, |d| {
                    d.child(div().pt(px(8.)).child(self.watch_card(watching, t, cx)))
                });
        let right = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(head("Did today", Some(did.len()), None))
            .when(did.is_empty(), |d| {
                d.child(
                    div()
                        .id("did-empty")
                        .text_color(c(t.t2))
                        .child("Nothing yet today."),
                )
            })
            .children(did.into_iter().enumerate().map(|(i, d)| {
                div()
                    .id(("did", i as u64))
                    .debug_selector(move || format!("did-{i}"))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .py(px(5.))
                    .child(
                        div()
                            .w(px(40.))
                            .flex_none()
                            .font_family(MONO_FONT)
                            .text_size(px(12.))
                            .text_color(c(t.t3))
                            .child(d.at),
                    )
                    .child(div().flex_1().min_w_0().whitespace_normal().child(d.words))
            }));
        div()
            .id("qass-page")
            .debug_selector(|| "qass-page".to_string())
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(ui::scroll(
                div()
                    .id("qass-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px(px(26.))
                            .pt(px(18.))
                            .pb(px(14.))
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(header)
                            .when_some(link, |d, link| d.child(link))
                            .when(
                                !self.qass_said.clone().unwrap_or_default().is_empty(),
                                |d| {
                                    d.child(
                                        div()
                                            .id("qass-start-said")
                                            .role(Role::Status)
                                            .text_size(px(12.5))
                                            .text_color(c(t.t2))
                                            .whitespace_normal()
                                            .child(self.qass_said.clone().unwrap_or_default()),
                                    )
                                },
                            )
                            .when(!watching && !asleep, |d| {
                                d.child(
                                    div()
                                        .id("qass-off")
                                        .role(Role::Status)
                                        .aria_label(
                                            "qass is not watching. Every question comes to you.",
                                        )
                                        .text_size(px(13.))
                                        .text_color(c(t.needs_text))
                                        .child(
                                            "qass is not watching. Every question comes to you.",
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("qass-columns")
                                    .flex()
                                    .flex_wrap()
                                    .items_start()
                                    .gap_x(px(28.))
                                    .child(div().flex_1().min_w(px(300.)).child(left))
                                    .child(div().flex_1().min_w(px(300.)).child(right)),
                            ),
                    ),
            ))
            .child(
                div()
                    .flex_none()
                    .px(px(26.))
                    .pb(px(14.))
                    .child(self.ask_bar(false, t, cx)),
            )
            .when(self.chat.open, |d| {
                d.child(self.chat_drawer(t, focused, cx))
            })
            .into_any_element()
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

//! qass's place in the sidebar. While qass runs it is qass's own agent page (`Page::Session` of
//! its terminal, as any agent's); this page is what stands there while it does not: "qass is not
//! running." with Start, or "qass is sleeping." with Wake. What qass watches, per space and per
//! agent, is a card of Settings (`watch_card`).

use gpui::{AnyElement, Context, FontWeight, Role, Window, div, prelude::*, px};
use qoop_runtime::control::{ControlRequest, WorkspaceRow};

use crate::app::Desk;
use crate::link::{self, Up};
use crate::state::{self, Kind};
use crate::theme::{Theme, c, ca};
use crate::ui::{self, Icon};

impl Desk {
    /// qass's space on this Mac, when there is one.
    fn qass_space(&self) -> Option<&WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .find(|w| w.name == state::QASS_SPACE)
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

    /// `qoop qass`, as the TUI's `ctrl-a Q` runs it: it starts qass in its own space, or wakes
    /// it. Off the window's thread; what it says when it cannot is shown.
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

    /// Say `text` to qass, as the TUI's talk line does: pasted into its terminal and sent with
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

    /// qass not running or asleep: the line, its button (Start greyed while a start is under
    /// way), and what `qoop qass` said. Its space asleep, or a sleeping tab holding its agent,
    /// is sleeping.
    pub(crate) fn qass_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let asleep = self
            .qass_space()
            .is_some_and(|w| w.asleep || w.sleeping.iter().any(|tab| tab.agent.is_some()));
        // The card is the terminal's ground, dark in both themes, and so is its button.
        let d = &crate::theme::DARK;
        let (line, id, label) = if asleep {
            ("qass is sleeping.", "wake-qass", "Wake it")
        } else {
            ("qass is not running.", "start-qass", "Start it")
        };
        let button = match (asleep, self.qass_starting()) {
            (true, _) => ui::pill(id, label, d)
                .child(label)
                .on_click(cx.listener(|this, _, _, cx| this.wake_qass(cx))),
            (false, false) => ui::pill(id, label, d)
                .child(label)
                .on_click(cx.listener(|this, _, _, cx| this.start_qass(cx))),
            (false, true) => {
                ui::pill_off(id, &format!("{label}, unavailable: qass is starting"), d).child(label)
            }
        };
        div()
            .id("qass-state")
            .debug_selector(|| "qass-state".to_string())
            .size_full()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(10.))
            .p(px(20.))
            .rounded(px(14.))
            .bg(c(crate::theme::TERMINAL_BG))
            .child(
                div()
                    .id("qass-line")
                    .role(Role::Status)
                    .aria_label(line)
                    .text_size(px(14.))
                    .text_color(c(d.t1))
                    .child(line),
            )
            .child(button.debug_selector(move || id.to_string()))
            .when_some(self.qass_said.clone(), |s, said| {
                s.child(
                    div()
                        .id("qass-said")
                        .role(Role::Status)
                        .aria_label(said.clone())
                        .text_size(px(12.5))
                        .text_color(c(d.t2))
                        .whitespace_normal()
                        .child(said),
                )
            })
            .into_any_element()
    }

    /// All, then each machine's spaces and their agents, each with its switch and what it
    /// means.
    pub(crate) fn watch_card(
        &self,
        watching: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let spaces: Vec<&WorkspaceRow> = self
            .model
            .spaces
            .iter()
            .filter(|w| w.name != state::QASS_SPACE && !state::empty_default(w))
            .collect();
        let mut machines: Vec<String> = Vec::new();
        for w in &spaces {
            let m = state::machine_name(w);
            if !machines.contains(&m) {
                machines.push(m);
            }
        }
        let (watched, all) = self.watched_of();
        side_card("What qass watches", None, t)
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

/// A card with its heading and an optional count.
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

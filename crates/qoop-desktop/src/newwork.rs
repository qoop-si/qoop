//! New agent: a sheet with the task and the target as chips: the computer, the space on it (the
//! space of the page on screen to begin with), the agent, and the branch, made from the task's
//! first words unless one is typed. Start makes a terminal in that space's folder and types the
//! shared `worktree_line` (`qoop worktree new <branch> --space N -- <agent> "<task>"`) into it,
//! so the CLI does every check and the agent opens in a tab of its own in a new worktree. The
//! sheet then waits for that agent: when a new agent row appears in the space it lands there,
//! else it says "Started in qoop" and stays until it is closed.
//!
//! The sheet holds the keyboard: Tab and Shift-Tab go round its own controls, Escape closes
//! it, Enter in the task or the branch field starts.

use std::collections::BTreeSet;

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, Role, Window, div,
    prelude::*, px,
};

use crate::app::{Desk, Page};
use crate::field::Field;
use crate::link::fresh_id;
use crate::state;
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

pub struct NewWork {
    pub task: Entity<Field>,
    /// The branch as typed; empty while it follows the task.
    pub branch: Entity<Field>,
    /// Index into `Desk::offers`.
    pub agent: usize,
    pub space: Option<i64>,
    pub machine_open: bool,
    pub space_open: bool,
    /// When the sheet opened: the branch of a task whose words give none is named after it.
    pub opened: i64,
    /// Why the last start did not go.
    pub refused: Option<String>,
    /// Why the space picked is not the one asked for: its computer is not reached.
    pub moved: Option<String>,
    /// The terminals listed when Start was pressed: an agent in the space that is not among them
    /// is the one that started.
    pub before: BTreeSet<u32>,
    /// The line is typed into its shell: now the sheet waits for the agent to appear.
    pub started: bool,
    /// The shell the line was typed into: where a refused `qoop worktree new` says why.
    pub shell: Option<u32>,
    /// The sheet's own stops, in Tab order: task, computer, space, agent, branch, Cancel, Start,
    /// Close.
    pub stops: [FocusHandle; 8],
    /// What had the focus when the sheet opened: it gets it back on close.
    pub back: Option<FocusHandle>,
}

impl Desk {
    /// The space of the page on screen: the agent's own, or a computer's first.
    fn current_space(&self) -> Option<i64> {
        match self.page {
            Page::Session(t) => self.model.row(t).map(|r| r.workspace),
            Page::Computer(m) => self
                .spaces_for_work()
                .into_iter()
                .find(|w| state::machine_of(w.id) == m)
                .map(|w| w.id),
            _ => None,
        }
    }

    /// Whether computer `m` is not reached now.
    fn computer_lost(&self, m: usize) -> bool {
        m != 0 && state::unreached(self.machine_state(m))
    }

    /// Open New agent, in `space` or the space of the page on screen. A task closed unstarted
    /// comes back as it was left (STATE-41), in `space` when one is asked for.
    pub fn new_work(&mut self, space: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sheet) = self.sheet.as_ref() {
            // Already open: never built again over what is typed in it.
            window.focus(&sheet.stops[0], cx);
            return;
        }
        let back = window.focused(cx);
        if let Some(mut draft) = self.work_draft.take() {
            draft.back = back;
            draft.space_open = false;
            draft.machine_open = false;
            if space.is_some() {
                draft.space = space;
            }
            window.focus(&draft.stops[0], cx);
            self.sheet = Some(draft);
            cx.notify();
            return;
        }
        let task = cx.new(|cx| Field::new("What should the agent do?", cx));
        let branch = cx.new(|cx| {
            let mut field = Field::new("named from the task", cx);
            field.label = Some("Branch".into());
            field
        });
        let field_focus = task.read(cx).focus.clone();
        let branch_focus = branch.read(cx).focus.clone();
        let (space, moved) = state::work_space(
            &self.model.spaces,
            space.or_else(|| self.current_space()),
            &|m| self.computer_lost(m),
        );
        window.focus(&field_focus, cx);
        cx.observe(&task, |_, _, cx| cx.notify()).detach();
        cx.observe(&branch, |_, _, cx| cx.notify()).detach();
        self.sheet = Some(NewWork {
            task,
            branch,
            agent: 0,
            space,
            machine_open: false,
            space_open: false,
            opened: crate::app::unix_now(),
            refused: None,
            moved,
            before: BTreeSet::new(),
            started: false,
            shell: None,
            stops: [
                field_focus,
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
                branch_focus,
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
            ],
            back,
        });
        cx.notify();
    }

    /// The branch as it will be made: the typed one, else one named after the task.
    fn branch_of(&self, sheet: &NewWork, cx: &gpui::App) -> String {
        let typed = sheet.branch.read(cx).text.trim().to_string();
        if typed.is_empty() {
            state::branch_or_clock(&sheet.task.read(cx).text, sheet.opened)
        } else {
            typed
        }
    }

    /// Computer `m` picked: its first space, or none when it has none.
    fn pick_computer(&mut self, m: usize) {
        let first = self
            .spaces_for_work()
            .into_iter()
            .find(|w| state::machine_of(w.id) == m)
            .map(|w| w.id);
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.space = first.or(sheet.space);
            sheet.moved = None;
            sheet.machine_open = false;
        }
    }

    /// After a new agent list: an agent that started in the sheet's space is where the person
    /// lands, and the sheet closes.
    pub(crate) fn follow_new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet.as_ref().filter(|s| s.started) else {
            return;
        };
        let Some(space) = sheet.space else { return };
        let Some((_, command)) = self.offers.get(sheet.agent) else {
            return;
        };
        let program = qoop_tui::answers::program_of(command);
        if let Some(terminal) = state::landed(&self.model.rows, &sheet.before, space, program) {
            self.close_sheet(false, window, cx);
            self.open(Page::Session(terminal), window, cx);
        }
    }

    fn spaces_for_work(&self) -> Vec<&qoop_runtime::control::WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .filter(|w| w.name != state::QASS_SPACE)
            .collect()
    }

    /// Close New agent and give the focus back. `keep`: a task typed in it stays for the next
    /// open (Escape, Close); a started one does not.
    pub fn close_sheet(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut sheet) = self.sheet.take() else {
            return;
        };
        let back = sheet.back.take();
        if keep && !sheet.task.read(cx).text.trim().is_empty() {
            self.work_draft = Some(sheet);
        }
        self.focus_back(back, window, cx);
        cx.notify();
    }

    /// The line Start would type, or why it cannot.
    fn line(&self, cx: &gpui::App) -> Result<String, String> {
        let sheet = self.sheet.as_ref().ok_or_else(String::new)?;
        let task = sheet.task.read(cx).text.clone();
        let (_, command) = self
            .offers
            .get(sheet.agent)
            .ok_or_else(|| "No agent can start this way.".to_string())?;
        let space = sheet.space.ok_or_else(|| "Pick a space.".to_string())?;
        if state::clean_prompt(&task).is_empty() {
            return Err("Say what the agent should do.".into());
        }
        state::worktree_line(
            &self.branch_of(sheet, cx),
            Some(state::daemon_space(space)),
            command,
            &task,
        )
        .map_err(str::to_string)
    }

    pub fn start(&mut self, cx: &mut Context<Self>) {
        let Ok(line) = self.line(cx) else { return };
        if self.pending.is_some() || self.sheet.as_ref().is_some_and(|s| s.started) {
            return;
        }
        let before = self.model.rows.iter().map(|r| r.terminal).collect();
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        sheet.refused = None;
        sheet.before = before;
        self.pending = Some((fresh_id("new-work"), line, sheet.space));
        self.ask_pending();
        cx.notify();
    }

    /// Ask for New work's terminal: when Start is pressed, and again under the same id after
    /// a reconnect, which the daemon answers with the first answer when it already made it.
    pub fn ask_pending(&self) {
        if let Some((id, _, workspace)) = &self.pending {
            self.ask(qoop_runtime::control::ControlRequest::CreateTerminal {
                id: id.clone(),
                cwd: None,
                workspace: *workspace,
                name: None,
            });
        }
    }

    /// The terminal for New work is there: type the line into it, close the sheet if it is
    /// still open. Typed even when the sheet was closed while it started.
    pub fn started(
        &mut self,
        id: &str,
        terminal: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .pending
            .as_ref()
            .is_none_or(|(pending, ..)| pending != id)
        {
            return;
        }
        let Some((_, line, _)) = self.pending.take() else {
            return;
        };
        let _ = self.up.send(crate::link::Up::Input(
            terminal,
            format!("{line}\r").into_bytes(),
        ));
        // Started: a sheet closed while it started is no draft any more. An open one stays and
        // waits for the agent to be listed (`follow_new_agent`).
        self.work_draft = None;
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.started = true;
            sheet.shell = Some(terminal);
            sheet.space_open = false;
            sheet.machine_open = false;
            // Its fields and chips are no longer drawn: the keys rest on the way out.
            window.focus(&sheet.stops[5], cx);
        }
        cx.notify();
    }

    pub fn refused(&mut self, id: &str, reason: String) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, ..)| pending == id)
        {
            self.pending = None;
            // Said in the sheet, or in the draft it was closed into.
            if let Some(sheet) = self.sheet.as_mut().or(self.work_draft.as_mut()) {
                sheet.refused = Some(reason);
            }
        }
    }

    /// Keys while the sheet is open, heard before anything inside it.
    pub fn sheet_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        let k = &event.keystroke;
        let at = sheet.stops.iter().position(|h| h.is_focused(window));
        match k.key.as_str() {
            "escape" if sheet.space_open || sheet.machine_open => {
                sheet.space_open = false;
                sheet.machine_open = false;
            }
            "escape" => {
                self.close_sheet(true, window, cx);
                cx.stop_propagation();
                return;
            }
            "tab" => {
                // Once started only the way out is drawn: Cancel, Start and Close, and Open its
                // terminal comes with the page's own order.
                let ring: Vec<usize> = if sheet.started {
                    vec![5, 6, 7]
                } else {
                    (0..sheet.stops.len()).collect()
                };
                let here = at.and_then(|i| ring.iter().position(|r| *r == i));
                let n = ring.len();
                let next = match (here, k.modifiers.shift) {
                    (Some(i), false) => ring[(i + 1) % n],
                    (Some(i), true) => ring[(i + n - 1) % n],
                    (None, _) => ring[0],
                };
                window.focus(&sheet.stops[next], cx);
            }
            "enter" if matches!(at, Some(0 | 4)) => self.start(cx),
            "left" | "right" if at == Some(3) => {
                let n = self.offers.len().max(1);
                sheet.agent = if k.key == "right" {
                    (sheet.agent + 1) % n
                } else {
                    (sheet.agent + n - 1) % n
                };
            }
            "up" | "down" if at == Some(1) => {
                let here = sheet.space.map_or(0, state::machine_of);
                let all: Vec<usize> = state::machines(&self.model.spaces)
                    .into_iter()
                    .map(|(m, _, _)| m)
                    .filter(|m| {
                        self.spaces_for_work()
                            .iter()
                            .any(|w| state::machine_of(w.id) == *m)
                            && !self.computer_lost(*m)
                    })
                    .collect();
                let i = all.iter().position(|m| *m == here).unwrap_or(0);
                let n = all.len().max(1);
                let next = if k.key == "down" {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                if let Some(m) = all.get(next).copied() {
                    self.pick_computer(m);
                }
            }
            "up" | "down" if at == Some(2) => {
                let here = sheet.space.map_or(0, state::machine_of);
                let ids: Vec<i64> = self
                    .spaces_for_work()
                    .iter()
                    .filter(|w| state::machine_of(w.id) == here)
                    .map(|w| w.id)
                    .collect();
                let sheet = self.sheet.as_mut().expect("open");
                let i = ids
                    .iter()
                    .position(|id| Some(*id) == sheet.space)
                    .unwrap_or(0);
                let n = ids.len().max(1);
                let next = if k.key == "down" {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                sheet.space = ids.get(next).copied();
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub fn sheet_view(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(sheet) = self.sheet.as_ref() else {
            return div().into_any_element();
        };
        let task = sheet.task.read(cx).text.clone();
        let line = self.line(cx);
        let branch = self.branch_of(sheet, cx);
        let space = sheet.space.and_then(|id| self.model.space(id));
        let machine = sheet.space.map_or(0, state::machine_of);
        let machine_name = space.map_or_else(|| "This Mac".to_string(), state::machine_name);
        let label = |text: &str| {
            div()
                .w(px(70.))
                .flex_none()
                .text_size(px(12.5))
                .text_color(c(t.t2))
                .child(text.to_string())
        };
        let ring = *t;
        let field_row = div().text_size(px(14.)).child(sheet.task.clone());
        let started = sheet.started;
        let agents = div()
            .id("agent")
            .role(Role::RadioGroup)
            .aria_label("Agent")
            .track_focus(&sheet.stops[3])
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |s| ui::focus_ring(s, &ring))
            .flex()
            .flex_wrap()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(15.))
            .map(|d| ui::paint(d, t.rest()))
            .children(self.offers.iter().enumerate().map(|(i, (name, _))| {
                let on = sheet.agent == i;
                div()
                    .id(("agent-option", i as u64))
                    .role(Role::RadioButton)
                    .aria_label(name.clone())
                    .aria_toggled(ui::toggled(on))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .px(px(10.))
                    .rounded_full()
                    .text_size(px(12.5))
                    .text_color(c(t.t2))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .when(on, |d| ui::picked(d, t))
                    .when_some(state::kind_pet(Some(name)), |d, pet| {
                        d.child(ui::pet(&pet, 1.))
                    })
                    .child(name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(s) = this.sheet.as_mut() {
                            s.agent = i;
                        }
                        cx.notify();
                    }))
            }));
        // A chip that opens a list: its words, a chevron, and the list under it.
        let chip = |id: &'static str, said: String, shown: String, open: bool, stop: usize| {
            ui::control(id, Role::ComboBox, said, t)
                .debug_selector(move || id.to_string())
                .aria_expanded(open)
                .track_focus(&sheet.stops[stop])
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(11.))
                .rounded_full()
                .map(|d| ui::paint(d, t.rest()))
                .border_1()
                .border_color(ca(t.line))
                .text_size(px(12.5))
                .child(shown)
                .child(ui::icon(Icon::Chevron, 11., c(t.t3)))
        };
        let machine_chip = chip(
            "machine",
            format!("Computer, {machine_name}"),
            machine_name.clone(),
            sheet.machine_open,
            1,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(s) = this.sheet.as_mut() {
                s.machine_open = !s.machine_open;
                s.space_open = false;
            }
            cx.notify();
        }));
        let machine_list = sheet.machine_open.then(|| {
            let spaces = self.spaces_for_work();
            div()
                .id("machine-list")
                .role(Role::ListBox)
                .aria_label("Computers")
                .absolute()
                .top(px(32.))
                .left_0()
                .w(px(300.))
                .p(px(4.))
                .rounded(px(10.))
                .bg(c(t.pop))
                .border_1()
                .border_color(ca(t.line))
                .children(
                    state::machines(&self.model.spaces)
                        .into_iter()
                        .filter(|(m, _, _)| spaces.iter().any(|w| state::machine_of(w.id) == *m))
                        .map(|(m, name, _)| {
                            let lost = self.computer_lost(m);
                            let said = if lost {
                                format!(
                                    "{name}, {}",
                                    state::reach_words(self.machine_state(m), self.zone)
                                )
                            } else {
                                name.clone()
                            };
                            let row = div()
                                .id(("machine-option", m as u64))
                                .role(Role::ListBoxOption)
                                .aria_label(said.clone())
                                .aria_selected(machine == m)
                                .px(px(8.))
                                .min_h(px(28.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(8.))
                                .text_size(px(12.5))
                                .child(div().child(name))
                                .when(lost, |d| {
                                    d.text_color(c(t.t3)).child(div().text_size(px(11.5)).child(
                                        state::reach_words(self.machine_state(m), self.zone),
                                    ))
                                });
                            if lost {
                                row.cursor_default().into_any_element()
                            } else {
                                row.cursor_pointer()
                                    .hover(|s| ui::paint(s, t.hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.pick_computer(m);
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            }
                        }),
                )
        });
        let space_name = space.map_or_else(|| "no space".to_string(), |w| w.name.clone());
        let space_chip = chip(
            "space",
            format!("Space, {space_name}"),
            state::short_space_or(space, &space_name),
            sheet.space_open,
            2,
        )
        .when_some(space, |d, w| d.text_color(t.space(state::space_slot(w))))
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(s) = this.sheet.as_mut() {
                s.space_open = !s.space_open;
                s.machine_open = false;
            }
            cx.notify();
        }));
        let space_list = sheet.space_open.then(|| {
            div()
                .id("space-list")
                .role(Role::ListBox)
                .aria_label("Spaces")
                .absolute()
                .top(px(32.))
                .left_0()
                .w(px(260.))
                .p(px(4.))
                .rounded(px(10.))
                .bg(c(t.pop))
                .border_1()
                .border_color(ca(t.line))
                .children(
                    self.spaces_for_work()
                        .into_iter()
                        .filter(|w| state::machine_of(w.id) == machine)
                        .map(|w| {
                            let id = w.id;
                            div()
                                .id(("space-option", id as u64))
                                .role(Role::ListBoxOption)
                                .aria_label(w.name.clone())
                                .aria_selected(sheet.space == Some(id))
                                .cursor_pointer()
                                .px(px(8.))
                                .min_h(px(28.))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .rounded(px(8.))
                                .hover(|s| ui::paint(s, t.hover()))
                                .text_size(px(12.5))
                                .child(
                                    div()
                                        .text_color(t.space(state::space_slot(w)))
                                        .child(state::short_space(w)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(s) = this.sheet.as_mut() {
                                        s.space = Some(id);
                                        s.space_open = false;
                                    }
                                    cx.notify();
                                }))
                        }),
                )
        });
        let ok = line.is_ok() && self.pending.is_none() && !started;
        let reason = sheet.refused.clone().or_else(|| line.clone().err());
        let note = match self.offers.get(sheet.agent) {
            Some((name, command)) if ok && !state::takes_prompt(command) => {
                format!("{name} starts without the task: say it once {name} opens.")
            }
            _ => String::new(),
        };
        let start = div()
            .id("start")
            .role(Role::Button)
            .aria_label(match &reason {
                Some(why) if !ok => format!("Start, unavailable: {why}"),
                _ if started => "Start, unavailable: already started".to_string(),
                _ => "Start".to_string(),
            })
            .track_focus(&sheet.stops[6])
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(34.))
            .px(px(14.))
            .rounded_full()
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |s| ui::focus_ring(s, &ring))
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(ok, |d| ui::paint(d.cursor_pointer(), t.on()))
            .when(!ok, |d| ui::paint(d, t.off()))
            .child(if started {
                "Started"
            } else if self.pending.is_some() {
                "Starting"
            } else {
                "Start"
            })
            .when(!started, |d| {
                d.child(ui::kbd_in("enter", if ok { t.key_on() } else { t.key() }))
            })
            .on_click(cx.listener(|this, _, _, cx| this.start(cx)));
        // The way out by mouse, as the schedule sheet's: Cancel drops the task, Close keeps it.
        let cancel_word = if started { "Close" } else { "Cancel" };
        let cancel = ui::pill("sheet-cancel", cancel_word, t)
            .debug_selector(|| "sheet-cancel".into())
            .track_focus(&sheet.stops[5])
            .child(cancel_word)
            .on_click(cx.listener(|this, _, window, cx| this.close_sheet(false, window, cx)));
        let close = ui::control("sheet-close", Role::Button, "Close New agent", t)
            .debug_selector(|| "sheet-close".into())
            .track_focus(&sheet.stops[7])
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .child(ui::icon(Icon::Close, 14., c(t.t2)))
            .on_click(cx.listener(|this, _, window, cx| this.close_sheet(true, window, cx)));
        let body = if started {
            // Started: what was asked is said back, nothing in it can be changed any more (the
            // agent it waits for is picked by the space and the program it was started with).
            let row = |name: &str, value: String| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(label(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(13.))
                            .whitespace_normal()
                            .child(value),
                    )
            };
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("New agent"),
                        )
                        .child(close),
                )
                .child(
                    div()
                        .id("started-task")
                        .text_size(px(14.))
                        .whitespace_normal()
                        .child(task.clone()),
                )
                .child(row("Computer", machine_name.clone()))
                .child(row("Space", space_name.clone()))
                .child(row(
                    "Agent",
                    self.offers
                        .get(sheet.agent)
                        .map_or_else(String::new, |(name, _)| name.clone()),
                ))
                .child(row("Branch", branch.clone()))
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("New agent"),
                        )
                        .child(ui::kbd("esc", t))
                        .child(close),
                )
                .child(field_row)
                // A computer that is not reached is never picked: said, with the one that was.
                .when_some(sheet.moved.clone(), |d, moved| {
                    d.child(
                        div()
                            .id("new-work-moved")
                            .role(Role::Status)
                            .aria_label(moved.clone())
                            .debug_selector(|| "new-work-moved".to_string())
                            .text_size(px(12.5))
                            .text_color(c(t.needs_text))
                            .whitespace_normal()
                            .child(moved),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(label("Computer"))
                        .child(div().relative().child(machine_chip).children(machine_list)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(label("Space"))
                        .child(div().relative().child(space_chip).children(space_list)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(label("Agent"))
                        .child(agents),
                )
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .child(label("Branch"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .font_family(MONO_FONT)
                                        .text_size(px(12.))
                                        .child(sheet.branch.clone()),
                                )
                                .child(
                                    div()
                                        .debug_selector(|| "branch-used".to_string())
                                        .text_size(px(12.))
                                        .text_color(c(t.t2))
                                        .whitespace_normal()
                                        .child(
                                            if state::clean_prompt(&task).is_empty()
                                                && sheet.branch.read(cx).text.trim().is_empty()
                                            {
                                                "A new worktree, named from the task.".to_string()
                                            } else {
                                                format!("New worktree: {branch}")
                                            },
                                        ),
                                ),
                        ),
                )
        };
        let footer = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .pt(px(4.))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.5))
                    .text_color(c(t.t2))
                    .whitespace_normal()
                    .debug_selector(|| "new-work-note".into())
                    .child(if started {
                        "Started in qoop. The agent opens here once it is listed.".to_string()
                    } else {
                        reason.filter(|_| !ok).unwrap_or(note)
                    }),
            )
            .when_some(
                sheet
                    .shell
                    .filter(|t| started && self.model.row(*t).is_some()),
                |d, shell| {
                    d.child(
                        ui::link_button("open-shell", "Open its terminal", t)
                            .debug_selector(|| "open-shell".to_string())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_sheet(false, window, cx);
                                this.open(Page::Session(shell), window, cx);
                            })),
                    )
                },
            )
            .child(cancel)
            .child(start);
        ui::sheet(
            "new-work-sheet",
            "New agent",
            t,
            body.into_any_element(),
            footer.into_any_element(),
        )
        .into_any_element()
    }
}

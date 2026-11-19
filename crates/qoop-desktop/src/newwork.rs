//! New work (item 21): a sheet with the task, the agent, the space (and so the machine) and the
//! branch made from the task's first words. Start makes a terminal in that space's folder and
//! types `qoop worktree new <branch> --space N -- <agent> "<task>"` into it, so the CLI does
//! every check and the agent opens in a tab of its own in a new worktree.
//!
//! The sheet holds the keyboard: Tab and Shift-Tab go round its own controls, Escape closes
//! it, Enter in the task field starts.

use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent, Role, Window, div,
    prelude::*, px,
};

use crate::app::Desk;
use crate::field::Field;
use crate::link::fresh_id;
use crate::state;
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

pub struct NewWork {
    pub task: Entity<Field>,
    /// Index into `Desk::offers`.
    pub agent: usize,
    pub space: Option<i64>,
    pub space_open: bool,
    /// When the sheet opened: the branch of a task whose words give none is named after it.
    pub opened: i64,
    /// Why the last start did not go.
    pub refused: Option<String>,
    /// The sheet's own stops, in Tab order: task, agent, space, Cancel, Start, Close.
    pub stops: [FocusHandle; 6],
    /// What had the focus when the sheet opened: it gets it back on close.
    pub back: Option<FocusHandle>,
}

impl Desk {
    /// Open New work, in `space` or the first of the owner's spaces. A task closed unstarted
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
            if space.is_some() {
                draft.space = space;
            }
            window.focus(&draft.stops[0], cx);
            self.sheet = Some(draft);
            cx.notify();
            return;
        }
        let task = cx.new(|cx| Field::new("What should the agent do?", cx));
        let field_focus = task.read(cx).focus.clone();
        let space = space.or_else(|| self.spaces_for_work().first().map(|w| w.id));
        window.focus(&field_focus, cx);
        cx.observe(&task, |_, _, cx| cx.notify()).detach();
        self.sheet = Some(NewWork {
            task,
            agent: 0,
            space,
            space_open: false,
            opened: crate::app::unix_now(),
            refused: None,
            stops: [
                field_focus,
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
                cx.focus_handle(),
            ],
            back,
        });
        cx.notify();
    }

    /// The empty first run: one field on Home, "What should an agent work on?", kept while
    /// there is no agent yet.
    pub(crate) fn first_run_field(&mut self, cx: &mut Context<Self>) {
        // By agents, not rows: `ensure-awake` always leaves one plain shell tab, so on a real
        // first open the rows are never empty.
        if !self.model.agents().is_empty() {
            self.first_run = None;
            return;
        }
        if self.first_run.is_some() {
            return;
        }
        let field = cx.new(|cx| Field::new("What should an agent work on?", cx));
        cx.observe(&field, |_, _, cx| cx.notify()).detach();
        self.first_run = Some(field);
    }

    /// Enter in the first run field: New work with those words, started at once in the first
    /// space with the first agent; the sheet stays open only to say why it could not.
    pub(crate) fn first_run_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(field) = self.first_run.clone() else {
            return false;
        };
        if !field.read(cx).focus.is_focused(window) {
            return false;
        }
        if state::clean_prompt(&field.read(cx).text).is_empty() {
            return true;
        }
        self.new_work(None, window, cx);
        let focus = field.read(cx).focus.clone();
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.stops[0] = focus.clone();
            sheet.task = field;
        }
        window.focus(&focus, cx);
        self.start(cx);
        true
    }

    fn spaces_for_work(&self) -> Vec<&qoop_runtime::control::WorkspaceRow> {
        self.model
            .spaces
            .iter()
            .filter(|w| w.name != state::QASS_SPACE)
            .collect()
    }

    /// Close New work and give the focus back. `keep`: a task typed in it stays for the next
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
            &state::branch_or_clock(&task, sheet.opened),
            Some(state::daemon_space(space)),
            command,
            &task,
        )
        .map_err(str::to_string)
    }

    pub fn start(&mut self, cx: &mut Context<Self>) {
        let Ok(line) = self.line(cx) else { return };
        if self.pending.is_some() {
            return;
        }
        let Some(sheet) = self.sheet.as_mut() else {
            return;
        };
        sheet.refused = None;
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
        // Started: a sheet closed while it started is no draft any more.
        self.work_draft = None;
        self.close_sheet(false, window, cx);
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
            "escape" if sheet.space_open => sheet.space_open = false,
            "escape" => {
                self.close_sheet(true, window, cx);
                cx.stop_propagation();
                return;
            }
            "tab" => {
                let n = sheet.stops.len();
                let next = match (at, k.modifiers.shift) {
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                    (None, _) => 0,
                };
                window.focus(&sheet.stops[next], cx);
            }
            "enter" if at == Some(0) => self.start(cx),
            "left" | "right" if at == Some(1) => {
                let n = self.offers.len().max(1);
                sheet.agent = if k.key == "right" {
                    (sheet.agent + 1) % n
                } else {
                    (sheet.agent + n - 1) % n
                };
            }
            "up" | "down" if at == Some(2) => {
                let ids: Vec<i64> = self.spaces_for_work().iter().map(|w| w.id).collect();
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
        let branch = state::branch_or_clock(&task, sheet.opened);
        let space = sheet.space.and_then(|id| self.model.space(id));
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
        let agents = div()
            .id("agent")
            .role(Role::RadioGroup)
            .aria_label("Agent")
            .track_focus(&sheet.stops[1])
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
        let space_name = space.map_or_else(|| "no space".to_string(), |w| w.name.clone());
        let space_button = div()
            .id("space")
            .role(Role::ComboBox)
            .aria_label(format!("Space, {space_name}"))
            .aria_expanded(sheet.space_open)
            .track_focus(&sheet.stops[2])
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(11.))
            .rounded_full()
            .map(|d| ui::paint(d, t.rest()))
            .border_1()
            .border_color(ca(t.line))
            .focus_visible(move |s| ui::focus_ring(s, &ring))
            .text_size(px(12.5))
            .when_some(space, |d, w| d.text_color(t.space(state::space_slot(w))))
            .child(space_name)
            .child(ui::icon(Icon::Chevron, 11., c(t.t3)))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(s) = this.sheet.as_mut() {
                    s.space_open = !s.space_open;
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
                .children(self.spaces_for_work().into_iter().map(|w| {
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
                                .child(w.name.clone()),
                        )
                        .child(div().text_color(c(t.t2)).child(state::machine_name(w)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(s) = this.sheet.as_mut() {
                                s.space = Some(id);
                                s.space_open = false;
                            }
                            cx.notify();
                        }))
                }))
        });
        let ok = line.is_ok() && self.pending.is_none();
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
                _ => "Start".to_string(),
            })
            .track_focus(&sheet.stops[4])
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
            .child(if self.pending.is_some() {
                "Starting"
            } else {
                "Start"
            })
            .child(ui::kbd_in("enter", if ok { t.key_on() } else { t.key() }))
            .on_click(cx.listener(|this, _, _, cx| this.start(cx)));
        // The way out by mouse, as the schedule sheet's: Cancel drops the task, Close keeps it.
        let cancel = ui::pill("sheet-cancel", "Cancel", t)
            .debug_selector(|| "sheet-cancel".into())
            .track_focus(&sheet.stops[3])
            .child("Cancel")
            .on_click(cx.listener(|this, _, window, cx| this.close_sheet(false, window, cx)));
        let close = ui::control("sheet-close", Role::Button, "Close New work", t)
            .debug_selector(|| "sheet-close".into())
            .track_focus(&sheet.stops[5])
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .child(ui::icon(Icon::Close, 14., c(t.t2)))
            .on_click(cx.listener(|this, _, window, cx| this.close_sheet(true, window, cx)));
        let body = div()
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
                            .child("New work"),
                    )
                    .child(ui::kbd("esc", t))
                    .child(close),
            )
            .child(field_row)
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
                    .items_center()
                    .gap(px(10.))
                    .child(label("Space"))
                    .child(div().relative().child(space_button).children(space_list))
                    .when_some(space, |d, w| {
                        d.child(
                            div()
                                .text_size(px(12.5))
                                .text_color(c(t.t1))
                                .child(format!("on {}", state::machine_name(w))),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(label("Branch"))
                    .child(
                        div()
                            .px(px(9.))
                            .py(px(3.))
                            .rounded_full()
                            .border_1()
                            .border_color(ca(t.line))
                            .font_family(MONO_FONT)
                            .text_size(px(11.5))
                            .text_color(c(t.t1))
                            .child(if state::clean_prompt(&task).is_empty() {
                                "named from the task".to_string()
                            } else {
                                branch
                            }),
                    ),
            );
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
                    .debug_selector(|| "new-work-note".into())
                    .child(reason.filter(|_| !ok).unwrap_or(note)),
            )
            .child(cancel)
            .child(start);
        ui::sheet(
            "new-work-sheet",
            "New work",
            t,
            body.into_any_element(),
            footer.into_any_element(),
        )
        .into_any_element()
    }
}

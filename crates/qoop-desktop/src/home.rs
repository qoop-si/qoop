//! Home (d-home): "All agents", the state chips, every agent under its space's header, and
//! Next runs. No spaces in the sidebar here: the same item twice is pointless.

use gpui::{AnyElement, Context, FontWeight, Role, div, prelude::*, px};
use qoop_runtime::control::AgentRow;

use crate::app::{Desk, Page};
use crate::state::{self, Chip, Group, Kind};
use crate::theme::{MONO_FONT, Theme, c, ca};
use crate::ui::{self, Icon};

impl Desk {
    pub fn home(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let groups = self.model.groups(self.chip);
        div()
            .px(px(24.))
            .pt(px(16.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .child(self.chips(t, cx))
            .when(groups.is_empty(), |d| {
                d.child(
                    div()
                        .py(px(24.))
                        .text_size(px(13.5))
                        .text_color(c(t.t2))
                        .child(self.chip.empty()),
                )
            })
            .children(groups.into_iter().map(|g| self.group(g, t, cx)))
            .child(self.next_runs(t, cx))
            .into_any_element()
    }

    fn chips(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().mb(px(12.)).child(
            div()
                .id("chips")
                .role(Role::RadioGroup)
                .aria_label("Show")
                .flex()
                .gap(px(2.))
                .p(px(2.))
                .rounded_full()
                .bg(ca(t.fill))
                .children(Chip::ALL.into_iter().map(|chip| {
                    ui::chip(
                        ("chip", chip as u64),
                        chip.label(),
                        Some(self.model.count(chip)),
                        chip.kind(),
                        self.chip == chip,
                        t,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chip = chip;
                        cx.notify();
                    }))
                })),
        )
    }

    fn group(&self, g: Group<'_>, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let space = g.space;
        let id = space.id;
        let folded = self.model.folded.contains(&id);
        let local = state::machine_of(id) == 0;
        let machine = state::machine_name(space);
        let agents = match g.count {
            1 => "1 agent".to_string(),
            n => format!("{n} agents"),
        };
        ui::card(t)
            .mb(px(10.))
            .px(px(6.))
            .pt(px(2.))
            .pb(px(4.))
            .child(
                div()
                    .debug_selector(move || format!("space-{id}"))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(36.))
                    .pl(px(2.))
                    .pr(px(4.))
                    .when(!folded, |d| d.border_b_1().border_color(ca(t.hair)))
                    .child(
                        ui::control(
                            ("fold", id as u64),
                            Role::Button,
                            format!(
                                "{}, {agents}, {}",
                                space.name,
                                if folded { "folded" } else { "open" }
                            ),
                            t,
                        )
                        .aria_expanded(!folded)
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(6.))
                        .h(px(28.))
                        .rounded(px(8.))
                        .child(
                            div()
                                .text_size(px(13.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.space(state::space_slot(space)))
                                .child(space.name.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.model.folded.remove(&id) {
                                this.model.folded.insert(id);
                            }
                            cx.notify();
                        })),
                    )
                    .when_some(self.my_pet.as_ref().filter(|_| local), |d, pet| {
                        d.child(ui::pet(pet, 1.))
                    })
                    .child(div().text_size(px(12.)).text_color(c(t.t1)).child(machine))
                    .child(ui::tag(if local { "local" } else { "remote" }, t))
                    .when_some(space.dir.clone(), |d, dir| {
                        d.child(
                            div()
                                .font_family(MONO_FONT)
                                .text_size(px(11.))
                                .text_color(c(t.t2))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(state::tilde(&dir)),
                        )
                    })
                    .child(div().flex_1())
                    .child(div().text_size(px(12.)).text_color(c(t.t2)).child(agents))
                    .child(
                        ui::control(
                            ("plus", id as u64),
                            Role::Button,
                            format!("New work in {}", space.name),
                            t,
                        )
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .hover(|s| s.bg(ca(t.fill)))
                        .child(ui::icon(Icon::Plus, 14., c(t.t2)))
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.new_work(Some(id), window, cx),
                        )),
                    ),
            )
            .when(!folded, |d| {
                d.children(g.rows.into_iter().map(|row| self.row(row, t, cx)))
            })
            .into_any_element()
    }

    fn row(&self, row: &AgentRow, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let kind = Kind::of(row);
        let terminal = row.terminal;
        let needs = kind.needs_you();
        ui::control(
            ("row", terminal as u64),
            Role::Link,
            self.model.row_label(row, self.now),
            t,
        )
        .debug_selector(move || format!("row-{terminal}"))
        .flex()
        .items_center()
        .gap(px(10.))
        .min_h(px(46.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(10.))
        .text_size(px(13.))
        .when(needs, |d| d.bg(ca(t.needs_bg)))
        .hover(|s| s.bg(ca(t.fill)))
        .child(ui::mark(kind, t, 12.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(state::title_of(row)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .text_size(px(12.))
                        .text_color(c(t.t2))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(state::sub_of(row))
                        .when(needs, |d| d.child(ui::icon(Icon::Chevron, 10., c(t.t2)))),
                ),
        )
        .child(
            div()
                .w(px(82.))
                .text_size(px(12.5))
                .text_color(c(ui::word_ink(kind, t)))
                .child(kind.word()),
        )
        .child(
            div()
                .w(px(86.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.5))
                .text_color(c(t.t1))
                .when_some(state::kind_pet(row.program.as_deref()), |d, pet| {
                    d.child(ui::pet(&pet, 1.))
                })
                .child(row.program.clone().unwrap_or_default()),
        )
        .child(
            div()
                .w(px(52.))
                .flex()
                .justify_end()
                .text_size(px(12.5))
                .text_color(c(t.t2))
                .child(state::age_of(row, self.now).unwrap_or_default()),
        )
        .on_click(
            cx.listener(move |this, _, window, cx| this.open(Page::Session(terminal), window, cx)),
        )
        .into_any_element()
    }

    /// This machine's coming runs, with the "now" rule between the one going now and the
    /// next ones. Absent with no schedules.
    fn next_runs(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let schedules = &self.model.schedules;
        let runs = state::next_runs(schedules, &self.model.rows, self.zone, 4);
        let on = schedules.iter().filter(|s| !s.paused).count();
        let clock = |at: i64| state::clock_at(at, self.now, self.zone);
        let now_line = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(18.))
            .text_color(c(t.t2))
            .child(
                div()
                    .font_family(MONO_FONT)
                    .text_size(px(10.5))
                    .child(format!("now {}", clock(self.now))),
            )
            .child(div().flex_1().h(px(1.)).bg(c(t.t3)));
        let running = runs.iter().filter(|r| r.running.is_some()).count();
        let line = |r: &state::NextRun| {
            let where_ = match (&r.space, r.running) {
                (_, Some(tab)) => format!("Running now in tab {tab}"),
                (Some(space), None) => format!("{space}, {}", r.when),
                (None, None) => r.when.clone(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_h(px(34.))
                .border_t_1()
                .border_color(ca(t.hair))
                .text_size(px(12.5))
                .child(div().w(px(76.)).font_family(MONO_FONT).child(clock(r.at)))
                .child(
                    div()
                        .w(px(112.))
                        .font_family(MONO_FONT)
                        .child(r.name.clone()),
                )
                .child(
                    div()
                        .w(px(72.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .when_some(state::kind_pet(Some(&r.agent)), |d, pet| {
                            d.child(ui::pet(&pet, 1.))
                        })
                        .child(r.agent.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(c(t.t1))
                        .child(where_),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(c(if r.running.is_some() {
                            t.work_text
                        } else {
                            t.t2
                        }))
                        .child(if r.running.is_some() {
                            "Working".to_string()
                        } else {
                            state::until(r.at, self.now)
                        }),
                )
        };
        div().when(!schedules.is_empty(), |d| {
            d.child(
                ui::card(t)
                    .px(px(14.))
                    .pt(px(12.))
                    .pb(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pb(px(8.))
                            .text_size(px(13.))
                            .child(ui::icon(Icon::Clock, 14., c(t.t2)))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child("Next runs"))
                            .child(
                                div()
                                    .flex_1()
                                    .text_color(c(t.t2))
                                    .child(format!("{} schedules, {on} on", schedules.len())),
                            )
                            .child(
                                ui::link_button("to-schedules", "Schedules", t)
                                    .child(ui::icon(Icon::Chevron, 10., c(t.t2)))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open(Page::Schedules, window, cx)
                                    })),
                            ),
                    )
                    .children(runs[..running].iter().map(line))
                    .child(now_line)
                    .children(runs[running..].iter().map(line)),
            )
        })
    }
}

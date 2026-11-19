//! The questions rail of an agent's pane: a thin column at the right of the pane's tile that
//! says, without opening anything, how many question cards that agent waits on, which, and how
//! far its task is (`2/4` steps and a stage word).
//!
//! Every pane of a local agent keeps one, folded, whether or not a card waits, so a card
//! arriving or answered changes the rail's rows and never the agent's columns. Two agents split
//! in one tab each carry their own. Pure functions of the cards and the row the caller read, so
//! a test reads the rail instead of looking at one.

use qoop_runtime::control::{AskInfo, TaskRef};

use crate::layout::Rect;
use crate::theme::{self, Role};
use crate::width;

/// The rail's columns, the line it shares with its agent left out: wide in a roomy tile, narrow
/// (count only, no stage word) in a smaller one.
pub const WIDE: usize = 6;
pub const NARROW: usize = 4;

/// A tile under this many columns, frame included, takes the narrow rail. The agent beside the
/// wide one then keeps well over [`PANE_MIN`].
const WIDE_TILE: usize = 120;
/// The columns an agent keeps beside its unfolded pane; with fewer the pane opens over its tile.
pub const PANE_MIN: usize = 60;

/// A tile smaller than this, frame included, gets no rail at all: the agent needs the room more.
const TILE_MIN: (usize, usize) = (40, 8);

/// The rows under the cards: the rule, the count, the stage word and a blank.
const FOOT: usize = 4;

/// An agent pane's rail: the agent terminal it speaks for and its width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rail {
    pub terminal: u32,
    pub w: usize,
}

impl Rail {
    /// The columns it takes from its tile: its own and the line between it and the agent.
    pub fn cols(self) -> usize {
        self.w + 1
    }
}

/// How wide the rail of a pane whose tile (its cells, inside the frame) is `tile`; `None` for a
/// tile too small to give any. Measured with the frame's two lines, so a tab of one pane keeps
/// the rail it always had.
pub fn width(tile: Rect) -> Option<usize> {
    let (cols, rows) = (tile.cols + 2, tile.rows + 2);
    if cols < TILE_MIN.0 || rows < TILE_MIN.1 {
        return None;
    }
    Some(if cols >= WIDE_TILE { WIDE } else { NARROW })
}

/// A tile cut for its rail: the agent's cells at the left, then the line, then the rail's cells
/// at the right edge, as tall as the tile.
pub fn split(tile: Rect, rail: Rail) -> (Rect, Rect) {
    let term = Rect {
        cols: tile.cols.saturating_sub(rail.cols()),
        ..tile
    };
    let cells = Rect {
        x: tile.x + term.cols + 1,
        cols: rail.w,
        ..tile
    };
    (term, cells)
}

/// The rail as a frame paints it: its cells in the pane, its terminal, and every live card (the
/// rail picks its terminal's open ones).
#[derive(Clone, Copy, Debug)]
pub struct Paint<'a> {
    pub rect: Rect,
    pub terminal: u32,
    pub cards: &'a [AskInfo],
    /// The rail unfolded to its pane, beside the agent or over its tile.
    pub open: Option<Open<'a>>,
}

/// The pane's columns beside the agent, the line it shares with the panes left out: the width
/// it unfolds to until its edge is dragged or `<` `>` step it.
pub const OPEN: usize = 52;
/// The narrowest a dragged pane goes.
pub const OPEN_MIN: usize = 40;
/// The columns `<` and `>` take off or add.
pub const OPEN_STEP: usize = 4;

/// The unfolded pane's width in `tile`, whose folded rail is `folded` wide, from the width he
/// chose: at least [`OPEN_MIN`], at most the agent's whole cells.
pub fn open_width(tile: Rect, folded: usize, want: usize) -> usize {
    let most = tile.cols.saturating_sub(folded + 1);
    want.clamp(OPEN_MIN.min(most), most)
}

/// Whether a pane `w` wide stands beside the agent in `tile`: the agent keeps [`PANE_MIN`]
/// columns. Else it opens over the agent's cells, which keep their size.
pub fn beside(tile: Rect, w: usize) -> bool {
    tile.cols >= w + 1 + PANE_MIN
}

/// The unfolded pane as a frame paints it: the cells it covers, frame lines included, the
/// questions and the task read for it.
#[derive(Clone, Copy, Debug)]
pub struct Open<'a> {
    pub rect: Rect,
    /// Over the agent's tile, which keeps its size, rather than beside it.
    pub over: bool,
    pub asks: &'a crate::asks::Pane,
    pub focus: bool,
    pub read: Option<&'a TaskRead>,
    /// `s` folded or opened the task part by hand; `None` leaves it to the room.
    pub task_open: Option<bool>,
}

/// What a read of the task said beyond its row: its plan's steps (done or not) and the newest
/// evidence, with when.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskRead {
    pub steps: Vec<(bool, String)>,
    pub evidence: Option<(i64, String)>,
}

impl TaskRead {
    /// From a `TaskRead` answer about `task`: the item's steps (phase headers left out) and the
    /// task's newest evidence, from the item's activity or else the task's own events.
    pub fn of(task: i64, response: &qoop_runtime::control::ControlResponse) -> Option<TaskRead> {
        let qoop_runtime::control::ControlResponse::Tasks { events, item, .. } = response else {
            return None;
        };
        let steps = item
            .as_ref()
            .map(|card| {
                card.steps
                    .iter()
                    .filter(|s| !s.text.starts_with("## "))
                    .map(|s| (s.done_at.is_some(), format!("{}. {}", s.n, s.text)))
                    .collect()
            })
            .unwrap_or_default();
        let from_item = item.as_ref().and_then(|card| {
            card.activity
                .iter()
                .filter(|e| e.task == task && e.kind == "evidence")
                .max_by_key(|e| e.at)
                .map(|e| (e.at, e.body.clone()))
        });
        let evidence = from_item.or_else(|| {
            events
                .iter()
                .filter(|e| e.kind == "evidence")
                .max_by_key(|e| e.at)
                .map(|e| (e.at, e.body.clone()))
        });
        Some(TaskRead { steps, evidence })
    }
}

/// What a read of the task is kept by: the task, the item it is on, its stage and its step
/// counts. Any of them moving reads it again.
pub type ReadKey = (i64, Option<i64>, String, u32, u32);

/// The [`ReadKey`] of `task`.
pub fn read_key(task: &TaskRef) -> ReadKey {
    (
        task.id,
        task.item,
        task.stage.clone(),
        task.steps_done,
        task.steps_total,
    )
}

/// The task under the cards, `cw` columns wide: a rule, the goal, the stage with the steps,
/// the steps (`[x]` done, the first undone `now`, `[ ]`) and the newest evidence; folded, one
/// line (`task #12 · waits · 2/4`).
pub(crate) fn task_rows(
    task: Option<&TaskRef>,
    read: Option<&TaskRead>,
    cw: usize,
    folded: bool,
    now: i64,
) -> Vec<Vec<crate::qass_screen::Seg>> {
    use crate::qass_screen::seg;
    let Some(task) = task else {
        return Vec::new();
    };
    let count = (task.steps_total > 0).then(|| format!("{}/{}", task.steps_done, task.steps_total));
    // `── head ───`, `cols` wide.
    let rule = |head: &str, cols: usize| {
        let rest = cols.saturating_sub(3 + width::width(head) + 1);
        vec![
            seg(Role::Rule, "\u{2500}\u{2500} "),
            seg(Role::FaintText, head.to_string()),
            seg(Role::Rule, format!(" {}", "\u{2500}".repeat(rest))),
        ]
    };
    if folded {
        let mut head = format!("task #{} \u{b7} {}", task.id, stage_word(&task.stage));
        if let Some(count) = &count {
            head = format!("{head} \u{b7} {count}");
        }
        let opens = " s opens";
        // Narrow, the rule gives way so the key that opens it stays in sight.
        if 3 + width::width(&head) + 2 + opens.len() > cw {
            let row = cut(&format!("{head} \u{b7}{opens}"), cw);
            return vec![blank_row(), vec![seg(Role::FaintText, row)]];
        }
        let mut row = rule(&head, cw.saturating_sub(opens.len()));
        row.push(seg(Role::FaintText, opens));
        return vec![blank_row(), row];
    }
    let mut out = vec![blank_row(), rule("task", cw)];
    for line in wrap(&format!("#{} {}", task.id, task.goal), cw, 2) {
        out.push(vec![seg(Role::Text, line)]);
    }
    let steps = count.map_or_else(
        || "no plan steps".to_string(),
        |count| format!("steps {count}"),
    );
    out.push(vec![seg(
        Role::Detail,
        format!("{} \u{b7} {steps}", task.stage),
    )]);
    let Some(read) = read else {
        return out;
    };
    if !read.steps.is_empty() {
        out.push(blank_row());
        let now_at = read.steps.iter().position(|(done, _)| !done);
        for (at, (done, text)) in read.steps.iter().enumerate() {
            let mark = if *done { "[x] " } else { "[ ] " };
            if now_at == Some(at) {
                let room = cw.saturating_sub(4 + 4);
                out.push(vec![
                    seg(
                        Role::Text,
                        format!("{mark}{}", width::fit(&cut(text, room), room)),
                    ),
                    seg(Role::Brand, " now"),
                ]);
            } else {
                let role = if *done { Role::FaintText } else { Role::Text };
                out.push(vec![seg(
                    role,
                    format!("{mark}{}", cut(text, cw.saturating_sub(4))),
                )]);
            }
        }
    }
    if let Some((at, text)) = &read.evidence {
        out.push(blank_row());
        out.push(vec![seg(
            Role::FaintText,
            format!("last evidence, {}", crate::asks::age_short(now - at)),
        )]);
        for line in wrap(text, cw, 2) {
            out.push(vec![seg(Role::Detail, line)]);
        }
    }
    out
}

fn blank_row() -> Vec<crate::qass_screen::Seg> {
    Vec::new()
}

/// `text` cut to `cols` columns, an ellipsis where it was cut.
fn cut(text: &str, cols: usize) -> String {
    if width::width(text) <= cols {
        return text.to_string();
    }
    format!(
        "{}\u{2026}",
        width::fit(text, cols.saturating_sub(1)).trim_end()
    )
}

/// `text` on whole words in rows of `cols`, at most `max` rows, the last cut when more remains.
fn wrap(text: &str, cols: usize, max: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let wide = width::width(&row) + usize::from(!row.is_empty()) + width::width(word);
        if wide > cols && !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.len() > max {
        let rest = rows.split_off(max - 1).join(" ");
        rows.push(cut(&rest, cols));
    }
    rows.into_iter().map(|r| cut(&r, cols)).collect()
}

/// The unfolded pane's rows and where a click lands: the questions of the tab's agent, titled
/// `questions · <agent>  ⠔ N`, its task under them. The paint and a click read the same view;
/// with it, whether the task part is shown whole (`s` flips that).
pub fn pane_view(
    paint: &Paint,
    open: &Open,
    rows: &[qoop_runtime::control::AgentRow],
    spaces: &[qoop_runtime::control::WorkspaceRow],
    now: i64,
    fade: bool,
) -> (crate::asks::View, bool) {
    let agent = rows.iter().find(|r| r.terminal == paint.terminal);
    let name = agent.map(crate::sidebar::session_name).unwrap_or_default();
    let task = agent.and_then(|r| r.task.as_ref());
    let title = format!(
        "questions \u{b7} {name}  \u{2814} {}",
        open_count(paint.cards, paint.terminal)
    );
    let empty = if task.is_some_and(|t| t.stage == "waiting-human") {
        "waiting on you, no question card: its last message says what"
    } else {
        "nothing waits on you here"
    };
    let under = |folded| {
        task_rows(
            task,
            open.read,
            open.rect.cols.saturating_sub(4),
            folded,
            now,
        )
    };
    let view = |under: &[Vec<crate::qass_screen::Seg>]| {
        let ctx = crate::asks::Ctx {
            rows,
            spaces,
            now_unix: now,
            fade,
            board_above: false,
            tab: Some(crate::asks::InTab {
                title: &title,
                empty,
                under,
                focus: open.focus,
            }),
        };
        crate::asks::view(open.asks, &ctx, open.rect.cols, open.rect.rows)
    };
    let (line, mut whole) = (under(true), under(false));
    // Whole when `s` said so, else when it fits in the rows the cards leave with it folded:
    // measured, so an opened card never gives up its rows to the task.
    let short = view(&line);
    let fits = whole.len() <= line.len() + short.spare;
    match (open.task_open.unwrap_or(fits), fits) {
        // Opened by hand where it does not fit: half the rows at most, the cards keep the rest.
        (true, false) => {
            whole.truncate(line.len().max(open.rect.rows.saturating_sub(2) / 2));
            (view(&whole), true)
        }
        (true, _) => (view(&whole), true),
        (false, _) => (short, false),
    }
}

/// The cards `terminal`'s agent waits on: open, not handed to qass or answered. What the rail
/// and the sidebar's count both read.
pub fn open_cards(cards: &[AskInfo], terminal: u32) -> Vec<&AskInfo> {
    cards.iter().filter(|c| waits(c, terminal)).collect()
}

/// How many of them there are.
pub fn open_count(cards: &[AskInfo], terminal: u32) -> usize {
    cards.iter().filter(|c| waits(c, terminal)).count()
}

fn waits(card: &AskInfo, terminal: u32) -> bool {
    card.status == "open" && card.terminal == Some(terminal)
}

/// A stage in a word that fits the wide rail.
pub fn stage_word(stage: &str) -> &str {
    match stage {
        "investigating" => "look",
        "planning" => "plan",
        "implementing" => "build",
        "reviewing" => "review",
        "verifying" => "verify",
        "waiting-human" => "waits",
        other => other,
    }
}

/// The rail's rows, each exactly its width: the open cards' count (`⠔2`, or a quiet `⠄0`), one
/// row per open card (`⠔Q5`), and at the foot a rule over the task's steps done and all (`2/4`)
/// and its stage word (wide only). No task, no foot. `fade`: a box is open over the screen.
pub fn rows(paint: &Paint, task: Option<&TaskRef>, fade: bool) -> Vec<String> {
    let w = paint.rect.cols;
    let total = paint.rect.rows;
    let ink = |role: Role| theme::fg(if fade { Role::Unobserved } else { role });
    // The wide rail stands its words a blank in from the line, the narrow one against it.
    let lead = if w >= WIDE { " " } else { "" };
    let cell = |text: &str, role: Role| {
        format!(
            "{}{}{}",
            ink(role),
            width::fit(&format!("{lead}{text}"), w),
            theme::RESET
        )
    };
    let blank = " ".repeat(w);
    let open = open_cards(paint.cards, paint.terminal);

    let mut foot = Vec::new();
    if let Some(task) = task {
        if task.steps_total > 0 {
            let done = task.steps_done == task.steps_total;
            let role = if done { Role::Done } else { Role::Detail };
            foot.push(cell(
                &format!("{}/{}", task.steps_done, task.steps_total),
                role,
            ));
        }
        if w >= WIDE {
            foot.push(cell(stage_word(&task.stage), Role::FaintText));
        }
    }
    let foot_rows = if foot.is_empty() { 0 } else { FOOT };

    let mut out = Vec::with_capacity(total);
    out.push(if open.is_empty() {
        cell("\u{2804}0", Role::Faint)
    } else {
        cell(&format!("\u{2814}{}", open.len()), Role::Blocked)
    });
    out.push(blank.clone());
    // The cards between the count and the foot, a blank row kept above the foot; the last row
    // says how many more there are when they do not all fit.
    let room = total.saturating_sub(out.len() + foot_rows + usize::from(foot_rows > 0));
    let shown = if open.len() > room {
        room.saturating_sub(1)
    } else {
        open.len()
    };
    for card in &open[..shown] {
        out.push(cell(&format!("\u{2814}{}", card.card), Role::Blocked));
    }
    if shown < open.len() && room > 0 {
        out.push(cell(&format!("+{}", open.len() - shown), Role::Blocked));
    }
    while out.len() + foot_rows < total {
        out.push(blank.clone());
    }
    if foot_rows > 0 {
        out.push(format!(
            "{}{}{}",
            ink(Role::Rule),
            "\u{2500}".repeat(w),
            theme::RESET
        ));
        out.extend(foot);
        while out.len() < total {
            out.push(blank.clone());
        }
    }
    out.truncate(total);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(n: u32, terminal: u32, status: &str) -> AskInfo {
        serde_json::from_value(serde_json::json!({
            "card": format!("Q{n}"),
            "terminal": terminal,
            "status": status,
            "opened_at": 0,
            "body": {
                "handle": "claude", "urgency": "later", "doing": "d", "why": "w",
                "asks": "a?", "options": [],
            },
        }))
        .expect("a card")
    }

    fn task(stage: &str, done: u32, total: u32) -> TaskRef {
        TaskRef {
            id: 12,
            goal: "g".into(),
            stage: stage.into(),
            claimed: false,
            item: (total > 0).then_some(3),
            steps_done: done,
            steps_total: total,
        }
    }

    /// The rail's text, escapes taken out and trailing blanks kept off.
    fn text(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let mut out = String::new();
                let mut chars = r.chars();
                while let Some(c) = chars.next() {
                    if c == '\x1b' {
                        for c in chars.by_ref() {
                            if c.is_ascii_alphabetic() {
                                break;
                            }
                        }
                        continue;
                    }
                    out.push(c);
                }
                out.trim_end().to_string()
            })
            .collect()
    }

    fn paint(cards: &[AskInfo], w: usize, rows: usize) -> Paint<'_> {
        Paint {
            rect: Rect {
                x: 0,
                y: 1,
                cols: w,
                rows,
            },
            terminal: 7,
            cards,
            open: None,
        }
    }

    /// Wide in a roomy tile, narrow under 120 columns with its frame, none in a tile too small
    /// to spare it; the tile splits into the agent's cells, the line and the rail's.
    #[test]
    fn the_rail_is_as_wide_as_the_tile_allows() {
        let tile = |cols, rows| Rect {
            x: 1,
            y: 1,
            cols,
            rows,
        };
        assert_eq!(width(tile(129, 39)), Some(WIDE));
        assert_eq!(width(tile(117, 39)), Some(NARROW));
        assert_eq!(width(tile(39, 17)), Some(NARROW));
        assert_eq!(width(tile(37, 17)), None);
        assert_eq!(width(tile(78, 5)), None);
        let rail = Rail {
            terminal: 1,
            w: WIDE,
        };
        let (term, cells) = split(tile(129, 39), rail);
        assert_eq!(term, tile(122, 39));
        assert_eq!(
            cells,
            Rect {
                x: 124,
                y: 1,
                cols: 6,
                rows: 39
            }
        );
    }

    /// Nothing open: a quiet `⠄0`, and the foot still says how far the task is.
    #[test]
    fn a_quiet_rail_counts_none_and_keeps_the_task() {
        let rows = rows(
            &paint(&[], WIDE, 12),
            Some(&task("implementing", 2, 4)),
            false,
        );
        assert_eq!(rows.len(), 12);
        let text = text(&rows);
        assert_eq!(text[0], " \u{2804}0");
        assert_eq!(
            &text[8..],
            [
                "\u{2500}".repeat(6),
                " 2/4".into(),
                " build".into(),
                String::new()
            ]
        );
        assert!(text[1..8].iter().all(String::is_empty), "{text:?}");
        assert!(text.iter().all(|t| width::width(t) <= WIDE));
    }

    /// The open cards of its own terminal only, one row each under the count; answered ones and
    /// other terminals' are not counted.
    #[test]
    fn the_rail_lists_its_terminals_open_cards() {
        let cards = [
            card(5, 7, "open"),
            card(6, 7, "open"),
            card(3, 7, "answered"),
            card(9, 8, "open"),
        ];
        let text = text(&rows(&paint(&cards, WIDE, 12), None, false));
        assert_eq!(text[0], " \u{2814}2");
        assert_eq!(text[2], " \u{2814}Q5");
        assert_eq!(text[3], " \u{2814}Q6");
        assert!(
            text[4..].iter().all(String::is_empty),
            "no task, no foot: {text:?}"
        );
    }

    /// A task with no linked item shows its stage word alone; at the narrow width the word drops
    /// and the count stays; a plan grown to six steps reads `d/6`.
    #[test]
    fn the_foot_says_steps_and_stage_as_the_width_allows() {
        let text_at = |w, t: TaskRef| text(&rows(&paint(&[], w, 10), Some(&t), false));
        let alone = text_at(WIDE, task("waiting-human", 0, 0));
        assert_eq!(
            &alone[6..],
            [
                "\u{2500}".repeat(6),
                " waits".into(),
                String::new(),
                String::new()
            ]
        );
        let narrow = text_at(NARROW, task("implementing", 2, 4));
        assert_eq!(narrow[0], "\u{2804}0");
        assert_eq!(
            &narrow[6..],
            [
                "\u{2500}".repeat(4),
                "2/4".into(),
                String::new(),
                String::new()
            ]
        );
        assert_eq!(text_at(WIDE, task("implementing", 2, 6))[7], " 2/6");
        // Narrow, a stage alone has nothing left to say: no foot.
        let bare = text_at(NARROW, task("waiting-human", 0, 0));
        assert_eq!(bare[0], "\u{2804}0");
        assert!(bare[1..].iter().all(String::is_empty), "{bare:?}");
    }

    /// More cards than rows: as many as fit and a `+n` for the rest, the foot kept whole.
    #[test]
    fn a_long_list_ends_in_how_many_more() {
        let cards: Vec<AskInfo> = (1..=9).map(|n| card(n, 7, "open")).collect();
        let text = text(&rows(
            &paint(&cards, NARROW, 10),
            Some(&task("planning", 0, 3)),
            false,
        ));
        assert_eq!(text.len(), 10);
        assert_eq!(text[0], "\u{2814}9");
        assert_eq!(text[2], "\u{2814}Q1");
        assert_eq!(text[3], "\u{2814}Q2");
        assert_eq!(text[4], "+7");
        assert_eq!(text[6], "\u{2500}".repeat(4));
        assert_eq!(text[7], "0/3");
    }
}

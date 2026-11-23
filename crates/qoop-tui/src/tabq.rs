//! The reserved line under an agent's pane: one row at the foot of its tile, framed like one
//! more pane under the agent, that says, without opening anything, that the agent needs the
//! person (and where to answer), and how far its task is (`2/4` steps and the stage).
//!
//! Every pane of a local agent keeps one whether or not anything is said in it, so a question
//! arriving or answered changes the line's words and never the agent's rows. Two agents split
//! in one tab each carry their own. Pure functions of the row the caller read, so a test reads
//! the line instead of looking at one. The questions themselves are the Inbox's (`ctrl-a i`).

use qoop_runtime::control::{AskInfo, TaskRef};

use crate::layout::Rect;
use crate::theme::{self, Role};
use crate::width;

/// What the line says under an agent that needs the person. Fixed words: a question is never
/// cut to fit here, it is read in the Inbox.
pub const WORDS: &str = "needs you: ctrl-a i answers";

/// A tile smaller than this, frame included, gets no line at all: the agent needs the room more.
const TILE_MIN: (usize, usize) = (40, 8);

/// Whether a tile (its cells, inside the frame) is big enough to spare the line. Measured with
/// the frame's two lines, so a tab of one pane keeps the line it always had.
pub fn fits(tile: Rect) -> bool {
    tile.cols + 2 >= TILE_MIN.0 && tile.rows + 2 >= TILE_MIN.1
}

/// A tile cut for its line: the agent's cells above, the frame's rule they share, and the line's
/// one row under it, as wide as the tile. The agent keeps every column.
pub fn split(tile: Rect) -> (Rect, Rect) {
    let term = Rect {
        rows: tile.rows.saturating_sub(2),
        ..tile
    };
    let line = Rect {
        y: tile.y + term.rows + 1,
        rows: 1,
        ..tile
    };
    (term, line)
}

/// The line as a frame paints it: its cells in the pane, the agent it speaks for, and whether
/// the Inbox counts something about that agent.
#[derive(Clone, Copy, Debug)]
pub struct Paint {
    pub rect: Rect,
    pub terminal: u32,
    pub waiting: bool,
}

/// The cards `terminal`'s agent waits on: open, not handed to qass or answered.
pub fn open_cards(cards: &[AskInfo], terminal: u32) -> Vec<&AskInfo> {
    cards.iter().filter(|c| waits(c, terminal)).collect()
}

fn waits(card: &AskInfo, terminal: u32) -> bool {
    card.status == "open" && card.terminal == Some(terminal)
}

/// The task's progress as the line's right end says it, the fullest first: `2/4 implementing`,
/// then `2/4`; a task with no plan steps says its stage alone.
fn progress(task: &TaskRef) -> Vec<String> {
    let stage = task.stage.replace('-', " ");
    if task.steps_total == 0 {
        return vec![stage];
    }
    let count = format!("{}/{}", task.steps_done, task.steps_total);
    vec![format!("{count} {stage}"), count]
}

/// The line, exactly its width: the words on the left in the needs-you ink when the agent
/// waits, the task's progress flush right in the most that fits whole after them. The words
/// are never cut and nothing is: what does not fit is left out whole, and the words win the
/// room. `fade`: a box is open over the screen.
pub fn row(paint: &Paint, task: Option<&TaskRef>, fade: bool) -> String {
    let w = paint.rect.cols;
    let ink = |role: Role| theme::fg(if fade { Role::Unobserved } else { role });
    let words = paint.waiting.then_some(WORDS);
    // A blank in from each edge, and two between the words and the progress.
    let left = words.map_or(0, |t| 1 + width::width(t));
    let right = task.and_then(|task| {
        let done = task.steps_total > 0 && task.steps_done == task.steps_total;
        progress(task)
            .into_iter()
            .find(|p| left + 2 + width::width(p) < w)
            .map(|p| (p, if done { Role::Done } else { Role::Detail }))
    });
    let mut out = String::new();
    let mut used = 0;
    if let Some(text) = words.filter(|t| width::width(t) < w) {
        out.push_str(&format!("{} {text}{}", ink(Role::Blocked), theme::RESET));
        used = 1 + width::width(text);
    }
    if let Some((text, role)) = right {
        let gap = w.saturating_sub(used + width::width(&text) + 1);
        out.push_str(&" ".repeat(gap));
        out.push_str(&format!("{}{text}{}", ink(role), theme::RESET));
        used += gap + width::width(&text);
    }
    out.push_str(&" ".repeat(w.saturating_sub(used)));
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

    /// The line's text, escapes taken out and trailing blanks kept.
    fn text(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
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
        out
    }

    fn paint(cols: usize, waiting: bool) -> Paint {
        Paint {
            rect: Rect {
                x: 0,
                y: 1,
                cols,
                rows: 1,
            },
            terminal: 7,
            waiting,
        }
    }

    /// A tile too small to spare the row gets none; the tile splits into the agent's cells,
    /// which keep every column of it, the frame's rule and the line's one row.
    #[test]
    fn the_agent_keeps_every_column_and_gives_one_row() {
        let tile = |cols, rows| Rect {
            x: 1,
            y: 1,
            cols,
            rows,
        };
        assert!(fits(tile(129, 39)));
        assert!(fits(tile(38, 6)));
        assert!(!fits(tile(36, 17)));
        assert!(!fits(tile(60, 5)));
        let (term, line) = split(tile(129, 39));
        assert_eq!((term.x, term.y, term.cols, term.rows), (1, 1, 129, 37));
        assert_eq!((line.x, line.y, line.cols, line.rows), (1, 39, 129, 1));
    }

    /// Fixed words under an agent that needs the person, task progress at the right end of the
    /// same line, and every row exactly its width.
    #[test]
    fn the_line_says_the_words_and_the_progress_at_its_right_end() {
        let row = text(&row(
            &paint(80, true),
            Some(&task("implementing", 2, 4)),
            false,
        ));
        assert_eq!(width::width(&row), 80);
        assert!(row.starts_with(" needs you: ctrl-a i answers"), "{row:?}");
        // Flush right, one blank in from the edge: the progress stands at the line's end.
        assert!(row.ends_with("2/4 implementing "), "{row:?}");
        // The words are the constant, whatever the task says.
        assert_eq!(WORDS, "needs you: ctrl-a i answers");
    }

    /// Nothing waits: no words, and the progress still stands at the right.
    #[test]
    fn a_quiet_agent_keeps_the_progress_alone() {
        let quiet = text(&row(
            &paint(60, false),
            Some(&task("verifying", 4, 4)),
            false,
        ));
        assert!(!quiet.contains("needs you"), "{quiet:?}");
        assert!(quiet.trim_end().ends_with("4/4 verifying"), "{quiet:?}");
        assert!(quiet.starts_with(' '));
        let bare = text(&row(&paint(60, false), None, false));
        assert_eq!(bare.trim(), "", "no task, nobody waiting: an empty line");
        assert_eq!(width::width(&bare), 60);
        // A task with no plan steps says its stage alone.
        let plain = text(&row(
            &paint(60, false),
            Some(&task("waiting-human", 0, 0)),
            false,
        ));
        assert!(plain.trim_end().ends_with("waiting human"), "{plain:?}");
    }

    /// The words keep the room: the stage goes first, then the count, never half of either, and
    /// the words are never cut however narrow the tile.
    #[test]
    fn the_progress_gives_way_before_the_words_are_cut() {
        let t = task("implementing", 2, 4);
        let words = WORDS.len();
        let at = |cols| text(&row(&paint(cols, true), Some(&t), false));
        assert!(
            at(words + 2 + 16 + 1 + 1)
                .trim_end()
                .ends_with("2/4 implementing")
        );
        let short = at(words + 2 + 3 + 1 + 1);
        assert!(
            short.trim_end().ends_with("2/4") && !short.contains("impl"),
            "{short:?}"
        );
        let tight = at(words + 2);
        assert!(tight.contains(WORDS) && !tight.contains("2/4"), "{tight:?}");
        assert_eq!(width::width(&tight), words + 2);
    }

    #[test]
    fn open_cards_are_the_agents_open_ones() {
        let cards = [
            card(1, 7, "open"),
            card(2, 7, "answered"),
            card(3, 8, "open"),
        ];
        let got = open_cards(&cards, 7);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].card, "Q1");
    }
}

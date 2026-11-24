//! The cards qass keeps for the owner, as this client reads them: the open ones and the last few
//! that ended, read with `AskRead` and read again when the daemon says they moved. The Inbox
//! lists them (`inbox_pane`); this holds what was read, and the words that turn a card into a
//! line to talk it through.

use qoop_runtime::control::{AgentRow, AskInfo, ControlRequest, ControlResponse, WorkspaceRow};

use crate::sidebar;
use crate::theme::Role;
use crate::width;

/// The id of the request that reads the cards.
pub const READ_ID: &str = "asks-read";

/// The cards as this client keeps them.
#[derive(Debug, Default)]
pub struct Pane {
    /// The cards waiting for the owner (`open`) or being relayed (`passed`), oldest first.
    pub live: Vec<AskInfo>,
    /// The last few that ended, newest first.
    pub ended: Vec<AskInfo>,
    /// The daemon has answered a read once.
    pub loaded: bool,
    /// The daemon predates the cards.
    pub unsupported: bool,
    /// The read is owed: a change event came, or the first read has not been asked.
    pub stale: bool,
    /// Cards already read again because their terminal left the rows.
    gone: Vec<String>,
}

impl Pane {
    /// Cards not yet read: the first read is owed.
    pub fn new() -> Pane {
        Pane {
            stale: true,
            ..Pane::default()
        }
    }

    /// Take a fresh read.
    pub fn take(&mut self, live: Vec<AskInfo>, ended: Vec<AskInfo>) {
        self.live = live;
        self.ended = ended;
        self.loaded = true;
        self.unsupported = false;
    }
    /// A card whose terminal has left the agent rows (and sleeps in no tab) is read again, once:
    /// the daemon closed or untied it with the terminal and sends no event for that.
    pub fn note_gone(&mut self, rows: &[AgentRow], spaces: &[WorkspaceRow]) {
        // Before the first index there is nobody to compare with.
        if !self.loaded || rows.is_empty() {
            return;
        }
        let here = |t: u32| {
            rows.iter().any(|r| r.terminal == t)
                || spaces
                    .iter()
                    .any(|w| w.sleeping.iter().any(|s| s.terminal == t))
        };
        // A terminal that is back (a wake) may leave again: it is watched afresh.
        let live = &self.live;
        self.gone.retain(|card| {
            !live
                .iter()
                .any(|c| c.card == *card && c.terminal.is_some_and(here))
        });
        for c in &self.live {
            let Some(t) = c.terminal else { continue };
            if !here(t) && !self.gone.contains(&c.card) {
                self.gone.push(c.card.clone());
                self.stale = true;
            }
        }
    }

    /// The read to send, when one is owed and none is in flight.
    pub fn read_request(&mut self) -> Option<ControlRequest> {
        if !self.stale {
            return None;
        }
        self.stale = false;
        Some(ControlRequest::AskRead {
            id: READ_ID.to_string(),
            all: false,
        })
    }
    /// The daemon's answer to the read. True when the screen changed.
    pub fn answered(&mut self, response: &ControlResponse) -> bool {
        match response {
            ControlResponse::Asks { id, live, ended } if id == READ_ID => {
                self.take(live.clone(), ended.clone());
                true
            }
            ControlResponse::Failed { id, reason }
                if id == READ_ID
                    || (id.is_empty()
                        && qoop_runtime::control::too_old_for(reason, Some("AskRead"))) =>
            {
                // Only a daemon that does not know the read has no cards; any other failure is
                // read again, and what was read stays.
                if qoop_runtime::control::too_old_for(reason, Some("AskRead")) {
                    self.unsupported = true;
                    self.loaded = false;
                } else {
                    self.stale = true;
                }
                true
            }
            _ => false,
        }
    }
}

pub(crate) fn age_short(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "now".to_string(),
        s @ 60..=3599 => format!("{}m", s / 60),
        s @ 3600..=259_199 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// An item's plan in the words every screen says it in, and its ink: `plan needs you` once it
/// waited too long for qass, `plan in review` while qass reads it, `plan ok · qass` or `plan ok
/// · you` approved. None without a plan file.
pub fn plan_word(plan: &str, by: Option<&str>) -> Option<(String, Role)> {
    let word = match (plan, by) {
        ("stale" | "asked", _) => "plan needs you",
        ("written" | "changed", _) => "plan in review",
        ("approved", Some("owner")) => "plan ok \u{b7} you",
        ("approved", Some("qass")) => "plan ok \u{b7} qass",
        ("approved", _) => "plan ok",
        _ => return None,
    };
    let role = match plan {
        "stale" | "asked" => Role::Blocked,
        "approved" => Role::Done,
        _ => Role::FaintText,
    };
    Some((word.to_string(), role))
}

/// A plan card: qass asks him to approve its item's plan, which is not approved yet.
pub fn plan_card(card: &AskInfo) -> bool {
    card.body.handle == "plan"
        && card
            .item
            .as_ref()
            .is_some_and(|i| matches!(i.plan.as_str(), "written" | "stale" | "changed" | "asked"))
}

/// The line that asks qass to talk `card` through, his words to follow. It never starts with
/// the handle or `E<n> B`, which qass reads as an answer. The daemon refuses card values with a
/// control character, so it stays one line.
pub fn talk_line(card: &AskInfo) -> String {
    format!(
        "About {} ({}): {} ",
        card.card, card.body.handle, card.body.asks
    )
}

/// Too short or too common to name an agent.
const COMMON: &[&str] = &[
    "about", "also", "been", "claude", "codex", "coding", "could", "from", "have", "into", "just",
    "need", "shall", "should", "some", "than", "that", "their", "them", "then", "there", "these",
    "they", "this", "what", "when", "which", "will", "with", "would", "your",
];

/// `T5` is a tab on this machine, `devbox/T5` a tab on the remote called devbox.
fn parse_ref(token: &str) -> Option<(Option<&str>, u32)> {
    let (remote, tab) = match token.rsplit_once('/') {
        Some((remote, tab)) => (Some(remote), tab),
        None => (None, token),
    };
    let n = tab.strip_prefix('T')?;
    n.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| n.parse().ok())
        .flatten()
        .map(|n| (remote, n))
}

/// The tab of that reference among `rows`: a remote tab is told by its number in its own
/// daemon and by the remote's name, which starts its space's name (`devbox:<space>`).
fn tab_of(
    (remote, n): (Option<&str>, u32),
    rows: &[AgentRow],
    spaces: &[WorkspaceRow],
    qass: Option<i64>,
) -> Option<u32> {
    rows.iter()
        .find(|r| {
            r.agent
                && match remote {
                    None => {
                        !crate::mux::is_remote_terminal(r.terminal)
                            && r.terminal == n
                            && Some(r.workspace) != qass
                    }
                    Some(name) => {
                        crate::mux::is_remote_terminal(r.terminal)
                            && crate::mux::daemon_terminal(r.terminal) == n
                            && spaces
                                .iter()
                                .find(|s| s.id == r.workspace)
                                .is_some_and(|s| {
                                    s.name
                                        .split(':')
                                        .next()
                                        .is_some_and(|p| p.eq_ignore_ascii_case(name))
                                })
                    }
                }
        })
        .map(|r| r.terminal)
}

fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 4 && !COMMON.contains(&w.as_str()) && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

/// The one agent whose name or folder shares a word with `said` (one starts the other, so a
/// handle like `shoppay` finds the `shop` folder). Names and folders are taken whole: a
/// branch-like name must not match on its everyday words, and titles and goals are left out
/// for the same reason. A word that fits more than three agents names none of them; the agent
/// with most shared words wins, then the one active last.
fn named(said: &str, rows: &[&AgentRow]) -> Option<u32> {
    let ids: Vec<(&AgentRow, Vec<String>)> = rows
        .iter()
        .map(|r| {
            let folder = r
                .cwd
                .as_deref()
                .and_then(|c| c.rsplit('/').find(|s| !s.is_empty()));
            let ids = [r.name.as_deref(), folder]
                .into_iter()
                .flatten()
                .map(str::to_lowercase)
                .filter(|x| x.chars().count() >= 4 && !COMMON.contains(&x.as_str()))
                .collect();
            (*r, ids)
        })
        .collect();
    let mut score: Vec<(&AgentRow, usize)> = Vec::new();
    for a in words(said) {
        let fits: Vec<&AgentRow> = ids
            .iter()
            .filter(|(_, w)| {
                w.iter()
                    .any(|b| a.starts_with(b.as_str()) || b.starts_with(&a))
            })
            .map(|(r, _)| *r)
            .collect();
        if fits.len() > 3 {
            continue;
        }
        for r in fits {
            match score.iter_mut().find(|(s, _)| std::ptr::eq(*s, r)) {
                Some(hit) => hit.1 += 1,
                None => score.push((r, 1)),
            }
        }
    }
    // A full tie goes to the first row, as on the phone.
    score
        .into_iter()
        .enumerate()
        .max_by_key(|(i, (r, n))| {
            (
                *n,
                r.last_active_at_unix.unwrap_or(0),
                std::cmp::Reverse(*i),
            )
        })
        .map(|(_, (r, _))| r.terminal)
}

/// The tab Talk it through opens for `card`: the agent it is about, `None` for a card about
/// nothing, which goes to qass. The first hit wins: the card's terminal; its `T5` or
/// `devbox/T5` agent; such a reference in its words; the one agent whose name or folder its
/// words name. The phone's `talkRoute` (mobile/src/ui/asks.ts) follows the same order.
pub fn talk_target(card: &AskInfo, rows: &[AgentRow], spaces: &[WorkspaceRow]) -> Option<u32> {
    let b = &card.body;
    let said = [
        b.handle.as_str(),
        b.asked.as_deref().unwrap_or(""),
        &b.doing,
        &b.why,
        &b.asks,
    ]
    .join(" ");
    let qass = sidebar::qass_space(spaces);
    let workers: Vec<&AgentRow> = rows
        .iter()
        .filter(|r| {
            r.agent && !crate::mux::is_remote_terminal(r.terminal) && Some(r.workspace) != qass
        })
        .collect();
    let by_agent = || {
        card.agent
            .as_deref()
            .and_then(parse_ref)
            .and_then(|r| tab_of(r, rows, spaces, qass))
    };
    let by_task = || {
        let id = card.task?;
        let hit = workers
            .iter()
            .find(|r| r.task.as_ref().is_some_and(|t| t.id == id));
        hit.map(|r| r.terminal)
    };
    let by_words = || {
        let tokens =
            said.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '-' || c == '_'));
        // `T24-worker` holds T24: a token that is no reference is tried by its `-` pieces.
        let mut refs = tokens.flat_map(|t| {
            parse_ref(t)
                .into_iter()
                .chain(t.split('-').filter_map(parse_ref))
        });
        refs.find_map(|r| tab_of(r, rows, spaces, qass))
    };
    // A terminal that is gone (a stale card) is not a tab to open; a draft, or the agent's own
    // question, keeps its tab.
    let own = card.terminal.filter(|t| {
        card.card.starts_with('W')
            || card.card.starts_with('Q')
            || workers.iter().any(|r| r.terminal == *t)
    });
    own.or_else(by_agent)
        .or_else(by_task)
        .or_else(by_words)
        .or_else(|| named(&said, &workers))
}

/// `text` wrapped on words into rows of `cols` columns, at most `max` of them; a word longer than a
/// row breaks on a character, and when text remains the last row ends in `\u{2026}`. With one row
/// it is [`cut_dots`].
fn wrap_dots(text: &str, cols: usize, max: usize) -> Vec<String> {
    if max <= 1 || width::width(text) <= cols {
        return vec![cut_dots(text, cols)];
    }
    let mut rows = vec![String::new()];
    let mut used = 0;
    for word in text.split_whitespace() {
        let chars: Vec<char> = word.chars().filter(|c| width::drawn(*c)).collect();
        let w: usize = chars.iter().map(|c| width::columns(*c)).sum();
        if w == 0 {
            continue;
        }
        if used > 0 && used + 1 + w <= cols {
            if let Some(r) = rows.last_mut() {
                r.push(' ');
            }
            used += 1;
        } else if used > 0 {
            rows.push(String::new());
            used = 0;
        }
        for c in chars {
            let cw = width::columns(c);
            if cw > cols {
                continue;
            }
            if used + cw > cols {
                rows.push(String::new());
                used = 0;
            }
            if let Some(r) = rows.last_mut() {
                r.push(c);
            }
            used += cw;
        }
    }
    if rows.len() > max {
        rows.truncate(max);
        if let Some(last) = rows.last_mut() {
            *last = cut_dots(&format!("{last}\u{2026}"), cols);
        }
    }
    rows
}

/// `text` wrapped on words into rows of `cols` columns, every word kept: what the Inbox
/// draws a card's words with, so none is ever cut.
pub(crate) fn wrap_whole(text: &str, cols: usize) -> Vec<String> {
    wrap_dots(text, cols, usize::MAX)
}

/// `text` cut to `cols` columns with a closing `\u{2026}` when it did not fit, never after a
/// space.
pub(crate) fn cut_dots(text: &str, cols: usize) -> String {
    if width::width(text) <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars().filter(|c| width::drawn(*c)) {
        let w = width::columns(c);
        if used + w + 1 > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    let mut out = out.trim_end().to_string();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use qoop_runtime::control::{AskCard, AskOption};

    fn card(n: u32, handle: &str, status: &str) -> AskInfo {
        AskInfo {
            card: format!("E{n}"),
            agent: Some(format!("T{n}")),
            terminal: Some(n),
            status: status.to_string(),
            note: None,
            opened_at: 1_000 - 360,
            closed_at: None,
            answer: None,
            option: None,
            task: None,
            item: None,
            body: AskCard {
                handle: handle.to_string(),
                urgency: "later".to_string(),
                asked: None,
                doing: "wrote migration 0042".to_string(),
                why: "the PR cannot open before 0042 has run once on staging".to_string(),
                asks: "Run 0042 on staging now?".to_string(),
                options: vec![AskOption {
                    key: "a".into(),
                    label: "run now".into(),
                    reason: "staging is a copy".into(),
                    words: "Run 0042 on staging now.".into(),
                }],
                pick: None,
            },
        }
    }

    fn agent_row(terminal: u32) -> AgentRow {
        AgentRow {
            command: None,
            clients: Vec::new(),
            menu: None,
            typed_by: None,
            parked: false,
            keep_awake: false,
            qass_skip: false,
            background: 0,
            subagents: 0,
            task: None,
            news: false,
            terminal,
            name: None,
            workspace: 1,
            agent: true,
            program: Some("claude".to_string()),
            behind: None,
            state: "idle".to_string(),
            generation: None,
            readiness: "ready".to_string(),
            source: None,
            rule: None,
            manifest: None,
            agent_session: None,
            agent_title: None,
            last_active_at_unix: None,
            stop: None,
            cwd: None,
            slept_at_unix: None,
            activity: None,
            pgid: None,
            shell_pid: None,
            discards: None,
            status: None,
            answered: None,
            with_qass: false,
            qass_missed: false,
            qass_away: None,
            menu_seal: None,
        }
    }

    fn row_at(terminal: u32, workspace: i64, name: Option<&str>, cwd: Option<&str>) -> AgentRow {
        AgentRow {
            workspace,
            name: name.map(str::to_string),
            cwd: cwd.map(str::to_string),
            ..agent_row(terminal)
        }
    }

    fn space_row(id: i64, name: &str) -> WorkspaceRow {
        WorkspaceRow {
            dir: None,
            keep_awake: false,
            qass_skip: false,
            id,
            name: name.to_string(),
            terminals: 1,
            asleep: false,
            sleeping: Vec::new(),
            closable: true,
            ink: None,
            link: None,
            link_reason: None,
            goal: None,
        }
    }

    /// Talk it through goes to the agent the card is about, in the phone's order: its terminal,
    /// its `T5` or `devbox/T5` agent, such a reference in its words, the agent its words name.
    #[test]
    fn talk_it_through_goes_to_the_agent_the_card_is_about() {
        let mut c = card(1, "websearch", "open");
        c.agent = None;
        c.terminal = None;
        let qass_space = 9;
        let spaces = vec![
            space_row(qass_space, sidebar::QASS_SPACE),
            space_row((1 << 40) | 2, "devbox:main"),
        ];
        let remote = crate::mux::REMOTE_TERMINAL | (1 << 24) | 25;
        let mut rows = vec![
            row_at(3, qass_space, Some("qass"), None),
            row_at(8, 1, None, Some("/Users/pat/coding/blog")),
            row_at(639, 1, None, Some("/Users/pat/coding/shop")),
            row_at(644, 1, None, Some("/Users/pat/coding/shop")),
            row_at(remote, (1 << 40) | 2, None, None),
        ];
        rows[2].last_active_at_unix = Some(100);
        rows[3].last_active_at_unix = Some(200);
        // About nothing: qass.
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // Its terminal, whatever the card's number says (E600 and T639 live).
        c.terminal = Some(639);
        assert_eq!(talk_target(&c, &rows, &spaces), Some(639));
        c.terminal = None;
        // A remote agent, told by its remote's name and its number there (E581 live).
        c.agent = Some("devbox/T25".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), Some(remote));
        c.agent = Some("build1/T25".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // A terminal that is gone is no tab: the card's agent, else qass.
        c.terminal = Some(77);
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        c.card = "W1".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(77));
        c.card = "E1".to_string();
        c.terminal = None;
        // qass's own tab is no agent a card names.
        c.agent = Some("T3".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        // A reference in its words.
        c.agent = None;
        c.body.doing = "T8 is waiting on the build".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        c.body.doing = "T8-worker is stuck".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        c.body.doing = "wrote migration 0042".to_string();
        // E601: nothing but "Shop" in its words and two agents in the shop folder, the last active wins.
        c.body.handle = "shoppay".to_string();
        c.body.asks = "Stripe account and mail sender for Shop?".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), Some(644));
        // The card's task is an agent's task.
        c.body.asks = "Which one?".to_string();
        c.body.handle = "x".to_string();
        assert_eq!(talk_target(&c, &rows, &spaces), None);
        c.task = Some(510);
        rows[1].task = Some(qoop_runtime::control::TaskRef {
            id: 510,
            goal: "g".to_string(),
            stage: "implementing".to_string(),
            claimed: false,
            item: None,
            steps_done: 0,
            steps_total: 0,
        });
        assert_eq!(talk_target(&c, &rows, &spaces), Some(8));
        // qass is never the agent a card names, and a branch-like name matches on no everyday word.
        c.task = None;
        c.body.asks = "Ask qass to wake it, in his words?".to_string();
        rows[1].name = Some("fix/draft-card-owner-words".to_string());
        assert_eq!(talk_target(&c, &rows, &spaces), None);
    }

    #[test]
    fn wrap_dots_wraps_on_words_and_ends_in_dots_only_when_text_is_left() {
        assert_eq!(wrap_dots("ab cd ef", 5, 3), ["ab cd", "ef"]);
        assert_eq!(wrap_dots("ab cd", 5, 3), ["ab cd"]);
        assert_eq!(wrap_dots("abcdefgh ij", 4, 5), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_dots("ab cd ef gh ij", 5, 2), ["ab cd", "ef g\u{2026}"]);
        assert_eq!(wrap_dots("ab cd ef gh", 5, 1), [cut_dots("ab cd ef gh", 5)]);
        assert_eq!(
            wrap_dots("\u{4e2d}\u{4e2d}\u{4e2d}", 4, 3),
            ["\u{4e2d}\u{4e2d}", "\u{4e2d}"]
        );
        assert_eq!(
            wrap_dots("\u{4e2d}x", 1, 3),
            ["x"],
            "a wide char that never fits is dropped"
        );
        assert_eq!(wrap_dots("", 5, 3), [""]);
    }

    #[test]
    fn ages_and_plans_are_said_in_the_words_every_screen_uses() {
        assert_eq!(age_short(30), "now");
        assert_eq!(age_short(26 * 3600), "26h");
        assert_eq!(age_short(80 * 3600), "3d");
        assert_eq!(plan_word("", None), None);
        assert_eq!(
            plan_word("approved", Some("owner")).map(|w| w.0),
            Some("plan ok \u{b7} you".to_string())
        );
        assert!(!plan_card(&card(1, "migrations", "open")));
    }

    /// A card whose terminal left the rows is read again, once.
    #[test]
    fn a_card_whose_terminal_is_gone_is_read_again_once() {
        let mut p = Pane::new();
        p.take(vec![card(5, "x", "open")], Vec::new());
        p.stale = false;
        let rows = vec![agent_row(1)];
        p.note_gone(&rows, &[]);
        assert!(p.read_request().is_some(), "read again");
        p.note_gone(&rows, &[]);
        assert!(p.read_request().is_none(), "once");
    }

    /// A read that fails for any other reason is asked again, and the daemon still keeps cards:
    /// what was read stays.
    #[test]
    fn a_read_that_fails_is_read_again_not_taken_for_an_old_daemon() {
        let mut p = Pane::new();
        assert!(p.read_request().is_some());
        p.take(Vec::new(), Vec::new());
        assert!(p.read_request().is_none());
        assert!(p.answered(&ControlResponse::Failed {
            id: READ_ID.into(),
            reason: "the store did not answer in time".into(),
        }));
        assert!(!p.unsupported && p.loaded);
        assert!(p.read_request().is_some(), "read again");
    }
}

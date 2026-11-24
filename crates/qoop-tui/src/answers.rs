//! How a question is answered, as plain values and functions with no window and no screen: the
//! offers a menu makes, the pick that waits out its hold, and whether an agent's question is
//! with qass now. The desktop and the terminal interface read the same rules from here.
//!
//! This is a copy of the desktop's answer logic (`qoop-desktop/src/state.rs`), made pure: what
//! the desktop's `Model` keeps about qass (its window's bookkeeping) is the explicit
//! [`HandInput`]. The desktop keeps its own code for now and moves onto this module in its own
//! round.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use qoop_domain::shell::quote as shell_quote;
use qoop_domain::status::{self, Status};
use qoop_runtime::control::{AgentMenu, AgentRow, AskInfo};
use serde::Deserialize;

use crate::mux::{daemon_terminal, machine_of};

/// What a row shows: the daemon's status, or a sleeping tab (a tab fact, not a status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Status(Status),
    Sleeping,
}

impl Kind {
    pub fn of(row: &AgentRow) -> Kind {
        if row.state == status::SLEEPING || row.slept_at_unix.is_some() {
            return Kind::Sleeping;
        }
        // An older daemon sends no status: shown as unsure rather than worked out here.
        Kind::Status(
            row.status
                .as_deref()
                .and_then(Status::from_token)
                .unwrap_or(Status::Unknown),
        )
    }

    pub fn needs_you(self) -> bool {
        self == Kind::Status(Status::Blocked)
    }
}

/// What the window types for "Explain this in more detail", pinned to the daemon's copy by a
/// test (`qoop_runtime::menu::EXPLAIN`).
pub const EXPLAIN: &str = "Explain this in more detail before I decide: what it will do, why, and \
                           what it changes. Then ask me again.";

/// How long a pick waits before it is sent, for Undo.
pub const HOLD: Duration = Duration::from_secs(3);

/// A menu younger than this takes no digit: a key meant for the screen before it.
pub const ARMING: Duration = Duration::from_millis(500);

/// What a pick sends once its hold is up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// One of the agent's own options, by its key (`AnswerMenu`).
    Key(String),
    /// Words typed through the option `key`, a `tell` or `input` one (`AnswerMenuText`):
    /// Explain, or the person's own answer.
    Text { key: String, text: String },
    /// The question handed to qass (`HandToQass`).
    Qass,
    /// An option of one of qass's cards (`AskWrite` `Answer`), which qass types to the agent.
    Card { card: String, option: String },
    /// The owner's own words for a card (`AskWrite` `Answer` with text).
    CardText { card: String, text: String },
    /// A card handed to qass to decide (`AskWrite` `Answer` with `to_qass`).
    CardQass { card: String },
    /// A card put away unanswered (`AskWrite` `Dismiss`): held like an answer, so it can be
    /// undone.
    PutAway { card: String },
    /// The person's words for an agent that ended its turn waiting on them (no menu, so no
    /// seal): typed into its terminal. Keyed by the agent's turn, its `AgentRow::stop`, which
    /// changes when it moves on.
    Reply { text: String, turn: Option<String> },
}

/// What a pick is about: an agent's menu, or one of qass's cards.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Agent(u32),
    Card(String),
}

/// One answer the question card offers: its digit (none for a standing permission, which is
/// click only), its words and what it sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub digit: Option<u8>,
    pub label: String,
    pub detail: Option<String>,
    pub pick: Pick,
}

/// Whether an option grants a standing permission: Claude Code's "Yes, and always allow
/// access to src/" and "Yes, and don't ask again for cargo test commands in /repo", Codex's
/// "Yes, and don't ask again for this command". Click only: no digit to press by habit.
pub fn standing(label: &str) -> bool {
    let words = label.to_lowercase().replace('\u{2019}', "'");
    ["always", "don't ask again", "do not ask again"]
        .iter()
        .any(|w| words.contains(w))
}

/// The card's answers: the agent's own options (its field for an answer of one's own is the
/// card's text field, not an option), then "Explain this in more detail" when the menu takes
/// words, then "Let qass handle it"; digits in that order, skipping standing permissions. A
/// multiple choice menu offers nothing here: the card says the question and Open.
pub fn offers(menu: &AgentMenu) -> Vec<Offer> {
    if menu.multi {
        return Vec::new();
    }
    let mut out: Vec<Offer> = menu
        .options
        .iter()
        .filter(|o| !o.input)
        .map(|o| Offer {
            digit: None,
            label: o.label.clone(),
            detail: o.detail.clone(),
            pick: Pick::Key(o.key.clone()),
        })
        .collect();
    if let Some(key) = text_key(menu) {
        out.push(Offer {
            digit: None,
            label: "Explain this in more detail".into(),
            detail: None,
            pick: Pick::Text {
                key,
                text: EXPLAIN.into(),
            },
        });
    }
    out.push(Offer {
        digit: None,
        label: "Let qass handle it".into(),
        detail: None,
        pick: Pick::Qass,
    });
    let mut next = 1u8;
    for offer in &mut out {
        if !standing(&offer.label) && next <= 9 {
            offer.digit = Some(next);
            next += 1;
        }
    }
    out
}

/// The option words go through: the agent's own answer field, else its "tell it what to do
/// differently"; `None` when the menu takes no words.
pub fn text_key(menu: &AgentMenu) -> Option<String> {
    menu.options
        .iter()
        .find(|o| o.input)
        .or_else(|| menu.options.iter().find(|o| o.tell))
        .map(|o| o.key.clone())
}

/// A pick waiting out its hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub slot: Slot,
    pub generation: Option<u64>,
    pub seal: u64,
    pub pick: Pick,
    /// What the card says it sends: "Yes".
    pub label: String,
    pub until: Instant,
}

impl Hold {
    /// The agent's terminal, for a pick on its menu.
    pub fn terminal(&self) -> Option<u32> {
        match self.slot {
            Slot::Agent(t) => Some(t),
            Slot::Card(_) => None,
        }
    }
}

/// The picks on hold, one per agent or card, oldest first, and when each menu was first seen
/// (for the arming).
#[derive(Clone, Debug, Default)]
pub struct Holds {
    pub held: Vec<Hold>,
    seen: HashMap<u32, (u64, Instant)>,
}

impl Holds {
    /// The menus on the rows now: a new one starts its arming; an answer whose menu changed or
    /// went is dropped and returned, so the card can say who answered. A reply is dropped the
    /// same way once its agent has moved on: another turn, another process, or no longer
    /// waiting on the person.
    pub fn menus(&mut self, rows: &[AgentRow], now: Instant) -> Vec<Hold> {
        let shown: HashMap<u32, u64> = rows
            .iter()
            .filter_map(|r| Some((r.terminal, r.menu.as_ref()?.seal)))
            .collect();
        let waiting: HashMap<u32, (Option<u64>, Option<String>)> = rows
            .iter()
            .filter(|r| Kind::of(r).needs_you() && r.menu.is_none())
            .map(|r| (r.terminal, (r.generation, r.stop.clone())))
            .collect();
        self.seen.retain(|t, (seal, _)| shown.get(t) == Some(seal));
        for (&t, &seal) in &shown {
            self.seen.entry(t).or_insert((seal, now));
        }
        let (kept, dropped): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| match (&h.pick, h.terminal()) {
                (Pick::Key(_) | Pick::Text { .. }, Some(t)) => shown.get(&t) == Some(&h.seal),
                (Pick::Reply { turn, .. }, Some(t)) => {
                    waiting.get(&t) == Some(&(h.generation, turn.clone()))
                }
                _ => true,
            });
        self.held = kept;
        dropped
    }

    /// Whether the menu `seal` of `terminal` takes a digit yet.
    pub fn armed(&self, terminal: u32, seal: u64, now: Instant) -> bool {
        self.seen
            .get(&terminal)
            .is_some_and(|&(s, at)| s == seal && now.duration_since(at) >= ARMING)
    }

    /// Hold `hold`, in place of any other pick for the same agent or card.
    pub fn start(&mut self, hold: Hold) {
        self.held.retain(|h| h.slot != hold.slot);
        self.held.push(hold);
    }

    /// Send now: take `slot`'s pick off its hold to be sent at once, never again by
    /// [`Holds::due`]; with none, the newest pick and every pick held with it (one shared
    /// Undo or Send now for a hand-off of several).
    pub fn send_now(&mut self, slot: Option<&Slot>) -> Vec<Hold> {
        let newest = self.held.last().map(|h| h.until);
        let (taken, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| match slot {
                Some(slot) => &h.slot == slot,
                None => Some(h.until) == newest,
            });
        self.held = kept;
        taken
    }

    /// Undo `slot`'s pick; with none, the newest pick, and every pick held with it (one
    /// shared Undo for a hand-off of several). Whether anything was on hold.
    pub fn undo(&mut self, slot: Option<&Slot>) -> bool {
        !self.send_now(slot).is_empty()
    }

    /// Undo the hand-off to qass: every agent held to be handed over, nothing else.
    pub fn undo_hand_off(&mut self) -> bool {
        let before = self.held.len();
        self.held.retain(|h| h.pick != Pick::Qass);
        self.held.len() != before
    }

    /// The picks whose hold is up, taken out to be sent.
    pub fn due(&mut self, now: Instant) -> Vec<Hold> {
        let (due, kept): (Vec<Hold>, Vec<Hold>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|h| h.until <= now);
        self.held = kept;
        due
    }

    /// Every pick on hold, to be sent now: the window is closing.
    pub fn flush(&mut self) -> Vec<Hold> {
        std::mem::take(&mut self.held)
    }

    pub fn of(&self, slot: &Slot) -> Option<&Hold> {
        self.held.iter().find(|h| &h.slot == slot)
    }
}

/// What a window knows about qass and the hands to it, passed in whole: everything
/// [`handed`] reads that is not on the row. The desktop's `Model` keeps these as fields; a
/// client that has no such model fills one in and calls [`note_qass`] after each new agent list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HandInput {
    /// This Mac's qass, when it runs: its tab and its generation (a restart changes it).
    /// Set from the rows before [`note_qass`].
    pub qass: Option<(u32, Option<u64>)>,
    /// qass's switch as `QassWatch` read it; `None` before the first answer.
    pub watching: Option<bool>,
    /// Machines whose qoop refused a hand for This Mac's qass as too old.
    pub no_hands: BTreeSet<usize>,
    /// Since when (this window's clock) This Mac's qass has run and watched, and which agent
    /// that is: [`note_qass`] keeps it.
    pub up: Option<(i64, (u32, Option<u64>))>,
    /// Each row's hand for another machine's qass as last seen (`qass_away.at`), and when this
    /// window first saw it: `None` for one already there in its machine's first list.
    pub away_seen: HashMap<u32, (i64, Option<i64>)>,
    /// The machines the last agent list had rows of.
    pub listed_machines: BTreeSet<usize>,
    /// The agents qass's open cards are about, as their keys (`T5`, `staging/T2`): see
    /// [`HandInput::set_carded`].
    pub carded: Vec<String>,
    /// This Mac's hand id (`paths::hand_id`), once known.
    pub hand_id: Option<String>,
    /// Each other machine's name as a person gives it, by its number: what keys a remote
    /// agent (`staging/T2`).
    pub machines: BTreeMap<usize, String>,
}

impl HandInput {
    /// Why This Mac's qass takes nothing now: it is not running, or not watching.
    pub fn qass_off(&self) -> Option<&'static str> {
        if self.qass.is_none() {
            Some("qass is not running")
        } else if self.watching == Some(false) {
            Some("qass is not watching")
        } else {
            None
        }
    }

    /// The agent as qass's cards key it: `T5` on This Mac, `staging/T2` on another machine.
    /// `None` for a machine whose name is not known: no key, so no false match with `T2`.
    pub fn agent_key(&self, row: &AgentRow) -> Option<String> {
        let n = daemon_terminal(row.terminal);
        match machine_of(row.workspace) {
            0 => Some(format!("T{n}")),
            m => Some(format!("{}/T{n}", self.machines.get(&m)?)),
        }
    }

    /// Take the agents of the open cards (`status` open) as `carded`.
    pub fn set_carded(&mut self, cards: &[AskInfo]) {
        self.carded = cards
            .iter()
            .filter(|a| a.status == "open")
            .filter_map(|a| a.agent.as_deref())
            .filter_map(qoop_runtime::asks::agent_key)
            .map(|(key, _)| key)
            .collect();
    }
}

/// Whether "hand to qass" can be pressed for `row`: qass does not leave the agent alone, runs
/// and watches, and the machine's qoop is not too old for it. One rule for every place that
/// offers it: This Mac's qass takes every machine's questions.
pub fn can_hand(row: &AgentRow, q: &HandInput) -> bool {
    let m = machine_of(row.workspace);
    !row.qass_skip && q.qass_off().is_none() && !(m != 0 && q.no_hands.contains(&m))
}

/// Whether `row`'s question waits on qass now, not on the owner. A hand to qass while it can
/// take it; a hand from this Mac for another machine's agent only while This Mac's qass has
/// run and watched since the window first saw that hand (a restart never heard of it), and no
/// card of qass's is about that agent (then the card is what waits on the owner).
pub fn handed(row: &AgentRow, q: &HandInput) -> bool {
    if !row.with_qass || !can_hand(row, q) {
        return false;
    }
    let Some(away) = &row.qass_away else {
        return true;
    };
    // Another Mac's hand: its qass was told, as far as this window can know.
    if q.hand_id.as_ref().is_some_and(|me| *me != away.by) {
        return true;
    }
    let seen = q
        .away_seen
        .get(&row.terminal)
        .filter(|(at, _)| *at == away.at)
        .and_then(|(_, when)| *when);
    let up = q.up.map(|(since, _)| since);
    matches!((up, seen), (Some(up), Some(seen)) if up <= seen)
        && !q.agent_key(row).is_some_and(|key| q.carded.contains(&key))
}

/// After a new agent list (`rows`, every machine's) read at `now` (the window's clock): since
/// when This Mac's qass runs and watches, and when each hand for another machine's qass was
/// first seen. Set `q.qass` and `q.watching` from the list and the switch first.
pub fn note_qass(q: &mut HandInput, rows: &[AgentRow], now: i64) {
    let up = q.qass_off().is_none();
    q.up = match (up, q.qass, q.up) {
        (true, Some(who), Some((since, was))) if was == who => Some((since, who)),
        (true, Some(who), _) => Some((now, who)),
        _ => None,
    };
    let mut seen = HashMap::new();
    for r in rows {
        if let Some(away) = &r.qass_away {
            // A machine's first list in this window (a remote's comes after the window
            // opens) holds hands older than anything the window knows.
            let first = !q.listed_machines.contains(&machine_of(r.workspace));
            let when = match q.away_seen.get(&r.terminal) {
                Some((at, when)) if *at == away.at => *when,
                _ if first => None,
                _ => Some(now),
            };
            seen.insert(r.terminal, (away.at, when));
        }
    }
    q.away_seen = seen;
    q.listed_machines = rows.iter().map(|r| machine_of(r.workspace)).collect();
}

/// How a refusal of `pick` reads as a sentence: the daemon's code never reaches the screen.
/// `machine` is the computer the agent runs on as a person names it ("This Mac", "staging").
pub fn refusal_words(reason: &str, machine: &str, pick: &Pick) -> String {
    let here = machine == "This Mac";
    let max = qoop_runtime::menu::TEXT_MAX;
    match reason {
        "already_answered" => "Someone already answered this question.".into(),
        "busy" => "The agent was still taking keys. Try again in a moment.".into(),
        "no_such_answer" => "That answer is no longer on the agent's screen.".into(),
        "not_at_input" => "The agent did not open its answer field, so nothing was typed.".into(),
        "no_text_field" => "This question takes no typed answer.".into(),
        "empty" => "There were no words to send.".into(),
        "too_long" => format!("The answer is too long. Keep it under {max} characters."),
        "not_a_person" => {
            "Only a person's window can answer, and this one was not taken as one.".into()
        }
        "qass_not_running" if here => "qass is not running, so it cannot take this.".into(),
        "qass_not_running" => format!("qass is not running on {machine}, so it cannot take this."),
        "not_watching" => "qass is not watching, so it cannot take this.".into(),
        "skipped" => "qass leaves this agent alone, so it cannot take this.".into(),
        r if crate::mux::refused_as_old(r) => match pick {
            Pick::Qass | Pick::CardQass { .. } => {
                format!("qoop on {machine} is too old to hand questions to qass.")
            }
            Pick::Text { .. } => format!("qoop on {machine} is too old for typed answers."),
            _ => format!("qoop on {machine} is too old for this answer."),
        },
        other => other.to_string(),
    }
}

/// qass's rules as its lessons file counts them: the lessons that keep a question the owner's
/// (`escalate`, `ask first`), the ones it answers alone (`do it`), and every lesson. A lesson
/// is a line `- <when> -> <action> (<source>)`, as `skills/qass/SKILL.md` writes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lessons {
    pub yours: usize,
    pub alone: usize,
    pub all: usize,
}

pub fn lessons_of(text: &str) -> Lessons {
    let mut out = Lessons::default();
    for line in text.lines() {
        let Some(entry) = line.trim_start().strip_prefix("- ") else {
            continue;
        };
        let Some((_, action)) = entry.rsplit_once(" -> ") else {
            continue;
        };
        let action = action
            .split(" (")
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        out.all += 1;
        match action.as_str() {
            "escalate" | "ask first" => out.yours += 1,
            "do it" => out.alone += 1,
            _ => {}
        }
    }
    out
}

// New work: the line typed into a new shell in the space's folder, ported from the phone's
// `worktree.ts` so both start an agent the same way and the CLI does every check.

/// The longest first prompt typed this way.
pub const PROMPT_MOST: usize = 4000;

/// The longest line typed. A new shell may still be starting when it arrives, with the
/// terminal in line mode, and macOS keeps about 1023 bytes of an unfinished line and drops
/// the rest: measured, 929 bytes typed a second before zsh read them ran, 1229 lost the end
/// (the quote, `&& exit`, the Enter) and ran nothing.
pub const LINE_MOST: usize = 1000;

/// The prompt as typed: control characters to spaces, trimmed, cut at [`PROMPT_MOST`].
pub fn clean_prompt(prompt: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in prompt.chars() {
        if c.is_control() {
            gap = true;
        } else {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        }
    }
    out.trim()
        .chars()
        .take(PROMPT_MOST)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A branch named after the prompt's first words: `feat/fix-the-login-page`. Letters with
/// marks keep their base letter, so `giriş sayfasını düzelt` is `feat/giris-sayfasini-duzelt`.
/// Empty when no Latin letter or digit is left (a task in Japanese): see [`branch_or_clock`].
pub fn branch_for(prompt: &str) -> String {
    let plain: String = prompt
        .chars()
        .filter(|c| *c != '\'' && *c != '\u{2019}')
        // Lowercase first, so the table needs only lowercase letters; `İ` alone, whose
        // lowercase adds a combining dot.
        .map(|c| if c == 'İ' { 'i' } else { c })
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'ç' => 'c',
            'ğ' => 'g',
            'ı' | 'í' | 'ì' | 'î' | 'ï' => 'i',
            'ö' | 'ó' | 'ò' | 'ô' | 'õ' => 'o',
            'ş' | 'ß' => 's',
            'ü' | 'ú' | 'ù' | 'û' => 'u',
            'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ñ' => 'n',
            c => c,
        })
        .collect();
    let words: Vec<&str> = plain
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(5)
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        format!("feat/{}", words.join("-"))
    }
}

/// The task's branch, or `feat/work-<at>` (unix seconds) when its words give none.
pub fn branch_or_clock(prompt: &str, at: i64) -> String {
    let branch = branch_for(prompt);
    if branch.is_empty() {
        format!("feat/work-{at}")
    } else {
        branch
    }
}

/// What is wrong with a branch name the CLI would take as something else.
pub fn branch_problem(branch: &str) -> Option<&'static str> {
    if branch.is_empty() {
        Some("Name the branch.")
    } else if branch.starts_with('-') {
        Some("A branch cannot start with -.")
    } else if branch.contains("@{") {
        Some("A branch cannot hold @{.")
    } else if branch.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("A branch has no spaces.")
    } else {
        None
    }
}

fn program_of(command: &str) -> &str {
    // Past any `NAME=value` in front, as the TUI's schedule form reads a launcher.
    let first = command
        .split_whitespace()
        .find(|w| !w.contains('=') && *w != "env")
        .unwrap_or("");
    first.rsplit('/').next().unwrap_or("")
}

/// A launcher typed as it is: plain words, nothing the shell reads as more than words.
fn plain(command: &str) -> bool {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c);
    let trimmed = command.trim();
    !trimmed.is_empty()
        && !trimmed.contains("  ")
        && trimmed
            .split(' ')
            .all(|w| !w.is_empty() && w.chars().all(safe))
}

/// How an agent takes its first prompt on its command line.
fn prompted(program: &str, prompt: &str) -> Option<Vec<String>> {
    match program {
        "claude" | "codex" => Some(vec![prompt.to_string()]),
        "gemini" => Some(vec!["-i".into(), prompt.to_string()]),
        "opencode" => Some(vec!["--prompt".into(), prompt.to_string()]),
        _ => None,
    }
}

/// The launchers that can start in a worktree, by name and command: every one typed as plain
/// words, since `&& exit` follows it on the line.
pub fn worktree_agents(offers: &[(String, String)]) -> Vec<(String, String)> {
    offers
        .iter()
        .filter(|(_, command)| plain(command))
        .cloned()
        .collect()
}

/// Whether the agent takes the task as its first prompt; one qoop does not know starts
/// without it, and the task only names the branch.
pub fn takes_prompt(command: &str) -> bool {
    prompted(program_of(command), "").is_some()
}

/// The agent's command with its first prompt on it when it takes one, or why it cannot start.
pub fn prompted_line(command: &str, prompt: &str) -> Result<String, &'static str> {
    let said = clean_prompt(prompt);
    if said.is_empty() {
        return Err("Say what the agent should do.");
    }
    if said.starts_with('-') {
        return Err("The prompt cannot start with -.");
    }
    if !plain(command) {
        return Err("That agent cannot start this way.");
    }
    let mut line = vec![command.trim().to_string()];
    if let Some(words) = prompted(program_of(command), &said) {
        line.extend(words.iter().map(|w| shell_quote(w)));
    }
    Ok(line.join(" "))
}

/// The line typed into the new shell, or why there is none. `space` is the space's number in
/// the daemon of the machine the shell runs on, as the CLI there reads it.
pub fn worktree_line(
    branch: &str,
    space: Option<i64>,
    command: &str,
    prompt: &str,
) -> Result<String, &'static str> {
    if let Some(problem) = branch_problem(branch) {
        return Err(problem);
    }
    let agent = prompted_line(command, prompt)?;
    let space = space.map_or_else(String::new, |s| format!(" --space {s}"));
    let line = format!(
        "qoop worktree new {}{space} -- {agent} && exit",
        shell_quote(branch)
    );
    if line.len() >= LINE_MOST {
        return Err("The task is too long to type: shorten it.");
    }
    Ok(line)
}

/// A space's number in its own machine's daemon: the mux numbers a remote one above 2^40.
pub fn daemon_space(workspace: i64) -> i64 {
    workspace & ((1 << 40) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(terminal: u32, workspace: i64, status: &str) -> AgentRow {
        let mut row: AgentRow = serde_json::from_value(serde_json::json!({
            "terminal": terminal,
            "workspace": workspace,
            "program": "claude",
            "behind": null,
            "state": status,
            "generation": 1,
            "agent": true,
        }))
        .expect("row");
        row.status = Some(status.to_string());
        row
    }

    /// Claude Code's edit question: Yes, an "always" option, No and tell it, and its field.
    fn edit_menu(seal: u64) -> AgentMenu {
        serde_json::from_value(serde_json::json!({
            "question": "Do you want to make this edit to README.md?",
            "seal": seal,
            "options": [
                {"key": "1", "label": "Yes"},
                {"key": "2", "label": "Yes, allow all edits during this session (shift+tab)"},
                {"key": "3", "label": "Yes, and always allow access to src/ from this project"},
                {"key": "4", "label": "No, and tell Claude what to do differently (esc)", "tell": true},
            ],
        }))
        .expect("menu")
    }

    #[test]
    fn branch_comes_from_the_first_words_turkish_included() {
        assert_eq!(
            branch_for("giriş sayfasını düzelt"),
            "feat/giris-sayfasini-duzelt"
        );
        assert_eq!(branch_for("İstanbul ÇĞÖŞÜ ı"), "feat/istanbul-cgosu-i");
        assert_eq!(
            branch_for("Fix the login page, it 500s on Safari"),
            "feat/fix-the-login-page-it"
        );
        assert_eq!(
            branch_for("Don't break it's login"),
            "feat/dont-break-its-login"
        );
        assert_eq!(branch_for("Don\u{2019}t stop"), "feat/dont-stop");
        assert_eq!(branch_for("!!!"), "");
        assert_eq!(
            branch_for("\u{c9}lan \u{d1}and\u{fa} \u{c1}LTO"),
            "feat/elan-nandu-alto"
        );
        let japanese = "\u{30ed}\u{30b0}\u{30a4}\u{30f3}\u{753b}\u{9762}\u{3092}\u{76f4}\u{3059}";
        assert_eq!(branch_for(japanese), "", "no Latin word left");
        assert_eq!(
            branch_or_clock(japanese, 1_763_100_000),
            "feat/work-1763100000"
        );
        assert_eq!(branch_or_clock("fix it", 1), "feat/fix-it");
        assert!(
            worktree_line(&branch_or_clock(japanese, 1), Some(1), "claude", japanese).is_ok(),
            "a task with no Latin letter still starts"
        );
        assert_eq!(branch_problem("feat/x"), None);
        assert_eq!(branch_problem("-b"), Some("A branch cannot start with -."));
        assert_eq!(branch_problem("a@{1}"), Some("A branch cannot hold @{."));
        assert_eq!(branch_problem("a b"), Some("A branch has no spaces."));
        assert_eq!(branch_problem("feat/a\nb"), Some("A branch has no spaces."));
        assert_eq!(branch_problem(""), Some("Name the branch."));
    }

    /// What a POSIX shell makes of a typed line: its words.
    fn words(line: &str) -> Vec<String> {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s\\0' {line}"))
            .output()
            .expect("sh");
        let text = String::from_utf8(out.stdout).expect("utf8");
        let mut words: Vec<String> = text.split('\0').map(str::to_string).collect();
        words.pop();
        words
    }

    /// A backslash before a quote ended the quote early in fish and ran the rest; it is now
    /// typed outside the quotes. Every shell's reading is checked in `qoop_domain::shell`.
    #[test]
    fn a_backslash_in_the_task_stays_one_word_in_every_shell() {
        let task = r"a\'; touch /tmp/pwn; #";
        let line = worktree_line("b", None, "claude", task).expect("line");
        assert_eq!(
            line,
            r"qoop worktree new b -- claude 'a'\\\''; touch /tmp/pwn; #' && exit"
        );
        assert_eq!(
            words(line.trim_end_matches(" && exit"))
                .last()
                .map(String::as_str),
            Some(task)
        );
    }

    /// A line the terminal would cut before the shell reads it is refused, not typed.
    #[test]
    fn a_line_too_long_to_type_is_refused() {
        let most = LINE_MOST - "qoop worktree new b -- claude  && exit".len();
        assert!(worktree_line("b", None, "claude", &"x".repeat(most - 1)).is_ok());
        assert_eq!(
            worktree_line("b", None, "claude", &"x".repeat(most)),
            Err("The task is too long to type: shorten it.")
        );
    }

    #[test]
    fn the_prompt_loses_control_characters_and_is_cut() {
        assert_eq!(
            clean_prompt(" fix\nthe\r\nlogin\x1b[2J page\t"),
            "fix the login [2J page"
        );
        assert_eq!(clean_prompt(&"é".repeat(5000)).chars().count(), PROMPT_MOST);
    }

    #[test]
    fn the_typed_line_starts_the_agent_in_a_worktree() {
        let prompt = "fix it's \"quoted\"\n$HOME `id` now";
        let line = worktree_line("feat/fix", Some(3), "claude", prompt).expect("line");
        assert_eq!(
            line,
            "qoop worktree new feat/fix --space 3 -- claude 'fix it'\\''s \"quoted\" $HOME `id` now' && exit"
        );
        assert_eq!(
            words(line.trim_end_matches(" && exit")),
            [
                "qoop",
                "worktree",
                "new",
                "feat/fix",
                "--space",
                "3",
                "--",
                "claude",
                "fix it's \"quoted\" $HOME `id` now"
            ]
        );
        assert_eq!(
            worktree_line("b", None, "opencode", "go").as_deref(),
            Ok("qoop worktree new b -- opencode --prompt go && exit")
        );
        assert_eq!(
            worktree_line("-x", None, "claude", "go"),
            Err("A branch cannot start with -.")
        );
        assert_eq!(
            worktree_line("b", None, "claude", "--help"),
            Err("The prompt cannot start with -.")
        );
        assert_eq!(
            worktree_line("b", None, "claude", " \n "),
            Err("Say what the agent should do.")
        );
        assert_eq!(
            worktree_line("b", None, "claude; rm -rf ~", "go"),
            Err("That agent cannot start this way.")
        );
        let offered = worktree_agents(&[
            ("claude".into(), "claude".into()),
            ("fast".into(), "claude --model opus".into()),
            ("cx".into(), "/opt/homebrew/bin/codex".into()),
            ("env".into(), "FOO=1 claude".into()),
            ("piped".into(), "claude | tee log".into()),
            ("aider".into(), "aider".into()),
        ]);
        let names: Vec<&str> = offered.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["claude", "fast", "cx", "env", "aider"]);
        assert_eq!(
            worktree_line("b", None, "aider --model x", "go").as_deref(),
            Ok("qoop worktree new b -- aider --model x && exit")
        );
        assert_eq!(
            worktree_line("b", None, "gemini", "go").as_deref(),
            Ok("qoop worktree new b -- gemini -i go && exit")
        );
        assert!(takes_prompt("/opt/homebrew/bin/codex") && !takes_prompt("aider"));
        assert!(
            takes_prompt("CLAUDE_CONFIG_DIR=/tmp/c claude"),
            "past an env assignment"
        );
        assert!(takes_prompt("env FOO=1 claude"), "past env");
    }

    #[test]
    fn the_explain_words_are_the_daemons() {
        assert_eq!(EXPLAIN, qoop_runtime::menu::EXPLAIN);
    }

    /// Digits 1 to 5 in order, none on the "always" option, Explain and qass after the agent's
    /// own; a multiple choice menu offers nothing to pick here.
    #[test]
    fn the_card_numbers_its_answers() {
        let menu = edit_menu(7);
        let all = offers(&menu);
        let offered: Vec<(Option<u8>, &str)> = all
            .iter()
            .map(|o| (o.digit, o.label.split(',').next().unwrap_or("")))
            .collect();
        assert_eq!(
            offered,
            [
                (Some(1), "Yes"),
                (Some(2), "Yes"),
                (None, "Yes"),
                (Some(3), "No"),
                (Some(4), "Explain this in more detail"),
                (Some(5), "Let qass handle it"),
            ]
        );
        assert_eq!(text_key(&menu).as_deref(), Some("4"));
        let multi = AgentMenu {
            multi: true,
            ..menu
        };
        assert!(offers(&multi).is_empty());
    }

    fn held(terminal: u32, seal: u64, until: Instant) -> Hold {
        Hold {
            slot: Slot::Agent(terminal),
            generation: Some(1),
            seal,
            pick: Pick::Key("1".into()),
            label: "Yes".into(),
            until,
        }
    }

    /// With a fake clock: a menu arms after 500 ms, a pick goes after 3 s, Undo cancels it, a
    /// new menu drops it, and closing the window sends what is on hold.
    #[test]
    fn a_pick_waits_three_seconds_and_can_be_undone() {
        let t0 = Instant::now();
        let mut rows = vec![row(1, 1, "working"), row(2, 1, "blocked")];
        rows[1].menu = Some(edit_menu(7));
        let mut holds = Holds::default();
        assert!(holds.menus(&rows, t0).is_empty());
        assert!(!holds.armed(2, 7, t0 + Duration::from_millis(499)));
        assert!(holds.armed(2, 7, t0 + ARMING));

        holds.start(held(2, 7, t0 + HOLD));
        assert!(holds.due(t0 + Duration::from_millis(2999)).is_empty());
        assert_eq!(holds.due(t0 + HOLD).len(), 1, "sent after 3 s");

        holds.start(held(2, 7, t0 + HOLD));
        assert!(holds.undo(Some(&Slot::Agent(2))));
        assert!(holds.due(t0 + HOLD).is_empty(), "Undo cancels");

        holds.start(held(2, 7, t0 + HOLD));
        rows[1].menu = Some(edit_menu(8));
        let dropped = holds.menus(&rows, t0 + Duration::from_secs(1));
        assert_eq!(dropped.len(), 1, "another menu drops the pick");
        assert!(
            !holds.armed(2, 8, t0 + Duration::from_secs(1)),
            "and arms anew"
        );

        holds.start(held(2, 8, t0 + HOLD));
        holds.start(held(3, 9, t0 + HOLD));
        assert_eq!(holds.flush().len(), 2, "closing sends both at once");
        assert!(holds.held.is_empty());

        // The hand-off's own Undo takes back the hand-off, not a single answer held later.
        let hand = |terminal| Hold {
            pick: Pick::Qass,
            ..held(terminal, 0, t0 + HOLD)
        };
        holds.start(hand(4));
        holds.start(hand(5));
        holds.start(held(2, 8, t0 + HOLD + Duration::from_secs(1)));
        assert!(holds.undo_hand_off());
        let left: Vec<Option<u32>> = holds.held.iter().map(Hold::terminal).collect();
        assert_eq!(left, [Some(2)]);
    }

    /// Claude Code's Bash question: its "don't ask again" answer is a standing permission,
    /// click only, as an "always" one is.
    #[test]
    fn a_standing_permission_has_no_digit() {
        let menu: AgentMenu = serde_json::from_value(serde_json::json!({
            "question": "Do you want to proceed?",
            "seal": 3,
            "options": [
                {"key": "1", "label": "Yes"},
                {"key": "2", "label": "Yes, and don't ask again for cargo test commands in /srv/repo"},
                {"key": "3", "label": "No, and tell Claude what to do differently (esc)", "tell": true},
            ],
        }))
        .expect("menu");
        let digits: Vec<Option<u8>> = offers(&menu).iter().map(|o| o.digit).collect();
        assert_eq!(digits, [Some(1), None, Some(2), Some(3), Some(4)]);
        assert!(standing("Yes, and don\u{2019}t ask again for this command"));
        assert!(!standing(
            "Yes, allow all edits during this session (shift+tab)"
        ));
    }

    /// The rules card counts a lessons file the way qass writes it.
    #[test]
    fn the_lessons_are_counted_by_their_action() {
        let file = "# Lessons\n\
                    \n\
                    - a release or a tag -> escalate (owner 2026-10-02)\n\
                    - an edit to README in acme-web -> do it (E7)\n\
                    - a delete outside the repo -> ask first (owner 2026-10-03)\n\
                    - a test run in the repo -> do it (owner 2026-10-04)\n\
                    - a dependency bump -> wait (E9)\n\
                    a note that is not a lesson -> do it\n";
        assert_eq!(
            lessons_of(file),
            Lessons {
                yours: 2,
                alone: 2,
                all: 5,
            }
        );
        assert_eq!(lessons_of(""), Lessons::default());
    }

    const REMOTE_SPACE: i64 = (1 << 40) | 1;
    const REMOTE_TERMINAL: u32 = 0x8100_0001;

    /// This Mac's qass (tab 9, generation 1) running and watching, one remote machine named
    /// staging, and a rows list with the remote agent in it.
    fn stack() -> (HandInput, Vec<AgentRow>) {
        let q = HandInput {
            qass: Some((9, Some(1))),
            watching: Some(true),
            // Never the owner's own file: a test's hand for a remote agent names this.
            hand_id: Some("testmac".into()),
            machines: BTreeMap::from([(1, "staging".to_string())]),
            ..HandInput::default()
        };
        let rows = vec![
            row(1, 1, "working"),
            row(REMOTE_TERMINAL, REMOTE_SPACE, "working"),
            row(9, 3, "working"),
        ];
        (q, rows)
    }

    /// The remote agent asking, handed by `by` at `at`.
    fn remote_asking(rows: &mut [AgentRow], by: Option<&str>, at: i64) {
        let r = rows
            .iter_mut()
            .find(|r| r.terminal == REMOTE_TERMINAL)
            .expect("remote");
        r.state = "blocked".into();
        r.status = Some("blocked".into());
        r.with_qass = by.is_some();
        r.qass_away = by.map(|by| qoop_runtime::control::AwayHand { at, by: by.into() });
    }

    fn remote(rows: &[AgentRow]) -> &AgentRow {
        rows.iter()
            .find(|r| r.terminal == REMOTE_TERMINAL)
            .expect("remote")
    }

    /// One rule offers the hand-off for every machine: This Mac's qass takes a remote agent's
    /// question too, and a machine whose qoop refused such a hand as too old is not offered it
    /// again.
    #[test]
    fn this_macs_qass_is_offered_for_remote_agents_too() {
        let (mut q, mut rows) = stack();
        remote_asking(&mut rows, None, 0);
        assert!(can_hand(remote(&rows), &q));
        assert_eq!(q.agent_key(remote(&rows)).as_deref(), Some("staging/T1"));
        q.machines.clear();
        assert_eq!(q.agent_key(remote(&rows)), None, "no name, no key");
        q.machines.insert(1, "staging".into());
        q.no_hands.insert(1);
        assert!(!can_hand(remote(&rows), &q));
        q.no_hands.clear();
        q.qass = None;
        assert!(!can_hand(remote(&rows), &q));
        assert_eq!(q.qass_off(), Some("qass is not running"));
        q.qass = Some((9, Some(1)));
        q.watching = Some(false);
        assert_eq!(q.qass_off(), Some("qass is not watching"));
    }

    /// A remote hand from this Mac counts as with qass only while This Mac's qass has run and
    /// watched since the window first saw it: one already there at launch, one made before a
    /// qass restart, and one qass wrote a card for wait on the owner again. A hand by another
    /// Mac is that Mac's qass's.
    #[test]
    fn a_remote_hand_is_with_qass_only_while_the_qass_told_of_it_runs() {
        let (mut q, mut rows) = stack();
        remote_asking(&mut rows, Some("testmac"), 100);
        // Already there in the window's first list: nothing says this qass heard of it.
        note_qass(&mut q, &rows, 10);
        assert!(!handed(remote(&rows), &q));
        // Handed again from here: a new hand, seen after qass came up.
        remote_asking(&mut rows, Some("testmac"), 200);
        note_qass(&mut q, &rows, 20);
        assert!(handed(remote(&rows), &q));
        // qass's card for it: the card is what waits on the owner now.
        q.carded = vec!["staging/T1".into()];
        assert!(!handed(remote(&rows), &q));
        q.carded.clear();
        // qass restarted (a new generation): it never heard of the hand.
        q.qass = Some((9, Some(2)));
        note_qass(&mut q, &rows, 30);
        assert!(!handed(remote(&rows), &q));
        // Stopped: nobody holds it.
        q.watching = Some(false);
        note_qass(&mut q, &rows, 40);
        assert!(!handed(remote(&rows), &q));
        // Another Mac's hand is that Mac's qass's.
        q.watching = Some(true);
        remote_asking(&mut rows, Some("othermac"), 300);
        note_qass(&mut q, &rows, 50);
        assert!(handed(remote(&rows), &q));
    }

    /// A remote's rows reach the window in a later list than This Mac's: a hand in that
    /// machine's first list is still one the window knows nothing of.
    #[test]
    fn a_remote_hand_in_its_machines_first_list_is_unknown() {
        let (mut q, mut rows) = stack();
        remote_asking(&mut rows, Some("testmac"), 100);
        let local: Vec<AgentRow> = rows
            .iter()
            .filter(|r| machine_of(r.workspace) == 0)
            .cloned()
            .collect();
        note_qass(&mut q, &local, 10);
        note_qass(&mut q, &rows, 20);
        assert!(!handed(remote(&rows), &q));
    }

    /// A hand on This Mac's own agent is with qass while qass takes it, and back with the
    /// owner when qass stops or leaves that agent alone.
    #[test]
    fn a_local_hand_is_with_qass_only_while_qass_runs_and_watches() {
        let (mut q, mut rows) = stack();
        rows[0].state = "blocked".into();
        rows[0].status = Some("blocked".into());
        rows[0].with_qass = true;
        assert!(handed(&rows[0], &q));
        rows[0].qass_skip = true;
        assert!(!handed(&rows[0], &q));
        rows[0].qass_skip = false;
        q.watching = Some(false);
        assert!(!handed(&rows[0], &q));
    }

    /// Send now takes the pick out of its hold at once, and `due` never sends it a second
    /// time; with no slot it takes the newest pick and those held with it.
    #[test]
    fn send_now_takes_the_pick_out_of_its_hold() {
        let t0 = Instant::now();
        let mut holds = Holds::default();
        holds.start(held(2, 7, t0 + HOLD));
        assert!(holds.due(t0).is_empty(), "still held");
        let sent = holds.send_now(Some(&Slot::Agent(2)));
        assert_eq!(sent.len(), 1, "taken out to send");
        assert_eq!(sent[0].pick, Pick::Key("1".into()));
        assert!(holds.held.is_empty());
        assert!(holds.due(t0 + HOLD).is_empty(), "not sent twice");
        assert!(
            holds.send_now(Some(&Slot::Agent(2))).is_empty(),
            "nothing left"
        );

        // With no slot: the newest, and every pick held with it; an older one stays.
        let hand = |terminal| Hold {
            pick: Pick::Qass,
            ..held(terminal, 0, t0 + HOLD + Duration::from_secs(1))
        };
        holds.start(held(3, 9, t0 + HOLD));
        holds.start(hand(4));
        holds.start(hand(5));
        let sent: Vec<Option<u32>> = holds.send_now(None).iter().map(Hold::terminal).collect();
        assert_eq!(sent, [Some(4), Some(5)]);
        let left: Vec<Option<u32>> = holds.held.iter().map(Hold::terminal).collect();
        assert_eq!(left, [Some(3)]);
    }

    fn reply_hold(terminal: u32, turn: Option<&str>, until: Instant) -> Hold {
        Hold {
            slot: Slot::Agent(terminal),
            generation: Some(1),
            seal: 0,
            pick: Pick::Reply {
                text: "xheron".into(),
                turn: turn.map(str::to_string),
            },
            label: "xheron".into(),
            until,
        }
    }

    /// A typed reply has no menu seal: it waits out its hold keyed by the agent's turn, and is
    /// dropped when the agent moves on, so words never land in a later turn.
    #[test]
    fn a_reply_is_dropped_when_its_agent_moves_on() {
        let t0 = Instant::now();
        let waiting = |stop: &str| {
            let mut r = row(4, 1, "blocked");
            r.stop = Some(stop.into());
            r
        };
        let mut holds = Holds::default();
        holds.start(reply_hold(4, Some("100.1"), t0 + HOLD));
        assert!(
            holds.menus(&[waiting("100.1")], t0).is_empty(),
            "the same turn keeps it"
        );
        assert_eq!(holds.held.len(), 1);

        let dropped = holds.menus(&[waiting("160.2")], t0);
        assert_eq!(dropped.len(), 1, "another turn drops it");
        assert!(holds.held.is_empty());

        holds.start(reply_hold(4, Some("100.1"), t0 + HOLD));
        let mut restarted = waiting("100.1");
        restarted.generation = Some(2);
        assert_eq!(holds.menus(&[restarted], t0).len(), 1, "a new process");

        holds.start(reply_hold(4, Some("100.1"), t0 + HOLD));
        let mut working = row(4, 1, "working");
        working.stop = Some("100.1".into());
        assert_eq!(holds.menus(&[working], t0).len(), 1, "no longer waiting");

        holds.start(reply_hold(4, Some("100.1"), t0 + HOLD));
        assert_eq!(holds.menus(&[], t0).len(), 1, "the agent is gone");

        // An older daemon sends no stop: the turn is none, and it holds while it waits.
        holds.start(reply_hold(4, None, t0 + HOLD));
        assert!(holds.menus(&[row(4, 1, "blocked")], t0).is_empty());
        assert_eq!(holds.held.len(), 1);
    }
}

//! The daemon's side of the question inbox: who may write a card, and where an answer goes.
//!
//! qass writes a card (`Add`) and ends one it has handled (`Close`), each with the token of a
//! terminal in its own space. The owner answers or dismisses from a client that speaks for no
//! terminal: any terminal's token is refused there, so a worker can neither forge a card nor
//! answer one. An answer is claimed once (`open` to `queued`) and is never refused: one
//! flusher types it as soon as its target takes input, then the card is `answered` or `passed`:
//!
//! 1. straight to the worker, when it is the same conversation the card was written about and
//!    said which conversation that is (at its next rest when it is working);
//! 2. otherwise typed to qass as `<handle> <letter>` (the form qass already reads in the
//!    chat), once qass is idle and nobody is typing in its terminal.
//!
//! The route is read again at every send, so a conversation that changed while the answer
//! waited is not typed into. A send that fails, or an answer that waits too long, gives the
//! card back to `open` with the reason, so an answer is never lost silently.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use qoop_domain::ClientId;
use qoop_store::{Added, AskError, AskRow};

use crate::agents::AgentRegistry;
use crate::control::{AskCard, AskInfo, AskOp, AskResult, ControlResponse};
use crate::daemon::{ClientCommand, ConnectionRegistry, TerminalRegistry};
use crate::store::{HandleError, StoreHandle, now};

/// The sender name of the inbox's own typing: never counted as a person typing.
pub(crate) const SENDER: &str = "qoop-ask";

/// The space qass works in.
const QASS_SPACE: &str = "qass";

/// How long after a person typed into qass's terminal the inbox leaves it alone: a draft
/// still reads `idle`, and typing over it would send half a sentence.
const TYPED_QUIET_SECS: i64 = 30;

/// The pause between the words and the Enter that sends them, the one `qoop prompt` leaves so
/// a bracketed paste lands before the newline submits it.
const BEFORE_ENTER: Duration = Duration::from_millis(300);

/// How often the flusher looks at the answers waiting for a target to take input.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// After words are typed to a target it is left alone this long: it still reads idle until its
/// turn starts, and the next answer must not land in a turn that has not begun.
const SETTLE: Duration = Duration::from_secs(3);

/// How long an answer may wait for its target before it is given back.
const GIVE_UP_SECS: i64 = 30 * 60;

/// The most characters of his own words one answer carries.
const TEXT_MAX: usize = 400;

/// How many ended cards a read carries: the last few for the pane, or all of the keep window.
const ENDED_SHOWN: usize = 5;
const ENDED_ALL: usize = 200;

/// Who is asking, as the request said it.
pub(crate) struct Caller {
    pub terminal: Option<u32>,
    pub token: Option<String>,
    /// The name of the space that terminal is in, when it could be read.
    pub space: Option<String>,
}

/// What the daemon knows of a terminal's agent, for deciding whether text may be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Standing {
    pub state: String,
    pub readiness: String,
    pub session: Option<String>,
}

/// Why text must not be sent to this agent, if it must not be: what `qoop prompt` refuses
/// (a trust prompt, a blocked agent, a sleeping tab) and, besides, a working one, because
/// Claude would queue the words behind its turn and the owner would see no answer.
pub fn refusal(state: &str, readiness: &str) -> Option<&'static str> {
    if readiness == "trust_required" {
        return Some("trust");
    }
    match state {
        "blocked" => Some("blocked"),
        "sleeping" => Some("sleeping"),
        "working" => Some("working"),
        _ => None,
    }
}

impl Standing {
    /// Will be at rest by itself: idle, done or mid-turn, and not at a trust prompt. A blocked
    /// agent waits on the owner, so words for it go through qass.
    fn coming_to_rest(&self) -> bool {
        matches!(self.state.as_str(), "idle" | "done" | "working")
            && self.readiness != "trust_required"
    }

    /// At rest and open to words.
    fn at_rest(&self) -> bool {
        matches!(self.state.as_str(), "idle" | "done")
            && refusal(&self.state, &self.readiness).is_none()
    }
}

fn state_word(state: qoop_domain::AgentRuntimeState) -> &'static str {
    use qoop_domain::AgentRuntimeState as S;
    match state {
        S::Starting => "starting",
        S::Idle => "idle",
        S::Working => "working",
        S::Blocked => "blocked",
        S::Done => "done",
        S::Failed => "failed",
        S::Uncertain => "uncertain",
        S::Unknown => "unknown",
    }
}

fn standing(
    agents: &AgentRegistry,
    terminals: &TerminalRegistry,
    terminal: u32,
) -> Option<Standing> {
    let agent = agents.agent(terminal)?;
    let state = state_word(agent.belief.state);
    let readiness = crate::control::readiness_of(
        match &agent.identity {
            qoop_domain::AgentIdentity::Known { program, .. } => Some(program.as_str()),
            _ => None,
        },
        None,
        state,
        agent.setup_gate,
    );
    Some(Standing {
        state: state.to_string(),
        readiness: readiness.to_string(),
        session: terminals.session_of(terminal),
    })
}

fn failed(id: String, reason: impl Into<String>) -> ControlResponse {
    ControlResponse::Failed {
        id,
        reason: reason.into(),
    }
}

/// A store refusal as a sentence: an ask refusal is already one.
fn why(error: HandleError) -> String {
    match error {
        // The deadline does not cancel the write, so it may still land.
        HandleError::Timeout => "qoop did not confirm in time; check qoop ask ls".to_string(),
        HandleError::Store(e) => e.to_string(),
    }
}

/// `T52` as terminal 52, `ubox/T2` and the like as a remote's (no terminal here), and the
/// spelling without the `T` too. `None` for what is neither.
pub fn agent_key(agent: &str) -> Option<(String, Option<u32>)> {
    let number = |s: &str| {
        let digits = s.strip_prefix('T').unwrap_or(s);
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse::<u32>().ok())
            .flatten()
    };
    match agent.split_once('/') {
        Some((machine, tail)) if !machine.is_empty() && number(tail).is_some() => {
            Some((format!("{machine}/T{}", number(tail)?), None))
        }
        Some(_) => None,
        None => number(agent).map(|n| (format!("T{n}"), Some(n))),
    }
}

/// Whether the caller holds a terminal's token that is in qass's own space: the only one
/// that may write a card.
fn qass_speaks(agents: &AgentRegistry, caller: &Caller) -> Result<(), String> {
    let (Some(terminal), Some(token)) = (caller.terminal, caller.token.as_deref()) else {
        return Err("a card is written from qass's own terminal".into());
    };
    if !crate::tasks::admitted(agents, terminal, token) {
        return Err(format!(
            "the ask request for terminal {terminal} was refused"
        ));
    }
    if caller.space.as_deref() != Some(QASS_SPACE) {
        return Err("a card is written from qass's own space".into());
    }
    Ok(())
}

fn info(row: AskRow) -> Option<AskInfo> {
    let body: AskCard = serde_json::from_str(&row.body).ok()?;
    Some(AskInfo {
        card: row.card,
        agent: row.agent,
        terminal: row.terminal.and_then(|t| u32::try_from(t).ok()),
        status: row.status,
        note: row.note,
        opened_at: row.opened_at,
        closed_at: row.closed_at,
        answer: row.answer,
        option: row.option,
        task: row.task,
        body,
    })
}

pub(crate) async fn read(
    id: String,
    all: bool,
    _terminals: &Arc<TerminalRegistry>,
    store: &StoreHandle,
) -> ControlResponse {
    match store
        .run(move |s| {
            Ok((
                s.live_asks()?,
                s.ended_asks(if all { ENDED_ALL } else { ENDED_SHOWN })?,
            ))
        })
        .await
    {
        Ok((live, ended)) => ControlResponse::Asks {
            id,
            live: live.into_iter().filter_map(info).collect(),
            ended: ended.into_iter().filter_map(info).collect(),
        },
        Err(e) => failed(id, why(e)),
    }
}

/// Why a draft card asks him: the daemon saw the agent stop on its own `waiting-human` stage.
const DRAFT_WHY: &str = "it stopped and waits for your answer";

/// Why a dialog draft asks him: a question dialog stayed up on the agent's screen.
const DIALOG_WHY: &str = "a question waits on its screen; qass answers it with your words";

/// How long a question dialog stays up before it gets a draft: qass wakes on the block at once
/// and often answers it first.
const DIALOG_DRAFT_SECS: u64 = 120;

/// Sets [`DIALOG_DRAFT_SECS`] instead, for a test that cannot wait two minutes.
const DIALOG_DRAFT_ENV: &str = "QOOP_DIALOG_DRAFT_SECS";

/// How long a question dialog stays up before it gets a draft, read once.
pub(crate) fn dialog_wait() -> Duration {
    static WAIT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *WAIT.get_or_init(|| {
        Duration::from_secs(
            crate::legacy::var(DIALOG_DRAFT_ENV)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(DIALOG_DRAFT_SECS),
        )
    })
}

/// Whether a `blocked` label is a question dialog: the hook passes the notification type of
/// those two, and of nothing else it reports as blocked.
pub(crate) fn is_dialog(label: &str) -> bool {
    matches!(label, "elicitation_dialog" | "elicitation_url_dialog")
}

/// Words that offer the owner an action and wait for his go, in the forms agents end a turn
/// with: "istersen ... yaparım", "'tamam' dersen ...", "shall I". Cut boilerplate stays out
/// ("let me know if", "if you want"): those close nearly every report. Lowercase, ASCII
/// spellings too, since agents often write Turkish without its letters.
const OFFERS: &[&str] = &[
    "istersen",
    "i̇stersen",
    "dersen",
    "ister misin",
    "shall i ",
    "want me to ",
    "would you like",
];

/// Whether the last paragraph of an agent's turn asks the owner something: a sentence in it
/// ends in "?", it offers him an action (see [`OFFERS`]), or it closes on the Turkish question
/// word without its mark ("hazırlayayım mı"). The one rule the daemon reads a turn's end by;
/// the hook only sends the paragraph.
pub(crate) fn asks_owner(line: &str) -> bool {
    let line = line
        .trim()
        .trim_end_matches(['.', '!', ' ', '*', '_', '`', '"', ')']);
    if line.ends_with('?') {
        return true;
    }
    // A question near its end: a "?" ending a word (not one in a link, `?q=`, or a ternary,
    // ` ? `), with at most one sentence after it ("Merge now or deploy first? Say which.").
    // Two or more after it is a write up that asked itself ("Why? Because ... Fixed.").
    if let Some(at) = line.rfind("? ").or_else(|| line.rfind("?*"))
        && line[..at].ends_with(|c: char| !c.is_whitespace())
        && line[at + 1..]
            .split_terminator(['.', '!', '?'])
            .filter(|s| !s.trim().trim_matches(['*', '_', '`', ')']).is_empty())
            .count()
            <= 1
    {
        return true;
    }
    let lower = format!(" {} ", line.to_lowercase());
    if OFFERS.iter().any(|w| lower.contains(&format!(" {w}"))) {
        return true;
    }
    matches!(
        line.rsplit(char::is_whitespace).next(),
        Some("mı" | "mi" | "mu" | "mü")
    )
}

/// Whether a prompt is one nobody typed: the harness's notice that the agent's own background
/// work ended, or another agent's message. Neither answers a question the agent left him.
pub(crate) fn not_his(prompt: &str) -> bool {
    let prompt = prompt.trim_start();
    prompt.starts_with("<task-notification") || prompt.starts_with("<agent-message")
}

/// A handle for a draft: the first word of the agent's name in lowercase ASCII letters and
/// digits, then `tail`: its task id, so no two drafts share one ("dub52"), or for an agent
/// with no task `-t` and its terminal, which no task's handle can be ("dub-t3").
fn draft_handle(name: &str, tail: impl std::fmt::Display) -> String {
    let word: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| !w.is_empty())
        .unwrap_or("agent")
        .to_ascii_lowercase()
        .chars()
        .take(20)
        .collect();
    format!("{word}{tail}")
}

/// One line of at most [`crate::control::ASK_FIELD_MAX`] characters, as a card value must be.
fn card_value(text: &str) -> String {
    qoop_store::task::one_line(text)
        .chars()
        .take(crate::control::ASK_FIELD_MAX)
        .collect()
}

/// The agent in `terminal` ended its turn waiting on the owner (its task in `waiting-human`):
/// a draft card, so his inbox shows it even when qass wrote nothing. Only on a Mac with qass's
/// space (the inbox lives there), never over a live card, once a waiting spell. No options:
/// he answers in his own words, which the worker route types to the agent. Never in the way of
/// the report that called it: a failure is dropped.
pub(crate) async fn draft_waiting(
    terminal: u32,
    name: Option<String>,
    question: Option<String>,
    session: Option<String>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let made = store
        .run(move |s| {
            let Some(task) = s.open_task_of(i64::from(terminal))? else {
                return Ok(None);
            };
            if task.stage != "waiting-human"
                || !s.workspaces()?.iter().any(|w| w.name == QASS_SPACE)
            {
                return Ok(None);
            }
            let name = name.unwrap_or_else(|| format!("terminal {terminal}"));
            let asks = question
                .filter(|q| !q.trim().is_empty())
                .unwrap_or_else(|| format!("{name} waits on you: {}", task.goal));
            let card = AskCard {
                handle: draft_handle(&name, task.id),
                urgency: "now".to_string(),
                asked: None,
                doing: card_value(&task.goal),
                why: DRAFT_WHY.to_string(),
                asks: card_value(&asks),
                options: Vec::new(),
                pick: None,
            };
            let Ok(body) = serde_json::to_string(&card) else {
                return Ok(None);
            };
            s.add_draft(
                &format!("T{terminal}"),
                i64::from(terminal),
                session.as_deref(),
                &body,
                now(),
            )
        })
        .await;
    if matches!(made, Ok(Some(_))) {
        connections.asks_changed();
    }
}

/// The agent in `terminal` still shows the question dialog its blocked report of `since`
/// opened: a draft card, "a question on its screen", task or not. Never for qass's own tab,
/// only on a Mac with qass's space, and once a spell. No options: his words go to qass, since
/// a blocked agent takes none, and qass answers the dialog. A failure is dropped.
pub(crate) async fn draft_dialog(
    terminal: u32,
    since: i64,
    name: Option<String>,
    (terminals, agents, connections, store): Hands<'_>,
) {
    // Still blocked: an answer given at the desk can leave a long tool running with no event.
    if !standing(agents, terminals, terminal).is_some_and(|s| s.state == "blocked")
        || qass_terminal(terminals, agents, store).await == Some(terminal)
    {
        return;
    }
    let session = terminals.session_of(terminal);
    let made = store
        .run(move |s| {
            if !s.workspaces()?.iter().any(|w| w.name == QASS_SPACE) {
                return Ok(None);
            }
            let task = s.open_task_of(i64::from(terminal))?;
            let name = name.unwrap_or_else(|| format!("terminal {terminal}"));
            let handle = match &task {
                Some(task) => draft_handle(&name, task.id),
                None => draft_handle(&name, format!("-t{terminal}")),
            };
            let card = AskCard {
                handle,
                urgency: "now".to_string(),
                asked: None,
                doing: card_value(task.as_ref().map_or("no open task", |t| t.goal.as_str())),
                why: DIALOG_WHY.to_string(),
                asks: card_value(&format!(
                    "{name} (T{terminal}) has a question on its screen"
                )),
                options: Vec::new(),
                pick: None,
            };
            let Ok(body) = serde_json::to_string(&card) else {
                return Ok(None);
            };
            s.add_dialog_draft(
                &format!("T{terminal}"),
                i64::from(terminal),
                session.as_deref(),
                &body,
                since,
                now(),
            )
        })
        .await;
    if matches!(made, Ok(Some(_))) {
        connections.asks_changed();
    }
}

/// The agent in `terminal` ended its turn of `since` asking the owner, and the wait is over:
/// when that end is still its newest event (no prompt, tool or turn came after) and it is not
/// qass, its open task waits on him and his inbox gets the draft. No task, no card: an edit
/// opens one. A failure is dropped.
pub(crate) async fn wait_on_question(
    terminal: u32,
    since: i64,
    name: Option<String>,
    question: String,
    (terminals, agents, connections, store): Hands<'_>,
) {
    // Its label too: two turn ends can share a second.
    let still = agents
        .focus_events(terminal)
        .last()
        .is_some_and(|e| e.kind == "done" && e.at_unix == since && e.label == question);
    if !still || qass_terminal(terminals, agents, store).await == Some(terminal) {
        return;
    }
    let now = now();
    let waits = store
        .run(move |s| s.wait_on_owner(i64::from(terminal), now))
        .await;
    if !matches!(waits, Ok(Some(_))) {
        return;
    }
    agents.tasks_changed();
    draft_waiting(
        terminal,
        name,
        Some(question),
        terminals.session_of(terminal),
        connections,
        store,
    )
    .await;
}

/// The agent in `terminal` reported a tool or a prompt: the dialog it showed is gone, and so is
/// its draft.
pub(crate) async fn dialog_ended(
    terminal: u32,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let closed = store
        .run(move |s| s.close_dialog_drafts(i64::from(terminal), now()))
        .await;
    if matches!(closed, Ok(true)) {
        connections.asks_changed();
    }
}

/// A card closed because the worker it was about was told its answer in the chat.
pub(crate) async fn relayed(
    label: Option<&str>,
    terminal: u32,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let Some(card) = label.and_then(card_in_prompt) else {
        return;
    };
    let closed = store
        .run(move |s| match s.ask(&card)? {
            Some(row) if row.terminal == Some(i64::from(terminal)) => {
                s.close_ask(&card, "answered in chat", now())
            }
            _ => Ok(false),
        })
        .await;
    if matches!(closed, Ok(true)) {
        connections.asks_changed();
    }
}

/// `E7` (qass's card) or `W12` (the daemon's draft) from a prompt head that starts `The owner
/// answered E7`, matched whole so it never reads E70 as E7.
fn card_in_prompt(head: &str) -> Option<String> {
    let rest = head.strip_prefix("The owner answered ")?;
    let kind = rest.chars().next().filter(|c| matches!(c, 'E' | 'W'))?;
    let rest = &rest[1..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = rest[digits.len()..].chars().next();
    (!digits.is_empty() && after.is_none_or(|c| !c.is_alphanumeric()))
        .then(|| format!("{kind}{digits}"))
}

pub(crate) async fn write(
    id: String,
    caller: Caller,
    action: AskOp,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) -> ControlResponse {
    let named = match &action {
        AskOp::Add { card, .. }
        | AskOp::Close { card, .. }
        | AskOp::Dismiss { card }
        | AskOp::Answer { card, .. } => card.clone(),
    };
    let result = match action {
        AskOp::Add { card, agent, body } => {
            if let Err(reason) = qass_speaks(agents, &caller) {
                return failed(id, reason);
            }
            match add(&card, agent, *body, terminals, store).await {
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
        AskOp::Close { card, note } => {
            if let Err(reason) = qass_speaks(agents, &caller) {
                return failed(id, reason);
            }
            match close(&card, &note, store).await {
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
        AskOp::Dismiss { card } => {
            if caller.terminal.is_some() {
                return failed(id, "a card is put away by the owner, not from a terminal");
            }
            match close(&card, "dismissed", store).await {
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
        AskOp::Answer {
            card,
            option,
            text,
            seen,
        } => {
            if caller.terminal.is_some() {
                return failed(id, "a card is answered by the owner, not from a terminal");
            }
            match answer(
                &card,
                option,
                text,
                seen,
                (terminals, agents, connections, store),
            )
            .await
            {
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
    };
    connections.asks_changed();
    ControlResponse::AskWritten {
        id,
        card: named,
        result,
    }
}

async fn add(
    card: &str,
    agent: Option<String>,
    body: AskCard,
    terminals: &Arc<TerminalRegistry>,
    store: &StoreHandle,
) -> Result<AskResult, String> {
    let problems = body.problems();
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    if !card.starts_with('E') || card.len() < 2 || !card[1..].bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{card} is not a card number: E<n> from escalate.sh new"
        ));
    }
    let (agent, terminal) = match agent.as_deref().map(agent_key) {
        None => (None, None),
        Some(Some((key, terminal))) => (Some(key), terminal),
        Some(None) => {
            return Err("the agent is T<n>, <machine>/T<n>, or - for none".to_string());
        }
    };
    let session = terminal.and_then(|t| terminals.session_of(t));
    let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let card = card.to_string();
    store
        .run(move |s| {
            s.add_ask(
                &card,
                agent.as_deref(),
                terminal.map(i64::from),
                session.as_deref(),
                &json,
                now(),
            )
        })
        .await
        .map(|added| match added {
            Added::New => AskResult::Added,
            Added::Same => AskResult::Unchanged,
            Added::Replaced => AskResult::Replaced,
        })
        .map_err(why)
}

async fn close(card: &str, note: &str, store: &StoreHandle) -> Result<AskResult, String> {
    let (card, note) = (card.to_string(), qoop_store::task::one_line(note));
    if note.is_empty() || note.chars().count() > 60 {
        return Err("the note is one to 60 characters".to_string());
    }
    store
        .run(move |s| s.close_ask(&card, &note, now()))
        .await
        .map(|closed| {
            if closed {
                AskResult::Closed
            } else {
                AskResult::Ended
            }
        })
        .map_err(why)
}

/// His own words as they are sent: one line, no control characters (what qass and the
/// worker read must be what he typed), `None` when nothing is left.
fn clean_text(text: &str) -> Option<String> {
    Some(qoop_store::task::one_line(text)).filter(|t| !t.is_empty())
}

/// What the daemon's ask code works with: terminals, agents, connections and the store.
type Hands<'a> = (
    &'a Arc<TerminalRegistry>,
    &'a Arc<AgentRegistry>,
    &'a Arc<ConnectionRegistry>,
    &'a StoreHandle,
);

/// What is sent to the worker for an answer: the line qass sends today.
fn line_for_worker(card: &str, said: &str, chose: Option<(&str, &str)>) -> String {
    let chose = chose
        .map(|(key, words)| {
            format!(
                " He chose {}: {}.",
                key.to_uppercase(),
                words.trim_end_matches(['.', ' '])
            )
        })
        .unwrap_or_default();
    format!(
        "The owner answered {card}: \"{said}\".{chose} Those are the words of the owner: do only what they allow, and ask before anything more."
    )
}

/// What is typed to qass: the form it already reads in the chat.
fn line_for_qass(handle: &str, option: Option<&str>, text: Option<&str>) -> String {
    match (option, text) {
        (Some(key), _) => format!("{handle} {}", key.to_uppercase()),
        (None, Some(text)) => format!("{handle} {text}"),
        (None, None) => handle.to_string(),
    }
}

/// qass's own terminal: the first agent in the space named `qass`.
async fn qass_terminal(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> Option<u32> {
    let spaces = tokio::time::timeout(Duration::from_secs(2), store.workspaces())
        .await
        .ok()?
        .ok()?;
    let qass = spaces.into_iter().find(|w| w.name == QASS_SPACE)?.id;
    let mut mine: Vec<u32> = terminals
        .streams()
        .into_iter()
        .filter(|t| terminals.workspace_of(*t) == Some(qass))
        .filter(|t| {
            agents.agent(*t).is_some_and(|a| {
                matches!(
                    a.identity,
                    qoop_domain::AgentIdentity::Known { agent: true, .. }
                )
            })
        })
        .collect();
    mine.sort_unstable();
    mine.first().copied()
}

/// Type `text`, a pause, then Enter, into a terminal.
async fn type_into(
    terminals: &Arc<TerminalRegistry>,
    terminal: u32,
    text: &str,
) -> Result<(), String> {
    let handle = terminals
        .get(terminal)
        .ok_or_else(|| format!("terminal {terminal} is gone"))?;
    let send = |bytes: Vec<u8>| {
        handle.commands.send(ClientCommand::Input {
            from: ClientId::new(SENDER),
            bytes: Bytes::from(bytes),
        })
    };
    send(text.as_bytes().to_vec())
        .await
        .map_err(|_| format!("terminal {terminal} did not take the words"))?;
    tokio::time::sleep(BEFORE_ENTER).await;
    if send(vec![b'\r']).await.is_err() {
        // The words may sit in the composer: clear the line so a second try does not double them.
        let _ = send(vec![0x15]).await;
        return Err(format!("terminal {terminal} did not take the Enter"));
    }
    Ok(())
}

/// Where an answer goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// Straight to the worker's own terminal.
    Worker(u32),
    /// Typed to qass, which relays it.
    Qass(u32),
    /// qass has no tab.
    NoQass,
}

/// Route 1 when the worker is provably the conversation the card was written about and is at
/// rest or on its way to it: the card has a session, the live row reports the same one, and
/// the agent is idle, done or working. A card without a session (an agent that reports none, or
/// a remote one) never takes it, nor does a dialog's draft. Otherwise route 2.
pub(crate) fn route(row: &AskRow, worker: Option<&(u32, Standing)>, qass: Option<u32>) -> Route {
    // A dialog's draft (a `W` card with no task): only qass answers a dialog, and once it is
    // gone his words would be a prompt about nothing.
    let dialog = row.card.starts_with('W') && row.task.is_none();
    if let Some((terminal, s)) = worker.filter(|_| !dialog)
        && row.session.is_some()
        && s.session == row.session
        && s.coming_to_rest()
    {
        return Route::Worker(*terminal);
    }
    qass.map_or(Route::NoQass, Route::Qass)
}

/// Whether the target of a route takes words now: at rest, and for qass nobody typed into it
/// lately (a draft still reads `idle`, and typing over it would send half a sentence).
pub(crate) fn takes_words(
    route: Route,
    worker: Option<&(u32, Standing)>,
    qass: Option<&(u32, Standing, Option<i64>)>,
) -> bool {
    match route {
        Route::Worker(_) => worker.is_some_and(|(_, s)| s.at_rest()),
        Route::Qass(_) => qass.is_some_and(|(_, s, typed_ago)| {
            s.at_rest() && typed_ago.is_none_or(|ago| ago >= TYPED_QUIET_SECS)
        }),
        Route::NoQass => false,
    }
}

async fn answer(
    card: &str,
    option: Option<String>,
    text: Option<String>,
    seen: Option<String>,
    hands: Hands<'_>,
) -> Result<AskResult, String> {
    let (terminals, agents, connections, store) = hands;
    let wanted = card.to_string();
    let row = store
        .run(move |s| s.ask(&wanted))
        .await
        .map_err(why)?
        .ok_or_else(|| AskError::NoCard(card.to_string()).to_string())?;
    if row.status != "open" {
        return Err(AskError::NotOpen(card.to_string(), row.status).to_string());
    }
    let body: AskCard = serde_json::from_str(&row.body).map_err(|e| e.to_string())?;
    let text = text.as_deref().and_then(clean_text);
    if let Some(t) = &text
        && t.chars().count() > TEXT_MAX
    {
        return Err(format!(
            "the answer is over {TEXT_MAX} characters; shorten it"
        ));
    }
    let chosen = match option.as_deref() {
        Some(key) => Some(
            body.options
                .iter()
                .find(|o| o.key == key)
                .ok_or_else(|| format!("{card} has no option {key}"))?,
        ),
        None => None,
    };
    if let (Some(o), Some(seen)) = (chosen, &seen)
        && o.words != *seen
    {
        return Err(format!("{card} changed since you read it; look again"));
    }
    if chosen.is_none() && text.is_none() {
        return Err("an answer is an option or his own words".to_string());
    }
    // An option's words are the card's own, and a typed answer wins when both come: his
    // words are the answer, the option only says which he looked at.
    let key = chosen.filter(|_| text.is_none()).map(|o| o.key.clone());

    let worker = worker_of(&row, agents, terminals);
    let qass = qass_terminal(terminals, agents, store).await;
    let result = match route(&row, worker.as_ref(), qass) {
        Route::Worker(_) => AskResult::Answered,
        Route::Qass(_) => AskResult::Passed,
        Route::NoQass => return Err("qass has no tab to hand this to".to_string()),
    };
    let said = text.clone().or_else(|| key.clone()).unwrap_or_default();
    let (wanted, option_key) = (card.to_string(), key);
    let claimed = store
        .run(move |s| {
            s.claim_ask(
                &wanted,
                "queued",
                option_key.as_deref(),
                &said,
                "queued",
                now(),
            )
        })
        .await
        .map_err(why)?;
    // Rewritten between the read and the claim: the words he read are not the ones in the
    // card now, so nothing is sent.
    if claimed.body != row.body {
        let back = card.to_string();
        let _ = store.run(move |s| s.reopen_ask(&back, None)).await;
        return Err(format!("{card} changed since you read it; look again"));
    }
    kick(terminals, agents, connections, store);
    Ok(result)
}

/// The worker a card is about, as it stands: its terminal, when that still holds an agent.
fn worker_of(
    row: &AskRow,
    agents: &Arc<AgentRegistry>,
    terminals: &Arc<TerminalRegistry>,
) -> Option<(u32, Standing)> {
    let terminal = u32::try_from(row.terminal?).ok()?;
    standing(agents, terminals, terminal).map(|s| (terminal, s))
}

/// What is typed for a queued answer on a route, rebuilt from the card and the stored answer.
fn line_for(row: &AskRow, route: Route) -> Option<String> {
    let body: AskCard = serde_json::from_str(&row.body).ok()?;
    let option = row.option.as_deref();
    let text = row.answer.as_deref().filter(|_| option.is_none());
    match route {
        Route::Worker(_) => {
            let said = match (option, text) {
                (Some(k), _) => format!("{} {}", body.handle, k.to_uppercase()),
                (None, Some(t)) => t.to_string(),
                (None, None) => return None,
            };
            let chose = option
                .and_then(|k| body.options.iter().find(|o| o.key == k))
                .map(|o| (o.key.as_str(), o.words.as_str()));
            Some(line_for_worker(&row.card, &said, chose))
        }
        Route::Qass(_) => Some(line_for_qass(&body.handle, option, text)),
        Route::NoQass => None,
    }
}

/// Whether a flusher is running: one types every queued answer, so they go in order.
static FLUSHING: AtomicBool = AtomicBool::new(false);

/// Start the flusher unless one runs.
fn kick(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    if FLUSHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (terminals, agents, connections, store) = (
        terminals.clone(),
        agents.clone(),
        connections.clone(),
        store.clone(),
    );
    tokio::spawn(async move {
        loop {
            // A panic ends this look, not the flag: the queue is looked at again below.
            let look = tokio::spawn(flush(
                terminals.clone(),
                agents.clone(),
                connections.clone(),
                store.clone(),
            ));
            if look.await.is_err() {
                tokio::time::sleep(FLUSH_EVERY).await;
            }
            FLUSHING.store(false, Ordering::SeqCst);
            // An answer queued after the last look and before the flag went down.
            let waiting = store.run(|s| s.queued_asks()).await;
            if !waiting.is_ok_and(|rows| !rows.is_empty()) || FLUSHING.swap(true, Ordering::SeqCst)
            {
                break;
            }
        }
    });
}

/// Type every queued answer as its target takes input; returns when none is queued and the
/// last target typed to has settled.
async fn flush(
    terminals: Arc<TerminalRegistry>,
    agents: Arc<AgentRegistry>,
    connections: Arc<ConnectionRegistry>,
    store: StoreHandle,
) {
    let mut sent: HashMap<u32, Instant> = HashMap::new();
    loop {
        let Ok(rows) = store.run(|s| s.queued_asks()).await else {
            return;
        };
        if rows.is_empty() && sent.values().all(|at| at.elapsed() >= SETTLE) {
            return;
        }
        // The oldest answer of a target goes first: a target seen once is left to it.
        let mut seen = HashSet::new();
        for row in rows {
            flush_card(
                &row,
                &mut seen,
                &mut sent,
                (&terminals, &agents, &connections, &store),
            )
            .await;
        }
        tokio::time::sleep(FLUSH_EVERY).await;
    }
}

/// One queued answer: typed if its target takes words now, given back if it cannot be
/// delivered, left alone otherwise.
async fn flush_card(
    row: &AskRow,
    seen: &mut HashSet<u32>,
    sent: &mut HashMap<u32, Instant>,
    (terminals, agents, connections, store): Hands<'_>,
) {
    let give_back = |why: String| async move {
        let (card, note) = (row.card.clone(), format!("not delivered: {why}"));
        let _ = store.run(move |s| s.reopen_ask(&card, Some(&note))).await;
        connections.asks_changed();
    };
    if now() - row.closed_at.unwrap_or_else(now) > GIVE_UP_SECS {
        return give_back("it waited too long".to_string()).await;
    }
    let worker = worker_of(row, agents, terminals);
    let qass = qass_terminal(terminals, agents, store).await;
    let route = route(row, worker.as_ref(), qass);
    let (target, to, note) = match route {
        Route::Worker(t) => (t, "answered", "owner, inbox"),
        Route::Qass(t) => (t, "passed", "passed to qass"),
        Route::NoQass => return give_back("qass has no tab".to_string()).await,
    };
    if !seen.insert(target) || sent.get(&target).is_some_and(|at| at.elapsed() < SETTLE) {
        return;
    }
    let qass_now = qass.and_then(|t| {
        standing(agents, terminals, t)
            .map(|s| (t, s, agents.typed_ago(t, crate::agents::now_unix())))
    });
    if !takes_words(route, worker.as_ref(), qass_now.as_ref()) {
        return;
    }
    let Some(line) = line_for(row, route) else {
        return give_back("its card could not be read".to_string()).await;
    };
    let card = row.card.clone();
    // Flipped before it is typed, so a chat answer or a close that came first wins and a
    // card is never typed twice.
    match store
        .run(move |s| s.deliver_ask(&card, to, note, now()))
        .await
    {
        Ok(true) => {}
        _ => return,
    }
    sent.insert(target, Instant::now());
    connections.asks_changed();
    if let Err(reason) = type_into(terminals, target, &line).await {
        give_back(reason).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three turn ends the devbox agent left him on 2026-10-12, two with no "?", ask him;
    /// a report, a cut courtesy and a word that only starts like an offer do not.
    #[test]
    fn a_turn_end_asks_him_by_its_mark_or_its_offer() {
        for asks in [
            "Worktree acip PR olarak hazirlayayim mi?",
            "Logo PR'i icin \"commit et, PR ac, merge et\" dersen /commit ile commit atip push'layacagim.",
            "İstersen bunu ayrı bir iş olarak güncellerim.",
            "Istersen bunu ayri bir is olarak guncellerim.",
            "Worktree açıp PR olarak hazırlayayım mı",
            "Shall I merge it now.",
            "Do you want me to deploy it as well.",
            "**Merge PR #513 now or deploy first?**",
            "Built and tested. Merge PR #513 now or deploy first? Say which.",
            "Makineyi yeniden başlatayım mı (`sudo reboot`)? Açılınca kontrol ederim.",
            "Merge it now (\"yes\" or \"no\")?\"",
        ] {
            assert!(asks_owner(asks), "{asks}");
        }
        for not in [
            "",
            "Merged and deployed, staging returns 200.",
            "Migration onaylandı, prod'a gitti.",
            "Let me know if you need anything else.",
            "If you want more detail, the log is in logs/run.txt.",
            "Bu dosyada midye yok.",
            "The report is at https://x.dev/r?id=4 now.",
            "Why did it fail? Because the cache was stale. Fixed in PR #3.",
            "let x = ok ? a : b; it is set.",
        ] {
            assert!(!asks_owner(not), "{not}");
        }
    }

    #[test]
    fn his_words_are_one_clean_line() {
        assert_eq!(
            clean_text("  run it\ntwice\u{1b}[31m\u{202e} ").as_deref(),
            Some("run it twice[31m")
        );
        assert_eq!(clean_text(" \n\t "), None);
    }

    #[test]
    fn agents_are_keyed_one_way_whatever_the_spelling() {
        assert_eq!(agent_key("T52"), Some(("T52".into(), Some(52))));
        assert_eq!(agent_key("52"), Some(("T52".into(), Some(52))));
        assert_eq!(agent_key("ubox/T2"), Some(("ubox/T2".into(), None)));
        assert_eq!(agent_key("ubox/2"), Some(("ubox/T2".into(), None)));
        for bad in ["", "T", "x52", "ubox/", "/T2", "ubox/x"] {
            assert_eq!(agent_key(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_prompt_head_names_its_card_whole() {
        assert_eq!(
            card_in_prompt("The owner answered E7: \"korea B\"").as_deref(),
            Some("E7")
        );
        assert_eq!(
            card_in_prompt("The owner answered E70: x").as_deref(),
            Some("E70")
        );
        assert_eq!(card_in_prompt("The owner answered E7x"), None);
        assert_eq!(card_in_prompt("The owner answered E"), None);
        assert_eq!(card_in_prompt("run the tests for E7"), None);
        assert_eq!(
            card_in_prompt("The owner answered W12: \"merge it\"").as_deref(),
            Some("W12")
        );
        assert_eq!(card_in_prompt("The owner answered X12"), None);
        assert_eq!(draft_handle("Dub-local PR 513", 52), "dub52");
        assert_eq!(draft_handle("ödeme", 3), "deme3");
        assert_eq!(draft_handle("***", 4), "agent4");
        assert_eq!(draft_handle("Dub-local PR 513", "-t3"), "dub-t3");
    }

    #[test]
    fn the_worker_is_told_what_qass_tells_it_today() {
        assert_eq!(
            line_for_worker("E7", "korea B", Some(("b", "Do not run 0042 yet."))),
            "The owner answered E7: \"korea B\". He chose B: Do not run 0042 yet. Those are the words of the owner: do only what they allow, and ask before anything more."
        );
        assert_eq!(
            line_for_worker("E7", "use the old column", None),
            "The owner answered E7: \"use the old column\". Those are the words of the owner: do only what they allow, and ask before anything more."
        );
        assert_eq!(line_for_qass("korea", Some("b"), None), "korea B");
        assert_eq!(
            line_for_qass("korea", None, Some("run it twice")),
            "korea run it twice"
        );
    }

    fn card(session: Option<&str>) -> AskRow {
        AskRow {
            id: 1,
            card: "E7".into(),
            agent: Some("T5".into()),
            terminal: Some(5),
            session: session.map(String::from),
            task: None,
            status: "open".into(),
            note: None,
            body: String::new(),
            answer: None,
            option: None,
            opened_at: 0,
            closed_at: None,
        }
    }

    fn at(state: &str, session: Option<&str>) -> Standing {
        Standing {
            state: state.into(),
            readiness: "ready".into(),
            session: session.map(String::from),
        }
    }

    /// A dialog's draft goes to qass even when its agent is back at rest in the same
    /// conversation: the dialog is gone, and his words would be a prompt about nothing.
    #[test]
    fn a_dialog_draft_always_goes_to_qass() {
        let dialog = AskRow {
            card: "W7".into(),
            task: None,
            ..card(Some("s1"))
        };
        let waiting = AskRow {
            task: Some(3),
            ..dialog.clone()
        };
        let worker = Some((5, at("idle", Some("s1"))));
        assert_eq!(route(&dialog, worker.as_ref(), Some(9)), Route::Qass(9));
        assert_eq!(route(&waiting, worker.as_ref(), Some(9)), Route::Worker(5));
    }

    #[test]
    fn a_card_takes_the_direct_route_only_with_a_matching_session_and_an_agent_coming_to_rest() {
        let worker = |state, session| Some((5, at(state, session)));
        // The same conversation, at rest or mid-turn: straight to the worker, never refused.
        for state in ["idle", "done", "working"] {
            assert_eq!(
                route(
                    &card(Some("s1")),
                    worker(state, Some("s1")).as_ref(),
                    Some(9)
                ),
                Route::Worker(5),
                "{state}"
            );
        }
        // No session on the card, or none reported by the agent: through qass.
        assert_eq!(
            route(&card(None), worker("idle", Some("s1")).as_ref(), Some(9)),
            Route::Qass(9)
        );
        assert_eq!(
            route(&card(Some("s1")), worker("idle", None).as_ref(), Some(9)),
            Route::Qass(9)
        );
        // Neither says a session (opencode, amp, goose, cursor): two blanks are not a match.
        assert_eq!(
            route(&card(None), worker("idle", None).as_ref(), Some(9)),
            Route::Qass(9)
        );
        // Another conversation in the terminal (a /clear, a new session): through qass.
        assert_eq!(
            route(
                &card(Some("s1")),
                worker("idle", Some("s2")).as_ref(),
                Some(9)
            ),
            Route::Qass(9)
        );
        // The worker waits on him or is gone: through qass.
        for state in ["blocked", "sleeping", "failed"] {
            assert_eq!(
                route(
                    &card(Some("s1")),
                    worker(state, Some("s1")).as_ref(),
                    Some(9)
                ),
                Route::Qass(9),
                "{state}"
            );
        }
        assert_eq!(route(&card(Some("s1")), None, Some(9)), Route::Qass(9));
        // Nobody to take it: refused, never "busy".
        assert_eq!(route(&card(None), None, None), Route::NoQass);
    }

    #[test]
    fn words_are_typed_only_to_a_target_at_rest_and_qass_only_when_nobody_is_typing() {
        let worker = |state| Some((5, at(state, Some("s1"))));
        let qass = |state, typed: Option<i64>| Some((9, at(state, None), typed));
        assert!(takes_words(Route::Worker(5), worker("idle").as_ref(), None));
        assert!(!takes_words(
            Route::Worker(5),
            worker("working").as_ref(),
            None
        ));
        assert!(takes_words(
            Route::Qass(9),
            None,
            qass("idle", None).as_ref()
        ));
        assert!(takes_words(
            Route::Qass(9),
            None,
            qass("done", Some(31)).as_ref()
        ));
        for state in ["working", "blocked", "sleeping", "starting"] {
            assert!(
                !takes_words(Route::Qass(9), None, qass(state, None).as_ref()),
                "{state}"
            );
        }
        assert!(!takes_words(
            Route::Qass(9),
            None,
            qass("idle", Some(5)).as_ref()
        ));
        assert!(!takes_words(
            Route::Qass(9),
            None,
            qass("idle", Some(29)).as_ref()
        ));
        assert!(!takes_words(Route::NoQass, None, None));
    }

    #[test]
    fn a_queued_answer_is_typed_as_the_route_says_when_it_is_sent() {
        let body = r#"{"handle":"korea","urgency":"now","doing":"d","why":"w","asks":"a","options":[{"key":"b","label":"B","reason":"r","words":"Do not run 0042 yet."}]}"#;
        let mut row = card(None);
        row.body = body.into();
        row.card = "E7".into();
        row.option = Some("b".into());
        row.answer = Some("b".into());
        assert_eq!(line_for(&row, Route::Qass(9)).as_deref(), Some("korea B"));
        assert!(
            line_for(&row, Route::Worker(5))
                .unwrap()
                .contains("He chose B: Do not run 0042 yet")
        );
        row.option = None;
        row.answer = Some("run it twice".into());
        assert_eq!(
            line_for(&row, Route::Qass(9)).as_deref(),
            Some("korea run it twice")
        );
        assert_eq!(line_for(&row, Route::NoQass), None);
    }

    #[test]
    fn text_is_refused_to_a_blocked_trusting_sleeping_or_working_agent() {
        assert_eq!(refusal("idle", "ready"), None);
        assert_eq!(refusal("done", "ready"), None);
        assert_eq!(refusal("blocked", "ready"), Some("blocked"));
        assert_eq!(refusal("idle", "trust_required"), Some("trust"));
        assert_eq!(refusal("sleeping", "ready"), Some("sleeping"));
        assert_eq!(refusal("working", "ready"), Some("working"));
        let rest = |state: &str| Standing {
            state: state.into(),
            readiness: "ready".into(),
            session: None,
        };
        assert!(rest("idle").at_rest() && rest("done").at_rest());
        assert!(!rest("failed").at_rest() && !rest("starting").at_rest());
    }
}

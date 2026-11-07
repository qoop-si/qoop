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
pub(crate) const QASS_SPACE: &str = "qass";

/// How long after a person typed into qass's terminal the inbox leaves it alone: a draft
/// still reads `idle`, and typing over it would send half a sentence.
pub const TYPED_QUIET_SECS: i64 = 30;

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
pub(crate) const TEXT_MAX: usize = 400;

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

pub(crate) fn standing(
    agents: &AgentRegistry,
    terminals: &TerminalRegistry,
    terminal: u32,
) -> Option<Standing> {
    let agent = agents.agent(terminal)?;
    let state = crate::control::state_name(agent.belief.state);
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

pub(crate) fn failed(id: String, reason: impl Into<String>) -> ControlResponse {
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

fn info((row, item): (AskRow, Option<crate::control::AskItem>)) -> Option<AskInfo> {
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
        item,
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
            // Each card with the board item it is about, when it is about one here: its plan
            // state and phase ride along, so no screen needs the board to read the card. Cards
            // share a few items: each is read once. A failed lookup leaves the card without its
            // item, never the inbox unread.
            let at = now();
            let mut known: HashMap<i64, Option<crate::control::AskItem>> = HashMap::new();
            let mut with_item = |rows: Vec<AskRow>, live: bool| -> Vec<_> {
                rows.into_iter()
                    .map(|r| {
                        let remote = r.agent.as_deref().is_some_and(|a| a.contains('/'));
                        let found = if remote {
                            None
                        } else {
                            s.item_of_card(r.task, r.terminal.filter(|_| live))
                                .ok()
                                .flatten()
                        };
                        let item = found.and_then(|(id, key)| {
                            known
                                .entry(id)
                                .or_insert_with(|| {
                                    let row = s.item_row(id, at).ok()?;
                                    Some(crate::control::AskItem {
                                        id,
                                        key,
                                        plan: match row.plan {
                                            qoop_store::plan::PlanState::None => String::new(),
                                            state => state.as_str().to_string(),
                                        },
                                        phase: row.phase.map(|(n, of, name)| {
                                            crate::control::PhaseInfo { n, of, name }
                                        }),
                                    })
                                })
                                .clone()
                        });
                        (r, item)
                    })
                    .collect()
            };
            let live = with_item(s.live_asks()?, true);
            let ended = with_item(
                s.ended_asks(if all { ENDED_ALL } else { ENDED_SHOWN })?,
                false,
            );
            Ok((live, ended))
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
/// with: "istersen ... yaparım", "'tamam' dersen ...", "shall I". The quoted answer closed
/// by "de" or "deyin" is [`quoted_answer`]. Cut boilerplate stays out ("let me know if",
/// "if you want"): those close nearly every report. Lowercase, ASCII spellings too, since
/// agents often write Turkish without its letters, and the dotted capital I lowercased.
const OFFERS: &[&str] = &[
    "istersen",
    "i̇stersen",
    "dersen",
    "demen yeter",
    "istiyorsan söyle",
    "i̇stiyorsan söyle",
    "istiyorsan soyle",
    "numarayla cevapla",
    "ister misin",
    "shall i ",
    "want me to ",
    "would you like",
];

/// Whether the line tells him a word to answer with: a quote (`"`, `”` or a backtick), a space,
/// then `de` or `deyin` and a mark or the end ("`sil` de."). "deyince", a report's "de çıkıyor"
/// and a quote followed by more words do not count.
fn quoted_answer(lower: &str) -> bool {
    let lower = lower.trim_end();
    lower.match_indices(['"', '”', '`']).any(|(at, quote)| {
        lower[at + quote.len()..]
            .strip_prefix(' ')
            .is_some_and(|rest| {
                ["deyin", "de"].iter().any(|word| {
                    rest.strip_prefix(word).is_some_and(|after| {
                        after.chars().next().is_none_or(|c| ".,;:)!".contains(c))
                    })
                })
            })
    })
}

/// Whether the last paragraph of an agent's turn asks the owner something: a sentence in it
/// ends in "?", it offers him an action (see [`OFFERS`], [`quoted_answer`]), or it closes on
/// the Turkish question word without its mark ("hazırlayayım mı"). The one rule the daemon
/// reads a turn's end by; the hook only sends the paragraph.
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
    if OFFERS.iter().any(|w| lower.contains(&format!(" {w}"))) || quoted_answer(&lower) {
        return true;
    }
    matches!(
        line.rsplit(char::is_whitespace).next(),
        Some("mı" | "mi" | "mu" | "mü")
    )
}

/// Whether the end of a turn waits on the owner: its last paragraph asks him something
/// ([`asks_owner`]) and none of the agent's sub-agents still runs. One that ends with its
/// sub-agents at work is woken by them, so a question there was put to them (a reviewer it
/// briefed), not to him; a question for him is still there at the turn that ends once they are
/// done. Shells do not count: a dev server never ends, and would keep him from ever being asked.
pub(crate) fn turn_asks_him(label: Option<&str>, subagents_running: usize) -> bool {
    subagents_running == 0 && label.is_some_and(asks_owner)
}

/// Whether a prompt is one nobody typed: the harness's notice that the agent's own background
/// work ended, or another agent's message. Neither answers a question the agent left him, nor
/// does qass's `While E7 waits on the owner: ...`, which sets it to the work the question does
/// not hold up and keeps the question open for his answer.
pub(crate) fn not_his(prompt: &str) -> bool {
    let prompt = untagged(prompt.trim_start()).trim_start();
    prompt.starts_with("<task-notification")
        || prompt.starts_with("<agent-message")
        || card_after(prompt, "While ").is_some_and(|card| {
            prompt["While ".len() + card.len()..].starts_with(" waits on the owner")
        })
}

/// A handle for a draft: the first word of the agent's name in lowercase ASCII letters and
/// digits, then `tail`: its task id, so no two drafts share one ("api52"), or for an agent
/// with no task `-t` and its terminal, which no task's handle can be ("api-t3").
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

/// How often the daemon looks for a plan that waited too long for qass.
const STALE_LOOK: Duration = Duration::from_secs(60);

/// Plans that waited [`qoop_store::plan::STALE_SECS`] for qass, with no card open on their
/// item: the owner is told once per version of the plan, with a draft card on the agent that
/// linked it. The card keeps no conversation, so his answer reaches qass, which records it.
/// Its own clock: the schedules loop sleeps while none is due.
pub(crate) fn watch_stale_plans(
    agents: Arc<AgentRegistry>,
    connections: Arc<ConnectionRegistry>,
    store: StoreHandle,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(STALE_LOOK);
        loop {
            tick.tick().await;
            stale_plans_once(&agents, &connections, &store).await;
        }
    });
}

pub(crate) async fn stale_plans_once(
    agents: &AgentRegistry,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let at = now();
    // No qass on this Mac: nobody approves here, and the inbox the card would go to is qass's.
    // A tab or a space the owner keeps from qass is his: no card, no stage moved.
    let Ok(stale) = store
        .run(move |s| {
            if !s.workspaces()?.iter().any(|w| w.name == QASS_SPACE) {
                return Ok(Vec::new());
            }
            let (spaces, tabs) = s.qass_skips()?;
            let mut out = Vec::new();
            for plan in s.stale_plans(at)? {
                let space = s.task(plan.item)?.and_then(|t| t.workspace);
                if !tabs.contains(&plan.terminal) && space.is_none_or(|w| !spaces.contains(&w)) {
                    out.push(plan);
                }
            }
            Ok(out)
        })
        .await
    else {
        return;
    };
    for plan in stale {
        let mut told = false;
        if let Ok(terminal) = u32::try_from(plan.terminal) {
            // Waiting first, so the draft and every reader of the stage agree.
            let t = plan.terminal;
            if matches!(
                store.run(move |s| s.wait_on_owner(t, at)).await,
                Ok(Some(_))
            ) {
                agents.tasks_changed();
            }
            told = file_waiting(
                terminal,
                None,
                Some(format!(
                    "{} plan waits for qass: approve it, or say what to change",
                    plan.key
                )),
                None,
                true,
                (agents, connections, store),
            )
            .await;
        }
        // Marked only once he was told, so a plan whose card could not be made is tried again
        // on the next look.
        if !told {
            continue;
        }
        let (item, hash) = (plan.item, plan.hash);
        if let Err(e) = store
            .run(move |s| s.mark_stale_carded(item, &hash, at))
            .await
        {
            crate::say!("qoopd: a stale plan's mark: {}", why(e));
        }
    }
}

/// The agent in `terminal` ended its turn waiting on the owner (its task in `waiting-human`):
/// a draft card, so his inbox shows it even when qass wrote nothing. Only on a Mac with qass's
/// space (the inbox lives there), never over a live card, once a waiting spell. No options:
/// he answers in his own words, which the worker route types to the agent. Never in the way of
/// the report that called it: a failure is dropped.
///
/// The card carries the agent's own words, never the owner's: a task qoop opened when the
/// agent changed files has his cut prompt for a goal, so that goal is neither the question nor
/// the "doing" line. With no question, a prompt that reached the agent since its wait began
/// means it works again (the stage lagged behind), so there is nothing to ask.
pub(crate) async fn draft_waiting(
    terminal: u32,
    name: Option<String>,
    question: Option<String>,
    session: Option<String>,
    hands: (&AgentRegistry, &Arc<ConnectionRegistry>, &StoreHandle),
) -> bool {
    file_waiting(terminal, name, question, session, false, hands).await
}

/// [`draft_waiting`], for a plan that waits for qass when `plan`: the agent goes on with its
/// item, so its work does not end the card.
async fn file_waiting(
    terminal: u32,
    name: Option<String>,
    question: Option<String>,
    session: Option<String>,
    plan: bool,
    (agents, connections, store): (&AgentRegistry, &Arc<ConnectionRegistry>, &StoreHandle),
) -> bool {
    let question = question.filter(|q| !q.trim().is_empty());
    let prompts: Vec<i64> = if question.is_none() {
        agents
            .focus_events(terminal)
            .iter()
            .filter(|e| e.kind == "user" && !not_his(&e.label))
            .map(|e| e.at_unix)
            .collect()
    } else {
        Vec::new()
    };
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
            // No stage event at all: nothing says when the wait began, so no prompt is later.
            if let Some(waiting) = s.task_waiting_since(task.id)?
                && prompts.iter().any(|&at| at > waiting)
            {
                return Ok(None);
            }
            let name = name.unwrap_or_else(|| format!("terminal {terminal}"));
            let owner_words = s.goal_is_prompt(task.id)?;
            let asks = question.unwrap_or_else(|| {
                if owner_words {
                    format!("{name} waits on you")
                } else {
                    format!("{name} waits on you: {}", task.goal)
                }
            });
            let doing = if owner_words {
                format!("task #{}", task.id)
            } else {
                task.goal.clone()
            };
            let card = AskCard {
                handle: draft_handle(&name, task.id),
                urgency: "now".to_string(),
                asked: None,
                doing: card_value(&doing),
                why: DRAFT_WHY.to_string(),
                asks: card_value(&asks),
                options: Vec::new(),
                pick: None,
            };
            let Ok(body) = serde_json::to_string(&card) else {
                return Ok(None);
            };
            let file = if plan {
                qoop_store::Store::add_plan_draft
            } else {
                qoop_store::Store::add_draft
            };
            file(
                s,
                &format!("T{terminal}"),
                i64::from(terminal),
                session.as_deref(),
                &body,
                now(),
            )
        })
        .await;
    let drafted = matches!(made, Ok(Some(_)));
    if drafted {
        crate::telemetry::count(crate::telemetry::Event::DraftAsked);
        connections.asks_changed();
    }
    drafted
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
        crate::telemetry::count(crate::telemetry::Event::DialogAsked);
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
        (agents, connections, store),
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

/// The agent in `terminal` ran a tool, or took a prompt somebody typed: it works again, so the
/// waiting draft its last turn left is stale. A stage that lagged behind (his input came
/// before the wait began) cannot keep it open.
pub(crate) async fn working_again(
    terminal: u32,
    at: i64,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let closed = store
        .run(move |s| s.close_waiting_drafts(i64::from(terminal), at))
        .await;
    if matches!(closed, Ok(true)) {
        connections.asks_changed();
    }
}

/// A card closed because its answer was told in the chat: the head names the card, and the tab
/// it was typed in is no matter (qass relays to the lead as often as to the worker the card is
/// about). An `open` card, or a `passed` one (his inbox answer reached qass, and this is qass
/// passing it on); never a `queued` one, which the daemon still has to type. The card's agent
/// stops waiting too when it still waits on that question ([`qoop_store::Store::relay_answered`]),
/// so no board says it waits on him after he answered.
pub(crate) async fn relayed(
    label: Option<&str>,
    typed_in: u32,
    agents: &AgentRegistry,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let Some(card) = label.and_then(card_in_prompt) else {
        return;
    };
    let ended = store
        .run(move |s| {
            let moved = s.relay_answered(&card, i64::from(typed_in), now())?;
            let closed = match s.ask(&card)? {
                Some(row) if matches!(row.status.as_str(), "open" | "passed") => {
                    s.close_ask(&card, "answered in chat", now())?
                }
                _ => false,
            };
            Ok::<_, qoop_store::StoreError>((moved, closed))
        })
        .await;
    let Ok((moved, closed)) = ended else {
        return;
    };
    if let Some(terminal) = moved.and_then(|t| u32::try_from(t).ok()) {
        agents.tasks_changed();
        agents.announce(terminal);
    }
    if moved.is_some() || closed {
        connections.asks_changed();
    }
}

/// `E7` (qass's card) or `W12` (the daemon's draft) from a prompt head that starts `The owner
/// answered E7`, matched whole so it never reads E70 as E7.
fn card_in_prompt(head: &str) -> Option<String> {
    card_after(
        untagged(head.trim_start()).trim_start(),
        "The owner answered ",
    )
}

/// The words `qoop prompt` put before what another tab wrote: `[from <name>, tab <n>]`, so the
/// agent reading it knows who wrote and where to answer. `name` comes in already plain.
pub fn sender_tag(name: &str, tab: u32) -> String {
    format!("[from {name}, tab {tab}]")
}

/// A prompt head without the [`sender_tag`] in front of it, if one is there: what follows is
/// matched as the sender wrote it.
fn untagged(head: &str) -> &str {
    let Some(rest) = head.strip_prefix("[from ") else {
        return head;
    };
    rest.split_once("] ")
        .filter(|(inside, _)| {
            inside
                .rsplit_once(", tab ")
                .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map_or(head, |(_, after)| after)
}

/// The card a head names right after `prefix`, matched whole.
fn card_after(head: &str, prefix: &str) -> Option<String> {
    let rest = head.strip_prefix(prefix)?;
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
        AskOp::Add {
            card,
            agent,
            body,
            via_qass,
            replaces,
        } => {
            if let Err(reason) = qass_speaks(agents, &caller) {
                return failed(id, reason);
            }
            match add(&card, agent, *body, (via_qass, &replaces), terminals, store).await {
                Ok(AskResult::Added) => {
                    crate::telemetry::count(crate::telemetry::Event::QassAsked);
                    AskResult::Added
                }
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
        AskOp::Close { card, note } => {
            if let Err(reason) = qass_speaks(agents, &caller) {
                return failed(id, reason);
            }
            match close(&card, &note, store).await {
                Ok(AskResult::Closed) => {
                    crate::telemetry::count(crate::telemetry::Event::ClosedByQass);
                    AskResult::Closed
                }
                Ok(result) => result,
                Err(reason) => return failed(id, reason),
            }
        }
        AskOp::Dismiss { card } => {
            if caller.terminal.is_some() {
                return failed(id, "a card is put away by the owner, not from a terminal");
            }
            match close(&card, "dismissed", store).await {
                Ok(AskResult::Closed) => {
                    crate::telemetry::count(crate::telemetry::Event::Dismissed);
                    AskResult::Closed
                }
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
    (via_qass, replaces): (bool, &[String]),
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
    // With no conversation on the card, his answer routes to qass (`route`), not the agent.
    let session = terminal
        .filter(|_| !via_qass)
        .and_then(|t| terminals.session_of(t));
    let json = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let (card, replaces) = (card.to_string(), replaces.to_vec());
    store
        .run(move |s| {
            let at = now();
            let added = s.add_ask(
                &card,
                agent.as_deref(),
                terminal.map(i64::from),
                session.as_deref(),
                &json,
                at,
            )?;
            // After the add succeeded, so a refused card ends nothing.
            s.supersede_asks(&card, &replaces, at)?;
            Ok(added)
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
pub(crate) fn clean_text(text: &str) -> Option<String> {
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

/// The id of the space named `qass`, when it exists.
pub(crate) async fn qass_space(store: &StoreHandle) -> Option<i64> {
    let spaces = tokio::time::timeout(Duration::from_secs(2), store.workspaces())
        .await
        .ok()?
        .ok()?;
    spaces
        .into_iter()
        .find(|w| w.name == QASS_SPACE)
        .map(|w| w.id)
}

/// qass's own terminal: the first agent in the space named `qass`, other than the nightly
/// learning run, which runs there too and is never qass.
pub(crate) async fn qass_terminal(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> Option<u32> {
    let qass = qass_space(store).await?;
    let learn = crate::paths::learn_dir();
    let mut mine: Vec<u32> = terminals
        .streams()
        .into_iter()
        .filter(|t| terminals.workspace_of(*t) == Some(qass))
        .filter(|t| {
            !learn.as_ref().is_some_and(|dir| {
                terminals
                    .cwd_of(*t)
                    .is_some_and(|cwd| std::path::Path::new(&cwd).starts_with(dir))
            })
        })
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

/// The sleeping tab that is qass: the lowest-numbered one of the `qass` space that held an
/// agent, the order [`qass_terminal`] picks a running one in. Its space with it.
pub(crate) async fn sleeping_qass(store: &StoreHandle) -> Option<(i64, u32)> {
    let space = qass_space(store).await?;
    let asleep = store.asleep().await.ok()?;
    let rows = store.open_terminals(space).await.ok()?;
    asleep
        .into_iter()
        .filter(|(_, workspace, _)| *workspace == space)
        .filter(|(tab, _, _)| rows.iter().any(|r| r.id == *tab && r.agent.is_some()))
        .filter_map(|(tab, _, _)| u32::try_from(tab).ok())
        .min()
        .map(|tab| (space, tab))
}

/// What qass is told when the owner turns its watching back on.
const WATCHING_ON: &str = "watching on: start your watcher again (qass skill, Start step 3) and give the owner one summary of the fleet now; the watcher says only what changes from here.";

/// How long the flusher keeps trying to tell qass its watching is on again, from the switch or
/// from the last time qass was seen working.
const WATCH_TELL_FOR: Duration = Duration::from_secs(30 * 60);

/// The oldest switch a daemon start or a working qass still tells qass of: a qass on an agent
/// with no watcher never marks the stamp seen, and must not hear of it days later.
const WATCH_TELL_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

/// Since when qass is to be told its watching is on again, while it has not been.
static WATCH_TELL: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Tell qass its watching was turned back on: the inbox's flusher types the line once qass
/// has a tab and takes words, between the answers it types, for 30 minutes, longer while qass
/// works. The caller wakes a qass that sleeps; one gone or stuck longer starts its watcher at
/// its next start or message instead (the skill). `again`: asked again while it waits, so the
/// told stamp is forgotten and qass told again, under the same lock the flusher keeps it under.
pub(crate) fn tell_qass_watching(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
    again: bool,
) {
    {
        let mut tell = WATCH_TELL.lock().unwrap_or_else(|e| e.into_inner());
        if again {
            let _ = crate::paths::set_qass_watch_told(None);
        }
        *tell = Some(Instant::now());
    }
    kick(terminals, agents, connections, store);
}

/// At a daemon start, a line the last process did not type (it moved onto a new binary or
/// died before qass came to rest) is told after all. Only 30 seconds in: who typed in qass is
/// not carried over a move, and his draft there must not be typed over.
pub(crate) fn tell_qass_watching_after_start(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    if !untold_recent() {
        return;
    }
    let (terminals, agents, connections, store) = (
        terminals.clone(),
        agents.clone(),
        connections.clone(),
        store.clone(),
    );
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(TYPED_QUIET_SECS.unsigned_abs())).await;
        if untold_recent() {
            tell_qass_watching(&terminals, &agents, &connections, &store, false);
        }
    });
}

/// How often the daemon looks whether qass's watcher still runs.
const HEAL_EVERY: Duration = Duration::from_secs(60);
/// Looks in a row that found no watcher before qass is told: a watcher starting or between two
/// runs (it ends itself every 29 minutes and qass starts the next) is not taken for gone.
const HEAL_AFTER: u32 = 2;
/// The least time between two tells: a qass that does not start its watcher is not typed to
/// every two minutes.
const HEAL_GAP: Duration = Duration::from_secs(30 * 60);
/// How long qass may be away from rest before it counts as stuck, on the heal's own clock of
/// when it last saw qass at rest. Plain working does not count, or every claim would flip
/// between checked and waiting on each qass turn. Apart from [`HEAL_GAP`], which only spaces
/// tells.
const STUCK_AFTER_SECS: i64 = 30 * 60;
/// A tell after which no watcher came within this long, in seconds, failed.
const TELL_FAILS_AFTER: i64 = 10 * 60;
/// Failed tells in a row that raise the owner's card: about 90 minutes of tells.
const CARD_AFTER: u32 = 3;

/// qass's watcher alive and qass not stuck, as the heal's last look found them. With the
/// switch, whether a claim can be checked ([`qass_checks`]).
static CHECKER_UP: AtomicBool = AtomicBool::new(false);
/// What [`announce_checks`] last told the board.
static CHECKS_TOLD: AtomicBool = AtomicBool::new(false);

/// Whether qass can check a done claim on this Mac now: watching on, its watcher alive, and
/// qass not stuck. The daemon's one snapshot; each tab adds its own skip and liveness.
pub(crate) fn qass_checks() -> bool {
    crate::paths::qass_watching() && CHECKER_UP.load(Ordering::SeqCst)
}

/// A change in [`qass_checks`] changes the status of every claim with no task written, so the
/// board is told as a task write would tell it.
pub(crate) fn announce_checks(agents: &AgentRegistry) {
    announce_change(&CHECKS_TOLD, qass_checks(), agents);
}

/// Tells the board when `now` is not what `told` holds, and keeps `now` there.
fn announce_change(told: &AtomicBool, now: bool, agents: &AgentRegistry) {
    if told.swap(now, Ordering::SeqCst) != now {
        agents.tasks_changed();
    }
}

/// qass's watcher lives in the Monitor of its Claude session. A session that restarts or
/// resumes takes it down, and with no watcher no event comes to wake qass to start one again
/// (2026-10-24: 16 hours with none). So every minute: watching on, a qass on Claude here at
/// rest or stuck, and no watcher on [`HEAL_AFTER`] looks in a row: the switch is renewed and
/// qass told, as `qoop qass watch off` then `on` would. An agent of another kind has no
/// Monitor, so it is never told: it watches when the owner writes to it (the skill).
///
/// The same look keeps the check snapshot ([`qass_checks`]) and counts tries that brought no
/// working checker: [`CARD_AFTER`] in a row give the owner one card, since a heal that fails
/// quietly is as silent as no heal. The card closes once qass can check again or the switch
/// is off.
pub(crate) fn heal_qass_watcher(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) {
    let (terminals, agents, connections, store) = (
        terminals.clone(),
        agents.clone(),
        connections.clone(),
        store.clone(),
    );
    tokio::spawn(async move {
        let mut heal = Heal {
            rested: crate::agents::now_unix(),
            // A card a past daemon raised is closed at the first look that finds qass fine.
            unwatched: Unwatched {
                raised: true,
                ..Unwatched::default()
            },
            ..Heal::default()
        };
        loop {
            // The snapshot is read from the first look, so a daemon start does not show every
            // claim waiting for a minute.
            let look = look_at_qass(&terminals, &agents, &store).await;
            let step = heal.step(look, crate::agents::now_unix(), || {
                crate::paths::renew_qass_watch().unwrap_or(false)
            });
            CHECKER_UP.store(step.checker_up, Ordering::SeqCst);
            announce_checks(&agents);
            if step.told {
                crate::say!("qoopd: qass has no watcher; told it to start one");
                tell_qass_watching(&terminals, &agents, &connections, &store, false);
            } else if step.tried {
                crate::say!("qoopd: qass is stuck mid-turn with its watcher running");
            }
            if step.failed {
                crate::say!(
                    "qoopd: qass could not check 10 minutes after the try ({} in a row)",
                    heal.unwatched.failed
                );
            }
            // The old card is closed before a new one is raised, so a new run never lands on
            // it; each is tried again at the next look until the store takes it.
            if step.cleared && close_unwatched(&connections, &store).await {
                heal.close_due = false;
            }
            if heal.raise_due
                && !heal.close_due
                && let Some(since) = heal.unwatched.since
                && raise_unwatched(since, &connections, &store).await
            {
                heal.raise_due = false;
            }
            tokio::time::sleep(HEAL_EVERY).await;
        }
    });
}

/// What the heal keeps between looks, on the unix clock.
#[derive(Clone, Copy, Debug, Default)]
struct Heal {
    /// Looks in a row that found qass not watching, for the tell; a tell starts it over.
    gone: u32,
    /// Looks in a row that found no watcher, for the snapshot; a tell never starts it over.
    no_watcher: u32,
    /// The last try: a tell, or the look a stuck qass with a watcher would have been told.
    tried: Option<i64>,
    /// When qass was last seen at rest, or not there at all: the clock that says stuck.
    rested: i64,
    unwatched: Unwatched,
    /// A run with a card ended and the card is not closed yet: a store that failed the close
    /// is tried again at every look until it does.
    close_due: bool,
    /// This run's card is due and not written yet; it waits for [`Heal::close_due`], so it
    /// never lands on the old card, and goes with the run.
    raise_due: bool,
}

/// What one look decided.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Step {
    /// A watcher alive, or gone for no more than [`HEAL_AFTER`] looks (it ends every 29
    /// minutes and qass starts the next), and qass not stuck: what [`CHECKER_UP`] keeps.
    checker_up: bool,
    /// qass is told to start its watcher.
    told: bool,
    /// A try: a tell, or a stuck qass with its watcher running, which no line reaches.
    tried: bool,
    /// A try before this look left qass unable to check.
    failed: bool,
    /// The owner's card is due and no old card is left to close first.
    raise: bool,
    /// A run with a card ended (qass can check again, or the switch is off) and its card is
    /// not closed yet: close it.
    cleared: bool,
}

impl Heal {
    /// One look at `now`. `renew` renews the switch for a tell and says whether it did: a
    /// switch turned off since is not told.
    fn step(&mut self, look: Look, now: i64, renew: impl FnOnce() -> bool) -> Step {
        if look.rested {
            self.rested = now;
        }
        let stuck = now - self.rested >= STUCK_AFTER_SECS;
        self.no_watcher = if look.alive { 0 } else { self.no_watcher + 1 };
        let since_tried = self
            .tried
            .map(|at| Duration::from_secs(u64::try_from(now - at).unwrap_or(0)));
        let due;
        (self.gone, due) = heal_step(self.gone, not_watching(&look, stuck), since_tried);
        // A running watcher is not told to start: the try is only counted.
        let told = due && !look.alive && renew();
        let tried = told || (due && look.alive);
        if tried {
            self.tried = Some(now);
        }
        let settled = (look.alive && !stuck) || !look.watching;
        let was = self.unwatched;
        let raise;
        (self.unwatched, raise) = unwatched_step(self.unwatched, settled, tried, now);
        self.close_due |= settled && was.raised;
        self.raise_due = (self.raise_due || raise) && self.unwatched.raised;
        Step {
            checker_up: (look.alive || self.no_watcher <= HEAL_AFTER) && !stuck,
            told,
            tried,
            failed: self.unwatched.failed > was.failed,
            raise: self.raise_due && !self.close_due,
            cleared: self.close_due,
        }
    }
}

/// One look: the looks in a row that found no watcher, and whether qass is to be told now.
fn heal_step(gone: u32, missing: bool, since_told: Option<Duration>) -> (u32, bool) {
    if !missing {
        return (0, false);
    }
    let gone = gone + 1;
    let tell = gone >= HEAL_AFTER && since_told.is_none_or(|ago| ago >= HEAL_GAP);
    (if tell { 0 } else { gone }, tell)
}

/// What one heal look finds of qass.
#[derive(Clone, Copy, Debug, Default)]
struct Look {
    /// The owner's switch is on.
    watching: bool,
    /// Its watcher runs. Only looked at while watching.
    alive: bool,
    /// qass is a Claude agent here: the kind with a Monitor to run a watcher in.
    claude: bool,
    /// qass is at rest, or there is no qass here to be stuck.
    rested: bool,
}

async fn look_at_qass(
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> Look {
    let watching = crate::paths::qass_watching();
    let alive = watching && crate::paths::qass_watcher_alive();
    let qass = qass_terminal(terminals, agents, store).await;
    let claude = qass.and_then(|q| agents.agent(q)).is_some_and(|a| {
        matches!(&a.identity, qoop_domain::AgentIdentity::Known { program, .. } if program == "claude")
    });
    let rested = qass.is_none_or(|q| standing(agents, terminals, q).is_some_and(|s| s.at_rest()));
    Look {
        watching,
        alive,
        claude,
        rested,
    }
}

/// Watching on and qass a Claude agent here that cannot watch: at rest with no watcher, or
/// stuck, watcher or not. A stuck qass takes no line until it rests, but nothing else would
/// start its watcher or end its turn either.
// ponytail: at rest is sampled once a minute, so a qass that only rests between looks reads
// stuck after 30 minutes; read the agent's own state changes if that shows up.
fn not_watching(look: &Look, stuck: bool) -> bool {
    look.watching && look.claude && (stuck || (!look.alive && look.rested))
}

/// The heal's count of tries that left qass unable to check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Unwatched {
    /// The last try not judged yet, at its unix time.
    tried: Option<i64>,
    /// Tries in a row after which qass still could not check [`TELL_FAILS_AFTER`] later.
    failed: u32,
    /// The first try since qass could last check: the time the card names.
    since: Option<i64>,
    /// The card was raised for this run, so the end of the run closes it.
    raised: bool,
}

/// One look: `settled` (qass can check, or the switch is off) ends the run; a try
/// [`TELL_FAILS_AFTER`] old with qass still unable failed; `tried` is the next to judge.
/// True on the [`CARD_AFTER`]th failure in a row, the one that raises the card: later ones
/// add none.
fn unwatched_step(u: Unwatched, settled: bool, tried: bool, now: i64) -> (Unwatched, bool) {
    if settled {
        return (Unwatched::default(), false);
    }
    let mut u = u;
    let mut raise = false;
    if u.tried.is_some_and(|at| now - at >= TELL_FAILS_AFTER) {
        u.tried = None;
        u.failed += 1;
        raise = u.failed == CARD_AFTER;
        u.raised |= raise;
    }
    if tried {
        u.tried = Some(now);
        u.since.get_or_insert(now);
    }
    (u, raise)
}

/// The owner's card for a qass that could not check after [`CARD_AFTER`] tries. None over one
/// still live. False when the store failed, so the caller tries again.
async fn raise_unwatched(
    since: i64,
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) -> bool {
    let body = serde_json::to_string(&unwatched_card(since, crate::schedule::system_zone()));
    let Ok(body) = body else {
        return false;
    };
    match store.run(move |s| s.add_notice(&body, now())).await {
        Ok(card) => {
            if let Some(card) = card {
                crate::say!("qoopd: qass is not watching; the owner has card {card}");
                connections.asks_changed();
            }
            true
        }
        Err(e) => {
            crate::say!(
                "qoopd: the card for qass not watching was not written: {}",
                why(e)
            );
            false
        }
    }
}

/// The handle of the card [`unwatched_card`] writes.
const UNWATCHED_HANDLE: &str = "qass";

/// qass can check again, or the switch is off: its open card says nothing true any more.
/// False when the store failed, so the caller tries again.
pub(crate) async fn close_unwatched(
    connections: &Arc<ConnectionRegistry>,
    store: &StoreHandle,
) -> bool {
    match store
        .run(|s| {
            s.close_notice(
                UNWATCHED_HANDLE,
                "qass checks again, or watching is off",
                now(),
            )
        })
        .await
    {
        Ok(closed) => {
            if closed {
                crate::say!("qoopd: qass can check again; its card is closed");
                connections.asks_changed();
            }
            true
        }
        Err(e) => {
            crate::say!(
                "qoopd: the card for qass not watching was not closed: {}",
                why(e)
            );
            false
        }
    }
}

/// "qass is not watching since 09:05", on the wall clock of `zone`. No options: his words go
/// to qass, which is what can start its watcher.
fn unwatched_card(since: i64, zone: crate::schedule::Zone) -> AskCard {
    AskCard {
        handle: UNWATCHED_HANDLE.to_string(),
        urgency: "now".to_string(),
        asked: None,
        doing: "watching your agents".to_string(),
        why: format!(
            "{CARD_AFTER} tries in a row, 30 minutes apart, found no watcher or qass stuck mid-turn"
        ),
        asks: format!("qass is not watching since {}", (zone.local)(since).clock()),
        options: Vec::new(),
        pick: None,
    }
}

/// Whether a switch under [`WATCH_TELL_MAX_AGE`] old is still untold.
fn untold_recent() -> bool {
    crate::paths::qass_watch_untold()
        .and_then(|stamp| crate::paths::qass_watch_age(&stamp))
        .is_some_and(|age| age < WATCH_TELL_MAX_AGE)
}

/// Whether the line is still to be typed: asked for, or qass last seen working, under 30
/// minutes ago, and the switch still waits for it (not off again, no watcher started since, not told yet).
fn watch_tell_due() -> bool {
    let mut tell = WATCH_TELL.lock().unwrap_or_else(|e| e.into_inner());
    if tell.is_some_and(|at| {
        at.elapsed() >= WATCH_TELL_FOR || crate::paths::qass_watch_untold().is_none()
    }) {
        *tell = None;
    }
    tell.is_some()
}

/// One look at the watch line, in the flusher's turn: typed when qass takes words and was not
/// typed to this round or just before.
async fn flush_watch(seen: &HashSet<u32>, sent: &mut HashMap<u32, Instant>, hands: Hands<'_>) {
    let (terminals, agents, _, store) = hands;
    if !watch_tell_due() {
        return;
    }
    // No tab yet, or the store slow to answer: looked at again next round, within the 30 minutes.
    let Some(qass) = qass_terminal(terminals, agents, store).await else {
        return;
    };
    let now = standing(agents, terminals, qass)
        .map(|s| (qass, s, agents.typed_ago(qass, crate::agents::now_unix())));
    if !takes_words(Route::Qass(qass), None, now.as_ref()) {
        // A turn ends by itself: the 30 minutes start again while qass works, an answer queued
        // for it or not. Blocked or starting may never end, and are left to run out.
        if now.as_ref().is_some_and(|(_, s, _)| s.state == "working") && untold_recent() {
            let mut tell = WATCH_TELL.lock().unwrap_or_else(|e| e.into_inner());
            if tell.is_some() {
                *tell = Some(Instant::now());
            }
        }
        return;
    }
    if seen.contains(&qass) || sent.get(&qass).is_some_and(|at| at.elapsed() < SETTLE) {
        return;
    }
    // Looked at again after the wait for the store: an off since then is not typed over. Kept
    // as told before it is typed, as a card is flipped before its answer goes, so a move onto a
    // new binary mid-type cannot tell it twice; and taken off the flusher either way, so a write
    // that fails does not type it every round.
    let (kept, was) = {
        let mut tell = WATCH_TELL.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = tell
            .filter(|at| at.elapsed() < WATCH_TELL_FOR)
            .and_then(|_| crate::paths::qass_watch_untold());
        let Some(stamp) = stamp else {
            *tell = None;
            return;
        };
        (
            crate::paths::set_qass_watch_told(Some(&stamp)).is_ok(),
            tell.take(),
        )
    };
    if type_into(terminals, qass, WATCHING_ON).await.is_ok() {
        sent.insert(qass, Instant::now());
        return;
    }
    // Not typed: forgotten again with the time it had left, unless an on since then asked anew.
    let mut tell = WATCH_TELL.lock().unwrap_or_else(|e| e.into_inner());
    if kept && tell.is_none() && crate::paths::qass_watch_untold().is_none() {
        let _ = crate::paths::set_qass_watch_told(None);
        *tell = was;
    }
}

/// Type `text`, a pause, then Enter, into a terminal.
pub(crate) async fn type_into(
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
    crate::paths::note_typed(terminal, "daemon", text);
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
            let waiting = waiting.is_ok_and(|rows| !rows.is_empty()) || watch_tell_due();
            if !waiting || FLUSHING.swap(true, Ordering::SeqCst) {
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
        if rows.is_empty() && !watch_tell_due() && sent.values().all(|at| at.elapsed() >= SETTLE) {
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
        flush_watch(
            &seen,
            &mut sent,
            (&terminals, &agents, &connections, &store),
        )
        .await;
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
    match type_into(terminals, target, &line).await {
        Err(reason) => give_back(reason).await,
        // The owner's answer reached the agent; one passed to qass has not yet.
        Ok(_) if matches!(route, Route::Worker(_)) => {
            crate::telemetry::count(crate::telemetry::Event::Answered {
                waited: now() - row.opened_at,
            });
        }
        Ok(_) => {}
    }
}

#[cfg(test)]
mod tests {

    /// qass is told only after two looks in a row found no watcher, then not again for 30
    /// minutes; a watcher seen in between starts the count over.
    #[test]
    fn a_gone_watcher_is_told_after_two_looks_and_not_again_soon() {
        assert_eq!(heal_step(0, false, None), (0, false));
        assert_eq!(heal_step(0, true, None), (1, false));
        assert_eq!(heal_step(1, true, None), (0, true));
        assert_eq!(heal_step(1, false, None), (0, false));
        let soon = Some(Duration::from_secs(60));
        assert_eq!(heal_step(1, true, soon), (2, false));
        assert_eq!(heal_step(5, true, soon), (6, false));
        assert_eq!(heal_step(5, true, Some(HEAL_GAP)), (0, true));
    }

    /// qass on Claude here, watching on, at rest or not, its watcher alive or not.
    fn qass(alive: bool, rested: bool) -> Look {
        Look {
            watching: true,
            alive,
            claude: true,
            rested,
        }
    }

    /// The heal over `minutes` looks a minute apart, every tell renewed: the minutes that
    /// raised the owner's card.
    fn cards(
        heal: &mut Heal,
        look: impl Fn(i64) -> Look,
        minutes: std::ops::Range<i64>,
    ) -> Vec<i64> {
        minutes
            .filter(|m| look_ok(heal, look(*m), m * 60).raise)
            .collect()
    }

    /// One look whose card writes go through, as the loop clears them.
    fn look_ok(heal: &mut Heal, look: Look, now: i64) -> Step {
        let step = heal.step(look, now, || true);
        if step.cleared {
            heal.close_due = false;
        }
        if step.raise {
            heal.raise_due = false;
        }
        step
    }

    /// The five ways a claim has nobody to check it each show `waits`, and each shows `checks`
    /// again once fixed: watching off, a dead watcher, qass stuck (30 minutes not at rest on
    /// the heal's clock), a tab qass leaves alone and an asleep tab. A claim with no tab at all
    /// shows `checks` whatever the watcher does: qass closes it at its Start.
    #[test]
    fn qass_checks_a_claim_only_while_a_check_can_happen() {
        use crate::control::{Live, Status, StatusInput, status, tab_of};
        // Looks a minute apart from a rest at 0, at least four, to `since_rest`.
        let up = |alive: bool, since_rest: i64| {
            let mut heal = Heal::default();
            (0..=(since_rest / 60).max(3))
                .map(|m| heal.step(qass(alive, m == 0), m * 60, || false))
                .last()
                .is_some_and(|step| step.checker_up)
        };
        let claim = |watching: bool, alive: bool, since_rest: i64, skipped: bool, tab: &str| {
            let live = (tab == "live").then_some(Live {
                state: "idle",
                last_active: Some(5),
                subagents: 0,
            });
            let asleep = std::collections::BTreeMap::from_iter((tab == "asleep").then_some((7, 3)));
            // `qass_checks` is the switch and the heal's snapshot; the daemon adds the skip.
            let checks = watching && up(alive, since_rest) && !skipped;
            status(StatusInput {
                claimed_at: Some(4),
                stage: "implementing",
                updated_at: 6,
                tab: tab_of(live, &asleep, 7, checks),
                ..StatusInput::default()
            })
        };
        let fine = (true, true, 60, false, "live");
        assert_eq!(
            claim(fine.0, fine.1, fine.2, fine.3, fine.4),
            (Status::Checks, 4)
        );
        let broken = [
            ("watch off", (false, true, 60, false, "live")),
            ("dead watcher", (true, false, 60, false, "live")),
            ("qass stuck", (true, true, STUCK_AFTER_SECS, false, "live")),
            ("skipped", (true, true, 60, true, "live")),
            ("asleep tab", (true, true, 60, false, "asleep")),
        ];
        for (why, (w, a, r, s, t)) in broken {
            assert_eq!(claim(w, a, r, s, t), (Status::Waits, 4), "{why}");
        }
        // No tab at all: qass closes it failed at its Start, watcher or not.
        assert_eq!(
            claim(false, false, 60, false, "gone"),
            (Status::Checks, 4),
            "no tab"
        );
        assert_eq!(
            claim(true, true, STUCK_AFTER_SECS - 1, false, "live").0,
            Status::Checks,
            "working, not stuck yet"
        );
        // Fixed again: a qass back at rest resets the stuck clock.
        let mut heal = Heal::default();
        assert!(
            !heal
                .step(qass(true, false), STUCK_AFTER_SECS, || false)
                .checker_up
        );
        assert!(
            heal.step(qass(true, true), STUCK_AFTER_SECS + 60, || false)
                .checker_up
        );
    }

    /// The watcher ends every 29 minutes and qass starts the next: a gap of a minute or two,
    /// seen by up to [`HEAL_AFTER`] looks, keeps every claim checked. A third look with no
    /// watcher does not.
    #[test]
    fn a_minute_or_two_between_watchers_changes_no_claim() {
        let mut heal = Heal::default();
        let mut up = |m: i64, alive: bool| heal.step(qass(alive, true), m * 60, || true).checker_up;
        assert!(up(0, true));
        assert!(up(1, false), "one look in the gap");
        assert!(up(2, true));
        assert!(up(3, false) && up(4, false), "two looks in the gap");
        assert!(up(5, true));
        assert!(up(6, false) && up(7, false));
        assert!(!up(8, false), "gone for three looks");
        assert!(up(9, true), "back with the next watcher");
    }

    /// A change in what qass can check tells every client the board changed, with no task
    /// written; the same snapshot again tells nobody.
    #[test]
    fn a_change_in_what_qass_can_check_reaches_clients_with_no_task_write() {
        let agents = AgentRegistry::new().expect("registry");
        let mut changed = agents.subscribe_tasks();
        let told = AtomicBool::new(false);
        for (now, says) in [(false, false), (true, true), (true, false), (false, true)] {
            announce_change(&told, now, &agents);
            assert_eq!(changed.try_recv().is_ok(), says, "{now}");
        }
    }

    /// A killed watcher with qass at rest: told every 30 minutes, each tell failed after 10
    /// with no watcher, and the third failure raises the card, once, in under 90 minutes.
    /// Failures after it raise none.
    #[test]
    fn a_dead_watcher_raises_one_card_after_three_failed_tells() {
        let mut heal = Heal::default();
        let raised = cards(&mut heal, |_| qass(false, true), 0..400);
        assert_eq!(
            raised,
            vec![71],
            "tells at 1, 31 and 61, each failed 10 later"
        );
        assert!(heal.unwatched.failed > CARD_AFTER, "it went on failing");
        assert_eq!(
            heal.unwatched.since,
            Some(60),
            "the card names the first tell"
        );
    }

    /// A qass stuck working (30 minutes not at rest) is told like one at rest, though the line
    /// waits for it to rest, and its failed tells raise the card the same way.
    #[test]
    fn a_qass_stuck_working_with_no_watcher_raises_the_card() {
        let mut heal = Heal::default();
        let raised = cards(&mut heal, |_| qass(false, false), 0..400);
        assert_eq!(raised, vec![101], "stuck at 30, tells at 31, 61 and 91");
        // Working, not stuck, is not missing.
        let mut heal = Heal::default();
        assert!(cards(&mut heal, |_| qass(false, false), 0..29).is_empty());
        assert_eq!(heal.tried, None);
    }

    /// A qass stuck working with its watcher running cannot check either: each try is
    /// counted without a tell (its watcher is fine, the line would wait for its rest), the
    /// third failure raises the card, and its rest closes it.
    #[test]
    fn a_qass_stuck_working_with_its_watcher_running_raises_the_card() {
        let mut heal = Heal::default();
        let mut told = false;
        let raised: Vec<i64> = (0..120)
            .filter(|m| {
                let step = heal.step(qass(true, false), m * 60, || {
                    told = true;
                    true
                });
                heal.raise_due &= !step.raise;
                step.raise
            })
            .collect();
        assert_eq!(raised, vec![101], "stuck at 30, tries at 31, 61 and 91");
        assert!(!told, "a running watcher is never told to start");
        let step = heal.step(qass(true, true), 120 * 60, || true);
        assert!(step.cleared && step.checker_up, "{step:?}");
        assert_eq!(heal.unwatched, Unwatched::default());
    }

    /// A watcher seen again starts the count over: two failures, a live watcher, and the card
    /// needs three more. The switch off starts it over too. A card raised is closed by
    /// either, and a later run raises a new one.
    #[test]
    fn a_watcher_seen_again_resets_the_failed_tells() {
        let mut heal = Heal::default();
        assert!(cards(&mut heal, |_| qass(false, true), 0..50).is_empty());
        assert_eq!(heal.unwatched.failed, 2);
        assert!(!heal.step(qass(true, true), 50 * 60, || true).raise);
        assert_eq!(heal.unwatched, Unwatched::default());
        let raised = cards(&mut heal, |_| qass(false, true), 51..400);
        assert_eq!(raised.len(), 1);
        assert!(raised[0] > 51 + 60, "three new failures: {raised:?}");
        let off = Look {
            watching: false,
            ..qass(false, true)
        };
        assert!(
            heal.step(off, 400 * 60, || true).cleared,
            "off closes the card"
        );
        heal.close_due = false;
        assert_eq!(heal.unwatched, Unwatched::default());
        assert!(!heal.step(off, 401 * 60, || true).cleared, "closed once");
        let raised = cards(&mut heal, |_| qass(false, true), 402..600);
        assert_eq!(raised.len(), 1, "a new run, a new card");
        assert!(heal.step(qass(true, true), 600 * 60, || true).cleared);
    }

    /// A close the store failed stays due at every look, through a new run of failures too.
    /// The new run's card waits for it, so it never lands on the old card, and once the close
    /// goes through it is raised at that same look. A raise the store failed is tried again.
    #[test]
    fn a_card_close_that_failed_is_tried_again() {
        let mut heal = Heal::default();
        assert_eq!(cards(&mut heal, |_| qass(false, true), 0..200).len(), 1);
        // From here every close fails: close_due stays as the step set it.
        assert!(heal.step(qass(true, true), 200 * 60, || true).cleared);
        assert!(
            heal.step(qass(true, true), 201 * 60, || true).cleared,
            "again"
        );
        for m in 202..400 {
            let step = heal.step(qass(false, true), m * 60, || true);
            assert!(step.cleared, "still due while a new run fails, minute {m}");
            assert!(!step.raise, "no card over the old one, minute {m}");
        }
        assert!(heal.raise_due, "the new run reached its card");
        // The close goes through; the card is raised at that look and, failed once, at the next.
        heal.close_due = false;
        assert!(heal.step(qass(false, true), 400 * 60, || true).raise);
        let step = look_ok(&mut heal, qass(false, true), 401 * 60);
        assert!(step.raise && !step.cleared, "{step:?}");
        assert!(
            !look_ok(&mut heal, qass(false, true), 402 * 60).raise,
            "raised once"
        );
        // The run ends: its own card is closed.
        assert!(look_ok(&mut heal, qass(true, true), 403 * 60).cleared);
        assert!(!heal.raise_due && !heal.close_due);
    }

    /// The card says since when, on the wall clock, in values the card format takes.
    #[test]
    fn the_card_says_qass_is_not_watching_since_its_first_tell() {
        let utc = crate::schedule::Zone {
            local: crate::schedule::Naive::from_secs,
            unix: crate::schedule::Naive::secs,
        };
        let card = unwatched_card(9 * 3600 + 5 * 60, utc);
        assert_eq!(card.asks, "qass is not watching since 09:05");
        // Like a draft it has no options; every other rule of the format holds.
        let problems = card.problems();
        assert!(
            problems.iter().all(|p| p.contains("options")),
            "{problems:?}"
        );
    }

    use super::*;

    /// The three turn ends an agent left him, two with no "?", ask him;
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
            "Hazır. Başka bir şey gerekmiyorsa demen yeter.",
            "Hazır, demen yeterli.",
            "Sıradaki işi başlatırım, istiyorsan söyle.",
            "Sıradaki işi başlatırım, İstiyorsan söyle.",
            "Siradaki isi baslatirim, istiyorsan soyle.",
            "Seçenekler yukarıda, numarayla cevapla.",
            "Silmemi istiyorsan \"sil\" de.",
            "Hepsini kapatmak için \"10'u da\" deyin.",
            "Devam için `evet` de, sonra açarım.",
            "Onay için “tamam” de",
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
            "Bulundu: kaynağında \"zaten ücretsiz\" de çıkıyor.",
            "Ron \"tamam\" deyince test eder.",
            "Düzelttim, şöyle yaptım.",
        ] {
            assert!(!asks_owner(not), "{not}");
        }
    }

    #[test]
    fn qass_setting_it_to_other_work_answers_nothing() {
        assert!(not_his(
            "While E7 waits on the owner: do the parts it does not hold up."
        ));
        assert!(not_his("While W12 waits on the owner: go on."));
        assert!(!not_his("While E2E tests run, fix the lint."));
        assert!(!not_his("While E7 runs, look at the logs."));
        assert!(!not_his("The owner answered E7: \"B\"."));
        assert!(not_his("<task-notification>done</task-notification>"));
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
            card_in_prompt("The owner answered E7: \"billing B\"").as_deref(),
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
        // Relayed by qass from its tab, the line carries `qoop prompt`'s sender tag first.
        let tagged = format!("{} The owner answered E7: \"go\"", sender_tag("qass", 473));
        assert_eq!(card_in_prompt(&tagged).as_deref(), Some("E7"));
        assert!(not_his(&format!(
            "{} While E7 waits on the owner: do the rest",
            sender_tag("qass", 473)
        )));
        assert_eq!(
            untagged("[from x] The owner answered E7"),
            "[from x] The owner answered E7"
        );
        assert!(not_his(
            "[from qass, tab 473]  While E7 waits on the owner: go"
        ));
        assert_eq!(draft_handle("Api-local PR 513", 52), "api52");
        assert_eq!(draft_handle("ödeme", 3), "deme3");
        assert_eq!(draft_handle("***", 4), "agent4");
        assert_eq!(draft_handle("Api-local PR 513", "-t3"), "api-t3");
    }

    #[test]
    fn the_worker_is_told_what_qass_tells_it_today() {
        assert_eq!(
            line_for_worker("E7", "billing B", Some(("b", "Do not run 0042 yet."))),
            "The owner answered E7: \"billing B\". He chose B: Do not run 0042 yet. Those are the words of the owner: do only what they allow, and ask before anything more."
        );
        assert_eq!(
            line_for_worker("E7", "use the old column", None),
            "The owner answered E7: \"use the old column\". Those are the words of the owner: do only what they allow, and ask before anything more."
        );
        assert_eq!(line_for_qass("billing", Some("b"), None), "billing B");
        assert_eq!(
            line_for_qass("billing", None, Some("run it twice")),
            "billing run it twice"
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
        let body = r#"{"handle":"billing","urgency":"now","doing":"d","why":"w","asks":"a","options":[{"key":"b","label":"B","reason":"r","words":"Do not run 0042 yet."}]}"#;
        let mut row = card(None);
        row.body = body.into();
        row.card = "E7".into();
        row.option = Some("b".into());
        row.answer = Some("b".into());
        assert_eq!(line_for(&row, Route::Qass(9)).as_deref(), Some("billing B"));
        assert!(
            line_for(&row, Route::Worker(5))
                .unwrap()
                .contains("He chose B: Do not run 0042 yet")
        );
        row.option = None;
        row.answer = Some("run it twice".into());
        assert_eq!(
            line_for(&row, Route::Qass(9)).as_deref(),
            Some("billing run it twice")
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

    /// A terminal 7 on a Mac with qass, its task opened as an edit opens one (the owner's cut
    /// prompt for a goal) and waiting on him since `waiting`.
    fn waiting_stack(goal: &str, waiting: i64) -> StoreHandle {
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_workspace(QASS_SPACE, 2).unwrap();
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).unwrap();
        s.begin_task(7, goal, None, waiting - 5).unwrap();
        assert!(s.wait_on_owner(7, waiting).unwrap().is_some());
        StoreHandle::new(s)
    }

    async fn live_cards(store: &StoreHandle) -> Vec<AskRow> {
        store.run(|s| s.live_asks()).await.unwrap()
    }

    /// The owner's own angry prompt is the goal of a task qoop opened when the agent changed
    /// files: the draft shows neither as the question nor as what it does. The agent's last
    /// question is the question; a goal somebody wrote stays the fallback.
    #[tokio::test]
    async fn a_draft_never_shows_the_owners_prompt_back_to_him() {
        let agents = AgentRegistry::new().expect("registry");
        let connections = Arc::new(ConnectionRegistry::default());
        let words = "this is not the average, you idiot, follow the real rate";
        let hands = (&agents, &connections);
        for question in [None, Some("Which rate table do I use?".to_string())] {
            let store = waiting_stack(words, 100);
            let made = draft_waiting(
                7,
                Some("gtm516".into()),
                question.clone(),
                None,
                (hands.0, hands.1, &store),
            )
            .await;
            assert!(made);
            let body = live_cards(&store).await.remove(0).body;
            assert!(!body.contains("idiot"), "{body}");
            let card: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                card["asks"].as_str().unwrap(),
                question.as_deref().unwrap_or("gtm516 waits on you")
            );
        }
        let store = waiting_stack(words, 100);
        store
            .run(|s| {
                let task = s.open_task_of(7)?.unwrap().id;
                s.set_task_goal(task, "Use the live rate; done when tests pass", 101)
                    .map(drop)
            })
            .await
            .unwrap();
        assert!(
            draft_waiting(
                7,
                Some("gtm516".into()),
                None,
                None,
                (&agents, &connections, &store)
            )
            .await
        );
        let body = live_cards(&store).await.remove(0).body;
        assert!(
            body.contains("gtm516 waits on you: Use the live rate"),
            "{body}"
        );
    }

    /// A prompt somebody typed reached the agent after its wait began, and its turn then ended
    /// with no question: it works again, so no draft. A notice the harness typed is no prompt.
    /// A tool or a prompt then closes a draft that stands.
    #[tokio::test]
    async fn an_agent_that_works_again_has_no_waiting_draft() {
        let agents = AgentRegistry::new().expect("registry");
        let connections = Arc::new(ConnectionRegistry::default());
        let event = |label: &str, at| crate::control::FocusEvent {
            kind: "user".into(),
            label: label.into(),
            at_unix: at,
        };
        let store = waiting_stack("fix the rate", 100);
        agents.push_focus_event(7, event("go on, use the live rate", 90));
        agents.push_focus_event(
            7,
            event("<task-notification>build done</task-notification>", 120),
        );
        let hands = (&agents, &connections, &store);
        assert!(
            draft_waiting(7, None, None, None, hands).await,
            "a prompt from before the wait"
        );
        working_again(7, now() + 5, &connections, &store).await;
        assert!(live_cards(&store).await.is_empty(), "it works again");

        let store = waiting_stack("fix the rate", 100);
        agents.push_focus_event(7, event("keep going", 130));
        assert!(!draft_waiting(7, None, None, None, (&agents, &connections, &store)).await);
        assert!(live_cards(&store).await.is_empty());
        // The question it ended its turn with is real whatever came before it.
        assert!(
            draft_waiting(
                7,
                None,
                Some("Merge it?".into()),
                None,
                (&agents, &connections, &store)
            )
            .await
        );
    }

    /// A plan that waited half an hour for qass: one draft card on the agent working on its item
    /// now, with no conversation (his answer goes to qass), its task waiting on him; told once
    /// for that version; and nothing at all on a Mac with no qass.
    #[tokio::test]
    async fn a_stale_plan_tells_the_owner_once_and_only_where_qass_lives() {
        let agents = AgentRegistry::new().expect("registry");
        let connections = Arc::new(ConnectionRegistry::default());
        let at = now() - qoop_store::plan::STALE_SECS - 60;
        let setup = |with_qass: bool| {
            let mut s = qoop_store::Store::in_memory().expect("store");
            let ws = s.create_workspace("work", 1).unwrap();
            if with_qass {
                s.create_workspace(QASS_SPACE, 2).unwrap();
            }
            s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).unwrap();
            let (item, _) = s.add_item(ws, "ship it", "", None, at).unwrap();
            let task = s.begin_task(7, "ship it", None, at).unwrap().0;
            s.link_task(task, item, at).unwrap();
            s.set_planfile(item, "/p/plan.md", b"v1", 7, at).unwrap();
            (StoreHandle::new(s), task)
        };

        let (store, task) = setup(true);
        stale_plans_once(&agents, &connections, &store).await;
        let (cards, stage) = store
            .run(move |s| Ok((s.live_asks()?, s.task(task)?.unwrap().stage)))
            .await
            .unwrap();
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!(cards[0].terminal, Some(7));
        assert_eq!(cards[0].session, None, "his answer goes to qass");
        assert!(
            cards[0].body.contains("plan waits for qass"),
            "{}",
            cards[0].body
        );
        assert_eq!(stage, "waiting-human");
        // Answered and gone, the worker back at work: only the mark keeps a second card away.
        let card = cards[0].card.clone();
        store
            .run(move |s| {
                s.close_ask(&card, "answered", now())?;
                s.set_task_stage(task, "implementing", now()).map(drop)
            })
            .await
            .unwrap();
        stale_plans_once(&agents, &connections, &store).await;
        let live = store.run(|s| s.live_asks()).await.unwrap();
        assert!(live.is_empty(), "told again: {live:?}");

        let (store, task) = setup(false);
        stale_plans_once(&agents, &connections, &store).await;
        let (cards, stage) = store
            .run(move |s| Ok((s.live_asks()?, s.task(task)?.unwrap().stage)))
            .await
            .unwrap();
        assert!(cards.is_empty(), "no qass, no card: {cards:?}");
        assert_ne!(stage, "waiting-human", "no qass, no stage moved");
    }

    /// A card read with the board item it is about: its key, its plan state and its phase, so
    /// a screen reads a plan card without the board.
    #[tokio::test]
    async fn a_card_is_read_with_its_items_plan_and_phase() {
        let agents = AgentRegistry::new().expect("registry");
        let connections = Arc::new(ConnectionRegistry::default());
        let at = now() - qoop_store::plan::STALE_SECS - 60;
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_workspace(QASS_SPACE, 2).unwrap();
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).unwrap();
        let (item, key) = s.add_item(ws, "ship it", "", None, at).unwrap();
        let task = s.begin_task(7, "ship it", None, at).unwrap().0;
        s.link_task(task, item, at).unwrap();
        let steps: Vec<String> = ["## Plan", "write it", "## Build", "code it"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        s.set_steps(item, &steps, at).unwrap();
        s.check_step(item, 2, true, at).unwrap();
        s.set_planfile(item, "/p/plan.md", b"v1", 7, at).unwrap();
        let store = StoreHandle::new(s);
        stale_plans_once(&agents, &connections, &store).await;
        let terminals = Arc::new(TerminalRegistry::default());
        let ControlResponse::Asks { live, .. } = read("r".into(), false, &terminals, &store).await
        else {
            panic!("no asks read");
        };
        assert_eq!(live.len(), 1, "{live:?}");
        assert_eq!(
            live[0].item,
            Some(crate::control::AskItem {
                id: item,
                key,
                // Its card is open now: written, no longer stale.
                plan: "written".into(),
                phase: Some(crate::control::PhaseInfo {
                    n: 2,
                    of: 2,
                    name: "Build".into(),
                }),
            })
        );
    }

    /// An ended card goes by its task only, since a terminal number is given again to a new
    /// tab; a live one goes by its terminal too. A remote card is never matched to an item here.
    #[tokio::test]
    async fn an_ended_card_by_its_terminal_and_a_remote_card_carry_no_item() {
        let at = now();
        let mut s = qoop_store::Store::in_memory().expect("store");
        let ws = s.create_workspace("work", 1).unwrap();
        s.create_workspace(QASS_SPACE, 2).unwrap();
        s.create_terminal(7, ws, "/tmp", "/bin/sh", 1).unwrap();
        let (item, key) = s.add_item(ws, "ship it", "", None, at).unwrap();
        let body = |h: &str| {
            format!(
                r#"{{"handle":"{h}","urgency":"later","doing":"d","why":"w","asks":"a","options":[{{"key":"a","label":"A","reason":"r","words":"go"}}]}}"#
            )
        };
        // Filed in terminal 7 before any task there: no task, the terminal only.
        s.add_ask("E1", None, Some(7), None, &body("old"), at)
            .unwrap();
        s.add_ask("E2", None, Some(7), None, &body("live"), at)
            .unwrap();
        s.close_ask("E1", "done", at).unwrap();
        // Terminal 7 now holds an agent linked to the item.
        let task = s.begin_task(7, "ship it", None, at).unwrap().0;
        s.link_task(task, item, at).unwrap();
        // A remote card whose task row is this one's: still no item here.
        s.add_ask("E3", Some("a/T2"), Some(7), None, &body("far"), at)
            .unwrap();
        let store = StoreHandle::new(s);
        let terminals = Arc::new(TerminalRegistry::default());
        let ControlResponse::Asks { live, ended, .. } =
            read("r".into(), true, &terminals, &store).await
        else {
            panic!("no asks read");
        };
        let of = |cards: &[AskInfo], card: &str| {
            cards
                .iter()
                .find(|c| c.card == card)
                .unwrap_or_else(|| panic!("{card}: {cards:?}"))
                .item
                .clone()
        };
        assert_eq!(of(&live, "E2").map(|i| i.key), Some(key));
        assert_eq!(of(&ended, "E1"), None);
        assert_eq!(of(&live, "E3"), None);
    }
}

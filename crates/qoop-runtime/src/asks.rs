//! The daemon's side of the question inbox: who may write a card, and where an answer goes.
//!
//! qass writes a card (`Add`) and ends one it has handled (`Close`), each with the token of a
//! terminal in its own space. The owner answers or dismisses from a client that speaks for no
//! terminal: any terminal's token is refused there, so a worker can neither forge a card nor
//! answer one. An answer is claimed once (`open` to `answered` or `passed`), then sent:
//!
//! 1. straight to the worker, when it is the same conversation the card was written about, at
//!    rest, and said which conversation that is;
//! 2. otherwise typed to qass as `<handle> <letter>` (the form qass already reads in the
//!    chat), but only while qass is idle and nobody is typing in its terminal.
//!
//! A send that fails gives the card back to `open`, so an answer is never lost silently.

use std::sync::Arc;
use std::time::Duration;

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
fn agent_key(agent: &str) -> Option<(String, Option<u32>)> {
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

/// `E7` from a prompt head that starts `The owner answered E7`, matched whole so it never
/// reads E70 as E7.
fn card_in_prompt(head: &str) -> Option<String> {
    let rest = head.strip_prefix("The owner answered E")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = rest[digits.len()..].chars().next();
    (!digits.is_empty() && after.is_none_or(|c| !c.is_alphanumeric())).then(|| format!("E{digits}"))
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
            match answer(&card, option, text, seen, terminals, agents, store).await {
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
            "{card} is not a card number: E<n> from escalate.sh next E"
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
    /// qass cannot be typed to now: the card stays open.
    Busy,
    /// qass has no tab.
    NoQass,
}

/// Route 1 when the worker is provably the conversation the card was written about and at
/// rest: the card has a session, the live row reports the same one, and the agent is idle or
/// done. A card without a session (an agent that reports none, or a remote one) never takes
/// it. Otherwise route 2, while qass is at rest and nobody typed into it lately.
pub(crate) fn route(
    row: &AskRow,
    worker: Option<&(u32, Standing)>,
    qass: Option<&(u32, Standing, Option<i64>)>,
) -> Route {
    if let Some((terminal, s)) = worker
        && row.session.is_some()
        && s.session == row.session
        && s.at_rest()
    {
        return Route::Worker(*terminal);
    }
    match qass {
        None => Route::NoQass,
        Some((terminal, s, typed_ago))
            if s.at_rest() && typed_ago.is_none_or(|ago| ago >= TYPED_QUIET_SECS) =>
        {
            Route::Qass(*terminal)
        }
        Some(_) => Route::Busy,
    }
}

async fn answer(
    card: &str,
    option: Option<String>,
    text: Option<String>,
    seen: Option<String>,
    terminals: &Arc<TerminalRegistry>,
    agents: &Arc<AgentRegistry>,
    store: &StoreHandle,
) -> Result<AskResult, String> {
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

    let worker = row
        .terminal
        .and_then(|t| u32::try_from(t).ok())
        .and_then(|t| standing(agents, terminals, t).map(|s| (t, s)));
    let qass = qass_terminal(terminals, agents, store).await;
    let qass_now = qass.and_then(|t| {
        standing(agents, terminals, t)
            .map(|s| (t, s, agents.typed_ago(t, crate::agents::now_unix())))
    });
    let (to, note, target, line) = match route(&row, worker.as_ref(), qass_now.as_ref()) {
        Route::Worker(terminal) => {
            let said = match (&key, &text) {
                (Some(k), _) => format!("{} {}", body.handle, k.to_uppercase()),
                (None, Some(t)) => t.clone(),
                (None, None) => unreachable!("checked above"),
            };
            let chose = chosen
                .filter(|_| text.is_none())
                .map(|o| (o.key.as_str(), o.words.as_str()));
            (
                "answered",
                "owner, inbox",
                terminal,
                line_for_worker(card, &said, chose),
            )
        }
        Route::Qass(terminal) => (
            "passed",
            "passed to qass",
            terminal,
            line_for_qass(&body.handle, key.as_deref(), text.as_deref()),
        ),
        Route::Busy => return Ok(AskResult::Busy),
        Route::NoQass => return Err("qass has no tab to hand this to".to_string()),
    };
    let said = text.clone().or_else(|| key.clone()).unwrap_or_default();
    let (wanted, option_key) = (card.to_string(), key);
    let claimed = store
        .run(move |s| s.claim_ask(&wanted, to, option_key.as_deref(), &said, note, now()))
        .await
        .map_err(why)?;
    let give_back = || {
        let back = card.to_string();
        async move {
            let _ = store.run(move |s| s.reopen_ask(&back)).await;
        }
    };
    // Rewritten between the read and the claim: the words he read are not the ones in the
    // card now, so nothing is sent.
    if claimed.body != row.body {
        give_back().await;
        return Err(format!("{card} changed since you read it; look again"));
    }
    if let Err(reason) = type_into(terminals, target, &line).await {
        give_back().await;
        return Err(format!("{reason}; the card is open again"));
    }
    Ok(if to == "answered" {
        AskResult::Answered
    } else {
        AskResult::Passed
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_card_takes_the_direct_route_only_with_a_matching_session_and_an_agent_at_rest() {
        let qass_free = (9, at("idle", None), None);
        let worker = |state, session| Some((5, at(state, session)));
        // The same conversation, at rest: straight to the worker.
        assert_eq!(
            route(
                &card(Some("s1")),
                worker("idle", Some("s1")).as_ref(),
                Some(&qass_free)
            ),
            Route::Worker(5)
        );
        // No session on the card, or none reported by the agent: through qass.
        assert_eq!(
            route(
                &card(None),
                worker("idle", Some("s1")).as_ref(),
                Some(&qass_free)
            ),
            Route::Qass(9)
        );
        assert_eq!(
            route(
                &card(Some("s1")),
                worker("idle", None).as_ref(),
                Some(&qass_free)
            ),
            Route::Qass(9)
        );
        // Neither says a session (opencode, amp, goose, cursor): two blanks are not a match.
        assert_eq!(
            route(&card(None), worker("idle", None).as_ref(), Some(&qass_free)),
            Route::Qass(9)
        );
        // Another conversation in the terminal (a /clear, a new session): through qass.
        assert_eq!(
            route(
                &card(Some("s1")),
                worker("idle", Some("s2")).as_ref(),
                Some(&qass_free)
            ),
            Route::Qass(9)
        );
        // The worker is working, blocked or gone: through qass.
        for state in ["working", "blocked", "sleeping"] {
            assert_eq!(
                route(
                    &card(Some("s1")),
                    worker(state, Some("s1")).as_ref(),
                    Some(&qass_free)
                ),
                Route::Qass(9),
                "{state}"
            );
        }
        assert_eq!(
            route(&card(Some("s1")), None, Some(&qass_free)),
            Route::Qass(9)
        );
    }

    #[test]
    fn qass_is_typed_to_only_while_it_is_at_rest_and_nobody_is_typing() {
        let no_worker = None;
        let qass = |state, typed: Option<i64>| Some((9, at(state, None), typed));
        assert_eq!(
            route(&card(None), no_worker, qass("idle", None).as_ref()),
            Route::Qass(9)
        );
        assert_eq!(
            route(&card(None), no_worker, qass("done", Some(31)).as_ref()),
            Route::Qass(9)
        );
        for state in ["working", "blocked", "sleeping", "starting"] {
            assert_eq!(
                route(&card(None), no_worker, qass(state, None).as_ref()),
                Route::Busy,
                "{state}"
            );
        }
        assert_eq!(
            route(&card(None), no_worker, qass("idle", Some(5)).as_ref()),
            Route::Busy
        );
        assert_eq!(
            route(&card(None), no_worker, qass("idle", Some(29)).as_ref()),
            Route::Busy
        );
        assert_eq!(route(&card(None), no_worker, None), Route::NoQass);
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

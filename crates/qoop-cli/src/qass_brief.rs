//! `qoop qass brief <n>`: what qass must know before it writes to one agent, worked out here
//! so the model does no arithmetic. Facts only, never a gate: times, counts and the record's
//! own words where they are short (the newest evidence line, cut to 80 characters); never a
//! screen's or a transcript's text. At most 200 words, cut here.

use std::path::Path;

use qoop_history::{BaseDirs, Message, Role};
use qoop_runtime::paths;

use crate::qass::{PROMPTED_LOG, json};

/// A message within this many seconds after a write from qass to that tab is qass's.
const QASS_SAYS: i64 = 5;
/// The owner typed into a tab this recently: he is at it.
const ENGAGED: i64 = 120;
/// Writes to one agent since the owner last spoke to qass, and to all in an hour.
const PER_AGENT: usize = 3;
const PER_HOUR: usize = 10;
/// Review rounds when the workflow names no limit.
const ROUNDS: u32 = 8;
const WORDS: usize = 200;
/// The words that start qass under each agent program: the program's, never his.
const LAUNCH: [&str; 3] = ["/qass", "$qass", "Use the qass skill."];

/// Whether a message is the line that started qass, with or without words after it.
fn launch(text: &str) -> bool {
    LAUNCH.iter().any(|l| text.starts_with(l))
}

/// One `prompted.log` record: `<unix> <from terminal> T<n>` (`<remote>/T<n>` from on.sh).
#[derive(Clone, Debug, PartialEq)]
struct Record {
    at: i64,
    from: u32,
    to: String,
}

fn parse_log(text: &str) -> Vec<Record> {
    text.lines()
        .filter_map(|line| {
            let mut w = line.split_whitespace();
            Some(Record {
                at: w.next()?.parse().ok()?,
                from: w.next()?.parse().ok()?,
                to: w.next()?.to_string(),
            })
        })
        .collect()
}

/// An agent's open task, as its record says.
#[derive(Clone, Debug, Default)]
pub(crate) struct Task {
    pub(crate) id: i64,
    pub(crate) stage: String,
    pub(crate) since: i64,
    /// When it was opened.
    pub(crate) opened: i64,
    pub(crate) claimed: bool,
    pub(crate) rounds: u32,
    /// Every evidence body, newest first.
    pub(crate) evidence: Vec<String>,
    /// Where its agent works, as the record keeps it past its tab.
    pub(crate) folder: Option<std::path::PathBuf>,
    /// What the owner wants and when it is done, as the record says it now.
    pub(crate) goal: String,
    /// The goal it was opened with, which the agent cannot rewrite.
    pub(crate) first_goal: String,
    /// Its last step forward: a stage above every earlier one, or evidence; else when opened.
    pub(crate) forward: i64,
}

/// Everything a brief is made of, read before it is composed.
#[derive(Clone, Debug, Default)]
struct Facts {
    terminal: u32,
    name: String,
    space: String,
    state: String,
    state_since: Option<i64>,
    generation: Option<u64>,
    rule: Option<String>,
    /// What was typed into the agent's tab; `None` when its transcript cannot be read.
    typed_here: Option<Vec<Message>>,
    qass: Option<u32>,
    /// What was typed into qass's own tab; `None` when it cannot be read.
    typed_to_qass: Option<Vec<Message>>,
    log: Vec<Record>,
    /// The open task on its row: its facts, or its id when they could not be read.
    task: Option<Result<Task, i64>>,
    limit: u32,
    /// The agent's open card: its number, handle, status and when it was opened.
    card: Option<(String, String, String, Option<i64>)>,
    /// `owner/name` when the agent works in the owner's own repository
    /// ([`crate::worktree::own_repo`]); `None` for any other, or when its folder is gone.
    own: Option<String>,
    now: i64,
    /// Seconds east of UTC, for a clock time he reads.
    offset: i64,
}

/// The newest message the owner typed, in seconds: not a notification, not the line that
/// started qass, and not one that follows a write `by_other` says came just before it. `Err`
/// when a message has no time.
fn owner_typed(messages: &[Message], by_other: impl Fn(i64) -> bool) -> Result<Option<i64>, ()> {
    for m in messages.iter().rev() {
        let text = m.text.trim();
        if text.starts_with('<') || launch(text) || peer(text) {
            continue;
        }
        let at = m.ts.ok_or(())? / 1000;
        if !by_other(at) {
            return Ok(Some(at));
        }
    }
    Ok(None)
}

/// What Claude Code types into a session for another one of an agent team: a peer's turn,
/// never his.
const PEER: &str = "Another Claude session sent a message";

fn peer(text: &str) -> bool {
    text.starts_with(PEER)
}

/// Who typed the agent's newest message, and when: him, qass, another agent through `qoop
/// prompt` (named by its tab, the one to ask when the agent waits on it), or a teammate. Not
/// who started its turn: a background job's notice starts one too and is no message here.
/// `None` with no message read.
fn last_typed(
    messages: &[Message],
    log: &[Record],
    tab: &str,
    qass: Option<u32>,
) -> Option<(String, Option<i64>)> {
    let m = messages.iter().rev().find(|m| !m.text.trim().is_empty())?;
    let text = m.text.trim();
    let at = m.ts.map(|t| t / 1000);
    if peer(text) || text.starts_with("<teammate-message") {
        return Some(("a teammate".into(), at));
    }
    let by = at.and_then(|at| {
        log.iter()
            .rev()
            .find(|r| r.to == tab && (-1..=QASS_SAYS).contains(&(at - r.at)))
            .map(|r| r.from)
    });
    Some((
        match by {
            Some(from) if Some(from) == qass => "qass".into(),
            Some(from) => format!("T{from}'s prompt"),
            None if text.starts_with('<') => "a notice".into(),
            None => "him".into(),
        },
        at,
    ))
}

/// Whether a record in `log` for which `pick` holds was written at most `QASS_SAYS` seconds
/// before `at`, or a second after it: the two clocks round apart.
fn written_before(log: &[Record], at: i64, pick: impl Fn(&Record) -> bool) -> bool {
    log.iter()
        .any(|r| pick(r) && (-1..=QASS_SAYS).contains(&(at - r.at)))
}

/// Review rounds: changes into `reviewing`, oldest first. A stay at `reviewing` is no new
/// round, and neither is the way back from `waiting-human` (the answer returns the stage).
fn rounds<'a>(stages: impl IntoIterator<Item = &'a str>) -> u32 {
    let mut last = "";
    let mut n = 0;
    for stage in stages {
        if stage == "waiting-human" {
            continue;
        }
        if stage == "reviewing" && last != "reviewing" {
            n += 1;
        }
        last = stage;
    }
    n
}

/// The stages in the order work moves through them; `waiting-human` is none of them.
const ORDER: [&str; 5] = [
    "investigating",
    "planning",
    "implementing",
    "reviewing",
    "verifying",
];

/// The last step forward, from `(kind, body, at)` events oldest first: a stage above every
/// stage before it (a task opens at the first), or an evidence line not written before. A
/// claim is no step: the missing step it is told and the claim again would reset the count
/// without end; nor is the same evidence again ("tests still fail").
fn forward<'a>(events: impl IntoIterator<Item = (&'a str, &'a str, i64)>, opened: i64) -> i64 {
    let mut top = 0;
    let mut at = opened;
    let mut seen = std::collections::HashSet::new();
    for (kind, body, when) in events {
        match kind {
            "evidence" if seen.insert(body.trim()) => at = at.max(when),
            "stage" => {
                if let Some(i) = ORDER.iter().position(|s| *s == body)
                    && i > top
                {
                    top = i;
                    at = at.max(when);
                }
            }
            _ => {}
        }
    }
    at
}

/// "at most N review rounds", any case, from a workflow's text, across a line break too.
fn review_limit(text: &str) -> Option<u32> {
    let text = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    text.match_indices("at most ").find_map(|(at, said)| {
        let rest = &text[at + said.len()..];
        let (n, rest) = rest.split_once(' ')?;
        rest.starts_with("review rounds").then(|| n.parse().ok())?
    })
}

pub(crate) fn clock(at: i64, offset: i64) -> String {
    let secs = (at + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

fn minutes(since: Option<i64>, now: i64) -> String {
    since.map_or("for unknown time".into(), |s| {
        format!("{} min", (now - s).max(0) / 60)
    })
}

/// The first line, at most `chars` characters with the `...` that says it was cut.
pub(crate) fn cut(text: &str, chars: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= chars {
        return line.to_string();
    }
    let kept: String = line.chars().take(chars.saturating_sub(3)).collect();
    format!("{kept}...")
}

/// Whether a task was opened as a report from a stranger's note: its goal starts `report:`,
/// as typed or after the `opened by <who>: ` its record may carry.
fn reported(first_goal: &str) -> bool {
    let goal = first_goal.trim_start();
    let goal = goal
        .strip_prefix("opened by ")
        .and_then(|rest| rest.split_once(": ").map(|(_, g)| g))
        .unwrap_or(goal);
    goal.trim_start()
        .to_ascii_lowercase()
        .starts_with("report:")
}

/// At most `max` words: lines before the last are dropped from the end, and `+N more` says
/// how many went. The last line (what is allowed now) always stays.
pub(crate) fn cap(mut lines: Vec<String>, max: usize) -> Vec<String> {
    let words = |l: &[String]| {
        l.iter()
            .map(|s| s.split_whitespace().count())
            .sum::<usize>()
    };
    let mut dropped = 0;
    while words(&lines) + usize::from(dropped > 0) * 2 > max && lines.len() > 1 {
        lines.remove(lines.len() - 2);
        dropped += 1;
    }
    if dropped > 0 {
        let at = lines.len() - 1;
        lines.insert(at, format!("+{dropped} more"));
    }
    lines
}

/// The brief's lines, and the same as JSON.
fn compose(f: &Facts) -> (Vec<String>, serde_json::Value) {
    let tab = format!("T{}", f.terminal);
    let mut lines = vec![format!(
        "{tab} {} ({}) {} {} g{}",
        cut(&f.name, 40),
        f.space,
        f.state,
        minutes(f.state_since, f.now),
        f.generation.map_or("?".into(), |g| g.to_string())
    )];
    let mut not_now = Vec::new();

    // Owner here: his newest message in that tab. One that follows a write to that tab is the
    // writer's, qass's or another agent's that drives this one.
    let to_tab = |r: &Record| r.to == tab;
    let here = f
        .typed_here
        .as_deref()
        .ok_or(())
        .and_then(|m| owner_typed(m, |at| written_before(&f.log, at, to_tab)));
    let (engaged, typed_at) = match here {
        Err(()) => {
            lines.push("owner here: unknown: treat as engaged".into());
            not_now.push("write (owner here unknown: treat as engaged)".to_string());
            (None, None)
        }
        Ok(None) => {
            lines.push("owner here: nothing he typed in this tab".into());
            (Some(false), None)
        }
        Ok(Some(at)) => {
            let ago = (f.now - at).max(0);
            let until = clock(at + ENGAGED, f.offset);
            if ago < ENGAGED {
                lines.push(format!(
                    "owner here: typed {ago} s ago, engaged: hands off until {until} ({})",
                    at + ENGAGED
                ));
                not_now.push(format!(
                    "write (owner typed here {ago} s ago, until {until})"
                ));
            } else {
                lines.push(format!(
                    "owner here: typed {} min ago, not engaged",
                    ago / 60
                ));
            }
            (Some(ago < ENGAGED), Some(at))
        }
    };

    if let Some((by, at)) = f
        .typed_here
        .as_deref()
        .and_then(|m| last_typed(m, &f.log, &tab, f.qass))
    {
        let ago = at.map_or(String::new(), |a| {
            format!(" {} min ago", (f.now - a).max(0) / 60)
        });
        lines.push(format!("last typed: by {by}{ago}"));
    }

    // Writes: qass's own records since the owner last wrote to qass, and in the last hour.
    let mine: Vec<&Record> = f.log.iter().filter(|r| Some(r.from) == f.qass).collect();
    let to_qass = f.qass.map(|q| format!("T{q}"));
    // No message of his found (none in the window read, or a new conversation) is as unknown
    // as no transcript: the last hour stands in, and the line says so.
    let spoke = f
        .typed_to_qass
        .as_deref()
        .ok_or(())
        .and_then(|m| {
            owner_typed(m, |at| {
                written_before(&f.log, at, |r| Some(&r.to) == to_qass.as_ref())
            })
        })
        .ok()
        .flatten();
    let since = spoke.unwrap_or(f.now - 3600);
    // A step forward gives room too, for that agent only: three pushes with none is a stall.
    let task = f.task.as_ref().and_then(|t| t.as_ref().ok());
    let moved = task
        .map(|t| t.forward.max(t.opened))
        .filter(|at| *at > since);
    let to_agent = mine
        .iter()
        .filter(|r| r.to == tab && r.at >= moved.unwrap_or(since))
        .count();
    // His word gives room: the hour's count starts again when he speaks.
    let hour_from = since.max(f.now - 3600);
    let hour = mine.iter().filter(|r| r.at >= hour_from).count();
    let since_words = match (moved, spoke) {
        (Some(_), _) => "since its last step forward",
        (None, Some(_)) => "since he spoke to you",
        (None, None) => "in the last hour (when he last spoke to you is unknown)",
    };
    let hour_words = if hour_from > f.now - 3600 {
        "since he spoke to you"
    } else {
        "in the last hour"
    };
    let qass_known = if f.qass.is_some() {
        ""
    } else {
        " (qass's terminal unknown)"
    };
    lines.push(format!(
        "writes: {to_agent} of {PER_AGENT} to {tab} {since_words}; {hour} of {PER_HOUR} {hour_words}{qass_known}"
    ));
    if to_agent >= PER_AGENT {
        not_now.push(format!("write ({to_agent} of {PER_AGENT} {since_words})"));
    }
    if hour >= PER_HOUR {
        not_now.push(format!("write ({hour} of {PER_HOUR} {hour_words})"));
    }
    if f.qass.is_none() {
        not_now.push("write (qass's terminal unknown)".to_string());
    }

    if let Some(Err(id)) = &f.task {
        lines.push(format!("task: #{id} unreadable"));
        not_now.push("another review round (task unreadable)".to_string());
    } else if let Some(t) = task {
        lines.push(format!(
            "task: #{} {} {}{}, review rounds {} of {}, last step forward {} ago; evidence: {}",
            t.id,
            t.stage,
            minutes(Some(t.since), f.now),
            if t.claimed { ", claimed" } else { "" },
            t.rounds,
            f.limit,
            minutes(Some(t.forward.max(t.opened)), f.now),
            t.evidence.first().map_or("none".into(), |e| cut(e, 80))
        ));
        if t.rounds >= f.limit {
            not_now.push(format!(
                "another review round ({} of {})",
                t.rounds, f.limit
            ));
        }
    } else {
        lines.push("task: none open".into());
    }

    lines.push(match &f.card {
        // He answered in the tab after the card: it may be settled there.
        Some((card, handle, status, opened)) => {
            match typed_at.filter(|at| opened.is_some_and(|o| *at > o)) {
                Some(at) => format!(
                    "card: {card} {handle} {status}; he typed here after it ({})",
                    clock(at, f.offset)
                ),
                None => format!("card: {card} {handle} {status}"),
            }
        }
        None => "card: none open".into(),
    });

    // Which of the skill's three prompt endings: merge in the owner's own repository, else the
    // strict one; the middle one (`pr`) where that repository's files ask before merging.
    // Work a stranger's note started (`report:`, as it was opened) stops at the PR: its AGENTS.md.
    // An unreadable task may be a report: it gets the PR too.
    let report = task.is_some_and(|t| reported(&t.first_goal)) || matches!(f.task, Some(Err(_)));
    let end = match (f.own.is_some(), report) {
        (true, false) => "merge",
        (true, true) => "pr",
        (false, _) => "none",
    };
    lines.push(match f.own.as_deref() {
        Some(r) if report => format!("repo: own {r}; ending pr (a report: task stops at its PR)"),
        Some(r) => format!("repo: own {r}; ending merge (pr if a repo file asks before merging)"),
        None => "repo: not own; ending none".into(),
    });

    let blocked = f.state == "blocked";
    lines.push(if blocked {
        format!(
            "screen: blocked on a menu (rule {})",
            f.rule.as_deref().unwrap_or("none")
        )
    } else {
        "screen: not blocked on a menu".into()
    });

    // The goal last before the verdict: the 200-word cut drops it before the ending.
    if let Some(t) = task.filter(|t| !t.goal.trim().is_empty()) {
        lines.push(format!("goal: {}", cut(&t.goal, 300)));
    }

    let write_ok = !not_now.iter().any(|n| n.starts_with("write"));
    let allowed = write_ok.then(|| format!("write (ending {end})"));
    let mut last = format!("Allowed now: {}", allowed.as_deref().unwrap_or("nothing"));
    if !not_now.is_empty() {
        last += &format!(". Not now: {}", not_now.join(", "));
    }
    lines.push(last);
    let lines = cap(lines, WORDS);

    let json = serde_json::json!({
        "terminal": f.terminal,
        "lines": lines,
        "owner_engaged": engaged,
        "owner_typed_at": typed_at,
        "hands_off_until": typed_at.filter(|_| engaged == Some(true)).map(|a| a + ENGAGED),
        "writes_to_agent": to_agent,
        "writes_last_hour": hour,
        "task": f.task.as_ref().map(|t| t.as_ref().map_or_else(|id| *id, |t| t.id)),
        "task_readable": f.task.as_ref().map(Result::is_ok),
        "review_rounds": task.map(|t| t.rounds),
        "goal": task.map(|t| &t.goal),
        "last_forward_at": task.map(|t| t.forward),
        "review_limit": f.limit,
        "card": f.card.as_ref().map(|c| &c.0),
        "own_repo": f.own,
        "ending": end,
        "blocked": blocked,
        "allowed": allowed,
        "not_now": not_now,
    });
    (lines, json)
}

/// What was typed into a tab, from its agent's own transcript.
fn typed(row: &serde_json::Value, dirs: Option<&BaseDirs>) -> Option<Vec<Message>> {
    let program = row["program"].as_str()?;
    let session = row["agent_session"].as_str();
    qoop_history::history(program, session, dirs?, usize::MAX, Some(Role::User))
        .ok()
        .map(|w| w.messages)
}

/// The task's facts: its stage events, all pages of them, give its rounds, and every
/// evidence event is read.
pub(crate) fn task_of(id: i64, now: i64) -> Option<Task> {
    let id_s = id.to_string();
    let mut events: Vec<serde_json::Value> = Vec::new();
    let mut before: Option<String> = None;
    let task = loop {
        let mut args = vec!["task", "show", &id_s, "--json"];
        if let Some(b) = &before {
            args.extend(["--before", b]);
        }
        let page = json(&args)?;
        let more = page["more"] == true;
        let got = page["events"].as_array().cloned().unwrap_or_default();
        before = got
            .last()
            .and_then(|e| e["id"].as_i64())
            .map(|i| i.to_string());
        events.extend(got);
        if !more || before.is_none() {
            break page["task"].clone();
        }
    };
    // Newest first as read; oldest first for the rounds.
    let of = |kind: &'static str| events.iter().filter(move |e| e["kind"] == kind);
    let stages: Vec<&str> = of("stage").filter_map(|e| e["body"].as_str()).collect();
    Some(Task {
        id,
        stage: task["stage"].as_str()?.to_string(),
        since: of("stage")
            .next()
            .and_then(|e| e["at"].as_i64())
            .or_else(|| task["opened_at"].as_i64())
            .unwrap_or(now),
        opened: task["opened_at"].as_i64().unwrap_or(now),
        claimed: task["claimed_at"].is_i64(),
        rounds: rounds(stages.into_iter().rev()),
        evidence: of("evidence")
            .filter_map(|e| e["body"].as_str())
            .map(String::from)
            .collect(),
        folder: task["folder"].as_str().map(std::path::PathBuf::from),
        goal: task["goal"].as_str().unwrap_or("").to_string(),
        // Events are newest first: the first `opened` is the last one.
        first_goal: events
            .iter()
            .rev()
            .find(|e| e["kind"] == "opened")
            .and_then(|e| e["body"].as_str())
            .or_else(|| task["goal"].as_str())
            .unwrap_or("")
            .to_string(),
        forward: forward(
            events.iter().rev().filter_map(|e| {
                Some((e["kind"].as_str()?, e["body"].as_str()?, e["at"].as_i64()?))
            }),
            task["opened_at"].as_i64().unwrap_or(now),
        ),
    })
}

/// Seconds east of UTC, as the machine's `date` says it.
fn utc_offset() -> i64 {
    let out = std::process::Command::new("date").arg("+%z").output().ok();
    let z = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    z.filter(|z| z.len() == 5)
        .and_then(|z| {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let h: i64 = z[1..3].parse().ok()?;
            let m: i64 = z[3..5].parse().ok()?;
            Some(sign * (h * 3600 + m * 60))
        })
        .unwrap_or(0)
}

/// An agent on a remote (`ubox/T2`) is read there, through on.sh, as before: its tab, its
/// transcript and its task are on that machine.
fn remote_line(target: &str) -> Option<(&str, String)> {
    let (remote, _) = target.split_once('/')?;
    Some((
        remote,
        format!(
            "{target}: an agent on {remote}; read it with `on.sh {remote}` as today (the strict ending only)"
        ),
    ))
}

pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    let as_json = args.iter().skip(4).any(|a| a == "--json");
    let target = args.get(3).map(String::as_str).unwrap_or("");
    if args.iter().skip(4).any(|a| a != "--json") {
        return Err(crate::usage("usage: qoop qass brief <n> [--json]"));
    }
    match remote_line(target) {
        Some((remote, line)) => {
            print_out(as_json, vec![line], serde_json::json!({ "remote": remote }))
        }
        None => {
            let n: u32 = target
                .trim_start_matches(['T', 't'])
                .parse()
                .map_err(|_| crate::usage("usage: qoop qass brief <n> [--json]"))?;
            crate::qass_guard(&n.to_string())?;
            let (lines, json) = compose(&gather(n)?);
            print_out(as_json, lines, json)
        }
    }
}

fn print_out(
    as_json: bool,
    lines: Vec<String>,
    mut json: serde_json::Value,
) -> std::io::Result<()> {
    if as_json {
        json["lines"] = lines.into();
        println!("{json}");
    } else {
        println!("{}", lines.join("\n"));
    }
    Ok(())
}

/// What the agent is called: its tab's name, else its own title, else its session id's first
/// eight characters.
fn head_name(row: &serde_json::Value, n: u32) -> String {
    let text = |key: &str| row[key].as_str().filter(|s| !s.is_empty());
    text("name")
        .or_else(|| text("agent_title"))
        .map(String::from)
        .or_else(|| text("agent_session").map(|s| s.chars().take(8).collect()))
        .unwrap_or_else(|| format!("T{n}"))
}

/// Reads every fact of terminal `n` from the daemon, the transcripts and qass's files.
fn gather(n: u32) -> std::io::Result<Facts> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let agents = json(&["agents", "--json"])
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| std::io::Error::other("the daemon did not list its agents"))?;
    let row = agents
        .iter()
        .find(|r| r["terminal"].as_u64() == Some(u64::from(n)))
        .ok_or_else(|| std::io::Error::other(format!("no terminal {n}")))?;
    let spaces = json(&["ls", "--json"]).unwrap_or_default();
    let space_of = |id: &serde_json::Value| {
        spaces
            .as_array()
            .and_then(|s| s.iter().find(|w| &w["id"] == id))
            .and_then(|w| w["name"].as_str())
            .map(String::from)
    };
    let folder = paths::qass_data_dir();
    // qass's terminal: the caller's own, as `qoop prompt` notes it in prompted.log, when this
    // daemon lists it; else the agent in qass's space and folder.
    let caller = qoop_runtime::legacy::var("QOOP_TERMINAL")
        .ok()
        .and_then(|t| t.parse::<u64>().ok());
    let qass_row = agents
        .iter()
        .find(|r| caller.is_some() && r["terminal"].as_u64() == caller)
        .or_else(|| {
            folder
                .as_deref()
                .and_then(|f| crate::qass::row_in(&agents, &spaces, f))
        });
    let dirs = BaseDirs::from_home().ok();
    let text = |key: &str| row[key].as_str().filter(|s| !s.is_empty());
    let name = head_name(row, n);
    // The repository the agent works in, by its main checkout; none when its folder is gone.
    let cwd = text("cwd").map(Path::new).filter(|p| p.is_dir());
    let root = cwd
        .and_then(|p| crate::worktree::repo_of(p).ok())
        .and_then(|r| r.root.canonicalize().ok());
    let workflow = |p: &Path| std::fs::read_to_string(p).ok();
    let limit = root
        .as_deref()
        .and_then(|r| workflow(&r.join(".qoop/workflow.md")))
        .or_else(|| {
            folder
                .as_deref()
                .and_then(|f| workflow(&f.join(crate::skills::QASS_SKILL_DIRS[0]).join("init.md")))
        })
        .and_then(|t| review_limit(&t))
        .unwrap_or(ROUNDS);
    let tab = format!("T{n}");
    let card = json(&["ask", "ls", "--json"]).and_then(|v| {
        v["live"].as_array()?.iter().find_map(|c| {
            let mine = c["terminal"].as_u64() == Some(u64::from(n)) || c["agent"] == tab.as_str();
            mine.then(|| {
                let s = |k: &serde_json::Value| k.as_str().unwrap_or("").to_string();
                (
                    s(&c["card"]),
                    s(&c["body"]["handle"]),
                    s(&c["status"]),
                    c["opened_at"].as_i64(),
                )
            })
        })
    });
    Ok(Facts {
        terminal: n,
        name,
        space: space_of(&row["workspace"]).unwrap_or_default(),
        state: row["state"].as_str().unwrap_or("unknown").to_string(),
        state_since: row["last_active_at_unix"].as_i64(),
        generation: row["generation"].as_u64(),
        rule: text("rule").map(String::from),
        typed_here: typed(row, dirs.as_ref()),
        qass: qass_row
            .and_then(|r| r["terminal"].as_u64())
            .and_then(|t| u32::try_from(t).ok()),
        typed_to_qass: qass_row.and_then(|r| typed(r, dirs.as_ref())),
        log: folder
            .and_then(|f| std::fs::read_to_string(f.join(PROMPTED_LOG)).ok())
            .map(|t| parse_log(&t))
            .unwrap_or_default(),
        task: row["task"]["id"]
            .as_i64()
            .map(|id| task_of(id, now).ok_or(id)),
        limit,
        card,
        own: cwd.and_then(crate::worktree::own_repo),
        now,
        offset: utc_offset(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_075_400;

    /// A Claude transcript of user lines (`at` in seconds), read back as qass reads one.
    fn transcript(lines: &[serde_json::Value]) -> Option<Vec<Message>> {
        let root = tempfile::tempdir().expect("dir");
        let project = root.path().join("projects/-x");
        std::fs::create_dir_all(&project).expect("project");
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(project.join("sid-1.jsonl"), body).expect("write");
        let dirs = BaseDirs {
            claude: vec![root.path().join("projects")],
            codex: root.path().join("codex"),
            opencode: None,
            gemini: root.path().join("gemini"),
        };
        let row = serde_json::json!({"program":"claude","agent_session":"sid-1"});
        typed(&row, Some(&dirs))
    }

    /// An RFC 3339 time in UTC, worked out here: no `date` flags differ between systems.
    fn iso(at: i64) -> String {
        format!(
            "{}T{}:{:02}Z",
            qoop_history::date_utc(at * 1000),
            clock(at, 0),
            at.rem_euclid(60)
        )
    }

    fn said(at: i64, text: &str) -> serde_json::Value {
        serde_json::json!({"type":"user","timestamp":iso(at),"origin":{"kind":"human"},
            "message":{"role":"user","content":text}})
    }

    fn facts() -> Facts {
        Facts {
            terminal: 52,
            name: "parser fix".into(),
            space: "qoop".into(),
            state: "idle".into(),
            state_since: Some(NOW - 240),
            generation: Some(3),
            qass: Some(128),
            typed_here: Some(Vec::new()),
            typed_to_qass: Some(Vec::new()),
            limit: ROUNDS,
            now: NOW,
            ..Facts::default()
        }
    }

    fn brief(f: &Facts) -> String {
        compose(f).0.join("\n")
    }

    fn words(text: &str) -> usize {
        text.split_whitespace().count()
    }

    /// His message is his; the one that came 2 s after qass wrote to that tab is qass's.
    #[test]
    fn a_message_right_after_qasss_write_is_qasss() {
        let mut f = facts();
        f.typed_here = transcript(&[
            said(NOW - 300, "fix the parser SECRETWORD"),
            said(NOW - 30, "Run the tests SECRETWORD"),
        ]);
        f.log = parse_log(&format!("{} 128 T52\n", NOW - 32));
        let out = brief(&f);
        assert!(
            out.contains("owner here: typed 5 min ago, not engaged"),
            "{out}"
        );
        assert!(out.ends_with("Allowed now: write (ending none)"), "{out}");
        assert_eq!(compose(&f).1["allowed"], "write (ending none)");
        // Rounded a second apart, the record may read a second after the message.
        f.log = parse_log(&format!("{} 128 T52\n", NOW - 29));
        assert!(brief(&f).contains("typed 5 min ago"), "{}", brief(&f));
        f.log = parse_log(&format!("{} 128 T52\n", NOW - 32));
        assert!(!out.contains("SECRETWORD"), "no transcript text: {out}");
        // A record 6 s before does not make it qass's.
        f.log = parse_log(&format!("{} 128 T52", NOW - 36));
        let out = brief(&f);
        let until = clock(NOW - 30 + ENGAGED, 0);
        assert!(
            out.contains(&format!(
                "typed 30 s ago, engaged: hands off until {until} ({})",
                NOW - 30 + ENGAGED
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "Not now: write (owner typed here 30 s ago, until {until})"
            )),
            "{out}"
        );
        assert!(out.contains("Allowed now: nothing"), "{out}");
    }

    /// Notifications after his message, more than any window of five, do not hide it.
    #[test]
    fn a_notification_storm_does_not_hide_him() {
        let mut lines = vec![said(NOW - 40, "go on")];
        lines.extend((0..60).map(|i| said(NOW - 39 + i / 2, "<task-notification>done")));
        let mut f = facts();
        f.typed_here = transcript(&lines);
        assert!(
            brief(&f).contains("typed 40 s ago, engaged"),
            "{}",
            brief(&f)
        );
    }

    /// What he typed while the agent worked, still queued, is him at that tab.
    #[test]
    fn a_queued_message_is_him_at_the_tab() {
        let mut f = facts();
        f.typed_here = transcript(&[
            said(NOW - 900, "start"),
            serde_json::json!({"type":"queue-operation","operation":"enqueue",
                "timestamp":iso(NOW - 20),"sessionId":"sid-1","content":"wait, not that"}),
        ]);
        assert!(
            brief(&f).contains("typed 20 s ago, engaged"),
            "{}",
            brief(&f)
        );
    }

    /// The summary a compacted session starts with is nobody typing.
    #[test]
    fn a_compaction_summary_is_not_him() {
        let mut summary = said(
            NOW - 10,
            "This session is being continued from a previous one.",
        );
        summary["isCompactSummary"] = true.into();
        let mut f = facts();
        f.typed_here = transcript(&[said(NOW - 600, "start"), summary]);
        assert!(
            brief(&f).contains("typed 10 min ago, not engaged"),
            "{}",
            brief(&f)
        );
    }

    /// No transcript, or a message with no time: unknown, and the careful side.
    #[test]
    fn a_missing_transcript_is_treated_as_engaged() {
        let mut f = facts();
        f.typed_here = None;
        let out = brief(&f);
        assert!(
            out.contains("owner here: unknown: treat as engaged"),
            "{out}"
        );
        assert!(out.contains("Not now: write (owner here unknown"), "{out}");
        f.typed_here = Some(vec![Message {
            role: Role::User,
            text: "hi".into(),
            ts: None,
        }]);
        assert!(brief(&f).contains("unknown: treat as engaged"));
        // A tab whose agent never reported its conversation has no transcript to read.
        let row = serde_json::json!({"program":"claude"});
        assert!(typed(&row, BaseDirs::from_home().ok().as_ref()).is_none());
    }

    /// qass's writes to the agent count from when the owner last spoke to qass, never a
    /// message another terminal typed into qass; all of qass's in the last hour, remotes too.
    #[test]
    fn writes_count_since_he_spoke_to_qass() {
        let mut f = facts();
        f.typed_to_qass = transcript(&[
            said(NOW - 1000, "korea B"),
            said(NOW - 500, "QOOP-9: merged"),
        ]);
        let log: String = [
            format!("{} 128 T52", NOW - 4000),
            format!("{} 205 T128", NOW - 502),
            format!("{} 128 T52", NOW - 900),
            format!("{} 128 T52", NOW - 800),
            format!("{} 128 T52", NOW - 700),
            format!("{} 128 ubox/T2", NOW - 600),
            format!("{} 77 T52", NOW - 600),
        ]
        .join("\n");
        f.log = parse_log(&log);
        let out = brief(&f);
        assert!(
            out.contains(
                "writes: 3 of 3 to T52 since he spoke to you; 4 of 10 since he spoke to you"
            ),
            "{out}"
        );
        assert!(
            out.contains("Not now: write (3 of 3 since he spoke to you)"),
            "{out}"
        );
        assert_eq!(compose(&f).1["writes_to_agent"], 3);
    }

    /// Rounds are changes into reviewing: a stay is none, and neither is the way back from
    /// waiting on the owner.
    #[test]
    fn a_repeated_stage_is_one_round() {
        let stages = [
            "planning",
            "reviewing",
            "reviewing",
            "implementing",
            "reviewing",
            "waiting-human",
            "reviewing",
        ];
        assert_eq!(rounds(stages), 2);
        let mut f = facts();
        f.limit = 2;
        f.task = Some(Ok(Task {
            id: 12,
            stage: "reviewing".into(),
            since: NOW - 600,
            opened: NOW - 900,
            claimed: true,
            rounds: 2,
            evidence: vec![format!("cargo test: ok {}\nsecond line", "x".repeat(120))],
            folder: None,
            ..Task::default()
        }));
        let out = brief(&f);
        assert!(
            out.contains("task: #12 reviewing 10 min, claimed, review rounds 2 of 2, last step forward 15 min ago; evidence: cargo test: ok xx"),
            "{out}"
        );
        let evidence = out.lines().find(|l| l.starts_with("task:")).expect("task");
        assert!(
            evidence.ends_with("...") && !evidence.contains("second line"),
            "{out}"
        );
        let (_, said) = evidence.split_once("evidence: ").expect("evidence");
        assert_eq!(said.chars().count(), 80, "80 with the dots: {said}");
        assert!(
            out.contains("Not now: another review round (2 of 2)"),
            "{out}"
        );
        assert_eq!(
            review_limit("Loop limit: At Most 5 Review Rounds per task"),
            Some(5)
        );
        assert_eq!(review_limit("at most 3 reviewers"), None);
    }

    /// The owner's own repository gets the merge ending, any other the strict one; writing is
    /// allowed with nothing set: qass has no mode, and the brief never names one.
    #[test]
    fn the_repo_line_gives_the_ending() {
        let mut f = facts();
        f.own = Some("pat/x".into());
        let out = brief(&f);
        assert!(
            out.contains(
                "\nrepo: own pat/x; ending merge (pr if a repo file asks before merging)\n"
            ),
            "{out}"
        );
        assert!(out.ends_with("Allowed now: write (ending merge)"), "{out}");
        let json = compose(&f).1;
        assert_eq!(json["own_repo"], "pat/x");
        assert_eq!(json["ending"], "merge");
        assert!(json.get("mode").is_none(), "{json}");
        f.own = None;
        let out = brief(&f);
        assert!(out.contains("\nrepo: not own; ending none\n"), "{out}");
        assert!(out.ends_with("Allowed now: write (ending none)"), "{out}");
        for bad in ["policy", "mode", "propose"] {
            assert!(!out.contains(bad), "{bad}: {out}");
        }
    }

    /// A remote agent is read there, as today.
    #[test]
    fn a_remote_is_read_with_on_sh() {
        let (remote, line) = remote_line("ubox/T2").expect("remote");
        assert_eq!(remote, "ubox");
        assert!(line.contains("`on.sh ubox`"), "{line}");
        assert!(remote_line("52").is_none());
        assert_eq!(
            parse_log("100 128 ubox/T2\nbroken\n"),
            [Record {
                at: 100,
                from: 128,
                to: "ubox/T2".into()
            }]
        );
    }

    /// At most 200 words, cut by the code with `+N more`, the last line kept.
    #[test]
    fn a_brief_is_at_most_200_words() {
        let lines: Vec<String> = (0..30)
            .map(|i| format!("line {i} one two three four five six seven eight"))
            .chain(["Allowed now: nothing".to_string()])
            .collect();
        let cut = cap(lines, WORDS);
        assert!(words(&cut.join(" ")) <= WORDS, "{cut:?}");
        assert_eq!(cut.last().map(String::as_str), Some("Allowed now: nothing"));
        assert_eq!(cut[cut.len() - 2], "+11 more");
        let mut f = facts();
        f.name = "a very long sidebar name ".repeat(20);
        f.card = Some(("E82".into(), "stash".into(), "open".into(), Some(NOW - 60)));
        f.state = "blocked".into();
        f.rule = Some("permission_prompt".into());
        let out = brief(&f);
        assert!(words(&out) <= WORDS, "{out}");
        assert!(out.contains("card: E82 stash open"), "{out}");
        assert!(
            out.contains("screen: blocked on a menu (rule permission_prompt)"),
            "{out}"
        );
    }

    /// With qass's terminal unknown its writes cannot be counted: no write now.
    #[test]
    fn an_unknown_qass_terminal_is_no_write() {
        let mut f = facts();
        f.qass = None;
        let out = brief(&f);
        assert!(
            out.contains("Not now: write (qass's terminal unknown)"),
            "{out}"
        );
        assert!(out.contains("Allowed now: nothing"), "{out}");
    }

    /// The review limit is read across a line break.
    #[test]
    fn the_review_limit_reads_across_a_line_break() {
        assert_eq!(
            review_limit("Loop limit: at most\n5 review\n  rounds per task"),
            Some(5)
        );
    }

    /// A task on the row whose record cannot be read lifts nothing.
    #[test]
    fn an_unreadable_task_is_no_new_round() {
        let mut f = facts();
        f.task = Some(Err(12));
        let out = brief(&f);
        assert!(out.contains("task: #12 unreadable"), "{out}");
        assert!(
            out.contains("Not now: another review round (task unreadable)"),
            "{out}"
        );
    }

    /// The head names the agent by its tab's name, then its own title, then its session.
    #[test]
    fn the_head_names_the_tab_first() {
        let row = |v: serde_json::Value| head_name(&v, 3);
        let all =
            serde_json::json!({"name":"worker","agent_title":"Fix","agent_session":"sid-work-1"});
        assert_eq!(row(all), "worker");
        assert_eq!(
            row(serde_json::json!({"agent_title":"Fix","agent_session":"s"})),
            "Fix"
        );
        assert_eq!(
            row(serde_json::json!({"agent_session":"sid-work-1"})),
            "sid-work"
        );
        assert_eq!(row(serde_json::json!({})), "T3");
    }

    /// No message of his found in qass's transcript is unknown: the last hour, said so.
    #[test]
    fn no_word_to_qass_found_counts_the_last_hour() {
        let mut f = facts();
        f.typed_to_qass = transcript(&[said(NOW - 100, "<task-notification>x")]);
        f.log = parse_log(&format!("{} 128 T52\n{} 128 T52\n", NOW - 7200, NOW - 60));
        let out = brief(&f);
        assert!(
            out.contains(
                "writes: 1 of 3 to T52 in the last hour (when he last spoke to you is unknown)"
            ),
            "{out}"
        );
    }

    /// A message right after another agent wrote to that tab is that agent's (a lead driving
    /// its child), never his.
    #[test]
    fn a_message_right_after_another_agents_write_is_not_his() {
        let mut f = facts();
        f.typed_here = transcript(&[said(NOW - 30, "Run the tests")]);
        f.log = parse_log(&format!("{} 205 T52\n", NOW - 32));
        let out = brief(&f);
        assert!(
            out.contains("owner here: nothing he typed in this tab"),
            "{out}"
        );
    }

    /// His word gives room: writes from before he spoke do not count toward the hour.
    #[test]
    fn the_hour_counts_again_from_his_last_word() {
        let mut f = facts();
        f.typed_to_qass = transcript(&[said(NOW - 300, "open an agent for the scroll bug")]);
        let log: String = (0..10)
            .map(|i| format!("{} 128 T{}\n", NOW - 3000 + i * 180, 60 + i))
            .collect();
        f.log = parse_log(&log);
        let out = brief(&f);
        assert!(out.contains("0 of 10 since he spoke to you"), "{out}");
        assert!(out.ends_with("Allowed now: write (ending none)"), "{out}");
        // With him silent for longer than the hour, the hour stands.
        f.typed_to_qass = transcript(&[said(NOW - 4000, "go")]);
        let out = brief(&f);
        assert!(out.contains("10 of 10 in the last hour"), "{out}");
        assert!(out.contains("Allowed now: nothing"), "{out}");
    }

    /// The line that started qass is the program's, not his word.
    #[test]
    fn the_launch_line_is_not_his_word() {
        let mut f = facts();
        for launch in LAUNCH {
            f.typed_to_qass = transcript(&[said(NOW - 100, launch)]);
            f.log = parse_log(&format!("{} 128 T52\n", NOW - 60));
            let out = brief(&f);
            assert!(
                out.contains("(when he last spoke to you is unknown)"),
                "{launch}: {out}"
            );
        }
    }

    /// His message in the tab after the card was written is said beside the card.
    #[test]
    fn the_card_says_he_typed_in_the_tab_after_it() {
        let mut f = facts();
        f.typed_here = transcript(&[said(NOW - 300, "only iOS, send it")]);
        f.card = Some(("E128".into(), "ios".into(), "open".into(), Some(NOW - 600)));
        let out = brief(&f);
        assert!(
            out.contains(&format!(
                "card: E128 ios open; he typed here after it ({})",
                clock(NOW - 300, 0)
            )),
            "{out}"
        );
        f.card = Some(("E128".into(), "ios".into(), "open".into(), Some(NOW - 200)));
        assert!(brief(&f).contains("card: E128 ios open\n"), "{}", brief(&f));
    }

    fn task_at(forward: i64, goal: &str) -> Task {
        Task {
            id: 9,
            stage: "implementing".into(),
            since: NOW - 3000,
            opened: NOW - 5000,
            goal: goal.into(),
            first_goal: goal.into(),
            forward,
            ..Task::default()
        }
    }

    /// A step forward is a stage above every earlier one, or evidence; the daemon's
    /// `waiting-human` and the way back from it, a stage written again, and a claim are none.
    #[test]
    fn a_step_forward_is_a_higher_stage_or_evidence() {
        let ev = |k: &'static str, b: &'static str, at: i64| (k, b, at);
        assert_eq!(forward([], 10), 10, "nothing yet: when it opened");
        assert_eq!(
            forward(
                [
                    ev("stage", "investigating", 20),
                    ev("stage", "implementing", 30),
                    ev("stage", "waiting-human", 40),
                    ev("stage", "implementing", 50),
                    ev("done", "claimed", 60),
                    ev("stage", "reviewing", 70),
                    ev("stage", "implementing", 80),
                    ev("stage", "reviewing", 90),
                ],
                10
            ),
            70,
            "the first rise to reviewing counts, the round trip back and forth does not"
        );
        assert_eq!(forward([ev("evidence", "tests: ok", 95)], 10), 95);
        assert_eq!(
            forward(
                [
                    ev("evidence", "tests still fail", 20),
                    ev("evidence", "tests still fail", 90)
                ],
                10
            ),
            20,
            "the same evidence again is no step"
        );
    }

    /// Three pushes with no step forward are the cap; a step forward after them gives room.
    #[test]
    fn writes_count_since_its_last_step_forward() {
        let mut f = facts();
        f.typed_to_qass = transcript(&[said(NOW - 2000, "go")]);
        f.log = parse_log(
            &[NOW - 1500, NOW - 1200, NOW - 900]
                .iter()
                .map(|at| format!("{at} 128 T52"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        f.task = Some(Ok(task_at(
            NOW - 2500,
            "fix the parser; done when cargo test passes",
        )));
        let out = brief(&f);
        assert!(
            out.contains("writes: 3 of 3 to T52 since he spoke to you"),
            "{out}"
        );
        assert!(out.contains("Not now: write (3 of 3"), "{out}");
        f.task = Some(Ok(task_at(
            NOW - 1000,
            "fix the parser; done when cargo test passes",
        )));
        let out = brief(&f);
        assert!(
            out.contains("writes: 1 of 3 to T52 since its last step forward"),
            "{out}"
        );
        assert!(out.contains("Allowed now: write"), "{out}");
    }

    /// The goal is said whole, last before the verdict, so a cut drops it before the ending.
    #[test]
    fn the_goal_is_said_and_cut_before_the_ending() {
        let mut f = facts();
        let goal = "make the parser right; done when cargo test passes and the real run shows it";
        f.task = Some(Ok(task_at(NOW - 600, goal)));
        let lines = compose(&f).0;
        let n = lines.len();
        assert_eq!(lines[n - 2], format!("goal: {goal}"));
        assert!(lines[n - 1].starts_with("Allowed now"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("last step forward 10 min ago")),
            "{lines:?}"
        );
        f.name = "x ".repeat(40);
        f.task = Some(Ok(task_at(NOW - 600, &"word ".repeat(60))));
        f.card = Some(("E1".into(), "h".into(), "open".into(), None));
        let lines = compose(&f).0;
        assert!(lines.iter().any(|l| l.starts_with("repo:")), "{lines:?}");
        assert!(
            lines
                .iter()
                .map(|l| l.split_whitespace().count())
                .sum::<usize>()
                <= WORDS
        );
    }

    /// Work opened from a stranger's note stops at its PR, whatever the goal says now.
    #[test]
    fn a_report_task_ends_at_the_pr() {
        assert!(reported("report: the tab title is wrong"));
        assert!(reported(
            "opened by a link to QOOP-9: Report: crash on start"
        ));
        assert!(!reported("fix the report page"));
        let mut f = facts();
        f.own = Some("me/app".into());
        let mut t = task_at(NOW - 600, "fix the crash");
        t.first_goal = "report: crash on start".into();
        f.task = Some(Ok(t));
        let (lines, json) = compose(&f);
        assert!(lines.iter().any(|l| l.contains("ending pr")), "{lines:?}");
        assert_eq!(json["ending"], "pr");
        f.task = Some(Ok(task_at(NOW - 600, "fix the crash")));
        assert_eq!(compose(&f).1["ending"], "merge");
    }

    /// A teammate's message is not his word, and the brief says who typed last.
    #[test]
    fn a_teammate_turn_is_not_his() {
        let mut f = facts();
        f.typed_here = transcript(&[
            said(NOW - 900, "build the dashboard"),
            said(
                NOW - 30,
                "Another Claude session sent a message: <teammate-message>done</teammate-message>",
            ),
        ]);
        let out = brief(&f);
        assert!(
            out.contains("owner here: typed 15 min ago, not engaged"),
            "{out}"
        );
        assert!(out.contains("last typed: by a teammate 0 min ago"), "{out}");
        f.typed_here = transcript(&[said(NOW - 900, "build the dashboard")]);
        f.log = parse_log(&format!("{} 77 T52", NOW - 902));
        assert!(brief(&f).contains("last typed: by T77's prompt 15 min ago"));
        f.log = parse_log(&format!("{} 128 T52", NOW - 902));
        assert!(brief(&f).contains("last typed: by qass 15 min ago"));
        f.log = Vec::new();
        assert!(brief(&f).contains("last typed: by him 15 min ago"));
    }
}

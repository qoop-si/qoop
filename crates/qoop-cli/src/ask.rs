//! `qoop ask`: the owner's question inbox, from qass's side.
//!
//! `qoop ask add` takes a card in the format below on standard input, checks it strictly and
//! hands the daemon a structured request: the daemon never reads text. `escalate.sh` is the
//! one door qass uses; this is what it calls.

use std::io::Read;
use std::time::Duration;

use qoop_runtime::control::{
    AskCard, AskInfo, AskOp, AskOption, AskPick, ControlRequest, ControlResponse, too_old_for,
};

use crate::task::{ask as ask_daemon_once, here, json};
use crate::usage;

const USAGE: &str = "usage:
  qoop ask add E<n> --agent <T52|ubox/T2|-> --handle <word> --urgency now|later [--via-qass]
                                         a card on stdin; --via-qass: his answer goes to qass,
                                         not the agent (a plan he approves)
  qoop ask close E<n> [--note <words>]   end a card that was handled (default note: closed)
  qoop ask answer E<n> [--option a|b|c|d] [--seen <its words>] [--text <words>]   answer as the owner (not from a terminal)
  qoop ask dismiss E<n>                  put a card away unanswered, as the owner
  qoop ask ls [--json]                   the cards waiting and the last few that ended";

/// What a card looks like, printed with every refusal so the writer fixes it on its next call.
const FORMAT: &str = "a card is `key: value` lines, one line each, any order:
  asked: <what the owner asked the agent for>            (optional)
  doing: <what it did and where it stands>
  why: <why it asks now>
  asks: <the question>
  A: <label> | <why this option> | <the words sent to the worker, in English>
  B: <label> | <why this option> | <the words sent to the worker, in English>
  (C and D optional; two to four options, A first, in order)
  pick: <A to D> | <why>                                 (optional)
every value is one line of at most 200 characters, and label, reason and words are not empty";

/// How long standard input may stay open with no end: a forgotten heredoc must not hang.
const STDIN_WAIT: Duration = Duration::from_secs(5);

/// The most standard input read.
const STDIN_MAX: usize = 16_000;

/// The daemon cannot store the card (too old, or not running): exit 3, so `escalate.sh` can
/// say so and still ring.
#[derive(Debug)]
pub(crate) struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

/// The card number is not free (used before, or another question has it): exit 4, so
/// `escalate.sh new` takes the next one.
#[derive(Debug)]
pub(crate) struct NotFree(pub String);

impl std::fmt::Display for NotFree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotFree {}

/// A card from its text, or every problem found. Pure: the grammar and the limits, nothing
/// of the daemon.
pub(crate) fn parse(text: &str, handle: &str, urgency: &str) -> Result<AskCard, Vec<String>> {
    let mut problems = Vec::new();
    let (mut asked, mut doing, mut why, mut asks, mut pick) = (None, None, None, None, None);
    let mut options: Vec<AskOption> = Vec::new();
    fn once(slot: &mut Option<String>, name: &str, value: &str, problems: &mut Vec<String>) {
        if slot.is_some() {
            problems.push(format!("{name} is given twice"));
        }
        *slot = Some(value.to_string());
    }
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            problems.push(format!("line {} has no `key: value`: {line:?}", n + 1));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "asked" => once(&mut asked, "asked", value, &mut problems),
            "doing" => once(&mut doing, "doing", value, &mut problems),
            "why" => once(&mut why, "why", value, &mut problems),
            "asks" => once(&mut asks, "asks", value, &mut problems),
            "pick" => {
                if pick.is_some() {
                    problems.push("pick is given twice".to_string());
                }
                pick = Some(value.to_string());
            }
            "A" | "B" | "C" | "D" => {
                let lower = key.to_lowercase();
                if options.iter().any(|o| o.key == lower) {
                    problems.push(format!("option {key} is given twice"));
                    continue;
                }
                // Split on the first two ` | `: the words may hold a pipe of their own.
                let mut parts = value.splitn(3, " | ");
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(label), Some(reason), Some(words)) => options.push(AskOption {
                        key: lower,
                        label: label.trim().to_string(),
                        reason: reason.trim().to_string(),
                        words: words.trim().to_string(),
                    }),
                    _ => problems.push(format!(
                        "option {key} is `label | reason | words`, with a ` | ` between the three"
                    )),
                }
            }
            other => problems.push(format!("{other:?} is not a field of a card")),
        }
    }
    // A field not given is named here; one given empty is named by the card check below.
    let mut missing = Vec::new();
    for (name, slot) in [("doing", &doing), ("why", &why), ("asks", &asks)] {
        if slot.is_none() {
            problems.push(format!("{name} is missing"));
            missing.push(format!("{name} is empty"));
        }
    }
    let pick = pick.and_then(|p| match p.split_once(" | ") {
        Some((key, reason)) => Some(AskPick {
            key: key.trim().to_lowercase(),
            reason: reason.trim().to_string(),
        }),
        None => {
            problems.push("pick is `<A to D> | <reason>`".to_string());
            None
        }
    });
    let card = AskCard {
        handle: handle.to_string(),
        urgency: urgency.to_string(),
        asked,
        doing: doing.unwrap_or_default(),
        why: why.unwrap_or_default(),
        asks: asks.unwrap_or_default(),
        options,
        pick,
    };
    // A value that was not given is already named above; the card check names the rest.
    problems.extend(card.problems().into_iter().filter(|p| !missing.contains(p)));
    if problems.is_empty() {
        Ok(card)
    } else {
        Err(problems)
    }
}

/// Standard input as the card text. A reader thread with a deadline, so a pipe that never
/// closes ends in a refusal, not a hang.
fn read_stdin() -> std::io::Result<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut all = String::new();
        let read = std::io::stdin()
            .take(STDIN_MAX as u64 + 1)
            .read_to_string(&mut all)
            .map(|_| all);
        let _ = tx.send(read);
    });
    match rx.recv_timeout(STDIN_WAIT) {
        Ok(Ok(all)) if all.len() > STDIN_MAX => Err(usage(format!(
            "the card is over {STDIN_MAX} bytes; shorten it\n{FORMAT}"
        ))),
        Ok(read) => read,
        Err(_) => Err(usage(format!(
            "no card arrived on standard input within {} s: write it as a heredoc\n{FORMAT}",
            STDIN_WAIT.as_secs()
        ))),
    }
}

/// `--flag value` out of the words, once.
fn flag<'a>(words: &'a [String], name: &str) -> Option<&'a str> {
    let at = words.iter().position(|w| w == name)?;
    words.get(at + 1).map(String::as_str)
}

/// The daemon's answer, or the exit-3 refusal when it cannot take the request at all.
fn send(action: AskOp, unavailable_is_exit_3: bool) -> std::io::Result<ControlResponse> {
    let (terminal, token) = here();
    let request = ControlRequest::AskWrite {
        id: crate::request_id("ask"),
        terminal,
        token,
        action,
    };
    let answer = ask_daemon_once(request, Duration::from_secs(10));
    let older = |reason: &str| too_old_for(reason, Some("AskWrite"));
    match answer {
        Ok(ControlResponse::Failed { reason, .. }) if older(&reason) && unavailable_is_exit_3 => {
            Err(std::io::Error::other(Unavailable(
                "the questions need a restart of qoop".to_string(),
            )))
        }
        Ok(response) => Ok(response),
        Err(e) if unavailable_is_exit_3 => Err(std::io::Error::other(Unavailable(format!(
            "the questions could not be reached: {e}"
        )))),
        Err(e) => Err(e),
    }
}

fn written(response: ControlResponse) -> std::io::Result<qoop_runtime::control::AskResult> {
    match response {
        ControlResponse::AskWritten { result, .. } => Ok(result),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

/// The inbox: the cards waiting and the ended ones, the last five or (`all`) every one kept.
fn read_all(all: bool) -> std::io::Result<(Vec<AskInfo>, Vec<AskInfo>)> {
    let request = ControlRequest::AskRead {
        id: crate::request_id("ask"),
        all,
    };
    match ask_daemon_once(request, Duration::from_secs(10))? {
        ControlResponse::Asks { live, ended, .. } => Ok((live, ended)),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

fn line(a: &AskInfo) -> String {
    let who = a.agent.as_deref().unwrap_or("-");
    format!(
        "{} {} {} {}: {}",
        a.card, a.status, who, a.body.handle, a.body.asks
    )
}

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest = &args[args.len().min(3)..];
    match args.get(2).map(String::as_str) {
        Some("add") => {
            let card = rest
                .first()
                .filter(|c| !c.starts_with("--"))
                .ok_or_else(|| usage(USAGE))?;
            let agent = flag(rest, "--agent").ok_or_else(|| usage(USAGE))?;
            let handle = flag(rest, "--handle").ok_or_else(|| usage(USAGE))?;
            let urgency = flag(rest, "--urgency").ok_or_else(|| usage(USAGE))?;
            let text = read_stdin()?;
            let body = parse(&text, handle, urgency).map_err(|problems| {
                usage(format!(
                    "the card was not stored:\n  - {}\n{FORMAT}",
                    problems.join("\n  - ")
                ))
            })?;
            let agent = (agent != "-").then(|| agent.to_string());
            // A new card ends that agent's other open one: read them first, so the add can say
            // which question went (it was silent, and a question was lost). A failed read
            // only loses the notice.
            // Stored as the daemon keys it: `52` is `T52`, `ubox/2` is `ubox/T2`.
            let key = agent
                .as_deref()
                .and_then(qoop_runtime::asks::agent_key)
                .map(|(k, _)| k);
            let replaced: Vec<String> = match &key {
                Some(a) => read_all(false)
                    .map(|(live, _)| live)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        c.agent.as_deref() == Some(a.as_str())
                            && c.status == "open"
                            && &c.card != card
                    })
                    .map(|c| format!("replaced {} {}: {}", c.card, c.body.handle, c.body.asks))
                    .collect(),
                None => Vec::new(),
            };
            let options: Vec<String> = body
                .options
                .iter()
                .map(|o| format!("  {}: {}", o.key.to_uppercase(), o.label))
                .collect();
            let sent = send(
                AskOp::Add {
                    card: card.clone(),
                    agent,
                    body: Box::new(body),
                    via_qass: rest.iter().any(|a| a == "--via-qass"),
                },
                true,
            );
            if matches!(&sent, Err(e) if e.get_ref().is_some_and(|i| i.is::<Unavailable>())) {
                // Not stored, so the card is shown as it was written: qass still has it.
                println!("{card}\n{}", options.join("\n"));
            }
            let result = match sent? {
                ControlResponse::Failed { reason, .. }
                    if reason.contains(qoop_store::ask::NOT_FREE) =>
                {
                    return Err(std::io::Error::other(NotFree(reason)));
                }
                answer => written(answer)?,
            };
            println!("{card} {result:?}");
            options.iter().for_each(|o| println!("{o}"));
            if result == qoop_runtime::control::AskResult::Added {
                replaced.iter().for_each(|r| println!("{r}"));
            }
            Ok(())
        }
        Some("close") => {
            let card = rest
                .first()
                .filter(|c| !c.starts_with("--"))
                .ok_or_else(|| usage(USAGE))?;
            let note = flag(rest, "--note").unwrap_or("closed").to_string();
            let result = written(send(
                AskOp::Close {
                    card: card.clone(),
                    note,
                },
                false,
            )?)?;
            println!("{card} {result:?}");
            Ok(())
        }
        Some("answer") => {
            let card = rest
                .first()
                .filter(|c| !c.starts_with("--"))
                .ok_or_else(|| usage(USAGE))?;
            let option = flag(rest, "--option").map(str::to_string);
            let seen = flag(rest, "--seen").map(str::to_string);
            let text = rest
                .iter()
                .position(|w| w == "--text")
                .map(|at| rest[at + 1..].join(" "));
            if option.is_none() && text.is_none() {
                return Err(usage(USAGE));
            }
            let result = written(send(
                AskOp::Answer {
                    card: card.clone(),
                    option,
                    text,
                    seen,
                },
                false,
            )?)?;
            println!("{card} {result:?}");
            Ok(())
        }
        Some("dismiss") => {
            let card = rest
                .first()
                .filter(|c| !c.starts_with("--"))
                .ok_or_else(|| usage(USAGE))?;
            let result = written(send(AskOp::Dismiss { card: card.clone() }, false)?)?;
            println!("{card} {result:?}");
            Ok(())
        }
        Some("ls") => {
            let json_out = rest.iter().any(|a| a == "--json");
            // The JSON is what qass reconciles its notes against: every card that ended, not five.
            let (live, ended) = read_all(json_out)?;
            if rest.iter().any(|a| a == "--json") {
                return json(&serde_json::json!({ "live": live, "ended": ended }));
            }
            if live.is_empty() {
                println!("nothing waits for the owner");
            }
            live.iter().for_each(|a| println!("{}", line(a)));
            ended.iter().for_each(|a| println!("{} (ended)", line(a)));
            Ok(())
        }
        _ => Err(usage(USAGE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "doing: wrote migration 0042, dry run on the copy is clean
why: the PR cannot open before 0042 has run once on staging
asks: Run 0042 on staging now?
A: run now | no rollback exists, staging is a copy | Run 0042 on staging now, then report the row counts.
B: wait for you | you may want to read the SQL first | Do not run 0042 yet; stay idle and wait for the owner.
C: skip | the old app may still read the column | Skip 0042 and take legacy_id out of this PR.
pick: B | a column drop cannot be undone
";

    #[test]
    fn a_good_card_parses_with_its_options_in_order() {
        let card = parse(GOOD, "migrations", "now").unwrap();
        assert_eq!(card.asks, "Run 0042 on staging now?");
        assert_eq!(card.asked, None);
        let keys: Vec<&str> = card.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert_eq!(card.options[1].label, "wait for you");
        assert_eq!(
            card.options[1].words,
            "Do not run 0042 yet; stay idle and wait for the owner."
        );
        let pick = card.pick.unwrap();
        assert_eq!(
            (pick.key.as_str(), pick.reason.as_str()),
            ("b", "a column drop cannot be undone")
        );
    }

    #[test]
    fn the_words_may_hold_a_pipe_and_the_fields_may_come_in_any_order() {
        let text = "asks: Which?\nA: yes | faster | run it\nB: no | safer | echo a | tee b\nwhy: now\ndoing: x\nasked: y";
        let card = parse(text, "h", "later").unwrap();
        assert_eq!(card.options[1].words, "echo a | tee b");
        assert_eq!(card.asked.as_deref(), Some("y"));
    }

    #[test]
    fn options_out_of_order_and_a_field_given_empty_are_refused() {
        let swapped = "doing: d\nwhy: w\nasks: q\nB: b | r | w\nA: a | r | w";
        let said = parse(swapped, "h", "now").unwrap_err().join("\n");
        assert!(said.contains("keyed A to D in order"), "{said}");
        let empty = "doing:\nwhy: w\nasks: q\nA: a | r | w\nB: b | r | w";
        let said = parse(empty, "h", "now").unwrap_err().join("\n");
        assert!(
            said.contains("doing is empty") && !said.contains("doing is missing"),
            "{said}"
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let text =
            "doing: a\ndoing: b\nwhat: c\nA: only | two\nB: x | y | z\nno colon here\npick: Z | r";
        let problems = parse(text, "h", "now").unwrap_err().join("\n");
        for want in [
            "doing is given twice",
            "\"what\" is not a field",
            "option A is `label | reason | words`",
            "line 6 has no `key: value`",
            "why is missing",
            "asks is missing",
            "two to four options",
            "pick names z, which is not an option",
        ] {
            assert!(problems.contains(want), "{want:?} in\n{problems}");
        }
    }

    #[test]
    fn limits_and_empty_values_are_refused_never_cut() {
        let long = "x".repeat(201);
        let text = format!("doing: {long}\nwhy: w\nasks: q\nA: a |  | words\nB: b | r | w");
        let problems = parse(&text, "h", "now").unwrap_err().join("\n");
        assert!(
            problems.contains("doing is 201 characters, over the 200 limit"),
            "{problems}"
        );
        assert!(problems.contains("option a reason is empty"), "{problems}");
        let at_limit = "x".repeat(200);
        let ok = format!("doing: {at_limit}\nwhy: w\nasks: q\nA: a | r | w\nB: b | r | w");
        assert!(parse(&ok, "h", "now").is_ok());
    }

    #[test]
    fn an_empty_block_and_a_bad_handle_or_urgency_are_refused() {
        assert!(parse("", "h", "now").is_err());
        let ok = "doing: d\nwhy: w\nasks: q\nA: a | r | w\nB: b | r | w";
        assert!(
            parse(ok, "Korea", "now")
                .unwrap_err()
                .join(" ")
                .contains("handle")
        );
        assert!(
            parse(ok, "korea", "soon")
                .unwrap_err()
                .join(" ")
                .contains("urgency")
        );
        assert!(parse("doing: d\nwhy: w\nasks: q\nA: a | r | w", "h", "now").is_err());
    }

    /// The plan card qass copies from its skill must be a card the parser takes: its options
    /// one a line, approve first, so the phone and qass's screen show A as the main button.
    /// No option's words hold a slot qass cannot fill when it writes the card (his words come
    /// with his answer): only the item's key.
    #[test]
    fn the_plan_card_in_the_skill_parses_with_approve_first() {
        let skill = include_str!("../../../skills/qass/plans.md");
        let start = skill
            .find("asked: <KEY> plan needs you")
            .expect("the card example");
        let block = &skill[start..];
        let block = &block[..block.find("```").expect("its fence")];
        let words: Vec<&str> = block
            .lines()
            .filter(|l| {
                l.len() > 2 && l.as_bytes()[1] == b':' && l.as_bytes()[0].is_ascii_uppercase()
            })
            .filter_map(|l| l.split('|').nth(2))
            .collect();
        assert_eq!(words.len(), 2, "{block}");
        for w in &words {
            assert!(
                !w.replace("<KEY>", "").contains('<'),
                "a slot in the words: {w}"
            );
        }
        // Its placeholders filled in, as qass writes them.
        let mut card = String::new();
        let mut rest = block;
        while let Some(open) = rest.find('<') {
            card.push_str(&rest[..open]);
            let close = rest[open..].find('>').expect("a closed placeholder") + open;
            card.push_str("QOOP-61");
            rest = &rest[close + 1..];
        }
        card.push_str(rest);
        let parsed = parse(&card, "plan", "now").unwrap_or_else(|p| panic!("{p:?}\n{card}"));
        let labels: Vec<&str> = parsed.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["Approve, build it", "Stop"]);
    }
}

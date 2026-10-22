//! `qoop schedule`: start an agent at a time, once or on a repeat.
//!
//! The daemon keeps the schedules and fires them (`qoop-runtime`'s `schedules.rs`); this is the
//! owner's way in, and qass's through its skill. `fire` is the other half of a run: the daemon
//! starts it as a child at a slot it has claimed, and it opens the terminal, types the agent
//! line and reports how that went. It is not for people and is not in `qoop help`.

use std::time::Duration;

use qoop_runtime::control::{
    ControlRequest, ControlResponse, ScheduleInfo, ScheduleOp, too_old_for,
};
use qoop_runtime::schedule::{Cron, Zone, parse_at, system_zone};
use qoop_tui::config;

use crate::qass::{first_message_args, program_of};
use crate::supervise::{NewOpts, open_terminal};
use crate::usage;

const USAGE: &str = "usage:
  qoop schedule add <name> (--cron \"<m h dom mon dow>\" | --at <HH:MM|YYYY-MM-DD HH:MM|+30s|+10m|+2h>)
                    [--agent NAME] [--cwd DIR] [--space NAME] [--json] -- <prompt>
  qoop schedule ls [--json]
  qoop schedule pause|resume|rm|run <id|name>
  times are the computer's local time; a bare HH:MM that has passed today means tomorrow";

/// How long the daemon has to answer.
const PATIENCE: Duration = Duration::from_secs(10);

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest = &args[args.len().min(2)..];
    match rest.first().map(String::as_str) {
        Some("add") => add(&rest[1..]),
        Some("ls") => list(&rest[1..]),
        Some(verb @ ("pause" | "resume" | "rm" | "run")) => change(verb, &rest[1..]),
        Some("fire") => fire(&rest[1..]),
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(usage(USAGE)),
    }
}

/// One round trip, the daemon started when there is none: a schedule is set from a window
/// that may have closed the daemon behind it.
fn send(request: ControlRequest) -> std::io::Result<ControlResponse> {
    let dir = qoop_runtime::paths::runtime_dir().map_err(std::io::Error::other)?;
    crate::ensure_daemon(&qoop_runtime::paths::socket_path(&dir))?;
    answered(crate::task::ask(request, PATIENCE)?)
}

/// A refusal as the error it is, an old daemon said so.
fn answered(response: ControlResponse) -> std::io::Result<ControlResponse> {
    match response {
        ControlResponse::Failed { reason, .. }
            if too_old_for(&reason, Some("ScheduleWrite"))
                || too_old_for(&reason, Some("ScheduleRead")) =>
        {
            Err(std::io::Error::other(
                "the running qoop daemon is older; run `qoop restart`",
            ))
        }
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Ok(other),
    }
}

fn read_all() -> std::io::Result<Vec<ScheduleInfo>> {
    match send(ControlRequest::ScheduleRead {
        id: crate::request_id("schedule"),
    })? {
        ControlResponse::Schedules { schedules, .. } => Ok(schedules),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

/// A write, with this terminal's token when it runs inside one: a worker's is refused by the
/// daemon, which is the point, and qass's is let through.
fn write(action: ScheduleOp) -> std::io::Result<i64> {
    let (terminal, token) = crate::task::here();
    match send(ControlRequest::ScheduleWrite {
        id: crate::request_id("schedule"),
        terminal,
        token,
        action: Box::new(action),
    })? {
        ControlResponse::ScheduleWritten { schedule, .. } => Ok(schedule),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

/// The agent names a schedule may use: qass's four and the person's own `[agents]` lines.
fn agent_ok(agent: &str, launchers: &[config::Launcher]) -> Result<(), String> {
    let program = launchers
        .iter()
        .find(|l| l.name == agent)
        .map_or(agent, |l| program_of(&l.command));
    if first_message_args(program, "x").is_some() {
        return Ok(());
    }
    Err(format!(
        "{agent} cannot be started with a message; scheduled runs use {} or a name from [agents] \
         that runs one of them",
        config::QASS_AGENTS.join(", ")
    ))
}

fn add(args: &[String]) -> std::io::Result<()> {
    let (mut name, mut cron, mut at, mut agent, mut cwd, mut space) =
        (None, None, None, None, None, None);
    let mut json = false;
    let mut prompt = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let mut value = || words_next(&mut words);
        match word.as_str() {
            "--cron" => cron = Some(value()?),
            "--at" => at = Some(value()?),
            "--agent" => agent = Some(value()?),
            "--cwd" => cwd = Some(value()?),
            "--space" => space = Some(value()?),
            "--json" => json = true,
            "--" => {
                prompt = words.by_ref().cloned().collect();
                break;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other if other.starts_with("--") => return Err(usage(USAGE)),
            _ if name.is_none() => name = Some(word.clone()),
            _ => return Err(usage(USAGE)),
        }
    }
    let name = name.ok_or_else(|| usage(USAGE))?;
    let prompt = crate::task::text(&prompt)?.trim().to_string();
    if prompt.is_empty() {
        return Err(usage(USAGE));
    }
    let (cron, at) = match (cron, at) {
        (Some(cron), None) => {
            Cron::parse(&cron).map_err(|e| std::io::Error::other(format!("{cron:?}: {e}")))?;
            (Some(cron), None)
        }
        (None, Some(text)) => {
            let at = parse_at(&text, qoop_runtime::store::now(), system_zone())
                .map_err(std::io::Error::other)?;
            (None, Some(at))
        }
        _ => return Err(usage(USAGE)),
    };
    let read = config::read();
    let agent = agent.unwrap_or(read.qass_agent);
    agent_ok(&agent, &read.agents).map_err(std::io::Error::other)?;
    let cwd = match cwd {
        Some(dir) => std::path::absolute(dir)?,
        None => std::env::current_dir()?,
    };
    let schedule = write(ScheduleOp::Add {
        name,
        cron,
        at,
        agent: Some(agent),
        cwd: cwd.to_string_lossy().into_owned(),
        space,
        prompt,
    })?;
    let made = read_all()?
        .into_iter()
        .find(|s| s.id == schedule)
        .ok_or_else(|| std::io::Error::other("the schedule was added and is not there"))?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&made).map_err(std::io::Error::other)?
        );
    } else {
        let next = made.next_at.map_or_else(String::new, |at| {
            format!(", next {}", clock_of(at, system_zone()))
        });
        println!("added {} {}: {}{next}", made.id, made.name, made.when);
    }
    Ok(())
}

fn words_next(words: &mut std::slice::Iter<'_, String>) -> std::io::Result<String> {
    words.next().cloned().ok_or_else(|| usage(USAGE))
}

/// A moment as the list shows it: the time today, else the date and the time.
fn clock_of(at: i64, zone: Zone) -> String {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let (wall, today) = ((zone.local)(at), (zone.local)(qoop_runtime::store::now()));
    if (wall.year, wall.month, wall.day) == (today.year, today.month, today.day) {
        wall.clock()
    } else {
        format!(
            "{} {} {}",
            MONTHS[(wall.month - 1) as usize],
            wall.day,
            wall.clock()
        )
    }
}

fn list(args: &[String]) -> std::io::Result<()> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(usage(USAGE)),
    };
    let all = read_all()?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&all).map_err(std::io::Error::other)?
        );
        return Ok(());
    }
    if all.is_empty() {
        println!("no schedules");
        return Ok(());
    }
    let zone = system_zone();
    for s in all {
        let next = match (s.paused, s.next_at) {
            (true, _) => "paused".to_string(),
            (false, Some(at)) => format!("next {}", clock_of(at, zone)),
            (false, None) => "done".to_string(),
        };
        let last = s
            .last_result
            .as_deref()
            .map_or_else(String::new, |r| match s.last_terminal {
                Some(t) => format!("  last {r}, tab {t}"),
                None => format!("  last {r}"),
            });
        println!("{:>3}  {}  {}  {next}{last}", s.id, s.name, s.when);
    }
    Ok(())
}

/// A schedule named by its id, or by its name (a name is never only digits).
fn resolve(wanted: &str) -> std::io::Result<i64> {
    let all = read_all()?;
    let found = match wanted.parse::<i64>() {
        Ok(id) => all.iter().find(|s| s.id == id),
        Err(_) => all.iter().find(|s| s.name == wanted),
    };
    found
        .map(|s| s.id)
        .ok_or_else(|| std::io::Error::other(format!("no schedule {wanted}")))
}

fn change(verb: &str, args: &[String]) -> std::io::Result<()> {
    let [wanted] = args else {
        return Err(usage(USAGE));
    };
    let schedule = resolve(wanted)?;
    let (action, done) = match verb {
        "pause" => (ScheduleOp::Pause { schedule }, "paused"),
        "resume" => (ScheduleOp::Resume { schedule }, "resumed"),
        "rm" => (ScheduleOp::Remove { schedule }, "removed"),
        _ => (ScheduleOp::Run { schedule }, "started"),
    };
    write(action)?;
    println!("{done} {schedule}");
    Ok(())
}

/// The tab of a schedule's runs is named `<name> HH:MM`: the time the run was due, so a run
/// stays addressable by the clock on its tab.
fn tab_name(name: &str, at: i64, zone: Zone) -> String {
    format!("{name} {}", (zone.local)(at).clock())
}

/// Whether `tab` is a run of the schedule `name`: its name and a clock time, nothing else.
fn is_run_of(tab: &str, name: &str) -> bool {
    tab.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|clock| {
            let digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
            clock
                .split_once(':')
                .is_some_and(|(h, m)| digits(h) && digits(m))
        })
}

/// Whether an earlier run of `name` is still working in a tab: the next one would pile on it.
fn still_working(name: &str) -> std::io::Result<bool> {
    let ControlResponse::Agents { agents, .. } = answered(crate::task::ask(
        ControlRequest::ListAgents {
            id: crate::request_id("schedule"),
        },
        PATIENCE,
    )?)?
    else {
        return Ok(false);
    };
    Ok(agents.iter().any(|row| {
        row.agent
            && row.state == "working"
            && row.name.as_deref().is_some_and(|tab| is_run_of(tab, name))
    }))
}

/// What the line typed into the new terminal is: a person's `[agents]` line as written, or the
/// program's name, then the prompt as that program takes a first message.
fn launch(
    agent: &str,
    prompt: &str,
    launchers: &[config::Launcher],
) -> Result<(Option<String>, Vec<String>), String> {
    let line = launchers
        .iter()
        .find(|l| l.name == agent)
        .map(|l| l.command.clone());
    let program = line.as_deref().map_or(agent, program_of).to_string();
    // A prompt that starts with a dash (a list of steps) would be read as the agent's own flag,
    // and `--dangerously-skip-permissions` as a prompt would change how it runs. A leading
    // space makes it a word for every agent, and none of them minds the space.
    let spaced;
    let prompt = if prompt.starts_with('-') {
        spaced = format!(" {prompt}");
        spaced.as_str()
    } else {
        prompt
    };
    let Some(said) = first_message_args(&program, prompt) else {
        return Err(format!(
            "{agent} runs {program}, which cannot be started with a message"
        ));
    };
    let command = if line.is_some() {
        said
    } else {
        std::iter::once(agent.to_string()).chain(said).collect()
    };
    Ok((line, command))
}

/// `qoop schedule fire <id> --slot <s> [--now]`: what the daemon starts at a claimed slot.
fn fire(args: &[String]) -> std::io::Result<()> {
    let bad = || usage("usage: qoop schedule fire <id> --slot <s> [--now]");
    let (mut id, mut slot, mut now) = (None, None, false);
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--slot" => slot = words.next().and_then(|s| s.parse::<i64>().ok()),
            "--now" => now = true,
            _ if id.is_none() => id = word.parse::<i64>().ok(),
            _ => return Err(bad()),
        }
    }
    let (Some(id), Some(slot)) = (id, slot) else {
        return Err(bad());
    };
    // The daemon starts this with no terminal's identity. One that has it is a worker running
    // it by hand, which is the write the daemon refuses it everywhere else.
    if crate::task::here().0.is_some() {
        return Err(std::io::Error::other("a run is started by the daemon"));
    }
    let Some(row) = read_all()?.into_iter().find(|s| s.id == id) else {
        return Ok(());
    };
    // Removed, or claimed again since: this run is not the slot's any more.
    if row.last_slot.unwrap_or(0) != slot {
        return Ok(());
    }
    let report = |result: String, terminal: Option<u32>| {
        write(ScheduleOp::Report {
            schedule: id,
            slot,
            result,
            terminal: terminal.map(i64::from),
        })
        .map(|_| ())
    };
    // `--now` is the owner asking, which is not held back by a run that is still going.
    if !now && still_working(&row.name)? {
        return report("skipped: last run still working".into(), None);
    }
    let read = config::read();
    let zone = system_zone();
    let due = if now || slot == 0 {
        qoop_runtime::store::now()
    } else {
        slot
    };
    let (typed, command) = match launch(&row.agent, &row.prompt, &read.agents) {
        Ok(line) => line,
        Err(why) => return report(format!("failed: {why}"), None),
    };
    let opts = NewOpts {
        name: Some(tab_name(&row.name, due, zone)),
        cwd: Some(row.cwd.clone().into()),
        space: row.space.clone(),
        command,
        typed,
    };
    let mut opened = None;
    match open_terminal(&opts, |terminal| opened = Some(terminal)) {
        Ok(()) => report("started".into(), opened),
        Err(e) => {
            let _ = report(format!("failed: {e}"), opened);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use qoop_runtime::schedule::Naive;

    use super::*;

    fn launchers() -> Vec<config::Launcher> {
        vec![config::Launcher {
            name: "sonnet".into(),
            command: "FOO=1 claude --model sonnet".into(),
        }]
    }

    #[test]
    fn a_run_starts_the_agent_by_name_or_by_the_line_as_written() {
        let (typed, command) = launch("claude", "do the thing", &launchers()).unwrap();
        assert_eq!(typed, None);
        assert_eq!(command, ["claude", "do the thing"]);
        let (typed, command) = launch("gemini", "x", &[]).unwrap();
        assert_eq!(
            (typed, command),
            (None, vec!["gemini".into(), "-i".into(), "x".into()])
        );
        let (typed, command) = launch("sonnet", "do the thing", &launchers()).unwrap();
        assert_eq!(typed.as_deref(), Some("FOO=1 claude --model sonnet"));
        assert_eq!(
            command,
            ["do the thing"],
            "the line is typed as written, then the prompt"
        );
        let (_, command) = launch("claude", "--dangerously-skip-permissions", &[]).unwrap();
        assert_eq!(
            command,
            ["claude", " --dangerously-skip-permissions"],
            "a prompt that starts with a dash is never a flag"
        );
        assert!(launch("aider", "x", &[]).is_err());
        assert!(agent_ok("opencode", &[]).is_ok());
        assert!(agent_ok("sonnet", &launchers()).is_ok());
        assert!(agent_ok("aider", &launchers()).is_err());
    }

    #[test]
    fn a_tab_of_a_run_is_named_for_the_schedule_and_the_time_it_was_due() {
        let zone = Zone {
            local: |at| Naive::from_secs(at + 3 * 3600),
            unix: |wall| wall.secs() - 3 * 3600,
        };
        let at = Naive::new(2026, 10, 22, 6, 5).secs();
        assert_eq!(tab_name("morning", at, zone), "morning 09:05");
        assert!(is_run_of("morning 09:05", "morning"));
        assert!(!is_run_of("morning", "morning"));
        assert!(!is_run_of("morning-2 09:05", "morning"));
        assert!(!is_run_of("morning 9:05", "morning"));
        assert!(!is_run_of("morning review", "morning"));
    }
}

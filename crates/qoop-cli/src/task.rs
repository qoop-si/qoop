//! `qoop task`: the work this terminal's agent is doing, recorded where qoop keeps it.
//!
//! An agent opens its task, writes its plan and evidence, and says when it believes it is
//! done; qass or the person checks and closes it. Inside a qoop terminal `QOOP_TERMINAL`
//! and `QOOP_TOKEN` say which terminal is asking, so no id is typed for "my task".

use std::io::Read;

use qoop_runtime::control::{
    ControlRequest, ControlResponse, TaskEventInfo, TaskInfo, TaskOp, TaskOutcome,
};
use qoop_runtime::paths;

use crate::usage;

/// What every agent opened in qoop is told at the start of a session, by its own hook.
/// Short on purpose: it is in every session's context.
pub(crate) const RULES: &str = "You are running inside qoop, which keeps a task record per \
terminal. It may open one by itself when you edit a file; `qoop task show` says whether there is \
one, and `qoop task new \"<outcome>\"` opens it. For real work (changing files, a multi-step investigation), give it a plan \
with `qoop task plan -` (acceptance, steps, how you will verify it), move it with `qoop task \
stage <stage>` (investigating, planning, implementing, reviewing, verifying, waiting-human), add \
`qoop task evidence \"<check: result>\"` for each check that proves an acceptance point, and \
finish with `qoop task done \"<summary>\"`. Done is a claim checked outside you, so keep working \
or stop as usual. If a `qoop task` command fails, carry on with the work.";

/// How long `rules` waits for the daemon. A hook waits on it at every session start and
/// every request, and gives it 2 s before going quiet; a daemon answers in milliseconds.
const RULES_PATIENCE: std::time::Duration = std::time::Duration::from_millis(800);

/// The most read from standard input for one text.
const STDIN_MAX: usize = 16_000;

/// The space qass lives in: its own sessions are not told to open tasks.
const QASS_SPACE: &str = crate::qass::SPACE;

const USAGE: &str = "usage:
  qoop task new [--replace] <goal>     open this terminal's task
  qoop task stage <stage>              investigating|planning|implementing|reviewing|verifying|waiting-human
  qoop task plan <text|->              acceptance, plan, how it will be verified, risks
  qoop task evidence <text|->          a check that proves an acceptance point, and its result
  qoop task done [summary]             claim it is done; it is checked outside the agent
  qoop task close <id> --verified|--failed [reason]
  qoop task ls [--json]                open tasks
  qoop task show [id] [--before E] [--json]   no id: this terminal's open task
  qoop task begin                      this terminal's open task, opened now if none (hooks)
  qoop task rules [--turn]             what an agent in qoop is told at session start
                                       (--turn: the line it is given with each request)
  <text> may be `-` to read it from standard input";

/// This terminal and its token, when run inside a qoop terminal.
fn here() -> (Option<u32>, Option<String>) {
    let terminal = std::env::var("QOOP_TERMINAL")
        .ok()
        .and_then(|t| t.parse().ok());
    let token = std::env::var("QOOP_TOKEN").ok().filter(|t| !t.is_empty());
    match (terminal, token) {
        (Some(t), Some(k)) => (Some(t), Some(k)),
        _ => (None, None),
    }
}

/// The words after the subcommand as one text, or standard input for `-`.
fn text(words: &[String]) -> std::io::Result<String> {
    if words.len() == 1 && words[0] == "-" {
        // Bounded: one request is one frame, and the longest text kept is far below this.
        let mut all = String::new();
        std::io::stdin()
            .take(STDIN_MAX as u64 + 1)
            .read_to_string(&mut all)?;
        if all.len() > STDIN_MAX {
            return Err(std::io::Error::other(format!(
                "over {STDIN_MAX} bytes on standard input; a plan is at most 2500 characters"
            )));
        }
        return Ok(all);
    }
    Ok(words.join(" "))
}

/// An older daemon's refusal of a method it does not know tells the caller to restart it,
/// which would end every tab. Said plainly instead.
fn explain(reason: String) -> std::io::Error {
    if reason.starts_with("this daemon is older") {
        return std::io::Error::other(
            "the running qoop daemon is older than this command and has no tasks; it picks \
             them up the next time it restarts on its own",
        );
    }
    std::io::Error::other(reason)
}

fn ask(request: ControlRequest, patience: std::time::Duration) -> std::io::Result<ControlResponse> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let dir = paths::runtime_dir().map_err(std::io::Error::other)?;
        let socket = paths::socket_path(&dir);
        tokio::time::timeout(patience, crate::ask_daemon(&socket, "qoop-task", &request))
            .await
            .map_err(|_| std::io::Error::other("the daemon did not answer in time"))?
    })
}

fn write(task: Option<i64>, action: TaskOp) -> std::io::Result<i64> {
    // A task named by id is spoken for by nobody: qass closes a worker's task from its own
    // terminal, and sending that terminal along would make the daemon ask whether the task
    // is qass's.
    let (terminal, token) = match task {
        Some(_) => (None, None),
        None => here(),
    };
    if task.is_none() && terminal.is_none() {
        return Err(std::io::Error::other(
            "QOOP_TERMINAL and QOOP_TOKEN are not set: run this in a qoop terminal, or name the task",
        ));
    }
    let request = ControlRequest::TaskWrite {
        id: crate::request_id("task"),
        terminal,
        token,
        task,
        action,
    };
    match ask(request, std::time::Duration::from_secs(10))? {
        ControlResponse::TaskWritten { task, .. } => Ok(task),
        ControlResponse::Failed { reason, .. } => Err(explain(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

struct Found {
    tasks: Vec<TaskInfo>,
    events: Vec<TaskEventInfo>,
    more: bool,
    space: Option<String>,
}

fn read(
    task: Option<i64>,
    before: Option<i64>,
    mine: bool,
    patience: std::time::Duration,
) -> std::io::Result<Found> {
    let (terminal, token) = if mine { here() } else { (None, None) };
    let request = ControlRequest::TaskRead {
        id: crate::request_id("task"),
        task,
        terminal,
        token,
        before,
    };
    match ask(request, patience)? {
        ControlResponse::Tasks {
            tasks,
            events,
            more,
            space,
            ..
        } => Ok(Found {
            tasks,
            events,
            more,
            space,
        }),
        ControlResponse::Failed { reason, .. } => Err(explain(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

fn ago(at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(at);
    let secs = (now - at).max(0);
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn line(t: &TaskInfo) -> String {
    let state = match (&t.outcome, t.claimed_at) {
        (Some(outcome), _) => outcome.clone(),
        (None, Some(_)) => format!("{}, claimed done", t.stage),
        (None, None) => t.stage.clone(),
    };
    let place = t
        .terminal
        .map(|n| format!(" terminal {n}"))
        .unwrap_or_default();
    format!(
        "#{} {} ({state}){place}, updated {}",
        t.id,
        t.goal,
        ago(t.updated_at)
    )
}

fn json(value: &impl serde::Serialize) -> std::io::Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(std::io::Error::other)?
    );
    Ok(())
}

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest = &args[args.len().min(3)..];
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    // The words of a text, raw: evidence like `cargo test --workspace: ok` keeps its flags.
    // Only the subcommands that take a flag of their own drop it.
    let raw: Vec<String> = rest.to_vec();
    let without = |flags: &[&str]| -> Vec<String> {
        rest.iter()
            .filter(|a| !flags.contains(&a.as_str()))
            .cloned()
            .collect()
    };
    match args.get(2).map(String::as_str) {
        Some("new") => {
            // Only a leading `--replace` is the flag: a goal may mention the word.
            let replace = rest.first().is_some_and(|a| a == "--replace");
            let goal = text(&rest[usize::from(replace)..])?;
            let id = write(None, TaskOp::New { goal, replace })?;
            println!("task #{id}");
            Ok(())
        }
        Some("stage") => {
            let [stage] = raw.as_slice() else {
                return Err(usage(USAGE));
            };
            write(
                None,
                TaskOp::Stage {
                    stage: stage.clone(),
                },
            )
            .map(drop)
        }
        // What an agent's hook runs at its first file edit: quiet, and never in the way.
        Some("begin") => write(None, TaskOp::Begin).map(drop),
        Some("plan") => write(None, TaskOp::Plan { text: text(&raw)? }).map(drop),
        Some("evidence") => write(None, TaskOp::Evidence { text: text(&raw)? }).map(drop),
        Some("done") => write(
            None,
            TaskOp::Done {
                summary: text(&raw)?,
            },
        )
        .map(drop),
        Some("close") => {
            let outcome = match (has("--verified"), has("--failed")) {
                (true, false) => TaskOutcome::Verified,
                (false, true) => TaskOutcome::Failed,
                _ => return Err(usage(USAGE)),
            };
            let words = without(&["--verified", "--failed"]);
            let Some((id, reason)) = words.split_first() else {
                return Err(usage(USAGE));
            };
            let id: i64 = id
                .trim_start_matches('#')
                .parse()
                .map_err(|_| usage(USAGE))?;
            write(
                Some(id),
                TaskOp::Close {
                    outcome,
                    reason: reason.join(" "),
                },
            )
            .map(drop)
        }
        Some("ls") => {
            let found = read(None, None, false, std::time::Duration::from_secs(10))?;
            if has("--json") {
                return json(&found.tasks);
            }
            if found.tasks.is_empty() {
                println!("no open task");
            }
            for t in &found.tasks {
                println!("{}", line(t));
            }
            if found.more {
                println!("(more not shown)");
            }
            Ok(())
        }
        Some("show") => {
            let mut task = None;
            let mut before = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--json" => {}
                    "--before" => {
                        before = Some(
                            it.next()
                                .and_then(|v| v.parse().ok())
                                .ok_or_else(|| usage(USAGE))?,
                        )
                    }
                    other => {
                        task = Some(
                            other
                                .trim_start_matches('#')
                                .parse()
                                .map_err(|_| usage(USAGE))?,
                        )
                    }
                }
            }
            if task.is_none() && here().0.is_none() {
                return Err(usage("not inside a qoop terminal: qoop task show <id>"));
            }
            let found = read(
                task,
                before,
                task.is_none(),
                std::time::Duration::from_secs(10),
            )?;
            if has("--json") {
                return json(&serde_json::json!({
                    "task": found.tasks.first(),
                    "events": found.events,
                    "more": found.more,
                }));
            }
            let Some(t) = found.tasks.first() else {
                println!("no open task here");
                return Ok(());
            };
            println!("{}", line(t));
            for e in &found.events {
                let mut lines = e.body.lines();
                println!(
                    "  [{}] {} {}: {}",
                    e.id,
                    ago(e.at),
                    e.kind,
                    lines.next().unwrap_or("")
                );
                for more in lines {
                    println!("      {more}");
                }
            }
            if found.more
                && let Some(last) = found.events.last()
            {
                println!("  older: qoop task show {} --before {}", t.id, last.id);
            }
            Ok(())
        }
        Some("rules") => {
            // Fast and quiet: a hook waits on this at every session start and request. No daemon in time
            // means the plain rules; qass's own space gets nothing.
            let (terminal, _) = here();
            let found = terminal.and_then(|_| read(None, None, true, RULES_PATIENCE).ok());
            if found.as_ref().and_then(|f| f.space.as_deref()) == Some(QASS_SPACE) {
                return Ok(());
            }
            let open = found.as_ref().and_then(|f| f.tasks.first());
            // Every prompt, one line: measured on a real Claude, the rules given once at the
            // start of a session were read and then not followed. A line beside each request
            // is what an agent acts on.
            if has("--turn") {
                match open {
                    Some(t) => println!(
                        "qoop: this terminal's task #{} is open (stage {}). Record each check \
                         that proves it with `qoop task evidence`, and `qoop task done` when \
                         finished.",
                        t.id, t.stage
                    ),
                    None => println!(
                        "qoop: if this request is real work, qoop opens its task when you edit \
                         a file; give it a plan with `qoop task plan -` and record checks with \
                         `qoop task evidence`."
                    ),
                }
                return Ok(());
            }
            println!("{RULES}");
            if let Some(t) = open {
                // Said as a record, not as an instruction: the goal is text an agent wrote, and
                // it reaches every later session in this terminal.
                let goal: String = t.goal.chars().take(100).collect();
                println!(
                    "Your open task #{}, as recorded earlier: \"{goal}\" (stage {}).",
                    t.id, t.stage
                );
            }
            Ok(())
        }
        _ => Err(usage(USAGE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_daemon_is_never_restarted_on_its_own_advice() {
        let said = explain(
            "this daemon is older than the client that called it: it does not know the method \
             that was asked for (unknown variant). `qoop stop`, then `qoop`, picks up the new \
             daemon; what is open comes back."
                .into(),
        )
        .to_string();
        assert!(!said.contains("qoop stop"), "{said}");
        assert!(said.contains("older than this command"), "{said}");
        assert_eq!(explain("no task #4".into()).to_string(), "no task #4");
    }

    #[test]
    fn the_rules_stay_short() {
        assert!(RULES.len() <= 900, "{} chars in every session", RULES.len());
        assert!(
            !RULES.contains("--replace"),
            "no invitation to replace a task"
        );
    }
}

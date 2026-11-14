//! `qoop race`: one prompt to several agents, each on a branch, in a worktree and a tab of its
//! own, then judged by what each changed. Nothing is kept but git: the branches
//! `race/<name>/<agent>`, their worktrees, and the ref `refs/qoop/race/<name>` holding the base
//! every racer started from. The tabs are found by their name (the branch), else by folder.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use qoop_runtime::control::AgentRow;

use crate::supervise::NewOpts;
use crate::worktree::{self, Worktree};

const USAGE: &str = "usage:\n\
     \x20 qoop race new <name> --agents A,B[,...] [--base REF] [--wait [--timeout SECS]] -- <prompt...>|-\n\
     \x20 qoop race ls <name>";

/// What every racer is told after the prompt.
const RULE: &str = "Race rules: change files in this worktree only. Do not commit, push, open a \
                    PR, merge, close your tab or start agents or races. When you are done, end \
                    with a short self-report: what you changed, what you ran, what is left.";

/// The fewest and the most agents in one race.
const AGENTS: std::ops::RangeInclusive<usize> = 2..=5;

/// How long a racer's tab may sit ready at its shell before it counts as never started.
const START_PATIENCE: Duration = Duration::from_secs(30);

/// `qoop race new|ls`.
pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    let rest = args.get(3..).unwrap_or_default();
    match args.get(2).map(String::as_str) {
        Some("new") => new(rest),
        Some("ls") => ls(rest),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(crate::usage(USAGE)),
    }
}

/// Parsed `qoop race new` arguments.
#[derive(Debug, PartialEq)]
struct NewArgs {
    name: String,
    agents: Vec<String>,
    base: Option<String>,
    wait: bool,
    timeout: Option<Duration>,
    prompt: Vec<String>,
}

/// A race name: lowercase letters, digits and dashes, a letter or digit first, at most 32.
fn name_ok(name: &str) -> bool {
    name.len() <= 32
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn parse_new(args: &[String]) -> Result<NewArgs, String> {
    let bad = |why: &str| format!("{why}; {USAGE}");
    let name = args.first().cloned().unwrap_or_default();
    if !name_ok(&name) {
        return Err(bad(
            "a race name is lowercase letters, digits and dashes, a letter or digit first, at most 32",
        ));
    }
    let (mut agents, mut base, mut wait, mut timeout, mut prompt) =
        (Vec::new(), None, false, None, Vec::new());
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| bad(&format!("missing value for {arg}")))
        };
        match arg.as_str() {
            "--agents" => agents = value()?.split(',').map(|a| a.trim().to_string()).collect(),
            "--base" => {
                let from = value()?;
                if from.starts_with('-') {
                    return Err(bad("--base cannot start with a dash"));
                }
                base = Some(from);
            }
            "--wait" => wait = true,
            "--timeout" => {
                let secs = value()?
                    .parse()
                    .map_err(|_| bad("--timeout takes seconds"))?;
                timeout = Some(Duration::from_secs(secs));
            }
            "--" => {
                prompt = it.by_ref().cloned().collect();
                break;
            }
            other => return Err(bad(&format!("unexpected {other}"))),
        }
    }
    if !AGENTS.contains(&agents.len()) || agents.iter().any(String::is_empty) {
        return Err(bad("--agents takes 2 to 5 agents, comma separated"));
    }
    if timeout.is_some() && !wait {
        return Err(bad("--timeout only applies with --wait"));
    }
    if prompt.join(" ").trim().is_empty() {
        return Err(bad("no prompt"));
    }
    Ok(NewArgs {
        name,
        agents,
        base,
        wait,
        timeout,
        prompt,
    })
}

/// Each racer's name: its agent, and a second of one agent `-2`, a third `-3`.
fn labels(agents: &[String]) -> Vec<String> {
    agents
        .iter()
        .enumerate()
        .map(
            |(i, a)| match agents[..i].iter().filter(|b| *b == a).count() {
                0 => a.clone(),
                n => format!("{a}-{}", n + 1),
            },
        )
        .collect()
}

fn branch_of(name: &str, label: &str) -> String {
    format!("race/{name}/{label}")
}

fn ref_of(name: &str) -> String {
    format!("refs/qoop/race/{name}")
}

/// The first message: the prompt, a blank line, then the racer rule.
fn first_message(prompt: &str) -> String {
    format!("{}\n\n{RULE}", prompt.trim_end())
}

/// The owner's gate: a race is started by the person (their own shell tab, or outside qoop)
/// or by qass, never from a worker agent's terminal. Read from the caller's row, not the
/// daemon: a speed bump like [`crate::qass_guard`], not a lock.
fn refuse_a_worker() -> std::io::Result<()> {
    let (Some(me), Some(_)) = crate::task::here() else {
        return Ok(());
    };
    let agent = worktree::session_rows()?
        .iter()
        .any(|r| r.terminal == me && r.agent);
    if agent && !crate::qass::called_by_qass() {
        return Err(std::io::Error::other(
            "a race is started by the person or qass, not from an agent's terminal",
        ));
    }
    Ok(())
}

/// One racer about to start.
struct Racer {
    agent: String,
    label: String,
    branch: String,
    path: PathBuf,
}

/// `qoop race new`: check everything first, write the base ref, then per racer a worktree,
/// its setup and a tab with the agent in it.
fn new(args: &[String]) -> std::io::Result<()> {
    let opts = parse_new(args).map_err(crate::usage)?;
    refuse_a_worker()?;
    let read = qoop_tui::config::read();
    for agent in &opts.agents {
        crate::schedule::agent_ok(agent, &read.agents).map_err(std::io::Error::other)?;
    }
    let message = first_message(&crate::task::text(&opts.prompt)?);
    worktree::awake()?;
    let list = worktree::worktrees()?;
    let main = list[0].path.clone();
    // The caller's checkout, not the main one: a race from a feature branch starts there.
    let here = std::env::current_dir()?;
    let wanted = opts.base.as_deref().unwrap_or("HEAD");
    let sha = worktree::git_in(
        &here,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{wanted}^{{commit}}"),
        ],
    )
    .map_err(|_| std::io::Error::other(format!("{wanted} is not a commit here")))?
    .trim()
    .to_string();
    let racers: Vec<Racer> = opts
        .agents
        .iter()
        .zip(labels(&opts.agents))
        .map(|(agent, label)| {
            let branch = branch_of(&opts.name, &label);
            let path = worktree::default_worktree_path(&main, &branch);
            Racer {
                agent: agent.clone(),
                label,
                branch,
                path,
            }
        })
        .collect();
    preflight(&main, &list, &opts.name, &racers)?;
    let place = worktree::place_for(&main, None, &read)?;
    if worktree::dirty(&here).unwrap_or(false) {
        eprintln!(
            "race: uncommitted changes in {} are not in the racers",
            here.display()
        );
    }
    let race_ref = ref_of(&opts.name);
    worktree::git_in(&main, &["update-ref", &race_ref, &sha, ""])?;
    println!("race={} base={sha} ({wanted})", opts.name);
    let mut started = Vec::new();
    for racer in &racers {
        let terminal = start(&main, racer, &sha, &message, &place, &read).map_err(|e| {
            std::io::Error::other(format!(
                "{}: {e}; `qoop race rm {} --force` removes what was made",
                racer.branch, opts.name
            ))
        })?;
        started.push((racer.label.clone(), terminal));
    }
    if opts.wait {
        return wait(&opts.name, &started, opts.timeout);
    }
    Ok(())
}

/// Everything that would stop a racer midway, checked before anything is written: branch
/// names git takes, no branch `race` or `race/<name>` (either would block the folder git
/// keeps the racers' branches in), no racer branch or race ref already, no remote branch of a
/// racer's name (the worktree would check it out and refuse the base), and each worktree
/// folder new: not on disk, not one git still lists, not shared by two racers.
fn preflight(main: &Path, list: &[Worktree], name: &str, racers: &[Racer]) -> std::io::Result<()> {
    let refuse = |why: String| Err(std::io::Error::other(why));
    for racer in racers {
        if worktree::git_in(main, &["check-ref-format", "--branch", &racer.branch]).is_err() {
            return refuse(format!("{} is not a branch name git takes", racer.branch));
        }
    }
    let race_ref = ref_of(name);
    let refs = worktree::git_in(
        main,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/race",
            &race_ref,
            "refs/remotes",
        ],
    )?;
    let race_dir = format!("refs/heads/race/{name}");
    for found in refs.lines() {
        if found == "refs/heads/race" || found == race_dir {
            return refuse(format!(
                "the branch {} blocks the racers' branches",
                &found["refs/heads/".len()..]
            ));
        }
        if found.starts_with(&format!("{race_dir}/")) || found == race_ref {
            return refuse(format!(
                "race {name} exists: `qoop race ls {name}` shows it"
            ));
        }
        // `refs/remotes/<remote>/<branch>`, as `worktree new` reads a remote branch.
        if let Some(remote) = found.strip_prefix("refs/remotes/")
            && let Some((_, branch)) = remote.split_once('/')
            && racers.iter().any(|r| r.branch == branch)
        {
            return refuse(format!("{remote} exists: a remote has the branch {branch}"));
        }
    }
    let listed: Vec<PathBuf> = list.iter().map(|w| worktree::real(&w.path)).collect();
    for (i, racer) in racers.iter().enumerate() {
        let shown = racer.path.display();
        if racer.path.exists() {
            return refuse(format!("{shown} exists"));
        }
        if listed.contains(&worktree::real(&racer.path)) {
            return refuse(format!(
                "git still lists a worktree at {shown}; `git worktree prune` forgets it"
            ));
        }
        if racers[..i].iter().any(|r| r.path == racer.path) {
            return refuse(format!("two racers would share {shown}"));
        }
    }
    Ok(())
}

/// One racer: its worktree from the base, set up as its space says, and a tab with the agent
/// started on the message. Its line once the tab has it; its terminal.
fn start(
    main: &Path,
    racer: &Racer,
    sha: &str,
    message: &str,
    place: &worktree::Place,
    read: &qoop_tui::config::Read,
) -> std::io::Result<u32> {
    let path = worktree::add(main, &racer.branch, &racer.path, false, Some(sha))?;
    let setup = worktree::prepare(main, &path, &racer.branch, place, read, false)
        .map_err(|why| std::io::Error::other(format!("setup failed ({why})")))?;
    let (typed, command) = crate::schedule::launch(&racer.agent, message, &read.agents)
        .map_err(std::io::Error::other)?;
    let new = NewOpts {
        command,
        typed,
        ..NewOpts::default()
    };
    let mut terminal = None;
    worktree::open_in(new, place, path.clone(), &racer.branch, |t| {
        terminal = Some(t)
    })?;
    let terminal = terminal.ok_or_else(|| std::io::Error::other("no terminal was opened"))?;
    println!(
        "agent={} branch={} worktree={} setup={setup} terminal={terminal} status=starting",
        racer.label,
        racer.branch,
        path.display()
    );
    Ok(terminal)
}

/// How a racer's wait ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum End {
    Finished,
    Failed,
    Blocked { trust: bool },
    Sleeping,
    Closed,
    Exited,
    NotStarted,
}

/// What `--wait` remembers of one racer between listings.
#[derive(Default)]
struct Seen {
    agent: bool,
    ready_since: Option<Instant>,
}

/// Whether a racer's row, in this listing, ends its wait. Level-triggered: only whether it
/// was ever an agent, and since when it sat ready without being one, carry over.
fn judge(row: Option<&AgentRow>, seen: &mut Seen, now: Instant) -> Option<End> {
    let Some(row) = row else {
        return Some(End::Closed);
    };
    if row.state == "sleeping" {
        return Some(End::Sleeping);
    }
    if row.agent {
        seen.agent = true;
    } else if seen.agent {
        // The agent quit: its shell is a new generation, which has no stop.
        return Some(End::Exited);
    }
    let status = row.status.as_deref().unwrap_or(&row.state);
    if status == "failed" {
        return Some(End::Failed);
    }
    if row.readiness == "trust_required" || status == "blocked" {
        return Some(End::Blocked {
            trust: row.readiness == "trust_required",
        });
    }
    if row.stop.is_some() && row.readiness != "spawning" {
        return Some(End::Finished);
    }
    if !seen.agent && row.readiness == "ready" {
        let since = *seen.ready_since.get_or_insert(now);
        if now.duration_since(since) >= START_PATIENCE {
            return Some(End::NotStarted);
        }
    }
    None
}

/// One line of the wait's answer.
fn end_line(label: &str, terminal: u32, end: End) -> String {
    let word = match end {
        End::Finished => "finished",
        End::Failed => "failed",
        End::Blocked { trust: false } => "blocked",
        End::Blocked { trust: true } => "blocked why=trust_required",
        End::Sleeping => "sleeping",
        End::Closed => "closed",
        End::Exited => "exited",
        End::NotStarted => "not-started",
    };
    let mut line = format!("agent={label} terminal={terminal} end={word}");
    if matches!(end, End::Blocked { .. }) {
        line.push_str(&format!(
            "; `qoop wait {terminal} --until done,idle,failed` keeps waiting on it"
        ));
    }
    line
}

/// `--wait`: a listing a second until, in one, every racer has ended (see [`judge`]); then a
/// line each. Exit 0 only when every racer finished, 124 past `timeout`.
fn wait(name: &str, racers: &[(String, u32)], timeout: Option<Duration>) -> std::io::Result<()> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut seen: Vec<Seen> = racers.iter().map(|_| Seen::default()).collect();
    loop {
        // A daemon gone answers no rows, which would read as every racer closed.
        worktree::awake().map_err(|_| {
            std::io::Error::other(format!(
                "qoop stopped while waiting; `qoop race ls {name}` shows each racer"
            ))
        })?;
        let rows = worktree::session_rows()?;
        let now = Instant::now();
        let ends: Vec<Option<End>> = racers
            .iter()
            .zip(&mut seen)
            .map(|((_, terminal), seen)| {
                judge(rows.iter().find(|r| r.terminal == *terminal), seen, now)
            })
            .collect();
        if ends.iter().all(Option::is_some) {
            for ((label, terminal), end) in racers.iter().zip(&ends) {
                println!("{}", end_line(label, *terminal, end.expect("all ended")));
            }
            let unfinished = ends.iter().filter(|e| **e != Some(End::Finished)).count();
            if unfinished > 0 {
                return Err(std::io::Error::other(format!(
                    "{unfinished} of {} racers did not finish",
                    racers.len()
                )));
            }
            return Ok(());
        }
        if deadline.is_some_and(|d| now >= d) {
            eprintln!("qoop: racers still going; `qoop race ls {name}` shows each");
            std::process::exit(crate::supervise::TIMED_OUT);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A racer found from its branch: its name, its branch and its worktree when it has one.
struct Found<'a> {
    label: String,
    branch: String,
    tree: Option<&'a Worktree>,
}

/// Every racer of race `name` still on a branch, in name order.
fn racers_of<'a>(main: &Path, list: &'a [Worktree], name: &str) -> std::io::Result<Vec<Found<'a>>> {
    let prefix = format!("refs/heads/race/{name}/");
    let refs = worktree::git_in(main, &["for-each-ref", "--format=%(refname)", &prefix])?;
    Ok(refs
        .lines()
        .filter_map(|r| r.strip_prefix(&prefix))
        .map(|label| {
            let branch = branch_of(name, label);
            let tree = list
                .iter()
                .find(|w| w.branch.as_deref() == Some(branch.as_str()));
            Found {
                label: label.to_string(),
                branch,
                tree,
            }
        })
        .collect())
}

/// A racer's tab: the row named after its branch, else one inside its worktree, an agent
/// before a shell.
fn tab_of<'a>(rows: &'a [AgentRow], branch: &str, tree: Option<&Path>) -> Option<&'a AgentRow> {
    rows.iter()
        .find(|r| r.name.as_deref() == Some(branch))
        .or_else(|| {
            let root = worktree::real(tree?);
            rows.iter()
                .filter(|r| {
                    r.cwd
                        .as_deref()
                        .is_some_and(|c| worktree::real(Path::new(c)).starts_with(&root))
                })
                .max_by_key(|r| r.agent)
        })
}

/// A count, with `+` when it may be low.
fn count(n: impl std::fmt::Display, cut: bool) -> String {
    format!("{n}{}", if cut { "+" } else { "" })
}

/// `qoop race ls <name>`: per racer its tab, the daemon's status, and the files and lines it
/// changed since the base (`-` when that cannot be said).
fn ls(args: &[String]) -> std::io::Result<()> {
    let [name] = args else {
        return Err(crate::usage(USAGE));
    };
    let list = worktree::worktrees()?;
    let main = &list[0].path;
    let found = racers_of(main, &list, name)?;
    if found.is_empty() {
        return Err(std::io::Error::other(format!("no race {name}")));
    }
    let race_ref = ref_of(name);
    let has_ref = worktree::git_in(main, &["rev-parse", "--verify", "--quiet", &race_ref]).is_ok();
    let rows = worktree::session_rows()?;
    for racer in &found {
        let path = racer.tree.map(|w| w.path.as_path());
        let tab = tab_of(&rows, &racer.branch, path);
        let lines = path
            .filter(|_| has_ref)
            .and_then(|p| qoop_runtime::changes::stat(&p.display().to_string(), &race_ref).ok())
            .map_or_else(
                || "files=- added=- removed=-".to_string(),
                |s| {
                    format!(
                        "files={} added={} removed={}",
                        count(s.files, s.cut),
                        count(s.added, s.cut),
                        count(s.removed, s.cut)
                    )
                },
            );
        println!(
            "agent={} terminal={} status={} {lines} tests=- worktree={}",
            racer.label,
            tab.map_or("-".to_string(), |r| r.terminal.to_string()),
            tab.map_or("closed", |r| r.status.as_deref().unwrap_or(&r.state)),
            path.map_or("-".to_string(), |p| p.display().to_string()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(String::from).collect()
    }

    #[test]
    fn a_race_is_named_and_given_two_to_five_agents_and_a_prompt() {
        let ok = parse_new(&args(
            "fix --agents claude,codex --wait --timeout 60 -- do it",
        ))
        .unwrap();
        assert_eq!(
            ok,
            NewArgs {
                name: "fix".into(),
                agents: vec!["claude".into(), "codex".into()],
                base: None,
                wait: true,
                timeout: Some(Duration::from_secs(60)),
                prompt: vec!["do".into(), "it".into()],
            }
        );
        for bad in [
            "Fix --agents claude,codex -- x",
            "-fix --agents claude,codex -- x",
            "a_b --agents claude,codex -- x",
            "fix --agents claude -- x",
            "fix --agents a,b,c,d,e,f -- x",
            "fix --agents claude,,codex -- x",
            "fix --agents claude,codex --base -x -- x",
            "fix --agents claude,codex --timeout 5 -- x",
            "fix --agents claude,codex --",
        ] {
            assert!(parse_new(&args(bad)).is_err(), "{bad}");
        }
        assert!(name_ok("9-lives") && !name_ok(&"a".repeat(33)) && !name_ok(""));
    }

    #[test]
    fn a_second_of_one_agent_is_numbered_and_the_rule_follows_the_prompt() {
        let agents = ["claude", "codex", "claude", "claude"].map(String::from);
        assert_eq!(labels(&agents), ["claude", "codex", "claude-2", "claude-3"]);
        let said = first_message("add a test\n");
        assert!(said.starts_with("add a test\n\nRace rules: "), "{said}");
        assert!(said.ends_with("what is left."), "{said}");
    }

    fn row(terminal: u32, agent: bool, state: &str, readiness: &str, stop: bool) -> AgentRow {
        serde_json::from_value(serde_json::json!({
            "terminal": terminal,
            "workspace": 1,
            "state": state,
            "readiness": readiness,
            "agent": agent,
            "status": state,
            "stop": stop.then_some("1.1"),
        }))
        .expect("a row")
    }

    #[test]
    fn a_wait_ends_per_racer_on_each_way_an_agent_stops() {
        let t0 = Instant::now();
        let judged = |r: Option<&AgentRow>| judge(r, &mut Seen::default(), t0);
        assert_eq!(
            judged(Some(&row(1, true, "done", "ready", true))),
            Some(End::Finished)
        );
        assert_eq!(judged(Some(&row(1, true, "working", "ready", false))), None);
        assert_eq!(
            judged(Some(&row(1, true, "blocked", "ready", false))),
            Some(End::Blocked { trust: false })
        );
        assert_eq!(
            judged(Some(&row(1, true, "idle", "trust_required", false))),
            Some(End::Blocked { trust: true })
        );
        assert_eq!(
            judged(Some(&row(1, true, "failed", "ready", false))),
            Some(End::Failed)
        );
        assert_eq!(
            judged(Some(&row(1, false, "sleeping", "ready", false))),
            Some(End::Sleeping)
        );
        assert_eq!(judged(None), Some(End::Closed));
        // A stop said while still spawning is not one yet.
        assert_eq!(judged(Some(&row(1, true, "done", "spawning", true))), None);
        // Seen as an agent, then back at its shell.
        let mut seen = Seen::default();
        assert_eq!(
            judge(
                Some(&row(1, true, "working", "ready", false)),
                &mut seen,
                t0
            ),
            None
        );
        assert_eq!(
            judge(Some(&row(1, false, "idle", "ready", false)), &mut seen, t0),
            Some(End::Exited)
        );
        // Never an agent: given its patience, then not started.
        let mut seen = Seen::default();
        let shell = row(1, false, "idle", "ready", false);
        assert_eq!(judge(Some(&shell), &mut seen, t0), None);
        assert_eq!(
            judge(Some(&shell), &mut seen, t0 + Duration::from_secs(29)),
            None
        );
        assert_eq!(
            judge(Some(&shell), &mut seen, t0 + START_PATIENCE),
            Some(End::NotStarted)
        );
    }

    #[test]
    fn a_blocked_racer_is_pointed_at_qoop_wait() {
        assert_eq!(
            end_line("claude", 3, End::Finished),
            "agent=claude terminal=3 end=finished"
        );
        let blocked = end_line("codex", 4, End::Blocked { trust: true });
        assert!(
            blocked.starts_with("agent=codex terminal=4 end=blocked why=trust_required; ")
                && blocked.contains("qoop wait 4 --until done,idle,failed"),
            "{blocked}"
        );
    }
}

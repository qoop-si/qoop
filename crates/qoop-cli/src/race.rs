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
     \x20 qoop race ls <name> [--test CMD]\n\
     \x20 qoop race rm <name> [--keep AGENT] [--force]";

/// What every racer is told after the prompt.
const RULE: &str = "Race rules: change files in this worktree only. Do not commit, push, open a \
                    PR, merge, close your tab or start agents or races. When you are done, end \
                    with a short self-report: what you changed, what you ran, what is left.";

/// The fewest and the most agents in one race.
const AGENTS: std::ops::RangeInclusive<usize> = 2..=5;

/// How long a racer's tab may sit ready at its shell before it counts as never started.
const START_PATIENCE: Duration = Duration::from_secs(30);

/// How long a racer must stay blocked before that ends its wait: an agent's own start can read
/// as a folder trust prompt for a moment and go on by itself, and a real block waits for a
/// person anyway.
const BLOCK_PATIENCE: Duration = Duration::from_secs(5);

/// How long `ls --test` lets one racer's command run.
const TEST_WITHIN: Duration = Duration::from_secs(600);

/// `qoop race new|ls|rm`.
pub(crate) fn run(args: &[String]) -> std::io::Result<()> {
    let rest = args.get(3..).unwrap_or_default();
    match args.get(2).map(String::as_str) {
        Some("new") => new(rest),
        Some("ls") => ls(rest),
        Some("rm") => rm(rest),
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

/// The owner's gate: a race is started and dropped by the person (their own shell tab, or
/// outside qoop) or by qass, never from a worker agent's terminal. Read from the caller's row, not the
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
            "a race is started and dropped by the person or qass, not from an agent's terminal",
        ));
    }
    Ok(())
}

/// One racer about to start.
struct Racer {
    label: String,
    branch: String,
    path: PathBuf,
    /// What its tab is typed: see [`crate::schedule::launch`].
    typed: Option<String>,
    command: Vec<String>,
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
    // Each launch line is made now, so an agent that cannot take the message stops nothing midway.
    let racers = opts
        .agents
        .iter()
        .zip(labels(&opts.agents))
        .map(|(agent, label)| {
            let branch = branch_of(&opts.name, &label);
            let path = worktree::default_worktree_path(&main, &branch);
            let (typed, command) = crate::schedule::launch(agent, &message, &read.agents)
                .map_err(std::io::Error::other)?;
            Ok(Racer {
                label,
                branch,
                path,
                typed,
                command,
            })
        })
        .collect::<std::io::Result<Vec<Racer>>>()?;
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
        let mut opened = None;
        if let Err(e) = start(&main, racer, &sha, &place, &read, &mut opened) {
            let tabs: Vec<u32> = started.iter().map(|(_, t)| *t).chain(opened).collect();
            return Err(std::io::Error::other(format!(
                "{} ({}): {e}; {}",
                racer.branch,
                racer.path.display(),
                undo_hint(&opts.name, &tabs)
            )));
        }
        started.push((
            racer.label.clone(),
            opened.expect("a started racer has a tab"),
        ));
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
    let labels: Vec<String> = racers.iter().map(|r| r.label.clone()).collect();
    if let Some(twice) = twice(&labels) {
        return refuse(format!("two racers would both be {twice}"));
    }
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
        if racer.path.symlink_metadata().is_ok() {
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
/// started on the message; its line once the tab has it. `opened` is its terminal as soon as one
/// exists, a failure after that included.
fn start(
    main: &Path,
    racer: &Racer,
    sha: &str,
    place: &worktree::Place,
    read: &qoop_tui::config::Read,
    opened: &mut Option<u32>,
) -> std::io::Result<()> {
    let path = worktree::add(main, &racer.branch, &racer.path, false, Some(sha))?;
    let setup = worktree::prepare(main, &path, &racer.branch, place, read, false)
        .map_err(|why| std::io::Error::other(format!("setup failed ({why})")))?;
    let new = NewOpts {
        command: racer.command.clone(),
        typed: racer.typed.clone(),
        ..NewOpts::default()
    };
    worktree::open_in(new, place, path.clone(), &racer.branch, |t| {
        *opened = Some(t)
    })?;
    let terminal = opened.ok_or_else(|| std::io::Error::other("no terminal was opened"))?;
    println!(
        "agent={} branch={} worktree={} setup={setup} terminal={terminal} status=starting",
        racer.label,
        racer.branch,
        path.display()
    );
    Ok(())
}

/// The first name two racers share, as `claude,claude-2,claude` would.
fn twice(labels: &[String]) -> Option<&str> {
    labels
        .iter()
        .enumerate()
        .find(|(i, l)| labels[..*i].contains(l))
        .map(|(_, l)| l.as_str())
}

/// What undoes a race that stopped midway: its tabs first, since `race rm` never drops a
/// working racer, then the rest.
fn undo_hint(name: &str, tabs: &[u32]) -> String {
    let close: Vec<String> = tabs.iter().map(|t| format!("`qoop close {t}`")).collect();
    match close.as_slice() {
        [] => format!("`qoop race rm {name} --force` removes what was made"),
        _ => format!(
            "end the tabs it started with {}, then `qoop race rm {name} --force` removes what was \
             made",
            close.join(", ")
        ),
    }
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
    /// Its turn end was seen: a tab that sleeps or quits to its shell after it finished.
    stopped: bool,
    ready_since: Option<Instant>,
    blocked_since: Option<Instant>,
}

/// Whether a racer's row, in this listing, ends its wait. Level-triggered: only whether it
/// was ever an agent, whether its turn end was seen, since when it sat ready without being one,
/// and since when it is blocked, carry over.
fn judge(row: Option<&AgentRow>, seen: &mut Seen, now: Instant) -> Option<End> {
    // Neither a closed or slept tab nor a new generation keeps a stop: one seen before counts.
    let stopped = seen.stopped;
    let finished_or = |end| Some(if stopped { End::Finished } else { end });
    let Some(row) = row else {
        return finished_or(End::Closed);
    };
    if row.state == "sleeping" {
        return finished_or(End::Sleeping);
    }
    if row.agent {
        seen.agent = true;
    } else if seen.agent {
        // The agent quit: its shell is a new generation, which has no stop.
        return finished_or(End::Exited);
    }
    let status = row.status.as_deref().unwrap_or(&row.state);
    // Back at work since its turn end: that one no longer counts.
    if status == "working" && row.stop.is_none() {
        seen.stopped = false;
    }
    if status == "failed" {
        return Some(End::Failed);
    }
    if row.readiness == "trust_required" || status == "blocked" {
        let since = *seen.blocked_since.get_or_insert(now);
        if now.duration_since(since) < BLOCK_PATIENCE {
            return None;
        }
        return Some(End::Blocked {
            trust: row.readiness == "trust_required",
        });
    }
    seen.blocked_since = None;
    if row.stop.is_some() && row.readiness != "spawning" {
        seen.stopped = true;
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

/// A racer found from its branch: its name, its branch and its linked worktree as git recorded
/// it, gone from disk or not, when it has one.
struct Found<'a> {
    label: String,
    branch: String,
    tree: Option<&'a Worktree>,
}

impl Found<'_> {
    /// Its worktree as recorded, for finding its tab even when the folder is gone.
    fn recorded(&self) -> Option<&Path> {
        self.tree.map(|w| w.path.as_path())
    }

    /// Its worktree when the folder is still there.
    fn on_disk(&self) -> Option<&Path> {
        self.recorded().filter(|p| p.exists())
    }
}

/// Every racer of race `name` still on a branch, in name order. Only a linked worktree is a
/// racer's: a racer branch checked out in the main checkout has none.
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
                .skip(1)
                .find(|w| w.branch.as_deref() == Some(branch.as_str()));
            Found {
                label: label.to_string(),
                branch,
                tree,
            }
        })
        .collect())
}

/// Whether the race ref is there.
fn has_ref(main: &Path, race_ref: &str) -> bool {
    worktree::git_in(main, &["rev-parse", "--verify", "--quiet", race_ref]).is_ok()
}

/// A path resolved as far as it exists: a folder that is gone keeps its resolved parent, so
/// `/var/x` and `/private/var/x` are one place even after `x` was deleted.
fn resolved(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(part)) => {
                rest.push(part);
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether a row's folder is inside `root`, both [`resolved`].
fn inside(row: &AgentRow, root: &Path) -> bool {
    row.cwd
        .as_deref()
        .is_some_and(|c| resolved(Path::new(c)).starts_with(resolved(root)))
}

/// A racer's tab: the row named after its branch whose folder is inside its worktree (a race of
/// the same name in another repository names its tabs alike), else an agent inside it. A shell
/// that only stands there is never taken for the racer. With no worktree record left (pruned),
/// its folder is where `race new` put it.
fn tab_of<'a>(
    rows: &'a [AgentRow],
    main: &Path,
    branch: &str,
    tree: Option<&Path>,
) -> Option<&'a AgentRow> {
    let root = tree.map_or_else(
        || worktree::default_worktree_path(main, branch),
        Path::to_path_buf,
    );
    rows.iter()
        .find(|r| r.name.as_deref() == Some(branch) && inside(r, &root))
        .or_else(|| rows.iter().find(|r| r.agent && inside(r, &root)))
}

/// A count, with `+` when it may be low.
fn count(n: impl std::fmt::Display, cut: bool) -> String {
    format!("{n}{}", if cut { "+" } else { "" })
}

/// `qoop race ls <name> [--test CMD]`: per racer its tab, the daemon's status, the files and
/// lines it changed since the base (`-` when that cannot be said) and, with `--test`, whether
/// `CMD` passed in its worktree. Exits 0 whatever the tests say.
fn ls(args: &[String]) -> std::io::Result<()> {
    let (name, test) = match args {
        [name] => (name, None),
        [name, flag, cmd] if flag == "--test" => (name, Some(cmd.as_str())),
        _ => return Err(crate::usage(USAGE)),
    };
    if !name_ok(name) {
        return Err(crate::usage(USAGE));
    }
    let list = worktree::worktrees()?;
    let main = &list[0].path;
    let found = racers_of(main, &list, name)?;
    if found.is_empty() {
        return Err(std::io::Error::other(format!("no race {name}")));
    }
    let race_ref = ref_of(name);
    let has_ref = has_ref(main, &race_ref);
    let rows = worktree::session_rows()?;
    for racer in &found {
        let path = racer.on_disk();
        let tab = tab_of(&rows, main, &racer.branch, racer.recorded());
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
        let tests = match (test, path) {
            (Some(cmd), Some(p)) => match crate::watch::run_sh(cmd, p, TEST_WITHIN) {
                Ok(exit) if exit.timed_out => "timeout",
                Ok(exit) if exit.code == Some(0) => "pass",
                Ok(_) => "fail",
                Err(e) => {
                    eprintln!("race ls: {}: the test could not start: {e}", racer.label);
                    "-"
                }
            },
            _ => "-",
        };
        println!(
            "agent={} terminal={} status={} {lines} tests={tests} worktree={}",
            racer.label,
            tab.map_or("-".to_string(), |r| r.terminal.to_string()),
            status_of(tab),
            path.map_or("-".to_string(), |p| p.display().to_string()),
        );
    }
    Ok(())
}

/// What the daemon says of a racer's tab; `closed` when it has none.
fn status_of(tab: Option<&AgentRow>) -> &str {
    tab.map_or("closed", |r| r.status.as_deref().unwrap_or(&r.state))
}

/// `qoop race rm <name> [--keep AGENT] [--force]`: drop every racer but `--keep`, in two
/// passes. The first touches nothing and names every reason not to, for every racer; the
/// second looks at each racer again right before it closes its tab, removes its worktree and
/// deletes its branch, and stops at the first refusal or error, so a run again finishes it.
/// The race ref goes once no racer branch is left.
fn rm(args: &[String]) -> std::io::Result<()> {
    let Some((name, flags)) = args.split_first().filter(|(n, _)| name_ok(n)) else {
        return Err(crate::usage(USAGE));
    };
    let (mut keep, mut force) = (None, false);
    let mut it = flags.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--keep" => {
                keep =
                    Some(it.next().cloned().ok_or_else(|| {
                        crate::usage(format!("missing value for --keep; {USAGE}"))
                    })?)
            }
            "--force" => force = true,
            other => return Err(crate::usage(format!("unexpected {other}; {USAGE}"))),
        }
    }
    refuse_a_worker()?;
    let list = worktree::worktrees()?;
    let main = list[0].path.clone();
    let found = racers_of(&main, &list, name)?;
    // A typo with `--force` would otherwise drop the winner, whose work is only in its tree.
    if let Some(keep) = &keep
        && !found.iter().any(|r| &r.label == keep)
    {
        return Err(crate::usage(format!("{keep} is no racer of race {name}")));
    }
    let race_ref = ref_of(name);
    let has_ref = has_ref(&main, &race_ref);
    if found.is_empty() {
        if !has_ref {
            return Err(std::io::Error::other(format!("no race {name}")));
        }
        worktree::git_in(&main, &["update-ref", "-d", &race_ref])?;
        println!("removed ref={race_ref}");
        return Ok(());
    }
    let losers: Vec<&Found> = found
        .iter()
        .filter(|r| keep.as_ref() != Some(&r.label))
        .collect();
    let check = |racer: &Found| -> std::io::Result<Option<(Option<u32>, Option<u64>)>> {
        let rows = worktree::session_rows()?;
        let tab = tab_of(&rows, &main, &racer.branch, racer.recorded());
        let why = refusals(&main, racer, tab, &rows, &race_ref, has_ref, force);
        for reason in &why {
            eprintln!("race rm: {}: {reason}", racer.label);
        }
        Ok(why
            .is_empty()
            .then(|| (tab.map(|t| t.terminal), tab.and_then(|t| t.generation))))
    };
    let mut refused = false;
    for racer in &losers {
        refused |= check(racer)?.is_none();
    }
    if refused {
        return Err(std::io::Error::other(format!(
            "nothing was removed from race {name}"
        )));
    }
    // A racer folder deleted by hand: forgotten first, so neither the removal nor the branch
    // delete trips on it. Only then: a prune forgets every missing worktree of the repository.
    if losers
        .iter()
        .any(|r| r.recorded().is_some() && r.on_disk().is_none())
    {
        worktree::git_in(&main, &["worktree", "prune"])?;
    }
    for racer in &losers {
        // Looked at again: a racer that went back to work since the first pass is not dropped.
        let Some((terminal, generation)) = check(racer)? else {
            return Err(std::io::Error::other(format!(
                "stopped at {}; the `removed` lines above, if any, were done",
                racer.label
            )));
        };
        if let Some(terminal) = terminal {
            crate::supervise::close_terminal(terminal, generation)?;
        }
        let tree = racer.tree.filter(|w| w.path.exists());
        if let Some(tree) = tree {
            worktree::remove(&main, tree, force)?;
        }
        worktree::git_in(&main, &["branch", "-D", &racer.branch])?;
        println!(
            "removed agent={} worktree={} branch={}",
            racer.label,
            tree.map_or("-".to_string(), |w| w.path.display().to_string()),
            racer.branch
        );
    }
    if racers_of(&main, &list, name)?.is_empty() {
        worktree::git_in(&main, &["update-ref", "-d", &race_ref])?;
        println!("removed ref={race_ref}");
    }
    Ok(())
}

/// Why a losing racer cannot be dropped now: its tab is still at work or its state is unknown
/// (never overridden, as `worktree rm` never overrides a live process), its tab runs something
/// that is neither its agent nor a shell, or is the caller's own, the caller or another tab
/// stands in its worktree, or, without `force`, it has commits past the base (unknown with no
/// race ref) or uncommitted changes.
fn refusals(
    main: &Path,
    racer: &Found,
    tab: Option<&AgentRow>,
    rows: &[AgentRow],
    race_ref: &str,
    has_ref: bool,
    force: bool,
) -> Vec<String> {
    let mut why = Vec::new();
    if let Some(tab) = tab.filter(|t| t.state != "sleeping") {
        let t = tab.terminal;
        let status = status_of(Some(tab));
        // A shell reads `unknown` too; what it runs is judged below.
        if matches!(status, "working" | "starting" | "background")
            || (tab.agent && status == "unknown")
        {
            why.push(format!(
                "terminal {t} is {status}: wait for it (`qoop wait {t}`) or close it first; \
                 --force does not override a running agent"
            ));
        }
        let program = tab.program.as_deref();
        if !tab.agent && !program.is_some_and(qoop_runtime::idle::is_shell_program) {
            why.push(format!(
                "terminal {t} runs {}, not its agent or a shell: close it first",
                program.unwrap_or("something qoop cannot name")
            ));
        }
    }
    if let Some(tab) = tab
        && crate::supervise::own_terminal() == Some(tab.terminal)
    {
        why.push(format!("terminal {} is the one this runs in", tab.terminal));
    }
    if !force {
        let ahead = has_ref
            .then(|| {
                worktree::git_in(
                    main,
                    &[
                        "rev-list",
                        "--count",
                        &format!("{race_ref}..refs/heads/{}", racer.branch),
                    ],
                )
                .ok()
            })
            .flatten();
        match ahead.as_deref().map(str::trim) {
            Some("0") => {}
            Some(n) => why.push(format!("commits past the base ({n}); --force drops them")),
            None => why.push(
                "no race ref, so its commits past the base cannot be counted; --force drops it"
                    .to_string(),
            ),
        }
    }
    let Some(root) = racer.on_disk().map(worktree::real) else {
        return why;
    };
    if !force && worktree::dirty(&root).unwrap_or(true) {
        why.push(format!(
            "uncommitted changes in {}; --force drops them",
            root.display()
        ));
    }
    if std::env::current_dir().is_ok_and(|here| worktree::real(&here).starts_with(&root)) {
        why.push(format!("this runs from inside {}", root.display()));
    }
    let others: Vec<String> = rows
        .iter()
        .filter(|r| r.state != "sleeping" && Some(r.terminal) != tab.map(|t| t.terminal))
        .filter(|r| inside(r, &root))
        .map(|r| r.terminal.to_string())
        .collect();
    if !others.is_empty() {
        why.push(format!(
            "qoop terminal {} is inside {}: close it first",
            others.join(", "),
            root.display()
        ));
    }
    why
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
        // Blocked ends it once it held: a trust prompt that goes on by itself does not.
        let held = |r: AgentRow| {
            let mut seen = Seen::default();
            let first = judge(Some(&r), &mut seen, t0);
            (first, judge(Some(&r), &mut seen, t0 + BLOCK_PATIENCE))
        };
        assert_eq!(
            held(row(1, true, "blocked", "ready", false)),
            (None, Some(End::Blocked { trust: false }))
        );
        assert_eq!(
            held(row(1, true, "idle", "trust_required", false)),
            (None, Some(End::Blocked { trust: true }))
        );
        let mut seen = Seen::default();
        let trust = row(1, true, "idle", "trust_required", false);
        assert_eq!(judge(Some(&trust), &mut seen, t0), None);
        let working = row(1, true, "working", "ready", false);
        assert_eq!(judge(Some(&working), &mut seen, t0 + BLOCK_PATIENCE), None);
        assert_eq!(
            judge(Some(&trust), &mut seen, t0 + BLOCK_PATIENCE),
            None,
            "held anew"
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

    #[test]
    fn a_racer_that_finished_still_reads_finished_once_it_sleeps_quits_or_closes() {
        let t0 = Instant::now();
        let done = row(1, true, "done", "ready", true);
        let sleeping = row(1, false, "sleeping", "ready", false);
        for after in [
            Some(&sleeping),
            Some(&row(1, false, "idle", "ready", false)),
            None,
        ] {
            let mut seen = Seen::default();
            assert_eq!(judge(Some(&done), &mut seen, t0), Some(End::Finished));
            assert_eq!(judge(after, &mut seen, t0), Some(End::Finished));
        }
        // Back at work after that turn end: it no longer counts.
        let mut seen = Seen::default();
        assert_eq!(judge(Some(&done), &mut seen, t0), Some(End::Finished));
        let working = row(1, true, "working", "ready", false);
        assert_eq!(judge(Some(&working), &mut seen, t0), None);
        assert_eq!(judge(Some(&sleeping), &mut seen, t0), Some(End::Sleeping));
    }

    #[test]
    fn a_folder_that_is_gone_resolves_through_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert_eq!(
            resolved(&dir.path().join("link/gone/deeper")),
            std::fs::canonicalize(&real).unwrap().join("gone/deeper")
        );
    }

    #[test]
    fn two_racers_of_one_name_are_refused_and_a_stop_midway_says_how_to_undo_it() {
        let agents = ["claude", "claude-2", "claude"].map(String::from);
        assert_eq!(twice(&labels(&agents)), Some("claude-2"));
        assert_eq!(
            twice(&labels(&["claude", "claude"].map(String::from))),
            None
        );
        assert_eq!(
            undo_hint("x", &[]),
            "`qoop race rm x --force` removes what was made"
        );
        assert_eq!(
            undo_hint("x", &[3, 4]),
            "end the tabs it started with `qoop close 3`, `qoop close 4`, then \
             `qoop race rm x --force` removes what was made"
        );
    }
}

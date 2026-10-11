//! `qoop qass`: open qass, the assistant over every agent, in one step.
//!
//! The one already running is shown; one asleep in the `qass` space is woken; otherwise a
//! new one starts in its own folder (`qoop paths qass-data`, where its skill, notes and
//! scratch live) on the agent `config.toml`
//! names (`[qass] agent`: claude, codex, gemini, opencode, or a person's own `[agents]`
//! line), told to start its qass skill. Closing it is the `x` on its row, or `qoop close`.

use std::path::{Path, PathBuf};
use std::process::Command;

use qoop_runtime::paths;
use qoop_tui::config;

use crate::supervise;

/// The space qass lives in.
pub(crate) const SPACE: &str = "qass";

/// Where `qoop prompt` and `qoop send` note who wrote to a terminal, in qass's folder: its
/// watcher says at once that an agent qass wrote to stopped, and lets the rest wait.
pub(crate) const PROMPTED_LOG: &str = "prompted.log";

/// Notes that the terminal this call runs in just wrote to `target`. Only into a qass folder
/// that already exists, and never failing the write it records: a person without qass gets
/// no file, and a prompt never fails for want of its note.
pub(crate) fn note_prompted(target: u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let from = qoop_runtime::legacy::var("QOOP_TERMINAL").ok();
    let _ = note_prompted_in(paths::qass_data_dir(), from.as_deref(), target, now);
}

fn note_prompted_in(
    folder: Option<PathBuf>,
    from: Option<&str>,
    target: u32,
    now: u64,
) -> std::io::Result<()> {
    use std::io::Write;
    let (Some(folder), Some(from)) = (folder, from.and_then(|v| v.parse::<u32>().ok())) else {
        return Ok(());
    };
    if !folder.is_dir() {
        return Ok(());
    }
    // One short write in append mode: notes from two callers never run into each other.
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(folder.join(PROMPTED_LOG))?
        .write_all(format!("{now} {from} T{target}\n").as_bytes())
}

/// What starts the qass skill as each agent's first message, as each was seen to take it:
/// Claude a slash command, Codex a `$skill` mention, Gemini and opencode a request to use
/// it (both pick a skill by name from the prompt; neither has a skill command).
fn first_message(program: &str) -> Option<[&'static str; 2]> {
    match program {
        "claude" => Some(["", "/qass"]),
        "codex" => Some(["", "$qass"]),
        "gemini" => Some(["-i", "Use the qass skill."]),
        "opencode" => Some(["--prompt", "Use the qass skill."]),
        _ => None,
    }
}

/// The program a launcher line runs: its first word that is not an assignment, by name.
fn program_of(line: &str) -> &str {
    line.split_whitespace()
        .find(|w| !w.contains('='))
        .map_or("", |w| w.rsplit('/').next().unwrap_or(w))
}

/// Another qoop command's JSON, from this same binary.
fn json(args: &[&str]) -> Option<serde_json::Value> {
    let me = std::env::current_exe().ok()?;
    let out = Command::new(me).args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Another qoop command run for its effect, failing with what it said.
pub(crate) fn run(args: &[&str]) -> std::io::Result<()> {
    let out = Command::new(std::env::current_exe()?).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

/// The agent row of a running qass: an agent whose folder is qass's own.
fn running_row(folder: &Path) -> Option<serde_json::Value> {
    let folder = folder.canonicalize().ok()?;
    json(&["agents", "--json"])?
        .as_array()?
        .iter()
        .filter(|row| row["agent"] == true && row["state"] != "sleeping")
        .find(|row| {
            row["cwd"]
                .as_str()
                .and_then(|cwd| Path::new(cwd).canonicalize().ok())
                .is_some_and(|cwd| cwd == folder)
        })
        .cloned()
}

/// The terminal of a running qass.
fn running(folder: &Path) -> Option<u64> {
    running_row(folder)?["terminal"].as_u64()
}

/// Whether a stored tab was started in `folder`, qass's own.
fn in_folder(cwd: &str, folder: &Path) -> bool {
    Path::new(cwd)
        .canonicalize()
        .ok()
        .zip(folder.canonicalize().ok())
        .is_some_and(|(cwd, folder)| cwd == folder)
}

/// The sleeping tab that is qass, among the stored tabs of the `qass` space: one that holds
/// an agent and was started in qass's own folder, the way a running one is found. The space
/// may hold other tabs (a worker parked there, this page's own tab), and none of them is it.
fn sleeping_qass(
    tabs: &[qoop_store::TerminalRow],
    asleep: &[(i64, i64, i64)],
    folder: &Path,
) -> Option<u64> {
    // Newest sleep first, as the store lists them.
    asleep.iter().find_map(|(terminal, _, _)| {
        tabs.iter()
            .find(|t| t.id == *terminal && t.agent.is_some() && in_folder(&t.cwd, folder))
            .and_then(|t| u64::try_from(t.id).ok())
    })
}

/// The tab asleep as qass, read from the store, which keeps every sleeping tab's folder.
fn asleep(folder: &Path) -> Option<u64> {
    let dir = paths::runtime_dir().ok()?;
    let store = crate::existing_store(&dir).ok()??;
    let space = store
        .workspaces()
        .ok()?
        .into_iter()
        .find(|w| w.name == SPACE)?;
    let tabs = store.open_terminals(space.id).ok()?;
    let asleep: Vec<_> = store
        .asleep()
        .ok()?
        .into_iter()
        .filter(|(_, workspace, _)| *workspace == space.id)
        .collect();
    sleeping_qass(&tabs, &asleep, folder)
}

/// The new tab's command: a person's `[agents]` line as written, or the agent by name, then
/// what starts the skill; with the program it runs.
fn launch_line(
    agent: &str,
    launchers: &[config::Launcher],
) -> Result<(Option<String>, Vec<String>, String), String> {
    let line = launchers
        .iter()
        .find(|a| a.name == agent)
        .map(|a| a.command.clone());
    let program = line.as_deref().map_or(agent, program_of).to_string();
    let Some(first) = first_message(&program) else {
        return Err(format!(
            "qass starts on {}; {agent} runs {program}",
            config::QASS_AGENTS.join(", ")
        ));
    };
    let words = line
        .is_none()
        .then(|| agent.to_string())
        .into_iter()
        .chain(
            first
                .iter()
                .filter(|w| !w.is_empty())
                .map(|w| w.to_string()),
        )
        .collect();
    Ok((line, words, program))
}

/// The same, refused when qass's skill is not where that program reads skills.
fn launch(
    agent: &str,
    read: &config::Read,
) -> std::io::Result<(Option<String>, Vec<String>, String)> {
    let (line, words, program) = launch_line(agent, &read.agents).map_err(std::io::Error::other)?;
    if !crate::skills::qass_installed_for(&program) {
        return Err(std::io::Error::other(
            "qass is not set up; run `qoop skills install` first",
        ));
    }
    Ok((line, words, program))
}

/// Where `qoop qass` writes which program it last started qass on, in qass's folder: an id is
/// only carried on by the program that made it.
const LAST_AGENT: &str = "last-agent";

/// The id of qass's conversation, as its agent's hook last reported it from qass's folder
/// ([`note_session`]): the daemon forgets a closed tab's id, this file keeps it. A fresh start
/// clears it, so it always belongs to the program `LAST_AGENT` names.
const LAST_SESSION: &str = "last-session";

/// A hook reporting from inside qass's folder speaks for qass: its conversation's id is kept
/// there for `qoop qass` to carry on. Anything else, or an id that is not a plain word, is
/// left alone.
pub(crate) fn note_session(session: &str) {
    let Some(folder) = paths::qass_data_dir().and_then(|f| f.canonicalize().ok()) else {
        return;
    };
    let here = std::env::current_dir().and_then(|d| d.canonicalize());
    let plain = session.len() <= 128
        && !session.is_empty()
        && !session.starts_with('-')
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain || here.ok() != Some(folder.clone()) {
        return;
    }
    let path = folder.join(LAST_SESSION);
    if std::fs::read_to_string(&path).is_ok_and(|kept| kept.trim() == session) {
        return;
    }
    let _ = std::fs::write(path, format!("{session}\n"));
}

/// The conversation qass carries on, on `program`. Claude: the newest conversation Claude
/// still keeps of qass's (`claude_newest`). Another agent: the id its hook last reported from
/// qass's folder (`session`), when qass last started on this program (`last`); else the newest
/// qass tab's, while that tab still says it ran this program. With none, qass starts afresh.
fn carry(
    program: &str,
    tabs: &[qoop_store::TerminalRow],
    last: Option<&str>,
    session: Option<&str>,
    claude_newest: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if program == "claude" {
        return claude_newest();
    }
    if last == Some(program)
        && let Some(id) = session
    {
        return Some(id.to_string());
    }
    let tab = tabs.first()?;
    (tab.agent.as_deref() == Some(program))
        .then(|| tab.agent_session.clone())
        .flatten()
}

/// Whether the agent still keeps that conversation on disk, where qoop can see it: resuming
/// one it never wrote down (a first turn that never finished) leaves a bare shell. Gemini
/// writes a chat file holding the id, and lists it only once it holds a reply of its own;
/// Codex writes a rollout file named with it; Claude's is checked
/// by [`claude_newest`], and opencode, which keeps its own in a database, is taken at its word.
fn still_kept(program: &str, id: &str, home: &Path) -> bool {
    let files = |dir: PathBuf, depth: usize| -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut todo = vec![(dir, 0)];
        while let Some((dir, level)) = todo.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(t) if t.is_dir() && level < depth => todo.push((path, level + 1)),
                    Ok(t) if t.is_file() => found.push(path),
                    _ => {}
                }
            }
        }
        found
    };
    let named = |path: &PathBuf| {
        path.file_name()
            .is_some_and(|n| n.to_string_lossy().contains(id))
    };
    match program {
        // Gemini names each chat file after the first eight characters of its id.
        "gemini" => files(home.join(".gemini/tmp"), 2)
            .iter()
            .filter(|p| p.parent().is_some_and(|d| d.ends_with("chats")))
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains(id.get(..8).unwrap_or(id)))
            })
            .any(|p| {
                std::fs::read_to_string(p).is_ok_and(|text| {
                    text.contains(id)
                        && (text.contains(r#""type":"gemini""#)
                            || text.contains(r#""type": "gemini""#))
                })
            }),
        "codex" => {
            let codex = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            files(codex.join("sessions"), 4).iter().any(named)
        }
        _ => true,
    }
}

/// The newest of qass's Claude conversations that holds a message: the ones qoop kept the
/// ids of, and those in the folder's own project directory (Claude names it after the
/// folder, every character but a letter or digit made `-`), newest first. A file with no
/// message (a title only) cannot be carried on.
fn claude_newest(ids: &[&str], folder: &Path) -> Option<String> {
    // Where Claude keeps its projects: `CLAUDE_CONFIG_DIR` first, as Claude reads it.
    let roots: Vec<PathBuf> = [
        std::env::var_os("CLAUDE_CONFIG_DIR").map(|d| Path::new(&d).join("projects")),
        std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude/projects")),
    ]
    .into_iter()
    .flatten()
    .collect();
    claude_newest_in(&roots, ids, folder)
}

fn claude_newest_in(roots: &[PathBuf], ids: &[&str], folder: &Path) -> Option<String> {
    use std::io::BufRead;
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let slug: String = folder
        .canonicalize()
        .ok()?
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let here = roots
        .iter()
        .filter_map(|root| std::fs::read_dir(root.join(&slug)).ok())
        .flat_map(|dir| dir.flatten())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"));
    let kept = ids
        .iter()
        .filter_map(|id| qoop_runtime::daemon::claude_transcript_in(roots, id));
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = kept
        .chain(here)
        .filter_map(|path| Some((modified(&path)?, path)))
        .collect();
    found.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let said = |path: &Path| {
        std::fs::File::open(path).is_ok_and(|f| {
            std::io::BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .any(|line| line.contains("\"type\":\"user\""))
        })
    };
    found
        .into_iter()
        .find(|(_, path)| said(path))
        .and_then(|(_, path)| Some(path.file_stem()?.to_str()?.to_string()))
}

/// qass's own tabs, open or closed, newest first: named `qass`, in its folder.
fn tabs_of(folder: &Path) -> Vec<qoop_store::TerminalRow> {
    let tabs = (|| {
        let dir = paths::runtime_dir().ok()?;
        let store = crate::existing_store(&dir).ok()??;
        store.named_terminals(SPACE).ok()
    })();
    tabs.unwrap_or_default()
        .into_iter()
        .filter(|t| in_folder(&t.cwd, folder))
        .collect()
}

/// What tells an agent carried on into its conversation to pick its work up again: Claude's
/// own hook says it at every resume, the others hear it as their first message.
fn resume_message(program: &str) -> Option<[&'static str; 2]> {
    const SAY: &str = "qass, carried on: start again from the skill's Start, then carry on.";
    match program {
        "codex" => Some(["", SAY]),
        "gemini" => Some(["-i", SAY]),
        "opencode" => Some(["--prompt", SAY]),
        _ => None,
    }
}

/// The line that carries the conversation on: a person's `[agents]` line as typed, flags and
/// all, with the resume words after it; else the manifest's whole resume line.
fn resume_command(
    typed: Option<String>,
    argv: Vec<String>,
    program: &str,
) -> (Option<String>, Vec<String>) {
    let said = resume_message(program)
        .into_iter()
        .flatten()
        .filter(|w| !w.is_empty())
        .map(String::from);
    match (typed, argv.split_first()) {
        (Some(line), Some((first, rest))) if first == program => {
            (Some(line), rest.iter().cloned().chain(said).collect())
        }
        _ => (None, argv.into_iter().chain(said).collect()),
    }
}

/// How qass stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Life {
    Running,
    Asleep,
    NotStarted,
}

/// What is known of qass right now, for its page: all of it read, none of it guessed.
#[derive(Clone, Debug)]
pub(crate) struct Facts {
    pub life: Life,
    /// Its tab, running or asleep.
    pub terminal: Option<u64>,
    /// What `[qass] agent` names, which a start runs on.
    pub agent: String,
    /// The agent's own state while it runs (`working`, `idle`, `blocked`, ...).
    pub state: Option<String>,
    /// What it is doing this moment, as its hook last said it, while it works.
    pub activity: Option<String>,
    /// When it last did something, unix seconds.
    pub active_at: Option<i64>,
    /// When it last said an event line (its watcher's `last-wake` file), unix seconds.
    pub event_at: Option<i64>,
    /// Why it cannot be started, when it cannot.
    pub blocker: Option<String>,
}

fn unix_of(time: std::time::SystemTime) -> Option<i64> {
    let secs = time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    i64::try_from(secs).ok()
}

/// Why qass cannot start on `agent`: not an agent it runs on, or its skill not installed.
fn blocker(agent: &str, read: &config::Read) -> Option<String> {
    match launch_line(agent, &read.agents) {
        Err(why) => Some(why),
        Ok((_, _, program)) if !crate::skills::qass_installed_for(&program) => {
            Some("qass is not set up; run `qoop skills install`".to_string())
        }
        Ok(_) => None,
    }
}

/// qass as the daemon and its folder say it is now.
pub(crate) fn look() -> Facts {
    let read = config::read();
    let folder = paths::qass_data_dir();
    let row = folder.as_deref().and_then(running_row);
    let (life, terminal) = match &row {
        Some(row) => (Life::Running, row["terminal"].as_u64()),
        None => match folder.as_deref().and_then(asleep) {
            Some(terminal) => (Life::Asleep, Some(terminal)),
            None => (Life::NotStarted, None),
        },
    };
    let text = |key: &str| row.as_ref().and_then(|r| r[key].as_str()).map(String::from);
    Facts {
        life,
        terminal,
        state: text("state"),
        activity: text("activity"),
        active_at: row.as_ref().and_then(|r| r["last_active_at_unix"].as_i64()),
        event_at: folder
            .and_then(|f| std::fs::metadata(f.join("last-wake")).ok())
            .and_then(|m| m.modified().ok())
            .and_then(unix_of),
        blocker: (life == Life::NotStarted)
            .then(|| blocker(&read.qass_agent, &read))
            .flatten(),
        agent: read.qass_agent,
    }
}

pub fn run_qass(args: &[String]) -> std::io::Result<()> {
    if args.get(2).map(String::as_str) == Some("page") && args.len() == 3 {
        return crate::qass_page::run();
    }
    if args.len() > 2 {
        return Err(crate::usage("usage: qoop qass [page]"));
    }
    let folder = paths::qass_data_dir()
        .ok_or_else(|| std::io::Error::other("no HOME to keep qass's folder under"))?;
    let terminal = if let Some(terminal) = running(&folder) {
        println!("qass is terminal {terminal}");
        terminal
    } else if let Some(terminal) = asleep(&folder) {
        run(&["wake", "--tab", &terminal.to_string()])?;
        println!("qass woke in terminal {terminal}");
        terminal
    } else {
        let read = config::read();
        let (typed, command, program) = launch(&read.qass_agent, &read)?;
        let tabs = tabs_of(&folder);
        let kept = |name| {
            std::fs::read_to_string(folder.join(name))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let (last, session) = (kept(LAST_AGENT), kept(LAST_SESSION));
        let ids: Vec<&str> = session
            .as_deref()
            .into_iter()
            .chain(tabs.iter().filter_map(|t| t.agent_session.as_deref()))
            .collect();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let resumed = carry(&program, &tabs, last.as_deref(), session.as_deref(), || {
            claude_newest(&ids, &folder)
        })
        .filter(|id| still_kept(&program, id, &home))
        .and_then(|id| qoop_runtime::daemon::resume_argv(&program, Some(&id)));
        let carried = resumed.is_some();
        let (typed, command) = match resumed {
            Some(argv) => resume_command(typed, argv, &program),
            None => (typed, command),
        };
        // No qass runs or sleeps: none can still be using the copies older installs left
        // in each agent's own skills.
        crate::skills::remove_old_qass();
        if program == "gemini" {
            crate::skills::trust_qass_for_gemini()?;
        }
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join(LAST_AGENT), format!("{program}\n"))?;
        if !carried {
            // A new conversation: the id kept is the last one's, and no longer qass's.
            let _ = std::fs::remove_file(folder.join(LAST_SESSION));
        }
        let opts = supervise::NewOpts {
            name: Some("qass".into()),
            cwd: Some(folder.clone()),
            space: Some(SPACE.into()),
            command,
            typed,
        };
        let mut opened = 0;
        supervise::open_terminal(&opts, |terminal| opened = terminal)?;
        let how = if carried {
            "carried on its conversation"
        } else {
            "started"
        };
        println!("qass {how} in terminal {opened} on {}", read.qass_agent);
        u64::from(opened)
    };
    // The board on top of it; qass is there without it, so a board that cannot open is said
    // and nothing more.
    if let Err(why) = show_board(terminal, &folder) {
        eprintln!("qoop: the board did not open above qass: {why}");
    }
    // Shown where the person is looking; with no interface open, the number says where.
    let _ = run(&["focus", &terminal.to_string()]);
    Ok(())
}

/// Refuses a line for qass while it waits on the owner (its state `blocked`): the enter after
/// it would answer the prompt qass is showing, a permission asked or a question, in the
/// owner's place.
pub(crate) fn hearing(state: Option<&str>) -> Result<(), String> {
    match state {
        Some("blocked") => Err("qass waits on you: answer it in its tab first".into()),
        _ => Ok(()),
    }
}

/// Type `text` into a terminal and press enter, as nobody: a send from inside a terminal
/// would say that terminal waits on the answer, and neither the board nor `qoop qass` does.
pub(crate) fn send(terminal: u64, text: &str) -> std::io::Result<()> {
    let out = Command::new(std::env::current_exe()?)
        .args(["send", &terminal.to_string(), "--enter", "--", text])
        .env_remove("QOOP_TERMINAL")
        .env_remove("QOOP_TOKEN")
        .env_remove("QORO_TERMINAL")
        .env_remove("QORO_TOKEN")
        .stdin(std::process::Stdio::null())
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(std::io::Error::other(
        said.trim().trim_start_matches("qoop: ").to_string(),
    ))
}

/// Put the board above qass in its tab (decision 12): `qoop task board --page` in a pane of
/// its own, the tab's first, stacked over qass through the split the interface keeps. A tab
/// that has a board pane is left as it is, even one at a shell: the wake that brought it back
/// typed its page (`restore_rows`), and typing it here too would put the second line into the
/// page as a new item.
fn show_board(qass: u64, folder: &Path) -> std::io::Result<()> {
    use qoop_runtime::control::{ControlRequest, ControlResponse, Pane, SplitDir};
    use qoop_runtime::idle::{BOARD_PANE, has_board};

    let qass = u32::try_from(qass).map_err(std::io::Error::other)?;
    let patience = std::time::Duration::from_secs(10);
    let ControlResponse::Agents { agents, splits, .. } = crate::task::ask(
        ControlRequest::ListAgents {
            id: crate::request_id("qass"),
        },
        patience,
    )?
    else {
        return Err(std::io::Error::other(
            "unexpected answer to the agent listing",
        ));
    };
    let tree = splits.into_iter().find(|t| t.leaves().contains(&qass));
    let panes = tree.as_ref().map(|t| t.leaves()).unwrap_or_default();
    if has_board(agents.iter().filter(|r| panes.contains(&r.terminal)), qass) {
        return Ok(());
    }
    let me = std::env::current_exe()?.display().to_string();
    let page = ["exec", &me, "task", "board", "--page"];
    let opts = supervise::NewOpts {
        name: Some(BOARD_PANE.into()),
        cwd: Some(folder.to_path_buf()),
        space: Some(SPACE.into()),
        command: page.iter().map(|w| w.to_string()).collect(),
        typed: None,
    };
    let mut board = 0;
    supervise::open_terminal(&opts, |t| board = t)?;
    // The board first, so the tab is still one tab and the board is on top.
    let tree = Pane::Split {
        dir: SplitDir::H,
        permille: 500,
        first: Box::new(Pane::Leaf { terminal: board }),
        second: Box::new(tree.unwrap_or(Pane::Leaf { terminal: qass })),
    };
    match crate::task::ask(
        ControlRequest::SetSplit {
            id: crate::request_id("qass"),
            tree,
        },
        patience,
    )? {
        ControlResponse::SplitSaved { .. } => Ok(()),
        ControlResponse::Failed { reason, .. } => Err(std::io::Error::other(reason)),
        other => Err(std::io::Error::other(format!(
            "unexpected answer: {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line for qass is refused while it waits on the owner, and only then.
    #[test]
    fn qass_hears_nothing_while_it_waits_on_the_owner() {
        let refused = hearing(Some("blocked")).unwrap_err();
        assert!(refused.contains("qass waits on you"), "{refused}");
        for state in [None, Some("idle"), Some("working"), Some("done")] {
            assert_eq!(hearing(state), Ok(()), "{state:?}");
        }
    }

    fn tab(id: i64, cwd: &str, agent: Option<&str>) -> qoop_store::TerminalRow {
        qoop_store::TerminalRow {
            id,
            workspace: 2,
            title: None,
            cwd: cwd.to_string(),
            argv: String::new(),
            agent: agent.map(String::from),
            agent_session: None,
            created_at: 0,
            closed_at: None,
        }
    }

    #[test]
    fn a_write_is_noted_for_the_watcher_only_in_a_qass_folder_that_exists() {
        let home = tempfile::tempdir().expect("a directory");
        let folder = home.path().join("qass");
        let at = || Some(folder.clone());
        note_prompted_in(at(), Some("128"), 52, 100).expect("no folder is no error");
        assert!(!folder.exists(), "a person without qass gets no folder");
        std::fs::create_dir(&folder).expect("folder");
        note_prompted_in(at(), Some("128"), 52, 100).expect("noted");
        note_prompted_in(at(), None, 52, 100).expect("outside a terminal, nothing");
        note_prompted_in(at(), Some("x"), 52, 100).expect("a broken number, nothing");
        note_prompted_in(None, Some("7"), 52, 100).expect("no home, nothing");
        note_prompted_in(at(), Some("7"), 52, 101).expect("noted");
        let log = std::fs::read_to_string(folder.join(PROMPTED_LOG)).expect("log");
        assert_eq!(log, "100 128 T52\n101 7 T52\n");
    }

    #[test]
    fn the_watcher_reads_the_log_qoop_writes() {
        // Both scripts name it under @QASS_DATA@, which the install fills in from the same
        // `qass_data_dir()` this module writes under (`skills.rs`, `QassDirs::here`).
        let watch = include_str!("../../../skills/qass/watch.sh");
        assert!(watch.contains("dir=\"@QASS_DATA@\""));
        assert!(watch.contains(&format!("\"$dir/{PROMPTED_LOG}\"")));
        let on = include_str!("../../../skills/qass/on.sh");
        assert!(on.contains(&format!("@QASS_DATA@/{PROMPTED_LOG}")));
    }

    #[test]
    fn only_the_agent_in_qasss_folder_is_the_sleeping_qass() {
        let dir = std::env::temp_dir();
        let folder = dir.to_str().expect("utf8 temp dir");
        let other = std::env::current_dir().expect("cwd");
        let other = other.to_str().expect("utf8 cwd");
        // A worker parked in the space, this page's own tab (no agent), then qass itself.
        let tabs = [
            tab(5, other, Some("claude")),
            tab(6, folder, None),
            tab(7, folder, Some("claude")),
        ];
        let asleep = [(5, 2, 30), (6, 2, 20), (7, 2, 10)];
        assert_eq!(sleeping_qass(&tabs, &asleep, &dir), Some(7));
        assert_eq!(sleeping_qass(&tabs[..2], &asleep, &dir), None);
        assert_eq!(sleeping_qass(&tabs, &[(5, 2, 30)], &dir), None);
    }

    /// Each agent carries on its own conversation: Claude its newest kept one; another agent
    /// the id its hook kept in qass's folder while qass last ran on it, else the newest qass
    /// tab's while that tab still names it, never an older tab's.
    #[test]
    fn qass_carries_on_its_own_conversation() {
        let with = |id: i64, agent: Option<&str>, session: Option<&str>| {
            let mut t = tab(id, "/q", agent);
            t.agent_session = session.map(String::from);
            t
        };
        let none = || -> Option<String> { panic!("not claude") };
        let older = with(4, Some("codex"), Some("s4"));
        let closed = with(9, None, None);
        assert_eq!(
            carry(
                "gemini",
                std::slice::from_ref(&closed),
                Some("gemini"),
                Some("g1"),
                none
            ),
            Some("g1".into())
        );
        assert_eq!(
            carry(
                "gemini",
                std::slice::from_ref(&closed),
                Some("codex"),
                Some("g1"),
                none
            ),
            None
        );
        assert_eq!(
            carry(
                "codex",
                &[with(9, Some("codex"), Some("s9")), older.clone()],
                None,
                None,
                none
            ),
            Some("s9".into())
        );
        assert_eq!(
            carry("codex", &[closed.clone(), older], Some("codex"), None, none),
            None
        );
        assert_eq!(carry("opencode", &[], Some("opencode"), None, none), None);
        assert_eq!(
            carry("claude", &[closed], None, None, || Some("c1".into())),
            Some("c1".into())
        );
    }

    /// Claude carries on the newest conversation it can: one qoop kept the id of (in another
    /// project too) or the newest in qass's folder, never a file with no message in it.
    #[test]
    fn claude_carries_on_its_newest_conversation_with_a_message() {
        let root = tempfile::tempdir().expect("dir");
        let folder = root.path().join("my qass");
        std::fs::create_dir_all(&folder).expect("folder");
        let slug: String = folder
            .canonicalize()
            .expect("canonical")
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let projects = root.path().join("projects");
        let here = projects.join(&slug);
        let elsewhere = projects.join("-old-qass");
        std::fs::create_dir_all(&here).expect("here");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere");
        let write = |path: PathBuf, body: &str, age: u64| {
            std::fs::write(&path, body).expect("write");
            let at = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(at))
                .expect("mtime");
        };
        let said = "{\"type\":\"user\",\"message\":{}}\n";
        write(here.join("a1.jsonl"), said, 300);
        write(here.join("t2.jsonl"), "{\"type\":\"ai-title\"}\n", 10);
        let roots = [projects];
        assert_eq!(
            claude_newest_in(&roots, &[], &folder).as_deref(),
            Some("a1")
        );
        write(elsewhere.join("k3.jsonl"), said, 100);
        assert_eq!(
            claude_newest_in(&roots, &["k3"], &folder).as_deref(),
            Some("k3")
        );
        assert_eq!(
            claude_newest_in(&roots, &["gone"], &root.path().join("none")),
            None
        );
    }

    /// Gemini and Codex conversations are carried on only while the agent still keeps them;
    /// opencode's are taken at its word.
    #[test]
    fn only_a_conversation_still_kept_is_carried_on() {
        let home = tempfile::tempdir().expect("home");
        let chats = home.path().join(".gemini/tmp/qass/chats");
        std::fs::create_dir_all(&chats).expect("chats");
        let reply = r#"{"sessionId":"g-1"}{"type":"user"}{"type":"gemini"}"#;
        std::fs::write(chats.join("session-2026-10-11-g-1.json"), reply).expect("w");
        // Asked, never answered: Gemini does not list it.
        std::fs::write(
            chats.join("session-2026-10-11-g-3.json"),
            r#"{"sessionId":"g-3"}{"type":"user"}"#,
        )
        .expect("w");
        let day = home.path().join(".codex/sessions/2026/10/11");
        std::fs::create_dir_all(&day).expect("day");
        std::fs::write(day.join("rollout-2026-10-11T00-00-00-c-1.jsonl"), "{}").expect("w");
        assert!(still_kept("gemini", "g-1", home.path()));
        assert!(!still_kept("gemini", "g-2", home.path()));
        assert!(!still_kept("gemini", "g-3", home.path()));
        assert!(still_kept("codex", "c-1", home.path()));
        assert!(!still_kept("codex", "c-2", home.path()));
        assert!(still_kept("opencode", "o-1", home.path()));
    }

    /// A person's own line keeps its flags, the resume words after it; agents other than
    /// Claude hear what to pick up again.
    #[test]
    fn a_resume_keeps_the_owners_flags() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let line = Some("claude --dangerously-skip-permissions".to_string());
        assert_eq!(
            resume_command(line.clone(), words(&["claude", "--resume", "c1"]), "claude"),
            (line, words(&["--resume", "c1"]))
        );
        let (typed, said) = resume_command(None, words(&["codex", "resume", "s9"]), "codex");
        assert_eq!(typed, None);
        assert_eq!(&said[..3], words(&["codex", "resume", "s9"]).as_slice());
        assert!(said[3].contains("carried on"));
        let (_, said) = resume_command(None, words(&["opencode", "--session", "o"]), "opencode");
        assert_eq!(said[3], "--prompt");
        // An override naming another binary is typed whole.
        let mine = Some("claude --x".to_string());
        assert_eq!(
            resume_command(mine, words(&["my-claude", "--resume", "c1"]), "claude"),
            (None, words(&["my-claude", "--resume", "c1"]))
        );
    }

    #[test]
    fn a_launcher_line_names_its_program() {
        assert_eq!(program_of("claude --model sonnet"), "claude");
        assert_eq!(program_of("FOO=1 /usr/local/bin/codex -m gpt-5.5"), "codex");
        assert_eq!(program_of(""), "");
        assert!(first_message("aider").is_none());
    }

    #[test]
    fn each_agent_is_told_to_start_qass_its_own_way() {
        let words = |agent: &str| launch_line(agent, &[]).expect(agent).1;
        assert_eq!(words("claude"), ["claude", "/qass"]);
        assert_eq!(words("codex"), ["codex", "$qass"]);
        assert_eq!(words("gemini"), ["gemini", "-i", "Use the qass skill."]);
        assert_eq!(
            words("opencode"),
            ["opencode", "--prompt", "Use the qass skill."]
        );
        assert!(launch_line("aider", &[]).is_err());
        let mine = config::Launcher {
            name: "claudes".into(),
            command: "claude --model sonnet".into(),
        };
        let (typed, words, program) = launch_line("claudes", &[mine]).expect("launcher");
        assert_eq!(typed.as_deref(), Some("claude --model sonnet"));
        assert_eq!(words, ["/qass"]);
        assert_eq!(program, "claude");
    }
}

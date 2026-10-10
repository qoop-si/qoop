//! `qoop qass`: open qass, the assistant over every agent, in one step.
//!
//! The one already running is shown; one asleep in the `qass` space is woken; otherwise a
//! new one starts in its own folder (`qoop paths qass-data`, where its skill, notes and
//! scratch live) on the agent `config.toml`
//! names (`[qass] agent`: claude, codex, gemini, opencode, or a person's own `[agents]`
//! line), told to start its qass skill. Closing it is the `x` on its row, or `qoop close`.

use std::path::Path;
use std::process::Command;

use qoop_runtime::paths;
use qoop_tui::config;

use crate::supervise;

/// The space qass lives in.
pub(crate) const SPACE: &str = "qass";

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
fn launch(agent: &str, read: &config::Read) -> std::io::Result<(Option<String>, Vec<String>)> {
    let (line, words, program) = launch_line(agent, &read.agents).map_err(std::io::Error::other)?;
    if !crate::skills::qass_installed_for(&program) {
        return Err(std::io::Error::other(
            "qass is not set up; run `qoop skills install` first",
        ));
    }
    Ok((line, words))
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
        let (typed, command) = launch(&read.qass_agent, &read)?;
        // No qass runs or sleeps: none can still be using the copies older installs left
        // in each agent's own skills.
        crate::skills::remove_old_qass();
        if program_of(typed.as_deref().unwrap_or(&read.qass_agent)) == "gemini" {
            crate::skills::trust_qass_for_gemini()?;
        }
        std::fs::create_dir_all(&folder)?;
        let opts = supervise::NewOpts {
            name: Some("qass".into()),
            cwd: Some(folder),
            space: Some(SPACE.into()),
            command,
            typed,
        };
        let mut opened = 0;
        supervise::open_terminal(&opts, |terminal| opened = terminal)?;
        println!("qass started in terminal {opened} on {}", read.qass_agent);
        u64::from(opened)
    };
    // Shown where the person is looking; with no interface open, the number says where.
    let _ = run(&["focus", &terminal.to_string()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

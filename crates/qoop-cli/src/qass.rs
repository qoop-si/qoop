//! `qoro qass`: open qass, the assistant over every agent, in one step.
//!
//! The one already running is shown; one asleep in the `qass` space is woken; otherwise a
//! new one starts in its own folder (`qoro paths qass-data`) on the agent `config.toml`
//! names (`[qass] agent`: claude, codex, gemini, opencode, or a person's own `[agents]`
//! line), told to start its qass skill. Closing it is the `x` on its row, or `qoro close`.

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

/// Another qoro command's JSON, from this same binary.
fn json(args: &[&str]) -> Option<serde_json::Value> {
    let me = std::env::current_exe().ok()?;
    let out = Command::new(me).args(args).output().ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Another qoro command run for its effect, failing with what it said.
fn run(args: &[&str]) -> std::io::Result<()> {
    let out = Command::new(std::env::current_exe()?).args(args).output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

/// The terminal of a running qass: an agent whose folder is qass's own.
fn running(folder: &Path) -> Option<u64> {
    let folder = folder.canonicalize().ok()?;
    json(&["agents", "--json"])?
        .as_array()?
        .iter()
        .filter(|row| row["agent"] == true)
        .find(|row| {
            row["cwd"]
                .as_str()
                .and_then(|cwd| Path::new(cwd).canonicalize().ok())
                .is_some_and(|cwd| cwd == folder)
        })
        .and_then(|row| row["terminal"].as_u64())
}

/// A tab asleep in the `qass` space.
fn asleep() -> Option<u64> {
    json(&["ls", "--json"])?
        .as_array()?
        .iter()
        .find(|space| space["name"] == SPACE)?["sleeping"]
        .as_array()?
        .first()?
        .as_u64()
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
        return Err(std::io::Error::other(format!(
            "qass is not installed for {program}; run `qoro skills install {program}` first"
        )));
    }
    Ok((line, words))
}

pub fn run_qass(args: &[String]) -> std::io::Result<()> {
    if args.len() > 2 {
        return Err(crate::usage("usage: qoro qass"));
    }
    let folder = paths::qass_data_dir()
        .ok_or_else(|| std::io::Error::other("no HOME to keep qass's folder under"))?;
    let terminal = if let Some(terminal) = running(&folder) {
        println!("qass is terminal {terminal}");
        terminal
    } else if let Some(terminal) = asleep() {
        run(&["wake", "--tab", &terminal.to_string()])?;
        println!("qass woke in terminal {terminal}");
        terminal
    } else {
        let read = config::read();
        let (typed, command) = launch(&read.qass_agent, &read)?;
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

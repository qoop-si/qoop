//! Installing the skill that teaches an agent how to use qoop.
//!
//! The hook tells the daemon what the agent is doing; the skill tells the
//! agent what qoop is for. Same three rules as the hooks installer:
//!
//! * Added beside what is already there, never instead of it. A skill
//!   directory holding a person's own skills keeps all of them.
//! * Installing twice changes nothing the second time.
//! * Uninstalling removes exactly what was added: the one file, and the
//!   directory only when it is left empty.
//!
//! Cursor is absent on purpose, like a half-understood hook format: no
//! skills directory convention was found, and writing one up would leave a
//! file no agent reads while reporting success. Opencode ships
//! opt-in only: its `skills/` directory exists but nothing proves it is
//! read, so `status` never calls it installed.

use std::path::{Path, PathBuf};

/// The skill text, compiled in so the binary needs nothing beside it.
const BODY: &str = include_str!("../../../skills/qoop/SKILL.md");

/// The file name every agent reads.
const FILE: &str = "SKILL.md";

/// qass, the assistant over every other agent, lives in one folder of its own (`qoop paths
/// qass-data`): qoop always starts it there, on whichever agent `[qass] agent` names, and that
/// folder holds its skill, its notes and its scratch. The skill sits where each agent reads a
/// folder's own skills, Claude in `.claude/skills`, Codex, Gemini and opencode in
/// `.agents/skills`: the same files in each, which find one another by their own directory.
/// Its rules are in its SKILL.md; nothing beside them holds it back.
const QASS_SKILL_DIRS: [&str; 2] = [".claude/skills/qass", ".agents/skills/qass"];

/// The folder's own instructions, which every agent reads at its start: who qass is, and where
/// its skill, the owner's lessons, its notes and its scratch are. Written only where the folder
/// has none: an agent may add to it, and nothing it added is ever written over.
const QASS_AGENTS: &str = include_str!("../../../skills/qass/home-agents.md");

/// Claude and Gemini read a file of their own name: each only points at `AGENTS.md`, and is
/// written only where the folder has none.
const QASS_POINTERS: [(&str, &str); 2] = [
    ("CLAUDE.md", "@AGENTS.md\n"),
    ("GEMINI.md", "@./AGENTS.md\n"),
];

/// Where qass keeps its own state and works (`data`), and the owner's files (`owner`), as
/// `qoop paths` shows them. The skill's files name them as `@QASS_DATA@` and `@QASS_CONFIG@`,
/// filled in here at install. The working folder's `.claude/settings.json` lets Claude run
/// qass's commands without asking and loads its lessons, for any Claude started there, by
/// hand, restored or woken (a restore types a bare `claude --resume`, so flags given at
/// launch do not come back).
struct QassDirs {
    data: PathBuf,
    owner: PathBuf,
}

impl QassDirs {
    fn here() -> std::io::Result<Self> {
        Ok(Self {
            data: qoop_runtime::paths::qass_data_dir()
                .ok_or_else(|| std::io::Error::other("no HOME to keep qass's files under"))?,
            owner: qoop_runtime::paths::qass_config_dir(),
        })
    }

    fn render(&self, body: &str) -> String {
        body.replace("@QASS_DATA@", &self.data.display().to_string())
            .replace("@QASS_CONFIG@", &self.owner.display().to_string())
    }

    fn folder_settings(&self) -> PathBuf {
        self.data.join(".claude/settings.json")
    }

    fn skill_dirs(&self) -> impl Iterator<Item = PathBuf> + '_ {
        QASS_SKILL_DIRS.iter().map(|dir| self.data.join(dir))
    }
}

/// Name, body, and whether it is a program.
const QASS: &[(&str, &str, bool)] = &[
    (
        "SKILL.md",
        include_str!("../../../skills/qass/SKILL.md"),
        false,
    ),
    (
        "watch.sh",
        include_str!("../../../skills/qass/watch.sh"),
        true,
    ),
    (
        "escalate.sh",
        include_str!("../../../skills/qass/escalate.sh"),
        true,
    ),
    (
        "spawn.sh",
        include_str!("../../../skills/qass/spawn.sh"),
        true,
    ),
    ("on.sh", include_str!("../../../skills/qass/on.sh"), true),
    (
        "init.md",
        include_str!("../../../skills/qass/init.md"),
        false,
    ),
    (
        "settings.json",
        include_str!("../../../skills/qass/settings.json"),
        false,
    ),
];

/// What older installs wrote and qass no longer has: taken out at the next install.
const QASS_GONE: [&str; 4] = ["gate.sh", "gate.py", "worker.json", "worker-opencode.json"];

/// One agent's skills directory, and whether it is proven to be read.
struct Target {
    /// What a person calls the agent, and what they type to install it.
    agent: &'static str,
    /// That agent's skills directory for qoop, under the person's home.
    dir: &'static str,
    /// True while no evidence shows the agent reads this directory.
    /// Experimental targets never install by default and are never
    /// reported as installed.
    experimental: bool,
}

/// Every agent this can install for.
fn targets() -> Vec<Target> {
    vec![
        Target {
            agent: "claude",
            dir: ".claude/skills/qoop",
            experimental: false,
        },
        Target {
            agent: "codex",
            dir: ".codex/skills/qoop",
            experimental: false,
        },
        Target {
            agent: "gemini",
            dir: ".gemini/skills/qoop",
            experimental: false,
        },
        Target {
            agent: "opencode",
            dir: ".config/opencode/skills/qoop",
            // Seen read on 2026-10-03 (opencode 1.18.34): `opencode run` loaded a skill
            // placed there.
            experimental: false,
        },
    ]
}

/// Whether qass's skill sits in its folder where this program reads skills.
pub(crate) fn qass_installed_for(program: &str) -> bool {
    let dir = match program {
        "claude" => QASS_SKILL_DIRS[0],
        _ => QASS_SKILL_DIRS[1],
    };
    QassDirs::here().is_ok_and(|dirs| dirs.data.join(dir).join(FILE).exists())
}

/// Where older installs put qass, beside each agent's own qoop skill, and `~/.agents/skills`.
fn old_qass_dirs(home: &Path) -> Vec<PathBuf> {
    targets()
        .iter()
        .map(|t| home.join(Path::new(t.dir).with_file_name("qass")))
        .chain([home.join(".agents/skills/qass")])
        .collect()
}

/// The rest of those older copies, taken away once no qass can still be running from them: at
/// a fresh start of qass, never a wake. Exactly qass's files; a directory only when left empty.
pub(crate) fn remove_old_qass() {
    if let Some(home) = home() {
        remove_old_qass_in(&home);
    }
}

fn remove_old_qass_in(home: &Path) {
    for dir in old_qass_dirs(home) {
        // A qass started some other way may still run something from there.
        if std::process::Command::new("pgrep")
            .args(["-f", "--"])
            .arg(&dir)
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            continue;
        }
        for name in QASS.iter().map(|(name, _, _)| *name).chain(QASS_GONE) {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_dir(&dir);
    }
}

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// `qoop skills ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("install") => install(&args[3..]),
        Some("uninstall") => uninstall(&args[3..]),
        Some("status") | None => status(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; usage: qoop skills <install|uninstall|status> [agent...]"
        ))),
    }
}

/// Which agents a command is about: the ones named, or every proven one.
///
/// An experimental target installs only when named.
fn wanted(args: &[String]) -> std::io::Result<Vec<Target>> {
    if args.is_empty() {
        return Ok(targets().into_iter().filter(|t| !t.experimental).collect());
    }
    let mut out = Vec::new();
    for name in args {
        let Some(target) = targets().into_iter().find(|t| t.agent == name.as_str()) else {
            let known: Vec<&str> = targets().iter().map(|t| t.agent).collect();
            return Err(std::io::Error::other(format!(
                "no skill for {name}; qoop ships one for {}",
                known.join(", ")
            )));
        };
        out.push(target);
    }
    Ok(out)
}

fn path_of(home: &Path, target: &Target) -> PathBuf {
    home.join(target.dir).join(FILE)
}

/// Whether this agent's skill file carries this build's body.
fn current(home: &Path, target: &Target) -> bool {
    std::fs::read_to_string(path_of(home, target))
        .map(|on_disk| on_disk == BODY)
        .unwrap_or(false)
}

/// What `status` reports for one agent, pure so the experimental mark is testable.
fn status_of(home: &Path, target: &Target) -> &'static str {
    match (
        path_of(home, target).exists(),
        current(home, target),
        target.experimental,
    ) {
        (_, _, true) => "experimental, unverified: install explicitly to try it",
        (false, _, _) => "not installed",
        (true, false, _) => "installed, and older than this build: run install again",
        (true, true, _) => "installed",
    }
}

/// Take away the skill this agent was given before the rename, so it is not told twice.
///
/// ponytail: with `legacy`; nothing else of the skills directory is touched.
fn remove_old_skill(home: &Path, target: &Target) -> bool {
    if !qoop_runtime::legacy::migrating() {
        return false;
    }
    let dir = home.join(target.dir.replacen("qoop", "qoro", 1));
    let gone = std::fs::remove_file(dir.join(FILE)).is_ok();
    let _ = std::fs::remove_dir(&dir);
    gone
}

/// Write one agent's skill, answering whether anything changed.
fn install_into(home: &Path, target: &Target) -> std::io::Result<bool> {
    let path = path_of(home, target);
    let mut changed = false;
    if !current(home, target) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, BODY)?;
        changed = true;
    }
    Ok(remove_old_skill(home, target) | changed)
}

/// Write a file unless it already holds `body`, answering whether it wrote.
fn write_changed(path: &Path, body: &str, program: bool) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    if std::fs::read_to_string(path).is_ok_and(|on_disk| on_disk == body) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, body)?;
    if program {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(true)
}

/// Lay out qass's folder: its skill where every agent reads it, its instructions, its scratch.
/// Then take the skill out of every other agent's own skills, so no other session is offered
/// it; the scripts there stay for a qass still running from them, until a fresh start
/// ([`remove_old_qass`]). Answers whether anything changed.
fn install_qass(home: &Path, dirs: &QassDirs) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mut changed = migrate_old_qass(home, dirs)?;
    for private in [&dirs.data, &dirs.owner] {
        std::fs::create_dir_all(private)?;
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
    }
    for dir in dirs.skill_dirs() {
        for gone in QASS_GONE {
            changed |= std::fs::remove_file(dir.join(gone)).is_ok();
        }
        for (name, body, program) in QASS {
            changed |= write_changed(&dir.join(name), &dirs.render(body), *program)?;
        }
    }
    changed |= write_changed(
        &dirs.folder_settings(),
        &dirs.render(qass_settings()),
        false,
    )?;
    let agents = [("AGENTS.md", dirs.render(QASS_AGENTS))];
    for (name, body) in agents
        .into_iter()
        .chain(QASS_POINTERS.map(|(n, b)| (n, b.to_string())))
    {
        let path = dirs.data.join(name);
        if !path.exists() {
            std::fs::write(&path, body)?;
            changed = true;
        }
    }
    std::fs::create_dir_all(dirs.data.join("scratch"))?;
    // An older qass's Codex hooks call a gate that is long gone, on every tool call.
    let hooks = dirs.data.join(".codex/hooks.json");
    if std::fs::read_to_string(&hooks).is_ok_and(|b| b.contains("skills/qass/gate.sh")) {
        changed |= std::fs::remove_file(&hooks).is_ok();
        let _ = std::fs::remove_dir(dirs.data.join(".codex"));
    }
    if home.join(".gemini").is_dir() {
        changed |= trust_for_gemini(home, dirs)?;
    }
    for dir in old_qass_dirs(home) {
        for name in std::iter::once(FILE).chain(QASS_GONE) {
            changed |= std::fs::remove_file(dir.join(name)).is_ok();
        }
    }
    Ok(changed)
}

/// Gemini loads a folder's own skills only from a folder it trusts, and a woken qass comes
/// back as a bare `gemini --resume`: qass's folder is trusted in Gemini's own list, beside
/// whatever the person trusted. At install where Gemini is set up, and again before qass
/// starts on it. A list it cannot read is left as it is.
pub(crate) fn trust_qass_for_gemini() -> std::io::Result<()> {
    let home = home().ok_or_else(|| std::io::Error::other("no HOME"))?;
    trust_for_gemini(&home, &QassDirs::here()?).map(|_| ())
}

fn trust_for_gemini(home: &Path, dirs: &QassDirs) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let gemini = home.join(".gemini");
    std::fs::create_dir_all(&gemini)?;
    let path = gemini.join("trustedFolders.json");
    let mut list = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(serde_json::Value::Object(list)) => list,
            _ => {
                eprintln!(
                    "  gemini     {} is not a list qoop reads; qass's folder is not trusted there",
                    path.display()
                );
                return Ok(false);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(e),
    };
    let folder = dirs.data.display().to_string();
    if list.get(&folder).and_then(|v| v.as_str()) == Some("TRUST_FOLDER") {
        return Ok(false);
    }
    list.insert(folder, "TRUST_FOLDER".into());
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(list))
        .map_err(std::io::Error::other)?;
    std::fs::write(&path, text + "\n")?;
    // As private as Gemini keeps it.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(true)
}

/// Move what an older qass kept in `~/.qass` to its two folders, once: the owner's `policy`
/// and `lessons.md` beside his settings, the rest beside qoop's data. Nothing already at the
/// new place is overwritten. The old folder goes when only what is made again is left in it
/// (its `.claude` settings and `ssh` sockets).
fn migrate_old_qass(home: &Path, dirs: &QassDirs) -> std::io::Result<bool> {
    let old = home.join(".qass");
    let Ok(entries) = std::fs::read_dir(&old) else {
        return Ok(false);
    };
    std::fs::create_dir_all(&dirs.data)?;
    std::fs::create_dir_all(&dirs.owner)?;
    let mut moved = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let to = match name.to_str() {
            Some(".claude" | "ssh") => continue,
            Some("policy" | "lessons.md") => dirs.owner.join(&name),
            _ => dirs.data.join(&name),
        };
        if !to.exists() {
            std::fs::rename(entry.path(), to)?;
            moved = true;
        }
    }
    let left = std::fs::read_dir(&old)?
        .flatten()
        .any(|e| !matches!(e.file_name().to_str(), Some(".claude" | "ssh")));
    if !left {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(moved)
}

fn qass_settings() -> &'static str {
    QASS.iter()
        .find(|(name, _, _)| *name == "settings.json")
        .map_or("", |(_, body, _)| body)
}

/// Take qass's files out of its folder, and each directory only when left empty. Its own
/// state (goals, notes, scratch) and the owner's policy and lessons stay; a `CLAUDE.md` or
/// `GEMINI.md` goes only while it is the one install wrote.
fn remove_qass(dirs: &QassDirs) -> std::io::Result<bool> {
    let mut removed = false;
    for dir in dirs.skill_dirs() {
        for (name, _, _) in QASS {
            removed |= std::fs::remove_file(dir.join(name)).is_ok();
        }
        // The skill's folder, `skills`, then `.claude` or `.agents`: each only when empty.
        for empty in dir.ancestors().take(3) {
            let _ = std::fs::remove_dir(empty);
        }
    }
    removed |= std::fs::remove_file(dirs.folder_settings()).is_ok();
    let _ = std::fs::remove_dir(dirs.data.join(".claude"));
    let ours = [("AGENTS.md", dirs.render(QASS_AGENTS))]
        .into_iter()
        .chain(QASS_POINTERS.map(|(name, body)| (name, body.to_string())));
    for (name, body) in ours {
        let path = dirs.data.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|b| b == body) {
            removed |= std::fs::remove_file(&path).is_ok();
        }
    }
    Ok(removed)
}

fn qass_current(dirs: &QassDirs) -> bool {
    let holds = |path: PathBuf, body: &str| std::fs::read_to_string(path).is_ok_and(|b| b == body);
    dirs.skill_dirs().all(|dir| {
        QASS.iter()
            .all(|(name, body, _)| holds(dir.join(name), &dirs.render(body)))
    }) && holds(dirs.folder_settings(), &dirs.render(qass_settings()))
        && dirs.data.join("AGENTS.md").exists()
}

/// Remove one agent's skill, answering whether anything changed.
///
/// The directory goes only when left empty; a directory holding somebody
/// else's files stays.
fn remove_from(home: &Path, target: &Target) -> std::io::Result<bool> {
    let path = path_of(home, target);
    let old = remove_old_skill(home, target);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent()
                && std::fs::read_dir(parent).is_ok_and(|mut e| e.next().is_none())
            {
                let _ = std::fs::remove_dir(parent);
            }
            Ok(true)
        }
        Err(_) => Ok(old),
    }
}

fn status() -> std::io::Result<()> {
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to inspect"));
    };
    let dirs = QassDirs::here()?;
    for target in targets() {
        println!("  {:<10} {}", target.agent, status_of(&home, &target));
    }
    let qass = match (dirs.skill_dirs().any(|d| d.exists()), qass_current(&dirs)) {
        (false, _) => "not installed",
        (true, false) => "installed, and older than this build: run install again",
        (true, true) => "installed",
    };
    println!(
        "  {:<10} {qass}, in its folder {}",
        "qass",
        dirs.data.display()
    );
    println!(
        "\n  qoop skills install [agent...]    add the skill, beside whatever is already there"
    );
    println!("  qoop skills uninstall [agent...]  remove exactly what was added");
    Ok(())
}

fn install(args: &[String]) -> std::io::Result<()> {
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to install into"));
    };
    let wanted = wanted(args)?;
    // qass is one folder for every agent it may run on.
    let dirs = QassDirs::here()?;
    if install_qass(&home, &dirs)? {
        println!(
            "  {:<10} installed into its folder {}",
            "qass",
            dirs.data.display()
        );
    }
    for target in wanted {
        // Asked for every agent, only the ones on this machine, as `hooks install` does:
        // an agent named on the command line is installed whether or not it is here yet.
        if args.is_empty() && !crate::hooks::agent_present(target.agent) {
            println!("  {:<10} not present, skipped", target.agent);
            continue;
        }
        if !install_into(&home, &target)? {
            println!("  {:<10} already installed", target.agent);
            continue;
        }
        if target.experimental {
            println!(
                "  {:<10} written (experimental, unverified: nothing proves this agent reads it)",
                target.agent
            );
        } else {
            println!("  {:<10} installed into {}", target.agent, target.dir);
        }
    }
    Ok(())
}

fn uninstall(args: &[String]) -> std::io::Result<()> {
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to uninstall from"));
    };
    // qass serves every agent from one folder: it goes only when every agent's skill does.
    if args.is_empty() {
        let dirs = QassDirs::here()?;
        if remove_qass(&dirs)? {
            println!(
                "  {:<10} removed from its folder {}",
                "qass",
                dirs.data.display()
            );
        }
    }
    for target in wanted(args)? {
        match remove_from(&home, &target)? {
            true => println!("  {:<10} removed from {}", target.agent, target.dir),
            false => println!("  {:<10} was not there", target.agent),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a directory")
    }

    fn target(agent: &str) -> Target {
        targets()
            .into_iter()
            .find(|t| t.agent == agent)
            .expect("a target")
    }

    /// Installing writes the file and reinstalling changes nothing.
    #[test]
    fn installing_writes_the_skill_and_again_changes_nothing() {
        let home = home_dir();
        let claude = target("claude");
        install_into(home.path(), &claude).expect("install");
        assert_eq!(
            std::fs::read_to_string(path_of(home.path(), &claude)).expect("read"),
            BODY
        );
        assert!(
            !install_into(home.path(), &claude).expect("reinstall"),
            "the second install reported a change"
        );
        assert_eq!(
            std::fs::read_to_string(path_of(home.path(), &claude)).expect("read"),
            BODY
        );
    }

    /// A neighbour skill survives both install and uninstall.
    #[test]
    fn neighbours_survive_install_and_uninstall() {
        let home = home_dir();
        let claude = target("claude");
        let neighbour = home.path().join(".claude/skills/other/SKILL.md");
        std::fs::create_dir_all(neighbour.parent().expect("a parent")).expect("dir");
        std::fs::write(&neighbour, "theirs").expect("write");

        install_into(home.path(), &claude).expect("install");
        assert_eq!(std::fs::read_to_string(&neighbour).expect("read"), "theirs");
        assert!(remove_from(home.path(), &claude).expect("uninstall"));
        assert_eq!(
            std::fs::read_to_string(&neighbour).expect("read"),
            "theirs",
            "their skill went with ours"
        );
        // Our directory is gone when left empty; theirs stays.
        assert!(!home.path().join(".claude/skills/qoop").exists());
        assert!(neighbour.exists());
    }

    /// The workflows qass reads stay inside the budget they promise an agent's context: the
    /// default in init.md and this repo's own, at most 12 steps and 4,000 characters each.
    #[test]
    fn the_workflows_stay_within_their_budget() {
        let init = QASS
            .iter()
            .find(|(name, _, _)| *name == "init.md")
            .expect("init.md is installed")
            .1;
        let default = init
            .split("## The default workflow")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("the default workflow section");
        let own = include_str!("../../../.qoop/workflow.md");
        for (what, text) in [("the default", default), ("qoop's own", own)] {
            let chars = text.chars().count();
            assert!(chars <= 4_000, "{what} workflow is {chars} characters");
            let steps = text
                .lines()
                .filter(|l| {
                    l.split_once(". ")
                        .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
                })
                .count();
            assert!(
                (1..=12).contains(&steps),
                "{what} workflow has {steps} steps"
            );
        }
        let skill = QASS
            .iter()
            .find(|(name, _, _)| *name == "SKILL.md")
            .expect("SKILL.md is installed")
            .1;
        assert!(skill.contains("init.md"), "SKILL.md points at init.md");
        assert!(skill.lines().count() <= 400, "SKILL.md grew past 400 lines");
    }

    fn qass_dirs(home: &tempfile::TempDir) -> QassDirs {
        QassDirs {
            data: home.path().join("data/qass"),
            owner: home.path().join("config/qass"),
        }
    }

    /// qass installs whole into its own folder, where every agent reads it, its programs
    /// runnable, changes nothing the second time, and uninstalls to its own state while a
    /// neighbour's file stays.
    #[test]
    fn qass_installs_whole_into_its_folder_and_leaves_neighbours() {
        use std::os::unix::fs::PermissionsExt;
        let home = home_dir();
        let dirs = qass_dirs(&home);
        assert!(install_qass(home.path(), &dirs).expect("install"));
        assert!(qass_current(&dirs));
        for dir in [".claude/skills/qass", ".agents/skills/qass"] {
            for (name, _, _) in QASS {
                let body = std::fs::read_to_string(dirs.data.join(dir).join(name)).expect("file");
                assert!(
                    !body.contains("@QASS_"),
                    "{dir}/{name} still names a placeholder"
                );
            }
            let mode = std::fs::metadata(dirs.data.join(dir).join("watch.sh"))
                .expect("watch")
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "{dir}/watch.sh is not runnable");
        }
        // Every agent is told where its lessons, notes and scratch are.
        let agents = std::fs::read_to_string(dirs.data.join("AGENTS.md")).expect("AGENTS.md");
        assert!(agents.contains(&dirs.owner.join("lessons.md").display().to_string()));
        assert!(agents.contains(&dirs.data.join("memory.md").display().to_string()));
        assert!(dirs.data.join("scratch").is_dir());
        for (name, body) in QASS_POINTERS {
            assert_eq!(
                std::fs::read_to_string(dirs.data.join(name)).expect(name),
                body
            );
        }
        // Its folder loads the lessons, so a restored or woken qass still has them.
        let folder = std::fs::read_to_string(dirs.folder_settings()).expect("folder settings");
        assert!(folder.contains(&dirs.owner.display().to_string()));
        assert!(
            !install_qass(home.path(), &dirs).expect("reinstall"),
            "the second install changed something"
        );
        // A CLAUDE.md qass or the owner wrote is never replaced, nor taken away.
        std::fs::write(dirs.data.join("CLAUDE.md"), "mine").expect("write");
        let theirs = dirs.data.join(".claude/skills/qass/notes.txt");
        std::fs::write(&theirs, "theirs").expect("write");
        std::fs::write(dirs.data.join("goals.md"), "goals").expect("write");
        assert!(!install_qass(home.path(), &dirs).expect("again"));
        assert!(remove_qass(&dirs).expect("uninstall"));
        assert_eq!(std::fs::read_to_string(&theirs).expect("read"), "theirs");
        assert_eq!(
            std::fs::read_to_string(dirs.data.join("CLAUDE.md")).expect("read"),
            "mine"
        );
        assert!(dirs.data.join("goals.md").exists() && dirs.data.join("scratch").is_dir());
        assert!(!dirs.data.join("AGENTS.md").exists() && !dirs.data.join("GEMINI.md").exists());
        assert!(!dirs.data.join(".agents").exists() && !dirs.folder_settings().exists());
        std::fs::remove_file(&theirs).expect("rm");
        assert!(!remove_qass(&dirs).expect("uninstall again"));
        assert!(!dirs.data.join(".claude").exists());
    }

    /// The copies older installs put in each agent's own skills lose their SKILL.md at
    /// install, so no other session is offered qass, and their gate; their scripts stay for
    /// a qass still running from them until a fresh start takes the rest, and nothing else.
    #[test]
    fn an_older_global_qass_goes_in_two_steps() {
        let home = home_dir();
        let dirs = qass_dirs(&home);
        let old = [
            ".claude/skills/qass",
            ".codex/skills/qass",
            ".agents/skills/qass",
        ]
        .map(|d| home.path().join(d));
        for dir in &old {
            std::fs::create_dir_all(dir).expect("dir");
            for name in ["SKILL.md", "watch.sh", "gate.py"] {
                std::fs::write(dir.join(name), "old").expect("old");
            }
        }
        let theirs = home.path().join(".claude/skills/other/SKILL.md");
        std::fs::create_dir_all(theirs.parent().expect("parent")).expect("dir");
        std::fs::write(&theirs, "theirs").expect("write");
        assert!(install_qass(home.path(), &dirs).expect("install"));
        for dir in &old {
            assert!(!dir.join("SKILL.md").exists() && !dir.join("gate.py").exists());
            assert!(
                dir.join("watch.sh").exists(),
                "a running qass lost its watcher"
            );
        }
        remove_old_qass_in(home.path());
        assert!(old.iter().all(|dir| !dir.exists()));
        assert!(theirs.exists());
        assert!(
            qass_current(&dirs),
            "the fresh start took qass's own folder"
        );
    }

    /// An AGENTS.md already in the folder, qoop's with an agent's notes added or anyone
    /// else's, is never written over.
    #[test]
    fn an_agents_md_already_there_is_kept() {
        let home = home_dir();
        let dirs = qass_dirs(&home);
        install_qass(home.path(), &dirs).expect("install");
        let agents = dirs.data.join("AGENTS.md");
        let added = std::fs::read_to_string(&agents).expect("ours") + "- ubox needs the VPN\n";
        std::fs::write(&agents, &added).expect("w");
        assert!(!install_qass(home.path(), &dirs).expect("again"));
        assert_eq!(std::fs::read_to_string(&agents).expect("read"), added);
        assert!(qass_current(&dirs));
        assert!(remove_qass(&dirs).expect("uninstall"));
        assert!(agents.exists(), "an AGENTS.md with notes in it went");
    }

    /// Gemini trusts qass's folder beside what the person trusted, once; a list it cannot
    /// read is left alone.
    #[test]
    fn gemini_trusts_qasss_folder() {
        let home = home_dir();
        let dirs = qass_dirs(&home);
        // Install leaves a machine without Gemini alone.
        install_qass(home.path(), &dirs).expect("install");
        assert!(!home.path().join(".gemini").exists());
        let list = home.path().join(".gemini/trustedFolders.json");
        std::fs::create_dir_all(list.parent().expect("parent")).expect("dir");
        std::fs::write(&list, r#"{"/work": "TRUST_PARENT"}"#).expect("write");
        assert!(trust_for_gemini(home.path(), &dirs).expect("trust"));
        let read: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&list).expect("read")).expect("json");
        assert_eq!(read["/work"], "TRUST_PARENT");
        assert_eq!(read[dirs.data.display().to_string()], "TRUST_FOLDER");
        assert!(!trust_for_gemini(home.path(), &dirs).expect("again"));
        std::fs::write(&list, "not json").expect("write");
        assert!(!trust_for_gemini(home.path(), &dirs).expect("unreadable"));
        assert_eq!(std::fs::read_to_string(&list).expect("read"), "not json");
    }

    /// An older qass's Codex hooks, which call a gate long gone, leave its folder; Codex
    /// hooks of anyone else's stay.
    #[test]
    fn a_stale_codex_gate_hook_goes() {
        let home = home_dir();
        let dirs = qass_dirs(&home);
        let hooks = dirs.data.join(".codex/hooks.json");
        std::fs::create_dir_all(hooks.parent().expect("parent")).expect("dir");
        std::fs::write(&hooks, "{\"cmd\": \"$HOME/.claude/skills/qass/gate.sh\"}").expect("w");
        install_qass(home.path(), &dirs).expect("install");
        assert!(!hooks.exists());
        std::fs::create_dir_all(hooks.parent().expect("parent")).expect("dir");
        std::fs::write(&hooks, "{}").expect("w");
        install_qass(home.path(), &dirs).expect("install");
        assert!(hooks.exists());
    }

    /// An older qass's `~/.qass` moves across once: the owner's files beside his settings,
    /// the rest beside qoop's data, nothing overwritten, and the old folder gone once only
    /// what is made again is left in it.
    #[test]
    fn an_old_qass_folder_moves_once() {
        let home = home_dir();
        let old = home.path().join(".qass");
        std::fs::create_dir_all(old.join(".claude")).expect("old");
        std::fs::create_dir_all(old.join("ssh")).expect("ssh");
        for (name, body) in [
            ("policy", "mode auto"),
            ("lessons.md", "- x -> escalate (E1)"),
            ("goals.md", "# Goals"),
            ("gate.jsonl", "{}"),
        ] {
            std::fs::write(old.join(name), body).expect("write");
        }
        let dirs = QassDirs {
            data: home.path().join("data/qass"),
            owner: home.path().join("config/qass"),
        };
        std::fs::create_dir_all(&dirs.data).expect("data");
        std::fs::write(dirs.data.join("gate.jsonl"), "newer").expect("newer");
        install_qass(home.path(), &dirs).expect("install");
        let read = |p: PathBuf| std::fs::read_to_string(p).expect("read");
        assert_eq!(read(dirs.owner.join("policy")), "mode auto");
        assert_eq!(read(dirs.owner.join("lessons.md")), "- x -> escalate (E1)");
        assert_eq!(read(dirs.data.join("goals.md")), "# Goals");
        assert_eq!(
            read(dirs.data.join("gate.jsonl")),
            "newer",
            "nothing overwritten"
        );
        // gate.jsonl stayed behind (its new place was taken), so the old folder stays too.
        assert!(old.join("gate.jsonl").exists());
        std::fs::remove_file(old.join("gate.jsonl")).expect("rm");
        install_qass(home.path(), &dirs).expect("again");
        assert!(!old.exists(), "only what is made again was left");
    }

    /// Uninstalling twice is not an error.
    #[test]
    fn uninstalling_again_is_not_an_error() {
        let home = home_dir();
        let codex = target("codex");
        assert!(!remove_from(home.path(), &codex).expect("uninstall"));
        install_into(home.path(), &codex).expect("install");
        assert!(remove_from(home.path(), &codex).expect("uninstall"));
        assert!(!remove_from(home.path(), &codex).expect("uninstall again"));
    }

    /// Every agent here is seen to read its directory, so all install by default; a name
    /// that is no target is refused.
    #[test]
    fn every_proven_target_installs_by_default() {
        let all: Vec<&str> = wanted(&[]).unwrap().iter().map(|t| t.agent).collect();
        assert_eq!(all, ["claude", "codex", "gemini", "opencode"]);
        let named = wanted(&["opencode".to_string()]).unwrap();
        assert_eq!(named.len(), 1);
        assert!(!named[0].experimental);
        assert!(wanted(&["cursor".to_string()]).is_err());
    }

    /// The shipped body teaches real commands and no invented ones.
    #[test]
    fn the_body_teaches_real_commands() {
        for command in ["agents", "context", "send", "prompt", "read", "history"] {
            assert!(BODY.contains(command), "the skill never names {command}");
        }
        assert!(
            !BODY.contains("writable=no means send refuses"),
            "send enforces nothing; the skill must not claim it does"
        );
    }

    /// Status names every state, and never calls an experimental file installed.
    #[test]
    fn status_names_every_state_including_experimental() {
        let home = home_dir();
        let claude = target("claude");
        let unproven = Target {
            agent: "unproven",
            dir: ".unproven/skills/qoop",
            experimental: true,
        };
        assert_eq!(status_of(home.path(), &claude), "not installed");
        install_into(home.path(), &claude).expect("install");
        assert_eq!(status_of(home.path(), &claude), "installed");
        std::fs::write(path_of(home.path(), &claude), "stale").expect("write");
        assert!(
            status_of(home.path(), &claude).contains("older than this build"),
            "a stale file reads as older"
        );
        // Present on disk yet never installed: nothing proves it is read.
        install_into(home.path(), &unproven).expect("install");
        assert!(
            status_of(home.path(), &unproven).contains("experimental, unverified"),
            "an experimental file must not read as installed"
        );
    }
}

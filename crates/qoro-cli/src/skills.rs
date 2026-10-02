//! Installing the skill that teaches an agent how to use qoro.
//!
//! The hook tells the daemon what the agent is doing; the skill tells the
//! agent what qoro is for. Same three rules as the hooks installer:
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
const BODY: &str = include_str!("../../../skills/qoro/SKILL.md");

/// The file name every agent reads.
const FILE: &str = "SKILL.md";

/// qass, the assistant over every other agent. Claude only: it stands on Claude Code's
/// Monitor tool, hooks and dontAsk mode. Installed with Claude's qoro skill, as files that
/// sit beside each other because the scripts find one another by their own directory.
const QASS_DIR: &str = ".claude/skills/qass";

/// Where qass keeps its own state and works (`data`), and the owner's files (`owner`), as
/// `qoro paths` shows them. The skill's files name them as `@QASS_DATA@` and `@QASS_CONFIG@`,
/// filled in here at install. The working folder's `.claude/settings.json` holds the gate
/// and its permission mode, loaded by any Claude started there, by hand, restored or woken
/// (a restore types a bare `claude --resume`, so flags given at launch do not come back).
struct QassDirs {
    data: PathBuf,
    owner: PathBuf,
}

impl QassDirs {
    fn here() -> std::io::Result<Self> {
        Ok(Self {
            data: qoro_runtime::paths::qass_data_dir()
                .ok_or_else(|| std::io::Error::other("no HOME to keep qass's files under"))?,
            owner: qoro_runtime::paths::qass_config_dir(),
        })
    }

    fn render(&self, body: &str) -> String {
        body.replace("@QASS_DATA@", &self.data.display().to_string())
            .replace("@QASS_CONFIG@", &self.owner.display().to_string())
    }

    fn folder_settings(&self) -> PathBuf {
        self.data.join(".claude/settings.json")
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
        "gate.sh",
        include_str!("../../../skills/qass/gate.sh"),
        true,
    ),
    (
        "gate.py",
        include_str!("../../../skills/qass/gate.py"),
        false,
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
        "settings.json",
        include_str!("../../../skills/qass/settings.json"),
        false,
    ),
    (
        "worker.json",
        include_str!("../../../skills/qass/worker.json"),
        false,
    ),
    (
        "worker-opencode.json",
        include_str!("../../../skills/qass/worker-opencode.json"),
        false,
    ),
];

/// One agent's skills directory, and whether it is proven to be read.
struct Target {
    /// What a person calls the agent, and what they type to install it.
    agent: &'static str,
    /// That agent's skills directory for qoro, under the person's home.
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
            dir: ".claude/skills/qoro",
            experimental: false,
        },
        Target {
            agent: "codex",
            dir: ".codex/skills/qoro",
            experimental: false,
        },
        Target {
            agent: "gemini",
            dir: ".gemini/skills/qoro",
            experimental: false,
        },
        Target {
            agent: "opencode",
            dir: ".config/opencode/skills/qoro",
            experimental: true,
        },
    ]
}

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// `qoro skills ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("install") => install(&args[3..]),
        Some("uninstall") => uninstall(&args[3..]),
        Some("status") | None => status(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; usage: qoro skills <install|uninstall|status> [agent...]"
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
                "no skill for {name}; qoro ships one for {}",
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

/// Write one agent's skill, answering whether anything changed.
fn install_into(home: &Path, target: &Target) -> std::io::Result<bool> {
    let path = path_of(home, target);
    if current(home, target) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, BODY)?;
    Ok(true)
}

/// Write qass's files, answering whether anything changed.
fn install_qass(home: &Path, dirs: &QassDirs) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join(QASS_DIR);
    std::fs::create_dir_all(&dir)?;
    let mut changed = migrate_old_qass(home, dirs)?;
    for (name, body, program) in QASS {
        let path = dir.join(name);
        let body = dirs.render(body);
        if std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == body) {
            continue;
        }
        std::fs::write(&path, &body)?;
        if *program {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        changed = true;
    }
    for private in [&dirs.data, &dirs.owner] {
        std::fs::create_dir_all(private)?;
        std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
    }
    let folder = dirs.folder_settings();
    let settings = dirs.render(qass_settings());
    if !std::fs::read_to_string(&folder).is_ok_and(|on_disk| on_disk == settings) {
        std::fs::create_dir_all(folder.parent().expect("has a parent"))?;
        std::fs::write(&folder, settings)?;
        changed = true;
    }
    Ok(changed)
}

/// Move what an older qass kept in `~/.qass` to its two folders, once: the owner's `policy`
/// and `lessons.md` beside his settings, the rest beside qoro's data. Nothing already at the
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

/// Remove exactly qass's files, and its directories only when left empty. Its state and the
/// owner's policy and lessons stay.
fn remove_qass(home: &Path, dirs: &QassDirs) -> std::io::Result<bool> {
    let dir = home.join(QASS_DIR);
    let mut removed = false;
    for (name, _, _) in QASS {
        removed |= std::fs::remove_file(dir.join(name)).is_ok();
    }
    let folder = dirs.folder_settings();
    removed |= std::fs::remove_file(&folder).is_ok();
    for empty in [folder.parent().expect("has a parent"), dir.as_path()] {
        if std::fs::read_dir(empty).is_ok_and(|mut e| e.next().is_none()) {
            let _ = std::fs::remove_dir(empty);
        }
    }
    Ok(removed)
}

fn qass_current(home: &Path, dirs: &QassDirs) -> bool {
    let dir = home.join(QASS_DIR);
    QASS.iter().all(|(name, body, _)| {
        std::fs::read_to_string(dir.join(name)).is_ok_and(|b| b == dirs.render(body))
    }) && std::fs::read_to_string(dirs.folder_settings())
        .is_ok_and(|b| b == dirs.render(qass_settings()))
}

/// Remove one agent's skill, answering whether anything changed.
///
/// The directory goes only when left empty; a directory holding somebody
/// else's files stays.
fn remove_from(home: &Path, target: &Target) -> std::io::Result<bool> {
    let path = path_of(home, target);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            if let Some(parent) = path.parent()
                && std::fs::read_dir(parent).is_ok_and(|mut e| e.next().is_none())
            {
                let _ = std::fs::remove_dir(parent);
            }
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn status() -> std::io::Result<()> {
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to inspect"));
    };
    for target in targets() {
        println!("  {:<10} {}", target.agent, status_of(&home, &target));
    }
    let dirs = QassDirs::here()?;
    let qass = match (home.join(QASS_DIR).exists(), qass_current(&home, &dirs)) {
        (false, _) => "not installed",
        (true, false) => "installed, and older than this build: run install again",
        (true, true) => "installed (with claude)",
    };
    println!("  {:<10} {qass}", "qass");
    println!(
        "\n  qoro skills install [agent...]    add the skill, beside whatever is already there"
    );
    println!("  qoro skills uninstall [agent...]  remove exactly what was added");
    Ok(())
}

fn install(args: &[String]) -> std::io::Result<()> {
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to install into"));
    };
    for target in wanted(args)? {
        if target.agent == "claude" && install_qass(&home, &QassDirs::here()?)? {
            println!("  {:<10} installed into {QASS_DIR}", "qass");
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
    for target in wanted(args)? {
        if target.agent == "claude" && remove_qass(&home, &QassDirs::here()?)? {
            println!("  {:<10} removed from {QASS_DIR}", "qass");
        }
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
        assert!(!home.path().join(".claude/skills/qoro").exists());
        assert!(neighbour.exists());
    }

    /// qass installs whole, its programs runnable, changes nothing the second time,
    /// and uninstalls to nothing while a neighbour's file stays.
    #[test]
    fn qass_installs_whole_and_leaves_neighbours() {
        use std::os::unix::fs::PermissionsExt;
        let home = home_dir();
        let dirs = QassDirs {
            data: home.path().join("data/qass"),
            owner: home.path().join("config/qass"),
        };
        assert!(install_qass(home.path(), &dirs).expect("install"));
        assert!(qass_current(home.path(), &dirs));
        // Every place name is filled in.
        for (name, _, _) in QASS {
            let body =
                std::fs::read_to_string(home.path().join(QASS_DIR).join(name)).expect("file");
            assert!(!body.contains("@QASS_"), "{name} still names a placeholder");
        }
        // Its folder carries the gate too, so a restored or woken qass is gated.
        let folder = std::fs::read_to_string(dirs.folder_settings()).expect("folder settings");
        assert!(folder.contains("gate.sh") && folder.contains(&dirs.owner.display().to_string()));
        let gate = home.path().join(QASS_DIR).join("gate.sh");
        let mode = std::fs::metadata(&gate).expect("gate").permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "gate.sh is not runnable");
        assert!(
            !install_qass(home.path(), &dirs).expect("reinstall"),
            "the second install changed something"
        );
        let theirs = home.path().join(QASS_DIR).join("notes.txt");
        std::fs::write(&theirs, "theirs").expect("write");
        assert!(remove_qass(home.path(), &dirs).expect("uninstall"));
        assert_eq!(std::fs::read_to_string(&theirs).expect("read"), "theirs");
        std::fs::remove_file(&theirs).expect("rm");
        assert!(!remove_qass(home.path(), &dirs).expect("uninstall again"));
        assert!(!home.path().join(QASS_DIR).exists());
        assert!(!dirs.folder_settings().exists());
    }

    /// An older qass's `~/.qass` moves across once: the owner's files beside his settings,
    /// the rest beside qoro's data, nothing overwritten, and the old folder gone once only
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

    /// Experimental targets stay out of the default set.
    #[test]
    fn experimental_targets_install_only_when_named() {
        let all: Vec<String> = wanted(&[])
            .unwrap()
            .iter()
            .map(|t| t.agent.into())
            .collect();
        assert!(all.contains(&"claude".to_string()));
        assert!(!all.contains(&"opencode".to_string()));
        let named = wanted(&["opencode".to_string()]).unwrap();
        assert_eq!(named.len(), 1);
        assert!(named[0].experimental);
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
        let opencode = target("opencode");
        assert_eq!(status_of(home.path(), &claude), "not installed");
        install_into(home.path(), &claude).expect("install");
        assert_eq!(status_of(home.path(), &claude), "installed");
        std::fs::write(path_of(home.path(), &claude), "stale").expect("write");
        assert!(
            status_of(home.path(), &claude).contains("older than this build"),
            "a stale file reads as older"
        );
        // Present on disk yet never installed: nothing proves it is read.
        install_into(home.path(), &opencode).expect("install");
        assert!(
            status_of(home.path(), &opencode).contains("experimental, unverified"),
            "an experimental file must not read as installed"
        );
    }
}

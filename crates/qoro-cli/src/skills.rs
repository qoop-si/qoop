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

/// qass, the assistant over every other agent: installed beside each agent's qoro skill
/// (`.claude/skills/qass`, `.codex/skills/qass`, ...), as files that sit beside each other
/// because the scripts find one another by their own directory. Its rules are in its
/// SKILL.md; nothing beside them holds it back.
fn qass_dir(target: &Target) -> PathBuf {
    Path::new(target.dir).with_file_name("qass")
}

/// Where qass keeps its own state and works (`data`), and the owner's files (`owner`), as
/// `qoro paths` shows them. The skill's files name them as `@QASS_DATA@` and `@QASS_CONFIG@`,
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
            // Seen read on 2026-10-03 (opencode 1.18.34): `opencode run` loaded a skill
            // placed there.
            experimental: false,
        },
    ]
}

/// Whether qass's skill sits where this agent reads skills.
pub(crate) fn qass_installed_for(agent: &str) -> bool {
    let (Some(home), Some(target)) = (home(), targets().into_iter().find(|t| t.agent == agent))
    else {
        return false;
    };
    home.join(qass_dir(&target)).join(FILE).exists()
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
fn install_qass(home: &Path, dirs: &QassDirs, target: &Target) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join(qass_dir(target));
    std::fs::create_dir_all(&dir)?;
    let mut changed = migrate_old_qass(home, dirs)?;
    for gone in QASS_GONE {
        changed |= std::fs::remove_file(dir.join(gone)).is_ok();
    }
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
/// owner's policy and lessons stay; the working folder's Claude settings go with Claude's.
fn remove_qass(home: &Path, dirs: &QassDirs, target: &Target) -> std::io::Result<bool> {
    let dir = home.join(qass_dir(target));
    let mut removed = false;
    for (name, _, _) in QASS {
        removed |= std::fs::remove_file(dir.join(name)).is_ok();
    }
    let folder = dirs.folder_settings();
    if target.agent == "claude" {
        removed |= std::fs::remove_file(&folder).is_ok();
    }
    for empty in [folder.parent().expect("has a parent"), dir.as_path()] {
        if std::fs::read_dir(empty).is_ok_and(|mut e| e.next().is_none()) {
            let _ = std::fs::remove_dir(empty);
        }
    }
    Ok(removed)
}

fn qass_current(home: &Path, dirs: &QassDirs, target: &Target) -> bool {
    let dir = home.join(qass_dir(target));
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
    let dirs = QassDirs::here()?;
    for target in targets() {
        let qass = match (
            home.join(qass_dir(&target)).exists(),
            qass_current(&home, &dirs, &target),
        ) {
            (false, _) => "no qass",
            (true, false) => "qass older than this build: run install again",
            (true, true) => "with qass",
        };
        println!(
            "  {:<10} {}; {qass}",
            target.agent,
            status_of(&home, &target)
        );
    }
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
        if install_qass(&home, &QassDirs::here()?, &target)? {
            println!(
                "  {:<10} qass installed into {}",
                target.agent,
                qass_dir(&target).display()
            );
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
        if remove_qass(&home, &QassDirs::here()?, &target)? {
            println!(
                "  {:<10} qass removed from {}",
                target.agent,
                qass_dir(&target).display()
            );
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
        let own = include_str!("../../../.qoro/workflow.md");
        for (what, text) in [("the default", default), ("qoro's own", own)] {
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
        assert!(install_qass(home.path(), &dirs, &target("claude")).expect("install"));
        assert!(qass_current(home.path(), &dirs, &target("claude")));
        // Every place name is filled in.
        for (name, _, _) in QASS {
            let body =
                std::fs::read_to_string(home.path().join(qass_dir(&target("claude"))).join(name))
                    .expect("file");
            assert!(!body.contains("@QASS_"), "{name} still names a placeholder");
        }
        // Its folder loads the lessons, so a restored or woken qass still has them.
        let folder = std::fs::read_to_string(dirs.folder_settings()).expect("folder settings");
        assert!(folder.contains(&dirs.owner.display().to_string()));
        let watch = home
            .path()
            .join(qass_dir(&target("claude")))
            .join("watch.sh");
        let mode = std::fs::metadata(&watch)
            .expect("watch")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "watch.sh is not runnable");
        assert!(
            !install_qass(home.path(), &dirs, &target("claude")).expect("reinstall"),
            "the second install changed something"
        );
        let theirs = home
            .path()
            .join(qass_dir(&target("claude")))
            .join("notes.txt");
        std::fs::write(&theirs, "theirs").expect("write");
        assert!(remove_qass(home.path(), &dirs, &target("claude")).expect("uninstall"));
        assert_eq!(std::fs::read_to_string(&theirs).expect("read"), "theirs");
        std::fs::remove_file(&theirs).expect("rm");
        assert!(!remove_qass(home.path(), &dirs, &target("claude")).expect("uninstall again"));
        assert!(!home.path().join(qass_dir(&target("claude"))).exists());
        assert!(!dirs.folder_settings().exists());
    }

    /// What an older install wrote and qass no longer has goes at the next install.
    #[test]
    fn an_older_install_loses_its_gate() {
        let home = home_dir();
        let dirs = QassDirs {
            data: home.path().join("data/qass"),
            owner: home.path().join("config/qass"),
        };
        let dir = home.path().join(qass_dir(&target("claude")));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("gate.py"), "old").expect("old gate");
        assert!(install_qass(home.path(), &dirs, &target("claude")).expect("install"));
        assert!(!dir.join("gate.py").exists());
    }

    /// Every agent gets qass beside its qoro skill; taking it from one leaves the others
    /// and Claude's working-folder settings.
    #[test]
    fn qass_installs_beside_every_agent() {
        let home = home_dir();
        let dirs = QassDirs {
            data: home.path().join("data/qass"),
            owner: home.path().join("config/qass"),
        };
        for agent in ["claude", "codex", "gemini", "opencode"] {
            install_qass(home.path(), &dirs, &target(agent)).expect("install");
            assert!(qass_current(home.path(), &dirs, &target(agent)), "{agent}");
        }
        assert!(home.path().join(".codex/skills/qass/SKILL.md").exists());
        assert!(
            home.path()
                .join(".config/opencode/skills/qass/SKILL.md")
                .exists()
        );
        assert!(remove_qass(home.path(), &dirs, &target("codex")).expect("uninstall"));
        assert!(!home.path().join(".codex/skills/qass").exists());
        assert!(
            dirs.folder_settings().exists(),
            "claude's settings went with codex"
        );
        assert!(qass_current(home.path(), &dirs, &target("claude")));
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
        install_qass(home.path(), &dirs, &target("claude")).expect("install");
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
        install_qass(home.path(), &dirs, &target("claude")).expect("again");
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
            dir: ".unproven/skills/qoro",
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

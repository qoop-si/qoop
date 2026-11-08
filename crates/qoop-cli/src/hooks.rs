//! Installing the hooks that let an agent say what it is doing.
//!
//! An agent's own hook is the difference between reading a screen and being told. It is
//! also the only way an agent's id for its conversation reaches this daemon, which is what
//! makes reopening a session come back to that conversation rather than to whichever one in
//! the directory is newest.
//!
//! Every hook shipped here is a file with instructions in its own header, and a person can
//! follow them by hand. This is the same thing done for them: the script is written into a
//! directory of its own, and one entry is added to that agent's configuration.
//!
//! Three rules, and they are the whole design:
//!
//! * Added beside what is already there, never instead of it. A person's own hooks are
//!   theirs, and an installer that replaced them would be a data loss bug that looks like
//!   a feature.
//! * Installing twice changes nothing the second time, so this is safe to run from a
//!   script or after an upgrade.
//! * Uninstalling removes exactly what was added and nothing else, which is only possible
//!   because every entry is recognised by the path it points at.
//!
//! The file is copied before it is written, every time, because this edits a configuration
//! a person did not write and may care about more than they care about qoop.

use std::path::{Path, PathBuf};

/// One agent's hook, and where it has to be registered.
struct Target {
    /// What a person calls the agent, and what they type to install it.
    agent: &'static str,
    /// The script or plugin, compiled in so the binary needs nothing beside it.
    body: &'static str,
    /// What the installed file is called.
    file: &'static str,
    /// That agent's configuration, under the person's home directory.
    config: &'static str,
    /// The variable that moves the agent's configuration directory (the first component
    /// of `config`) somewhere else, when the agent has one. An install that ignored it
    /// would write to a file that agent never reads.
    home_env: Option<&'static str>,
    /// A file beside `config` that the agent reads instead of it whenever it exists, with
    /// the event map at its root rather than under `hooks`. Factory Droid's: it reads
    /// `~/.factory/hooks.json` when there is one and only falls back to the `hooks` key of
    /// `settings.json` when there is not, so an entry written to the file it ignores would
    /// be an install that never runs.
    instead: Option<&'static str>,
    shape: Shape,
}

/// How an agent's configuration says "run this".
enum Shape {
    /// `hooks.<event>[] = { matcher?, hooks: [{ type: "command", command }] }`, which is
    /// what Claude Code, Codex, Gemini and Goose all read, with a different event list
    /// each and a matcher only where that agent has one.
    Nested {
        events: &'static [Event],
        matcher: Option<&'static str>,
    },
    /// `hooks.<event>[] = { command }`, with no wrapper object. Cursor's, whose file also
    /// carries a `version` of its own schema (`versioned`), and Copilot's inline settings
    /// block, which has none because it lives inside a settings file.
    Flat {
        events: &'static [Event],
        versioned: bool,
    },
    /// A directory the agent loads everything out of. Dropping the file in is the whole
    /// installation, and there is no configuration to edit: opencode reads every plugin in
    /// `~/.config/opencode/plugin`, which is also how anything else a person has put there
    /// is loaded. Writing a `plugin` array into a config instead would have created a
    /// second config file beside the one they already keep, and opencode reads one of them.
    Dropped,
}

/// One lifecycle event, and the state to report when it fires.
///
/// The argument is `None` where the hook reads the event out of the payload on stdin and
/// decides for itself, which is what Claude Code's does.
struct Event(&'static str, Option<&'static str>);

/// Every agent this can install for.
///
/// An agent whose hook is shipped but whose configuration is not edited here is absent on
/// purpose: an installer that half understands a format writes a file that agent then
/// refuses to start with, and telling a person to do it by hand costs them a minute.
fn targets() -> Vec<Target> {
    vec![
        Target {
            agent: "claude",
            body: include_str!("../../../hooks/claude/qoop-hook.sh"),
            file: "qoop-hook.sh",
            config: ".claude/settings.json",
            instead: None,
            home_env: None,
            shape: Shape::Nested {
                // Claude's hook reads the event name from its payload, so every entry is
                // the same command and the mapping lives in the script.
                events: &[
                    Event("SessionStart", None),
                    Event("UserPromptSubmit", None),
                    Event("PreToolUse", None),
                    Event("PostToolUse", None),
                    Event("Notification", None),
                    Event("PermissionRequest", None),
                    Event("SubagentStart", None),
                    Event("SubagentStop", None),
                    Event("Stop", None),
                    Event("StopFailure", None),
                    Event("SessionEnd", None),
                ],
                matcher: Some(""),
            },
        },
        Target {
            agent: "codex",
            body: include_str!("../../../hooks/codex/qoop-hook.sh"),
            file: "qoop-codex-hook.sh",
            config: ".codex/hooks.json",
            instead: None,
            home_env: None,
            shape: Shape::Nested {
                // Codex runs a hook per event, so which state each event means is an
                // argument here; the script reads the event name only to label the
                // focus event.
                events: &[
                    Event("SessionStart", Some("starting")),
                    Event("UserPromptSubmit", Some("working")),
                    Event("PreToolUse", Some("working")),
                    Event("PostToolUse", Some("working")),
                    Event("PermissionRequest", Some("blocked")),
                    Event("Stop", Some("idle")),
                    // `subagent` is no state: the script counts the sub-agent beside the tab.
                    Event("SubagentStart", Some("subagent")),
                    Event("SubagentStop", Some("subagent")),
                ],
                matcher: None,
            },
        },
        Target {
            agent: "gemini",
            body: include_str!("../../../hooks/gemini/qoop-hook.sh"),
            file: "qoop-gemini-hook.sh",
            config: ".gemini/settings.json",
            instead: None,
            home_env: None,
            shape: Shape::Nested {
                events: &[
                    Event("SessionStart", Some("starting")),
                    Event("BeforeAgent", Some("working")),
                    Event("BeforeTool", Some("working")),
                    Event("AfterTool", Some("subagent")),
                    Event("AfterAgent", Some("idle")),
                    Event("SessionEnd", Some("idle")),
                ],
                matcher: Some("*"),
            },
        },
        Target {
            agent: "cursor",
            body: include_str!("../../../hooks/cursor/qoop-hook.sh"),
            file: "qoop-cursor-hook.sh",
            config: ".cursor/hooks.json",
            instead: None,
            home_env: None,
            shape: Shape::Flat {
                events: &[
                    Event("sessionStart", Some("starting")),
                    Event("postToolUse", Some("working")),
                    Event("stop", Some("idle")),
                ],
                versioned: true,
            },
        },
        Target {
            agent: "opencode",
            body: include_str!("../../../hooks/opencode/qoop-plugin.js"),
            file: "qoop-plugin.js",
            config: ".config/opencode/plugin",
            instead: None,
            home_env: None,
            shape: Shape::Dropped,
        },
        // Session-only hooks: one shared script that reports the agent's own id for its
        // conversation on SessionStart and nothing else, so a restored tab resumes that
        // conversation. State for these agents stays with the screen.
        //
        // Copilot reads an inline `hooks` block in its user settings. The PascalCase event
        // name is the one whose payload says `session_id`, and `command` is its documented
        // cross-platform field.
        Target {
            agent: "copilot",
            body: SESSION_HOOK,
            file: "qoop-copilot-session.sh",
            config: ".copilot/settings.json",
            instead: None,
            home_env: Some("COPILOT_HOME"),
            shape: Shape::Flat {
                events: &[Event("SessionStart", None)],
                versioned: false,
            },
        },
        Target {
            agent: "droid",
            body: SESSION_HOOK,
            file: "qoop-droid-session.sh",
            config: ".factory/settings.json",
            instead: Some(".factory/hooks.json"),
            home_env: None,
            shape: Shape::Nested {
                events: &[Event("SessionStart", None)],
                matcher: None,
            },
        },
        Target {
            agent: "qwen",
            body: SESSION_HOOK,
            file: "qoop-qwen-session.sh",
            config: ".qwen/settings.json",
            instead: None,
            home_env: Some("QWEN_HOME"),
            shape: Shape::Nested {
                events: &[Event("SessionStart", None)],
                matcher: Some("*"),
            },
        },
        Target {
            agent: "qodercli",
            body: SESSION_HOOK,
            file: "qoop-qodercli-session.sh",
            config: ".qoder/settings.json",
            instead: None,
            home_env: Some("QODER_CONFIG_DIR"),
            shape: Shape::Nested {
                events: &[Event("SessionStart", None)],
                matcher: Some("*"),
            },
        },
    ]
}

/// The session-only hook, installed once per agent that uses it.
const SESSION_HOOK: &str = include_str!("../../../hooks/session/qoop-session-hook.sh");

/// The file this agent reads its hooks from, and whether the event map sits at its root.
///
/// Where an install writes. Droid's choice is made when this is called, so a file that
/// appears later can hold an entry this wrote into the other one: status and uninstall
/// look in both, through [`configs_of`].
fn config_of(target: &Target, home: &Path) -> (PathBuf, bool) {
    match target.instead.map(|file| home.join(file)) {
        Some(file) if file.exists() => (file, true),
        _ => (config_path(target, home), false),
    }
}

/// Every file an entry of this agent's could be in, each with where its event map sits.
fn configs_of(target: &Target, home: &Path) -> Vec<(PathBuf, bool)> {
    let mut files = vec![(config_path(target, home), false)];
    files.extend(target.instead.map(|file| (home.join(file), true)));
    files
}

/// The agent's own configuration file, under its home variable when the person set one.
fn config_path(target: &Target, home: &Path) -> PathBuf {
    let moved = target
        .home_env
        .and_then(std::env::var_os)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    config_path_in(target, home, moved)
}

/// The same, with the variable's value given, so a test need not set it for the process.
///
/// A leading `~` is the home directory, as a shell would have expanded it: a value set in a
/// file the shell never read arrives with it still in place.
fn config_path_in(target: &Target, home: &Path, moved: Option<PathBuf>) -> PathBuf {
    let moved = moved.map(|dir| match dir.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => dir,
    });
    match moved {
        Some(dir) => dir.join(
            target
                .config
                .split_once('/')
                .map_or(target.config, |(_, rest)| rest),
        ),
        None => home.join(target.config),
    }
}

/// Whether a person has this agent: its binary on `PATH` or in a folder agents install into,
/// or its configuration directory.
///
/// An install that names no agent installs only for these, rather than creating a
/// directory for every agent qoop knows of on a machine that has two of them.
fn present(target: &Target, home: &Path) -> bool {
    present_in(
        target,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        &agent_dirs(home),
    )
}

/// The same, with the search path and the install folders given.
fn present_in(target: &Target, home: &Path, path: &str, dirs: &[PathBuf]) -> bool {
    on_path_in(target.agent, path)
        || dirs
            .iter()
            .any(|dir| runnable(&dir.join(binary(target.agent))))
        || config_path(target, home)
            .parent()
            .is_some_and(|dir| dir.is_dir())
}

/// The folders people install agents into, looked at beside `PATH`: a daemon started by
/// launchd, systemd or ssh has a bare `PATH` that misses every one of them, and so did the
/// install a new ssh computer gets, which skipped a `claude` sitting in `~/.local/bin`.
///
/// ponytail: a fixed list; a login shell's `PATH` if a real install folder turns up outside it.
pub(crate) fn agent_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        ".local/bin",
        ".npm-global/bin",
        ".bun/bin",
        ".volta/bin",
        ".claude/local",
        ".opencode/bin",
        ".local/share/fnm/aliases/default/bin",
        ".local/share/mise/shims",
        ".asdf/shims",
    ]
    .iter()
    .map(|dir| home.join(dir))
    .collect();
    if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        dirs.extend(versions.flatten().map(|v| v.path().join("bin")));
    }
    dirs.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ]
        .map(PathBuf::from),
    );
    dirs
}

/// An executable file, through any link: what a package manager leaves in its bin folder.
fn runnable(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether the agent named is on this machine, for `skills install` to skip the ones that
/// are not, the way `hooks install` does.
pub fn agent_present(agent: &str) -> bool {
    home().is_some_and(|home| {
        let path = std::env::var("PATH").unwrap_or_default();
        present_with(agent, &home, &path, &agent_dirs(&home))
    })
}

/// The same, with the home, the search path and the install folders given.
pub(crate) fn present_with(agent: &str, home: &Path, path: &str, dirs: &[PathBuf]) -> bool {
    targets()
        .iter()
        .any(|t| t.agent == agent && present_in(t, home, path, dirs))
}

/// Held across every change to agents' hooks and skills and to the lists below, by every
/// `install` and `uninstall` and by the daemon's look, so a removal and a look never interleave
/// and two writers never share a half-written file. `flock`, released when the holder exits.
pub(crate) fn setup_lock(config: &Path) -> std::io::Result<qoop_runtime::lock::StartupLock> {
    std::fs::create_dir_all(config)?;
    qoop_runtime::lock::StartupLock::acquire(&config.join("setup.lock"))
}

/// The file that lists the agents whose qoop `kind` ("hooks" or "skills") a person removed on
/// purpose, one per line, beside the `no-auto-update` mark. An install that names no agent
/// (the install script, an update, a new ssh computer, the daemon's own look) skips them;
/// naming the agent takes it off the list.
pub(crate) fn off_list(config: &Path, kind: &str) -> PathBuf {
    config.join(format!("no-auto-{kind}"))
}

/// Whether this agent's `kind` was removed on purpose.
pub(crate) fn is_off(config: &Path, kind: &str, agent: &str) -> bool {
    std::fs::read_to_string(off_list(config, kind))
        .is_ok_and(|list| list.lines().any(|line| line.trim() == agent))
}

/// Put these agents on the list (`off`) or take them off it.
pub(crate) fn set_off(
    config: &Path,
    kind: &str,
    agents: &[&str],
    off: bool,
) -> std::io::Result<()> {
    let file = off_list(config, kind);
    let text = match std::fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        read => read?,
    };
    let mut list: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !agents.contains(line))
        .map(String::from)
        .collect();
    if off {
        list.extend(agents.iter().map(|a| a.to_string()));
    }
    let was: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if was == list {
        return Ok(());
    }
    if list.is_empty() {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(config)?;
    // Beside and renamed over, so a failed write never loses the list there was.
    let fresh = file.with_extension(format!("{}.new", std::process::id()));
    let written = std::fs::write(&fresh, list.join("\n") + "\n")
        .and_then(|()| std::fs::rename(&fresh, &file));
    if written.is_err() {
        let _ = std::fs::remove_file(&fresh);
    }
    written
}

/// Where the installed scripts live.
///
/// Beside the agent-detection overrides, under the person's own configuration, because
/// that is where a thing they can read and edit belongs. Not in the runtime directory:
/// that is for sockets and databases and is thrown away.
fn hooks_dir() -> PathBuf {
    qoop_runtime::paths::config_dir().join("hooks")
}

/// What the hook of this agent was called before the rename.
///
/// ponytail: with `legacy`, goes when no agent started before the rename is running. Until
/// then the old script stays on disk (an agent that is already running keeps calling it),
/// its entries are taken out of the agent's configuration, and the new ones put in.
fn old_file(target: &Target) -> String {
    target.file.replacen("qoop", "qoro", 1)
}

/// Where the script of the old name was registered from: under the old configuration
/// directory, which is how an agent's file spells it.
fn old_script(target: &Target) -> PathBuf {
    qoop_runtime::paths::config_dir()
        .with_file_name("qoro")
        .join("hooks")
        .join(old_file(target))
}

/// Refuse a hooks directory that a hook command cannot carry as it is.
///
/// Refused rather than quoted, because quoting is only right if every agent runs the
/// command through a shell. Claude does; Cursor documents command lines but not how it
/// splits them, and an agent that splits on whitespace would receive the quote characters
/// and break every hook for everybody. Only whitespace and what a shell reads as syntax are
/// refused: an `@` in a directory-joined Linux home, or a non-ASCII name, is carried fine.
fn safe_hook_dir(dir: &Path) -> std::io::Result<()> {
    let special =
        |c: char| c.is_whitespace() || c.is_control() || "'\"\\$`;&|<>()*?[]{}!#~".contains(c);
    if dir.to_str().is_some_and(|text| !text.chars().any(special)) {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "{} has a character a hook command cannot carry; set XDG_CONFIG_HOME to a plain path",
        dir.display()
    )))
}

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Whether this agent's hook is registered where that agent will read it.
fn registered(target: &Target, dir: &Path, home: &Path) -> bool {
    if matches!(target.shape, Shape::Dropped) {
        return home.join(target.config).join(target.file).exists();
    }
    let script = dir.join(target.file).display().to_string();
    configs_of(target, home).iter().any(|(config, _)| {
        std::fs::read_to_string(config)
            .map(|text| text.contains(&script))
            .unwrap_or(false)
    })
}

/// Whether a person has this agent at all.
///
/// Read from `PATH` directly rather than by running anything: asking six agents to start is
/// six processes on every first start, and what is wanted is whether the file is there.
pub(crate) fn on_path(agent: &str) -> bool {
    match std::env::var("PATH") {
        Ok(path) => on_path_in(agent, &path),
        Err(_) => false,
    }
}

/// The same question, asked of a given path.
///
/// Split out so a test can ask it without setting `PATH` for the whole process: a test
/// that changes one process-wide thing is a test that answers somebody else's question
/// wrongly, and the parallel runner makes it look like a flake.
fn on_path_in(agent: &str, path: &str) -> bool {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| Path::new(dir).join(binary(agent)).exists())
}

/// The name a person types, which is not always the name of the binary.
fn binary(agent: &str) -> &str {
    match agent {
        "cursor" => "cursor-agent",
        other => other,
    }
}

/// `qoop hooks ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("install") => install(&args[3..]),
        Some("uninstall") => uninstall(&args[3..]),
        Some("status") | None => status(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; usage: qoop hooks <install|uninstall|status> [agent...]"
        ))),
    }
}

/// Which agents a command is about: the ones named, or every one there is.
fn wanted(args: &[String]) -> std::io::Result<Vec<Target>> {
    if args.is_empty() {
        return Ok(targets());
    }
    let mut out = Vec::new();
    for name in args {
        let Some(target) = targets().into_iter().find(|t| t.agent == name.as_str()) else {
            let known: Vec<&str> = targets().iter().map(|t| t.agent).collect();
            return Err(std::io::Error::other(format!(
                "no hook for {name}; qoop ships one for {}",
                known.join(", ")
            )));
        };
        out.push(target);
    }
    Ok(out)
}

fn status() -> std::io::Result<()> {
    let dir = hooks_dir();
    let home = home();
    for target in targets() {
        let installed = dir.join(target.file);
        let current = std::fs::read_to_string(&installed)
            .map(|on_disk| on_disk == target.body)
            .unwrap_or(false);
        let (registered, current) = if matches!(target.shape, Shape::Dropped) {
            // The file being in the directory is the whole of it.
            let dropped = home
                .as_ref()
                .map(|h| h.join(target.config).join(target.file));
            let body = dropped.and_then(|p| std::fs::read_to_string(p).ok());
            (body.is_some(), body.as_deref() == Some(target.body))
        } else {
            let registered = home.as_ref().is_some_and(|h| registered(&target, &dir, h));
            (registered, current)
        };
        let here = home.as_ref().is_some_and(|h| present(&target, h));
        let off = is_off(&qoop_runtime::paths::config_dir(), "hooks", target.agent);
        let state = match (registered, current) {
            (false, _) if off => {
                "off, removed on purpose: `qoop hooks install <agent>` brings it back"
            }
            (false, _) if !here => "not present",
            (false, _) => "not installed",
            (true, false) => "installed, and older than this build: run install again",
            (true, true) => "installed",
        };
        println!("  {:<10} {state}", target.agent);
    }
    println!("\n  qoop hooks install [agent...]    add them, beside whatever is already there");
    println!("  qoop hooks uninstall [agent...]  remove exactly what was added");
    Ok(())
}

fn install(args: &[String]) -> std::io::Result<()> {
    let dir = hooks_dir();
    safe_hook_dir(&dir)?;
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to install into"));
    };
    let config_dir = qoop_runtime::paths::config_dir();
    let wanted = wanted(args)?;
    let _held = setup_lock(&config_dir)?;
    // Named, so wanted again: off the list of the ones removed on purpose.
    if !args.is_empty() {
        let names: Vec<&str> = wanted.iter().map(|t| t.agent).collect();
        set_off(&config_dir, "hooks", &names, false)?;
    }
    // Headed: the install script and an update print this list and the skills' one back to
    // back, the same agents in each.
    println!("hooks, which tell qoop what each agent is doing:");
    let mut any = false;
    for target in wanted {
        // Asked for every agent, only the ones on this machine: an agent named on the
        // command line is installed whether or not it is here yet.
        if args.is_empty() && !present(&target, &home) {
            println!("  {:<10} not present, skipped", target.agent);
            continue;
        }
        if args.is_empty() && is_off(&config_dir, "hooks", target.agent) {
            println!(
                "  {:<10} off, removed on purpose: `qoop hooks install {}` brings it back",
                target.agent, target.agent
            );
            continue;
        }
        any = true;
        // A script that cannot be written stops the command, as it always has: the install
        // script and an update read the exit status.
        let edited = install_one(&target, &home, &dir)?;
        // Dropped hooks have no configuration to edit: the file itself goes
        // into the agent's plugin directory.
        if matches!(target.shape, Shape::Dropped) {
            match edited {
                Ok(true) => println!("  {:<10} installed into {}", target.agent, target.config),
                Ok(false) => println!("  {:<10} already installed", target.agent),
                Err(e) => println!("  {:<10} {} not written: {e}", target.agent, target.config),
            }
            continue;
        }
        let (config, _) = config_of(&target, &home);
        let shown = config.strip_prefix(&home).unwrap_or(&config).display();
        // The entries of the script it replaces, so an agent does not report twice.
        let mut replaced_old = false;
        // Only once the new entry is in: an agent keeps its old hook rather than none.
        let cleanup = if edited.is_ok() && qoop_runtime::legacy::migrating() {
            remove_everywhere(&target, &home, &old_script(&target), false)
        } else {
            Vec::new()
        };
        for (file, outcome) in cleanup {
            match outcome {
                Ok(changed) => replaced_old |= changed,
                Err(e) => println!("  {:<10} {} not cleaned: {e}", target.agent, file.display()),
            }
        }
        match edited {
            Ok(true) => println!("  {:<10} installed, and {shown} updated", target.agent),
            Ok(false) => println!("  {:<10} already installed", target.agent),
            Err(e) => println!("  {:<10} {shown} not set up: {e}", target.agent),
        }
        // Codex skips a hook it has not been told to trust, without a word: measured, the
        // hooks were installed and none of them ran. Trusting is the person's to do.
        if target.agent == "codex" {
            println!(
                "  {:<10} Codex runs these hooks only once trusted: start Codex and pick \
                 \"Trust all\" when it lists them, or use /hooks",
                ""
            );
        }
        // The hook of before the rename was trusted by its path; this one is a new path, so
        // new Codex sessions run without hooks until it is trusted once.
        if target.agent == "codex" && replaced_old {
            println!(
                "  {:<10} the hook moved to a new path, which Codex has not trusted yet: until \
                 you pick \"Trust all\" (or /hooks) in a new Codex session, it runs without",
                ""
            );
        }
        // Said, never changed for the person: it widens what Codex may reach. Quiet once set.
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let reachable = std::fs::read_to_string(codex_home.join("config.toml")).is_ok_and(|c| {
            c.lines()
                .any(|l| l.replace(' ', "") == "network_access=true")
        });
        if target.agent == "codex" && !reachable {
            println!(
                "  {:<10} its workspace-write sandbox keeps qoop's socket out of reach; set \
                 sandbox_workspace_write.network_access = true in ~/.codex/config.toml, or run \
                 it with full access, for qoop commands from inside Codex",
                ""
            );
        }
    }
    if any {
        println!(
            "\nan agent already running does not read its configuration again: start it afresh."
        );
    } else {
        // A new computer often has qoop before its agent: the daemon's own look picks it up.
        println!(
            "\nno agent on this machine yet: qoop sets up its hook and skill by itself once one \
             is installed"
        );
    }
    Ok(())
}

/// Put one agent's hook in place: its script, then its entry in the agent's configuration.
/// The outer error is the script (or a dropped plugin's directory), which stops an install;
/// the inner one the agent's configuration, which an install reports and goes on past. True
/// when the entry (or a dropped plugin) was added; the script alone does not count.
///
/// Dropped hooks have no configuration to edit: the file itself goes into the agent's plugin
/// directory. The file is qoop's own and the person never edits it, so missing or different
/// means install.
fn install_one(target: &Target, home: &Path, dir: &Path) -> std::io::Result<std::io::Result<bool>> {
    if matches!(target.shape, Shape::Dropped) {
        return Ok(install_dropped(home, target));
    }
    // Made here rather than up front, so an install that skips every agent leaves
    // nothing behind.
    std::fs::create_dir_all(dir)?;
    let script = dir.join(target.file);
    write_script(&script, target.body)?;
    let (config, at_root) = config_of(target, home);
    Ok(edit(&config, target, &script, at_root))
}

/// Write a hook's script unless it already holds `body`, beside and renamed over, so an agent
/// running the old one while this writes reads it whole, and a failure never leaves it cut.
fn write_script(script: &Path, body: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let runs = script.extension().is_some_and(|e| e == "sh");
    let mode = |file: &Path| match runs {
        true => std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)),
        false => Ok(()),
    };
    if std::fs::read_to_string(script).ok().as_deref() == Some(body) {
        let current = std::fs::metadata(script)?.permissions().mode() & 0o777;
        return match !runs || current == 0o700 {
            true => Ok(()),
            false => mode(script),
        };
    }
    // Named for this process, so two writers never share one, and made runnable before it
    // takes the script's name.
    let name = script.file_name().unwrap_or_default().to_string_lossy();
    let fresh = script.with_file_name(format!(".{name}.{}.new", std::process::id()));
    let written = std::fs::write(&fresh, body)
        .and_then(|()| mode(&fresh))
        .and_then(|()| std::fs::rename(&fresh, script));
    if written.is_err() {
        let _ = std::fs::remove_file(&fresh);
    }
    written
}

/// Set up the hook of every agent on this machine that has none, for the daemon's own look.
/// Each agent it tried, with whether its entry was added. A hook that is there but older is
/// left to `qoop hooks install` and the update: a daemon outlives the binary it came from,
/// and an old one must not write its old script over a new install.
pub(crate) fn keep(
    home: &Path,
    config: &Path,
    path: &str,
    dirs: &[PathBuf],
) -> Vec<(&'static str, std::io::Result<bool>)> {
    let dir = config.join("hooks");
    if safe_hook_dir(&dir).is_err() {
        return Vec::new();
    }
    targets()
        .into_iter()
        .filter(|t| present_in(t, home, path, dirs))
        .filter(|t| !set_up(t, &dir, home))
        // Read last, right before the write, so an uninstall racing this look wins.
        .filter(|t| !is_off(config, "hooks", t.agent))
        .map(|t| {
            (
                t.agent,
                install_one(&t, home, &dir).and_then(|edited| edited),
            )
        })
        .collect()
}

/// Whether this agent's hook is registered and its script is there to run.
fn set_up(target: &Target, dir: &Path, home: &Path) -> bool {
    registered(target, dir, home)
        && (matches!(target.shape, Shape::Dropped) || dir.join(target.file).exists())
}

/// Drop a Dropped hook's file into the agent's plugin directory.
///
/// True when written, false when the file on disk already carries this
/// build's body. Missing parents are made. Before this existed, install
/// wrote the script to qoop's own hooks directory and then failed editing
/// the plugin *directory* as a file, so `install opencode` reported the
/// script written and installed nothing at all.
fn install_dropped(home: &Path, target: &Target) -> std::io::Result<bool> {
    let dropped = home.join(target.config).join(target.file);
    // The plugin it replaces: the agent loads every file in the directory, so both would run.
    if qoop_runtime::legacy::migrating() {
        let _ = std::fs::remove_file(home.join(target.config).join(old_file(target)));
    }
    if std::fs::read_to_string(&dropped).ok().as_deref() == Some(target.body) {
        return Ok(false);
    }
    if let Some(parent) = dropped.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&dropped, target.body)?;
    Ok(true)
}

fn uninstall(args: &[String]) -> std::io::Result<()> {
    let dir = hooks_dir();
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to uninstall from"));
    };
    let wanted = wanted(args)?;
    let config_dir = qoop_runtime::paths::config_dir();
    let _held = setup_lock(&config_dir)?;
    // Listed before anything is removed, so no install that names no agent puts it back. Asked
    // for every agent, only the ones here or set up: one installed next month is still wanted.
    let names: Vec<&str> = wanted
        .iter()
        .filter(|t| !args.is_empty() || present(t, &home) || registered(t, &dir, &home))
        .map(|t| t.agent)
        .collect();
    set_off(&config_dir, "hooks", &names, true)?;
    for target in wanted {
        let script = dir.join(target.file);
        if matches!(target.shape, Shape::Dropped) {
            let dropped = home.join(target.config).join(target.file);
            if qoop_runtime::legacy::migrating() {
                let _ = std::fs::remove_file(home.join(target.config).join(old_file(&target)));
            }
            match std::fs::remove_file(&dropped) {
                Ok(()) => println!("  {:<10} removed from {}", target.agent, target.config),
                Err(_) => println!("  {:<10} was not there", target.agent),
            }
            continue;
        }
        let mut removed = false;
        let mut failed = false;
        if qoop_runtime::legacy::migrating() {
            let old = old_script(&target);
            let _ = remove_everywhere(&target, &home, &old, false);
            let _ = std::fs::remove_file(&old);
        }
        for (config, outcome) in remove_everywhere(&target, &home, &script, true) {
            let shown = config.strip_prefix(&home).unwrap_or(&config).display();
            match outcome {
                Ok(true) => {
                    removed = true;
                    println!("  {:<10} removed from {shown}", target.agent);
                }
                Ok(false) => {}
                Err(e) => {
                    failed = true;
                    println!("  {:<10} {shown} not edited: {e}", target.agent);
                }
            }
        }
        if !removed {
            println!("  {:<10} was not there", target.agent);
        }
        let _ = std::fs::remove_file(&script);
        // The permissions went with the hooks, in the same rewrite; kept while a file could
        // not be edited, so a second uninstall still knows they are ours.
        if target.agent == "claude" && !failed {
            let _ = std::fs::remove_file(dir.join(ALLOW_MARKER));
        }
    }
    Ok(())
}

/// Take this hook out of every file an entry of it could be in, not only the one an
/// install would pick now: Droid's pick changes when its `hooks.json` appears, and an entry
/// left behind in the other file would point at a script uninstall deletes.
fn remove_everywhere(
    target: &Target,
    home: &Path,
    script: &Path,
    rules: bool,
) -> Vec<(PathBuf, std::io::Result<bool>)> {
    configs_of(target, home)
        .into_iter()
        .map(|(config, at_root)| {
            let outcome = remove(&config, script, at_root, rules);
            (config, outcome)
        })
        .collect()
}

/// Read a configuration, or an empty object if there is not one yet.
fn read(config: &Path) -> std::io::Result<serde_json::Value> {
    match std::fs::read_to_string(config) {
        Ok(text) if text.trim().is_empty() => Ok(serde_json::json!({})),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| std::io::Error::other(format!("it is not valid JSON: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(e),
    }
}

/// Write a configuration back, keeping a copy of what was there.
///
/// When the value being written is exactly what one of those copies holds, the copy's own
/// bytes go back rather than a fresh rendering of them. That is what makes an uninstall
/// return a person's file as they wrote it, indentation, key order and all, instead of a
/// reformatted file that means the same thing and shows up as a diff of every line.
///
/// The new content goes to a temporary file beside the real one and is renamed over it,
/// so a crash or a full disk never leaves half a settings file. A symlinked configuration
/// (a dotfiles checkout) is followed first, so the link survives and its target changes.
fn write(config: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Looked up before this write's own copy is taken, so that copy can never be the one
    // that is chosen and can never have overwritten the one that should be.
    let text = kept_copy_of(config, value)
        .map_or_else(|| serde_json::to_string_pretty(value).map(|t| t + "\n"), Ok)?;
    let real = link_target(config);
    // A file this process cannot write is the person saying no (read-only, or owned by
    // somebody else). The rename below asks only the directory, so it would replace the
    // file anyway; refuse, as a plain write would have, and before any copy is taken, so
    // a refused install leaves nothing behind. Probed by opening it for writing, without
    // truncating, because the permission bits alone do not say who may write.
    if real.exists()
        && let Err(e) = std::fs::OpenOptions::new().write(true).open(&real)
    {
        return Err(if e.kind() == std::io::ErrorKind::PermissionDenied {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "it is read-only")
        } else {
            e
        });
    }
    // A copy before every write, not only the first: the second write is the one that
    // happens after a person has spent an afternoon on the file. Beside the path the
    // person knows, not beside a link's target, which may sit in a dotfiles repository.
    if config.exists() {
        keep_copy(config)?;
    }
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = real
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = real.with_file_name(format!(".{name}.qoop-{}", std::process::id()));
    // The mode the file already has, when it exists. A new file gets whatever the umask
    // gives any new file, so a person with `umask 077` gets 0600 and not a mode of ours.
    let existing = std::fs::metadata(&real).ok().map(|m| m.permissions());
    // A leftover from a write that died half way would make the exclusive create fail.
    let _ = std::fs::remove_file(&temp);
    let written = (|| {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Created with the file's own mode, so a 0600 settings file holding a token is
        // never readable by others, not even for the moment before the rename.
        if let Some(permissions) = &existing {
            options.mode(permissions.mode() & 0o7777);
        }
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        // The umask may have narrowed the mode on create; the file keeps what it had.
        if let Some(permissions) = existing {
            file.set_permissions(permissions)?;
        }
        std::fs::rename(&temp, &real)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The file a configuration path names, following symlinks even when the last one
/// dangles, so the link survives a write and its target is what is created or changed.
fn link_target(config: &Path) -> PathBuf {
    let mut path = config.to_path_buf();
    // Bounded, so a symlink loop is a write to the last link rather than a hang.
    for _ in 0..40 {
        match std::fs::read_link(&path) {
            Ok(next) if next.is_absolute() => path = next,
            Ok(next) => {
                path = path
                    .parent()
                    .map_or_else(|| next.clone(), |parent| parent.join(&next))
            }
            Err(_) => break,
        }
    }
    path
}

/// What a kept copy is called after the file's stem.
const BACKUP: &str = "qoop-backup-";

/// How many of the newest kept copies stay, beside the oldest.
const KEPT_NEWEST: usize = 3;

/// Copy a configuration aside before it is changed, then prune the copies.
///
/// The oldest copy is always kept, because it is the file as the person had it before qoop
/// first touched it, which is what an uninstall gives back. Past that only the newest few
/// stay: a copy per write, unpruned, is a directory that fills up with them.
fn keep_copy(config: &Path) -> std::io::Result<()> {
    // Nanoseconds, so an install and an uninstall in the same second keep two copies
    // rather than the second overwriting the original.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::fs::copy(config, config.with_extension(format!("{BACKUP}{stamp}")))?;
    let mut copies = kept_copies(config);
    if copies.len() > KEPT_NEWEST + 1 {
        copies.sort_unstable_by_key(|c| c.0);
        let excess = copies.len() - KEPT_NEWEST - 1;
        for (_, path) in copies.drain(1..=excess) {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

/// Every kept copy of this file, with its stamp, in no particular order.
fn kept_copies(config: &Path) -> Vec<(u128, PathBuf)> {
    let (Some(stem), Some(parent)) = (config.file_stem(), config.parent()) else {
        return Vec::new();
    };
    let prefixes = [BACKUP, "qoro-backup-"].map(|b| format!("{}.{b}", stem.to_string_lossy()));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stamp = prefixes
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix.as_str()))?
                .parse()
                .ok()?;
            Some((stamp, entry.path()))
        })
        .collect()
}

/// The newest kept copy of this file whose content is `value`, as its original bytes.
fn kept_copy_of(config: &Path, value: &serde_json::Value) -> Option<String> {
    let mut copies = kept_copies(config);
    copies.sort_unstable_by_key(|c| std::cmp::Reverse(c.0));
    copies.into_iter().find_map(|(_, path)| {
        let text = std::fs::read_to_string(path).ok()?;
        // Read the way `read` reads a configuration: a blank file is an empty object.
        let parsed: serde_json::Value = if text.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&text).ok()?
        };
        (&parsed == value).then_some(text)
    })
}

/// Add this hook to a configuration. Answers whether anything changed.
///
/// `at_root` is a file whose event map is the whole file rather than its `hooks` key.
fn edit(config: &Path, target: &Target, script: &Path, at_root: bool) -> std::io::Result<bool> {
    let mut root = read(config)?;
    let path = script.display().to_string();
    let changed = match &target.shape {
        Shape::Nested { events, matcher } => {
            let hooks = if at_root {
                object_root(&mut root)
            } else {
                object(&mut root, "hooks")
            };
            let mut changed = false;
            for Event(event, argument) in events.iter() {
                let command = match argument {
                    Some(argument) => format!("{path} {argument}"),
                    None => path.clone(),
                };
                let list = array(hooks, event);
                if mentions(list, &path) {
                    continue;
                }
                let entry = serde_json::json!({ "type": "command", "command": command });
                // Into the group that matches everything, if there is one, so that one
                // event does not grow a second group saying the same thing.
                let existing = list
                    .iter_mut()
                    .find(|group| group.get("matcher").and_then(|m| m.as_str()) == *matcher);
                match existing {
                    Some(group) => {
                        array(group.as_object_mut().expect("a group"), "hooks").push(entry)
                    }
                    None => {
                        let mut group = serde_json::Map::new();
                        if let Some(matcher) = matcher {
                            group.insert("matcher".into(), serde_json::json!(matcher));
                        }
                        group.insert("hooks".into(), serde_json::json!([entry]));
                        list.push(serde_json::Value::Object(group));
                    }
                }
                changed = true;
            }
            changed
        }
        Shape::Flat { events, versioned } => {
            // Cursor's file carries a version of its own schema, and refuses one without.
            if *versioned && root.get("version").is_none() {
                object_root(&mut root).insert("version".into(), serde_json::json!(1));
            }
            let hooks = object(&mut root, "hooks");
            let mut changed = false;
            for Event(event, argument) in events.iter() {
                let command = match argument {
                    Some(argument) => format!("{path} {argument}"),
                    None => path.clone(),
                };
                let list = array(hooks, event);
                if mentions(list, &path) {
                    continue;
                }
                list.push(serde_json::json!({ "command": command }));
                changed = true;
            }
            changed
        }
        // Handled before this is ever called: there is no file to edit.
        Shape::Dropped => false,
    };
    let added = if target.agent == "claude" {
        allow_task(&mut root)
    } else {
        Vec::new()
    };
    let allowed = !added.is_empty();
    if changed || allowed {
        write(config, &root)?;
    }
    // Only once the rules are on disk: a marker for rules that never landed would let an
    // uninstall take away identical ones the person added later.
    // Exactly the rules this install added, with any an earlier one added: a rule the person
    // already had is not ours to take away.
    if allowed {
        let marker = script.with_file_name(ALLOW_MARKER);
        let mut ours = ours(&marker);
        ours.extend(added);
        std::fs::write(marker, ours.join("\n"))?;
    }
    Ok(changed || allowed)
}

/// The Claude permissions that let an agent run `qoop task` without asking each time: the
/// rules its hook gives it ask for several per task. Every subcommand an agent uses on its
/// own task and on the board item it works on, and not `close`, `add` or `start`, which end
/// somebody's task or put new work on the board and stay a question.
const ALLOW: [&str; 13] = [
    "Bash(qoop task new:*)",
    "Bash(qoop task stage:*)",
    "Bash(qoop task plan:*)",
    "Bash(qoop task evidence:*)",
    "Bash(qoop task done:*)",
    "Bash(qoop task show:*)",
    "Bash(qoop task ls:*)",
    "Bash(qoop task rules:*)",
    "Bash(qoop task goal:*)",
    "Bash(qoop task link:*)",
    "Bash(qoop task check:*)",
    "Bash(qoop task steps:*)",
    "Bash(qoop task describe:*)",
];

/// Rules the person may already have that cover all of `ALLOW`.
const COVERING: [&str; 4] = [
    "Bash(qoop task:*)",
    "Bash(qoop task *)",
    "Bash(qoop:*)",
    "Bash(qoop *)",
];

/// What says the permissions are ours: written beside the scripts once the install added
/// them, so an uninstall takes away only rules it put there, never ones the person had.
const ALLOW_MARKER: &str = "claude-allow-added";

/// Add `ALLOW` to Claude's `permissions.allow` unless a rule there already covers it.
/// Answers whether anything was added.
fn allow_task(root: &mut serde_json::Value) -> Vec<String> {
    let present: Vec<String> = root
        .pointer("/permissions/allow")
        .and_then(|a| a.as_array())
        .map(|rules| {
            rules
                .iter()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if present.iter().any(|r| COVERING.contains(&r.as_str())) {
        return Vec::new();
    }
    let missing: Vec<String> = ALLOW
        .iter()
        .filter(|rule| !present.iter().any(|r| r == *rule))
        .map(|rule| (*rule).to_string())
        .collect();
    if !missing.is_empty() {
        let allow = array(object(root, "permissions"), "allow");
        allow.extend(missing.iter().map(|rule| serde_json::json!(rule)));
    }
    missing
}

/// The rules the marker says an install added.
fn ours(marker: &Path) -> Vec<String> {
    std::fs::read_to_string(marker)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Take `ALLOW` back out of a settings value, and with it a list or a `permissions` object
/// it left empty. Called by `remove` only while the marker says the install added it, in the
/// same rewrite as the hooks, so a file the install created still goes away whole.
fn strip_allow(root: &mut serde_json::Value, ours: &[String]) -> bool {
    let Some(permissions) = root.get_mut("permissions").and_then(|p| p.as_object_mut()) else {
        return false;
    };
    let Some(allow) = permissions.get_mut("allow").and_then(|a| a.as_array_mut()) else {
        return false;
    };
    let before = allow.len();
    allow.retain(|r| !r.as_str().is_some_and(|r| ours.iter().any(|o| o == r)));
    if allow.len() == before {
        return false;
    }
    if allow.is_empty() {
        permissions.shift_remove("allow");
    }
    if permissions.is_empty() {
        root.as_object_mut().map(|o| o.shift_remove("permissions"));
    }
    true
}

/// Take this hook back out. Answers whether anything changed.
///
/// Recognised by the path it points at, which is why the script goes in a directory of its
/// own: an entry mentioning that path is one this put there, and nothing else in the file
/// can be mistaken for it. Only what this emptied is taken away with it: a group, an event
/// list, or the `hooks` object, left holding nothing because the entry was the last thing
/// in it. An empty group the person had stays, and so does an empty list for any other
/// event; one they had for the event this registered is taken away with the entry,
/// because nothing tells it apart from one this created. A file with no kept copy at all
/// was created by the install; when it holds nothing of the person's afterwards it is
/// removed rather than left as `{}`.
fn remove(config: &Path, script: &Path, at_root: bool, rules: bool) -> std::io::Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    let mut root = read(config)?;
    let path = script.display().to_string();
    let mut changed = false;
    let hooks = if at_root {
        root.as_object_mut()
    } else {
        root.get_mut("hooks").and_then(|h| h.as_object_mut())
    };
    let mut hooks_emptied = false;
    if let Some(hooks) = hooks {
        let mut emptied = Vec::new();
        for (event, list) in hooks.iter_mut() {
            let Some(groups) = list.as_array_mut() else {
                continue;
            };
            let before = groups.len();
            let mut touched = false;
            // Only a group this pass emptied goes: an empty group the person already had
            // is theirs, and dropping it would leave a diff they never asked for. A group
            // that was empty until install filled it still goes, since nothing records
            // that it was empty before; not worth a marker.
            let mut gone = Vec::with_capacity(before);
            for group in groups.iter_mut() {
                // The flat shape: the group is itself the entry.
                let mut ours = says(group, &path);
                if let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    let was = inner.len();
                    inner.retain(|entry| !says(entry, &path));
                    touched |= inner.len() != was;
                    ours |= was != 0 && inner.is_empty();
                }
                gone.push(ours);
            }
            let mut gone = gone.into_iter();
            groups.retain(|_| !gone.next().unwrap_or(false));
            touched |= groups.len() != before;
            if touched && groups.is_empty() {
                emptied.push(event.clone());
            }
            changed |= touched;
        }
        for event in emptied {
            hooks.shift_remove(&event);
        }
        hooks_emptied = changed && hooks.is_empty();
    }
    if hooks_emptied
        && !at_root
        && let Some(root) = root.as_object_mut()
    {
        root.shift_remove("hooks");
    }
    // The permissions go only with the install itself, never with an old script's entries.
    let added = if rules {
        ours(&script.with_file_name(ALLOW_MARKER))
    } else {
        Vec::new()
    };
    if !added.is_empty() {
        changed |= strip_allow(&mut root, &added);
    }
    if changed {
        // No kept copy at all means no write ever found the file there before it: the
        // install created it. Holding nothing of the person's, it goes.
        if holds_nothing(&root) && kept_copies(config).is_empty() {
            // The file, not a link to it: a symlinked configuration keeps its link, left
            // dangling again the way the install found it.
            std::fs::remove_file(link_target(config))?;
        } else {
            write(config, &root)?;
        }
    }
    Ok(changed)
}

/// Whether a configuration holds nothing but what an install would have put in a file it
/// created: nothing, or Cursor's schema version.
fn holds_nothing(root: &serde_json::Value) -> bool {
    root.as_object().is_some_and(|o| {
        o.is_empty() || (o.len() == 1 && o.get("version") == Some(&serde_json::json!(1)))
    })
}

/// Whether anything in this list points at our script.
fn mentions(list: &[serde_json::Value], path: &str) -> bool {
    list.iter().any(|entry| {
        says(entry, path)
            || entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|inner| inner.iter().any(|e| says(e, path)))
                .unwrap_or(false)
    })
}

/// Whether one entry points at our script, whatever shape it is in.
fn says(entry: &serde_json::Value, path: &str) -> bool {
    match entry {
        serde_json::Value::String(text) => text.contains(path),
        other => other
            .get("command")
            .and_then(|c| c.as_str())
            .map(|c| c.contains(path))
            .unwrap_or(false),
    }
}

fn object_root(root: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    root.as_object_mut().expect("an object")
}

fn object<'a>(
    root: &'a mut serde_json::Value,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    let root = object_root(root);
    root.entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let slot = root.get_mut(key).expect("just inserted");
    if !slot.is_object() {
        *slot = serde_json::json!({});
    }
    slot.as_object_mut().expect("an object")
}

fn array<'a>(
    parent: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut Vec<serde_json::Value> {
    parent
        .entry(key.to_string())
        .or_insert_with(|| serde_json::json!([]));
    let slot = parent.get_mut(key).expect("just inserted");
    if !slot.is_array() {
        *slot = serde_json::json!([]);
    }
    slot.as_array_mut().expect("an array")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration with somebody's own hook already in it.
    fn existing() -> serde_json::Value {
        serde_json::json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [{
                    "matcher": "",
                    "hooks": [{ "type": "command", "command": "~/mine.sh" }]
                }]
            }
        })
    }

    fn target(agent: &str) -> Target {
        targets()
            .into_iter()
            .find(|t| t.agent == agent)
            .expect("a target")
    }

    /// The Claude hook holds its program in a quoted string: a heredoc needs a temporary file,
    /// and on a full disk the hook then reported nothing at all.
    #[test]
    fn the_claude_hook_holds_its_program_without_a_heredoc() {
        let body = target("claude").body;
        assert!(!body.contains("<<"), "a heredoc in the Claude hook");
    }

    /// A dropped hook lands in the agent's own directory: install used to
    /// write the script to qoop's directory and then fail editing the plugin
    /// directory as a file, so opencode never got its hook at all.
    #[test]
    fn installing_a_dropped_hook_writes_the_plugin_directory() {
        let home = tempfile::tempdir().expect("a home");
        let opencode = target("opencode");
        assert!(install_dropped(home.path(), &opencode).expect("install"));
        let dropped = home.path().join(opencode.config).join(opencode.file);
        assert_eq!(
            std::fs::read_to_string(&dropped).expect("read"),
            opencode.body
        );
        assert!(
            !install_dropped(home.path(), &opencode).expect("reinstall"),
            "reinstalling says installed"
        );
    }

    /// The file an older qoop dropped (opencode 1's plugin shape, which opencode 2 refuses to
    /// load) is replaced by this build's, and a plugin beside it is left alone.
    #[test]
    fn an_older_dropped_plugin_is_replaced() {
        let home = tempfile::tempdir().expect("a home");
        let opencode = target("opencode");
        let dir = home.path().join(opencode.config);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join(opencode.file),
            "export const Old = async () => ({});",
        )
        .expect("old");
        std::fs::write(dir.join("mine.js"), "// mine").expect("mine");
        assert!(install_dropped(home.path(), &opencode).expect("install"));
        assert_eq!(
            std::fs::read_to_string(dir.join(opencode.file)).expect("read"),
            opencode.body
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("mine.js")).expect("mine"),
            "// mine"
        );
    }

    /// Installing adds beside what is there, and never over it.
    ///
    /// The whole reason this is a gate: a person's own hooks are theirs, and an installer
    /// that replaced them would lose work in a way that looks like it worked.
    #[test]
    fn installing_keeps_every_setting_and_every_hook_that_was_already_there() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, serde_json::to_string(&existing()).expect("json")).expect("write");
        let script = dir.path().join("qoop-hook.sh");

        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).expect("read")).expect("json");

        assert_eq!(after["model"], "opus", "an unrelated setting was lost");
        let pre = after["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .expect("the group");
        assert_eq!(
            pre.len(),
            2,
            "the person's own hook and ours have to be there together: {pre:?}"
        );
        assert_eq!(
            pre[0]["command"], "~/mine.sh",
            "theirs came first and stays"
        );
        assert!(
            pre[1]["command"]
                .as_str()
                .expect("a command")
                .contains("qoop-hook.sh")
        );
        // Every event the target names is registered.
        for event in ["SessionStart", "Stop", "SessionEnd", "Notification"] {
            assert!(
                after["hooks"][event].is_array(),
                "{event} was not registered: {after}"
            );
        }
        // And a copy of what was there was kept.
        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read")
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("qoop-backup"))
            .collect();
        assert_eq!(backups.len(), 1, "no copy was kept before writing");
    }

    /// Installing twice changes nothing the second time.
    #[test]
    fn installing_again_changes_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        let script = dir.path().join("qoop-hook.sh");
        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        let once = std::fs::read_to_string(&config).expect("read");
        assert!(
            !edit(&config, &target("claude"), &script, false).expect("install again"),
            "the second install reported a change"
        );
        assert_eq!(once, std::fs::read_to_string(&config).expect("read"));
    }

    /// Uninstalling takes out exactly what was put in.
    #[test]
    fn uninstalling_removes_ours_and_leaves_theirs() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, serde_json::to_string(&existing()).expect("json")).expect("write");
        let script = dir.path().join("qoop-hook.sh");
        edit(&config, &target("claude"), &script, false).expect("install");

        assert!(remove(&config, &script, false, true).expect("uninstall"));
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).expect("read")).expect("json");
        assert_eq!(after["model"], "opus");
        assert_eq!(
            after["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "~/mine.sh",
            "their hook went with ours: {after}"
        );
        // The events that held nothing but ours are gone rather than left empty.
        assert!(after["hooks"].get("SessionEnd").is_none(), "{after}");
        // And taking it out twice is not an error.
        assert!(!remove(&config, &script, false, true).expect("uninstall again"));
    }

    /// A settings file kept in dotfiles comes back in the order it was written, so the
    /// only diff an install leaves is the hooks it added.
    #[test]
    fn installing_keeps_the_order_of_everything_it_did_not_add() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, r#"{"model":"opus","env":{"B":"1","A":"2"}}"#).expect("write");
        let script = dir.path().join("qoop-hook.sh");
        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        let text = std::fs::read_to_string(&config).expect("read");
        let at = |k: &str| text.find(&format!("\"{k}\"")).expect(k);
        assert!(at("model") < at("env") && at("env") < at("hooks"), "{text}");
        assert!(at("B") < at("A"), "{text}");
    }

    /// An empty group or event list the person already had is theirs: uninstall takes out
    /// only what its own pass emptied.
    #[test]
    fn uninstalling_leaves_empty_groups_and_events_that_were_already_empty() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        let mut before = existing();
        before["hooks"]["Foo"] = serde_json::json!([]);
        before["hooks"]["PreToolUse"]
            .as_array_mut()
            .expect("a list")
            .insert(0, serde_json::json!({ "matcher": "Bash", "hooks": [] }));
        std::fs::write(&config, serde_json::to_string(&before).expect("json")).expect("write");
        let script = dir.path().join("qoop-hook.sh");
        edit(&config, &target("claude"), &script, false).expect("install");

        assert!(remove(&config, &script, false, true).expect("uninstall"));
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).expect("read")).expect("json");
        assert_eq!(after["hooks"]["Foo"], serde_json::json!([]), "{after}");
        assert_eq!(
            after["hooks"]["PreToolUse"][0],
            serde_json::json!({ "matcher": "Bash", "hooks": [] }),
            "{after}"
        );
        assert_eq!(
            after["hooks"]["PreToolUse"][1]["hooks"][0]["command"],
            "~/mine.sh"
        );
        assert!(after["hooks"].get("SessionEnd").is_none(), "{after}");
    }

    /// A hooks directory a command line cannot carry as is is refused, not quoted.
    #[test]
    fn a_hooks_directory_with_a_space_is_refused() {
        let err = safe_hook_dir(Path::new("/Users/a/my config/qoop/hooks")).expect_err("space");
        assert!(err.to_string().contains("XDG_CONFIG_HOME"), "{err}");
        safe_hook_dir(Path::new("/Users/a.b/.config/qoop/hooks")).expect("a plain path");
        // A directory-joined Linux home, and a non-ASCII name: both carried fine unquoted.
        safe_hook_dir(Path::new("/home/jo@corp.example/.config/qoop/hooks")).expect("an @");
        safe_hook_dir(Path::new("/home/çağrı/.config/qoop/hooks")).expect("non-ASCII");
        safe_hook_dir(Path::new("/tmp/$(x)/qoop/hooks")).expect_err("shell syntax");
    }

    /// The flat shape and the plugin shape are written the way their agent reads them.
    #[test]
    fn each_agent_gets_the_shape_its_own_configuration_uses() {
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoop-hook.sh");

        // Cursor: a version, and entries with nothing around them.
        let cursor = dir.path().join("cursor.json");
        edit(&cursor, &target("cursor"), &script, false).expect("install");
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cursor).expect("read")).expect("json");
        assert_eq!(after["version"], 1, "cursor refuses a file with no version");
        let entry = &after["hooks"]["sessionStart"][0];
        assert!(
            entry.get("hooks").is_none(),
            "cursor has no wrapper: {entry}"
        );
        assert!(
            entry["command"]
                .as_str()
                .expect("a command")
                .ends_with(" starting"),
            "the state has to be on the command line: {entry}"
        );

        // opencode: the file in its plugin directory, and no configuration touched. It
        // reads every plugin in that directory, and writing a config beside the one a
        // person keeps would have left two of them with opencode reading one of them.
        let opencode = target("opencode");
        assert!(
            matches!(opencode.shape, Shape::Dropped),
            "opencode is installed by dropping the file in, not by editing a config"
        );
        assert!(
            opencode.config.ends_with("plugin"),
            "the target is the plugin directory: {}",
            opencode.config
        );
    }

    /// A config kept by a dotfile manager is a link, and a settings file holding a key
    /// is 0600: the edit goes through the link and keeps the mode.
    #[test]
    fn a_linked_private_config_stays_a_link_and_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let real = dir.path().join("dotfiles-settings.json");
        std::fs::write(&real, serde_json::to_string(&existing()).expect("json")).expect("write");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let config = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&real, &config).expect("link");
        let script = dir.path().join("qoop-hook.sh");

        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        assert!(
            std::fs::symlink_metadata(&config)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link was replaced by a plain file"
        );
        assert!(
            std::fs::read_to_string(&real)
                .expect("read")
                .contains("qoop-hook.sh"),
            "the file behind the link was not edited"
        );
        let mode = std::fs::metadata(&real).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a private config was left readable: {mode:o}");
    }

    /// Claude's `Notification` covers much more than a question: `idle_prompt` fires a
    /// minute after an agent finished. Only the kinds that wait on the person are
    /// blocked; a payload with no kind at all is an older build and stays blocked.
    #[test]
    fn the_claude_hook_reports_blocked_only_for_notifications_that_wait_on_the_person() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoop-hook.sh");
        std::fs::write(&script, target("claude").body).expect("write");
        // A stand-in `qoop` that writes down what it was asked to report.
        let log = dir.path().join("reports");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let report = |payload: &str| -> String {
            let _ = std::fs::remove_file(&log);
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        for kind in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
            "quota_auto_resume_stale",
        ] {
            let said = report(&format!(
                r#"{{"hook_event_name":"Notification","notification_type":"{kind}"}}"#
            ));
            assert!(said.contains("--state blocked"), "{kind} reported {said:?}");
            // A question dialog names its type, which the daemon drafts a card for; the
            // others carry no label.
            let dialog = kind.starts_with("elicitation_");
            assert_eq!(
                said.contains(&format!("--label {kind}")),
                dialog,
                "{kind} reported {said:?}"
            );
            assert_eq!(said.contains("--label"), dialog, "{kind} reported {said:?}");
        }
        let said = report(r#"{"hook_event_name":"Notification"}"#);
        assert!(
            said.contains("--state blocked"),
            "no kind reported {said:?}"
        );
        for kind in ["idle_prompt", "auth_success", "agent_completed"] {
            let said = report(&format!(
                r#"{{"hook_event_name":"Notification","notification_type":"{kind}"}}"#
            ));
            assert!(said.is_empty(), "{kind} reported {said:?}");
        }
    }

    /// A backgrounded Claude session runs under Claude's own daemon, which carries the
    /// terminal and token of the tab that started it: reporting from there filed the
    /// onboarding conversation, its title and its subagent under another space's tab.
    #[test]
    fn the_claude_hook_reports_nothing_from_a_backgrounded_session() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoop-hook.sh");
        std::fs::write(&script, target("claude").body).expect("write");
        let log = dir.path().join("reports");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let report = |job_dir: &str| -> String {
            let _ = std::fs::remove_file(&log);
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .env("CLAUDE_JOB_DIR", job_dir)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            // A hook that has nothing to report ends without reading its input: the pipe
            // closing under the write is that, not a failure.
            let wrote = child.stdin.take().expect("stdin").write_all(
                br#"{"hook_event_name":"UserPromptSubmit","session_id":"s1","prompt":"hi"}"#,
            );
            if let Err(e) = wrote {
                assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "payload: {e}");
            }
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        assert!(
            report("/Users/me/.claude/jobs/s1").is_empty(),
            "a backgrounded session reported"
        );
        assert!(
            report("").contains("--session-id s1"),
            "a session in its own tab has to keep reporting"
        );
    }

    /// A session opening is `starting`, never `working`: an agent that opens and sits
    /// idle did no turn, and a turn settling is what says "finished". Codex and gemini
    /// installed earlier still pass `working` for it, and still report `starting`.
    #[test]
    fn a_session_start_reports_starting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let log = dir.path().join("reports");
        let fake = dir.path().join("qoop");
        // The rules a session start also asks for are not a report.
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n[ \"$1\" = task ] && exit 0\necho \"$*\" >> '{}'\n",
                log.display()
            ),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let report = |agent: &str, arg: &str, payload: &str| -> String {
            let _ = std::fs::remove_file(&log);
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .arg(arg)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        for source in ["startup", "resume", "clear"] {
            let said = report(
                "claude",
                "",
                &format!(r#"{{"hook_event_name":"SessionStart","source":"{source}"}}"#),
            );
            assert_eq!(
                said.trim(),
                "hook-report --agent claude --state starting",
                "{source}"
            );
        }
        let said = report(
            "claude",
            "",
            r#"{"hook_event_name":"SessionStart","source":"compact"}"#,
        );
        // Claude compacts an idle session too, with no Stop after it: a compaction leaves
        // the state as it was, working inside a turn and idle outside one.
        assert!(
            !said.contains("hook-report"),
            "compact reports no state: {said:?}"
        );
        for agent in ["codex", "gemini"] {
            let said = report(agent, "working", r#"{"hook_event_name":"SessionStart"}"#);
            assert_eq!(
                said.trim(),
                format!("hook-report --agent {agent} --state starting"),
                "{agent}"
            );
            let said = report(
                agent,
                "working",
                r#"{"hook_event_name":"UserPromptSubmit"}"#,
            );
            assert!(
                said.contains("--state working --visible-working"),
                "{agent}: {said:?}"
            );
        }
    }

    /// `qoop task rules` runs where the agent works, the payload's `cwd`, as the other calls
    /// do, so its repo block is that repository's; not where the hook happened to start.
    #[test]
    fn the_rules_run_in_the_payloads_cwd() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let repo = dir.path().join("work repo");
        let elsewhere = dir.path().join("elsewhere");
        for d in [&repo, &elsewhere] {
            std::fs::create_dir_all(d).expect("dir");
        }
        let repo = repo.canonicalize().expect("real path");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1 $2\" = \"task rules\" ] && echo \"rules in $(pwd -P)\"\nexit 0\n",
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let payload = serde_json::json!({"hook_event_name": "SessionStart", "cwd": repo});
        for (agent, arg) in [("claude", ""), ("codex", "working"), ("gemini", "working")] {
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .arg(arg)
                .current_dir(&elsewhere)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.to_string().as_bytes())
                .expect("payload");
            let out = child.wait_with_output().expect("wait");
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(
                said.contains(&format!("rules in {}", repo.display())),
                "{agent}: {said:?}"
            );
        }
    }

    /// A qoop older than the hook refuses `--said` and with it the whole report: the hook
    /// sends the turn's end again without it, so the state still arrives. Any other failure
    /// (a daemon slow to answer a report it took) is not sent twice.
    #[test]
    fn the_claude_hook_still_reports_a_turns_end_to_an_older_qoop() {
        use std::os::unix::fs::PermissionsExt;
        // The reports one Stop makes against a fake qoop whose `hook-report` runs `refuse`.
        let reports_with = |refuse: &str| -> Vec<String> {
            let dir = tempfile::tempdir().expect("a directory");
            let log = dir.path().join("reports");
            let fake = dir.path().join("qoop");
            std::fs::write(
                &fake,
                format!(
                    "#!/bin/sh\nprintf '%s|' \"$@\" >> '{}'\n{refuse}\nexit 0\n",
                    log.display()
                ),
            )
            .expect("write");
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let script = dir.path().join("qoop-hook.sh");
            std::fs::write(&script, target("claude").body).expect("write");
            let payload = serde_json::json!({
                "session_id": "f339fcc3-ddfe-4fe2-8dc0-e4b6ddbf18f9",
                "transcript_path": dir.path().join("none.jsonl"), "cwd": dir.path(),
                "hook_event_name": "Stop", "stop_hook_active": false, "background_tasks": [],
                "last_assistant_message": "Built it.\n\nMerge now?"});
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.to_string().as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success());
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .split("hook-report|")
                .filter(|r| r.contains("--state|idle"))
                .map(str::to_string)
                .collect()
        };
        let older = reports_with(
            "for a in \"$@\"; do [ \"$a\" = --said ] && echo 'qoop: unknown flag --said' >&2 && exit 2; done",
        );
        assert_eq!(older.len(), 2, "{older:?}");
        assert!(older[0].contains("--said|Built it."), "{older:?}");
        assert!(
            !older[1].contains("--said") && older[1].contains("--event|done|--label|Merge now?|"),
            "{older:?}"
        );
        let slow = reports_with("echo 'qoop: no answer from the daemon' >&2; exit 1");
        assert_eq!(slow.len(), 1, "never twice: {slow:?}");
    }

    /// A turn's end carries its last paragraph as one line, markdown off, read from the
    /// payload's own message, else the transcript, so the daemon can tell whether it asks the
    /// owner and put it in his inbox; a long one keeps both ends within the label's 120
    /// characters.
    #[test]
    fn the_claude_hook_reports_the_turns_last_paragraph_at_its_end() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let log = dir.path().join("reports");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\nprintf '%s|' \"$@\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let script = dir.path().join("qoop-hook.sh");
        std::fs::write(&script, target("claude").body).expect("write");
        // A Stop payload as a real one has it (captured from Claude Code), with the transcript
        // the test chooses and, when given, the turn's message.
        let stop = |transcript: &str, message: Option<&str>| -> String {
            let _ = std::fs::remove_file(&log);
            let path = dir.path().join("t.jsonl");
            std::fs::write(&path, transcript).expect("write");
            let mut payload = serde_json::json!({
                "session_id": "f339fcc3-ddfe-4fe2-8dc0-e4b6ddbf18f9",
                "transcript_path": path, "cwd": dir.path(),
                "prompt_id": "7fc6b60f-5f51-4333-999e-4b03ffb3218e",
                "permission_mode": "default", "effort": {"level": "medium"},
                "hook_event_name": "Stop", "stop_hook_active": false,
                "background_tasks": [], "session_crons": []});
            if let Some(message) = message {
                payload["last_assistant_message"] = message.into();
            }
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.to_string().as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        let turn = |said: &str| -> String {
            let turn = serde_json::json!({"type": "assistant", "message": {"role": "assistant",
                "content": [{"type": "text", "text": said}]}});
            format!("{turn}\n")
        };
        let said = stop(
            &turn("Built and tested.\n**Merge PR #513 now or deploy first?**\nSay which."),
            None,
        );
        assert!(
            said.contains(
                "--event|done|--label|Built and tested. Merge PR #513 now or deploy first? Say which.|"
            ),
            "{said}"
        );
        let said = stop(
            &turn("Fixed the 404.\n\nIstersen bunu ayri bir is olarak guncellerim."),
            None,
        );
        assert!(
            said.contains("--label|Istersen bunu ayri bir is olarak guncellerim.|"),
            "only the last paragraph: {said}"
        );
        let long = format!("Istersen {} hazirlayayim mi?", "x ".repeat(100));
        let said = stop(&turn(&long), None);
        let label = said
            .split("--label|")
            .nth(1)
            .and_then(|l| l.split('|').next())
            .unwrap_or_default();
        assert!(
            label.chars().count() <= 120
                && label.starts_with("Istersen ")
                && label.ends_with("mi?"),
            "{label}"
        );
        // A question he already answered is not the turn's: his prompt ends the read back.
        let ask = serde_json::json!({"message": {"role": "assistant",
            "content": [{"type": "text", "text": "Merge PR #513 now?"}]}});
        let answer = serde_json::json!({"message": {"role": "user", "content": "merge it"}});
        let tool = serde_json::json!({"message": {"role": "assistant",
            "content": [{"type": "tool_use", "input": {"command": "qoop task stage implementing"}}]}});
        let said = stop(&format!("{ask}\n{answer}\n{tool}\n"), None);
        assert!(!said.contains("--label"), "an answered question: {said}");
        // The payload carries the message itself while the transcript does not hold the turn
        // yet (it is written after the hook runs): the payload's message is the label.
        let said = stop("", Some("Built it.\n\nMerge now?"));
        assert!(said.contains("--event|done|--label|Merge now?|"), "{said}");
        // The card's own words: what it did and the whole question, a blank line between,
        // markdown off; the label stays the cut line the question reading judges.
        assert!(said.contains("--said|Built it.\n\nMerge now?|"), "{said}");
        let said = stop(
            "",
            Some("I rebuilt the **rate table** in `rates.rs`.\n\nMerge now?"),
        );
        assert!(
            said.contains("--said|I rebuilt the rate table in rates.rs.\n\nMerge now?|"),
            "plain words: {said}"
        );
        let said = stop("", Some(&format!("Done.\n\n{long}")));
        let whole = said
            .split("--said|")
            .nth(1)
            .and_then(|l| l.split('|').next())
            .unwrap_or_default();
        assert!(whole.starts_with("Done.\n\nIstersen "), "{whole}");
        assert!(
            whole.ends_with("mi?"),
            "whole, never cut in the middle: {whole}"
        );
        let cut = format!("{}?", "word ".repeat(200));
        let said = stop("", Some(&cut));
        let whole = said
            .split("--said|")
            .nth(1)
            .and_then(|l| l.split('|').next())
            .unwrap_or_default();
        assert!(
            whole.chars().count() <= 600 && whole.ends_with("word\u{2026}"),
            "cut at a word: {whole}"
        );
        // A 300 character question keeps the file it names on the card, with no markdown.
        let named = format!(
            "**Senin isin:** siteye bak. \"evet\" dersen `design/site/index.html` {}acarim mi?",
            "x ".repeat(120)
        );
        let said = stop("", Some(&named));
        let expected = named.replace("**", "").replace('`', "");
        assert!(
            said.contains(&format!("--said|{}|", expected.trim_end())),
            "{said}"
        );
        // Code ticks keep what they quote in the label (the daemon reads a quoted answer by
        // them); the card drops the ticks, and a file name keeps its own marks.
        let said = stop(
            "",
            Some("**Sil** `__init__.py` mi? `src/*.rs` ve `tests/*.rs`, `__name__`?"),
        );
        assert!(
            said.contains("--label|Sil `__init__.py` mi? `src/*.rs` ve `tests/*.rs`, `__name__`?|"),
            "{said}"
        );
        assert!(
            said.contains("--said|Sil __init__.py mi? src/*.rs ve tests/*.rs, __name__?|"),
            "{said}"
        );
        // A bare path keeps its marks too: a "/" before them is no emphasis.
        let said = stop("", Some("Sil path/_build_/x ve src/*.rs/tests/*.rs mi?"));
        assert!(
            said.contains("--label|Sil path/_build_/x ve src/*.rs/tests/*.rs mi?|"),
            "{said}"
        );
        // A file name stays whole in a code span of any tick run, at either end of the line,
        // and bare: a mark never closes before an extension or a "/".
        for kept in [
            "Use ``__init__.py`` here?",
            "Choose __init__.py or __main__.py?",
            "Delete **/*.rs and **/*.ts?",
            "`__init__.py` silinsin mi?",
            "Silinsin mi `__pycache__`",
        ] {
            let said = stop("", Some(kept));
            assert!(said.contains(&format!("--label|{kept}|")), "{said}");
        }
        // Emphasis around a code span comes off as a whole; a fence line still loses its ticks.
        let said = stop("", Some("Should I **delete `foo.rs`** too?"));
        assert!(
            said.contains("--label|Should I delete `foo.rs` too?|"),
            "{said}"
        );
        // Emphasis never closes inside a code span it wraps: the span stays whole, and an
        // opener with no close outside the span keeps its marks.
        let said = stop("", Some("Should I **run `git diff -- \"**\"`**?"));
        assert!(
            said.contains("--label|Should I run `git diff -- \"**\"`?|"),
            "{said}"
        );
        let said = stop("", Some("Should I **run `a \"**\"` now?"));
        assert!(
            said.contains("--label|Should I **run `a \"**\"` now?|"),
            "{said}"
        );
        // Many spans behind unclosed openers: each span is stepped over once, not split every
        // way it could be, so the hook ends at once (30 of them backtracked for minutes).
        let spans = "**a `b` ".repeat(30);
        let started = std::time::Instant::now();
        let said = stop("", Some(&spans));
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        // The line kept, then cut to 120 with both ends, as any label.
        let line = &spans.trim_end()[2..];
        let kept = format!("{} ... {}", &line[..57], &line[line.len() - 57..]);
        assert!(said.contains(&format!("--label|{kept}|")), "{said}");
        let said = stop("", Some("Run this?\n```sh\nls -la\n```"));
        assert!(said.contains("--label|Run this? sh ls -la|"), "{said}");
    }

    /// What a subagent fires describes the subagent: its tool calls and its end would
    /// otherwise put a tab whose own turn is over back to working.
    #[test]
    fn the_claude_hook_reports_nothing_for_a_subagent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoop-hook.sh");
        std::fs::write(&script, target("claude").body).expect("write");
        let log = dir.path().join("reports");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let report = |payload: &str| -> String {
            let _ = std::fs::remove_file(&log);
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        for payload in [
            r#"{"hook_event_name":"PreToolUse","agent_id":"def456","tool_name":"Bash"}"#,
            r#"{"hook_event_name":"PostToolUse","agent_id":"def456","tool_name":"Read"}"#,
        ] {
            let said = report(payload);
            assert!(said.is_empty(), "{payload} reported {said:?}");
        }
        // The main thread's own tool call still reports.
        let said = report(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#);
        assert!(said.contains("--state working"), "{said:?}");
        // His answer in a question dialog is his answer, as a prompt is: the daemon takes a
        // task waiting on him back to work. Asking is still a tool, and so is any other end.
        let said = report(r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion"}"#);
        assert!(
            said.contains("--event user") && !said.contains("--label"),
            "{said:?}"
        );
        let said = report(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion"}"#);
        assert!(said.contains("--event tool"), "{said:?}");
        let said = report(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#);
        assert!(said.contains("--event tool"), "{said:?}");
    }

    /// A configuration that is not JSON is reported, not overwritten.
    #[test]
    fn a_file_that_cannot_be_read_is_left_exactly_as_it_is() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, "{ this is not json").expect("write");
        let script = dir.path().join("qoop-hook.sh");
        let err = edit(&config, &target("claude"), &script, false).expect_err("not JSON");
        assert!(err.to_string().contains("valid JSON"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "{ this is not json",
            "a file that could not be parsed was written anyway"
        );
    }

    /// Every shipped hook is installable, and every target names a real script.
    #[test]
    fn every_target_carries_its_own_script_and_a_place_to_put_it() {
        let mut seen = std::collections::BTreeSet::new();
        for target in targets() {
            assert!(
                !target.body.trim().is_empty(),
                "{} has no script",
                target.agent
            );
            assert!(
                target.body.contains("qoop"),
                "{} ships something that is not the hook",
                target.agent
            );
            assert!(
                seen.insert(target.file),
                "{} would overwrite another agent's script",
                target.agent
            );
            assert!(
                !target.config.starts_with('/'),
                "{} writes outside the home directory",
                target.agent
            );
        }
        assert!(
            seen.len() >= 5,
            "only {} agents can be installed",
            seen.len()
        );
    }

    /// What a first start says, and what it stops saying.
    ///
    /// An agent that is not on this machine is never mentioned, and neither is one whose
    /// hook is already where that agent reads it: a line that keeps appearing after a
    /// person has done what it asked is a line they learn to ignore.
    #[test]
    fn only_agents_that_are_here_and_unhooked_are_mentioned() {
        let dir = tempfile::tempdir().expect("a directory");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("dir");
        // An agent on this machine, and one that is not.
        std::fs::write(bin.join("claude"), "#!/bin/sh\n").expect("write");

        let home = dir.path().join("home");
        let hooks = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks).expect("dir");
        let claude = targets().into_iter().find(|t| t.agent == "claude").unwrap();
        let codex = targets().into_iter().find(|t| t.agent == "codex").unwrap();

        // Nothing registered: the one that is here is the one to mention.
        assert!(!registered(&claude, &hooks, &home));
        assert!(!registered(&codex, &hooks, &home));

        // Registered, and it stops being mentioned. Recognised by the path, which is why
        // the script lives in a directory of its own.
        let config = home.join(claude.config);
        std::fs::create_dir_all(config.parent().expect("a parent")).expect("dir");
        std::fs::write(
            &config,
            format!(
                "{{\"hooks\":{{\"Stop\":[{{\"hooks\":[{{\"command\":\"{}\"}}]}}]}}}}",
                hooks.join(claude.file).display()
            ),
        )
        .expect("write");
        assert!(registered(&claude, &hooks, &home));

        // The dropped shape is the file being there, not a configuration mentioning it.
        let opencode = targets()
            .into_iter()
            .find(|t| t.agent == "opencode")
            .unwrap();
        assert!(!registered(&opencode, &hooks, &home));
        let plugin_dir = home.join(opencode.config);
        std::fs::create_dir_all(&plugin_dir).expect("dir");
        std::fs::write(plugin_dir.join(opencode.file), "// x").expect("write");
        assert!(registered(&opencode, &hooks, &home));
    }

    /// An agent is looked for under the name its binary actually has.
    #[test]
    fn an_agent_whose_binary_is_named_differently_is_still_found() {
        let dir = tempfile::tempdir().expect("a directory");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("dir");
        std::fs::write(bin.join("cursor-agent"), "#!/bin/sh\n").expect("write");
        let path = bin.display().to_string();
        assert!(on_path_in("cursor", &path), "cursor runs as cursor-agent");
        assert!(!on_path_in("claude", &path));
        assert!(!on_path_in("codex", &path));
        // An empty entry in a path is not the current directory to look in.
        assert!(!on_path_in("claude", "::"));
    }

    /// A file a person wrote by hand, with their own indentation, key order and an empty
    /// list of their own, none of which a JSON library would reproduce.
    const HANDWRITTEN: &str = "{\n    \"zeta\": true,\n    \"alpha\": [1, 2],\n    \"hooks\": {\n        \"PreToolUse\": [],\n        \"Stop\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"~/mine.sh\" } ] } ]\n    }\n}\n";

    /// Installing and then uninstalling a session hook gives back the file byte for byte,
    /// for every agent that takes one, whether or not the file had a `hooks` key.
    #[test]
    fn a_session_hook_round_trip_leaves_the_file_exactly_as_it_was() {
        for agent in ["copilot", "droid", "qwen", "qodercli"] {
            for original in [HANDWRITTEN, "{ \"model\":\"x\" }\n"] {
                let home = tempfile::tempdir().expect("a home");
                let t = target(agent);
                let (config, at_root) = config_of(&t, home.path());
                assert!(!at_root, "{agent}: no hooks.json was made");
                std::fs::create_dir_all(config.parent().expect("a parent")).expect("dir");
                std::fs::write(&config, original).expect("write");
                let script = home.path().join(t.file);

                assert!(
                    edit(&config, &t, &script, at_root).expect("install"),
                    "{agent}"
                );
                let installed = std::fs::read_to_string(&config).expect("read");
                assert!(installed.contains(t.file), "{agent}: {installed}");
                assert!(
                    remove(&config, &script, at_root, true).expect("uninstall"),
                    "{agent}"
                );
                assert_eq!(
                    std::fs::read_to_string(&config).expect("read"),
                    original,
                    "{agent}: the file came back different"
                );
            }
        }
    }

    /// Each session hook is written in the shape its agent reads, for SessionStart only.
    #[test]
    fn each_session_hook_lands_where_and_how_its_agent_reads_it() {
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoop-session.sh");
        let installed = |agent: &str| {
            let config = dir.path().join(format!("{agent}.json"));
            edit(&config, &target(agent), &script, false).expect("install");
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(&config).expect("read"),
            )
            .expect("json")
        };

        // Copilot: flat entries in its settings, and no version key: that belongs to a
        // hook file, not to a settings file.
        let copilot = installed("copilot");
        assert!(copilot.get("version").is_none(), "{copilot}");
        let entry = &copilot["hooks"]["SessionStart"][0];
        assert!(
            entry.get("hooks").is_none(),
            "copilot has no wrapper: {entry}"
        );
        assert_eq!(entry["command"], script.display().to_string());

        // Droid: nested, no matcher. Qwen and Qoder: nested, matching everything.
        let droid = installed("droid");
        let group = &droid["hooks"]["SessionStart"][0];
        assert!(group.get("matcher").is_none(), "{group}");
        assert_eq!(group["hooks"][0]["type"], "command");
        for agent in ["qwen", "qodercli"] {
            let value = installed(agent);
            assert_eq!(value["hooks"]["SessionStart"][0]["matcher"], "*", "{value}");
        }

        // Every one of them reports the session only, so one event and no argument.
        for agent in ["copilot", "droid", "qwen", "qodercli"] {
            let t = target(agent);
            let events = match &t.shape {
                Shape::Nested { events, .. } | Shape::Flat { events, .. } => events,
                Shape::Dropped => panic!("{agent} edits a configuration"),
            };
            assert!(
                matches!(events, [Event("SessionStart", None)]),
                "{agent} registers more than SessionStart"
            );
            assert_eq!(t.body, SESSION_HOOK, "{agent} ships its own script");
        }
    }

    /// Droid reads `hooks.json` instead of `settings.json` whenever there is one, and there
    /// the event map is the whole file. An entry written to the file it ignores would be an
    /// install that never runs.
    #[test]
    fn droid_writes_to_hooks_json_when_the_person_keeps_one() {
        let home = tempfile::tempdir().expect("a home");
        let droid = target("droid");
        assert_eq!(
            config_of(&droid, home.path()),
            (home.path().join(".factory/settings.json"), false)
        );

        let hooks_json = home.path().join(".factory/hooks.json");
        std::fs::create_dir_all(hooks_json.parent().expect("a parent")).expect("dir");
        let original = "{\"PreToolUse\":[{\"matcher\":\"Execute\",\"hooks\":[{\"type\":\"command\",\"command\":\"/a.sh\"}]}]}";
        std::fs::write(&hooks_json, original).expect("write");
        let (config, at_root) = config_of(&droid, home.path());
        assert_eq!((config.clone(), at_root), (hooks_json.clone(), true));

        let script = home.path().join(droid.file);
        assert!(edit(&config, &droid, &script, at_root).expect("install"));
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).expect("read")).expect("json");
        assert!(
            value.get("hooks").is_none(),
            "no wrapper in hooks.json: {value}"
        );
        assert_eq!(
            value["SessionStart"][0]["hooks"][0]["command"],
            script.display().to_string()
        );
        assert_eq!(value["PreToolUse"][0]["hooks"][0]["command"], "/a.sh");
        assert!(registered(&droid, home.path(), home.path()));

        assert!(remove(&config, &script, at_root, true).expect("uninstall"));
        assert_eq!(std::fs::read_to_string(&config).expect("read"), original);
    }

    /// The session hook reads the id out of the payload, reports it alone, and says
    /// nothing for any other event or for an id that is not only an id.
    #[test]
    fn the_session_hook_reports_the_id_and_nothing_else() {
        let dir = tempfile::tempdir().expect("a directory");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("dir");
        let log = dir.path().join("argv");
        // A stand-in qoop that records its argv.
        let qoop = bin.join("qoop");
        std::fs::write(
            &qoop,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&qoop, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let hook = dir.path().join("qoop-droid-session.sh");
        std::fs::write(&hook, SESSION_HOOK).expect("write");

        let run = |payload: &str| {
            use std::io::Write;
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&hook)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("QOOP_TERMINAL", "3")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("spawn");
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(
                child.wait().expect("wait").success(),
                "the hook must exit zero"
            );
        };
        run(r#"{"hook_event_name":"SessionStart","session_id":"abc-123","source":"startup"}"#);
        run(r#"{"hookEventName":"sessionStart","sessionId":"def_456"}"#);
        run(r#"{"hook_event_name":"PreToolUse","session_id":"nope"}"#);
        run(r#"{"hook_event_name":"SessionStart","session_id":"a b; rm -rf /"}"#);
        run(r#"{"hook_event_name":"SessionStart","session_id":"--resume"}"#);
        run(r#"{"hook_event_name":"SessionStart","session_id":"abcé"}"#);
        run(r#"{"hook_event_name":"SessionStart","session_id":"٣٤"}"#);
        run("not json");
        let lines = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(
            lines,
            "hook-report --agent droid --session-id abc-123\nhook-report --agent droid --session-id def_456\n"
        );
    }

    /// An entry written into Droid's settings before its `hooks.json` appeared is still
    /// seen by status and taken out by uninstall, rather than left pointing at a deleted
    /// script while status says nothing is installed and a reinstall adds a second one.
    #[test]
    fn droid_entry_in_settings_is_found_and_removed_after_hooks_json_appears() {
        let home = tempfile::tempdir().expect("a home");
        let droid = target("droid");
        let script = home.path().join(droid.file);
        let (settings, at_root) = config_of(&droid, home.path());
        assert!(!at_root);
        assert!(edit(&settings, &droid, &script, at_root).expect("install"));

        let hooks_json = home.path().join(".factory/hooks.json");
        std::fs::write(&hooks_json, "{}\n").expect("write");
        assert!(
            registered(&droid, home.path(), home.path()),
            "status must still see the entry in settings.json"
        );
        let outcomes = remove_everywhere(&droid, home.path(), &script, true);
        assert!(
            outcomes
                .iter()
                .any(|(path, r)| path == &settings && matches!(r, Ok(true))),
            "{outcomes:?}"
        );
        assert!(!registered(&droid, home.path(), home.path()));
        assert_eq!(std::fs::read_to_string(&hooks_json).expect("read"), "{}\n");
    }

    /// A configuration directory moved by the agent's own variable is where the entry goes.
    #[test]
    fn a_moved_configuration_directory_is_followed() {
        let home = Path::new("/home/u");
        let moved = Some(PathBuf::from("/elsewhere/copilot"));
        assert_eq!(
            config_path_in(&target("copilot"), home, moved),
            PathBuf::from("/elsewhere/copilot/settings.json")
        );
        assert_eq!(
            config_path_in(&target("qodercli"), home, None),
            PathBuf::from("/home/u/.qoder/settings.json")
        );
        for (agent, var) in [
            ("copilot", "COPILOT_HOME"),
            ("qwen", "QWEN_HOME"),
            ("qodercli", "QODER_CONFIG_DIR"),
        ] {
            assert_eq!(target(agent).home_env, Some(var), "{agent}");
        }
    }

    /// An agent in a folder agents install into counts with a bare `PATH`, the way a daemon
    /// started by ssh or launchd sees it; a file there that cannot run does not.
    #[test]
    fn an_agent_in_an_install_folder_is_present_without_path() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("home");
        let home = home.path();
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let dirs = agent_dirs(home);
        assert!(dirs.contains(&bin));
        assert!(!present_in(
            &target("gemini"),
            home,
            "/usr/bin:/bin",
            std::slice::from_ref(&bin)
        ));
        std::fs::write(bin.join("gemini"), "").expect("file");
        assert!(
            !present_in(&target("gemini"), home, "", std::slice::from_ref(&bin)),
            "not executable"
        );
        std::fs::set_permissions(bin.join("gemini"), std::fs::Permissions::from_mode(0o755))
            .expect("mode");
        assert!(present_in(
            &target("gemini"),
            home,
            "",
            std::slice::from_ref(&bin)
        ));
        assert!(!present_in(&target("claude"), home, "", &[bin]));
        let nvm = home.join(".nvm/versions/node/v22.1.0/bin");
        std::fs::create_dir_all(&nvm).expect("nvm");
        assert!(agent_dirs(home).contains(&nvm), "every nvm node version");
    }

    /// Removing on purpose is written before anything goes, naming takes it back, and the
    /// list goes once empty.
    #[test]
    fn the_list_of_removed_agents_adds_and_takes_back() {
        let config = tempfile::tempdir().expect("config");
        let config = config.path();
        assert!(!is_off(config, "hooks", "gemini"));
        set_off(config, "hooks", &["gemini", "claude"], true).expect("off");
        set_off(config, "hooks", &["gemini"], true).expect("again");
        assert!(is_off(config, "hooks", "gemini"));
        assert!(
            !is_off(config, "skills", "gemini"),
            "hooks and skills apart"
        );
        let list = std::fs::read_to_string(off_list(config, "hooks")).expect("list");
        assert_eq!(list, "claude\ngemini\n", "once each");
        set_off(config, "hooks", &["gemini", "claude"], false).expect("on");
        assert!(!is_off(config, "hooks", "claude"));
        assert!(
            !off_list(config, "hooks").exists(),
            "an empty list is no file"
        );
    }

    /// An agent is present by its binary or by its configuration directory, and neither
    /// means absent: an unnamed install skips it rather than creating its directory.
    #[test]
    fn an_agent_with_neither_binary_nor_configuration_is_not_present() {
        let dir = tempfile::tempdir().expect("a directory");
        let home = dir.path().join("home");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("dir");
        let path = bin.display().to_string();
        let qwen = target("qwen");
        assert!(!present_in(&qwen, &home, &path, &[]));
        std::fs::create_dir_all(home.join(".qwen")).expect("dir");
        assert!(
            present_in(&qwen, &home, &path, &[]),
            "its directory is enough"
        );
        std::fs::write(bin.join("qodercli"), "#!/bin/sh\n").expect("write");
        assert!(
            present_in(&target("qodercli"), &home, &path, &[]),
            "its binary is enough"
        );
    }

    /// A configuration that did not exist before the install does not survive the
    /// uninstall as an empty `{}`.
    #[test]
    fn a_file_the_install_created_is_removed_by_the_uninstall() {
        let home = tempfile::tempdir().expect("a home");
        let qwen = target("qwen");
        let config = home.path().join(qwen.config);
        let script = home.path().join(qwen.file);
        assert!(edit(&config, &qwen, &script, false).expect("install"));
        assert!(config.exists());
        assert!(remove(&config, &script, false, true).expect("uninstall"));
        assert!(
            !config.exists(),
            "an empty file the person never had was left"
        );

        // A file that was `{}` before the install is theirs, and comes back.
        std::fs::write(&config, "{ }\n").expect("write");
        edit(&config, &qwen, &script, false).expect("install");
        remove(&config, &script, false, true).expect("uninstall");
        assert_eq!(std::fs::read_to_string(&config).expect("read"), "{ }\n");
    }

    /// A symlinked configuration keeps its link, and the target keeps its permissions.
    #[test]
    fn writing_follows_a_symlink_and_keeps_the_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let real = dir.path().join("dotfiles-settings.json");
        std::fs::write(&real, "{\"a\": 1}\n").expect("write");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let script = dir.path().join("qoop.sh");
        assert!(edit(&link, &target("qwen"), &script, false).expect("install"));
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("meta")
                .file_type()
                .is_symlink(),
            "the link was replaced by a file"
        );
        assert!(
            std::fs::read_to_string(&real)
                .expect("read")
                .contains("qoop.sh")
        );
        let mode = std::fs::metadata(&real).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// Kept copies are pruned to the oldest plus the newest few, and the oldest (the
    /// file as it was before qoop first touched it) is the one that survives.
    #[test]
    fn kept_copies_are_pruned_to_the_oldest_and_the_newest_three() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, "{\"original\": true}\n").expect("write");
        for i in 0..8 {
            write(&config, &serde_json::json!({ "n": i })).expect("write");
        }
        let mut copies = kept_copies(&config);
        assert_eq!(copies.len(), KEPT_NEWEST + 1, "{copies:?}");
        copies.sort_unstable_by_key(|c| c.0);
        assert_eq!(
            std::fs::read_to_string(&copies[0].1).expect("read"),
            "{\"original\": true}\n"
        );
    }

    /// A configuration that was an empty file (or only whitespace) before the install is
    /// theirs, and comes back exactly: its existence alone decides things, Droid's
    /// `hooks.json` among them.
    #[test]
    fn an_empty_or_blank_original_comes_back_as_it_was() {
        for original in ["", "  \n"] {
            let dir = tempfile::tempdir().expect("a directory");
            let config = dir.path().join("hooks.json");
            std::fs::write(&config, original).expect("write");
            let script = dir.path().join("qoop.sh");
            let droid = target("droid");
            assert!(edit(&config, &droid, &script, true).expect("install"));
            assert!(remove(&config, &script, true, true).expect("uninstall"));
            assert_eq!(
                std::fs::read_to_string(&config).ok().as_deref(),
                Some(original),
                "{original:?}"
            );
        }
    }

    /// Cursor's file, created by the install, goes with the uninstall: the version key
    /// this wrote into it is not the person's.
    #[test]
    fn a_cursor_file_the_install_created_is_removed_by_the_uninstall() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("hooks.json");
        let script = dir.path().join("qoop.sh");
        assert!(edit(&config, &target("cursor"), &script, false).expect("install"));
        assert!(remove(&config, &script, false, true).expect("uninstall"));
        assert!(!config.exists(), "{:?}", std::fs::read_to_string(&config));
    }

    /// A dangling symlink stays a symlink, and the file it points at is created.
    #[test]
    fn a_dangling_symlink_keeps_its_link_and_gets_its_target() {
        let dir = tempfile::tempdir().expect("a directory");
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink("dotfiles/qwen/settings.json", &link).expect("symlink");
        let script = dir.path().join("qoop.sh");
        assert!(edit(&link, &target("qwen"), &script, false).expect("install"));
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("meta")
                .file_type()
                .is_symlink(),
            "the link was replaced by a file"
        );
        let real = dir.path().join("dotfiles/qwen/settings.json");
        assert!(
            std::fs::read_to_string(&real)
                .expect("the target was created")
                .contains("qoop.sh")
        );
        // Uninstalled, it is as it was: the link still there, dangling again.
        assert!(remove(&link, &script, false, true).expect("uninstall"));
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("the link survives the uninstall")
                .file_type()
                .is_symlink()
        );
        assert!(!real.exists(), "the file the install created is gone");
    }

    /// A leading `~` in an agent's home variable is the person's home, as a shell would
    /// have expanded it.
    #[test]
    fn a_tilde_in_a_home_variable_is_the_home_directory() {
        let home = Path::new("/home/u");
        for (value, expected) in [
            ("~/cp", "/home/u/cp/settings.json"),
            ("~", "/home/u/settings.json"),
            ("/abs/cp", "/abs/cp/settings.json"),
        ] {
            assert_eq!(
                config_path_in(&target("copilot"), home, Some(PathBuf::from(value))),
                PathBuf::from(expected),
                "{value}"
            );
        }
    }

    /// A settings file the person made read-only is refused, and left exactly as it is:
    /// a rename would replace it regardless, because only the directory's permission is
    /// asked.
    #[test]
    fn a_read_only_configuration_is_refused_and_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, "{\"mine\": true}\n").expect("write");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o444)).expect("chmod");
        let script = dir.path().join("qoop.sh");
        let err = edit(&config, &target("qwen"), &script, false).expect_err("read-only");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(
            std::fs::read_to_string(&config).expect("read"),
            "{\"mine\": true}\n"
        );
        let mode = std::fs::metadata(&config)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o444);
        assert!(
            kept_copies(&config).is_empty(),
            "a refused install left a backup behind"
        );
    }

    /// A configuration the install creates gets the mode the person's umask gives any new
    /// file, not a mode of qoop's choosing: with `umask 077` it is 0600.
    ///
    /// The umask is process-wide, so the check runs in a child of this test binary with
    /// the umask set there, rather than changing it under every other test running now.
    #[test]
    fn a_created_configuration_follows_the_umask() {
        let exe = std::env::current_exe().expect("the test binary");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"umask 077 && exec "$0" --exact hooks::tests::created_file_mode_under_umask_077 --ignored"#)
            .arg(&exe)
            .output()
            .expect("run the child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "the created file was not 0600 under umask 077: {stdout}"
        );
        // A renamed or filtered-out test runs nothing and succeeds; it must have run.
        assert!(
            stdout.contains("1 passed"),
            "the child ran no test: {stdout}"
        );
    }

    #[test]
    #[ignore = "run by a_created_configuration_follows_the_umask with umask 077 set"]
    fn created_file_mode_under_umask_077() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        let script = dir.path().join("qoop.sh");
        assert!(edit(&config, &target("qwen"), &script, false).expect("install"));
        let mode = std::fs::metadata(&config)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A session start hands the agent the task rules on its standard output, which is
    /// what Claude and Codex add to its context; Gemini only reads JSON there. Every other
    /// event prints nothing, outside qoop nothing prints, and a `qoop` without tasks (an
    /// older one on a remote) leaves the hook quiet and successful.
    #[test]
    fn a_session_start_prints_the_task_rules_and_nothing_else_does() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let fake = dir.path().join("qoop");
        let set_fake = |rules: &str| {
            std::fs::write(
                &fake,
                format!("#!/bin/sh\n[ \"$1\" = task ] && {rules}\nexit 0\n"),
            )
            .expect("write");
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        };
        let run = |agent: &str, payload: &str, inside: bool| -> String {
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg(&script)
                .arg("working")
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env_remove("QOOP_TERMINAL")
                .env_remove("QORO_TERMINAL")
                .env_remove("QOOP_TOKEN")
                .env_remove("QORO_TOKEN")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
            if inside {
                cmd.env("QOOP_TERMINAL", "1").env("QOOP_TOKEN", "t");
            }
            let mut child = cmd.spawn().expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            let out = child.wait_with_output().expect("wait");
            assert!(out.status.success(), "a hook must exit 0");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        set_fake("echo \"RULES LINE $3\"; exit 0");
        let start = r#"{"hook_event_name":"SessionStart","source":"startup"}"#;
        for agent in ["claude", "codex"] {
            assert_eq!(run(agent, start, true).trim(), "RULES LINE", "{agent}");
            assert_eq!(run(agent, start, false), "", "{agent} outside qoop");
            assert_eq!(
                run(
                    agent,
                    r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
                    true
                )
                .trim(),
                "RULES LINE --turn",
                "{agent} gets the line with each request"
            );
            assert_eq!(
                run(agent, r#"{"hook_event_name":"Stop"}"#, true),
                "",
                "{agent} stop"
            );
        }
        for source in ["resume", "clear", "compact"] {
            let payload = format!(r#"{{"hook_event_name":"SessionStart","source":"{source}"}}"#);
            assert_eq!(
                run("claude", &payload, true).trim(),
                "RULES LINE",
                "{source}"
            );
        }
        for source in ["resume", "clear"] {
            let payload = format!(r#"{{"hook_event_name":"SessionStart","source":"{source}"}}"#);
            assert_eq!(
                run("codex", &payload, true).trim(),
                "RULES LINE",
                "codex {source}"
            );
            let json: serde_json::Value =
                serde_json::from_str(&run("gemini", &payload, true)).expect("json");
            assert_eq!(
                json["hookSpecificOutput"]["additionalContext"],
                "RULES LINE"
            );
        }
        let gemini = run("gemini", r#"{"hook_event_name":"SessionStart"}"#, true);
        let json: serde_json::Value = serde_json::from_str(&gemini).expect("Gemini gets JSON only");
        assert_eq!(json["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(
            json["hookSpecificOutput"]["additionalContext"],
            "RULES LINE"
        );
        let turn: serde_json::Value =
            serde_json::from_str(&run("gemini", r#"{"hook_event_name":"BeforeAgent"}"#, true))
                .expect("Gemini gets JSON with each request too");
        assert_eq!(turn["hookSpecificOutput"]["hookEventName"], "BeforeAgent");
        assert_eq!(
            turn["hookSpecificOutput"]["additionalContext"],
            "RULES LINE --turn"
        );
        assert_eq!(
            run("gemini", r#"{"hook_event_name":"AfterAgent"}"#, true),
            ""
        );

        // A qoop that does not know `task` answers 2 and says so on stderr.
        set_fake("echo 'unknown command' >&2; exit 2");
        for agent in ["claude", "codex", "gemini"] {
            assert_eq!(run(agent, start, true), "", "{agent} with an older qoop");
        }
        // And one that prints nothing (qass's own space) prints nothing.
        set_fake("exit 0");
        for agent in ["claude", "codex", "gemini"] {
            assert_eq!(run(agent, start, true), "", "{agent} with nothing to say");
        }
    }

    /// The Claude permission for `qoop task` is added once, never over a rule that already
    /// covers it, and an uninstall takes back only one it added.
    #[test]
    fn the_task_permission_is_ours_to_add_and_ours_to_remove() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        let script = dir.path().join("qoop-hook.sh");
        let marker = dir.path().join(ALLOW_MARKER);
        let allow = |config: &Path| -> serde_json::Value {
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(config).expect("read"),
            )
            .expect("json")
            .pointer("/permissions/allow")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
        };
        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        assert_eq!(allow(&config), serde_json::json!(ALLOW));
        assert!(marker.exists());
        assert!(
            !ALLOW
                .iter()
                .any(|r| ["close", " add", " start"].iter().any(|w| r.contains(w))),
            "ending a task, or adding or starting work, is never pre-approved"
        );
        assert!(!edit(&config, &target("claude"), &script, false).expect("again"));
        assert!(remove(&config, &script, false, true).expect("remove"));
        assert!(
            !config.exists(),
            "a file the install created goes away whole"
        );

        // Beside the person's own settings, only ours goes.
        std::fs::write(
            &config,
            r#"{"model":"opus","permissions":{"allow":["Read"]}}"#,
        )
        .expect("write");
        assert!(edit(&config, &target("claude"), &script, false).expect("install"));
        let mut both = vec!["Read"];
        both.extend(ALLOW);
        assert_eq!(allow(&config), serde_json::json!(both));
        assert!(remove(&config, &script, false, true).expect("remove"));
        assert_eq!(allow(&config), serde_json::json!(["Read"]));

        // A person who already allows all of qoop keeps exactly their rule.
        let theirs = dir.path().join("theirs.json");
        std::fs::write(&theirs, r#"{"permissions":{"allow":["Bash(qoop:*)"]}}"#).expect("write");
        let elsewhere = tempfile::tempdir().expect("a directory");
        let their_script = elsewhere.path().join("qoop-hook.sh");
        assert!(edit(&theirs, &target("claude"), &their_script, false).expect("install"));
        assert!(
            !elsewhere.path().join(ALLOW_MARKER).exists(),
            "covered, so not ours"
        );
        assert_eq!(allow(&theirs), serde_json::json!(["Bash(qoop:*)"]));
        assert!(remove(&theirs, &their_script, false, true).expect("remove"));
        assert_eq!(
            allow(&theirs),
            serde_json::json!(["Bash(qoop:*)"]),
            "their rule stays"
        );

        // A person who already allows one of the subcommands keeps it after an uninstall.
        let mine = dir.path().join("mine.json");
        std::fs::write(
            &mine,
            r#"{"permissions":{"allow":["Bash(qoop task show:*)"]}}"#,
        )
        .expect("write");
        let third = tempfile::tempdir().expect("a directory");
        let third_script = third.path().join("qoop-hook.sh");
        assert!(edit(&mine, &target("claude"), &third_script, false).expect("install"));
        assert_eq!(allow(&mine).as_array().map(Vec::len), Some(ALLOW.len()));
        assert!(remove(&mine, &third_script, false, true).expect("remove"));
        assert_eq!(
            allow(&mine),
            serde_json::json!(["Bash(qoop task show:*)"]),
            "theirs stays"
        );
    }

    /// A sub-agent starting and ending is counted beside the tab, by the agent's own id for
    /// it, and never claims a state: Claude and Codex name it in their own events, Gemini runs
    /// one as an `invoke_agent` call. The end of a turn says what still runs where the agent
    /// knows it: Claude's own list, none for Gemini.
    #[test]
    fn every_hook_counts_sub_agents_and_claims_no_state_for_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let log = dir.path().join("calls");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let calls = |agent: &str, arg: &str, payload: &str| -> Vec<String> {
            let _ = std::fs::remove_file(&log);
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .arg(arg)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("hook-report"))
                .map(|l| {
                    // Every report names its agent; the rest of the line is what is checked.
                    let tag = format!("hook-report --agent {agent} ");
                    assert!(l.starts_with(&tag), "{agent}: {l}");
                    l.replacen(&tag, "hook-report ", 1)
                })
                .collect()
        };
        let only = |said: Vec<String>, want: &str| assert_eq!(said, vec![want.to_string()]);

        only(
            calls(
                "claude",
                "",
                r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#,
            ),
            "hook-report --subagent-start a1",
        );
        only(
            calls(
                "claude",
                "",
                r#"{"hook_event_name":"SubagentStop","agent_id":"a1","background_tasks":[]}"#,
            ),
            "hook-report --subagent-end a1",
        );
        // Claude's turn ends with its own list: running sub-agents only, shells left out, sent
        // before the state so the daemon judges the turn's end on this turn's own count.
        let said = calls(
            "claude",
            "",
            r#"{"hook_event_name":"Stop","background_tasks":[
                {"id":"a2","type":"subagent","status":"running"},
                {"id":"a3","type":"subagent","status":"completed"},
                {"id":"b1","type":"shell","status":"running"},
                {"id":"a4","type":"subagent","status":"running"}]}"#,
        );
        assert_eq!(said[0], "hook-report --subagents a2,a4", "{said:?}");
        assert!(said[1].starts_with("hook-report --state idle"), "{said:?}");
        // A running workflow counts too, sent as a start since it fires no hook of its own; a
        // teammate stays out: an idle one is listed as running, and would hold every
        // question at the end of its lead's turn.
        let said = calls(
            "claude",
            "",
            r#"{"hook_event_name":"Stop","background_tasks":[
                {"id":"a2","type":"subagent","status":"running"},
                {"id":"w1","type":"workflow","status":"running"},
                {"id":"t1","type":"teammate","status":"running"}]}"#,
        );
        assert_eq!(said.len(), 3, "{said:?}");
        assert_eq!(said[0], "hook-report --subagents a2,w1", "{said:?}");
        assert_eq!(said[1], "hook-report --subagent-start w1", "{said:?}");
        assert!(said[2].starts_with("hook-report --state idle"), "{said:?}");
        let said = calls("claude", "", r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(
            said.len(),
            1,
            "a build without the list keeps the count: {said:?}"
        );
        // Odd payloads say nothing wrong and still exit 0.
        let said = calls(
            "claude",
            "",
            r#"{"hook_event_name":"Stop","background_tasks":"none"}"#,
        );
        assert_eq!(said.len(), 1, "{said:?}");
        let said = calls(
            "claude",
            "",
            r#"{"hook_event_name":"Stop","background_tasks":[7,{"id":3,"type":"subagent","status":"running"}]}"#,
        );
        assert_eq!(said[0], "hook-report --subagents 3", "{said:?}");

        only(
            calls(
                "codex",
                "subagent",
                r#"{"hook_event_name":"SubagentStart","agent_id":"019a-c1","agent_type":"default"}"#,
            ),
            "hook-report --subagent-start 019a-c1",
        );
        only(
            calls(
                "codex",
                "subagent",
                r#"{"hook_event_name":"SubagentStop","agent_id":"019a-c1"}"#,
            ),
            "hook-report --subagent-end 019a-c1",
        );

        let call = r#""tool_name":"invoke_agent","tool_input":{"agent_name":"codebase_investigator","prompt":"map it"}"#;
        let said = calls(
            "gemini",
            "working",
            &format!(r#"{{"hook_event_name":"BeforeTool",{call}}}"#),
        );
        let started = said[0]
            .strip_prefix("hook-report --subagent-start ")
            .expect("a start");
        assert!(
            said[1].starts_with("hook-report --state working"),
            "{said:?}"
        );
        only(
            calls(
                "gemini",
                "subagent",
                &format!(r#"{{"hook_event_name":"AfterTool",{call}}}"#),
            ),
            &format!("hook-report --subagent-end {started}"),
        );
        assert!(
            calls(
                "gemini",
                "subagent",
                r#"{"hook_event_name":"AfterTool","tool_name":"read_file"}"#,
            )
            .is_empty(),
            "any other tool ending says nothing"
        );
        let said = calls(
            "gemini",
            "working",
            r#"{"hook_event_name":"BeforeTool","tool_name":"invoke_agent","tool_input":"just text"}"#,
        );
        assert!(
            said[0].starts_with("hook-report --subagent-start "),
            "{said:?}"
        );
        let said = calls("gemini", "idle", r#"{"hook_event_name":"AfterAgent"}"#);
        assert_eq!(said[1], "hook-report --subagents ", "{said:?}");
    }

    /// An edit attempt, by the agent or one of its subagents, opens the terminal's task
    /// through `qoop task begin`; nothing else does. Codex edits through `apply_patch`,
    /// Gemini through `write_file` and `replace`. Codex and Gemini send the prompt head.
    #[test]
    fn an_edit_attempt_begins_the_task_and_nothing_else_does() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let log = dir.path().join("calls");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let calls = |agent: &str, arg: &str, payload: &str| -> String {
            let _ = std::fs::remove_file(&log);
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .arg(arg)
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        let begins = |said: &str| said.lines().any(|l| l == "task begin");
        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            let said = calls(
                "claude",
                "",
                &format!(r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}"}}"#),
            );
            assert!(begins(&said), "{tool}: {said:?}");
        }
        let said = calls(
            "claude",
            "",
            r#"{"hook_event_name":"PreToolUse","tool_name":"Edit","agent_id":"sub1"}"#,
        );
        assert!(begins(&said), "a subagent's edit is work too: {said:?}");
        assert!(
            !said.contains("hook-report"),
            "and still says nothing about state"
        );
        for payload in [
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"Read"}"#,
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"edit it"}"#,
        ] {
            assert!(!begins(&calls("claude", "", payload)), "{payload}");
        }
        let said = calls(
            "codex",
            "working",
            r#"{"hook_event_name":"PreToolUse","tool_name":"apply_patch"}"#,
        );
        assert!(begins(&said), "codex apply_patch: {said:?}");
        let said = calls(
            "codex",
            "working",
            r#"{"hook_event_name":"PreToolUse","tool_name":"shell","tool_input":{"command":["apply_patch","*** Begin Patch"]}}"#,
        );
        assert!(
            begins(&said),
            "codex apply_patch through the shell: {said:?}"
        );
        assert!(!begins(&calls(
            "codex",
            "working",
            r#"{"hook_event_name":"PreToolUse","tool_name":"shell","tool_input":{"command":["ls"]}}"#,
        )));
        for tool in ["write_file", "replace"] {
            let said = calls(
                "gemini",
                "working",
                &format!(r#"{{"hook_event_name":"BeforeTool","tool_name":"{tool}"}}"#),
            );
            assert!(begins(&said), "gemini {tool}: {said:?}");
        }
        assert!(!begins(&calls(
            "gemini",
            "working",
            r#"{"hook_event_name":"BeforeTool","tool_name":"read_file"}"#,
        )));
        let said = calls(
            "codex",
            "working",
            r#"{"hook_event_name":"UserPromptSubmit","prompt":"make it  shout\nplease"}"#,
        );
        assert!(
            said.contains("--event user --label make it shout please"),
            "{said:?}"
        );
        let said = calls(
            "gemini",
            "working",
            r#"{"hook_event_name":"BeforeAgent","prompt":"make it shout"}"#,
        );
        assert!(
            said.contains("--event user --label make it shout"),
            "{said:?}"
        );
    }

    /// A turn that changed the files or the history, by whatever tool (an agent may write
    /// through its shell, not its edit tool), begins the task at its end; a turn that changed
    /// nothing does not. Measured on a real Claude: it wrote both files through Bash.
    #[test]
    fn a_turn_that_changed_the_repo_begins_the_task() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        let log = dir.path().join("calls");
        let fake = dir.path().join("qoop");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n", log.display()),
        )
        .expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let turns = dir.path().join("turns");
        std::fs::create_dir(&turns).expect("turns");
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).expect("a directory outside git");
        let run_in = |agent: &str, event: &str, cwd: &std::path::Path| -> String {
            let _ = std::fs::remove_file(&log);
            let script = dir.path().join(format!("{agent}.sh"));
            std::fs::write(&script, target(agent).body).expect("write");
            let payload = format!(
                r#"{{"hook_event_name":"{event}","session_id":"s-{agent}","cwd":"{}"}}"#,
                cwd.display()
            );
            let mut child = std::process::Command::new("/bin/sh")
                .arg(&script)
                .arg("working")
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .env("QOOP_TERMINAL", "1")
                .env("QOOP_TOKEN", "t")
                .env("TMPDIR", &turns)
                .env_remove("XDG_RUNTIME_DIR")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("run the hook");
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(payload.as_bytes())
                .expect("payload");
            assert!(child.wait().expect("wait").success(), "a hook must exit 0");
            std::fs::read_to_string(&log).unwrap_or_default()
        };
        let run = |agent: &str, event: &str| run_in(agent, event, &repo);
        let begins = |said: &str| said.lines().any(|l| l == "task begin");
        for (agent, starts, ends) in [
            ("claude", "UserPromptSubmit", "Stop"),
            ("codex", "UserPromptSubmit", "Stop"),
            ("gemini", "BeforeAgent", "AfterAgent"),
        ] {
            run(agent, starts);
            assert!(
                !begins(&run(agent, ends)),
                "{agent}: nothing changed, no task"
            );
            run(agent, starts);
            std::fs::write(repo.join("a.txt"), format!("changed by {agent}\n")).expect("write");
            assert!(
                begins(&run(agent, ends)),
                "{agent}: a file changed in the turn"
            );
            run(agent, starts);
            git(&["commit", "-qam", agent]);
            assert!(
                begins(&run(agent, ends)),
                "{agent}: a commit is a change too"
            );
            run(agent, starts);
            std::fs::write(repo.join("a.txt"), format!("again by {agent}\n")).expect("write");
            assert!(
                !begins(&run_in(agent, ends, &plain)),
                "{agent}: a turn that ends in another directory compares nothing"
            );
            run(agent, starts);
            run_in(agent, starts, &plain);
            std::fs::write(repo.join("a.txt"), format!("last by {agent}\n")).expect("write");
            assert!(
                !begins(&run(agent, ends)),
                "{agent}: a turn begun outside git leaves no old mark behind"
            );
        }
    }
}

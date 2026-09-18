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
//! a person did not write and may care about more than they care about qoro.

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
    /// `hooks.<event>[] = { command }`. Cursor's, which has no wrapper object.
    Flat { events: &'static [Event] },
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
            body: include_str!("../../../hooks/claude/qoro-hook.sh"),
            file: "qoro-hook.sh",
            config: ".claude/settings.json",
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
                    Event("SubagentStop", None),
                    Event("Stop", None),
                    Event("SessionEnd", None),
                ],
                matcher: Some(""),
            },
        },
        Target {
            agent: "codex",
            body: include_str!("../../../hooks/codex/qoro-hook.sh"),
            file: "qoro-codex-hook.sh",
            config: ".codex/hooks.json",
            shape: Shape::Nested {
                // Codex runs a hook per event and passes nothing to read, so which state
                // each event means is an argument here.
                events: &[
                    Event("SessionStart", Some("working")),
                    Event("PreToolUse", Some("working")),
                    Event("PostToolUse", Some("working")),
                    Event("PermissionRequest", Some("blocked")),
                    Event("Stop", Some("idle")),
                ],
                matcher: None,
            },
        },
        Target {
            agent: "gemini",
            body: include_str!("../../../hooks/gemini/qoro-hook.sh"),
            file: "qoro-gemini-hook.sh",
            config: ".gemini/settings.json",
            shape: Shape::Nested {
                events: &[
                    Event("SessionStart", Some("working")),
                    Event("BeforeAgent", Some("working")),
                    Event("AfterAgent", Some("idle")),
                    Event("SessionEnd", Some("idle")),
                ],
                matcher: Some("*"),
            },
        },
        Target {
            agent: "cursor",
            body: include_str!("../../../hooks/cursor/qoro-hook.sh"),
            file: "qoro-cursor-hook.sh",
            config: ".cursor/hooks.json",
            shape: Shape::Flat {
                events: &[
                    Event("sessionStart", Some("working")),
                    Event("postToolUse", Some("working")),
                    Event("stop", Some("idle")),
                ],
            },
        },
        Target {
            agent: "opencode",
            body: include_str!("../../../hooks/opencode/qoro-plugin.js"),
            file: "qoro-plugin.js",
            config: ".config/opencode/plugin",
            shape: Shape::Dropped,
        },
    ]
}

/// Where the installed scripts live.
///
/// Beside the agent-detection overrides, under the person's own configuration, because
/// that is where a thing they can read and edit belongs. Not in the runtime directory:
/// that is for sockets and databases and is thrown away.
fn hooks_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .ok()
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("qoro").join("hooks")
}

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Agents that are on this machine and whose hook is not installed.
///
/// What a first start says out loud. An agent is read from its screen with no hook at all,
/// so this is never an error and never blocks anything; it is the one line that tells a
/// person a better reading is one command away, and it says nothing at all once they have
/// run it or if they have no agents installed.
pub fn missing() -> Vec<&'static str> {
    let dir = hooks_dir();
    let Some(home) = home() else {
        return Vec::new();
    };
    targets()
        .into_iter()
        .filter(|target| on_path(target.agent))
        .filter(|target| !registered(target, &dir, &home))
        .map(|target| target.agent)
        .collect()
}

/// Whether this agent's hook is registered where that agent will read it.
fn registered(target: &Target, dir: &Path, home: &Path) -> bool {
    if matches!(target.shape, Shape::Dropped) {
        return home.join(target.config).join(target.file).exists();
    }
    let script = dir.join(target.file).display().to_string();
    std::fs::read_to_string(home.join(target.config))
        .map(|text| text.contains(&script))
        .unwrap_or(false)
}

/// Whether a person has this agent at all.
///
/// Read from `PATH` directly rather than by running anything: asking six agents to start is
/// six processes on every first start, and what is wanted is whether the file is there.
fn on_path(agent: &str) -> bool {
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
    // The name a person types, which is not always the name of the binary.
    let binary = match agent {
        "cursor" => "cursor-agent",
        other => other,
    };
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .any(|dir| Path::new(dir).join(binary).exists())
}

/// `qoro hooks ...`
pub fn run(args: &[String]) -> std::io::Result<()> {
    match args.get(2).map(String::as_str) {
        Some("install") => install(&args[3..]),
        Some("uninstall") => uninstall(&args[3..]),
        Some("status") | None => status(),
        Some(other) => Err(std::io::Error::other(format!(
            "unknown command {other}; usage: qoro hooks <install|uninstall|status> [agent...]"
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
                "no hook for {name}; qoro ships one for {}",
                known.join(", ")
            )));
        };
        out.push(target);
    }
    Ok(out)
}

fn status() -> std::io::Result<()> {
    let dir = hooks_dir();
    for target in targets() {
        let installed = dir.join(target.file);
        let current = std::fs::read_to_string(&installed)
            .map(|on_disk| on_disk == target.body)
            .unwrap_or(false);
        let (registered, current) = if matches!(target.shape, Shape::Dropped) {
            // The file being in the directory is the whole of it.
            let dropped = home().map(|h| h.join(target.config).join(target.file));
            let body = dropped.and_then(|p| std::fs::read_to_string(p).ok());
            (body.is_some(), body.as_deref() == Some(target.body))
        } else {
            let registered = home()
                .map(|h| h.join(target.config))
                .and_then(|c| std::fs::read_to_string(c).ok())
                .map(|text| text.contains(&installed.display().to_string()))
                .unwrap_or(false);
            (registered, current)
        };
        let state = match (registered, current) {
            (false, _) => "not installed",
            (true, false) => "installed, and older than this build: run install again",
            (true, true) => "installed",
        };
        println!("  {:<10} {state}", target.agent);
    }
    println!("\n  qoro hooks install [agent...]    add them, beside whatever is already there");
    println!("  qoro hooks uninstall [agent...]  remove exactly what was added");
    Ok(())
}

fn install(args: &[String]) -> std::io::Result<()> {
    let dir = hooks_dir();
    std::fs::create_dir_all(&dir)?;
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to install into"));
    };
    for target in wanted(args)? {
        let script = dir.join(target.file);
        std::fs::write(&script, target.body)?;
        if target.file.ends_with(".sh") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
        }
        let config = home.join(target.config);
        match edit(&config, &target, &script) {
            Ok(true) => println!(
                "  {:<10} installed, and {} updated",
                target.agent, target.config
            ),
            Ok(false) => println!("  {:<10} already installed", target.agent),
            Err(e) => println!(
                "  {:<10} script written, {} not edited: {e}",
                target.agent, target.config
            ),
        }
    }
    println!("\nan agent already running does not read its configuration again: start it afresh.");
    Ok(())
}

fn uninstall(args: &[String]) -> std::io::Result<()> {
    let dir = hooks_dir();
    let Some(home) = home() else {
        return Err(std::io::Error::other("no home directory to uninstall from"));
    };
    for target in wanted(args)? {
        let script = dir.join(target.file);
        let config = home.join(target.config);
        if matches!(target.shape, Shape::Dropped) {
            let dropped = config.join(target.file);
            match std::fs::remove_file(&dropped) {
                Ok(()) => println!("  {:<10} removed from {}", target.agent, target.config),
                Err(_) => println!("  {:<10} was not there", target.agent),
            }
            continue;
        }
        match remove(&config, &script) {
            Ok(true) => println!("  {:<10} removed from {}", target.agent, target.config),
            Ok(false) => println!("  {:<10} was not there", target.agent),
            Err(e) => println!("  {:<10} {} not edited: {e}", target.agent, target.config),
        }
        let _ = std::fs::remove_file(&script);
    }
    Ok(())
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
fn write(config: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(parent) = config.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A copy before every write, not only the first: the second write is the one that
    // happens after a person has spent an afternoon on the file.
    if config.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = config.with_extension(format!("qoro-backup-{stamp}"));
        std::fs::copy(config, &backup)?;
    }
    std::fs::write(config, serde_json::to_string_pretty(value)? + "\n")
}

/// Add this hook to a configuration. Answers whether anything changed.
fn edit(config: &Path, target: &Target, script: &Path) -> std::io::Result<bool> {
    let mut root = read(config)?;
    let path = script.display().to_string();
    let changed = match &target.shape {
        Shape::Nested { events, matcher } => {
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
        Shape::Flat { events } => {
            // Cursor's file carries a version of its own schema, and refuses one without.
            if root.get("version").is_none() {
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
    if changed {
        write(config, &root)?;
    }
    Ok(changed)
}

/// Take this hook back out. Answers whether anything changed.
///
/// Recognised by the path it points at, which is why the script goes in a directory of its
/// own: an entry mentioning that path is one this put there, and nothing else in the file
/// can be mistaken for it.
fn remove(config: &Path, script: &Path) -> std::io::Result<bool> {
    if !config.exists() {
        return Ok(false);
    }
    let mut root = read(config)?;
    let path = script.display().to_string();
    let mut changed = false;
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for (_, list) in hooks.iter_mut() {
            let Some(groups) = list.as_array_mut() else {
                continue;
            };
            let before = groups.len();
            for group in groups.iter_mut() {
                if let Some(inner) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    let was = inner.len();
                    inner.retain(|entry| !says(entry, &path));
                    changed |= inner.len() != was;
                }
            }
            // A group left holding nothing is a group this emptied.
            groups.retain(|group| {
                !says(group, &path)
                    && group
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .map(|h| !h.is_empty())
                        .unwrap_or(true)
            });
            changed |= groups.len() != before;
        }
        hooks.retain(|_, list| list.as_array().map(|l| !l.is_empty()).unwrap_or(true));
    }
    if let Some(plugins) = root.get_mut("plugin").and_then(|p| p.as_array_mut()) {
        let before = plugins.len();
        plugins.retain(|entry| !says(entry, &path));
        changed |= plugins.len() != before;
    }
    if changed {
        write(config, &root)?;
    }
    Ok(changed)
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

    /// Installing adds beside what is there, and never over it.
    ///
    /// The whole reason this is a gate: a person's own hooks are theirs, and an installer
    /// that replaced them would lose work in a way that looks like it worked.
    #[test]
    fn installing_keeps_every_setting_and_every_hook_that_was_already_there() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, serde_json::to_string(&existing()).expect("json")).expect("write");
        let script = dir.path().join("qoro-hook.sh");

        assert!(edit(&config, &target("claude"), &script).expect("install"));
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
                .contains("qoro-hook.sh")
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
            .filter(|e| e.file_name().to_string_lossy().contains("qoro-backup"))
            .collect();
        assert_eq!(backups.len(), 1, "no copy was kept before writing");
    }

    /// Installing twice changes nothing the second time.
    #[test]
    fn installing_again_changes_nothing() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        let script = dir.path().join("qoro-hook.sh");
        assert!(edit(&config, &target("claude"), &script).expect("install"));
        let once = std::fs::read_to_string(&config).expect("read");
        assert!(
            !edit(&config, &target("claude"), &script).expect("install again"),
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
        let script = dir.path().join("qoro-hook.sh");
        edit(&config, &target("claude"), &script).expect("install");

        assert!(remove(&config, &script).expect("uninstall"));
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
        assert!(!remove(&config, &script).expect("uninstall again"));
    }

    /// The flat shape and the plugin shape are written the way their agent reads them.
    #[test]
    fn each_agent_gets_the_shape_its_own_configuration_uses() {
        let dir = tempfile::tempdir().expect("a directory");
        let script = dir.path().join("qoro-hook.sh");

        // Cursor: a version, and entries with nothing around them.
        let cursor = dir.path().join("cursor.json");
        edit(&cursor, &target("cursor"), &script).expect("install");
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
                .ends_with(" working"),
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

    /// A configuration that is not JSON is reported, not overwritten.
    #[test]
    fn a_file_that_cannot_be_read_is_left_exactly_as_it_is() {
        let dir = tempfile::tempdir().expect("a directory");
        let config = dir.path().join("settings.json");
        std::fs::write(&config, "{ this is not json").expect("write");
        let script = dir.path().join("qoro-hook.sh");
        let err = edit(&config, &target("claude"), &script).expect_err("not JSON");
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
                target.body.contains("qoro"),
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
}

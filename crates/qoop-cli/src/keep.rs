//! The daemon's look for agents that arrived after qoop.
//!
//! qoop sets up an agent's hook and skill when qoop itself is installed or updated, and a new
//! computer often gets its agents later: Gemini put on a server a week after it was added, a
//! Claude Code installed the day after qoop. Every daemon (this machine's, each ssh computer's)
//! looks at start and then every few seconds, and sets up any agent that has neither. Only what
//! is missing: a hook or skill that is there but older is the update's to refresh, and one a
//! person removed on purpose (`qoop hooks uninstall <agent>`) stays removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often a daemon looks. A few `stat`s and the configuration files of the agents present,
/// and nothing is written when all of them are set up.
const EVERY: Duration = Duration::from_secs(10);

pub struct Keeper {
    home: PathBuf,
    config: PathBuf,
    exe: PathBuf,
    /// The binary this daemon runs, as it was when the daemon started.
    born: (u64, u64),
}

impl Keeper {
    /// The keeper of this daemon, or none when its home is not a person's own: a test's
    /// sandbox, a build run from a cargo `target` directory, or a configuration directory
    /// other than `~/.config/qoop` (an isolated stack with a temporary one would register a
    /// script in that temporary directory in the person's real agent settings, and a daemon
    /// that reads another directory than its person's shell would set up a second hook).
    /// `born` is the binary's identity read at the very start of the daemon, before anything
    /// could replace it.
    pub fn for_this(exe: &Path, born: Option<(u64, u64)>) -> Option<Self> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let config = qoop_runtime::paths::config_dir();
        let sandboxed = std::env::var_os(qoop_runtime::legacy::SANDBOX_ENV).is_some();
        let born = born?;
        wanted(exe, &home, &config, sandboxed).then(|| Self {
            home,
            config,
            exe: exe.to_path_buf(),
            born,
        })
    }

    /// Look now, then every [`EVERY`], until this daemon's binary is replaced: from then on the
    /// new binary's install and its daemon do this, and an old daemon would write old bytes.
    pub async fn run(self) {
        let mut said = BTreeMap::new();
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let (home, config, exe, born) = (
                self.home.clone(),
                self.config.clone(),
                self.exe.clone(),
                self.born,
            );
            let looked = tokio::task::spawn_blocking(move || {
                // Under the lock every install and uninstall holds, and with the binary
                // checked again inside it: an update that swapped it while this waited has
                // already set things up with the new one.
                let _held = crate::hooks::setup_lock(&config)?;
                if identity(&exe) != Some(born) {
                    return Ok(None);
                }
                let path = std::env::var("PATH").unwrap_or_default();
                Ok(Some(look(
                    &home,
                    &config,
                    &path,
                    &crate::hooks::agent_dirs(&home),
                )))
            })
            .await;
            let done = match looked {
                Ok(Ok(Some(done))) => done,
                Ok(Ok(None)) => return,
                Ok(Err(e)) => vec![("setup", "agents", Err::<bool, std::io::Error>(e))],
                Err(e) => vec![("setup", "agents", Err(std::io::Error::other(e.to_string())))],
            };
            for line in report(done, &mut said) {
                qoop_runtime::say!("qoopd: {line}");
            }
        }
    }
}

fn wanted(exe: &Path, home: &Path, config: &Path, sandboxed: bool) -> bool {
    !sandboxed
        && !exe.components().any(|part| part.as_os_str() == "target")
        && config == home.join(".config/qoop")
}

/// Which file the binary at this path is, so a replaced one is noticed.
pub fn identity(exe: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(exe).ok().map(|m| (m.dev(), m.ino()))
}

/// What a look did: each hook or skill it tried, for which agent, and whether it wrote.
type Done = Vec<(&'static str, &'static str, std::io::Result<bool>)>;

/// One look: every agent on this machine whose hook or skill is missing, set up.
fn look(home: &Path, config: &Path, path: &str, dirs: &[PathBuf]) -> Done {
    let hooks = crate::hooks::keep(home, config, path, dirs)
        .into_iter()
        .map(|(agent, done)| ("hook", agent, done));
    let skills = crate::skills::keep(home, config, path, dirs)
        .into_iter()
        .map(|(agent, done)| ("skill", agent, done));
    hooks.chain(skills).collect()
}

/// What a look is worth saying: each setup once, each failure once until it changes or clears,
/// so a settings file that cannot be written is one line in the log, not one every look.
fn report(done: Done, said: &mut BTreeMap<(&'static str, &'static str), String>) -> Vec<String> {
    let mut lines = Vec::new();
    for (what, agent, outcome) in done {
        match outcome {
            Ok(changed) => {
                said.remove(&(what, agent));
                if changed {
                    lines.push(format!("set up the qoop {what} for {agent}"));
                }
            }
            Err(e) => {
                let e = e.to_string();
                if said.get(&(what, agent)) != Some(&e) {
                    lines.push(format!("the qoop {what} for {agent} is not set up: {e}"));
                    said.insert((what, agent), e);
                }
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_persons_own_daemon_looks() {
        let home = Path::new("/home/a");
        let bin = Path::new("/home/a/.local/bin/qoop");
        let config = Path::new("/home/a/.config/qoop");
        assert!(wanted(bin, home, config, false));
        assert!(!wanted(bin, home, config, true), "a test's sandbox");
        assert!(
            !wanted(
                Path::new("/src/qoop/target/release/qoop"),
                home,
                config,
                false
            ),
            "a build run by hand"
        );
        assert!(
            !wanted(bin, home, Path::new("/tmp/qv/config/qoop"), false),
            "an isolated stack with the real home"
        );
        assert!(
            !wanted(bin, home, Path::new("/home/a/.cache/stack/qoop"), false),
            "another configuration directory, even inside the home"
        );
    }

    #[test]
    fn a_failure_is_said_once_until_it_changes_or_clears() {
        let mut said = BTreeMap::new();
        let fail = || vec![("hook", "gemini", Err(std::io::Error::other("read-only")))];
        assert_eq!(report(fail(), &mut said).len(), 1);
        assert!(
            report(fail(), &mut said).is_empty(),
            "the same failure again"
        );
        assert!(report(vec![("hook", "gemini", Ok(false))], &mut said).is_empty());
        assert_eq!(report(fail(), &mut said).len(), 1, "back after it cleared");
        assert_eq!(
            report(vec![("skill", "gemini", Ok(true))], &mut said),
            vec!["set up the qoop skill for gemini".to_string()]
        );
    }

    /// The whole look on a home that just got Claude and Gemini in `~/.local/bin`: both are set
    /// up, a second look writes nothing, and an agent removed on purpose stays removed.
    #[test]
    fn a_look_sets_up_what_arrived_once_and_respects_a_removal() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("home");
        let home = home.path();
        let config = home.join(".config/qoop");
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).expect("bin");
        for agent in ["claude", "gemini"] {
            std::fs::write(bin.join(agent), "#!/bin/sh\n").expect("agent");
            std::fs::set_permissions(bin.join(agent), std::fs::Permissions::from_mode(0o755))
                .expect("mode");
        }
        crate::hooks::set_off(&config, "skills", &["claude"], true).expect("off");

        // Only this home's own folder, so nothing on the machine running the test counts.
        let look = || look(home, &config, "", std::slice::from_ref(&bin));
        let first = look();
        let set: Vec<_> = first
            .iter()
            .filter(|(_, _, done)| matches!(done, Ok(true)))
            .map(|(what, agent, _)| format!("{what} {agent}"))
            .collect();
        assert_eq!(
            set,
            ["hook claude", "hook gemini", "skill gemini"],
            "{first:?}"
        );
        let gemini = std::fs::read_to_string(home.join(".gemini/settings.json")).expect("gemini");
        assert!(gemini.contains("qoop-gemini-hook.sh"), "{gemini}");
        assert!(home.join(".gemini/skills/qoop/SKILL.md").exists());
        assert!(
            !home.join(".claude/skills/qoop/SKILL.md").exists(),
            "removed on purpose"
        );

        let settings = home.join(".claude/settings.json");
        let before = std::fs::metadata(&settings)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert!(look().is_empty(), "nothing left to do");
        let after = std::fs::metadata(&settings)
            .and_then(|m| m.modified())
            .expect("mtime");
        assert_eq!(before, after, "a second look writes nothing");
    }
}

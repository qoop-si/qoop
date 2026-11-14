//! A herdr plugin's `herdr-plugin.toml`, as qoop reads it.
//!
//! Lenient where herdr is lenient: a key qoop does not read (`name`, `description`, a pane's
//! size, `[[link_handlers]]`) is ignored, so a manifest written for a newer herdr still
//! installs. Strict where a mistake would run something unexpected: the id must be one qoop
//! can use as a directory name, and every command is a non-empty argv.

use serde::Deserialize;

/// The herdr release qoop answers as. A plugin that asks for a newer one is warned about.
pub(crate) const HERDR_VERSION: &str = super::translate::VERSION;

/// The name of the file at a plugin's root.
pub(crate) const FILE: &str = "herdr-plugin.toml";

/// The most of a manifest qoop reads: far beyond any real one, and a bound on what a
/// repository can make it hold in memory or in the consent ledger.
pub(crate) const MAX_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) min_herdr_version: Option<String>,
    /// Where the plugin runs at all. Absent is everywhere; an empty list is nowhere, since
    /// herdr reads the list as the platforms the plugin declares it can run on.
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) build: Vec<Step>,
    #[serde(default)]
    pub(crate) actions: Vec<Action>,
    #[serde(default)]
    pub(crate) panes: Vec<PaneEntry>,
    #[serde(default)]
    pub(crate) events: Vec<Hook>,
    #[serde(default)]
    pub(crate) startup: Vec<Step>,
}

/// A `[[build]]` or `[[startup]]` entry: one command, on some platforms.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Step {
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Action {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// `global`, `workspace`, `tab`, `pane`, `selection`; none is everywhere.
    #[serde(default)]
    pub(crate) contexts: Vec<String>,
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct PaneEntry {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// `overlay`, `popup`, `split`, `tab` or `zoomed`; herdr's default when absent is overlay.
    #[serde(default)]
    pub(crate) placement: Option<String>,
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Hook {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) on: String,
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
}

/// This machine, in herdr's platform names.
fn this_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    }
}

/// Whether an entry with these `platforms` runs on this machine: absent is everywhere, a list
/// (an empty one too) is only where it names.
pub(crate) fn runs_here(platforms: Option<&[String]>) -> bool {
    platforms.is_none_or(|p| p.iter().any(|name| name == this_os()))
}

/// Whether `id` can name a plugin: lowercase, a letter or digit first, at most 64 bytes, and
/// nothing a path could read as a separator or a parent.
pub(crate) fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.len() <= 64
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        // The consent ledger and its lock sit beside the plugin folders.
        && !matches!(id, "consent.json" | "consent.lock")
}

impl Manifest {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self =
            toml::from_str(text).map_err(|e| format!("{FILE} does not parse: {e}"))?;
        if !valid_id(&manifest.id) {
            return Err(format!(
                "{FILE}: id {:?} is not a plugin id (a-z, 0-9, `.`, `_`, `-`, at most 64)",
                manifest.id
            ));
        }
        let commands = manifest
            .build
            .iter()
            .chain(&manifest.startup)
            .map(|s| &s.command)
            .chain(manifest.actions.iter().map(|a| &a.command))
            .chain(manifest.panes.iter().map(|p| &p.command))
            .chain(manifest.events.iter().map(|e| &e.command));
        for command in commands {
            if command.first().is_none_or(String::is_empty) {
                return Err(format!("{FILE}: a command with no program"));
            }
        }
        Ok(manifest)
    }

    /// Whether the plugin runs on this machine at all, by its top-level `platforms`.
    pub(crate) fn runs_here(&self) -> bool {
        runs_here(self.platforms.as_deref())
    }

    /// The manifest as this machine runs it: only the entries for this platform.
    pub(crate) fn here(&self) -> Self {
        let mut out = self.clone();
        out.build.retain(|s| runs_here(s.platforms.as_deref()));
        out.startup.retain(|s| runs_here(s.platforms.as_deref()));
        out.actions.retain(|a| runs_here(a.platforms.as_deref()));
        out.panes.retain(|p| runs_here(p.platforms.as_deref()));
        out.events.retain(|e| runs_here(e.platforms.as_deref()));
        out
    }

    /// Whether it asks for a newer herdr than qoop answers as.
    pub(crate) fn wants_newer_herdr(&self) -> bool {
        self.min_herdr_version
            .as_deref()
            .is_some_and(|wanted| newer(wanted, HERDR_VERSION))
    }
}

/// Whether dotted version `a` is above `b`, number by number. A part that is not a number (a
/// `-beta` suffix) counts as zero, which reads a pre-release as its release.
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            })
            .collect()
    };
    let (a, b) = (parts(a), parts(b));
    let len = a.len().max(b.len());
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| at(&a, i).cmp(&at(&b, i)))
        .find(|o| o.is_ne())
        .is_some_and(std::cmp::Ordering::is_gt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_TABLE: &str = r#"
id = "pat.notes"
name = "Notes"
version = "1.2.3"
min_herdr_version = "0.8.0"
description = "Keep notes beside the agent."
platforms = ["linux", "macos"]
homepage = "a key qoop does not know"

[[build]]
command = ["sh", "scripts/build.sh"]

[[build]]
command = ["powershell", "build.ps1"]
platforms = ["windows"]

[[actions]]
id = "open"
title = "Open notes"
contexts = ["pane", "workspace"]
command = ["bin/notes", "--open"]

[[actions]]
id = "everywhere"
title = "Everywhere"
command = ["bin/notes"]

[[panes]]
id = "editor"
title = "Notes"
placement = "popup"
width = "74%"
height = 24
command = ["bin/notes", "--pane"]

[[events]]
id = "on-close"
on = "pane.closed"
command = ["bin/notes", "--closed"]

[[events]]
on = "workspace.created"
command = ["bin/notes", "--new"]
platforms = ["windows"]

[[startup]]
command = ["bin/notes", "--daemon"]

[[link_handlers]]
id = "md"
title = "Markdown"
pattern = "^file://.*\\.md$"
action = "open"
"#;

    #[test]
    fn every_table_is_read() {
        let m = Manifest::parse(EVERY_TABLE).expect("parses");
        assert_eq!(m.id, "pat.notes");
        assert_eq!(m.version.as_deref(), Some("1.2.3"));
        assert_eq!(m.min_herdr_version.as_deref(), Some("0.8.0"));
        assert_eq!(m.platforms, Some(vec!["linux".into(), "macos".into()]));
        assert_eq!(m.build.len(), 2);
        assert_eq!(m.build[1].platforms, Some(vec!["windows".into()]));
        assert_eq!(
            m.actions[0],
            Action {
                id: "open".into(),
                title: Some("Open notes".into()),
                contexts: vec!["pane".into(), "workspace".into()],
                command: vec!["bin/notes".into(), "--open".into()],
                platforms: None,
            }
        );
        assert!(m.actions[1].contexts.is_empty());
        assert_eq!(m.panes[0].placement.as_deref(), Some("popup"));
        assert_eq!(m.panes[0].title.as_deref(), Some("Notes"));
        assert_eq!(m.events[0].id.as_deref(), Some("on-close"));
        assert_eq!(m.events[0].on, "pane.closed");
        assert_eq!(m.events[1].id, None);
        assert_eq!(m.startup[0].command, ["bin/notes", "--daemon"]);
        assert!(!m.wants_newer_herdr());
        assert!(m.runs_here(), "this test runs on linux or macos");
    }

    #[test]
    fn here_keeps_only_this_platforms_entries() {
        let m = Manifest::parse(EVERY_TABLE).expect("parses").here();
        assert_eq!(m.build.len(), 1, "the windows build is not this machine's");
        assert_eq!(m.events.len(), 1);
        assert_eq!(m.actions.len(), 2);
    }

    #[test]
    fn top_level_platforms_decide_whether_it_runs_here_at_all() {
        let runs = |platforms: &str| {
            Manifest::parse(&format!("id = \"a\"\n{platforms}"))
                .expect("parses")
                .runs_here()
        };
        assert!(runs(""), "absent is everywhere");
        assert!(!runs("platforms = []"), "empty is nowhere");
        assert!(!runs("platforms = [\"windows\"]"));
        assert!(runs("platforms = [\"linux\", \"macos\", \"windows\"]"));
    }

    #[test]
    fn a_bare_manifest_parses() {
        let m = Manifest::parse("id = \"x\"").expect("parses");
        assert!(m.actions.is_empty() && m.panes.is_empty() && m.build.is_empty());
    }

    #[test]
    fn a_bad_id_or_an_empty_command_is_refused() {
        let long = "a".repeat(65);
        for id in [
            "",
            "X",
            "-a",
            ".a",
            "a/b",
            "a b",
            "../a",
            long.as_str(),
            "consent.json",
            "consent.lock",
        ] {
            let text = format!("id = {id:?}");
            assert!(Manifest::parse(&text).is_err(), "{id:?}");
        }
        assert!(valid_id(&"a".repeat(64)));
        assert!(valid_id("persiyanov.reviewr"));
        for command in ["[]", "[\"\"]"] {
            let text = format!("id = \"a\"\n[[actions]]\nid = \"x\"\ncommand = {command}\n");
            assert!(Manifest::parse(&text).is_err(), "{command}");
        }
        assert!(Manifest::parse("id = \"a\"\n[[actions]]\nid = \"x\"\n").is_err());
        assert!(Manifest::parse("not toml at all [").is_err());
    }

    #[test]
    fn a_newer_herdr_is_noticed() {
        let wants = |v: &str| {
            Manifest::parse(&format!("id = \"a\"\nmin_herdr_version = {v:?}"))
                .expect("parses")
                .wants_newer_herdr()
        };
        assert!(wants("0.9.6"));
        assert!(wants("0.10.0"));
        assert!(wants("1.0"));
        assert!(!wants("0.9.5"));
        assert!(!wants("0.9"));
        assert!(!wants("0.7.2"));
    }
}

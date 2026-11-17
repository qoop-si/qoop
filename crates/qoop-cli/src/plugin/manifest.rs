//! A herdr plugin's `herdr-plugin.toml`, as qoop reads it.
//!
//! Checked the way herdr 0.9.5 checks it, so a manifest herdr refuses is refused here too: a
//! name, a version and a `min_herdr_version` no newer than the herdr qoop answers as; titles on
//! actions and panes; ids in herdr's character sets; platforms from herdr's three; every
//! command a non-empty argv; `[[link_handlers]]` too, which a ctrl+click on a link runs. A key
//! qoop does not read is left alone. On top of herdr's rules, a plugin id is never one a
//! path could read as a parent or a hidden name, since it names a folder here.

use std::collections::BTreeSet;

use serde::Deserialize;

/// The herdr release qoop answers as.
pub(crate) const HERDR_VERSION: &str = super::translate::VERSION;

/// The name of the file at a plugin's root.
pub(crate) const FILE: &str = "herdr-plugin.toml";

/// The most of a manifest qoop reads: far beyond any real one, and a bound on what a
/// repository can make it hold in memory or in the consent ledger.
pub(crate) const MAX_BYTES: u64 = 1 << 20;

/// The platforms herdr names.
const PLATFORMS: [&str; 3] = ["linux", "macos", "windows"];

/// The longest id herdr takes, for a plugin and for its actions and panes.
const MAX_ID: usize = 120;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) version: Option<String>,
    #[serde(default)]
    pub(crate) min_herdr_version: Option<String>,
    /// Where the plugin runs at all; absent is everywhere.
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
    /// Read and listed as herdr lists them; a ctrl+click on a link runs the first that matches.
    #[serde(default)]
    pub(crate) link_handlers: Vec<LinkHandler>,
}

/// A `[[link_handlers]]` entry: links matching `pattern` go to the action `action`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LinkHandler {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) pattern: String,
    pub(crate) action: String,
    #[serde(default)]
    pub(crate) platforms: Option<Vec<String>>,
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
    #[serde(default)]
    pub(crate) description: Option<String>,
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
    #[serde(default)]
    pub(crate) description: Option<String>,
    /// A popup's size, as written: cells or a percentage.
    #[serde(default)]
    pub(crate) width: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) height: Option<serde_json::Value>,
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
/// is only where it names.
pub(crate) fn runs_here(platforms: Option<&[String]>) -> bool {
    platforms.is_none_or(|p| p.iter().any(|name| name == this_os()))
}

/// Whether `id` can name a plugin: herdr's set (ASCII letters and digits, `:`, `.`, `_`, `-`,
/// at most 120), and never a name a path reads as hidden or as a parent, since it names a
/// folder here.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._-".contains(&b))
        && !id.starts_with('.')
        && !id.contains("..")
        // The consent ledger, its lock, its temporary file and a set-aside ledger sit beside
        // the plugin folders, on a file system that may ignore case.
        && !id.to_ascii_lowercase().starts_with("consent.")
}

/// Whether `id` can name an action or a pane: herdr's set, with no dot (a dot separates a
/// plugin from its action).
fn valid_item_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":_-".contains(&b))
}

/// A `min_herdr_version` as herdr reads it: three numbers, an optional leading `v`.
fn semver(text: &str) -> Option<[u64; 3]> {
    let parts: Vec<&str> = text.trim().trim_start_matches('v').split('.').collect();
    let [a, b, c] = parts.as_slice() else {
        return None;
    };
    Some([a.parse().ok()?, b.parse().ok()?, c.parse().ok()?])
}

fn blank(text: Option<&String>) -> bool {
    text.is_none_or(|t| t.trim().is_empty())
}

fn check_platforms(platforms: Option<&[String]>) -> Result<(), String> {
    let Some(platforms) = platforms else {
        return Ok(());
    };
    if platforms.is_empty() {
        return Err(
            "platforms must not be an empty array; omit the field to leave platforms undeclared"
                .to_string(),
        );
    }
    match platforms.iter().find(|p| !PLATFORMS.contains(&p.as_str())) {
        Some(unknown) => Err(format!("unknown platform '{unknown}'")),
        None => Ok(()),
    }
}

impl Manifest {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut m: Self =
            toml::from_str(text).map_err(|e| format!("{FILE} does not parse: {e}"))?;
        m.id = m.id.trim().to_string();
        let refuse = |why: String| Err(format!("{FILE}: {why}"));
        if !valid_id(&m.id) {
            return refuse("invalid plugin id".into());
        }
        if blank(m.name.as_ref()) {
            return refuse("plugin name is required".into());
        }
        if blank(m.version.as_ref()) {
            return refuse("plugin version is required".into());
        }
        let Some(wanted) = m
            .min_herdr_version
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        else {
            return refuse("plugin min_herdr_version is required".into());
        };
        let here = semver(HERDR_VERSION).expect("qoop's herdr version is three numbers");
        let Some(wanted) = semver(wanted) else {
            return refuse(format!(
                "plugin min_herdr_version must be a semantic version like {HERDR_VERSION}"
            ));
        };
        if wanted > here {
            let [a, b, c] = wanted;
            return refuse(format!(
                "plugin requires Herdr {a}.{b}.{c} or newer; current Herdr is {HERDR_VERSION}"
            ));
        }
        m.min_herdr_version = Some(format!("{}.{}.{}", wanted[0], wanted[1], wanted[2]));

        let platforms = std::iter::once(m.platforms.as_deref())
            .chain(
                m.build
                    .iter()
                    .chain(&m.startup)
                    .map(|s| s.platforms.as_deref()),
            )
            .chain(m.actions.iter().map(|a| a.platforms.as_deref()))
            .chain(m.panes.iter().map(|p| p.platforms.as_deref()))
            .chain(m.events.iter().map(|e| e.platforms.as_deref()));
        for list in platforms {
            check_platforms(list).map_err(|e| format!("{FILE}: {e}"))?;
        }

        let mut seen = BTreeSet::new();
        for action in &m.actions {
            if !valid_item_id(&action.id) {
                return refuse("invalid action id".into());
            }
            if !seen.insert(action.id.as_str()) {
                return refuse(format!("duplicate action id '{}'", action.id));
            }
            if blank(action.title.as_ref()) {
                return refuse("action title is required".into());
            }
        }
        let mut seen = BTreeSet::new();
        for pane in &m.panes {
            if !valid_item_id(&pane.id) {
                return refuse("invalid pane id".into());
            }
            if !seen.insert(pane.id.as_str()) {
                return refuse(format!("duplicate pane id '{}'", pane.id));
            }
            if blank(pane.title.as_ref()) {
                return refuse("pane title is required".into());
            }
        }
        if m.events.iter().any(|e| e.on.trim().is_empty()) {
            return refuse("an event hook with no event".into());
        }
        // As herdr checks them: an id, a title, a pattern that compiles and a known action.
        let mut seen = BTreeSet::new();
        for handler in &m.link_handlers {
            if !valid_item_id(&handler.id) {
                return refuse("invalid link handler id".into());
            }
            if !seen.insert(handler.id.as_str()) {
                return refuse(format!("duplicate link handler id '{}'", handler.id));
            }
            if handler.title.trim().is_empty() {
                return refuse("link handler title is required".into());
            }
            if handler.pattern.trim().is_empty() {
                return refuse("link handler pattern is required".into());
            }
            if let Err(e) = regex::Regex::new(&handler.pattern) {
                return refuse(e.to_string());
            }
            if !m.actions.iter().any(|a| a.id == handler.action) {
                return refuse(format!(
                    "link handler '{}' references unknown action '{}'",
                    handler.id, handler.action
                ));
            }
            check_platforms(handler.platforms.as_deref()).map_err(|e| format!("{FILE}: {e}"))?;
        }
        let commands = m
            .build
            .iter()
            .chain(&m.startup)
            .map(|s| &s.command)
            .chain(m.actions.iter().map(|a| &a.command))
            .chain(m.panes.iter().map(|p| &p.command))
            .chain(m.events.iter().map(|e| &e.command));
        for command in commands {
            if command.is_empty() || command.iter().any(String::is_empty) {
                return refuse("command must contain non-empty argv strings".into());
            }
        }
        Ok(m)
    }

    /// Whether the plugin runs on this machine at all, by its top-level `platforms`.
    pub(crate) fn runs_here(&self) -> bool {
        runs_here(self.platforms.as_deref())
    }

    /// The manifest as this machine runs it: only the entries for this platform. An entry's own
    /// `platforms` replace the plugin's, as herdr reads them.
    pub(crate) fn here(&self) -> Self {
        let top = self.platforms.as_deref();
        let runs = |own: Option<&[String]>| runs_here(own.or(top));
        let mut out = self.clone();
        out.build.retain(|s| runs(s.platforms.as_deref()));
        out.startup.retain(|s| runs(s.platforms.as_deref()));
        out.actions.retain(|a| runs(a.platforms.as_deref()));
        out.panes.retain(|p| runs(p.platforms.as_deref()));
        out.events.retain(|e| runs(e.platforms.as_deref()));
        out.link_handlers.retain(|l| runs(l.platforms.as_deref()));
        out
    }
}

/// A manifest head that passes, for the tests of this crate.
#[cfg(test)]
pub(crate) fn head(id: &str) -> String {
    format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nmin_herdr_version = \"0.9.0\"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_TABLE: &str = r#"
id = "pat.notes"
name = "Notes"
version = "1.2.3"
min_herdr_version = "v0.8.0"
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
        assert_eq!(m.name.as_deref(), Some("Notes"));
        assert_eq!(m.version.as_deref(), Some("1.2.3"));
        assert_eq!(m.min_herdr_version.as_deref(), Some("0.8.0"), "normalised");
        assert_eq!(m.platforms, Some(vec!["linux".into(), "macos".into()]));
        assert_eq!(m.build.len(), 2);
        assert_eq!(m.build[1].platforms, Some(vec!["windows".into()]));
        assert_eq!(
            m.actions[0],
            Action {
                id: "open".into(),
                title: Some("Open notes".into()),
                description: None,
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
        assert_eq!(m.link_handlers[0].action, "open");
        assert!(m.runs_here(), "this test runs on linux or macos");
        let stray = EVERY_TABLE.replace("action = \"open\"", "action = \"gone\"");
        let refused = Manifest::parse(&stray).expect_err("an unknown action");
        assert!(refused.contains("unknown action 'gone'"), "{refused}");
    }

    #[test]
    fn here_keeps_only_this_platforms_entries() {
        let m = Manifest::parse(EVERY_TABLE).expect("parses").here();
        assert_eq!(m.build.len(), 1, "the windows build is not this machine's");
        assert_eq!(m.events.len(), 1);
        assert_eq!(m.actions.len(), 2);
        // An entry's own platforms replace the plugin's.
        let text = format!(
            "{}platforms = [\"windows\"]\n[[actions]]\nid = \"a\"\ntitle = \"A\"\ncommand = [\"a\"]\nplatforms = [\"linux\", \"macos\"]\n",
            head("x")
        );
        let m = Manifest::parse(&text).expect("parses");
        assert!(!m.runs_here());
        assert_eq!(m.here().actions.len(), 1);
    }

    #[test]
    fn top_level_platforms_decide_whether_it_runs_here_at_all() {
        let runs = |platforms: &str| {
            Manifest::parse(&format!("{}{platforms}", head("a")))
                .expect("parses")
                .runs_here()
        };
        assert!(runs(""), "absent is everywhere");
        assert!(!runs("platforms = [\"windows\"]"));
        assert!(runs("platforms = [\"linux\", \"macos\", \"windows\"]"));
        for bad in ["platforms = []", "platforms = [\"beos\"]"] {
            assert!(
                Manifest::parse(&format!("{}{bad}", head("a"))).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_bare_manifest_parses() {
        let m = Manifest::parse(&head("x")).expect("parses");
        assert!(m.actions.is_empty() && m.panes.is_empty() && m.build.is_empty());
    }

    #[test]
    fn what_herdr_requires_is_required() {
        let without = |key: &str| {
            head("a")
                .lines()
                .filter(|l| !l.starts_with(key))
                .collect::<Vec<_>>()
                .join("\n")
        };
        for key in ["name", "version", "min_herdr_version"] {
            let err = Manifest::parse(&without(key)).expect_err(key);
            assert!(err.contains("is required"), "{err}");
        }
        let newer = head("a").replace("0.9.0", "0.9.6");
        let err = Manifest::parse(&newer).expect_err("newer");
        assert!(
            err.contains("plugin requires Herdr 0.9.6 or newer; current Herdr is 0.9.5"),
            "{err}"
        );
        let odd = head("a").replace("0.9.0", "0.9");
        assert!(
            Manifest::parse(&odd)
                .expect_err("odd")
                .contains("semantic version")
        );
        let untitled = format!("{}[[actions]]\nid = \"x\"\ncommand = [\"x\"]\n", head("a"));
        assert!(
            Manifest::parse(&untitled)
                .expect_err("title")
                .contains("title is required")
        );
        let untitled = format!("{}[[panes]]\nid = \"x\"\ncommand = [\"x\"]\n", head("a"));
        assert!(
            Manifest::parse(&untitled)
                .expect_err("title")
                .contains("title is required")
        );
    }

    #[test]
    fn a_bad_id_or_an_empty_command_is_refused() {
        let long = "a".repeat(121);
        for id in [
            "",
            ".a",
            "a/b",
            "a b",
            "a\\b",
            "a..b",
            "../a",
            long.as_str(),
            "consent.json",
            "consent.lock",
            "Consent.JSON",
            "consent.tmp",
            "consent.json.bad-1",
        ] {
            assert!(Manifest::parse(&head(id)).is_err(), "{id:?}");
        }
        assert!(valid_id(&"a".repeat(120)));
        assert!(valid_id("persiyanov.reviewr"));
        assert!(valid_id("Pat:Notes_1-x"), "herdr's own set");
        for item in ["a.b", "a b", ""] {
            let text = format!(
                "{}[[actions]]\nid = {item:?}\ntitle = \"T\"\ncommand = [\"x\"]\n",
                head("a")
            );
            assert!(Manifest::parse(&text).is_err(), "{item:?}");
        }
        let twice = format!(
            "{0}[[panes]]\nid = \"p\"\ntitle = \"P\"\ncommand = [\"x\"]\n[[panes]]\nid = \"p\"\ntitle = \"P\"\ncommand = [\"y\"]\n",
            head("a")
        );
        assert!(
            Manifest::parse(&twice)
                .expect_err("dup")
                .contains("duplicate pane id")
        );
        for command in ["[]", "[\"\"]", "[\"a\", \"\"]"] {
            let text = format!(
                "{}[[actions]]\nid = \"x\"\ntitle = \"T\"\ncommand = {command}\n",
                head("a")
            );
            assert!(Manifest::parse(&text).is_err(), "{command}");
        }
        assert!(Manifest::parse("not toml at all [").is_err());
    }
}

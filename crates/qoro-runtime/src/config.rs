//! The daemon's part of `config.toml`: when a tab goes to sleep by itself, and whether
//! the daemon leaves once nothing is awake.
//!
//! ```toml
//! [sleep]
//! idle_minutes = 0          # 0 = a tab never sleeps by itself
//!
//! [daemon]
//! exit_when_asleep = true   # leave when nothing is awake and nobody is attached
//! ```
//!
//! The same file the interface reads, and the rules for these keys live here once: the
//! daemon takes its values from [`read`], the interface reports what it could not use
//! through [`lifecycle_from`]. The file is the only source: a daemon is started from a
//! terminal, a notch, an ssh relay, and a setting that came from whichever environment
//! started it would change with who happened to start it.

use std::path::Path;

/// What the daemon reads from the file, every default filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lifecycle {
    /// Minutes a tab is idle and unwatched before it sleeps by itself. `0` is never.
    pub idle_minutes: u64,
    /// Leave when nothing is awake and nobody is attached.
    pub exit_when_asleep: bool,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Lifecycle {
            idle_minutes: 0,
            exit_when_asleep: true,
        }
    }
}

/// The two sections, from a parsed file. Answers the settings and, for each key that could
/// not be used, its dotted name and what was wrong with it; that key keeps its default.
/// Keys of other sections are not this function's to judge and are left alone.
pub fn lifecycle_from(table: &toml::Table) -> (Lifecycle, Vec<(String, String)>) {
    let mut settings = Lifecycle::default();
    let mut problems = Vec::new();
    for (section, value) in table {
        if section != "sleep" && section != "daemon" {
            continue;
        }
        let Some(keys) = value.as_table() else {
            problems.push((
                section.clone(),
                format!("{section} is a table: [{section}] then its keys"),
            ));
            continue;
        };
        for (key, value) in keys {
            let name = format!("{section}.{key}");
            match (section.as_str(), key.as_str(), value) {
                ("sleep", "idle_minutes", toml::Value::Integer(n)) if *n >= 0 => {
                    settings.idle_minutes = *n as u64;
                }
                ("sleep", "idle_minutes", _) => problems.push((
                    name,
                    format!("idle_minutes = {value} is not a whole number of minutes, 0 or more"),
                )),
                ("daemon", "exit_when_asleep", toml::Value::Boolean(on)) => {
                    settings.exit_when_asleep = *on;
                }
                ("daemon", "exit_when_asleep", _) => problems.push((
                    name,
                    format!("exit_when_asleep = {value} is not true or false"),
                )),
                _ => problems.push((name.clone(), format!("there is no setting {name}"))),
            }
        }
    }
    (settings, problems)
}

/// The settings in the file at `path`. A file that is missing, unreadable or not TOML is
/// every default: the interface is where a person is told about it, and a daemon that
/// refused to decide anything over a typo would be worse than one that kept its defaults.
pub fn read_from(path: &Path) -> Lifecycle {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .map(|table| lifecycle_from(&table).0)
        .unwrap_or_default()
}

/// The settings in the person's `config.toml`.
pub fn read() -> Lifecycle {
    read_from(&crate::paths::config_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Lifecycle, Vec<(String, String)>) {
        lifecycle_from(&text.parse::<toml::Table>().expect("toml"))
    }

    #[test]
    fn no_sections_is_every_default() {
        let (settings, problems) = parse("theme = \"dark\"\n[notify]\nsound = true\n");
        assert_eq!(settings, Lifecycle::default());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.idle_minutes, 0);
        assert!(settings.exit_when_asleep);
    }

    #[test]
    fn the_documented_keys_read() {
        let (settings, problems) =
            parse("[sleep]\nidle_minutes = 30\n[daemon]\nexit_when_asleep = false\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            settings,
            Lifecycle {
                idle_minutes: 30,
                exit_when_asleep: false
            }
        );
    }

    #[test]
    fn a_bad_value_keeps_its_default_and_is_named() {
        let (settings, problems) =
            parse("[sleep]\nidle_minutes = -5\nnap = 1\n[daemon]\nexit_when_asleep = \"yes\"\n");
        assert_eq!(settings, Lifecycle::default());
        let mut named: Vec<&str> = problems.iter().map(|(k, _)| k.as_str()).collect();
        named.sort();
        assert_eq!(
            named,
            vec!["daemon.exit_when_asleep", "sleep.idle_minutes", "sleep.nap"]
        );
    }

    #[test]
    fn a_missing_or_broken_file_is_every_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read_from(&dir.path().join("none.toml")),
            Lifecycle::default()
        );
        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "[sleep\nidle_minutes = ").expect("write");
        assert_eq!(read_from(&broken), Lifecycle::default());
    }
}

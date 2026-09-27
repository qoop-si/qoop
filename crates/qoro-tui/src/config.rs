//! The few things a person sets for the interface: `config.toml` in the qoro config
//! directory.
//!
//! ```toml
//! prefix = "ctrl-a"   # ctrl- and one of b e f g k l n o p r t u v w x y
//! theme = "auto"      # auto | dark | light
//! [notify]
//! desktop = true
//! sound = false
//! ```
//!
//! Read by the interface alone, at start and again whenever the settings box opens, so an
//! edit takes effect without a restart and without a watcher. The daemon never reads it:
//! how long an idle terminal lives stays `QORO_IDLE_TIMEOUT_MINUTES`, because that is the
//! daemon's decision and a client's file would be one more place to look for it.
//!
//! Nothing here is ever fatal. A value this cannot use keeps that key's default and is
//! said, with the file and the line, so a typo costs a person one setting and a sentence,
//! never the interface. Only these keys and this much of TOML are read: `key = value`
//! lines, a `[notify]` table, comments, quoted strings and booleans. That is the whole
//! file, and a parser for the rest of TOML would be a dependency to read four values.

use std::path::{Path, PathBuf};

/// The keys a prefix may be. The others are taken: `c d h i j m q s z` are either
/// commands after the prefix, backspace, tab, enter, detach or the shell's own job
/// control, and a prefix on one of them would take that key away from the program.
const PREFIX_LETTERS: &str = "befgklnoprtuvwxy";

/// Which palette the chrome is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// Asked of the terminal: its background decides.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

/// What the file says, with every default filled in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The byte that starts a command.
    pub prefix: u8,
    pub theme: Theme,
    /// Tell the desktop when an agent needs a person who is not looking.
    pub desktop: bool,
    /// And play a sound when it does.
    pub sound: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: crate::keys::PREFIX,
            theme: Theme::Auto,
            desktop: true,
            sound: false,
        }
    }
}

/// A read of the file: what it says, and what in it could not be used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    pub config: Config,
    /// One sentence per line that kept a default, each naming the file and the line.
    pub problems: Vec<String>,
}

/// Where the file is.
pub fn path() -> PathBuf {
    qoro_runtime::paths::config_dir().join("config.toml")
}

/// Read the file where it is. No file is every default and nothing to say.
pub fn read() -> Read {
    read_from(&path())
}

/// Read a file by its path, so a test never touches a home directory.
pub fn read_from(path: &Path) -> Read {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let (config, problems) = parse(&text);
            Read {
                config,
                problems: problems
                    .into_iter()
                    .map(|(line, what)| {
                        format!("{} line {line}: {what}, kept the default", path.display())
                    })
                    .collect(),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::default(),
        Err(error) => Read {
            config: Config::default(),
            problems: vec![format!(
                "{} could not be read ({error}), using the defaults",
                path.display()
            )],
        },
    }
}

/// The settings, and for every line that could not be used its number and why.
pub fn parse(text: &str) -> (Config, Vec<(usize, String)>) {
    let mut config = Config::default();
    let mut problems = Vec::new();
    let mut table = String::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        // A `#` is never part of a value this file can hold, so it always starts a comment.
        let text = raw.split('#').next().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        if let Some(name) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            table = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = text.split_once('=') else {
            problems.push((line, format!("`{text}` is not `key = value`")));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let wrong = |what: &str| format!("{key} = {value} is not {what}");
        match (table.as_str(), key) {
            ("", "prefix") => match quoted(value).and_then(prefix_byte) {
                Some(byte) => config.prefix = byte,
                None => problems.push((
                    line,
                    wrong("ctrl- and one of b e f g k l n o p r t u v w x y"),
                )),
            },
            ("", "theme") => match quoted(value) {
                Some("auto") => config.theme = Theme::Auto,
                Some("dark") => config.theme = Theme::Dark,
                Some("light") => config.theme = Theme::Light,
                _ => problems.push((line, wrong("\"auto\", \"dark\" or \"light\""))),
            },
            ("notify", "desktop") => match boolean(value) {
                Some(on) => config.desktop = on,
                None => problems.push((line, wrong("true or false"))),
            },
            ("notify", "sound") => match boolean(value) {
                Some(on) => config.sound = on,
                None => problems.push((line, wrong("true or false"))),
            },
            ("", _) => problems.push((line, format!("there is no setting `{key}`"))),
            _ => problems.push((line, format!("there is no setting `{table}.{key}`"))),
        }
    }
    (config, problems)
}

/// A TOML string in either quote.
fn quoted(value: &str) -> Option<&str> {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
}

fn boolean(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// `ctrl-b` is `0x02`, for the letters a prefix may be.
fn prefix_byte(name: &str) -> Option<u8> {
    let letter = name.strip_prefix("ctrl-")?;
    let &[byte] = letter.as_bytes() else {
        return None;
    };
    PREFIX_LETTERS
        .as_bytes()
        .contains(&byte)
        .then(|| byte - b'a' + 1)
}

/// The read in effect, for the settings box to describe.
///
/// Held here for the reason `keys::show_prefix` holds the prefix: the box is painted from
/// the frame, and threading one more value through every frame for a box that is open a
/// few seconds a week is a larger change than the box.
static SHOWN: std::sync::Mutex<Option<Read>> = std::sync::Mutex::new(None);

/// Say which read is in effect. Set where the interface starts and wherever it reads again.
pub fn show(read: &Read) {
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(read.clone());
}

/// The read in effect, or the defaults before anything was read.
pub fn shown() -> Read {
    SHOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// What the settings box says about the file: where it is and what is in effect.
pub fn describe(read: &Read) -> Vec<String> {
    let config = read.config;
    let file = path();
    let mut rows = vec![
        format!(
            " config          {}{}",
            file.display(),
            if file.exists() {
                ""
            } else {
                " (none, defaults)"
            }
        ),
        format!(" prefix          {}", crate::keys::name_of(config.prefix)),
        format!(" theme           {}", config.theme.name()),
        format!(
            " notify          desktop {}, sound {}",
            if config.desktop { "on" } else { "off" },
            if config.sound { "on" } else { "off" }
        ),
    ];
    rows.extend(read.problems.iter().map(|problem| format!(" ! {problem}")));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_is_every_default_and_nothing_to_say() {
        let dir = tempfile::tempdir().expect("tempdir");
        let read = read_from(&dir.path().join("config.toml"));
        assert_eq!(read, Read::default());
        assert_eq!(read.config.prefix, 0x01);
        assert_eq!(read.config.theme, Theme::Auto);
        assert!(read.config.desktop && !read.config.sound);
    }

    #[test]
    fn every_key_is_read() {
        let (config, problems) = parse(
            "# mine\nprefix = \"ctrl-b\"\ntheme = 'light'  # bright room\n\n[notify]\ndesktop = false\nsound = true\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            config,
            Config {
                prefix: 0x02,
                theme: Theme::Light,
                desktop: false,
                sound: true,
            }
        );
    }

    #[test]
    fn a_taken_key_is_never_a_prefix() {
        for letter in "cdhijmqsz".chars() {
            let (config, problems) = parse(&format!("prefix = \"ctrl-{letter}\""));
            assert_eq!(config.prefix, 0x01, "ctrl-{letter} was taken");
            assert_eq!(problems.len(), 1, "ctrl-{letter}");
        }
        for letter in PREFIX_LETTERS.chars() {
            let (config, problems) = parse(&format!("prefix = \"ctrl-{letter}\""));
            assert!(problems.is_empty(), "ctrl-{letter}: {problems:?}");
            assert_eq!(
                crate::keys::name_of(config.prefix),
                format!("ctrl-{letter}")
            );
        }
    }

    /// A bad line costs that one setting and is said with its line; everything around it
    /// is still read.
    #[test]
    fn each_refusal_keeps_its_default_and_names_its_line() {
        let (config, problems) = parse(
            "prefix = ctrl-b\ntheme = \"blue\"\ncolour = \"red\"\n[notify]\nsound = yes\ndesktop = false\nloud = true\njunk\n",
        );
        assert_eq!(
            config,
            Config {
                desktop: false,
                ..Config::default()
            }
        );
        let lines: Vec<usize> = problems.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, vec![1, 2, 3, 5, 7, 8], "{problems:?}");
        assert!(problems[2].1.contains("`colour`"), "{problems:?}");
        assert!(problems[4].1.contains("`notify.loud`"), "{problems:?}");
    }

    #[test]
    fn the_message_names_the_file_and_the_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "\n\ntheme = \"blue\"\n").expect("write");
        let read = read_from(&file);
        assert_eq!(read.config, Config::default());
        assert_eq!(read.problems.len(), 1);
        let said = &read.problems[0];
        assert!(said.contains(&file.display().to_string()), "{said}");
        assert!(said.contains("line 3"), "{said}");
        assert!(said.contains("kept the default"), "{said}");
    }
}

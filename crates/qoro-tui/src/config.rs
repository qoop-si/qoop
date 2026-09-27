//! The few things a person sets for the interface: `config.toml` in the qoro config
//! directory.
//!
//! ```toml
//! prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
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
//! Read as TOML by the `toml` crate, so dotted keys (`notify.desktop = false`), inline
//! tables and quoting behave the way the format says. Nothing here is ever fatal: a value
//! this cannot use keeps that key's default and is said, so a typo costs a person one
//! setting and a sentence, never the interface. A line that is not TOML at all is said
//! with its number and left out, and the rest of the file is still read.

use std::path::{Path, PathBuf};

/// The keys a prefix may be. The others are taken: `c d h i j m q s z` are either
/// commands after the prefix, backspace, tab, enter, detach or the shell's own job
/// control, and a prefix on one of them would take that key away from the program.
const PREFIX_LETTERS: &str = "abefgklnoprtuvwxy";

/// What a person is told a prefix may be.
const PREFIX_SAID: &str = "ctrl- and one of a b e f g k l n o p r t u v w x y";

/// Lines left out of a file that is not TOML before the rest is given up on.
const MAX_BAD_LINES: usize = 16;

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

/// One thing in the file that could not be used, and the line it is on when that is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: Option<usize>,
    pub what: String,
}

impl Problem {
    /// As the settings box says it, beside the file's own path.
    pub fn short(&self) -> String {
        match self.line {
            Some(line) => format!("line {line}: {}", self.what),
            None => self.what.clone(),
        }
    }
}

/// A read of the file: what it says, what in it could not be used, and where it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    pub config: Config,
    pub problems: Vec<Problem>,
    pub path: PathBuf,
    /// Whether there was a file at all, noted once here so nothing asks the disk per frame.
    pub exists: bool,
}

impl Read {
    /// The first problem, as the hint row says it: the file, the line, and what was kept.
    pub fn hint(&self) -> Option<String> {
        let first = self.problems.first()?;
        let more = match self.problems.len() {
            1 => String::new(),
            n => format!(" (and {} more, see settings)", n - 1),
        };
        Some(format!(
            "{} {}, kept the default{more}",
            self.path.display(),
            first.short()
        ))
    }
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
    let (config, problems, exists) = match std::fs::read_to_string(path) {
        Ok(text) => {
            let (config, problems) = parse(&text);
            (config, problems, true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (Config::default(), Vec::new(), false)
        }
        Err(error) => (
            Config::default(),
            vec![Problem {
                line: None,
                what: format!("could not be read ({error})"),
            }],
            true,
        ),
    };
    Read {
        config,
        problems,
        path: path.to_path_buf(),
        exists,
    }
}

/// The settings, and everything that could not be used.
pub fn parse(text: &str) -> (Config, Vec<Problem>) {
    let mut problems = Vec::new();
    let mut text = text.strip_prefix('\u{feff}').unwrap_or(text).to_string();
    let table = loop {
        match text.parse::<toml::Table>() {
            Ok(table) => break table,
            Err(error) => {
                let line = error
                    .span()
                    .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
                problems.push(Problem {
                    line,
                    what: unreadable(&text, line, error.message()),
                });
                // Leave the line out and read the rest, so one bad line costs one setting.
                match line {
                    Some(line) if problems.len() < MAX_BAD_LINES => {
                        text = text
                            .lines()
                            .enumerate()
                            .map(|(at, l)| if at + 1 == line { "" } else { l })
                            .collect::<Vec<_>>()
                            .join("\n");
                    }
                    _ => break toml::Table::new(),
                }
            }
        }
    };
    let mut config = Config::default();
    let mut refuse = |what: String| problems.push(Problem { line: None, what });
    for (key, value) in &table {
        match (key.as_str(), value) {
            ("prefix", toml::Value::String(name)) => match prefix_byte(name) {
                Some(byte) => config.prefix = byte,
                None => refuse(format!("prefix = {value} is not {PREFIX_SAID}")),
            },
            ("theme", toml::Value::String(name)) if name == "auto" => config.theme = Theme::Auto,
            ("theme", toml::Value::String(name)) if name == "dark" => config.theme = Theme::Dark,
            ("theme", toml::Value::String(name)) if name == "light" => {
                config.theme = Theme::Light;
            }
            ("prefix", _) => refuse(format!("prefix = {value} is not {PREFIX_SAID}")),
            ("theme", _) => refuse(format!(
                "theme = {value} is not \"auto\", \"dark\" or \"light\""
            )),
            ("notify", toml::Value::Table(notify)) => {
                for (key, value) in notify {
                    let slot = match key.as_str() {
                        "desktop" => &mut config.desktop,
                        "sound" => &mut config.sound,
                        _ => {
                            refuse(format!("there is no setting notify.{key}"));
                            continue;
                        }
                    };
                    match value {
                        toml::Value::Boolean(on) => *slot = *on,
                        _ => refuse(format!("notify.{key} = {value} is not true or false")),
                    }
                }
            }
            ("notify", _) => refuse("notify is a table: [notify] then desktop = true".into()),
            _ => refuse(format!("there is no setting {key}")),
        }
    }
    (config, problems)
}

/// What to say about a line that is not TOML. The mistake a person actually makes is a
/// bare word where a string goes, and for that the fix is spelled out.
fn unreadable(text: &str, line: Option<usize>, message: &str) -> String {
    let bare = line
        .and_then(|line| text.lines().nth(line - 1))
        .and_then(|l| l.split('#').next())
        .and_then(|l| l.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .filter(|(key, value)| {
            !key.is_empty()
                && !value.is_empty()
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    match bare {
        Some((key, value)) => format!("needs quotes: {key} = \"{value}\""),
        None => format!("not TOML ({})", message.trim()),
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

/// The settings box's rows, computed once when the file is read.
///
/// Held here for the reason `keys::show_prefix` holds the prefix: the box is painted from
/// the frame, and every frame test in the interface builds a `Frame` by hand, so a field
/// there for a box that is open a few seconds a week is a larger change than the box.
static SHOWN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Say what the settings box should show. Set wherever the file is read.
pub fn show(rows: Vec<String>) {
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = rows;
}

/// The settings box's rows about the file, as last shown.
pub fn shown() -> Vec<String> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The rows about the file, in its own syntax, so what the box shows is what a person
/// would write: where the file is, each setting in effect with what it may be, where a
/// notification goes (`notes`), and what could not be used.
pub fn describe(read: &Read, light: bool, notes: &str) -> Vec<String> {
    let config = read.config;
    // Each setting as it is written in the file, then what it may be, in one column.
    let row = |written: String, about: &str| format!(" {written:<20}{about}");
    let theme_now = match config.theme {
        Theme::Auto if light => "auto | dark | light, light now",
        Theme::Auto => "auto | dark | light, dark now",
        _ => "auto | dark | light",
    };
    let notes = if config.desktop {
        format!("notes go to {notes}")
    } else {
        "no desktop notes".to_string()
    };
    let mut rows = vec![
        format!(
            " {}{}",
            read.path.display(),
            if read.exists {
                ""
            } else {
                "  (no file: defaults)"
            }
        ),
        row(
            format!("prefix = \"{}\"", crate::keys::name_of(config.prefix)),
            PREFIX_SAID,
        ),
        row(format!("theme = \"{}\"", config.theme.name()), theme_now),
        " [notify]".to_string(),
        row(format!("desktop = {}", config.desktop), &notes),
        row(format!("sound = {}", config.sound), "true | false"),
    ];
    rows.extend(read.problems.iter().map(|p| format!(" ! {}", p.short())));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_is_every_default_and_nothing_to_say() {
        let dir = tempfile::tempdir().expect("tempdir");
        let read = read_from(&dir.path().join("config.toml"));
        assert_eq!(read.config, Config::default());
        assert!(read.problems.is_empty() && !read.exists);
        assert_eq!(read.config.prefix, 0x01);
        assert_eq!(read.config.theme, Theme::Auto);
        assert!(read.config.desktop && !read.config.sound);
    }

    /// The file as the README writes it reads with nothing to say, the default prefix
    /// included.
    #[test]
    fn the_documented_file_reads_cleanly() {
        let (config, problems) = parse(
            "prefix = \"ctrl-a\"   # ctrl- and one letter\ntheme = \"auto\"\n[notify]\ndesktop = true\nsound = false\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn every_key_is_read_in_every_toml_spelling() {
        let (config, problems) = parse(
            "\u{feff}# mine\nprefix = \"ctrl-b\"\ntheme = 'light'  # bright room\n\n[notify]\ndesktop = false\nsound = true\n",
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
        let dotted = parse("notify.desktop = false\n").0;
        assert!(!dotted.desktop, "the hint's own advice has to work");
        let inline = parse("notify = { sound = true }\n\"theme\" = \"dark\"\n").0;
        assert!(inline.sound);
        assert_eq!(inline.theme, Theme::Dark);
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

    /// A bad value costs that one setting and is said; a line that is not TOML is said
    /// with its number and the rest of the file is still read.
    #[test]
    fn each_refusal_keeps_its_default_and_the_rest_is_still_read() {
        let (config, problems) = parse(
            "prefix = ctrl-b\ntheme = \"blue\"\ncolour = \"red\"\n[notify]\nsound = \"yes\"\ndesktop = false\nloud = true\n",
        );
        assert_eq!(
            config,
            Config {
                desktop: false,
                ..Config::default()
            }
        );
        let said: Vec<String> = problems.iter().map(Problem::short).collect();
        assert_eq!(said.len(), 5, "{said:?}");
        assert_eq!(said[0], "line 1: needs quotes: prefix = \"ctrl-b\"");
        for needle in [
            "theme = \"blue\"",
            "no setting colour",
            "notify.sound = \"yes\"",
            "no setting notify.loud",
        ] {
            assert!(
                said.iter().any(|s| s.contains(needle)),
                "{needle}: {said:?}"
            );
        }
    }

    #[test]
    fn a_repeated_key_is_said_and_the_first_one_stands() {
        let (config, problems) = parse("theme = \"light\"\ntheme = \"dark\"\n");
        assert_eq!(config.theme, Theme::Light);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].line, Some(2), "{problems:?}");
    }

    #[test]
    fn the_hint_names_the_file_and_the_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "\n\ntheme = blue\n").expect("write");
        let read = read_from(&file);
        assert_eq!(read.config, Config::default());
        let hint = read.hint().expect("something to say");
        assert!(hint.contains(&file.display().to_string()), "{hint}");
        assert!(hint.contains("line 3"), "{hint}");
        assert!(hint.contains("kept the default"), "{hint}");
    }

    /// The settings box shows each setting as it is written in the file, where a note goes,
    /// and each problem by its line without the path again.
    #[test]
    fn the_settings_rows_read_like_the_file() {
        let (config, problems) = parse("theme = blue\n[notify]\nsound = true\n");
        let read = Read {
            config,
            problems,
            path: PathBuf::from("/c/config.toml"),
            exists: true,
        };
        let rows = describe(&read, true, "terminal-notifier");
        assert_eq!(rows[0], " /c/config.toml");
        assert!(rows[1].starts_with(" prefix = \"ctrl-a\""), "{rows:?}");
        assert!(rows[2].ends_with("light now"), "{rows:?}");
        assert!(
            rows[4].contains("notes go to terminal-notifier"),
            "{rows:?}"
        );
        assert!(rows[5].starts_with(" sound = true"), "{rows:?}");
        assert_eq!(rows[6], " ! line 1: needs quotes: theme = \"blue\"");
    }
}

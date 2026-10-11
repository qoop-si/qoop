//! The few things a person sets for the interface: `config.toml` in the qoop config
//! directory.
//!
//! ```toml
//! prefix = "ctrl-a"   # ctrl- and one of a b e f g k l n o p r t u v w x y
//! theme = "auto"      # auto | dark | light
//! icons = "auto"      # auto | on | off: the icon font, where it was seen to draw
//! [notify]
//! desktop = true
//! sound = false
//! [notch]
//! enabled = true      # macOS: the agents in the notch
//! [sleep]
//! idle_minutes = 0
//! qass_idle_minutes = 30
//! [daemon]
//! exit_when_asleep = true
//! [qass]
//! agent = "claude"    # claude | codex | gemini | opencode, or a name from [agents]
//! [board]
//! cards = "full"      # full | quiet: a word on a card only when someone must act
//! [agents]            # a name for a new agent, and the line typed to start it
//! claudec = "claude --dangerously-skip-permissions"
//! lm = "opencode"
//! ```
//!
//! Read by the interface at start and again within two seconds of a change (its modified
//! time, checked with the ack), so an edit takes effect without a restart; `qoop bridge`
//! reads `[agents]` the same way whenever a phone asks for the list. `[sleep]` and
//! `[daemon]` are the daemon's: it reads them itself when it decides, and their rules live
//! beside it in `qoop_runtime::config`, which this file asks rather than repeating them.
//!
//! Read as TOML by the `toml` crate, so dotted keys (`notify.desktop = false`), inline
//! tables and quoting behave the way the format says. Nothing here is ever fatal: a value
//! this cannot use keeps that key's default and is said, so a typo costs a person one
//! setting and a sentence, never the interface. A line that is not TOML at all is said
//! with its number and left out, and the rest of the file is still read.

use std::collections::BTreeMap;
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
    /// Icons from the bundled font, or the plain glyphs. See [`crate::icons`].
    pub icons: crate::icons::Setting,
    /// Tell the desktop when an agent needs a person who is not looking.
    pub desktop: bool,
    /// And play a sound when it does.
    pub sound: bool,
    /// The sound for an agent that finished, one of [`sounds_here`].
    pub done_sound: &'static str,
    /// The sound for one that waits on a person or failed, one of [`sounds_here`].
    pub alert_sound: &'static str,
    /// Show the agents in the macOS notch. Read by the notch app, through `qoop config`.
    pub notch: bool,
    /// The daemon's settings, shown here and read by the daemon itself.
    pub lifecycle: qoop_runtime::config::Lifecycle,
    /// The board's quiet cards: one line, a word only on a card someone must act on.
    pub quiet_cards: bool,
}

/// The agents qass's skill installs beside, and so the ones `qoop qass` can start by name.
pub const QASS_AGENTS: [&str; 4] = ["claude", "codex", "gemini", "opencode"];

/// `[report] send`: whether qass's notes to qoop's maker go out after a yes for each, by
/// themselves once a first went out after a yes, or never. No answer reads as `ask`.
pub const REPORT_SENDS: [&str; 3] = ["ask", "always", "never"];

impl Default for Config {
    fn default() -> Self {
        Config {
            prefix: crate::keys::PREFIX,
            theme: Theme::Auto,
            icons: crate::icons::Setting::Auto,
            desktop: true,
            sound: false,
            done_sound: "Glass",
            alert_sound: "Funk",
            notch: true,
            lifecycle: qoop_runtime::config::Lifecycle::default(),
            quiet_cards: false,
        }
    }
}

/// The sounds macOS keeps in `/System/Library/Sounds`, by the names the files have there.
pub const SOUNDS: [&str; 14] = [
    "Basso",
    "Blow",
    "Bottle",
    "Frog",
    "Funk",
    "Glass",
    "Hero",
    "Morse",
    "Ping",
    "Pop",
    "Purr",
    "Sosumi",
    "Submarine",
    "Tink",
];

/// The freedesktop sound theme's event sounds, in `/usr/share/sounds/freedesktop/stereo` as
/// `<name>.oga`: what Linux picks from. The speaker-test channels are left out.
pub const FREEDESKTOP_SOUNDS: [&str; 26] = [
    "alarm-clock-elapsed",
    "audio-volume-change",
    "bell",
    "camera-shutter",
    "complete",
    "device-added",
    "device-removed",
    "dialog-error",
    "dialog-information",
    "dialog-warning",
    "message",
    "message-new-instant",
    "network-connectivity-established",
    "network-connectivity-lost",
    "phone-incoming-call",
    "phone-outgoing-busy",
    "phone-outgoing-calling",
    "power-plug",
    "power-unplug",
    "screen-capture",
    "service-login",
    "service-logout",
    "suspend-error",
    "trash-empty",
    "window-attention",
    "window-question",
];

/// The sounds this machine picks from: macOS's own, or the freedesktop ones elsewhere.
pub fn sounds_here() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &SOUNDS
    } else {
        &FREEDESKTOP_SOUNDS
    }
}

/// A sound named as the file is, in any case: `hero` is `Hero`. Either list is taken, so
/// one `config.toml` reads on a Mac and on Linux; each plays only its own names.
fn sound_named(name: &str) -> Option<&'static str> {
    SOUNDS
        .iter()
        .chain(FREEDESKTOP_SOUNDS.iter())
        .copied()
        .find(|s| s.eq_ignore_ascii_case(name))
}

/// One thing in the file that could not be used, and the line it is on when that is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: Option<usize>,
    pub what: String,
}

impl Problem {
    /// A problem, its text made plain for the screen (see [`plain`]).
    fn new(line: Option<usize>, what: &str) -> Problem {
        Problem {
            line,
            what: plain(what),
        }
    }

    /// As the hint row says it, beside the file's own path.
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
    /// `[agents]`, in the order the file lists them.
    pub agents: Vec<Launcher>,
    /// `[qass] agent`: what `qoop qass` starts, one of [`QASS_AGENTS`] or an `[agents]` name.
    pub qass_agent: String,
    /// `[qass.models]`: the model each of [`QASS_TIERS`] starts a worker on, in that order.
    pub qass_models: [String; 3],
    /// `[report] send`, one of [`REPORT_SENDS`].
    pub report_send: String,
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
            plain(&self.path.display().to_string()),
            first.short()
        ))
    }
}

/// A name an agent may have: one word, as `[agents]` keys are written.
fn agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The tiers qass starts a worker on, and the family each one is when the file names none:
/// Claude Code's aliases, which always mean the newest model of that family.
pub const QASS_TIERS: [(&str, &str); 3] = [("best", "opus"), ("mid", "sonnet"), ("low", "haiku")];

/// Whether a model may be a tier's: a plain name, and never of the Fable family.
fn tier_model(name: &str) -> Result<(), String> {
    if name.to_ascii_lowercase().contains("fable") {
        return Err("the Fable family is not used".to_string());
    }
    // A letter or digit first, so it never reads as a flag; `[` `]` for a suffix like `opus[1m]`.
    let plain = name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '[' | ']'));
    plain
        .then_some(())
        .ok_or_else(|| format!("{name} is not a model name"))
}

/// `[qass.models]` as the file says it, a tier it names wrongly left at its default family.
fn qass_models_in(text: &str) -> [String; 3] {
    let table: Option<toml::Table> = toml::from_str(text).ok();
    let models = table
        .as_ref()
        .and_then(|t| t.get("qass")?.get("models")?.as_table());
    QASS_TIERS.map(|(tier, family)| {
        models
            .and_then(|m| m.get(tier)?.as_str())
            .filter(|name| tier_model(name).is_ok())
            .unwrap_or(family)
            .to_string()
    })
}

/// `[qass] agent` as the file says it, when it is a name an agent may have.
fn qass_agent_in(text: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(text).ok()?;
    let agent = table.get("qass")?.get("agent")?.as_str()?;
    agent_name(agent).then(|| agent.to_string())
}

/// `[report] send` as the file says it, `ask` when it says nothing usable.
fn report_send_in(text: &str) -> String {
    let table: Option<toml::Table> = toml::from_str(text).ok();
    table
        .as_ref()
        .and_then(|t| t.get("report")?.get("send")?.as_str())
        .filter(|v| REPORT_SENDS.contains(v))
        .unwrap_or("ask")
        .to_string()
}

/// Where the file is.
pub fn path() -> PathBuf {
    qoop_runtime::paths::config_dir().join("config.toml")
}

/// Read the file where it is. No file is every default and nothing to say.
pub fn read() -> Read {
    read_from(&path())
}

/// The agents the file names, read now: what a palette or a phone offers as a new agent.
pub fn agents() -> Vec<Launcher> {
    read().agents
}

/// Read a file by its path, so a test never touches a home directory.
pub fn read_from(path: &Path) -> Read {
    let mut qass_agent = "claude".to_string();
    let mut qass_models = QASS_TIERS.map(|(_, family)| family.to_string());
    let mut report_send = "ask".to_string();
    let (config, agents, problems, exists) = match std::fs::read_to_string(path) {
        Ok(text) => {
            let (config, agents, problems) = parse_all(&text);
            if let Some(agent) = qass_agent_in(&text) {
                qass_agent = agent;
            }
            qass_models = qass_models_in(&text);
            report_send = report_send_in(&text);
            (config, agents, problems, true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (Config::default(), Vec::new(), Vec::new(), false)
        }
        Err(error) => (
            Config::default(),
            Vec::new(),
            vec![Problem::new(None, &format!("could not be read ({error})"))],
            true,
        ),
    };
    Read {
        config,
        agents,
        qass_agent,
        qass_models,
        report_send,
        problems,
        path: path.to_path_buf(),
        exists,
    }
}

/// A named line from `[agents]`: the name a palette and a phone show, and the command
/// typed into a new terminal's shell to start that agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub name: String,
    pub command: String,
}

/// One `[agents]` entry, or why it cannot be one. The command is typed into a shell, so
/// it must be one line: a newline or a tab in it would be typed as that key, and a second
/// line would run on its own.
fn launcher(name: &str, value: &toml::Value) -> Result<Launcher, String> {
    let toml::Value::String(command) = value else {
        return Err(format!(
            "agents.{name} = {value} is not a command line in quotes"
        ));
    };
    let (name, command) = (name.trim(), command.trim());
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err("an agent needs a name of plain characters".to_string());
    }
    if command.is_empty() {
        return Err(format!("agents.{name} has no command"));
    }
    if command.chars().any(char::is_control) {
        return Err(format!("agents.{name} is one line, with no newline or tab"));
    }
    Ok(Launcher {
        name: name.to_string(),
        command: command.to_string(),
    })
}

/// The settings, and everything that could not be used.
pub fn parse(text: &str) -> (Config, Vec<Problem>) {
    let (config, _, problems) = parse_all(text);
    (config, problems)
}

/// The settings, the agents, and everything that could not be used.
fn parse_all(text: &str) -> (Config, Vec<Launcher>, Vec<Problem>) {
    let mut problems: Vec<Problem> = Vec::new();
    let mut text = text.strip_prefix('\u{feff}').unwrap_or(text).to_string();
    // The problem for a construct left open to the end of the file, while it is walked back.
    let mut open_at: Option<usize> = None;
    let table = loop {
        let error = match text.parse::<toml::Table>() {
            Ok(table) => break table,
            Err(error) => error,
        };
        let lines: Vec<&str> = text.lines().collect();
        let blamed = error
            .span()
            .map(|span| line_at(&text, span.start))
            .filter(|&line| lines.get(line - 1).is_some_and(|l| !l.trim().is_empty()));
        // Blamed on nothing (an array or table left open to the end of the file): the cut
        // walks back from the last line with anything on it, and the one problem it
        // reports ends on the line that opened the construct.
        let open = blamed.is_none();
        let Some(line) = blamed.or_else(|| {
            lines
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map(|at| at + 1)
        }) else {
            break toml::Table::new();
        };
        let problem = Problem::new(Some(line), &unreadable(&text, Some(line), error.message()));
        match open_at {
            Some(at) if open => problems[at] = problem,
            _ => {
                // Said once: a line already reported would only be reported again.
                if problems.iter().any(|p| p.line == Some(line)) {
                    break toml::Table::new();
                }
                problems.push(problem);
                open_at = open.then(|| problems.len() - 1);
            }
        }
        if problems.len() > MAX_BAD_LINES {
            break toml::Table::new();
        }
        // Leave the line out and read the rest, so one bad line costs one setting.
        text = lines
            .iter()
            .enumerate()
            .map(|(at, l)| if at + 1 == line { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
    };
    // Where each value is, for the ones refused below. The table itself carries no spans,
    // so the same text is read once more for them, top level and `[notify]`.
    type Spans = BTreeMap<String, toml::Spanned<toml::Value>>;
    #[derive(Default, serde::Deserialize)]
    struct NestedSpans {
        #[serde(default)]
        notify: Spans,
        #[serde(default)]
        notch: Spans,
        #[serde(default)]
        sleep: Spans,
        #[serde(default)]
        daemon: Spans,
        #[serde(default)]
        agents: Spans,
        #[serde(default)]
        qass: Spans,
        #[serde(default)]
        board: Spans,
        report: Spans,
    }
    let top: Spans = toml::from_str(&text).unwrap_or_default();
    let nested: NestedSpans = toml::from_str(&text).unwrap_or_default();
    let line_of = |spans: &Spans, key: &str| spans.get(key).map(|s| line_at(&text, s.span().start));

    let mut config = Config::default();
    // The daemon's sections, judged by the daemon's own rules.
    let (lifecycle, refused) = qoop_runtime::config::lifecycle_from(&table);
    config.lifecycle = lifecycle;
    for (key, what) in refused {
        let (section, name) = key.split_once('.').unwrap_or((key.as_str(), ""));
        let spans = match section {
            "sleep" => &nested.sleep,
            _ => &nested.daemon,
        };
        let line = if name.is_empty() {
            line_of(&top, section)
        } else {
            line_of(spans, name)
        };
        problems.push(Problem::new(line, &what));
    }
    let mut agents: Vec<(Option<usize>, Launcher)> = Vec::new();
    let mut refuse = |line: Option<usize>, what: String| problems.push(Problem::new(line, &what));
    for (key, value) in &table {
        let line = line_of(&top, key);
        match (key.as_str(), value) {
            ("prefix", toml::Value::String(name)) if prefix_byte(name).is_some() => {
                config.prefix = prefix_byte(name).unwrap_or(config.prefix);
            }
            ("theme", toml::Value::String(name)) if name == "auto" => config.theme = Theme::Auto,
            ("theme", toml::Value::String(name)) if name == "dark" => config.theme = Theme::Dark,
            ("theme", toml::Value::String(name)) if name == "light" => {
                config.theme = Theme::Light;
            }
            ("icons", toml::Value::String(name)) if name == "auto" => {
                config.icons = crate::icons::Setting::Auto;
            }
            ("icons", toml::Value::String(name)) if name == "on" => {
                config.icons = crate::icons::Setting::On;
            }
            ("icons", toml::Value::String(name)) if name == "off" => {
                config.icons = crate::icons::Setting::Off;
            }
            ("icons", _) => refuse(
                line,
                format!("icons = {value} is not \"auto\", \"on\" or \"off\""),
            ),
            ("prefix", _) => refuse(line, format!("prefix = {value} is not {PREFIX_SAID}")),
            ("theme", _) => refuse(
                line,
                format!("theme = {value} is not \"auto\", \"dark\" or \"light\""),
            ),
            ("notify", toml::Value::Table(notify)) => {
                for (key, value) in notify {
                    let line = line_of(&nested.notify, key);
                    if let Some(slot) = match key.as_str() {
                        "done_sound" => Some(&mut config.done_sound),
                        "alert_sound" => Some(&mut config.alert_sound),
                        _ => None,
                    } {
                        match value.as_str().and_then(sound_named) {
                            Some(name) => *slot = name,
                            None => refuse(
                                line,
                                format!(
                                    "{key} = {value} is not one of {}",
                                    sounds_here().join(", ")
                                ),
                            ),
                        }
                        continue;
                    }
                    let slot = match key.as_str() {
                        "desktop" => &mut config.desktop,
                        "sound" => &mut config.sound,
                        _ => {
                            refuse(line, format!("there is no setting notify.{key}"));
                            continue;
                        }
                    };
                    match value {
                        toml::Value::Boolean(on) => *slot = *on,
                        _ => refuse(line, format!("{key} = {value} is not true or false")),
                    }
                }
            }
            ("notify", _) => refuse(
                line,
                "notify is a table: [notify] then desktop = true".into(),
            ),
            ("notch", toml::Value::Table(notch)) => {
                for (key, value) in notch {
                    let line = line_of(&nested.notch, key);
                    match (key.as_str(), value) {
                        ("enabled", toml::Value::Boolean(on)) => config.notch = *on,
                        ("enabled", _) => {
                            refuse(line, format!("enabled = {value} is not true or false"));
                        }
                        _ => refuse(line, format!("there is no setting notch.{key}")),
                    }
                }
            }
            ("notch", _) => refuse(line, "notch is a table: [notch] then enabled = true".into()),
            ("board", toml::Value::Table(board)) => {
                for (key, value) in board {
                    let line = line_of(&nested.board, key);
                    match (key.as_str(), value.as_str()) {
                        ("cards", Some("full")) => config.quiet_cards = false,
                        ("cards", Some("quiet")) => config.quiet_cards = true,
                        ("cards", _) => {
                            refuse(
                                line,
                                format!("cards = {value} is not \"full\" or \"quiet\""),
                            );
                        }
                        _ => refuse(line, format!("there is no setting board.{key}")),
                    }
                }
            }
            ("board", _) => refuse(
                line,
                "board is a table: [board] then cards = \"full\"".into(),
            ),
            ("qass", toml::Value::Table(qass)) => {
                for (key, value) in qass {
                    let line = line_of(&nested.qass, key);
                    if let ("models", toml::Value::Table(models)) = (key.as_str(), value) {
                        for (tier, model) in models {
                            let Some(&(_, family)) = QASS_TIERS.iter().find(|(t, _)| t == tier)
                            else {
                                refuse(
                                    line,
                                    format!(
                                        "there is no tier qass.models.{tier}; the tiers are best, mid, low"
                                    ),
                                );
                                continue;
                            };
                            if let Err(why) = model
                                .as_str()
                                .ok_or("not text".to_string())
                                .and_then(tier_model)
                            {
                                refuse(
                                    line,
                                    format!("qass.models.{tier}: {why}; {tier} is {family}"),
                                );
                            }
                        }
                        continue;
                    }
                    match (key.as_str(), value.as_str()) {
                        ("agent", Some(name)) if agent_name(name) => {}
                        ("agent", _) => refuse(
                            line,
                            format!(
                                "agent = {value} is not one of {} or a name from [agents]",
                                QASS_AGENTS.join(", ")
                            ),
                        ),
                        _ => refuse(line, format!("there is no setting qass.{key}")),
                    }
                }
            }
            ("qass", _) => refuse(
                line,
                "qass is a table: [qass] then agent = \"claude\"".into(),
            ),
            ("report", toml::Value::Table(report)) => {
                for (key, value) in report {
                    let line = line_of(&nested.report, key);
                    match (key.as_str(), value.as_str()) {
                        ("send", Some(v)) if REPORT_SENDS.contains(&v) => {}
                        ("send", _) => refuse(
                            line,
                            format!("send = {value} is not one of {}", REPORT_SENDS.join(", ")),
                        ),
                        _ => refuse(line, format!("there is no setting report.{key}")),
                    }
                }
            }
            ("report", _) => refuse(
                line,
                "report is a table: [report] then send = \"ask\"".into(),
            ),
            ("sleep" | "daemon", _) => {}
            ("agents", toml::Value::Table(table)) => {
                for (name, value) in table {
                    let line = line_of(&nested.agents, name);
                    match launcher(name, value) {
                        Ok(agent) => agents.push((line, agent)),
                        Err(why) => refuse(line, why),
                    }
                }
            }
            ("agents", _) => refuse(
                line,
                "agents is a table: [agents] then lm = \"opencode\"".into(),
            ),
            _ => refuse(line, format!("there is no setting {key}")),
        }
    }
    // In the order a person reads the file, not the order the table holds its keys.
    problems.sort_by_key(|p| p.line.unwrap_or(usize::MAX));
    agents.sort_by_key(|(line, _)| line.unwrap_or(usize::MAX));
    (
        config,
        agents.into_iter().map(|(_, a)| a).collect(),
        problems,
    )
}

/// The line a byte offset is on, counted from one.
fn line_at(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Text from the file as the screen may show it: every run of whitespace or control
/// characters becomes one space. A parser's message spans lines, a quoted key can hold a
/// bell, and either one painted raw would run past its row and put the rows after it out of
/// step with what is on the terminal.
fn plain(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What to say about a line that is not TOML. The mistake a person actually makes is a
/// bare word where a value goes, and for that the fix is spelled out: quotes for a word,
/// `true` or `false` for a switch.
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
        Some((key, _))
            if matches!(
                key.rsplit('.').next(),
                Some("desktop" | "sound" | "enabled")
            ) =>
        {
            format!("{} is true or false", key.rsplit('.').next().unwrap_or(key))
        }
        Some((key, value)) => format!("needs quotes: {key} = \"{value}\""),
        None => format!("not TOML ({message})"),
    }
}

/// Every prefix the file may name, in the order a picker lists them.
pub fn prefixes() -> Vec<String> {
    PREFIX_LETTERS
        .chars()
        .map(|letter| format!("ctrl-{letter}"))
        .collect()
}

/// The keys `set` writes, each as the file spells it.
pub const KEYS: [&str; 17] = [
    "prefix",
    "theme",
    "icons",
    "notify.desktop",
    "notify.sound",
    "notify.done_sound",
    "notify.alert_sound",
    "notch.enabled",
    "sleep.idle_minutes",
    "sleep.qass_idle_minutes",
    "daemon.exit_when_asleep",
    "qass.agent",
    "qass.models.best",
    "qass.models.mid",
    "qass.models.low",
    "board.cards",
    "report.send",
];

/// Write one setting into the file at `path`, leaving every other line as it was:
/// comments, order and keys this build does not know. The value is checked first, so
/// the file never holds something the reader would refuse; a file that is not TOML is
/// left alone and said, since writing over it would lose what a person was typing.
pub fn set(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let item = match key {
        "prefix" if prefix_byte(value).is_some() => toml_edit::value(value),
        "prefix" => return Err(format!("prefix is {PREFIX_SAID}")),
        "theme" if matches!(value, "auto" | "dark" | "light") => toml_edit::value(value),
        "theme" => return Err("theme is auto, dark or light".to_string()),
        "icons" if matches!(value, "auto" | "on" | "off") => toml_edit::value(value),
        "icons" => return Err("icons is auto, on or off".to_string()),
        "board.cards" if matches!(value, "full" | "quiet") => toml_edit::value(value),
        "board.cards" => return Err("board.cards is full or quiet".to_string()),
        "notify.done_sound" | "notify.alert_sound" => match sound_named(value) {
            Some(name) => toml_edit::value(name),
            None => return Err(format!("{key} is one of {}", sounds_here().join(", "))),
        },
        "qass.agent" if agent_name(value) => toml_edit::value(value),
        "qass.agent" => {
            return Err(format!(
                "{key} is one of {} or a name from [agents]",
                QASS_AGENTS.join(", ")
            ));
        }
        "qass.models.best" | "qass.models.mid" | "qass.models.low" => match tier_model(value) {
            Ok(()) => toml_edit::value(value),
            Err(why) => return Err(format!("{key}: {why}")),
        },
        "report.send" if REPORT_SENDS.contains(&value) => toml_edit::value(value),
        "report.send" => return Err(format!("{key} is one of {}", REPORT_SENDS.join(", "))),
        "sleep.idle_minutes" | "sleep.qass_idle_minutes" => match value.parse::<i64>() {
            Ok(minutes) if minutes >= 0 => toml_edit::value(minutes),
            _ => return Err(format!("{key} is a whole number of minutes, 0 or more")),
        },
        "notify.desktop" | "notify.sound" | "notch.enabled" | "daemon.exit_when_asleep" => {
            match value {
                "true" => toml_edit::value(true),
                "false" => toml_edit::value(false),
                _ => return Err(format!("{key} is true or false")),
            }
        }
        _ => {
            return Err(format!(
                "there is no setting {key}; the settings are {}",
                KEYS.join(", ")
            ));
        }
    };
    qoop_runtime::config::edit(path, &[(key, item)])
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `set` now writes through the runtime's one core; every key still lands, reads back,
    /// keeps what was around it, and a file that is not TOML is refused with the old words.
    #[test]
    fn every_key_is_written_through_the_shared_core_and_read_back() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\nunknown = 1\n[extra]\nkept = true\n").expect("write");
        let sound = sounds_here()[0];
        let values = [
            ("prefix", "ctrl-b"),
            ("theme", "light"),
            ("icons", "off"),
            ("notify.desktop", "false"),
            ("notify.sound", "true"),
            ("notify.done_sound", sound),
            ("notify.alert_sound", sound),
            ("notch.enabled", "false"),
            ("sleep.idle_minutes", "30"),
            ("sleep.qass_idle_minutes", "0"),
            ("daemon.exit_when_asleep", "false"),
            ("qass.agent", "codex"),
            ("qass.models.best", "opus"),
            ("qass.models.mid", "claude-sonnet-5-5"),
            ("qass.models.low", "haiku"),
            ("board.cards", "quiet"),
            ("report.send", "always"),
        ];
        assert_eq!(values.len(), KEYS.len());
        for (key, value) in values {
            assert!(KEYS.contains(&key), "{key}");
            set(&path, key, value).unwrap_or_else(|why| panic!("{key}: {why}"));
        }
        let read = read_from(&path);
        // The reader names only the two keys of its own that it does not know.
        let named: Vec<_> = read.problems.iter().map(|p| p.what.as_str()).collect();
        assert_eq!(
            named,
            ["there is no setting unknown", "there is no setting extra"]
        );
        assert_eq!(read.config.prefix, 0x02);
        assert!(read.config.quiet_cards);
        assert!(set(&path, "board.cards", "loud").is_err());
        assert_eq!(read.config.theme, Theme::Light);
        assert_eq!(read.config.lifecycle.idle_minutes, 30);
        assert_eq!(read.config.lifecycle.qass_idle_minutes, Some(0));
        assert!(!read.config.lifecycle.exit_when_asleep);
        assert_eq!(read.qass_agent, "codex");
        assert_eq!(read.report_send, "always");
        assert_eq!(read.qass_models, ["opus", "claude-sonnet-5-5", "haiku"]);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("# mine") && text.contains("unknown = 1") && text.contains("kept = true")
        );

        std::fs::write(&path, "[sleep\n").expect("write");
        let said = set(&path, "theme", "dark").expect_err("refused");
        assert!(said.contains("is not TOML; fix it first"), "{said}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "[sleep\n");
    }

    #[test]
    fn the_model_tiers_default_to_the_newest_families_and_never_fable() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        assert_eq!(read_from(&path).qass_models, ["opus", "sonnet", "haiku"]);
        set(&path, "qass.models.low", "claude-haiku-4-5").expect("set");
        let read = read_from(&path);
        assert_eq!(read.qass_models, ["opus", "sonnet", "claude-haiku-4-5"]);
        assert!(read.problems.is_empty(), "{:?}", read.problems);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("[qass.models]"), "{text}");
        for bad in ["fable", "Claude-Fable-5-1", "--dangerous", "two words", ""] {
            assert!(set(&path, "qass.models.best", bad).is_err(), "{bad}");
        }
        std::fs::write(
            &path,
            "[qass.models]\nbest = \"claude-fable-5-1\"\nhuge = \"opus\"\n",
        )
        .expect("write");
        let read = read_from(&path);
        // A Fable model falls back to its tier's family and the file is told so.
        assert_eq!(read.qass_models, ["opus", "sonnet", "haiku"]);
        let said: Vec<_> = read.problems.iter().map(|p| p.what.as_str()).collect();
        assert!(
            said.iter()
                .any(|w| w.contains("Fable") && w.contains("best is opus")),
            "{said:?}"
        );
        assert!(
            said.iter().any(|w| w.contains("qass.models.huge")),
            "{said:?}"
        );
    }

    #[test]
    fn qass_agent_is_read_and_checked() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        assert_eq!(read_from(&path).qass_agent, "claude");
        set(&path, "qass.agent", "codex").expect("set");
        assert!(set(&path, "qass.agent", "two words").is_err());
        let read = read_from(&path);
        assert_eq!(read.qass_agent, "codex");
        assert!(read.problems.is_empty(), "{:?}", read.problems);
        std::fs::write(&path, "[qass]\nagent = \"a b\"\n").expect("write");
        let read = read_from(&path);
        assert_eq!(read.qass_agent, "claude");
        assert_eq!(read.problems.len(), 1);
    }

    /// The agents come in the order the file lists them, not sorted by name, and each is
    /// the command a person wrote, trimmed.
    #[test]
    fn agents_read_in_file_order() {
        let (_, agents, problems) = parse_all(
            "[agents]\nzed = \"claude --dangerously-skip-permissions\"\nlm = \" opencode \"\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        let got: Vec<(&str, &str)> = agents
            .iter()
            .map(|a| (a.name.as_str(), a.command.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("zed", "claude --dangerously-skip-permissions"),
                ("lm", "opencode")
            ]
        );
    }

    /// An entry that is not one line of text is said with its line and left out, and the
    /// rest still start: a newline would be typed as Enter and run a second command.
    #[test]
    fn an_agent_that_is_not_one_line_is_refused() {
        let (_, agents, problems) = parse_all(
            "[agents]\nok = \"codex\"\ntwo = \"\"\"\nclaude\nrm -rf x\"\"\"\nnum = 3\nblank = \"  \"\n",
        );
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "ok");
        let lines: Vec<Option<usize>> = problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, [Some(3), Some(6), Some(7)], "{problems:?}");
        assert!(problems[0].what.contains("one line"), "{problems:?}");
    }

    #[test]
    fn agents_that_are_not_a_table_are_said() {
        let (_, agents, problems) = parse_all("agents = \"claude\"\n");
        assert!(agents.is_empty());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].what.contains("[agents]"), "{problems:?}");
    }

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
            "\u{feff}# mine\nprefix = \"ctrl-b\"\ntheme = 'light'  # bright room\n\n[notify]\ndesktop = false\nsound = true\n[notch]\nenabled = false\n",
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            config,
            Config {
                prefix: 0x02,
                theme: Theme::Light,
                desktop: false,
                sound: true,
                notch: false,
                ..Config::default()
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
            "prefix = ctrl-b\ntheme = \"blue\"\ncolour = \"red\"\n[notify]\nsound = yes\ndesktop = false\nloud = true\n",
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
            "sound is true or false",
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

    /// Nothing from the file reaches the screen with a line break or a control character
    /// in it: the parser's own message spans lines, and a quoted key can hold anything.
    #[test]
    fn no_problem_carries_a_control_character_to_the_screen() {
        let (_, problems) = parse("theme = \u{e9}\n\"a\\u0007b\" = 1\n");
        assert!(!problems.is_empty());
        for problem in &problems {
            assert!(
                !problem.short().chars().any(char::is_control),
                "{problem:?}"
            );
        }
    }

    /// A bare word where a boolean goes is told what a boolean is, not to quote it.
    #[test]
    fn a_bare_word_for_a_switch_is_told_true_or_false() {
        let (config, problems) = parse("[notify]\nsound = yes\ndesktop = false\n");
        assert!(!config.desktop, "the rest is still read");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].short(), "line 2: sound is true or false");
    }

    /// A file that stays broken to its end is said once, not once per retry.
    #[test]
    fn an_unclosed_array_is_said_once() {
        let (config, problems) = parse("theme = \"dark\"\nx = [1,\n");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, Some(2), "{problems:?}");
        assert_eq!(config.theme, Theme::Dark, "the rest is still read");
        // Open across several lines: said once, at the line that opened it.
        let text = "prefix = \"ctrl-b\"\nx = [1,\n2,\n";
        let (config, problems) = parse(text);
        assert_eq!(config.prefix, 0x02, "{problems:?}");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, Some(2), "{problems:?}");
        assert!(text.lines().count() >= 2);
    }

    /// A value refused after parsing still names its line.
    #[test]
    fn a_refused_value_names_its_line() {
        let (_, problems) =
            parse("prefix = \"ctrl-a\"\ntheme = \"blue\"\n[notify]\n\nsound = \"yes\"\n");
        let lines: Vec<Option<usize>> = problems.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![Some(2), Some(5)], "{problems:?}");
    }

    /// The two daemon settings are written as the daemon reads them, a whole number of
    /// minutes and a true or false, and a minus or a fraction is refused.
    #[test]
    fn set_writes_the_daemon_sleep_settings_it_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        set(&file, "sleep.idle_minutes", "20").unwrap();
        set(&file, "daemon.exit_when_asleep", "false").unwrap();
        let lifecycle = read_from(&file).config.lifecycle;
        assert_eq!(
            (lifecycle.idle_minutes, lifecycle.exit_when_asleep),
            (20, false)
        );
        for bad in ["-1", "x", "1.5"] {
            assert!(set(&file, "sleep.idle_minutes", bad).is_err(), "{bad}");
        }
    }

    /// One setting is written and nothing else moves: comments, order and a key this
    /// build does not know stay; a refused value leaves the file untouched.
    #[test]
    fn set_writes_one_key_and_keeps_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("config.toml");
        // No file yet: made with just the key.
        set(&file, "notch.enabled", "false").unwrap();
        assert!(!read_from(&file).config.notch);
        std::fs::write(
            &file,
            "# mine\ntheme = \"dark\"  # night\nlater = 1\n[notify]\nsound = true # beep\n",
        )
        .unwrap();
        set(&file, "notify.sound", "false").unwrap();
        set(&file, "prefix", "ctrl-b").unwrap();
        set(&file, "notch.enabled", "true").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("# mine\ntheme = \"dark\"  # night\nlater = 1\n"),
            "{text}"
        );
        // A changed value keeps the comment on its line.
        assert!(text.contains("sound = false # beep"), "{text}");
        let (config, _) = parse(&text);
        assert_eq!(
            (config.prefix, config.sound, config.notch),
            (0x02, false, true)
        );
        // A sound is written as its file is named, whatever case it was typed in.
        set(&file, "notify.alert_sound", "hero").unwrap();
        // A Linux name is taken too, so one file reads on both.
        set(&file, "notify.done_sound", "Bell").unwrap();
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains("alert_sound = \"Hero\""), "{written}");
        let (config, problems) = parse(&written);
        assert_eq!((config.done_sound, config.alert_sound), ("bell", "Hero"));
        assert!(
            !problems.iter().any(|p| p.what.contains("sound")),
            "{problems:?}"
        );
        let text = written;
        for (key, value) in [
            ("prefix", "ctrl-c"),
            ("theme", "blue"),
            ("notify.sound", "yes"),
            ("notify.done_sound", "Kazoo"),
            ("nope", "1"),
        ] {
            assert!(set(&file, key, value).is_err(), "{key} = {value}");
        }
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            text,
            "a refusal writes nothing"
        );
        // However the table is spelled, it stays spelled that way.
        std::fs::write(&file, "notify = { sound = true }\nnotch.enabled = false\n").unwrap();
        set(&file, "notify.desktop", "false").unwrap();
        set(&file, "notch.enabled", "true").unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("notify = {") && text.contains("desktop = false }"),
            "{text}"
        );
        assert!(text.contains("notch.enabled = true"), "{text}");
        let (config, problems) = parse(&text);
        assert!(
            problems.is_empty() && !config.desktop && config.sound && config.notch,
            "{text}"
        );
        // A file that is not TOML is not written over.
        std::fs::write(&file, "theme = dark\n").unwrap();
        assert!(set(&file, "theme", "light").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "theme = dark\n");
    }

    #[test]
    fn the_daemon_sections_are_read_and_their_mistakes_named_by_line() {
        let (config, problems) =
            parse("[sleep]\nidle_minutes = 20\n[daemon]\nexit_when_asleep = maybe\n");
        assert_eq!(config.lifecycle.idle_minutes, 20);
        assert!(
            config.lifecycle.exit_when_asleep,
            "a bad value kept its default"
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, Some(4), "{problems:?}");
    }

    /// No answer is `ask`; a wrong one is said and read as `ask`, never as `always`.
    #[test]
    fn report_send_is_ask_unless_the_file_says_otherwise() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").expect("write");
        assert_eq!(read_from(&path).report_send, "ask");
        std::fs::write(&path, "[report]\nsend = \"yes\"\n").expect("write");
        let read = read_from(&path);
        assert_eq!(read.report_send, "ask");
        assert_eq!(read.problems.len(), 1, "{:?}", read.problems);
        assert!(set(&path, "report.send", "sometimes").is_err());
        set(&path, "report.send", "never").expect("set");
        assert_eq!(read_from(&path).report_send, "never");
    }
}

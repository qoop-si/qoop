//! The daemon's part of `config.toml`: when a tab goes to sleep by itself, and whether
//! the daemon leaves once nothing is awake.
//!
//! ```toml
//! [sleep]
//! idle_minutes = 0          # 0 = a tab never sleeps by itself
//! qass_idle_minutes = 30    # the same for qass's own space; left out, it follows idle_minutes
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
    /// The same for the tab in qass's space: `0` is never, and `None` (not in the file) is
    /// whatever `idle_minutes` says, as it was before this key.
    pub qass_idle_minutes: Option<u64>,
    /// Leave when nothing is awake and nobody is attached.
    pub exit_when_asleep: bool,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Lifecycle {
            idle_minutes: 0,
            qass_idle_minutes: None,
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
                ("sleep", "qass_idle_minutes", toml::Value::Integer(n)) if *n >= 0 => {
                    settings.qass_idle_minutes = Some(*n as u64);
                }
                ("sleep", "qass_idle_minutes", _) => problems.push((
                    name,
                    format!(
                        "qass_idle_minutes = {value} is not a whole number of minutes, 0 or more"
                    ),
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

/// The person's `config.toml`.
pub fn path() -> std::path::PathBuf {
    crate::paths::config_dir().join("config.toml")
}

/// The settings in the person's `config.toml`.
pub fn read() -> Lifecycle {
    read_from(&path())
}

/// What a person is told about a file that is not TOML.
fn not_toml(path: &Path) -> String {
    format!(
        "{} is not TOML; fix it first (qoop config says where)",
        path.display()
    )
}

/// The settings in the file at `path` for a display: a missing file is every default, but one
/// that cannot be read or is not TOML is an `Err` saying so, since showing its defaults would
/// be a lie and the next write would be refused. A valid file with a value this build cannot
/// use answers the settings it can, and the first such problem as a sentence.
pub fn read_strict(path: &Path) -> Result<(Lifecycle, Option<String>), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Lifecycle::default(), None));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let table = text.parse::<toml::Table>().map_err(|_| not_toml(path))?;
    let (settings, problems) = lifecycle_from(&table);
    Ok((settings, problems.into_iter().next().map(|(_, said)| said)))
}

/// Writers inside this process take turns, so the second reads what the first wrote.
static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Makes each writer's staged file its own, whatever else shares the process id.
static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Put each `(key, value)` into the file at `path`, leaving every other line as it was:
/// comments, order and keys this build does not know. A key is `name` or `table.name`,
/// written the way the file has the table (`[table]`, dotted or inline). A file that is not
/// TOML is left alone and said, since writing over it would lose what a person was typing.
/// The file is replaced whole or not at all, so a reader never meets half of it.
///
/// Against another process this is last writer wins on a whole file, as any editor is.
pub fn edit(path: &Path, edits: &[(&str, toml_edit::Item)]) -> Result<(), String> {
    edit_doc(path, |doc| {
        for (key, item) in edits {
            // Every part but the last is a table, made when missing: `qass.models.best` is the
            // key `best` of `[qass.models]`.
            let (tables, leaf) = key.rsplit_once('.').unwrap_or(("", key));
            let mut at: &mut dyn toml_edit::TableLike = doc.as_table_mut();
            let parts: Vec<&str> = tables.split('.').filter(|t| !t.is_empty()).collect();
            for (n, table) in parts.iter().copied().enumerate() {
                if at.get(table).is_none() {
                    let mut fresh = toml_edit::Table::new();
                    // A table that only holds a deeper one gets no header of its own.
                    fresh.set_implicit(n + 1 < parts.len());
                    at.insert(table, toml_edit::Item::Table(fresh));
                }
                at = match at.get_mut(table).and_then(|t| t.as_table_like_mut()) {
                    Some(next) => next,
                    None => return Err(format!("{table} is not a table in {}", path.display())),
                };
            }
            put(at, leaf, item);
        }
        Ok(())
    })
}

/// Turn usage counts on or off (ADR-023): every `telemetry` key in the file, at any depth, is
/// taken out and one `telemetry` at the top says `on`. A `telemetry = false` that ended up
/// under `[daemon]` would otherwise outlive every later "on".
pub fn set_telemetry(path: &Path, on: bool) -> Result<(), String> {
    edit_doc(path, |doc| {
        strip_telemetry(doc.as_table_mut());
        // A top level key is written above the first table, where a person looks for it.
        doc["telemetry"] = toml_edit::value(on);
        Ok(())
    })
}

fn strip_telemetry(table: &mut dyn toml_edit::TableLike) {
    table.remove("telemetry");
    for (_, item) in table.iter_mut() {
        strip_item(item);
    }
}

fn strip_item(item: &mut toml_edit::Item) {
    match item {
        toml_edit::Item::Table(t) => strip_telemetry(t),
        toml_edit::Item::ArrayOfTables(a) => a.iter_mut().for_each(|t| strip_telemetry(t)),
        toml_edit::Item::Value(v) => strip_value(v),
        toml_edit::Item::None => {}
    }
}

fn strip_value(value: &mut toml_edit::Value) {
    match value {
        toml_edit::Value::InlineTable(t) => strip_telemetry(t),
        toml_edit::Value::Array(a) => a.iter_mut().for_each(strip_value),
        _ => {}
    }
}

/// What the file says about usage counts (ADR-023).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TelemetrySetting {
    /// No `telemetry` key anywhere, or every one is `true`.
    On,
    /// A `telemetry` that is not `true`; the dotted place it was found.
    Off(String),
    /// The file is not TOML or cannot be read: nothing is counted or sent until it is, and
    /// nothing is deleted either, since this is often an editor halfway through a save.
    Unreadable(String),
}

/// The `telemetry` setting in the file at `path`. Every depth is looked at, so a line put
/// below `[daemon]` (`daemon.telemetry`) counts; any value other than `true`, of any type, is
/// off. A missing file is on.
pub fn telemetry_from(path: &Path) -> TelemetrySetting {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TelemetrySetting::On;
        }
        Err(error) => return TelemetrySetting::Unreadable(format!("{}: {error}", path.display())),
    };
    match text.parse::<toml::Table>() {
        Ok(table) => match telemetry_off_in(&toml::Value::Table(table), "") {
            Some(at) => TelemetrySetting::Off(at),
            None => TelemetrySetting::On,
        },
        Err(_) => TelemetrySetting::Unreadable(not_toml(path)),
    }
}

/// The first place under `value` where `telemetry` is anything but `true`.
fn telemetry_off_in(value: &toml::Value, at: &str) -> Option<String> {
    let place = |key: &str| match at {
        "" => key.to_string(),
        _ => format!("{at}.{key}"),
    };
    match value {
        toml::Value::Table(table) => table.iter().find_map(|(key, value)| {
            if key == "telemetry" && value != &toml::Value::Boolean(true) {
                Some(place(key))
            } else {
                telemetry_off_in(value, &place(key))
            }
        }),
        toml::Value::Array(items) => items.iter().find_map(|v| telemetry_off_in(v, at)),
        _ => None,
    }
}

/// Read the file at `path` as TOML, let `change` edit it, and put it back whole: the one way
/// this process writes `config.toml`. A file that is not TOML is left alone and said, since
/// writing over it would lose what a person was typing.
fn edit_doc(
    path: &Path,
    change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    // A panic in one writer must not turn every later write into an error.
    let _one = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|_| not_toml(path))?;
    change(&mut doc)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Through a link to where the file really is (a dotfiles checkout stays linked), with
    // its permissions.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let staged = target.with_extension(format!(
        "toml.{}.{}.new",
        std::process::id(),
        STAGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = std::fs::write(&staged, doc.to_string()).and_then(|()| {
        // On disk before the rename, so a crash cannot leave an empty config.toml behind it.
        std::fs::File::open(&staged)?.sync_all()?;
        if let Ok(meta) = std::fs::metadata(&target) {
            let _ = std::fs::set_permissions(&staged, meta.permissions());
        }
        std::fs::rename(&staged, &target)
    });
    // A full disk or a refused rename leaves no half-written stage behind.
    written.map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        e.to_string()
    })
}

/// `item` under `key`; a value that was there gives its comment on the line to the new one,
/// and its key keeps the comments above it.
fn put(table: &mut dyn toml_edit::TableLike, key: &str, item: &toml_edit::Item) {
    let mut item = item.clone();
    match table.get_mut(key) {
        Some(slot) => {
            if let (Some(old), Some(new)) = (slot.as_value(), item.as_value_mut()) {
                *new.decor_mut() = old.decor().clone();
            }
            *slot = item;
        }
        None => {
            table.insert(key, item);
        }
    }
}

/// Change the two sleep keys a phone or a web page may set, in one rewrite. A number must
/// fit what TOML holds (`i64`); `qoop config set` takes the same numbers.
pub fn write_sleep(
    path: &Path,
    idle_minutes: Option<u64>,
    exit_when_asleep: Option<bool>,
) -> Result<(), String> {
    let mut edits = Vec::new();
    if let Some(minutes) = idle_minutes {
        let minutes = i64::try_from(minutes).map_err(|_| {
            "sleep.idle_minutes is a whole number of minutes, 0 or more".to_string()
        })?;
        edits.push(("sleep.idle_minutes", toml_edit::value(minutes)));
    }
    if let Some(on) = exit_when_asleep {
        edits.push(("daemon.exit_when_asleep", toml_edit::value(on)));
    }
    if edits.is_empty() {
        return Ok(());
    }
    edit(path, &edits)
}

/// `[notify] mute`: who stays quiet when an agent needs a person. The interface reads it for
/// its desktop notes, sounds and bell, and the bridge for the phones' alerts, so the rule
/// lives here once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mute {
    #[default]
    Off,
    /// This Mac says nothing: no desktop note, no sound, no bell. The phones still hear.
    Here,
    /// And the phones get no alert from this Mac either.
    Everywhere,
}

/// The words `notify.mute` takes, in the order a click goes through them.
pub const MUTES: [&str; 3] = ["off", "here", "everywhere"];

impl Mute {
    pub fn name(self) -> &'static str {
        match self {
            Mute::Off => "off",
            Mute::Here => "here",
            Mute::Everywhere => "everywhere",
        }
    }

    pub fn parse(word: &str) -> Option<Mute> {
        [Mute::Off, Mute::Here, Mute::Everywhere]
            .into_iter()
            .find(|mute| mute.name() == word)
    }

    /// The state a click on the bell moves to.
    pub fn next(self) -> Mute {
        match self {
            Mute::Off => Mute::Here,
            Mute::Here => Mute::Everywhere,
            Mute::Everywhere => Mute::Off,
        }
    }
}

/// `[notify] mute` from a parsed file: off when it is not there or not one of [`MUTES`].
pub fn mute_from(table: &toml::Table) -> Mute {
    table
        .get("notify")
        .and_then(|notify| notify.get("mute"))
        .and_then(|mute| mute.as_str())
        .and_then(Mute::parse)
        .unwrap_or_default()
}

/// `[notify] mute` in the file at `path`; a missing or unreadable file is off.
pub fn mute_in(path: &Path) -> Mute {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .map(|table| mute_from(&table))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_reads_its_three_words_and_anything_else_is_off() {
        let mute = |text: &str| mute_from(&text.parse::<toml::Table>().expect("toml"));
        assert_eq!(mute(""), Mute::Off);
        assert_eq!(mute("[notify]\nsound = true\n"), Mute::Off);
        assert_eq!(mute("[notify]\nmute = \"here\"\n"), Mute::Here);
        assert_eq!(mute("notify.mute = \"everywhere\"\n"), Mute::Everywhere);
        assert_eq!(mute("[notify]\nmute = \"loud\"\n"), Mute::Off);
        assert_eq!(mute("[notify]\nmute = true\n"), Mute::Off);
        assert_eq!(mute("notify = 1\n"), Mute::Off);
    }

    #[test]
    fn a_file_that_is_missing_or_not_toml_is_not_muted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        assert_eq!(mute_in(&path), Mute::Off);
        std::fs::write(&path, "[notify]\nmute = \"here\"\n").expect("write");
        assert_eq!(mute_in(&path), Mute::Here);
        std::fs::write(&path, "[notify\nmute = \"here\"\n").expect("write");
        assert_eq!(mute_in(&path), Mute::Off);
    }

    #[test]
    fn a_click_goes_off_here_everywhere_and_back() {
        let mut seen = vec![Mute::Off];
        for _ in 0..3 {
            seen.push(seen.last().expect("one").next());
        }
        assert_eq!(
            seen.iter().map(|m| m.name()).collect::<Vec<_>>(),
            [MUTES.as_slice(), &["off"]].concat()
        );
        assert!(
            MUTES
                .iter()
                .all(|word| Mute::parse(word).is_some_and(|m| m.name() == *word))
        );
    }

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
                qass_idle_minutes: None,
                exit_when_asleep: false
            }
        );
    }

    #[test]
    fn qass_has_its_own_idle_minutes() {
        let (settings, problems) = parse("[sleep]\nidle_minutes = 30\nqass_idle_minutes = 0\n");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(settings.qass_idle_minutes, Some(0));
        let (settings, problems) = parse("[sleep]\nqass_idle_minutes = -1\n");
        assert_eq!(settings.qass_idle_minutes, None);
        assert_eq!(problems[0].0, "sleep.qass_idle_minutes");
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

    fn file_with(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write");
        (dir, path)
    }

    #[test]
    fn write_sleep_changes_each_key_and_both_in_one_rewrite() {
        let (_dir, path) = file_with("# mine\ntheme = \"dark\"\n");
        write_sleep(&path, Some(15), None).expect("idle");
        assert_eq!(read_from(&path).idle_minutes, 15);
        assert!(read_from(&path).exit_when_asleep);
        write_sleep(&path, None, Some(false)).expect("leave");
        assert!(!read_from(&path).exit_when_asleep);
        assert_eq!(read_from(&path).idle_minutes, 15);
        write_sleep(&path, Some(0), Some(true)).expect("both");
        let after = read_from(&path);
        assert_eq!((after.idle_minutes, after.exit_when_asleep), (0, true));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("# mine") && text.contains("theme = \"dark\""),
            "{text}"
        );
        // Nothing to change is not a rewrite.
        let before = std::fs::metadata(&path)
            .expect("meta")
            .modified()
            .expect("mtime");
        write_sleep(&path, None, None).expect("noop");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("meta")
                .modified()
                .expect("mtime"),
            before
        );
    }

    #[test]
    fn a_number_toml_cannot_hold_is_refused_and_writes_nothing() {
        let (_dir, path) = file_with("[sleep]\nidle_minutes = 5\n");
        let said = write_sleep(&path, Some(i64::MAX as u64 + 1), Some(false)).expect_err("refused");
        assert!(said.contains("idle_minutes"), "{said}");
        assert_eq!(read_from(&path).idle_minutes, 5);
        assert!(
            read_from(&path).exit_when_asleep,
            "the other key waited too"
        );
        write_sleep(&path, Some(i64::MAX as u64), None).expect("the largest TOML holds");
        assert_eq!(read_from(&path).idle_minutes, i64::MAX as u64);
    }

    #[test]
    fn a_file_that_is_not_toml_is_left_alone() {
        let (_dir, path) = file_with("[sleep\nidle_minutes = ");
        let said = write_sleep(&path, Some(15), None).expect_err("refused");
        assert!(said.contains("is not TOML"), "{said}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[sleep\nidle_minutes = "
        );
    }

    #[test]
    fn edit_keeps_the_form_the_file_has() {
        let (_dir, path) = file_with(
            "# top\n[sleep]\n# how long\nidle_minutes = 5 # five\nnap = 1\n[daemon]\nexit_when_asleep = true\n[other]\nkept = \"yes\"\n",
        );
        write_sleep(&path, Some(30), Some(false)).expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("# top") && text.contains("# how long"),
            "{text}"
        );
        assert!(
            text.contains("nap = 1") && text.contains("kept = \"yes\""),
            "{text}"
        );
        assert!(text.contains("idle_minutes = 30"), "{text}");
        assert!(text.contains("exit_when_asleep = false"), "{text}");

        let (_dir, dotted) = file_with("sleep.idle_minutes = 5\n");
        write_sleep(&dotted, Some(60), None).expect("dotted");
        assert_eq!(read_from(&dotted).idle_minutes, 60);

        let (_dir, inline) = file_with("daemon = { exit_when_asleep = true }\n");
        write_sleep(&inline, None, Some(false)).expect("inline");
        assert!(!read_from(&inline).exit_when_asleep);
        assert!(
            std::fs::read_to_string(&inline)
                .expect("read")
                .contains("daemon = {"),
            "an inline table stays inline"
        );

        let missing = tempfile::tempdir().expect("tempdir");
        let fresh = missing.path().join("deeper").join("config.toml");
        write_sleep(&fresh, Some(15), None).expect("a missing file is made");
        assert_eq!(read_from(&fresh).idle_minutes, 15);
    }

    #[test]
    fn a_linked_file_stays_linked_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("dotfiles.toml");
        std::fs::write(&real, "# mine\n").expect("write");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        write_sleep(&link, Some(15), None).expect("write");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert_eq!(read_from(&real).idle_minutes, 15);
        let mode = std::fs::metadata(&real).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn writers_at_once_take_turns_and_none_is_lost() {
        let (dir, path) = file_with("# kept\n");
        let handles: Vec<_> = (0..32)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let key = format!("writer.k{n}");
                    edit(&path, &[(key.as_str(), toml_edit::value(n as i64))]).expect("edit");
                    // A reader at any moment meets a whole file.
                    let text = std::fs::read_to_string(&path).expect("read");
                    text.parse::<toml::Table>().expect("always valid TOML");
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("writer");
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let table: toml::Table = text.parse().expect("toml");
        let writer = table["writer"].as_table().expect("table");
        assert_eq!(writer.len(), 32, "a writer's key was lost:\n{text}");
        assert!(text.contains("# kept"));
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("config.toml")],
            "staged files left"
        );
    }

    #[test]
    fn a_panic_in_one_writer_does_not_stop_the_next() {
        let (_dir, path) = file_with("");
        let poisoner = std::thread::spawn(|| {
            let _held = WRITING.lock().unwrap();
            panic!("a writer died holding the lock");
        });
        assert!(poisoner.join().is_err());
        assert!(WRITING.is_poisoned());
        write_sleep(&path, Some(15), None).expect("still writes");
        assert_eq!(read_from(&path).idle_minutes, 15);
    }

    #[test]
    fn read_strict_tells_a_missing_file_a_bad_value_and_a_broken_file_apart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (life, problem) = read_strict(&dir.path().join("none.toml")).expect("missing");
        assert_eq!((life, problem), (Lifecycle::default(), None));

        let (_d, valid) =
            file_with("[sleep]\nidle_minutes = 30\n[daemon]\nexit_when_asleep = false\n");
        let (life, problem) = read_strict(&valid).expect("valid");
        assert_eq!(
            (life.idle_minutes, life.exit_when_asleep, problem),
            (30, false, None)
        );

        let (_d, bad) =
            file_with("[sleep]\nidle_minutes = -5\n[daemon]\nexit_when_asleep = false\n");
        let (life, problem) = read_strict(&bad).expect("a bad value still answers");
        assert_eq!((life.idle_minutes, life.exit_when_asleep), (0, false));
        assert!(problem.expect("said").contains("idle_minutes"));

        let (_d, broken) = file_with("[sleep\nidle_minutes = ");
        let said = read_strict(&broken).expect_err("not TOML is not defaults");
        assert!(said.contains("is not TOML"), "{said}");
    }

    #[test]
    fn telemetry_is_off_for_anything_but_true_at_any_depth_and_paused_when_broken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let on = TelemetrySetting::On;
        assert_eq!(telemetry_from(&dir.path().join("none.toml")), on);
        for (text, expect) in [
            ("", on.clone()),
            ("telemetry = true\n", on.clone()),
            (
                "telemetry = false\n",
                TelemetrySetting::Off("telemetry".into()),
            ),
            (
                "telemetry = \"false\"\n",
                TelemetrySetting::Off("telemetry".into()),
            ),
            ("telemetry = 0\n", TelemetrySetting::Off("telemetry".into())),
            (
                "[telemetry]\nx = 1\n",
                TelemetrySetting::Off("telemetry".into()),
            ),
            (
                "[daemon]\nexit_when_asleep = true\ntelemetry = false\n",
                TelemetrySetting::Off("daemon.telemetry".into()),
            ),
            (
                "[a.b]\ntelemetry = false\n",
                TelemetrySetting::Off("a.b.telemetry".into()),
            ),
            (
                "[[x]]\ntelemetry = true\n[[x]]\ntelemetry = 1\n",
                TelemetrySetting::Off("x.telemetry".into()),
            ),
            (
                "items = [{ telemetry = false }]\n",
                TelemetrySetting::Off("items.telemetry".into()),
            ),
        ] {
            let (_d, path) = file_with(text);
            assert_eq!(telemetry_from(&path), expect, "{text:?}");
        }
        let (_d, broken) = file_with("[sleep\nidle_minutes = ");
        assert!(matches!(
            telemetry_from(&broken),
            TelemetrySetting::Unreadable(said) if said.contains("is not TOML")
        ));
    }

    #[test]
    fn turning_telemetry_on_takes_every_nested_off_away_and_keeps_comments() {
        let (_d, path) = file_with(
            "# mine\n[daemon]\nexit_when_asleep = true # stays\ntelemetry = false\n\
             [[x]]\ntelemetry = false\nitems = [{ telemetry = false, keep = 1 }]\n",
        );
        set_telemetry(&path, true).expect("on");
        assert_eq!(telemetry_from(&path), TelemetrySetting::On);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("telemetry = true\n# mine\n"), "{text}");
        assert!(text.contains("exit_when_asleep = true # stays"), "{text}");
        assert!(text.contains("keep = 1"), "{text}");
        assert_eq!(text.matches("telemetry").count(), 1, "{text}");

        set_telemetry(&path, false).expect("off");
        assert_eq!(
            telemetry_from(&path),
            TelemetrySetting::Off("telemetry".into())
        );

        let (_d, broken) = file_with("[sleep\nidle_minutes = ");
        let said = set_telemetry(&broken, true).expect_err("not TOML is left alone");
        assert!(said.contains("is not TOML"), "{said}");
    }
}

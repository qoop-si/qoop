//! The remotes a person has added, and the one command that reaches each.
//!
//! A remote is a name for the interface and an ssh target for the person's own ssh config
//! to resolve. Nothing else: the host, the user, the key
//! and the jump host all live in `~/.ssh/config`, where the person already keeps them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Overrides where the list is kept, so tests never touch a home.
pub const REMOTES_ENV: &str = "QORO_REMOTES";

/// The line the remote command prints right before qoro starts. Everything before it is
/// whatever the remote's shell startup printed, and a banner read as a frame is a
/// connection that fails on every attempt with a message about frame lengths.
pub const RELAY_MARKER: &str = "-----qoro-relay-----";

/// How much a remote shell may print before the marker. A login banner is a few lines; a
/// megabyte is a shell that is not going to start qoro.
pub const MARKER_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub target: String,
}

/// Where the list lives: the override when set, else `qoro/remotes.json` under the
/// person's own configuration, beside the avatar.
pub fn path() -> PathBuf {
    match std::env::var(REMOTES_ENV) {
        Ok(path) if !path.is_empty() => return PathBuf::from(path),
        _ => {}
    }
    // An empty or relative XDG_CONFIG_HOME is ignored, as the spec says and as
    // `qoro_runtime::paths::config_dir` does: a relative one resolved against wherever
    // the process happened to start.
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .ok()
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".config"))
                .ok()
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("qoro").join("remotes.json")
}

/// The list, or an empty one when there is no file. A file that does not parse is an
/// error rather than an empty list: saving over it would lose what the person wrote.
pub fn load_from(path: &Path) -> std::io::Result<Vec<Remote>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut list: Vec<Remote> = serde_json::from_str(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?;
            // A list written when a machine had several sessions can name one machine
            // twice; it has one daemon now, and twice would list its spaces twice.
            let mut seen = std::collections::HashSet::new();
            list.retain(|r| seen.insert(r.target.clone()));
            Ok(list)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub fn load() -> std::io::Result<Vec<Remote>> {
    load_from(&path())
}

pub fn save_to(path: &Path, remotes: &[Remote]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(remotes).map_err(std::io::Error::other)?;
    // Written beside it and renamed over it, so a reader never sees half a list and a
    // write cut short leaves the old one whole.
    let partial = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&partial, text + "\n")?;
    std::fs::rename(&partial, path)
}

/// Whether a name can be a remote's name.
///
/// It is shown before a colon in the space list and typed before one to make a space
/// over there, so a colon or a space in it would make the prefix ambiguous.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether a target can be handed to ssh as its host argument.
///
/// A leading `-` would be read as an option, which is how a host name becomes
/// `-oProxyCommand=...`.
pub fn valid_target(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('-')
        && !target.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// One word for a posix shell, whatever is in it.
pub fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The command ssh runs on the remote: print the marker, then run qoro's `verb`
/// (`relay`, or `--hello`).
///
/// `--session default` is passed on purpose: a qoro from before sessions were folded into
/// spaces opens the last session opened there when given none, and this machine's spaces
/// are the default one's. A newer qoro accepts and ignores it.
///
/// Run through `sh -c` so a fish login shell runs it as well, and the verb is a positional
/// parameter rather than part of the script: data spliced into a script is a script
/// somebody else wrote. qoro is looked for on the PATH and in the two places an install
/// puts it, because a non-interactive ssh command gets a PATH without whatever the
/// person's interactive profile adds.
pub fn remote_command(verb: &str) -> String {
    let script = format!(
        "printf '%s\\n' {RELAY_MARKER}; \
         for q in qoro \"$HOME/.local/bin/qoro\" \"$HOME/.cargo/bin/qoro\"; do \
         if command -v \"$q\" >/dev/null 2>&1; then exec \"$q\" --session default \"$1\"; fi; \
         done; echo 'qoro is not installed on this host' >&2; exit 127"
    );
    format!("sh -c {} qoro {}", shell_quote(&script), shell_quote(verb))
}

/// A name for a machine added by its address: the host, without the user, with anything a
/// name cannot hold turned into `-`, and a number after it when the name is taken.
pub fn name_for(target: &str, taken: &[&str]) -> String {
    let host = target.rsplit_once('@').map_or(target, |(_, host)| host);
    let base: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(20)
        .collect();
    // Never a leading dash: the name is typed after `qoro remote rm`, where it would read
    // as a flag.
    let mut base = base.trim_start_matches('-').to_string();
    if base.is_empty() {
        base = "remote".to_string();
    }
    let mut name = base.clone();
    let mut n = 2;
    while taken.contains(&name.as_str()) {
        name = format!("{base}-{n}");
        n += 1;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_is_named_after_its_host_and_never_twice_the_same() {
        assert_eq!(name_for("admin@10.0.0.5", &[]), "10.0.0.5");
        assert_eq!(name_for("devbox", &[]), "devbox");
        assert_eq!(name_for("me@devbox", &["devbox"]), "devbox-2");
        assert_eq!(name_for("me@devbox", &["devbox", "devbox-2"]), "devbox-3");
        assert_eq!(name_for("me@[fe80::1]", &[]), "fe80--1-");
        assert!(valid_name(&name_for("me@[fe80::1]", &[])));
        assert_eq!(name_for("me@", &[]), "remote");
    }

    #[test]
    fn a_missing_file_is_an_empty_list_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("remotes.json");
        assert_eq!(load_from(&file).expect("missing"), Vec::new());
        std::fs::write(&file, "{ not json").expect("write");
        assert!(load_from(&file).is_err());
    }

    #[test]
    fn a_saved_list_reads_back_and_an_old_one_names_each_machine_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("qoro").join("remotes.json");
        let list = vec![Remote {
            name: "devbox".into(),
            target: "me@devbox".into(),
        }];
        save_to(&file, &list).expect("save");
        assert_eq!(load_from(&file).expect("load"), list);
        // Written when a machine had sessions: the field is read past, and a second entry
        // for the same machine is dropped.
        std::fs::write(
            &file,
            r#"[{"name":"a","target":"b","session":"work"},{"name":"a2","target":"b","session":"play"}]"#,
        )
        .expect("write");
        assert_eq!(
            load_from(&file).expect("load"),
            vec![Remote {
                name: "a".into(),
                target: "b".into()
            }]
        );
    }

    #[test]
    fn names_and_targets_that_would_be_misread_are_refused() {
        assert!(valid_name("devbox-2.eu"));
        for bad in ["", "dev box", "dev:box", "dév", &"x".repeat(25)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_target("me@host"));
        for bad in ["", "-oProxyCommand=x", "a b", "a\nb"] {
            assert!(!valid_target(bad), "{bad}");
        }
    }

    /// The command goes through the remote's login shell and then `sh`. Run it through a
    /// real `sh` here with a verb built to break out of quoting: it must arrive as one
    /// argument after `--session default`, and nothing in it may run.
    #[test]
    fn a_hostile_verb_arrives_as_data_and_runs_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = dir.path().join("ran");
        let verb = format!(
            "x'; touch {} ; echo '$(touch {})",
            probe.display(),
            probe.display()
        );
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let fake = bin.join("qoro");
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s|' \"$@\"\n").expect("fake");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_command(&verb))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .expect("sh");
        let text = String::from_utf8_lossy(&out.stdout);
        let after = text
            .split_once(RELAY_MARKER)
            .expect("marker")
            .1
            .trim_start();
        assert_eq!(after, format!("--session|default|{verb}|"));
        assert!(!probe.exists(), "the verb ran as a command");
    }

    #[test]
    fn a_host_without_qoro_says_so() {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(remote_command("relay"))
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent")
            .output()
            .expect("sh");
        assert_eq!(out.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&out.stderr).contains("not installed"));
    }
}

//! The remotes a person has added, and the one command that reaches each.
//!
//! A remote is a name for the interface and an ssh target for the person's own ssh config
//! to resolve: a host, or the ssh command a person types to reach it, with its port, user,
//! key and jump host (see [`parse`]). Everything else lives in `~/.ssh/config`, where the
//! person already keeps it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Overrides where the list is kept, so tests never touch a home.
pub const REMOTES_ENV: &str = "QOOP_REMOTES";

/// The line the remote command prints right before qoop starts. Everything before it is
/// whatever the remote's shell startup printed, and a banner read as a frame is a
/// connection that fails on every attempt with a message about frame lengths.
// Fixed wire label kept from the old name: crosses the link to remotes of any version.
pub const RELAY_MARKER: &str = "-----qoro-relay-----";

/// How much a remote shell may print before the marker. A login banner is a few lines; a
/// megabyte is a shell that is not going to start qoop.
pub const MARKER_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub target: String,
}

/// Where the list lives: the override when set, else `qoop/remotes.json` under the
/// person's own configuration, beside the avatar.
pub fn path() -> PathBuf {
    match std::env::var(REMOTES_ENV) {
        Ok(path) if !path.is_empty() => return PathBuf::from(path),
        _ => {}
    }
    // An empty or relative XDG_CONFIG_HOME is ignored, as the spec says and as
    // `qoop_runtime::paths::config_dir` does: a relative one resolved against wherever
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
    base.join("qoop").join("remotes.json")
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

/// Whether a target can be handed to ssh: a host, or the ssh command a person types to reach
/// it (see [`parse`]).
pub fn valid_target(target: &str) -> bool {
    parse(target).is_some()
}

/// A target taken apart: the ssh options a person gave, then the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub options: Vec<String>,
    pub host: String,
}

/// ssh's flags that take a value and are let through: the port, the user, the key and the
/// jump host. Everything else a connection needs lives in `~/.ssh/config`.
const VALUE_FLAGS: [char; 4] = ['p', 'l', 'i', 'J'];
/// ssh's flags without a value that are let through: the address family.
const PLAIN_FLAGS: [char; 2] = ['4', '6'];
/// The `-o` options let through, the long names of the flags above and two more a typed
/// command carries. Every other option can run a command here (`ProxyCommand`,
/// `LocalCommand`, `KnownHostsCommand`), swap the config, forward the agent or change what
/// qoop runs over the connection, and a target in `remotes.json` is run by the background
/// fleet with nobody watching.
const OPTIONS: [&str; 6] = [
    "Port",
    "User",
    "IdentityFile",
    "ProxyJump",
    "HostName",
    "IdentitiesOnly",
];

/// A target as ssh's options and its host: `me@host`, or the command a person types,
/// `ssh -p 2222 -i ~/.ssh/k -J jump me@host`, with or without the leading `ssh`. `None` for
/// anything else: an option not let through (see [`VALUE_FLAGS`], [`PLAIN_FLAGS`],
/// [`OPTIONS`]), a word with a character outside [`SAFE`] (a quote, a space inside a word, a
/// `;`), a value or a host that starts with `-`, no host, or a command after the host.
pub fn parse(text: &str) -> Option<Target> {
    let mut words = text.split_whitespace().peekable();
    if words.peek() == Some(&"ssh") {
        words.next();
    }
    let mut options = Vec::new();
    let mut host: Option<&str> = None;
    while let Some(word) = words.next() {
        if !word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || SAFE.contains(c))
        {
            return None;
        }
        let Some(flags) = word.strip_prefix('-') else {
            // One host, and nothing after it: a command there would run instead of qoop.
            if host.replace(word).is_some() {
                return None;
            }
            continue;
        };
        let mut chars = flags.chars();
        let flag = chars.next()?;
        let joined = chars.as_str();
        if PLAIN_FLAGS.contains(&flag) && joined.is_empty() {
            options.push(word.to_string());
            continue;
        }
        if !VALUE_FLAGS.contains(&flag) && flag != 'o' {
            return None;
        }
        let value = if joined.is_empty() {
            words.next()?
        } else {
            joined
        };
        if value.starts_with('-')
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || SAFE.contains(c))
        {
            return None;
        }
        if flag == 'o' {
            let (name, set) = value.split_once('=')?;
            if set.is_empty() || !OPTIONS.iter().any(|o| o.eq_ignore_ascii_case(name)) {
                return None;
            }
        }
        // A jump list is hosts, each of which ssh reads as a command line of its own: none
        // may start like an option.
        let hops = match value.split_once('=') {
            Some((_, set)) if flag == 'o' => set,
            _ => value,
        };
        if hops
            .split(',')
            .any(|hop| hop.is_empty() || hop.starts_with('-'))
        {
            return None;
        }
        options.push(format!("-{flag}"));
        options.push(value.to_string());
    }
    let host = host.filter(|h| !h.starts_with('-'))?;
    Some(Target {
        options,
        host: host.to_string(),
    })
}

/// The characters a word may hold besides letters and digits: what hosts, users, ports,
/// paths and jump specs are written with, and nothing a shell reads as more than a letter.
/// A `-J` value ends up in a command ssh runs through a shell.
const SAFE: &str = "._-@:,/~%+[]=";

/// The one way a target is written down: the options, then the host, one space between.
/// A bare host stays as it was typed, so a list written before options were allowed reads
/// the same.
pub fn canonical(text: &str) -> Option<String> {
    let target = parse(text)?;
    let mut words = target.options;
    words.push(target.host);
    Some(words.join(" "))
}

/// One word for a shell, whatever is in it: an ssh command runs in the remote's login shell,
/// which may be fish.
pub use qoop_domain::shell::quote as shell_quote;

/// The command ssh runs on the remote: print the marker, then run qoop's `verb`
/// (`relay`, or `--hello`).
///
/// `--session default` is passed on purpose: a qoop from before sessions were folded into
/// spaces opens the last session opened there when given none, and this machine's spaces
/// are the default one's. A newer qoop accepts and ignores it.
///
/// Run through `sh -c` so a fish login shell runs it as well, and the verb is a positional
/// parameter rather than part of the script: data spliced into a script is a script
/// somebody else wrote. qoop is looked for on the PATH and in the two places an install
/// puts it, because a non-interactive ssh command gets a PATH without whatever the
/// person's interactive profile adds. A machine that has not been updated since the
/// rename has it as `qoro`: asked for second, and gone with the last 0.7.x machine.
pub fn remote_command(verb: &str) -> String {
    let script = format!(
        "printf '%s\\n' {RELAY_MARKER}; \
         for q in qoop \"$HOME/.local/bin/qoop\" \"$HOME/.cargo/bin/qoop\" \
         qoro \"$HOME/.local/bin/qoro\" \"$HOME/.cargo/bin/qoro\"; do \
         if command -v \"$q\" >/dev/null 2>&1; then exec \"$q\" --session default \"$1\"; fi; \
         done; echo 'qoop is not installed on this host' >&2; exit 127"
    );
    format!("sh -c {} qoop {}", shell_quote(&script), shell_quote(verb))
}

/// A name for a machine added by its address: the host, without the user or the options,
/// with anything a name cannot hold turned into `-`, and a number after it when the name is
/// taken.
pub fn name_for(target: &str, taken: &[&str]) -> String {
    let parsed = parse(target).map(|t| t.host);
    let target = parsed.as_deref().unwrap_or(target);
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
    // Never a leading dash: the name is typed after `qoop remote rm`, where it would read
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
        let file = dir.path().join("qoop").join("remotes.json");
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

    /// The ssh command a person types is taken apart into the options let through and the
    /// host, written down one way, and anything that could run a command, forward the agent
    /// or replace what qoop runs over the connection is refused.
    #[test]
    fn a_typed_ssh_command_keeps_its_port_key_and_jump_and_nothing_else() {
        let typed = "ssh -p 2222 -i ~/.ssh/work -J me@bastion:22 -4 -oIdentitiesOnly=yes -l me box";
        let target = parse(typed).expect("parses");
        assert_eq!(target.host, "box");
        assert_eq!(
            target.options,
            [
                "-p",
                "2222",
                "-i",
                "~/.ssh/work",
                "-J",
                "me@bastion:22",
                "-4",
                "-o",
                "IdentitiesOnly=yes",
                "-l",
                "me"
            ]
        );
        let once = canonical(typed).expect("canonical");
        assert_eq!(
            once,
            "-p 2222 -i ~/.ssh/work -J me@bastion:22 -4 -o IdentitiesOnly=yes -l me box"
        );
        // Written down again it is the same, so a second connect finds the same machine.
        assert_eq!(canonical(&once).as_deref(), Some(once.as_str()));
        assert_eq!(canonical("me@host").as_deref(), Some("me@host"));
        assert_eq!(canonical("  ssh   me@host ").as_deref(), Some("me@host"));
        assert_eq!(
            canonical("-p2222 me@host").as_deref(),
            Some("-p 2222 me@host")
        );
        assert_eq!(name_for(&once, &[]), "box");
        for bad in [
            "ssh",
            "ssh -J a,-oProxyCommand=x me@host",
            "ssh -o ProxyJump=a,-x me@host",
            "ssh -p 22",
            "ssh me@host uptime",
            "ssh -A me@host",
            "ssh -X me@host",
            "ssh -v me@host",
            "ssh -N me@host",
            "ssh -F /tmp/c me@host",
            "ssh -o ProxyCommand=nc me@host",
            "ssh -oLocalCommand=x me@host",
            "ssh -o Port me@host",
            "ssh -o Port=22 -- me@host",
            "ssh -J -oProxyCommand=x me@host",
            "ssh -J 'a;touch /tmp/x' me@host",
            "ssh -J a$(id) me@host",
            "ssh -i \"my key\" me@host",
            "ssh -p",
            "ssh -46 me@host",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
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
        let fake = bin.join("qoop");
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
    fn a_host_without_qoop_says_so() {
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

    /// The line before the protocol is the old one: the remote's shell prints what the
    /// script it was sent says, and a remote on 0.7.x is also read by Macs of that version.
    #[test]
    fn the_relay_marker_is_the_old_string() {
        assert_eq!(RELAY_MARKER, "-----qoro-relay-----");
    }
}

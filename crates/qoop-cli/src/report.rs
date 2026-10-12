//! `qoop report`: qass's notes to qoop's maker about its own way of working.
//!
//! `draft` reads the five fields on stdin, checks them (`qoop_domain::report`, plus the
//! person's own names, read here and never sent), and keeps the note in qass's folder with the
//! exact bytes it would send. `list`, `show` and `rm` work on what is kept. `send`, `delete`
//! and `inbox` talk to the hub, so they belong to the qoop release (`qoop login` is there too);
//! it uses [`load`], [`may_send`] and [`mark_sent`] from here.

use std::path::{Path, PathBuf};
use std::process::Command;

use qoop_domain::report::{self, AGENTS, Note};
use qoop_runtime::paths;
use qoop_tui::config;

const USAGE: &str = "\
usage: qoop report draft            read area, what, cost, often, fix on stdin; check and keep it
       qoop report list             the kept notes, and which were sent
       qoop report show <n>         one note, as it would be sent
       qoop report rm <n>           forget a kept note (a sent one: `qoop report delete <n>` first)
       qoop report send <n> [--yes] send one note (`--yes`: the person said yes to it)
       qoop report delete <n>       take a sent note off the hub
       qoop report inbox            the notes people sent (the hub's owner only)";

pub fn run(args: &[String]) -> std::io::Result<()> {
    let rest: Vec<&str> = args[2..].iter().map(String::as_str).collect();
    match rest.as_slice() {
        ["draft"] => draft(),
        ["list"] => list(),
        ["show", n] => {
            print!("{}", load(number(n)?)?.1.text());
            Ok(())
        }
        ["rm", n] => remove(number(n)?),
        ["-h" | "--help"] | [] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(std::io::Error::other(USAGE)),
    }
}

/// Where notes are kept: `reports` in qass's folder.
pub fn folder() -> std::io::Result<PathBuf> {
    paths::qass_data_dir()
        .map(|d| d.join("reports"))
        .ok_or_else(|| std::io::Error::other("no qass folder: start qass once with `qoop qass`"))
}

/// A kept note and its file, checked again: a note kept while signed out is sent later.
pub fn load(n: u32) -> std::io::Result<(PathBuf, Note)> {
    let path = folder()?.join(format!("{n}.txt"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| std::io::Error::other(format!("note {n}: {e}")))?;
    let mut note = Note::parse(&text).map_err(std::io::Error::other)?;
    // The code's own fields, never what the file says: qass's tools can write the file.
    stamp(&mut note);
    let (problems, daemon) = check(&note);
    refuse(&problems)?;
    if !daemon {
        return Err(std::io::Error::other(
            "qoop's daemon is not running, so the note cannot be checked against tab and remote \
             names; start qoop and send it again",
        ));
    }
    Ok((path, note))
}

/// The fields the code fills: this qoop's version and the agent qass runs on.
fn stamp(note: &mut Note) {
    let agent = config::read().qass_agent;
    note.version = env!("CARGO_PKG_VERSION").to_string();
    note.agent = if AGENTS.contains(&agent.as_str()) {
        agent
    } else {
        "other".to_string()
    };
}

/// Whether note `n` may go now, under `[report] send`: never under `never`; under `ask` only
/// with `yes` (the person said yes to this note); under `always` by itself once a first note
/// went out after a yes.
pub fn may_send(n: u32, yes: bool) -> std::io::Result<()> {
    let send = config::read().report_send;
    let dir = folder()?;
    if allowed(&send, !any_sent(&dir), yes) {
        return Ok(());
    }
    Err(std::io::Error::other(if send == "never" {
        format!(
            "report.send is never: note {n} stays in {}; `qoop settings` changes it",
            dir.display()
        )
    } else {
        format!(
            "show the person note {n} (`qoop report show {n}`) and send it only after they said \
             yes: `qoop report send {n} --yes`"
        )
    }))
}

/// [`may_send`]'s rule: `send` is the setting, `first` whether no note went out yet.
fn allowed(send: &str, first: bool, yes: bool) -> bool {
    match send {
        "never" => false,
        "always" => yes || !first,
        _ => yes,
    }
}

/// Note `n` went out: the hub's key that takes it off again, kept beside it.
pub fn mark_sent(n: u32, delete_key: &str) -> std::io::Result<()> {
    std::fs::write(
        folder()?.join(format!("{n}.sent")),
        format!("{delete_key}\n"),
    )
}

/// The key that takes sent note `n` off the hub.
pub fn sent_key(n: u32) -> Option<String> {
    let key = std::fs::read_to_string(folder().ok()?.join(format!("{n}.sent"))).ok()?;
    Some(key.trim().to_string()).filter(|k| !k.is_empty())
}

/// Sent note `n` is off the hub: it is kept here as a note not sent.
pub fn mark_deleted(n: u32) -> std::io::Result<()> {
    match std::fs::remove_file(folder()?.join(format!("{n}.sent"))) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A note number as typed.
pub fn number(n: &str) -> std::io::Result<u32> {
    n.parse().map_err(|_| {
        std::io::Error::other(format!("{n}: a note number, as `qoop report list` shows"))
    })
}

fn draft() -> std::io::Result<()> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
    let mut note = Note::parse(&text).map_err(std::io::Error::other)?;
    let read = config::read();
    stamp(&mut note);
    let (problems, daemon) = check(&note);
    refuse(&problems)?;
    let dir = folder()?;
    std::fs::create_dir_all(&dir)?;
    // One problem is one note: the hub would only count it twice.
    if let Some(kept) = alike(&dir, &note.what) {
        return Err(std::io::Error::other(format!(
            "this problem is already note {kept} (`qoop report show {kept}`)"
        )));
    }
    let n = next_number(&dir);
    let path = dir.join(format!("{n}.txt"));
    std::fs::write(&path, note.text())?;
    println!(
        "note {n} is kept in {}. It would send exactly these bytes:\n\n{}",
        path.display(),
        note.text()
    );
    println!(
        "{}",
        match read.report_send.as_str() {
            "never" => "report.send is never: it stays here and nothing is sent.".to_string(),
            "always" if any_sent(&dir) => format!(
                "report.send is always: send it with `qoop report send {n}` and tell the person \
                 in one line."
            ),
            how => format!(
                "report.send is {how}: show the person the lines above exactly as they are, \
                 with one sentence in their language saying what they are, and send it only \
                 after they said yes: `qoop report send {n} --yes`."
            ),
        }
    );
    if !daemon {
        println!(
            "qoop's daemon did not answer: the note was not checked against space, tab and \
             remote names"
        );
    }
    Ok(())
}

fn list() -> std::io::Result<()> {
    let dir = folder()?;
    let mut numbers = numbers(&dir);
    numbers.sort_unstable();
    if numbers.is_empty() {
        println!(
            "no notes kept. When a problem in qass's own way of working is qoop's, qass drafts \
             one here and shows it to you first; `qoop settings` (notes to qoop's maker) says \
             whether it may send them."
        );
    }
    for n in numbers {
        let what = std::fs::read_to_string(dir.join(format!("{n}.txt")))
            .ok()
            .and_then(|t| Note::parse(&t).ok())
            .map(|note| note.what.chars().take(60).collect::<String>())
            .unwrap_or_default();
        let state = if sent_key(n).is_some() {
            "sent"
        } else {
            "kept"
        };
        println!("{n}  {state}  {what}");
    }
    Ok(())
}

fn remove(n: u32) -> std::io::Result<()> {
    if sent_key(n).is_some() {
        return Err(std::io::Error::other(format!(
            "note {n} was sent: `qoop report delete {n}` takes it off the hub first"
        )));
    }
    std::fs::remove_file(folder()?.join(format!("{n}.txt")))
        .map_err(|e| std::io::Error::other(format!("note {n}: {e}")))
}

/// Every rule the note breaks, its own-name ones included, and whether the daemon answered
/// for the space, tab and remote names.
fn check(note: &Note) -> (Vec<String>, bool) {
    let mut out = report::problems(note);
    let (names, daemon) = own_names();
    out.extend(report::own_name_problems(note, &names));
    (out, daemon)
}

fn refuse(problems: &[String]) -> std::io::Result<()> {
    if problems.is_empty() {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "the note does not pass; write it again:\n  {}",
        problems.join("\n  ")
    )))
}

fn numbers(dir: &Path) -> Vec<u32> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".txt")?.parse().ok())
        .collect()
}

/// The kept or sent note that names the same problem as `what`, if any.
fn alike(dir: &Path, what: &str) -> Option<u32> {
    let key = report::group_key(what);
    numbers(dir).into_iter().find(|n| {
        std::fs::read_to_string(dir.join(format!("{n}.txt")))
            .ok()
            .and_then(|t| Note::parse(&t).ok())
            .is_some_and(|note| report::group_key(&note.what) == key)
    })
}

fn next_number(dir: &Path) -> u32 {
    numbers(dir).into_iter().max().unwrap_or(0) + 1
}

fn any_sent(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".sent"))
}

/// The person's own names, read here and never sent: the user and full name, git's name and
/// email, the computer's names, `~/.ssh/config` hosts, qoop's spaces, tabs, their folders and
/// remotes, and the last folders of Claude Code's project list. A name of several words also
/// counts by its parts, and a folder's name counts, when they are not common words: a
/// worktree called `search-empty` must not ban the word `empty`.
fn own_names() -> (Vec<String>, bool) {
    // The person's names count by every part, common words too: a surname may be `king`.
    let mut person: Vec<String> = ["USER", "LOGNAME"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .collect();
    person.extend(output("id", &["-F"]));
    person.extend(output("git", &["config", "--global", "--get", "user.name"]));
    if let Some(email) = output("git", &["config", "--global", "--get", "user.email"]) {
        person.extend(email.split(['@', '.']).map(str::to_string));
    }
    let mut whole = Vec::new();
    whole.extend(output("hostname", &[]));
    for key in ["ComputerName", "LocalHostName"] {
        whole.extend(output("scutil", &["--get", key]));
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    if let Ok(ssh) = std::fs::read_to_string(home.join(".ssh/config")) {
        for line in ssh.lines() {
            let mut words = line.split_whitespace();
            if let Some(key) = words.next()
                && (key.eq_ignore_ascii_case("host") || key.eq_ignore_ascii_case("hostname"))
            {
                whole.extend(words.flat_map(|w| w.split('.')).map(str::to_string));
            }
        }
    }
    let (names, mut folders, cwds, daemon) = qoop_names();
    whole.extend(names);
    // The owner and name of each tab's repository (`acme/billing`).
    for cwd in cwds.iter().take(40) {
        if let Some(url) = output("git", &["-C", cwd, "remote", "get-url", "origin"]) {
            let url = url.trim_end_matches(".git");
            let mut parts = url.rsplit(['/', ':']);
            whole.extend(parts.next().map(str::to_string));
            whole.extend(parts.next().map(str::to_string));
        }
    }
    for project in std::fs::read_dir(home.join(".claude/projects"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let slug = project.file_name().to_string_lossy().to_string();
        let parts: Vec<&str> = slug.split('-').filter(|p| !p.is_empty()).collect();
        if let [.., before, last] = parts.as_slice() {
            folders.push(last.to_string());
            folders.push(format!("{before}{last}"));
        }
    }
    let mut out: Vec<String> = folders
        .into_iter()
        .filter(|f| !report::common_word(&f.to_ascii_lowercase()))
        .collect();
    for name in &person {
        out.extend(
            name.split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|p| p.len() >= 2)
                .map(str::to_string),
        );
        out.push(name.clone());
    }
    for name in whole {
        let name = name.trim().to_string();
        let parts: Vec<&str> = name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|p| p.len() >= 3)
            .collect();
        if parts.len() > 1 {
            out.extend(
                parts
                    .iter()
                    .filter(|p| !report::common_word(&p.to_ascii_lowercase()))
                    .map(|p| p.to_string()),
            );
        }
        out.push(name);
    }
    out.retain(|n| !n.is_empty());
    out.sort();
    out.dedup();
    (out, daemon)
}

/// qoop's space, tab and remote names, the last two folders of each tab, the tabs' folders,
/// and whether this machine's daemon answered at all.
fn qoop_names() -> (Vec<String>, Vec<String>, Vec<String>, bool) {
    let Ok(me) = std::env::current_exe() else {
        return (Vec::new(), Vec::new(), Vec::new(), false);
    };
    let json = |args: &[&str]| -> Vec<serde_json::Value> {
        Command::new(&me)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .and_then(|o| serde_json::from_slice(&o.stdout).ok())
            .unwrap_or_default()
    };
    let (mut out, mut folders, mut cwds) = (Vec::new(), Vec::new(), Vec::new());
    // `ls` reads the store without a daemon; only a daemon knows the tabs.
    let daemon = paths::runtime_dir()
        .is_ok_and(|dir| std::os::unix::net::UnixStream::connect(paths::socket_path(&dir)).is_ok());
    for space in json(&["ls", "--json"]) {
        out.extend(space["name"].as_str().map(str::to_string));
    }
    for tab in json(&["agents", "--json"]) {
        out.extend(tab["name"].as_str().map(str::to_string));
        let cwd = Path::new(tab["cwd"].as_str().unwrap_or_default());
        if !cwd.as_os_str().is_empty() && !cwds.contains(&cwd.display().to_string()) {
            cwds.push(cwd.display().to_string());
        }
        for dir in [cwd.file_name(), cwd.parent().and_then(Path::file_name)] {
            folders.extend(dir.map(|d| d.to_string_lossy().to_string()));
        }
    }
    if let Ok(remotes) = Command::new(&me)
        .args(["remote", "ls"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        for line in String::from_utf8_lossy(&remotes.stdout).lines() {
            out.extend(
                line.split_whitespace()
                    .flat_map(|w| w.split(['@', '.']))
                    .map(str::to_string),
            );
        }
    }
    (out, folders, cwds, daemon)
}

/// One line a command prints, or nothing when it is missing or fails.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No answer is `ask`: nothing goes without a yes; `always` still waits for the first.
    #[test]
    fn a_note_goes_only_after_a_yes_or_under_always_after_the_first() {
        for (send, first, yes, want) in [
            ("never", false, true, false),
            ("ask", false, false, false),
            ("ask", true, true, true),
            ("always", true, false, false),
            ("always", true, true, true),
            ("always", false, false, true),
            ("", false, false, false),
        ] {
            assert_eq!(allowed(send, first, yes), want, "{send} {first} {yes}");
        }
    }

    #[test]
    fn numbers_count_up_and_a_sent_note_is_seen() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(next_number(dir.path()), 1);
        std::fs::write(dir.path().join("1.txt"), "").expect("write");
        std::fs::write(dir.path().join("7.txt"), "").expect("write");
        std::fs::write(dir.path().join("notes.txt"), "").expect("write");
        assert_eq!(next_number(dir.path()), 8);
        assert!(!any_sent(dir.path()));
        std::fs::write(dir.path().join("7.sent"), "key\n").expect("write");
        assert!(any_sent(dir.path()));
    }

    #[test]
    fn a_problem_kept_already_is_found_by_its_what() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(
            dir.path().join("3.txt"),
            "area: watcher\nwhat: The watcher slept.\ncost: a wake\noften: 1 time in 1 day\n",
        )
        .expect("write");
        assert_eq!(alike(dir.path(), "the  watcher slept"), Some(3));
        assert_eq!(alike(dir.path(), "a tab woke"), None);
    }
}

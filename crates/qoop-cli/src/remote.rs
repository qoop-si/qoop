//! `qoop remote`: the machines whose spaces the interface shows beside this one's.
//!
//! Adding one is the only step that talks to a person: it runs ssh in the foreground, so a
//! password, a passphrase, a new host key or a hardware key touch is answered where it is
//! asked. It also leaves ssh's shared connection open for a while, which is what the
//! interface's own connection, which never asks anything, then rides on.

use std::io;
use std::process::{Command, Stdio};

use qoop_remote::saved::{self, RELAY_MARKER, Remote, remote_command};
use qoop_remote::{Compatibility, RemoteHello, SshTransport, compatibility};

pub fn run(args: &[String]) -> io::Result<()> {
    let done = match args.get(2).map(String::as_str) {
        Some("add") => add(&args[3..]),
        Some("rm") => rm(args.get(3).map(String::as_str)),
        Some("ls") | None => ls(),
        _ => {
            eprintln!(
                "usage: qoop remote add <name> <ssh-target>\n\
                 \x20      qoop remote rm <name>\n\
                 \x20      qoop remote ls"
            );
            std::process::exit(2);
        }
    };
    // Said as a sentence: these are answers to a person at a prompt, not a trace.
    if let Err(e) = done {
        eprintln!("qoop: {e}");
        std::process::exit(1);
    }
    Ok(())
}

fn refuse(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn add(rest: &[String]) -> io::Result<()> {
    let [name, target] = rest else {
        return Err(refuse("usage: qoop remote add <name> <ssh-target>".into()));
    };
    if !saved::valid_name(name) {
        return Err(refuse(format!(
            "{name:?} cannot be a remote's name: letters, digits, `-`, `_` and `.`, up to 24"
        )));
    }
    if !saved::valid_target(target) {
        return Err(refuse(format!("{target:?} cannot be an ssh target")));
    }
    let file = saved::path();
    let mut list = saved::load_from(&file)?;
    if list.iter().any(|r| &r.name == name) {
        return Err(refuse(format!(
            "{name} is already added; `qoop remote rm {name}` first to change it"
        )));
    }
    // The same machine under a second name would list every one of its spaces twice, once
    // under each name, both driving the same terminals.
    if let Some(same) = list.iter().find(|r| &r.target == target) {
        return Err(refuse(format!(
            "{target} is already added as {}",
            same.name
        )));
    }
    let remote = Remote {
        name: name.clone(),
        target: target.clone(),
    };
    let hello = check(&remote)?;
    list.push(remote);
    saved::save_to(&file, &list)?;
    println!(
        "added {name}: qoop {} on {}. its spaces show in `qoop` as {name}:<space>, and \
         the server shows on qoop.si/app and your phone once it joins",
        hello.version, hello.platform
    );
    Ok(())
}

/// Reach the remote in the foreground and ask its qoop what it is, refusing one this
/// build cannot talk to.
pub fn check(remote: &Remote) -> io::Result<RemoteHello> {
    let hello = match probe(remote) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            offer_install(remote)?;
            probe(remote)?
        }
        other => other?,
    };
    match compatibility(&hello) {
        Ok(Compatibility::AsIs) => {}
        Ok(Compatibility::NeedsRestart) => eprintln!(
            "qoop on {} is {}, older than this one: its spaces show as offline until it is \
             updated there",
            remote.name, hello.version
        ),
        Err(e) => return Err(io::Error::other(format!("{}: {e}", remote.name))),
    }
    Ok(hello)
}

/// Wake the machine's last space, or open its first tab, as a bare `qoop` does here. Best
/// effort: an older qoop there does not know the verb, and its spaces are shown as they are.
pub fn wake(remote: &Remote) {
    let mut transport = SshTransport::new(&remote.target);
    if let Some(path) = qoop_tui::link::control_path() {
        transport = transport.with_control_path(path);
    }
    let argv = transport.argv(&remote_command("ensure-awake"));
    let _ = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn probe(remote: &Remote) -> io::Result<RemoteHello> {
    let mut transport = SshTransport::new(&remote.target);
    if let Some(path) = qoop_tui::link::control_path() {
        transport = transport.with_control_path(path);
    }
    let argv = transport.argv(&remote_command("--hello"));
    // Standard input and error stay with the person: that is where ssh asks, and where it
    // says why it could not connect.
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let answer = text
        .split_once(RELAY_MARKER)
        .map(|(_, after)| after.trim())
        .unwrap_or_default();
    // Told apart by what happened: the remote command prints the marker and exits 127 when
    // it finds no qoop, and nothing else does both.
    if out.status.code() == Some(127) && text.contains(RELAY_MARKER) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("qoop is not installed on {}", remote.target),
        ));
    }
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "could not reach qoop on {} (ssh exited with {})",
            remote.target, out.status
        )));
    }
    serde_json::from_str(answer).map_err(|_| {
        io::Error::other(format!(
            "{} answered something that is not qoop: {answer:?}",
            remote.target
        ))
    })
}

/// Offer to put this version of qoop on a machine that has none, and do it on a yes.
///
/// Asked only of a person at a terminal, after ssh has finished with it: a pipe answering
/// on their behalf is not a yes, and a script gets the way to do it instead.
fn offer_install(remote: &Remote) -> io::Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let version = env!("CARGO_PKG_VERSION");
    if !std::io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "qoop is not installed on {}; run this from a terminal to install it there",
            remote.target
        )));
    }
    print!(
        "qoop is not installed on {}. install qoop {version} there, in ~/.local/bin? [y/N] ",
        remote.target
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        return Err(io::Error::other("not installed; nothing was changed"));
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let installed = runtime.block_on(qoop_tui::link::install(&remote.target))?;
    println!("installed qoop {installed} on {}", remote.target);
    Ok(())
}

fn rm(name: Option<&str>) -> io::Result<()> {
    let Some(name) = name else {
        return Err(refuse("usage: qoop remote rm <name>".into()));
    };
    let file = saved::path();
    let mut list = saved::load_from(&file)?;
    let before = list.len();
    list.retain(|r| r.name != name);
    if list.len() == before {
        return Err(refuse(format!("there is no remote called {name}")));
    }
    saved::save_to(&file, &list)?;
    println!("removed {name}");
    Ok(())
}

fn ls() -> io::Result<()> {
    let list = saved::load()?;
    // On stderr: standard output is the list, and a script reading it (qass's watch) took the
    // first word of this sentence for a machine called `no`.
    if list.is_empty() {
        eprintln!("no remotes. `qoop remote add <name> <ssh-target>` adds one");
    }
    for r in list {
        println!("{}  {}", r.name, r.target);
    }
    Ok(())
}
